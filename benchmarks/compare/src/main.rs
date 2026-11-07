//! `bench-compare`: reproducible comparison of dir-24-8 with other LPM and
//! membership structures (latency, throughput, memory, build time, thread
//! scaling) with JSON/CSV results and a standalone HTML report. See
//! `benchmarks/README.md` and `bench-compare --help`.
//!
//! Orchestration: datasets are generated once per (family, size) and
//! cached; then each (size, family, structure) runs in its own process
//! (memory isolation, `RLIMIT_DATA` limit, timeout), which builds the
//! structure once and measures every operation x distribution x thread
//! count on it. Runs go by increasing size so that the memory and time of
//! larger sizes are extrapolated from the smaller ones and doomed runs are
//! reported as skipped instead of being attempted.

use dir_24_8_compare::{adapter, bench, dataset, measure, options, registry, report, sys, timer};

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use bench::{Op, RunCfg, RunResult, Status};
use dataset::{Family, Params};
use options::{Options, size_label};

#[global_allocator]
static ALLOC: measure::Counting = measure::Counting;

#[derive(Serialize, Deserialize)]
struct PrepareSpec {
    family: Family,
    size: usize,
    params: Params,
    path: PathBuf,
}

fn main() {
    let o = options::parse();
    if let Some(spec) = &o.child {
        let result = o.result.as_deref().unwrap_or_else(|| options::usage());
        child(spec, result);
        return;
    }
    if let Some(spec) = &o.prepare {
        let s: PrepareSpec = serde_json::from_str(spec).expect("valid prepare spec");
        if let Err(e) = dataset::prepare(s.family, s.size, &s.params, &s.path) {
            eprintln!("dataset generation failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(path) = &o.report_only {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let r: report::Results = serde_json::from_str(&text).expect("valid results file");
        let files = report::write_all(&o.out, &r, false).expect("writable output directory");
        eprintln!("reports regenerated from {}:", path.display());
        for f in files {
            eprintln!("  {}", f.display());
        }
        refresh_readme(&o, &r);
        return;
    }
    let entries = registry::select(&o.engines);
    if entries.is_empty() {
        eprintln!("no structure matches --engines {:?}", o.engines);
        std::process::exit(2);
    }
    if o.list {
        print_plan(&o, &entries);
        return;
    }
    let failures = orchestrate(&o, &entries);
    if failures > 0 {
        std::process::exit(1);
    }
}

fn print_plan(o: &Options, entries: &[registry::Entry]) {
    println!(
        "sizes: {}",
        o.sizes
            .iter()
            .map(|&s| size_label(s))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "families: {:?}  ops: {:?}  dists: {:?}  threads: {:?}  batch: {:?}",
        o.families, o.ops, o.dists, o.threads, o.batch
    );
    println!("structures:");
    for e in entries {
        let i = &e.info;
        println!(
            "  {:<26} lookup={:?} batch={:?} exists={:?} families={:?}{}",
            i.name,
            i.lookup,
            i.batch,
            i.exists,
            i.families,
            if i.default { "" } else { "  (not in default)" }
        );
    }
    println!(
        "processes: {} (one per size x family x structure)",
        o.sizes.len() * o.families.len() * entries.len()
    );
}

/// Measuring process: loads the dataset, runs one structure, writes JSON.
fn child(spec: &str, result: &Path) {
    let cfg: RunCfg = serde_json::from_str(spec).expect("valid run spec");
    let entry = registry::catalog()
        .into_iter()
        .find(|e| e.info.name == cfg.engine)
        .expect("known structure");
    let file = std::fs::metadata(&cfg.data).map_or(0, |m| m.len() as usize);
    // Inputs expand ~2.5x in memory; 512 MiB covers the binary, buffers and
    // worker stacks. A limit of 0 means none.
    if cfg.mem_limit > 0 {
        sys::set_data_limit(cfg.mem_limit + file * 3 + (512 << 20));
    }
    let t = Instant::now();
    let mut r = match cfg.family {
        Family::Ipv4 => {
            let ds = dataset::load::<u32>(&cfg.data).expect("dataset");
            let load = t.elapsed().as_secs_f64();
            let mut r = entry.run4(&ds, &cfg);
            r.load_s = Some(load);
            r
        }
        Family::Ipv6 => {
            let ds = dataset::load::<u128>(&cfg.data).expect("dataset");
            let load = t.elapsed().as_secs_f64();
            let mut r = entry.run6(&ds, &cfg);
            r.load_s = Some(load);
            r
        }
    };
    r.process_s = t.elapsed().as_secs_f64();
    std::fs::write(result, serde_json::to_vec(&r).expect("serializable")).expect("result file");
}

/// Regenerates the "Detailed results" block of `README.md` when
/// `BENCH_README` allows it; a failure is reported, not fatal.
fn refresh_readme(o: &Options, r: &report::Results) {
    let path = &o.readme_file;
    if !dir_24_8_compare::readme::wanted(&o.readme, r) {
        eprintln!(
            "  {} not updated (README={}: auto needs a complete run of the default selection up to 1M entries)",
            path.display(),
            o.readme
        );
        return;
    }
    match dir_24_8_compare::readme::update(path, r) {
        Ok(true) => eprintln!("  updated the detailed results of {}", path.display()),
        Ok(false) => eprintln!(
            "  detailed results of {} already up to date",
            path.display()
        ),
        Err(e) => eprintln!("  warning: README not updated: {e}"),
    }
}

enum Exit {
    Success,
    Timeout,
    Failed(String),
}

/// Runs `exe` (this executable when `None`) with `args`, killing it after
/// `timeout`.
fn spawn(exe: Option<&Path>, args: &[String], timeout: Duration) -> (Exit, String) {
    let exe = exe.map_or_else(
        || std::env::current_exe().expect("current executable"),
        Path::to_path_buf,
    );
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn measuring process");
    let mut pipe = child.stderr.take().expect("stderr");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().expect("wait") {
            break Some(st);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stderr = reader.join().unwrap_or_default();
    let exit = match status {
        None => Exit::Timeout,
        Some(st) if st.success() => Exit::Success,
        Some(st) => {
            #[cfg(unix)]
            let sig = std::os::unix::process::ExitStatusExt::signal(&st);
            #[cfg(not(unix))]
            let sig: Option<i32> = None;
            Exit::Failed(match sig {
                Some(s) => format!("terminated by signal {s}"),
                None => format!("exit code {}", st.code().unwrap_or(-1)),
            })
        }
    };
    (exit, stderr)
}

fn last_lines(s: &str, n: usize) -> String {
    let v: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    v[v.len().saturating_sub(n)..].join(" | ")
}

fn mib(b: usize) -> String {
    format!("{:.0} MiB", b as f64 / (1u64 << 20) as f64)
}

/// Completed measurement of one (structure, family) at one size.
#[derive(Clone, Copy)]
struct Point {
    size: usize,
    peak: usize,
    build_s: f64,
    process_s: f64,
}

/// Linear extrapolation through the last two points; with a single point,
/// proportional when `proportional` (build time), otherwise no estimate
/// (memory: fixed allocations would be scaled too).
fn extrapolate(
    pts: &[Point],
    size: usize,
    proportional: bool,
    f: impl Fn(&Point) -> f64,
) -> Option<f64> {
    match pts {
        [] => None,
        [p] => proportional.then(|| f(p) * size as f64 / p.size as f64),
        [.., a, b] => {
            let slope = (f(b) - f(a)) / (b.size as f64 - a.size as f64);
            Some((f(b) + slope.max(0.0) * (size as f64 - b.size as f64)).max(f(b)))
        }
    }
}

fn fmt_dur(s: f64) -> String {
    let s = s.round() as u64;
    if s >= 3600 {
        format!("{}h{:02}m{:02}s", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// Runs the whole plan; returns the number of invalid or failed runs.
fn orchestrate(o: &Options, entries: &[registry::Entry]) -> usize {
    let start = Instant::now();
    let env = measure::env();
    let ticks_per_s = timer::ticks_per_second(Duration::from_millis(200));
    // 0 = no limit.
    let (mem_limit, mem_rule) = match o.mem_limit {
        options::MemLimit::None => (
            0,
            "none: every size is attempted (MEM_LIMIT_MIB=<n> or MEM_LIMIT_MIB=auto sets one)"
                .to_string(),
        ),
        options::MemLimit::Mib(m) => (m << 20, format!("{m} MiB (configured)")),
        options::MemLimit::Auto => {
            let avail = sys::meminfo("MemAvailable").unwrap_or(4 << 30);
            let lim = (avail as f64 * 0.7) as usize;
            (
                lim,
                format!(
                    "{} = 70% of MemAvailable ({}) at start",
                    mib(lim),
                    mib(avail)
                ),
            )
        }
    };
    let params = Params {
        seed: o.seed,
        queries: o.queries,
        hit_permille: o.hit_permille,
    };
    let cpus = sys::cpu_order();
    let mut results = report::Results::new(o, env, ticks_per_s, mem_limit, mem_rule.clone(), &cpus);
    results.engines = entries.iter().map(|e| e.info.clone()).collect();
    let total = o.sizes.len() * o.families.len() * entries.len();
    let tmp = o.cache.join("results");
    std::fs::create_dir_all(&tmp).expect("cache directory");
    std::fs::create_dir_all(&o.out).expect("output directory");
    eprintln!(
        "bench-compare: {} sizes x {} families x {} structures = {total} runs; ops {:?}, dists {:?}, threads {:?}, batch {:?} (native batch APIs also {:?} with {} thread); dir-24-8 batches: {}, dir-24-8[scalar]: scalar",
        o.sizes.len(),
        o.families.len(),
        entries.len(),
        o.ops,
        o.dists,
        o.threads,
        o.batch,
        o.batch_sweep,
        o.threads.first().copied().unwrap_or(1),
        adapter::batch_kernel()
    );
    eprintln!(
        "  memory limit {mem_rule}; budget {}; timeout {} s per process; timer {} ({:.3} GHz); cache {}",
        if o.budget_s == 0 {
            "none".into()
        } else {
            format!("{} s", o.budget_s)
        },
        o.timeout_s,
        timer::source(),
        ticks_per_s / 1e9,
        o.cache.display()
    );

    let mut history: HashMap<(String, Family), Vec<Point>> = HashMap::new();
    let mut failed: HashMap<(String, Family), (Status, String)> = HashMap::new();
    let mut k = 0usize;
    let mut prep_s = 0.0f64;
    for &size in &o.sizes {
        for &fam in &o.families {
            let path = dataset::cache_path(&o.cache, fam, size, &params);
            let mut ready: Option<Result<(), String>> = None;
            for e in entries {
                k += 1;
                let name = e.info.name.clone();
                let key = (name.clone(), fam);
                let cfg = RunCfg {
                    family: fam,
                    size,
                    engine: name.clone(),
                    data: path.clone(),
                    ops: o.ops.clone(),
                    dists: o.dists.clone(),
                    threads: o.threads.clone(),
                    batch: o.batch.clone(),
                    batch_sweep: o.batch_sweep.clone(),
                    warmup_ms: o.warmup_ms,
                    window_ms: o.window_ms,
                    reps: o.reps,
                    lat_samples: o.lat_samples,
                    lat_batches: o.lat_batches,
                    lat_max_ms: o.lat_max_ms,
                    validate: o.validate,
                    ticks_per_s,
                    mem_limit,
                    pin: o.pin,
                };
                let supported = o.ops.iter().any(|op| match op {
                    Op::Lookup => e.info.lookup != adapter::Api::Unsupported,
                    Op::Batch => e.info.batch != adapter::Api::Unsupported,
                    Op::Exists => e.info.exists != adapter::Api::Unsupported,
                });
                let pts = history.get(&key).cloned().unwrap_or_default();
                let est_mem = extrapolate(&pts, size, false, |p| p.peak as f64);
                let est_time = extrapolate(&pts, size, true, |p| p.build_s)
                    .zip(pts.last().map(|p| p.process_s - p.build_s));
                let skip: Option<(Status, String)> = if let Some(r) = e.unavailable(fam) {
                    Some((Status::NotApplicable, r))
                } else if !supported {
                    Some((
                        Status::NotApplicable,
                        "none of the selected operations is supported".into(),
                    ))
                } else if let Some((s, r)) = failed.get(&key) {
                    Some((s.clone(), r.clone()))
                } else if o.budget_s > 0 && start.elapsed().as_secs() >= o.budget_s {
                    Some((
                        Status::Budget,
                        format!("time budget of {} s exhausted", o.budget_s),
                    ))
                } else if let Some(est) = est_mem.filter(|&m| mem_limit > 0 && m > mem_limit as f64)
                {
                    Some((
                        Status::MemoryLimit,
                        format!(
                            "estimated peak {} (extrapolated from {} entries) exceeds the {} limit",
                            mib(est as usize),
                            size_label(pts.last().map_or(0, |p| p.size)),
                            mib(mem_limit)
                        ),
                    ))
                } else if let Some((b, rest)) =
                    est_time.filter(|(b, rest)| b + rest > o.timeout_s as f64)
                {
                    Some((
                        Status::Timeout,
                        format!(
                            "estimated {:.0} s build + {:.0} s measurement exceeds the {} s timeout",
                            b, rest, o.timeout_s
                        ),
                    ))
                } else {
                    None
                };
                let t0 = Instant::now();
                let r = if let Some((status, reason)) = skip {
                    RunResult::status(&cfg, status, reason)
                } else {
                    // Dataset: generated once per (family, size), reused from
                    // the cache when the parameters did not change.
                    let ds_ok = ready
                        .get_or_insert_with(|| {
                            let t = Instant::now();
                            let cached = dataset::read_meta(&path).is_ok();
                            if !cached {
                                eprint!(
                                    "  generating {} {} dataset... ",
                                    fam.label(),
                                    size_label(size)
                                );
                                let spec = PrepareSpec {
                                    family: fam,
                                    size,
                                    params,
                                    path: path.clone(),
                                };
                                let (exit, err) = spawn(
                                    None,
                                    &[
                                        "--prepare".into(),
                                        serde_json::to_string(&spec).expect("json"),
                                    ],
                                    Duration::from_secs(o.timeout_s.max(1800)),
                                );
                                if !matches!(exit, Exit::Success) {
                                    eprintln!("failed");
                                    return Err(format!(
                                        "dataset generation failed: {}",
                                        last_lines(&err, 3)
                                    ));
                                }
                                eprintln!("{:.1} s", t.elapsed().as_secs_f64());
                            }
                            prep_s += t.elapsed().as_secs_f64();
                            let mut meta = dataset::read_meta(&path).map_err(|e| e.to_string())?;
                            meta.cached = cached;
                            results.datasets.push(meta);
                            Ok(())
                        })
                        .clone();
                    match ds_ok {
                        Err(reason) => RunResult::status(&cfg, Status::Failed, reason),
                        Ok(()) => {
                            let out = tmp.join(format!("{}-{}-{k}.json", fam.name(), size));
                            // The scalar dir-24-8 entry runs in the build
                            // without `batch-simd` (checked by `unavailable`).
                            let exe = (name == adapter::OURS_SCALAR
                                && adapter::batch_kernel() != "scalar")
                                .then(registry::scalar_exe)
                                .and_then(Result::ok);
                            let (exit, err) = spawn(
                                exe.as_deref(),
                                &[
                                    "--child".into(),
                                    serde_json::to_string(&cfg).expect("json"),
                                    "--result".into(),
                                    out.display().to_string(),
                                ],
                                Duration::from_secs(o.timeout_s),
                            );
                            let parsed = std::fs::read(&out)
                                .ok()
                                .and_then(|b| serde_json::from_slice::<RunResult>(&b).ok());
                            let _ = std::fs::remove_file(&out);
                            match (exit, parsed) {
                                (Exit::Success, Some(r)) => r,
                                (Exit::Timeout, _) => RunResult::status(
                                    &cfg,
                                    Status::Timeout,
                                    format!("exceeded the {} s timeout", o.timeout_s),
                                ),
                                (Exit::Failed(how), _)
                                    if err.contains("memory allocation of")
                                        || err.contains("AllocationFailed")
                                        || err.contains("capacity overflow")
                                        || err.contains("Cannot allocate memory") =>
                                {
                                    if mem_limit > 0 {
                                        RunResult::status(
                                            &cfg,
                                            Status::MemoryLimit,
                                            format!(
                                                "allocation failed under the {} limit ({how})",
                                                mib(mem_limit)
                                            ),
                                        )
                                    } else {
                                        RunResult::status(
                                            &cfg,
                                            Status::Failed,
                                            format!("out of memory, no limit set ({how})"),
                                        )
                                    }
                                }
                                (Exit::Failed(how), _) if how.ends_with("signal 9") => {
                                    RunResult::status(
                                        &cfg,
                                        Status::Failed,
                                        format!(
                                            "{how}: killed, most likely by the kernel OOM killer (machine out of memory)"
                                        ),
                                    )
                                }
                                (Exit::Failed(how), _) => RunResult::status(
                                    &cfg,
                                    Status::Failed,
                                    format!("{how}: {}", last_lines(&err, 3)),
                                ),
                                (Exit::Success, None) => RunResult::status(
                                    &cfg,
                                    Status::Failed,
                                    "no result produced".to_string(),
                                ),
                            }
                        }
                    }
                };
                // Bookkeeping for the estimates of larger sizes.
                match r.status {
                    Status::Ok => {
                        if let Some(m) = &r.memory {
                            history.entry(key.clone()).or_default().push(Point {
                                size,
                                peak: m
                                    .build_peak_heap
                                    .max(m.footprint)
                                    .max(m.build_peak_rss.unwrap_or(0)),
                                build_s: r.build_s.unwrap_or(0.0),
                                process_s: r.process_s,
                            });
                        }
                    }
                    Status::MemoryLimit | Status::Timeout => {
                        failed.entry(key.clone()).or_insert_with(|| {
                            (
                                r.status.clone(),
                                format!(
                                    "not attempted: {} at {} entries ({})",
                                    if r.status == Status::Timeout {
                                        "timed out"
                                    } else {
                                        "exceeded the memory limit"
                                    },
                                    size_label(size),
                                    r.reason.clone().unwrap_or_default()
                                ),
                            )
                        });
                    }
                    _ => {}
                }
                let detail = match (&r.status, r.build_s) {
                    (Status::Ok, Some(b)) => format!(
                        "build {:>8.3} s  {:>9}  {:>5.1} s",
                        b,
                        r.memory
                            .as_ref()
                            .map_or(String::new(), |m| mib(m.footprint)),
                        t0.elapsed().as_secs_f64()
                    ),
                    _ => r.reason.clone().unwrap_or_default(),
                };
                eprintln!(
                    "[{k:>4}/{total}] {:>4} {:>4} {:<24} {:<14} {detail}  (elapsed {})",
                    size_label(size),
                    fam.name(),
                    name,
                    format!("{:?}", r.status).to_lowercase(),
                    fmt_dur(start.elapsed().as_secs_f64())
                );
                results.runs.push(r);
                results.meta.duration_s = start.elapsed().as_secs_f64();
                // Partial results survive an interruption.
                let _ = report::write_json(&o.out, &results);
            }
        }
    }
    results.meta.duration_s = start.elapsed().as_secs_f64();
    results.meta.datasets_s = prep_s;
    results.meta.complete = true;
    results.meta.load_end = sys::loadavg();
    let files = report::write_all(&o.out, &results, true).expect("writable output directory");
    let mut counts: HashMap<String, usize> = HashMap::new();
    for r in &results.runs {
        *counts
            .entry(format!("{:?}", r.status).to_lowercase())
            .or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort();
    eprintln!(
        "done in {} ({} measurements); runs: {}",
        fmt_dur(start.elapsed().as_secs_f64()),
        results.runs.iter().map(|r| r.measures.len()).sum::<usize>(),
        counts
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for f in files {
        eprintln!("  {}", f.display());
    }
    refresh_readme(o, &results);
    results
        .runs
        .iter()
        .filter(|r| matches!(r.status, Status::Invalid | Status::Failed))
        .count()
}

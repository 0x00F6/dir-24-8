//! Orchestration of `bench-compare-lib`: dependencies, corpora, MMDB files,
//! then one fresh runner process per (library, family, size, repetition),
//! repetitions in rounds with a rotating library order. Each process is
//! watched (timeout, resident memory); its JSON lines are kept verbatim in
//! the cache (`runs/<run id>/`) and aggregated into the results.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::corpus::{self, CorpusInfo};
use super::deps::{self, Deps};
use super::libs::{LIBS, LibDef, RunnerKind};
use super::results;
use super::settings::Settings;
use crate::dataset::Family;
use crate::{measure, sys};

/// One planned measurement of a process: `(metric, op, scenario, threads)`.
pub type Planned = (String, String, String, usize);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proc {
    pub lib: String,
    pub family: Family,
    pub size: usize,
    pub rep: usize,
    /// `main` or `edge:<case>`.
    pub kind: String,
    pub corpus: String,
    /// `ok`, `failed`, `timeout`, `skipped`.
    pub status: String,
    pub reason: Option<String>,
    pub wall_s: f64,
    pub exit: Option<i32>,
    pub order: usize,
    pub planned: Vec<Planned>,
    pub raw: String,
    #[serde(skip)]
    pub records: Vec<Value>,
    pub stderr_tail: String,
    pub watch_peak_rss: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MmdbInfo {
    pub corpus: String,
    pub family: Family,
    pub size: usize,
    pub path: String,
    pub status: String,
    pub reason: Option<String>,
    pub cached: bool,
    pub file_bytes: u64,
    pub fabricate_s: f64,
    pub writer: Value,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn cmd_line(cmd: &mut Command) -> Option<String> {
    let o = cmd.output().ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Version of a crate in the workspace `Cargo.lock`.
fn locked_version(repo: &Path, name: &str) -> Option<String> {
    let lock = std::fs::read_to_string(repo.join("Cargo.lock")).ok()?;
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == format!("name = \"{name}\"") {
            return lines
                .next()?
                .trim()
                .strip_prefix("version = ")?
                .trim_matches('"')
                .to_string()
                .into();
        }
    }
    None
}

fn project_info(repo: &Path) -> Value {
    let git = |args: &[&str]| cmd_line(Command::new("git").current_dir(repo).args(args));
    let version = std::fs::read_to_string(repo.join("Cargo.toml"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("version = ")
                    .map(|v| v.trim_matches('"').to_string())
            })
        });
    json!({"name": "dir-24-8", "version": version, "commit": git(&["rev-parse", "HEAD"]),
        "dirty": git(&["status", "--porcelain"]).map(|s| !s.is_empty()), "branch": git(&["rev-parse", "--abbrev-ref", "HEAD"])})
}

fn env_info(repo: &Path, s: &Settings, load_start: Option<f64>) -> Value {
    let e = measure::env();
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or(e.os.clone());
    let flags = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix("flags")
                    .map(|v| v.trim_start_matches([' ', '\t', ':']).to_string())
            })
        })
        .unwrap_or_default();
    let wanted = [
        "sse4_2",
        "popcnt",
        "bmi1",
        "bmi2",
        "lzcnt",
        "avx",
        "avx2",
        "avx512f",
        "avx512bw",
        "avx512vl",
        "avx512vpopcntdq",
    ];
    let present: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|f| flags.split_whitespace().any(|x| x == *f))
        .collect();
    let cc = cmd_line(Command::new("cc").arg("--version"))
        .map(|v| v.lines().next().unwrap_or("").to_string());
    json!({"cpu": e.cpu, "arch": e.arch, "logical_cpus": e.logical_cpus, "physical_cores": e.physical_cores,
        "cpu_max_mhz": e.cpu_max_mhz, "governor": e.governor, "boost": e.boost, "caches": e.caches,
        "cpu_features": present, "ram_total": e.ram_total, "ram_available_at_start": e.ram_available_at_start,
        "swap_used_at_start": e.swap_used_at_start, "os": os, "kernel": e.kernel, "thp": e.thp,
        "load_avg_at_start": load_start, "load_avg_at_end": sys::loadavg(),
        "compilers": {"rustc": e.rustc, "llvm": e.llvm, "rust_profile": format!("{} (opt-level {}, codegen-units 1, lto thin)", e.profile, e.opt_level),
            "rust_target_features": e.compile_target_features, "rustflags": e.rustflags,
            "go": format!("{} (GOTOOLCHAIN pinned in go.mod; CGO_ENABLED=0, GOAMD64={}, -trimpath)", deps::GO_TOOLCHAIN, s.goamd64),
            "cc": cc, "cflags": deps::C_FLAGS, "libmaxminddb_cflags": format!("{} (+ upstream -O2 -g)", deps::MMDB_CFLAGS)},
        "rust_crates": {"prefix-trie": locked_version(repo, "prefix-trie"), "iprange": locked_version(repo, "iprange"),
            "ipnet": locked_version(repo, "ipnet"), "dir-24-8": locked_version(repo, "dir-24-8")},
        "timestamp_unix": e.timestamp_unix})
}

fn mmdb_for(cache: &Path, mmdbgen: &Path, c: &CorpusInfo) -> MmdbInfo {
    let dir = cache.join("mmdb");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-{}-r32.mmdb", c.id, c.prefix_hash()));
    let side = path.with_extension("json");
    let base = MmdbInfo {
        corpus: c.id.clone(),
        family: c.family,
        size: c.size,
        path: path.display().to_string(),
        status: "ok".into(),
        reason: None,
        cached: false,
        file_bytes: 0,
        fabricate_s: 0.0,
        writer: Value::Null,
    };
    if let (Ok(meta), Ok(m)) = (std::fs::read(&side), std::fs::metadata(&path))
        && let Ok(v) = serde_json::from_slice::<Value>(&meta)
    {
        return MmdbInfo {
            cached: true,
            file_bytes: m.len(),
            fabricate_s: v["fabricate_s"].as_f64().unwrap_or(0.0),
            writer: v,
            ..base
        };
    }
    let t0 = Instant::now();
    let out = Command::new(mmdbgen)
        .arg("-corpus")
        .arg(&c.dir)
        .arg("-out")
        .arg(&path)
        .output();
    let fabricate_s = t0.elapsed().as_secs_f64();
    match out {
        Ok(o) if o.status.success() => {
            let mut v: Value = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
            v["fabricate_s"] = json!(fabricate_s);
            let _ = std::fs::write(&side, v.to_string());
            MmdbInfo {
                file_bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
                fabricate_s,
                writer: v,
                ..base
            }
        }
        Ok(o) => MmdbInfo {
            status: "failed".into(),
            reason: Some(format!(
                "mmdbgen: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            ..base
        },
        Err(e) => MmdbInfo {
            status: "failed".into(),
            reason: Some(format!("mmdbgen: {e}")),
            ..base
        },
    }
}

fn plan_text(
    lib: &LibDef,
    s: &Settings,
    corpus: &CorpusInfo,
    mmdb: Option<&str>,
    rep: usize,
    edge: bool,
) -> (String, Vec<Planned>) {
    let mut t = format!(
        "# bench-compare-lib plan\nlib {}\ncorpus {}\n",
        lib.id,
        corpus.dir.display()
    );
    if let Some(m) = mmdb {
        t += &format!("mmdb {m}\n");
    }
    let mut planned = Vec::new();
    if edge {
        t += "edge\nvalidate validate 0\n";
        return (t, planned);
    }
    t += &format!(
        "target_ms {}\nwarmup {}\nprewarm_ms {}\n",
        s.target_ms, s.warmup, s.prewarm_ms
    );
    let detail = s.detail_sizes.contains(&corpus.size);
    let scenarios: Vec<&str> = if detail {
        corpus::SCENARIOS.to_vec()
    } else {
        vec!["mixed"]
    };
    let limit = if rep == 0 {
        s.validate_first
    } else {
        s.validate_rest
    };
    t += "validate validate 0\n";
    for sc in &scenarios {
        t += &format!("validate {sc} {limit}\n");
    }
    let ops: Vec<&str> = if lib.supports_lookup() {
        vec!["contains", "lookup"]
    } else {
        vec!["contains"]
    };
    for sc in &scenarios {
        // Multithreaded runs on the mixed scenario, after the 1-thread run
        // that calibrates the volume.
        let threads: &[usize] = if *sc == "mixed" { &s.threads } else { &[1] };
        for op in &ops {
            for &th in threads {
                t += &format!("throughput {op} {sc} {th}\n");
                planned.push(("throughput".into(), op.to_string(), sc.to_string(), th));
            }
        }
    }
    for sc in &scenarios {
        for op in &ops {
            t += &format!("latency {op} {sc} {}\n", s.latency_samples);
            planned.push(("latency".into(), op.to_string(), sc.to_string(), 1));
        }
    }
    (t, planned)
}

fn proc_rss(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| {
                    l.strip_prefix("VmRSS:")
                        .map(|v| v.trim().trim_end_matches(" kB").trim().parse::<u64>().ok())
                })
                .flatten()
        })
        .map_or(0, |k| k * 1024)
}

struct Launch<'a> {
    lib: &'static LibDef,
    corpus: &'a CorpusInfo,
    mmdb: Option<&'a str>,
    rep: usize,
    edge: Option<&'a str>,
    order: usize,
}

fn launch(l: &Launch, s: &Settings, deps: &Deps, run_dir: &Path, mem_limit: u64) -> Proc {
    let kind = l.edge.map_or("main".to_string(), |c| format!("edge:{c}"));
    let name = format!(
        "{}-{}-{}-{}-r{}",
        l.lib.id,
        l.corpus.family.name(),
        l.corpus.size,
        kind.replace(':', "-"),
        l.rep
    );
    let (text, planned) = plan_text(l.lib, s, l.corpus, l.mmdb, l.rep, l.edge.is_some());
    let plan = run_dir.join(format!("{name}.plan"));
    let raw = run_dir.join(format!("{name}.jsonl"));
    let err = run_dir.join(format!("{name}.stderr"));
    let mut p = Proc {
        lib: l.lib.id.into(),
        family: l.corpus.family,
        size: l.corpus.size,
        rep: l.rep,
        kind,
        corpus: l.corpus.id.clone(),
        status: "ok".into(),
        reason: None,
        wall_s: 0.0,
        exit: None,
        order: l.order,
        planned,
        raw: raw.display().to_string(),
        records: Vec::new(),
        stderr_tail: String::new(),
        watch_peak_rss: 0,
    };
    let exe: Result<PathBuf, String> = match l.lib.runner {
        RunnerKind::Rust => std::env::current_exe().map_err(|e| e.to_string()),
        RunnerKind::Go => deps.go_runner.clone(),
        RunnerKind::C => deps.c_runner.clone(),
    };
    let exe = match exe {
        Ok(e) => e,
        Err(e) => {
            p.status = "failed".into();
            p.reason = Some(format!("runner unavailable: {e}"));
            return p;
        }
    };
    if std::fs::write(&plan, &text).is_err() {
        p.status = "failed".into();
        p.reason = Some("cannot write the plan".into());
        return p;
    }
    let mut cmd = Command::new(&exe);
    if l.lib.runner == RunnerKind::Rust {
        cmd.arg("runner");
    }
    cmd.arg("--plan").arg(&plan);
    let (Ok(fo), Ok(fe)) = (std::fs::File::create(&raw), std::fs::File::create(&err)) else {
        p.status = "failed".into();
        p.reason = Some("cannot create output files".into());
        return p;
    };
    cmd.stdin(Stdio::null()).stdout(fo).stderr(fe);
    let t0 = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            p.status = "failed".into();
            p.reason = Some(format!("cannot start {}: {e}", exe.display()));
            return p;
        }
    };
    let timeout = Duration::from_secs_f64(s.timeout_s);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {}
            Err(_) => break None,
        }
        let rss = proc_rss(child.id());
        p.watch_peak_rss = p.watch_peak_rss.max(rss);
        if rss > mem_limit {
            let _ = child.kill();
            let _ = child.wait();
            p.status = "failed".into();
            p.reason = Some(format!(
                "memory limit: resident memory {} MiB exceeded the limit of {} MiB",
                rss >> 20,
                mem_limit >> 20
            ));
            break None;
        }
        if t0.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            p.status = "timeout".into();
            p.reason = Some(format!("no result within {:.0} s", s.timeout_s));
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    p.wall_s = t0.elapsed().as_secs_f64();
    p.exit = status.and_then(|s| s.code());
    let stdout = std::fs::read_to_string(&raw).unwrap_or_default();
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            p.records.push(v);
        }
    }
    let stderr = std::fs::read_to_string(&err).unwrap_or_default();
    p.stderr_tail = stderr
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if p.status == "ok" {
        let ended = p.records.iter().any(|r| r["type"] == "end");
        if !ended || status.is_none_or(|s| !s.success()) {
            p.status = "failed".into();
            let msg = p
                .records
                .iter()
                .find(|r| r["type"] == "error")
                .and_then(|r| r["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| {
                    format!(
                        "runner exited with {:?}{}",
                        status.and_then(|s| s.code()),
                        if p.stderr_tail.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", p.stderr_tail.replace('\n', " | "))
                        }
                    )
                });
            p.reason = Some(msg);
        }
    }
    p
}

fn skipped(lib: &'static LibDef, c: &CorpusInfo, rep: usize, s: &Settings, reason: String) -> Proc {
    let (_, planned) = plan_text(lib, s, c, None, rep, false);
    Proc {
        lib: lib.id.into(),
        family: c.family,
        size: c.size,
        rep,
        kind: "main".into(),
        corpus: c.id.clone(),
        status: "skipped".into(),
        reason: Some(reason),
        wall_s: 0.0,
        exit: None,
        order: 0,
        planned,
        raw: String::new(),
        records: Vec::new(),
        stderr_tail: String::new(),
        watch_peak_rss: 0,
    }
}

fn progress(msg: &str) {
    eprintln!("[bench-compare-lib] {msg}");
}

pub fn run(s: Settings) {
    let repo = repo_root();
    let started = Instant::now();
    let started_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let run_id = format!("{started_unix}-{}", s.profile);
    let load_start = sys::loadavg();
    for n in &s.notes {
        progress(n);
    }
    let selected: Vec<&'static LibDef> = LIBS
        .iter()
        .filter(|l| s.libs.is_empty() || s.libs.iter().any(|x| x == l.id))
        .collect();
    if selected.is_empty() {
        super::settings::die("BENCH_LIBS selects no library");
    }

    // Dependencies.
    progress("preparing dependencies (Go toolchain and modules, libmaxminddb, runners)");
    let need_go = selected.iter().any(|l| l.runner == RunnerKind::Go);
    let need_c = selected.iter().any(|l| l.runner == RunnerKind::C);
    let deps = deps::prepare(&repo, &s.cache, &s.goamd64, need_go, need_c);
    for e in &deps.report.errors {
        progress(&format!("dependency error: {e}"));
    }

    // Corpora.
    let t_corpus = Instant::now();
    let croot = s.cache.join("corpus");
    let mut corpora: Vec<CorpusInfo> = Vec::new();
    for &size in &s.sizes {
        for &family in &s.families {
            progress(&format!("corpus {} {size}", family.label()));
            match corpus::ensure(&croot, family, size, s.seed, s.queries) {
                Ok(c) => corpora.push(c),
                Err(e) => progress(&format!("corpus {} {size} failed: {e}", family.label())),
            }
        }
    }
    let mut edges: Vec<CorpusInfo> = Vec::new();
    for &family in &s.families {
        for case in corpus::EDGE_CASES {
            match corpus::ensure_edge(&croot, family, case) {
                Ok(c) => edges.push(c),
                Err(e) => progress(&format!("edge corpus {case} failed: {e}")),
            }
        }
    }
    let corpus_s = t_corpus.elapsed().as_secs_f64();

    // MMDB files (preparation of libmaxminddb, not a competitor).
    let t_mmdb = Instant::now();
    let mut mmdbs: Vec<MmdbInfo> = Vec::new();
    if need_c {
        for c in edges.iter().chain(&corpora) {
            match &deps.mmdbgen {
                Ok(g) => {
                    if c.kind == "main" {
                        progress(&format!("MMDB {} {}", c.family.label(), c.size));
                    }
                    mmdbs.push(mmdb_for(&s.cache, g, c));
                }
                Err(e) => mmdbs.push(MmdbInfo {
                    corpus: c.id.clone(),
                    family: c.family,
                    size: c.size,
                    path: String::new(),
                    status: "failed".into(),
                    reason: Some(format!("MMDB writer unavailable: {e}")),
                    cached: false,
                    file_bytes: 0,
                    fabricate_s: 0.0,
                    writer: Value::Null,
                }),
            }
        }
    }
    let mmdb_s = t_mmdb.elapsed().as_secs_f64();
    let mmdb_of = |c: &CorpusInfo| mmdbs.iter().find(|m| m.corpus == c.id);

    let run_dir = s.cache.join("runs").join(&run_id);
    let _ = std::fs::create_dir_all(&run_dir);
    let avail = sys::meminfo("MemAvailable:").unwrap_or(8 << 30) as u64;
    let mem_limit = s.mem_limit_mib.map_or(avail / 10 * 8, |m| m << 20);
    let mut procs: Vec<Proc> = Vec::new();

    // Edge validation (dedicated corpora: nested, /0, duplicates).
    let t_measure = Instant::now();
    for c in &edges {
        for &lib in &selected {
            let mm = if lib.runner == RunnerKind::C {
                mmdb_of(c)
            } else {
                None
            };
            if let Some(m) = mm
                && m.status != "ok"
            {
                let mut p = skipped(lib, c, 0, &s, m.reason.clone().unwrap_or_default());
                p.kind = c.kind.clone();
                p.status = "failed".into();
                procs.push(p);
                continue;
            }
            let l = Launch {
                lib,
                corpus: c,
                mmdb: mm.map(|m| m.path.as_str()),
                rep: 0,
                edge: c.kind.strip_prefix("edge:"),
                order: 0,
            };
            procs.push(launch(&l, &s, &deps, &run_dir, mem_limit));
        }
    }
    progress(&format!("edge validation done ({} processes)", procs.len()));

    // Measurements: rounds of repetitions, rotating order.
    let budget = (s.budget_s > 0.0).then(|| Duration::from_secs_f64(s.budget_s));
    let mut dead: HashMap<(String, Family), (usize, String)> = HashMap::new();
    let mut peak: HashMap<(String, Family, usize), u64> = HashMap::new();
    let mut order = 0usize;
    let total = s.repetitions * corpora.len() * selected.len();
    for rep in 0..s.repetitions {
        for (ci, c) in corpora.iter().enumerate() {
            let k = selected.len();
            for j in 0..k {
                let lib = selected[(j + rep + ci) % k];
                order += 1;
                let key = (lib.id.to_string(), c.family);
                if let Some(b) = budget
                    && t_measure.elapsed() > b
                {
                    procs.push(skipped(
                        lib,
                        c,
                        rep,
                        &s,
                        format!("measurement budget of {:.0} s exhausted", s.budget_s),
                    ));
                    continue;
                }
                if let Some((size, why)) = dead.get(&key)
                    && (*size < c.size || (*size == c.size && rep > 0))
                {
                    procs.push(skipped(
                        lib,
                        c,
                        rep,
                        &s,
                        format!("not run: {why} at {size} prefixes"),
                    ));
                    continue;
                }
                // Memory estimate from the previous size of this library.
                let prev = peak
                    .iter()
                    .filter(|((l, f, n), _)| l == lib.id && *f == c.family && *n < c.size)
                    .max_by_key(|((_, _, n), _)| *n)
                    .map(|((_, _, n), &b)| (*n, b));
                let now_avail = sys::meminfo("MemAvailable:").unwrap_or(avail as usize) as u64;
                if let Some((n, b)) = prev {
                    let est = (b as f64 * c.size as f64 / n as f64) as u64;
                    if est > now_avail / 10 * 9 || est > mem_limit {
                        procs.push(skipped(lib, c, rep, &s, format!(
                            "estimated memory {} MiB (peak {} MiB at {n} prefixes, scaled) exceeds the available memory ({} MiB) or the limit ({} MiB)",
                            est >> 20, b >> 20, now_avail >> 20, mem_limit >> 20)));
                        continue;
                    }
                }
                let mm = if lib.runner == RunnerKind::C {
                    mmdb_of(c)
                } else {
                    None
                };
                if let Some(m) = mm
                    && m.status != "ok"
                {
                    let mut p = skipped(lib, c, rep, &s, m.reason.clone().unwrap_or_default());
                    p.status = "failed".into();
                    procs.push(p);
                    continue;
                }
                progress(&format!(
                    "[{order}/{total}] rep {} {} {} {}",
                    rep + 1,
                    c.family.label(),
                    c.size,
                    lib.id
                ));
                let l = Launch {
                    lib,
                    corpus: c,
                    mmdb: mm.map(|m| m.path.as_str()),
                    rep,
                    edge: None,
                    order,
                };
                let p = launch(&l, &s, &deps, &run_dir, mem_limit);
                if p.status != "ok" {
                    progress(&format!(
                        "  {}: {}",
                        p.status,
                        p.reason.as_deref().unwrap_or("")
                    ));
                    if p.status == "timeout"
                        || p.reason
                            .as_deref()
                            .is_some_and(|r| r.starts_with("memory limit"))
                    {
                        dead.insert(key, (c.size, p.status.clone()));
                    }
                }
                let hwm = p
                    .records
                    .iter()
                    .find(|r| r["type"] == "end")
                    .and_then(|r| r["hwm"].as_u64())
                    .unwrap_or(p.watch_peak_rss);
                let e = peak.entry((lib.id.into(), c.family, c.size)).or_default();
                *e = (*e).max(hwm);
                procs.push(p);
            }
        }
    }
    let measure_s = t_measure.elapsed().as_secs_f64();

    let rust_build_s = std::env::var("BENCH_RUST_BUILD_MS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|ms| ms / 1e3);
    let stages = json!({"dependencies": deps.report.stages, "rust_build_s": rust_build_s, "corpus_s": corpus_s, "mmdb_s": mmdb_s, "measure_s": measure_s,
        "total_s": started.elapsed().as_secs_f64(),
        "corpus_cached": corpora.iter().all(|c| c.cached), "mmdb_cached": mmdbs.iter().all(|m| m.cached)});
    let meta = json!({"run_id": run_id, "profile": s.profile, "started_unix": started_unix,
        "duration_s": started.elapsed().as_secs_f64(), "project": project_info(&repo), "settings": s,
        "env": env_info(&repo, &s, load_start), "deps": deps.report, "stages": stages, "mem_limit_bytes": mem_limit,
        "raw_dir": run_dir.display().to_string()});
    let mut doc = results::build(meta, &selected, &corpora, &edges, &mmdbs, &procs);
    results::finish(&mut doc);
    let files = results::write_all(&s.out, &doc)
        .unwrap_or_else(|e| super::settings::die(&format!("cannot write the report: {e}")));
    let _ = std::fs::write(
        run_dir.join("processes.json"),
        serde_json::to_vec_pretty(&procs).unwrap_or_default(),
    );
    progress(&format!(
        "status {}: {} (total {:.0} s)",
        doc["meta"]["status"].as_str().unwrap_or("?"),
        doc["meta"]["status_reason"].as_str().unwrap_or(""),
        started.elapsed().as_secs_f64()
    ));
    for f in files {
        progress(&format!("wrote {}", f.display()));
    }
    refresh_readme(&s, &doc);
}

/// Regenerates the summary block of `README.md` when `BENCH_README` allows
/// it; a failure is reported but does not fail the run.
fn refresh_readme(s: &Settings, doc: &Value) {
    let path = &s.readme_file;
    if !super::readme::wanted(&s.readme, doc) {
        progress(&format!(
            "{} not updated (BENCH_README={}, profile {}, status {})",
            path.display(),
            s.readme,
            doc["meta"]["profile"].as_str().unwrap_or("?"),
            doc["meta"]["status"].as_str().unwrap_or("?")
        ));
        return;
    }
    match super::readme::update(path, doc) {
        Ok(true) => progress(&format!("updated the summary of {}", path.display())),
        Ok(false) => progress(&format!("summary of {} already up to date", path.display())),
        Err(e) => progress(&format!("warning: README summary not updated: {e}")),
    }
}

/// `BENCH_REPORT_ONLY=1`: rebuild the HTML and CSV from the JSON results.
pub fn report_only(s: &Settings) {
    let path = s.out.join("bench-compare-lib.json");
    let doc: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| super::settings::die(&format!("cannot read {}", path.display())));
    match results::write_all(&s.out, &doc) {
        Ok(files) => {
            for f in files {
                progress(&format!("wrote {}", f.display()));
            }
        }
        Err(e) => super::settings::die(&format!("cannot write the report: {e}")),
    }
    refresh_readme(s, &doc);
}

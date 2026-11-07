//! Command line and environment settings of `bench-compare`.
//!
//! Every option has a `--flag` and a `BENCH_*` environment variable (the
//! Makefile forwards its variables through the environment). Lists are
//! comma separated.

use std::path::PathBuf;

use crate::bench::Op;
use crate::dataset::{Dist, Family};

/// Memory limit per measuring process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemLimit {
    /// No limit (default): every size is attempted.
    None,
    /// 70 % of `MemAvailable` when the run starts.
    Auto,
    /// A fixed limit in MiB.
    Mib(usize),
}

/// `0`, `none`, `off` -> no limit; `auto` -> 70 % of `MemAvailable`;
/// a number -> MiB.
pub fn parse_mem_limit(s: &str) -> Option<MemLimit> {
    match s.trim() {
        "" | "0" | "none" | "off" | "no" => Some(MemLimit::None),
        "auto" => Some(MemLimit::Auto),
        v => v.parse().ok().map(MemLimit::Mib),
    }
}

pub const DEFAULT_SIZES: &str = "1k,10k,50k,100k,250k,500k,1M,2M,5M";

#[derive(Clone, Debug)]
pub struct Options {
    pub sizes: Vec<usize>,
    pub families: Vec<Family>,
    pub ops: Vec<Op>,
    pub dists: Vec<Dist>,
    pub threads: Vec<usize>,
    pub batch: Vec<usize>,
    /// Extra batch sizes for structures with a native batch API (dir-24-8),
    /// measured with the first thread count only (batch-size chart).
    pub batch_sweep: Vec<usize>,
    pub engines: Vec<String>,
    pub hit_permille: u32,
    pub queries: usize,
    pub seed: u64,
    pub warmup_ms: u64,
    pub window_ms: u64,
    pub reps: usize,
    pub lat_samples: usize,
    pub lat_batches: usize,
    pub lat_max_ms: u64,
    /// Queries per distribution checked against the oracle before timing
    /// (0 = all).
    pub validate: usize,
    pub mem_limit: MemLimit,
    pub budget_s: u64,
    pub timeout_s: u64,
    pub pin: bool,
    pub out: PathBuf,
    pub cache: PathBuf,
    /// `BENCH_README` (`README=` in the Makefile): `auto`, `1` or `0`, see
    /// [`crate::readme::wanted`].
    pub readme: String,
    /// The repository `README.md` whose "Detailed results" block is regenerated.
    pub readme_file: PathBuf,
    pub report_only: Option<PathBuf>,
    pub list: bool,
    /// Internal: measure one structure (JSON `RunCfg`).
    pub child: Option<String>,
    /// Internal: generate one dataset (JSON `PrepareSpec`).
    pub prepare: Option<String>,
    /// Internal: where a child writes its result.
    pub result: Option<PathBuf>,
}

const USAGE: &str = "\
usage: bench-compare [options]

Matrix (comma-separated lists):
  --sizes LIST        entries per table          (BENCH_SIZES, default 1k,10k,50k,100k,250k,500k,1M,2M,5M)
  --families LIST     ipv4,ipv6                  (BENCH_FAMILIES)
  --ops LIST          lookup,batch,exists        (BENCH_OPS)
  --dists LIST        random,hits,misses         (BENCH_DISTS)
  --threads LIST      worker threads             (BENCH_THREADS, default 1,4,8)
  --batch LIST        addresses per batch call   (BENCH_BATCH, default 256)
  --batch-sweep LIST  extra batch sizes for native batch APIs (dir-24-8, SIMD and scalar), first
                      thread count only; `none` disables (BENCH_BATCH_SWEEP, default
                      1,4,8,16,32,64,128,512,1024)
  --engines LIST      default | all | name substrings, combinable (BENCH_ENGINES, default: default)
  --hit-ratio R       hits in the random distribution: 50, 50% or 0.5 (BENCH_HIT_RATIO, default 50%)
  --queries N         queries per distribution   (BENCH_QUERIES, default 1048576: larger than the caches)
  --validate N        queries per distribution checked against the oracle before timing, 0 = all
                      (BENCH_VALIDATE, default 131072)
  --seed N            data seed                  (BENCH_SEED)

Measurement:
  --warmup-ms N       warm-up / calibration per operation and distribution (BENCH_WARMUP_MS, default 20)
  --window-ms N       target duration of one timed repetition (BENCH_WINDOW_MS, default 10)
  --reps N            timed repetitions per configuration (BENCH_REPS, default 5)
  --lat-samples N     latency samples per single-address configuration (BENCH_LAT_SAMPLES, default 20000)
  --lat-batches N     latency samples (batch calls) per batch configuration (BENCH_LAT_BATCHES, default 2000)
  --lat-max-ms N      cap of one latency pass for slow structures, never below 2000 samples / 200 batch
                      calls (BENCH_LAT_MAX_MS, default 100)
  --no-pin            do not pin worker threads to cores (BENCH_PIN=0)

Limits:
  --mem-limit-mib N   memory limit per structure in MiB, `auto` = 70% of MemAvailable, 0 = none
                      (BENCH_MEM_LIMIT_MIB, default none: every size is attempted)
  --budget S          total time budget in seconds, 0 = none (BENCH_BUDGET, default 3600)
  --timeout S         limit per measuring process in seconds (BENCH_TIMEOUT, default 600)

Output:
  --out DIR           reports directory (BENCH_OUT, default benchmarks/reports)
  --cache DIR         dataset cache (BENCH_CACHE, default target/bench-compare/data)
  --readme MODE       refresh the Detailed results block of README.md: auto (complete runs of the
                      default selection up to 1M entries or more), 1 or 0 (BENCH_README, default auto)
  --report-only [F]   regenerate the reports from F (default OUT/results.json), no measurement
  --list              print the plan and exit
";

pub fn usage() -> ! {
    eprint!("{USAGE}");
    std::process::exit(2)
}

fn fail(msg: String) -> ! {
    eprintln!("bench-compare: {msg}");
    usage()
}

fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

/// `1k` -> 1 000, `2.5M` -> 2 500 000, `500` -> 500.
pub fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim().replace('_', "");
    let (num, mul) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1e3),
        'm' | 'M' => (&s[..s.len() - 1], 1e6),
        _ => (s.as_str(), 1.0),
    };
    let v: f64 = num.parse().ok()?;
    let n = (v * mul).round();
    (n >= 1.0).then_some(n as usize)
}

/// Inverse of [`parse_size`] for display: 1 000 -> `1k`, 10 000 000 -> `10M`.
pub fn size_label(n: usize) -> String {
    if n >= 1_000_000 && n.is_multiple_of(100_000) {
        let v = n as f64 / 1e6;
        format!("{}M", trim(v))
    } else if n >= 1_000 && n.is_multiple_of(100) {
        format!("{}k", trim(n as f64 / 1e3))
    } else {
        n.to_string()
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

fn parse_ratio(s: &str) -> Option<u32> {
    let s = s.trim();
    let v: f64 = s.trim_end_matches('%').parse().ok()?;
    let pct = if s.ends_with('%') || v > 1.0 {
        v
    } else {
        v * 100.0
    };
    (0.0..=100.0)
        .contains(&pct)
        .then_some((pct * 10.0).round() as u32)
}

fn parse_list<T>(s: &str, what: &str, f: impl Fn(&str) -> Option<T>) -> Vec<T> {
    let v: Vec<T> = list(s)
        .iter()
        .map(|x| f(x).unwrap_or_else(|| fail(format!("invalid {what}: {x}"))))
        .collect();
    if v.is_empty() {
        fail(format!("empty {what} list"));
    }
    v
}

fn root() -> PathBuf {
    let p = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    p.canonicalize().unwrap_or(p)
}

pub fn parse() -> Options {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let num = |k: &str, d: u64| -> u64 {
        env(k).map_or(d, |v| {
            v.trim()
                .parse()
                .unwrap_or_else(|_| fail(format!("invalid {k}: {v}")))
        })
    };
    let mut o = Options {
        sizes: Vec::new(),
        families: Vec::new(),
        ops: Vec::new(),
        dists: Vec::new(),
        threads: Vec::new(),
        batch: Vec::new(),
        batch_sweep: Vec::new(),
        engines: env("BENCH_ENGINES").map(|s| list(&s)).unwrap_or_default(),
        hit_permille: 500,
        queries: num("BENCH_QUERIES", 1 << 20) as usize,
        seed: num("BENCH_SEED", 0x5EED),
        warmup_ms: num("BENCH_WARMUP_MS", 20),
        window_ms: num("BENCH_WINDOW_MS", 10),
        reps: num("BENCH_REPS", 5) as usize,
        lat_samples: num("BENCH_LAT_SAMPLES", 20_000) as usize,
        lat_batches: num("BENCH_LAT_BATCHES", 2_000) as usize,
        lat_max_ms: num("BENCH_LAT_MAX_MS", 100),
        validate: num("BENCH_VALIDATE", 131_072) as usize,
        mem_limit: env("BENCH_MEM_LIMIT_MIB").map_or(MemLimit::None, |v| {
            parse_mem_limit(&v).unwrap_or_else(|| fail(format!("invalid BENCH_MEM_LIMIT_MIB: {v}")))
        }),
        budget_s: num("BENCH_BUDGET", 3600),
        timeout_s: num("BENCH_TIMEOUT", 600),
        pin: env("BENCH_PIN").is_none_or(|v| !matches!(v.trim(), "0" | "false" | "no" | "off")),
        out: env("BENCH_OUT").map_or_else(|| root().join("benchmarks/reports"), PathBuf::from),
        readme: env("BENCH_README").unwrap_or_else(|| "auto".into()),
        readme_file: root().join("README.md"),
        cache: env("BENCH_CACHE")
            .map_or_else(|| root().join("target/bench-compare/data"), PathBuf::from),
        report_only: None,
        list: false,
        child: None,
        prepare: None,
        result: None,
    };
    let mut sizes = env("BENCH_SIZES").unwrap_or_else(|| DEFAULT_SIZES.into());
    let mut families = env("BENCH_FAMILIES").unwrap_or_else(|| "ipv4,ipv6".into());
    let mut ops = env("BENCH_OPS").unwrap_or_else(|| "lookup,batch,exists".into());
    let mut dists = env("BENCH_DISTS").unwrap_or_else(|| "random,hits,misses".into());
    let mut threads = env("BENCH_THREADS").unwrap_or_else(|| "1,4,8".into());
    let mut batch = env("BENCH_BATCH").unwrap_or_else(|| "256".into());
    let mut batch_sweep =
        env("BENCH_BATCH_SWEEP").unwrap_or_else(|| "1,4,8,16,32,64,128,512,1024".into());
    let mut ratio = env("BENCH_HIT_RATIO").unwrap_or_else(|| "50%".into());
    let mut report_only = env("BENCH_REPORT_ONLY").filter(|v| !matches!(v.trim(), "0" | "no"));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = || {
            i += 1;
            args.get(i)
                .cloned()
                .unwrap_or_else(|| fail(format!("{a} needs a value")))
        };
        match a {
            "--sizes" => sizes = val(),
            "--families" => families = val(),
            "--ops" => ops = val(),
            "--dists" => dists = val(),
            "--threads" => threads = val(),
            "--batch" => batch = val(),
            "--batch-sweep" => batch_sweep = val(),
            "--engines" => o.engines = list(&val()),
            "--hit-ratio" => ratio = val(),
            "--queries" => o.queries = val().parse().unwrap_or_else(|_| usage()),
            "--seed" => o.seed = val().parse().unwrap_or_else(|_| usage()),
            "--warmup-ms" => o.warmup_ms = val().parse().unwrap_or_else(|_| usage()),
            "--window-ms" => o.window_ms = val().parse().unwrap_or_else(|_| usage()),
            "--reps" => o.reps = val().parse().unwrap_or_else(|_| usage()),
            "--lat-samples" => o.lat_samples = val().parse().unwrap_or_else(|_| usage()),
            "--lat-batches" => o.lat_batches = val().parse().unwrap_or_else(|_| usage()),
            "--lat-max-ms" => o.lat_max_ms = val().parse().unwrap_or_else(|_| usage()),
            "--validate" => o.validate = val().parse().unwrap_or_else(|_| usage()),
            "--mem-limit-mib" => o.mem_limit = parse_mem_limit(&val()).unwrap_or_else(|| usage()),
            "--budget" => o.budget_s = val().parse().unwrap_or_else(|_| usage()),
            "--timeout" => o.timeout_s = val().parse().unwrap_or_else(|_| usage()),
            "--no-pin" => o.pin = false,
            "--out" => o.out = PathBuf::from(val()),
            "--cache" => o.cache = PathBuf::from(val()),
            "--report-only" => {
                report_only = Some(match args.get(i + 1) {
                    Some(f) if !f.starts_with("--") => {
                        i += 1;
                        f.clone()
                    }
                    _ => "1".into(),
                })
            }
            "--list" => o.list = true,
            "--readme" => o.readme = val(),
            "--child" => o.child = Some(val()),
            "--prepare" => o.prepare = Some(val()),
            "--result" => o.result = Some(PathBuf::from(val())),
            "-h" | "--help" => usage(),
            // `cargo bench`/`cargo run` may forward this one.
            "--bench" => {}
            _ => fail(format!("unknown argument {a}")),
        }
        i += 1;
    }
    o.report_only = report_only.map(|v| {
        if matches!(v.trim(), "1" | "yes" | "true") {
            o.out.join("results.json")
        } else {
            PathBuf::from(v)
        }
    });
    o.readme = o.readme.trim().to_string();
    if !matches!(o.readme.as_str(), "auto" | "1" | "0") {
        fail(format!(
            "--readme / BENCH_README must be auto, 1 or 0, not {:?}",
            o.readme
        ));
    }
    o.sizes = parse_list(&sizes, "size", parse_size);
    o.sizes.sort_unstable();
    o.sizes.dedup();
    o.families = parse_list(&families, "family", Family::parse);
    o.ops = parse_list(&ops, "operation", Op::parse);
    o.dists = parse_list(&dists, "distribution", Dist::parse);
    o.threads = parse_list(&threads, "thread count", |s| {
        s.parse().ok().filter(|&n: &usize| n > 0)
    });
    o.threads.sort_unstable();
    o.threads.dedup();
    o.batch = parse_list(&batch, "batch size", |s| {
        s.parse().ok().filter(|&n: &usize| n > 0)
    });
    if !matches!(batch_sweep.trim(), "" | "none" | "0") {
        o.batch_sweep = parse_list(&batch_sweep, "batch size", |s| {
            s.parse().ok().filter(|&n: &usize| n > 0)
        });
        o.batch_sweep.sort_unstable();
        o.batch_sweep.dedup();
        o.batch_sweep.retain(|b| !o.batch.contains(b));
    }
    o.hit_permille =
        parse_ratio(&ratio).unwrap_or_else(|| fail(format!("invalid hit ratio {ratio}")));
    if o.batch.iter().chain(&o.batch_sweep).any(|&b| b > o.queries) {
        fail(format!(
            "batch sizes must not exceed --queries ({})",
            o.queries
        ));
    }
    if o.queries == 0 || o.reps == 0 {
        fail("--queries and --reps must be positive".into());
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1k"), Some(1_000));
        assert_eq!(parse_size("10M"), Some(10_000_000));
        assert_eq!(parse_size("2.5k"), Some(2_500));
        assert_eq!(parse_size("1_000"), Some(1_000));
        assert_eq!(parse_size("x"), None);
        for s in DEFAULT_SIZES.split(',') {
            assert_eq!(size_label(parse_size(s).unwrap()), s);
        }
        assert_eq!(size_label(1234), "1234");
    }

    #[test]
    fn mem_limits() {
        assert_eq!(parse_mem_limit("0"), Some(MemLimit::None));
        assert_eq!(parse_mem_limit("none"), Some(MemLimit::None));
        assert_eq!(parse_mem_limit("auto"), Some(MemLimit::Auto));
        assert_eq!(parse_mem_limit("4096"), Some(MemLimit::Mib(4096)));
        assert_eq!(parse_mem_limit("x"), None);
    }

    #[test]
    fn ratios() {
        assert_eq!(parse_ratio("50%"), Some(500));
        assert_eq!(parse_ratio("50"), Some(500));
        assert_eq!(parse_ratio("0.25"), Some(250));
        assert_eq!(parse_ratio("1"), Some(1000));
        assert_eq!(parse_ratio("150"), None);
    }
}

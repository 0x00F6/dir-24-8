//! Profiles and options of `bench-compare-lib` (every option is a `BENCH_*`
//! environment variable, forwarded by `make bench-compare-lib`).

use std::path::PathBuf;

use serde::Serialize;

use crate::dataset::Family;

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    pub profile: String,
    pub sizes: Vec<usize>,
    pub families: Vec<Family>,
    pub seed: u64,
    pub repetitions: usize,
    /// Worker threads of the throughput measurements (always with 1); the
    /// multithreaded runs use the mixed scenario.
    pub threads: Vec<usize>,
    /// Queries per scenario file (power of two).
    pub queries: usize,
    /// Target duration of one throughput measurement.
    pub target_ms: u64,
    /// Lookups of the untimed warm-up / calibration run of a measurement.
    pub warmup: usize,
    /// Warm-up after the validation: one pass over each measured scenario
    /// per operation, stopped after this many milliseconds.
    pub prewarm_ms: u64,
    pub latency_samples: usize,
    /// Sizes measured on every scenario (the others: mixed only).
    pub detail_sizes: Vec<usize>,
    /// Queries validated per scenario in the first repetition (0: all) and
    /// in the others; the boundary file is always validated whole, and the
    /// checksum of every timed loop is compared with the oracle's.
    pub validate_first: usize,
    pub validate_rest: usize,
    pub budget_s: f64,
    pub timeout_s: f64,
    pub mem_limit_mib: Option<u64>,
    pub libs: Vec<String>,
    pub goamd64: String,
    pub out: PathBuf,
    pub cache: PathBuf,
    pub report_only: bool,
    /// `BENCH_README`: `auto`, `1` or `0` (see [`super::readme::wanted`]).
    pub readme: String,
    /// The repository `README.md` whose summary block is regenerated.
    #[serde(skip)]
    pub readme_file: PathBuf,
    pub notes: Vec<String>,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn parse_size(s: &str) -> Result<usize, String> {
    let t = s.trim().to_ascii_lowercase().replace('_', "");
    let (d, m) = if let Some(d) = t.strip_suffix('m') {
        (d.to_string(), 1_000_000)
    } else if let Some(d) = t.strip_suffix('k') {
        (d.to_string(), 1_000)
    } else {
        (t.clone(), 1)
    };
    d.parse::<usize>()
        .map(|v| v * m)
        .map_err(|_| format!("invalid size {s:?}"))
}

fn list<T>(k: &str, f: impl Fn(&str) -> Result<T, String>) -> Option<Vec<T>> {
    var(k).map(|v| {
        v.split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| f(s).unwrap_or_else(|e| die(&format!("{k}: {e}"))))
            .collect()
    })
}

fn num<T: std::str::FromStr>(k: &str) -> Option<T> {
    var(k).map(|v| {
        v.parse()
            .unwrap_or_else(|_| die(&format!("{k}: invalid value {v:?}")))
    })
}

pub fn die(msg: &str) -> ! {
    eprintln!("bench-compare-lib: {msg}");
    std::process::exit(2)
}

pub const USAGE: &str = "\
bench-compare-lib: IPv4/IPv6 lookup comparison (1 to 8 threads) of dir-24-8 (scalar single
lookups), prefix-trie, iprange (Rust),
go-cidranger (libp2p), cidranger (yl2chen), go-iptrie, bart (Go) and libmaxminddb (C).
Run through `make bench-compare-lib`; every option is an environment variable:

  BENCH_PROFILE=smoke|standard|full   (default standard)
  BENCH_SIZES=1000,10000,100000,1000000  input prefixes per table (k/M suffixes accepted)
  BENCH_FAMILIES=ipv4,ipv6             BENCH_SEED=42
  BENCH_REPETITIONS=2                  independent processes per (library, family, size)
  BENCH_QUERIES=1048576                queries per scenario file (power of two)
  BENCH_TARGET_MS=40                   duration of one throughput measurement
  BENCH_LATENCY_SAMPLES=50000          individually timed lookups per latency measurement (p99.9 from 100000)
  BENCH_DETAIL_SIZES=100000            sizes measured on every scenario (hits, misses, mixed, local)
  BENCH_LIBS=<ids>                     subset of libraries (default: all)
  BENCH_BUDGET=900                     measurement budget in seconds (0: none)
  BENCH_TIMEOUT=300                    limit per runner process in seconds
  BENCH_MEM_LIMIT_MIB=<n>              RSS limit per runner process (default: 80 % of MemAvailable)
  BENCH_GOAMD64=v1                     GOAMD64 of the Go build (same for every Go library)
  BENCH_OUT=benchmarks/reports         report directory
  BENCH_CACHE=target/bench-compare-lib corpora, MMDB files, dependencies, raw runner outputs
  BENCH_REPORT_ONLY=1                  rebuild the HTML/CSV from bench-compare-lib.json
  BENCH_README=auto|1|0                refresh the summary block of README.md (auto: complete runs,
                                       not the smoke profile; also with BENCH_REPORT_ONLY=1)
  BENCH_THREADS=1,4,8                  worker threads of the throughput measurements (mixed scenario;
                                       latency and memory stay single-thread)
";

impl Settings {
    pub fn from_env(repo: &std::path::Path) -> Settings {
        let profile = var("BENCH_PROFILE").unwrap_or_else(|| "standard".into());
        let mut s = match profile.as_str() {
            "smoke" => Settings {
                profile: profile.clone(),
                sizes: vec![1_000, 10_000],
                families: Family::ALL.to_vec(),
                seed: 42,
                repetitions: 1,
                threads: vec![1, 4],
                queries: 1 << 14,
                target_ms: 10,
                warmup: 1 << 12,
                prewarm_ms: 20,
                latency_samples: 4_096,
                detail_sizes: vec![10_000],
                validate_first: 0,
                validate_rest: 4_096,
                budget_s: 300.0,
                timeout_s: 120.0,
                mem_limit_mib: None,
                libs: Vec::new(),
                goamd64: "v1".into(),
                out: repo.join("benchmarks/reports"),
                cache: repo.join("target/bench-compare-lib"),
                report_only: false,
                readme: "auto".into(),
                readme_file: repo.join("README.md"),
                notes: Vec::new(),
            },
            "standard" => Settings {
                profile: profile.clone(),
                sizes: vec![1_000, 10_000, 100_000, 1_000_000],
                families: Family::ALL.to_vec(),
                seed: 42,
                repetitions: 2,
                threads: vec![1, 4, 8],
                queries: 1 << 20,
                target_ms: 40,
                warmup: 1 << 14,
                prewarm_ms: 250,
                latency_samples: 50_000,
                detail_sizes: vec![100_000],
                validate_first: 65_536,
                validate_rest: 16_384,
                budget_s: 900.0,
                timeout_s: 300.0,
                mem_limit_mib: None,
                libs: Vec::new(),
                goamd64: "v1".into(),
                out: repo.join("benchmarks/reports"),
                cache: repo.join("target/bench-compare-lib"),
                report_only: false,
                readme: "auto".into(),
                readme_file: repo.join("README.md"),
                notes: Vec::new(),
            },
            "full" => Settings {
                profile: profile.clone(),
                sizes: vec![
                    1_000, 10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 5_000_000,
                    10_000_000,
                ],
                families: Family::ALL.to_vec(),
                seed: 42,
                repetitions: 2,
                threads: vec![1, 4, 8],
                queries: 1 << 20,
                target_ms: 150,
                warmup: 1 << 16,
                prewarm_ms: 500,
                latency_samples: 200_000,
                detail_sizes: vec![100_000, 1_000_000],
                validate_first: 262_144,
                validate_rest: 65_536,
                budget_s: 3_600.0,
                timeout_s: 900.0,
                mem_limit_mib: None,
                libs: Vec::new(),
                goamd64: "v1".into(),
                out: repo.join("benchmarks/reports"),
                cache: repo.join("target/bench-compare-lib"),
                report_only: false,
                readme: "auto".into(),
                readme_file: repo.join("README.md"),
                notes: Vec::new(),
            },
            p => die(&format!(
                "unknown BENCH_PROFILE {p:?} (smoke, standard, full)"
            )),
        };
        if let Some(v) = list("BENCH_SIZES", parse_size) {
            s.sizes = v;
        }
        s.sizes.sort_unstable();
        s.sizes.dedup();
        if let Some(v) = list("BENCH_FAMILIES", |f| {
            Family::parse(f.trim()).ok_or(format!("unknown family {f:?}"))
        }) {
            s.families = v;
        }
        if let Some(v) = num("BENCH_SEED") {
            s.seed = v;
        }
        if let Some(v) = num::<usize>("BENCH_REPETITIONS") {
            s.repetitions = v.max(1);
        }
        if let Some(v) = num::<usize>("BENCH_QUERIES") {
            if !v.is_power_of_two() || v < 1024 {
                die("BENCH_QUERIES must be a power of two >= 1024");
            }
            s.queries = v;
        }
        if let Some(v) = num("BENCH_TARGET_MS") {
            s.target_ms = v;
        }
        if let Some(v) = num::<usize>("BENCH_LATENCY_SAMPLES") {
            s.latency_samples = v.max(100);
        }
        if let Some(v) = list("BENCH_DETAIL_SIZES", parse_size) {
            s.detail_sizes = v;
        }
        if let Some(v) = num("BENCH_BUDGET") {
            s.budget_s = v;
        }
        if let Some(v) = num("BENCH_TIMEOUT") {
            s.timeout_s = v;
        }
        s.mem_limit_mib = num("BENCH_MEM_LIMIT_MIB").or(s.mem_limit_mib);
        if let Some(v) = list("BENCH_LIBS", |l| Ok(l.trim().to_string())) {
            s.libs = v;
        }
        if let Some(v) = var("BENCH_GOAMD64") {
            s.goamd64 = v;
        }
        if let Some(v) = var("BENCH_OUT") {
            s.out = repo.join(v);
        }
        if let Some(v) = var("BENCH_CACHE") {
            s.cache = repo.join(v);
        }
        s.report_only = var("BENCH_REPORT_ONLY").is_some_and(|v| v != "0");
        if let Some(v) = var("BENCH_README") {
            if !matches!(v.as_str(), "auto" | "1" | "0") {
                die(&format!("BENCH_README must be auto, 1 or 0, not {v:?}"));
            }
            s.readme = v;
        }
        if let Some(v) = list("BENCH_THREADS", |t| {
            t.trim()
                .parse::<usize>()
                .map_err(|_| format!("invalid thread count {t:?}"))
        }) {
            s.threads = v;
        }
        s.threads.push(1);
        s.threads.retain(|&t| t >= 1);
        s.threads.sort_unstable();
        s.threads.dedup();
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        if s.threads.iter().any(|&t| t > cpus) {
            s.notes.push(format!(
                "thread counts above the {cpus} available CPUs removed"
            ));
            s.threads.retain(|&t| t <= cpus);
        }
        if !s.detail_sizes.iter().any(|d| s.sizes.contains(d)) {
            // Detail at the largest size <= 100k, so that scenarios are always measured.
            let pick = s
                .sizes
                .iter()
                .copied()
                .filter(|&n| n <= 100_000)
                .max()
                .or(s.sizes.first().copied());
            if let Some(p) = pick {
                s.notes.push(format!(
                    "no detail size among the sizes: scenarios measured at {p}"
                ));
                s.detail_sizes = vec![p];
            }
        }
        s
    }
}

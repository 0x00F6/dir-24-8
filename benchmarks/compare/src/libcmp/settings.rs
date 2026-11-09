//! Profiles and options of the libraries part of `bench-compare`: the
//! `LIB_*` variables of `make bench-compare` (environment `BENCH_LIB_*`,
//! flags `--lib-*`, see [`KEYS`]), plus the families, thread counts, output
//! directory and README mode shared with the structures part ([`Shared`]).

use std::path::{Path, PathBuf};

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
    /// `README`: `auto`, `1` or `0` (see [`super::readme::wanted`]).
    pub readme: String,
    /// The repository `README.md` whose summary block is regenerated.
    #[serde(skip)]
    pub readme_file: PathBuf,
    pub notes: Vec<String>,
}

/// The options of this part, without prefix: Makefile variable `LIB_<KEY>`,
/// environment variable `BENCH_LIB_<KEY>`, flag `--lib-<key>` (lower case,
/// `-` for `_`).
pub const KEYS: &[&str] = &[
    "PROFILE",
    "SIZES",
    "REPETITIONS",
    "SEED",
    "QUERIES",
    "TARGET_MS",
    "LATENCY_SAMPLES",
    "DETAIL_SIZES",
    "LIBS",
    "BUDGET",
    "TIMEOUT",
    "MEM_LIMIT_MIB",
    "GOAMD64",
    "CACHE",
];

/// Settings of `bench-compare` that apply to both parts.
pub struct Shared<'a> {
    pub families: &'a [Family],
    /// Thread counts given explicitly (`THREADS`); `None` keeps the profile's.
    pub threads: Option<&'a [usize]>,
    pub out: &'a Path,
    pub readme: &'a str,
    pub readme_file: &'a Path,
    /// `--lib-*` flags as `(BENCH_LIB_<KEY>, value)`: they override the
    /// environment.
    pub vars: &'a [(String, String)],
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

pub fn die(msg: &str) -> ! {
    eprintln!("bench-compare: {msg}");
    std::process::exit(2)
}

impl Settings {
    /// The settings of a run: the `LIB_*` options over the profile's
    /// defaults, and the shared ones.
    pub fn new(repo: &Path, sh: &Shared) -> Settings {
        // `LIB_<KEY>` option: flag, else environment; empty means unset.
        let var = |k: &str| -> Option<String> {
            let name = format!("BENCH_LIB_{k}");
            sh.vars
                .iter()
                .rev()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var(&name).ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let list = |k: &str, f: &dyn Fn(&str) -> Result<usize, String>| -> Option<Vec<usize>> {
            var(k).map(|v| {
                v.split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| f(s).unwrap_or_else(|e| die(&format!("LIB_{k}: {e}"))))
                    .collect()
            })
        };
        fn num<T: std::str::FromStr>(k: &str, v: Option<String>) -> Option<T> {
            v.map(|v| {
                v.parse()
                    .unwrap_or_else(|_| die(&format!("LIB_{k}: invalid value {v:?}")))
            })
        }
        let profile = var("PROFILE").unwrap_or_else(|| "standard".into());
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
                readme: sh.readme.to_string(),
                readme_file: sh.readme_file.to_path_buf(),
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
                readme: sh.readme.to_string(),
                readme_file: sh.readme_file.to_path_buf(),
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
                readme: sh.readme.to_string(),
                readme_file: sh.readme_file.to_path_buf(),
                notes: Vec::new(),
            },
            p => die(&format!(
                "unknown LIB_PROFILE {p:?} (smoke, standard, full)"
            )),
        };
        if let Some(v) = list("SIZES", &parse_size) {
            s.sizes = v;
        }
        s.sizes.sort_unstable();
        s.sizes.dedup();
        s.families = sh.families.to_vec();
        if let Some(v) = num("SEED", var("SEED")) {
            s.seed = v;
        }
        if let Some(v) = num::<usize>("REPETITIONS", var("REPETITIONS")) {
            s.repetitions = v.max(1);
        }
        if let Some(v) = num::<usize>("QUERIES", var("QUERIES")) {
            if !v.is_power_of_two() || v < 1024 {
                die("LIB_QUERIES must be a power of two >= 1024");
            }
            s.queries = v;
        }
        if let Some(v) = num("TARGET_MS", var("TARGET_MS")) {
            s.target_ms = v;
        }
        if let Some(v) = num::<usize>("LATENCY_SAMPLES", var("LATENCY_SAMPLES")) {
            s.latency_samples = v.max(100);
        }
        if let Some(v) = list("DETAIL_SIZES", &parse_size) {
            s.detail_sizes = v;
        }
        if let Some(v) = num("BUDGET", var("BUDGET")) {
            s.budget_s = v;
        }
        if let Some(v) = num("TIMEOUT", var("TIMEOUT")) {
            s.timeout_s = v;
        }
        s.mem_limit_mib = num("MEM_LIMIT_MIB", var("MEM_LIMIT_MIB")).or(s.mem_limit_mib);
        if let Some(v) = var("LIBS") {
            s.libs = v
                .split(',')
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = var("GOAMD64") {
            s.goamd64 = v;
        }
        s.out = sh.out.to_path_buf();
        if let Some(v) = var("CACHE") {
            s.cache = repo.join(v);
        }
        if let Some(t) = sh.threads {
            s.threads = t.to_vec();
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

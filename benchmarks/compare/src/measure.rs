//! Allocation accounting and environment capture.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use serde::{Deserialize, Serialize};

use crate::sys;

/// Global allocator that tracks live and peak heap bytes.
pub struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator and only updates two
// atomic counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: as above.
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: as above.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                let now = CURRENT.fetch_add(new - l.size(), Relaxed) + new - l.size();
                PEAK.fetch_max(now, Relaxed);
            } else {
                CURRENT.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

/// Live heap bytes.
pub fn heap_now() -> usize {
    CURRENT.load(Relaxed)
}

/// Resets the peak to the current value and returns it.
pub fn reset_peak() -> usize {
    let now = CURRENT.load(Relaxed);
    PEAK.store(now, Relaxed);
    now
}

pub fn heap_peak() -> usize {
    PEAK.load(Relaxed)
}

/// Measurement environment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Env {
    pub cpu: String,
    pub arch: String,
    pub logical_cpus: usize,
    pub physical_cores: Option<usize>,
    pub cpu_max_mhz: Option<f64>,
    pub governor: String,
    pub boost: String,
    pub caches: String,
    pub tsc_flags: String,
    pub simd_detected: Vec<String>,
    /// Batch kernel of dir-24-8 in this build on this CPU (`dir-24-8`
    /// entry); `dir-24-8[scalar]` always batches with `scalar`.
    #[serde(alias = "auto_simd_backend")]
    pub batch_kernel: String,
    pub ram_total: Option<usize>,
    pub ram_available_at_start: Option<usize>,
    pub swap_used_at_start: Option<usize>,
    pub os: String,
    pub kernel: String,
    pub thp: String,
    pub rustc: String,
    pub llvm: String,
    pub target: String,
    pub profile: String,
    pub opt_level: String,
    pub rustflags: String,
    pub compile_target_features: String,
    pub dependencies: Vec<(String, String)>,
    pub timestamp_unix: u64,
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn cpuinfo_field(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

fn caches() -> String {
    let mut out = Vec::new();
    for i in 0..8 {
        let base = format!("/sys/devices/system/cpu/cpu0/cache/index{i}");
        let level = read(&format!("{base}/level"));
        if level.is_empty() {
            break;
        }
        let ty = read(&format!("{base}/type"));
        if ty == "Instruction" {
            continue;
        }
        let size = read(&format!("{base}/size"));
        let shared = read(&format!("{base}/shared_cpu_list"));
        out.push(format!("L{level} {size} (cpus {shared})"));
    }
    out.join(", ")
}

pub fn env() -> Env {
    let cpuinfo = read("/proc/cpuinfo");
    let cpu = cpuinfo_field(&cpuinfo, "model name")
        .or_else(|| cpuinfo_field(&cpuinfo, "Model"))
        .unwrap_or_else(|| "unknown".into());
    let flags = cpuinfo_field(&cpuinfo, "flags").unwrap_or_default();
    let tsc_flags = ["constant_tsc", "nonstop_tsc", "rdtscp"]
        .iter()
        .filter(|f| flags.split_whitespace().any(|x| x == **f))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let mut simd = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! f {
            ($($n:tt),*) => {$(
                if std::arch::is_x86_feature_detected!($n) { simd.push($n.to_string()); }
            )*};
        }
        f!(
            "sse4.2",
            "popcnt",
            "avx",
            "avx2",
            "bmi2",
            "avx512f",
            "avx512bw",
            "avx512vl",
            "avx512dq",
            "avx512vbmi"
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            simd.push("neon".into());
        }
        if std::arch::is_aarch64_feature_detected!("sve") {
            simd.push("sve".into());
        }
    }
    let os = read("/etc/os-release")
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| std::env::consts::OS.into());
    let max_mhz = read("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .parse::<f64>()
        .ok()
        .map(|khz| khz / 1000.0);
    let boost = {
        let b = read("/sys/devices/system/cpu/cpufreq/boost");
        if b.is_empty() {
            read("/sys/devices/system/cpu/intel_pstate/no_turbo")
                .parse::<u8>()
                .map(|v| if v == 0 { "on" } else { "off" }.to_string())
                .unwrap_or_else(|_| "unknown".into())
        } else if b == "1" {
            "on".into()
        } else {
            "off".into()
        }
    };
    let swap_used = sys::meminfo("SwapTotal")
        .zip(sys::meminfo("SwapFree"))
        .map(|(t, f)| t.saturating_sub(f));
    let deps = lock_versions(&[
        "ipnet",
        "libc",
        "prefix-trie",
        "ip_network_table-deps-treebitmap",
        "rustc-hash",
        "serde_json",
    ]);
    Env {
        cpu,
        arch: std::env::consts::ARCH.into(),
        logical_cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
        physical_cores: sys::physical_cores(),
        cpu_max_mhz: max_mhz,
        governor: read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        boost,
        caches: caches(),
        tsc_flags,
        simd_detected: simd,
        batch_kernel: crate::adapter::batch_kernel().to_string(),
        ram_total: sys::meminfo("MemTotal"),
        ram_available_at_start: sys::meminfo("MemAvailable"),
        swap_used_at_start: swap_used,
        os,
        kernel: read("/proc/sys/kernel/osrelease"),
        thp: read("/sys/kernel/mm/transparent_hugepage/enabled"),
        rustc: env!("COMPARE_RUSTC_RELEASE").into(),
        llvm: env!("COMPARE_RUSTC_LLVM").into(),
        target: env!("COMPARE_TARGET").into(),
        profile: env!("COMPARE_PROFILE").into(),
        opt_level: env!("COMPARE_OPT_LEVEL").into(),
        rustflags: env!("COMPARE_RUSTFLAGS").into(),
        compile_target_features: env!("COMPARE_TARGET_FEATURES").into(),
        dependencies: deps,
        timestamp_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    }
}

/// Versions of selected packages from the workspace `Cargo.lock`.
fn lock_versions(names: &[&str]) -> Vec<(String, String)> {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .unwrap_or_default();
    let mut out = vec![("dir-24-8".to_string(), dir_version())];
    let mut name = String::new();
    for line in lock.lines() {
        if let Some(n) = line.strip_prefix("name = ") {
            name = n.trim_matches('"').to_string();
        } else if let Some(v) = line.strip_prefix("version = ")
            && names.contains(&name.as_str())
        {
            out.push((name.clone(), v.trim_matches('"').to_string()));
        }
    }
    out
}

fn dir_version() -> String {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
            .unwrap_or_default();
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_default()
}

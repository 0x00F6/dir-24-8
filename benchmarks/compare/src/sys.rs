//! Process probes (Linux): resident memory, peak reset, memory limit, CPU
//! topology and thread affinity. Elsewhere every probe returns `None` /
//! `false` and the report says the value was not measured.

/// Resident set size (`VmRSS`) in bytes.
pub fn rss() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * page_size())
}

/// Peak resident set size (`VmHWM`) since the start or the last
/// [`reset_hwm`], in bytes.
pub fn hwm() -> Option<usize> {
    status_kib("VmHWM:").map(|k| k << 10)
}

fn status_kib(key: &str) -> Option<usize> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Resets `VmHWM` to the current RSS (`echo 5 > /proc/self/clear_refs`,
/// Linux >= 4.0). `false` when unsupported.
pub fn reset_hwm() -> bool {
    std::fs::write("/proc/self/clear_refs", "5").is_ok()
}

/// A `/proc/meminfo` field in bytes (`MemTotal`, `MemAvailable`, ...).
pub fn meminfo(key: &str) -> Option<usize> {
    let s = std::fs::read_to_string("/proc/meminfo").ok()?;
    let v: usize = s
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(v << 10)
}

pub fn page_size() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no memory-safety preconditions.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v > 0 {
            return v as usize;
        }
    }
    4096
}

/// Limits the data segment of this process (`RLIMIT_DATA`: heap and
/// private writable mappings, including `mmap`ed tables). An allocation
/// beyond it fails, which aborts the process: the parent reports
/// "memory limit exceeded" instead of letting the machine swap.
pub fn set_data_limit(bytes: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        let lim = libc::rlimit {
            rlim_cur: bytes as libc::rlim_t,
            rlim_max: libc::RLIM_INFINITY,
        };
        // SAFETY: `lim` is a valid, initialized `rlimit`.
        unsafe { libc::setrlimit(libc::RLIMIT_DATA, &lim) == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = bytes;
        false
    }
}

/// Logical CPUs usable by this process, one per physical core first, then
/// the SMT siblings (so that `n` workers use `n` distinct cores when the
/// machine has them).
pub fn cpu_order() -> Vec<usize> {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    let allowed = allowed_cpus();
    let mut primary = Vec::new();
    let mut siblings = Vec::new();
    for cpu in 0..n.max(allowed.iter().copied().max().map_or(0, |m| m + 1)) {
        if !allowed.is_empty() && !allowed.contains(&cpu) {
            continue;
        }
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
        let first = std::fs::read_to_string(path).ok().and_then(|s| {
            s.trim()
                .split([',', '-'])
                .next()
                .and_then(|x| x.parse::<usize>().ok())
        });
        match first {
            Some(f) if f != cpu => siblings.push(cpu),
            _ => primary.push(cpu),
        }
    }
    // CPU 0 serves most interrupts: use it (and its sibling) last.
    if let Some(p) = primary.iter().position(|&c| c == 0) {
        let c = primary.remove(p);
        primary.push(c);
    }
    primary.extend(siblings);
    primary
}

/// Number of physical cores (distinct `thread_siblings_list` entries).
pub fn physical_cores() -> Option<usize> {
    let order = cpu_order();
    let n = order
        .iter()
        .filter(|&&cpu| {
            let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
            std::fs::read_to_string(path)
                .ok()
                .and_then(|s| {
                    s.trim()
                        .split([',', '-'])
                        .next()
                        .and_then(|x| x.parse::<usize>().ok())
                })
                .is_some_and(|f| f == cpu)
        })
        .count();
    (n > 0).then_some(n)
}

fn allowed_cpus() -> Vec<usize> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `set` is a plain bit set, zero-initialized, and its size is
        // passed to the kernel.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
                return (0..libc::CPU_SETSIZE as usize)
                    .filter(|&c| libc::CPU_ISSET(c, &set))
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Pins the calling thread to `cpu`.
pub fn pin(cpu: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: as in `allowed_cpus`; `cpu < CPU_SETSIZE` is checked.
        unsafe {
            if cpu >= libc::CPU_SETSIZE as usize {
                return false;
            }
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cpu;
        false
    }
}

/// 1-minute load average (`/proc/loadavg`).
pub fn loadavg() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

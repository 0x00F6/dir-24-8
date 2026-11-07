//! Timestamps for latency samples and percentile summaries.
//!
//! On x86-64 a sample is `lfence; rdtsc; lfence` before the operation and
//! `rdtscp; lfence` after it: the operation cannot start before the first
//! stamp nor overlap the next one, so a sample is the latency of one
//! isolated operation. The constant cost of the stamps themselves (measured
//! on an empty operation) is subtracted. The time-stamp counter is invariant
//! on the CPUs this harness targets (`constant_tsc`, `nonstop_tsc`; recorded
//! in the report) and its frequency is calibrated against the monotonic
//! clock. Other architectures use `Instant` (coarser, documented in the
//! report).

use std::hint::black_box;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "x86_64")]
mod imp {
    use std::arch::x86_64::{__rdtscp, _mm_lfence, _rdtsc};

    pub const NAME: &str = "rdtsc/rdtscp + lfence (TSC ticks)";

    #[inline(always)]
    pub fn begin() -> u64 {
        // SAFETY: lfence and rdtsc are baseline x86-64 instructions without
        // memory operands.
        unsafe {
            _mm_lfence();
            let t = _rdtsc();
            _mm_lfence();
            t
        }
    }

    #[inline(always)]
    pub fn end() -> u64 {
        let mut aux = 0u32;
        // SAFETY: `aux` is a valid `u32` destination; rdtscp is available on
        // every x86-64 CPU this harness runs on (checked by `available`).
        unsafe {
            let t = __rdtscp(&mut aux);
            _mm_lfence();
            t
        }
    }

    pub fn available() -> bool {
        std::fs::read_to_string("/proc/cpuinfo")
            .map(|s| s.contains(" rdtscp"))
            .unwrap_or(true)
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod imp {
    use std::sync::OnceLock;
    use std::time::Instant;

    pub const NAME: &str = "Instant::now (monotonic clock, ns)";

    fn origin() -> Instant {
        static O: OnceLock<Instant> = OnceLock::new();
        *O.get_or_init(Instant::now)
    }

    #[inline(always)]
    pub fn begin() -> u64 {
        origin().elapsed().as_nanos() as u64
    }

    #[inline(always)]
    pub fn end() -> u64 {
        origin().elapsed().as_nanos() as u64
    }

    pub fn available() -> bool {
        true
    }
}

pub use imp::{begin, end};

/// A timestamp source usable in generic sampling loops.
pub trait Stamp {
    fn begin(&self) -> u64;
    fn end(&self) -> u64;
    /// Counter width mask (deltas are taken modulo the width).
    fn mask(&self) -> u64 {
        u64::MAX
    }
}

/// Time-stamp counter (or `Instant` off x86-64).
pub struct Tsc;

impl Stamp for Tsc {
    #[inline(always)]
    fn begin(&self) -> u64 {
        begin()
    }
    #[inline(always)]
    fn end(&self) -> u64 {
        end()
    }
}

/// Core cycle counter of the calling thread, read with `rdpmc` (Linux
/// `perf_event`, user-mode cycles). Resolution: one cycle. Cycles are
/// converted to nanoseconds with the effective frequency measured over the
/// same sampling pass (cycles elapsed / TSC time elapsed).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod pmc {
    use std::arch::asm;
    use std::arch::x86_64::_mm_lfence;
    use std::sync::atomic::{Ordering, compiler_fence};

    /// `struct perf_event_attr`, first published layout (`PERF_ATTR_SIZE_VER0`).
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
    }

    const EXCLUDE_KERNEL: u64 = 1 << 5;
    const EXCLUDE_HV: u64 = 1 << 6;

    /// An open cycle counter (closed on drop). Not `Send`: it counts the
    /// thread that opened it.
    pub struct Counter {
        fd: i32,
        page: *mut u8,
        len: usize,
        /// `64 - pmc_width`: sign-extension shift of a raw `rdpmc` value.
        shift: u32,
    }

    impl Counter {
        /// Opens a user-mode cycle counter for the calling thread; `None`
        /// when perf events or user-space `rdpmc` are not permitted.
        pub fn open() -> Option<Counter> {
            let attr = Attr {
                size: std::mem::size_of::<Attr>() as u32,
                flags: EXCLUDE_KERNEL | EXCLUDE_HV,
                ..Attr::default()
            };
            // SAFETY: `attr` is a valid `perf_event_attr` (VER0 layout, size
            // set); pid 0 / cpu -1 = calling thread on any CPU.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_perf_event_open,
                    &attr as *const Attr,
                    0,
                    -1,
                    -1,
                    0,
                )
            } as i32;
            if fd < 0 {
                return None;
            }
            let len = crate::sys::page_size();
            // SAFETY: maps the first (metadata) page of the event, read only.
            let page = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd,
                    0,
                )
            };
            if page == libc::MAP_FAILED {
                // SAFETY: `fd` is an open descriptor owned here.
                unsafe { libc::close(fd) };
                return None;
            }
            let page = page as *mut u8;
            // SAFETY: the mapping is at least one page; `perf_event_mmap_page`
            // has `index: u32` at offset 12, `capabilities: u64` at 40 and
            // `pmc_width: u16` at 48. Volatile reads: the kernel updates them.
            let (index, caps, width) = unsafe {
                (
                    std::ptr::read_volatile(page.add(12) as *const u32),
                    std::ptr::read_volatile(page.add(40) as *const u64),
                    std::ptr::read_volatile(page.add(48) as *const u16),
                )
            };
            let c = Counter {
                fd,
                page,
                len,
                shift: 64 - u32::from(width.clamp(1, 64)),
            };
            // cap_user_rdpmc is bit 2; index 0 means "not readable now".
            (caps & (1 << 2) != 0 && index != 0).then_some(c)
        }

        /// Current count, following the kernel's self-monitoring protocol:
        /// `lock` (seqcount), `index` (hardware counter + 1, 0 when the
        /// event is not on the PMU) and `offset` (count accumulated by the
        /// kernel across context switches) are read under the seqcount, the
        /// raw counter is sign-extended to `pmc_width` and added to `offset`.
        #[inline(always)]
        fn read(&self) -> u64 {
            loop {
                // SAFETY: `page` maps this event's `perf_event_mmap_page`
                // (`lock: u32` at 8, `index: u32` at 12, `offset: i64` at 16);
                // the kernel updates it, hence volatile reads and the retry.
                let (seq, idx, offset) = unsafe {
                    let seq = std::ptr::read_volatile(self.page.add(8) as *const u32);
                    compiler_fence(Ordering::SeqCst);
                    (
                        seq,
                        std::ptr::read_volatile(self.page.add(12) as *const u32),
                        std::ptr::read_volatile(self.page.add(16) as *const i64),
                    )
                };
                let mut count = offset;
                if idx != 0 {
                    let lo: u32;
                    let hi: u32;
                    // SAFETY: rdpmc of the counter the kernel exposed to this
                    // thread (cap_user_rdpmc checked in `open`); no memory
                    // access.
                    unsafe {
                        asm!("rdpmc", in("ecx") idx - 1, out("eax") lo, out("edx") hi,
                             options(nomem, nostack, preserves_flags));
                    }
                    let raw = ((u64::from(hi) << 32) | u64::from(lo)) << self.shift;
                    count = count.wrapping_add((raw as i64) >> self.shift);
                }
                compiler_fence(Ordering::SeqCst);
                // SAFETY: as above.
                if unsafe { std::ptr::read_volatile(self.page.add(8) as *const u32) } == seq {
                    return count as u64;
                }
            }
        }
    }

    impl super::Stamp for Counter {
        #[inline(always)]
        fn begin(&self) -> u64 {
            // SAFETY: lfence has no memory operand.
            unsafe { _mm_lfence() };
            let t = self.read();
            // SAFETY: as above.
            unsafe { _mm_lfence() };
            t
        }
        #[inline(always)]
        fn end(&self) -> u64 {
            // SAFETY: as above; waits for the measured operation to complete.
            unsafe { _mm_lfence() };
            let t = self.read();
            // SAFETY: as above.
            unsafe { _mm_lfence() };
            t
        }
    }

    impl Drop for Counter {
        fn drop(&mut self) {
            // SAFETY: unmaps / closes what `open` created.
            unsafe {
                libc::munmap(self.page as *mut libc::c_void, self.len);
                libc::close(self.fd);
            }
        }
    }
}

/// Latency clock of this machine (cycle counter when permitted, else the
/// time-stamp counter).
pub fn source() -> String {
    if cycles_available() {
        "core cycle counter (rdpmc on a perf_event user-mode cycle counter, 1-cycle resolution), \
         converted to ns with the frequency measured over each sampling pass"
            .into()
    } else if imp::available() {
        imp::NAME.into()
    } else {
        "unavailable".into()
    }
}

/// Effective resolution of a fenced time-stamp-counter timestamp: most
/// frequent non-zero difference between consecutive fenced reads, in ticks
/// (the hardware step, 10 ns on AMD Zen, plus the cost of the fences).
pub fn tsc_granularity() -> u64 {
    let mut counts = std::collections::HashMap::new();
    let mut prev = begin();
    for _ in 0..200_000 {
        let t = begin();
        let d = t.wrapping_sub(prev);
        if d > 0 {
            *counts.entry(d).or_insert(0u32) += 1;
        }
        prev = t;
    }
    counts
        .into_iter()
        .max_by_key(|&(d, c)| (c, std::cmp::Reverse(d)))
        .map_or(1, |(d, _)| d)
}

/// `true` when the calling thread can open a cycle counter.
pub fn cycles_available() -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        pmc::Counter::open().is_some()
    }
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    {
        false
    }
}

/// Median cost of an empty timed section with `s`, in its units.
pub fn stamp_overhead<S: Stamp>(s: &S) -> u64 {
    let mut v: Vec<u64> = (0..2_000)
        .map(|i| {
            let t0 = s.begin();
            black_box(i);
            let t1 = s.end();
            t1.wrapping_sub(t0) & s.mask()
        })
        .collect();
    v.sort_unstable();
    v[v.len() / 2]
}

/// Timestamp ticks per second, calibrated against `Instant` over `dur`.
pub fn ticks_per_second(dur: Duration) -> f64 {
    let t0 = Instant::now();
    let c0 = begin();
    while t0.elapsed() < dur {
        std::hint::spin_loop();
    }
    let c1 = end();
    let el = t0.elapsed();
    (c1.wrapping_sub(c0)) as f64 / el.as_secs_f64()
}

/// Cost of an empty timed section, in ticks: `(min, median)` of 20 000
/// samples.
pub fn overhead() -> (u64, u64) {
    let mut v: Vec<u64> = (0..20_000)
        .map(|i| {
            let t0 = begin();
            black_box(i);
            let t1 = end();
            t1.wrapping_sub(t0)
        })
        .collect();
    v.sort_unstable();
    (v[0], v[v.len() / 2])
}

/// Distribution summary in nanoseconds (nearest-rank percentiles).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

/// Summarizes raw tick samples: subtracts `overhead` ticks (saturating) and
/// converts with `ns_per_tick`. Sorts `samples` in place.
pub fn summarize(samples: &mut [u64], overhead: u64, ns_per_tick: f64) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    samples.sort_unstable();
    let n = samples.len();
    let at = |q: f64| {
        let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
        samples[rank - 1].saturating_sub(overhead) as f64 * ns_per_tick
    };
    let sum: f64 = samples
        .iter()
        .map(|&s| s.saturating_sub(overhead) as f64)
        .sum();
    Summary {
        n,
        min: at(0.0),
        p50: at(0.50),
        p90: at(0.90),
        p95: at(0.95),
        p99: at(0.99),
        max: at(1.0),
        mean: sum / n as f64 * ns_per_tick,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let mut v: Vec<u64> = (1..=100).rev().collect();
        let s = summarize(&mut v, 0, 1.0);
        assert_eq!(s.n, 100);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.p50, 50.0);
        assert_eq!(s.p90, 90.0);
        assert_eq!(s.p95, 95.0);
        assert_eq!(s.p99, 99.0);
        assert_eq!(s.max, 100.0);
        assert_eq!(s.mean, 50.5);
        let mut one = vec![7u64];
        let s = summarize(&mut one, 2, 2.0);
        assert_eq!((s.min, s.p99, s.max), (10.0, 10.0, 10.0));
    }

    #[test]
    fn clock_is_monotonic_and_calibrates() {
        let a = begin();
        let b = end();
        assert!(b >= a);
        let hz = ticks_per_second(Duration::from_millis(20));
        assert!(hz > 1e6, "{hz}");
        let (min, med) = overhead();
        assert!(min <= med);
    }
}

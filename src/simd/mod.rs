//! Lookup kernels: portable scalar, plus AVX2, AVX-512 and NEON batch
//! kernels with the `batch-simd` feature.
//!
//! The scalar family serves single lookups in every build, and batches
//! when `batch-simd` is off or the CPU has no supported extension:
//!
//! * `v4_one(tables, key: u32) -> cell`
//! * `v4_batch(tables, keys: &[u32], out: &mut [u32])`
//! * `v6_one(tables, key: u128) -> cell`
//! * `v6_batch(tables, keys: &[[u8; 16]], out: &mut [u32])`
//!
//! The SIMD families only provide the two batch entry points, operating on
//! raw cells (no payload access). They are `#[target_feature]` functions
//! compiled only with `batch-simd` and for their architecture, reachable
//! only through [`Kernel`](crate::lookup::dispatch::Kernel) values, which
//! are only constructed after runtime feature detection succeeded.

pub(crate) mod scalar;
pub(crate) mod tables;

#[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
pub(crate) mod avx2;
#[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
pub(crate) mod avx512;
#[cfg(all(feature = "batch-simd", target_arch = "aarch64"))]
pub(crate) mod neon;

/// Hints the CPU to bring the cache line holding `p` into L1. Never
/// faults, whatever the address; a no-op on other architectures.
#[inline(always)]
pub(crate) fn prefetch<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `prefetcht0` is only a hint and cannot fault (SSE, baseline
    // on x86-64).
    unsafe {
        core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_T0 }>(p.cast());
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `prfm` is a hint and cannot fault; it reads no register but
    // the address and writes nothing.
    unsafe {
        core::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly, preserves_flags));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = p;
}

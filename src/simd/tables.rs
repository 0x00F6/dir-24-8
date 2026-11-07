//! Borrowed view of one engine's lookup arrays, passed to every kernel.

use crate::route::entry::{GROUP_SIZE, TBL24_SIZE};

/// `tbl24` plus the flat secondary-group array of one engine.
///
/// Invariants (established by [`Tables::new`]):
/// * `tbl24.len() == 2^24`, so any 24-bit index is in bounds;
/// * `GROUP_SIZE <= tbl8.len() <= 2^31`, so `tbl8.len() - 1` is a valid
///   clamp bound for signed 32-bit gather indices.
#[derive(Clone, Copy)]
pub(crate) struct Tables<'a> {
    tbl24: &'a [u32],
    tbl8: &'a [u32],
}

impl<'a> Tables<'a> {
    /// Builds the view.
    ///
    /// # Safety
    ///
    /// `tbl24.len()` must be exactly `2^24` and `tbl8.len()` must be in
    /// `GROUP_SIZE..=2^31`. The engines own arrays sized at construction
    /// that only ever grow within these bounds.
    #[inline(always)]
    pub(crate) unsafe fn new(tbl24: &'a [u32], tbl8: &'a [u32]) -> Self {
        debug_assert_eq!(tbl24.len(), TBL24_SIZE);
        debug_assert!(tbl8.len() >= GROUP_SIZE && tbl8.len() <= 1 << 31);
        Self { tbl24, tbl8 }
    }

    /// Root cell for a 24-bit index (upper bits are ignored).
    #[inline(always)]
    pub(crate) fn root(&self, idx: u32) -> u32 {
        let i = (idx & 0x00FF_FFFF) as usize;
        // SAFETY: `i < 2^24 == tbl24.len()` by the type invariant.
        unsafe { *self.tbl24.get_unchecked(i) }
    }

    /// Secondary cell (bounds checked: a corrupted index panics instead of
    /// reading out of bounds).
    #[inline(always)]
    pub(crate) fn group(&self, i: usize) -> u32 {
        self.tbl8[i]
    }

    /// Base of `tbl24` for the x86-64 gathers.
    #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
    #[inline(always)]
    pub(crate) fn tbl24_ptr(&self) -> *const u32 {
        self.tbl24.as_ptr()
    }

    /// Base of the group array for the x86-64 gathers.
    #[cfg(all(feature = "batch-simd", target_arch = "x86_64"))]
    #[inline(always)]
    pub(crate) fn tbl8_ptr(&self) -> *const u32 {
        self.tbl8.as_ptr()
    }

    /// Largest valid `tbl8` index; SIMD kernels clamp group indices to it.
    #[cfg(all(
        feature = "batch-simd",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[inline(always)]
    pub(crate) fn tbl8_max(&self) -> u32 {
        (self.tbl8.len() - 1) as u32
    }
}

//! Zero-initialized `u32` arrays backing `tbl24` and the secondary groups.
//!
//! * On Linux, buffers of at least 2 MiB are anonymous `mmap` mappings. The
//!   kernel zero-fills pages lazily, so a 64 MiB `tbl24` costs resident
//!   memory only for the pages that routes actually touch. When huge pages
//!   are requested (`Config::huge_pages`, off by default) the mapping is
//!   aligned to 2 MiB and advised with `MADV_HUGEPAGE`. These mappings
//!   bypass the global allocator.
//! * Elsewhere (and for small buffers) the global allocator's `alloc_zeroed`
//!   is used through a `Vec<u32>`; large `calloc` requests are lazily zeroed
//!   by most allocators as well.

use core::ptr::NonNull;

/// Size of an x86-64 / AArch64 (4 KiB granule) transparent huge page.
#[cfg(target_os = "linux")]
const HUGE_PAGE: usize = 2 << 20;

/// Buffers at least this large are mapped directly.
#[cfg(target_os = "linux")]
const MMAP_THRESHOLD: usize = 2 << 20;

pub(crate) struct TableBuffer {
    ptr: NonNull<u32>,
    len: usize,
    backing: Backing,
}

enum Backing {
    /// `Vec<u32>` with this capacity, reconstructed on drop.
    Heap { cap: usize },
    /// Anonymous mapping of `size` bytes starting at `ptr`.
    #[cfg(target_os = "linux")]
    Mmap { size: usize, huge: bool },
}

// SAFETY: `TableBuffer` uniquely owns its allocation (like a `Box<[u32]>`);
// shared access only reads plain integers and mutation requires `&mut self`.
unsafe impl Send for TableBuffer {}
// SAFETY: see above; there is no interior mutability.
unsafe impl Sync for TableBuffer {}

/// Allocation failure, with the requested size in bytes.
pub(crate) struct AllocError(pub(crate) usize);

impl TableBuffer {
    /// Allocates `len` zeroed cells.
    pub(crate) fn zeroed(len: usize, huge_pages: bool) -> Result<Self, AllocError> {
        let bytes = len
            .checked_mul(core::mem::size_of::<u32>())
            .ok_or(AllocError(usize::MAX))?;
        #[cfg(target_os = "linux")]
        if bytes >= MMAP_THRESHOLD {
            return Self::mmap_zeroed(len, bytes, huge_pages);
        }
        let _ = huge_pages;
        let mut v: Vec<u32> = Vec::new();
        v.try_reserve_exact(len).map_err(|_| AllocError(bytes))?;
        v.resize(len, 0);
        let mut v = core::mem::ManuallyDrop::new(v);
        let cap = v.capacity();
        // SAFETY: `Vec::as_mut_ptr` is never null (dangling for len 0).
        let ptr = unsafe { NonNull::new_unchecked(v.as_mut_ptr()) };
        Ok(Self {
            ptr,
            len,
            backing: Backing::Heap { cap },
        })
    }

    #[cfg(target_os = "linux")]
    fn mmap_zeroed(len: usize, bytes: usize, huge: bool) -> Result<Self, AllocError> {
        let size = bytes
            .checked_next_multiple_of(HUGE_PAGE)
            .ok_or(AllocError(bytes))?;
        let map_len = if huge {
            size.checked_add(HUGE_PAGE).ok_or(AllocError(bytes))?
        } else {
            size
        };
        // SAFETY: anonymous private mapping with no address hint; the result
        // is checked against MAP_FAILED before use.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(AllocError(bytes));
        }
        let mut start = base as usize;
        if huge {
            // Trim the mapping to a 2 MiB aligned window of `size` bytes so
            // the kernel can back it with transparent huge pages.
            let aligned = start.next_multiple_of(HUGE_PAGE);
            let head = aligned - start;
            let tail = map_len - head - size;
            // SAFETY: both ranges lie inside the mapping created above, are
            // page aligned (mmap returns page-aligned addresses and HUGE_PAGE
            // is a multiple of the page size) and are not used afterwards.
            unsafe {
                if head != 0 {
                    libc::munmap(start as *mut libc::c_void, head);
                }
                if tail != 0 {
                    libc::munmap((aligned + size) as *mut libc::c_void, tail);
                }
                // Advisory only: failure (THP disabled) keeps 4 KiB pages.
                libc::madvise(aligned as *mut libc::c_void, size, libc::MADV_HUGEPAGE);
            }
            start = aligned;
        }
        // SAFETY: mmap never returns null on success.
        let ptr = unsafe { NonNull::new_unchecked(start as *mut u32) };
        Ok(Self {
            ptr,
            len,
            backing: Backing::Mmap { size, huge },
        })
    }

    /// Grows the buffer to `new_len` cells, preserving contents; new cells
    /// are zero. Leaves `self` untouched on failure.
    pub(crate) fn grow(&mut self, new_len: usize, huge_pages: bool) -> Result<(), AllocError> {
        if new_len <= self.len {
            return Ok(());
        }
        let mut next = Self::zeroed(new_len, huge_pages)?;
        next.as_mut_slice()[..self.len].copy_from_slice(self.as_slice());
        *self = next;
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[u32] {
        // SAFETY: `ptr` is valid for `len` initialized (zeroed or written)
        // `u32`s for as long as `self` lives; shared borrow of `self`.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    #[inline(always)]
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u32] {
        // SAFETY: as in `as_slice`, and `&mut self` guarantees uniqueness.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Bytes reserved for this buffer (virtual size for mappings).
    pub(crate) fn reserved_bytes(&self) -> usize {
        match self.backing {
            Backing::Heap { cap } => cap * core::mem::size_of::<u32>(),
            #[cfg(target_os = "linux")]
            Backing::Mmap { size, .. } => size,
        }
    }

    /// `true` when the buffer is a mapping advised for huge pages.
    pub(crate) fn huge_pages(&self) -> bool {
        match self.backing {
            Backing::Heap { .. } => false,
            #[cfg(target_os = "linux")]
            Backing::Mmap { huge, .. } => huge,
        }
    }

    /// `true` when the buffer bypasses the global allocator.
    pub(crate) fn is_mapped(&self) -> bool {
        match self.backing {
            Backing::Heap { .. } => false,
            #[cfg(target_os = "linux")]
            Backing::Mmap { .. } => true,
        }
    }
}

impl Drop for TableBuffer {
    fn drop(&mut self) {
        match self.backing {
            Backing::Heap { cap } => {
                // SAFETY: ptr/len/cap come from the `Vec` leaked in `zeroed`.
                drop(unsafe { Vec::from_raw_parts(self.ptr.as_ptr(), self.len, cap) });
            }
            #[cfg(target_os = "linux")]
            Backing::Mmap { size, .. } => {
                // SAFETY: exactly the window kept by `mmap_zeroed`.
                unsafe {
                    libc::munmap(self.ptr.as_ptr().cast(), size);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_and_large_buffers_are_zeroed_and_growable() {
        for huge in [false, true] {
            let mut b = TableBuffer::zeroed(256, huge).ok().unwrap();
            assert!(b.as_slice().iter().all(|&x| x == 0));
            b.as_mut_slice()[255] = 7;
            b.grow(1 << 20, huge).ok().unwrap();
            assert_eq!(b.len(), 1 << 20);
            assert_eq!(b.as_slice()[255], 7);
            assert!(b.as_slice()[256..].iter().all(|&x| x == 0));
            assert!(b.reserved_bytes() >= 4 << 20);
        }
    }
}

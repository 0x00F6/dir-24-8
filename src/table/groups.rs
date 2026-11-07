//! Pool of secondary 256-cell groups (`tbl8`), shared design for both
//! engines.
//!
//! All groups live in one flat zeroed buffer so a lookup addresses any group
//! with `g * 256 + byte` from a single base pointer (required by gathers).
//! Group `0` is reserved and never handed out: the buffer is therefore never
//! empty and clamped SIMD indices (`min(index, len - 1)`) always stay inside
//! the allocation. Released groups go to a free list and are reused before
//! the buffer grows; the buffer itself is never shrunk while the table lives.

use super::buffer::TableBuffer;
use crate::route::entry::{GROUP_LIMIT, GROUP_SIZE};

pub(crate) struct GroupPool {
    buf: TableBuffer,
    /// Groups `1..hwm` have been handed out at least once.
    hwm: u32,
    free: Vec<u32>,
    /// Maximum number of groups in use at the same time.
    limit: u32,
    huge: bool,
}

pub(crate) enum PoolError {
    Limit(u32),
    Alloc(usize),
}

impl GroupPool {
    pub(crate) fn new(limit: u32, huge: bool) -> Result<Self, PoolError> {
        let buf = TableBuffer::zeroed(GROUP_SIZE, huge).map_err(|e| PoolError::Alloc(e.0))?;
        Ok(Self {
            buf,
            hwm: 1,
            free: Vec::new(),
            limit: limit.min(GROUP_LIMIT - 1),
            huge,
        })
    }

    pub(crate) fn in_use(&self) -> u32 {
        self.hwm - 1 - self.free.len() as u32
    }

    pub(crate) fn free_count(&self) -> u32 {
        self.free.len() as u32
    }

    pub(crate) fn capacity(&self) -> u32 {
        (self.buf.len() / GROUP_SIZE) as u32 - 1
    }

    pub(crate) fn limit(&self) -> u32 {
        self.limit
    }

    /// Guarantees that `n` subsequent [`alloc`](Self::alloc) calls succeed
    /// without allocating and that releases never allocate.
    /// Limit check only (no allocation): can `n` more groups be in use?
    pub(crate) fn check(&self, n: u32) -> Result<(), PoolError> {
        let fresh = n.saturating_sub(self.free.len() as u32);
        if self.in_use() + n > self.limit || self.hwm + fresh > GROUP_LIMIT {
            return Err(PoolError::Limit(self.limit));
        }
        Ok(())
    }

    pub(crate) fn reserve(&mut self, n: u32) -> Result<(), PoolError> {
        if n == 0 {
            return Ok(());
        }
        self.check(n)?;
        let fresh = n.saturating_sub(self.free.len() as u32);
        let need_hwm = self.hwm + fresh;
        let cap_groups = (self.buf.len() / GROUP_SIZE) as u32;
        if need_hwm > cap_groups {
            let target = need_hwm.max(cap_groups.saturating_mul(2)).min(GROUP_LIMIT);
            self.buf
                .grow(target as usize * GROUP_SIZE, self.huge)
                .map_err(|e| PoolError::Alloc(e.0))?;
        }
        let want = need_hwm as usize;
        if self.free.capacity() < want {
            self.free
                .try_reserve(want - self.free.len())
                .map_err(|_| PoolError::Alloc(want * 4))?;
        }
        Ok(())
    }

    /// Hands out a group whose 256 cells all equal `fill` (inheritance of the
    /// covering route). Requires a prior successful `reserve`.
    pub(crate) fn alloc(&mut self, fill: u32) -> u32 {
        let g = match self.free.pop() {
            Some(g) => g,
            None => {
                let g = self.hwm;
                self.hwm += 1;
                g
            }
        };
        debug_assert!((g as usize + 1) * GROUP_SIZE <= self.buf.len());
        self.cells_mut(g).fill(fill);
        g
    }

    pub(crate) fn release(&mut self, g: u32) {
        debug_assert!(g != 0 && g < self.hwm);
        self.free.push(g);
    }

    #[inline(always)]
    pub(crate) fn cells(&self, g: u32) -> &[u32] {
        let start = g as usize * GROUP_SIZE;
        &self.buf.as_slice()[start..start + GROUP_SIZE]
    }

    #[inline(always)]
    pub(crate) fn cells_mut(&mut self, g: u32) -> &mut [u32] {
        let start = g as usize * GROUP_SIZE;
        &mut self.buf.as_mut_slice()[start..start + GROUP_SIZE]
    }

    /// The whole flat cell array (for lookups).
    #[inline(always)]
    pub(crate) fn flat(&self) -> &[u32] {
        self.buf.as_slice()
    }

    pub(crate) fn buffer(&self) -> &TableBuffer {
        &self.buf
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.free.capacity() * 4
    }
}

/// Returns the common value of a group whose cells are all identical
/// terminal (non-group) cells, i.e. a group that can be folded back into its
/// parent cell.
#[inline]
pub(crate) fn uniform_value(cells: &[u32]) -> Option<u32> {
    let first = cells[0];
    if crate::route::entry::is_group(first) {
        return None;
    }
    // Branch-free OR-reduction so the compiler vectorizes the scan.
    let diff = cells.iter().fold(0u32, |acc, &c| acc | (c ^ first));
    (diff == 0).then_some(first)
}

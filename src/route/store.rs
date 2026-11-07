//! Payload store: one slot per route, referenced by every table cell the
//! route wins.
//!
//! The lookup tables never contain payloads, only compact slot numbers. A
//! slot keeps the normalized prefix (needed to build
//! [`LookupMatch::prefix`](crate::LookupMatch::prefix)) next to the optional
//! payload, so materializing a borrowed result touches a single slot.
//! Removed slots go to a free list and are reused by later insertions.

use core::mem::size_of;

use super::entry::ROUTE_LIMIT;
use super::prefix::Prefix;

pub(crate) struct Slot<N, T> {
    pub(crate) net: N,
    pub(crate) data: Option<T>,
    pub(crate) live: bool,
}

pub(crate) struct RouteStore<N, T> {
    slots: Vec<Slot<N, T>>,
    free: Vec<u32>,
    live: usize,
}

/// Why a new slot could not be reserved.
pub(crate) enum ReserveError {
    /// The configured route limit is reached.
    Limit,
    /// The allocator refused to grow the slot array to this many bytes.
    Alloc(usize),
}

impl<N: Prefix, T> RouteStore<N, T> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }

    /// Limit check only (no allocation): can one more route be stored?
    pub(crate) fn check_one(&self, limit: u32) -> Result<(), ReserveError> {
        let limit = limit.min(ROUTE_LIMIT) as usize;
        if self.free.is_empty() && self.slots.len() >= limit {
            return Err(ReserveError::Limit);
        }
        Ok(())
    }

    /// Makes sure [`alloc`](Self::alloc) will not allocate nor exceed `limit`.
    pub(crate) fn reserve_one(&mut self, limit: u32) -> Result<(), ReserveError> {
        if !self.free.is_empty() {
            return Ok(());
        }
        self.check_one(limit)?;
        self.slots
            .try_reserve(1)
            .map_err(|_| ReserveError::Alloc((self.slots.len() + 1) * size_of::<Slot<N, T>>()))?;
        // Keep the free list able to hold every slot so `release` never
        // allocates.
        let want = self.slots.capacity();
        if self.free.capacity() < want {
            self.free
                .try_reserve(want - self.free.len())
                .map_err(|_| ReserveError::Alloc(want * size_of::<u32>()))?;
        }
        Ok(())
    }

    /// Stores a new route and returns its slot. Call `reserve_one` first.
    pub(crate) fn alloc(&mut self, net: N, data: Option<T>) -> u32 {
        self.live += 1;
        if let Some(slot) = self.free.pop() {
            let s = &mut self.slots[slot as usize];
            debug_assert!(!s.live);
            s.net = net;
            s.data = data;
            s.live = true;
            return slot;
        }
        let slot = self.slots.len() as u32;
        self.slots.push(Slot {
            net,
            data,
            live: true,
        });
        slot
    }

    /// Frees a slot and returns its payload.
    pub(crate) fn release(&mut self, slot: u32) -> Option<T> {
        let s = &mut self.slots[slot as usize];
        debug_assert!(s.live);
        s.live = false;
        self.live -= 1;
        let data = s.data.take();
        self.free.push(slot);
        data
    }

    /// Slot by number, without liveness check (internal cells only reference
    /// live slots).
    #[inline(always)]
    pub(crate) fn get(&self, slot: u32) -> Option<&Slot<N, T>> {
        self.slots.get(slot as usize)
    }

    /// Prefetches the slot of a terminal cell (no-op for empty cells).
    #[inline(always)]
    pub(crate) fn prefetch(&self, slot: u32) {
        let i = slot as usize;
        if i < self.slots.len() {
            crate::simd::prefetch(self.slots.as_ptr().wrapping_add(i));
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, slot: u32) -> &mut Slot<N, T> {
        &mut self.slots[slot as usize]
    }

    /// Slot by number, `None` for removed or out-of-range slots.
    #[inline]
    pub(crate) fn get_live(&self, slot: u32) -> Option<&Slot<N, T>> {
        self.slots.get(slot as usize).filter(|s| s.live)
    }

    /// Prefix length of a live slot.
    #[inline]
    pub(crate) fn prefix_len(&self, slot: u32) -> u8 {
        self.slots[slot as usize].net.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.live
    }

    /// Live slots in slot order. The order only depends on the history of
    /// the store (insertions and removals), never on hashing.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Slot<N, T>> + Clone {
        self.slots.iter().filter(|s| s.live)
    }

    /// Numbers of the live slots, in slot order.
    #[cfg(feature = "snapshot")]
    pub(crate) fn live_slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.live)
            .map(|(i, _)| i as u32)
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Slot<N, T>> {
        self.slots.iter_mut().filter(|s| s.live)
    }

    pub(crate) fn with_data(&self) -> usize {
        self.iter().filter(|s| s.data.is_some()).count()
    }

    /// Heap bytes owned by the store itself (payload-owned heap not included).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.slots.capacity() * core::mem::size_of::<Slot<N, T>>()
            + self.free.capacity() * core::mem::size_of::<u32>()
    }
}

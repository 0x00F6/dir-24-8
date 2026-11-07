//! IPv4 engine: DIR-24-8.
//!
//! * `tbl24` has `2^24` cells indexed by the 24 most significant bits of the
//!   address (`u32::from_be_bytes(octets) >> 8`).
//! * A cell either holds the winning route of the whole /24, nothing, or a
//!   secondary group of 256 cells indexed by the last octet.
//! * A lookup therefore performs one access to `tbl24` and at most one
//!   access to a group: **one or two dependent memory accesses**, whatever
//!   the prefix length (`/0` to `/32`).
//!
//! Maintenance rule (both for insertion and removal): every cell holds the
//! longest route covering all addresses of the cell. Inserting a route of
//! length `L` overwrites, inside its range, exactly the cells whose current
//! winner is shorter than `L` (or empty); removing it rewrites the cells it
//! owns with its parent (the longest remaining route containing it). A group
//! whose 256 cells become identical is folded back into its `tbl24` cell and
//! released for reuse.

use ipnet::{IpNet, Ipv4Net};

use crate::error::{AddressFamily, Error};
use crate::lookup::LookupMatch;
use crate::route::entry::{
    EMPTY, TBL24_SIZE, group_cell, group_index, is_group, route_cell, route_slot,
};
use crate::route::index::PrefixIndex;
use crate::route::store::{RouteStore, Slot};
use crate::simd::tables::Tables;
use crate::table::buffer::TableBuffer;
use crate::table::engine::{EngineLimits, alloc_error, pool_error, store_error};
use crate::table::groups::{GroupPool, uniform_value};
use crate::table::stats::FamilyStats;

const FAMILY: AddressFamily = AddressFamily::Ipv4;

pub(crate) struct Ipv4Engine<T> {
    tbl24: TableBuffer,
    groups: GroupPool,
    routes: RouteStore<Ipv4Net, T>,
    index: PrefixIndex<Ipv4Net>,
    max_routes: u32,
}

impl<T> Ipv4Engine<T> {
    pub(crate) fn new(limits: &EngineLimits) -> Result<Self, Error> {
        let tbl24 = TableBuffer::zeroed(TBL24_SIZE, limits.huge_pages)
            .map_err(|e| alloc_error(FAMILY, e.0))?;
        let groups = GroupPool::new(limits.max_ipv4_groups, limits.huge_pages)
            .map_err(|e| pool_error(FAMILY, e))?;
        Ok(Self {
            tbl24,
            groups,
            routes: RouteStore::new(),
            index: PrefixIndex::new(),
            max_routes: limits.max_routes,
        })
    }

    #[inline(always)]
    pub(crate) fn tables(&self) -> Tables<'_> {
        // SAFETY: `tbl24` is allocated with exactly TBL24_SIZE cells and never
        // resized; the group pool always holds the reserved group 0 and
        // never exceeds GROUP_LIMIT * 256 = 2^31 cells.
        unsafe { Tables::new(self.tbl24.as_slice(), self.groups.flat()) }
    }

    /// Converts a terminal cell into a borrowed result.
    #[inline(always)]
    pub(crate) fn resolve(&self, cell: u32) -> Option<LookupMatch<'_, T>> {
        // `route_slot(EMPTY)` wraps to `u32::MAX`, which is never a valid
        // slot, so the bounds check of `get` also covers "no match".
        let slot = self.routes.get(route_slot(cell))?;
        Some(LookupMatch {
            prefix: IpNet::V4(slot.net),
            data: slot.data.as_ref(),
        })
    }

    /// Prefetches the route slot `resolve(cell)` will read.
    #[inline(always)]
    pub(crate) fn prefetch(&self, cell: u32) {
        self.routes.prefetch(route_slot(cell));
    }

    pub(crate) fn resolve_live(&self, cell: u32) -> Option<LookupMatch<'_, T>> {
        let slot = self.routes.get_live(route_slot(cell))?;
        Some(LookupMatch {
            prefix: IpNet::V4(slot.net),
            data: slot.data.as_ref(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.routes.len()
    }

    pub(crate) fn get(&self, net: Ipv4Net) -> Option<&Slot<Ipv4Net, T>> {
        let slot = self.index.get(&net.trunc())?;
        self.routes.get(slot)
    }

    pub(crate) fn get_mut(&mut self, net: Ipv4Net) -> Option<&mut Option<T>> {
        let slot = self.index.get(&net.trunc())?;
        Some(&mut self.routes.get_mut(slot).data)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Slot<Ipv4Net, T>> {
        self.routes.iter()
    }

    /// The route store (every live route, including masked ones).
    #[cfg(feature = "snapshot")]
    pub(crate) fn routes(&self) -> &RouteStore<Ipv4Net, T> {
        &self.routes
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Slot<Ipv4Net, T>> {
        self.routes.iter_mut()
    }

    /// Inserts or replaces a route. Returns the previous payload when the
    /// normalized CIDR already existed (`Some(previous)`), `None` otherwise.
    pub(crate) fn insert(
        &mut self,
        net: Ipv4Net,
        data: Option<T>,
    ) -> Result<Option<Option<T>>, Error> {
        let net = net.trunc();
        if let Some(slot) = self.index.get(&net) {
            let s = self.routes.get_mut(slot);
            return Ok(Some(core::mem::replace(&mut s.data, data)));
        }
        let len = net.prefix_len();
        let addr = u32::from(net.network());
        let root = (addr >> 8) as usize;
        let needs_group = len > 24 && !is_group(self.tbl24.as_slice()[root]);

        // Limit checks first (no allocation), then the fallible
        // reservations; every table write comes after both.
        self.routes
            .check_one(self.max_routes)
            .map_err(|e| store_error(FAMILY, self.max_routes, e))?;
        if needs_group {
            self.groups.check(1).map_err(|e| pool_error(FAMILY, e))?;
        }
        self.routes
            .reserve_one(self.max_routes)
            .map_err(|e| store_error(FAMILY, self.max_routes, e))?;
        self.index
            .try_reserve_one()
            .map_err(|_| alloc_error(FAMILY, core::mem::size_of::<(Ipv4Net, u32)>()))?;
        if needs_group {
            self.groups.reserve(1).map_err(|e| pool_error(FAMILY, e))?;
        }

        let slot = self.routes.alloc(net, data);
        self.index.insert(net, slot);
        let cell = route_cell(slot);
        let Self {
            tbl24,
            groups,
            routes,
            ..
        } = self;
        let tbl24 = tbl24.as_mut_slice();
        if len <= 24 {
            let count = 1usize << (24 - len);
            let mut cache = ShorterCache::new(len);
            for c in &mut tbl24[root..root + count] {
                let cur = *c;
                if is_group(cur) {
                    let g = group_index(cur);
                    let cells = groups.cells_mut(g);
                    overwrite_shorter(cells, len, cell, routes);
                    if let Some(v) = uniform_value(cells) {
                        *c = v;
                        groups.release(g);
                    }
                } else if cache.is_shorter(cur, routes) {
                    *c = cell;
                }
            }
        } else {
            let g = if needs_group {
                let g = groups.alloc(tbl24[root]);
                tbl24[root] = group_cell(g);
                g
            } else {
                group_index(tbl24[root])
            };
            let lo = (addr & 0xFF) as usize;
            let count = 1usize << (32 - len);
            overwrite_shorter(&mut groups.cells_mut(g)[lo..lo + count], len, cell, routes);
        }
        Ok(None)
    }

    /// Removes a route and returns its payload (`Some(data)`), or `None`
    /// when the normalized CIDR is absent.
    pub(crate) fn remove(&mut self, net: Ipv4Net) -> Option<Option<T>> {
        let net = net.trunc();
        let slot = self.index.remove(&net)?;
        let parent = self.index.parent(&net).map_or(EMPTY, route_cell);
        let old = route_cell(slot);
        let len = net.prefix_len();
        let addr = u32::from(net.network());
        let root = (addr >> 8) as usize;
        let Self { tbl24, groups, .. } = self;
        let tbl24 = tbl24.as_mut_slice();
        if len <= 24 {
            let count = 1usize << (24 - len);
            for c in &mut tbl24[root..root + count] {
                let cur = *c;
                if cur == old {
                    *c = parent;
                } else if is_group(cur) {
                    let g = group_index(cur);
                    let cells = groups.cells_mut(g);
                    replace_cells(cells, old, parent);
                    if let Some(v) = uniform_value(cells) {
                        *c = v;
                        groups.release(g);
                    }
                }
            }
        } else {
            let cur = tbl24[root];
            debug_assert!(is_group(cur), "a /25+ route always lives in a group");
            if is_group(cur) {
                let g = group_index(cur);
                let lo = (addr & 0xFF) as usize;
                let count = 1usize << (32 - len);
                let cells = groups.cells_mut(g);
                replace_cells(&mut cells[lo..lo + count], old, parent);
                if let Some(v) = uniform_value(cells) {
                    tbl24[root] = v;
                    groups.release(g);
                }
            }
        }
        Some(self.routes.release(slot))
    }

    pub(crate) fn stats(&self) -> FamilyStats {
        let tbl8 = self.groups.buffer();
        FamilyStats {
            allocated: true,
            routes: self.routes.len(),
            routes_with_data: self.routes.with_data(),
            groups_in_use: self.groups.in_use() as usize,
            groups_free: self.groups.free_count() as usize,
            groups_capacity: self.groups.capacity() as usize,
            groups_limit: self.groups.limit() as usize,
            tbl24_bytes: self.tbl24.reserved_bytes(),
            tbl8_bytes: tbl8.reserved_bytes(),
            route_store_bytes: self.routes.heap_bytes(),
            index_bytes: self.index.heap_bytes() + self.groups.heap_bytes(),
            huge_pages: self.tbl24.huge_pages(),
            mapped_bytes: [&self.tbl24, tbl8]
                .iter()
                .filter(|b| b.is_mapped())
                .map(|b| b.reserved_bytes())
                .sum(),
        }
    }
}

/// Memoizes "is the winner of this cell shorter than `len`" for runs of
/// identical cells (large ranges are mostly made of the same value).
pub(crate) struct ShorterCache {
    len: u8,
    cell: u32,
    shorter: bool,
}

impl ShorterCache {
    #[inline]
    pub(crate) fn new(len: u8) -> Self {
        Self {
            len,
            cell: EMPTY,
            shorter: true,
        }
    }

    /// `cur` must be a terminal cell.
    #[inline]
    pub(crate) fn is_shorter<N: crate::route::prefix::Prefix, T>(
        &mut self,
        cur: u32,
        routes: &RouteStore<N, T>,
    ) -> bool {
        if cur != self.cell {
            self.cell = cur;
            self.shorter = cur == EMPTY || routes.prefix_len(route_slot(cur)) < self.len;
        }
        self.shorter
    }
}

/// Overwrites the terminal cells whose winner is shorter than `len`.
fn overwrite_shorter<T>(cells: &mut [u32], len: u8, cell: u32, routes: &RouteStore<Ipv4Net, T>) {
    let mut cache = ShorterCache::new(len);
    for c in cells {
        debug_assert!(!is_group(*c));
        if cache.is_shorter(*c, routes) {
            *c = cell;
        }
    }
}

/// Rewrites every cell equal to `old` with `new`.
#[inline]
pub(crate) fn replace_cells(cells: &mut [u32], old: u32, new: u32) {
    for c in cells {
        if *c == old {
            *c = new;
        }
    }
}

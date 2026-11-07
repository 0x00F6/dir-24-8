//! IPv6 engine: multi-level 24/8 trie (FIB6 / DPDK `trie` layout).
//!
//! * Level 0 is a `2^24`-cell root indexed by the first three octets.
//! * Levels 1 to 13 are 256-cell groups indexed by octets 3 to 15, allocated
//!   only on the paths of routes longer than /24: `24 + 13 * 8 = 128`.
//! * A route of length `L > 24` ends in the group of level
//!   `k = ceil((L - 24) / 8)` and covers `2^(24 + 8k - L)` of its cells, so
//!   lengths that are not multiples of 8 are expanded inside one group.
//! * A lookup walks one level per octet until it reaches a terminal cell:
//!   between **1 and 14 dependent accesses** (`/48` routes: 4, `/64`: 6),
//!   unlike the one-or-two bound of the IPv4 DIR-24-8.
//!
//! Keys are handled as network-order octets (`Ipv6Addr::octets`) or as the
//! big-endian numeric value `u128::from_be_bytes(octets)`; both describe the
//! same bit order (octet 0 holds the most significant bits).
//!
//! Maintenance follows the same rule as IPv4, applied recursively: insertion
//! overwrites the cells of its range whose winner is shorter, descending into
//! nested groups; removal rewrites the cells it owns with its parent and
//! folds every group that became uniform, bottom-up along the path.

use ipnet::{IpNet, Ipv6Net};

use crate::error::{AddressFamily, Error};
use crate::ipv4::{ShorterCache, replace_cells};
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

const FAMILY: AddressFamily = AddressFamily::Ipv6;

/// Number of 8-bit levels below the 24-bit root.
pub(crate) const LEVELS: usize = 13;

pub(crate) struct Ipv6Engine<T> {
    tbl24: TableBuffer,
    groups: GroupPool,
    routes: RouteStore<Ipv6Net, T>,
    index: PrefixIndex<Ipv6Net>,
    max_routes: u32,
}

/// Octet of `addr` consumed by level `k` (`1..=13`).
#[inline]
fn level_byte(addr: u128, k: usize) -> usize {
    ((addr >> (104 - 8 * k)) & 0xFF) as usize
}

/// Level whose group holds a route of length `len > 24`.
#[inline]
fn final_level(len: u8) -> usize {
    (usize::from(len) - 24).div_ceil(8)
}

impl<T> Ipv6Engine<T> {
    pub(crate) fn new(limits: &EngineLimits) -> Result<Self, Error> {
        let tbl24 = TableBuffer::zeroed(TBL24_SIZE, limits.huge_pages)
            .map_err(|e| alloc_error(FAMILY, e.0))?;
        let groups = GroupPool::new(limits.max_ipv6_groups, limits.huge_pages)
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
        // SAFETY: same invariants as the IPv4 engine: fixed 2^24-cell root and
        // a group pool holding between 1 and GROUP_LIMIT groups.
        unsafe { Tables::new(self.tbl24.as_slice(), self.groups.flat()) }
    }

    #[inline(always)]
    pub(crate) fn resolve(&self, cell: u32) -> Option<LookupMatch<'_, T>> {
        let slot = self.routes.get(route_slot(cell))?;
        Some(LookupMatch {
            prefix: IpNet::V6(slot.net),
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
            prefix: IpNet::V6(slot.net),
            data: slot.data.as_ref(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.routes.len()
    }

    pub(crate) fn get(&self, net: Ipv6Net) -> Option<&Slot<Ipv6Net, T>> {
        let slot = self.index.get(&net.trunc())?;
        self.routes.get(slot)
    }

    pub(crate) fn get_mut(&mut self, net: Ipv6Net) -> Option<&mut Option<T>> {
        let slot = self.index.get(&net.trunc())?;
        Some(&mut self.routes.get_mut(slot).data)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Slot<Ipv6Net, T>> {
        self.routes.iter()
    }

    /// The route store (every live route, including masked ones).
    #[cfg(feature = "snapshot")]
    pub(crate) fn routes(&self) -> &RouteStore<Ipv6Net, T> {
        &self.routes
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Slot<Ipv6Net, T>> {
        self.routes.iter_mut()
    }

    pub(crate) fn insert(
        &mut self,
        net: Ipv6Net,
        data: Option<T>,
    ) -> Result<Option<Option<T>>, Error> {
        let net = net.trunc();
        if let Some(slot) = self.index.get(&net) {
            let s = self.routes.get_mut(slot);
            return Ok(Some(core::mem::replace(&mut s.data, data)));
        }
        let len = net.prefix_len();
        let addr = u128::from(net.network());
        let root = (addr >> 104) as usize;

        // Count the groups the path still lacks before touching anything.
        let mut needed = 0u32;
        if len > 24 {
            let kf = final_level(len);
            let mut cur = self.tbl24.as_slice()[root];
            for k in 1..=kf {
                if !is_group(cur) {
                    needed = (kf - k + 1) as u32;
                    break;
                }
                if k < kf {
                    cur = self.groups.cells(group_index(cur))[level_byte(addr, k)];
                }
            }
        }
        // Limit checks first (no allocation), then the fallible
        // reservations; every table write comes after both.
        self.routes
            .check_one(self.max_routes)
            .map_err(|e| store_error(FAMILY, self.max_routes, e))?;
        self.groups
            .check(needed)
            .map_err(|e| pool_error(FAMILY, e))?;
        self.routes
            .reserve_one(self.max_routes)
            .map_err(|e| store_error(FAMILY, self.max_routes, e))?;
        self.index
            .try_reserve_one()
            .map_err(|_| alloc_error(FAMILY, core::mem::size_of::<(Ipv6Net, u32)>()))?;
        self.groups
            .reserve(needed)
            .map_err(|e| pool_error(FAMILY, e))?;

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
                    fill_group(groups, routes, g, 0, 256, len, cell);
                    if let Some(v) = uniform_value(groups.cells(g)) {
                        *c = v;
                        groups.release(g);
                    }
                } else if cache.is_shorter(cur, routes) {
                    *c = cell;
                }
            }
            return Ok(None);
        }

        let kf = final_level(len);
        // Walk (and create) the path down to the final group.
        let mut g = {
            let cur = tbl24[root];
            if is_group(cur) {
                group_index(cur)
            } else {
                let g = groups.alloc(cur);
                tbl24[root] = group_cell(g);
                g
            }
        };
        for k in 1..kf {
            let b = level_byte(addr, k);
            let cur = groups.cells(g)[b];
            g = if is_group(cur) {
                group_index(cur)
            } else {
                let child = groups.alloc(cur);
                groups.cells_mut(g)[b] = group_cell(child);
                child
            };
        }
        let count = 1usize << (24 + 8 * kf - usize::from(len));
        let lo = level_byte(addr, kf);
        debug_assert_eq!(lo % count, 0);
        fill_group(groups, routes, g, lo, lo + count, len, cell);
        Ok(None)
    }

    pub(crate) fn remove(&mut self, net: Ipv6Net) -> Option<Option<T>> {
        let net = net.trunc();
        let slot = self.index.remove(&net)?;
        let parent = self.index.parent(&net).map_or(EMPTY, route_cell);
        let old = route_cell(slot);
        let len = net.prefix_len();
        let addr = u128::from(net.network());
        let root = (addr >> 104) as usize;
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
                    replace_group(groups, g, 0, 256, old, parent);
                    if let Some(v) = uniform_value(groups.cells(g)) {
                        *c = v;
                        groups.release(g);
                    }
                }
            }
        } else {
            let kf = final_level(len);
            let mut path = [0u32; LEVELS];
            let mut cur = tbl24[root];
            let mut complete = true;
            for (k, entry) in path.iter_mut().enumerate().take(kf) {
                if !is_group(cur) {
                    debug_assert!(false, "route path must exist while the route lives");
                    complete = false;
                    break;
                }
                *entry = group_index(cur);
                if k + 1 < kf {
                    cur = groups.cells(*entry)[level_byte(addr, k + 1)];
                }
            }
            if complete {
                let count = 1usize << (24 + 8 * kf - usize::from(len));
                let lo = level_byte(addr, kf);
                replace_group(groups, path[kf - 1], lo, lo + count, old, parent);
                // Fold uniform groups bottom-up; stop at the first survivor.
                for k in (1..=kf).rev() {
                    let g = path[k - 1];
                    let Some(v) = uniform_value(groups.cells(g)) else {
                        break;
                    };
                    if k == 1 {
                        tbl24[root] = v;
                    } else {
                        groups.cells_mut(path[k - 2])[level_byte(addr, k - 1)] = v;
                    }
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

/// Applies an insertion of length `len` to cells `lo..hi` of group `g`,
/// recursing into nested groups and folding the ones that become uniform.
fn fill_group<T>(
    groups: &mut GroupPool,
    routes: &RouteStore<Ipv6Net, T>,
    g: u32,
    lo: usize,
    hi: usize,
    len: u8,
    cell: u32,
) {
    let mut cache = ShorterCache::new(len);
    for b in lo..hi {
        let cur = groups.cells(g)[b];
        if is_group(cur) {
            let child = group_index(cur);
            fill_group(groups, routes, child, 0, 256, len, cell);
            if let Some(v) = uniform_value(groups.cells(child)) {
                groups.cells_mut(g)[b] = v;
                groups.release(child);
            }
        } else if cache.is_shorter(cur, routes) {
            groups.cells_mut(g)[b] = cell;
        }
    }
}

/// Rewrites cells equal to `old` with `new` in cells `lo..hi` of group `g`
/// and below, folding nested groups that become uniform.
fn replace_group(groups: &mut GroupPool, g: u32, lo: usize, hi: usize, old: u32, new: u32) {
    let has_children = groups.cells(g)[lo..hi].iter().any(|&c| is_group(c));
    if !has_children {
        replace_cells(&mut groups.cells_mut(g)[lo..hi], old, new);
        return;
    }
    for b in lo..hi {
        let cur = groups.cells(g)[b];
        if cur == old {
            groups.cells_mut(g)[b] = new;
        } else if is_group(cur) {
            let child = group_index(cur);
            replace_group(groups, child, 0, 256, old, new);
            if let Some(v) = uniform_value(groups.cells(child)) {
                groups.cells_mut(g)[b] = v;
                groups.release(child);
            }
        }
    }
}

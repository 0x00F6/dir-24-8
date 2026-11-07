//! Hash-table engines.
//!
//! * [`HashLpm`] (`hashmap-lpm(std)`, `hashmap-lpm(fxhash)`): one
//!   `HashMap<(K, u8), RouteId>` keyed by (masked prefix, length). The
//!   distinct lengths present are kept sorted longest first; a lookup masks
//!   the key for each length in turn and stops at the first hit (one hash
//!   probe per present length in the worst case).
//!
//! The `std` variants use the default `RandomState` (SipHash-1-3), the
//! `fxhash` variants `rustc_hash::FxBuildHasher`.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use rustc_hash::FxBuildHasher;

use super::{route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId, Updates};

/// Hasher selection with the label used in engine names.
pub trait NamedHasher: BuildHasher + Default + Send + Sync + 'static {
    const NAME: &'static str;
}

impl NamedHasher for RandomState {
    const NAME: &'static str = "std";
}

impl NamedHasher for FxBuildHasher {
    const NAME: &'static str = "fxhash";
}

/// Heap bytes of a std `HashMap` with `capacity()` `cap` and entry type `T`.
///
/// std's map is hashbrown (SwissTable): `buckets` slots of `T` plus
/// `buckets + GROUP_WIDTH` control bytes (16 on SSE2 targets) in one
/// allocation, with `capacity() == buckets - 1` for fewer than 8 buckets and
/// `buckets / 8 * 7` otherwise. The allocation's alignment padding is
/// ignored.
pub(crate) fn hashmap_bytes<T>(cap: usize) -> usize {
    if cap == 0 {
        return 0;
    }
    let buckets = if cap < 7 { cap + 1 } else { cap / 7 * 8 };
    buckets * std::mem::size_of::<T>() + buckets + 16
}

/// Hash-per-length LPM.
pub struct HashLpm<K, H> {
    map: HashMap<(K, u8), RouteId, H>,
    /// `(mask, len)` of every length present, longest first.
    lens: Vec<(K, u8)>,
    /// Number of routes per length, to maintain `lens` on updates.
    counts: Vec<u32>,
}

pub type HashLpmStd<K> = HashLpm<K, RandomState>;
pub type HashLpmFx<K> = HashLpm<K, FxBuildHasher>;

impl<K: Key, H: NamedHasher> HashLpm<K, H> {
    fn refresh_lens(&mut self) {
        self.lens.clear();
        for l in (0..=K::BITS as u8).rev() {
            if self.counts[usize::from(l)] != 0 {
                self.lens.push((K::mask(l), l));
            }
        }
    }

    fn add(&mut self, r: &Route<K>, id: RouteId) {
        if self.map.insert((r.net, r.len), id).is_none() {
            let c = &mut self.counts[usize::from(r.len)];
            *c += 1;
            if *c == 1 {
                self.refresh_lens();
            }
        }
    }
}

impl<K: Key, H: NamedHasher> LpmEngine<K> for HashLpm<K, H> {
    fn name() -> String {
        format!("hashmap-lpm({})", H::NAME)
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut e = Self {
            map: HashMap::with_capacity_and_hasher(routes.len(), H::default()),
            lens: Vec::with_capacity(K::BITS as usize + 1),
            counts: vec![0; K::BITS as usize + 1],
        };
        for (i, r) in routes.iter().enumerate() {
            e.add(r, route_id(i));
        }
        e
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        for &(mask, len) in &self.lens {
            if let Some(&id) = self.map.get(&(key.and(mask), len)) {
                return Some(id);
            }
        }
        None
    }

    fn memory_bytes(&self) -> usize {
        hashmap_bytes::<((K, u8), RouteId)>(self.map.capacity())
            + vec_bytes(&self.lens)
            + vec_bytes(&self.counts)
    }

    fn updates() -> Updates {
        Updates::Incremental
    }

    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.add(&r, id);
    }

    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        if self.map.remove(&(r.net, r.len)).is_some() {
            let c = &mut self.counts[usize::from(r.len)];
            *c -= 1;
            if *c == 0 {
                self.refresh_lens();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{incremental_suite, lpm_suite};

    #[test]
    fn hashmap_bytes_matches_hashbrown_layout() {
        for n in [1usize, 3, 7, 8, 100, 1000, 12345] {
            let m: HashMap<u32, u32> = HashMap::with_capacity(n);
            let cap = m.capacity();
            let b = hashmap_bytes::<(u32, u32)>(cap);
            assert!(b >= cap * 8, "{n}: {b} < {cap} entries");
            assert!(b <= (cap * 2 + 8) * 9 + 16, "{n}: {b}");
        }
        assert_eq!(hashmap_bytes::<(u32, u32)>(0), 0);
    }

    #[test]
    fn hash_lpm_std() {
        lpm_suite::<u32, HashLpmStd<u32>>();
        lpm_suite::<u128, HashLpmStd<u128>>();
    }

    #[test]
    fn hash_lpm_fx() {
        lpm_suite::<u32, HashLpmFx<u32>>();
        lpm_suite::<u128, HashLpmFx<u128>>();
    }

    #[test]
    fn hash_lpm_incremental() {
        incremental_suite::<u32, HashLpmStd<u32>>();
        incremental_suite::<u128, HashLpmStd<u128>>();
        incremental_suite::<u32, HashLpmFx<u32>>();
        incremental_suite::<u128, HashLpmFx<u128>>();
    }

    #[test]
    fn names() {
        assert_eq!(
            <HashLpmStd<u32> as LpmEngine<u32>>::name(),
            "hashmap-lpm(std)"
        );
        assert_eq!(
            <HashLpmFx<u32> as LpmEngine<u32>>::name(),
            "hashmap-lpm(fxhash)"
        );
    }
}

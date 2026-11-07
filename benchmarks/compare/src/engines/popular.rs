//! Adapters over popular crates (crates.io downloads in parentheses when
//! they were added): every one is used through its public API, keyed and
//! valued exactly as a user of the crate would.
//!
//! * [`RadixTrieCrate`] (`radix_trie(0.3)`, ~100 M downloads): generic radix
//!   trie (`radix_trie::Trie`). It works on byte strings, so a prefix is
//!   stored as one byte per bit (`0`/`1`, `len` bytes) and a lookup asks for
//!   the deepest ancestor (`get_ancestor_value`) of the address's bit string
//!   (32 or 128 bytes, built on the stack).
//! * [`PatriciaTreeCrate`] (`patricia_tree(0.10)`, ~2 M): generic PATRICIA
//!   (path-compressed radix) tree over byte strings, same bit-string keys,
//!   lookup by `get_longest_common_prefix`.
//! * [`IptrieRadix`] (`iptrie-radix(0.11)`) and [`IptrieLc`]
//!   (`iptrie-lctrie(0.11)`): the `iptrie` crate's IP radix trie
//!   (`RTrieMap`) and its level-compressed version (`LCTrieMap`, the LC-trie
//!   algorithm of the Linux kernel's IPv4 FIB), keyed by `Ipv4Prefix` /
//!   `Ipv6Prefix`, looked up with the address.
//! * [`RangeMapEngine`] (`rangemap(1.8)`, ~48 M): `RangeInclusiveMap` from
//!   address ranges to route ids (a `BTreeMap` of disjoint ranges). Routes
//!   are inserted from the shortest to the longest, each insertion
//!   overwriting the overlapped part, so every address maps to its LPM winner.
//! * [`IpNetworkTableEngine`] (`ip_network_table(0.2)`, ~8 M):
//!   `IpNetworkTable` (Tree Bitmap from `ip_network_table-deps-treebitmap`),
//!   `longest_match_ipv4` / `longest_match_ipv6`.
//!
//! None of these crates accounts for its memory (`REPORTS_MEMORY = false`):
//! the harness measures every structure with its counting allocator.

use std::marker::PhantomData;
use std::net::{Ipv4Addr, Ipv6Addr};

use ip_network::{Ipv4Network, Ipv6Network};
use ip_network_table::IpNetworkTable;
use iptrie::map::{LCTrieMap, RTrieMap};
use iptrie::{Ipv4Prefix, Ipv6Prefix};
use patricia_tree::PatriciaMap;
use rangemap::{RangeInclusiveMap, StepLite};

use super::{NONE, opt, route_id};
use crate::core::{Key, LpmEngine, Route, RouteId, Updates};

/// The bits of `net` (MSB first) as `0`/`1` bytes, `len` of them.
fn bit_string<K: Key>(net: K, len: u8) -> Vec<u8> {
    (0..u32::from(len)).map(|i| net.bit(i) as u8).collect()
}

/// The full bit string of `key` in `buf` (stack buffer of 128 bytes).
#[inline(always)]
fn key_bits<K: Key>(key: K, buf: &mut [u8; 128]) -> &[u8] {
    let n = K::BITS as usize;
    let v = key.to_u128() << (128 - K::BITS);
    for (i, b) in buf[..n].iter_mut().enumerate() {
        *b = (v >> (127 - i)) as u8 & 1;
    }
    &buf[..n]
}

// ------------------------------------------------------------- radix_trie

/// `radix_trie(0.3)`: generic radix trie over bit strings.
pub struct RadixTrieCrate<K> {
    trie: radix_trie::Trie<Vec<u8>, RouteId>,
    _k: PhantomData<K>,
}

impl<K: Key> LpmEngine<K> for RadixTrieCrate<K> {
    fn name() -> String {
        "radix_trie(0.3)".into()
    }
    fn build(routes: &[Route<K>]) -> Self {
        let mut trie = radix_trie::Trie::new();
        for (i, r) in routes.iter().enumerate() {
            trie.insert(bit_string(r.net, r.len), route_id(i));
        }
        Self {
            trie,
            _k: PhantomData,
        }
    }
    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let mut buf = [0u8; 128];
        self.trie
            .get_ancestor_value(key_bits(key, &mut buf))
            .copied()
    }
    fn memory_bytes(&self) -> usize {
        0
    }
    const REPORTS_MEMORY: bool = false;
    fn updates() -> Updates {
        Updates::Incremental
    }
    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = &routes[id as usize];
        self.trie.insert(bit_string(r.net, r.len), id);
    }
    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = &routes[id as usize];
        self.trie.remove(&bit_string(r.net, r.len));
    }
}

// ---------------------------------------------------------- patricia_tree

/// `patricia_tree(0.10)`: generic PATRICIA tree over bit strings.
pub struct PatriciaTreeCrate<K> {
    map: PatriciaMap<RouteId>,
    _k: PhantomData<K>,
}

impl<K: Key> LpmEngine<K> for PatriciaTreeCrate<K> {
    fn name() -> String {
        "patricia_tree(0.10)".into()
    }
    fn build(routes: &[Route<K>]) -> Self {
        let mut map = PatriciaMap::new();
        for (i, r) in routes.iter().enumerate() {
            map.insert(bit_string(r.net, r.len), route_id(i));
        }
        Self {
            map,
            _k: PhantomData,
        }
    }
    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let mut buf = [0u8; 128];
        self.map
            .get_longest_common_prefix(key_bits(key, &mut buf))
            .map(|(_, &id)| id)
    }
    fn memory_bytes(&self) -> usize {
        0
    }
    const REPORTS_MEMORY: bool = false;
    fn updates() -> Updates {
        Updates::Incremental
    }
    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = &routes[id as usize];
        self.map.insert(bit_string(r.net, r.len), id);
    }
    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = &routes[id as usize];
        self.map.remove(bit_string(r.net, r.len));
    }
}

// ----------------------------------------------------------------- iptrie

/// Prefix and address types of the `iptrie` crate for a key type.
pub trait IptrieFamily: Key {
    type Prefix: iptrie::IpPrefix;
}

impl IptrieFamily for u32 {
    type Prefix = Ipv4Prefix;
}

impl IptrieFamily for u128 {
    type Prefix = Ipv6Prefix;
}

/// `iptrie-radix(0.11)`: the `iptrie` IP radix trie (`RTrieMap`).
pub struct IptrieRadix<K: IptrieFamily> {
    map: RTrieMap<K::Prefix, RouteId>,
}

/// `iptrie-lctrie(0.11)`: the `iptrie` level-compressed trie (`LCTrieMap`).
pub struct IptrieLc<K: IptrieFamily> {
    map: LCTrieMap<K::Prefix, RouteId>,
}

macro_rules! iptrie_family {
    ($k:ty, $prefix:ty, $addr:ty) => {
        fn radix_map(routes: &[Route<$k>]) -> RTrieMap<$prefix, RouteId> {
            // The root (/0) always exists in an iptrie map: it carries the
            // "no route" marker unless a /0 route replaces it.
            let mut map = RTrieMap::with_root_and_capacity(NONE, routes.len());
            for (i, r) in routes.iter().enumerate() {
                let p = <$prefix>::new(<$addr>::from(r.net), r.len).expect("valid prefix");
                map.insert(p, route_id(i));
            }
            map
        }

        impl LpmEngine<$k> for IptrieRadix<$k> {
            fn name() -> String {
                "iptrie-radix(0.11)".into()
            }
            fn build(routes: &[Route<$k>]) -> Self {
                Self {
                    map: radix_map(routes),
                }
            }
            #[inline]
            fn lookup(&self, key: $k) -> Option<RouteId> {
                opt(*self.map.lookup(&<$addr>::from(key)).1)
            }
            fn memory_bytes(&self) -> usize {
                0
            }
            const REPORTS_MEMORY: bool = false;
        }

        impl LpmEngine<$k> for IptrieLc<$k> {
            fn name() -> String {
                "iptrie-lctrie(0.11)".into()
            }
            fn build(routes: &[Route<$k>]) -> Self {
                Self {
                    map: radix_map(routes).compress(),
                }
            }
            #[inline]
            fn lookup(&self, key: $k) -> Option<RouteId> {
                opt(*self.map.lookup(&<$addr>::from(key)).1)
            }
            fn memory_bytes(&self) -> usize {
                0
            }
            const REPORTS_MEMORY: bool = false;
        }
    };
}

mod iptrie_v4 {
    use super::*;
    iptrie_family!(u32, Ipv4Prefix, Ipv4Addr);
}

mod iptrie_v6 {
    use super::*;
    iptrie_family!(u128, Ipv6Prefix, Ipv6Addr);
}

// --------------------------------------------------------------- rangemap

/// `rangemap(1.8)`: map of disjoint address ranges to LPM winners.
pub struct RangeMapEngine<K: Ord + Clone> {
    map: RangeInclusiveMap<K, RouteId>,
}

impl<K: Key + StepLite> LpmEngine<K> for RangeMapEngine<K> {
    fn name() -> String {
        "rangemap(1.8)".into()
    }
    fn build(routes: &[Route<K>]) -> Self {
        let mut order: Vec<usize> = (0..routes.len()).collect();
        order.sort_by_key(|&i| routes[i].len);
        let mut map = RangeInclusiveMap::new();
        for i in order {
            let r = &routes[i];
            map.insert(r.net..=K::last(r.net, r.len), route_id(i));
        }
        Self { map }
    }
    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        self.map.get(&key).copied()
    }
    fn memory_bytes(&self) -> usize {
        0
    }
    const REPORTS_MEMORY: bool = false;
}

// ------------------------------------------------------- ip_network_table

/// `ip_network_table(0.2)`: `IpNetworkTable` (Tree Bitmap).
pub struct IpNetworkTableEngine<K> {
    table: IpNetworkTable<RouteId>,
    _k: PhantomData<K>,
}

impl LpmEngine<u32> for IpNetworkTableEngine<u32> {
    fn name() -> String {
        "ip_network_table(0.2)".into()
    }
    fn build(routes: &[Route<u32>]) -> Self {
        let mut table = IpNetworkTable::new();
        for (i, r) in routes.iter().enumerate() {
            let n = Ipv4Network::new(Ipv4Addr::from(r.net), r.len).expect("valid network");
            table.insert(n, route_id(i));
        }
        Self {
            table,
            _k: PhantomData,
        }
    }
    #[inline]
    fn lookup(&self, key: u32) -> Option<RouteId> {
        self.table
            .longest_match_ipv4(Ipv4Addr::from(key))
            .map(|(_, &id)| id)
    }
    fn memory_bytes(&self) -> usize {
        0
    }
    const REPORTS_MEMORY: bool = false;
}

impl LpmEngine<u128> for IpNetworkTableEngine<u128> {
    fn name() -> String {
        "ip_network_table(0.2)".into()
    }
    fn build(routes: &[Route<u128>]) -> Self {
        let mut table = IpNetworkTable::new();
        for (i, r) in routes.iter().enumerate() {
            let n = Ipv6Network::new(Ipv6Addr::from(r.net), r.len).expect("valid network");
            table.insert(n, route_id(i));
        }
        Self {
            table,
            _k: PhantomData,
        }
    }
    #[inline]
    fn lookup(&self, key: u128) -> Option<RouteId> {
        self.table
            .longest_match_ipv6(Ipv6Addr::from(key))
            .map(|(_, &id)| id)
    }
    fn memory_bytes(&self) -> usize {
        0
    }
    const REPORTS_MEMORY: bool = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{incremental_suite, lpm_suite};

    #[test]
    fn radix_trie_crate() {
        lpm_suite::<u32, RadixTrieCrate<u32>>();
        lpm_suite::<u128, RadixTrieCrate<u128>>();
        incremental_suite::<u32, RadixTrieCrate<u32>>();
    }

    #[test]
    fn patricia_tree_crate() {
        lpm_suite::<u32, PatriciaTreeCrate<u32>>();
        lpm_suite::<u128, PatriciaTreeCrate<u128>>();
        incremental_suite::<u32, PatriciaTreeCrate<u32>>();
    }

    #[test]
    fn iptrie() {
        lpm_suite::<u32, IptrieRadix<u32>>();
        lpm_suite::<u128, IptrieRadix<u128>>();
        lpm_suite::<u32, IptrieLc<u32>>();
        lpm_suite::<u128, IptrieLc<u128>>();
    }

    #[test]
    fn rangemap() {
        lpm_suite::<u32, RangeMapEngine<u32>>();
        lpm_suite::<u128, RangeMapEngine<u128>>();
    }

    #[test]
    fn ip_network_table() {
        lpm_suite::<u32, IpNetworkTableEngine<u32>>();
        lpm_suite::<u128, IpNetworkTableEngine<u128>>();
    }

    #[test]
    fn bit_strings() {
        assert_eq!(bit_string(0xA000_0000u32, 4), vec![1, 0, 1, 0]);
        let mut buf = [0u8; 128];
        let b = key_bits(0x8000_0001u32, &mut buf);
        assert_eq!(b.len(), 32);
        assert_eq!((b[0], b[1], b[30], b[31]), (1, 0, 0, 1));
        let b = key_bits(1u128, &mut buf);
        assert_eq!((b.len(), b[127], b[0]), (128, 1, 0));
    }
}

//! Adapters over recognized LPM crates.
//!
//! * [`PrefixTrieEngine`] (`prefix-trie(0.10)`): `prefix_trie::PrefixMap`
//!   keyed by `ipnet::Ipv4Net` / `Ipv6Net`; lookup = `get_lpm` on the host
//!   prefix (`/32`, `/128`) of the key. The crate's map is a tree-bitmap
//!   style multibit trie (5 binary levels per node).
//! * [`TreeBitmapCrate`] (`treebitmap-crate(0.5)`):
//!   `ip_network_table_deps_treebitmap::IpLookupTable` keyed by
//!   `Ipv4Addr` / `Ipv6Addr` (`insert(ip, masklen, value)`,
//!   `longest_match(ip)`), a tree bitmap with stride 4.
//!
//! Both store the `RouteId` as the value and are updated in place. Their
//! `memory_bytes` come from the crates' own accounting (`mem_size`,
//! `mem_usage`), which is `capacity * size_of` of their node and value
//! arrays (plus free lists).

use std::net::{Ipv4Addr, Ipv6Addr};

use ip_network_table_deps_treebitmap::IpLookupTable;
use ip_network_table_deps_treebitmap::address::Address;
use ipnet::{Ipv4Net, Ipv6Net};
use prefix_trie::{Prefix, PrefixMap};

use super::route_id;
use crate::core::{Key, LpmEngine, Route, RouteId, Updates};

/// Maps a key type to the address / network types of the external crates.
pub trait IpFamily: Key {
    type Net: Prefix + Copy;
    type Addr: Address;
    fn net(net: Self, len: u8) -> Self::Net;
    fn addr(self) -> Self::Addr;
}

impl IpFamily for u32 {
    type Net = Ipv4Net;
    type Addr = Ipv4Addr;

    #[inline(always)]
    fn net(net: Self, len: u8) -> Ipv4Net {
        Ipv4Net::new_assert(Ipv4Addr::from(net), len)
    }

    #[inline(always)]
    fn addr(self) -> Ipv4Addr {
        Ipv4Addr::from(self)
    }
}

impl IpFamily for u128 {
    type Net = Ipv6Net;
    type Addr = Ipv6Addr;

    #[inline(always)]
    fn net(net: Self, len: u8) -> Ipv6Net {
        Ipv6Net::new_assert(Ipv6Addr::from(net), len)
    }

    #[inline(always)]
    fn addr(self) -> Ipv6Addr {
        Ipv6Addr::from(self)
    }
}

/// `prefix-trie(0.10)`.
pub struct PrefixTrieEngine<K: IpFamily> {
    map: PrefixMap<K::Net, RouteId>,
}

impl<K: IpFamily> LpmEngine<K> for PrefixTrieEngine<K> {
    fn name() -> String {
        "prefix-trie(0.10)".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut map = PrefixMap::new();
        for (i, r) in routes.iter().enumerate() {
            map.insert(K::net(r.net, r.len), route_id(i));
        }
        Self { map }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        self.map
            .get_lpm(&K::net(key, K::BITS as u8))
            .map(|(_, &id)| id)
    }

    /// `PrefixMap::mem_size()`: node and value arrays by capacity, free
    /// lists and the map header.
    fn memory_bytes(&self) -> usize {
        self.map.mem_size()
    }

    fn updates() -> Updates {
        Updates::Incremental
    }

    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.map.insert(K::net(r.net, r.len), id);
    }

    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.map.remove(&K::net(r.net, r.len));
    }
}

/// `treebitmap-crate(0.5)`.
pub struct TreeBitmapCrate<K: IpFamily> {
    table: IpLookupTable<K::Addr, RouteId>,
}

impl<K: IpFamily> LpmEngine<K> for TreeBitmapCrate<K> {
    fn name() -> String {
        "treebitmap-crate(0.5)".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut table = IpLookupTable::new();
        for (i, r) in routes.iter().enumerate() {
            table.insert(r.net.addr(), u32::from(r.len), route_id(i));
        }
        Self { table }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        self.table.longest_match(key.addr()).map(|(_, _, &id)| id)
    }

    /// `IpLookupTable::mem_usage()`: node and result allocators by capacity
    /// (bucket vectors plus free lists), plus the table header.
    fn memory_bytes(&self) -> usize {
        let (nodes, results) = self.table.mem_usage();
        nodes + results + std::mem::size_of::<Self>()
    }

    fn updates() -> Updates {
        Updates::Incremental
    }

    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.table.insert(r.net.addr(), u32::from(r.len), id);
    }

    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.table.remove(r.net.addr(), u32::from(r.len));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{incremental_suite, lpm_suite};

    #[test]
    fn prefix_trie_v4() {
        lpm_suite::<u32, PrefixTrieEngine<u32>>();
    }

    #[test]
    fn prefix_trie_v6() {
        lpm_suite::<u128, PrefixTrieEngine<u128>>();
    }

    #[test]
    fn prefix_trie_incremental() {
        incremental_suite::<u32, PrefixTrieEngine<u32>>();
        incremental_suite::<u128, PrefixTrieEngine<u128>>();
    }

    #[test]
    fn treebitmap_crate_v4() {
        lpm_suite::<u32, TreeBitmapCrate<u32>>();
    }

    #[test]
    fn treebitmap_crate_v6() {
        lpm_suite::<u128, TreeBitmapCrate<u128>>();
    }

    #[test]
    fn treebitmap_crate_incremental() {
        incremental_suite::<u32, TreeBitmapCrate<u32>>();
        incremental_suite::<u128, TreeBitmapCrate<u128>>();
    }
}

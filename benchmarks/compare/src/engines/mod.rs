//! Competitor implementations compared against `dir-24-8`.
//!
//! Every engine below is either an own implementation of a published
//! algorithm (generic over [`Key`](crate::core::Key), one implementation
//! serving IPv4 `u32` and IPv6 `u128`) or a thin adapter over a recognized
//! crate. All own implementations use arena `Vec`s with `u32` indices, never
//! allocate in their lookup path and report `memory_bytes()` as
//! `capacity() * size_of` of everything they own (the shared route slice is
//! excluded).
//!
//! | Name | Algorithm | Configuration | Updates | Families | Semantics |
//! |------|-----------|---------------|---------|----------|-----------|
//! | `radix-trie(binary)` | binary (unibit) trie | 1 bit per level, nodes `{child: [u32; 2], route: u32}` | incremental (remove clears the mark) | ipv4, ipv6 | lpm |
//! | `head(2^24)+radix-trie(binary)` | head table + binary suffix tries | 2^24 head entries indexed by the first 24 bits (routes <= /24 prefix-expanded), binary radix-trie walk from bit 24 | rebuild | ipv4, ipv6 | lpm |
//! | `head(2^20)+radix-trie(binary)` | head table + binary suffix tries | same, k = 20 | rebuild | ipv4, ipv6 | lpm |
//! | `head(2^18)+radix-trie(binary)` | head table + binary suffix tries | same, k = 18 | rebuild | ipv4, ipv6 | lpm |
//! | `head(2^16)+radix-trie(binary)` | head table + binary suffix tries | same, k = 16 | rebuild | ipv4, ipv6 | lpm |
//! | `radix-trie(stride=4,16-way)` | multibit trie, controlled prefix expansion | fixed stride 4, 16 slots `{route, child}` per node, nodes on demand | rebuild | ipv4, ipv6 | lpm |
//! | `radix-trie(stride=8,256-way)` | multibit trie, controlled prefix expansion | fixed stride 8, 256 slots `{route, child}` per node, nodes on demand | rebuild | ipv4, ipv6 | lpm |
//! | `radix-tree(patricia)` | path-compressed binary trie | nodes store key bits + length, split nodes on insert | incremental (remove clears the mark) | ipv4, ipv6 | lpm |
//! | `tree-bitmap(stride=4)` | Tree Bitmap (Eatherton, Varghese, Dittia 2004) | stride 4, 15-bit internal + 16-bit external bitmaps, 12-byte nodes, popcount ranks, BFS layout | rebuild | ipv4, ipv6 | lpm |
//! | `poptrie(s=16,k=6)` | Poptrie (Asai, Ohara 2015) | direct pointing on 16 bits, stride 6, `vector`/`leafvec` popcount, leaf-pushed u32 leaves | rebuild | ipv4, ipv6 | lpm |
//! | `poptrie(s=18,k=6)` | Poptrie (Asai, Ohara 2015) | direct pointing on 18 bits (Poptrie18), stride 6 | rebuild | ipv4, ipv6 | lpm |
//! | `hashmap-lpm(std)` | one hash probe per present length | `HashMap<(K, u8), RouteId>`, SipHash-1-3, lengths longest first | incremental | ipv4, ipv6 | lpm |
//! | `hashmap-lpm(fxhash)` | one hash probe per present length | same with `rustc_hash::FxBuildHasher` | incremental | ipv4, ipv6 | lpm |
//! | `sorted-intervals` | binary search over disjoint LPM segments | `starts`/`ends`/`winners` arrays, `partition_point` | rebuild | ipv4, ipv6 | lpm |
//! | `prefix-trie(0.10)` | crate `prefix-trie` `PrefixMap` (tree-bitmap style multibit trie) | `PrefixMap<Ipv4Net/Ipv6Net, RouteId>`, `get_lpm` | incremental | ipv4, ipv6 | lpm |
//! | `treebitmap-crate(0.5)` | crate `ip_network_table-deps-treebitmap` `IpLookupTable` (tree bitmap) | `IpLookupTable<Ipv4Addr/Ipv6Addr, RouteId>`, `longest_match` | incremental | ipv4, ipv6 | lpm |
//! | `bitmap-dense(2^32-bit)` | one bit per IPv4 address | `Vec<u64>` of 2^26 words (512 MiB, zeroed lazily mapped allocation) | rebuild | ipv4 | membership |
//! | `radix_trie(0.3)` | generic radix trie (crate `radix_trie`) | prefixes as bit strings (one byte per bit), `get_ancestor_value` | incremental | ipv4, ipv6 | lpm |
//! | `patricia_tree(0.10)` | generic PATRICIA radix tree (crate `patricia_tree`) | bit-string keys, `get_longest_common_prefix` | incremental | ipv4, ipv6 | lpm |
//! | `iptrie-radix(0.11)` | IP radix tree (crate `iptrie`, `RTrieMap`) | root /0 = no-route marker, `lookup(address)` | incremental | ipv4, ipv6 | lpm |
//! | `iptrie-lctrie(0.11)` | LC-trie (crate `iptrie`, `LCTrieMap`) | `RTrieMap::compress()`, `lookup(address)` | rebuild | ipv4, ipv6 | lpm |
//! | `rangemap(1.8)` | range map (crate `rangemap`) | `RangeInclusiveMap`, routes inserted shortest first | rebuild | ipv4, ipv6 | lpm |
//! | `ip_network_table(0.2)` | Tree Bitmap (crate `ip_network_table`) | `IpNetworkTable`, `longest_match_ipv4/ipv6` | incremental | ipv4, ipv6 | lpm |
//!
//! The popcount-based engines (Tree Bitmap, Poptrie) use the hardware
//! `popcnt` instruction: statically when the binary is compiled with it
//! (e.g. `-C target-cpu=native`), otherwise through a runtime-detected
//! `#[target_feature(enable = "popcnt")]` clone of their lookup (see
//! `popcnt_dispatch`). The external crates only get `popcnt` from the
//! compile flags.
//!
//! The same data is available programmatically through [`catalog`].

mod bintrie;
mod bitmap;
mod external;
mod hashlpm;
mod intervals;
mod patricia;
mod poptrie;
mod popular;
mod radix;
mod treebitmap;

#[cfg(test)]
mod testutil;

// Generic types and helper traits are re-exported for the harness even where
// only their aliases are used inside this module.
#[allow(unused_imports)]
pub use bintrie::{BinTrie, HeadBin16, HeadBin18, HeadBin20, HeadBin24, HeadBinTrie};
pub use bitmap::DenseBitmap;
#[allow(unused_imports)]
pub use external::{IpFamily, PrefixTrieEngine, TreeBitmapCrate};
#[allow(unused_imports)]
pub use hashlpm::{HashLpm, HashLpmFx, HashLpmStd, NamedHasher};
pub use intervals::SortedIntervals;
pub use patricia::Patricia;
#[allow(unused_imports)]
pub use poptrie::{Poptrie, Poptrie16, Poptrie18};
#[allow(unused_imports)]
pub use popular::{
    IpNetworkTableEngine, IptrieFamily, IptrieLc, IptrieRadix, PatriciaTreeCrate, RadixTrieCrate,
    RangeMapEngine,
};
#[allow(unused_imports)]
pub use radix::{Radix, Radix4, Radix8};
pub use treebitmap::TreeBitmap;

use crate::core::{LpmEngine, MembershipEngine, RouteId};

/// "No route" marker stored in nodes, slots and leaves.
pub(crate) const NONE: u32 = u32::MAX;

/// Converts an internal route marker to the public result.
#[inline(always)]
pub(crate) fn opt(id: u32) -> Option<RouteId> {
    if id == NONE { None } else { Some(id) }
}

/// Heap bytes of a `Vec` (capacity, not length).
#[inline]
pub(crate) fn vec_bytes<T>(v: &Vec<T>) -> usize {
    v.capacity() * std::mem::size_of::<T>()
}

/// `true` when lookups should go through their
/// `#[target_feature(enable = "popcnt")]` clone: the binary is not compiled
/// with `popcnt` (e.g. no `-C target-cpu=native`) but the CPU has it. The
/// popcount-based engines (Tree Bitmap, Poptrie) are designed around the
/// hardware instruction; with `popcnt` enabled at compile time this is a
/// constant `false` and the dispatch disappears. The detection result is
/// cached by std (one relaxed atomic load per call).
#[inline(always)]
pub(crate) fn popcnt_dispatch() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        !cfg!(target_feature = "popcnt") && std::arch::is_x86_feature_detected!("popcnt")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Converts a route index into a [`RouteId`], panicking on overflow.
#[inline]
pub(crate) fn route_id(i: usize) -> u32 {
    let id = u32::try_from(i).expect("route index exceeds u32");
    assert!(id != NONE, "route index collides with the NONE marker");
    id
}

/// One row of the engine catalog (reused by the report generator).
#[derive(Clone, Debug)]
pub struct EngineInfo {
    /// Exact `name()` of the engine.
    pub name: String,
    /// Algorithm family.
    pub algorithm: &'static str,
    /// Configuration / layout details.
    pub config: String,
    /// `"incremental"` or `"rebuild"` (with a short qualifier when relevant).
    pub updates: &'static str,
    /// `"own implementation of <paper/algorithm>"` or `"crate <name> <version>"`.
    pub source: &'static str,
}

fn lpm_name<E: LpmEngine<u32>>() -> String {
    E::name()
}

fn membership_name<E: MembershipEngine<u32>>() -> String {
    E::name()
}

/// Every engine of this module with its algorithm, configuration, update
/// mode, families, semantics and provenance.
pub fn catalog() -> Vec<EngineInfo> {
    let mut v = vec![EngineInfo {
        name: lpm_name::<BinTrie<u32>>(),
        algorithm: "binary radix trie (radix 2, one bit per level, uncompressed)",
        config: "1 bit per level; arena nodes {child: [u32; 2], route: u32} (12 B); \
                 walk of up to W levels remembering the best route"
            .into(),
        updates: "incremental (remove clears the route mark, nodes stay)",
        source: "own implementation of the classic binary trie",
    }];
    for k in [24u32, 20, 18, 16] {
        let name = match k {
            24 => lpm_name::<HeadBin24<u32>>(),
            20 => lpm_name::<HeadBin20<u32>>(),
            18 => lpm_name::<HeadBin18<u32>>(),
            _ => lpm_name::<HeadBin16<u32>>(),
        };
        v.push(EngineInfo {
            name,
            algorithm: "direct-indexed head table + binary suffix tries",
            config: format!(
                "2^{k} head entries {{route: u32, root: u32}} indexed by the first {k} bits; \
                 routes <= /{k} prefix-expanded (longest wins); longer routes in per-entry \
                 binary suffix tries (same node arena and walk as radix-trie(binary), from bit {k})"
            ),
            updates: "rebuild",
            source: "own implementation of a head table + binary trie (DIR-style split)",
        });
    }
    for s in [4u32, 8] {
        let name = if s == 4 {
            lpm_name::<Radix4<u32>>()
        } else {
            lpm_name::<Radix8<u32>>()
        };
        v.push(EngineInfo {
            name,
            algorithm: "multibit radix trie (radix 2^stride) with controlled prefix expansion",
            config: format!(
                "stride {s}, {} slots {{route: u32, child: u32}} per node, nodes allocated on \
                 demand, routes expanded inside their node (longer wins), best route kept per level",
                1u32 << s
            ),
            updates: "rebuild",
            source: "own implementation of controlled prefix expansion (Srinivasan & Varghese, 1999)",
        });
    }
    v.push(EngineInfo {
        name: lpm_name::<Patricia<u32>>(),
        algorithm: "radix tree: path-compressed binary radix trie (PATRICIA, as in BSD routing tables)",
        config: "nodes {key bits, bit length, route, child: [u32; 2]}; split nodes created on \
                 insert; lookup verifies the node prefix (xor + leading_zeros) before descending"
            .into(),
        updates: "incremental (remove clears the route mark, nodes stay)",
        source: "own implementation of the PATRICIA / path-compressed trie (Morrison, 1968)",
    });
    v.push(EngineInfo {
        name: lpm_name::<TreeBitmap<u32>>(),
        algorithm: "Tree Bitmap",
        config: "stride 4; node {internal: u16 (15 prefix bits), external: u16 (16 child bits), \
                 child_base: u32, result_base: u32} (12 B); contiguous children and results \
                 (BFS layout); hardware popcnt ranks (runtime-dispatched if not compiled in); deferred \
                 result access"
            .into(),
        updates: "rebuild",
        source: "own implementation of Tree Bitmap (Eatherton, Varghese, Dittia, CCR 2004)",
    });
    for s in [16u32, 18] {
        let name = if s == 16 {
            lpm_name::<Poptrie16<u32>>()
        } else {
            lpm_name::<Poptrie18<u32>>()
        };
        v.push(EngineInfo {
            name,
            algorithm: "Poptrie",
            config: format!(
                "direct pointing on the first {s} bits (leaf flag + route, or node index); \
                 stride k=6; node {{vector: u64, leafvec: u64, base0: u32, base1: u32}} (24 B); \
                 leaf pushing + leaf compression; u32 leaves (RouteId); hardware popcnt \
                 (runtime-dispatched if not compiled in)"
            ),
            updates: "rebuild",
            source: "own implementation of Poptrie (Asai & Ohara, SIGCOMM 2015)",
        });
    }
    v.push(EngineInfo {
        name: lpm_name::<HashLpmStd<u32>>(),
        algorithm: "hash table keyed by (masked prefix, length), one probe per present length",
        config: "std HashMap<(K, u8), RouteId> with the default SipHash-1-3 hasher; distinct \
                 lengths kept sorted longest first; stops at the first hit"
            .into(),
        updates: "incremental",
        source: "own implementation of hash-per-length LPM over std HashMap",
    });
    v.push(EngineInfo {
        name: lpm_name::<HashLpmFx<u32>>(),
        algorithm: "hash table keyed by (masked prefix, length), one probe per present length",
        config: "std HashMap<(K, u8), RouteId> with rustc_hash::FxBuildHasher; distinct lengths \
                 kept sorted longest first; stops at the first hit"
            .into(),
        updates: "incremental",
        source: "own implementation of hash-per-length LPM over std HashMap + crate rustc-hash 2",
    });
    v.push(EngineInfo {
        name: lpm_name::<SortedIntervals<u32>>(),
        algorithm: "binary search over disjoint LPM segments",
        config: "routes compiled into disjoint inclusive segments [start, end] carrying the LPM \
                 winner (adjacent equal winners merged, uncovered gaps absent); separate \
                 starts/ends/winners arrays; partition_point then end check"
            .into(),
        updates: "rebuild",
        source: "own implementation of range-based LPM (interval compilation + binary search)",
    });
    v.push(EngineInfo {
        name: lpm_name::<PrefixTrieEngine<u32>>(),
        algorithm: "prefix-trie PrefixMap (tree-bitmap style multibit trie, 5 levels per node)",
        config: "PrefixMap<Ipv4Net | Ipv6Net, RouteId>; insert / remove / get_lpm".into(),
        updates: "incremental",
        source: "crate prefix-trie 0.10",
    });
    v.push(EngineInfo {
        name: lpm_name::<TreeBitmapCrate<u32>>(),
        algorithm: "IpLookupTable (Tree Bitmap, stride 4)",
        config: "IpLookupTable<Ipv4Addr | Ipv6Addr, RouteId>; insert / remove / longest_match"
            .into(),
        updates: "incremental",
        source: "crate ip_network_table-deps-treebitmap 0.5",
    });
    v.push(EngineInfo {
        name: membership_name::<DenseBitmap>(),
        algorithm: "dense bitmap, one bit per IPv4 address",
        config: "Vec<u64> of 2^26 words (512 MiB) from a zeroed (lazily mapped) allocation; \
                 whole-word range fills; contains = one bit test"
            .into(),
        updates: "rebuild",
        source: "own implementation of a dense address bitmap",
    });
    let popular: [(String, &'static str, &str, &'static str, &'static str); 6] = [
        (
            lpm_name::<RadixTrieCrate<u32>>(),
            "radix trie (generic, byte keys, 16-way nibble nodes with path compression)",
            "radix_trie::Trie<Vec<u8>, RouteId>; a prefix is stored as one byte per bit (len \
             bytes of 0/1), lookup = get_ancestor_value of the address bit string (32/128 bytes \
             built on the stack per lookup)",
            "incremental",
            "crate radix_trie 0.3",
        ),
        (
            lpm_name::<PatriciaTreeCrate<u32>>(),
            "radix tree (PATRICIA, generic byte keys)",
            "patricia_tree::PatriciaMap<RouteId>; same one-byte-per-bit keys, lookup = \
             get_longest_common_prefix of the address bit string",
            "incremental",
            "crate patricia_tree 0.10",
        ),
        (
            lpm_name::<IptrieRadix<u32>>(),
            "radix tree for IP prefixes (iptrie RTrieMap, path-compressed binary trie)",
            "RTrieMap<Ipv4Prefix | Ipv6Prefix, RouteId> with the root (/0) set to the no-route \
             marker; lookup(address)",
            "incremental",
            "crate iptrie 0.11",
        ),
        (
            lpm_name::<IptrieLc<u32>>(),
            "LC-trie (level-compressed trie, the Linux kernel IPv4 FIB algorithm)",
            "LCTrieMap<Ipv4Prefix | Ipv6Prefix, RouteId> = RTrieMap::compress(); lookup(address)",
            "rebuild",
            "crate iptrie 0.11",
        ),
        (
            lpm_name::<RangeMapEngine<u32>>(),
            "range map (BTreeMap of disjoint address ranges)",
            "rangemap::RangeInclusiveMap<K, RouteId>; routes inserted shortest first, longer ones \
             overwrite the overlapped range, so each address maps to its LPM winner; get",
            "rebuild",
            "crate rangemap 1.8",
        ),
        (
            lpm_name::<IpNetworkTableEngine<u32>>(),
            "Tree Bitmap (ip_network_table::IpNetworkTable)",
            "IpNetworkTable<RouteId>; insert(Ipv4Network | Ipv6Network), longest_match_ipv4 / \
             longest_match_ipv6",
            "incremental",
            "crate ip_network_table 0.2",
        ),
    ];
    for (name, algorithm, config, updates, source) in popular {
        v.push(EngineInfo {
            name,
            algorithm,
            config: config.into(),
            updates,
            source,
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_are_unique_and_complete() {
        let c = catalog();
        assert_eq!(c.len(), 23);
        let mut names: Vec<&str> = c.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), c.len());
        for expected in [
            "radix-trie(binary)",
            "head(2^24)+radix-trie(binary)",
            "head(2^20)+radix-trie(binary)",
            "head(2^18)+radix-trie(binary)",
            "head(2^16)+radix-trie(binary)",
            "radix-trie(stride=4,16-way)",
            "radix-trie(stride=8,256-way)",
            "radix-tree(patricia)",
            "tree-bitmap(stride=4)",
            "poptrie(s=16,k=6)",
            "poptrie(s=18,k=6)",
            "hashmap-lpm(std)",
            "hashmap-lpm(fxhash)",
            "sorted-intervals",
            "prefix-trie(0.10)",
            "treebitmap-crate(0.5)",
            "bitmap-dense(2^32-bit)",
            "radix_trie(0.3)",
            "patricia_tree(0.10)",
            "iptrie-radix(0.11)",
            "iptrie-lctrie(0.11)",
            "rangemap(1.8)",
            "ip_network_table(0.2)",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }
}

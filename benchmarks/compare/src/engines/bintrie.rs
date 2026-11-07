//! Binary (unibit) tries.
//!
//! * [`BinTrie`] (`radix-trie(binary)`): the classic binary trie, one bit per
//!   level, walked from the root while remembering the last route seen.
//! * [`HeadBinTrie`] (`head(2^k)+radix-trie(binary)`): a direct-indexed head table of
//!   `2^k` entries indexed by the first `k` key bits. Routes of length `<= k`
//!   are prefix-expanded into the head (longest wins); longer routes live in
//!   per-entry binary suffix tries that use the very same node arena and walk
//!   function as [`BinTrie`], started at bit `k`. The variants only differ by
//!   the const generic `KB`.

use std::marker::PhantomData;

use super::{NONE, opt, route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId, Updates};

/// Binary trie node. `child[b] == 0` means "no child" (index 0 is the root
/// or a reserved dummy and is never anybody's child); `route == NONE` means
/// no route ends here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BinNode {
    pub child: [u32; 2],
    pub route: u32,
}

impl BinNode {
    pub const EMPTY: Self = Self {
        child: [0, 0],
        route: NONE,
    };

    #[inline(always)]
    pub fn is_leaf(&self) -> bool {
        self.child == [0, 0]
    }
}

/// Node arena shared by [`BinTrie`], the suffix tries of [`HeadBinTrie`] and
/// the temporary tries of the static builders. Node 0 is created up front.
#[derive(Clone, Debug)]
pub(crate) struct BinArena {
    pub nodes: Vec<BinNode>,
}

impl BinArena {
    pub fn new() -> Self {
        Self {
            nodes: vec![BinNode::EMPTY],
        }
    }

    pub fn alloc(&mut self) -> u32 {
        let i = u32::try_from(self.nodes.len()).expect("binary trie exceeds u32 nodes");
        self.nodes.push(BinNode::EMPTY);
        i
    }

    /// Returns the node for bits `from..len` of `net` below `start` (a node
    /// at depth `from`), creating the missing nodes.
    pub fn insert_path<K: Key>(&mut self, start: u32, from: u32, net: K, len: u32) -> u32 {
        let mut n = start;
        for depth in from..len {
            let b = net.bit(depth) as usize;
            let c = self.nodes[n as usize].child[b];
            n = if c != 0 {
                c
            } else {
                let c = self.alloc();
                self.nodes[n as usize].child[b] = c;
                c
            };
        }
        n
    }

    /// Node for bits `from..len` of `net` below `start`, if it exists.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn find<K: Key>(&self, start: u32, from: u32, net: K, len: u32) -> Option<u32> {
        let mut n = start;
        for depth in from..len {
            n = self.nodes[n as usize].child[net.bit(depth) as usize];
            if n == 0 {
                return None;
            }
        }
        Some(n)
    }

    /// Inserts `net/len -> id` (the arena root, node 0, is at depth 0).
    pub fn insert_route<K: Key>(&mut self, net: K, len: u8, id: u32) {
        let n = self.insert_path(0, 0, net, u32::from(len));
        self.nodes[n as usize].route = id;
    }

    /// Longest-prefix walk starting at node `start` sitting at depth `from`,
    /// with `best` inherited from above. No depth check is needed: nodes at
    /// depth `K::BITS` never have children, and `Key::bit` reads 0 past the
    /// end of the key.
    #[inline(always)]
    pub fn walk<K: Key>(&self, start: u32, from: u32, key: K, mut best: u32) -> u32 {
        let nodes = self.nodes.as_slice();
        let mut n = start as usize;
        let mut depth = from;
        loop {
            let node = &nodes[n];
            if node.route != NONE {
                best = node.route;
            }
            let c = node.child[key.bit(depth) as usize];
            if c == 0 {
                return best;
            }
            n = c as usize;
            depth += 1;
        }
    }

    pub fn bytes(&self) -> usize {
        vec_bytes(&self.nodes)
    }
}

/// `radix-trie(binary)`: classic binary trie.
pub struct BinTrie<K> {
    arena: BinArena,
    _key: PhantomData<K>,
}

impl<K: Key> LpmEngine<K> for BinTrie<K> {
    fn name() -> String {
        "radix-trie(binary)".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut arena = BinArena::new();
        for (i, r) in routes.iter().enumerate() {
            arena.insert_route(r.net, r.len, route_id(i));
        }
        arena.nodes.shrink_to_fit();
        Self {
            arena,
            _key: PhantomData,
        }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        opt(self.arena.walk(0, 0, key, NONE))
    }

    fn memory_bytes(&self) -> usize {
        self.arena.bytes()
    }

    fn updates() -> Updates {
        Updates::Incremental
    }

    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.arena.insert_route(r.net, r.len, id);
    }

    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        if let Some(n) = self.arena.find(0, 0, r.net, u32::from(r.len)) {
            let node = &mut self.arena.nodes[n as usize];
            if node.route == id {
                node.route = NONE;
            }
        }
    }
}

/// Head table entry: best route of length `<= KB` covering the entry and the
/// root of the binary suffix trie for longer routes (0 = none).
#[derive(Clone, Copy, Debug)]
struct HeadEntry {
    route: u32,
    root: u32,
}

/// `head(2^KB)+radix-trie(binary)`: head table of `2^KB` entries + binary suffix tries.
pub struct HeadBinTrie<K, const KB: u32> {
    head: Vec<HeadEntry>,
    /// Suffix tries; node 0 is a reserved dummy so that `root == 0` means
    /// "no suffix trie".
    arena: BinArena,
    _key: PhantomData<K>,
}

pub type HeadBin24<K> = HeadBinTrie<K, 24>;
pub type HeadBin20<K> = HeadBinTrie<K, 20>;
pub type HeadBin18<K> = HeadBinTrie<K, 18>;
pub type HeadBin16<K> = HeadBinTrie<K, 16>;

impl<K: Key, const KB: u32> LpmEngine<K> for HeadBinTrie<K, KB> {
    fn name() -> String {
        format!("head(2^{KB})+radix-trie(binary)")
    }

    fn build(routes: &[Route<K>]) -> Self {
        const { assert!(KB >= 1 && KB <= 24, "head size out of range") };
        assert!(KB <= K::BITS);
        let mut head = vec![
            HeadEntry {
                route: NONE,
                root: 0
            };
            1usize << KB
        ];
        let mut arena = BinArena::new();

        // Prefix expansion of the short routes, shortest first so that
        // longer (more specific) routes overwrite.
        let mut short: Vec<usize> = (0..routes.len())
            .filter(|&i| u32::from(routes[i].len) <= KB)
            .collect();
        short.sort_by_key(|&i| routes[i].len);
        for i in short {
            let r = routes[i];
            let len = u32::from(r.len);
            let start = r.net.bits(0, KB) as usize;
            let count = 1usize << (KB - len);
            for e in &mut head[start..start + count] {
                e.route = route_id(i);
            }
        }

        // Longer routes go into the suffix trie of their head entry.
        for (i, r) in routes.iter().enumerate() {
            let len = u32::from(r.len);
            if len <= KB {
                continue;
            }
            let e = r.net.bits(0, KB) as usize;
            if head[e].root == 0 {
                head[e].root = arena.alloc();
            }
            let n = arena.insert_path(head[e].root, KB, r.net, len);
            arena.nodes[n as usize].route = route_id(i);
        }
        arena.nodes.shrink_to_fit();
        Self {
            head,
            arena,
            _key: PhantomData,
        }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let e = self.head[key.bits(0, KB) as usize];
        if e.root == 0 {
            opt(e.route)
        } else {
            opt(self.arena.walk(e.root, KB, key, e.route))
        }
    }

    fn memory_bytes(&self) -> usize {
        vec_bytes(&self.head) + self.arena.bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{heavy_lock, incremental_suite, lpm_suite};

    #[test]
    fn bin_trie_v4() {
        lpm_suite::<u32, BinTrie<u32>>();
    }

    #[test]
    fn bin_trie_v6() {
        lpm_suite::<u128, BinTrie<u128>>();
    }

    #[test]
    fn bin_trie_incremental_v4() {
        incremental_suite::<u32, BinTrie<u32>>();
    }

    #[test]
    fn bin_trie_incremental_v6() {
        incremental_suite::<u128, BinTrie<u128>>();
    }

    #[test]
    fn head24() {
        let _g = heavy_lock(); // 128 MiB head per build
        lpm_suite::<u32, HeadBin24<u32>>();
        lpm_suite::<u128, HeadBin24<u128>>();
    }

    #[test]
    fn head20() {
        lpm_suite::<u32, HeadBin20<u32>>();
        lpm_suite::<u128, HeadBin20<u128>>();
    }

    #[test]
    fn head18() {
        lpm_suite::<u32, HeadBin18<u32>>();
        lpm_suite::<u128, HeadBin18<u128>>();
    }

    #[test]
    fn head16() {
        lpm_suite::<u32, HeadBin16<u32>>();
        lpm_suite::<u128, HeadBin16<u128>>();
    }

    #[test]
    fn names() {
        assert_eq!(
            <HeadBin24<u32> as LpmEngine<u32>>::name(),
            "head(2^24)+radix-trie(binary)"
        );
        assert_eq!(
            <HeadBin16<u128> as LpmEngine<u128>>::name(),
            "head(2^16)+radix-trie(binary)"
        );
        assert_eq!(
            <BinTrie<u32> as LpmEngine<u32>>::name(),
            "radix-trie(binary)"
        );
    }
}

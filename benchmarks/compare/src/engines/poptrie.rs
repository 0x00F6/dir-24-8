//! `poptrie(s=16,k=6)` and `poptrie(s=18,k=6)`: Poptrie (H. Asai, Y. Ohara,
//! "Poptrie: A Compressed Trie with Population Count for Fast and Scalable
//! Software IP Routing Table Lookup", ACM SIGCOMM 2015), with direct
//! pointing on the first `s` bits (the paper's Poptrie16 / Poptrie18).
//!
//! * `dir`: `2^s` entries; bit 31 set = leaf (the low 31 bits hold the
//!   route), otherwise the index of an internal node.
//! * Internal node (stride `k = 6`, 64 positions): `vector` has bit `v` set
//!   when position `v` is an internal child; children are contiguous from
//!   `base1` (child = `base1 + popcnt(vector & ((2 << v) - 1)) - 1`).
//!   Positions with a clear `vector` bit are leaves. Leaves are leaf-pushed
//!   (each one holds the longest-prefix winner of its block) and compressed:
//!   `leafvec` has a bit set at every leaf position that starts a new run of
//!   identical leaves (internal positions never start a run), and only the
//!   run heads are stored, contiguously from `base0` (leaf =
//!   `leaves[base0 + popcnt(leafvec & ((2 << v) - 1)) - 1]`).
//! * Key bits past the end of the key read as zero (`Key::bits`).
//!
//! Deviation from the paper: leaves are `u32` route ids instead of 16-bit
//! FIB indices, because a route id must identify the winning route.
//!
//! Static structure built from a temporary binary trie.

use std::marker::PhantomData;

use super::bintrie::BinArena;
use super::{NONE, opt, popcnt_dispatch, route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId};

/// Node stride (bits per internal node).
const STRIDE: u32 = 6;
/// Direct-pointing entry flag: the entry is a leaf.
const LEAF_FLAG: u32 = 1 << 31;
/// Leaf payload of a direct-pointing entry meaning "no route".
const DIR_NONE: u32 = LEAF_FLAG - 1;

#[derive(Clone, Copy, Debug, Default)]
struct PopNode {
    vector: u64,
    leafvec: u64,
    base0: u32,
    base1: u32,
}

pub struct Poptrie<K, const S: u32> {
    dir: Vec<u32>,
    nodes: Vec<PopNode>,
    leaves: Vec<u32>,
    _key: PhantomData<K>,
}

pub type Poptrie16<K> = Poptrie<K, 16>;
pub type Poptrie18<K> = Poptrie<K, 18>;

/// Mask of bits `0..=v` (`(2 << v) - 1` without overflow at `v = 63`).
#[inline(always)]
fn upto(v: u32) -> u64 {
    u64::MAX >> (63 - v)
}

/// One position of a node (or of the direct-pointing array) at build time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pos {
    /// Leaf with the winning route (or `NONE`).
    Leaf(u32),
    /// Subtrie below binary node `node` (at the depth of the position's
    /// prefix), with `best` the winner inherited at that depth.
    Inner { node: u32, best: u32 },
}

struct Builder<'a> {
    trie: &'a BinArena,
    nodes: Vec<PopNode>,
    leaves: Vec<u32>,
}

impl Builder<'_> {
    /// Fills `out[base..base + 2^rem]` with the positions of the next `rem`
    /// key bits below binary node `node`, whose own route is already folded
    /// into `best` (leaf pushing).
    fn expand(&self, node: u32, rem: u32, best: u32, base: usize, out: &mut [Pos]) {
        let n = self.trie.nodes[node as usize];
        if rem == 0 {
            out[base] = if n.is_leaf() {
                Pos::Leaf(best)
            } else {
                Pos::Inner { node, best }
            };
            return;
        }
        if n.is_leaf() {
            out[base..base + (1usize << rem)].fill(Pos::Leaf(best));
            return;
        }
        let half = 1usize << (rem - 1);
        for b in 0..2 {
            let c = n.child[b];
            let at = base + b * half;
            if c == 0 {
                out[at..at + half].fill(Pos::Leaf(best));
            } else {
                let r = self.trie.nodes[c as usize].route;
                self.expand(c, rem - 1, if r != NONE { r } else { best }, at, out);
            }
        }
    }

    /// Builds the internal node stored at `self.nodes[idx]` for binary node
    /// `node` (route folded into `best`); its children are allocated as one
    /// contiguous block, its leaf runs as one contiguous block.
    fn build_node(&mut self, idx: usize, node: u32, best: u32) {
        let mut pos = [Pos::Leaf(NONE); 1 << STRIDE];
        self.expand(node, STRIDE, best, 0, &mut pos);
        let mut vector = 0u64;
        let mut leafvec = 0u64;
        let base0 = u32::try_from(self.leaves.len()).expect("poptrie exceeds u32 leaves");
        let mut prev: Option<u32> = None;
        for (v, p) in pos.iter().enumerate() {
            match *p {
                Pos::Inner { .. } => vector |= 1 << v,
                Pos::Leaf(r) => {
                    if prev != Some(r) {
                        leafvec |= 1 << v;
                        self.leaves.push(r);
                        prev = Some(r);
                    }
                }
            }
        }
        let base1 = u32::try_from(self.nodes.len()).expect("poptrie exceeds u32 nodes");
        let n_children = vector.count_ones() as usize;
        self.nodes
            .resize(self.nodes.len() + n_children, PopNode::default());
        self.nodes[idx] = PopNode {
            vector,
            leafvec,
            base0,
            base1,
        };
        let mut child = base1 as usize;
        for p in pos {
            if let Pos::Inner { node, best } = p {
                self.build_node(child, node, best);
                child += 1;
            }
        }
    }
}

impl<K: Key, const S: u32> Poptrie<K, S> {
    /// Poptrie lookup with direct pointing, as in the paper.
    #[inline(always)]
    fn lookup_impl(&self, key: K) -> Option<RouteId> {
        let d = self.dir[key.bits(0, S) as usize];
        if d & LEAF_FLAG != 0 {
            let r = d & !LEAF_FLAG;
            return if r == DIR_NONE { None } else { Some(r) };
        }
        let nodes = self.nodes.as_slice();
        let mut node = nodes[d as usize];
        let mut offset = S;
        let mut v = key.bits(offset, STRIDE);
        while node.vector & (1u64 << v) != 0 {
            let bc = (node.vector & upto(v)).count_ones();
            node = nodes[(node.base1 + bc - 1) as usize];
            offset += STRIDE;
            v = key.bits(offset, STRIDE);
        }
        let bc = (node.leafvec & upto(v)).count_ones();
        opt(self.leaves[(node.base0 + bc - 1) as usize])
    }

    /// `lookup_impl` compiled with the hardware popcount instruction.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "popcnt")]
    fn lookup_popcnt(&self, key: K) -> Option<RouteId> {
        self.lookup_impl(key)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "popcnt")]
    fn batch_popcnt(&self, keys: &[K], out: &mut [Option<RouteId>]) {
        for (o, &k) in out.iter_mut().zip(keys) {
            *o = self.lookup_impl(k);
        }
    }
}

impl<K: Key, const S: u32> LpmEngine<K> for Poptrie<K, S> {
    fn name() -> String {
        format!("poptrie(s={S},k={STRIDE})")
    }

    fn build(routes: &[Route<K>]) -> Self {
        const { assert!(S >= 1 && S <= 24, "direct pointing size out of range") };
        assert!(S < K::BITS);
        let mut trie = BinArena::new();
        for (i, r) in routes.iter().enumerate() {
            let id = route_id(i);
            assert!(
                id < DIR_NONE,
                "route id does not fit a direct-pointing leaf"
            );
            trie.insert_route(r.net, r.len, id);
        }
        let mut b = Builder {
            trie: &trie,
            nodes: Vec::new(),
            leaves: Vec::new(),
        };
        let mut top = vec![Pos::Leaf(NONE); 1usize << S];
        b.expand(0, S, trie.nodes[0].route, 0, &mut top);
        let mut dir = Vec::with_capacity(top.len());
        for p in top {
            dir.push(match p {
                Pos::Leaf(r) => LEAF_FLAG | if r == NONE { DIR_NONE } else { r },
                Pos::Inner { node, best } => {
                    let idx = b.nodes.len();
                    b.nodes.push(PopNode::default());
                    b.build_node(idx, node, best);
                    u32::try_from(idx)
                        .ok()
                        .filter(|&i| i < LEAF_FLAG)
                        .expect("poptrie exceeds 2^31 nodes")
                }
            });
        }
        let Builder {
            mut nodes,
            mut leaves,
            ..
        } = b;
        nodes.shrink_to_fit();
        leaves.shrink_to_fit();
        Self {
            dir,
            nodes,
            leaves,
            _key: PhantomData,
        }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        #[cfg(target_arch = "x86_64")]
        if popcnt_dispatch() {
            // SAFETY: `popcnt_dispatch` checked at runtime that the CPU
            // supports popcnt, the only feature enabled by the callee.
            return unsafe { self.lookup_popcnt(key) };
        }
        self.lookup_impl(key)
    }

    #[inline]
    fn lookup_batch(&self, keys: &[K], out: &mut [Option<RouteId>]) {
        #[cfg(target_arch = "x86_64")]
        if popcnt_dispatch() {
            // SAFETY: as in `lookup`.
            return unsafe { self.batch_popcnt(keys, out) };
        }
        for (o, &k) in out.iter_mut().zip(keys) {
            *o = self.lookup_impl(k);
        }
    }

    fn memory_bytes(&self) -> usize {
        vec_bytes(&self.dir) + vec_bytes(&self.nodes) + vec_bytes(&self.leaves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::lpm_suite;

    #[test]
    fn upto_masks() {
        assert_eq!(upto(0), 1);
        assert_eq!(upto(5), 0b11_1111);
        assert_eq!(upto(63), u64::MAX);
    }

    #[test]
    fn poptrie16_v4() {
        lpm_suite::<u32, Poptrie16<u32>>();
    }

    #[test]
    fn poptrie16_v6() {
        lpm_suite::<u128, Poptrie16<u128>>();
    }

    #[test]
    fn poptrie18_v4() {
        lpm_suite::<u32, Poptrie18<u32>>();
    }

    #[test]
    fn poptrie18_v6() {
        lpm_suite::<u128, Poptrie18<u128>>();
    }

    #[test]
    fn leaf_compression_merges_runs() {
        // A single /20 under 10.0.0.0/8: the node at offset 16 has one run
        // for the /20 block and one for the /8 remainder on each side.
        let routes = [
            crate::engines::testutil::route(0x0A00_0000u32, 8),
            crate::engines::testutil::route(0x0A01_1000u32, 20),
        ];
        let t = <Poptrie16<u32> as LpmEngine<u32>>::build(&routes);
        assert_eq!(t.nodes.len(), 1);
        assert_eq!(t.nodes[0].vector, 0);
        assert_eq!(t.leaves, vec![0, 1, 0]);
        assert_eq!(t.nodes[0].leafvec.count_ones(), 3);
    }

    #[test]
    fn names() {
        assert_eq!(
            <Poptrie16<u32> as LpmEngine<u32>>::name(),
            "poptrie(s=16,k=6)"
        );
        assert_eq!(
            <Poptrie18<u128> as LpmEngine<u128>>::name(),
            "poptrie(s=18,k=6)"
        );
    }
}

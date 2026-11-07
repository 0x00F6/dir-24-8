//! `tree-bitmap(stride=4)`: Tree Bitmap (W. Eatherton, G. Varghese,
//! Z. Dittia, "Tree bitmap: hardware/software IP lookups with incremental
//! updates", ACM SIGCOMM CCR 34(2), 2004).
//!
//! Each node covers 4 key bits. Its *internal bitmap* (15 used bits) marks
//! the prefixes of length 0..=3 stored inside the node, position
//! `(1 << l) - 1 + (v >> (4 - l))` for the 4-bit chunk `v` and inner length
//! `l`; its *external bitmap* (16 bits) marks which of the 16 children
//! exist. All children of a node are stored contiguously from `child_base`
//! and all its results contiguously from `result_base`, so the index of a
//! child / result is its base plus the popcount of the bitmap below its
//! position. A node at depth `d` holds prefix lengths `4d..=4d+3`; a full
//! length (`K::BITS`) prefix sits at internal bit 0 of a node at depth
//! `K::BITS / 4`. As in the paper the result array is only accessed once,
//! at the end of the walk (the walk remembers the best result index).
//!
//! Static structure built from a temporary uncompressed multibit trie and
//! laid out breadth-first.

use std::collections::VecDeque;
use std::marker::PhantomData;

use super::{NONE, opt, popcnt_dispatch, route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId};

#[derive(Clone, Copy, Debug, Default)]
struct TbmNode {
    internal: u16,
    external: u16,
    child_base: u32,
    result_base: u32,
}

/// `MATCH[v]`: the 4 internal-bitmap positions (lengths 0..=3) that match
/// the chunk `v`. Longer prefixes have higher positions, so the longest
/// match inside a node is the highest set bit of `internal & MATCH[v]`.
const MATCH: [u16; 16] = {
    let mut t = [0u16; 16];
    let mut v = 0;
    while v < 16 {
        let mut m = 0u16;
        let mut l = 0;
        while l < 4 {
            m |= 1 << ((1 << l) - 1 + (v >> (4 - l)));
            l += 1;
        }
        t[v] = m;
        v += 1;
    }
    t
};

pub struct TreeBitmap<K> {
    nodes: Vec<TbmNode>,
    results: Vec<u32>,
    _key: PhantomData<K>,
}

/// Uncompressed build-time node.
#[derive(Clone)]
struct TmpNode {
    internal: u16,
    res: [u32; 15],
    child: [u32; 16],
}

const TMP_EMPTY: TmpNode = TmpNode {
    internal: 0,
    res: [NONE; 15],
    child: [0; 16],
};

impl<K: Key> TreeBitmap<K> {
    /// Tree Bitmap search: per node, longest internal match (deferred
    /// result index), then descend through the external bitmap.
    #[inline(always)]
    fn lookup_impl(&self, key: K) -> Option<RouteId> {
        let nodes = self.nodes.as_slice();
        let mut idx = 0usize;
        let mut depth = 0;
        let mut best = NONE; // index into `results`
        loop {
            let n = nodes[idx];
            let v = key.bits(depth, 4);
            let m = n.internal & MATCH[v as usize];
            if m != 0 {
                let p = 15 - m.leading_zeros();
                best = n.result_base + (n.internal & ((1u16 << p) - 1)).count_ones();
            }
            let bit = 1u16 << v;
            if n.external & bit == 0 {
                break;
            }
            idx = (n.child_base + (n.external & (bit - 1)).count_ones()) as usize;
            depth += 4;
        }
        if best == NONE {
            None
        } else {
            opt(self.results[best as usize])
        }
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

impl<K: Key> LpmEngine<K> for TreeBitmap<K> {
    fn name() -> String {
        "tree-bitmap(stride=4)".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        // 1. Uncompressed multibit trie (child 0 = none; node 0 = root).
        let mut tmp = vec![TMP_EMPTY];
        for (i, r) in routes.iter().enumerate() {
            let len = u32::from(r.len);
            let (d, l) = (len / 4, len % 4);
            let mut n = 0usize;
            for depth in 0..d {
                let v = r.net.bits(4 * depth, 4) as usize;
                let mut c = tmp[n].child[v];
                if c == 0 {
                    c = u32::try_from(tmp.len()).expect("tree bitmap exceeds u32 nodes");
                    tmp.push(TMP_EMPTY);
                    tmp[n].child[v] = c;
                }
                n = c as usize;
            }
            let pos = if l == 0 {
                0
            } else {
                (1usize << l) - 1 + r.net.bits(4 * d, l) as usize
            };
            tmp[n].internal |= 1 << pos;
            tmp[n].res[pos] = route_id(i);
        }

        // 2. Breadth-first layout: the children of every node are appended
        //    together, so they are contiguous; results likewise.
        let mut nodes = vec![TbmNode::default()];
        let mut results = Vec::with_capacity(routes.len());
        let mut queue = VecDeque::from([(0usize, 0usize)]);
        while let Some((t, f)) = queue.pop_front() {
            let tn = &tmp[t];
            let child_base = u32::try_from(nodes.len()).expect("tree bitmap exceeds u32 nodes");
            let mut external = 0u16;
            for v in 0..16 {
                let c = tn.child[v];
                if c != 0 {
                    external |= 1 << v;
                    queue.push_back((c as usize, nodes.len()));
                    nodes.push(TbmNode::default());
                }
            }
            let result_base = u32::try_from(results.len()).expect("too many results");
            for p in 0..15 {
                if tn.internal & (1 << p) != 0 {
                    results.push(tn.res[p]);
                }
            }
            nodes[f] = TbmNode {
                internal: tn.internal,
                external,
                child_base,
                result_base,
            };
        }
        nodes.shrink_to_fit();
        Self {
            nodes,
            results,
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
        vec_bytes(&self.nodes) + vec_bytes(&self.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::lpm_suite;

    #[test]
    fn match_table() {
        // v = 0b1011: lengths 0 (pos 0), 1 ("1": pos 2), 2 ("10": pos 5), 3 ("101": pos 12).
        assert_eq!(MATCH[0b1011], (1 << 0) | (1 << 2) | (1 << 5) | (1 << 12));
        assert_eq!(MATCH[0], (1 << 0) | (1 << 1) | (1 << 3) | (1 << 7));
        assert_eq!(MATCH[15], (1 << 0) | (1 << 2) | (1 << 6) | (1 << 14));
    }

    #[test]
    fn tree_bitmap_v4() {
        lpm_suite::<u32, TreeBitmap<u32>>();
    }

    #[test]
    fn tree_bitmap_v6() {
        lpm_suite::<u128, TreeBitmap<u128>>();
    }

    #[test]
    fn node_is_12_bytes() {
        assert_eq!(std::mem::size_of::<TbmNode>(), 12);
    }
}

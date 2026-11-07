//! `radix-tree(patricia)`: path-compressed binary radix tree (PATRICIA, Morrison 1968, in the
//! prefix-matching form used by BSD radix trees).
//!
//! Every node stores its full key prefix (`key`, host bits cleared) and its
//! bit length, an optional route and two children. Chains of one-child
//! nodes are collapsed: a child may be many bits below its parent, and split
//! nodes (without route) are created on insert where two prefixes diverge.
//! Lookup verifies that the node prefix matches the key (xor +
//! leading-zero count) before using it, remembers the last matching route,
//! and descends on the key bit right after the node prefix.

use super::{NONE, opt, route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId, Updates};

#[derive(Clone, Copy, Debug)]
struct PNode<K> {
    key: K,
    /// Children (0 = none; the root is node 0 and never a child).
    child: [u32; 2],
    route: u32,
    len: u8,
}

pub struct Patricia<K> {
    /// Node 0 is the root: the empty prefix (`0/0`), always present.
    nodes: Vec<PNode<K>>,
}

/// Number of leading bits `a` and `b` share (`0..=K::BITS`).
#[inline(always)]
fn common_len<K: Key>(a: K, b: K) -> u32 {
    (a.to_u128() ^ b.to_u128()).leading_zeros() - (128 - K::BITS)
}

impl<K: Key> Patricia<K> {
    fn push(&mut self, key: K, len: u32, route: u32) -> u32 {
        let i = u32::try_from(self.nodes.len()).expect("patricia exceeds u32 nodes");
        self.nodes.push(PNode {
            key,
            child: [0, 0],
            route,
            len: len as u8,
        });
        i
    }

    fn insert_route(&mut self, net: K, len: u8, id: u32) {
        let len = u32::from(len);
        let mut idx = 0usize;
        loop {
            // Invariant: node `idx` is a prefix of `net/len`.
            let n = self.nodes[idx];
            let nlen = u32::from(n.len);
            if nlen == len {
                self.nodes[idx].route = id;
                return;
            }
            let b = net.bit(nlen) as usize;
            let c = n.child[b];
            if c == 0 {
                let leaf = self.push(net, len, id);
                self.nodes[idx].child[b] = leaf;
                return;
            }
            let ch = self.nodes[c as usize];
            let clen = u32::from(ch.len);
            let cl = common_len(ch.key, net).min(clen).min(len);
            if cl == clen {
                idx = c as usize; // the child is a prefix of the new route
                continue;
            }
            if cl == len {
                // The new route sits between `idx` and the child.
                let m = self.push(net, len, id);
                self.nodes[m as usize].child[ch.key.bit(len) as usize] = c;
                self.nodes[idx].child[b] = m;
                return;
            }
            // Divergence at bit `cl` (nlen < cl < min(len, clen)): split node.
            let s = self.push(net.and(K::mask(cl as u8)), cl, NONE);
            let leaf = self.push(net, len, id);
            self.nodes[s as usize].child[ch.key.bit(cl) as usize] = c;
            self.nodes[s as usize].child[net.bit(cl) as usize] = leaf;
            self.nodes[idx].child[b] = s;
            return;
        }
    }

    /// Exact node for `net/len`, if present.
    #[cfg_attr(not(test), allow(dead_code))]
    fn find(&self, net: K, len: u8) -> Option<usize> {
        let mut idx = 0usize;
        loop {
            let n = &self.nodes[idx];
            if n.len > len || common_len(n.key, net) < u32::from(n.len) {
                return None;
            }
            if n.len == len {
                return Some(idx);
            }
            let c = n.child[net.bit(u32::from(n.len)) as usize];
            if c == 0 {
                return None;
            }
            idx = c as usize;
        }
    }
}

impl<K: Key> LpmEngine<K> for Patricia<K> {
    fn name() -> String {
        "radix-tree(patricia)".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut t = Self {
            nodes: Vec::with_capacity(routes.len() * 2 + 1),
        };
        t.push(K::default(), 0, NONE);
        for (i, r) in routes.iter().enumerate() {
            t.insert_route(r.net, r.len, route_id(i));
        }
        t.nodes.shrink_to_fit();
        t
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let nodes = self.nodes.as_slice();
        let mut best = NONE;
        let mut idx = 0usize;
        loop {
            let n = &nodes[idx];
            if common_len(n.key, key) < u32::from(n.len) {
                break;
            }
            if n.route != NONE {
                best = n.route;
            }
            // Full-length nodes have no children; `bit` reads 0 past the end.
            let c = n.child[key.bit(u32::from(n.len)) as usize];
            if c == 0 {
                break;
            }
            idx = c as usize;
        }
        opt(best)
    }

    fn memory_bytes(&self) -> usize {
        vec_bytes(&self.nodes)
    }

    fn updates() -> Updates {
        Updates::Incremental
    }

    fn insert(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        self.insert_route(r.net, r.len, id);
    }

    /// Clears the route mark; the (possibly now route-less) node stays.
    fn remove(&mut self, id: RouteId, routes: &[Route<K>]) {
        let r = routes[id as usize];
        if let Some(i) = self.find(r.net, r.len)
            && self.nodes[i].route == id
        {
            self.nodes[i].route = NONE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{incremental_suite, lpm_suite};

    #[test]
    fn patricia_v4() {
        lpm_suite::<u32, Patricia<u32>>();
    }

    #[test]
    fn patricia_v6() {
        lpm_suite::<u128, Patricia<u128>>();
    }

    #[test]
    fn patricia_incremental_v4() {
        incremental_suite::<u32, Patricia<u32>>();
    }

    #[test]
    fn patricia_incremental_v6() {
        incremental_suite::<u128, Patricia<u128>>();
    }

    #[test]
    fn path_compression_bounds_nodes() {
        // n routes need at most 2n + 1 nodes (leaves + split nodes + root).
        let routes = crate::engines::testutil::gen_routes::<u128>(5, 2_000, true, 0);
        let t = <Patricia<u128> as LpmEngine<u128>>::build(&routes);
        assert!(t.nodes.len() <= 2 * routes.len() + 1);
    }
}

//! Fixed-stride multibit tries with controlled prefix expansion
//! (Srinivasan & Varghese, "Fast address lookups using controlled prefix
//! expansion", ACM TOCS 1999): `radix-trie(stride=4,16-way)` and
//! `radix-trie(stride=8,256-way)`.
//!
//! A node at depth `d` (a multiple of the stride `S`) is an array of `2^S`
//! slots `{route, child}` indexed by key bits `d..d+S`. A route of length
//! `L` with `d < L <= d + S` (or `L == 0` at the root) is expanded over the
//! `2^(d+S-L)` slots it covers; routes are applied in increasing length
//! order so a longer route overwrites a shorter one. Nodes are allocated on
//! demand. Lookup walks the nodes and keeps the best route seen per level.
//! Both strides divide 32 and 128, so the last level is always complete.

use std::marker::PhantomData;

use super::{NONE, opt, route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId};

#[derive(Clone, Copy, Debug)]
struct Slot {
    route: u32,
    /// Child node index (0 = none; the root is node 0 and never a child).
    child: u32,
}

const EMPTY: Slot = Slot {
    route: NONE,
    child: 0,
};

/// Multibit trie of stride `S`; node `n` occupies `slots[n << S .. (n + 1) << S]`.
pub struct Radix<K, const S: u32> {
    slots: Vec<Slot>,
    _key: PhantomData<K>,
}

pub type Radix4<K> = Radix<K, 4>;
pub type Radix8<K> = Radix<K, 8>;

impl<K: Key, const S: u32> LpmEngine<K> for Radix<K, S> {
    fn name() -> String {
        format!("radix-trie(stride={S},{}-way)", 1u32 << S)
    }

    fn build(routes: &[Route<K>]) -> Self {
        const { assert!(S >= 1 && S <= 16, "stride out of range") };
        const { assert!(K::BITS % S == 0, "stride must divide the key width") };
        let fan = 1usize << S;
        let mut slots = vec![EMPTY; fan];
        let mut order: Vec<usize> = (0..routes.len()).collect();
        order.sort_by_key(|&i| routes[i].len);
        for i in order {
            let r = routes[i];
            let len = u32::from(r.len);
            let target = if len == 0 { 0 } else { (len - 1) / S * S };
            let mut node = 0usize;
            let mut depth = 0;
            while depth < target {
                let s = (node << S) + r.net.bits(depth, S) as usize;
                let mut c = slots[s].child;
                if c == 0 {
                    c = u32::try_from(slots.len() >> S).expect("radix exceeds u32 nodes");
                    slots.resize(slots.len() + fan, EMPTY);
                    slots[s].child = c;
                }
                node = c as usize;
                depth += S;
            }
            // `len - depth` bits of the route fall inside this node.
            let inside = len - depth;
            let first = if inside == 0 {
                0
            } else {
                (r.net.bits(depth, inside) as usize) << (S - inside)
            };
            let count = 1usize << (S - inside);
            let base = node << S;
            for s in &mut slots[base + first..base + first + count] {
                s.route = route_id(i);
            }
        }
        slots.shrink_to_fit();
        Self {
            slots,
            _key: PhantomData,
        }
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let slots = self.slots.as_slice();
        let mut base = 0usize;
        let mut depth = 0;
        let mut best = NONE;
        loop {
            let s = slots[base + key.bits(depth, S) as usize];
            if s.route != NONE {
                best = s.route;
            }
            // Nodes of the last level never have children.
            if s.child == 0 {
                return opt(best);
            }
            base = (s.child as usize) << S;
            depth += S;
        }
    }

    fn memory_bytes(&self) -> usize {
        vec_bytes(&self.slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::lpm_suite;

    #[test]
    fn radix4_v4() {
        lpm_suite::<u32, Radix4<u32>>();
    }

    #[test]
    fn radix4_v6() {
        lpm_suite::<u128, Radix4<u128>>();
    }

    #[test]
    fn radix8_v4() {
        lpm_suite::<u32, Radix8<u32>>();
    }

    #[test]
    fn radix8_v6() {
        lpm_suite::<u128, Radix8<u128>>();
    }

    #[test]
    fn names() {
        assert_eq!(
            <Radix4<u32> as LpmEngine<u32>>::name(),
            "radix-trie(stride=4,16-way)"
        );
        assert_eq!(
            <Radix8<u128> as LpmEngine<u128>>::name(),
            "radix-trie(stride=8,256-way)"
        );
    }
}

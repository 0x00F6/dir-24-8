//! Exact-match index of the routes of one family.
//!
//! The lookup tables only store the *winner* of each address range. To
//! maintain them (inherit the best less-specific route into a new group,
//! restore the parent when a route is removed) the engines keep this index of
//! every normalized prefix together with a per-length population count, so
//! that the parent search skips lengths that have no route at all.

use std::collections::HashMap;
use std::collections::TryReserveError;

use super::prefix::Prefix;

pub(crate) struct PrefixIndex<N: Prefix> {
    map: HashMap<N, u32>,
    /// Number of routes per prefix length (`0..=N::MAX_LEN`).
    per_len: [u32; 129],
}

impl<N: Prefix> PrefixIndex<N> {
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
            per_len: [0; 129],
        }
    }

    #[inline]
    pub(crate) fn get(&self, net: &N) -> Option<u32> {
        self.map.get(net).copied()
    }

    pub(crate) fn try_reserve_one(&mut self) -> Result<(), TryReserveError> {
        self.map.try_reserve(1)
    }

    /// Inserts a new prefix. The caller guarantees it is absent.
    pub(crate) fn insert(&mut self, net: N, slot: u32) {
        let prev = self.map.insert(net, slot);
        debug_assert!(prev.is_none());
        self.per_len[usize::from(net.len())] += 1;
    }

    pub(crate) fn remove(&mut self, net: &N) -> Option<u32> {
        let slot = self.map.remove(net)?;
        self.per_len[usize::from(net.len())] -= 1;
        Some(slot)
    }

    /// Slot of the longest route strictly containing `net`, if any.
    pub(crate) fn parent(&self, net: &N) -> Option<u32> {
        (0..net.len())
            .rev()
            .filter(|&len| self.per_len[usize::from(len)] != 0)
            .find_map(|len| self.map.get(&net.truncated(len)).copied())
    }

    /// Approximate heap footprint (hashbrown: one control byte per bucket).
    pub(crate) fn heap_bytes(&self) -> usize {
        let buckets = self.map.capacity().next_power_of_two().max(1);
        buckets * (core::mem::size_of::<(N, u32)>() + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipnet::Ipv4Net;

    #[test]
    fn parent_finds_longest_strict_ancestor() {
        let mut idx = PrefixIndex::<Ipv4Net>::new();
        let net = |s: &str| s.parse::<Ipv4Net>().unwrap();
        idx.insert(net("10.0.0.0/8"), 1);
        idx.insert(net("10.1.0.0/16"), 2);
        idx.insert(net("10.1.2.0/24"), 3);
        assert_eq!(idx.parent(&net("10.1.2.0/24")), Some(2));
        assert_eq!(idx.parent(&net("10.1.2.128/25")), Some(3));
        assert_eq!(idx.parent(&net("10.2.0.0/16")), Some(1));
        assert_eq!(idx.parent(&net("10.0.0.0/8")), None);
        assert_eq!(idx.remove(&net("10.1.0.0/16")), Some(2));
        assert_eq!(idx.parent(&net("10.1.2.0/24")), Some(1));
    }
}

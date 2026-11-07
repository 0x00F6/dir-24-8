//! `sorted-intervals`: the routes compiled into disjoint address segments.
//!
//! Every segment `[start, end]` (inclusive bounds, so the last address of
//! the space needs no overflow) carries the RouteId of the longest route
//! covering it. Addresses covered by no route are simply absent (holes);
//! adjacent segments with the same winner are merged. `starts`, `ends` and
//! `winners` are separate arrays; a lookup binary-searches the last start
//! `<= key` (`partition_point`) then checks `key <= end` for holes.
//!
//! Compilation is one stack sweep over the routes sorted by (start asc,
//! length asc): CIDR blocks either nest or are disjoint, so the stack holds
//! the chain of routes enclosing the current position, innermost on top.

use super::{route_id, vec_bytes};
use crate::core::{Key, LpmEngine, Route, RouteId};

pub struct SortedIntervals<K> {
    starts: Vec<K>,
    ends: Vec<K>,
    winners: Vec<u32>,
}

impl<K: Key> SortedIntervals<K> {
    /// Appends `[from, to]` -> `id`, merging with the previous segment when
    /// it is adjacent and has the same winner.
    fn emit(&mut self, from: u128, to: u128, id: u32) {
        if let (Some(last_end), Some(&last_id)) = (self.ends.last_mut(), self.winners.last())
            && last_id == id
            && last_end.to_u128().checked_add(1) == Some(from)
        {
            *last_end = K::from_u128(to);
            return;
        }
        self.starts.push(K::from_u128(from));
        self.ends.push(K::from_u128(to));
        self.winners.push(id);
    }
}

impl<K: Key> LpmEngine<K> for SortedIntervals<K> {
    fn name() -> String {
        "sorted-intervals".into()
    }

    fn build(routes: &[Route<K>]) -> Self {
        let mut order: Vec<usize> = (0..routes.len()).collect();
        order.sort_by_key(|&i| (routes[i].net, routes[i].len));
        let mut s = Self {
            starts: Vec::new(),
            ends: Vec::new(),
            winners: Vec::new(),
        };
        // Enclosing routes (inclusive end, id), innermost on top.
        let mut stack: Vec<(u128, u32)> = Vec::new();
        // Next address not yet emitted; `None` once past the last address.
        let mut pos: Option<u128> = Some(0);
        for i in order {
            let r = routes[i];
            let start = r.net.to_u128();
            let end = K::last(r.net, r.len).to_u128();
            // Close the routes that end before this one starts.
            while let Some(&(e, id)) = stack.last() {
                if e >= start {
                    break;
                }
                if let Some(p) = pos
                    && p <= e
                {
                    s.emit(p, e, id);
                }
                pos = Some(e + 1); // e < start <= MAX: no overflow
                stack.pop();
            }
            // The enclosing route (if any) wins up to just before `start`.
            if let (Some(&(_, id)), Some(p)) = (stack.last(), pos)
                && p < start
            {
                s.emit(p, start - 1, id);
            }
            pos = Some(start);
            stack.push((end, route_id(i)));
        }
        while let Some((e, id)) = stack.pop() {
            if let Some(p) = pos
                && p <= e
            {
                s.emit(p, e, id);
            }
            pos = e.checked_add(1).filter(|&n| n <= K::max_value().to_u128());
        }
        s.starts.shrink_to_fit();
        s.ends.shrink_to_fit();
        s.winners.shrink_to_fit();
        s
    }

    #[inline]
    fn lookup(&self, key: K) -> Option<RouteId> {
        let i = self.starts.partition_point(|&st| st <= key);
        if i == 0 {
            return None;
        }
        let j = i - 1;
        if key <= self.ends[j] {
            Some(self.winners[j])
        } else {
            None
        }
    }

    fn memory_bytes(&self) -> usize {
        vec_bytes(&self.starts) + vec_bytes(&self.ends) + vec_bytes(&self.winners)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::testutil::{lpm_suite, route};

    #[test]
    fn intervals_v4() {
        lpm_suite::<u32, SortedIntervals<u32>>();
    }

    #[test]
    fn intervals_v6() {
        lpm_suite::<u128, SortedIntervals<u128>>();
    }

    fn boundaries<K: Key>() {
        let max = K::max_value();
        let w = K::BITS as u8;
        // Default route + hosts at both ends: segments touch 0 and MAX.
        let routes = vec![
            route(K::default(), 0),
            route(K::default(), w),
            route(max, w),
        ];
        let t = <SortedIntervals<K> as LpmEngine<K>>::build(&routes);
        assert_eq!(t.lookup(K::default()), Some(1));
        assert_eq!(t.lookup(K::from_u128(1)), Some(0));
        assert_eq!(t.lookup(K::from_u128(max.to_u128() - 1)), Some(0));
        assert_eq!(t.lookup(max), Some(2));
        assert_eq!(t.starts.len(), 3);
        // Only the top half: a hole from 0 to MAX/2.
        let routes = vec![route(max, 1)];
        let t = <SortedIntervals<K> as LpmEngine<K>>::build(&routes);
        assert_eq!(t.lookup(K::default()), None);
        assert_eq!(t.lookup(K::from_u128(max.to_u128() >> 1)), None);
        assert_eq!(t.lookup(K::from_u128((max.to_u128() >> 1) + 1)), Some(0));
        assert_eq!(t.lookup(max), Some(0));
        // Same winner on both sides of a nested route: no merge across it,
        // and siblings with distinct winners stay separate.
        let routes = vec![
            route(K::default(), 0),
            route(K::default(), 2),
            route(max, 2),
        ];
        let t = <SortedIntervals<K> as LpmEngine<K>>::build(&routes);
        assert_eq!(t.winners, vec![1, 0, 2]);
        assert_eq!(t.ends.last().copied(), Some(max));
    }

    #[test]
    fn boundaries_v4() {
        boundaries::<u32>();
    }

    #[test]
    fn boundaries_v6() {
        boundaries::<u128>();
    }
}

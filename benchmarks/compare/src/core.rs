//! Shared vocabulary of the comparison: keys, routes, engine traits and the
//! reference oracle every engine is validated against.

#[cfg(test)]
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// Numeric address key (`u32` for IPv4, `u128` for IPv6), big-endian
/// numeric convention: `u32::from_be_bytes(octets)` / `u128::from_be_bytes`.
/// Bit 0 is the most significant bit.
pub trait Key: Copy + Eq + Ord + Hash + Debug + Default + Send + Sync + 'static {
    /// Key width in bits (32 or 128).
    const BITS: u32;
    /// `"ipv4"` or `"ipv6"`.
    #[allow(dead_code)]
    const FAMILY: &'static str;

    fn to_u128(self) -> u128;
    fn from_u128(v: u128) -> Self;

    /// `n` bits (`1..=32`) starting at bit `start` (0 = MSB). Bits past the
    /// end of the key read as zero.
    #[inline(always)]
    fn bits(self, start: u32, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        let v = self.to_u128() << (128 - Self::BITS); // left-align
        if start >= 128 {
            return 0;
        }
        let shifted = v << start; // bit `start` now at the top
        (shifted >> (128 - n)) as u32
    }

    /// Bit `i` (0 = MSB) as 0 or 1.
    #[inline(always)]
    fn bit(self, i: u32) -> u32 {
        self.bits(i, 1)
    }

    /// Network mask of `len` bits.
    #[inline(always)]
    fn mask(len: u8) -> Self {
        let len = u32::from(len);
        if len == 0 {
            Self::from_u128(0)
        } else {
            let all = u128::MAX >> (128 - Self::BITS);
            Self::from_u128(all & !(all >> len))
        }
    }

    #[inline(always)]
    fn and(self, o: Self) -> Self {
        Self::from_u128(self.to_u128() & o.to_u128())
    }

    /// Last address (inclusive) of `net/len`.
    #[inline(always)]
    fn last(net: Self, len: u8) -> Self {
        let all = u128::MAX >> (128 - Self::BITS);
        let host = if u32::from(len) >= Self::BITS {
            0
        } else {
            all >> u32::from(len)
        };
        Self::from_u128(net.to_u128() | host)
    }

    fn max_value() -> Self {
        Self::from_u128(u128::MAX >> (128 - Self::BITS))
    }
}

impl Key for u32 {
    const BITS: u32 = 32;
    const FAMILY: &'static str = "ipv4";
    #[inline(always)]
    fn to_u128(self) -> u128 {
        u128::from(self)
    }
    #[inline(always)]
    fn from_u128(v: u128) -> Self {
        v as u32
    }
    #[inline(always)]
    fn bits(self, start: u32, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        if start >= 32 {
            return 0;
        }
        let v = u64::from(self) << 32 << start; // bit `start` at bit 63
        (v >> (64 - n)) as u32
    }
    #[inline(always)]
    fn mask(len: u8) -> Self {
        if len == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(len))
        }
    }
    #[inline(always)]
    fn and(self, o: Self) -> Self {
        self & o
    }
}

impl Key for u128 {
    const BITS: u32 = 128;
    const FAMILY: &'static str = "ipv6";
    #[inline(always)]
    fn to_u128(self) -> u128 {
        self
    }
    #[inline(always)]
    fn from_u128(v: u128) -> Self {
        v
    }
    #[inline(always)]
    fn mask(len: u8) -> Self {
        if len == 0 {
            0
        } else {
            u128::MAX << (128 - u32::from(len))
        }
    }
    #[inline(always)]
    fn and(self, o: Self) -> Self {
        self & o
    }
}

/// One normalized route. Routes of a scenario are distinct; a route's index
/// in the scenario's route slice is its [`RouteId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route<K> {
    /// Network address with host bits cleared.
    pub net: K,
    /// Prefix length (`0..=K::BITS`).
    pub len: u8,
    /// Optional payload (the same for every engine).
    pub payload: Option<u32>,
}

/// Index of the winning route in the scenario's route slice.
pub type RouteId = u32;

/// How an engine applies updates (catalog information; incremental updates
/// are exercised by the engine unit tests, not measured).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum Updates {
    /// Single routes can be inserted and removed in place.
    Incremental,
    /// The structure is rebuilt from the route list.
    Rebuild,
}

/// Longest-prefix-match engine for one family.
///
/// `lookup` returns the id of the longest route containing `key`, or
/// `None`. Implementations must not allocate in `lookup`/`lookup_batch`.
pub trait LpmEngine<K: Key>: Sized {
    /// Identifiable name, e.g. `"radix(stride=4,16-way)"`.
    fn name() -> String;
    fn build(routes: &[Route<K>]) -> Self;
    fn lookup(&self, key: K) -> Option<RouteId>;
    #[inline]
    fn lookup_batch(&self, keys: &[K], out: &mut [Option<RouteId>]) {
        for (o, &k) in out.iter_mut().zip(keys) {
            *o = self.lookup(k);
        }
    }
    /// Bytes owned by the structure (nodes, tables, auxiliary indexes),
    /// excluding the shared route slice.
    fn memory_bytes(&self) -> usize;
    /// `false` for crates that do not account for their memory
    /// (`memory_bytes` is then meaningless; the harness measures every
    /// structure with its counting allocator anyway).
    const REPORTS_MEMORY: bool = true;
    #[cfg_attr(not(test), allow(dead_code))]
    fn updates() -> Updates {
        Updates::Rebuild
    }
    /// Inserts `routes[id]` (only called when `updates()` is incremental).
    #[cfg_attr(not(test), allow(dead_code))]
    fn insert(&mut self, _id: RouteId, _routes: &[Route<K>]) {
        unimplemented!("static structure")
    }
    /// Removes `routes[id]` (only called when `updates()` is incremental).
    #[cfg_attr(not(test), allow(dead_code))]
    fn remove(&mut self, _id: RouteId, _routes: &[Route<K>]) {
        unimplemented!("static structure")
    }
}

/// Membership-only engine: "is the key covered by any route?".
pub trait MembershipEngine<K: Key>: Sized {
    fn name() -> String;
    /// `None` when the structure cannot represent this family/scenario
    /// (reported as not applicable).
    fn applicable(routes: &[Route<K>]) -> Result<(), &'static str>;
    fn build(routes: &[Route<K>]) -> Self;
    fn contains(&self, key: K) -> bool;
    fn memory_bytes(&self) -> usize;
    /// See [`LpmEngine::REPORTS_MEMORY`].
    const REPORTS_MEMORY: bool = true;
}

/// Reference LPM used by the engine unit tests: one hash map per length,
/// scanned from the longest.
#[cfg(test)]
pub struct Oracle<K: Key> {
    by_len: Vec<HashMap<K, RouteId>>,
    lens: Vec<u8>,
}

#[cfg(test)]
impl<K: Key> Oracle<K> {
    pub fn new(routes: &[Route<K>]) -> Self {
        let mut by_len: Vec<HashMap<K, RouteId>> = (0..=K::BITS).map(|_| HashMap::new()).collect();
        for (i, r) in routes.iter().enumerate() {
            assert_eq!(r.net, r.net.and(K::mask(r.len)), "route not normalized");
            let prev = by_len[usize::from(r.len)].insert(r.net, i as RouteId);
            assert!(prev.is_none(), "duplicate route");
        }
        let mut lens: Vec<u8> = (0..=K::BITS as u8)
            .filter(|&l| !by_len[usize::from(l)].is_empty())
            .collect();
        lens.reverse();
        Self { by_len, lens }
    }

    pub fn lookup(&self, key: K) -> Option<RouteId> {
        self.lens.iter().find_map(|&l| {
            self.by_len[usize::from(l)]
                .get(&key.and(K::mask(l)))
                .copied()
        })
    }
}

//! Deterministic route tables, reference oracle and query workloads shared
//! by the Criterion benches (`benches/*.rs`) and the `bench-compare`
//! harness (`benchmarks/compare`, included with `#[path]`): both measure the
//! same data.
//!
//! Address plan:
//!
//! * IPv4 routes: clustered /12 blocks inside `1.0.0.0 - 223.255.255.255`,
//!   lengths /8../32 with a BGP-like mix (mostly /24).
//! * IPv6 routes: clustered /24 blocks inside `2000::/3`, lengths /16../128
//!   with a BGP-like mix (mostly /48 and /32).
//!
//! Routes are distinct normalized prefixes: a table of `n` entries stores
//! exactly `n` CIDRs (unless the address plan is exhausted, which does not
//! happen up to 10M entries).
//!
//! Workloads (every address is checked against the [`Oracle`]):
//!
//! * [`Workload::Hits`]: a random route, a random address inside it (the
//!   LPM winner may be a more specific route): every address is covered.
//! * [`Workload::Misses`]: addresses covered by **no** prefix. Half are
//!   "near" misses drawn next to a random route (inside its prefix widened
//!   by 8 bits), half are drawn uniformly over the family space (IPv4: the
//!   whole space, IPv6: `2000::/3`), rejected while covered.
//! * [`Workload::Random`]: hits and misses shuffled, with a configurable
//!   ratio of hits (per mille).
//! * Criterion only: [`Workload::Deep`] (hits in the longest routes),
//!   [`Workload::Sequential`] (runs of 64 consecutive addresses),
//!   [`Workload::Local`] (hits inside 8 routes, cache resident).
#![allow(dead_code)]

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::net::{Ipv4Addr, Ipv6Addr};

use dir_24_8::{Ipv4Net, Ipv6Net};

/// Bump whenever the generated data changes: cached datasets of
/// `bench-compare` are keyed by it.
pub const GENERATOR_VERSION: u32 = 2;

/// SplitMix64.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0xD1B5_4A32_D192_ED03)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (`n > 0`).
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }
    #[inline]
    pub fn u128(&mut self) -> u128 {
        (u128::from(self.next_u64()) << 64) | u128::from(self.next_u64())
    }
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i as u64 + 1) as usize);
        }
    }
}

/// Fast hasher for the generator's own maps (multiply-rotate, murmur3
/// finalizer so that masked network keys spread over the low bits).
#[derive(Default, Clone, Copy)]
pub struct MixHasher(u64);

impl MixHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7C_C1_B7_27_22_0A_95);
    }
}

impl Hasher for MixHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add(u64::from(b));
        }
    }
    fn write_u8(&mut self, v: u8) {
        self.add(u64::from(v));
    }
    fn write_u32(&mut self, v: u32) {
        self.add(u64::from(v));
    }
    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }
    fn write_u128(&mut self, v: u128) {
        self.add(v as u64);
        self.add((v >> 64) as u64);
    }
    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }
    fn finish(&self) -> u64 {
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        h ^ (h >> 33)
    }
}

type Mix = BuildHasherDefault<MixHasher>;

/// Numeric address of one family (`u32::from_be_bytes(octets)` /
/// `u128::from_be_bytes(octets)`).
pub trait Addr: Copy + Eq + Hash + Ord + Default + std::fmt::Debug {
    /// Address width.
    const BITS: u8;
    /// Routes longer than this need a secondary level (deep workload).
    const DEEP: u8;
    /// Seed salt of the family.
    const SALT: u64;
    fn to_u128(self) -> u128;
    fn from_u128(v: u128) -> Self;
    /// BGP-like prefix length.
    fn random_len(rng: &mut Rng) -> u8;
    /// Number of address clusters for a table of `n` routes.
    fn clusters(n: usize) -> usize;
    /// Random cluster base.
    fn cluster(rng: &mut Rng) -> Self;
    /// Random address inside the cluster of `base`.
    fn in_cluster(base: Self, rng: &mut Rng) -> Self;
    /// Uniform candidate over the family space (misses are rejected while
    /// covered).
    fn far(rng: &mut Rng) -> Self;
}

#[inline]
fn all_bits<A: Addr>() -> u128 {
    u128::MAX >> (128 - u32::from(A::BITS))
}

/// Network mask of `len` bits.
#[inline]
pub fn mask<A: Addr>(len: u8) -> A {
    let all = all_bits::<A>();
    if len == 0 {
        A::from_u128(0)
    } else if len >= A::BITS {
        A::from_u128(all)
    } else {
        A::from_u128(all & !(all >> len))
    }
}

/// `addr` with its host bits (below `len`) cleared.
#[inline]
pub fn network<A: Addr>(addr: A, len: u8) -> A {
    A::from_u128(addr.to_u128() & mask::<A>(len).to_u128())
}

/// Random address inside `net/len`.
#[inline]
pub fn inside<A: Addr>(net: A, len: u8, rng: &mut Rng) -> A {
    let host = if len >= A::BITS {
        0
    } else {
        rng.u128() & (all_bits::<A>() >> len)
    };
    A::from_u128(net.to_u128() | host)
}

fn pick(rng: &mut Rng, table: &[(u32, u8, u8)]) -> u8 {
    let total: u32 = table.iter().map(|t| t.0).sum();
    let mut x = rng.below(u64::from(total)) as u32;
    for &(w, lo, hi) in table {
        if x < w {
            return lo + rng.below(u64::from(hi - lo) + 1) as u8;
        }
        x -= w;
    }
    table[table.len() - 1].2
}

impl Addr for u32 {
    const BITS: u8 = 32;
    const DEEP: u8 = 24;
    const SALT: u64 = 0;
    #[inline]
    fn to_u128(self) -> u128 {
        u128::from(self)
    }
    #[inline]
    fn from_u128(v: u128) -> Self {
        v as u32
    }
    fn random_len(rng: &mut Rng) -> u8 {
        // weight (per mille), min len, max len
        pick(
            rng,
            &[
                (3, 8, 15),
                (15, 16, 16),
                (60, 17, 19),
                (100, 20, 21),
                (110, 22, 22),
                (100, 23, 23),
                (560, 24, 24),
                (30, 25, 28),
                (22, 29, 32),
            ],
        )
    }
    fn clusters(n: usize) -> usize {
        (n / 64).clamp(16, 3500)
    }
    fn cluster(rng: &mut Rng) -> Self {
        ((1 + rng.below(223) as u32) << 24) | (rng.next_u64() as u32 & 0x00F0_0000)
    }
    fn in_cluster(base: Self, rng: &mut Rng) -> Self {
        (base & 0xFFF0_0000) | (rng.next_u64() as u32 & 0x000F_FFFF)
    }
    fn far(rng: &mut Rng) -> Self {
        rng.next_u64() as u32
    }
}

impl Addr for u128 {
    const BITS: u8 = 128;
    const DEEP: u8 = 48;
    const SALT: u64 = 0x6666;
    #[inline]
    fn to_u128(self) -> u128 {
        self
    }
    #[inline]
    fn from_u128(v: u128) -> Self {
        v
    }
    fn random_len(rng: &mut Rng) -> u8 {
        pick(
            rng,
            &[
                (10, 16, 28),
                (40, 29, 31),
                (150, 32, 32),
                (80, 33, 39),
                (150, 40, 47),
                (450, 48, 48),
                (100, 49, 64),
                (10, 65, 127),
                (10, 128, 128),
            ],
        )
    }
    fn clusters(n: usize) -> usize {
        (n / 32).clamp(16, 4000)
    }
    fn cluster(rng: &mut Rng) -> Self {
        (0x2u128 << 124) | ((rng.u128() >> 3) & (u128::MAX << 104))
    }
    fn in_cluster(base: Self, rng: &mut Rng) -> Self {
        (base & (u128::MAX << 104)) | (rng.u128() & !(u128::MAX << 104))
    }
    fn far(rng: &mut Rng) -> Self {
        (0x2u128 << 124) | (rng.u128() >> 3)
    }
}

/// Reference longest-prefix match: one hash map per prefix length, probed
/// from the longest present length. Independent of every measured engine.
pub struct Oracle<A: Addr> {
    by_len: Vec<HashMap<A, u32, Mix>>,
    lens: Vec<(u8, A)>,
}

impl<A: Addr> Oracle<A> {
    fn empty() -> Self {
        Self {
            by_len: (0..=A::BITS).map(|_| HashMap::default()).collect(),
            lens: Vec::new(),
        }
    }

    /// `false` when `net/len` is already present.
    fn insert(&mut self, net: A, len: u8, id: u32) -> bool {
        match self.by_len[usize::from(len)].entry(net) {
            Entry::Vacant(v) => {
                v.insert(id);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    fn finish(&mut self) {
        self.lens = (0..=A::BITS)
            .rev()
            .filter(|&l| !self.by_len[usize::from(l)].is_empty())
            .map(|l| (l, mask::<A>(l)))
            .collect();
    }

    /// Oracle over existing routes (`(network, len)`, distinct).
    pub fn from_routes(routes: &[(A, u8)]) -> Self {
        let mut o = Self::empty();
        for (i, &(net, len)) in routes.iter().enumerate() {
            assert!(o.insert(net, len, i as u32), "duplicate route");
        }
        o.finish();
        o
    }

    /// Index of the longest route containing `a`.
    pub fn lookup(&self, a: A) -> Option<u32> {
        let v = a.to_u128();
        self.lens.iter().find_map(|&(l, m)| {
            self.by_len[usize::from(l)]
                .get(&A::from_u128(v & m.to_u128()))
                .copied()
        })
    }

    pub fn covered(&self, a: A) -> bool {
        self.lookup(a).is_some()
    }
}

/// A generated table: distinct normalized routes and their oracle.
pub struct Table<A: Addr> {
    /// `(network, length)`; the index is the route id.
    pub routes: Vec<(A, u8)>,
    pub oracle: Oracle<A>,
}

/// `n` distinct BGP-like routes of family `A` (deterministic for a seed).
pub fn table<A: Addr>(n: usize, seed: u64) -> Table<A> {
    let mut rng = Rng::new(seed ^ A::SALT);
    let clusters: Vec<A> = (0..A::clusters(n)).map(|_| A::cluster(&mut rng)).collect();
    let mut oracle = Oracle::empty();
    let mut routes = Vec::with_capacity(n);
    let max_attempts = n.saturating_mul(50).max(10_000);
    let mut attempts = 0usize;
    while routes.len() < n && attempts < max_attempts {
        attempts += 1;
        let len = A::random_len(&mut rng);
        let base = clusters[rng.below(clusters.len() as u64) as usize];
        let net = network(A::in_cluster(base, &mut rng), len);
        if oracle.insert(net, len, routes.len() as u32) {
            routes.push((net, len));
        }
    }
    oracle.finish();
    Table { routes, oracle }
}

/// IPv4 table as `Ipv4Net`s.
pub fn ipv4_routes(n: usize, seed: u64) -> Vec<Ipv4Net> {
    v4_nets(&table::<u32>(n, seed).routes)
}

/// IPv6 table as `Ipv6Net`s.
pub fn ipv6_routes(n: usize, seed: u64) -> Vec<Ipv6Net> {
    v6_nets(&table::<u128>(n, seed).routes)
}

pub fn v4_nets(routes: &[(u32, u8)]) -> Vec<Ipv4Net> {
    routes
        .iter()
        .map(|&(a, l)| Ipv4Net::new(Ipv4Addr::from(a), l).expect("valid length"))
        .collect()
}

pub fn v6_nets(routes: &[(u128, u8)]) -> Vec<Ipv6Net> {
    routes
        .iter()
        .map(|&(a, l)| Ipv6Net::new(Ipv6Addr::from(a), l).expect("valid length"))
        .collect()
}

/// Query workload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workload {
    /// Hits and misses shuffled; `hit_permille` hits per 1000 addresses.
    Random { hit_permille: u32 },
    /// Only addresses covered by a route.
    Hits,
    /// Only addresses covered by no route.
    Misses,
    /// Hits inside the longest routes (secondary levels).
    Deep,
    /// Runs of 64 consecutive addresses starting at random routes.
    Sequential,
    /// Hits inside 8 random routes (cache resident).
    Local,
}

impl Workload {
    /// Default `Random` workload: 50 % hits.
    pub const RANDOM: Workload = Workload::Random { hit_permille: 500 };

    pub fn name(self) -> &'static str {
        match self {
            Workload::Random { .. } => "random",
            Workload::Hits => "hits",
            Workload::Misses => "misses",
            Workload::Deep => "deep",
            Workload::Sequential => "sequential",
            Workload::Local => "local",
        }
    }

    fn salt(self) -> u64 {
        match self {
            Workload::Random { hit_permille } => 0x1000 + u64::from(hit_permille),
            Workload::Hits => 0x2000,
            Workload::Misses => 0x3000,
            Workload::Deep => 0x4000,
            Workload::Sequential => 0x5000,
            Workload::Local => 0x6000,
        }
    }
}

fn random_route<A: Addr>(t: &Table<A>, rng: &mut Rng) -> (A, u8) {
    t.routes[rng.below(t.routes.len() as u64) as usize]
}

fn hits<A: Addr>(t: &Table<A>, count: usize, rng: &mut Rng) -> Vec<A> {
    assert!(!t.routes.is_empty(), "hits need at least one route");
    (0..count)
        .map(|_| {
            let (net, len) = random_route(t, rng);
            inside(net, len, rng)
        })
        .collect()
}

fn misses<A: Addr>(t: &Table<A>, count: usize, rng: &mut Rng) -> Vec<A> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        if !t.routes.is_empty() && rng.below(2) == 0 {
            // Near miss: next to a route, inside its prefix widened by 8 bits.
            let (net, len) = random_route(t, rng);
            let wide = len.saturating_sub(8);
            let block = network(net, wide);
            for _ in 0..8 {
                let a = inside(block, wide, rng);
                if !t.oracle.covered(a) {
                    out.push(a);
                    break;
                }
            }
        } else {
            let found = (0..100_000)
                .map(|_| A::far(rng))
                .find(|&a| !t.oracle.covered(a))
                .expect("the address space is fully covered: no miss can be generated");
            out.push(found);
        }
    }
    out
}

/// `count` addresses of workload `w` (deterministic for a seed).
pub fn queries<A: Addr>(t: &Table<A>, w: Workload, count: usize, seed: u64) -> Vec<A> {
    let mut rng = Rng::new(seed ^ A::SALT ^ w.salt());
    match w {
        Workload::Hits => hits(t, count, &mut rng),
        Workload::Misses => misses(t, count, &mut rng),
        Workload::Random { hit_permille } => {
            let k = (count as u64 * u64::from(hit_permille.min(1000)) / 1000) as usize;
            let mut v = if t.routes.is_empty() {
                Vec::new()
            } else {
                hits(t, k, &mut rng)
            };
            v.extend(misses(t, count - v.len(), &mut rng));
            rng.shuffle(&mut v);
            v
        }
        Workload::Deep => {
            let deep: Vec<(A, u8)> = t
                .routes
                .iter()
                .copied()
                .filter(|&(_, l)| l > A::DEEP)
                .collect();
            let pool = if deep.is_empty() { &t.routes } else { &deep };
            (0..count)
                .map(|_| {
                    let (net, len) = pool[rng.below(pool.len() as u64) as usize];
                    inside(net, len, &mut rng)
                })
                .collect()
        }
        Workload::Sequential => {
            let mut v = Vec::with_capacity(count);
            while v.len() < count {
                let start = random_route(t, &mut rng).0.to_u128();
                let run = (count - v.len()).min(64);
                v.extend((0..run as u128).map(|i| A::from_u128(start.wrapping_add(i))));
            }
            v
        }
        Workload::Local => {
            let hot: Vec<(A, u8)> = (0..8).map(|_| random_route(t, &mut rng)).collect();
            (0..count)
                .map(|_| {
                    let (net, len) = hot[rng.below(8) as usize];
                    inside(net, len, &mut rng)
                })
                .collect()
        }
    }
}

/// Payload of route `i`: `None` for one route in four (routes without data
/// still win the LPM), otherwise the route index.
pub fn payload(i: usize) -> Option<u32> {
    (i % 4 != 3).then_some(i as u32)
}

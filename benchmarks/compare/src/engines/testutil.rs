//! Shared test helpers: a tiny deterministic PRNG, route-set generators,
//! probe keys and comparisons of every engine kind against
//! [`Oracle`](crate::core::Oracle).

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};

use crate::core::{Key, LpmEngine, MembershipEngine, Oracle, Route, RouteId, Updates};

/// Serializes tests that allocate large tables (hundreds of MiB) so that
/// parallel test threads never hold several of them at once.
static HEAVY: Mutex<()> = Mutex::new(());

pub fn heavy_lock() -> MutexGuard<'static, ()> {
    HEAVY.lock().unwrap_or_else(|e| e.into_inner())
}

/// SplitMix64 (Steele, Lea, Flood 2014): small, fast, deterministic.
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish value in `0..n` (`n > 0`; the modulo bias is irrelevant here).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// `true` with probability `pct` percent.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    /// Uniformly random key of the family.
    pub fn key<K: Key>(&mut self) -> K {
        let v = (u128::from(self.next_u64()) << 64) | u128::from(self.next_u64());
        K::from_u128(v) // truncates to the low 32 bits for IPv4
    }

    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }
}

/// `net/len` with host bits cleared.
pub fn route<K: Key>(net: K, len: u8) -> Route<K> {
    Route {
        net: net.and(K::mask(len)),
        len,
        payload: None,
    }
}

fn width<K: Key>() -> u8 {
    K::BITS as u8
}

/// Bits of `x` outside the first `len` bits.
fn host_bits<K: Key>(x: K, len: u8) -> u128 {
    x.to_u128() & !K::mask(len).to_u128() & K::max_value().to_u128()
}

/// Prefix length with a distribution loosely shaped like real tables, plus
/// uniform lengths (not multiples of any stride) and host routes.
fn rand_len<K: Key>(rng: &mut SplitMix64) -> u8 {
    let w = u64::from(K::BITS);
    match rng.below(10) {
        0 | 1 => rng.below(w + 1) as u8,
        2 => w as u8,
        _ if w == 32 => (8 + rng.below(17)) as u8, // 8..=24
        _ => (16 + rng.below(49)) as u8,           // 16..=64
    }
}

/// Lengths around the head / direct-pointing boundaries of the engines.
fn boundary_len(rng: &mut SplitMix64) -> u8 {
    const LENS: [u8; 14] = [12, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 28];
    LENS[rng.below(LENS.len() as u64) as usize]
}

/// Deterministic random route set of (at most) `n` distinct normalized
/// routes: fresh prefixes, nested prefixes (chains of more specifics), dense
/// clusters under a few hot prefixes, host routes, routes at the bottom and
/// the top of the address space, and optionally the default route. Routes
/// shorter than `min_len` (other than the default route) are skipped, so
/// that sets with `min_len > 0` leave uncovered holes.
pub fn gen_routes<K: Key>(seed: u64, n: usize, with_default: bool, min_len: u8) -> Vec<Route<K>> {
    let mut rng = SplitMix64::new(seed);
    let mut seen: HashSet<(K, u8)> = HashSet::new();
    let mut out: Vec<Route<K>> = Vec::with_capacity(n);
    let w = width::<K>();
    let mut push = |r: Route<K>, out: &mut Vec<Route<K>>| {
        if out.len() < n && seen.insert((r.net, r.len)) {
            out.push(r);
        }
    };
    if with_default {
        push(route(K::default(), 0), &mut out);
    }
    let hot: Vec<Route<K>> = (0..6)
        .map(|_| {
            let l = boundary_len(&mut rng);
            route(rng.key(), l)
        })
        .collect();
    let mut attempts = 0usize;
    while out.len() < n && attempts < n * 50 {
        attempts += 1;
        let r = match rng.below(100) {
            0..30 => {
                let l = rand_len::<K>(&mut rng);
                route(rng.key(), l)
            }
            30..65 if !out.is_empty() => {
                let p = out[rng.below(out.len() as u64) as usize];
                let extra = 1 + rng.below(12) as u8;
                let len = p.len.saturating_add(extra).min(w);
                let bits = host_bits(rng.key::<K>(), p.len);
                route(K::from_u128(p.net.to_u128() | bits), len)
            }
            65..80 => {
                let p = hot[rng.below(hot.len() as u64) as usize];
                let len = (p.len + rng.below(u64::from(w - p.len) + 1) as u8).min(w);
                let bits = host_bits(rng.key::<K>(), p.len);
                route(K::from_u128(p.net.to_u128() | bits), len)
            }
            80..86 => route(rng.key(), w),
            86..93 => {
                // Bottom of the space: small keys, any length.
                let l = rng.below(u64::from(w) + 1) as u8;
                route(K::from_u128(u128::from(rng.below(1024))), l)
            }
            _ => {
                // Top of the space.
                let l = rng.below(u64::from(w) + 1) as u8;
                let low = u128::from(rng.below(1024));
                route(K::from_u128(K::max_value().to_u128() - low), l)
            }
        };
        if r.len >= min_len {
            push(r, &mut out);
        }
    }
    rng.shuffle(&mut out);
    out
}

/// Every prefix length `0..=W` of `key`: maximal nesting.
pub fn chain<K: Key>(key: K) -> Vec<Route<K>> {
    (0..=width::<K>()).map(|l| route(key, l)).collect()
}

/// Probe keys: 0, 1, MAX-1, MAX, the first and last address of every route,
/// their outer neighbours, one random address inside every route, and
/// `n_random` uniformly random keys.
pub fn probes<K: Key>(routes: &[Route<K>], seed: u64, n_random: usize) -> Vec<K> {
    let mut rng = SplitMix64::new(seed ^ 0x5EED);
    let max = K::max_value().to_u128();
    let mut v: Vec<K> = [0, 1, max - 1, max].into_iter().map(K::from_u128).collect();
    for r in routes {
        let first = r.net.to_u128();
        let last = K::last(r.net, r.len).to_u128();
        v.push(r.net);
        v.push(K::from_u128(last));
        if let Some(p) = first.checked_sub(1) {
            v.push(K::from_u128(p));
        }
        if last < max {
            v.push(K::from_u128(last + 1));
        }
        v.push(K::from_u128(first | host_bits(rng.key::<K>(), r.len)));
    }
    v.extend((0..n_random).map(|_| rng.key::<K>()));
    v
}

/// Named route sets used by every LPM / membership suite.
pub fn route_sets<K: Key>() -> Vec<(&'static str, Vec<Route<K>>)> {
    let w = width::<K>();
    let max = K::max_value();
    let zero = K::default();
    let big = if K::BITS == 32 { 20_000 } else { 3_000 };
    let (min_mid, min_sparse) = if K::BITS == 32 { (8, 16) } else { (12, 32) };
    let mut rng = SplitMix64::new(0xC0FFEE ^ u64::from(K::BITS));
    let rand_key: K = rng.key();
    vec![
        ("empty", vec![]),
        ("default-only", vec![route(zero, 0)]),
        ("host-0", vec![route(zero, w)]),
        ("hosts-0-max", vec![route(zero, w), route(max, w)]),
        ("halves", vec![route(zero, 1), route(max, 1)]),
        ("chain-0", chain(zero)),
        ("chain-max", chain(max)),
        ("chain-rand", chain(rand_key)),
        ("small", gen_routes(1, 60, true, 0)),
        ("small-sparse", gen_routes(5, 60, false, min_sparse)),
        ("medium-nodefault", gen_routes(2, 1_500, false, min_mid)),
        ("big", gen_routes(3, big, true, 0)),
        ("big-nodefault", gen_routes(4, big, false, min_mid)),
        ("big-sparse", gen_routes(6, big, false, min_sparse)),
    ]
}

/// Builds `E` from `routes` and compares `lookup` / `lookup_batch` with the
/// oracle on `probes`.
pub fn check_lpm<K: Key, E: LpmEngine<K>>(label: &str, routes: &[Route<K>], probes: &[K]) {
    let oracle = Oracle::new(routes);
    let e = E::build(routes);
    let mut batch = vec![None; probes.len()];
    e.lookup_batch(probes, &mut batch);
    for (i, &k) in probes.iter().enumerate() {
        let want = oracle.lookup(k);
        let got = e.lookup(k);
        assert_eq!(
            got,
            want,
            "{} {} set {label} ({} routes): key {:#x}",
            E::name(),
            K::FAMILY,
            routes.len(),
            k.to_u128()
        );
        assert_eq!(
            batch[i],
            want,
            "{} batch: key {:#x}",
            E::name(),
            k.to_u128()
        );
    }
    if !routes.is_empty() && E::REPORTS_MEMORY {
        assert!(e.memory_bytes() > 0, "{} reports no memory", E::name());
    }
}

/// Runs [`check_lpm`] on every set of [`route_sets`].
pub fn lpm_suite<K: Key, E: LpmEngine<K>>() {
    for (label, routes) in route_sets::<K>() {
        let p = probes(&routes, routes.len() as u64, 3_000);
        check_lpm::<K, E>(label, &routes, &p);
    }
}

fn verify_subset<K: Key, E: LpmEngine<K>>(
    step: &str,
    e: &E,
    routes: &[Route<K>],
    active: &[bool],
    probes: &[K],
) {
    let ids: Vec<RouteId> = (0..routes.len())
        .filter(|&i| active[i])
        .map(|i| i as RouteId)
        .collect();
    let sub: Vec<Route<K>> = ids.iter().map(|&i| routes[i as usize]).collect();
    let oracle = Oracle::new(&sub);
    for &k in probes {
        let want = oracle.lookup(k).map(|j| ids[j as usize]);
        assert_eq!(
            e.lookup(k),
            want,
            "{} {} after {step} ({} active routes): key {:#x}",
            E::name(),
            K::FAMILY,
            ids.len(),
            k.to_u128()
        );
    }
}

/// Insert / remove sequences checked against an oracle rebuilt from the
/// active subset after every phase.
pub fn incremental_suite<K: Key, E: LpmEngine<K>>() {
    assert_eq!(E::updates(), Updates::Incremental);
    let mut rng = SplitMix64::new(0xD1CE ^ u64::from(K::BITS));
    for (label, routes) in route_sets::<K>() {
        if routes.len() < 2 {
            continue;
        }
        let n = routes.len();
        let p = probes(&routes, n as u64 + 7, 1_000);
        let half = n / 2;
        let mut e = E::build(&routes[..half]);
        let mut active: Vec<bool> = (0..n).map(|i| i < half).collect();
        verify_subset(&format!("{label}: build of half"), &e, &routes, &active, &p);
        for (id, a) in active.iter_mut().enumerate().skip(half) {
            e.insert(id as RouteId, &routes);
            *a = true;
        }
        verify_subset(&format!("{label}: inserts"), &e, &routes, &active, &p);
        let mut order: Vec<usize> = (0..n).collect();
        rng.shuffle(&mut order);
        for &id in &order {
            if rng.chance(50) {
                e.remove(id as RouteId, &routes);
                active[id] = false;
            }
        }
        verify_subset(&format!("{label}: removals"), &e, &routes, &active, &p);
        rng.shuffle(&mut order);
        for &id in &order {
            if !active[id] && rng.chance(50) {
                e.insert(id as RouteId, &routes);
                active[id] = true;
            } else if active[id] && rng.chance(20) {
                e.remove(id as RouteId, &routes);
                active[id] = false;
            }
        }
        verify_subset(&format!("{label}: mixed"), &e, &routes, &active, &p);
        for (id, a) in active.iter_mut().enumerate() {
            if *a {
                e.remove(id as RouteId, &routes);
                *a = false;
            }
        }
        verify_subset(&format!("{label}: remove all"), &e, &routes, &active, &p);
    }
}

/// Membership engines: IPv4 sets compared with `oracle.lookup(k).is_some()`;
/// IPv6 must be reported as not applicable.
pub fn membership_suite<K: Key, E: MembershipEngine<K>>() {
    for (label, routes) in route_sets::<K>() {
        // IPv4-only structures report IPv6 as not applicable.
        if let Err(err) = E::applicable(&routes) {
            assert!(K::BITS != 32, "IPv4 must be applicable");
            assert!(err.starts_with("not applicable"), "{err}");
            continue;
        }
        let oracle = Oracle::new(&routes);
        let p = probes(&routes, routes.len() as u64, 3_000);
        let e = E::build(&routes);
        for &k in &p {
            let want = oracle.lookup(k).is_some();
            assert_eq!(
                e.contains(k),
                want,
                "{} set {label}: key {:#x}",
                E::name(),
                k.to_u128()
            );
        }
        if !routes.is_empty() && E::REPORTS_MEMORY {
            assert!(e.memory_bytes() > 0);
        }
    }
}

#[test]
fn generators_are_valid() {
    fn check<K: Key>() {
        for (_, routes) in route_sets::<K>() {
            let _ = Oracle::new(&routes); // asserts normalized + distinct
        }
        let r = gen_routes::<K>(9, 5_000, true, 0);
        assert_eq!(r.len(), 5_000);
        assert!(r.iter().any(|r| r.len == 0));
        assert!(r.iter().any(|r| u32::from(r.len) == K::BITS));
        assert!(r.iter().any(|r| r.len % 4 != 0));
    }
    check::<u32>();
    check::<u128>();
}

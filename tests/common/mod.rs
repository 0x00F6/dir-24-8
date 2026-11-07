//! Shared test helpers: a simple LPM oracle, a deterministic RNG and
//! generators.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use dir_24_8::{IpNet, IpTable, Ipv4Net, Ipv6Net, LookupMatch, MatchId};

/// Reference LPM: one hash map per prefix length, scanned from the longest
/// length down. Obviously correct, independent of the table code.
pub struct Oracle<T> {
    v4: Vec<HashMap<u32, Option<T>>>,
    v6: Vec<HashMap<u128, Option<T>>>,
}

impl<T> Default for Oracle<T> {
    fn default() -> Self {
        Self {
            v4: (0..=32).map(|_| HashMap::new()).collect(),
            v6: (0..=128).map(|_| HashMap::new()).collect(),
        }
    }
}

pub fn mask4(len: u8) -> u32 {
    if len == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(len))
    }
}

pub fn mask6(len: u8) -> u128 {
    if len == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(len))
    }
}

impl<T> Oracle<T> {
    pub fn insert(&mut self, net: IpNet, data: Option<T>) -> Option<Option<T>> {
        match net.trunc() {
            IpNet::V4(n) => {
                self.v4[usize::from(n.prefix_len())].insert(u32::from(n.network()), data)
            }
            IpNet::V6(n) => {
                self.v6[usize::from(n.prefix_len())].insert(u128::from(n.network()), data)
            }
        }
    }

    pub fn remove(&mut self, net: IpNet) -> Option<Option<T>> {
        match net.trunc() {
            IpNet::V4(n) => self.v4[usize::from(n.prefix_len())].remove(&u32::from(n.network())),
            IpNet::V6(n) => self.v6[usize::from(n.prefix_len())].remove(&u128::from(n.network())),
        }
    }

    pub fn get_mut(&mut self, net: IpNet) -> Option<&mut Option<T>> {
        match net.trunc() {
            IpNet::V4(n) => self.v4[usize::from(n.prefix_len())].get_mut(&u32::from(n.network())),
            IpNet::V6(n) => self.v6[usize::from(n.prefix_len())].get_mut(&u128::from(n.network())),
        }
    }

    pub fn lookup(&self, ip: IpAddr) -> Option<(IpNet, Option<&T>)> {
        match ip {
            IpAddr::V4(a) => {
                let a = u32::from(a);
                (0..=32u8).rev().find_map(|len| {
                    let net = a & mask4(len);
                    self.v4[usize::from(len)].get(&net).map(|d| {
                        (
                            IpNet::V4(Ipv4Net::new(Ipv4Addr::from(net), len).unwrap()),
                            d.as_ref(),
                        )
                    })
                })
            }
            IpAddr::V6(a) => {
                let a = u128::from(a);
                (0..=128u8).rev().find_map(|len| {
                    let net = a & mask6(len);
                    self.v6[usize::from(len)].get(&net).map(|d| {
                        (
                            IpNet::V6(Ipv6Net::new(Ipv6Addr::from(net), len).unwrap()),
                            d.as_ref(),
                        )
                    })
                })
            }
        }
    }

    pub fn len(&self) -> usize {
        self.v4.iter().map(HashMap::len).sum::<usize>()
            + self.v6.iter().map(HashMap::len).sum::<usize>()
    }

    pub fn routes(&self) -> Vec<IpNet> {
        let mut out = Vec::new();
        for (len, m) in self.v4.iter().enumerate() {
            out.extend(
                m.keys()
                    .map(|&n| IpNet::V4(Ipv4Net::new(Ipv4Addr::from(n), len as u8).unwrap())),
            );
        }
        for (len, m) in self.v6.iter().enumerate() {
            out.extend(
                m.keys()
                    .map(|&n| IpNet::V6(Ipv6Net::new(Ipv6Addr::from(n), len as u8).unwrap())),
            );
        }
        out
    }
}

/// SplitMix64: tiny deterministic generator.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    pub fn u128(&mut self) -> u128 {
        (u128::from(self.next_u64()) << 64) | u128::from(self.next_u64())
    }
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// Address generator biased towards a small set of "hot" regions so that
/// random prefixes overlap and nest.
pub struct Gen {
    pub rng: Rng,
    hot4: Vec<u32>,
    hot6: Vec<u128>,
}

impl Gen {
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let hot4 = (0..8).map(|_| rng.next_u64() as u32).collect();
        let hot6 = (0..8).map(|_| rng.u128()).collect();
        Self { rng, hot4, hot6 }
    }

    pub fn addr4(&mut self) -> u32 {
        if self.rng.chance(3, 4) {
            let base = self.hot4[self.rng.below(self.hot4.len() as u64) as usize];
            let keep = self.rng.below(33) as u8;
            (base & mask4(keep)) | (self.rng.next_u64() as u32 & !mask4(keep))
        } else {
            self.rng.next_u64() as u32
        }
    }

    pub fn addr6(&mut self) -> u128 {
        if self.rng.chance(3, 4) {
            let base = self.hot6[self.rng.below(self.hot6.len() as u64) as usize];
            let keep = self.rng.below(129) as u8;
            (base & mask6(keep)) | (self.rng.u128() & !mask6(keep))
        } else {
            self.rng.u128()
        }
    }

    pub fn len4(&mut self) -> u8 {
        match self.rng.below(10) {
            0 => self.rng.below(9) as u8,          // short, /0../8
            1..=4 => 16 + self.rng.below(9) as u8, // /16../24
            _ => 24 + self.rng.below(9) as u8,     // /24../32
        }
    }

    pub fn len6(&mut self) -> u8 {
        match self.rng.below(10) {
            0 => self.rng.below(25) as u8,          // /0../24
            1..=5 => 24 + self.rng.below(41) as u8, // /24../64
            _ => 64 + self.rng.below(65) as u8,     // /64../128
        }
    }

    pub fn net4(&mut self) -> IpNet {
        let len = self.len4();
        IpNet::V4(
            Ipv4Net::new(Ipv4Addr::from(self.addr4()), len)
                .unwrap()
                .trunc(),
        )
    }

    pub fn net6(&mut self) -> IpNet {
        let len = self.len6();
        IpNet::V6(
            Ipv6Net::new(Ipv6Addr::from(self.addr6()), len)
                .unwrap()
                .trunc(),
        )
    }

    pub fn net(&mut self) -> IpNet {
        if self.rng.chance(1, 2) {
            self.net4()
        } else {
            self.net6()
        }
    }

    pub fn ip(&mut self) -> IpAddr {
        if self.rng.chance(1, 2) {
            IpAddr::V4(Ipv4Addr::from(self.addr4()))
        } else {
            IpAddr::V6(Ipv6Addr::from(self.addr6()))
        }
    }
}

/// Compares a lookup result with the oracle's (prefix and payload).
pub fn same<T: PartialEq + Debug>(
    got: Option<LookupMatch<'_, T>>,
    want: Option<(IpNet, Option<&T>)>,
    ctx: &dyn Fn() -> String,
) {
    let got = got.map(|m| (m.prefix, m.data));
    assert_eq!(got, want, "{}", ctx());
}

/// Checks every lookup flavor of `table` against `oracle` for `ips`: single
/// lookups (scalar kernels) and batches (the batch kernel of this build and
/// CPU, named in the messages).
pub fn check_all<T: PartialEq + Debug>(table: &IpTable<T>, oracle: &Oracle<T>, ips: &[IpAddr]) {
    let kernel = table.stats().batch_kernel;
    for &ip in ips {
        let want = oracle.lookup(ip);
        same(table.lookup(ip), want, &|| format!("lookup {ip}"));
        assert_eq!(table.exist(ip), want.is_some(), "exist {ip}");
        assert_eq!(
            table.lookup_data(ip),
            want.and_then(|w| w.1),
            "lookup_data {ip}"
        );
        match ip {
            IpAddr::V4(a) => same(table.lookup_ipv4(a), want, &|| format!("lookup_ipv4 {ip}")),
            IpAddr::V6(a) => same(table.lookup_ipv6(a), want, &|| format!("lookup_ipv6 {ip}")),
        }
    }
    // Mixed batch.
    let mut out = vec![None; ips.len()];
    table.lookup_batch_into(ips, &mut out).unwrap();
    for (i, &ip) in ips.iter().enumerate() {
        same(out[i], oracle.lookup(ip), &|| {
            format!("batch[{i}] {ip} [{kernel}]")
        });
    }
    let mut ex = vec![false; ips.len()];
    table.exist_batch_into(ips, &mut ex).unwrap();
    for (i, &ip) in ips.iter().enumerate() {
        assert_eq!(ex[i], oracle.lookup(ip).is_some(), "exist batch {ip}");
    }
    let mut ids = vec![MatchId::NONE; ips.len()];
    table.lookup_ids_into(ips, &mut ids).unwrap();
    for (i, &ip) in ips.iter().enumerate() {
        same(table.resolve(ids[i]), oracle.lookup(ip), &|| {
            format!("ids {ip}")
        });
    }
    // Homogeneous batches in every representation.
    let v4: Vec<Ipv4Addr> = ips
        .iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(a) => Some(*a),
            _ => None,
        })
        .collect();
    let v6: Vec<Ipv6Addr> = ips
        .iter()
        .filter_map(|ip| match ip {
            IpAddr::V6(a) => Some(*a),
            _ => None,
        })
        .collect();
    let want4: Vec<_> = v4.iter().map(|&a| oracle.lookup(IpAddr::V4(a))).collect();
    let want6: Vec<_> = v6.iter().map(|&a| oracle.lookup(IpAddr::V6(a))).collect();

    let mut o4 = vec![None; v4.len()];
    table.lookup_batch_into(&v4, &mut o4).unwrap();
    o4.iter()
        .zip(&want4)
        .for_each(|(g, w)| same(*g, *w, &|| "v4 batch".into()));
    let n4: Vec<u32> = v4.iter().map(|&a| u32::from(a)).collect();
    table.lookup_batch_into(&n4, &mut o4).unwrap();
    o4.iter()
        .zip(&want4)
        .for_each(|(g, w)| same(*g, *w, &|| "u32 batch".into()));
    let b4: Vec<[u8; 4]> = v4.iter().map(|a| a.octets()).collect();
    table.lookup_batch_into(&b4, &mut o4).unwrap();
    o4.iter()
        .zip(&want4)
        .for_each(|(g, w)| same(*g, *w, &|| "[u8;4] batch".into()));
    let mut id4 = vec![MatchId::NONE; n4.len()];
    table.lookup_ids_into(&n4, &mut id4).unwrap();
    id4.iter()
        .zip(&want4)
        .for_each(|(g, w)| same(table.resolve(*g), *w, &|| "u32 ids".into()));

    let mut o6 = vec![None; v6.len()];
    table.lookup_batch_into(&v6, &mut o6).unwrap();
    o6.iter()
        .zip(&want6)
        .for_each(|(g, w)| same(*g, *w, &|| "v6 batch".into()));
    let n6: Vec<u128> = v6.iter().map(|&a| u128::from(a)).collect();
    table.lookup_batch_into(&n6, &mut o6).unwrap();
    o6.iter()
        .zip(&want6)
        .for_each(|(g, w)| same(*g, *w, &|| "u128 batch".into()));
    let b6: Vec<[u8; 16]> = v6.iter().map(|a| a.octets()).collect();
    table.lookup_batch_into(&b6, &mut o6).unwrap();
    o6.iter()
        .zip(&want6)
        .for_each(|(g, w)| same(*g, *w, &|| "[u8;16] batch".into()));
    let mut id6 = vec![MatchId::NONE; b6.len()];
    table.lookup_ids_into(&b6, &mut id6).unwrap();
    id6.iter()
        .zip(&want6)
        .for_each(|(g, w)| same(table.resolve(*g), *w, &|| "[u8;16] ids".into()));
}

/// Queries that hit route boundaries: first/last address of every route,
/// their neighbors, plus random addresses.
pub fn probe_addresses(routes: &[IpNet], g: &mut Gen, random: usize) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for net in routes {
        match net {
            IpNet::V4(n) => {
                let lo = u32::from(n.network());
                let hi = u32::from(n.broadcast());
                for a in [
                    lo,
                    hi,
                    lo.wrapping_sub(1),
                    hi.wrapping_add(1),
                    lo | ((hi - lo) / 2),
                ] {
                    ips.push(IpAddr::V4(Ipv4Addr::from(a)));
                }
            }
            IpNet::V6(n) => {
                let lo = u128::from(n.network());
                let hi = u128::from(n.broadcast());
                for a in [
                    lo,
                    hi,
                    lo.wrapping_sub(1),
                    hi.wrapping_add(1),
                    lo | ((hi - lo) / 2),
                ] {
                    ips.push(IpAddr::V6(Ipv6Addr::from(a)));
                }
            }
        }
    }
    ips.extend((0..random).map(|_| g.ip()));
    ips
}

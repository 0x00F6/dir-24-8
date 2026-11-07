//! Property tests: random mutation sequences against the oracle.

mod common;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use common::{Oracle, check_all};
use dir_24_8::{IpNet, IpTable, Ipv4Net, Ipv6Net};
use dir_24_8::{MergeWith, Replace};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Insert(IpNet, Option<u16>),
    Remove(usize),
    Merge(usize, Option<u16>),
}

fn net4() -> impl Strategy<Value = IpNet> {
    // Few distinct high octets so prefixes nest and collide.
    (0u32..4, any::<u32>(), 0u8..=32).prop_map(|(hi, lo, len)| {
        let a = (hi << 30) | (lo >> 2);
        IpNet::V4(Ipv4Net::new(Ipv4Addr::from(a), len).unwrap().trunc())
    })
}

fn net6() -> impl Strategy<Value = IpNet> {
    (0u128..4, any::<u128>(), 0u8..=128).prop_map(|(hi, lo, len)| {
        let a = (hi << 126) | (lo >> 2);
        IpNet::V6(Ipv6Net::new(Ipv6Addr::from(a), len).unwrap().trunc())
    })
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (prop_oneof![net4(), net6()], proptest::option::of(any::<u16>())).prop_map(|(n, d)| Op::Insert(n, d)),
        2 => any::<usize>().prop_map(Op::Remove),
        1 => (any::<usize>(), proptest::option::of(any::<u16>())).prop_map(|(i, d)| Op::Merge(i, d)),
    ]
}

fn probe() -> impl Strategy<Value = IpAddr> {
    prop_oneof![
        (0u32..4, any::<u32>())
            .prop_map(|(hi, lo)| IpAddr::V4(Ipv4Addr::from((hi << 30) | (lo >> 2)))),
        (0u128..4, any::<u128>())
            .prop_map(|(hi, lo)| IpAddr::V6(Ipv6Addr::from((hi << 126) | (lo >> 2)))),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn tables_follow_the_oracle(ops in proptest::collection::vec(op(), 1..120), probes in proptest::collection::vec(probe(), 1..200)) {
        let mut t: IpTable<u16> = IpTable::new();
        let mut o: Oracle<u16> = Oracle::default();
        let mut live: Vec<IpNet> = Vec::new();
        for op in &ops {
            match *op {
                Op::Insert(n, d) => {
                    prop_assert_eq!(t.insert(n, d, Replace).unwrap(), o.insert(n, d));
                    live.push(n);
                }
                Op::Remove(i) if !live.is_empty() => {
                    let n = live.swap_remove(i % live.len());
                    prop_assert_eq!(t.remove(n), o.remove(n));
                }
                Op::Merge(i, d) if !live.is_empty() => {
                    let n = live[i % live.len()];
                    t.insert(n, d, MergeWith(|a: &mut u16, b: u16| *a = a.wrapping_add(b)))
                        .unwrap();
                    match (o.get_mut(n), d) {
                        (None, d) => { o.insert(n, d); }
                        (Some(_), None) => {}
                        (Some(slot @ None), Some(d)) => *slot = Some(d),
                        (Some(Some(cur)), Some(d)) => *cur = cur.wrapping_add(d),
                    }
                }
                _ => {}
            }
        }
        prop_assert_eq!(t.len(), o.len());
        let mut ips = probes.clone();
        ips.extend(common::probe_addresses(&o.routes(), &mut common::Gen::new(1), 0));
        check_all(&t, &o, &ips);
    }
}

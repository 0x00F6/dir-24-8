//! The table against the reference oracle on deterministic data sets and
//! generated mutation sequences (single lookups and every batch flavor).

mod common;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use common::*;
use dir_24_8::{IpNet, IpTable, MergeOutcome};
use dir_24_8::{Merge, Replace};

fn build(seed: u64, n: usize) -> (IpTable<u32>, Oracle<u32>, Vec<IpNet>, Gen) {
    let mut g = Gen::new(seed);
    let mut table = IpTable::new();
    let mut oracle = Oracle::default();
    let mut routes = Vec::new();
    for i in 0..n {
        let net = g.net();
        let data = if g.rng.chance(1, 4) {
            None
        } else {
            Some(i as u32)
        };
        let a = table.insert(net, data, Replace).unwrap();
        let b = oracle.insert(net, data);
        assert_eq!(a, b, "insert {net}");
        routes.push(net);
    }
    (table, oracle, routes, g)
}

#[test]
fn random_sets_match_oracle() {
    for (seed, n) in [(1u64, 50usize), (2, 400), (3, 1500)] {
        let (table, oracle, routes, mut g) = build(seed, n);
        assert_eq!(table.len(), oracle.len());
        let ips = probe_addresses(&routes, &mut g, 2000);
        check_all(&table, &oracle, &ips);
    }
}

#[test]
fn mutation_sequences_match_oracle() {
    let mut g = Gen::new(0xC0FFEE);
    let mut table: IpTable<Vec<u32>> = IpTable::new();
    let mut oracle: Oracle<Vec<u32>> = Oracle::default();
    let mut live: Vec<IpNet> = Vec::new();
    for step in 0..3000u32 {
        let op = g.rng.below(10);
        if op < 5 || live.is_empty() {
            let net = g.net();
            let data = (!g.rng.chance(1, 5)).then(|| vec![step]);
            assert_eq!(
                table.insert(net, data.clone(), Replace).unwrap(),
                oracle.insert(net, data)
            );
            live.push(net);
        } else if op < 8 {
            let i = g.rng.below(live.len() as u64) as usize;
            let net = live.swap_remove(i);
            assert_eq!(table.remove(net), oracle.remove(net), "remove {net}");
        } else if op < 9 {
            let i = g.rng.below(live.len() as u64) as usize;
            let net = live[i];
            let data = (!g.rng.chance(1, 3)).then(|| vec![step]);
            let got = table.insert(net, data.clone(), Merge).unwrap();
            let want = match (oracle.get_mut(net), data) {
                (None, d) => {
                    oracle.insert(net, d);
                    MergeOutcome::Inserted
                }
                (Some(_), None) => MergeOutcome::Unchanged,
                (Some(slot @ None), Some(d)) => {
                    *slot = Some(d);
                    MergeOutcome::Adopted
                }
                (Some(Some(cur)), Some(mut d)) => {
                    cur.append(&mut d);
                    MergeOutcome::Merged
                }
            };
            assert_eq!(got, want, "merge {net}");
        } else {
            let i = g.rng.below(live.len() as u64) as usize;
            let net = live[i];
            let data = (!g.rng.chance(1, 2)).then(|| vec![step, step]);
            let want = oracle
                .get_mut(net)
                .map(|slot| std::mem::replace(slot, data.clone()));
            assert_eq!(table.replace(net, data), want, "replace {net}");
        }
        if step % 250 == 0 {
            let routes = oracle.routes();
            let ips = probe_addresses(&routes, &mut g, 300);
            check_all(&table, &oracle, &ips);
        }
    }
    assert_eq!(table.len(), oracle.len());
    // Remove everything: the table must become empty and release groups.
    for net in oracle.routes() {
        assert!(table.remove(net).is_some(), "final remove {net}");
    }
    assert!(table.is_empty());
    let s = table.stats();
    assert_eq!(s.ipv4.groups_in_use, 0, "IPv4 groups leaked");
    assert_eq!(s.ipv6.groups_in_use, 0, "IPv6 groups leaked");
    let mut g2 = Gen::new(9);
    for _ in 0..200 {
        let ip = g2.ip();
        assert!(table.lookup(ip).is_none());
        assert!(!table.exist(ip));
    }
}

#[test]
fn nested_prefixes_in_every_insertion_order() {
    let chain: Vec<IpNet> = [
        "10.0.0.0/8",
        "10.1.0.0/16",
        "10.1.2.0/23",
        "10.1.2.0/24",
        "10.1.2.128/25",
        "10.1.2.200/30",
        "10.1.2.201/32",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();
    let chain6: Vec<IpNet> = [
        "2001:db8::/32",
        "2001:db8:1::/48",
        "2001:db8:1:2::/63",
        "2001:db8:1:2::/64",
        "2001:db8:1:2:8000::/65",
        "2001:db8:1:2:8000::1/127",
        "2001:db8:1:2:8000::1/128",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();
    let mut rng = Rng::new(77);
    for routes in [&chain, &chain6] {
        for round in 0..24 {
            let mut order: Vec<usize> = (0..routes.len()).collect();
            if round == 1 {
                order.reverse();
            } else if round > 1 {
                for i in (1..order.len()).rev() {
                    order.swap(i, rng.below(i as u64 + 1) as usize);
                }
            }
            let mut table: IpTable<usize> = IpTable::new();
            let mut oracle = Oracle::default();
            for &i in &order {
                table.insert(routes[i], Some(i), Replace).unwrap();
                oracle.insert(routes[i], Some(i));
            }
            let mut g = Gen::new(round);
            let ips = probe_addresses(routes, &mut g, 50);
            check_all(&table, &oracle, &ips);
            // Remove in another order and check after every removal.
            let mut rm = order.clone();
            rm.rotate_left(round as usize % routes.len());
            for &i in &rm {
                assert_eq!(table.remove(routes[i]), Some(Some(i)));
                oracle.remove(routes[i]);
                check_all(&table, &oracle, &ips);
            }
            assert_eq!(table.groups_in_use_for_tests(), (0, 0));
        }
    }
}

#[test]
fn batch_sizes_around_simd_widths() {
    let sizes = [
        0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 47, 63, 64, 65, 127, 128, 129, 255,
        1000,
    ];
    let (table, oracle, routes, mut g) = build(42, 800);
    let pool = probe_addresses(&routes, &mut g, 1000);
    for &n in &sizes {
        for offset in [0usize, 1, 3] {
            // Unaligned sub-slices of a larger buffer.
            let ips = &pool[offset..offset + n];
            check_all(&table, &oracle, ips);
        }
    }
}

#[test]
fn ipv6_lanes_finishing_at_every_depth() {
    // One route per possible depth on the same 24-bit root, plus addresses
    // sharing the first 64 bits but differing afterwards.
    let mut table: IpTable<u8> = IpTable::new();
    let mut oracle = Oracle::default();
    let base: u128 = 0x2001_0db8_aaaa_bbbb_cccc_dddd_eeee_ffff;
    for len in (0..=128u8).step_by(1) {
        if len % 3 != 0 && len != 128 {
            continue;
        }
        let net = IpNet::V6(
            dir_24_8::Ipv6Net::new(Ipv6Addr::from(base), len)
                .unwrap()
                .trunc(),
        );
        table.insert(net, Some(len), Replace).unwrap();
        oracle.insert(net, Some(len));
    }
    let mut ips = Vec::new();
    for bit in 0..128u32 {
        ips.push(IpAddr::V6(Ipv6Addr::from(base ^ (1u128 << bit))));
    }
    ips.push(IpAddr::V6(Ipv6Addr::from(base)));
    // Same first 64 bits, different low half.
    let mut rng = Rng::new(5);
    for _ in 0..100 {
        ips.push(IpAddr::V6(Ipv6Addr::from(
            (base & !0u128 << 64 >> 64 << 64) | u128::from(rng.next_u64()),
        )));
    }
    // Shuffle so every SIMD vector mixes depths.
    for i in (1..ips.len()).rev() {
        ips.swap(i, rng.below(i as u64 + 1) as usize);
    }
    check_all(&table, &oracle, &ips);
}

#[test]
fn all_prefix_lengths_and_boundaries() {
    let mut table: IpTable<u32> = IpTable::new();
    let mut oracle = Oracle::default();
    let a4 = 0xC0A8_7B2D_u32; // 192.168.123.45
    for len in 0..=32u8 {
        let net = IpNet::V4(
            dir_24_8::Ipv4Net::new(Ipv4Addr::from(a4), len)
                .unwrap()
                .trunc(),
        );
        table.insert(net, Some(u32::from(len)), Replace).unwrap();
        oracle.insert(net, Some(u32::from(len)));
    }
    let a6 = 0xfe80_1234_5678_9abc_def0_1357_9bdf_2468_u128;
    for len in 0..=128u8 {
        let net = IpNet::V6(
            dir_24_8::Ipv6Net::new(Ipv6Addr::from(a6), len)
                .unwrap()
                .trunc(),
        );
        table
            .insert(net, Some(1000 + u32::from(len)), Replace)
            .unwrap();
        oracle.insert(net, Some(1000 + u32::from(len)));
    }
    let mut ips = vec![
        IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
        IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::from(u128::MAX)),
    ];
    for bit in 0..32 {
        ips.push(IpAddr::V4(Ipv4Addr::from(a4 ^ (1 << bit))));
    }
    for bit in 0..128 {
        ips.push(IpAddr::V6(Ipv6Addr::from(a6 ^ (1u128 << bit))));
    }
    ips.push(IpAddr::V4(Ipv4Addr::from(a4)));
    ips.push(IpAddr::V6(Ipv6Addr::from(a6)));
    check_all(&table, &oracle, &ips);
    // Peel the prefixes from the most specific one.
    for len in (0..=32u8).rev() {
        let net = IpNet::V4(
            dir_24_8::Ipv4Net::new(Ipv4Addr::from(a4), len)
                .unwrap()
                .trunc(),
        );
        assert_eq!(table.remove(net), Some(Some(u32::from(len))));
        oracle.remove(net);
        check_all(&table, &oracle, &ips);
    }
    for len in 0..=128u8 {
        let net = IpNet::V6(
            dir_24_8::Ipv6Net::new(Ipv6Addr::from(a6), len)
                .unwrap()
                .trunc(),
        );
        assert!(table.remove(net).is_some());
        oracle.remove(net);
        if len % 16 == 0 {
            check_all(&table, &oracle, &ips);
        }
    }
    assert!(table.is_empty());
}

#[test]
fn edge_of_address_space_routes() {
    let mut table: IpTable<&str> = IpTable::new();
    let mut oracle = Oracle::default();
    for (s, d) in [
        ("255.255.255.255/32", "max4"),
        ("255.255.255.254/31", "max4/31"),
        ("255.255.255.0/24", "max4/24"),
        ("0.0.0.0/32", "zero4"),
        ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128", "max6"),
        ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127", "max6/127"),
        ("ffff:ffff:ffff::/48", "max6/48"),
        ("::/128", "zero6"),
    ] {
        let net: IpNet = s.parse().unwrap();
        table.insert(net, Some(d), Replace).unwrap();
        oracle.insert(net, Some(d));
    }
    let routes = oracle.routes();
    let mut g = Gen::new(3);
    let ips = probe_addresses(&routes, &mut g, 20);
    check_all(&table, &oracle, &ips);
}

#[test]
fn huge_pages_and_limits() {
    use dir_24_8::Config;
    for config in [
        Config::new().huge_pages(true),
        Config::new().max_routes(10_000).max_ipv4_groups(1 << 12),
    ] {
        let mut g = Gen::new(99);
        let mut table: IpTable<u32> = IpTable::with_config(config).unwrap();
        let mut oracle = Oracle::default();
        let mut routes = Vec::new();
        for i in 0..600u32 {
            let net = g.net();
            table.insert(net, Some(i), Replace).unwrap();
            oracle.insert(net, Some(i));
            routes.push(net);
        }
        let ips = probe_addresses(&routes, &mut g, 500);
        check_all(&table, &oracle, &ips);
    }
}

//! Documented semantics of lookups, `exist`, payloads and families.

mod common;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use dir_24_8::{Error, IpNet, IpTable, LookupMatch, Replace};

fn net(s: &str) -> IpNet {
    s.parse().unwrap()
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn none_vs_some_without_data_vs_some_with_data() {
    let mut t: IpTable<&str> = IpTable::new();
    t.insert(net("10.0.0.0/8"), Some("ten"), Replace).unwrap();
    t.insert(net("10.1.0.0/16"), None, Replace).unwrap();
    assert!(t.lookup(ip("11.0.0.1")).is_none());
    let m = t.lookup(ip("10.1.2.3")).unwrap();
    assert_eq!(m.prefix, net("10.1.0.0/16"));
    assert_eq!(m.data, None, "a more specific route without payload wins");
    let m = t.lookup(ip("10.2.2.3")).unwrap();
    assert_eq!((m.prefix, m.data), (net("10.0.0.0/8"), Some(&"ten")));
    assert_eq!(t.lookup_data(ip("10.1.2.3")), None);
    assert_eq!(t.lookup_data(ip("10.2.2.3")), Some(&"ten"));
}

#[test]
fn exist_semantics() {
    let mut t: IpTable<()> = IpTable::new();
    assert!(!t.exist(ip("1.2.3.4")));
    t.insert(net("1.2.3.0/24"), None, Replace).unwrap();
    assert!(t.exist(ip("1.2.3.4")), "routes without payload count");
    assert!(!t.exist(ip("1.2.4.4")));
    assert!(!t.exist(ip("::1")));
    t.insert(net("0.0.0.0/0"), None, Replace).unwrap();
    assert!(t.exist(ip("200.1.1.1")), "/0 covers the whole family");
    assert!(!t.exist(ip("2001:db8::1")), "but not the other family");
    t.insert(net("::/0"), None, Replace).unwrap();
    assert!(t.exist(ip("2001:db8::1")));
    assert!(t.contains_prefix(net("1.2.3.0/24")));
    assert!(
        !t.contains_prefix(net("1.2.3.0/25")),
        "exact match is not LPM"
    );
}

#[test]
fn ipv4_and_ipv4_mapped_ipv6_are_distinct_keys() {
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("192.0.2.0/24"), Some(4), Replace).unwrap();
    assert!(t.lookup(ip("::ffff:192.0.2.1")).is_none());
    t.insert(net("::ffff:192.0.2.0/120"), Some(6), Replace)
        .unwrap();
    assert_eq!(t.lookup_data(ip("192.0.2.1")), Some(&4));
    assert_eq!(t.lookup_data(ip("::ffff:192.0.2.1")), Some(&6));
    t.remove(net("192.0.2.0/24"));
    assert!(t.lookup(ip("192.0.2.1")).is_none());
    assert_eq!(t.lookup_data(ip("::ffff:192.0.2.1")), Some(&6));
}

#[test]
fn cidrs_are_normalized() {
    let mut t: IpTable<u8> = IpTable::new();
    assert_eq!(t.insert(net("10.1.2.3/8"), Some(1), Replace).unwrap(), None);
    assert_eq!(
        t.insert(net("10.0.0.0/8"), Some(2), Replace).unwrap(),
        Some(Some(1))
    );
    assert_eq!(t.len(), 1);
    assert_eq!(t.lookup(ip("10.9.9.9")).unwrap().prefix, net("10.0.0.0/8"));
    assert_eq!(t.get(net("10.200.0.0/8")), Some(Some(&2)));
    t.insert(net("2001:db8::1/32"), None, Replace).unwrap();
    assert_eq!(
        t.lookup(ip("2001:db8:ffff::1")).unwrap().prefix,
        net("2001:db8::/32")
    );
}

#[test]
fn insert_replace_remove_return_values() {
    let mut t: IpTable<String> = IpTable::new();
    let n = net("198.51.100.0/24");
    assert_eq!(t.insert(n, Some("a".into()), Replace).unwrap(), None);
    assert_eq!(
        t.insert(n, None, Replace).unwrap(),
        Some(Some("a".into())),
        "insert replaces by None"
    );
    assert_eq!(t.get(n), Some(None));
    assert_eq!(t.replace(n, Some("b".into())), Some(None));
    assert_eq!(
        t.replace(net("198.51.101.0/24"), Some("x".into())),
        None,
        "replace never inserts"
    );
    assert_eq!(t.len(), 1);
    *t.get_mut(n).unwrap() = Some("c".into());
    assert_eq!(
        t.lookup_data(ip("198.51.100.7")).map(String::as_str),
        Some("c")
    );
    assert_eq!(t.remove(n), Some(Some("c".into())));
    assert_eq!(t.remove(n), None);
    assert!(t.is_empty());
}

#[test]
fn removal_restores_the_parent() {
    let mut t: IpTable<u32> = IpTable::new();
    t.insert(net("0.0.0.0/0"), Some(0), Replace).unwrap();
    t.insert(net("10.0.0.0/8"), Some(8), Replace).unwrap();
    t.insert(net("10.0.0.0/30"), Some(30), Replace).unwrap();
    t.insert(net("10.0.0.1/32"), Some(32), Replace).unwrap();
    assert_eq!(t.lookup_data(ip("10.0.0.1")), Some(&32));
    t.remove(net("10.0.0.1/32"));
    assert_eq!(t.lookup_data(ip("10.0.0.1")), Some(&30));
    t.remove(net("10.0.0.0/30"));
    assert_eq!(t.lookup_data(ip("10.0.0.1")), Some(&8));
    t.remove(net("10.0.0.0/8"));
    assert_eq!(t.lookup_data(ip("10.0.0.1")), Some(&0));
    t.remove(net("0.0.0.0/0"));
    assert!(t.lookup(ip("10.0.0.1")).is_none());
    assert_eq!(t.groups_in_use_for_tests(), (0, 0));

    t.insert(net("2001:db8::/32"), Some(32), Replace).unwrap();
    t.insert(net("2001:db8:0:1::/64"), Some(64), Replace)
        .unwrap();
    t.insert(net("2001:db8:0:1::1/128"), Some(128), Replace)
        .unwrap();
    assert!(t.groups_in_use_for_tests().1 >= 13);
    t.remove(net("2001:db8:0:1::1/128"));
    assert_eq!(t.lookup_data(ip("2001:db8:0:1::1")), Some(&64));
    t.remove(net("2001:db8:0:1::/64"));
    assert_eq!(t.lookup_data(ip("2001:db8:0:1::1")), Some(&32));
    assert_eq!(
        t.groups_in_use_for_tests().1,
        1,
        "only the /32 group remains"
    );
    t.remove(net("2001:db8::/32"));
    assert_eq!(t.groups_in_use_for_tests(), (0, 0));
}

#[test]
fn short_prefix_inserted_after_long_ones_preserves_them() {
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("10.1.1.0/25"), Some(25), Replace).unwrap();
    t.insert(net("10.1.0.0/16"), Some(16), Replace).unwrap();
    t.insert(net("2001:db8:1:2::/64"), Some(64), Replace)
        .unwrap();
    t.insert(net("2001:db8::/29"), Some(29), Replace).unwrap();
    // Shorter prefixes inserted last must not overwrite longer ones.
    t.insert(net("0.0.0.0/0"), Some(0), Replace).unwrap();
    t.insert(net("::/0"), Some(0), Replace).unwrap();
    assert_eq!(t.lookup_data(ip("10.1.1.1")), Some(&25));
    assert_eq!(t.lookup_data(ip("10.1.1.200")), Some(&16));
    assert_eq!(t.lookup_data(ip("10.2.0.0")), Some(&0));
    assert_eq!(t.lookup_data(ip("2001:db8:1:2::9")), Some(&64));
    assert_eq!(t.lookup_data(ip("2001:db8:1:3::9")), Some(&29));
    assert_eq!(t.lookup_data(ip("2001:db9::")), Some(&29), "inside the /29");
    assert_eq!(t.lookup_data(ip("2001:dc0::")), Some(&0));
}

#[test]
fn groups_are_reused_after_release() {
    let mut t: IpTable<u8> = IpTable::new();
    for i in 0..64u32 {
        t.insert(
            IpNet::V4(dir_24_8::Ipv4Net::new(Ipv4Addr::from(i << 8), 25).unwrap()),
            Some(1),
            Replace,
        )
        .unwrap();
    }
    let s = t.stats();
    assert_eq!(s.ipv4.groups_in_use, 64);
    let cap = s.ipv4.groups_capacity;
    for i in 0..64u32 {
        t.remove(IpNet::V4(
            dir_24_8::Ipv4Net::new(Ipv4Addr::from(i << 8), 25).unwrap(),
        ));
    }
    let s = t.stats();
    assert_eq!((s.ipv4.groups_in_use, s.ipv4.groups_free), (0, 64));
    for i in 100..164u32 {
        t.insert(
            IpNet::V4(dir_24_8::Ipv4Net::new(Ipv4Addr::from(i << 8), 26).unwrap()),
            Some(1),
            Replace,
        )
        .unwrap();
    }
    let s = t.stats();
    assert_eq!((s.ipv4.groups_in_use, s.ipv4.groups_free), (64, 0));
    assert_eq!(
        s.ipv4.groups_capacity, cap,
        "released groups are reused before growing"
    );
}

#[test]
fn payload_types_need_no_clone_and_results_are_copy() {
    struct NoClone(#[allow(dead_code)] Vec<u8>);
    let mut t: IpTable<NoClone> = IpTable::new();
    t.insert(net("10.0.0.0/8"), Some(NoClone(vec![1, 2, 3])), Replace)
        .unwrap();
    t.insert(net("::/0"), None, Replace).unwrap();
    let a = t.lookup(ip("10.0.0.1")).unwrap();
    let b: LookupMatch<'_, NoClone> = a; // Copy without T: Copy
    assert!(std::ptr::eq(a.data.unwrap(), b.data.unwrap()));
    let arr = t.lookup_array(&[ip("10.1.1.1"), ip("::1"), ip("11.0.0.0")]);
    assert!(arr[0].unwrap().data.is_some());
    assert!(arr[1].unwrap().data.is_none());
    assert!(arr[2].is_none());
    // All cells of a route share the same payload (same address).
    let p1 = t.lookup_data(ip("10.0.0.1")).unwrap() as *const NoClone;
    let p2 = t.lookup_data(ip("10.255.255.255")).unwrap() as *const NoClone;
    assert_eq!(p1, p2);
}

#[test]
fn batch_length_is_validated_before_writing() {
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("10.0.0.0/8"), Some(1), Replace).unwrap();
    let keys = [ip("10.0.0.1"), ip("10.0.0.2")];
    let mut short = [None; 1];
    assert!(matches!(
        t.lookup_batch_into(&keys, &mut short),
        Err(Error::LengthMismatch {
            input: 2,
            output: 1
        })
    ));
    assert!(short[0].is_none(), "nothing written on error");
    let mut long = [None; 3];
    assert!(t.lookup_batch_into(&keys, &mut long).is_err());
    let mut ex = [false; 3];
    assert!(t.exist_batch_into(&keys, &mut ex).is_err());
    assert_eq!(ex, [false; 3]);
    let mut empty: [Option<dir_24_8::LookupMatch<'_, u8>>; 0] = [];
    t.lookup_batch_into::<IpAddr>(&[], &mut empty).unwrap();
}

#[test]
fn lookups_on_unallocated_families() {
    let t: IpTable<u8> = IpTable::new();
    assert!(t.lookup(ip("1.1.1.1")).is_none());
    assert!(t.lookup(ip("::1")).is_none());
    assert!(!t.stats().ipv4.allocated);
    let mut out = [None; 3];
    t.lookup_batch_into(&[ip("1.1.1.1"), ip("::1"), ip("2.2.2.2")], &mut out)
        .unwrap();
    assert!(out.iter().all(Option::is_none));
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("1.0.0.0/8"), Some(1), Replace).unwrap();
    let s = t.stats();
    assert!(s.ipv4.allocated && !s.ipv6.allocated);
    assert_eq!(s.ipv4.tbl24_bytes, 64 << 20);
    assert_eq!(s.ipv6.total_bytes(), 0);
    let out = t.lookup_array(&[ip("1.1.1.1"), ip("::1")]);
    assert!(out[0].is_some() && out[1].is_none());
}

#[test]
fn iteration_and_stats() {
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("10.0.0.0/8"), Some(1), Replace).unwrap();
    t.insert(net("10.0.0.0/25"), None, Replace).unwrap();
    t.insert(net("2001:db8::/48"), Some(2), Replace).unwrap();
    let mut routes: Vec<_> = t.iter().map(|(n, d)| (n, d.copied())).collect();
    routes.sort();
    assert_eq!(routes.len(), 3);
    assert!(routes.contains(&(net("10.0.0.0/25"), None)));
    for (_, d) in t.iter_mut() {
        if let Some(v) = d {
            *v += 10;
        }
    }
    assert_eq!(t.lookup_data(ip("10.1.1.1")), Some(&11));
    let s = t.stats();
    assert_eq!((s.ipv4.routes, s.ipv4.routes_with_data), (2, 1));
    assert_eq!(s.ipv4.groups_in_use, 1);
    assert_eq!(s.ipv6.groups_in_use, 3, "/48 = root + 3 levels");
    assert_eq!(s.routes(), 3);
    assert!(s.total_bytes() >= 128 << 20);
    let text = s.to_string();
    assert!(text.contains("IPv4") && text.contains("IPv6"));
    t.clear();
    assert!(t.is_empty() && !t.stats().ipv4.allocated);
}

#[test]
fn ipv6_unaligned_prefixes_expand_inside_one_group() {
    let mut t: IpTable<u8> = IpTable::new();
    t.insert(net("2001:db8:1200::/41"), Some(41), Replace)
        .unwrap();
    assert_eq!(t.lookup_data(ip("2001:db8:127f:ffff::")), Some(&41));
    assert_eq!(t.lookup_data(ip("2001:db8:1280::")), None);
    t.insert(net("2001:db8:1200::/39"), Some(39), Replace)
        .unwrap();
    assert_eq!(t.lookup_data(ip("2001:db8:1280::")), Some(&39));
    assert_eq!(t.lookup_data(ip("2001:db8:1400::")), None);
    t.insert(net("2001:d00::/23"), Some(23), Replace).unwrap();
    assert_eq!(t.lookup_data(ip("2001:dff::")), Some(&23));
    assert_eq!(t.lookup_data(ip("2001:e00::")), None);
    let v6 = Ipv6Addr::from(0x2001_0db8_1234_0000_0000_0000_0000_00ffu128);
    t.insert(
        IpNet::V6(dir_24_8::Ipv6Net::new(v6, 121).unwrap().trunc()),
        Some(121),
        Replace,
    )
    .unwrap();
    assert_eq!(t.lookup_data(IpAddr::V6(v6)), Some(&121));
}

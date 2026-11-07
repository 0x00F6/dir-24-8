//! Capacity limits, configuration validation and consistency after errors.

mod common;

use std::net::IpAddr;

use dir_24_8::Replace;
use dir_24_8::{AddressFamily, Config, Error, IpNet, IpTable, Resource};

fn net(s: &str) -> IpNet {
    s.parse().unwrap()
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn ipv4_group_limit_leaves_table_unchanged() {
    let mut t: IpTable<u8> = IpTable::with_config(Config::new().max_ipv4_groups(2)).unwrap();
    t.insert(net("10.0.0.0/8"), Some(8), Replace).unwrap();
    t.insert(net("10.0.0.0/25"), Some(1), Replace).unwrap();
    t.insert(net("10.0.1.0/25"), Some(2), Replace).unwrap();
    let before = t.stats();
    let err = t.insert(net("10.0.2.0/25"), Some(3), Replace).unwrap_err();
    assert!(matches!(
        err,
        Error::CapacityExceeded {
            family: AddressFamily::Ipv4,
            resource: Resource::Groups,
            limit: 2
        }
    ));
    assert_eq!(t.stats(), before);
    assert_eq!(t.len(), 3);
    assert_eq!(
        t.lookup_data(ip("10.0.2.1")),
        Some(&8),
        "failed route is not visible"
    );
    assert!(!t.contains_prefix(net("10.0.2.0/25")));
    // Routes that need no new group still fit.
    t.insert(net("10.0.0.128/25"), Some(4), Replace).unwrap();
    t.insert(net("10.0.3.0/24"), Some(5), Replace).unwrap();
    t.insert(net("10.0.0.0/25"), Some(9), Replace).unwrap(); // replacement
    // Releasing a group makes room again.
    t.remove(net("10.0.1.0/25"));
    t.insert(net("10.0.2.0/25"), Some(3), Replace).unwrap();
    assert_eq!(t.lookup_data(ip("10.0.2.1")), Some(&3));
}

#[test]
fn ipv6_path_needing_several_groups_is_all_or_nothing() {
    let mut t: IpTable<u8> = IpTable::with_config(Config::new().max_ipv6_groups(4)).unwrap();
    t.insert(net("2001:db8::/48"), Some(48), Replace).unwrap(); // 3 groups
    let before = t.stats();
    // A /64 elsewhere needs 5 new groups: refused before any write.
    let err = t
        .insert(net("2001:db9::/64"), Some(64), Replace)
        .unwrap_err();
    assert!(matches!(
        err,
        Error::CapacityExceeded {
            family: AddressFamily::Ipv6,
            ..
        }
    ));
    assert_eq!(t.stats(), before);
    assert!(t.lookup(ip("2001:db9::1")).is_none());
    assert_eq!(t.lookup_data(ip("2001:db8::1")), Some(&48));
    // Extending the existing path by one level fits (4th group).
    t.insert(net("2001:db8:0:1::/56"), Some(56), Replace)
        .unwrap();
    assert_eq!(t.lookup_data(ip("2001:db8:0:1::1")), Some(&56));
}

#[test]
fn route_limit() {
    let mut t: IpTable<()> = IpTable::with_config(Config::new().max_routes(2)).unwrap();
    t.insert(net("1.0.0.0/8"), None, Replace).unwrap();
    t.insert(net("2.0.0.0/8"), None, Replace).unwrap();
    let err = t.insert(net("3.0.0.0/8"), None, Replace).unwrap_err();
    assert!(matches!(
        err,
        Error::CapacityExceeded {
            resource: Resource::Routes,
            limit: 2,
            ..
        }
    ));
    assert!(!t.exist(ip("3.1.1.1")));
    t.insert(net("1.0.0.0/8"), None, Replace).unwrap(); // replacement is fine
    t.remove(net("1.0.0.0/8"));
    t.insert(net("3.0.0.0/8"), None, Replace).unwrap(); // freed slot reused
    // Limits are per family.
    t.insert(net("2001:db8::/32"), None, Replace).unwrap();
    t.insert(net("2001:db9::/32"), None, Replace).unwrap();
    assert!(t.insert(net("2001:dba::/32"), None, Replace).is_err());
}

#[test]
fn invalid_configurations() {
    assert!(matches!(
        IpTable::<u8>::with_config(Config::new().max_ipv4_groups(Config::MAX_GROUPS + 1)),
        Err(Error::InvalidConfig(_))
    ));
    assert!(matches!(
        IpTable::<u8>::with_config(Config::new().max_routes(u32::MAX)),
        Err(Error::InvalidConfig(_))
    ));
    let e = Error::LengthMismatch {
        input: 1,
        output: 2,
    };
    assert!(e.to_string().contains("2 slots"));
}

#[test]
fn many_groups_grow_the_buffer() {
    let mut t: IpTable<u32> = IpTable::new();
    for i in 0..5000u32 {
        let n = IpNet::V4(
            dir_24_8::Ipv4Net::new(std::net::Ipv4Addr::from((i << 8) | 0x80), 26)
                .unwrap()
                .trunc(),
        );
        t.insert(n, Some(i), Replace).unwrap();
    }
    let s = t.stats();
    assert_eq!(s.ipv4.groups_in_use, 5000);
    assert!(s.ipv4.groups_capacity >= 5000);
    for i in (0..5000u32).step_by(97) {
        let a = std::net::Ipv4Addr::from((i << 8) | 0x81);
        assert_eq!(t.lookup_data(IpAddr::V4(a)), Some(&i));
        assert!(
            t.lookup(IpAddr::V4(std::net::Ipv4Addr::from(i << 8)))
                .is_none()
        );
    }
}

//! IPv6 longest-prefix match, including prefixes that are not multiples of 8
//! and the separation from IPv4-mapped addresses.
//!
//! Run with `cargo run --example ipv6_lpm`.

use std::net::{IpAddr, Ipv6Addr};

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable};

fn main() -> Result<(), dir_24_8::Error> {
    let mut table: IpTable<u32> = IpTable::new();
    let net = |s: &str| s.parse::<IpNet>().unwrap();

    table.insert(net("2001:db8::/32"), Some(32), Replace)?;
    table.insert(net("2001:db8:1200::/41"), Some(41), Replace)?; // not a multiple of 8
    table.insert(net("2001:db8:1234::/48"), Some(48), Replace)?;
    table.insert(net("2001:db8:1234:5678::/64"), Some(64), Replace)?;
    table.insert(net("2001:db8:1234:5678::1/128"), Some(128), Replace)?;
    table.insert(net("::ffff:192.0.2.0/120"), Some(6), Replace)?;

    for (addr, expected) in [
        ("2001:db8:1234:5678::1", Some(128)),
        ("2001:db8:1234:5678::2", Some(64)),
        ("2001:db8:1234:ffff::", Some(48)),
        ("2001:db8:127f::", Some(41)),
        ("2001:db8:1280::", Some(32)),
        ("2001:db9::", None),
    ] {
        let ip: Ipv6Addr = addr.parse().unwrap();
        let got = table.lookup_ipv6(ip).and_then(|m| m.data.copied());
        println!("{addr:<26} -> {got:?}");
        assert_eq!(got, expected);
    }

    // IPv4 and IPv4-mapped IPv6 are distinct keys.
    assert!(
        table
            .lookup("192.0.2.1".parse::<IpAddr>().unwrap())
            .is_none()
    );
    assert_eq!(
        table.lookup_data("::ffff:192.0.2.1".parse().unwrap()),
        Some(&6)
    );

    let s = table.stats().ipv6;
    println!(
        "{} IPv6 routes use {} secondary groups",
        s.routes, s.groups_in_use
    );
    Ok(())
}

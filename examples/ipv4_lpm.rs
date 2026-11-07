//! IPv4 longest-prefix match with nested prefixes and single lookups.
//!
//! Run with `cargo run --example ipv4_lpm`.

use std::net::{IpAddr, Ipv4Addr};

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable};

fn main() -> Result<(), dir_24_8::Error> {
    let mut table: IpTable<&'static str> = IpTable::new();
    let net = |s: &str| s.parse::<IpNet>().unwrap();

    table.insert(net("0.0.0.0/0"), Some("default"), Replace)?;
    table.insert(net("10.0.0.0/8"), Some("corp"), Replace)?;
    table.insert(net("10.20.0.0/16"), Some("site-20"), Replace)?;
    table.insert(net("10.20.30.0/24"), Some("lab"), Replace)?;
    table.insert(net("10.20.30.128/25"), Some("lab-dmz"), Replace)?;
    table.insert_addr(Ipv4Addr::new(10, 20, 30, 200), Some("bastion"), Replace)?; // /32

    for (addr, expected_prefix, expected) in [
        ("10.20.30.200", "10.20.30.200/32", "bastion"),
        ("10.20.30.129", "10.20.30.128/25", "lab-dmz"),
        ("10.20.30.1", "10.20.30.0/24", "lab"),
        ("10.20.99.1", "10.20.0.0/16", "site-20"),
        ("10.99.0.1", "10.0.0.0/8", "corp"),
        ("8.8.8.8", "0.0.0.0/0", "default"),
    ] {
        let ip: Ipv4Addr = addr.parse().unwrap();
        // `lookup_ipv4` returns the winning prefix and a borrowed payload.
        let m = table.lookup_ipv4(ip).expect("the /0 covers everything");
        println!("{addr:<15} -> {:<18} {:?}", m.prefix.to_string(), m.data);
        assert_eq!(m.prefix, net(expected_prefix));
        assert_eq!(m.data, Some(&expected));
        // `exist` only answers "is any route covering it?".
        assert!(table.exist(IpAddr::V4(ip)));
    }

    // Numeric keys use the network-order convention u32::from_be_bytes(octets).
    let numeric = u32::from_be_bytes([10, 20, 30, 1]);
    assert_eq!(
        table.lookup_ipv4(Ipv4Addr::from(numeric)).unwrap().data,
        Some(&"lab")
    );
    println!("{} routes", table.len());
    Ok(())
}

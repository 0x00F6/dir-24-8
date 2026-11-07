//! Using the table as a CIDR set: routes without payload and `exist`.
//!
//! Run with `cargo run --example exist_without_payload`.

use std::net::IpAddr;

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable};

fn main() -> Result<(), dir_24_8::Error> {
    // `()` payloads, inserted as `None`: the table only stores the prefixes.
    let mut blocklist: IpTable<()> = IpTable::new();
    for cidr in ["192.0.2.0/24", "198.51.100.128/25", "2001:db8:bad::/48"] {
        blocklist.insert(cidr.parse::<IpNet>().unwrap(), None, Replace)?;
    }
    blocklist.insert_addr("203.0.113.7".parse::<IpAddr>().unwrap(), None, Replace)?;

    for (addr, blocked) in [
        ("192.0.2.55", true),
        ("198.51.100.200", true),
        ("198.51.100.1", false),
        ("203.0.113.7", true),
        ("203.0.113.8", false),
        ("2001:db8:bad:1::1", true),
        ("2001:db8:900d::1", false),
    ] {
        let ip: IpAddr = addr.parse().unwrap();
        // `exist` is true for routes without payload as well.
        assert_eq!(blocklist.exist(ip), blocked);
        // `lookup` still reports the matching prefix, with `data: None`.
        if let Some(m) = blocklist.lookup(ip) {
            assert!(m.data.is_none());
            println!("{addr:<20} blocked by {}", m.prefix);
        } else {
            println!("{addr:<20} allowed");
        }
    }

    // A /0 covers its whole family, and only that family.
    blocklist.insert("0.0.0.0/0".parse::<IpNet>().unwrap(), None, Replace)?;
    assert!(blocklist.exist("8.8.8.8".parse().unwrap()));
    assert!(!blocklist.exist("2001:4860::8888".parse().unwrap()));

    let queries: Vec<IpAddr> = vec!["1.1.1.1".parse().unwrap(), "::1".parse().unwrap()];
    let mut flags = vec![false; queries.len()];
    blocklist.exist_batch_into(&queries, &mut flags)?;
    assert_eq!(flags, [true, false]);
    Ok(())
}

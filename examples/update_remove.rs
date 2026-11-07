//! Insertions, replacements and removals: removing a route restores the best
//! remaining less specific route, and released groups are reused.
//!
//! Run with `cargo run --example update_remove`.

use std::net::IpAddr;

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable};

fn main() -> Result<(), dir_24_8::Error> {
    let mut t: IpTable<&str> = IpTable::new();
    let net = |s: &str| s.parse::<IpNet>().unwrap();
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();

    assert_eq!(t.insert(net("10.0.0.0/8"), Some("a"), Replace)?, None);
    assert_eq!(
        t.insert(net("10.0.0.0/8"), Some("b"), Replace)?,
        Some(Some("a"))
    ); // replaced
    assert_eq!(t.insert(net("10.1.2.3/8"), None, Replace)?, Some(Some("b"))); // normalized, replaced by None
    assert_eq!(t.replace(net("10.0.0.0/8"), Some("c")), Some(None)); // payload only
    assert_eq!(t.replace(net("11.0.0.0/8"), Some("x")), None); // never inserts

    t.insert(net("10.1.0.0/16"), Some("d"), Replace)?;
    t.insert(net("10.1.1.0/28"), Some("e"), Replace)?; // needs a secondary group
    assert_eq!(t.lookup_data(ip("10.1.1.1")), Some(&"e"));
    println!("groups in use: {}", t.stats().ipv4.groups_in_use);

    assert_eq!(t.remove(net("10.1.1.0/28")), Some(Some("e")));
    assert_eq!(t.lookup_data(ip("10.1.1.1")), Some(&"d")); // parent restored
    assert_eq!(t.remove(net("10.1.0.0/16")), Some(Some("d")));
    assert_eq!(t.lookup_data(ip("10.1.1.1")), Some(&"c"));
    assert_eq!(t.remove(net("10.1.0.0/16")), None); // already gone

    let s = t.stats().ipv4;
    println!(
        "after removals: {} groups in use, {} free for reuse",
        s.groups_in_use, s.groups_free
    );
    assert_eq!(s.groups_in_use, 0);
    assert_eq!(t.len(), 1);
    Ok(())
}

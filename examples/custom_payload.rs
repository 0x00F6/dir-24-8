//! A user-defined payload type without `Clone`: lookups borrow it, all cells
//! of a route share it, and nothing is copied.
//!
//! Run with `cargo run --example custom_payload`.

use std::net::IpAddr;

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable};

/// Deliberately neither `Clone` nor `Copy`.
#[derive(Debug)]
struct NextHop {
    interface: String,
    gateway: IpAddr,
    metrics: Vec<u32>,
}

fn main() -> Result<(), dir_24_8::Error> {
    let mut fib: IpTable<NextHop> = IpTable::new();
    fib.insert(
        "10.0.0.0/8".parse::<IpNet>().unwrap(),
        Some(NextHop {
            interface: "eth0".into(),
            gateway: "10.0.0.1".parse().unwrap(),
            metrics: vec![10],
        }),
        Replace,
    )?;
    fib.insert(
        "2001:db8::/32".parse::<IpNet>().unwrap(),
        Some(NextHop {
            interface: "eth1".into(),
            gateway: "fe80::1".parse().unwrap(),
            metrics: vec![20, 5],
        }),
        Replace,
    )?;

    let a = fib.lookup_data("10.1.2.3".parse().unwrap()).unwrap();
    let b = fib.lookup_data("10.200.0.1".parse().unwrap()).unwrap();
    // Same route, same payload: both lookups borrow the very same value.
    assert!(std::ptr::eq(a, b));
    println!(
        "10.1.2.3 via {} ({}) metrics {:?}",
        a.interface, a.gateway, a.metrics
    );

    let v6 = fib.lookup("2001:db8::42".parse().unwrap()).unwrap();
    println!(
        "2001:db8::42 matched {} via {:?}",
        v6.prefix,
        v6.data.map(|h| &h.interface)
    );

    // Payloads are mutated in place without touching the lookup tables.
    if let Some(Some(hop)) = fib.get_mut("10.0.0.0/8".parse::<IpNet>().unwrap()) {
        hop.metrics.push(99);
    }
    assert_eq!(
        fib.lookup_data("10.9.9.9".parse().unwrap())
            .unwrap()
            .metrics,
        [10, 99]
    );
    Ok(())
}

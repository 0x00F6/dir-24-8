//! Batch lookups: mixed families, homogeneous slices, numeric fast paths,
//! fixed-size arrays and compact identifiers.
//!
//! Run with `cargo run --release --example batch_lookup` (scalar batch
//! kernel) or add `--features batch-simd` (AVX-512, AVX2 or NEON when the
//! CPU has it); the results are the same.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use dir_24_8::Replace;
use dir_24_8::{IpNet, IpTable, LookupMatch, MatchId};

fn main() -> Result<(), dir_24_8::Error> {
    let mut table: IpTable<String> = IpTable::new();
    for (cidr, name) in [
        ("192.0.2.0/24", "doc-v4"),
        ("198.51.100.0/24", "doc-v4-2"),
        ("2001:db8::/32", "doc-v6"),
    ] {
        table.insert(
            cidr.parse::<IpNet>().unwrap(),
            Some(name.to_string()),
            Replace,
        )?;
    }

    // Mixed batch: the output buffer is provided by the caller, results keep
    // the input order and borrow the stored strings.
    let keys: Vec<IpAddr> = [
        "192.0.2.7",
        "2001:db8::1",
        "203.0.113.1",
        "198.51.100.9",
        "::1",
    ]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();
    let mut out: Vec<Option<LookupMatch<'_, String>>> = vec![None; keys.len()];
    table.lookup_batch_into(&keys, &mut out)?;
    for (k, r) in keys.iter().zip(&out) {
        println!(
            "{k:<14} -> {:?}",
            r.map(|m| (m.prefix.to_string(), m.data.map(String::as_str)))
        );
    }
    let names: Vec<Option<&str>> = out
        .iter()
        .map(|r| r.and_then(|m| m.data).map(String::as_str))
        .collect();
    assert_eq!(
        names,
        [Some("doc-v4"), Some("doc-v6"), None, Some("doc-v4-2"), None]
    );

    // The output length is validated before anything is written.
    let mut short = vec![None; 2];
    assert!(table.lookup_batch_into(&keys, &mut short).is_err());

    // Homogeneous and numeric keys skip the family partitioning.
    let v4: Vec<Ipv4Addr> = (0..100u8).map(|i| Ipv4Addr::new(192, 0, 2, i)).collect();
    let mut out4 = vec![None; v4.len()];
    table.lookup_batch_into(&v4, &mut out4)?;
    assert!(out4.iter().all(|r| r.is_some()));
    let n4: Vec<u32> = v4.iter().map(|&a| u32::from(a)).collect(); // u32::from_be_bytes(octets)
    table.lookup_batch_into(&n4, &mut out4)?;
    let n6: Vec<u128> = vec![u128::from("2001:db8::5".parse::<Ipv6Addr>().unwrap())];
    let mut out6 = vec![None; 1];
    table.lookup_batch_into(&n6, &mut out6)?;
    assert_eq!(out6[0].unwrap().data.map(String::as_str), Some("doc-v6"));

    // Fixed-size arrays are returned by value.
    let arr = table.lookup_array(&[Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(10, 0, 0, 1)]);
    assert!(arr[0].is_some() && arr[1].is_none());
    let exists = table.exist_array(&keys[..3].try_into().map(|a: [IpAddr; 3]| a).unwrap());
    assert_eq!(exists, [true, true, false]);

    // Identifiers: the kernels' raw output, resolved on demand.
    let mut ids = vec![MatchId::NONE; n4.len()];
    table.lookup_ids_into(&n4, &mut ids)?;
    let first = table.resolve(ids[0]).unwrap();
    assert_eq!(first.prefix.to_string(), "192.0.2.0/24");
    println!(
        "{} identifiers matched, batch kernel {}",
        ids.iter().filter(|i| i.is_match()).count(),
        table.stats().batch_kernel
    );
    Ok(())
}

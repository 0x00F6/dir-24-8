//! Compressed snapshots: save a table to memory and to a file, restore it,
//! and check that the restored table behaves like the original.
//!
//! ```text
//! cargo run --release --features snapshot --example snapshot
//! ```

use std::net::IpAddr;

use dir_24_8::{
    EntryOrder, IpNet, IpTable, LoadOptions, Merge, OverBudget, Replace, SnapshotError,
    SnapshotLimit, SnapshotOptions,
};
use serde::{Deserialize, Serialize};

/// Payload: any `Serialize` type can be saved, any `DeserializeOwned` type
/// can be loaded. `skip_serializing_if` and other serde attributes are
/// fine: payloads are stored as CBOR, a self-describing format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Route {
    asn: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    tags: Vec<String>,
}

impl dir_24_8::DeepMerge for Route {
    fn deep_merge(&mut self, incoming: Self) {
        self.asn = incoming.asn;
        if incoming.country.is_some() {
            self.country = incoming.country;
        }
        self.tags.extend(incoming.tags);
    }
}

fn route(asn: u32, country: Option<&str>, tags: &[&str]) -> Route {
    Route {
        asn,
        country: country.map(str::to_owned),
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
    }
}

fn net(s: &str) -> IpNet {
    s.parse().expect("valid CIDR")
}

fn ip(s: &str) -> IpAddr {
    s.parse().expect("valid address")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A table with both families, nested and masked prefixes, /0, /32,
    // /128 and a route without payload.
    let mut table: IpTable<Route> = IpTable::new();
    table.insert(
        net("0.0.0.0/0"),
        Some(route(1, None, &["default"])),
        Replace,
    )?;
    table.insert(
        net("10.0.0.0/8"),
        Some(route(64500, Some("FR"), &["corp"])),
        Replace,
    )?;
    table.insert(net("10.1.0.0/16"), None, Replace)?; // present, no payload
    table.insert(net("10.2.0.0/24"), Some(route(64501, None, &[])), Replace)?; // masked below
    table.insert(
        net("10.2.0.0/25"),
        Some(route(64502, None, &["lo"])),
        Replace,
    )?;
    table.insert(
        net("10.2.0.128/25"),
        Some(route(64503, None, &["hi"])),
        Replace,
    )?;
    table.insert(
        net("192.0.2.1/32"),
        Some(route(64504, Some("US"), &["host"])),
        Replace,
    )?;
    table.insert(
        net("2001:db8::/32"),
        Some(route(64496, None, &["doc"])),
        Replace,
    )?;
    table.insert(
        net("2001:db8::1/128"),
        Some(route(64497, Some("DE"), &[])),
        Replace,
    )?;
    for i in 0..2000u32 {
        let v4 = format!("172.{}.{}.0/24", 16 + (i >> 8) % 16, i & 0xFF);
        table.insert(
            net(&v4),
            Some(route(65000 + i % 50, Some("NL"), &["bulk"])),
            Replace,
        )?;
    }

    // 1. In memory: the encoder writes straight into the returned Vec.
    let bytes: Vec<u8> = table.snapshot()?;
    println!(
        "snapshot: {} routes -> {} bytes ({:.2} bytes per route)",
        table.len(),
        bytes.len(),
        bytes.len() as f64 / table.len() as f64
    );

    // 2. Load from the borrowed Vec: no copy of `bytes`.
    let restored: IpTable<Route> = IpTable::load(&bytes)?;
    check_same(&table, &restored);

    // 3. To a file (temporary file + rename), then back.
    let dir = std::env::temp_dir().join(format!("dir-24-8-example-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("routes.snap");
    table.snapshot_to_file(&path)?;
    let from_file: IpTable<Route> = IpTable::load_from_file(&path)?;
    check_same(&table, &from_file);
    // The same format in memory and on disk.
    check_same(&table, &IpTable::<Route>::load(std::fs::read(&path)?)?);

    // 4. Options: faster compression, sorted entries, bounded encoder
    //    memory (shrinking the dictionary rather than failing), 2 threads.
    let options = SnapshotOptions::new()
        .level(6)
        .extreme(false)
        .order(EntryOrder::Sorted)
        .memory_budget(64 << 20)
        .over_budget(OverBudget::ShrinkDictionary)
        .threads(2);
    let mut out = Vec::new();
    let summary = table.snapshot_to_writer(&mut out, &options)?;
    println!(
        "level 6, sorted: {} -> {} bytes, dictionary {} KiB, encoder memory {} KiB",
        summary.uncompressed_bytes,
        summary.compressed_bytes,
        summary.dict_size >> 10,
        summary.encoder_memory >> 10
    );

    // 5. Limits for untrusted input.
    let strict = LoadOptions::new()
        .decoder_memory_limit(32 << 20)
        .max_decompressed_bytes(16 << 20)
        .max_entries(10_000)
        .max_value_bytes(1024);
    check_same(&table, &IpTable::load_with_options(&out, &strict)?);
    match IpTable::<Route>::load_with_options(&out, &LoadOptions::new().max_entries(100)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::Entries,
            max,
            requested,
        }) => println!("limit enforced: {requested:?} entries declared, max {max}"),
        other => panic!("expected an entry limit error, got {other:?}"),
    }

    // 6. Damaged data is detected.
    let mut damaged = bytes.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x40;
    let err = IpTable::<Route>::load(&damaged).unwrap_err();
    println!("damaged snapshot: {err}");
    let err = IpTable::<Route>::load(&bytes[..bytes.len() - 1]).unwrap_err();
    assert!(matches!(err, SnapshotError::Truncated), "{err}");

    // 7. The restored table is a regular, mutable table.
    let mut restored = from_file;
    restored.remove(net("10.2.0.0/25")); // the masked /24 becomes visible
    assert_eq!(
        restored.lookup_data(ip("10.2.0.1")).map(|r| r.asn),
        Some(64501)
    );
    restored.insert(
        net("10.0.0.0/8"),
        Some(route(64999, None, &["merged"])),
        Merge,
    )?;
    let corp = restored.lookup_data(ip("10.9.9.9")).expect("covered");
    assert_eq!((corp.asn, corp.country.as_deref()), (64999, Some("FR")));
    assert_eq!(corp.tags, ["corp", "merged"]);

    std::fs::remove_dir_all(&dir)?;
    println!("ok");
    Ok(())
}

/// Same routes, payloads and lookup results.
fn check_same(a: &IpTable<Route>, b: &IpTable<Route>) {
    assert_eq!(a.len_ipv4(), b.len_ipv4());
    assert_eq!(a.len_ipv6(), b.len_ipv6());
    for (prefix, data) in a.iter() {
        assert_eq!(b.get(prefix), Some(data), "{prefix}");
    }
    let probes = [
        "10.0.0.1",
        "10.1.2.3",
        "10.2.0.1",
        "10.2.0.200",
        "192.0.2.1",
        "192.0.2.2",
        "8.8.8.8",
        "172.16.3.4",
        "2001:db8::1",
        "2001:db8::2",
        "2001:db9::1",
    ];
    for p in probes {
        assert_eq!(a.lookup(ip(p)), b.lookup(ip(p)), "{p}");
        assert_eq!(a.exist(ip(p)), b.exist(ip(p)), "{p}");
    }
    let keys: Vec<IpAddr> = probes.iter().map(|p| ip(p)).collect();
    let (mut x, mut y) = (vec![None; keys.len()], vec![None; keys.len()]);
    a.lookup_batch_into(&keys, &mut x).expect("same length");
    b.lookup_batch_into(&keys, &mut y).expect("same length");
    assert_eq!(x, y);
}

//! Typed deep merge: records inserted for the same CIDR are merged with
//! `#[derive(DeepMerge)]` and the `Merge` insert mode.
//!
//! Run with `cargo run --example typed_deep_merge --features derive`.

use std::collections::BTreeMap;

use dir_24_8::{DeepMerge, IpNet, IpTable, Merge, MergeOutcome, Replace};

// No hand-written `impl DeepMerge`: every field is merged with its own rule.
#[derive(DeepMerge, Debug, Default, PartialEq)]
struct Intel {
    sources: Vec<String>,        // concatenated
    risk: BTreeMap<String, i32>, // merged per key
    country: Option<String>,     // `None` keeps the existing value
    asn: u32,                    // replaced
    #[deep_merge(skip)]
    first_seen: u64, // the first value is kept
}

fn main() -> Result<(), dir_24_8::Error> {
    let mut table: IpTable<Intel> = IpTable::new();
    let net: IpNet = "203.0.113.0/24".parse().unwrap();

    let first = Intel {
        sources: vec!["geo-feed".into()],
        risk: BTreeMap::from([("geo".into(), 1), ("fraud".into(), 2)]),
        country: Some("FR".into()),
        asn: 64500,
        first_seen: 1_700_000_000,
    };
    let second = Intel {
        sources: vec!["threat-feed".into()],
        risk: BTreeMap::from([("fraud".into(), 9), ("bot".into(), 4)]),
        country: None,
        asn: 64501,
        first_seen: 1_800_000_000,
    };

    assert_eq!(
        table.insert(net, Some(first), Merge)?,
        MergeOutcome::Inserted
    );
    assert_eq!(table.insert(net, None, Merge)?, MergeOutcome::Unchanged); // None keeps data
    assert_eq!(
        table.insert(net, Some(second), Merge)?,
        MergeOutcome::Merged
    );

    let merged = table.lookup_data("203.0.113.9".parse().unwrap()).unwrap();
    println!("{merged:#?}");
    assert_eq!(merged.sources, ["geo-feed", "threat-feed"]);
    assert_eq!(
        merged.risk,
        BTreeMap::from([("geo".into(), 1), ("fraud".into(), 9), ("bot".into(), 4)])
    );
    assert_eq!(merged.country.as_deref(), Some("FR"));
    assert_eq!(merged.asn, 64501);
    assert_eq!(merged.first_seen, 1_700_000_000);

    // Merging into a route without payload adopts the incoming value.
    let bare: IpNet = "198.51.100.0/24".parse().unwrap();
    table.insert(bare, None, Replace)?;
    assert_eq!(
        table.insert(
            bare,
            Some(Intel {
                asn: 1,
                ..Intel::default()
            }),
            Merge
        )?,
        MergeOutcome::Adopted
    );

    // A merge never touches other CIDRs, even nested ones.
    table.insert(
        "203.0.113.128/25".parse::<IpNet>().unwrap(),
        Some(Intel::default()),
        Replace,
    )?;
    assert_eq!(
        table.lookup_data("203.0.113.200".parse().unwrap()),
        Some(&Intel::default())
    );
    Ok(())
}

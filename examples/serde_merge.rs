//! Serde deep merge (feature `serde`), following the rules of the
//! libmaxminddb-rs `deep_merge` example: maps merge recursively, arrays are
//! concatenated, scalars are replaced, omitted fields are preserved.
//!
//! Run with `cargo run --example serde_merge --features serde`.

use std::collections::{BTreeMap, HashMap};

use dir_24_8::SerdeMerge;
use dir_24_8::{IpNet, IpTable, MergeOutcome};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
enum PolicyMode {
    Observe,
    Enforce,
}

#[derive(Debug, Serialize, Deserialize)]
struct Coordinates {
    #[serde(skip_serializing_if = "Option::is_none")]
    latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    longitude: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Location {
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coordinates: Option<Coordinates>,
}

#[derive(Debug, Serialize, Deserialize)]
struct NetworkIntelligence {
    source: String,
    asn: u32,
    is_proxy: bool,
    mode: PolicyMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_only: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_only: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
    tags: Vec<String>,
    risk_by_feed: BTreeMap<String, i32>,
    labels: HashMap<String, String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let network: IpNet = "203.0.113.0/24".parse()?;
    let base = NetworkIntelligence {
        source: "geo-feed".into(),
        asn: 64500,
        is_proxy: false,
        mode: PolicyMode::Observe,
        base_only: Some("legacy-contract".into()),
        source_only: None,
        location: Some(Location {
            country: Some("FR".into()),
            city: Some("Paris".into()),
            coordinates: Some(Coordinates {
                latitude: Some(48.8566),
                longitude: Some(2.3522),
            }),
        }),
        tags: vec!["residential".into(), "shared".into()],
        risk_by_feed: BTreeMap::from([("geo".into(), 1), ("fraud".into(), 2)]),
        labels: HashMap::from([("region".into(), "eu".into())]),
    };
    let source = NetworkIntelligence {
        source: "threat-feed".into(),
        asn: 64501,
        is_proxy: true,
        mode: PolicyMode::Enforce,
        base_only: None,
        source_only: Some("new-contract".into()),
        location: Some(Location {
            country: None,
            city: Some("Lyon".into()),
            coordinates: Some(Coordinates {
                latitude: None,
                longitude: Some(4.8357),
            }),
        }),
        tags: vec!["vpn".into(), "shared".into()],
        risk_by_feed: BTreeMap::from([("fraud".into(), 9), ("bot".into(), 4)]),
        labels: HashMap::from([
            ("region".into(), "fr".into()),
            ("priority".into(), "high".into()),
        ]),
    };

    let mut table: IpTable<NetworkIntelligence> = IpTable::new();
    assert_eq!(
        table.insert(network, Some(base), SerdeMerge)?,
        MergeOutcome::Inserted
    );
    // Serialization and merge happen here, once.
    assert_eq!(
        table.insert(network, Some(source), SerdeMerge)?,
        MergeOutcome::Merged
    );

    // Lookups borrow the already materialized struct.
    let m = table.lookup_data("203.0.113.42".parse()?).unwrap();
    println!("{}", serde_json::to_string_pretty(m)?);

    // Preserved: omitted (None + skip_serializing_if) fields keep their value.
    assert_eq!(m.base_only.as_deref(), Some("legacy-contract"));
    let loc = m.location.as_ref().unwrap();
    assert_eq!(loc.country.as_deref(), Some("FR"));
    assert_eq!(loc.coordinates.as_ref().unwrap().latitude, Some(48.8566));
    // Added.
    assert_eq!(m.source_only.as_deref(), Some("new-contract"));
    assert_eq!(m.risk_by_feed["bot"], 4);
    assert_eq!(m.labels["priority"], "high");
    // Replaced scalars (strings, numbers, bools, unit enums).
    assert_eq!(m.source, "threat-feed");
    assert_eq!(m.asn, 64501);
    assert!(m.is_proxy);
    assert!(matches!(m.mode, PolicyMode::Enforce));
    // Recursively merged maps.
    assert_eq!(loc.city.as_deref(), Some("Lyon"));
    assert_eq!(loc.coordinates.as_ref().unwrap().longitude, Some(4.8357));
    assert_eq!(m.risk_by_feed["fraud"], 9);
    assert_eq!(m.risk_by_feed["geo"], 1);
    assert_eq!(m.labels["region"], "fr");
    // Concatenated arrays (order and duplicates kept).
    assert_eq!(m.tags, ["residential", "shared", "vpn", "shared"]);

    println!("All deep-merge outcomes matched the expected values.");
    Ok(())
}

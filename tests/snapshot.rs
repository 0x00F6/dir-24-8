//! Snapshot round trips, format validation, limits and failure handling
//! (feature `snapshot`).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use dir_24_8::{
    Config, EntryOrder, Error, FileSync, IpNet, IpTable, LoadOptions, Merge, MergeWith, OverBudget,
    Replace, Resource, SnapshotError, SnapshotLimit, SnapshotOptions,
};
use serde::{Deserialize, Serialize};

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn net(s: &str) -> IpNet {
    s.parse().unwrap()
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

/// Fast profile for tests that are not about compression settings.
fn fast() -> SnapshotOptions {
    SnapshotOptions::new().level(1).extreme(false)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dir-24-8-snapshot-test-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Deterministic generator (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random table with overlapping prefixes of both families and a mix of
/// payloads and routes without payload.
fn random_table(seed: u64, v4: usize, v6: usize) -> IpTable<String> {
    let mut rng = Rng(seed);
    let mut table = IpTable::new();
    for i in 0..v4 {
        let len = [8, 12, 16, 20, 22, 24, 24, 24, 25, 28, 32][rng.below(11) as usize];
        let addr = Ipv4Addr::from(0x0A00_0000 | (rng.next() as u32 & 0x00FF_FFFF));
        let net = IpNet::new(addr.into(), len).unwrap().trunc();
        let data = (i % 5 != 0).then(|| format!("v4-{i}"));
        table.insert(net, data, Replace).unwrap();
    }
    for i in 0..v6 {
        let len = [16, 24, 32, 40, 48, 48, 56, 64, 96, 128][rng.below(10) as usize];
        let addr = Ipv6Addr::from((0x2001_0db8u128 << 96) | (u128::from(rng.next()) << 32));
        let net = IpNet::new(addr.into(), len).unwrap().trunc();
        let data = (i % 7 != 0).then(|| format!("v6-{i}"));
        table.insert(net, data, Replace).unwrap();
    }
    table
}

/// Addresses inside, around and outside the routes of `table`.
fn probes<T>(table: &IpTable<T>, seed: u64) -> Vec<IpAddr> {
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    for (prefix, _) in table.iter() {
        match prefix {
            IpNet::V4(n) => {
                let base = u32::from(n.network());
                let span = u32::MAX >> n.prefix_len().min(31);
                out.push(IpAddr::V4(base.into()));
                out.push(IpAddr::V4((base | (rng.next() as u32 & span)).into()));
                out.push(IpAddr::V4(base.wrapping_sub(1).into()));
            }
            IpNet::V6(n) => {
                let base = u128::from(n.network());
                let span = u128::MAX >> n.prefix_len().min(127);
                out.push(IpAddr::V6(base.into()));
                let r = (u128::from(rng.next()) << 64) | u128::from(rng.next());
                out.push(IpAddr::V6((base | (r & span)).into()));
                out.push(IpAddr::V6(base.wrapping_sub(1).into()));
            }
        }
    }
    for _ in 0..2000 {
        out.push(IpAddr::V4((rng.next() as u32).into()));
        out.push(IpAddr::V6(
            ((u128::from(rng.next()) << 64) | u128::from(rng.next())).into(),
        ));
    }
    out
}

/// Same routes, payloads, configuration and results for every lookup API.
fn assert_equivalent<T: PartialEq + std::fmt::Debug>(a: &IpTable<T>, b: &IpTable<T>, seed: u64) {
    assert_eq!(a.len_ipv4(), b.len_ipv4());
    assert_eq!(a.len_ipv6(), b.len_ipv6());
    assert_eq!(a.config(), b.config());
    for (prefix, data) in a.iter() {
        assert_eq!(b.get(prefix), Some(data), "{prefix}");
    }
    let keys = probes(a, seed);
    for &k in &keys {
        assert_eq!(a.lookup(k), b.lookup(k), "lookup {k}");
        assert_eq!(a.exist(k), b.exist(k), "exist {k}");
    }
    let (mut x, mut y) = (vec![None; keys.len()], vec![None; keys.len()]);
    a.lookup_batch_into(&keys, &mut x).unwrap();
    b.lookup_batch_into(&keys, &mut y).unwrap();
    assert_eq!(x, y);
    let (mut x, mut y) = (vec![false; keys.len()], vec![false; keys.len()]);
    a.exist_batch_into(&keys, &mut x).unwrap();
    b.exist_batch_into(&keys, &mut y).unwrap();
    assert_eq!(x, y);
}

fn round_trip<T: Serialize + serde::de::DeserializeOwned>(table: &IpTable<T>) -> IpTable<T> {
    IpTable::load(table.snapshot_with_options(&fast()).unwrap()).unwrap()
}

// ----------------------------------------------------------------------
// Crafted snapshots (format tests)
// ----------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn header(version: u16, codec: u8, payload: u8) -> Vec<u8> {
    let mut h = b"\x89DIR248\n".to_vec();
    h.extend(version.to_le_bytes());
    h.extend([codec, payload]);
    let crc = crc32(&h);
    h.extend(crc.to_le_bytes());
    h
}

fn xz(body: &[u8], check: liblzma::stream::Check) -> Vec<u8> {
    let stream = liblzma::stream::Stream::new_easy_encoder(1, check).unwrap();
    let mut enc = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
    enc.write_all(body).unwrap();
    enc.finish().unwrap()
}

fn craft(body: &[u8]) -> Vec<u8> {
    let mut out = header(1, 1, 1);
    out.extend(xz(body, liblzma::stream::Check::Crc64));
    out
}

fn cbor<T: Serialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).unwrap();
    out
}

/// Body builder following the version 1 layout.
struct Body(Vec<u8>);

impl Body {
    fn new() -> Self {
        Self::with_config(0, 0xFF, 0, 0x7FFF_FFFF)
    }

    /// `batch` and `single` are the retired backend bytes.
    fn with_config(batch: u8, single: u8, flags: u8, max_routes: u32) -> Self {
        let mut b = vec![batch, single, flags];
        b.extend(max_routes.to_le_bytes());
        b.extend(((1u32 << 23) - 1).to_le_bytes());
        b.extend(((1u32 << 23) - 1).to_le_bytes());
        Self(b)
    }

    /// One section: `(length, address bytes, payload frame)`.
    fn family(mut self, entries: &[(u8, &[u8], &[u8])]) -> Self {
        self.0.extend((entries.len() as u32).to_le_bytes());
        self.0.extend(entries.iter().map(|e| e.0));
        for e in entries {
            self.0.extend(e.1);
        }
        for e in entries {
            self.0.extend(e.2);
        }
        self
    }

    fn empty(self) -> Self {
        self.family(&[])
    }

    fn end(mut self) -> Vec<u8> {
        self.0.extend(b"END\n");
        self.0
    }
}

/// Payload frame of a CBOR-encoded `u32`.
fn frame(v: u32) -> Vec<u8> {
    let c = cbor(&v);
    let mut f = vec![c.len() as u8 + 1];
    f.extend(c);
    f
}

fn malformed(bytes: &[u8]) -> String {
    match IpTable::<u32>::load(bytes) {
        Err(SnapshotError::Malformed(what)) => what,
        other => panic!("expected Malformed, got {other:?}"),
    }
}

// ----------------------------------------------------------------------
// Round trips
// ----------------------------------------------------------------------

#[test]
fn empty_table() {
    let table: IpTable<u32> = IpTable::new();
    let bytes = table.snapshot().unwrap();
    let restored: IpTable<u32> = IpTable::load(&bytes).unwrap();
    assert!(restored.is_empty());
    assert!(!restored.stats().ipv4.allocated && !restored.stats().ipv6.allocated);
    assert!(restored.lookup(ip("10.0.0.1")).is_none());

    let dir = temp_dir("empty");
    let path = dir.join("empty.snap");
    table.snapshot_to_file(&path).unwrap();
    assert!(IpTable::<u32>::load_from_file(&path).unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn edge_prefixes_nested_masked_and_without_payload() {
    let mut table: IpTable<u32> = IpTable::new();
    for (cidr, data) in [
        ("0.0.0.0/0", Some(0)),
        ("::/0", None),
        ("10.0.0.0/8", Some(8)),
        ("10.1.0.0/16", None),
        ("10.1.2.0/24", Some(24)), // masked by the two /25s
        ("10.1.2.0/25", Some(250)),
        ("10.1.2.128/25", None),
        ("192.0.2.7/32", Some(32)),
        ("255.255.255.255/32", Some(33)),
        ("2001:db8::/32", Some(6)),
        ("2001:db8::/48", None),
        ("2001:db8::1/128", Some(128)),
        ("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128", Some(129)),
    ] {
        table.insert(net(cidr), data, Replace).unwrap();
    }
    let restored = round_trip(&table);
    assert_equivalent(&table, &restored, 1);
    // Masked route kept, present-without-payload distinct from absent.
    assert_eq!(restored.get(net("10.1.2.0/24")), Some(Some(&24)));
    assert_eq!(
        restored.lookup(ip("10.1.2.1")).unwrap().prefix,
        net("10.1.2.0/25")
    );
    assert_eq!(restored.get(net("10.1.0.0/16")), Some(None));
    assert_eq!(restored.get(net("10.2.0.0/16")), None);
    assert_eq!(restored.get(net("::/0")), Some(None));
    assert_eq!(restored.lookup(ip("3000::1")).unwrap().data, None);
    assert_eq!(restored.lookup_data(ip("2001:db8::1")), Some(&128));
}

#[test]
fn option_payloads_keep_some_none_apart_from_none() {
    let mut table: IpTable<Option<u8>> = IpTable::new();
    table
        .insert(net("10.0.0.0/8"), Some(None), Replace)
        .unwrap();
    table.insert(net("11.0.0.0/8"), None, Replace).unwrap();
    table
        .insert(net("12.0.0.0/8"), Some(Some(3)), Replace)
        .unwrap();
    let restored = round_trip(&table);
    assert_eq!(restored.get(net("10.0.0.0/8")), Some(Some(&None)));
    assert_eq!(restored.get(net("11.0.0.0/8")), Some(None));
    assert_eq!(restored.get(net("12.0.0.0/8")), Some(Some(&Some(3))));
}

#[test]
fn random_tables_are_equivalent_after_restore() {
    for seed in [1, 2, 3] {
        let table = random_table(seed, 3000, 2000);
        assert_equivalent(&table, &round_trip(&table), seed);
    }
}

#[test]
fn restored_table_is_fully_mutable() {
    let mut table: IpTable<Vec<String>> = IpTable::new();
    table
        .insert(net("10.0.0.0/8"), Some(vec!["a".into()]), Replace)
        .unwrap();
    table
        .insert(net("10.0.0.0/24"), Some(vec!["masked".into()]), Replace)
        .unwrap();
    table.insert(net("10.0.0.0/25"), None, Replace).unwrap();
    table.insert(net("10.0.0.128/25"), None, Replace).unwrap();
    table
        .insert(net("2001:db8::/32"), Some(vec![]), Replace)
        .unwrap();
    let mut restored = round_trip(&table);

    // Removing a more specific route uncovers the masked one.
    assert_eq!(restored.remove(net("10.0.0.0/25")), Some(None));
    assert_eq!(
        restored.lookup(ip("10.0.0.1")).unwrap().prefix,
        net("10.0.0.0/24")
    );
    // Merge modes still apply.
    restored
        .insert(net("10.0.0.0/8"), Some(vec!["b".into()]), Merge)
        .unwrap();
    assert_eq!(restored.lookup_data(ip("10.9.9.9")).unwrap(), &["a", "b"]);
    restored
        .insert(
            net("2001:db8::/32"),
            Some(vec!["x".into()]),
            MergeWith(|cur: &mut Vec<String>, inc: Vec<String>| cur.extend(inc)),
        )
        .unwrap();
    assert_eq!(restored.lookup_data(ip("2001:db8::1")).unwrap(), &["x"]);
    // New routes of both families, then removal of everything.
    restored
        .insert(net("192.0.2.0/24"), Some(vec![]), Replace)
        .unwrap();
    restored
        .insert(net("2001:db8:1::/48"), None, Replace)
        .unwrap();
    assert_eq!(restored.len(), 6);
    let all: Vec<IpNet> = restored.iter().map(|(n, _)| n).collect();
    for n in all {
        assert!(restored.remove(n).is_some());
    }
    assert!(restored.is_empty());
    assert!(restored.lookup(ip("10.0.0.1")).is_none());
    // And it can be saved again.
    assert!(round_trip(&restored).is_empty());
}

#[test]
fn snapshots_of_restored_tables_are_identical() {
    let table = random_table(9, 500, 300);
    let first = table.snapshot_with_options(&fast()).unwrap();
    let restored: IpTable<String> = IpTable::load(&first).unwrap();
    // Storage order is the order of the snapshot: same bytes again.
    assert_eq!(restored.snapshot_with_options(&fast()).unwrap(), first);
}

#[test]
fn memory_and_file_apis_share_the_format() {
    let table = random_table(4, 400, 200);
    let dir = temp_dir("cross");

    // Memory snapshot written to a file, loaded by the file API.
    let bytes = table.snapshot().unwrap();
    let a = dir.join("from-memory.snap");
    fs::write(&a, &bytes).unwrap();
    assert_equivalent(&table, &IpTable::load_from_file(&a).unwrap(), 4);

    // File snapshot read into memory, loaded by the memory API.
    let b = dir.join("from-file.snap");
    table.snapshot_to_file(&b).unwrap();
    let read = fs::read(&b).unwrap();
    assert_eq!(read, bytes, "same options, same bytes");
    assert_equivalent(&table, &IpTable::load(&read).unwrap(), 4);

    // Only the two snapshots remain (no temporary file).
    assert_eq!(dir_entries(&dir), ["from-file.snap", "from-memory.snap"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn load_accepts_owned_borrowed_and_sliced_bytes() {
    let table = random_table(5, 50, 50);
    let bytes: Vec<u8> = table.snapshot_with_options(&fast()).unwrap();
    let by_ref: IpTable<String> = IpTable::load(&bytes).unwrap();
    let by_slice: IpTable<String> = IpTable::load(bytes.as_slice()).unwrap();
    let boxed: Box<[u8]> = bytes.clone().into_boxed_slice();
    let by_box: IpTable<String> = IpTable::load(&boxed).unwrap();
    let by_value: IpTable<String> = IpTable::load(bytes).unwrap();
    for t in [&by_ref, &by_slice, &by_box, &by_value] {
        assert_eq!(t.len(), table.len());
    }
}

#[test]
fn configuration_is_restored_and_the_kernel_detected_again() {
    let config = Config::new()
        .huge_pages(true)
        .max_routes(1000)
        .max_ipv4_groups(100)
        .max_ipv6_groups(200);
    let mut table = IpTable::with_config(config.clone()).unwrap();
    table
        .insert(net("10.0.0.0/8"), Some(1u32), Replace)
        .unwrap();
    let restored = round_trip(&table);
    assert_eq!(restored.config(), &config);
    assert_eq!(
        restored.stats().batch_kernel,
        IpTable::<u32>::new().stats().batch_kernel
    );

    // The restored capacity limits apply.
    let mut limited = IpTable::with_config(Config::new().max_routes(2)).unwrap();
    limited
        .insert(net("10.0.0.0/8"), Some(1u32), Replace)
        .unwrap();
    limited
        .insert(net("11.0.0.0/8"), Some(1u32), Replace)
        .unwrap();
    let mut restored = round_trip(&limited);
    let err = restored
        .insert(net("12.0.0.0/8"), Some(3), Replace)
        .unwrap_err();
    assert!(matches!(
        err,
        Error::CapacityExceeded {
            resource: Resource::Routes,
            ..
        }
    ));

    // A configuration override replaces the stored one.
    let bytes = limited.snapshot_with_options(&fast()).unwrap();
    let options = LoadOptions::new().config(Config::new());
    let mut wider: IpTable<u32> = IpTable::load_with_options(&bytes, &options).unwrap();
    wider.insert(net("12.0.0.0/8"), Some(3), Replace).unwrap();
    // ... and its limits are checked against the snapshot's counts.
    let options = LoadOptions::new().config(Config::new().max_routes(1));
    let err = IpTable::<u32>::load_with_options(&bytes, &options).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Table(Error::CapacityExceeded {
            resource: Resource::Routes,
            limit: 1,
            ..
        })
    ));
}

#[test]
fn retired_backend_bytes_are_written_as_defaults_and_ignored_on_load() {
    // The writer stores the former defaults (automatic SIMD, no
    // single-lookup override), which every version 1 reader accepts.
    let mut table = IpTable::new();
    table
        .insert(net("10.0.0.0/8"), Some(1u32), Replace)
        .unwrap();
    let bytes = table.snapshot_with_options(&fast()).unwrap();
    let mut body = Vec::new();
    liblzma::read::XzDecoder::new(&bytes[16..])
        .read_to_end(&mut body)
        .unwrap();
    assert_eq!(body[..2], [0, 0xFF]);
    // Codes stored by writers that could force a backend still load, and
    // only the rest of the configuration is restored.
    for (batch, single) in [(0, 0xFF), (1, 0xFF), (2, 1), (3, 4), (4, 0)] {
        let bytes = craft(
            &Body::with_config(batch, single, 0, 10)
                .empty()
                .empty()
                .end(),
        );
        let restored = IpTable::<u32>::load(&bytes).unwrap();
        assert_eq!(
            restored.config(),
            &Config::new().max_routes(10),
            "{batch} {single}"
        );
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Loose {
    Number(u64),
    Text(String),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Meta {
    source: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Rich {
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    tags: Vec<String>,
    loose: Loose,
    #[serde(flatten)]
    meta: Meta,
    extra: BTreeMap<String, serde_json::Value>,
}

#[test]
fn serde_attributes_and_self_describing_payloads_round_trip() {
    let mut table: IpTable<Rich> = IpTable::new();
    table
        .insert(
            net("10.0.0.0/8"),
            Some(Rich {
                country: None,
                tags: vec![],
                loose: Loose::Number(7),
                meta: Meta {
                    source: "rir".into(),
                },
                extra: BTreeMap::from([("k".into(), serde_json::json!({"a": [1, null, "x"]}))]),
            }),
            Replace,
        )
        .unwrap();
    table
        .insert(
            net("2001:db8::/32"),
            Some(Rich {
                country: Some("FR".into()),
                tags: vec!["t".into()],
                loose: Loose::Text("x".into()),
                meta: Meta {
                    source: "bgp".into(),
                },
                extra: BTreeMap::new(),
            }),
            Replace,
        )
        .unwrap();
    let restored = round_trip(&table);
    for (prefix, data) in table.iter() {
        assert_eq!(restored.get(prefix), Some(data));
    }

    let mut json: IpTable<serde_json::Value> = IpTable::new();
    json.insert(
        net("10.0.0.0/8"),
        Some(serde_json::json!({"x": [1.5, true]})),
        Replace,
    )
    .unwrap();
    assert_eq!(
        round_trip(&json).lookup_data(ip("10.1.1.1")),
        json.lookup_data(ip("10.1.1.1"))
    );
}

#[test]
fn multithreaded_snapshots_round_trip() {
    let table = random_table(6, 6000, 3000);
    let options = SnapshotOptions::new()
        .level(3)
        .extreme(false)
        .threads(3)
        .block_size(16 << 10);
    let mut bytes = Vec::new();
    let summary = table.snapshot_to_writer(&mut bytes, &options).unwrap();
    assert_eq!(summary.threads, 3);
    assert!(
        summary.uncompressed_bytes > 3 * (16 << 10),
        "several blocks"
    );
    for threads in [1, 2, 4] {
        let restored: IpTable<String> =
            IpTable::load_with_options(&bytes, &LoadOptions::new().threads(threads)).unwrap();
        assert_equivalent(&table, &restored, 6);
    }
}

#[test]
fn sorted_order_is_independent_of_history() {
    let routes = [
        ("10.0.0.0/8", Some(1u32)),
        ("10.0.0.0/16", None),
        ("2001:db8::/32", Some(3)),
        ("9.0.0.0/8", Some(4)),
        ("::/0", Some(5)),
        ("10.0.0.0/9", Some(6)),
    ];
    let mut a = IpTable::new();
    for &(n, d) in &routes {
        a.insert(net(n), d, Replace).unwrap();
    }
    let mut b = IpTable::new();
    for &(n, d) in routes.iter().rev() {
        b.insert(net("192.0.2.0/24"), Some(0), Replace).unwrap(); // churn: slots reused
        b.remove(net("192.0.2.0/24"));
        b.insert(net(n), d, Replace).unwrap();
    }
    let sorted = fast().order(EntryOrder::Sorted);
    let (sa, sb) = (
        a.snapshot_with_options(&sorted).unwrap(),
        b.snapshot_with_options(&sorted).unwrap(),
    );
    assert_eq!(sa, sb);
    assert_ne!(
        a.snapshot_with_options(&fast()).unwrap(),
        b.snapshot_with_options(&fast()).unwrap(),
        "storage order follows the history"
    );
    let restored: IpTable<u32> = IpTable::load(&sa).unwrap();
    assert_equivalent(&a, &restored, 7);
    let order: Vec<IpNet> = restored.iter().map(|(n, _)| n).collect();
    assert_eq!(
        order,
        [
            "9.0.0.0/8",
            "10.0.0.0/8",
            "10.0.0.0/9",
            "10.0.0.0/16",
            "::/0",
            "2001:db8::/32"
        ]
        .map(net)
    );
}

/// The default profile (XZ 9 + extreme) on a representative table.
#[test]
fn maximal_profile_on_a_representative_table() {
    let table = random_table(8, 30_000, 10_000);
    let mut bytes = Vec::new();
    let summary = table
        .snapshot_to_writer(&mut bytes, &SnapshotOptions::new())
        .unwrap();
    assert_eq!(summary.compressed_bytes, bytes.len() as u64);
    assert_eq!(summary.ipv4_routes + summary.ipv6_routes, table.len());
    assert!(summary.dict_size as u64 >= summary.uncompressed_bytes.min(64 << 20));
    assert!(summary.compressed_bytes * 2 < summary.uncompressed_bytes);
    let restored: IpTable<String> = IpTable::load(&bytes).unwrap();
    assert_equivalent(&table, &restored, 8);
    // Faster presets are larger on this table.
    let fast_bytes = table.snapshot_with_options(&fast()).unwrap();
    assert!(bytes.len() < fast_bytes.len());
}

// ----------------------------------------------------------------------
// Readers and writers that process small fragments
// ----------------------------------------------------------------------

/// Returns at most `max` bytes per call (and sometimes `Interrupted`).
struct Trickle<R> {
    inner: R,
    max: usize,
    calls: usize,
}

impl<R: Read> Read for Trickle<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls.is_multiple_of(5) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(1 + self.calls % self.max);
        self.inner.read(&mut buf[..n])
    }
}

impl<W: Write> Write for Trickle<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls.is_multiple_of(5) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(1 + self.calls % self.max);
        self.inner.write(&buf[..n])
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[test]
fn small_fragment_readers_and_writers() {
    let table = random_table(10, 800, 400);
    let reference = table.snapshot_with_options(&fast()).unwrap();

    let mut trickle = Trickle {
        inner: Vec::new(),
        max: 7,
        calls: 0,
    };
    table.snapshot_to_writer(&mut trickle, &fast()).unwrap();
    assert!(trickle.calls > 100);
    assert_eq!(trickle.inner, reference);

    for capacity in [1, 3, 64] {
        let reader = Trickle {
            inner: &reference[..],
            max: 5,
            calls: 0,
        };
        let restored: IpTable<String> = IpTable::load_from_reader(
            BufReader::with_capacity(capacity, reader),
            &LoadOptions::new(),
        )
        .unwrap();
        assert_equivalent(&table, &restored, 10);
    }
}

// ----------------------------------------------------------------------
// Damaged, truncated and incompatible inputs
// ----------------------------------------------------------------------

#[test]
fn every_truncation_is_detected() {
    let table = random_table(11, 20, 20);
    let bytes = table.snapshot_with_options(&fast()).unwrap();
    for cut in 0..bytes.len() {
        match IpTable::<String>::load(&bytes[..cut]) {
            Err(SnapshotError::Truncated) => {}
            other => panic!("cut at {cut}/{}: {other:?}", bytes.len()),
        }
    }
}

#[test]
fn every_damaged_byte_is_detected() {
    let mut table: IpTable<u32> = IpTable::new();
    table.insert(net("10.0.0.0/8"), Some(1), Replace).unwrap();
    table.insert(net("10.1.0.0/16"), None, Replace).unwrap();
    table
        .insert(net("2001:db8::/32"), Some(2), Replace)
        .unwrap();
    let bytes = table.snapshot().unwrap();
    for i in 0..bytes.len() {
        for flip in [0x01, 0x80] {
            let mut damaged = bytes.clone();
            damaged[i] ^= flip;
            match IpTable::<u32>::load(&damaged) {
                Err(
                    SnapshotError::Corrupt(_)
                    | SnapshotError::NotASnapshot
                    | SnapshotError::Truncated,
                ) => {}
                other => panic!("byte {i} ^ {flip:#x}: {other:?}"),
            }
        }
    }
}

#[test]
fn header_errors_are_specific() {
    let bytes = IpTable::<u32>::new().snapshot().unwrap();
    assert!(matches!(
        IpTable::<u32>::load(b"PK\x03\x04 zip"),
        Err(SnapshotError::NotASnapshot)
    ));
    assert!(matches!(
        IpTable::<u32>::load(&bytes[..5]),
        Err(SnapshotError::Truncated)
    ));

    let body = &bytes[16..];
    let with_header = |h: Vec<u8>| [h, body.to_vec()].concat();
    match IpTable::<u32>::load(with_header(header(2, 1, 1))) {
        Err(SnapshotError::UnsupportedVersion {
            found: 2,
            supported: 1,
        }) => {}
        other => panic!("{other:?}"),
    }
    match IpTable::<u32>::load(with_header(header(1, 2, 1))) {
        Err(SnapshotError::UnsupportedCodec {
            compression: 2,
            payload: 1,
        }) => {}
        other => panic!("{other:?}"),
    }
    match IpTable::<u32>::load(with_header(header(1, 1, 9))) {
        Err(SnapshotError::UnsupportedCodec {
            compression: 1,
            payload: 9,
        }) => {}
        other => panic!("{other:?}"),
    }
    let mut bad_crc = header(1, 1, 1);
    bad_crc[12] ^= 1;
    assert!(matches!(
        IpTable::<u32>::load(with_header(bad_crc)),
        Err(SnapshotError::Corrupt(_))
    ));
}

#[test]
fn trailing_data_is_rejected() {
    let bytes = random_table(12, 10, 10)
        .snapshot_with_options(&fast())
        .unwrap();
    for extra in [&[0u8][..], b"\0\0\0\0", &bytes[..]] {
        let what = match IpTable::<String>::load([&bytes[..], extra].concat()) {
            Err(SnapshotError::Malformed(what)) => what,
            other => panic!("{other:?}"),
        };
        assert!(what.contains("trailing"), "{what}");
    }
}

#[test]
fn multithreaded_decoder_detects_truncation_and_trailing_data() {
    let table = random_table(14, 3000, 1000);
    let options = fast().threads(2).block_size(16 << 10);
    let bytes = table.snapshot_with_options(&options).unwrap();
    let mt = LoadOptions::new().threads(2);
    for cut in [
        17,
        40,
        bytes.len() / 3,
        bytes.len() / 2,
        bytes.len() - 13,
        bytes.len() - 1,
    ] {
        match IpTable::<String>::load_with_options(&bytes[..cut], &mt) {
            Err(SnapshotError::Truncated) => {}
            other => panic!("cut at {cut}/{}: {:?}", bytes.len(), other.map(|t| t.len())),
        }
    }
    let longer = [&bytes[..], b"tail"].concat();
    assert!(matches!(
        IpTable::<String>::load_with_options(&longer, &mt),
        Err(SnapshotError::Malformed(what)) if what.contains("trailing")
    ));
    let mut damaged = bytes.clone();
    let middle = damaged.len() / 2;
    damaged[middle] ^= 0x10;
    assert!(matches!(
        IpTable::<String>::load_with_options(&damaged, &mt),
        Err(SnapshotError::Corrupt(_))
    ));
}

#[test]
fn crafted_bodies_are_validated() {
    let v4 = |len: u8, addr: &'static [u8]| (len, addr, &[0u8][..]);
    // The builder itself produces valid snapshots.
    let ok = craft(
        &Body::new()
            .family(&[v4(8, &[10]), v4(32, &[1, 2, 3, 4])])
            .empty()
            .end(),
    );
    assert_eq!(IpTable::<u32>::load(&ok).unwrap().len(), 2);

    let dup = craft(
        &Body::new()
            .family(&[v4(8, &[10]), v4(8, &[10])])
            .empty()
            .end(),
    );
    assert!(malformed(&dup).contains("duplicate prefix 10.0.0.0/8"));
    let long = craft(
        &Body::new()
            .family(&[v4(33, &[1, 2, 3, 4, 5])])
            .empty()
            .end(),
    );
    assert!(malformed(&long).contains("length 33"));
    let v6_long = Body::new().empty().family(&[(129, &[0; 17], &[0])]).end();
    assert!(malformed(&craft(&v6_long)).contains("length 129"));
    let host_bits = craft(&Body::new().family(&[v4(7, &[11])]).empty().end());
    assert!(malformed(&host_bits).contains("host bits"));
    let mut no_end = Body::new().empty().empty().0;
    no_end.extend(b"FIN\n");
    assert!(malformed(&craft(&no_end)).contains("end marker"));
    let mut after_end = Body::new().empty().empty().end();
    after_end.push(0);
    assert!(malformed(&craft(&after_end)).contains("after the end marker"));
    let short = Body::new().empty().0;
    assert!(malformed(&craft(&short)).contains("ends early"));
    let backend = craft(&Body::with_config(9, 0xFF, 0, 10).empty().empty().end());
    assert!(malformed(&backend).contains("backend code 9"));
    let single = craft(&Body::with_config(0, 5, 0, 10).empty().empty().end());
    assert!(malformed(&single).contains("backend code 5"));
    let flags = craft(&Body::with_config(0, 0xFF, 0x80, 10).empty().empty().end());
    assert!(malformed(&flags).contains("flags"));
    let count = {
        let mut b = Body::new().0;
        b.extend(0x8000_0000u32.to_le_bytes());
        craft(&b)
    };
    assert!(malformed(&count).contains("above the format maximum"));
    let over = craft(
        &Body::with_config(0, 0xFF, 0, 1)
            .family(&[v4(8, &[10]), v4(8, &[11])])
            .empty()
            .end(),
    );
    assert!(matches!(
        IpTable::<u32>::load(&over),
        Err(SnapshotError::Table(Error::CapacityExceeded {
            limit: 1,
            ..
        }))
    ));

    // Payload frames.
    let seven = frame(7);
    let good = craft(&Body::new().family(&[(8, &[10], &seven)]).empty().end());
    assert_eq!(
        IpTable::<u32>::load(&good)
            .unwrap()
            .lookup_data(ip("10.0.0.1")),
        Some(&7)
    );
    let mut padded = cbor(&7u32);
    padded.push(0);
    let padded_frame = [vec![padded.len() as u8 + 1], padded].concat();
    let bad = craft(
        &Body::new()
            .family(&[(8, &[10], &padded_frame)])
            .empty()
            .end(),
    );
    assert!(malformed(&bad).contains("unused bytes"));
    let big = cbor(&100_000u32); // 5 bytes declared as 2
    let cut_frame = [vec![3], big].concat();
    let bad = craft(&Body::new().family(&[(8, &[10], &cut_frame)]).empty().end());
    assert!(
        malformed(&bad).contains("longer than"),
        "{}",
        malformed(&bad)
    );
    let overlong_varint = craft(
        &Body::new()
            .family(&[(8, &[10], &[0x80, 0x00])])
            .empty()
            .end(),
    );
    assert!(malformed(&overlong_varint).contains("variable-length"));
    let wrong_type = craft(
        &Body::new()
            .family(&[(8, &[10], &[[2u8].as_slice(), &cbor(&"x")].concat())])
            .empty()
            .end(),
    );
    assert!(matches!(
        IpTable::<u32>::load(&wrong_type),
        Err(SnapshotError::Deserialize(_))
    ));
}

#[test]
fn streams_without_integrity_check_are_rejected() {
    let body = Body::new().empty().empty().end();
    let mut bytes = header(1, 1, 1);
    bytes.extend(xz(&body, liblzma::stream::Check::None));
    assert!(malformed(&bytes).contains("integrity check"));
    // Other checks are accepted.
    for check in [
        liblzma::stream::Check::Crc32,
        liblzma::stream::Check::Sha256,
    ] {
        let mut bytes = header(1, 1, 1);
        bytes.extend(xz(&body, check));
        assert!(IpTable::<u32>::load(&bytes).unwrap().is_empty());
    }
}

#[test]
fn payload_type_mismatch_is_a_deserialize_error() {
    let mut table: IpTable<String> = IpTable::new();
    table
        .insert(net("10.0.0.0/8"), Some("text".into()), Replace)
        .unwrap();
    let bytes = table.snapshot_with_options(&fast()).unwrap();
    assert!(matches!(
        IpTable::<u32>::load(&bytes),
        Err(SnapshotError::Deserialize(_))
    ));
}

// ----------------------------------------------------------------------
// Limits and options
// ----------------------------------------------------------------------

#[test]
fn load_limits_are_enforced() {
    let mut table: IpTable<String> = IpTable::new();
    table
        .insert(net("10.0.0.0/8"), Some("x".repeat(100)), Replace)
        .unwrap();
    table
        .insert(net("2001:db8::/32"), Some("y".into()), Replace)
        .unwrap();
    let bytes = table.snapshot().unwrap();
    let load = |o: LoadOptions| IpTable::<String>::load_with_options(&bytes, &o);

    assert!(load(LoadOptions::new().max_entries(2).max_value_bytes(102)).is_ok());
    match load(LoadOptions::new().max_entries(1)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::Entries,
            max: 1,
            requested: Some(2),
        }) => {}
        other => panic!("{other:?}"),
    }
    match load(LoadOptions::new().max_value_bytes(50)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::ValueBytes,
            max: 50,
            requested: Some(102),
        }) => {}
        other => panic!("{other:?}"),
    }
    match load(LoadOptions::new().max_decompressed_bytes(64)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::DecompressedBytes,
            max: 64,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    match load(LoadOptions::new().decoder_memory_limit(1024)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::DecoderMemory,
            max: 1024,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    match load(LoadOptions::new().threads(2).decoder_memory_limit(1024)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::DecoderMemory,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        load(LoadOptions::new().threads(0)),
        Err(SnapshotError::InvalidOptions(_))
    ));
}

#[test]
fn encoder_memory_budget_fails_or_shrinks_the_dictionary() {
    let table = random_table(13, 2000, 1000);
    let mut out = Vec::new();
    let base = table
        .snapshot_to_writer(&mut out, &SnapshotOptions::new())
        .unwrap();
    // The dictionary was fitted to the snapshot.
    assert!(u64::from(base.dict_size) <= base.uncompressed_bytes.max(4096));

    let budget = base.encoder_memory - 1;
    match table.snapshot_with_options(&SnapshotOptions::new().memory_budget(budget)) {
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::EncoderMemory,
            max,
            requested,
        }) => {
            assert_eq!((max, requested), (budget, Some(base.encoder_memory)));
        }
        other => panic!("{other:?}"),
    }
    let shrink = SnapshotOptions::new()
        .memory_budget(budget)
        .over_budget(OverBudget::ShrinkDictionary);
    let mut out = Vec::new();
    let small = table.snapshot_to_writer(&mut out, &shrink).unwrap();
    assert!(small.dict_size < base.dict_size);
    assert!(small.encoder_memory <= budget);
    assert_equivalent(&table, &IpTable::load(&out).unwrap(), 13);
    // Nothing fits in 1 KiB, even with the policy.
    let tiny = shrink.clone().memory_budget(1024);
    assert!(matches!(
        table.snapshot_with_options(&tiny),
        Err(SnapshotError::LimitExceeded {
            limit: SnapshotLimit::EncoderMemory,
            ..
        })
    ));
}

#[test]
fn invalid_options_are_rejected() {
    let table: IpTable<u32> = IpTable::new();
    for options in [
        SnapshotOptions::new().level(10),
        SnapshotOptions::new().threads(0),
        SnapshotOptions::new().dict_size(100),
        SnapshotOptions::new().block_size(0),
    ] {
        assert!(matches!(
            table.snapshot_with_options(&options),
            Err(SnapshotError::InvalidOptions(_))
        ));
    }
    let custom = SnapshotOptions::new().dict_size(1 << 20).level(0);
    assert!(IpTable::<u32>::load(table.snapshot_with_options(&custom).unwrap()).is_ok());
}

// ----------------------------------------------------------------------
// Write failures
// ----------------------------------------------------------------------

/// Serializes normally until `FAIL_AFTER` serializations, then fails.
#[derive(Debug, Deserialize)]
struct Fragile(u32);

static SERIALIZED: AtomicUsize = AtomicUsize::new(0);
static FAIL_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);

impl Serialize for Fragile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if SERIALIZED.fetch_add(1, Ordering::SeqCst) >= FAIL_AFTER.load(Ordering::SeqCst) {
            return Err(serde::ser::Error::custom("refusing to serialize"));
        }
        s.serialize_u32(self.0)
    }
}

/// Fails every write after `budget` bytes.
struct FailingWriter {
    written: Vec<u8>,
    budget: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written.len() >= self.budget {
            return Err(io::Error::other("disk full"));
        }
        let n = buf.len().min(self.budget - self.written.len());
        self.written.extend(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_failures_are_reported_and_keep_the_previous_file() {
    // Only this test uses `Fragile`.
    let mut table: IpTable<Fragile> = IpTable::new();
    for i in 0..300u32 {
        let addr = Ipv4Addr::from(0x0A00_0000 + (i << 8));
        table
            .insert(
                IpNet::new(addr.into(), 24).unwrap(),
                Some(Fragile(i)),
                Replace,
            )
            .unwrap();
    }

    // A failing destination.
    let mut sink = FailingWriter {
        written: Vec::new(),
        budget: 40,
    };
    let err = table.snapshot_to_writer(&mut sink, &fast()).unwrap_err();
    assert!(matches!(err, SnapshotError::Io(_)), "{err:?}");

    // A serializer failing in the first pass (before any output).
    SERIALIZED.store(0, Ordering::SeqCst);
    FAIL_AFTER.store(10, Ordering::SeqCst);
    let mut out = Vec::new();
    let err = table.snapshot_to_writer(&mut out, &fast()).unwrap_err();
    assert!(matches!(err, SnapshotError::Serialize(_)), "{err:?}");
    assert!(out.is_empty());

    // A serializer failing while streaming: the partial output is never
    // completed into a loadable snapshot.
    SERIALIZED.store(0, Ordering::SeqCst);
    FAIL_AFTER.store(300 + 150, Ordering::SeqCst);
    let mut out = Vec::new();
    let err = table.snapshot_to_writer(&mut out, &fast()).unwrap_err();
    assert!(matches!(err, SnapshotError::Serialize(_)), "{err:?}");
    assert!(IpTable::<u32>::load(&out).is_err());

    // File snapshots: the previous snapshot survives, no temporary file.
    let dir = temp_dir("failures");
    let path = dir.join("routes.snap");
    FAIL_AFTER.store(usize::MAX, Ordering::SeqCst);
    table.snapshot_to_file(&path).unwrap();
    let previous = fs::read(&path).unwrap();
    SERIALIZED.store(0, Ordering::SeqCst);
    FAIL_AFTER.store(300 + 150, Ordering::SeqCst);
    let err = table.snapshot_to_file(&path).unwrap_err();
    assert!(matches!(err, SnapshotError::Serialize(_)), "{err:?}");
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_eq!(dir_entries(&dir), ["routes.snap"]);
    let restored: IpTable<u32> = IpTable::load_from_file(&path).unwrap();
    assert_eq!(restored.len(), 300);

    // A missing directory: nothing is created.
    FAIL_AFTER.store(usize::MAX, Ordering::SeqCst);
    let err = table
        .snapshot_to_file(dir.join("missing/routes.snap"))
        .unwrap_err();
    assert!(matches!(err, SnapshotError::Io(_)));
    // A directory in place of the file: the rename fails, cleanup happens.
    fs::create_dir(dir.join("taken")).unwrap();
    assert!(matches!(
        table.snapshot_to_file(dir.join("taken")),
        Err(SnapshotError::Io(_))
    ));
    assert_eq!(dir_entries(&dir), ["routes.snap", "taken"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn file_snapshots_replace_atomically_and_keep_permissions() {
    let dir = temp_dir("replace");
    let path = dir.join("routes.snap");
    let mut table: IpTable<u32> = IpTable::new();
    table.insert(net("10.0.0.0/8"), Some(1), Replace).unwrap();
    table.snapshot_to_file(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    table.insert(net("11.0.0.0/8"), Some(2), Replace).unwrap();
    for sync in [FileSync::None, FileSync::File, FileSync::FileAndDirectory] {
        table
            .snapshot_to_file_with_options(&path, &fast().file_sync(sync))
            .unwrap();
        assert_eq!(IpTable::<u32>::load_from_file(&path).unwrap().len(), 2);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    assert_eq!(dir_entries(&dir), ["routes.snap"]);
    assert!(matches!(
        IpTable::<u32>::load_from_file(dir.join("absent.snap")),
        Err(SnapshotError::Io(e)) if e.kind() == io::ErrorKind::NotFound
    ));
    fs::remove_dir_all(dir).unwrap();
}

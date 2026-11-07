//! Deep merge rules (typed and serde).

use std::collections::BTreeMap;
use std::net::IpAddr;

use dir_24_8::{DeepMerge, IpNet, IpTable, Merge, MergeOutcome, MergeWith, Replace};

fn net(s: &str) -> IpNet {
    s.parse().unwrap()
}

#[derive(Debug, PartialEq)]
struct Info {
    asn: u32,
    tags: Vec<String>,
    attrs: BTreeMap<String, Vec<u32>>,
    note: Option<String>,
}

impl DeepMerge for Info {
    fn deep_merge(&mut self, incoming: Self) {
        self.asn.deep_merge(incoming.asn);
        self.tags.deep_merge(incoming.tags);
        self.attrs.deep_merge(incoming.attrs);
        self.note.deep_merge(incoming.note);
    }
}

fn info(asn: u32, tags: &[&str], attrs: &[(&str, &[u32])], note: Option<&str>) -> Info {
    Info {
        asn,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        attrs: attrs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect(),
        note: note.map(str::to_string),
    }
}

#[test]
fn payload_rules() {
    let mut t: IpTable<Info> = IpTable::new();
    let n = net("10.0.0.0/8");
    assert_eq!(t.insert(n, None, Merge).unwrap(), MergeOutcome::Inserted);
    assert_eq!(t.get(n), Some(None));
    assert_eq!(t.insert(n, None, Merge).unwrap(), MergeOutcome::Unchanged);
    assert_eq!(
        t.insert(n, Some(info(1, &["a"], &[("x", &[1])], Some("n1"))), Merge)
            .unwrap(),
        MergeOutcome::Adopted
    );
    assert_eq!(t.insert(n, None, Merge).unwrap(), MergeOutcome::Unchanged);
    assert_eq!(t.get(n).unwrap().unwrap().asn, 1);
    assert_eq!(
        t.insert(
            n,
            Some(info(2, &["b", "a"], &[("x", &[2]), ("y", &[3])], None)),
            Merge
        )
        .unwrap(),
        MergeOutcome::Merged
    );
    assert_eq!(
        t.get(n).unwrap().unwrap(),
        &info(
            2,
            &["a", "b", "a"],
            &[("x", &[1, 2]), ("y", &[3])],
            Some("n1")
        )
    );
    // Normal insertion replaces, including by None.
    assert!(t.insert(n, None, Replace).unwrap().unwrap().is_some());
    assert_eq!(t.get(n), Some(None));
}

#[test]
fn merge_targets_only_the_normalized_cidr_and_family() {
    let mut t: IpTable<Vec<u8>> = IpTable::new();
    t.insert(net("10.0.0.0/8"), Some(vec![8]), Replace).unwrap();
    t.insert(net("10.0.0.0/16"), Some(vec![16]), Replace)
        .unwrap();
    t.insert(net("::ffff:10.0.0.0/104"), Some(vec![6]), Replace)
        .unwrap();
    assert_eq!(
        t.insert(net("10.9.9.9/8"), Some(vec![9]), Merge).unwrap(),
        MergeOutcome::Merged
    );
    assert_eq!(t.get(net("10.0.0.0/8")), Some(Some(&vec![8, 9])));
    assert_eq!(t.get(net("10.0.0.0/16")), Some(Some(&vec![16])));
    assert_eq!(t.get(net("::ffff:10.0.0.0/104")), Some(Some(&vec![6])));
    let ip: IpAddr = "10.200.0.1".parse().unwrap();
    assert_eq!(t.lookup_data(ip), Some(&vec![8, 9]));
}

#[test]
fn merge_moves_without_clone() {
    // Boxes make the "moved, not cloned" check observable (stable heap
    // addresses).
    #[allow(clippy::vec_box)]
    struct Moved(Vec<Box<u32>>);
    impl DeepMerge for Moved {
        fn deep_merge(&mut self, incoming: Self) {
            self.0.deep_merge(incoming.0);
        }
    }
    let mut t: IpTable<Moved> = IpTable::new();
    let n = net("2001:db8::/32");
    let b = Box::new(7u32);
    let addr = &*b as *const u32;
    t.insert(n, Some(Moved(vec![Box::new(1)])), Merge).unwrap();
    t.insert(n, Some(Moved(vec![b])), Merge).unwrap();
    let v = &t.get(n).unwrap().unwrap().0;
    assert_eq!(v.len(), 2);
    assert_eq!(
        &*v[1] as *const u32, addr,
        "the incoming box was moved, not cloned"
    );
}

#[test]
fn merge_with_closure() {
    let mut t: IpTable<u32> = IpTable::new();
    let n = net("192.0.2.0/24");
    let add = |a: &mut u32, b: u32| *a += b;
    assert_eq!(
        t.insert(n, Some(1), MergeWith(add)).unwrap(),
        MergeOutcome::Inserted
    );
    assert_eq!(
        t.insert(n, Some(2), MergeWith(add)).unwrap(),
        MergeOutcome::Merged
    );
    assert_eq!(t.get(n), Some(Some(&3)));
    // `None` keeps the payload, the closure is not called.
    assert_eq!(
        t.insert(n, None, MergeWith(|_: &mut u32, _| unreachable!()))
            .unwrap(),
        MergeOutcome::Unchanged
    );
}

#[cfg(feature = "serde")]
mod serde_merge {
    use super::*;
    use dir_24_8::SerdeMerge;
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Rec {
        source: String,
        asn: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        city: Option<String>,
        tags: Vec<String>,
        scores: BTreeMap<String, i32>,
        nested: Option<Nested>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Nested {
        #[serde(skip_serializing_if = "Option::is_none")]
        lat: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lon: Option<f64>,
    }

    #[test]
    fn serde_rules_match_the_reference() {
        let mut t: IpTable<Rec> = IpTable::new();
        let n = net("203.0.113.0/24");
        t.insert(
            n,
            Some(Rec {
                source: "geo".into(),
                asn: 1,
                note: Some("keep".into()),
                city: Some("Paris".into()),
                tags: vec!["a".into(), "shared".into()],
                scores: BTreeMap::from([("geo".into(), 1), ("fraud".into(), 2)]),
                nested: Some(Nested {
                    lat: Some(48.8),
                    lon: Some(2.3),
                }),
            }),
            SerdeMerge,
        )
        .unwrap();
        let out = t
            .insert(
                n,
                Some(Rec {
                    source: "threat".into(),
                    asn: 2,
                    note: None,                // skipped: keeps "keep"
                    city: Some("Lyon".into()), // scalar: replaced
                    tags: vec!["b".into(), "shared".into()],
                    scores: BTreeMap::from([("fraud".into(), 9), ("bot".into(), 4)]),
                    nested: Some(Nested {
                        lat: None,
                        lon: Some(4.8),
                    }),
                }),
                SerdeMerge,
            )
            .unwrap();
        assert_eq!(out, MergeOutcome::Merged);
        let r = t.get(n).unwrap().unwrap();
        assert_eq!(r.source, "threat");
        assert_eq!(r.asn, 2);
        assert_eq!(r.note.as_deref(), Some("keep"));
        assert_eq!(r.city.as_deref(), Some("Lyon"));
        assert_eq!(r.tags, ["a", "shared", "b", "shared"]);
        assert_eq!(
            r.scores,
            BTreeMap::from([("geo".into(), 1), ("fraud".into(), 9), ("bot".into(), 4)])
        );
        assert_eq!(
            r.nested,
            Some(Nested {
                lat: Some(48.8),
                lon: Some(4.8)
            })
        );
    }

    #[test]
    fn explicit_null_replaces_and_incompatible_types_take_incoming() {
        let mut t: IpTable<Value> = IpTable::new();
        let n = net("2001:db8::/32");
        t.insert(
            n,
            Some(json!({"a": 1, "b": [1], "c": {"x": 1}, "d": "s"})),
            Merge,
        )
        .unwrap();
        t.insert(
            n,
            Some(json!({"a": null, "b": {"k": 2}, "c": {"y": 2}, "e": [3]})),
            Merge,
        )
        .unwrap();
        assert_eq!(
            t.get(n).unwrap().unwrap(),
            &json!({"a": null, "b": {"k": 2}, "c": {"x": 1, "y": 2}, "d": "s", "e": [3]})
        );
    }

    #[test]
    fn serde_failure_keeps_existing() {
        let mut t: IpTable<[u8; 2]> = IpTable::new();
        let n = net("10.0.0.0/8");
        t.insert(n, Some([1, 2]), SerdeMerge).unwrap();
        // Concatenation yields 4 elements, which cannot deserialize as [u8; 2].
        let err = t.insert(n, Some([3, 4]), SerdeMerge).unwrap_err();
        assert!(matches!(err, dir_24_8::Error::MergeFailed(_)));
        assert_eq!(t.get(n), Some(Some(&[1, 2])));
    }
}

#[cfg(feature = "derive")]
mod derive {
    use super::*;
    use std::collections::HashMap;

    #[derive(DeepMerge, Debug, Default, PartialEq)]
    struct Record {
        tags: Vec<String>,
        attrs: HashMap<String, Vec<u32>>,
        note: Option<String>,
        asn: u32,
        #[deep_merge(skip)]
        first_seen: u64,
        #[deep_merge(replace)]
        raw: Vec<u8>,
        nested: Inner,
    }

    #[derive(DeepMerge, Debug, Default, PartialEq)]
    struct Inner(Vec<u32>, Option<u8>);

    #[derive(DeepMerge, Debug, Default, PartialEq)]
    struct Marker;

    #[derive(DeepMerge, Debug, PartialEq)]
    enum Origin {
        Igp { metric: u32, tags: Vec<u8> },
        Egp(Vec<u32>),
        Incomplete,
    }

    #[derive(DeepMerge, Debug, PartialEq)]
    struct Generic<T, U> {
        items: Vec<T>,
        meta: Option<U>,
        #[deep_merge(replace)]
        label: &'static str,
    }

    // A field type that does not implement `DeepMerge` is fine when it is
    // replaced or skipped.
    #[derive(Debug, PartialEq)]
    struct Opaque(u8);

    #[derive(DeepMerge, Debug, PartialEq)]
    struct WithOpaque {
        #[deep_merge(replace)]
        a: Opaque,
        #[deep_merge(skip)]
        b: Opaque,
    }

    #[test]
    fn struct_fields_follow_their_own_rules() {
        let mut a = Record {
            tags: vec!["a".into()],
            attrs: HashMap::from([("x".into(), vec![1])]),
            note: Some("first".into()),
            asn: 1,
            first_seen: 10,
            raw: vec![1, 2],
            nested: Inner(vec![1], None),
        };
        a.deep_merge(Record {
            tags: vec!["b".into()],
            attrs: HashMap::from([("x".into(), vec![2]), ("y".into(), vec![3])]),
            note: None,
            asn: 2,
            first_seen: 20,
            raw: vec![9],
            nested: Inner(vec![2], Some(7)),
        });
        assert_eq!(a.tags, ["a", "b"]);
        assert_eq!(a.attrs["x"], [1, 2]);
        assert_eq!(a.attrs["y"], [3]);
        assert_eq!(a.note.as_deref(), Some("first"));
        assert_eq!(a.asn, 2);
        assert_eq!(a.first_seen, 10, "skip keeps the existing value");
        assert_eq!(a.raw, [9], "replace does not concatenate");
        assert_eq!(a.nested, Inner(vec![1, 2], Some(7)));
        let mut m = Marker;
        m.deep_merge(Marker);
        assert_eq!(m, Marker);
    }

    #[test]
    fn enums_merge_same_variants_and_replace_others() {
        let mut o = Origin::Igp {
            metric: 1,
            tags: vec![1],
        };
        o.deep_merge(Origin::Igp {
            metric: 2,
            tags: vec![2],
        });
        assert_eq!(
            o,
            Origin::Igp {
                metric: 2,
                tags: vec![1, 2]
            }
        );
        o.deep_merge(Origin::Egp(vec![5]));
        assert_eq!(o, Origin::Egp(vec![5]));
        o.deep_merge(Origin::Egp(vec![6]));
        assert_eq!(o, Origin::Egp(vec![5, 6]));
        o.deep_merge(Origin::Incomplete);
        assert_eq!(o, Origin::Incomplete);
    }

    #[test]
    fn generics_and_non_mergeable_fields() {
        let mut g = Generic {
            items: vec![1u8],
            meta: None::<String>,
            label: "a",
        };
        g.deep_merge(Generic {
            items: vec![2],
            meta: Some("m".into()),
            label: "b",
        });
        assert_eq!(g.items, [1, 2]);
        assert_eq!(g.meta.as_deref(), Some("m"));
        assert_eq!(g.label, "b");
        let mut w = WithOpaque {
            a: Opaque(1),
            b: Opaque(1),
        };
        w.deep_merge(WithOpaque {
            a: Opaque(2),
            b: Opaque(2),
        });
        assert_eq!((w.a, w.b), (Opaque(2), Opaque(1)));
    }

    #[test]
    fn derived_payloads_merge_in_the_table() {
        let mut t: IpTable<Record> = IpTable::new();
        let n = net("192.0.2.0/24");
        let rec = |tag: &str, seen: u64| Record {
            tags: vec![tag.into()],
            first_seen: seen,
            ..Record::default()
        };
        assert_eq!(
            t.insert(n, Some(rec("a", 1)), Merge).unwrap(),
            MergeOutcome::Inserted
        );
        assert_eq!(
            t.insert(n, Some(rec("b", 2)), Merge).unwrap(),
            MergeOutcome::Merged
        );
        let got = t
            .lookup_data("192.0.2.9".parse::<IpAddr>().unwrap())
            .unwrap();
        assert_eq!(got.tags, ["a", "b"]);
        assert_eq!(got.first_seen, 1);
        // Replace mode still overwrites.
        t.insert(n, Some(rec("c", 3)), Replace).unwrap();
        assert_eq!(t.get(n).unwrap().unwrap().tags, ["c"]);
    }
}

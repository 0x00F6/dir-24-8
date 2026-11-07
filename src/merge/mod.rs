//! Deep merge of payloads inserted for the same normalized CIDR.
//!
//! Payload rules applied by [`IpTable::insert`](crate::IpTable::insert)
//! with the [`Merge`](crate::Merge), [`MergeWith`](crate::MergeWith) and
//! `SerdeMerge` modes (the CIDR is normalized first and merges never cross
//! families or CIDRs):
//!
//! | Existing route | Incoming     | Result                        | Outcome                 |
//! |----------------|--------------|-------------------------------|-------------------------|
//! | absent         | `None`/`Some`| new route with the incoming   | [`MergeOutcome::Inserted`] |
//! | any            | `None`       | existing payload kept         | [`MergeOutcome::Unchanged`] |
//! | `None`         | `Some(v)`    | `v` adopted (moved)           | [`MergeOutcome::Adopted`] |
//! | `Some(cur)`    | `Some(v)`    | `cur.deep_merge(v)`           | [`MergeOutcome::Merged`] |
//!
//! The [`Replace`](crate::Replace) mode instead replaces the payload of an
//! existing CIDR, including by `None`.
//!
//! With the `derive` feature, `#[derive(DeepMerge)]` implements
//! [`DeepMerge`] for your structs and enums field by field (no hand-written
//! `impl`), with `#[deep_merge(replace)]` and `#[deep_merge(skip)]` field
//! attributes.
//!
//! [`DeepMerge`] consumes the incoming value so implementations move fields,
//! strings and vectors instead of cloning them. The provided implementations
//! follow the same rules as the serde adapter:
//!
//! | Data                          | Behavior                                |
//! |-------------------------------|-----------------------------------------|
//! | maps (`HashMap`, `BTreeMap`)  | recursive merge per key, new keys added |
//! | `Vec`, `VecDeque`             | concatenation, order and duplicates kept|
//! | `Option<T>`                   | `None` keeps, `Some` adopts or merges   |
//! | scalars, `String`, `Box<str>` | incoming value wins                     |

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{BuildHasher, Hash};

#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub mod serde;

/// Merges an incoming value into an existing one, consuming the incoming
/// value (fields, strings and vectors are moved, never cloned).
///
/// Implemented for scalars, strings, `Vec`, `VecDeque`, `Option`, `Box`,
/// `HashMap`, `BTreeMap` and the IP types (rules in the [module
/// documentation](self)). For your own types, derive it with the `derive`
/// feature:
///
/// ```
/// # #[cfg(feature = "derive")] {
/// use dir_24_8::DeepMerge;
///
/// #[derive(DeepMerge)]
/// struct Info { tags: Vec<String>, asn: u32 }
///
/// let mut a = Info { tags: vec!["a".into()], asn: 1 };
/// a.deep_merge(Info { tags: vec!["b".into()], asn: 2 });
/// assert_eq!((a.tags.len(), a.asn), (2, 2));
/// # }
/// ```
///
/// or implement it by hand:
///
/// ```
/// use dir_24_8::DeepMerge;
///
/// struct Info { tags: Vec<String>, asn: u32 }
///
/// impl DeepMerge for Info {
///     fn deep_merge(&mut self, incoming: Self) {
///         self.tags.deep_merge(incoming.tags); // concatenation
///         self.asn.deep_merge(incoming.asn);   // replacement
///     }
/// }
///
/// let mut a = Info { tags: vec!["a".into()], asn: 1 };
/// a.deep_merge(Info { tags: vec!["b".into()], asn: 2 });
/// assert_eq!(a.tags, ["a", "b"]);
/// assert_eq!(a.asn, 2);
/// ```
pub trait DeepMerge {
    /// Merges `incoming` into `self`, consuming it.
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use dir_24_8::DeepMerge;
    ///
    /// let mut seen: HashMap<&str, Vec<u32>> = HashMap::from([("a", vec![1])]);
    /// seen.deep_merge(HashMap::from([("a", vec![2]), ("b", vec![3])]));
    /// assert_eq!(seen["a"], [1, 2]); // per key, vectors concatenated
    /// assert_eq!(seen["b"], [3]);
    ///
    /// let mut note = Some(String::from("kept"));
    /// note.deep_merge(None); // `None` keeps the existing value
    /// assert_eq!(note.as_deref(), Some("kept"));
    /// ```
    fn deep_merge(&mut self, incoming: Self);
}

/// What [`IpTable::insert`](crate::IpTable::insert) did in a merging mode
/// ([`Merge`](crate::Merge), [`MergeWith`](crate::MergeWith), `SerdeMerge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeOutcome {
    /// The CIDR was absent: a new route was inserted.
    Inserted,
    /// The incoming payload was `None`: the existing payload was kept.
    Unchanged,
    /// The route had no payload: the incoming payload was adopted.
    Adopted,
    /// Both payloads existed and were deep merged.
    Merged,
}

macro_rules! replace_merge {
    ($($t:ty),* $(,)?) => {$(
        impl DeepMerge for $t {
            #[inline]
            fn deep_merge(&mut self, incoming: Self) {
                *self = incoming;
            }
        }
    )*};
}

replace_merge!(
    bool,
    char,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    String,
    Box<str>,
    core::net::IpAddr,
    core::net::Ipv4Addr,
    core::net::Ipv6Addr,
    ipnet::IpNet,
);

impl<T> DeepMerge for Vec<T> {
    #[inline]
    fn deep_merge(&mut self, mut incoming: Self) {
        if self.is_empty() {
            // Reuse the incoming allocation.
            *self = incoming;
        } else {
            self.append(&mut incoming);
        }
    }
}

impl<T> DeepMerge for VecDeque<T> {
    #[inline]
    fn deep_merge(&mut self, mut incoming: Self) {
        self.append(&mut incoming);
    }
}

impl<T: DeepMerge> DeepMerge for Option<T> {
    #[inline]
    fn deep_merge(&mut self, incoming: Self) {
        match (self.as_mut(), incoming) {
            (_, None) => {}
            (None, Some(v)) => *self = Some(v),
            (Some(cur), Some(v)) => cur.deep_merge(v),
        }
    }
}

impl<T: DeepMerge> DeepMerge for Box<T> {
    #[inline]
    fn deep_merge(&mut self, incoming: Self) {
        (**self).deep_merge(*incoming);
    }
}

impl<K: Eq + Hash, V: DeepMerge, S: BuildHasher> DeepMerge for HashMap<K, V, S> {
    fn deep_merge(&mut self, incoming: Self) {
        self.reserve(incoming.len());
        for (k, v) in incoming {
            match self.get_mut(&k) {
                Some(cur) => cur.deep_merge(v),
                None => {
                    self.insert(k, v);
                }
            }
        }
    }
}

impl<K: Ord, V: DeepMerge> DeepMerge for BTreeMap<K, V> {
    fn deep_merge(&mut self, incoming: Self) {
        for (k, v) in incoming {
            match self.get_mut(&k) {
                Some(cur) => cur.deep_merge(v),
                None => {
                    self.insert(k, v);
                }
            }
        }
    }
}

/// Applies the payload rules of the module documentation to an existing
/// route's payload. `merge` is only called when both sides are `Some`.
pub(crate) fn merge_payload<T, E>(
    existing: &mut Option<T>,
    incoming: Option<T>,
    merge: impl FnOnce(&mut T, T) -> Result<(), E>,
) -> Result<MergeOutcome, E> {
    match (existing.as_mut(), incoming) {
        (_, None) => Ok(MergeOutcome::Unchanged),
        (None, Some(v)) => {
            *existing = Some(v);
            Ok(MergeOutcome::Adopted)
        }
        (Some(cur), Some(v)) => merge(cur, v).map(|()| MergeOutcome::Merged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_impls_follow_the_rules() {
        let mut v = vec![1, 2];
        v.deep_merge(vec![2, 3]);
        assert_eq!(v, [1, 2, 2, 3]);

        let mut o = Some(vec![1]);
        o.deep_merge(None);
        assert_eq!(o, Some(vec![1]));
        o.deep_merge(Some(vec![2]));
        assert_eq!(o, Some(vec![1, 2]));
        let mut n: Option<u8> = None;
        n.deep_merge(Some(4));
        assert_eq!(n, Some(4));

        let mut m = BTreeMap::from([("a", vec![1]), ("b", vec![2])]);
        m.deep_merge(BTreeMap::from([("b", vec![3]), ("c", vec![4])]));
        assert_eq!(m["a"], [1]);
        assert_eq!(m["b"], [2, 3]);
        assert_eq!(m["c"], [4]);

        let mut s = String::from("old");
        s.deep_merge("new".into());
        assert_eq!(s, "new");
    }

    #[test]
    fn payload_rules() {
        let ok = |a: &mut Vec<u8>, b: Vec<u8>| -> Result<(), ()> {
            a.deep_merge(b);
            Ok(())
        };
        let mut p = Some(vec![1]);
        assert_eq!(merge_payload(&mut p, None, ok), Ok(MergeOutcome::Unchanged));
        assert_eq!(p, Some(vec![1]));
        assert_eq!(
            merge_payload(&mut p, Some(vec![2]), ok),
            Ok(MergeOutcome::Merged)
        );
        assert_eq!(p, Some(vec![1, 2]));
        let mut e: Option<Vec<u8>> = None;
        assert_eq!(
            merge_payload(&mut e, Some(vec![9]), ok),
            Ok(MergeOutcome::Adopted)
        );
        assert_eq!(e, Some(vec![9]));
    }
}

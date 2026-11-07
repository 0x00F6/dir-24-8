//! Insert modes: what [`IpTable::insert`] does when the CIDR is already in
//! the table.
//!
//! The mode is the third argument of [`IpTable::insert`],
//! [`IpTable::insert_addr`] and [`IpTable::try_extend`]. Each mode is a
//! zero-sized (or closure-sized) type, so the choice costs nothing at run
//! time and only the merging modes require anything from the payload type:
//!
//! | Mode | CIDR absent | CIDR present | Requires | Returns |
//! |---|---|---|---|---|
//! | [`Replace`] | inserted | payload replaced (including by `None`) | nothing | previous payload: `Option<Option<T>>` |
//! | [`Merge`] | inserted | payload deep merged ([rules](crate::merge)) | `T: DeepMerge` | [`MergeOutcome`] |
//! | [`MergeWith`] | inserted | your function merges the two payloads | `FnOnce(&mut T, T)` | [`MergeOutcome`] |
//! | `SerdeMerge` | inserted | deep merge through `serde_json::Value` | feature `serde`, `T: Serialize + DeserializeOwned` | [`MergeOutcome`] |
//!
//! ```
//! use dir_24_8::{DeepMerge, IpNet, IpTable, Merge, MergeOutcome, Replace};
//!
//! let net: IpNet = "10.0.0.0/8".parse().unwrap();
//! let mut table: IpTable<Vec<&str>> = IpTable::new();
//!
//! assert_eq!(table.insert(net, Some(vec!["a"]), Replace)?, None); // new CIDR
//! assert_eq!(table.insert(net, Some(vec!["b"]), Merge)?, MergeOutcome::Merged);
//! assert_eq!(table.get(net), Some(Some(&vec!["a", "b"])));
//! assert_eq!(table.insert(net, Some(vec!["c"]), Replace)?, Some(Some(vec!["a", "b"])));
//! # Ok::<(), dir_24_8::Error>(())
//! ```

use crate::merge::{DeepMerge, MergeOutcome};
use crate::{Error, IpNet, IpTable};

mod sealed {
    pub trait Sealed {}
}

/// An insert mode, see the [module documentation](self). Implemented by
/// [`Replace`], [`Merge`], [`MergeWith`] and `SerdeMerge`; sealed.
pub trait InsertMode<T>: sealed::Sealed {
    /// What [`IpTable::insert`] returns with this mode.
    type Output;

    #[doc(hidden)]
    fn apply(
        self,
        table: &mut IpTable<T>,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Self::Output, Error>;
}

/// Insert mode: a CIDR already present gets the new payload (including
/// `None`); [`IpTable::insert`] returns the previous payload, or `None` when
/// the CIDR was absent. Works for any payload type.
///
/// ```
/// use dir_24_8::{IpTable, Replace};
///
/// let mut table = IpTable::new();
/// assert_eq!(table.insert("192.0.2.0/24".parse::<dir_24_8::IpNet>()?, Some("a"), Replace)?, None);
/// assert_eq!(table.insert("192.0.2.0/24".parse::<dir_24_8::IpNet>()?, Some("b"), Replace)?, Some(Some("a")));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Replace;

/// Insert mode: a CIDR already present gets its payload deep merged with
/// [`DeepMerge`] (rules in [`crate::merge`]); [`IpTable::insert`] returns a
/// [`MergeOutcome`].
///
/// With the `derive` feature, `#[derive(DeepMerge)]` implements the trait
/// for your own payload types.
///
/// ```
/// use std::collections::BTreeMap;
/// use dir_24_8::{IpNet, IpTable, Merge, MergeOutcome};
///
/// let net: IpNet = "2001:db8::/32".parse()?;
/// let mut table: IpTable<BTreeMap<&str, u32>> = IpTable::new();
/// table.insert(net, Some(BTreeMap::from([("asn", 64500)])), Merge)?;
/// let outcome = table.insert(net, Some(BTreeMap::from([("pop", 7)])), Merge)?;
/// assert_eq!(outcome, MergeOutcome::Merged);
/// assert_eq!(table.get(net).unwrap().unwrap().len(), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Merge;

/// Insert mode: like [`Merge`] with your own merge function, called only
/// when both the existing and the incoming payloads are `Some`.
///
/// ```
/// use dir_24_8::{IpNet, IpTable, MergeWith};
///
/// let net: IpNet = "198.51.100.0/24".parse()?;
/// let mut hits: IpTable<u64> = IpTable::new();
/// hits.insert(net, Some(3), MergeWith(|cur: &mut u64, inc| *cur += inc))?;
/// hits.insert(net, Some(4), MergeWith(|cur: &mut u64, inc| *cur += inc))?;
/// assert_eq!(hits.get(net), Some(Some(&7)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct MergeWith<F>(pub F);

/// Insert mode (feature `serde`): a CIDR already present gets its payload
/// deep merged through `serde_json::Value` (rules in
/// [`crate::merge::serde`]). The merge happens once, at insertion; lookups
/// borrow the resulting `T`. A failed merge returns [`Error::MergeFailed`]
/// and leaves the payload unchanged.
#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SerdeMerge;

impl sealed::Sealed for Replace {}
impl sealed::Sealed for Merge {}
impl<F> sealed::Sealed for MergeWith<F> {}
#[cfg(feature = "serde")]
impl sealed::Sealed for SerdeMerge {}

impl<T> InsertMode<T> for Replace {
    type Output = Option<Option<T>>;

    #[inline]
    fn apply(
        self,
        table: &mut IpTable<T>,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Self::Output, Error> {
        table.insert_replacing(net, data)
    }
}

impl<T: DeepMerge> InsertMode<T> for Merge {
    type Output = MergeOutcome;

    #[inline]
    fn apply(
        self,
        table: &mut IpTable<T>,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Self::Output, Error> {
        table.insert_merging(net, data, |cur, inc| {
            cur.deep_merge(inc);
            Ok::<(), core::convert::Infallible>(())
        })
    }
}

impl<T, F: FnOnce(&mut T, T)> InsertMode<T> for MergeWith<F> {
    type Output = MergeOutcome;

    #[inline]
    fn apply(
        self,
        table: &mut IpTable<T>,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Self::Output, Error> {
        let f = self.0;
        table.insert_merging(net, data, |cur, inc| {
            f(cur, inc);
            Ok::<(), core::convert::Infallible>(())
        })
    }
}

#[cfg(feature = "serde")]
impl<T> InsertMode<T> for SerdeMerge
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    type Output = MergeOutcome;

    fn apply(
        self,
        table: &mut IpTable<T>,
        net: IpNet,
        data: Option<T>,
    ) -> Result<Self::Output, Error> {
        table.insert_merging(net, data, crate::merge::serde::serde_deep_merge)
    }
}

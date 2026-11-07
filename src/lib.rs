//! Longest-prefix-match (LPM) tables for **IPv4 and IPv6** CIDRs, with
//! optional payloads, borrowed lookup results and optional SIMD batch
//! lookups (**AVX-512, AVX2, NEON**, feature `batch-simd`; scalar kernels
//! otherwise).
//!
//! | Family | Structure | Accesses per lookup | Prefix lengths |
//! |---|---|---|---|
//! | IPv4 | **DIR-24-8**: a 2^24-cell root (`tbl24`) + 256-cell groups (`tbl8`) | 1 or 2 | `/0` to `/32` |
//! | IPv6 | **24/8 multi-level trie**: a 2^24-cell root + up to 13 levels of 256-cell groups | 1 to 14 | `/0` to `/128` |
//!
//! * Lookups return [`Option<LookupMatch<'_, T>>`](LookupMatch): the
//!   winning prefix and a **borrowed** payload. They never allocate, clone
//!   `T` or lock, and `T` needs no `Clone` bound.
//! * Single lookups use scalar kernels. Batches
//!   ([`IpTable::lookup_batch_into`] and the other `*_batch_into`/`*_array`
//!   methods) do too by default; with the `batch-simd` feature they use
//!   AVX-512 or AVX2 gathers, or NEON, detected once per table.
//! * Every insertion takes an [insert mode](mode): [`Replace`] the payload
//!   of an existing CIDR, or deep [`Merge`] it ([`DeepMerge`], derivable
//!   with the `derive` feature).
//! * With the `snapshot` feature, a table is saved as a compressed snapshot
//!   (XZ, streamed into a `Vec<u8>`, a file or a writer) and rebuilt from
//!   it: see the `snapshot` module.
//!
//! # Quick start
//!
//! ```
//! use dir_24_8::{IpNet, IpTable, Replace};
//!
//! let mut table: IpTable<&str> = IpTable::new();
//! table.insert("10.0.0.0/8".parse::<IpNet>()?, Some("corp"), Replace)?;
//! table.insert("10.1.0.0/16".parse::<IpNet>()?, None, Replace)?; // a route without payload
//! table.insert("2001:db8::/32".parse::<IpNet>()?, Some("doc"), Replace)?;
//!
//! // Longest prefix match: the winning prefix and a borrowed payload.
//! let m = table.lookup("10.2.3.4".parse()?).unwrap();
//! assert_eq!(m.prefix.to_string(), "10.0.0.0/8");
//! assert_eq!(m.data, Some(&"corp"));
//!
//! // The more specific route wins even without payload.
//! let m = table.lookup("10.1.9.9".parse()?).unwrap();
//! assert_eq!((m.prefix.to_string(), m.data), ("10.1.0.0/16".to_string(), None));
//!
//! // `exist`: is the address covered by any route?
//! assert!(table.exist("2001:db8::1".parse()?));
//! assert!(!table.exist("192.0.2.1".parse()?));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Lookup results
//!
//! | Result | Meaning |
//! |---|---|
//! | `None` | no route covers the address |
//! | `Some(LookupMatch { data: None, .. })` | the winning route has no payload |
//! | `Some(LookupMatch { data: Some(&t), .. })` | the winning route's payload, borrowed |
//!
//! [`IpTable::lookup_data`] returns the payload alone, [`IpTable::exist`]
//! only whether a route covers the address. IPv4 and IPv6 are separate key
//! spaces: `192.0.2.1` and `::ffff:192.0.2.1` are different keys.
//!
//! # Batches
//!
//! [`IpTable::lookup_batch_into`] resolves a slice of keys into a caller
//! buffer of the same length, in input order; [`IpTable::lookup_array`]
//! does the same for fixed-size arrays and returns the array. Keys
//! ([`BatchKey`]) may be [`IpAddr`](std::net::IpAddr) (mixed families),
//! [`Ipv4Addr`](std::net::Ipv4Addr), [`Ipv6Addr`](std::net::Ipv6Addr),
//! numeric (`u32`, `u128`) or octets (`[u8; 4]`, `[u8; 16]`).
//!
//! ```
//! use std::net::IpAddr;
//! use dir_24_8::{IpNet, IpTable, Replace};
//!
//! let mut table = IpTable::new();
//! table.insert("10.0.0.0/8".parse::<IpNet>()?, Some(1), Replace)?;
//! table.insert("2001:db8::/32".parse::<IpNet>()?, Some(2), Replace)?;
//!
//! let keys: Vec<IpAddr> = vec!["10.9.9.9".parse()?, "2001:db8::1".parse()?, "192.0.2.1".parse()?];
//! let mut out = vec![None; keys.len()];
//! table.lookup_batch_into(&keys, &mut out)?;
//! let data: Vec<Option<i32>> = out.iter().map(|m| m.and_then(|m| m.data.copied())).collect();
//! assert_eq!(data, [Some(1), Some(2), None]);
//!
//! // Membership only, numeric keys, fixed-size array:
//! assert_eq!(table.exist_array(&[0x0A00_0001u32, 0xC000_0201]), [true, false]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Insert modes and deep merge
//!
//! [`IpTable::insert`] normalizes the CIDR (host bits cleared) and takes a
//! mode for the case where that CIDR is already present (details in
//! [`mode`]):
//!
//! | Mode | CIDR present | Returns |
//! |---|---|---|
//! | [`Replace`] | payload replaced, including by `None` | previous payload |
//! | [`Merge`] | payload deep merged with [`DeepMerge`] | [`MergeOutcome`] |
//! | [`MergeWith`] | payload merged by your function | [`MergeOutcome`] |
//! | `SerdeMerge` (feature `serde`) | merged through `serde_json::Value` | [`MergeOutcome`] |
//!
//! With the `derive` feature, `#[derive(DeepMerge)]` implements the trait
//! field by field, so a payload type needs no hand-written `impl`:
//!
//! ```
//! # #[cfg(feature = "derive")] {
//! use dir_24_8::{DeepMerge, IpNet, IpTable, Merge, MergeOutcome};
//!
//! #[derive(DeepMerge, Debug, Default)]
//! struct Info {
//!     tags: Vec<String>, // concatenated
//!     asn: u32,          // incoming value wins
//!     #[deep_merge(skip)]
//!     first_seen: u64,   // existing value kept
//! }
//!
//! let net: IpNet = "192.0.2.0/24".parse().unwrap();
//! let mut table = IpTable::new();
//! let a = Info { tags: vec!["geo".into()], asn: 1, first_seen: 10 };
//! let b = Info { tags: vec!["threat".into()], asn: 2, first_seen: 20 };
//! assert_eq!(table.insert(net, Some(a), Merge).unwrap(), MergeOutcome::Inserted);
//! assert_eq!(table.insert(net, Some(b), Merge).unwrap(), MergeOutcome::Merged);
//!
//! let info = table.lookup_data("192.0.2.7".parse().unwrap()).unwrap();
//! assert_eq!(info.tags, ["geo", "threat"]);
//! assert_eq!((info.asn, info.first_seen), (2, 10));
//! # }
//! ```
//!
//! # Lookup kernels
//!
//! There is no kernel to choose: single lookups ([`IpTable::lookup`],
//! [`IpTable::exist`], ...) always run the scalar kernels, the fastest for
//! one isolated lookup (see `docs/performance.md`). Batch lookups
//! ([`IpTable::lookup_batch_into`], [`IpTable::lookup_array`],
//! [`IpTable::exist_batch_into`], [`IpTable::exist_array`],
//! [`IpTable::lookup_ids_into`]) run the kernel below, detected once when
//! the table is built:
//!
//! | Build | CPU | Batch kernel |
//! |---|---|---|
//! | default | any | `scalar`: portable two-phase blocks |
//! | `batch-simd` | x86-64 with AVX-512 F, BW, VL | `avx512`: 16-lane masked gathers |
//! | `batch-simd` | x86-64 with AVX2 | `avx2`: 8-lane masked gathers |
//! | `batch-simd` | AArch64 | `neon`: 4-lane index arithmetic, per-lane loads |
//! | `batch-simd` | other | `scalar` |
//!
//! Every kernel returns the same results; [`TableStats::batch_kernel`]
//! names the one in use.
//!
//! ```
//! use dir_24_8::IpTable;
//!
//! let table: IpTable<u32> = IpTable::new();
//! println!("batch kernel: {}", table.stats().batch_kernel);
//! if cfg!(not(feature = "batch-simd")) {
//!     assert_eq!(table.stats().batch_kernel, "scalar");
//! }
//! ```
//!
//! # Memory
//!
//! Each family's root is 2^24 cells of 4 bytes (**64 MiB**), mapped on the
//! first insertion of that family and zeroed lazily by the kernel; a
//! secondary group is 1 KiB. IPv6 routes longer than /24 need one group per
//! level on their path, so IPv6 tables are dominated by groups (about
//! 1.8-2.3 KiB per route on BGP-like tables). [`IpTable::stats`] reports
//! routes, groups and bytes per component; [`Config`] sets capacity limits,
//! and exceeding them returns [`Error::CapacityExceeded`] with the table
//! unchanged. Allocations only happen in mutations.
//!
//! # Concurrency
//!
//! `IpTable<T>` is `Send` and `Sync` when `T` is. Lookups take `&self` and
//! need no synchronization; mutations take `&mut self`.
//!
//! # Feature flags
//!
//! | Feature | Adds |
//! |---|---|
//! | `batch-simd` | AVX-512, AVX2 (x86-64) and NEON (AArch64) batch kernels, selected at runtime from the CPU |
//! | `derive` | `#[derive(DeepMerge)]` (from the `dir-24-8-derive` crate) |
//! | `serde` | the `SerdeMerge` insert mode and `DeepMerge for serde_json::Value` |
//! | `snapshot` | compressed snapshots: `IpTable::snapshot`, `snapshot_to_file`, `load`, `load_from_file` (module `snapshot`; links liblzma) |
//!
//! # Platform support and safety
//!
//! Stable Rust 1.89 or later. With `batch-simd`, SIMD kernels are compiled
//! for x86-64 (AVX2, AVX-512) and AArch64 (NEON) behind `cfg` and
//! `#[target_feature]`, and only selected after runtime detection. Every gather index is either a
//! 24-bit root index or clamped to the group buffer, so no lane can read out
//! of bounds. On Linux, large tables are `mmap`ed outside the global
//! allocator; elsewhere they are heap allocated.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

mod error;
mod ipv4;
mod ipv6;
mod lookup;
pub mod merge;
pub mod mode;
mod route;
mod simd;
#[cfg(feature = "snapshot")]
#[cfg_attr(docsrs, doc(cfg(feature = "snapshot")))]
pub mod snapshot;
mod table;

pub use error::{AddressFamily, Error, Resource};
pub use ipnet::{IpNet, Ipv4Net, Ipv6Net};
pub use lookup::{BatchKey, LookupMatch, MatchId};
pub use merge::{DeepMerge, MergeOutcome};
#[cfg(feature = "serde")]
pub use mode::SerdeMerge;
pub use mode::{InsertMode, Merge, MergeWith, Replace};
#[cfg(feature = "snapshot")]
pub use snapshot::{
    EntryOrder, FileSync, LoadOptions, OverBudget, SnapshotError, SnapshotLimit, SnapshotOptions,
    SnapshotSummary,
};
pub use table::{Config, FamilyStats, IpTable, TableStats};

/// Derives [`DeepMerge`](trait@DeepMerge) field by field (feature
/// `derive`); see the [module documentation](merge) for the merge rules.
#[cfg(feature = "derive")]
#[cfg_attr(docsrs, doc(cfg(feature = "derive")))]
pub use dir_24_8_derive::DeepMerge;

// Lets `#[derive(DeepMerge)]` (which names `::dir_24_8`) be used inside this
// crate's own tests.
extern crate self as dir_24_8;

// The README examples (repository and crates.io pages) are compiled and run
// as doctests.
#[cfg(all(doctest, feature = "derive", feature = "snapshot"))]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

#[cfg(all(doctest, feature = "derive", feature = "snapshot"))]
#[doc = include_str!("../README.crates.md")]
pub struct ReadmeCratesDoctests;

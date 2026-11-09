//! Library of `bench-compare` (`src/main.rs`), which measures two
//! comparisons and draws them in one report (`report.html`, a tab each):
//! the structures part (dir-24-8 against Rust LPM and membership
//! structures, in process) and the libraries part (lookups of six
//! libraries in Rust, Go and C, each in a native runner, [`libcmp`]). They
//! share the data generator and its oracle, the memory and system probes
//! and the report assets ([`page`]).

#![allow(clippy::new_without_default)]

pub mod adapter;
pub mod bench;
pub mod core;
pub mod dataset;
pub mod engines;
pub mod libcmp;
pub mod measure;
pub mod options;
pub mod page;
pub mod readme;
pub mod registry;
pub mod report;
pub mod sys;
pub mod timer;

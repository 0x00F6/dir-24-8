//! Library of the two comparison binaries: `bench-compare` (dir-24-8
//! against Rust LPM and membership structures, `src/main.rs`) and
//! `bench-compare-lib` (lookups of seven libraries in Rust, Go and C,
//! `src/libcmp/`). They share the data generator and its oracle, the
//! memory and system probes and the report assets.

#![allow(clippy::new_without_default)]

pub mod adapter;
pub mod bench;
pub mod core;
pub mod dataset;
pub mod engines;
pub mod libcmp;
pub mod measure;
pub mod options;
pub mod readme;
pub mod registry;
pub mod report;
pub mod sys;
pub mod timer;

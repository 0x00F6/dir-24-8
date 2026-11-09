//! Libraries part of `bench-compare` (`PARTS=libraries`): IPv4 and IPv6
//! lookup comparison of dir-24-8 with seven libraries in their own
//! languages (Rust, Go, C): membership and longest-prefix lookups,
//! throughput, sampled latencies and memory per input prefix, with
//! JSON/CSV results shown in the libraries tab of the report. See
//! `benchmarks/compare-lib/README.md`.
//!
//! * [`corpus`]: generated corpora (language-neutral files) and edge cases;
//! * [`deps`]: Go toolchain and modules, libmaxminddb, native runners;
//! * [`runner`]: the Rust runner (prefix-trie, iprange, dir-24-8),
//!   `bench-compare runner --plan <file>`;
//! * [`orchestrate`]: processes, repetitions, limits;
//! * [`results`]: aggregation, JSON and CSV;
//! * [`readme`]: the generated summary of the repository `README.md`.

pub mod corpus;
pub mod deps;
pub mod libs;
pub mod orchestrate;
pub mod readme;
pub mod results;
pub mod runner;
pub mod settings;

//! `bench-compare-lib`: IPv4 and IPv6 lookup comparison of dir-24-8 with
//! seven libraries in their own languages (Rust, Go, C): membership and
//! longest-prefix lookups, throughput, sampled latencies and memory per
//! input prefix, with a standalone HTML report and JSON/CSV results. See
//! `benchmarks/compare-lib/README.md`.
//!
//! * [`corpus`]: generated corpora (language-neutral files) and edge cases;
//! * [`deps`]: Go toolchain and modules, libmaxminddb, native runners;
//! * [`runner`]: the Rust runner (prefix-trie, iprange, dir-24-8);
//! * [`orchestrate`]: processes, repetitions, limits;
//! * [`results`] and [`html`]: aggregation, JSON, CSV and HTML;
//! * [`readme`]: the generated summary of the repository `README.md`.

pub mod corpus;
pub mod deps;
pub mod html;
pub mod libs;
pub mod orchestrate;
pub mod readme;
pub mod results;
pub mod runner;
pub mod settings;

/// Entry point of the `bench-compare-lib` binary.
pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("runner") if args.len() == 4 && args[2] == "--plan" => {
            runner::main(std::path::Path::new(&args[3]))
        }
        Some("-h" | "--help") => print!("{}", settings::USAGE),
        None => {
            let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let repo = repo.canonicalize().unwrap_or(repo);
            let s = settings::Settings::from_env(&repo);
            if s.report_only {
                orchestrate::report_only(&s);
            } else {
                orchestrate::run(s);
            }
        }
        Some(a) => settings::die(&format!("unexpected argument {a:?} (see --help)")),
    }
}

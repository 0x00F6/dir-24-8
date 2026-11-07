//! Refreshes the performance summary of the repository `README.md` from a
//! `bench-compare-lib` result document: only the block between [`START`]
//! and [`END`] is regenerated, the rest of the file is left untouched.
//!
//! `BENCH_README` decides when a run writes it: `auto` (default) for
//! complete runs of any profile but `smoke`, `1` always, `0` never. The
//! same applies to `BENCH_REPORT_ONLY=1`, which refreshes the README from
//! the existing JSON without measuring.

use std::path::Path;

use serde_json::Value;

/// First line of the generated block.
pub const START: &str = "<!-- bench-compare-lib:summary:start -->";
/// Last line of the generated block.
pub const END: &str = "<!-- bench-compare-lib:summary:end -->";

/// Size of the summary when measured (1M input prefixes), else the largest.
const REFERENCE_SIZE: u64 = 1_000_000;

/// Whether `mode` (`BENCH_README`) lets this result document update the
/// README.
pub fn wanted(mode: &str, doc: &Value) -> bool {
    match mode {
        "1" => true,
        "0" => false,
        _ => doc["meta"]["status"] == "complete" && doc["meta"]["profile"] != "smoke",
    }
}

/// Replaces the generated block of `readme`. Returns `Ok(false)` when the
/// block was already up to date.
pub fn update(readme: &Path, doc: &Value) -> Result<bool, String> {
    replace_block(readme, START, END, &summary(doc)?)
}

/// Replaces the lines between `start` and `end` (both kept) of the file
/// `path` with `body` (which ends with a newline). Returns `Ok(false)` when
/// the file already had this content. Shared by the `bench-compare` and
/// `bench-compare-lib` README generators.
pub fn replace_block(path: &Path, start: &str, end: &str, body: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (Some(a), Some(b)) = (text.find(start), text.find(end)) else {
        return Err(format!("{} has no {start} ... {end} block", path.display()));
    };
    if b < a {
        return Err(format!("{end} precedes {start} in {}", path.display()));
    }
    let new = format!(
        "{}{start}\n{body}{end}{}",
        &text[..a],
        &text[b + end.len()..]
    );
    if new == text {
        return Ok(false);
    }
    // Written beside the file, then renamed over it.
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, &new).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
    Ok(true)
}

/// What a row measures and which value wins.
#[derive(Clone, Copy)]
enum Metric {
    /// Median aggregate lookups per second; higher is better.
    Throughput { op: &'static str, threads: u64 },
    /// p99 of individually timed lookups, one thread; lower is better.
    P99 { op: &'static str },
    /// Structure memory per input prefix; lower is better.
    BytesPerPrefix,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Metric::Throughput { .. } => "Throughput",
            Metric::P99 { .. } => "p99 Tail Latency",
            Metric::BytesPerPrefix => "Bytes per prefix",
        }
    }

    fn lower_is_better(self) -> bool {
        !matches!(self, Metric::Throughput { .. })
    }
}

/// The generated Markdown: a subtitle describing the run, then one table
/// with a column per library and a 🏆 on the best value of every row.
pub fn summary(doc: &Value) -> Result<String, String> {
    let meta = &doc["meta"];
    let settings = &meta["settings"];
    let libs = sorted_libs(doc);
    if libs.is_empty() {
        return Err("the result document lists no library".into());
    }
    let sizes: Vec<u64> = settings["sizes"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let size = if sizes.contains(&REFERENCE_SIZE) {
        REFERENCE_SIZE
    } else {
        sizes.iter().copied().max().ok_or("no measured size")?
    };
    let threads = settings["threads"]
        .as_array()
        .and_then(|v| v.iter().filter_map(Value::as_u64).max())
        .unwrap_or(1);
    let families: Vec<&str> = settings["families"]
        .as_array()
        .map(|v| v.iter().filter_map(Value::as_str).collect())
        .unwrap_or_else(|| vec!["ipv4", "ipv6"]);

    let mut out = String::new();
    out.push_str(&subtitle(doc, size));
    out.push_str("\n\n| Scenario | Metric |");
    for l in &libs {
        out.push_str(&format!(
            " {} `{}` |",
            lang_emoji(l["lang"].as_str().unwrap_or("")),
            l["label"].as_str().or(l["id"].as_str()).unwrap_or("?")
        ));
    }
    out.push_str("\n|:---|:---:|");
    out.push_str(&":---:|".repeat(libs.len()));
    out.push('\n');

    for fam in families {
        let f = if fam == "ipv6" { "IPv6" } else { "IPv4" };
        let mut rows = vec![(
            format!("{f} LPM lookup"),
            Metric::Throughput {
                op: "lookup",
                threads: 1,
            },
        )];
        if threads > 1 {
            rows.push((
                format!("{f} LPM lookup, {threads} threads"),
                Metric::Throughput {
                    op: "lookup",
                    threads,
                },
            ));
        }
        rows.push((format!("{f} LPM lookup"), Metric::P99 { op: "lookup" }));
        rows.push((
            format!("{f} membership"),
            Metric::Throughput {
                op: "contains",
                threads: 1,
            },
        ));
        rows.push((format!("{f} structure"), Metric::BytesPerPrefix));

        for (scenario, metric) in rows {
            let values: Vec<Option<f64>> = libs
                .iter()
                .map(|l| value(doc, l, fam, size, metric))
                .collect();
            let best = values
                .iter()
                .flatten()
                .copied()
                .fold(None, |acc, v| match acc {
                    None => Some(v),
                    Some(b) if metric.lower_is_better() => Some(f64::min(b, v)),
                    Some(b) => Some(f64::max(b, v)),
                });
            out.push_str(&format!("| {scenario} | {} |", metric.label()));
            for (l, v) in libs.iter().zip(&values) {
                let cell = match v {
                    None => "—".to_string(),
                    Some(v) => {
                        let mut text = format_value(*v, metric);
                        if Some(*v) == best {
                            text = format!("🏆 **{text}**");
                        }
                        let adapted = matches!(
                            metric,
                            Metric::Throughput {
                                op: "lookup",
                                threads: 1
                            }
                        ) && l["lookup"] == "adapted";
                        if adapted {
                            text.push_str(" (adapted)");
                        }
                        text
                    }
                };
                out.push_str(&format!(" {cell} |"));
            }
            out.push('\n');
        }
    }
    Ok(out)
}

/// Libraries in report order.
fn sorted_libs(doc: &Value) -> Vec<&Value> {
    let mut libs: Vec<&Value> = doc["libs"]
        .as_array()
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    libs.sort_by_key(|l| l["index"].as_u64().unwrap_or(u64::MAX));
    libs
}

fn lang_emoji(lang: &str) -> &'static str {
    match lang {
        "Rust" => "🦀",
        "Go" => "🐹",
        "C" => "🏛️",
        _ => "📦",
    }
}

/// The measured value of `lib` for a row, `None` when it is missing,
/// unsupported or failed.
fn value(doc: &Value, lib: &Value, fam: &str, size: u64, metric: Metric) -> Option<f64> {
    let id = lib["id"].as_str()?;
    let entry = |list: &str, op: Option<&str>, threads: Option<u64>| {
        doc[list].as_array()?.iter().find(|e| {
            e["lib"] == id
                && e["family"] == fam
                && e["status"] == "ok"
                && (e["size"].as_u64() == Some(size) || e["prefixes"].as_u64() == Some(size))
                && op.is_none_or(|op| e["op"] == op && e["scenario"] == "mixed")
                && threads.is_none_or(|t| e["threads"].as_u64() == Some(t))
        })
    };
    match metric {
        Metric::Throughput { op, threads } => {
            entry("throughput", Some(op), Some(threads))?["summary"]["median_lps"].as_f64()
        }
        Metric::P99 { op } => entry("latency", Some(op), Some(1))?["summary"]["p99"].as_f64(),
        Metric::BytesPerPrefix => {
            entry("memory", None, None)?["summary"]["bytes_per_prefix"].as_f64()
        }
    }
}

fn format_value(v: f64, metric: Metric) -> String {
    match metric {
        Metric::Throughput { .. } => format!("{:.1} M/s", v / 1e6),
        Metric::P99 { .. } => format!("{} ns", thousands(v.round() as u64)),
        Metric::BytesPerPrefix => format!("{} B", thousands(v.round() as u64)),
    }
}

/// `1052` -> `1 052`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn size_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 && n.is_multiple_of(1_000_000) => format!("{}M", n / 1_000_000),
        n if n >= 1_000 && n.is_multiple_of(1_000) => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// Run description as a bullet list: profile, date, CPU, status when not
/// complete, reference size, hit rate and repetitions.
fn subtitle(doc: &Value, size: u64) -> String {
    let meta = &doc["meta"];
    let profile = meta["profile"].as_str().unwrap_or("?");
    let date = meta["started_unix"]
        .as_u64()
        .map(utc_date)
        .unwrap_or_else(|| "?".into());
    let cpu = meta["env"]["cpu"].as_str().unwrap_or("this machine");
    let reps = meta["settings"]["repetitions"].as_u64().unwrap_or(1);
    let hits = doc["corpora"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["size"].as_u64() == Some(size))
        .flat_map(|c| c["files"].as_array().into_iter().flatten())
        .find(|f| f["name"] == "q-mixed.bin")
        .and_then(|f| f["hit_rate"].as_f64())
        .map_or_else(|| "?".to_string(), |r| format!("{:.0}", r * 100.0));
    let status = match meta["status"].as_str() {
        Some("complete") | None => String::new(),
        Some(s) => format!(
            "- **Status**: {s} ({})\n",
            meta["status_reason"].as_str().unwrap_or("")
        ),
    };
    format!(
        "- **Run**: `make bench-compare-lib`, {profile} profile, {date}, {cpu}\n\
         {status}\
         - **Workload**: {} input prefixes, mixed scenario ({hits} % hits), one thread unless noted\n\
         - **Statistic**: median of {reps} process{} per library\n\
         - **Every size and scenario**: `benchmarks/reports/bench-compare-lib.html`, \
         [benchmarks/compare-lib/README.md](benchmarks/compare-lib/README.md)\n\
         \n\
         <!-- Generated by `make bench-compare-lib` (BENCH_README): do not edit by hand. -->",
        size_label(size),
        if reps == 1 { "" } else { "es" }
    )
}

/// `YYYY-MM-DD` (UTC) of a Unix timestamp (days-to-civil conversion).
fn utc_date(unix: u64) -> String {
    let z = (unix / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(status: &str, profile: &str) -> Value {
        let tp = |lib: &str, op: &str, threads: u64, lps: f64| {
            json!({"lib": lib, "family": "ipv4", "op": op, "scenario": "mixed", "size": 1_000_000,
                   "threads": threads, "status": "ok", "summary": {"median_lps": lps}})
        };
        json!({
            "meta": {"status": status, "status_reason": "measurements: 3 ok", "profile": profile,
                     "started_unix": 1_794_063_228u64, "env": {"cpu": "Test CPU"},
                     "settings": {"sizes": [1_000, 1_000_000], "threads": [1, 8], "families": ["ipv4"], "repetitions": 5}},
            "libs": [
                {"id": "a", "label": "lib-a", "lang": "Rust", "index": 0, "lookup": "native"},
                {"id": "b", "label": "lib-b", "lang": "Go", "index": 1, "lookup": "adapted"},
            ],
            "corpora": [{"size": 1_000_000, "files": [{"name": "q-mixed.bin", "hit_rate": 0.5}]}],
            "throughput": [tp("a", "lookup", 1, 28.7e6), tp("b", "lookup", 1, 0.6e6),
                           tp("a", "lookup", 8, 149.9e6), tp("a", "contains", 1, 50e6), tp("b", "contains", 1, 61.9e6)],
            "latency": [
                {"lib": "a", "family": "ipv4", "op": "lookup", "scenario": "mixed", "size": 1_000_000, "threads": 1, "status": "ok", "summary": {"p99": 992.0}},
                {"lib": "b", "family": "ipv4", "op": "lookup", "scenario": "mixed", "size": 1_000_000, "threads": 1, "status": "ok", "summary": {"p99": 6733.0}},
            ],
            "memory": [
                {"lib": "a", "family": "ipv4", "prefixes": 1_000_000, "status": "ok", "summary": {"bytes_per_prefix": 160.3}},
                {"lib": "b", "family": "ipv4", "prefixes": 1_000_000, "status": "ok", "summary": {"bytes_per_prefix": 40.1}},
            ],
        })
    }

    #[test]
    fn summary_marks_the_best_value_of_every_row() {
        let md = summary(&doc("complete", "standard")).unwrap();
        assert!(md.starts_with(
            "- **Run**: `make bench-compare-lib`, standard profile, 2026-11-07, Test CPU\n\
             - **Workload**: 1M input prefixes, mixed scenario (50 % hits), one thread unless noted\n\
             - **Statistic**: median of 5 processes per library\n"
        ));
        assert!(md.contains("| Scenario | Metric | 🦀 `lib-a` | 🐹 `lib-b` |"));
        assert!(
            md.contains("| IPv4 LPM lookup | Throughput | 🏆 **28.7 M/s** | 0.6 M/s (adapted) |")
        );
        assert!(md.contains("| IPv4 LPM lookup, 8 threads | Throughput | 🏆 **149.9 M/s** | — |"));
        assert!(md.contains("| IPv4 LPM lookup | p99 Tail Latency | 🏆 **992 ns** | 6 733 ns |"));
        assert!(md.contains("| IPv4 membership | Throughput | 50.0 M/s | 🏆 **61.9 M/s** |"));
        assert!(md.contains("| IPv4 structure | Bytes per prefix | 160 B | 🏆 **40 B** |"));
        assert!(!md.contains("**Status**"));
    }

    #[test]
    fn update_replaces_only_the_block() {
        let dir = std::env::temp_dir().join(format!("readme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("README.md");
        std::fs::write(
            &path,
            format!("# Title\n\nbefore\n\n{START}\nold\n{END}\n\nafter\n"),
        )
        .unwrap();
        let d = doc("complete", "standard");
        assert!(update(&path, &d).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(
            "# Title\n\nbefore\n\n<!-- bench-compare-lib:summary:start -->\n- **Run**"
        ));
        assert!(text.ends_with(&format!("|\n{END}\n\nafter\n")));
        assert!(!text.contains("old"));
        assert!(!update(&path, &d).unwrap(), "unchanged on a second run");
        std::fs::write(&path, "no markers").unwrap();
        assert!(update(&path, &d).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_mode_skips_smoke_and_incomplete_runs() {
        assert!(wanted("auto", &doc("complete", "standard")));
        assert!(wanted("auto", &doc("complete", "full")));
        assert!(!wanted("auto", &doc("complete", "smoke")));
        assert!(!wanted("auto", &doc("partial", "standard")));
        assert!(wanted("1", &doc("partial", "smoke")));
        assert!(!wanted("0", &doc("complete", "standard")));
        let partial = summary(&doc("partial", "standard")).unwrap();
        assert!(partial.contains("- **Status**: partial (measurements: 3 ok)\n- **Workload**"));
    }

    #[test]
    fn helpers() {
        assert_eq!(thousands(1_052), "1 052");
        assert_eq!(thousands(15_390), "15 390");
        assert_eq!(thousands(992), "992");
        assert_eq!(size_label(1_000_000), "1M");
        assert_eq!(size_label(100_000), "100k");
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
    }
}

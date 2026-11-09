//! Aggregation of the runner records into the results document of the
//! libraries part (`libraries.json`) and its CSV export.
//!
//! Every planned measurement yields one entry with a status (`ok`,
//! `unsupported`, `skipped`, `failed`, `timeout`): a missing value stays
//! `null`, never zero. Repetitions are kept; summaries are medians with the
//! slowest and fastest repetition.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::corpus::CorpusInfo;
use super::libs::LibDef;
use super::orchestrate::{MmdbInfo, Proc};
use crate::dataset::Family;

const PCTS: [&str; 12] = [
    "min", "p1", "p5", "p25", "p50", "p75", "p90", "p95", "p99", "p99_9", "max", "mean",
];

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

fn min_max(v: &[f64]) -> (Option<f64>, Option<f64>) {
    (
        v.iter().copied().reduce(f64::min),
        v.iter().copied().reduce(f64::max),
    )
}

fn f(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// Status and reason of an item that has no valid repetition.
fn missing_status(procs: &[&Proc]) -> (String, String) {
    if procs.is_empty() {
        return ("skipped".into(), "not run".into());
    }
    for st in ["timeout", "failed", "skipped"] {
        if let Some(p) = procs.iter().find(|p| p.status == st) {
            return (st.into(), p.reason.clone().unwrap_or_default());
        }
    }
    (
        "failed".into(),
        "the runner produced no value for this measurement".into(),
    )
}

fn records<'a>(p: &'a Proc, ty: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    p.records.iter().filter(move |r| r["type"] == ty)
}

#[allow(clippy::too_many_arguments)]
fn item_entry(
    metric: &str,
    lib: &LibDef,
    family: Family,
    size: usize,
    op: &str,
    sc: &str,
    threads: usize,
    procs: &[&Proc],
) -> Value {
    let th = |r: &Value| r["threads"].as_u64().unwrap_or(1) as usize == threads;
    let mut reps = Vec::new();
    let mut problems = Vec::new();
    let mut skipped_validation = false;
    for p in procs {
        let clock = p
            .records
            .iter()
            .find(|r| r["type"] == "meta")
            .map(|m| m["clock"].clone())
            .unwrap_or(Value::Null);
        if records(p, "skip")
            .any(|r| r["op"] == op && r["scenario"] == sc && r["task"] == metric && th(r))
        {
            skipped_validation = true;
        }
        for r in records(p, metric).filter(|r| r["op"] == op && r["scenario"] == sc && th(r)) {
            let errors = r["errors"].as_u64().unwrap_or(0);
            let ok_sum = r["checksum"] == r["expected_checksum"];
            if errors > 0 {
                problems.push(format!("rep {}: {errors} lookup errors", p.rep + 1));
                continue;
            }
            if !ok_sum {
                problems.push(format!(
                    "rep {}: checksum differs from the oracle during the measurement",
                    p.rep + 1
                ));
                continue;
            }
            if metric == "throughput" {
                let lookups = r["lookups"].as_f64().unwrap_or(0.0);
                let wall = r["wall_ns"].as_f64().unwrap_or(f64::NAN);
                reps.push(
                    json!({"rep": p.rep + 1, "order": p.order, "lookups": lookups, "wall_ns": wall,
                    "lps": lookups / wall * 1e9, "ns_per_op": wall / lookups}),
                );
            } else {
                let over = clock["overhead_ns"].as_f64().unwrap_or(0.0);
                let mut corrected = Map::new();
                for k in PCTS {
                    let v = &r["raw_ns"][k];
                    corrected.insert(
                        k.into(),
                        v.as_f64()
                            .map_or(Value::Null, |x| json!((x - over).max(0.0))),
                    );
                }
                reps.push(json!({"rep": p.rep + 1, "order": p.order, "samples": r["samples"], "overhead_ns": over,
                    "resolution_ns": clock["resolution_ns"], "clock": clock["source"], "raw_ns": r["raw_ns"], "ns": corrected}));
            }
        }
    }
    let base = json!({"metric": metric, "lib": lib.id, "family": family, "size": size, "op": op, "scenario": sc,
        "threads": threads});
    let mut e = base.as_object().unwrap().clone();
    let (status, reason) = if skipped_validation {
        (
            "failed".to_string(),
            "validation failed: results differ from the oracle (measurement not run)".to_string(),
        )
    } else if !problems.is_empty() {
        ("failed".to_string(), problems.join("; "))
    } else if !reps.is_empty() {
        let n = procs
            .iter()
            .filter(|p| p.status != "skipped")
            .count()
            .max(reps.len());
        (
            "ok".to_string(),
            if reps.len() < procs.len() {
                format!("{} of {} repetitions", reps.len(), procs.len().max(n))
            } else {
                String::new()
            },
        )
    } else {
        missing_status(procs)
    };
    e.insert("status".into(), json!(status));
    e.insert("reason".into(), json!(reason));
    if status == "ok" {
        if metric == "throughput" {
            let v: Vec<f64> = reps.iter().filter_map(|r| f(&r["lps"])).collect();
            let (lo, hi) = min_max(&v);
            let m = median(v.clone());
            e.insert(
                "summary".into(),
                json!({"reps": v.len(), "median_lps": m, "min_lps": lo, "max_lps": hi,
                "ns_per_op": m.map(|m| 1e9 / m), "spread": m.and_then(|m| Some((hi? - lo?) / m))}),
            );
        } else {
            let mut s = Map::new();
            for k in PCTS {
                s.insert(
                    k.into(),
                    json!(median(reps.iter().filter_map(|r| f(&r["ns"][k])).collect())),
                );
            }
            let p50s: Vec<f64> = reps.iter().filter_map(|r| f(&r["ns"]["p50"])).collect();
            let (lo, hi) = min_max(&p50s);
            let over =
                median(reps.iter().filter_map(|r| f(&r["overhead_ns"])).collect()).unwrap_or(0.0);
            let res = reps
                .iter()
                .filter_map(|r| f(&r["resolution_ns"]))
                .fold(0.0, f64::max);
            let p50 = s["p50"].as_f64().unwrap_or(0.0);
            s.insert("p50_min".into(), json!(lo));
            s.insert("p50_max".into(), json!(hi));
            s.insert("overhead_ns".into(), json!(over));
            s.insert("resolution_ns".into(), json!(res));
            s.insert("reps".into(), json!(reps.len()));
            s.insert("samples_per_rep".into(), reps[0]["samples"].clone());
            s.insert(
                "precision_limited".into(),
                json!(p50 < 3.0 * res || over >= p50),
            );
            e.insert("summary".into(), Value::Object(s));
        }
    }
    e.insert("reps".into(), json!(reps));
    Value::Object(e)
}

fn memory_entry(
    lib: &LibDef,
    family: Family,
    size: usize,
    prefixes: usize,
    procs: &[&Proc],
) -> Value {
    let mut reps = Vec::new();
    for p in procs {
        let Some(b) = records(p, "build").next() else {
            continue;
        };
        let warm = records(p, "memory").next();
        let end = records(p, "end").next();
        let rss0 = f(&b["rss_before"]);
        let rss1 = f(&b["rss_after"]);
        let rssw = warm.and_then(|w| f(&w["rss"]));
        let after = rssw.or(rss1);
        reps.push(json!({"rep": p.rep + 1, "build_s": f(&b["build_ns"]).map(|n| n / 1e9),
            "open_s": f(&b["open_ns"]).map(|n| n / 1e9), "rss_before": rss0, "rss_after_build": rss1, "rss_after_measure": rssw,
            "struct_rss_build": rss0.zip(rss1).map(|(a, b)| b - a), "struct_rss": rss0.zip(after).map(|(a, b)| b - a),
            "struct_estimate": f(&b["structure_bytes"]), "struct_method": b["structure_method"],
            "hwm": end.and_then(|e| f(&e["hwm"])), "units": b["units"], "units_name": b["units_name"],
            "mmdb_resident": warm.and_then(|w| f(&w["mmdb_resident"])), "mmdb": b["mmdb"]}));
    }
    let mut e = json!({"metric": "memory", "lib": lib.id, "family": family, "size": size, "prefixes": prefixes});
    if reps.is_empty() {
        let (st, why) = missing_status(procs);
        e["status"] = json!(st);
        e["reason"] = json!(why);
    } else {
        e["status"] = json!("ok");
        e["reason"] = json!("");
        let med = |k: &str| median(reps.iter().filter_map(|r| f(&r[k])).collect());
        let struct_rss = med("struct_rss");
        e["summary"] = json!({"reps": reps.len(), "build_s": med("build_s"), "open_s": med("open_s"),
            "struct_rss": struct_rss, "struct_rss_build": med("struct_rss_build"), "struct_estimate": med("struct_estimate"),
            "bytes_per_prefix": struct_rss.map(|v| v / prefixes.max(1) as f64),
            "estimate_per_prefix": med("struct_estimate").map(|v| v / prefixes.max(1) as f64),
            "rss_after_measure": med("rss_after_measure"), "hwm": med("hwm"), "units": reps[0]["units"], "units_name": reps[0]["units_name"],
            "struct_method": reps[0]["struct_method"], "mmdb_resident": med("mmdb_resident"), "mmdb": reps[0]["mmdb"]});
    }
    e["reps"] = json!(reps);
    e
}

fn validation_entries(lib: &LibDef, family: Family, size: usize, procs: &[&Proc]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in procs {
        for r in records(p, "validation") {
            out.push(json!({"lib": lib.id, "family": family, "size": size, "kind": p.kind, "rep": p.rep + 1,
                "scenario": r["scenario"], "op": r["op"], "checked": r["checked"], "mismatches": r["mismatches"],
                "errors": r["errors"], "examples": r["examples"],
                "ok": r["mismatches"].as_u64() == Some(0) && r["errors"].as_u64() == Some(0)}));
        }
    }
    out
}

/// Display name of a dir-24-8 backend.
pub fn isa_name(backend: &str) -> &str {
    match backend {
        "avx512" => "AVX-512",
        "avx2" => "AVX2",
        "neon" => "NEON",
        other => other,
    }
}

fn lib_entry(lib: &LibDef, index: usize, procs: &[Proc]) -> Value {
    let meta = procs
        .iter()
        .filter(|p| p.lib == lib.id)
        .find_map(|p| p.records.iter().find(|r| r["type"] == "meta").cloned())
        .unwrap_or(Value::Null);
    let single = meta["impl"]["single_backend"].as_str().map(str::to_string);
    let label = match (lib.package, single.as_deref()) {
        ("dir-24-8", Some(b)) => format!("dir-24-8 ({})", isa_name(b)),
        _ => lib.id.to_string(),
    };
    let version = match lib.package {
        "dir-24-8" | "prefix-trie" => Value::Null,
        pkg => meta["modules"]
            .as_array()
            .and_then(|m| m.iter().find(|d| d["path"] == pkg))
            .map_or(meta["impl"]["library_version"].clone(), |d| {
                d["version"].clone()
            }),
    };
    json!({"id": lib.id, "index": index, "lang": lib.lang, "repo": lib.repo, "runner": lib.runner, "package": lib.package,
        "contains": lib.contains, "lookup": lib.lookup, "contains_api": lib.contains_api, "lookup_api": lib.lookup_api,
        "reference": lib.reference, "variant_of": lib.variant_of, "label": label, "single_backend": single,
        "notes": lib.notes, "version": version,
        "implementation": meta["impl"], "build": meta["build"], "runtime": meta["runtime"], "clock": meta["clock"]})
}

pub fn build(
    meta: Value,
    libs: &[&'static LibDef],
    corpora: &[CorpusInfo],
    edges: &[CorpusInfo],
    mmdbs: &[MmdbInfo],
    procs: &[Proc],
) -> Value {
    let mut throughput = Vec::new();
    let mut latency = Vec::new();
    let mut memory = Vec::new();
    let mut validation = Vec::new();
    for c in corpora {
        // Scenarios planned for this corpus (from any library).
        let mut planned_sc: Vec<(String, String, usize)> = procs
            .iter()
            .filter(|p| p.kind == "main" && p.corpus == c.id)
            .flat_map(|p| p.planned.iter().map(|x| (x.0.clone(), x.2.clone(), x.3)))
            .collect();
        planned_sc.sort();
        planned_sc.dedup();
        for lib in libs {
            let group: Vec<&Proc> = procs
                .iter()
                .filter(|p| p.kind == "main" && p.corpus == c.id && p.lib == lib.id)
                .collect();
            let mut items: Vec<(String, String, String, usize)> =
                group.iter().flat_map(|p| p.planned.clone()).collect();
            items.sort();
            items.dedup();
            for (metric, op, sc, th) in &items {
                let e = item_entry(metric, lib, c.family, c.size, op, sc, *th, &group);
                if metric == "throughput" {
                    throughput.push(e)
                } else {
                    latency.push(e)
                }
            }
            if !lib.supports_lookup() {
                for (metric, sc, th) in &planned_sc {
                    let metric = metric.as_str();
                    {
                        let e = json!({"metric": metric, "lib": lib.id, "family": c.family, "size": c.size, "op": "lookup",
                            "scenario": sc, "threads": th, "status": "unsupported", "reason": "N/A - set without associated values", "reps": []});
                        if metric == "throughput" {
                            throughput.push(e)
                        } else {
                            latency.push(e)
                        }
                    }
                }
            }
            memory.push(memory_entry(lib, c.family, c.size, c.prefixes, &group));
            validation.extend(validation_entries(lib, c.family, c.size, &group));
        }
    }
    let mut edge = Vec::new();
    for c in edges {
        for lib in libs {
            let Some(p) = procs
                .iter()
                .find(|p| p.kind == c.kind && p.corpus == c.id && p.lib == lib.id)
            else {
                continue;
            };
            let v = validation_entries(lib, c.family, c.size, &[p]);
            let case = c.kind.trim_start_matches("edge:");
            let ok = p.status == "ok" && !v.is_empty() && v.iter().all(|x| x["ok"] == true);
            edge.push(json!({"lib": lib.id, "family": c.family, "case": case, "status": if ok { "ok" } else if p.status == "ok" { "failed" } else { p.status.as_str() },
                "reason": if ok { String::new() } else if p.status == "ok" { "results differ from the linear-scan oracle".to_string() } else { p.reason.clone().unwrap_or_default() },
                "checks": v}));
        }
    }
    let lib_entries: Vec<Value> = libs
        .iter()
        .enumerate()
        .map(|(i, l)| lib_entry(l, i, procs))
        .collect();
    let processes: Vec<Value> = procs
        .iter()
        .map(|p| json!({"lib": p.lib, "family": p.family, "size": p.size, "rep": p.rep + 1, "kind": p.kind, "status": p.status,
            "reason": p.reason, "wall_s": p.wall_s, "exit": p.exit, "order": p.order, "raw": p.raw, "stderr_tail": p.stderr_tail,
            "watch_peak_rss": p.watch_peak_rss}))
        .collect();
    json!({"schema": 1, "meta": meta, "libs": lib_entries, "corpora": corpora, "edge_corpora": edges, "mmdb": mmdbs,
        "throughput": throughput, "latency": latency, "memory": memory, "validation": validation, "edge": edge,
        "processes": processes})
}

/// Overall status of the run.
pub fn finish(doc: &mut Value) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for k in ["throughput", "latency", "memory"] {
        for e in doc[k].as_array().into_iter().flatten() {
            *counts
                .entry(e["status"].as_str().unwrap_or("?").to_string())
                .or_default() += 1;
        }
    }
    let edge_bad = doc["edge"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["status"] != "ok" && e["case"] != "duplicates")
        .count();
    let dep_errors = doc["meta"]["deps"]["errors"].as_array().map_or(0, Vec::len);
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    for p in doc["processes"].as_array().into_iter().flatten() {
        if p["status"] != "ok" {
            let why = p["reason"]
                .as_str()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("")
                .to_string();
            *missing
                .entry(format!("{} ({why})", p["status"].as_str().unwrap_or("?")))
                .or_default() += 1;
        }
    }
    let lost: usize = missing.values().sum();
    let ok = counts.get("ok").copied().unwrap_or(0);
    let bad: usize = counts
        .iter()
        .filter(|(k, _)| k.as_str() != "ok" && k.as_str() != "unsupported")
        .map(|(_, v)| *v)
        .sum();
    let status = if ok == 0 {
        "failed"
    } else if bad == 0 && edge_bad == 0 && dep_errors == 0 && lost == 0 {
        "complete"
    } else {
        "partial"
    };
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
    let mut reason = format!("measurements: {}", summary.join(", "));
    if edge_bad > 0 {
        reason += &format!("; {edge_bad} edge validations failed");
    }
    if dep_errors > 0 {
        reason += &format!("; {dep_errors} dependency errors");
    }
    if lost > 0 {
        let parts: Vec<String> = missing.iter().map(|(k, v)| format!("{v} {k}")).collect();
        reason += &format!(
            "; runner processes not completed (fewer repetitions): {}",
            parts.join(", ")
        );
    }
    doc["meta"]["status"] = json!(status);
    doc["meta"]["status_reason"] = json!(reason);
    doc["meta"]["counts"] = json!(counts);
}

fn csv_field(v: &Value) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    };
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

pub const CSV_HEADER: &[&str] = &[
    "metric",
    "lib",
    "label",
    "threads",
    "family",
    "size",
    "op",
    "scenario",
    "status",
    "reason",
    "reps",
    "median_mlookups_s",
    "min_mlookups_s",
    "max_mlookups_s",
    "ns_per_op_amortized",
    "lat_p1_ns",
    "lat_p25_ns",
    "lat_p50_ns",
    "lat_p75_ns",
    "lat_p90_ns",
    "lat_p95_ns",
    "lat_p99_ns",
    "lat_p99_9_ns",
    "clock_overhead_ns",
    "clock_resolution_ns",
    "precision_limited",
    "prefixes",
    "build_s",
    "open_s",
    "struct_rss_bytes",
    "bytes_per_prefix",
    "struct_estimate_bytes",
    "peak_rss_bytes",
    "units",
    "units_name",
];

pub fn to_csv(doc: &Value) -> String {
    let mut out = CSV_HEADER.join(",") + "\n";
    let m = |v: &Value| v.as_f64().map_or(Value::Null, |x| json!(x / 1e6));
    let labels: std::collections::HashMap<String, Value> = doc["libs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            (
                l["id"].as_str().unwrap_or("").to_string(),
                l["label"].clone(),
            )
        })
        .collect();
    for k in ["throughput", "latency", "memory"] {
        for e in doc[k].as_array().into_iter().flatten() {
            let s = &e["summary"];
            let row: Vec<Value> = vec![
                e["metric"].clone(),
                e["lib"].clone(),
                labels
                    .get(e["lib"].as_str().unwrap_or(""))
                    .cloned()
                    .unwrap_or(Value::Null),
                if e["threads"].is_null() {
                    json!(1)
                } else {
                    e["threads"].clone()
                },
                e["family"].clone(),
                e["size"].clone(),
                e["op"].clone(),
                e["scenario"].clone(),
                e["status"].clone(),
                e["reason"].clone(),
                s["reps"].clone(),
                m(&s["median_lps"]),
                m(&s["min_lps"]),
                m(&s["max_lps"]),
                s["ns_per_op"].clone(),
                s["p1"].clone(),
                s["p25"].clone(),
                s["p50"].clone(),
                s["p75"].clone(),
                s["p90"].clone(),
                s["p95"].clone(),
                s["p99"].clone(),
                s["p99_9"].clone(),
                s["overhead_ns"].clone(),
                s["resolution_ns"].clone(),
                s["precision_limited"].clone(),
                e["prefixes"].clone(),
                s["build_s"].clone(),
                s["open_s"].clone(),
                s["struct_rss"].clone(),
                s["bytes_per_prefix"].clone(),
                s["struct_estimate"].clone(),
                s["hwm"].clone(),
                s["units"].clone(),
                s["units_name"].clone(),
            ];
            out += &row.iter().map(csv_field).collect::<Vec<_>>().join(",");
            out.push('\n');
        }
    }
    out
}

/// Result document of the libraries part, next to the report.
pub const JSON: &str = "libraries.json";
/// One row per measurement of the libraries part.
pub const CSV: &str = "libraries.csv";

/// Writes `libraries.json` and `libraries.csv`.
pub fn write_all(out: &Path, doc: &Value) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out)?;
    let json_path = out.join(JSON);
    let tmp = out.join(format!("{JSON}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(doc)?)?;
    std::fs::rename(&tmp, &json_path)?;
    Ok(vec![json_path, write_csv(out, doc)?])
}

/// Writes `libraries.csv`.
pub fn write_csv(out: &Path, doc: &Value) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let csv_path = out.join(CSV);
    std::fs::write(&csv_path, to_csv(doc))?;
    Ok(csv_path)
}

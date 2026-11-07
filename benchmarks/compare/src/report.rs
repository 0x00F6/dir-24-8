//! Results model and report writers: `results.json` (everything, the input
//! of `--report-only`), `results.csv` (one row per measurement) and
//! `report.html` (standalone page: the data is embedded, the charts are
//! drawn by the embedded script).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bench::{Op, RunResult};
use crate::dataset::{DatasetMeta, Dist, Family};
use crate::measure::Env;
use crate::options::Options;
use crate::registry::EngineInfo;
use crate::timer;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub sizes: Vec<usize>,
    pub families: Vec<Family>,
    pub ops: Vec<Op>,
    pub dists: Vec<Dist>,
    pub threads: Vec<usize>,
    pub batch: Vec<usize>,
    /// Extra batch sizes of the native batch APIs (first thread count).
    #[serde(default)]
    pub batch_sweep: Vec<usize>,
    pub engines: Vec<String>,
    pub hit_permille: u32,
    pub queries: usize,
    pub seed: u64,
    pub warmup_ms: u64,
    pub window_ms: u64,
    pub reps: usize,
    pub lat_samples: usize,
    pub lat_batches: usize,
    #[serde(default)]
    pub lat_max_ms: u64,
    #[serde(default)]
    pub validate: usize,
    pub mem_limit_bytes: usize,
    pub mem_limit_rule: String,
    pub budget_s: u64,
    pub timeout_s: u64,
    pub pin: bool,
    pub cpu_order: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub tool: String,
    pub command: String,
    pub duration_s: f64,
    /// Time spent generating or loading datasets in the orchestrator.
    pub datasets_s: f64,
    /// `false` while the run is in progress (partial results).
    pub complete: bool,
    /// 1-minute load average at the start and at the end of the run.
    #[serde(default)]
    pub load_start: Option<f64>,
    #[serde(default)]
    pub load_end: Option<f64>,
    pub timer_source: String,
    pub ticks_per_s: f64,
    /// Effective resolution of a fenced TSC timestamp (why the TSC is not
    /// used for latency when the cycle counter is available).
    #[serde(default)]
    pub tsc_granularity_ns: f64,
    pub env: Env,
    pub settings: Settings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Results {
    pub meta: Meta,
    pub engines: Vec<EngineInfo>,
    pub datasets: Vec<DatasetMeta>,
    pub runs: Vec<RunResult>,
}

impl Results {
    pub fn new(
        o: &Options,
        env: Env,
        ticks_per_s: f64,
        mem_limit: usize,
        mem_rule: String,
        cpus: &[usize],
    ) -> Self {
        Results {
            meta: Meta {
                tool: format!("bench-compare {}", env!("CARGO_PKG_VERSION")),
                command: std::env::args().collect::<Vec<_>>().join(" "),
                duration_s: 0.0,
                datasets_s: 0.0,
                complete: false,
                load_start: crate::sys::loadavg(),
                load_end: None,
                timer_source: timer::source(),
                tsc_granularity_ns: timer::tsc_granularity() as f64 * 1e9 / ticks_per_s,
                ticks_per_s,
                env,
                settings: Settings {
                    sizes: o.sizes.clone(),
                    families: o.families.clone(),
                    ops: o.ops.clone(),
                    dists: o.dists.clone(),
                    threads: o.threads.clone(),
                    batch: o.batch.clone(),
                    batch_sweep: o.batch_sweep.clone(),
                    engines: o.engines.clone(),
                    hit_permille: o.hit_permille,
                    queries: o.queries,
                    seed: o.seed,
                    warmup_ms: o.warmup_ms,
                    window_ms: o.window_ms,
                    reps: o.reps,
                    lat_samples: o.lat_samples,
                    lat_batches: o.lat_batches,
                    lat_max_ms: o.lat_max_ms,
                    validate: o.validate,
                    mem_limit_bytes: mem_limit,
                    mem_limit_rule: mem_rule,
                    budget_s: o.budget_s,
                    timeout_s: o.timeout_s,
                    pin: o.pin,
                    cpu_order: if o.pin { cpus.to_vec() } else { Vec::new() },
                },
            },
            engines: Vec::new(),
            datasets: Vec::new(),
            runs: Vec::new(),
        }
    }
}

pub fn write_json(out: &Path, r: &Results) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let p = out.join("results.json");
    let tmp = out.join("results.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Writes every report; `json` also rewrites `results.json`.
pub fn write_all(out: &Path, r: &Results, json: bool) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out)?;
    let mut files = Vec::new();
    if json {
        files.push(write_json(out, r)?);
    }
    let csv = out.join("results.csv");
    std::fs::write(&csv, to_csv(r))?;
    files.push(csv);
    let html = out.join("report.html");
    std::fs::write(&html, to_html(r))?;
    files.push(html);
    Ok(files)
}

fn esc(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

pub fn to_csv(r: &Results) -> String {
    let mut s = String::from(
        "family,size,entries,engine,status,reason,kernels,op,dist,threads,batch,\
         throughput_ips_median,throughput_ips_min,throughput_ips_max,reps,addrs_per_rep,\
         amortized_ns_per_addr,latency_kind,latency_samples,latency_min_ns,latency_p50_ns,\
         latency_p90_ns,latency_p95_ns,latency_p99_ns,latency_max_ns,latency_mean_ns,\
         build_s,footprint_bytes,bytes_per_entry,struct_heap_bytes,struct_mapped_bytes,\
         entry_table_bytes,rss_delta_bytes,build_peak_heap_bytes,build_peak_rss_bytes,\
         lookup_peak_rss_bytes,lookup_peak_heap_bytes\n",
    );
    for run in &r.runs {
        let head = format!(
            "{},{},{},{},{},{},{}",
            run.family.name(),
            run.size,
            run.entries,
            esc(&run.engine),
            format!("{:?}", run.status).to_lowercase(),
            esc(run.reason.as_deref().unwrap_or("")),
            esc(run.kernels.as_deref().unwrap_or(""))
        );
        let m = run.memory.as_ref();
        let tail = format!(
            "{},{},{},{},{},{},{},{},{},{},{}",
            opt(run.build_s),
            opt(m.map(|m| m.footprint)),
            opt(m.map(|m| format!("{:.2}", m.bytes_per_entry))),
            opt(m.map(|m| m.struct_heap)),
            opt(m.map(|m| m.struct_mapped)),
            opt(m.map(|m| m.entry_table)),
            opt(m.and_then(|m| m.rss_delta)),
            opt(m.map(|m| m.build_peak_heap)),
            opt(m.and_then(|m| m.build_peak_rss)),
            opt(m.and_then(|m| m.lookup_peak_rss)),
            opt(m.map(|m| m.lookup_peak_heap)),
        );
        if run.measures.is_empty() {
            let _ = writeln!(s, "{head},,,,,,,,,,,,,,,,,,,,{tail}");
        }
        for x in &run.measures {
            let l = &x.latency;
            let _ = writeln!(
                s,
                "{head},{},{},{},{},{:.0},{:.0},{:.0},{},{},{:.3},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{tail}",
                x.op.name(),
                x.dist.name(),
                x.threads,
                opt(x.batch),
                x.throughput.median,
                x.throughput.min,
                x.throughput.max,
                x.throughput.reps,
                x.throughput.addrs_per_rep,
                x.amortized_ns,
                x.latency_kind,
                l.n,
                l.min,
                l.p50,
                l.p90,
                l.p95,
                l.p99,
                l.max,
                l.mean,
            );
        }
    }
    s
}

const TEMPLATE: &str = include_str!("../assets/report.html");

/// Report assets: the dark theme stylesheet and the chart kit are shared
/// with the `bench-compare-lib` report.
pub const CSS: &str = include_str!("../assets/report.css");
pub const KIT_JS: &str = include_str!("../assets/kit.js");
const JS: &str = include_str!("../assets/report.js");

/// Inlines the `<link rel="stylesheet" href="X">` and `<script src="X">`
/// references of `template` (which can be opened as is next to its assets)
/// and the JSON `data`, so that the report is a single offline file.
pub fn standalone(template: &str, data: &str, css: &[(&str, &str)], js: &[(&str, &str)]) -> String {
    // `</` cannot appear inside the embedded JSON script element.
    let mut out = template.replacen("__DATA__", &data.replace("</", "<\\/"), 1);
    for (href, body) in css {
        let link = format!(r#"<link rel="stylesheet" href="{href}">"#);
        assert!(out.contains(&link), "template references {href}");
        out = out.replace(&link, &format!("<style>\n{body}</style>"));
    }
    for (src, body) in js {
        let tag = format!(r#"<script src="{src}"></script>"#);
        assert!(out.contains(&tag), "template references {src}");
        out = out.replace(&tag, &format!("<script>\n{body}</script>"));
    }
    out
}

pub fn to_html(r: &Results) -> String {
    let data = serde_json::to_string(r).expect("serializable");
    standalone(
        TEMPLATE,
        &data,
        &[("report.css", CSS)],
        &[("kit.js", KIT_JS), ("report.js", JS)],
    )
}

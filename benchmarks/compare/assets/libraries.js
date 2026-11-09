"use strict";
// Libraries tab of report.html (libraries.json): its element ids carry the
// "libraries-" prefix.
(() => {
  const $ = (id) => document.getElementById("libraries-" + id);
  const root = document.getElementById("libraries");
  let R = null;
  try {
    R = JSON.parse($("data").textContent);
  } catch {
    // The template opened directly: no results embedded.
  }
  if (!R) {
    root.dataset.status = "none";
    root.dataset.summary = "not measured";
    $("subtitle").textContent =
      "No results for this tab: run make bench-compare (PARTS=libraries measures this tab only), or make bench-compare REPORT_ONLY=1 to redraw the report from benchmarks/reports/libraries.json.";
    for (const id of ["toc", "filters", "main", "footer"]) $(id).hidden = true;
    return;
  }
  const S = R.meta.settings;
  root.dataset.status = { complete: "ok", partial: "warn" }[R.meta.status] || "bad";
  root.dataset.summary = `${R.meta.status} · ${new Date(R.meta.started_unix * 1000).toISOString().slice(0, 10)} · ${R.meta.env.cpu}`;

  // ------------------------------------------------------------ identity
  const LIBS = R.libs.slice().sort((a, b) => a.index - b.index);
  const libBy = new Map(LIBS.map((l) => [l.id, l]));
  // One color per library; a variant (`variant_of`, none at the moment) shares its
  // library's color and is drawn dashed / hollow.
  const BASES = LIBS.filter((l) => !l.variant_of).map((l) => l.id);
  const baseOf = (id) => libBy.get(id)?.variant_of || id;
  const colorOf = (id) => `var(--s${(Math.max(0, BASES.indexOf(baseOf(id))) % 8) + 1})`;
  const dashed = (id) => !!libBy.get(id)?.variant_of;
  // Display name: the library and its language, e.g. "bart (Go)" or
  // "dir-24-8 (Rust, AVX2)" for a label that already names a variant.
  const label = (id) => {
    const l = libBy.get(id);
    if (!l) return id;
    const name = l.label || id;
    const m = /^(.*) \((.*)\)$/.exec(name);
    return m ? `${m[1]} (${l.lang}, ${m[2]})` : `${name} (${l.lang})`;
  };
  /** SVG style of a filled mark (bar, candle body): variants are hollow and dashed. */
  const fillStyle = (id, opacity) =>
    dashed(id)
      ? `fill:${colorOf(id)};fill-opacity:.18;stroke:${colorOf(id)};stroke-width:1.5;stroke-dasharray:4 2`
      : `fill:${colorOf(id)};fill-opacity:${opacity};stroke:${colorOf(id)};stroke-width:1.5`;
  const thLabel = (t) => (t === 1 ? "1 thread" : `${t} threads`);
  const LAT_THREADS = "1 thread (individual calls)";
  const STATUS = {
    ok: ["ok", "st-ok"],
    unsupported: ["not applicable", "st-skip"],
    skipped: ["skipped", "st-time"],
    failed: ["failed", "st-bad"],
    timeout: ["timeout", "st-time"],
  };
  const K = Kit({ colorOf, dashed, label, status: () => STATUS, onChange: () => renderAll() });
  const {
    sizeLabel, fmtNs, fmtRate, fmtBytes, fmtS, fmtBpe, tickRate, tickNs, tickBytes, pct, el, sv, clear,
    showTip, hideTip, ttTitle, ttRow, ttGrid, scale, niceTicks, logTicks, valueDomain, segGroup, swatch,
    chartWidth, cardHead, appendMissing, lineChart, table, kvTable,
  } = K;

  const FAMILY = { ipv4: "IPv4", ipv6: "IPv6" };
  const OP = { contains: "Membership: contains(ip) -> bool", lookup: "LPM lookup: lookup(ip) -> record id" };
  const OP_SHORT = { contains: "membership", lookup: "LPM lookup" };
  const SCEN = {
    mixed: "Mixed (50 % hits)",
    hits: "100 % hits",
    misses: "100 % misses",
    local: "Locality (90 % from 1 024 hot addresses)",
  };
  const fmtLps = (v) => fmtRate(v).replace("IP/s", "lookups/s");
  const k = (...a) => a.join("|");
  const TP = new Map(R.throughput.map((e) => [k(e.lib, e.family, e.size, e.op, e.scenario, e.threads ?? 1), e]));
  const THREAD_LIST = [...new Set(R.throughput.map((e) => e.threads ?? 1))].sort((a, b) => a - b);
  const LAT = new Map(R.latency.map((e) => [k(e.lib, e.family, e.size, e.op, e.scenario), e]));
  const MEM = new Map(R.memory.map((e) => [k(e.lib, e.family, e.size), e]));
  const sizes = S.sizes.slice().sort((a, b) => a - b);
  const scenarios = ["mixed", "hits", "misses", "local"].filter((s) => R.throughput.some((e) => e.scenario === s));
  const REF = LIBS.find((l) => l.reference)?.id;

  /** API tag of a library for an operation ("adapted LPM", ...). */
  const adaptedTag = (id, op) => {
    const l = libBy.get(id);
    if (!l) return null;
    if (op === "lookup" && l.lookup === "adapted") return "adapted LPM";
    if (op === "contains" && l.contains === "adapted") return "adapted membership";
    return null;
  };

  // ------------------------------------------------------------ state
  const detail = (S.detail_sizes || []).filter((s) => sizes.includes(s));
  const state = {
    family: S.families[0],
    op: "lookup",
    size: detail.length ? Math.max(...detail) : sizes.filter((s) => s <= 1e5).slice(-1)[0] ?? sizes[0],
    scenario: "mixed",
    threads: 1,
    ylog: false,
    mem: "struct_rss",
    hidden: new Set(),
  };
  const visible = () => LIBS.filter((l) => !state.hidden.has(l.id));
  const config = () =>
    `${FAMILY[state.family]} · ${sizeLabel(state.size)} prefixes · ${OP_SHORT[state.op]} · ${SCEN[state.scenario]} · ${thLabel(state.threads)}`;
  const latConfig = () =>
    `${FAMILY[state.family]} · ${sizeLabel(state.size)} prefixes · ${OP_SHORT[state.op]} · ${SCEN[state.scenario]} · ${LAT_THREADS}`;
  const ok = (e) => e && e.status === "ok" && e.summary;
  const reasonOf = (e, id, size) => {
    if (!e) return size != null && !S.detail_sizes.includes(size) && state.scenario !== "mixed"
      ? "scenario measured at the detail sizes only"
      : "not planned in this run";
    const st = STATUS[e.status]?.[0] ?? e.status;
    return e.reason ? `${st}: ${e.reason}` : st;
  };

  // ------------------------------------------------------------ header
  function fmtDuration(s) {
    s = Math.round(s || 0);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return (h ? h + " h " : "") + m + " min " + (s % 60) + " s";
  }
  function renderHeader() {
    const m = R.meta;
    const d = new Date(m.started_unix * 1000);
    $("subtitle").textContent =
      `${d.toISOString().replace("T", " ").slice(0, 16)} UTC · run ${m.run_id} · profile ${m.profile} · ` +
      `${fmtDuration(m.duration_s)} · ${LIBS.length} libraries · ${S.repetitions} repetitions`;
    const st = clear($("runstatus"));
    const commit = m.project?.commit ? m.project.commit.slice(0, 12) + (m.project.dirty ? " (uncommitted changes)" : "") : "unknown";
    st.append(
      el("span", { class: `badge-st ${m.status}` }, el("i"), document.createTextNode(m.status)),
      el("span", { text: m.status_reason }),
      el("span", { class: "fixed" }, document.createTextNode("project commit "), el("b", { text: commit }))
    );
    const e = m.env;
    const chips = clear($("env-chips"));
    const chip = (a, b) => el("span", { class: "chip" }, document.createTextNode(a + " "), el("b", { text: b }));
    chips.append(
      chip("CPU", `${e.cpu} (${e.arch})`),
      chip("cores", `${e.physical_cores ?? "?"} physical / ${e.logical_cpus} logical`),
      chip("RAM", fmtBytes(e.ram_total)),
      chip("OS", `${e.os}, kernel ${e.kernel}`),
      chip("threads", `throughput at ${THREAD_LIST.join(", ")} threads; latency and memory: 1 thread`),
      chip("seed", String(S.seed)),
      chip("clock", "CLOCK_MONOTONIC in every runner")
    );
  }

  // ------------------------------------------------------------ filters, legend
  function renderFilters() {
    const row = clear($("filters").querySelector(".filters-row"));
    const hasData = (sc) => R.throughput.some((e) => e.family === state.family && e.scenario === sc && e.size === state.size && e.status === "ok");
    row.append(
      segGroup("Family", S.families.map((f) => [f, FAMILY[f]]), () => state.family, (v) => (state.family = v)),
      segGroup("Operation", [["contains", "Membership"], ["lookup", "LPM lookup"]], () => state.op, (v) => (state.op = v)),
      segGroup("Prefixes", sizes.map((s) => [s, sizeLabel(s)]), () => state.size, (v) => (state.size = v)),
      segGroup("Scenario", scenarios.map((s) => [s, s === "local" ? "locality" : s]), () => state.scenario, (v) => {
        state.scenario = v;
        if (v !== "mixed") state.threads = 1;
      }, (sc) => !hasData(sc)),
      segGroup("Threads (throughput)", THREAD_LIST.map((t) => [t, String(t)]), () => state.threads, (v) => (state.threads = v),
        (t) => t > 1 && state.scenario !== "mixed"),
      segGroup("Value axis", [[false, "linear"], [true, "log"]], () => state.ylog, (v) => (state.ylog = v)),
      el("span", { class: "fixed" }, document.createTextNode("latency "), el("b", { text: "1 thread" }),
        document.createTextNode(" · mode "), el("b", { text: "one lookup per call" }))
    );
  }
  function renderLegend() {
    const lg = clear($("legend"));
    lg.append(el("span", { class: "fixed" }, document.createTextNode("Throughput at "), el("b", { text: THREAD_LIST.join(", ") + " threads" }),
      document.createTextNode(", latency and memory at "), el("b", { text: "1 thread" }), document.createTextNode(" · click to show or hide:")));
    for (const l of LIBS) {
      lg.append(
        el("button", {
          type: "button",
          class: "lchip",
          "aria-pressed": String(!state.hidden.has(l.id)),
          title: `Show or hide ${label(l.id)}`,
          onclick: () => {
            if (state.hidden.has(l.id)) state.hidden.delete(l.id);
            else state.hidden.add(l.id);
            renderAll();
          },
        }, swatch(l.id), document.createTextNode(label(l.id)))
      );
    }
  }

  // ------------------------------------------------------------ line charts over sizes
  function seriesOver(valueOf) {
    return visible().map((l) => {
      const pts = new Map();
      for (const s of sizes) {
        const v = valueOf(l.id, s);
        if (v != null && isFinite(v) && (!state.ylog || v > 0)) pts.set(s, v);
      }
      return { name: l.id, pts };
    });
  }
  const tpReason = (id, s) => reasonOf(TP.get(k(id, state.family, s, state.op, state.scenario, state.threads)), id, s);
  function renderSizes() {
    $("sizes-note").textContent =
      "Median aggregate lookups per second over the independent repetitions (one fresh process each), one thread, " +
      "for the selected family, operation and scenario. The mixed scenario is measured at every size; the other scenarios at the detail sizes only. Higher is better.";
    lineChart($("line-throughput"), {
      title: `Throughput - ${OP_SHORT[state.op]}`,
      sub: `${FAMILY[state.family]} · ${SCEN[state.scenario]} · ${thLabel(state.threads)} · median of the repetitions`,
      series: seriesOver((id, s) => {
        const e = TP.get(k(id, state.family, s, state.op, state.scenario, state.threads));
        return ok(e) ? e.summary.median_lps : null;
      }),
      xs: sizes, xlog: true, ylog: state.ylog, height: 300,
      yFmt: fmtLps, tick: tickRate, xFmt: sizeLabel,
      tipTitle: (s) => `${sizeLabel(s)} prefixes · ${FAMILY[state.family]} · ${OP_SHORT[state.op]}`,
      reason: tpReason,
    });
  }

  const MEMK = {
    struct_rss: ["Structure RSS", "resident memory of the structure: RSS after the measurements minus RSS before the build"],
    hwm: ["Peak RSS", "peak resident memory of the process (VmHWM: queries, input and structure included)"],
    struct_estimate: ["Allocator estimate", "heap delta of the build (Go, Rust) or MMDB file size (memory-mapped)"],
  };
  function renderMemory() {
    $("memory-note").textContent =
      "Measured in the same fresh processes, with one library loaded. The structure's memory is the resident memory it adds once built and used by every measured query (memory-mapped pages included) " +
      "(the query buffers and the input prefixes are allocated before and excluded; freed memory is returned to the system first: GC + FreeOSMemory in Go, malloc_trim in Rust and C). " +
      "Bytes per prefix divide it by the number of input prefixes, not by the nodes or merged ranges a library keeps (see the units column of the table). Lower is better; log scale.";
    const controls = segGroup("Measure", Object.entries(MEMK).map(([key, v]) => [key, v[0]]), () => state.mem, (v) => (state.mem = v));
    const memOf = (id, s) => MEM.get(k(id, state.family, s));
    const mreason = (id, s) => reasonOf(memOf(id, s), id, null);
    lineChart($("line-memory"), {
      title: { struct_rss: "Resident memory of the structure", hwm: "Peak resident memory of the process", struct_estimate: "Allocator estimate of the structure" }[state.mem],
      sub: `${FAMILY[state.family]} · ${MEMK[state.mem][1]} · single-thread phase · median of the repetitions`,
      controls,
      series: seriesOver((id, s) => {
        const e = memOf(id, s);
        return ok(e) && e.summary[state.mem] > 0 ? e.summary[state.mem] : null;
      }),
      xs: sizes, xlog: true, ylog: true, bytes: true, height: 280,
      yFmt: fmtBytes, tick: tickBytes, xFmt: sizeLabel, lowerIsBetter: true,
      tipTitle: (s) => `${sizeLabel(s)} prefixes · ${FAMILY[state.family]}`,
      reason: mreason,
    });
    lineChart($("line-bpp"), {
      title: "Bytes per input prefix",
      sub: `${FAMILY[state.family]} · resident memory of the structure / input prefixes`,
      series: seriesOver((id, s) => {
        const e = memOf(id, s);
        return ok(e) && e.summary.bytes_per_prefix > 0 ? e.summary.bytes_per_prefix : null;
      }),
      xs: sizes, xlog: true, ylog: true, height: 280,
      yFmt: fmtBpe, tick: (v) => fmtBpe(v), xFmt: sizeLabel, lowerIsBetter: true,
      tipTitle: (s) => `${sizeLabel(s)} prefixes · ${FAMILY[state.family]}`,
      reason: mreason,
    });
  }

  // ------------------------------------------------------------ bars
  function renderBars() {
    const host = $("bars-card");
    cardHead(host, "Throughput", config());
    $("bars-note").textContent =
      `Aggregate throughput of all worker threads (${thLabel(state.threads)} selected): median of the repetitions; the whisker spans the slowest and the fastest repetition. Ratios compare with ${REF ? label(REF) : "the reference"} on exactly the same family, operation, size and scenario. ` +
      "When a library's range overlaps the fastest one, it is marked as tied (≈): the difference is within the run-to-run dispersion.";
    const rows = visible()
      .map((l) => ({ id: l.id, e: TP.get(k(l.id, state.family, state.size, state.op, state.scenario, state.threads)) }))
      .filter((r) => ok(r.e))
      .sort((a, b) => b.e.summary.median_lps - a.e.summary.median_lps);
    const missing = visible()
      .filter((l) => !rows.some((r) => r.id === l.id))
      .map((l) => [l.id, tpReason(l.id, state.size)]);
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "No measurement for this configuration." }));
      appendMissing(host, missing);
      return;
    }
    const ref = rows.find((r) => r.id === REF);
    const best = rows[0].e.summary;
    const W = chartWidth(host);
    const labelW = Math.min(300, W * 0.3);
    const x0 = labelW;
    const x1 = W - 230;
    const rowH = 30;
    const H = rows.length * rowH + 30;
    const dom = valueDomain(rows.flatMap((r) => [r.e.summary.max_lps, state.ylog ? r.e.summary.min_lps : 0]), state.ylog, true);
    const xs = scale(dom[0], dom[1], x0, x1, state.ylog);
    const svg = sv("svg", { width: W, height: H, role: "img", "aria-label": "Throughput bars" });
    for (const t of state.ylog ? logTicks(dom[0], dom[1]) : niceTicks(dom[0], dom[1], 5)) {
      sv("line", { x1: xs(t), y1: 0, x2: xs(t), y2: H - 26, class: "grid" }, svg);
      sv("text", { x: xs(t), y: H - 10, "text-anchor": "middle", class: "tick", text: tickRate(t) }, svg);
    }
    rows.forEach((r, i) => {
      const T = r.e.summary;
      const y = i * rowH + rowH / 2;
      const c = colorOf(r.id);
      const tie = i > 0 && T.max_lps >= best.min_lps;
      const g = sv("g", {}, svg);
      sv("rect", { x: 0, y: y - rowH / 2, width: W, height: rowH, class: "rowbg" }, g);
      const tag = adaptedTag(r.id, state.op);
      sv("text", { x: labelW - 8, y: y + 4, "text-anchor": "end", class: "ink", text: `${tie ? "≈1" : "#" + (i + 1)}  ${label(r.id)}${tag ? " · " + tag : ""}` }, g);
      const xb = xs(dom[0]);
      const xv = xs(T.median_lps);
      sv("rect", { x: xb, y: y - 8, width: Math.max(2, xv - xb), height: 16, rx: 4, style: fillStyle(r.id, 1) }, g);
      sv("line", { x1: xs(T.min_lps), y1: y, x2: xs(T.max_lps), y2: y, style: "stroke:var(--text-primary);stroke-width:1.5;stroke-opacity:.75" }, g);
      const ratio = ref ? T.median_lps / ref.e.summary.median_lps : null;
      sv("text", {
        x: Math.max(xv, xs(T.max_lps)) + 8, y: y + 4, class: "vals",
        text: `${fmtLps(T.median_lps)}${ratio != null && r.id !== REF ? ` · ×${ratio.toFixed(2)} vs ${label(REF)}` : ""}`,
      }, g);
      g.addEventListener("pointermove", (evt) =>
        showTip(evt, (t) => {
          t.append(ttRow(r.id, fmtLps(T.median_lps)), ttTitle(config()));
          t.append(ttGrid([
            ["median", fmtLps(T.median_lps)], ["slowest repetition", fmtLps(T.min_lps)], ["fastest repetition", fmtLps(T.max_lps)],
            ["repetitions", String(T.reps)], ["amortized time / lookup", fmtNs(T.ns_per_op)],
            ["lookups / repetition", Math.round(r.e.reps[0]?.lookups || 0).toLocaleString("en")],
            ...(ratio != null ? [[`vs ${label(REF)}`, `×${ratio.toFixed(2)}`]] : []),
            ["threads", `${thLabel(state.threads)} (same total volume as 1 thread)`],
            ["tie with the fastest", i === 0 ? "fastest" : tie ? "yes (ranges overlap)" : "no"],
            ["API", state.op === "lookup" ? libBy.get(r.id).lookup_api : libBy.get(r.id).contains_api],
          ]));
        })
      );
      g.addEventListener("pointerleave", hideTip);
    });
    sv("line", { x1: x0, y1: H - 26, x2: x1, y2: H - 26, class: "axis" }, svg);
    host.append(svg);
    appendMissing(host, missing);
  }

  // ------------------------------------------------------------ candlesticks
  function renderCandles() {
    const host = $("candle-card");
    cardHead(host, `Latency of one ${OP_SHORT[state.op]} - individually timed calls`, `${latConfig()} · ranked by p99, lower is better`);
    $("latency-note").textContent =
      "Each sample times one isolated call with the monotonic clock of the runner's language (one CLOCK_MONOTONIC read before and after); the median cost of a pair of clock reads is subtracted. " +
      "These are percentiles of individual latencies, never of averaged batches; the amortized cost per lookup of the throughput loops is a different quantity (table). " +
      "Values close to the clock resolution are marked as limited by the instrumentation.";
    const anat = el("div", { class: "anatomy" });
    const a = sv("svg", { width: 470, height: 16, "aria-hidden": "true" });
    sv("line", { x1: 4, y1: 8, x2: 40, y2: 8, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("line", { x1: 4, y1: 3, x2: 4, y2: 13, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("text", { x: 46, y: 12, text: "p1 (lower whisker)" }, a);
    sv("rect", { x: 160, y: 2, width: 34, height: 12, rx: 2, style: "fill:var(--text-secondary);fill-opacity:.35;stroke:var(--text-secondary);stroke-width:1.5" }, a);
    sv("line", { x1: 177, y1: 1, x2: 177, y2: 15, style: "stroke:var(--text-primary);stroke-width:2" }, a);
    sv("text", { x: 200, y: 12, text: "p25 to p75, mark at p50" }, a);
    sv("line", { x1: 360, y1: 8, x2: 388, y2: 8, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("line", { x1: 388, y1: 3, x2: 388, y2: 13, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("text", { x: 394, y: 12, text: "p99 (upper)" }, a);
    anat.append(a, el("span", { text: "p90 and p95 in the tooltip" }));
    host.append(anat);
    const rows = visible()
      .map((l) => ({ id: l.id, e: LAT.get(k(l.id, state.family, state.size, state.op, state.scenario)) }))
      .filter((r) => ok(r.e))
      .sort((x, y) => x.e.summary.p99 - y.e.summary.p99);
    const missing = visible()
      .filter((l) => !rows.some((r) => r.id === l.id))
      .map((l) => [l.id, reasonOf(LAT.get(k(l.id, state.family, state.size, state.op, state.scenario)), l.id, state.size)]);
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "No measurement for this configuration." }));
      appendMissing(host, missing);
      return;
    }
    const W = chartWidth(host);
    const labelW = Math.min(290, W * 0.28);
    const valsW = Math.min(300, W * 0.28);
    const x0 = labelW + 8;
    const x1 = W - valsW - 12;
    const rowH = 34;
    const H = 6 + rows.length * rowH + 30;
    const lo = state.ylog ? Math.min(...rows.map((r) => Math.max(r.e.summary.p1, 1))) : 0;
    const dom = valueDomain([lo, ...rows.map((r) => r.e.summary.p99)], state.ylog, true);
    const xs = scale(dom[0], dom[1], x0, x1, state.ylog);
    const svg = sv("svg", { width: W, height: H, role: "img", "aria-label": "Latency candlesticks" });
    for (const t of state.ylog ? logTicks(dom[0], dom[1]) : niceTicks(dom[0], dom[1], 5)) {
      sv("line", { x1: xs(t), y1: 6, x2: xs(t), y2: H - 26, class: "grid" }, svg);
      sv("text", { x: xs(t), y: H - 10, "text-anchor": "middle", class: "tick", text: tickNs(t) }, svg);
    }
    rows.forEach((r, i) => {
      const L = r.e.summary;
      const y = 6 + i * rowH + rowH / 2;
      const c = colorOf(r.id);
      const g = sv("g", {}, svg);
      sv("rect", { x: 0, y: y - rowH / 2, width: W, height: rowH, class: "rowbg" }, g);
      sv("rect", { x: 2, y: y - 10, width: 30, height: 20, rx: 4, class: "badge" }, g);
      sv("text", { x: 17, y: y + 4, "text-anchor": "middle", class: "ink", text: "#" + (i + 1) }, g);
      const tag = adaptedTag(r.id, state.op);
      sv("text", { x: 40, y: y + 4, class: "ink", text: label(r.id) + (tag ? " · " + tag : "") }, g);
      const X = (v) => xs(Math.max(v, dom[0]));
      sv("line", { x1: X(L.p1), y1: y, x2: X(L.p25), y2: y, style: `stroke:${c};stroke-width:2` }, g);
      sv("line", { x1: X(L.p1), y1: y - 6, x2: X(L.p1), y2: y + 6, style: `stroke:${c};stroke-width:2` }, g);
      sv("rect", { x: X(L.p25), y: y - 9, width: Math.max(3, X(L.p75) - X(L.p25)), height: 18, rx: 3, style: fillStyle(r.id, 0.35) }, g);
      sv("line", { x1: X(L.p50), y1: y - 10, x2: X(L.p50), y2: y + 10, style: "stroke:var(--text-primary);stroke-width:2.5" }, g);
      sv("line", { x1: X(L.p75), y1: y, x2: X(L.p99), y2: y, style: `stroke:${c};stroke-width:2` }, g);
      sv("line", { x1: X(L.p99), y1: y - 6, x2: X(L.p99), y2: y + 6, style: `stroke:${c};stroke-width:2` }, g);
      sv("text", {
        x: W - valsW, y: y + 4, class: "vals",
        text: `p50 ${fmtNs(L.p50)} · p99 ${fmtNs(L.p99)}${L.precision_limited ? " · clock-limited" : ""}`,
      }, g);
      g.addEventListener("pointermove", (evt) =>
        showTip(evt, (t) => {
          t.append(ttRow(r.id, `rank #${i + 1} by p99`), ttTitle(latConfig()));
          t.append(ttGrid([
            ["p1", fmtNs(L.p1)], ["p25", fmtNs(L.p25)], ["p50", fmtNs(L.p50)], ["p75", fmtNs(L.p75)],
            ["p90", fmtNs(L.p90)], ["p95", fmtNs(L.p95)], ["p99", fmtNs(L.p99)],
            ["p99.9", L.p99_9 == null ? "n/a (< 100 000 samples)" : fmtNs(L.p99_9)], ["mean", fmtNs(L.mean)],
            ["p50 across repetitions", `${fmtNs(L.p50_min)} to ${fmtNs(L.p50_max)}`],
            ["samples", `${(L.samples_per_rep || 0).toLocaleString("en")} × ${L.reps} repetitions`],
            ["clock pair cost (subtracted)", fmtNs(L.overhead_ns)], ["clock resolution", fmtNs(L.resolution_ns)],
            ["precision", L.precision_limited ? "limited by the instrumentation" : "ok"], ["threads", LAT_THREADS],
          ]));
        })
      );
      g.addEventListener("pointerleave", hideTip);
    });
    sv("line", { x1: x0, y1: H - 26, x2: x1, y2: H - 26, class: "axis" }, svg);
    host.append(svg);
    appendMissing(host, missing);
  }

  function renderLatencySizes() {
    for (const [id, p] of [["line-p50", "p50"], ["line-p99", "p99"]]) {
      lineChart($(id), {
        title: `Latency ${p} - ${OP_SHORT[state.op]}`,
        sub: `${FAMILY[state.family]} · ${SCEN[state.scenario]} · ${LAT_THREADS} · median of the repetitions`,
        series: seriesOver((lib, s) => {
          const e = LAT.get(k(lib, state.family, s, state.op, state.scenario));
          return ok(e) ? e.summary[p] : null;
        }),
        xs: sizes, xlog: true, ylog: state.ylog, height: 260, lowerIsBetter: true,
        yFmt: fmtNs, tick: tickNs, xFmt: sizeLabel,
        tipTitle: (s) => `${sizeLabel(s)} prefixes · ${p}`,
        reason: (lib, s) => reasonOf(LAT.get(k(lib, state.family, s, state.op, state.scenario)), lib, s),
      });
    }
  }

  function renderThreads() {
    const go = LIBS.find((l) => l.runtime?.GOMAXPROCS);
    $("threads-note").textContent =
      `Mixed scenario, ${THREAD_LIST.join(", ")} worker threads sharing one index (read only) in the same process. Every thread count runs the same total number of lookups ` +
      "(calibrated once on one thread), split in contiguous ranges of the query array; the workers are created before the clock and released together, and the throughput is the total over the wall time from the release to the end of the last worker. " +
      (go ? `Go: GOMAXPROCS = ${go.runtime.GOMAXPROCS} (default), GOGC = ${go.runtime.GOGC}. ` : "") +
      `No CPU affinity in any runner; this CPU has ${R.meta.env.physical_cores ?? "?"} physical cores and ${R.meta.env.logical_cpus} logical CPUs.`;
    if (THREAD_LIST.length < 2) {
      clear($("line-threads")).append(el("p", { class: "placeholder", text: "Only one thread count was measured in this run (THREADS)." }));
      clear($("line-speedup"));
      return;
    }
    const at = (id, t) => TP.get(k(id, state.family, state.size, state.op, "mixed", t));
    const series = (f) => visible().map((l) => {
      const pts = new Map();
      for (const t of THREAD_LIST) {
        const v = f(l.id, t);
        if (v != null && isFinite(v)) pts.set(t, v);
      }
      return { name: l.id, pts };
    });
    const reason = (id, t) => reasonOf(at(id, t), id, state.size);
    lineChart($("line-threads"), {
      title: `Throughput vs threads - ${OP_SHORT[state.op]}`,
      sub: `${FAMILY[state.family]} · ${sizeLabel(state.size)} prefixes · ${SCEN.mixed} · median of the repetitions`,
      series: series((id, t) => (ok(at(id, t)) ? at(id, t).summary.median_lps : null)),
      xs: THREAD_LIST, xlog: false, ylog: state.ylog, height: 280,
      yFmt: fmtLps, tick: tickRate, xFmt: (t) => String(t),
      tipTitle: (t) => `${thLabel(t)} · ${sizeLabel(state.size)} prefixes · ${FAMILY[state.family]}`,
      reason,
    });
    lineChart($("line-speedup"), {
      title: "Speed-up over 1 thread",
      sub: `throughput(n threads) / throughput(1 thread), same library · ${sizeLabel(state.size)} prefixes · ideal = n`,
      series: series((id, t) => {
        const a = at(id, t);
        const b = at(id, 1);
        return ok(a) && ok(b) ? a.summary.median_lps / b.summary.median_lps : null;
      }),
      xs: THREAD_LIST, xlog: false, ylog: false, height: 280,
      yFmt: (v) => `×${v.toFixed(2)}`, tick: (v) => `×${+v.toFixed(1)}`, xFmt: (t) => String(t),
      tipTitle: (t) => `${thLabel(t)} · speed-up over 1 thread`,
      reason,
    });
  }

  // ------------------------------------------------------------ results table
  function resultRows() {
    const rows = [];
    for (const l of visible()) {
      for (const s of sizes) {
        const t = TP.get(k(l.id, state.family, s, state.op, state.scenario, state.threads));
        const la = LAT.get(k(l.id, state.family, s, state.op, state.scenario));
        const m = MEM.get(k(l.id, state.family, s));
        if (!t && !la) continue;
        const T = ok(t) ? t.summary : {};
        const L = ok(la) ? la.summary : {};
        const M = ok(m) ? m.summary : {};
        rows.push({
          name: l.id, lang: l.lang, threads: state.threads, size: s, status: t?.status ?? la?.status ?? "skipped",
          mlps: T.median_lps ?? null, minl: T.min_lps ?? null, maxl: T.max_lps ?? null, nsop: T.ns_per_op ?? null,
          p50: L.p50 ?? null, p99: L.p99 ?? null, p999: L.p99_9 ?? null, limited: L.precision_limited == null ? null : L.precision_limited ? "yes" : "no",
          rss: M.struct_rss ?? null, bpp: M.bytes_per_prefix ?? null, hwm: M.hwm ?? null,
          build: M.build_s ?? null, units: M.units != null && M.units >= 0 ? `${Number(M.units).toLocaleString("en")} ${M.units_name}` : "-",
          reason: [t, la].filter((e) => e && e.status !== "ok").map((e) => `${e.metric}: ${e.reason}`).join("; ") || (t?.reason || ""),
          api: state.op === "lookup" ? l.lookup_api : l.contains_api,
        });
      }
    }
    return rows;
  }
  const COLS = [
    { k: "name", t: "Library" }, { k: "lang", t: "Lang." }, { k: "threads", t: "Threads (throughput)", num: true }, { k: "size", t: "Prefixes", num: true, fmt: sizeLabel },
    { k: "status", t: "Status", status: true },
    { k: "mlps", t: "Throughput (median)", num: true, fmt: fmtLps }, { k: "minl", t: "slowest rep.", num: true, fmt: fmtLps },
    { k: "maxl", t: "fastest rep.", num: true, fmt: fmtLps }, { k: "nsop", t: "ns / lookup (amortized)", num: true, fmt: fmtNs },
    { k: "p50", t: "p50 (1 thread)", num: true, fmt: fmtNs }, { k: "p99", t: "p99 (1 thread)", num: true, fmt: fmtNs }, { k: "p999", t: "p99.9 (1 thread)", num: true, fmt: fmtNs },
    { k: "limited", t: "clock-limited" },
    { k: "rss", t: "Structure RAM", num: true, fmt: fmtBytes }, { k: "bpp", t: "B / prefix", num: true, fmt: fmtBpe },
    { k: "hwm", t: "Peak RSS", num: true, fmt: fmtBytes }, { k: "build", t: "Build / open", num: true, fmt: fmtS },
    { k: "units", t: "Stored units" }, { k: "api", t: "API", wrap: true }, { k: "reason", t: "Reason", wrap: true },
  ];
  function csvOf(rows) {
    const esc = (v) => {
      const s = v == null ? "" : String(v);
      return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
    };
    const head = ["family", "operation", "scenario", "label", ...COLS.map((c) => c.k)];
    return [head.join(","), ...rows.map((r) => [state.family, state.op, state.scenario, label(r.name), ...COLS.map((c) => r[c.k])].map(esc).join(","))].join("\n") + "\n";
  }
  function renderTable() {
    $("table-note").textContent =
      `${FAMILY[state.family]} · ${OP_SHORT[state.op]} · ${SCEN[state.scenario]} · throughput at ${thLabel(state.threads)}, latency and memory at 1 thread: every size and library (use the filters). Click a header to sort. ` +
      "The export contains the rows shown; libraries.csv and libraries.json next to this report hold every measurement and every repetition.";
    const rows = resultRows();
    const bar = clear($("table-bar"));
    bar.append(
      el("button", {
        type: "button", class: "btn", text: "Export these rows (CSV)",
        onclick: () => {
          const blob = new Blob([csvOf(rows)], { type: "text/csv" });
          const a = el("a", { href: URL.createObjectURL(blob), download: `libraries-${state.family}-${state.op}-${state.scenario}-${state.threads}t.csv` });
          document.body.append(a);
          a.click();
          a.remove();
        },
      }),
      el("span", { text: `${rows.length} rows` }),
      el("a", { href: "libraries.csv", text: "full CSV" }),
      el("a", { href: "libraries.json", text: "full JSON" })
    );
    clear($("results-table")).append(table(COLS, rows, { sortKey: "mlps", desc: true, swatch: true }));
  }

  // ------------------------------------------------------------ validation and status
  function renderValidation() {
    $("validation-note").textContent =
      "Before any timing, every runner compares its answers with the expected record ids of the independent oracle (hash map per prefix length; corpora up to 20 000 prefixes and the edge cases also against a linear scan). " +
      `The boundary file (first and last address of sampled prefixes, the addresses just outside, host routes) is checked whole in every repetition, and the first ${S.validate_first ? S.validate_first.toLocaleString("en") : "all"} queries of each scenario in the first repetition (${S.validate_rest.toLocaleString("en")} in the others). ` +
      "A result that differs invalidates the measurements of that operation (reported as failed). During the timed loops a checksum of the results is compared with the oracle's.";
    const groups = new Map();
    for (const v of R.validation) {
      const key = k(v.lib, v.family, v.size);
      const g = groups.get(key) || { name: v.lib, family: v.family, size: v.size, checked: 0, mism: 0, errors: 0, ex: null };
      g.checked += v.checked;
      g.mism += v.mismatches;
      g.errors += v.errors;
      if (!g.ex && v.examples?.length) g.ex = `${v.scenario}/${v.op}: ${v.examples.map((e) => `${e.addr} want ${e.want} got ${e.got}`).join(", ")}`;
      groups.set(key, g);
    }
    const rows = [...groups.values()].map((g) => ({ ...g, status: g.mism || g.errors ? "failed" : "ok", fam: FAMILY[g.family] }));
    clear($("validation-table")).append(
      el("h3", { text: "Generated corpora" }),
      table(
        [{ k: "name", t: "Library" }, { k: "fam", t: "Family" }, { k: "size", t: "Prefixes", num: true, fmt: sizeLabel },
          { k: "status", t: "Status", status: true }, { k: "checked", t: "Answers checked", num: true, fmt: (v) => v.toLocaleString("en") },
          { k: "mism", t: "Mismatches", num: true }, { k: "errors", t: "Errors", num: true }, { k: "ex", t: "First differences", wrap: true }],
        rows, { sortKey: "name", swatch: true }
      )
    );
    const CASE = {
      nested: "nested networks, first/last addresses, host routes, misses",
      default: "same with the family's /0 (covers every address)",
      duplicates: "two CIDRs inserted twice: the last insertion must win",
    };
    const erows = R.edge.map((e) => ({
      name: e.lib, fam: FAMILY[e.family], case: e.case, what: CASE[e.case], status: e.status,
      checked: e.checks.reduce((a, c) => a + c.checked, 0),
      mism: e.checks.reduce((a, c) => a + c.mismatches + c.errors, 0),
      detail: e.reason + (e.checks.find((c) => !c.ok)?.examples?.length
        ? " - " + e.checks.filter((c) => !c.ok).map((c) => `${c.op}: ` + c.examples.map((x) => `${x.addr} want ${x.want} got ${x.got}`).join(", ")).join("; ")
        : ""),
    }));
    clear($("edge-table")).append(
      el("h3", { text: "Dedicated edge cases (also the only corpora with /0 routes and duplicates)" }),
      table(
        [{ k: "name", t: "Library" }, { k: "fam", t: "Family" }, { k: "case", t: "Case" }, { k: "what", t: "Content", wrap: true },
          { k: "status", t: "Status", status: true }, { k: "checked", t: "Answers checked", num: true }, { k: "mism", t: "Differences", num: true },
          { k: "detail", t: "Details", wrap: true }],
        erows, { sortKey: "case", swatch: true }
      )
    );
  }

  function renderStatus() {
    const rows = [];
    for (const [kind, list] of [["throughput", R.throughput], ["latency", R.latency], ["memory", R.memory]]) {
      for (const e of list) {
        if (e.status === "ok" && !e.reason) continue;
        rows.push({ name: e.lib, kind, fam: FAMILY[e.family], size: e.size, op: e.op || "-", sc: e.scenario || "-", status: e.status, reason: e.reason || "" });
      }
    }
    for (const e of R.edge) {
      if (e.status !== "ok") rows.push({ name: e.lib, kind: `edge: ${e.case}`, fam: FAMILY[e.family], size: null, op: "-", sc: "-", status: e.status, reason: e.reason });
    }
    for (const m of R.mmdb) {
      if (m.status !== "ok") rows.push({ name: "libmaxminddb", kind: "MMDB preparation", fam: FAMILY[m.family], size: m.size, op: "-", sc: "-", status: m.status, reason: m.reason });
    }
    for (const e of R.meta.deps.errors || []) rows.push({ name: "-", kind: "dependency", fam: "-", size: null, op: "-", sc: "-", status: "failed", reason: e });
    const host = clear($("status-table"));
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "Every planned measurement has a value." }));
      return;
    }
    host.append(table(
      [{ k: "name", t: "Library" }, { k: "kind", t: "Measurement" }, { k: "fam", t: "Family" }, { k: "size", t: "Prefixes", num: true, fmt: sizeLabel },
        { k: "op", t: "Operation" }, { k: "sc", t: "Scenario" }, { k: "status", t: "Status", status: true }, { k: "reason", t: "Reason", wrap: true }],
      rows, { sortKey: "status", swatch: true }
    ));
  }

  // ------------------------------------------------------------ libraries, corpora, environment
  function renderLibraries() {
    $("libraries-note").textContent =
      "Real implementations through their public APIs, built from pinned versions. Membership uses the native API when there is one; the LPM lookup returns the record id of the most specific prefix. " +
      "Adapted operations are marked in every chart. go-cidranger-libp2p is a fork of cidranger-yl2chen (same path-compressed trie algorithm): two distinct modules, adapters and series, not two independent algorithms.";
    const versionOf = (l) => {
      const crates = R.meta.env.rust_crates || {};
      if (l.package === "dir-24-8") return `${R.meta.project?.version ?? ""} @ ${(R.meta.project?.commit || "").slice(0, 12)}`;
      if (l.lang === "Rust") return crates[l.package] ?? "-";
      return l.version ?? "-";
    };
    const rows = LIBS.map((l) => ({
      name: l.id, lang: l.lang, repo: l.repo, version: versionOf(l), role: l.reference ? "reference" : l.variant_of ? `variant of ${label(l.variant_of)}` : "",
      impl: l.implementation?.implementation || l.implementation?.backend || l.package,
      contains: `${l.contains_api}${l.contains === "adapted" ? " [adapted]" : ""}`,
      lookup: `${l.lookup_api}${l.lookup === "adapted" ? " [adapted LPM]" : ""}`,
      notes: l.notes,
    }));
    clear($("libs-table")).append(table(
      [{ k: "name", t: "Library" }, { k: "lang", t: "Language" }, { k: "role", t: "Role" }, { k: "version", t: "Version / commit" }, { k: "repo", t: "Repository" },
        { k: "impl", t: "Implementation (as reported by the runner)", wrap: true }, { k: "contains", t: "Membership", wrap: true },
        { k: "lookup", t: "LPM lookup", wrap: true }, { k: "notes", t: "Notes", wrap: true }],
      rows, { sortKey: "name", swatch: true }
    ));
    const go = R.meta.deps.go || {};
    const mods = (go.modules || []).map((m) => ({ path: m.path, version: m.version, sum: m.sum, replace: m.replace || "none" }));
    clear($("modules-table")).append(
      el("h3", { text: "Go modules compiled into the runner (go version -m)" }),
      el("p", { class: "csub", text: `Toolchain ${go.toolchain || "?"} · replace directives in go.mod: ${go.replace_directives ? "yes" : "none"} · two cidranger modules distinct: ${go.cidranger_distinct ? "yes" : "NO"} (${go.cidranger_check || ""})` }),
      table([{ k: "path", t: "Module" }, { k: "version", t: "Version" }, { k: "sum", t: "go.sum hash" }, { k: "replace", t: "Replacement" }], mods, { sortKey: "path" })
    );
  }

  function renderCorpora() {
    $("corpora-note").textContent =
      "Generated once by the shared generator (benches/support/data.rs) and read unchanged by every runner: same prefixes, same prefix -> record id association (id = index + 1), same query order. " +
      "Prefixes are distinct canonical CIDRs: BGP-like lengths (IPv4 /8 to /32, mostly /24; IPv6 /16 to /128, mostly /48 and /32), clustered, with nested networks and host routes; /0 routes only in the edge cases. " +
      "Scenarios: mixed (50 % hits, shuffled), hits (a random address inside a random prefix), misses (verified absent: near misses and uniform addresses), locality (90 % of the queries from a hot set of 1 024 mixed addresses). " +
      "IPv4 queries are given to every library in 4-byte form; IPv6 queries stay in 2000::/3, so no IPv4-mapped address reaches a table. Hashes are FNV-1a 64 of the files; cached corpora are reused only when every hash matches.";
    const rate = (c, n) => {
      const f = c.files.find((x) => x.name === `q-${n}.bin`);
      return f?.hit_rate == null ? null : f.hit_rate;
    };
    const rows = R.corpora.map((c) => ({
      fam: FAMILY[c.family], size: c.prefixes, id: c.id, hash: c.files.find((f) => f.name === "prefixes.bin")?.fnv64,
      hosts: c.host_routes, nested: c.nested, lengths: c.lengths.slice().sort((a, b) => b[1] - a[1]).slice(0, 4).map(([l, n]) => `/${l}: ${pct(n / c.prefixes)}`).join(", "),
      mixed: rate(c, "mixed"), hits: rate(c, "hits"), misses: rate(c, "misses"), local: rate(c, "local"),
      queries: c.queries, gen: `${fmtS(c.generate_s)}${c.cached ? " (cached)" : ""}`,
    }));
    clear($("corpora-table")).append(table(
      [{ k: "fam", t: "Family" }, { k: "size", t: "Input prefixes", num: true, fmt: (v) => v.toLocaleString("en") }, { k: "hash", t: "prefixes.bin hash" },
        { k: "hosts", t: "Host routes", num: true }, { k: "nested", t: "Nested prefixes", num: true }, { k: "lengths", t: "Main lengths", wrap: true },
        { k: "mixed", t: "hits: mixed", num: true, fmt: pct }, { k: "hits", t: "hits", num: true, fmt: pct }, { k: "misses", t: "misses", num: true, fmt: pct },
        { k: "local", t: "locality", num: true, fmt: pct }, { k: "queries", t: "Queries / scenario", num: true, fmt: (v) => v.toLocaleString("en") },
        { k: "gen", t: "Generation" }, { k: "id", t: "Corpus id" }],
      rows, { sortKey: "size" }
    ));
    const mrows = R.mmdb.filter((m) => !m.corpus.startsWith("edge")).map((m) => {
      const mem = R.memory.find((e) => e.lib === "libmaxminddb" && e.family === m.family && e.size === m.size);
      const meta = mem?.summary?.mmdb || {};
      return {
        fam: FAMILY[m.family], size: m.size, status: m.status, bytes: m.file_bytes || null, fab: m.fabricate_s || null,
        cached: m.cached ? "cached" : "written in this run", nodes: meta.node_count ?? null, rec: meta.record_size ?? m.writer?.record_size ?? null,
        open: mem?.summary?.open_s ?? null, res: mem?.summary?.mmdb_resident ?? null, writer: m.writer?.writer || "", reason: m.reason || "",
      };
    });
    clear($("mmdb-table")).append(
      el("h3", { text: "MMDB files for libmaxminddb (prepared from the same corpora, not timed as lookups)" }),
      table(
        [{ k: "fam", t: "Family" }, { k: "size", t: "Input prefixes", num: true, fmt: sizeLabel }, { k: "status", t: "Status", status: true },
          { k: "bytes", t: "File size", num: true, fmt: fmtBytes }, { k: "res", t: "Resident after the lookups", num: true, fmt: fmtBytes },
          { k: "nodes", t: "Tree nodes", num: true }, { k: "rec", t: "Record size (bits)", num: true },
          { k: "fab", t: "Fabrication (mmdbwriter)", num: true, fmt: fmtS }, { k: "cached", t: "File" },
          { k: "open", t: "MMDB_open (median)", num: true, fmt: fmtS },
          { k: "writer", t: "Writer" }, { k: "reason", t: "Reason", wrap: true }],
        mrows, { sortKey: "size" }
      )
    );
  }

  function renderMethod() {
    const host = clear($("method-text"));
    const sec = (title, ...paras) => {
      host.append(el("h3", { text: title }));
      for (const p of paras) host.append(el("p", { text: p }));
    };
    const st = R.meta.stages || {};
    const dep = st.dependencies || {};
    sec("Threads",
      `Throughput is measured with ${THREAD_LIST.join(", ")} worker threads on the mixed scenario (1 thread on the other scenarios): the workers share one index read-only, start together after being created, and run the same total number of lookups as the 1-thread measurement; throughput = total lookups / wall time from the release to the last worker's end. ` +
        "Latency (individually timed calls), validation and memory are measured on one thread. No CPU affinity is set; Go uses its default GOMAXPROCS. " +
        "dir-24-8 runs its default configuration: single lookups and exist always use its scalar kernels (its SIMD kernels, feature batch-simd, only serve batch lookups, which are not measured here).");
    sec("Processes and repetitions",
      `Each (library, family, size) is measured in ${S.repetitions} independent fresh processes, one library loaded per process. Repetitions run in rounds, by increasing size, and the order of the libraries rotates from one round and size to the next. ` +
      `A process is stopped after ${S.timeout_s} s (timeout) or when its resident memory exceeds the limit (${fmtBytes(R.meta.mem_limit_bytes)}); larger sizes are skipped when the memory scaled from the previous size would not fit. ` +
      `The measurement budget is ${S.budget_s ? S.budget_s + " s" : "unlimited"}; anything not run is listed with its reason.`);
    sec("Runners",
      "Each library runs in a native runner of its language (Rust: prefix-trie, dir-24-8; Go: both cidrangers, go-iptrie, bart; C: libmaxminddb) that executes the lookup loop itself and prints JSON lines. " +
      "The orchestrator only starts processes and reads their output: no process, IPC, JSON or cgo crossing per lookup. Addresses are converted to each library's native type, tables built, files opened and buffers allocated before any timing. " +
      "Go: one module, one toolchain, the same flags for every library (CGO_ENABLED=0, -trimpath), GC left at its default (GOGC=100), GOMAXPROCS default; runtime.GC() runs between measurements, outside the timed sections.");
    sec("Throughput",
      `After the validation, each measured scenario is warmed up with one pass over its queries per operation, stopped after ${S.prewarm_ms} ms (fast libraries complete the pass; the count is kept in the raw outputs). Per measurement: an untimed run of ${S.warmup.toLocaleString("en")} lookups calibrates the count so that the timed loop lasts about ${S.target_ms} ms; the loop walks the scenario's ${S.queries.toLocaleString("en")} queries in order (wrapping), one thread. ` +
      "Reported: lookups per second (exact count / monotonic wall time) and amortized ns per lookup, median of the repetitions with the slowest and fastest. A checksum of the results (sum of ids, or count of hits) is compared with the oracle's for the same queries, so a loop cannot be optimized away or return wrong answers silently; lookup errors are counted separately from misses.");
    sec("Latency",
      `A separate campaign times ${S.latency_samples.toLocaleString("en")} individual calls per measurement (queries in order), each between two reads of the monotonic clock (runtime.nanotime in Go, Instant in Rust, clock_gettime in C: CLOCK_MONOTONIC through the vDSO in all three). ` +
      "The median cost of two back-to-back clock reads, measured in each process, is subtracted; the clock resolution (smallest observed step) is reported, and percentiles below three steps or under the clock cost are flagged as limited by the instrumentation. " +
      "Percentiles are nearest-rank over the samples of one repetition; the report shows their median over the repetitions. p99.9 is only given with at least 100 000 samples.");
    sec("Memory",
      "Resident memory (VmRSS) is read before the build (after the queries and the input prefixes are loaded), after the build and after the measurements, freed memory being returned to the system first (Go: GC + FreeOSMemory; Rust and C: malloc_trim). " +
      "The structure's memory is RSS after the measurements minus RSS before the build: it includes allocator and GC overhead and, for memory-mapped structures (dir-24-8 roots, MMDB files), the pages touched by the measured queries. " +
      "The allocator estimate is the heap delta of the build (Go runtime.MemStats, Rust counting allocator) or the MMDB file size. Peak RSS is VmHWM at the end of the process. Bytes per prefix use the number of input prefixes; libraries that merge networks (iprange) or split them (MMDB tree) report their own unit counts.");
    sec("libmaxminddb",
      "The MMDB file is written from the same corpus by MaxMind's official Go writer (mmdbwriter), prefixes inserted by increasing length so that the most specific network wins, one uint32 per network (the record id), IPv4 or IPv6 tree, record size 32, reserved networks allowed, IPv4 aliasing disabled. " +
      "Its fabrication time is a preparation step; MMDB_open is timed separately (build / open column); lookups use MMDB_lookup_sockaddr on prebuilt sockaddr structures (membership: found_entry; LPM lookup: MMDB_aget_value of the uint32). " +
      "The ids are compared with the oracle, not the returned netmask (the tree may split networks into different blocks). The file is memory-mapped; its resident share after the lookups is reported.");
    sec("Run time",
      `Dependencies: download and verification ${fmtS(dep.go_download_s + dep.mmdb_download_s)} (Go toolchain and modules ${fmtS(dep.go_download_s)}, libmaxminddb tarball ${fmtS(dep.mmdb_download_s)}); ` +
      `compilation: Rust runner ${st.rust_build_s == null ? "not recorded (built outside make)" : fmtS(st.rust_build_s)} (cargo, incremental), Go runners ${fmtS(dep.go_build_s)}, libmaxminddb ${fmtS(dep.mmdb_build_s)}${R.meta.deps.libmaxminddb?.cached ? " (cached)" : ""}, C runner ${fmtS(dep.c_build_s)}${R.meta.deps.c_runner?.cached ? " (cached)" : ""}. ` +
      `Corpora ${fmtS(st.corpus_s)}${st.corpus_cached ? " (cached)" : ""}, MMDB files ${fmtS(st.mmdb_s)}${st.mmdb_cached ? " (cached)" : ""}, measurements ${fmtS(st.measure_s)}, total ${fmtS(st.total_s)}.`);
  }

  function renderEnv() {
    const e = R.meta.env;
    const c = e.compilers || {};
    clear($("env-table")).append(el("h3", { text: "Machine and toolchains" }), kvTable([
      ["CPU", `${e.cpu} (${e.arch})`], ["cores", `${e.physical_cores ?? "?"} physical, ${e.logical_cpus} logical`],
      ["CPU features", (e.cpu_features || []).join(" ")], ["caches", e.caches], ["governor / boost", `${e.governor} / ${e.boost}`],
      ["RAM", `${fmtBytes(e.ram_total)} (available at start ${fmtBytes(e.ram_available_at_start)})`], ["OS", e.os], ["kernel", e.kernel],
      ["load average (1 min) at start / end", `${e.load_avg_at_start ?? "?"} / ${e.load_avg_at_end ?? "?"}`], ["transparent huge pages", e.thp],
      ["rustc", `${c.rustc} (LLVM ${c.llvm})`], ["Rust profile", c.rust_profile], ["Rust target features", c.rust_target_features],
      ["Go", c.go], ["C compiler", c.cc], ["C flags", `runner ${c.cflags}; libmaxminddb ${c.libmaxminddb_cflags}`],
      ["dir-24-8 kernels", LIBS.filter((l) => l.package === "dir-24-8").map((l) => `${label(l.id)}: ${l.implementation?.backend}`).join("; ")],
      ["libmaxminddb", `${R.meta.deps.libmaxminddb?.version} (${R.meta.deps.libmaxminddb?.url}, SHA-256 ${R.meta.deps.libmaxminddb?.sha256_verified ? "verified" : "not verified"})`],
    ]));
    clear($("settings-table")).append(el("h3", { text: "Settings" }), kvTable([
      ["profile", S.profile], ["sizes", S.sizes.map(sizeLabel).join(", ")], ["detail sizes (all scenarios)", S.detail_sizes.map(sizeLabel).join(", ")],
      ["families", S.families.join(", ")], ["seed", S.seed], ["repetitions", S.repetitions], ["queries per scenario", S.queries],
      ["target per throughput measurement", `${S.target_ms} ms (at least 16 384 lookups)`], ["calibration run", `${S.warmup} lookups`],
      ["warm-up", `one pass per scenario and operation, at most ${S.prewarm_ms} ms`],
      ["latency samples per measurement", S.latency_samples],
      ["validation", `boundary file whole; per scenario ${S.validate_first || "all"} queries (first repetition), ${S.validate_rest} (others); checksum of every timed loop`],
      ["budget", S.budget_s ? `${S.budget_s} s` : "none"], ["timeout per process", `${S.timeout_s} s`],
      ["memory limit per process", fmtBytes(R.meta.mem_limit_bytes)], ["GOAMD64", S.goamd64], ["threads", `throughput: ${THREAD_LIST.join(", ")} (mixed scenario); latency, validation and memory: 1`],
      ["notes", (S.notes || []).join("; ")], ["raw runner outputs", R.meta.raw_dir],
    ]));
    $("footer").textContent = `Libraries part of bench-compare · schema ${R.schema} · raw data: libraries.json and libraries.csv next to this file (raw runner outputs: ${R.meta.raw_dir}); regenerate this page without measuring with "make bench-compare REPORT_ONLY=1".`;
  }

  // ------------------------------------------------------------ main
  function renderAll() {
    hideTip();
    renderFilters();
    renderLegend();
    renderSizes();
    renderMemory();
    renderBars();
    renderCandles();
    renderLatencySizes();
    renderThreads();
    renderTable();
  }
  // Charts are sized from their cards: a hidden tab is drawn when shown.
  let stale = false;
  function draw() {
    stale = root.hidden;
    if (!stale) renderAll();
  }
  renderHeader();
  draw();
  renderValidation();
  renderStatus();
  renderLibraries();
  renderCorpora();
  renderMethod();
  renderEnv();
  root.addEventListener("tabshow", () => {
    if (stale) draw();
  });
  let resizeTimer = 0;
  window.addEventListener("resize", () => {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(draw, 150);
  });
})();

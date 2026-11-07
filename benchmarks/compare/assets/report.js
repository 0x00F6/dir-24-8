"use strict";
(() => {
  let R;
  try {
    R = JSON.parse(document.getElementById("data").textContent);
  } catch {
    // The template opened directly: no results embedded.
    document.getElementById("subtitle").textContent =
      "Template without results: run `make bench-compare` (or `make bench-compare REPORT_ONLY=1`) to generate benchmarks/reports/report.html.";
    return;
  }
  const S = R.meta.settings;
  const $ = (id) => document.getElementById(id);

  const K = Kit({
    colorOf: (n) => colorOf(n),
    dashed: (n) => dashed(n),
    label: (n) => label(n),
    status: () => STATUS,
    onChange: () => renderAll(),
  });
  const { trim, sizeLabel, fmtNs, fmtRate, fmtBytes, fmtS, fmtBpe, short, tickRate, tickNs, tickS, tickBytes, pct, el, sv, clear, showTip, hideTip, ttTitle, ttRow, ttGrid, scale, niceTicks, byteTicks, logTicks, valueDomain, segGroup, swatch, markerAt, chartWidth, cardHead, appendMissing, lineChart, table, kvTable } = K;

  const FAMILY = { ipv4: "IPv4", ipv6: "IPv6" };
  const OP = { lookup: "Lookup", batch: "Batch (array)", exists: "Exists" };
  const hitPct = S.hit_permille / 10;
  const DIST = { random: `Random (${trim(hitPct)} % hits)`, hits: "Only existing", misses: "Only absent" };
  const STATUS = {
    ok: ["ok", "st-ok"],
    "not-applicable": ["not applicable", "st-skip"],
    "memory-limit": ["memory limit", "st-mem"],
    timeout: ["timeout", "st-time"],
    budget: ["time budget", "st-time"],
    invalid: ["invalid results", "st-bad"],
    failed: ["failed", "st-bad"],
    "not-run": ["not run", "st-skip"],
    adapted: ["adapted", "st-skip"],
  };
  const API = {
    native: "native batch API",
    loop: "loop of single lookups",
    dedicated: "dedicated exists API",
    derived: "derived from lookup",
    unsupported: "not supported",
  };

  // ------------------------------------------------------------ data index
  const ENG = R.engines.slice().sort((a, b) => a.index - b.index);
  const engBy = new Map(ENG.map((e) => [e.name, e]));
  const ISA = { avx512: "AVX-512", avx2: "AVX2", neon: "NEON", scalar: "scalar" };
  // Results written before the batch-simd feature measured runtime-selected
  // backends (`dir-24-8` = AutoSimd, SIMD single lookups too, plus forced ones).
  const LEGACY = R.engines.some((e) => /^dir-24-8\[(avx2|avx512|neon)\]$/.test(e.name));
  /** Display name: every dir-24-8 series says which kernel its batches run (single lookups are always scalar). */
  const label = (name) => {
    if (name === "dir-24-8") {
      const b = R.meta.env.batch_kernel;
      if (LEGACY) return b === "scalar" ? "dir-24-8 (scalar, no SIMD on this CPU)" : `dir-24-8 (SIMD ${ISA[b] || b})`;
      return b === "scalar" ? "dir-24-8 (batch-simd, no SIMD kernel on this CPU)" : `dir-24-8 (SIMD ${ISA[b] || b} batches)`;
    }
    const m = /^dir-24-8\[(\w+)\]$/.exec(name);
    if (m) return m[1] === "scalar" ? "dir-24-8 (scalar)" : `dir-24-8 (SIMD ${ISA[m[1]] || m[1]}, forced)`;
    return name;
  };
  const slot = (name) => ((engBy.get(name)?.index ?? 0) % 8) + 1;
  const colorOf = (name) => `var(--s${slot(name)})`;
  const dashed = (name) => (engBy.get(name)?.index ?? 0) >= 8;
  const runKey = (f, s, e) => `${f}|${s}|${e}`;
  const runs = new Map(R.runs.map((r) => [runKey(r.family, r.size, r.engine), r]));
  const sizes = S.sizes.slice().sort((a, b) => a - b);

  const opSupported = (e, op) => engBy.get(e)?.[op] !== "unsupported";

  function getMeasure(run, st) {
    if (!run || run.status !== "ok") return null;
    return (
      run.measures.find(
        (m) =>
          m.op === st.op &&
          m.dist === st.dist &&
          m.threads === st.threads &&
          (st.op !== "batch" || m.batch === st.batch)
      ) || null
    );
  }

  function reasonFor(run, name, op) {
    if (op && !opSupported(name, op)) return "operation not supported by this structure";
    if (!run) return "not run (not selected or interrupted)";
    if (run.status === "ok") return "configuration not measured";
    const s = STATUS[run.status]?.[0] ?? run.status;
    return run.reason ? `${s}: ${run.reason}` : s;
  }

  // ------------------------------------------------------------ state
  const pickDefaultSize = (fam) => {
    const okSizes = sizes.filter((s) =>
      ENG.some((e) => runs.get(runKey(fam, s, e.name))?.status === "ok")
    );
    const withOurs = okSizes.filter((s) => runs.get(runKey(fam, s, ENG[0].name))?.status === "ok");
    const pool = withOurs.length ? withOurs : okSizes;
    const pref = pool.filter((s) => s <= 1e6);
    return (pref.length ? pref : pool).slice(-1)[0] ?? sizes[0];
  };
  const state = {
    family: S.families[0],
    op: S.ops.includes("lookup") ? "lookup" : S.ops[0],
    dist: S.dists.includes("random") ? "random" : S.dists[0],
    size: 0,
    threads: S.threads[0],
    batch: S.batch[0],
    xlog: true,
    ylog: false,
    mem: "footprint",
    hidden: new Set(),
  };
  state.size = pickDefaultSize(state.family);

  // ------------------------------------------------------------ filters
  function renderFilters() {
    const row = clear(document.querySelector(".filters-row"));
    row.append(
      segGroup("Family", S.families.map((f) => [f, FAMILY[f]]), () => state.family, (v) => {
        state.family = v;
      }),
      segGroup("Operation", S.ops.map((o) => [o, OP[o]]), () => state.op, (v) => {
        state.op = v;
      }),
      segGroup("Distribution", S.dists.map((d) => [d, DIST[d]]), () => state.dist, (v) => {
        state.dist = v;
      }),
      segGroup("Entries", sizes.map((s) => [s, sizeLabel(s)]), () => state.size, (v) => {
        state.size = v;
      }),
      segGroup("Threads", S.threads.map((t) => [t, String(t)]), () => state.threads, (v) => {
        state.threads = v;
      })
    );
    if (S.batch.length > 1)
      row.append(
        segGroup("Batch", S.batch.map((b) => [b, String(b)]), () => state.batch, (v) => {
          state.batch = v;
        })
      );
    row.append(
      segGroup("Entries axis", [[true, "log"], [false, "linear"]], () => state.xlog, (v) => {
        state.xlog = v;
      }),
      segGroup("Value axis", [[false, "linear"], [true, "log"]], () => state.ylog, (v) => {
        state.ylog = v;
      })
    );
  }

  function renderLegend() {
    const lg = clear($("legend"));
    for (const e of ENG) {
      lg.append(
        el("button", {
          type: "button",
          class: "lchip",
          "aria-pressed": String(!state.hidden.has(e.name)),
          title: "Show or hide " + label(e.name),
          onclick: () => {
            if (state.hidden.has(e.name)) state.hidden.delete(e.name);
            else state.hidden.add(e.name);
            renderAll();
          },
        }, swatch(e.name), document.createTextNode(label(e.name)))
      );
    }
  }
  const visible = () => ENG.filter((e) => !state.hidden.has(e.name));

  // ------------------------------------------------------------ header
  function renderHeader() {
    const env = R.meta.env;
    const d = new Date(env.timestamp_unix * 1000);
    $("subtitle").textContent =
      `${d.toISOString().replace("T", " ").slice(0, 16)} UTC · run time ${fmtDuration(R.meta.duration_s)} · ` +
      `${R.runs.length} structure runs · ${R.runs.reduce((a, r) => a + r.measures.length, 0)} measurements`;
    const chips = clear($("env-chips"));
    const chip = (k, v) => el("span", { class: "chip" }, document.createTextNode(k + " "), el("b", { text: v }));
    chips.append(
      chip("CPU", env.cpu),
      chip("cores", `${env.physical_cores ?? "?"} physical / ${env.logical_cpus} logical`),
      chip("RAM", fmtBytes(env.ram_total)),
      chip("OS", env.os),
      chip("rustc", env.rustc),
      chip("dir-24-8 batch kernel", env.batch_kernel),
      chip("latency clock", R.meta.timer_source.startsWith("core cycle") ? "core cycles (rdpmc)" : "TSC")
    );
    $("partial").hidden = !!R.meta.complete;
  }
  function fmtDuration(s) {
    s = Math.round(s);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return (h ? h + " h " : "") + m + " min " + (s % 60) + " s";
  }

  // ------------------------------------------------------------ overview
  function configLabel() {
    return `${FAMILY[state.family]} · ${sizeLabel(state.size)} entries · ${OP[state.op]}${
      state.op === "batch" ? " of " + state.batch : ""
    } · ${DIST[state.dist]} · ${state.threads} thread${state.threads > 1 ? "s" : ""}`;
  }
  function currentRows() {
    const rows = [];
    for (const e of visible()) {
      const run = runs.get(runKey(state.family, state.size, e.name));
      const m = getMeasure(run, state);
      if (m) rows.push({ name: e.name, run, m });
    }
    return rows;
  }
  function renderOverview() {
    const t = clear($("tiles"));
    const tile = (k, v, d) =>
      el("div", { class: "tile" }, el("div", { class: "k", text: k }), el("div", { class: "v", text: v }), el("div", { class: "d", text: d || "" }));
    const notRun = R.runs.filter((r) => r.status !== "ok").length;
    t.append(
      tile("Run time", fmtDuration(R.meta.duration_s), `of which datasets ${fmtS(R.meta.datasets_s)} (${R.datasets.filter((d) => d.cached).length} of ${R.datasets.length} reused from the cache)`),
      tile("Measurements", String(R.runs.reduce((a, r) => a + r.measures.length, 0)), `${R.runs.length - notRun} structure builds measured`),
      tile("Not executed", String(notRun), "see the table below for reasons")
    );
    const rows = currentRows();
    if (rows.length) {
      const best = rows.slice().sort((a, b) => b.m.throughput.median - a.m.throughput.median)[0];
      const p99 = rows.slice().sort((a, b) => a.m.latency.p99 - b.m.latency.p99)[0];
      t.append(
        tile("Highest throughput", fmtRate(best.m.throughput.median), label(best.name)),
        tile("Lowest p99 latency", fmtNs(p99.m.latency.p99), label(p99.name))
      );
    }
    const mems = visible()
      .map((e) => ({ name: e.name, run: runs.get(runKey(state.family, state.size, e.name)) }))
      .filter((x) => x.run?.status === "ok" && x.run.memory);
    if (mems.length) {
      const small = mems.sort((a, b) => a.run.memory.footprint - b.run.memory.footprint)[0];
      t.append(tile("Smallest footprint", fmtBytes(small.run.memory.footprint), label(small.name)));
    }
    $("overview-note").textContent = `The first three tiles cover the whole run; the others follow the filters: ${configLabel()}.`;
  }

  // ------------------------------------------------------------ candlesticks
  function renderCandles() {
    const host = $("candle-card");
    const batch = state.op === "batch";
    const title = batch
      ? `Batch call latency - ${state.batch} addresses per call`
      : state.op === "exists"
        ? "Exists latency - one isolated call per sample"
        : "Lookup latency - one isolated call per sample";
    cardHead(host, title, `${configLabel()} · ranked by p99, lower is better`);
    $("latency-note").textContent = batch
      ? "Each sample times one whole batch call. The amortized cost per address (batch time / batch size) is an average, not a latency distribution: it is reported in the results table, never as percentiles."
      : "Each sample times one isolated operation (fenced timestamps, timer cost subtracted): the distribution of individual latencies, not an average per iteration.";

    const anat = el("div", { class: "anatomy" });
    const a = sv("svg", { width: 330, height: 16, "aria-hidden": "true" });
    sv("line", { x1: 4, y1: 8, x2: 40, y2: 8, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("line", { x1: 4, y1: 3, x2: 4, y2: 13, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("text", { x: 46, y: 12, text: "min to p50" }, a);
    sv("rect", { x: 118, y: 2, width: 34, height: 12, rx: 2, style: "fill:var(--text-secondary);fill-opacity:.35;stroke:var(--text-secondary);stroke-width:1.5" }, a);
    sv("text", { x: 158, y: 12, text: "p50 to p95 (body)" }, a);
    sv("line", { x1: 262, y1: 8, x2: 290, y2: 8, style: "stroke:var(--text-secondary);stroke-width:2" }, a);
    sv("circle", { cx: 292, cy: 8, r: 4, style: "fill:var(--text-secondary)" }, a);
    sv("text", { x: 300, y: 12, text: "p99" }, a);
    anat.append(a);
    host.append(anat);

    const rows = currentRows();
    const missing = visible()
      .filter((e) => !rows.some((r) => r.name === e.name))
      .map((e) => [e.name, reasonFor(runs.get(runKey(state.family, state.size, e.name)), e.name, state.op)]);
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "No measurement for this configuration." }));
      appendMissing(host, missing);
      return;
    }
    rows.sort((x, y) => x.m.latency.p99 - y.m.latency.p99);
    const W = chartWidth(host);
    const labelW = Math.min(250, W * 0.26);
    const valsW = Math.min(360, W * 0.34);
    const x0 = labelW + 8;
    const x1 = W - valsW - 12;
    const rowH = 34;
    const top = 6;
    const H = top + rows.length * rowH + 30;
    const lo = state.ylog ? Math.min(...rows.map((r) => Math.max(r.m.latency.min, 0.1))) : 0;
    const dom = valueDomain([lo, ...rows.map((r) => r.m.latency.p99)], state.ylog, true);
    const xs = scale(dom[0], dom[1], x0, x1, state.ylog);
    const svg = sv("svg", { width: W, height: H, role: "img", "aria-label": title });
    const ticks = state.ylog ? logTicks(dom[0], dom[1]) : niceTicks(dom[0], dom[1], 5);
    for (const t of ticks) {
      const x = xs(t);
      sv("line", { x1: x, y1: top, x2: x, y2: H - 26, class: "grid" }, svg);
      sv("text", { x, y: H - 10, "text-anchor": "middle", class: "tick", text: tickNs(t) }, svg);
    }
    rows.forEach((r, i) => {
      const y = top + i * rowH + rowH / 2;
      const L = r.m.latency;
      const c = colorOf(r.name);
      const g = sv("g", {}, svg);
      sv("rect", { x: 0, y: y - rowH / 2, width: W, height: rowH, class: "rowbg" }, g);
      sv("rect", { x: 2, y: y - 10, width: 30, height: 20, rx: 4, class: "badge" }, g);
      sv("text", { x: 17, y: y + 4, "text-anchor": "middle", class: "ink", text: "#" + (i + 1) }, g);
      sv("text", { x: 40, y: y + 4, class: "ink", text: label(r.name) }, g);
      const xm = xs(Math.max(L.min, dom[0]));
      const x50 = xs(L.p50);
      const x95 = xs(L.p95);
      const x99 = xs(L.p99);
      sv("line", { x1: xm, y1: y, x2: x50, y2: y, style: `stroke:${c};stroke-width:2` }, g);
      sv("line", { x1: xm, y1: y - 6, x2: xm, y2: y + 6, style: `stroke:${c};stroke-width:2` }, g);
      sv("rect", {
        x: x50, y: y - 9, width: Math.max(3, x95 - x50), height: 18, rx: 3,
        style: `fill:${c};fill-opacity:.35;stroke:${c};stroke-width:1.5`,
      }, g);
      sv("line", { x1: x95, y1: y, x2: x99, y2: y, style: `stroke:${c};stroke-width:2` }, g);
      sv("circle", { cx: x99, cy: y, r: 4.5, style: `fill:${c};stroke:var(--surface-1);stroke-width:2` }, g);
      sv("text", {
        x: W - valsW, y: y + 4, class: "vals",
        text: `min ${fmtNs(L.min)} · p50 ${fmtNs(L.p50)} · p95 ${fmtNs(L.p95)} · p99 ${fmtNs(L.p99)}`,
      }, g);
      const hover = (evt) =>
        showTip(evt, (t) => {
          t.append(ttRow(r.name, `rank #${i + 1} by p99`), ttTitle(configLabel()));
          t.append(
            ttGrid([
              ["min", fmtNs(L.min)], ["p50", fmtNs(L.p50)], ["p90", fmtNs(L.p90)], ["p95", fmtNs(L.p95)],
              ["p99", fmtNs(L.p99)], ["max", fmtNs(L.max)], ["mean", fmtNs(L.mean)],
              ["samples", L.n.toLocaleString("en")],
              ["clock", r.m.clock === "cycles" ? `core cycles @ ${(r.m.clock_ghz || 0).toFixed(2)} GHz` : "TSC"],
              ["machine load", r.run.load_avg != null ? r.run.load_avg.toFixed(1) + " (1 min)" : "-"],
              ...(batch ? [["amortized / address", fmtNs(r.m.amortized_ns) + " (mean)"]] : []),
            ])
          );
        });
      g.addEventListener("pointermove", hover);
      g.addEventListener("pointerleave", hideTip);
    });
    sv("line", { x1: x0, y1: H - 26, x2: x1, y2: H - 26, class: "axis" }, svg);
    host.append(svg);
    appendMissing(host, missing);
  }

  // ------------------------------------------------------------ bars
  function renderBars() {
    const host = $("bars-card");
    cardHead(host, "Throughput", configLabel());
    const rows = currentRows().sort((a, b) => b.m.throughput.median - a.m.throughput.median);
    const missing = visible()
      .filter((e) => !rows.some((r) => r.name === e.name))
      .map((e) => [e.name, reasonFor(runs.get(runKey(state.family, state.size, e.name)), e.name, state.op)]);
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "No measurement for this configuration." }));
      appendMissing(host, missing);
      return;
    }
    const W = chartWidth(host);
    const labelW = Math.min(230, W * 0.25);
    const x0 = labelW;
    const x1 = W - 120;
    const rowH = 28;
    const H = rows.length * rowH + 30;
    const dom = valueDomain(rows.flatMap((r) => [r.m.throughput.max, state.ylog ? r.m.throughput.min : 0]), state.ylog, true);
    const xs = scale(dom[0], dom[1], x0, x1, state.ylog);
    const svg = sv("svg", { width: W, height: H, role: "img", "aria-label": "Throughput" });
    const ticks = state.ylog ? logTicks(dom[0], dom[1]) : niceTicks(dom[0], dom[1], 5);
    for (const t of ticks) {
      const x = xs(t);
      sv("line", { x1: x, y1: 0, x2: x, y2: H - 26, class: "grid" }, svg);
      sv("text", { x, y: H - 10, "text-anchor": "middle", class: "tick", text: tickRate(t) }, svg);
    }
    rows.forEach((r, i) => {
      const y = i * rowH + rowH / 2;
      const T = r.m.throughput;
      const c = colorOf(r.name);
      const g = sv("g", {}, svg);
      sv("rect", { x: 0, y: y - rowH / 2, width: W, height: rowH, class: "rowbg" }, g);
      sv("text", { x: labelW - 8, y: y + 4, "text-anchor": "end", class: "ink", text: label(r.name) }, g);
      const xb = xs(dom[0]);
      const xv = xs(T.median);
      sv("rect", { x: xb, y: y - 8, width: Math.max(2, xv - xb), height: 16, rx: 4, style: `fill:${c}` }, g);
      sv("line", { x1: xs(T.min), y1: y, x2: xs(T.max), y2: y, style: "stroke:var(--text-primary);stroke-width:1.5;stroke-opacity:.7" }, g);
      sv("text", { x: Math.max(xv, xs(T.max)) + 8, y: y + 4, class: "vals", text: fmtRate(T.median) }, g);
      const eng = engBy.get(r.name);
      const api = state.op === "batch" ? API[eng.batch] : state.op === "exists" ? API[eng.exists] : "single lookup";
      g.addEventListener("pointermove", (evt) =>
        showTip(evt, (t) => {
          t.append(ttRow(r.name, fmtRate(T.median)), ttTitle(configLabel()));
          t.append(
            ttGrid([
              ["median", fmtRate(T.median)], ["slowest repetition", fmtRate(T.min)], ["fastest repetition", fmtRate(T.max)],
              ["repetitions", String(T.reps)], ["addresses / repetition", T.addrs_per_rep.toLocaleString("en")],
              ["mean time / address / worker", fmtNs(r.m.amortized_ns)], ["API", api],
            ])
          );
        })
      );
      g.addEventListener("pointerleave", hideTip);
    });
    sv("line", { x1: x0, y1: H - 26, x2: x1, y2: H - 26, class: "axis" }, svg);
    host.append(svg);
    appendMissing(host, missing);
  }

  function seriesBy(valueOf) {
    return visible().map((e) => {
      const pts = new Map();
      for (const s of sizes) {
        const v = valueOf(runs.get(runKey(state.family, s, e.name)), e.name);
        if (v != null && isFinite(v) && (!state.ylog || v > 0)) pts.set(s, v);
      }
      return { name: e.name, pts };
    });
  }
  const sizeReason = (name, s) => reasonFor(runs.get(runKey(state.family, s, name)), name, null);
  const measReason = (name, s) => reasonFor(runs.get(runKey(state.family, s, name)), name, state.op);

  const MEM = {
    footprint: ["Footprint", "structure heap + mapped tables + entry table (prefix + payload)", (m) => m.footprint],
    rss: ["Resident growth", "VmRSS after the build - before the build", (m) => m.rss_delta],
    build_heap: ["Build peak (allocated)", "peak live heap during the build above the pre-build level, plus mapped tables", (m) => m.build_peak_heap],
    build_rss: ["Build peak (RSS)", "VmHWM during the build - VmRSS before it", (m) => m.build_peak_rss],
    lookup_rss: ["Lookup peak (RSS)", "VmHWM during all lookups - VmRSS before them (worker stacks, buffers, faults)", (m) => m.lookup_peak_rss],
  };

  function renderScaling() {
    const sub = `${FAMILY[state.family]} · ${OP[state.op]}${state.op === "batch" ? " of " + state.batch : ""} · ${DIST[state.dist]} · ${state.threads} thread${state.threads > 1 ? "s" : ""}`;
    $("scaling-note").textContent =
      "Points at every measured table size (numeric axis). A missing point is a configuration that was not executed (hover for the reason); lines are not drawn across gaps.";
    const tipSize = (s) => `${FAMILY[state.family]} · ${sizeLabel(s)} entries`;
    const common = { xs: sizes, xlog: state.xlog, ylog: state.ylog, xFmt: sizeLabel, tipTitle: tipSize };
    lineChart($("line-throughput"), {
      ...common, title: "Throughput vs entries", sub,
      series: seriesBy((r) => getMeasure(r, state)?.throughput.median),
      yFmt: fmtRate, tick: tickRate, reason: measReason,
    });
    for (const p of ["p50", "p95", "p99"]) {
      lineChart($("line-" + p), {
        ...common, title: `${p} latency vs entries`,
        sub: state.op === "batch" ? `per batch call of ${state.batch} · ${DIST[state.dist]}` : `per operation · ${DIST[state.dist]} · ${state.threads}T`,
        series: seriesBy((r) => getMeasure(r, state)?.latency[p]),
        yFmt: fmtNs, tick: tickNs, reason: measReason, lowerIsBetter: true, height: 220,
      });
    }
    lineChart($("line-build"), {
      ...common, title: "Build time vs entries", sub: `${FAMILY[state.family]} · from the route list, single thread`,
      series: seriesBy((r) => (r?.status === "ok" ? r.build_s : null)),
      yFmt: fmtS, tick: tickS, reason: sizeReason, lowerIsBetter: true,
    });
    const [mt, mdesc, mget] = MEM[state.mem];
    const ctr = el("div", { class: "fgroup" });
    const seg = el("div", { class: "seg", role: "group", "aria-label": "Memory metric" });
    for (const [k, [label]] of Object.entries(MEM)) {
      seg.append(el("button", {
        type: "button", text: label, "aria-pressed": String(state.mem === k),
        onclick: () => {
          state.mem = k;
          renderScaling();
        },
      }));
    }
    ctr.append(seg);
    ctr.style.marginBottom = "8px";
    lineChart($("line-memory"), {
      ...common, title: `Memory vs entries - ${mt}`, sub: mdesc, controls: ctr,
      series: seriesBy((r) => (r?.status === "ok" && r.memory ? mget(r.memory) : null)),
      yFmt: fmtBytes, tick: tickBytes, bytes: true, reason: sizeReason, lowerIsBetter: true,
    });
    lineChart($("line-bpe"), {
      ...common, title: "Bytes per stored entry vs entries", sub: "footprint / entries actually stored",
      series: seriesBy((r) => (r?.status === "ok" && r.memory ? r.memory.bytes_per_entry : null)),
      yFmt: fmtBpe, tick: tickBytes, bytes: true, reason: sizeReason, lowerIsBetter: true,
    });
  }

  // ------------------------------------------------------------ threads
  function renderThreads() {
    const ts = S.threads.slice().sort((a, b) => a - b);
    $("threads-note").textContent =
      "Every worker shares the same structure read-only; each one processes the same amount of work from a different offset of the query set, after a synchronized start. Aggregate throughput = all addresses / wall time from the first start to the last finish. Speedup is relative to one thread.";
    const series = visible().map((e) => {
      const run = runs.get(runKey(state.family, state.size, e.name));
      const pts = new Map();
      for (const t of ts) {
        const m = getMeasure(run, { ...state, threads: t });
        if (m && (!state.ylog || m.throughput.median > 0)) pts.set(t, m.throughput.median);
      }
      return { name: e.name, pts };
    });
    lineChart($("line-threads"), {
      title: "Throughput vs threads",
      sub: `${FAMILY[state.family]} · ${sizeLabel(state.size)} entries · ${OP[state.op]} · ${DIST[state.dist]}`,
      xs: ts, xlog: false, ylog: state.ylog, xFmt: (t) => t + "T",
      tipTitle: (t) => `${t} thread${t > 1 ? "s" : ""} · ${sizeLabel(state.size)} entries`,
      series, yFmt: fmtRate, tick: tickRate,
      reason: (name) => reasonFor(runs.get(runKey(state.family, state.size, name)), name, state.op),
    });
    const host = $("speedup-card");
    cardHead(host, "Speedup over one thread", `${FAMILY[state.family]} · ${sizeLabel(state.size)} entries · ${OP[state.op]} · ${DIST[state.dist]}`);
    const cols = [{ k: "name", t: "Structure" }];
    for (const t of ts) cols.push({ k: "t" + t, t: `${t}T`, num: true, fmt: fmtRate });
    for (const t of ts.slice(1)) cols.push({ k: "s" + t, t: `x ${t}T/1T`, num: true, fmt: (v) => (v == null ? "-" : "×" + v.toFixed(2)) });
    const rows = series.map((s) => {
      const row = { name: s.name };
      for (const t of ts) row["t" + t] = s.pts.get(t) ?? null;
      for (const t of ts.slice(1)) row["s" + t] = row["t" + t] != null && row["t" + ts[0]] ? row["t" + t] / row["t" + ts[0]] : null;
      return row;
    });
    host.append(table(cols, rows, { sortKey: "t" + ts[ts.length - 1], desc: true, swatch: true }));
  }

  // ------------------------------------------------------------ batch size
  /** Throughput of the batch operation per batch size: dir-24-8 with SIMD batches against scalar batches (and the loops of the other structures). */
  function renderBatchSizes() {
    const host = $("batch-section");
    const bs = [...new Set([...S.batch, ...(S.batch_sweep || [])])].sort((a, b) => a - b);
    const t1 = S.threads[0];
    const ok = S.ops.includes("batch") && bs.length > 1;
    host.hidden = !ok;
    if (!ok) return;
    $("batch-note").textContent =
      `Batch operation (lookup_batch_into for dir-24-8, a loop of single lookups for the structures without a batch API) at every measured batch size, ${t1} thread${t1 > 1 ? "s" : ""}. ` +
      `dir-24-8 is measured twice: with the batch-simd feature (batches through ${ISA[R.meta.env.batch_kernel] || R.meta.env.batch_kernel}, detected on this CPU) and built without it (scalar batch kernel). ` +
      "The extra sizes (" + (S.batch_sweep || []).join(", ") + ") are measured for native batch APIs only.";
    const st = { ...state, op: "batch", threads: t1 };
    const series = visible()
      .filter((e) => opSupported(e.name, "batch"))
      .map((e) => {
        const run = runs.get(runKey(state.family, state.size, e.name));
        const pts = new Map();
        for (const b of bs) {
          const m = getMeasure(run, { ...st, batch: b });
          if (m && (!state.ylog || m.throughput.median > 0)) pts.set(b, m.throughput.median);
        }
        return { name: e.name, pts };
      });
    const sub = `${FAMILY[state.family]} · ${sizeLabel(state.size)} entries · ${DIST[state.dist]} · ${t1} thread${t1 > 1 ? "s" : ""}`;
    lineChart($("line-batch"), {
      title: "Batch throughput vs batch size",
      sub,
      xs: bs, xlog: true, ylog: state.ylog, xFmt: (b) => String(b),
      tipTitle: (b) => `batch of ${b} · ${sizeLabel(state.size)} entries`,
      series, yFmt: fmtRate, tick: tickRate,
      reason: (name) => reasonFor(runs.get(runKey(state.family, state.size, name)), name, "batch"),
    });
    const card = $("batch-speedup-card");
    cardHead(card, "SIMD batches over scalar batches", sub);
    const simd = series.find((s) => s.name === "dir-24-8");
    const scalar = series.find((s) => s.name === "dir-24-8[scalar]");
    const cols = [
      { k: "b", t: "Batch size", num: true, fmt: (v) => String(v) },
      { k: "simd", t: label("dir-24-8"), num: true, fmt: fmtRate },
      { k: "scalar", t: label("dir-24-8[scalar]"), num: true, fmt: fmtRate },
      { k: "x", t: "SIMD / scalar", num: true, fmt: (v) => (v == null ? "-" : "×" + v.toFixed(2)) },
    ];
    const rows = bs.map((b) => {
      const a = simd?.pts.get(b) ?? null;
      const c = scalar?.pts.get(b) ?? null;
      return { b, simd: a, scalar: c, x: a != null && c ? a / c : null };
    });
    card.append(table(cols, rows, { sortKey: "b", desc: false }));
  }

  function renderResults() {
    const host = clear($("results-table"));
    $("results-note").textContent =
      `${FAMILY[state.family]} · ${OP[state.op]}${state.op === "batch" ? " of " + state.batch : ""} · ${DIST[state.dist]} · ${state.threads} thread(s), every size. ` +
      (state.op === "batch"
        ? "Latency columns are per batch call; \"amortized\" is the mean time per address per worker (not a distribution)."
        : "Latency columns are per isolated operation; \"amortized\" is the mean time per address per worker under throughput load.");
    const rows = [];
    for (const s of sizes) {
      for (const e of visible()) {
        const run = runs.get(runKey(state.family, s, e.name));
        const m = getMeasure(run, state);
        const L = m?.latency;
        rows.push({
          size: s, entries: run?.entries || null, name: e.name,
          status: !run ? "not-run" : run.status === "ok" && !m ? (opSupported(e.name, state.op) ? "not-run" : "not-applicable") : run.status,
          tp: m?.throughput.median ?? null, am: m?.amortized_ns ?? null,
          min: L?.min ?? null, p50: L?.p50 ?? null, p90: L?.p90 ?? null, p95: L?.p95 ?? null, p99: L?.p99 ?? null, max: L?.max ?? null,
          build: run?.status === "ok" ? run.build_s : null, fp: run?.memory && run.status === "ok" ? run.memory.footprint : null,
          load: run?.load_avg ?? null,
          bpe: run?.memory && run.status === "ok" ? run.memory.bytes_per_entry : null,
          api: state.op === "batch" ? API[e.batch] : state.op === "exists" ? API[e.exists] : opSupported(e.name, "lookup") ? "single lookup" : API.unsupported,
          why: m ? "" : reasonFor(run, e.name, state.op),
        });
      }
    }
    const missing = (r) => (r.why ? "-" : "-");
    host.append(
      table(
        [
          { k: "size", t: "Entries", num: true, fmt: sizeLabel },
          { k: "name", t: "Structure" },
          { k: "status", t: "Status", status: true },
          { k: "tp", t: "Throughput", num: true, fmt: fmtRate, missing },
          { k: "am", t: "Amortized", num: true, fmt: fmtNs, title: "mean time per address per worker" },
          { k: "min", t: "min", num: true, fmt: fmtNs },
          { k: "p50", t: "p50", num: true, fmt: fmtNs },
          { k: "p90", t: "p90", num: true, fmt: fmtNs },
          { k: "p95", t: "p95", num: true, fmt: fmtNs },
          { k: "p99", t: "p99", num: true, fmt: fmtNs },
          { k: "max", t: "max", num: true, fmt: fmtNs },
          { k: "build", t: "Build", num: true, fmt: fmtS },
          { k: "fp", t: "Footprint", num: true, fmt: fmtBytes },
          { k: "bpe", t: "B/entry", num: true, fmt: fmtBpe },
          { k: "api", t: "API" },
          { k: "load", t: "Load", num: true, fmt: (v) => v.toFixed(1), title: "1-minute load average of the machine when this structure was measured" },
          { k: "why", t: "Reason (not measured)", wrap: true },
        ],
        rows,
        { sortKey: "size", desc: false, swatch: true }
      )
    );
  }

  function renderMemory() {
    const host = clear($("memory-table"));
    $("memory-note").textContent =
      `${FAMILY[state.family]}, every size. Footprint = structure (live heap allocated by the build, counting allocator) + tables mapped outside the allocator (mmap) + the entry table competitors need to return prefix and payload (dir-24-8 stores them inside its structure). Inputs (route list, queries, expected results) and per-worker buffers are measured separately and excluded from the footprint. Resident values come from /proc/self (VmRSS, VmHWM reset with clear_refs before each phase).`;
    const rows = [];
    for (const s of sizes) {
      for (const e of visible()) {
        const run = runs.get(runKey(state.family, s, e.name));
        const m = run?.status === "ok" ? run.memory : null;
        rows.push({
          size: s, name: e.name, status: run ? run.status : "not-run",
          fp: m?.footprint ?? null, bpe: m?.bytes_per_entry ?? null, heap: m?.struct_heap ?? null,
          mapped: m?.struct_mapped ?? null, entry: m?.entry_table ?? null, rss: m?.rss_delta ?? null,
          bheap: m?.build_peak_heap ?? null, brss: m?.build_peak_rss ?? null, lrss: m?.lookup_peak_rss ?? null,
          lheap: m?.lookup_peak_heap ?? null, inputs: m?.inputs ?? null, buffers: m?.buffers ?? null,
          reported: m?.reported ?? null, build: run?.status === "ok" ? run.build_s : null,
        });
      }
    }
    host.append(
      table(
        [
          { k: "size", t: "Entries", num: true, fmt: sizeLabel },
          { k: "name", t: "Structure" },
          { k: "status", t: "Status", status: true },
          { k: "fp", t: "Footprint", num: true, fmt: fmtBytes },
          { k: "bpe", t: "B/entry", num: true, fmt: fmtBpe },
          { k: "heap", t: "Structure heap", num: true, fmt: fmtBytes },
          { k: "mapped", t: "Mapped", num: true, fmt: fmtBytes },
          { k: "entry", t: "Entry table", num: true, fmt: fmtBytes },
          { k: "rss", t: "RSS growth", num: true, fmt: fmtBytes },
          { k: "bheap", t: "Build peak (alloc)", num: true, fmt: fmtBytes },
          { k: "brss", t: "Build peak (RSS)", num: true, fmt: fmtBytes },
          { k: "lrss", t: "Lookup peak (RSS)", num: true, fmt: fmtBytes },
          { k: "lheap", t: "Lookup heap", num: true, fmt: fmtBytes, title: "heap allocated during all lookup phases (thread bookkeeping)" },
          { k: "inputs", t: "Inputs", num: true, fmt: fmtBytes },
          { k: "buffers", t: "Buffers", num: true, fmt: fmtBytes },
          { k: "reported", t: "Self-reported", num: true, fmt: fmtBytes, title: "the structure's own accounting" },
          { k: "build", t: "Build", num: true, fmt: fmtS },
        ],
        rows,
        { sortKey: "size", desc: false, swatch: true }
      )
    );
  }

  function renderStatus() {
    const host = clear($("status-table"));
    const rows = R.runs
      .filter((r) => r.status !== "ok")
      .map((r) => ({ fam: FAMILY[r.family], size: r.size, name: r.engine, status: r.status, why: r.reason || "" }));
    for (const e of ENG) {
      for (const op of S.ops) {
        if (!opSupported(e.name, op))
          rows.push({ fam: e.families.map((f) => FAMILY[f]).join(", "), size: null, name: e.name, status: "not-applicable", why: `${OP[op]}: ${API.unsupported} (${e.algorithm})` });
      }
      const fams = e.families.map((f) => FAMILY[f]).join(", ");
      if (e.batch === "loop" && S.ops.includes("batch"))
        rows.push({ fam: fams, size: null, name: e.name, status: "adapted", why: "Batch: no native batch API, measured as a loop of single lookups" });
      if (e.exists === "derived" && S.ops.includes("exists"))
        rows.push({ fam: fams, size: null, name: e.name, status: "adapted", why: "Exists: no dedicated API, measured as lookup(ip).is_some()" });
    }
    if (!rows.length) {
      host.append(el("div", { class: "empty", text: "Every configuration was measured." }));
      return;
    }
    host.append(
      table(
        [
          { k: "fam", t: "Family" },
          { k: "size", t: "Entries", num: true, fmt: sizeLabel, missing: () => "all" },
          { k: "name", t: "Structure" },
          { k: "status", t: "Status", status: true },
          { k: "why", t: "Reason / adaptation", wrap: true },
        ],
        rows,
        { sortKey: "size", desc: false, swatch: true }
      )
    );
  }

  function renderDatasets() {
    const host = clear($("datasets-table"));
    $("datasets-note").textContent =
      `Entries are distinct normalized CIDR prefixes actually stored (BGP-like length mix); queries are separate (${S.queries.toLocaleString("en")} per distribution, fixed seed ${S.seed}). Every query's expected result comes from an independent oracle; "only absent" addresses are covered by no prefix. A dataset is generated once and reused from the cache while its parameters do not change.`;
    const rows = R.datasets.map((d) => {
      const top = d.lengths.slice().sort((a, b) => b[1] - a[1]).slice(0, 3).map(([l, c]) => `/${l} ${pct(c / d.entries)}`).join(", ");
      return {
        fam: FAMILY[d.family], size: d.size, entries: d.entries, queries: d.queries,
        hr: d.hit_ratio.map((h, i) => `${["random", "hits", "misses"][i]} ${pct(h)}`).join(" · "),
        lens: `/${d.lengths[0][0]}../${d.lengths[d.lengths.length - 1][0]} (top: ${top})`,
        nopl: d.without_payload, gen: d.generate_s, cached: d.cached ? "reused" : "generated", bytes: d.file_bytes,
      };
    });
    host.append(
      table(
        [
          { k: "fam", t: "Family" },
          { k: "size", t: "Requested", num: true, fmt: sizeLabel },
          { k: "entries", t: "Entries stored", num: true, fmt: (v) => v.toLocaleString("en") },
          { k: "queries", t: "Queries / distribution", num: true, fmt: (v) => v.toLocaleString("en") },
          { k: "hr", t: "Measured hit ratio" },
          { k: "lens", t: "Prefix lengths", wrap: true },
          { k: "nopl", t: "Without payload", num: true, fmt: (v) => v.toLocaleString("en") },
          { k: "gen", t: "Generation", num: true, fmt: fmtS },
          { k: "cached", t: "This run" },
          { k: "bytes", t: "Cache file", num: true, fmt: fmtBytes },
        ],
        rows,
        { sortKey: "size", desc: false }
      )
    );
  }

  // ------------------------------------------------------------ static text
  function renderMethod() {
    const env = R.meta.env;
    const host = clear($("method-text"));
    const sec = (title, ...paras) => {
      host.append(el("h3", { text: title }));
      for (const p of paras) host.append(el("p", { text: p }));
    };
    sec(
      "Data",
      `For each family and size, one table of distinct BGP-like prefixes (IPv4 /8../32, mostly /24, clustered inside 1.0.0.0-223.255.255.255; IPv6 /16../128, mostly /48 and /32, clustered inside 2000::/3) with a payload for three routes in four. The size is the number of prefixes stored, distinct from the number of queries. Every structure gets the same routes, payloads and queries, in the same order.`,
      `Distributions: "only existing" picks a random route and a random address inside it; "only absent" draws addresses covered by no prefix (half next to a route, inside its prefix widened by 8 bits, half uniformly over the family space; IPv6: 2000::/3), verified with the oracle; "random" shuffles hits and misses with ${trim(hitPct)} % hits. Structures that return the longest matching prefix keep the same semantics; membership-only structures (bitmaps) answer only "exists".`
    );
    sec(
      "Operations",
      `Lookup: one address per call, the result is the winning prefix and a borrowed payload (Option<LookupMatch>), the same type for every structure; competitors map the address to a route index and resolve it through the shared entry table. Batch: ${S.batch.join(", ")} addresses per call through the native batch API when the structure has one (dir-24-8 lookup_batch_into), otherwise a loop of single lookups, as stated in the tables${S.batch_sweep?.length ? `; native batch APIs are also measured with ${S.batch_sweep.join(", ")} addresses per call on ${S.threads[0]} thread${S.threads[0] > 1 ? "s" : ""} (batch-size chart)` : ""}. dir-24-8 is built twice: with the batch-simd feature (entry dir-24-8, batches through the SIMD kernel detected on the CPU) and without it (dir-24-8[scalar], scalar batch kernel); single lookups and exists run the scalar kernels in both. Exists: a boolean through the dedicated API when there is one (dir-24-8 exist, bitmaps), otherwise lookup(ip).is_some().`
    );
    sec(
      "Validation",
      `Every generated query has its expected result computed by the oracle when the dataset is generated (hits are checked to be covered, misses to be uncovered). Before any timing, ${S.validate ? "the first " + S.validate.toLocaleString("en") : "every"} query of every selected distribution goes through every measured operation (and every batch size) of the structure and is compared with that expected result (winning prefix and payload, or the boolean). A mismatch marks the structure "invalid" and it is not measured. Results go through std::hint::black_box so that no lookup can be optimized away.`
    );
    sec(
      "Throughput",
      `Per configuration: a warm-up of ${S.warmup_ms} ms calibrates the cost of one unit on the coordinator thread; each worker then processes the number of addresses that takes ~${S.window_ms} ms, ${S.reps} timed repetitions after one discarded warm-up repetition. Workers are spawned and their buffers allocated before timing, then start together on a spin flag; each worker's cursor keeps advancing through the query set across repetitions and into the latency pass, so recently used addresses are never replayed. Aggregate throughput = addresses of all workers / (last worker end - first worker start); the median repetition is reported with the slowest and fastest. Amortized cost = wall time x threads / addresses (mean time per address per worker).`
    );
    sec(
      "Latency",
      `Single-address operations: ${S.lat_samples.toLocaleString("en")} samples per configuration (split between the workers, which run concurrently; for slow structures fewer, so that a pass lasts about ${S.lat_max_ms || 100} ms, never below 2,000), each sample timing one isolated call between fenced timestamps; the cost of an empty timed section is measured and subtracted. Clock: ${R.meta.timer_source}. Consecutive fenced time-stamp-counter reads on this CPU differ by ${R.meta.tsc_granularity_ns.toFixed(1)} ns most of the time (AMD Zen advances its TSC in 10 ns steps), too coarse for single lookups, which is why the cycle counter is preferred when the kernel allows it (perf_event_paranoid <= 2). The cycle counter counts user-mode cycles: time spent in interrupts is not part of a sample. Batch operations: ${S.lat_batches.toLocaleString("en")} samples of one whole batch call each (same cap, never below 200). The sample count of every configuration is in its tooltip. Percentiles are nearest-rank over all samples of the configuration; they are never derived from per-iteration averages.`
    );
    sec(
      "Threads",
      `1, 4 and 8 threads share one structure read-only (every structure here is Sync). Each worker covers the same amount of work starting at its own offset of the query set. Workers are ${S.pin ? "pinned to distinct physical cores first (CPU order " + S.cpu_order.join(",") + ")" : "not pinned"}. The coordinator blocks while workers run. Structures run one after the other, in separate processes, so they never compete for the machine.`
    );
    sec(
      "Memory",
      `Measured in the process that builds the structure, one structure per process. Structure heap: live bytes of the counting global allocator after the build minus before it (fixed allocations included). Mapped: tables the structure maps outside the allocator (dir-24-8 mmap roots, reported by its stats). Entry table: the prefix + payload array competitors need to return a full result (dir-24-8 keeps them inside). Footprint = the three; bytes per entry = footprint / entries stored. Build peak: allocator peak during the build (plus mapped) and VmHWM growth during the build. Lookup peak: VmHWM growth during all lookup phases (worker stacks, page faults, buffers), heap growth reported separately. Inputs (route list, entry table, queries, expected results) and per-worker buffers are reported apart.`
    );
    sec(
      "Limits and duration",
      `Memory limit per structure: ${S.mem_limit_rule}.${S.mem_limit_bytes ? " Each process runs with RLIMIT_DATA set to the limit plus its inputs, so an allocation beyond it fails instead of swapping; larger sizes are skipped when the extrapolation of the two previous sizes exceeds the limit." : " Without a limit every size is attempted: a structure larger than the free memory makes the machine swap, which slows its measurements down (they are reported as measured)."} Sizes whose extrapolated build time exceeds the ${S.timeout_s} s timeout are skipped. Total budget: ${S.budget_s ? S.budget_s + " s" : "none"}. Datasets are generated once per family and size and cached; the binary is built once and re-executes itself for each measurement process; each structure is built once per size and reused for every operation, distribution and thread count.`
    );
    sec(
      "Caveats",
      `Measured on one machine (${env.cpu}, governor ${env.governor || "unknown"}, boost ${env.boost}), not in isolation: background load adds noise, visible as the spread between repetitions; the load average when each structure was measured is in the results table (${loadSummary() || "not recorded"}). The query sets (${S.queries.toLocaleString("en")} addresses per distribution) are larger than the caches so that random lookups measure the memory system rather than the share of the last-level cache left by other processes; with small query sets the same configuration varied by up to 5x between processes on this machine. Single-thread memory-bound lookups can be slower per thread than with several threads on mobile CPUs whose memory fabric clocks down at low load. Results for structures that do not support an operation or a family are listed in the "not executed" table.`
    );
  }

  function renderEnv() {
    const e = R.meta.env;
    clear($("env-table")).append(
      kvTable([
        ["CPU", e.cpu], ["Architecture", e.arch], ["Cores", `${e.physical_cores ?? "?"} physical, ${e.logical_cpus} logical`],
        ["Max frequency", e.cpu_max_mhz ? e.cpu_max_mhz.toFixed(0) + " MHz" : null], ["Governor / boost", `${e.governor} / ${e.boost}`],
        ["Caches (cpu0)", e.caches], ["TSC flags", e.tsc_flags],
        ["RAM", `${fmtBytes(e.ram_total)} total, ${fmtBytes(e.ram_available_at_start)} available at start, swap used ${fmtBytes(e.swap_used_at_start)}`],
        ["OS / kernel", `${e.os} / ${e.kernel}`], ["Transparent huge pages", e.thp],
        ["SIMD detected", e.simd_detected.join(", ")],
        ["dir-24-8 batch kernel", `${e.batch_kernel} (entry dir-24-8, feature batch-simd); dir-24-8[scalar] is the same code built without batch-simd; single lookups are scalar in both`],
        ["Compiler", `rustc ${e.rustc} (LLVM ${e.llvm}), target ${e.target}`],
        ["Build profile", `${e.profile}, opt-level ${e.opt_level}, RUSTFLAGS "${e.rustflags}"`],
        ["Compile-time target features", e.compile_target_features],
        ["Dependencies", e.dependencies.map(([n, v]) => `${n} ${v}`).join(", ")],
        ["Latency clock", R.meta.timer_source], ["TSC", `${(R.meta.ticks_per_s / 1e9).toFixed(3)} GHz, fenced-read resolution ${R.meta.tsc_granularity_ns.toFixed(1)} ns`],
        ["Load average (1 min)", loadSummary()],
        ["Command", R.meta.command], ["Tool", R.meta.tool], ["Total run time", fmtDuration(R.meta.duration_s)],
      ])
    );
    clear($("settings-table")).append(
      kvTable([
        ["Sizes (entries)", S.sizes.map(sizeLabel).join(", ")], ["Families", S.families.map((f) => FAMILY[f]).join(", ")],
        ["Operations", S.ops.map((o) => OP[o]).join(", ")], ["Distributions", S.dists.map((d) => DIST[d]).join(", ")],
        ["Threads", S.threads.join(", ")], ["Batch sizes", S.batch.join(", ") + (S.batch_sweep?.length ? ` (native batch APIs also ${S.batch_sweep.join(", ")} with ${S.threads[0]} thread${S.threads[0] > 1 ? "s" : ""})` : "")],
        ["Queries per distribution", S.queries.toLocaleString("en")], ["Validated before timing", S.validate ? S.validate.toLocaleString("en") + " per distribution and operation" : "all"], ["Seed", S.seed],
        ["Warm-up / window / repetitions", `${S.warmup_ms} ms / ${S.window_ms} ms / ${S.reps} (+1 discarded)`],
        ["Latency samples", `${S.lat_samples.toLocaleString("en")} per single-address configuration, ${S.lat_batches.toLocaleString("en")} batch calls per batch configuration`],
        ["Memory limit", S.mem_limit_rule], ["Time budget / timeout", `${S.budget_s ? S.budget_s + " s" : "none"} / ${S.timeout_s} s per process`],
        ["Thread pinning", S.pin ? "on (" + S.cpu_order.join(",") + ")" : "off"], ["Structure selection", S.engines.length ? S.engines.join(", ") : "default"],
      ])
    );
  }
  function loadSummary() {
    const l = R.runs.map((r) => r.load_avg).filter((v) => v != null);
    if (!l.length) return null;
    const f = (v) => (v == null ? "-" : v.toFixed(1));
    return `start ${f(R.meta.load_start)}, end ${f(R.meta.load_end)}; over the ${l.length} measuring processes: min ${f(Math.min(...l))}, median ${f(l.slice().sort((a, b) => a - b)[l.length >> 1])}, max ${f(Math.max(...l))} (${R.meta.env.logical_cpus} logical CPUs)`;
  }
  function renderCatalog() {
    const host = clear($("catalog-table"));
    const rows = ENG.map((e) => ({
      name: e.name, id: e.name, alg: e.algorithm, cfg: e.config, src: e.source, fam: e.families.map((f) => FAMILY[f]).join(", "),
      lookup: API[e.lookup] === API.native ? "yes" : API[e.lookup], batch: API[e.batch], exists: API[e.exists],
      entry: e.entry_table ? "shared entry table" : "inside the structure", upd: e.updates, def: e.default ? "default" : "on request",
    }));
    host.append(
      table(
        [
          { k: "name", t: "Structure" }, { k: "id", t: "ENGINES name" }, { k: "alg", t: "Algorithm", wrap: true }, { k: "cfg", t: "Configuration", wrap: true },
          { k: "src", t: "Source" }, { k: "fam", t: "Families" }, { k: "lookup", t: "Lookup" }, { k: "batch", t: "Batch" },
          { k: "exists", t: "Exists" }, { k: "entry", t: "Prefix + payload" }, { k: "upd", t: "Updates" }, { k: "def", t: "Selection" },
        ],
        rows,
        { sortKey: null, swatch: true }
      )
    );
    $("footer").textContent = `Generated by ${R.meta.tool}. Raw data: results.json and results.csv next to this file; regenerate this page without measuring with "make bench-compare REPORT_ONLY=1".`;
  }

  // ------------------------------------------------------------ main
  function renderAll() {
    hideTip();
    renderFilters();
    renderLegend();
    renderOverview();
    renderCandles();
    renderBars();
    renderScaling();
    renderThreads();
    renderBatchSizes();
    renderMemory();
    renderResults();
  }
  renderHeader();
  renderStatus();
  renderDatasets();
  renderMethod();
  renderEnv();
  renderCatalog();
  renderAll();
  let rt = 0;
  window.addEventListener("resize", () => {
    clearTimeout(rt);
    rt = setTimeout(renderAll, 150);
  });
})();

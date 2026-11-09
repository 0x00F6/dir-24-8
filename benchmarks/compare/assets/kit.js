"use strict";
/**
 * Report kit shared by the two tabs of report.html (structures.js and
 * libraries.js): formatting, DOM and SVG
 * helpers, tooltip, scales and ticks, filter buttons, legend swatches,
 * line chart and sortable table. Series styling comes from the caller.
 *
 *   const K = Kit({ colorOf, dashed, label, status: () => STATUS, onChange });
 */
window.Kit = (opts) => {
  const o = Object.assign(
    { colorOf: () => "var(--s1)", dashed: () => false, label: (n) => n, status: () => ({}), onChange: () => {} },
    opts
  );
  const NS = "http://www.w3.org/2000/svg";
  const colorOf = (n) => o.colorOf(n);
  const dashed = (n) => o.dashed(n);
  const label = (n) => o.label(n);

  // ------------------------------------------------------------ formatting
  const trim = (v) => String(+v.toFixed(1));
  const sizeLabel = (n) =>
    n >= 1e6 && n % 1e5 === 0 ? trim(n / 1e6) + "M" : n >= 1e3 && n % 100 === 0 ? trim(n / 1e3) + "k" : String(n);
  const fmtNs = (v) => {
    if (v == null || !isFinite(v)) return "-";
    if (v < 10) return v.toFixed(2) + " ns";
    if (v < 100) return v.toFixed(1) + " ns";
    if (v < 1e3) return v.toFixed(0) + " ns";
    if (v < 1e6) return (v / 1e3).toFixed(v < 1e4 ? 2 : 1) + " µs";
    return (v / 1e6).toFixed(2) + " ms";
  };
  const fmtRate = (v) => {
    if (v == null || !isFinite(v)) return "-";
    if (v >= 1e9) return (v / 1e9).toFixed(2) + " G IP/s";
    if (v >= 1e6) return (v / 1e6).toFixed(v >= 1e8 ? 0 : v >= 1e7 ? 1 : 2) + " M IP/s";
    return (v / 1e3).toFixed(1) + " k IP/s";
  };
  const fmtBytes = (b) => {
    if (b == null || !isFinite(b)) return "-";
    const a = Math.abs(b);
    if (a < 1024) return b.toFixed(0) + " B";
    if (a < 1048576) return (b / 1024).toFixed(1) + " KiB";
    if (a < 1073741824) return (b / 1048576).toFixed(a < 10485760 ? 2 : 1) + " MiB";
    return (b / 1073741824).toFixed(2) + " GiB";
  };
  const fmtS = (s) => {
    if (s == null || !isFinite(s)) return "-";
    if (s < 1e-3) return (s * 1e6).toFixed(0) + " µs";
    if (s < 1) return (s * 1e3).toFixed(s < 0.01 ? 2 : 1) + " ms";
    return s.toFixed(2) + " s";
  };
  const fmtBpe = (v) => (v == null || !isFinite(v) ? "-" : v >= 1024 ? fmtBytes(v) : v.toFixed(v < 10 ? 2 : 1) + " B");
  // Axis labels: shortest exact form of a tick value.
  const short = (v) => String(+v.toPrecision(3));
  const tickRate = (v) => (v === 0 ? "0" : v >= 1e9 ? short(v / 1e9) + " G" : v >= 1e6 ? short(v / 1e6) + " M" : short(v / 1e3) + " k");
  const tickNs = (v) => (v === 0 ? "0" : v < 1e3 ? short(v) + " ns" : v < 1e6 ? short(v / 1e3) + " \u00b5s" : short(v / 1e6) + " ms");
  const tickS = (v) => (v === 0 ? "0" : v < 1e-3 ? short(v * 1e6) + " \u00b5s" : v < 1 ? short(v * 1e3) + " ms" : short(v) + " s");
  const tickBytes = (v) => {
    if (v === 0) return "0";
    const u = ["B", "KiB", "MiB", "GiB", "TiB"];
    let i = 0;
    while (Math.abs(v) >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return short(v) + " " + u[i];
  };
  const pct = (v) => (v * 100).toFixed(1) + " %";

  // ------------------------------------------------------------ DOM helpers
  function el(tag, attrs, ...kids) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null) continue;
      if (k === "class") n.className = v;
      else if (k === "text") n.textContent = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else n.setAttribute(k, v);
    }
    for (const k of kids) if (k != null) n.append(k);
    return n;
  }
  function sv(tag, attrs, parent) {
    const n = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null) continue;
      if (k === "text") n.textContent = v;
      else if (k === "style") n.setAttribute("style", v);
      else n.setAttribute(k, v);
    }
    if (parent) parent.append(n);
    return n;
  }
  const clear = (n) => {
    while (n.firstChild) n.firstChild.remove();
    return n;
  };

  // ------------------------------------------------------------ tooltip
  const tip = document.getElementById("tooltip");
  function showTip(evt, build) {
    clear(tip);
    build(tip);
    tip.hidden = false;
    const pad = 14;
    const r = tip.getBoundingClientRect();
    let x = evt.clientX + pad;
    let y = evt.clientY + pad;
    if (x + r.width > window.innerWidth - 8) x = evt.clientX - r.width - pad;
    if (y + r.height > window.innerHeight - 8) y = evt.clientY - r.height - pad;
    tip.style.left = Math.max(4, x) + "px";
    tip.style.top = Math.max(4, y) + "px";
  }
  const hideTip = () => {
    tip.hidden = true;
  };
  const ttTitle = (t) => el("div", { class: "tt-title", text: t });
  const ttRow = (name, value, muted) => {
    const key = el("span", { class: "tt-key" });
    key.style.borderTopColor = colorOf(name);
    if (dashed(name)) key.style.borderTopStyle = "dashed";
    const v = el(muted ? "span" : "b", { text: value, class: muted ? "muted" : null });
    return el("div", { class: "tt-row" }, key, v, el("span", { class: "nm", text: label(name) }));
  };
  const ttGrid = (pairs) => {
    const g = el("div", { class: "tt-grid" });
    for (const [k, v] of pairs) g.append(el("span", { text: k }), el("span", { text: v }));
    return g;
  };


  // ------------------------------------------------------------ scales
  function scale(d0, d1, r0, r1, log) {
    if (log) {
      const l0 = Math.log10(d0);
      const l1 = Math.log10(d1);
      const f = (v) => r0 + ((Math.log10(Math.max(v, d0)) - l0) / (l1 - l0 || 1)) * (r1 - r0);
      f.log = true;
      f.d = [d0, d1];
      return f;
    }
    const f = (v) => r0 + ((v - d0) / (d1 - d0 || 1)) * (r1 - r0);
    f.log = false;
    f.d = [d0, d1];
    return f;
  }
  function niceTicks(lo, hi, n) {
    const span = hi - lo || 1;
    const step0 = span / n;
    const mag = Math.pow(10, Math.floor(Math.log10(step0)));
    const m = step0 / mag;
    const step = (m < 1.5 ? 1 : m < 3 ? 2 : m < 7 ? 5 : 10) * mag;
    const out = [];
    for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step) out.push(+v.toPrecision(12));
    return out;
  }
  /** Linear ticks on a power-of-1024 unit (1 MiB, 2 MiB, 5 MiB...). */
  function byteTicks(lo, hi, n) {
    let unit = 1;
    while (hi / unit >= 1024) unit *= 1024;
    return niceTicks(lo / unit, hi / unit, n).map((t) => t * unit);
  }
  /** Log ticks on binary units: 1, 10 and 100 B, KiB, MiB, GiB. */
  function byteLogTicks(lo, hi) {
    const out = [];
    for (let unit = 1; unit <= hi * 1024; unit *= 1024) {
      for (const m of [1, 10, 100]) {
        const v = m * unit;
        if (v >= lo * 0.999 && v <= hi * 1.001) out.push(v);
      }
    }
    return out.length >= 2 ? out : logTicks(lo, hi);
  }
  function logTicks(lo, hi) {
    const out = [];
    const a = Math.floor(Math.log10(lo));
    const b = Math.ceil(Math.log10(hi));
    for (let e = a; e <= b; e++) {
      for (const m of b - a <= 2 ? [1, 2, 5] : [1]) {
        const v = m * Math.pow(10, e);
        if (v >= lo * 0.999 && v <= hi * 1.001) out.push(v);
      }
    }
    return out;
  }
  function valueDomain(vals, log, zeroBase, bytes) {
    const v = vals.filter((x) => x != null && isFinite(x) && (!log || x > 0));
    if (!v.length) return null;
    let lo = Math.min(...v);
    let hi = Math.max(...v);
    if (log) {
      lo = Math.pow(10, Math.floor(Math.log10(lo) * 4) / 4);
      hi = Math.pow(10, Math.ceil(Math.log10(hi) * 4) / 4);
      if (hi <= lo) hi = lo * 10;
      return [lo, hi];
    }
    if (zeroBase) lo = 0;
    if (hi === lo) hi = lo + 1;
    const t = bytes ? byteTicks(lo, hi * 1.0001, 5) : niceTicks(lo, hi, 5);
    const last = t[t.length - 1];
    // Extend to the next tick so the top value sits under a labelled line.
    const step = t.length > 1 ? t[1] - t[0] : hi;
    return [lo, last >= hi ? last : last + step];
  }


  function segGroup(label, options, get, set, disabled) {
    const seg = el("div", { class: "seg", role: "group", "aria-label": label });
    for (const [value, text] of options) {
      const b = el("button", {
        type: "button",
        text,
        "aria-pressed": String(get() === value),
        onclick: () => {
          set(value);
          o.onChange();
        },
      });
      if (disabled && disabled(value)) b.disabled = true;
      seg.append(b);
    }
    return el("div", { class: "fgroup" }, el("span", { class: "flabel", text: label }), seg);
  }

  function swatch(name, size) {
    const s = sv("svg", { width: 26, height: 12, "aria-hidden": "true" });
    sv("line", {
      x1: 1, y1: 6, x2: 25, y2: 6,
      style: `stroke:${colorOf(name)};stroke-width:2;${dashed(name) ? "stroke-dasharray:5 3" : ""}`,
    }, s);
    markerAt(s, name, 13, 6, size || 3.5);
    return s;
  }
  function markerAt(g, name, x, y, r) {
    const st = `fill:${colorOf(name)};stroke:var(--surface-1);stroke-width:2`;
    if (dashed(name)) return sv("rect", { x: x - r, y: y - r, width: 2 * r, height: 2 * r, style: st }, g);
    return sv("circle", { cx: x, cy: y, r, style: st }, g);
  }

  // ------------------------------------------------------------ chart frame
  function chartWidth(host) {
    return Math.max(320, Math.floor(host.clientWidth - 36));
  }
  function cardHead(host, title, sub) {
    clear(host);
    host.append(el("h3", { text: title }));
    if (sub) host.append(el("p", { class: "csub", text: sub }));
  }


  function appendMissing(host, missing) {
    if (!missing.length) return;
    const p = el("p", { class: "missing" }, document.createTextNode("Not measured here: "));
    missing.forEach(([n, why], i) => {
      if (i) p.append(document.createTextNode("; "));
      p.append(el("span", { text: label(n) }), document.createTextNode(` (${why})`));
    });
    host.append(p);
  }

  // ------------------------------------------------------------ line charts
  /**
   * series: [{name, pts: Map(x -> y)}]; reason(name, x) explains a gap.
   */
  function lineChart(host, o) {
    cardHead(host, o.title, o.sub);
    if (o.controls) host.append(o.controls);
    const series = o.series.filter((s) => s.pts.size);
    if (!series.length) {
      host.append(el("div", { class: "empty", text: "No measurement for this selection." }));
      return;
    }
    const W = chartWidth(host);
    const H = o.height || 250;
    const m = { l: 66, r: 14, t: 10, b: 34 };
    const xsV = o.xs;
    const xlog = o.xlog && xsV[0] > 0;
    const xpad = xlog ? 1.25 : (xsV[xsV.length - 1] - xsV[0]) * 0.03 || 1;
    const xs = xlog
      ? scale(xsV[0] / xpad, xsV[xsV.length - 1] * xpad, m.l, W - m.r, true)
      : scale(Math.max(0, xsV[0] - xpad), xsV[xsV.length - 1] + xpad, m.l, W - m.r, false);
    const all = series.flatMap((s) => [...s.pts.values()]);
    const dom = o.domain || valueDomain(all, o.ylog, o.zero !== false, o.bytes);
    const ys = scale(dom[0], dom[1], H - m.b, m.t, o.ylog);
    const svg = sv("svg", { width: W, height: H, role: "img", "aria-label": o.title });
    const yt = o.ylog
      ? o.bytes ? byteLogTicks(dom[0], dom[1]) : logTicks(dom[0], dom[1])
      : o.bytes ? byteTicks(dom[0], dom[1], 5) : niceTicks(dom[0], dom[1], 5);
    for (const t of yt) {
      const y = ys(t);
      sv("line", { x1: m.l, y1: y, x2: W - m.r, y2: y, class: "grid" }, svg);
      sv("text", { x: m.l - 8, y: y + 4, "text-anchor": "end", class: "tick", text: (o.tick || o.yFmt)(t) }, svg);
    }
    // Every point gets a tick; labels that would overlap the previous one
    // are skipped (the tooltip still names the exact size).
    let lastRight = -Infinity;
    for (const x of xsV) {
      const label = o.xFmt(x);
      const px = xs(x);
      const half = label.length * 3.6 + 4;
      sv("line", { x1: px, y1: H - m.b, x2: px, y2: H - m.b + 4, class: "axis" }, svg);
      if (px - half < lastRight) continue;
      sv("text", { x: px, y: H - 12, "text-anchor": "middle", class: "tick", text: label }, svg);
      lastRight = px + half;
    }
    sv("line", { x1: m.l, y1: H - m.b, x2: W - m.r, y2: H - m.b, class: "axis" }, svg);
    for (const s of series) {
      const c = colorOf(s.name);
      let d = "";
      let prevIn = false;
      for (const x of xsV) {
        const v = s.pts.get(x);
        if (v == null) {
          prevIn = false;
          continue;
        }
        d += (prevIn ? "L" : "M") + xs(x).toFixed(1) + "," + ys(v).toFixed(1);
        prevIn = true;
      }
      sv("path", {
        d,
        style: `fill:none;stroke:${c};stroke-width:2;stroke-linejoin:round;${dashed(s.name) ? "stroke-dasharray:6 3" : ""}`,
      }, svg);
      for (const [x, v] of s.pts) markerAt(svg, s.name, xs(x), ys(v), 4);
    }
    const cross = sv("line", { x1: 0, y1: m.t, x2: 0, y2: H - m.b, class: "cross", visibility: "hidden" }, svg);
    const hit = sv("rect", { x: m.l, y: 0, width: W - m.l - m.r, height: H, class: "hit" }, svg);
    hit.addEventListener("pointermove", (evt) => {
      const bx = svg.getBoundingClientRect().left;
      const px = evt.clientX - bx;
      let best = xsV[0];
      for (const x of xsV) if (Math.abs(xs(x) - px) < Math.abs(xs(best) - px)) best = x;
      cross.setAttribute("x1", xs(best));
      cross.setAttribute("x2", xs(best));
      cross.setAttribute("visibility", "visible");
      showTip(evt, (t) => {
        t.append(ttTitle(o.tipTitle(best)));
        const rows = o.series.map((s) => [s.name, s.pts.get(best)]);
        rows.sort((a, b) => (b[1] ?? -Infinity) - (a[1] ?? -Infinity));
        if (o.lowerIsBetter) rows.sort((a, b) => (a[1] ?? Infinity) - (b[1] ?? Infinity));
        for (const [name, v] of rows) {
          if (v != null) t.append(ttRow(name, o.yFmt(v)));
          else t.append(ttRow(name, o.reason(name, best), true));
        }
      });
    });
    hit.addEventListener("pointerleave", () => {
      cross.setAttribute("visibility", "hidden");
      hideTip();
    });
    host.append(svg);
  }


  // ------------------------------------------------------------ tables
  function table(cols, rows, opt) {
    const st = { key: opt.sortKey, desc: !!opt.desc };
    const wrap = el("div");
    const draw = () => {
      clear(wrap);
      const t = el("table");
      const tr = el("tr");
      for (const c of cols) {
        const th = el("th", {
          text: c.t, class: c.num ? "num" : null, tabindex: "0", scope: "col",
          "aria-sort": st.key === c.k ? (st.desc ? "descending" : "ascending") : null,
          title: c.title || null,
        });
        const sortBy = () => {
          if (st.key === c.k) st.desc = !st.desc;
          else {
            st.key = c.k;
            st.desc = !!c.num;
          }
          draw();
        };
        th.addEventListener("click", sortBy);
        th.addEventListener("keydown", (e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            sortBy();
          }
        });
        tr.append(th);
      }
      t.append(el("thead", {}, tr));
      const sorted = rows.slice().sort((a, b) => {
        const x = a[st.key];
        const y = b[st.key];
        if (x == null && y == null) return 0;
        if (x == null) return 1;
        if (y == null) return -1;
        const r = typeof x === "string" ? x.localeCompare(y) : x - y;
        return st.desc ? -r : r;
      });
      const tb = el("tbody");
      for (const r of sorted) {
        const row = el("tr");
        for (const c of cols) {
          const v = r[c.k];
          const td = el("td", { class: c.num ? "num" : c.wrap ? "wrapcell" : null });
          if (c.k === "name" && opt.swatch) {
            const sw = el("span", { class: "sw" });
            sw.style.borderTopColor = colorOf(v);
            if (dashed(v)) sw.style.borderTopStyle = "dashed";
            td.append(sw, document.createTextNode(label(v)));
          } else if (c.status) {
            const [label, cls] = o.status()[v] || [v, "st-skip"];
            td.append(el("span", { class: "st " + cls }, el("i"), document.createTextNode(label)));
          } else if (c.fmt) {
            td.textContent = v == null ? (c.missing ? c.missing(r) : "-") : c.fmt(v);
            if (v == null) td.classList.add("muted");
          } else td.textContent = v == null ? "-" : String(v);
          row.append(td);
        }
        tb.append(row);
      }
      t.append(tb);
      wrap.append(t);
    };
    draw();
    return wrap;
  }


  function kvTable(pairs) {
    const t = el("table", { class: "kv" });
    const tb = el("tbody");
    for (const [k, v] of pairs) tb.append(el("tr", {}, el("td", { text: k }), el("td", { text: v == null || v === "" ? "-" : String(v) })));
    t.append(tb);
    return t;
  }

  return { trim, sizeLabel, fmtNs, fmtRate, fmtBytes, fmtS, fmtBpe, short, tickRate, tickNs, tickS, tickBytes, pct, el, sv, clear, showTip, hideTip, ttTitle, ttRow, ttGrid, scale, niceTicks, byteTicks, logTicks, valueDomain, segGroup, swatch, markerAt, chartWidth, cardHead, appendMissing, lineChart, table, kvTable };
};

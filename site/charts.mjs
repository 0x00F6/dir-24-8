// Captures the benchmark images of README.md (benchmarks/charts/*.png) from
// the two HTML reports with headless Chrome, at device scale factor 2.
//
// usage: node site/charts.mjs <report.html> <bench-compare-lib.html> <out-dir>
//   CHROME=<path>  browser (default: google-chrome, google-chrome-stable,
//                  chromium, chromium-browser)
// Needs Node 22 or later (global WebSocket). site/charts.sh wraps it: it
// stitches the full-report bands and compresses the images.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [compareHtml, libHtml, outDir] = process.argv.slice(2);
if (!compareHtml || !libHtml || !outDir) {
  console.error("usage: node site/charts.mjs <report.html> <bench-compare-lib.html> <out-dir>");
  process.exit(2);
}
mkdirSync(outDir, { recursive: true });
const compare = pathToFileURL(resolve(compareHtml)).href;
const lib = pathToFileURL(resolve(libHtml)).href;

// One entry per README image. `clicks`: [filter group, button text, fallback]
// where fallback "last" picks the group's last button when the text is
// missing (a matrix without 1M entries). `legend` copies the structure
// legend into the card; `fit` crops a card stretched by its grid row;
// `optional` skips an absent or hidden element (older reports).
const SHOTS = [
  { url: compare, out: "candlestick-ipv4-lookup", element: "#candle-card", clicks: [["Family", "IPv4"], ["Operation", "Lookup"], ["Entries", "1M", "last"]] },
  { url: compare, out: "candlestick-ipv6-lookup", element: "#candle-card", clicks: [["Family", "IPv6"], ["Operation", "Lookup"], ["Entries", "1M", "last"]] },
  { url: compare, out: "throughput-ipv4-batch", element: "#bars-card", clicks: [["Family", "IPv4"], ["Operation", "Batch (array)"], ["Entries", "1M", "last"]] },
  { url: compare, out: "throughput-ipv6-batch", element: "#bars-card", clicks: [["Family", "IPv6"], ["Operation", "Batch (array)"], ["Entries", "1M", "last"]] },
  { url: compare, out: "scaling-ipv4-batch", element: "#line-throughput", width: 820, legend: true, clicks: [["Family", "IPv4"], ["Operation", "Batch (array)"]] },
  { url: compare, out: "threads-ipv4-batch", element: "#line-threads", width: 820, fit: true, legend: true, clicks: [["Family", "IPv4"], ["Operation", "Batch (array)"], ["Entries", "1M", "last"]] },
  { url: compare, out: "batch-size-ipv4", element: "#line-batch", width: 820, fit: true, legend: true, optional: true, clicks: [["Family", "IPv4"], ["Entries", "1M", "last"]] },
  { url: compare, out: "memory-ipv6", element: "#line-memory", width: 820, legend: true, clicks: [["Family", "IPv6"]] },
  { url: compare, out: "report-full", fullPage: true, tile: 4000, clicks: [["Family", "IPv4"], ["Operation", "Lookup"], ["Entries", "1M", "last"]] },
  { url: lib, out: "lib-throughput-ipv4", element: "#line-throughput", legend: true, clicks: [["Family", "IPv4"], ["Operation", "LPM lookup"]] },
  { url: lib, out: "lib-throughput-ipv6", element: "#line-throughput", legend: true, clicks: [["Family", "IPv6"], ["Operation", "LPM lookup"]] },
  { url: lib, out: "lib-candlestick-ipv4", element: "#candle-card", clicks: [["Family", "IPv4"], ["Operation", "LPM lookup"]] },
];
const WIDTH = 1400;
const DPR = 2;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function findChrome() {
  if (process.env.CHROME) return process.env.CHROME;
  for (const dir of (process.env.PATH || "").split(":")) {
    for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
      if (existsSync(join(dir, name))) return join(dir, name);
    }
  }
  throw new Error("no Chrome or Chromium found (set CHROME)");
}

// Starts Chrome with a DevTools port chosen by the browser.
async function launch() {
  const profile = mkdtempSync(join(tmpdir(), "charts-chrome-"));
  const proc = spawn(findChrome(), [
    "--headless=new", "--disable-gpu", "--no-sandbox", "--hide-scrollbars",
    "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank",
  ], { stdio: "ignore" });
  const portFile = join(profile, "DevToolsActivePort");
  for (let i = 0; i < 200 && !existsSync(portFile); i++) await sleep(50);
  if (!existsSync(portFile)) throw new Error("Chrome did not start");
  const port = readFileSync(portFile, "utf8").split("\n")[0].trim();
  const exited = new Promise((res) => proc.once("exit", res));
  const stop = async () => {
    proc.kill();
    await Promise.race([exited, sleep(5000)]);
    try {
      rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch (e) {
      console.warn(`could not remove ${profile}: ${e.message}`);
    }
  };
  return { port, stop };
}

async function connect(port) {
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let id = 0;
  const pending = new Map();
  const events = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(JSON.stringify(msg.error)));
      else res(msg.result);
    } else if (msg.method) events.push(msg);
  };
  const send = (method, params = {}) => new Promise((res, rej) => {
    const i = ++id;
    pending.set(i, { res, rej });
    ws.send(JSON.stringify({ id: i, method, params }));
  });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };
  return { ws, send, evaluate, events };
}

const clickJs = (group, text, fallback) => `(() => {
  const g = [...document.querySelectorAll('.seg')].find((s) => s.getAttribute('aria-label') === ${JSON.stringify(group)});
  if (!g) return 'no group';
  const buttons = [...g.querySelectorAll('button')];
  const b = buttons.find((x) => x.textContent.trim() === ${JSON.stringify(text)})
    || (${JSON.stringify(fallback)} === 'last' ? buttons[buttons.length - 1] : null);
  if (!b) return 'no button';
  b.click();
  return 'ok';
})()`;

// Bounding box of the element in page coordinates, the structure legend
// copied in first when asked, the height cropped to the children with fit.
const boxJs = (shot) => `(() => {
  document.querySelectorAll('.charts-legend-copy').forEach((n) => n.remove());
  const e = document.querySelector(${JSON.stringify(shot.element)});
  if (!e || e.offsetParent === null || e.closest('[hidden]')) return null;
  if (${!!shot.legend}) {
    const legend = document.querySelector('#legend');
    if (legend) {
      const copy = legend.cloneNode(true);
      copy.removeAttribute('id');
      copy.classList.add('charts-legend-copy');
      copy.style.margin = '0 0 16px';
      e.prepend(copy);
    }
  }
  const b = e.getBoundingClientRect();
  let height = b.height;
  if (${!!shot.fit}) {
    height = Math.max(...[...e.children].map((c) => c.getBoundingClientRect().bottom)) - b.top + 12;
  }
  return { x: b.left + scrollX, y: b.top + scrollY, width: b.width, height };
})()`;

async function main() {
  const { port, stop } = await launch();
  try {
    const { ws, send, evaluate, events } = await connect(port);
    await send("Page.enable");
    await send("Runtime.enable");
    let loaded = null;
    for (const shot of SHOTS) {
      await send("Emulation.setDeviceMetricsOverride", { width: shot.width || WIDTH, height: 1000, deviceScaleFactor: DPR, mobile: false });
      if (loaded !== shot.url) {
        events.length = 0;
        await send("Page.navigate", { url: shot.url });
        for (let t = 0; t < 300 && !events.some((e) => e.method === "Page.loadEventFired"); t++) await sleep(50);
        await sleep(800);
        loaded = shot.url;
      }
      await evaluate("window.dispatchEvent(new Event('resize'))");
      await sleep(400);
      for (const [group, text, fallback] of shot.clicks || []) {
        const r = await evaluate(clickJs(group, text, fallback || ""));
        if (r !== "ok") console.warn(`${shot.out}: filter ${group} / ${text}: ${r}`);
        await sleep(250);
      }
      if (shot.fullPage) {
        const h = await evaluate("document.documentElement.scrollHeight");
        const width = shot.width || WIDTH;
        let n = 0;
        for (let y = 0; y < h; y += shot.tile, n++) {
          const clip = { x: 0, y, width, height: Math.min(shot.tile, h - y), scale: 1 };
          const img = await send("Page.captureScreenshot", { format: "png", clip, captureBeyondViewport: true });
          writeFileSync(join(outDir, `${shot.out}.part${String(n).padStart(2, "0")}.png`), Buffer.from(img.data, "base64"));
        }
        console.log(`${shot.out}: ${width}x${h} CSS px in ${n} bands`);
        continue;
      }
      const box = await evaluate(boxJs(shot));
      if (!box) {
        if (shot.optional) {
          console.warn(`${shot.out}: ${shot.element} absent or hidden in this report, skipped`);
          continue;
        }
        throw new Error(`${shot.out}: ${shot.element} not found`);
      }
      const pad = 8;
      const clip = { x: Math.max(0, box.x - pad), y: Math.max(0, box.y - pad), width: box.width + 2 * pad, height: box.height + 2 * pad, scale: 1 };
      const img = await send("Page.captureScreenshot", { format: "png", clip, captureBeyondViewport: true });
      writeFileSync(join(outDir, `${shot.out}.png`), Buffer.from(img.data, "base64"));
      console.log(`${shot.out}: ${Math.round(clip.width)}x${Math.round(clip.height)} CSS px`);
    }
    ws.close();
  } finally {
    await stop();
  }
}

main().catch((e) => {
  console.error(e.message || e);
  process.exit(1);
});

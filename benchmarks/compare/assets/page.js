"use strict";
/**
 * Tabs of report.html, one per comparison (ARIA tabs: click, arrow keys,
 * Home and End). The URL fragment picks the tab: #structures, #libraries or
 * the id of an element inside a tab. Without one, the first tab that has
 * results is shown. Each tab script sets data-status (ok, warn, bad, none)
 * and data-summary on its panel; a shown panel receives a "tabshow" event
 * (charts of a hidden tab are drawn then).
 */
(() => {
  const tabs = [...document.querySelectorAll('[role="tab"]')];
  const panelOf = (tab) => document.getElementById(tab.getAttribute("aria-controls"));
  const STATUS = { ok: "st-ok", warn: "st-mem", bad: "st-bad", none: "st-skip" };

  for (const tab of tabs) {
    const p = panelOf(tab);
    const status = p.dataset.status || "none";
    const out = document.getElementById(tab.id + "-status");
    const dot = document.createElement("span");
    dot.className = "st " + STATUS[status];
    dot.append(document.createElement("i"), document.createTextNode(p.dataset.summary || "not measured"));
    out.replaceChildren(dot);
  }

  function select(tab, focus) {
    for (const t of tabs) {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
      panelOf(t).hidden = !on;
    }
    if (focus) tab.focus();
    panelOf(tab).dispatchEvent(new Event("tabshow"));
  }
  function choose(tab, focus) {
    select(tab, focus);
    history.replaceState(null, "", "#" + panelOf(tab).id);
  }

  function fromHash() {
    const id = decodeURIComponent(location.hash.slice(1));
    const target = id ? document.getElementById(id) : null;
    const tab =
      tabs.find((t) => target && panelOf(t).contains(target)) ||
      tabs.find((t) => panelOf(t).dataset.status !== "none") ||
      tabs[0];
    select(tab, false);
    if (target && target !== panelOf(tab)) target.scrollIntoView();
  }

  for (const tab of tabs) {
    tab.addEventListener("click", () => choose(tab, false));
    tab.addEventListener("keydown", (e) => {
      const i = tabs.indexOf(tab);
      const n = tabs.length;
      const j = { ArrowRight: (i + 1) % n, ArrowLeft: (i + n - 1) % n, Home: 0, End: n - 1 }[e.key];
      if (j === undefined) return;
      e.preventDefault();
      choose(tabs[j], true);
    });
  }
  window.addEventListener("hashchange", fromHash);
  fromHash();
})();

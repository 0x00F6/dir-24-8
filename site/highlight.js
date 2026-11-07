// Syntax colouring of the landing page code blocks (<code class="language-rust">
// and <code class="language-toml">), without dependency: each language is a
// list of [class, regular expression] tried at every position, the first
// match wins. Colours are CSS tokens (.tok-* in style.css). Without
// JavaScript the code stays plain text.
"use strict";
(() => {
  const RULES = {
    rust: [
      ["comment", String.raw`//[^\n]*`],
      ["string", String.raw`"(?:\\.|[^"\\])*"`],
      ["attr", String.raw`#!?\[[^\]\n]*\]`],
      ["macro", String.raw`\b[a-z_][a-z0-9_]*!`],
      ["keyword", String.raw`\b(?:as|async|await|break|const|continue|crate|dyn|else|enum|extern|false|fn|for|if|impl|in|let|loop|match|mod|move|mut|pub|ref|return|self|Self|static|struct|super|trait|true|type|unsafe|use|where|while)\b`],
      ["type", String.raw`\b[A-Z][A-Za-z0-9_]*\b`],
      ["number", String.raw`\b\d[\d_]*(?:\.\d[\d_]*)?(?:[iu](?:8|16|32|64|128|size)|f32|f64)?\b`],
      ["function", String.raw`\b[a-z_][a-z0-9_]*(?=\s*\()`],
    ],
    toml: [
      ["comment", String.raw`#[^\n]*`],
      ["table", String.raw`^\s*\[\[?[^\]\n]*\]\]?`],
      ["string", String.raw`"(?:\\.|[^"\\])*"`],
      ["key", String.raw`[A-Za-z0-9_.-]+(?=\s*=)`],
      ["number", String.raw`\b(?:true|false|\d[\d_.]*)\b`],
    ],
  };

  const escape = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);

  function colour(text, rules) {
    const re = new RegExp(rules.map(([, r]) => `(${r})`).join("|"), "gm");
    let html = "";
    let last = 0;
    for (const m of text.matchAll(re)) {
      if (m[0] === "") continue;
      const kind = rules[m.slice(1).findIndex((g) => g !== undefined)][0];
      html += escape(text.slice(last, m.index)) + `<span class="tok-${kind}">${escape(m[0])}</span>`;
      last = m.index + m[0].length;
    }
    return html + escape(text.slice(last));
  }

  for (const code of document.querySelectorAll("pre code[class*='language-']")) {
    const lang = [...code.classList].find((c) => c.startsWith("language-"))?.slice("language-".length);
    if (RULES[lang]) code.innerHTML = colour(code.textContent, RULES[lang]);
  }
})();

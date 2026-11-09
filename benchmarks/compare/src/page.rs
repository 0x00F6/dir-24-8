//! The report of `bench-compare`: one standalone page, `report.html` (dark
//! theme, no external resource), with a tab per comparison. The structures
//! tab draws `results.json` ([`crate::report`]), the libraries tab
//! `libraries.json` ([`crate::libcmp::results`]); both documents are
//! embedded with the stylesheets and scripts of `assets/`.
//!
//! The page always shows every result document of the output directory: a
//! part that a run does not measure (`--parts`) keeps its previous results,
//! dated in its tab, and a part without results says how to measure it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{libcmp, report};

/// File name of the report, in the output directory.
pub const FILE: &str = "report.html";

const TEMPLATE: &str = include_str!("../assets/report.html");
/// Dark theme tokens and rules shared by both tabs.
const CSS: &str = include_str!("../assets/report.css");
const LIBRARIES_CSS: &str = include_str!("../assets/libraries.css");
/// Chart kit shared by both tabs.
const KIT_JS: &str = include_str!("../assets/kit.js");
const STRUCTURES_JS: &str = include_str!("../assets/structures.js");
const LIBRARIES_JS: &str = include_str!("../assets/libraries.js");
/// Tabs.
const PAGE_JS: &str = include_str!("../assets/page.js");
/// Logo of the header (the one of the site), linked to the home page.
const LOGO_PNG: &[u8] = include_bytes!("../../../site/assets/logo-dark.png");

/// Standard base64 (RFC 4648, with padding), for the `data:` URI of the logo.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(c.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(c.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The result documents of the report; `None` leaves a tab without results.
#[derive(Default)]
pub struct Parts {
    pub structures: Option<Value>,
    pub libraries: Option<Value>,
}

impl Parts {
    /// The result documents of the output directory.
    pub fn read(out: &Path) -> Parts {
        Parts {
            structures: read_json(&out.join(report::JSON)),
            libraries: read_json(&out.join(libcmp::results::JSON)),
        }
    }
}

/// A result document, `None` when the file is absent (or unreadable, which
/// is reported).
pub fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes)
        .inspect_err(|e| eprintln!("warning: {} left out of the report: {e}", path.display()))
        .ok()
}

/// Inlines the `<link rel="stylesheet" href="X">` and `<script src="X">`
/// references of `template` (which can be opened as is next to its assets)
/// and the JSON documents in place of their placeholders, so that the page
/// is a single offline file.
fn standalone(
    template: &str,
    data: &[(&str, String)],
    css: &[(&str, &str)],
    js: &[(&str, &str)],
) -> String {
    let mut out = template.to_string();
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
    let logo = r#"src="logo-dark.png""#;
    assert!(out.contains(logo), "template references logo-dark.png");
    out = out.replace(
        logo,
        &format!(r#"src="data:image/png;base64,{}""#, base64(LOGO_PNG)),
    );
    for (placeholder, json) in data {
        assert_eq!(
            out.matches(placeholder).count(),
            1,
            "template holds {placeholder} once"
        );
        // `<` only occurs in JSON strings, where `\u003c` is the same
        // character: the script element cannot be closed or commented out.
        out = out.replacen(placeholder, &json.replace('<', "\\u003c"), 1);
    }
    out
}

/// The page, with `null` for a tab without results.
pub fn render(parts: &Parts) -> String {
    let json = |v: &Option<Value>| {
        v.as_ref().map_or_else(
            || "null".to_string(),
            |v| serde_json::to_string(v).expect("serializable"),
        )
    };
    standalone(
        TEMPLATE,
        &[
            ("__STRUCTURES_DATA__", json(&parts.structures)),
            ("__LIBRARIES_DATA__", json(&parts.libraries)),
        ],
        &[("report.css", CSS), ("libraries.css", LIBRARIES_CSS)],
        &[
            ("kit.js", KIT_JS),
            ("structures.js", STRUCTURES_JS),
            ("libraries.js", LIBRARIES_JS),
            ("page.js", PAGE_JS),
        ],
    )
}

/// Writes `report.html` in `out`.
pub fn write(out: &Path, parts: &Parts) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let path = out.join(FILE);
    let tmp = out.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, render(parts))?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Rewrites `report.html` from the result documents of `out`; a failure is
/// reported, not fatal (the JSON and CSV files are written).
pub fn refresh(out: &Path) {
    match write(out, &Parts::read(out)) {
        Ok(p) => eprintln!("  {}", p.display()),
        Err(e) => eprintln!("warning: cannot write {}: {e}", out.join(FILE).display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    #[test]
    fn inlines_assets_and_data() {
        let parts = Parts {
            structures: Some(serde_json::json!({"reason": "</script><!-- x"})),
            libraries: None,
        };
        let html = render(&parts);
        for asset in ["report.css", "libraries.css", "kit.js", "page.js"] {
            assert!(!html.contains(&format!("\"{asset}\"")), "{asset} inlined");
        }
        assert!(!html.contains("__STRUCTURES_DATA__") && !html.contains("__LIBRARIES_DATA__"));
        assert!(html.contains(r#"<img src="data:image/png;base64,iVBORw0KGgo"#));
        assert!(html.contains(r#"{"reason":"\u003c/script>\u003c!-- x"}"#));
        assert!(
            html.contains(r#"<script id="libraries-data" type="application/json">null</script>"#)
        );
    }
}

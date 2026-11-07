//! Standalone HTML report of `bench-compare-lib` (dark theme, offline):
//! the template, the shared stylesheet and chart kit of `bench-compare`,
//! the report's own stylesheet and script, and the results JSON, inlined.

use serde_json::Value;

use crate::report::{CSS, KIT_JS, standalone};

const TEMPLATE: &str = include_str!("../../assets/lib-report.html");
const LIB_CSS: &str = include_str!("../../assets/lib-report.css");
const LIB_JS: &str = include_str!("../../assets/lib-report.js");

pub fn render(doc: &Value) -> String {
    let data = serde_json::to_string(doc).expect("serializable");
    standalone(
        TEMPLATE,
        &data,
        &[("report.css", CSS), ("lib-report.css", LIB_CSS)],
        &[("kit.js", KIT_JS), ("lib-report.js", LIB_JS)],
    )
}

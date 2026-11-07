//! Captures the toolchain and compilation settings for the report.

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(&rustc)
        .arg("-vV")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let line = |k: &str| {
        version
            .lines()
            .find_map(|l| l.strip_prefix(k).map(str::trim).map(str::to_string))
            .unwrap_or_else(|| "unknown".into())
    };
    println!("cargo:rustc-env=COMPARE_RUSTC_RELEASE={}", line("release:"));
    println!(
        "cargo:rustc-env=COMPARE_RUSTC_COMMIT={}",
        line("commit-hash:")
    );
    println!("cargo:rustc-env=COMPARE_RUSTC_HOST={}", line("host:"));
    println!(
        "cargo:rustc-env=COMPARE_RUSTC_LLVM={}",
        line("LLVM version:")
    );
    for (k, v) in [
        ("COMPARE_PROFILE", std::env::var("PROFILE")),
        ("COMPARE_OPT_LEVEL", std::env::var("OPT_LEVEL")),
        ("COMPARE_TARGET", std::env::var("TARGET")),
        (
            "COMPARE_TARGET_FEATURES",
            std::env::var("CARGO_CFG_TARGET_FEATURE"),
        ),
    ] {
        println!("cargo:rustc-env={k}={}", v.unwrap_or_default());
    }
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .unwrap_or_default()
        .replace('\u{1f}', " ");
    println!("cargo:rustc-env=COMPARE_RUSTFLAGS={flags}");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
}

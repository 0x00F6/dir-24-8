//! Dependency preparation of the libraries part of `bench-compare`: the Go
//! toolchain and modules (pinned by `go.mod` / `go.sum`, verified), the Go
//! runner and MMDB writer, libmaxminddb (pinned release tarball, SHA-256
//! checked, static build) and the C runner. Everything is cached under
//! `<cache>/deps`; a failed step only disables the libraries that need it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};

use super::corpus::fnv64;

/// Go toolchain used for every Go library (also the `toolchain` line of
/// `go.mod`).
pub const GO_TOOLCHAIN: &str = "go1.27.3";
pub const MMDB_VERSION: &str = "1.14.1";
pub const MMDB_SHA256: &str = "ca5c87d41339f8bc4daabb53e8a9356b3c995f2d2419b85d7bff823b2ecc252d";
pub const MMDB_CFLAGS: &str = "-O2";
pub const C_FLAGS: &str = "-O2 -std=c11 -Wall -Wextra -pthread";

fn mmdb_url() -> String {
    format!(
        "https://github.com/maxmind/libmaxminddb/releases/download/{MMDB_VERSION}/libmaxminddb-{MMDB_VERSION}.tar.gz"
    )
}

#[derive(Default, Serialize)]
pub struct Stages {
    pub go_download_s: f64,
    pub go_build_s: f64,
    pub mmdb_download_s: f64,
    pub mmdb_build_s: f64,
    pub c_build_s: f64,
}

#[derive(Default, Serialize)]
pub struct DepsReport {
    pub go: Value,
    pub libmaxminddb: Value,
    pub c_runner: Value,
    pub stages: Stages,
    pub errors: Vec<String>,
}

pub struct Deps {
    pub go_runner: Result<PathBuf, String>,
    pub mmdbgen: Result<PathBuf, String>,
    pub c_runner: Result<PathBuf, String>,
    pub report: DepsReport,
}

/// Runs a command, returns stdout or a diagnostic with the stderr tail.
fn run(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{what}: cannot start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(12).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!(
            "{what} failed ({}): {}",
            out.status,
            tail.join(" | ")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn go(go_dir: &Path, goamd64: &str) -> Command {
    let mut c = Command::new("go");
    c.current_dir(go_dir)
        .env("GOTOOLCHAIN", GO_TOOLCHAIN)
        .env("CGO_ENABLED", "0")
        .env("GOAMD64", goamd64)
        .env("GOFLAGS", "-mod=readonly");
    c
}

/// `go version -m` of a binary: module list and build settings.
fn modules_of(go_dir: &Path, goamd64: &str, bin: &Path) -> (Vec<Value>, Value) {
    let text = run(
        go(go_dir, goamd64).arg("version").arg("-m").arg(bin),
        "go version -m",
    )
    .unwrap_or_default();
    let mut mods: Vec<Value> = Vec::new();
    let mut settings = serde_json::Map::new();
    for line in text.lines() {
        let f: Vec<&str> = line.trim().split('\t').collect();
        match f.as_slice() {
            ["dep", path, version, sum @ ..] => {
                mods.push(json!({"path": path, "version": version, "sum": sum.first().copied().unwrap_or(""), "replace": Value::Null}))
            }
            ["=>", path, version, ..] => {
                if let Some(Value::Object(m)) = mods.last_mut() {
                    m.insert("replace".into(), json!(format!("{path}@{version}")));
                }
            }
            ["build", kv] => {
                if let Some((k, v)) = kv.split_once('=') {
                    settings.insert(k.into(), json!(v));
                }
            }
            _ => {}
        }
    }
    (mods, Value::Object(settings))
}

fn prepare_go(
    repo: &Path,
    out: &Path,
    goamd64: &str,
    r: &mut DepsReport,
) -> Result<(PathBuf, PathBuf), String> {
    let go_dir = repo.join("benchmarks/compare-lib/go");
    let t0 = Instant::now();
    let version = run(
        go(&go_dir, goamd64).arg("version"),
        "go version (toolchain download)",
    )?;
    run(
        go(&go_dir, goamd64).args(["mod", "download"]),
        "go mod download",
    )?;
    let verify = run(
        go(&go_dir, goamd64).args(["mod", "verify"]),
        "go mod verify",
    )?;
    r.stages.go_download_s = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let runner = out.join("go-runner");
    let mmdbgen = out.join("mmdbgen");
    let flags = ["build", "-trimpath"];
    run(
        go(&go_dir, goamd64)
            .args(flags)
            .arg("-o")
            .arg(&runner)
            .arg("./cmd/runner"),
        "go build runner",
    )?;
    run(
        go(&go_dir, goamd64)
            .args(flags)
            .arg("-o")
            .arg(&mmdbgen)
            .arg("./cmd/mmdbgen"),
        "go build mmdbgen",
    )?;
    r.stages.go_build_s = t1.elapsed().as_secs_f64();
    let (mods, settings) = modules_of(&go_dir, goamd64, &runner);
    let (writer_mods, _) = modules_of(&go_dir, goamd64, &mmdbgen);
    let gomod = std::fs::read_to_string(go_dir.join("go.mod")).unwrap_or_default();
    let replaces = gomod.lines().any(|l| l.trim_start().starts_with("replace"));
    let find = |p: &str| mods.iter().find(|m| m["path"] == p).cloned();
    let libp2p = find("github.com/libp2p/go-cidranger");
    let yl2chen = find("github.com/yl2chen/cidranger");
    let distinct = match (&libp2p, &yl2chen) {
        (Some(a), Some(b)) => {
            a["replace"].is_null() && b["replace"].is_null() && a["sum"] != b["sum"]
        }
        _ => false,
    };
    r.go = json!({
        "toolchain": version.trim(), "requested": GO_TOOLCHAIN,
        "env": {"GOTOOLCHAIN": GO_TOOLCHAIN, "CGO_ENABLED": "0", "GOAMD64": goamd64, "GOFLAGS": "-mod=readonly", "GOGC": "default (100)", "GOMAXPROCS": "default (all CPUs)"},
        "build_command": "go build -trimpath (one module, one toolchain, same flags for every Go library)",
        "build_settings": settings, "modules": mods, "mmdbgen_modules": writer_mods,
        "go_mod_verify": verify.trim(), "replace_directives": replaces,
        "cidranger_distinct": distinct,
        "cidranger_check": "both modules compiled, no replace directive, different module hashes (h1 sums); the runner also reports the Go package of each ranger at run time",
    });
    if replaces || !distinct {
        return Err(
            "go.mod has replace directives or the two cidranger modules are not distinct".into(),
        );
    }
    Ok((runner, mmdbgen))
}

fn sha256(path: &Path) -> Result<String, String> {
    let out = run(Command::new("sha256sum").arg(path), "sha256sum")?;
    Ok(out.split_whitespace().next().unwrap_or("").to_string())
}

fn prepare_mmdb(deps: &Path, r: &mut DepsReport) -> Result<PathBuf, String> {
    let root = deps.join(format!("libmaxminddb-{MMDB_VERSION}"));
    let install = root.join("install");
    let stamp = install.join(".stamp");
    let want = format!("{MMDB_VERSION} {MMDB_SHA256} {MMDB_CFLAGS}");
    let mut info = json!({"version": MMDB_VERSION, "url": mmdb_url(), "sha256": MMDB_SHA256,
        "configure": format!("CFLAGS={MMDB_CFLAGS} ./configure --disable-shared --enable-static --disable-tests (upstream adds -O2 -g)"),
        "cached": false});
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(want.as_str()) {
        info["cached"] = json!(true);
        info["sha256_verified"] = json!(true);
        r.libmaxminddb = info;
        return Ok(install);
    }
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let tarball = root.join(format!("libmaxminddb-{MMDB_VERSION}.tar.gz"));
    let t0 = Instant::now();
    if sha256(&tarball).ok().as_deref() != Some(MMDB_SHA256) {
        run(
            Command::new("curl")
                .args(["-fsSL", "--retry", "3", "-o"])
                .arg(&tarball)
                .arg(mmdb_url()),
            "download libmaxminddb",
        )?;
    }
    let got = sha256(&tarball)?;
    r.stages.mmdb_download_s = t0.elapsed().as_secs_f64();
    if got != MMDB_SHA256 {
        return Err(format!("libmaxminddb tarball SHA-256 mismatch: {got}"));
    }
    info["sha256_verified"] = json!(true);
    let t1 = Instant::now();
    run(
        Command::new("tar")
            .arg("xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&root),
        "extract libmaxminddb",
    )?;
    let src = root.join(format!("libmaxminddb-{MMDB_VERSION}"));
    run(
        Command::new("./configure")
            .current_dir(&src)
            .env("CFLAGS", MMDB_CFLAGS)
            .arg(format!("--prefix={}", install.display()))
            .args(["--disable-shared", "--enable-static", "--disable-tests"]),
        "libmaxminddb configure",
    )?;
    let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    run(
        Command::new("make")
            .current_dir(&src)
            .arg(format!("-j{jobs}")),
        "libmaxminddb make",
    )?;
    run(
        Command::new("make").current_dir(&src).arg("install"),
        "libmaxminddb make install",
    )?;
    r.stages.mmdb_build_s = t1.elapsed().as_secs_f64();
    std::fs::write(&stamp, &want).map_err(|e| e.to_string())?;
    r.libmaxminddb = info;
    Ok(install)
}

fn prepare_c(
    repo: &Path,
    deps: &Path,
    install: &Path,
    r: &mut DepsReport,
) -> Result<PathBuf, String> {
    let src = repo.join("benchmarks/compare-lib/c/runner.c");
    let bin = deps.join("c-runner");
    let source = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    let compiler = run(Command::new("cc").arg("--version"), "cc --version")?
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let want = format!(
        "{:016x} {C_FLAGS} {compiler} {MMDB_VERSION}",
        fnv64(&source)
    );
    let stamp = deps.join("c-runner.stamp");
    r.c_runner = json!({"compiler": compiler, "cflags": C_FLAGS, "link": format!("static libmaxminddb {MMDB_VERSION}"), "cached": true});
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(want.as_str()) && bin.exists() {
        return Ok(bin);
    }
    let t0 = Instant::now();
    let mut cmd = Command::new("cc");
    cmd.args(C_FLAGS.split_whitespace())
        .arg(format!("-DRUNNER_CFLAGS=\"{C_FLAGS}\""))
        .arg(format!("-I{}", install.join("include").display()))
        .arg(&src)
        .arg(install.join("lib/libmaxminddb.a"))
        .arg("-o")
        .arg(&bin);
    run(&mut cmd, "compile C runner")?;
    r.stages.c_build_s = t0.elapsed().as_secs_f64();
    r.c_runner["cached"] = json!(false);
    std::fs::write(&stamp, want).map_err(|e| e.to_string())?;
    Ok(bin)
}

/// Prepares every external dependency (Rust ones are built by cargo).
pub fn prepare(repo: &Path, cache: &Path, goamd64: &str, need_go: bool, need_c: bool) -> Deps {
    let deps = cache.join("deps");
    let _ = std::fs::create_dir_all(&deps);
    let mut report = DepsReport::default();
    let (go_runner, mmdbgen) = if need_go || need_c {
        match prepare_go(repo, &deps, goamd64, &mut report) {
            Ok((a, b)) => (Ok(a), Ok(b)),
            Err(e) => {
                report.errors.push(e.clone());
                (Err(e.clone()), Err(e))
            }
        }
    } else {
        (Err("not needed".into()), Err("not needed".into()))
    };
    let c_runner = if need_c {
        match prepare_mmdb(&deps, &mut report)
            .and_then(|install| prepare_c(repo, &deps, &install, &mut report))
        {
            Ok(p) => Ok(p),
            Err(e) => {
                report.errors.push(e.clone());
                Err(e)
            }
        }
    } else {
        Err("not needed".into())
    };
    Deps {
        go_runner,
        mmdbgen,
        c_runner,
        report,
    }
}

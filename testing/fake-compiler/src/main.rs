//! A compiler stand-in that speaks the real subprocess protocol.
//!
//! Framework tests must not depend on compiler stability. The compiler is under
//! active development and its output for a given input will change; making
//! every framework test a compiler test drags framework CI on every codegen
//! tweak (PLAN.md §7).
//!
//! This binary implements exactly the contract in Platform 14 §14.2.2:
//!
//! ```text
//!   fake-compiler --request - --out <dir>   < request.json
//!   fake-compiler --version
//!
//! The argv shape mirrors the real compiler (0.1.0), which takes an output
//! directory rather than the `compile --stdout-tar` mode the framework
//! originally assumed. A stand-in that accepted a protocol the real binary
//! rejects is worse than no stand-in: every test would pass against a seam
//! that cannot work.
//! ```
//!
//! It validates the parts of the request the real compiler validates and that
//! the framework is responsible for getting right:
//!
//! - `spec_version` is present and equal to "1".
//! - Every `sources[].sha256` matches its decoded content (`RQD001`).
//! - `sources[]` is sorted by path (FRM-BO-06) — not a compiler rule, but the
//!   framework's determinism invariant, and this is the cheapest place to
//!   catch a regression in it.
//!
//! Behaviour is steered by environment variables so a test can drive failure
//! paths through the real transport:
//!
//! - `FAKE_COMPILER_FAIL=<code>` — exit with that code, emitting a diagnostic
//!   on stdout.
//! - `FAKE_COMPILER_DIAGNOSTIC=<msg>` — the message for the above.
//! - `FAKE_COMPILER_WARN=<msg>` — succeed, but include a warning.
//! - `FAKE_COMPILER_VERSION=<v>` — what `--version` reports.
//! - `FAKE_COMPILER_GARBAGE=1` — exit 0 with non-tar bytes on stdout, to
//!   exercise the malformed-output path.
//! - `FAKE_COMPILER_ECHO_REQUEST=<p>` — also write the received request to path
//!   `<p>`, so a test can assert on exactly what crossed the seam.
//! - `FAKE_COMPILER_WASM=<p>` — emit the component at `<p>` instead of a bare
//!   preamble, so an end-to-end run can actually serve traffic from the guest.

use std::io::Read;
use std::process::ExitCode;

use sha2::{Digest, Sha256};

const DEFAULT_VERSION: &str = "0.0.0-fake";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        let version = env_or(String::from(DEFAULT_VERSION), "FAKE_COMPILER_VERSION");
        println!("clean-compiler {version}");
        return ExitCode::SUCCESS;
    }

    // `--request -` means "read the request from stdin", the only mode the
    // framework uses. Anything else is a caller that has drifted from the
    // real compiler's interface, and saying so loudly is the point of this
    // binary existing.
    if flag_value(&args, "--request").as_deref() != Some("-") {
        eprintln!("fake-compiler: expected `--request -`, got {args:?}");
        return ExitCode::from(2);
    }

    let Some(out_dir) = flag_value(&args, "--out") else {
        eprintln!("fake-compiler: --out <dir> is required");
        return ExitCode::from(2);
    };
    let out_dir = std::path::PathBuf::from(out_dir);

    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("fake-compiler: could not read request from stdin: {e}");
        return ExitCode::from(2);
    }

    if let Some(path) = std::env::var_os("FAKE_COMPILER_ECHO_REQUEST") {
        if let Err(e) = std::fs::write(&path, &input) {
            eprintln!("fake-compiler: could not echo request: {e}");
            return ExitCode::from(2);
        }
    }

    let request: serde_json::Value = match serde_json::from_slice(&input) {
        Ok(value) => value,
        Err(e) => {
            return fail_in(
                Some(&out_dir),
                1,
                "RQD002",
                &format!("request is not valid JSON: {e}"),
            )
        }
    };

    if let Err(message) = validate(&request) {
        return fail_in(Some(&out_dir), 1, "RQD001", &message);
    }

    if let Ok(code) = std::env::var("FAKE_COMPILER_FAIL") {
        let code: u8 = code.parse().unwrap_or(1);
        let message = env_or("compilation failed".to_string(), "FAKE_COMPILER_DIAGNOSTIC");
        return fail_in(Some(&out_dir), code, "SEM001", &message);
    }

    if std::env::var_os("FAKE_COMPILER_GARBAGE").is_some() {
        // Exits 0 having written nothing usable. The framework must report a
        // seam error rather than treating an empty output directory as a
        // successful build.
        let _ = std::fs::create_dir_all(&out_dir);
        let _ = std::fs::write(out_dir.join("component.wasm"), b"this is not wasm");
        return ExitCode::SUCCESS;
    }

    match emit_artifact(&request, &input, &out_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake-compiler: could not write artifact: {e}");
            ExitCode::from(2)
        }
    }
}

/// The checks the real compiler performs on the request document, plus the
/// framework's own ordering invariant.
fn validate(request: &serde_json::Value) -> Result<(), String> {
    match request.get("spec_version").and_then(|v| v.as_str()) {
        Some("1") => {}
        Some(other) => return Err(format!("unsupported spec_version '{other}'")),
        None => return Err("request has no spec_version".into()),
    }

    let sources = request
        .get("sources")
        .and_then(|v| v.as_array())
        .ok_or("request has no sources array")?;

    let mut previous: Option<&str> = None;
    for source in sources {
        let path = source
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("a source entry has no path")?;
        let content = source
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("source {path} has no content"))?;
        let declared = source
            .get("sha256")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("source {path} has no sha256"))?;

        let actual = hex_sha256(content.as_bytes());
        if actual != declared {
            return Err(format!(
                "source {path} declares sha256 {declared} but content hashes to {actual}"
            ));
        }

        if let Some(prev) = previous {
            if prev >= path {
                return Err(format!("sources are not sorted: {prev:?} precedes {path:?}"));
            }
        }
        previous = Some(path);
    }

    Ok(())
}

/// Produce a plausible artifact: a valid empty WASM component preamble, a
/// build manifest shaped per Platform 14 §14.8, and diagnostics.
fn emit_artifact(
    request: &serde_json::Value,
    raw_request: &[u8],
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let version = env_or(String::from(DEFAULT_VERSION), "FAKE_COMPILER_VERSION");

    // The WASM component preamble: magic + version 0x0d, layer 1. Enough to be
    // recognizably a component, which is all the framework ever inspects.
    //
    // `FAKE_COMPILER_WASM` substitutes a real pre-compiled component instead.
    // Framework tests never need one — nothing above the seam looks past the
    // header — but an end-to-end run through Cloud does: the runtime actually
    // instantiates the guest and serves from it, so a preamble alone would
    // deploy and then fail health. Pointing this at a real component is what
    // lets the whole pipeline be exercised before the compiler can produce
    // one, and it keeps the substitution at the seam rather than letting any
    // component grow a test mode.
    let wasm: Vec<u8> = match std::env::var("FAKE_COMPILER_WASM") {
        Ok(path) => std::fs::read(&path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("FAKE_COMPILER_WASM={path} could not be read: {e}"),
            )
        })?,
        Err(_) => vec![0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00],
    };

    let mut diagnostics = Vec::new();
    if let Ok(message) = std::env::var("FAKE_COMPILER_WARN") {
        diagnostics.push(serde_json::json!({
            "level": "warning",
            "code": "SEM900",
            "message": message,
        }));
    }

    let sources: Vec<serde_json::Value> = request
        .get("sources")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "path": s.get("path").cloned().unwrap_or_default(),
                        "sha256": s.get("sha256").cloned().unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let manifest = serde_json::json!({
        "spec_version": "1",
        "compiler": { "version": version, "sha256": hex_sha256(b"fake-compiler") },
        "request_sha256": hex_sha256(raw_request),
        "inputs": { "sources": sources, "library_manifests": [] },
        "resolved_config": {
            "build": request.get("build").cloned().unwrap_or(serde_json::Value::Null),
        },
        "overrides": request.get("overrides").cloned().unwrap_or(serde_json::json!([])),
        "outputs": { "wasm_sha256": hex_sha256(&wasm) },
        "diagnostics": diagnostics,
        "timings": {},
    });

    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join("component.wasm"), &wasm)?;
    std::fs::write(
        out_dir.join("build-manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    std::fs::write(
        out_dir.join("diagnostics.json"),
        serde_json::to_string(&diagnostics)?,
    )?;
    Ok(())
}

/// Emit diagnostics and exit non-zero, mirroring CMP-05: no partial artifact
/// is written on failure.
///
/// The real compiler writes `diagnostics.json` into the output directory even
/// when it rejects the program, and the framework reads it from there — so a
/// stand-in that only printed to stdout would leave that path untested.
fn fail_in(
    out_dir: Option<&std::path::Path>,
    code: u8,
    diagnostic_code: &str,
    message: &str,
) -> ExitCode {
    let diagnostics = serde_json::json!([{
        "level": "error",
        "code": diagnostic_code,
        "message": message,
        "doc_url": format!("https://errors.cleanlanguage.dev/E/{diagnostic_code}"),
    }]);

    if let Some(dir) = out_dir {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join("diagnostics.json"), diagnostics.to_string());
    }

    println!("{diagnostics}");
    eprintln!("fake-compiler: {message}");
    ExitCode::from(code)
}

/// The value following `name` in argv, for `--flag value` pairs.
fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn env_or(default: String, var: &str) -> String {
    std::env::var(var).unwrap_or(default)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

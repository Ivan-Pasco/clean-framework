//! End-to-end through the `clean-framework` binary — the shape Manager
//! actually invokes (Manager §00.4).
//!
//! The orchestration suite covers the build logic in-process; this suite covers
//! what the *binary* contract owes Manager: argv in, a JSON envelope on stdout,
//! diagnostics on stderr, and the right exit code. Those are the parts an
//! in-process test cannot see.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// The `clean-framework` binary under test. Cargo builds it for us and points
/// `CARGO_BIN_EXE_<name>` at it.
fn framework_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clean-framework"))
}

/// `fake-compiler` lives in another crate, so there is no `CARGO_BIN_EXE_` for
/// it here. Build it on demand — see the note in the orchestration suite.
fn fake_compiler() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY
        .get_or_init(|| {
            let mut target_dir = framework_binary();
            target_dir.pop();

            let binary = target_dir.join("fake-compiler");
            if binary.exists() {
                return binary;
            }

            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/fake-compiler/Cargo.toml");
            let status = Command::new(env!("CARGO"))
                .args(["build", "--quiet", "--bin", "fake-compiler", "--manifest-path"])
                .arg(&manifest)
                .status()
                .expect("could not run cargo to build fake-compiler");
            assert!(status.success(), "building fake-compiler failed");
            binary
        })
        .clone()
}

fn hello_world_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("clean.toml"),
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\n[build]\ntarget = \"wasm32-cli\"\n\n[target]\nhost = \"wasmtime_runner\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/main.cln"), "start:\n\tprint(\"hello\")\n").unwrap();
    dir
}

/// The checked-in host contracts these tests build against.
///
/// A directory rather than the real `~/.cln/host-wit/`, so a test run neither
/// reads the developer's cache nor writes to it — and so no test can pass by
/// accident because a contract happened to be cached locally.
fn host_wit_cache() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/host-wit")
}

fn run_build(project: &Path, extra: &[&str]) -> Output {
    Command::new(framework_binary())
        .arg("build")
        .arg(project)
        .arg("--compiler")
        .arg(fake_compiler())
        .arg("--host-wit-cache")
        .arg(host_wit_cache())
        .args(extra)
        .output()
        .expect("could not run clean-framework")
}

fn envelope(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a JSON envelope ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn build_succeeds_and_reports_a_machine_readable_envelope() {
    let project = hello_world_project();
    let output = run_build(project.path(), &[]);

    assert!(
        output.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope = envelope(&output);
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["diagnostics"].as_array().unwrap().len(), 0);
    assert!(envelope["framework_version"].is_string());
    assert_eq!(envelope["request_sha256"].as_str().unwrap().len(), 64);

    // The acceptance criterion for M0: dist/app.wasm exists and is a component.
    let wasm_path = project.path().join("dist/app.wasm");
    assert_eq!(
        Path::new(envelope["dist_wasm"].as_str().unwrap()),
        wasm_path,
        "the envelope must point at the artifact it wrote"
    );
    let wasm = std::fs::read(&wasm_path).unwrap();
    assert_eq!(&wasm[..4], b"\0asm");
}

#[test]
fn a_compiler_rejection_exits_one_with_diagnostics() {
    let project = hello_world_project();
    let output = Command::new(framework_binary())
        .arg("build")
        .arg(project.path())
        .arg("--compiler")
        .arg(fake_compiler())
        .arg("--host-wit-cache")
        .arg(host_wit_cache())
        // Per-process env, so this cannot leak into a concurrent test the way
        // an in-process `set_var` would.
        .env("FAKE_COMPILER_FAIL", "1")
        .env("FAKE_COMPILER_DIAGNOSTIC", "unknown identifier `pritn`")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));

    let envelope = envelope(&output);
    assert_eq!(envelope["status"], "error");
    let diagnostics = envelope["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], "unknown identifier `pritn`");
    assert_eq!(diagnostics[0]["level"], "error");

    // Humans read stderr; Manager reads stdout.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown identifier"), "stderr was: {stderr}");

    assert!(!project.path().join("dist/app.wasm").exists());
}

#[test]
fn a_missing_manifest_exits_one_with_cfg003() {
    let dir = tempfile::tempdir().unwrap();
    let output = run_build(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(1));
    let envelope = envelope(&output);
    assert_eq!(envelope["diagnostics"][0]["code"], "CFG003");
    // A good diagnostic says what to do next (Platform 13 §1).
    assert!(!envelope["diagnostics"][0]["helps"].as_array().unwrap().is_empty());
}

#[test]
fn overrides_reach_the_request_document() {
    let project = hello_world_project();
    let echo = project.path().join("echoed.json");

    let output = Command::new(framework_binary())
        .arg("build")
        .arg(project.path())
        .arg("--compiler")
        .arg(fake_compiler())
        .arg("--host-wit-cache")
        .arg(host_wit_cache())
        .args(["--override", "build.optimization=debug"])
        .env("FAKE_COMPILER_ECHO_REQUEST", &echo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&echo).unwrap()).unwrap();
    let overrides = request["overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[0]["path"], "build.optimization");
    assert_eq!(overrides[0]["value"], "debug");
    assert_eq!(overrides[0]["source"], "cli");
}

#[test]
fn usage_errors_exit_two_not_one() {
    // Manager distinguishes "the build failed" (1) from "you invoked me
    // wrongly" (2); conflating them would make a framework bug look like a
    // user's broken code.
    let project = hello_world_project();
    let output = run_build(project.path(), &["--override", "no-equals-sign"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn version_flag_reports_the_crate_version() {
    let output = Command::new(framework_binary()).arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "got: {text}");
}

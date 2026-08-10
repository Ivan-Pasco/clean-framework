//! Layer C — the real compiler (PLAN.md §7).
//!
//! This is the M0 acceptance criterion end to end: `cln build hello-world`
//! produces `dist/app.wasm`, and `cln run dist/app.wasm` prints "hello".
//!
//! It requires a compiler installed by Manager at
//! `~/.cln/versions/compiler/<version>/clean-compiler` and a `.cln/version`
//! pin naming it. When that is absent the test **skips loudly** rather than
//! passing quietly: a green suite that silently never exercised the real
//! compiler is worse than a visible gap, because it reads as proof of
//! something it never checked.
//!
//! CI provisions this with `cln install compiler <pinned>` before running
//! `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Set to the version to test against, e.g. `CLEAN_TEST_COMPILER=1.4.0`.
const COMPILER_VERSION_VAR: &str = "CLEAN_TEST_COMPILER";

fn framework_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clean-framework"))
}

fn cln_home() -> Option<PathBuf> {
    std::env::var_os("CLN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cln")))
}

/// The installed compiler to test against, or `None` with the reason.
fn installed_compiler() -> Result<(String, PathBuf), String> {
    let home = cln_home().ok_or("no HOME and no CLN_HOME")?;
    let versions = home.join("versions/compiler");
    if !versions.is_dir() {
        return Err(format!("{} does not exist", versions.display()));
    }

    // An explicit pin wins; otherwise take whatever single version is present.
    if let Ok(version) = std::env::var(COMPILER_VERSION_VAR) {
        let binary = versions.join(&version).join("clean-compiler");
        return if binary.exists() {
            Ok((version, binary))
        } else {
            Err(format!("{COMPILER_VERSION_VAR}={version} but {} is missing", binary.display()))
        };
    }

    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(&versions)
        .map_err(|e| format!("could not read {}: {e}", versions.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = entry.file_name().to_string_lossy().into_owned();
            let binary = entry.path().join("clean-compiler");
            binary.exists().then_some((version, binary))
        })
        .collect();
    found.sort();

    found
        .pop()
        .ok_or_else(|| format!("no clean-compiler installed under {}", versions.display()))
}

fn hello_world_project(compiler_version: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/hello-world");

    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::copy(fixture.join("clean.toml"), dir.path().join("clean.toml")).unwrap();
    std::fs::copy(fixture.join("app/main.cln"), dir.path().join("app/main.cln")).unwrap();

    // The pin Manager would have written (Manager §00.3.3).
    std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
    std::fs::write(dir.path().join(".cln/version"), compiler_version).unwrap();

    dir
}

/// `cargo test -- --ignored` runs this. It is not in the default suite because
/// it needs a provisioned toolchain (PLAN.md §7: Layer C gates releases, not
/// day-to-day commits).
#[test]
#[ignore = "requires a Manager-installed compiler; run with --ignored in CI"]
fn hello_world_builds_with_the_real_compiler() {
    let (version, binary) = match installed_compiler() {
        Ok(found) => found,
        Err(reason) => panic!(
            "Layer C cannot run: {reason}.\n\
             Provision with `cln install compiler <version>` and re-run, or set \
             {COMPILER_VERSION_VAR}=<version>."
        ),
    };

    eprintln!("testing against compiler {version} at {}", binary.display());
    let project = hello_world_project(&version);

    // No --compiler flag: this exercises the real `.cln/version` resolution
    // path, which is the thing Manager depends on.
    let output = Command::new(framework_binary())
        .arg("build")
        .arg(project.path())
        .output()
        .expect("could not run clean-framework");

    assert!(
        output.status.success(),
        "build failed:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let wasm_path = project.path().join("dist/app.wasm");
    assert!(wasm_path.exists(), "dist/app.wasm was not produced");

    let wasm = std::fs::read(&wasm_path).unwrap();
    assert_eq!(&wasm[..4], b"\0asm", "dist/app.wasm is not WASM");
    assert!(wasm.len() > 8, "dist/app.wasm is only a preamble — no real code was emitted");

    // The build manifest must name the compiler that actually ran, read from
    // the binary rather than the folder name (PLAN.md open question #2).
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.path().join("dist/build-manifest.json")).unwrap())
            .unwrap();
    assert!(
        manifest["compiler"]["version"].is_string(),
        "build manifest records no compiler version"
    );
}

/// The other half of the acceptance criterion: `cln run dist/app.wasm` prints
/// "hello". `run` is Manager-owned (Manager §00.4), so this drives `cln`.
#[test]
#[ignore = "requires a Manager-installed compiler and runtime; run with --ignored in CI"]
fn hello_world_runs_and_prints_hello() {
    let (version, _) = installed_compiler()
        .unwrap_or_else(|reason| panic!("Layer C cannot run: {reason}"));
    let project = hello_world_project(&version);

    let build = Command::new(framework_binary())
        .arg("build")
        .arg(project.path())
        .output()
        .expect("could not run clean-framework");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new("cln")
        .arg("run")
        .arg(project.path().join("dist/app.wasm"))
        .output()
        .expect("could not run `cln run` — is Manager on PATH?");

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "cln run failed:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout.contains("hello"), "expected \"hello\" in output, got: {stdout:?}");
}

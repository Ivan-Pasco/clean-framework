//! Layer B+ — the whole pipeline carrying a *real* WASM component.
//!
//! The rest of the suite runs against the fake compiler's default output: an
//! 8-byte component preamble. That is deliberate and sufficient for anything
//! above the seam, because the framework never looks past the header (see
//! `testing/fake-compiler`).
//!
//! It is not sufficient for the deploy contract. A preamble is not
//! instantiable, so a `.clapp` built from one packages, verifies, and then
//! fails the moment a runtime tries to run it — the failure lands at deploy
//! time, in whichever component receives the archive, not here where it was
//! introduced. These tests close that gap without waiting on the compiler: the
//! fixture at `testing/fixtures/prebuilt/hello-cli.wasm` is a genuine
//! component, built with `wasm-tools` from the `.wat` beside it, exporting
//! exactly the `cli` world that `testing/fixtures/host-wit/clean-cli@0.1.0.wit`
//! declares.
//!
//! What that buys, concretely: the bytes that reach `app.wasm` inside the
//! archive are proved to survive the compiler seam, the dist write, and the
//! ZIP round-trip unaltered, and to still be a valid component with the right
//! imports and exports at the far end. When the real compiler arrives, the
//! only thing that changes is who produces the fixture.
//!
//! The last test in this file goes further and closes the loop: it unzips the
//! `.clapp` and runs the extracted component through the real `clean-cli` host,
//! asserting the exact bytes on stdout and the exit code. That is the only
//! check here that proves the artifact is *runnable* rather than merely
//! well-formed, so it is the one that would catch a `[guest] wasm` path that
//! resolves from the wrong directory — a defect every other assertion passes.
//!
//! The fixture is regenerated with `python3 gen.py && wasm-tools parse
//! hello-cli.wat -o hello-cli.wasm` in `testing/fixtures/prebuilt/`. It is
//! authored at the component level rather than through `component embed`,
//! because WASI 0.3's stream-based stdout needs canonical built-ins a core
//! module cannot express — see the header of `gen.py`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// The WASM component preamble every component starts with: magic, version
/// 0x0d, layer 1. A file that is *only* these 8 bytes is the fake compiler's
/// default — recognizably a component, but not an instantiable one.
const COMPONENT_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

fn framework_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clean-framework"))
}

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

fn host_wit_cache() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/host-wit")
}

/// The prebuilt component standing in for compiler output.
fn prebuilt_component() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/fixtures/prebuilt/hello-cli.wasm")
}

/// A copy of the `hello-world` fixture — the `cli` world, matching the
/// component the fake compiler is pointed at.
fn hello_world_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/hello-world");

    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::copy(fixture.join("clean.toml"), dir.path().join("clean.toml")).unwrap();
    std::fs::copy(fixture.join("app/main.cln"), dir.path().join("app/main.cln")).unwrap();
    dir
}

fn run(verb: &str, project: &Path) -> Output {
    Command::new(framework_binary())
        .arg(verb)
        .arg(project)
        .arg("--compiler")
        .arg(fake_compiler())
        .arg("--host-wit-cache")
        .arg(host_wit_cache())
        // Hand the seam a real component instead of a preamble. This is the
        // single substitution these tests rest on, and it lives at the
        // subprocess boundary rather than inside any shipped code path.
        .env("FAKE_COMPILER_WASM", prebuilt_component())
        .env("SOURCE_DATE_EPOCH", "1786708800")
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

fn read_from_archive(archive: &Path, path: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(archive).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name(path).ok()?;
    let mut out = Vec::new();
    entry.read_to_end(&mut out).ok()?;
    Some(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The fixture must actually be a component, not a preamble or a core module.
/// If this fails, every other test in the file is testing nothing, so it is
/// checked directly rather than inferred from the ones downstream.
#[test]
fn the_fixture_is_a_real_component_not_a_preamble() {
    let wasm = std::fs::read(prebuilt_component()).expect(
        "testing/fixtures/prebuilt/hello-cli.wasm is missing — regenerate it \
         with the wasm-tools invocation in this file's header comment",
    );

    assert_eq!(&wasm[..4], b"\0asm", "fixture is not WASM at all");
    assert_eq!(
        &wasm[..8],
        &COMPONENT_PREAMBLE,
        "fixture is a core module, not a component"
    );
    assert!(
        wasm.len() > COMPONENT_PREAMBLE.len(),
        "fixture is only a preamble — it carries no code, which is the exact \
         condition these tests exist to rule out"
    );
}

/// `build` must deliver the component the compiler emitted, byte for byte.
/// Anything the framework did to it in between would surface at instantiation
/// time in the runtime, far from the cause.
#[test]
fn build_writes_the_compilers_component_through_unaltered() {
    let project = hello_world_project();
    let output = run("build", project.path());

    assert!(
        output.status.success(),
        "build failed:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let envelope = envelope(&output);
    assert_eq!(envelope["status"], "ok");

    let dist = std::fs::read(project.path().join("dist/app.wasm")).expect("dist/app.wasm");
    let expected = std::fs::read(prebuilt_component()).unwrap();
    assert_eq!(
        dist, expected,
        "dist/app.wasm is not the component the compiler emitted"
    );

    // The envelope's hash is the one a caller would verify against, so it has
    // to describe the bytes actually on disk.
    assert_eq!(
        envelope["wasm_sha256"].as_str().unwrap(),
        sha256_hex(&dist),
        "envelope wasm_sha256 does not match dist/app.wasm"
    );
}

/// The end of the producer half: a `.clapp` whose `app.wasm` is the real
/// component, with a manifest hash that matches it.
#[test]
fn the_clapp_carries_the_real_component_and_a_matching_integrity_hash() {
    let project = hello_world_project();
    let output = run("package", project.path());

    assert!(
        output.status.success(),
        "package failed:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let envelope = envelope(&output);
    assert_eq!(envelope["status"], "ok");
    assert_eq!(
        envelope["kind"], "clapp",
        "a non-server world packages as a clapp"
    );

    let archive = PathBuf::from(envelope["package"].as_str().unwrap());
    let packaged = read_from_archive(&archive, "app.wasm")
        .expect("a clapp puts its single component at app.wasm");

    let expected = std::fs::read(prebuilt_component()).unwrap();
    assert_eq!(
        packaged, expected,
        "the archived component differs from what the compiler emitted — the \
         ZIP round-trip altered it"
    );

    // The manifest is what a consumer checks before running anything, so a
    // hash that does not match the payload is the one failure that must never
    // ship.
    let raw = read_from_archive(&archive, "manifest.toml").expect("manifest.toml at the root");
    let manifest: toml::Value = toml::from_str(&String::from_utf8(raw).unwrap()).unwrap();

    let declared = manifest["integrity"]["wasm_sha256"]["app.wasm"]
        .as_str()
        .expect("manifest declares no hash for app.wasm");
    assert_eq!(
        declared,
        sha256_hex(&packaged),
        "manifest.toml's integrity hash does not match the archived app.wasm"
    );

    assert_eq!(manifest["artifact"]["entry_wasm"].as_str(), Some("app.wasm"));
    assert_eq!(
        manifest["artifact"]["worlds"]
            .as_array()
            .map(|w| w.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
        Some(vec!["cli"]),
        "the archive must name the world it was built for"
    );
}

/// The property the whole exercise is for: what comes out of the archive is
/// still a valid component declaring the `cli` world's imports and exports.
/// This is the check the runtime would otherwise be the first to make.
#[test]
fn the_archived_component_still_validates_and_matches_the_cli_world() {
    let project = hello_world_project();
    let output = run("package", project.path());
    assert!(output.status.success());

    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());
    let packaged = read_from_archive(&archive, "app.wasm").expect("app.wasm");

    // Parsing with wasmparser in component mode is what proves instantiability
    // is even possible; a preamble reaches this line and fails it.
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&packaged)
        .expect("the archived component does not validate");

    // Imports and exports are the contract with the host. Checking the names
    // catches a component that validates but targets the wrong world — which
    // would deploy cleanly and then trap on a missing import.
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(&packaged) {
        match payload.expect("parsing the archived component") {
            wasmparser::Payload::ComponentImportSection(section) => {
                for import in section {
                    imports.push(import.expect("component import").name.name.to_string());
                }
            }
            wasmparser::Payload::ComponentExportSection(section) => {
                for export in section {
                    exports.push(export.expect("component export").name.name.to_string());
                }
            }
            _ => {}
        }
    }

    // The `cli-default` contract, as clean-cli's host.wit declares it: stdout
    // arrives through WASI 0.3's stream-based interface, not through a
    // Clean-specific console import.
    assert!(
        imports.iter().any(|i| i.contains("wasi:cli/stdout@0.3.0")),
        "the component does not import wasi:cli/stdout@0.3.0, which is how a \
         cli-default guest writes to stdout; got {imports:?}"
    );
    assert!(
        exports.iter().any(|e| e == "run"),
        "the component does not export `run`, the cli-default world's entry \
         point; got {exports:?}"
    );
}

/// The `.clapp` must carry a `config/host.toml` whose `[guest] wasm` resolves
/// to the component beside it (FRM-BO-11, Manager §00.13).
///
/// Checked separately from the run test below so the reason for a failure is
/// legible without a runtime installed: this asserts the agreement, the run
/// test proves it.
#[test]
fn the_clapp_carries_a_host_config_pointing_at_its_own_component() {
    let project = hello_world_project();
    let output = run("package", project.path());
    assert!(output.status.success());

    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());
    let raw = read_from_archive(&archive, "config/host.toml").expect(
        "a .clapp must carry config/host.toml — without one `cln run` has no \
         configuration to pass, and CLNH-13 makes that a startup error",
    );
    let config: toml::Value = toml::from_str(&String::from_utf8(raw).unwrap())
        .expect("config/host.toml is not valid TOML");

    // The path agreement. `config/host.toml` sits one level below the archive
    // root, and host-core resolves relative paths against the config file's
    // own directory — so reaching `app.wasm` at the root means going up one.
    assert_eq!(
        config["guest"]["wasm"].as_str(),
        Some("../app.wasm"),
        "[guest] wasm must resolve from config/ to the archive root's app.wasm"
    );
    assert_eq!(
        config["guest"]["world"].as_str(),
        Some("cli-default"),
        "a cli target declares the default-handler world clean-cli implements"
    );

    // The three keys host-core requires; an absent one is a startup error.
    for key in ["name", "version", "component-model"] {
        assert!(
            config["host"].get(key).is_some(),
            "[host] {key} is required by CLNH-13 and is missing"
        );
    }

    // FRM-BO-14: deployment values are the operator's. A packaged artifact
    // that declared its own mode would let a build decide how it is deployed.
    assert!(
        config["host"].get("deployment-mode").is_none(),
        "the generated config must not declare deployment-mode"
    );
}

/// The producer/consumer loop, closed: unzip the `.clapp` and run its
/// component through the real `clean-cli` host using the archive's own
/// configuration, exactly as `cln run` will.
///
/// This is the test that would have caught both gaps this file previously had.
/// A guest exporting the wrong entry point, or a config whose `[guest] wasm`
/// resolves from the wrong directory, both produce an archive that passes
/// every structural assertion above and then cannot be executed.
///
/// Skips when no runtime binary is present. The framework's suite must not
/// require a sibling repository to be built, so a missing runtime is a skip
/// rather than a failure — but the path is reported, so a silent skip in CI is
/// visible in the log rather than looking like a pass.
#[test]
fn the_packaged_artifact_actually_runs_and_prints_hello() {
    let Some(runtime) = clean_runtime() else {
        eprintln!(
            "SKIP: no clean-runtime binary found. Build one at \
             ../clean-runtime/target/release/clean-runtime, or set \
             CLEAN_RUNTIME, to run the end-to-end check."
        );
        return;
    };

    let project = hello_world_project();
    let output = run("package", project.path());
    assert!(output.status.success());
    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());

    // Unpack the way a consumer does: everything, preserving layout, so the
    // relative path in the config resolves against real directories rather
    // than one the test arranged to be convenient.
    let unpacked = tempfile::tempdir().unwrap();
    let file = std::fs::File::open(&archive).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let Some(path) = entry.enclosed_name() else { continue };
        let target = unpacked.path().join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        std::fs::write(&target, bytes).unwrap();
    }

    let wasm = unpacked.path().join("app.wasm");
    let config = unpacked.path().join("config/host.toml");
    assert!(wasm.exists(), "app.wasm missing from the unpacked archive");
    assert!(config.exists(), "config/host.toml missing from the unpacked archive");

    let run = Command::new(&runtime)
        .arg("--world=cli")
        .arg(&wasm)
        .arg(format!("--config={}", config.display()))
        .output()
        .expect("could not run clean-runtime");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    assert!(
        run.status.success(),
        "the packaged artifact did not run\n  exit: {:?}\n  stdout: {stdout:?}\n  stderr: {stderr}",
        run.status.code()
    );
    // CLIH-10 makes stdout byte-exact: the guest's output and nothing else.
    assert_eq!(stdout, "hello\n", "stdout is not exactly the guest's output");
    assert_eq!(run.status.code(), Some(0), "exit code is not 0");
    // A warning here would mean the generated config disagrees with the guest —
    // `[guest] world` not matching the guest's exports is the likely cause, and
    // it is exactly the kind of drift that otherwise goes unnoticed.
    assert!(
        stderr.is_empty(),
        "the host wrote to stderr, which means it had something to complain \
         about: {stderr}"
    );
}

/// The runtime binary to run the packaged artifact against, if one exists.
///
/// `CLEAN_RUNTIME` wins so CI can point at wherever it built one; otherwise a
/// sibling checkout's release build is the conventional location.
fn clean_runtime() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLEAN_RUNTIME") {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }

    let sibling = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../clean-runtime/target/release/clean-runtime");
    sibling.exists().then_some(sibling)
}

/// Packaging twice must produce the same archive, hash included. The prebuilt
/// component makes this worth asserting on real payload bytes rather than on a
/// constant preamble that could never have varied.
#[test]
fn packaging_the_same_component_twice_is_byte_identical() {
    let first = hello_world_project();
    let second = hello_world_project();

    let one = envelope(&run("package", first.path()));
    let two = envelope(&run("package", second.path()));

    assert_eq!(
        one["package_sha256"], two["package_sha256"],
        "two packages of identical inputs differ — something in the archive \
         still varies run to run"
    );

    let bytes_one = std::fs::read(one["package"].as_str().unwrap()).unwrap();
    let bytes_two = std::fs::read(two["package"].as_str().unwrap()).unwrap();
    assert_eq!(bytes_one, bytes_two, "archives differ byte for byte");
    assert_eq!(
        sha256_hex(&bytes_one),
        one["package_sha256"].as_str().unwrap(),
        "the reported package_sha256 is not the hash of the file written"
    );
}

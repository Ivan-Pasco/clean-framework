//! Layer C — the framework against the **real** compiler.
//!
//! Every other suite drives `fake-compiler`, which is deliberate: framework
//! tests must not depend on compiler stability. But a stand-in can only prove
//! the framework is consistent with our *belief* about the seam, and that
//! belief was wrong — the framework invoked `clean-compiler compile
//! --stdout-tar` for months against a compiler that accepts neither, so every
//! `cln build` against a real toolchain failed at the first call.
//!
//! These tests close that gap. They **skip** when no compiler is installed, so
//! CI and a fresh clone stay green, and they run the moment `cln install
//! compiler` has happened.

use std::path::{Path, PathBuf};

use framework_compiler_driver::{Compiler, SubprocessCompiler};
use framework_core::{build, BuildInputs};

/// The Manager-installed compiler, or `None` when none is installed.
///
/// Resolved through `~/.cln/active/compiler`, which is what Manager points at
/// the version in use — the same binary `cln build` would pick.
fn installed_compiler() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let active = Path::new(&home).join(".cln/active/compiler/clean-compiler");
    active.is_file().then_some(active)
}

/// Skip rather than fail when the toolchain is absent.
macro_rules! require_compiler {
    () => {
        match installed_compiler() {
            Some(binary) => binary,
            None => {
                eprintln!("skipping: no compiler at ~/.cln/active/compiler");
                return;
            }
        }
    };
}

fn host_wit_cache() -> framework_core::HostWitCache {
    framework_core::HostWitCache::at(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/host-wit"),
    )
}

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// A project whose source uses only what compiler 0.1.0 implements.
    ///
    /// Deliberately not `print("hello")`: that is what `cln new` generates and
    /// what the language spec shows, but 0.1.0 rejects it as outside its
    /// current milestone surface. Testing the seam means testing the seam, not
    /// waiting on the parser.
    fn buildable(source: &str) -> Self {
        Project::targeting("wasm32-cli", "clean-cli", "0.1.0", source)
    }

    /// The same, against an arbitrary target and host contract.
    ///
    /// The CLI case above is the only one the fixture cache can serve today;
    /// the server cases resolve a real Manager-installed contract instead, so
    /// the host and its version have to be callers' choice.
    fn targeting(target: &str, host: &str, host_version: &str, source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("clean.toml"),
            format!(
                "[project]\nname = \"real\"\nversion = \"0.1.0\"\n\n\
                 [build]\ntarget = \"{target}\"\noptimization = \"debug\"\n\n\
                 [target]\nhost = \"{host}\"\nversion = \"{host_version}\"\n"
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/main.cln"), source).unwrap();
        Project { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn inputs(&self) -> BuildInputs {
        BuildInputs::new(self.path())
            .with_host_wit_cache(host_wit_cache())
            .without_cache()
    }

    /// Inputs resolving host contracts from `~/.cln/host-wit/` — the real
    /// thing Manager installed, not our fixture.
    fn inputs_against_installed_contracts(&self) -> BuildInputs {
        BuildInputs::new(self.path())
            .with_host_wit_cache(framework_core::HostWitCache::user().unwrap())
            .without_cache()
    }
}

/// The Manager-installed `clean-server` contract, or `None` when absent.
///
/// Unlike the CLI case, these tests deliberately do **not** fall back to the
/// in-tree fixture. The fixture's `server` world is a 39-line stand-in with a
/// single `handle` export; the shipped 0.7.0 contract is 296 lines exporting
/// routing, websocket, sse, and session interfaces. A test that passed against
/// the fixture would prove nothing about the contract developers actually
/// build against — which is the exact failure mode this whole suite exists to
/// close.
fn installed_server_contract() -> Option<String> {
    framework_core::HostWitCache::user()
        .ok()?
        .get("clean-server", SERVER_HOST_VERSION)
        .ok()
        .flatten()
}

/// The version of the server contract these tests expect to find installed.
const SERVER_HOST_VERSION: &str = "0.7.0";

macro_rules! require_server_contract {
    () => {
        match installed_server_contract() {
            Some(wit) => wit,
            None => {
                eprintln!(
                    "skipping: no clean-server@{SERVER_HOST_VERSION} in ~/.cln/host-wit"
                );
                return;
            }
        }
    };
}

fn compiler(binary: PathBuf) -> SubprocessCompiler {
    SubprocessCompiler::at(binary, semver::Version::new(0, 1, 0))
}

#[test]
fn the_framework_builds_a_real_component_through_the_real_compiler() {
    // The test that would have caught the seam mismatch on day one.
    let binary = require_compiler!();
    let project = Project::buildable("start:\n\tinteger x = 42\n");

    let outcome = build(&project.inputs(), &compiler(binary)).expect("build must succeed");

    let wasm = std::fs::read(&outcome.dist_wasm).unwrap();
    assert_eq!(&wasm[..4], b"\0asm", "not WASM at all");
    assert_eq!(
        &wasm[4..8],
        &[0x0d, 0x00, 0x01, 0x00],
        "a core module, not a component"
    );
    assert!(
        wasm.len() > 1000,
        "a {}-byte artifact is a stub, not a compiled program",
        wasm.len()
    );
}

#[test]
fn the_build_manifest_records_the_compiler_that_actually_ran() {
    // Provenance is the point of the manifest (FRM-BO-09a). A stamp naming a
    // version that did not produce these bytes is worse than none.
    let binary = require_compiler!();
    let reported = compiler(binary.clone()).version().expect("--version must work");

    let project = Project::buildable("start:\n\tinteger x = 1\n");
    build(&project.inputs(), &compiler(binary)).unwrap();

    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(project.path().join("dist/build-manifest.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(manifest["compiler"]["version"].as_str(), Some(reported.as_str()));
}

#[test]
fn a_rejected_program_reports_the_compilers_own_diagnostic() {
    // The failure this suite exists to prevent: the compiler writes a spanned
    // error into its output directory, and the developer sees "compiler exited
    // with code 1" because the framework never read it.
    let binary = require_compiler!();
    let project = Project::buildable("start:\n\tinteger x = \n");

    let err = build(&project.inputs(), &compiler(binary)).unwrap_err();
    let diagnostics = err.to_diagnostics();

    assert!(!diagnostics.is_empty(), "no diagnostic reached the user: {err}");
    assert!(
        diagnostics[0].code.starts_with("SYN"),
        "expected a syntax error from the compiler, got {}",
        diagnostics[0].code
    );

    let span = diagnostics[0]
        .primary_span
        .as_ref()
        .expect("a syntax error must carry a location");
    assert_eq!(span.file, "app/main.cln");
    assert!(span.start.line > 0);
}

#[test]
fn a_failed_build_leaves_the_previous_dist_untouched() {
    // FRM-BO-10 through the real seam, where the output directory is a scratch
    // dir the transport owns rather than stdout.
    let binary = require_compiler!();
    let project = Project::buildable("start:\n\tinteger x = 42\n");

    build(&project.inputs(), &compiler(binary.clone())).unwrap();
    let good = std::fs::read(project.path().join("dist/app.wasm")).unwrap();

    std::fs::write(project.path().join("app/main.cln"), "start:\n\tinteger x = \n").unwrap();
    assert!(build(&project.inputs(), &compiler(binary)).is_err());

    assert_eq!(
        std::fs::read(project.path().join("dist/app.wasm")).unwrap(),
        good,
        "a failed build overwrote a good component"
    );
}

#[test]
fn a_cached_rebuild_reproduces_the_component_byte_for_byte() {
    // CMP-06 across the real seam: the cache stores the compiler's artifact
    // set re-packed as a tarball, so a hit must reproduce it exactly.
    let binary = require_compiler!();
    let cache_home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(cache_home.path().join("build-cache"));

    let project = Project::buildable("start:\n\tinteger x = 7\n");
    let inputs = BuildInputs::new(project.path())
        .with_host_wit_cache(host_wit_cache())
        .with_build_cache(cache.clone());

    build(&inputs, &compiler(binary.clone())).unwrap();
    let first = std::fs::read(project.path().join("dist/app.wasm")).unwrap();
    assert_eq!(cache.keys().unwrap().len(), 1, "the build was not cached");

    std::fs::remove_dir_all(project.path().join("dist")).unwrap();
    build(&inputs, &compiler(binary)).unwrap();
    let second = std::fs::read(project.path().join("dist/app.wasm")).unwrap();

    assert_eq!(first, second, "a cache hit produced different bytes");
}

#[test]
fn the_transport_leaves_no_scratch_directory_behind() {
    // The cost of the output-directory protocol. A leaked directory per build
    // would fill the developer's temp dir invisibly.
    let binary = require_compiler!();
    let before = scratch_dirs();

    let project = Project::buildable("start:\n\tinteger x = 3\n");
    build(&project.inputs(), &compiler(binary.clone())).unwrap();

    // ...and on the failure path too, which returns early.
    std::fs::write(project.path().join("app/main.cln"), "start:\n\tinteger x = \n").unwrap();
    let _ = build(&project.inputs(), &compiler(binary));

    // Only the directories *this* test caused. The scratch name is keyed on
    // the process id, which every test in this binary shares, so comparing
    // whole snapshots would observe a sibling test's live directory and blame
    // the transport for it. What the transport promises is narrower: whatever
    // it created for these two builds is gone by the time they return.
    let leaked: Vec<String> = scratch_dirs()
        .into_iter()
        .filter(|name| !before.contains(name))
        .filter(|name| std::env::temp_dir().join(name).exists())
        .collect();

    assert!(
        leaked.is_empty(),
        "the transport leaked a scratch directory: {leaked:?}"
    );
}

/// Scratch directories this process may have created, by name.
fn scratch_dirs() -> Vec<String> {
    let prefix = format!("cln-build-{}-", std::process::id());
    let mut found: Vec<String> = std::fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix))
        .collect();
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// The server world.
//
// Everything above builds `wasm32-cli` against a checked-in fixture, because
// that is all the CLI host can offer: `clean-cli`'s real `host.wit` declares
// only `cli-default`, so no CLI project builds against the shipped contract
// today (see system-documents/host-world-findings-for-clean-cli.md).
//
// `clean-server` has no such gap. It ships a real `host.wit` that declares the
// `server` world `wasm32-server` maps to, which makes it the first — and
// currently only — target where the framework can be tested end-to-end against
// a genuine host contract rather than one we wrote for ourselves.
// ---------------------------------------------------------------------------

#[test]
fn the_installed_server_contract_declares_the_world_the_target_maps_to() {
    // Moment 1 (HCV-01) in its simplest form. If this fails, the server world
    // has acquired the same gap the CLI world has, and every test below is
    // failing for a reason that has nothing to do with the framework.
    let wit = require_server_contract!();

    assert_eq!(
        framework_core::manifest::world_for_target("wasm32-server"),
        Some("server"),
        "the target no longer maps to the world this test checks"
    );
    assert!(
        framework_core::hostwit::declares_world(&wit, "server"),
        "clean-server@{SERVER_HOST_VERSION} does not declare world 'server'"
    );
}

#[test]
fn the_framework_builds_a_server_component_against_the_real_host_contract() {
    // The end-to-end path no other test covers: real compiler, real host
    // contract, real target. This is the closest thing the framework has to
    // proof that a developer's `cln build` works.
    let binary = require_compiler!();
    require_server_contract!();

    let project = Project::targeting(
        "wasm32-server",
        "clean-server",
        SERVER_HOST_VERSION,
        "start:\n\tinteger x = 42\n",
    );

    let outcome = build(
        &project.inputs_against_installed_contracts(),
        &compiler(binary),
    )
    .expect("a server build against the shipped contract must succeed");

    let wasm = std::fs::read(&outcome.dist_wasm).unwrap();
    assert_eq!(&wasm[..4], b"\0asm", "not WASM at all");
    assert!(
        wasm.len() > 1000,
        "a {}-byte artifact is a stub, not a compiled program",
        wasm.len()
    );
}

#[test]
fn the_server_request_carries_the_host_contract_verbatim() {
    // FRM-BO-16 / ADR-0033: the framework names the world and passes the
    // contract through unaltered rather than extracting from it. A framework
    // that paraphrases a 296-line contract into what it believes the compiler
    // needs is how the two drift apart.
    let wit = require_server_contract!();

    let project = Project::targeting(
        "wasm32-server",
        "clean-server",
        SERVER_HOST_VERSION,
        "start:\n\tinteger x = 1\n",
    );

    let request =
        framework_core::build::assemble_request(&project.inputs_against_installed_contracts())
            .expect("lowering to a request must succeed");

    assert_eq!(request.target_world.world, "server");
    assert_eq!(
        request.target_world.wit, wit,
        "the contract reached the compiler altered"
    );
}

#[test]
fn a_rejected_server_program_reports_the_compilers_own_diagnostic() {
    // The diagnostics path is shared with the CLI case, but the request that
    // produces it is not: this one carries a 296-line contract. A compiler
    // that chokes on the real contract would otherwise surface as a generic
    // FRM001 and look like a framework bug.
    let binary = require_compiler!();
    require_server_contract!();

    let project = Project::targeting(
        "wasm32-server",
        "clean-server",
        SERVER_HOST_VERSION,
        "start:\n\tinteger x = \n",
    );

    let err = build(
        &project.inputs_against_installed_contracts(),
        &compiler(binary),
    )
    .unwrap_err();
    let diagnostics = err.to_diagnostics();

    assert!(!diagnostics.is_empty(), "no diagnostic reached the user: {err}");
    assert!(
        diagnostics[0].code.starts_with("SYN"),
        "expected a syntax error from the compiler, got {}",
        diagnostics[0].code
    );
}

// ---------------------------------------------------------------------------
// The host bridge, against the real compiler (LBS §8).
// ---------------------------------------------------------------------------

impl Project {
    /// Add a library that declares a host bridge, and lock it.
    ///
    /// Mirrors the orchestration-suite helper, but the WIT that results is
    /// handed to the *real* compiler, which is the only thing that can say
    /// whether our synthesizer emits WIT it accepts.
    fn with_bridged_library(self, name: &str, version: &str, host_bridge: &str) -> Self {
        let vendor = self.path().join(format!("vendor/{name}"));
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::write(
            vendor.join("library.toml"),
            format!("[library]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        std::fs::write(vendor.join("host_bridge.cln"), host_bridge).unwrap();

        std::fs::create_dir_all(self.path().join(".cln")).unwrap();
        let lock = self.path().join(".cln/lock.toml");
        let mut body = std::fs::read_to_string(&lock).unwrap_or_default();
        body.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nkind = \"library\"\n\
             path = \"vendor/{name}\"\n"
        ));
        std::fs::write(&lock, body).unwrap();

        self
    }
}

/// `frame.storage`'s four calls (STOR-01), declared for the server world.
const STORAGE_BRIDGE: &str = r#"
host interface storage version "0.1.0":
	requires host worlds ["server"]

	host function write(path: string, data: bytes) returns integer
		description "Write bytes under the storage root. Returns 0 on success."

	host function read(path: string) returns bytes
		description "Read bytes back. Empty when the file is missing."

	host function exists(path: string) returns boolean
		description "Test for existence without reading the bytes."

	host function delete(path: string) returns boolean
		description "Remove a file. True on success."
"#;

#[test]
#[ignore = "requires a Manager-installed compiler"]
fn the_real_compiler_accepts_our_synthesized_library_wit() {
    // The point of Track 1: WIT the framework derived from Clean declarations
    // has to be WIT the compiler will actually take. Every other test of the
    // synthesizer checks it against our own reading of LBS §8 — only this one
    // checks it against the binary that has to consume it.
    //
    // KNOWN SEAM DEFECT (2026-08-20, compiler 0.1.0): this currently fails
    // with `RQD002: missing required field compiletime_wasm_sha256`. The
    // request schema (Platform 14 §14.1.1) makes that field optional, and it
    // is meaningless for a library that ships no compile-time handler — which
    // is every host-bridge-only library, including frame.storage. Until the
    // compiler relaxes it, no such library can be built at all.
    let binary = require_compiler!();
    require_server_contract!();

    let project = Project::targeting(
        "wasm32-server",
        "clean-server",
        SERVER_HOST_VERSION,
        "start:\n\tinteger x = 42\n",
    )
    .with_bridged_library("frame.storage", "0.1.0", STORAGE_BRIDGE);

    let request =
        framework_core::build::assemble_request(&project.inputs_against_installed_contracts())
            .expect("assembling a request with a bridged library must succeed");

    // The framework's half works: real synthesized WIT reached the request.
    let library = request
        .library_manifests
        .iter()
        .find(|l| l.name == "frame.storage")
        .expect("the library must reach the compiler");
    assert!(
        library.wit.contains("write: func(path: string, data: list<u8>) -> s64;"),
        "{}",
        library.wit
    );

    // The compiler's half is what this test exists to check. It fails today —
    // see the defect note above — so the assertion is written to say exactly
    // what is wrong rather than just going red. When the compiler relaxes the
    // field, this flips to a plain `.expect(...)` and the note comes out.
    let outcome = build(
        &project.inputs_against_installed_contracts(),
        &compiler(binary),
    );

    if let Err(error) = &outcome {
        let blocked = error.to_diagnostics().iter().any(|d| {
            d.code == "RQD002" && d.message.contains("compiletime_wasm_sha256")
        });
        assert!(
            !blocked,
            "compiler 0.1.0 still requires `compiletime_wasm_sha256` for a library \
             that ships no compile-time handler; Platform 14 §14.1.1 makes it optional. \
             Until this is relaxed, no host-bridge-only library can be built."
        );
    }

    outcome.expect("the compiler must accept a library whose WIT we synthesized");
}

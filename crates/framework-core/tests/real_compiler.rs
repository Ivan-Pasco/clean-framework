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
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("clean.toml"),
            "[project]\nname = \"real\"\nversion = \"0.1.0\"\n\n\
             [build]\ntarget = \"wasm32-cli\"\noptimization = \"debug\"\n\n\
             [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n",
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

    assert_eq!(before, scratch_dirs(), "the transport leaked a scratch directory");
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

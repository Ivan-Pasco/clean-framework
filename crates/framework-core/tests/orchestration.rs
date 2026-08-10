//! Layer B — orchestration against `fake-compiler` (PLAN.md §7).
//!
//! These drive the *real* subprocess transport against a stand-in binary, so
//! both the orchestration logic and the seam itself are covered without
//! depending on compiler stability. Every FRM-BO rule that survives to M0 gets
//! a test here.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use framework_compiler_driver::{CompileError, Compiler, SubprocessCompiler};
use framework_core::{assemble_request, build, BuildInputs, ConfigOverride};

/// Serializes every test that spawns `fake-compiler`.
///
/// The fake compiler is steered by environment variables, which the child
/// inherits. Environment variables are process-global and cargo runs tests in
/// parallel threads, so a test that sets `FAKE_COMPILER_FAIL` would otherwise
/// leak into any *other* test spawning the compiler at that moment — including
/// tests that set nothing at all. So the lock guards spawning, not just
/// mutation: [`FakeCompilerEnv::none`] is what a test takes when it wants the
/// default behaviour and needs to be sure nobody else has changed it.
///
/// The guard clears its variables on drop, including on panic, so one failing
/// assertion cannot cascade into unrelated failures.
struct FakeCompilerEnv {
    _guard: MutexGuard<'static, ()>,
    keys: Vec<&'static str>,
}

impl FakeCompilerEnv {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // A poisoned lock means an earlier test panicked while holding it. The
        // guard below still cleared the variables, so the state is sound and
        // recovering keeps one failure from cascading.
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for (key, value) in pairs {
            std::env::set_var(key, value);
        }
        FakeCompilerEnv { _guard: guard, keys: pairs.iter().map(|(k, _)| *k).collect() }
    }

    /// Take the lock without setting anything — for tests that need the fake
    /// compiler's default behaviour, uncontaminated by a concurrent test.
    fn none() -> Self {
        Self::set(&[])
    }
}

impl Drop for FakeCompilerEnv {
    fn drop(&mut self) {
        for key in &self.keys {
            std::env::remove_var(key);
        }
    }
}

/// The `fake-compiler` binary.
///
/// `cargo test --workspace` builds it for us, but `cargo test -p
/// framework-core` does not — cargo has no stable way to declare "this test
/// needs that crate's binary" (artifact dependencies are still unstable). So
/// we build it on demand and cache the result for the rest of the run. That
/// keeps every invocation style working instead of documenting a footgun.
fn fake_compiler() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();

    BINARY
        .get_or_init(|| {
            // The integration-test binary lives in target/<profile>/deps/, so
            // its sibling binaries are one level up.
            let mut target_dir = std::env::current_exe().expect("test binary path");
            target_dir.pop();
            if target_dir.ends_with("deps") {
                target_dir.pop();
            }

            let binary = target_dir.join("fake-compiler");
            if binary.exists() {
                return binary;
            }

            let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/fake-compiler/Cargo.toml");
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "--quiet", "--bin", "fake-compiler", "--manifest-path"])
                .arg(&manifest)
                .status()
                .expect("could not run cargo to build fake-compiler");
            assert!(status.success(), "building fake-compiler failed");
            assert!(
                binary.exists(),
                "fake-compiler still missing at {} after build",
                binary.display()
            );
            binary
        })
        .clone()
}

fn compiler() -> SubprocessCompiler {
    SubprocessCompiler::at(fake_compiler(), semver::Version::new(0, 0, 0))
}

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    /// A minimal buildable project: clean.toml + one source file.
    fn hello() -> Self {
        let project = Project { dir: tempfile::tempdir().unwrap() };
        project.write(
            "clean.toml",
            r#"
[project]
name = "hello-world"
version = "0.1.0"

[build]
target = "wasm32-cli"
"#,
        );
        project.write("app/main.cln", "start:\n\tprint(\"hello\")\n");
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, relative: &str, body: &str) {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn write_bytes(&self, relative: &str, body: &[u8]) {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn read(&self, relative: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(relative)).unwrap()
    }

    fn exists(&self, relative: &str) -> bool {
        self.dir.path().join(relative).exists()
    }
}

#[test]
fn hello_world_builds_to_dist_app_wasm() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    let outcome = build(&BuildInputs::new(project.path()), &compiler()).expect("build must succeed");

    // FRM-BO-09: the framework owns naming and placement.
    assert_eq!(outcome.dist_wasm, project.path().join("dist/app.wasm"));
    assert!(project.exists("dist/app.wasm"));
    assert!(project.exists("dist/build-manifest.json"));

    // The bytes are a WASM component preamble, not an empty file.
    let wasm = project.read("dist/app.wasm");
    assert_eq!(&wasm[..4], b"\0asm", "dist/app.wasm must be WASM");

    assert!(outcome.diagnostics.is_empty());
    assert_eq!(outcome.request_sha256.len(), 64);
    assert_eq!(outcome.wasm_sha256.len(), 64);
}

#[test]
fn build_manifest_is_written_verbatim_from_the_compiler() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    build(&BuildInputs::new(project.path()), &compiler()).unwrap();

    let manifest: serde_json::Value =
        serde_json::from_slice(&project.read("dist/build-manifest.json")).unwrap();

    // FRM-BO-09a: manifest values are recorded verbatim — no re-derivation.
    assert_eq!(manifest["spec_version"], "1");
    assert!(manifest["compiler"]["version"].is_string());
    assert!(manifest["request_sha256"].is_string());
    assert_eq!(manifest["inputs"]["sources"][0]["path"], "app/main.cln");
}

#[test]
fn the_request_crossing_the_seam_is_what_we_assembled() {
    let project = Project::hello();
    let echo = project.path().join("echoed-request.json");

    // Drive the real transport, capturing exactly what landed on the
    // compiler's stdin.
    {
        let _env =
            FakeCompilerEnv::set(&[("FAKE_COMPILER_ECHO_REQUEST", &echo.to_string_lossy())]);
        build(&BuildInputs::new(project.path()), &compiler()).unwrap();
    }

    let on_the_wire = std::fs::read(&echo).unwrap();
    let assembled = assemble_request(&BuildInputs::new(project.path()))
        .unwrap()
        .to_canonical_json()
        .unwrap();

    assert_eq!(on_the_wire, assembled, "the seam must not alter the request");
}

#[test]
fn sources_are_sorted_and_hashed_correctly() {
    let project = Project::hello();
    project.write("app/zebra.cln", "z()\n");
    project.write("app/alpha.cln", "a()\n");

    let request = assemble_request(&BuildInputs::new(project.path())).unwrap();
    let paths: Vec<_> = request.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["app/alpha.cln", "app/main.cln", "app/zebra.cln"]);

    // fake-compiler rejects an unsorted or mis-hashed sources array, so a
    // successful build is itself an assertion — but check explicitly too.
    for source in &request.sources {
        let expected = framework_compiler_driver::artifact::hex_sha256(source.content.as_bytes());
        assert_eq!(source.sha256, expected, "{} has a wrong hash", source.path);
    }
}

#[test]
fn overrides_ride_alongside_the_config_frm_bo_08() {
    let project = Project::hello();
    let inputs = BuildInputs::new(project.path())
        .with_overrides(vec![ConfigOverride::cli("build.optimization", "debug")]);

    let request = assemble_request(&inputs).unwrap();
    assert_eq!(request.overrides.len(), 1);
    assert_eq!(request.overrides[0].source, "cli");
    // Not merged into the lowered config.
    assert_eq!(request.build.optimization, None);

    let _env = FakeCompilerEnv::none();
    build(&inputs, &compiler()).expect("a build with overrides must still succeed");
}

#[test]
fn failure_is_total_dist_is_untouched_frm_bo_10() {
    let project = Project::hello();

    // First build succeeds and populates dist/.
    {
        let _env = FakeCompilerEnv::none();
        build(&BuildInputs::new(project.path()), &compiler()).unwrap();
    }
    let good_wasm = project.read("dist/app.wasm");
    let good_manifest = project.read("dist/build-manifest.json");

    // Second build fails at the compiler.
    let result = {
        let _env = FakeCompilerEnv::set(&[
            ("FAKE_COMPILER_FAIL", "1"),
            ("FAKE_COMPILER_DIAGNOSTIC", "unknown identifier `pritn`"),
        ]);
        build(&BuildInputs::new(project.path()), &compiler())
    };

    let err = result.expect_err("compiler failure must fail the build");

    // The previous successful build survives, byte for byte.
    assert_eq!(project.read("dist/app.wasm"), good_wasm);
    assert_eq!(project.read("dist/build-manifest.json"), good_manifest);

    // And the failure carries the compiler's own diagnostic, not an envelope.
    let diagnostics = err.to_diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "unknown identifier `pritn`");
    assert_eq!(diagnostics[0].code, "SEM001");
}

#[test]
fn no_staging_directory_survives_a_failed_build() {
    let project = Project::hello();

    {
        let _env = FakeCompilerEnv::set(&[("FAKE_COMPILER_FAIL", "1")]);
        let _ = build(&BuildInputs::new(project.path()), &compiler());
    }

    let leftovers: Vec<_> = std::fs::read_dir(project.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("dist.tmp-") || name.starts_with("dist.old-"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}

#[test]
fn a_second_successful_build_replaces_dist_cleanly() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    build(&BuildInputs::new(project.path()), &compiler()).unwrap();

    // A stale file from an older build must not survive the swap — dist/ is
    // replaced, not merged.
    project.write("dist/stale-artifact.txt", "left over");
    build(&BuildInputs::new(project.path()), &compiler()).unwrap();

    assert!(project.exists("dist/app.wasm"));
    assert!(!project.exists("dist/stale-artifact.txt"), "dist/ must be replaced, not merged");
}

#[test]
fn warnings_survive_a_successful_build() {
    let project = Project::hello();

    let outcome = {
        let _env = FakeCompilerEnv::set(&[("FAKE_COMPILER_WARN", "unused variable `x`")]);
        build(&BuildInputs::new(project.path()), &compiler())
    };

    let outcome = outcome.unwrap();
    assert_eq!(outcome.diagnostics.len(), 1);
    assert_eq!(outcome.diagnostics[0].message, "unused variable `x`");
    assert!(!outcome.diagnostics[0].is_error(), "a warning must not fail the build");
    assert!(project.exists("dist/app.wasm"));
}

#[test]
fn malformed_compiler_output_is_a_seam_error_not_a_crash() {
    let project = Project::hello();

    let result = {
        let _env = FakeCompilerEnv::set(&[("FAKE_COMPILER_GARBAGE", "1")]);
        build(&BuildInputs::new(project.path()), &compiler())
    };

    let err = result.expect_err("non-tar stdout must fail the build");
    assert_eq!(err.code(), "FRM001");
    assert!(!project.exists("dist/app.wasm"), "no artifact may be written");
}

#[test]
fn non_utf8_source_fails_with_cfg005_before_the_compiler_runs() {
    let project = Project::hello();
    project.write_bytes("app/broken.cln", &[0x73, 0xFF, 0x0A]);

    let err = build(&BuildInputs::new(project.path()), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG005");
    assert!(!project.exists("dist/app.wasm"));
}

#[test]
fn missing_manifest_fails_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/main.cln"), "start:\n\tprint(\"hi\")\n").unwrap();

    let err = build(&BuildInputs::new(dir.path()), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG003");
}

#[test]
fn unknown_target_fails_with_cfg001() {
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-toaster\"\n",
    );

    let err = build(&BuildInputs::new(project.path()), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG001");
}

#[test]
fn the_request_document_is_deterministic() {
    // The framework's own determinism invariant (PLAN.md §7): identical
    // project state must produce a byte-identical request document.
    let project = Project::hello();
    project.write("app/b.cln", "b()\n");
    project.write("app/a.cln", "a()\n");

    let first = assemble_request(&BuildInputs::new(project.path())).unwrap();
    let second = assemble_request(&BuildInputs::new(project.path())).unwrap();
    assert_eq!(first.to_canonical_json().unwrap(), second.to_canonical_json().unwrap());
    assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());
}

#[test]
fn editing_a_source_changes_the_request_hash() {
    // The flip side of determinism: `cln dev` relies on this hash to decide
    // whether anything actually changed (PLAN.md §6 step 4b).
    let project = Project::hello();
    let before = assemble_request(&BuildInputs::new(project.path())).unwrap().sha256().unwrap();

    project.write("app/main.cln", "start:\n\tprint(\"goodbye\")\n");
    let after = assemble_request(&BuildInputs::new(project.path())).unwrap().sha256().unwrap();

    assert_ne!(before, after);
}

#[test]
fn compiler_version_is_read_from_the_binary_not_the_path() {
    // PLAN.md open question #2: we parse `--version` rather than trusting the
    // version encoded in the install folder name.
    let reported = {
        let _env = FakeCompilerEnv::set(&[("FAKE_COMPILER_VERSION", "9.9.9")]);
        compiler().version()
    };

    assert_eq!(reported.unwrap(), "9.9.9");
}

#[test]
fn a_missing_compiler_binary_is_a_spawn_error() {
    let project = Project::hello();
    let absent = SubprocessCompiler::at("/nonexistent/clean-compiler", semver::Version::new(1, 0, 0));

    let err = build(&BuildInputs::new(project.path()), &absent).unwrap_err();
    assert_eq!(err.code(), "FRM001");
    assert!(
        matches!(
            err,
            framework_core::FrameworkError::Compiler(CompileError::Spawn { .. })
        ),
        "got {err:?}"
    );
}

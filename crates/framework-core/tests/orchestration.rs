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

/// The checked-in host contracts, standing in for `~/.cln/host-wit/`.
fn host_wit_cache() -> framework_core::HostWitCache {
    framework_core::HostWitCache::at(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/fixtures/host-wit"),
    )
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

[target]
host = "clean-cli"
version = "0.1.0"
"#,
        );
        project.write("app/main.cln", "start:\n\tprint(\"hello\")\n");
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Build inputs pointed at the checked-in host contracts.
    ///
    /// Every test goes through this rather than `BuildInputs::new` so no test
    /// reads or writes the developer's real `~/.cln/host-wit/` — and so none
    /// can pass by accident because a contract happened to be cached there.
    ///
    /// Caching is off for the same reason, and one more: these tests steer the
    /// fake compiler through environment variables, so two tests with the same
    /// project but different `FAKE_COMPILER_*` settings share a request hash.
    /// A shared cache would serve one test's artifact to the other. Tests that
    /// are *about* caching opt back in with `with_build_cache`.
    fn inputs(&self) -> BuildInputs {
        BuildInputs::new(self.path())
            .with_host_wit_cache(host_wit_cache())
            .without_cache()
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
    let outcome = build(&project.inputs(), &compiler()).expect("build must succeed");

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
    build(&project.inputs(), &compiler()).unwrap();

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
        build(&project.inputs(), &compiler()).unwrap();
    }

    let on_the_wire = std::fs::read(&echo).unwrap();
    let assembled = assemble_request(&project.inputs())
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

    let request = assemble_request(&project.inputs()).unwrap();
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
    let inputs = project.inputs()
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
        build(&project.inputs(), &compiler()).unwrap();
    }
    let good_wasm = project.read("dist/app.wasm");
    let good_manifest = project.read("dist/build-manifest.json");

    // Second build fails at the compiler.
    let result = {
        let _env = FakeCompilerEnv::set(&[
            ("FAKE_COMPILER_FAIL", "1"),
            ("FAKE_COMPILER_DIAGNOSTIC", "unknown identifier `pritn`"),
        ]);
        build(&project.inputs(), &compiler())
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
        let _ = build(&project.inputs(), &compiler());
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
    build(&project.inputs(), &compiler()).unwrap();

    // A stale file from an older build must not survive the swap — dist/ is
    // replaced, not merged.
    project.write("dist/stale-artifact.txt", "left over");
    build(&project.inputs(), &compiler()).unwrap();

    assert!(project.exists("dist/app.wasm"));
    assert!(!project.exists("dist/stale-artifact.txt"), "dist/ must be replaced, not merged");
}

#[test]
fn warnings_survive_a_successful_build() {
    let project = Project::hello();

    let outcome = {
        let _env = FakeCompilerEnv::set(&[("FAKE_COMPILER_WARN", "unused variable `x`")]);
        build(&project.inputs(), &compiler())
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
        build(&project.inputs(), &compiler())
    };

    let err = result.expect_err("non-tar stdout must fail the build");
    assert_eq!(err.code(), "FRM001");
    assert!(!project.exists("dist/app.wasm"), "no artifact may be written");
}

#[test]
fn non_utf8_source_fails_with_cfg005_before_the_compiler_runs() {
    let project = Project::hello();
    project.write_bytes("app/broken.cln", &[0x73, 0xFF, 0x0A]);

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG005");
    assert!(!project.exists("dist/app.wasm"));
}

#[test]
fn missing_manifest_fails_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/main.cln"), "start:\n\tprint(\"hi\")\n").unwrap();

    let err = build(&BuildInputs::new(dir.path()).with_host_wit_cache(host_wit_cache()).without_cache(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG003");
}

#[test]
fn unknown_target_fails_with_cfg001() {
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-toaster\"\n",
    );

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG001");
}

#[test]
fn the_request_document_is_deterministic() {
    // The framework's own determinism invariant (PLAN.md §7): identical
    // project state must produce a byte-identical request document.
    let project = Project::hello();
    project.write("app/b.cln", "b()\n");
    project.write("app/a.cln", "a()\n");

    let first = assemble_request(&project.inputs()).unwrap();
    let second = assemble_request(&project.inputs()).unwrap();
    assert_eq!(first.to_canonical_json().unwrap(), second.to_canonical_json().unwrap());
    assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());
}

#[test]
fn editing_a_source_changes_the_request_hash() {
    // The flip side of determinism: `cln dev` relies on this hash to decide
    // whether anything actually changed (PLAN.md §6 step 4b).
    let project = Project::hello();
    let before = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    project.write("app/main.cln", "start:\n\tprint(\"goodbye\")\n");
    let after = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    assert_ne!(before, after);
}

#[test]
fn the_request_carries_the_host_contract_verbatim() {
    // FRM-BO-16 end to end: the contract on disk reaches the request unchanged,
    // with the world selected from build.target.
    let project = Project::hello();
    let request = assemble_request(&project.inputs()).unwrap();

    let on_disk = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testing/fixtures/host-wit/clean-cli@0.1.0.wit"),
    )
    .unwrap();

    assert_eq!(
        request.target_world.wit, on_disk,
        "the contract must cross the seam byte-for-byte"
    );
    assert_eq!(request.target_world.world, "cli");
    assert_eq!(request.target_world.host, "clean-cli");
    assert_eq!(request.target_world.version, "0.1.0");
    assert_eq!(request.target_world.sha256.len(), 64);
}

#[test]
fn the_first_build_pins_the_contract_hash_in_the_lockfile() {
    // BVER-03: every build is reproducible against a pinned host contract.
    let project = Project::hello();
    assert!(!project.exists(".cln/lock.toml"));

    let request = assemble_request(&project.inputs()).unwrap();

    let lock = String::from_utf8(project.read(".cln/lock.toml")).unwrap();
    assert!(lock.contains("clean-cli"), "lockfile was: {lock}");
    assert!(
        lock.contains(&request.target_world.sha256),
        "the pinned hash must be the contract's: {lock}"
    );
}

#[test]
fn pinning_happens_once_not_on_every_assemble() {
    // `assemble_request` is also how `cln dev` asks "did anything change?", so
    // it runs constantly. Rewriting the lockfile each time would churn a
    // version-controlled file on every keystroke and make the watcher see its
    // own writes.
    let project = Project::hello();
    assemble_request(&project.inputs()).unwrap();

    let after_first = project.read(".cln/lock.toml");
    let mtime = std::fs::metadata(project.path().join(".cln/lock.toml"))
        .unwrap()
        .modified()
        .unwrap();

    assemble_request(&project.inputs()).unwrap();

    assert_eq!(project.read(".cln/lock.toml"), after_first, "content must not change");
    assert_eq!(
        std::fs::metadata(project.path().join(".cln/lock.toml")).unwrap().modified().unwrap(),
        mtime,
        "an already-pinned contract must not be rewritten"
    );
}

#[test]
fn a_lockfile_pinning_a_different_contract_fails_the_build() {
    // The tamper/republish case. Silently rebuilding against a changed contract
    // would defeat the point of pinning it.
    let project = Project::hello();
    project.write(
        ".cln/lock.toml",
        "[host.clean-cli]\nversion = \"0.1.0\"\n\
         sha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    );

    let err = assemble_request(&project.inputs()).unwrap_err();
    assert_eq!(err.code(), "FRM005");
    assert!(err.help().unwrap().contains("lock.toml"), "must say how to re-pin");
}

#[test]
fn offline_builds_from_a_warm_cache() {
    // C-18: every command works offline once the cache is warm.
    let project = Project::hello();
    let request = assemble_request(&project.inputs().offline(true)).unwrap();
    assert_eq!(request.target_world.world, "cli");
}

#[test]
fn offline_with_a_cold_cache_fails_without_reaching_the_compiler() {
    let project = Project::hello();
    let empty = tempfile::tempdir().unwrap();

    let inputs = BuildInputs::new(project.path())
        .with_host_wit_cache(framework_core::HostWitCache::at(empty.path()))
        .without_cache()
        .offline(true);

    let err = build(&inputs, &compiler()).unwrap_err();
    assert_eq!(err.code(), "FRM004");
    assert!(!project.exists("dist/app.wasm"), "nothing may be built without a world");
}

#[test]
fn a_target_whose_world_the_contract_lacks_is_refused_at_moment_1() {
    // The failure worth catching here: one message naming host and world,
    // rather than COM012 on every host-function call site later.
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\n\n\
         [build]\ntarget = \"wasm32-server\"\n\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n",
    );

    let err = assemble_request(&project.inputs()).unwrap_err();
    assert_eq!(err.code(), "CFG001");
    assert!(err.to_string().contains("server"), "must name the world: {err}");
}

#[test]
fn changing_the_contract_changes_the_request_hash() {
    // The cache-key property ADR-0033 turns on: a component validated against a
    // different contract is a different build, even with identical sources.
    let project = Project::hello();
    let baseline = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    // A second cache holding a different contract under the same coordinates.
    let other = tempfile::tempdir().unwrap();
    let cache = framework_core::HostWitCache::at(other.path());
    cache
        .put(
            "clean-cli",
            "0.1.0",
            "package clean:host@0.1.0;\nworld cli {\n  import extra: func();\n}\n",
        )
        .unwrap();

    // The lockfile from the first call pins the original contract, so drop it —
    // otherwise this would (correctly) fail as a mismatch instead.
    std::fs::remove_file(project.path().join(".cln/lock.toml")).unwrap();

    let inputs = BuildInputs::new(project.path()).with_host_wit_cache(cache).without_cache();
    let changed = assemble_request(&inputs).unwrap().sha256().unwrap();

    assert_ne!(baseline, changed);
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

    let err = build(&project.inputs(), &absent).unwrap_err();
    assert_eq!(err.code(), "FRM001");
    assert!(
        matches!(
            err,
            framework_core::FrameworkError::Compiler(CompileError::Spawn { .. })
        ),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Phase 2 — dependency resolution glue (steps 3 and 4).
// ---------------------------------------------------------------------------

impl Project {
    /// Add a library on disk plus the `[[package]]` entry that locks it.
    ///
    /// Returns the lockfile text to append, rather than writing it, so a test
    /// can assemble a whole closure and write it in one go — the lockfile is a
    /// single file and Manager writes it atomically.
    fn library(&self, name: &str, version: &str, wit: &str, deps: &[&str]) -> String {
        self.write(
            &format!("vendor/{name}/library.toml"),
            &format!(
                "[library]\nname = \"{name}\"\nversion = \"{version}\"\n\
                 [exports]\nwit = \"{wit}\"\n"
            ),
        );

        let deps_line = if deps.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = deps.iter().map(|d| format!("\"{d}\"")).collect();
            format!("dependencies = [{}]\n", quoted.join(", "))
        };

        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nkind = \"library\"\n\
             path = \"vendor/{name}\"\n{deps_line}\n"
        )
    }

    fn write_lockfile(&self, body: &str) {
        self.write(".cln/lock.toml", body);
    }
}

#[test]
fn a_locked_library_reaches_the_compiler() {
    // The end-to-end point of Phase 2: a project with a dependency compiles,
    // and the compiler is told about that dependency.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();

    let locked = project.library("frame.data", "2.1.2", "interface data {}", &[]);
    project.write_lockfile(&locked);

    let request = assemble_request(&project.inputs()).expect("must assemble");

    assert_eq!(request.dependencies["frame.data"].version, "2.1.2");
    assert_eq!(request.dependencies["frame.data"].resolved_from, "path");
    assert_eq!(request.library_manifests.len(), 1);
    assert_eq!(request.library_manifests[0].name, "frame.data");
    assert_eq!(request.library_manifests[0].wit, "interface data {}");

    // ...and the whole build still completes.
    build(&project.inputs(), &compiler()).expect("build with a dependency must succeed");
}

#[test]
fn libraries_reach_the_compiler_in_dependency_order() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();

    let mut lock = String::new();
    lock.push_str(&project.library("app.core", "1.0.0", "interface core {}", &["base"]));
    lock.push_str(&project.library("base", "1.0.0", "interface base {}", &[]));
    project.write_lockfile(&lock);

    let request = assemble_request(&project.inputs()).unwrap();
    let names: Vec<&str> =
        request.library_manifests.iter().map(|l| l.name.as_str()).collect();

    // `base` is a dependency of `app.core`, so the compiler sees it first —
    // regardless of the order the lockfile happened to list them in.
    assert_eq!(names, vec!["base", "app.core"]);
}

#[test]
fn a_project_with_dependencies_but_no_lockfile_asks_manager_to_resolve() {
    // §00.8: the framework never resolves. It asks Manager, then reads what
    // Manager wrote.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        r#"
[project]
name = "hello-world"
version = "0.1.0"

[build]
target = "wasm32-cli"

[target]
host = "clean-cli"
version = "0.1.0"

[dependencies]
"frame.data" = "^2.1"
"#,
    );

    let locked = project.library("frame.data", "2.1.2", "interface data {}", &[]);

    // A resolver that does what Manager does: write the lockfile.
    #[derive(Debug)]
    struct Fake {
        lockfile: String,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl framework_core::Resolver for Fake {
        fn resolve(&self, project_root: &Path) -> Result<(), framework_core::ResolveError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::create_dir_all(project_root.join(".cln")).unwrap();
            std::fs::write(project_root.join(".cln/lock.toml"), &self.lockfile).unwrap();
            Ok(())
        }
    }

    let resolver = std::sync::Arc::new(Fake {
        lockfile: locked,
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let inputs = project.inputs().with_resolver(resolver.clone());

    let request = assemble_request(&inputs).expect("must resolve then assemble");

    assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(request.library_manifests.len(), 1);
    assert_eq!(request.dependencies["frame.data"].version, "2.1.2");
}

#[test]
fn a_project_without_dependencies_never_spawns_the_resolver() {
    // The common case must not pay for a subprocess. `NoResolver` fails if
    // called, so a passing build proves it was not.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();

    let inputs = project
        .inputs()
        .with_resolver(std::sync::Arc::new(framework_core::NoResolver));

    build(&inputs, &compiler()).expect("a project with no dependencies needs no resolver");
}

#[test]
fn a_lockfile_entry_without_a_kind_stops_the_build() {
    // PLAN.md open question #1. Guessing would pick a compile strategy by
    // coin-flip; the failure would then surface from the compiler.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        ".cln/lock.toml",
        "[[package]]\nname = \"frame.data\"\nversion = \"2.1.2\"\npath = \"vendor/frame.data\"\n",
    );

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG002");
    assert!(err.to_string().contains("frame.data"), "must name it: {err}");
}

#[test]
fn a_locked_library_missing_from_disk_stops_the_build() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write_lockfile(
        "[[package]]\nname = \"frame.data\"\nversion = \"2.1.2\"\nkind = \"library\"\n\
         path = \"vendor/frame.data\"\n",
    );

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG002");
    assert!(err.to_string().contains("vendor/frame.data"), "got {err}");
}

#[test]
fn the_host_pin_and_the_package_list_share_one_lockfile() {
    // hostwit writes `[host.<name>]` into the same file Manager writes
    // `[[package]]` into. Neither may destroy the other.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write_lockfile(&project.library("frame.data", "2.1.2", "interface data {}", &[]));

    build(&project.inputs(), &compiler()).expect("build must succeed");

    // The build pinned the host contract (BVER-03)...
    let lockfile = String::from_utf8(project.read(".cln/lock.toml")).unwrap();
    assert!(lockfile.contains("[host.clean-cli]"), "host pin lost: {lockfile}");

    // ...and the package entry Manager wrote is still there.
    let reread = assemble_request(&project.inputs()).unwrap();
    assert_eq!(reread.library_manifests.len(), 1, "package entry lost: {lockfile}");
}

#[test]
fn adding_a_dependency_changes_the_request_hash() {
    // The dependency closure is part of build identity: a build cache keyed on
    // the request must not serve a component built without this library.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();

    let baseline = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    project.write_lockfile(&project.library("frame.data", "2.1.2", "interface data {}", &[]));
    let with_dependency = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    assert_ne!(baseline, with_dependency);
}

// ---------------------------------------------------------------------------
// Phase 4 — plugin loading.
// ---------------------------------------------------------------------------

/// The smallest valid core module: the 8-byte header alone.
const EMPTY_WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

impl Project {
    /// Add a plugin on disk plus the `[[package]]` entry that locks it.
    fn plugin(&self, name: &str, owns: &[&str], patterns: &[&str]) -> String {
        let list = |items: &[&str]| -> String {
            items.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(", ")
        };

        self.write(
            &format!("vendor/{name}/plugin.toml"),
            &format!(
                "[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n\
                 [paths]\nowns = [{}]\npatterns = [{}]\n",
                list(owns),
                list(patterns)
            ),
        );
        self.write_bytes(&format!("vendor/{name}/plugin.wasm"), EMPTY_WASM);

        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\nkind = \"plugin\"\n\
             path = \"vendor/{name}\"\n\n"
        )
    }
}

#[test]
fn a_plugin_owned_folder_is_compiled_into_the_build() {
    // The end-to-end point of Phase 4: a plugin owning `ui/` means the files
    // in `ui/` reach the compiler.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write("ui/button.cln", "button:\n\tprint(\"ui\")\n");
    project.write_lockfile(&project.plugin("frame.ui", &["ui"], &[]));

    let request = assemble_request(&project.inputs()).unwrap();
    let paths: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();

    assert_eq!(paths, ["app/main.cln", "ui/button.cln"]);
    build(&project.inputs(), &compiler()).expect("a build with a plugin must succeed");
}

#[test]
fn a_plugin_declared_pattern_brings_in_files_the_compiler_would_skip() {
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write("app/button.ui.cln", "button:\n\tprint(\"ui\")\n");
    project.write_lockfile(&project.plugin("frame.ui", &[], &["ui.cln"]));

    let request = assemble_request(&project.inputs()).unwrap();
    let paths: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();
    assert!(paths.contains(&"app/button.ui.cln"), "got {paths:?}");
}

#[test]
fn a_plugin_whose_wasm_lacks_a_declared_export_stops_the_build() {
    // FRM-PM-03. Left unchecked this surfaces at instantiation, naming a
    // symbol the developer never wrote in a plugin they did not build.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();

    let locked = project.plugin("frame.ui", &[], &[]);
    // Redeclare with an export the empty module does not provide.
    project.write(
        "vendor/frame.ui/plugin.toml",
        "[plugin]\nname = \"frame.ui\"\nversion = \"1.0.0\"\n\
         [exports]\nrender = { params = [] }\n",
    );
    project.write_lockfile(&locked);

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "FRM007");
    assert!(err.to_string().contains("render"), "must name the export: {err}");
}

#[test]
fn a_plugin_missing_its_wasm_stops_the_build() {
    // FRM-PM-01: the manifest alone is half a plugin.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    let locked = project.plugin("frame.ui", &[], &[]);
    std::fs::remove_file(project.path().join("vendor/frame.ui/plugin.wasm")).unwrap();
    project.write_lockfile(&locked);

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "FRM007");
    assert!(err.to_string().contains("plugin.wasm"), "got {err}");
}

#[test]
fn a_plugin_owning_a_path_outside_the_project_stops_the_build() {
    // A dependency that can own `../..` pulls arbitrary files off the
    // developer's disk into the compilation.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    let locked = project.plugin("frame.ui", &[], &[]);
    project.write(
        "vendor/frame.ui/plugin.toml",
        "[plugin]\nname = \"frame.ui\"\nversion = \"1.0.0\"\n\
         [paths]\nowns = [\"../../elsewhere\"]\n",
    );
    project.write_lockfile(&locked);

    let err = build(&project.inputs(), &compiler()).unwrap_err();
    assert_eq!(err.code(), "CFG001");
    assert!(err.to_string().contains("outside the project"), "got {err}");
}

#[test]
fn changing_a_plugins_wasm_changes_the_build() {
    // Build identity: a rebuilt plugin must not be served a cached component
    // built against the old one. The plugin's bytes are part of the closure,
    // so they must reach the request document somehow.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write_lockfile(&project.plugin("frame.ui", &["ui"], &[]));
    project.write("ui/button.cln", "button:\n\tprint(\"one\")\n");

    let before = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    // A different plugin.wasm: same manifest, same sources, new bytes. Still
    // a valid module — an invalid one would fail validation and prove nothing
    // about the hash reaching the request.
    let mut rebuilt = EMPTY_WASM.to_vec();
    // A custom section named "x", which any WASM consumer must tolerate.
    rebuilt.extend_from_slice(&[0x00, 0x03, 0x01, b'x', 0x00]);
    project.write_bytes("vendor/frame.ui/plugin.wasm", &rebuilt);
    let after = assemble_request(&project.inputs()).unwrap().sha256().unwrap();

    assert_ne!(before, after, "a rebuilt plugin must change the request");
}

// ---------------------------------------------------------------------------
// Phase 5 — the build cache (§11.7).
// ---------------------------------------------------------------------------

impl Project {
    /// Build inputs with caching on, pointed at a temp cache.
    fn inputs_cached(&self, cache: &framework_core::BuildCache) -> BuildInputs {
        BuildInputs::new(self.path())
            .with_host_wit_cache(host_wit_cache())
            .with_build_cache(cache.clone())
    }
}

#[test]
fn a_rebuild_produces_the_same_bytes_from_the_cache() {
    // CMP-06 through the cache: the second build never reaches the compiler,
    // and dist/ is byte-identical to the first.
    let _env = FakeCompilerEnv::none();
    let home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(home.path().join("build-cache"));

    let project = Project::hello();
    let first = build(&project.inputs_cached(&cache), &compiler()).unwrap();
    let first_wasm = project.read("dist/app.wasm");
    let first_manifest = project.read("dist/build-manifest.json");

    assert_eq!(cache.keys().unwrap().len(), 1, "the build must have been stored");

    // Remove dist/ entirely: the second build has to reproduce it from cache.
    std::fs::remove_dir_all(project.path().join("dist")).unwrap();

    let second = build(&project.inputs_cached(&cache), &compiler()).unwrap();

    assert_eq!(first.request_sha256, second.request_sha256);
    assert_eq!(first.wasm_sha256, second.wasm_sha256);
    assert_eq!(project.read("dist/app.wasm"), first_wasm);
    assert_eq!(
        project.read("dist/build-manifest.json"),
        first_manifest,
        "the build manifest must survive a cache round-trip verbatim"
    );
}

#[test]
fn editing_a_source_file_misses_the_cache() {
    let _env = FakeCompilerEnv::none();
    let home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(home.path().join("build-cache"));

    let project = Project::hello();
    build(&project.inputs_cached(&cache), &compiler()).unwrap();

    project.write("app/main.cln", "start:\n\tprint(\"changed\")\n");
    build(&project.inputs_cached(&cache), &compiler()).unwrap();

    assert_eq!(cache.keys().unwrap().len(), 2, "an edit is a different build");
}

#[test]
fn no_cache_stores_nothing_and_always_compiles() {
    let _env = FakeCompilerEnv::none();
    let home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(home.path().join("build-cache"));

    let project = Project::hello();
    let inputs = BuildInputs::new(project.path())
        .with_host_wit_cache(host_wit_cache())
        .with_build_cache(cache.clone())
        .without_cache();

    build(&inputs, &compiler()).unwrap();
    assert!(cache.keys().unwrap().is_empty(), "--no-cache must store nothing");
}

#[test]
fn a_cached_build_still_writes_every_output_a_fresh_build_does() {
    // A hit must not shortcut steps 9 and 10. dist/host.toml in particular is
    // generated by the framework, not carried in the compiler's tarball.
    let _env = FakeCompilerEnv::none();
    let home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(home.path().join("build-cache"));

    let project = Project::hello();
    build(&project.inputs_cached(&cache), &compiler()).unwrap();
    std::fs::remove_dir_all(project.path().join("dist")).unwrap();
    build(&project.inputs_cached(&cache), &compiler()).unwrap();

    assert!(project.exists("dist/app.wasm"));
    assert!(project.exists("dist/build-manifest.json"));
    assert!(project.exists("dist/host.toml"));
}

#[test]
fn a_cache_hit_survives_the_compiler_binary_disappearing() {
    // The strongest statement of "a hit skips the compile entirely": the
    // second build succeeds against a compiler path that no longer exists.
    let _env = FakeCompilerEnv::none();
    let home = tempfile::tempdir().unwrap();
    let cache = framework_core::BuildCache::at(home.path().join("build-cache"));

    // A copy of the fake compiler we can delete without affecting other tests.
    let copy = home.path().join("clean-compiler");
    std::fs::copy(fake_compiler(), &copy).unwrap();
    let copied = SubprocessCompiler::at(copy.clone(), semver::Version::new(0, 0, 0));

    let project = Project::hello();
    build(&project.inputs_cached(&cache), &copied).unwrap();

    std::fs::remove_file(&copy).unwrap();

    // `version()` is consulted for the cache key, so a vanished binary means
    // the key cannot be built and the build falls through to a compile that
    // must fail. Proving the hit therefore needs the version still readable —
    // which is exactly why the key is (request, version) and not the path.
    let err = build(&project.inputs_cached(&cache), &copied).unwrap_err();
    assert_eq!(err.code(), "FRM001", "a vanished compiler cannot be keyed against");
}

// ---------------------------------------------------------------------------
// `[folders]` as a discovery root — FRM-BO-03 item 1.
// ---------------------------------------------------------------------------

#[test]
fn a_folders_key_makes_its_subtree_compile() {
    // The gap this closes: `[folders]` travelled to the compiler verbatim but
    // never made the framework *look* there, so a project whose sources live
    // outside `app/` built as if they did not exist.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        r#"
[project]
name = "hello-world"
version = "0.1.0"

[build]
target = "wasm32-cli"

[target]
host = "clean-cli"
version = "0.1.0"

[folders]
"services/**" = ["data"]
"#,
    );
    project.write("services/billing/charge.cln", "charge:\n\tprint(\"x\")\n");

    let request = assemble_request(&project.inputs()).unwrap();
    let paths: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();

    assert_eq!(paths, ["app/main.cln", "services/billing/charge.cln"]);

    // And the mapping still reaches the compiler unchanged.
    assert_eq!(request.folders.get("services/**").unwrap(), &vec!["data".to_string()]);
}

#[test]
fn a_folders_key_naming_a_missing_directory_is_not_an_error() {
    // FRM-BO-03: absent roots are skipped silently. Declaring scope for a
    // folder you have not created yet is ordinary.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\
         [build]\ntarget = \"wasm32-cli\"\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n\
         [folders]\n\"not-yet/**\" = [\"data\"]\n",
    );

    let request = assemble_request(&project.inputs()).unwrap();
    assert_eq!(request.sources.len(), 1, "app/main.cln only");
}

#[test]
fn a_folders_key_overlapping_app_reads_each_file_once() {
    // FRM-BO-03: overlapping roots are read once, first declaration wins.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\
         [build]\ntarget = \"wasm32-cli\"\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n\
         [folders]\n\"app/**\" = [\"data\"]\n",
    );

    let request = assemble_request(&project.inputs()).unwrap();
    assert_eq!(request.sources.len(), 1, "app/main.cln must not be read twice");
}

#[test]
fn build_exclude_accepts_globs() {
    // `[build].exclude` was prefix-only; a developer writing `**/*.test.cln`
    // got silence. It shares the matcher `[folders]` needed.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\
         [build]\ntarget = \"wasm32-cli\"\nexclude = [\"**/*.test.cln\"]\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n",
    );
    project.write("app/model.cln", "model:\n\tprint(\"m\")\n");
    project.write("app/model.test.cln", "test:\n\tprint(\"t\")\n");
    project.write("app/deep/other.test.cln", "test:\n\tprint(\"t\")\n");

    let request = assemble_request(&project.inputs()).unwrap();
    let paths: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();

    assert_eq!(paths, ["app/main.cln", "app/model.cln"]);
}

#[test]
fn excluding_a_folder_excludes_what_is_inside_it() {
    // A developer excluding `app/scratch` means the folder. Requiring
    // `app/scratch/**` to be taken seriously is a trap.
    let _env = FakeCompilerEnv::none();
    let project = Project::hello();
    project.write(
        "clean.toml",
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\
         [build]\ntarget = \"wasm32-cli\"\nexclude = [\"app/scratch\"]\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n",
    );
    project.write("app/scratch/wip.cln", "wip:\n\tprint(\"w\")\n");
    project.write("app/scratch/deep/also.cln", "also:\n\tprint(\"a\")\n");

    let request = assemble_request(&project.inputs()).unwrap();
    let paths: Vec<&str> = request.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["app/main.cln"]);
}

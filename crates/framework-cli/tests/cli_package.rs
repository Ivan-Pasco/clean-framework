//! `clean-framework package` end to end through the binary.
//!
//! This is the producer half of the deploy contract proved against a real
//! archive: run the binary, unzip what it wrote, and assert a consumer finds
//! what it needs. The build suite covers argv and the envelope; this one
//! covers the artifact.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

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

fn server_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("clean.toml"),
        "[project]\nname = \"invoice-app\"\nversion = \"1.2.0\"\n\n\
         [build]\ntarget = \"wasm32-server\"\n\n\
         [target]\nhost = \"clean-server\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(dir.path().join("app/main.cln"), "start:\n\tprint(\"hello\")\n").unwrap();
    dir
}

fn run_package(project: &Path) -> Output {
    Command::new(framework_binary())
        .arg("package")
        .arg(project)
        .arg("--compiler")
        .arg(fake_compiler())
        .arg("--host-wit-cache")
        .arg(host_wit_cache())
        // Pin the build timestamp, so these tests observe the reproducibility
        // the format promises rather than the wall clock. A build that does
        // not set this still records when it ran.
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

#[test]
fn packaging_an_unbuilt_project_builds_it_first_and_writes_an_archive() {
    let project = server_project();
    let output = run_package(project.path());

    assert!(
        output.status.success(),
        "package failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let envelope = envelope(&output);
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["kind"], "serve");
    assert_eq!(
        envelope["rebuilt"], true,
        "a project with no dist/ must be built before it can be packaged"
    );
    assert_eq!(envelope["package_sha256"].as_str().unwrap().len(), 64);

    let archive = PathBuf::from(envelope["package"].as_str().unwrap());
    assert!(archive.exists());
    assert_eq!(
        archive.file_name().unwrap(),
        "invoice-app.clapp",
        "one extension for both kinds; manifest.toml carries the distinction"
    );
}

#[test]
fn the_archive_describes_itself_to_a_consumer_that_only_has_the_bytes() {
    let project = server_project();
    let output = run_package(project.path());
    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());

    let raw = read_from_archive(&archive, "manifest.toml").expect("manifest.toml at the root");
    let manifest: toml::Value = toml::from_str(&String::from_utf8(raw).unwrap()).unwrap();

    // Every fact here is one a bare .wasm cannot carry, and each is something
    // the deploy contract checks at upload.
    assert_eq!(manifest["package"]["name"].as_str(), Some("invoice-app"));
    assert_eq!(manifest["package"]["version"].as_str(), Some("1.2.0"));
    assert_eq!(manifest["artifact"]["kind"].as_str(), Some("serve"));
    assert!(manifest["build"]["compiler_version"].is_str());
    assert!(manifest["build"]["runtime_version"].is_str());
    assert!(manifest["spec_version"].is_str());

    // `SOURCE_DATE_EPOCH` is honoured, which is what makes the byte-identical
    // guarantee below reachable for a reproducible build.
    assert_eq!(
        manifest["build"]["built_at"].as_str(),
        Some("2026-08-14T12:00:00Z")
    );

    // A server bundle maps worlds to components rather than naming one entry,
    // which is what lets an app ship a worker beside its server.
    let entries = manifest["artifact"]["entries"].as_table().unwrap();
    let server = entries["server"].as_str().unwrap();
    assert_eq!(server, "wasm/server.wasm");

    // The component is really in there, and its declared hash matches.
    let wasm = read_from_archive(&archive, server).expect("the component the manifest names");
    assert_eq!(&wasm[..4], b"\0asm");

    let declared = manifest["integrity"]["wasm_sha256"][server].as_str().unwrap();
    assert_eq!(
        declared,
        framework_package::sha256_hex(&wasm),
        "a consumer must be able to verify without recomputing from another source"
    );
}

#[test]
fn packaging_twice_without_changing_anything_skips_the_rebuild() {
    let project = server_project();

    let first = run_package(project.path());
    assert_eq!(envelope(&first)["rebuilt"], true);

    let second = run_package(project.path());
    assert!(second.status.success());
    assert_eq!(
        envelope(&second)["rebuilt"],
        false,
        "dist/ is current, so packaging must not recompile"
    );

    // And the artifact is identical, which is what lets a content-addressed
    // store deduplicate it.
    assert_eq!(
        envelope(&first)["package_sha256"],
        envelope(&second)["package_sha256"]
    );
}

#[test]
fn editing_a_source_file_makes_the_next_package_rebuild() {
    let project = server_project();
    run_package(project.path());

    std::fs::write(
        project.path().join("app/main.cln"),
        "start:\n\tprint(\"changed\")\n",
    )
    .unwrap();

    let output = run_package(project.path());
    assert_eq!(
        envelope(&output)["rebuilt"],
        true,
        "the request document changed, so dist/ is stale"
    );
}

#[test]
fn a_cli_project_packages_as_an_application_not_a_server_bundle() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("clean.toml"),
        "[project]\nname = \"hello-world\"\nversion = \"0.1.0\"\n\n\
         [build]\ntarget = \"wasm32-cli\"\n\n\
         [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(project.path().join("app")).unwrap();
    std::fs::write(project.path().join("app/main.cln"), "start:\n\tprint(\"hi\")\n").unwrap();

    let output = run_package(project.path());
    assert!(output.status.success());
    assert_eq!(envelope(&output)["kind"], "clapp");

    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());
    // An application keeps its single component at the root; only a server
    // bundle needs the wasm/ directory.
    assert!(read_from_archive(&archive, "app.wasm").is_some());
}

#[test]
fn static_assets_ride_along_in_the_archive() {
    let project = server_project();
    std::fs::create_dir_all(project.path().join("public/css")).unwrap();
    std::fs::write(project.path().join("public/css/site.css"), b"body{}").unwrap();

    let output = run_package(project.path());
    let archive = PathBuf::from(envelope(&output)["package"].as_str().unwrap());

    // Cloud serves these over HTTP, so they must travel with the component
    // rather than being uploaded separately.
    assert_eq!(
        read_from_archive(&archive, "assets/public/css/site.css").as_deref(),
        Some(&b"body{}"[..])
    );
}

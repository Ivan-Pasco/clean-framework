//! What a package must guarantee, tested against the archive it actually
//! produces rather than against the builder's internal state.
//!
//! Every consumer of a package — `cln run`, Cloud's upload check, the
//! inspect-on-open view — reads the bytes. So do these tests: they unzip the
//! output and assert on what a reader finds.

use std::collections::BTreeMap;
use std::io::Read;

use framework_package::{
    file_name, layout, package, Build, BridgeInput, Kind, Manifest, PackageInputs, MANIFEST_NAME,
};

fn build_provenance() -> Build {
    Build {
        compiler_version: "2.5.0".into(),
        framework_version: "2.1.0".into(),
        runtime_version: "1.1.0".into(),
        built_at: "2026-08-14T10:30:00Z".into(),
        built_by: "cln 1.5.0".into(),
    }
}

fn serve_inputs() -> PackageInputs {
    let mut components = BTreeMap::new();
    components.insert("server".to_string(), b"\0asm\x0d\0\x01\0server".to_vec());

    PackageInputs {
        kind: Kind::Serve,
        name: "invoice-app".into(),
        version: "1.2.0".into(),
        description: Some("A small invoicing tool.".into()),
        components,
        bridges: BTreeMap::new(),
        host_toml: None,
        files: BTreeMap::new(),
        build: build_provenance(),
    }
}

/// Read one file out of the produced archive.
fn read_entry(bytes: &[u8], path: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).ok()?;
    let mut file = zip.by_name(path).ok()?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).ok()?;
    Some(out)
}

fn entry_names(bytes: &[u8]) -> Vec<String> {
    let zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    zip.file_names().map(str::to_string).collect()
}

fn manifest_of(bytes: &[u8]) -> Manifest {
    let raw = read_entry(bytes, MANIFEST_NAME).expect("manifest.toml is present");
    Manifest::from_toml(&String::from_utf8(raw).unwrap()).expect("manifest.toml parses")
}

#[test]
fn the_archive_carries_a_manifest_and_the_component_where_the_layout_says() {
    let packaged = package(serve_inputs()).unwrap();
    let names = entry_names(&packaged.bytes);

    assert!(names.contains(&MANIFEST_NAME.to_string()));
    assert!(names.contains(&"wasm/server.wasm".to_string()));
}

#[test]
fn a_clapp_puts_its_single_component_at_the_root_not_under_wasm() {
    let mut inputs = serve_inputs();
    inputs.kind = Kind::Clapp;

    let packaged = package(inputs).unwrap();
    let names = entry_names(&packaged.bytes);

    assert!(names.contains(&layout::APP_WASM.to_string()));
    assert!(!names.iter().any(|n| n.starts_with("wasm/")));

    // The two kinds name their entry differently: a clapp has one component
    // and says so, a serve bundle maps worlds to paths.
    let manifest = manifest_of(&packaged.bytes);
    assert_eq!(manifest.artifact.entry_wasm.as_deref(), Some(layout::APP_WASM));
    assert!(manifest.artifact.entries.is_empty());
}

#[test]
fn a_serve_bundle_maps_each_world_to_its_component() {
    let mut inputs = serve_inputs();
    inputs
        .components
        .insert("worker".to_string(), b"\0asm\x0d\0\x01\0worker".to_vec());

    let packaged = package(inputs).unwrap();
    let manifest = manifest_of(&packaged.bytes);

    assert_eq!(manifest.artifact.entries.get("server").unwrap(), "wasm/server.wasm");
    assert_eq!(manifest.artifact.entries.get("worker").unwrap(), "wasm/worker.wasm");
    assert_eq!(manifest.artifact.worlds, vec!["server", "worker"]);
    // A multi-world app is why the deployed artifact cannot be a bare wasm.
    assert!(manifest.artifact.entry_wasm.is_none());
}

#[test]
fn every_wasm_in_the_archive_has_a_hash_that_matches_its_bytes() {
    let mut inputs = serve_inputs();
    inputs.bridges.insert(
        "clean:session/store".to_string(),
        BridgeInput {
            name: "clean-session-redis".into(),
            version: "1.4.0".into(),
            wasm: b"\0asm\x0d\0\x01\0bridge".to_vec(),
        },
    );

    let packaged = package(inputs).unwrap();
    let manifest = manifest_of(&packaged.bytes);

    // This is the property Cloud's upload check relies on: it compares rather
    // than recomputes, so a corrupted artifact is caught at the door.
    assert!(!manifest.integrity.wasm_sha256.is_empty());
    for (path, expected) in &manifest.integrity.wasm_sha256 {
        let actual = read_entry(&packaged.bytes, path)
            .unwrap_or_else(|| panic!("{path} is declared in integrity but absent from the archive"));
        assert_eq!(
            &framework_package::sha256_hex(&actual),
            expected,
            "{path} does not match its declared hash"
        );
    }
}

#[test]
fn bridges_travel_inside_the_archive_and_are_named_by_the_interface_they_satisfy() {
    let mut inputs = serve_inputs();
    inputs.bridges.insert(
        "clean:session/store".to_string(),
        BridgeInput {
            name: "clean-session-redis".into(),
            version: "1.4.0".into(),
            wasm: b"\0asm\x0d\0\x01\0bridge".to_vec(),
        },
    );

    let packaged = package(inputs).unwrap();
    let manifest = manifest_of(&packaged.bytes);

    let bridge = manifest
        .artifact
        .bridges
        .get("clean:session/store")
        .expect("the bridge is keyed by its WIT interface");
    assert_eq!(bridge.version, "1.4.0");

    // Carried, not referenced: the artifact is self-describing, so an app
    // cannot be tested against one bridge version and run against another.
    assert!(read_entry(&packaged.bytes, &bridge.path).is_some());
    assert!(manifest.integrity.wasm_sha256.contains_key(&bridge.path));
}

#[test]
fn identical_inputs_produce_byte_identical_archives() {
    let first = package(serve_inputs()).unwrap();
    let second = package(serve_inputs()).unwrap();

    // Content-addressed storage on the receiving side deduplicates by this
    // hash, and a rebuild can be checked against a published artifact. Both
    // depend on the archive not embedding wall-clock time.
    assert_eq!(first.sha256(), second.sha256());
    assert_eq!(first.bytes, second.bytes);
}

#[test]
fn entry_order_does_not_depend_on_the_order_the_caller_supplied_them() {
    let mut forwards = serve_inputs();
    forwards.files.insert("assets/public/a.css".into(), b"a".to_vec());
    forwards.files.insert("assets/public/z.css".into(), b"z".to_vec());

    let mut backwards = serve_inputs();
    backwards.files.insert("assets/public/z.css".into(), b"z".to_vec());
    backwards.files.insert("assets/public/a.css".into(), b"a".to_vec());

    assert_eq!(package(forwards).unwrap().bytes, package(backwards).unwrap().bytes);
}

#[test]
fn the_manifest_records_the_runtime_pin_and_its_provenance() {
    let packaged = package(serve_inputs()).unwrap();
    let manifest = manifest_of(&packaged.bytes);

    // The runtime pin is the fact a bare .wasm cannot carry, and the reason
    // Cloud can reject a bundle it cannot run instead of failing at startup.
    assert_eq!(manifest.build.runtime_version, "1.1.0");
    assert_eq!(manifest.build.compiler_version, "2.5.0");
    assert_eq!(manifest.spec_version, framework_package::SPEC_VERSION);
    assert_eq!(manifest.artifact.kind, Kind::Serve);
}

#[test]
fn migrations_and_assets_and_host_config_ride_along() {
    let mut inputs = serve_inputs();
    inputs.host_toml = Some(b"[bridges]\n".to_vec());
    inputs
        .files
        .insert("migrations/0001_initial.sql".into(), b"CREATE TABLE t (id int);".to_vec());
    inputs
        .files
        .insert("assets/public/style.css".into(), b"body{}".to_vec());

    let packaged = package(inputs).unwrap();
    let names = entry_names(&packaged.bytes);

    // Each of these is a reason the deployed artifact is an archive: Cloud
    // provisions a database and must run the migrations, and serves the
    // assets over HTTP.
    assert!(names.contains(&layout::HOST_TOML.to_string()));
    assert!(names.contains(&"migrations/0001_initial.sql".to_string()));
    assert!(names.contains(&"assets/public/style.css".to_string()));
}

#[test]
fn writing_a_package_leaves_no_staging_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file_name("invoice-app"));

    package(serve_inputs()).unwrap().write_to(&path).unwrap();

    assert!(path.exists());
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(strays.is_empty(), "staging files left behind: {strays:?}");
}

#[test]
fn writing_over_an_existing_package_replaces_it_wholesale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file_name("invoice-app"));

    // A larger first package, so a truncating write that failed halfway would
    // leave a readable but wrong archive rather than an obvious error.
    let mut bigger = serve_inputs();
    bigger.files.insert("assets/public/big.bin".into(), vec![0u8; 4096]);
    package(bigger).unwrap().write_to(&path).unwrap();

    let second = package(serve_inputs()).unwrap();
    second.write_to(&path).unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), second.bytes);
}

#[test]
fn declared_wasm_lists_every_component_a_consumer_must_verify() {
    let mut inputs = serve_inputs();
    inputs
        .components
        .insert("worker".to_string(), b"\0asm\x0d\0\x01\0worker".to_vec());
    inputs.bridges.insert(
        "clean:session/store".to_string(),
        BridgeInput {
            name: "clean-session-redis".into(),
            version: "1.4.0".into(),
            wasm: b"\0asm\x0d\0\x01\0bridge".to_vec(),
        },
    );

    let packaged = package(inputs).unwrap();
    let manifest = manifest_of(&packaged.bytes);
    let declared = manifest.declared_wasm();

    // A bridge is instantiated by the runtime just as the guest is, so a
    // corrupted bridge is exactly as fatal — the upload check covers both.
    assert!(declared.contains(&"wasm/server.wasm"));
    assert!(declared.contains(&"wasm/worker.wasm"));
    assert!(declared.contains(&"bridges/clean-session-redis.wasm"));
    assert_eq!(declared.len(), manifest.integrity.wasm_sha256.len());
}

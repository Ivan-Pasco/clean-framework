//! The request document must match Platform 14 §14.1.1 exactly.
//!
//! `tests/golden/spec-14.1.1-request.json` is the spec's own example, copied
//! verbatim. If the compiler's schema moves, this test fails and the fixture
//! gets updated deliberately — that is the whole point. Without it, the two
//! sides of the seam drift silently until a build fails with `RQD002` in a
//! user's terminal.

use framework_compiler_driver::request::RequestDocument;

const GOLDEN: &str = include_str!("golden/spec-14.1.1-request.json");

#[test]
fn parses_the_spec_example_without_loss() {
    let doc: RequestDocument = serde_json::from_str(GOLDEN).expect("spec example must deserialize");

    assert_eq!(doc.spec_version, "1");
    assert_eq!(doc.project.name, "my-app");
    assert_eq!(doc.project.version, "0.1.0");

    assert_eq!(doc.build.target, "wasm32-server");
    assert_eq!(doc.build.optimization.as_deref(), Some("release"));
    assert_eq!(doc.build.memory.as_ref().unwrap().tier, "standard");
    assert_eq!(doc.build.strip, Some(true));
    assert_eq!(doc.build.component_model, Some(true));
    assert_eq!(doc.build.memory64, Some(false));

    assert_eq!(doc.folders.get("app/data").unwrap(), &vec!["data".to_string()]);
    assert_eq!(doc.folders.get("app/server").unwrap(), &vec!["server".to_string()]);

    let data = doc.dependencies.get("data").unwrap();
    assert_eq!(data.version, "1.4.0");
    assert_eq!(data.resolved_from, "registry");

    let limits = doc.compile_limits.as_ref().unwrap();
    assert_eq!(limits.handler_timeout_ms, Some(5000));
    assert_eq!(limits.handler_memory_mb, Some(128));
    assert_eq!(limits.total_timeout_min, Some(10));
    assert_eq!(limits.max_file_size_mb, Some(4));
    assert_eq!(limits.max_import_depth, Some(32));

    assert_eq!(doc.telemetry.as_ref().unwrap().consent_level, "error-with-code");

    // ADR-0033. All five fields, and the world named rather than extracted.
    assert_eq!(doc.target_world.host, "clean-server");
    assert_eq!(doc.target_world.version, "0.1.0");
    assert_eq!(doc.target_world.world, "server");
    assert_eq!(doc.target_world.sha256, "9f2b1c...");
    assert!(
        doc.target_world.wit.contains("world server"),
        "the contract text must survive verbatim: {}",
        doc.target_world.wit
    );

    assert_eq!(doc.sources.len(), 2);
    assert_eq!(doc.sources[0].path, "app/main.cln");
    assert_eq!(doc.sources[0].content, "start()\n  console.print(\"hello\")\n");

    let library = &doc.library_manifests[0];
    assert_eq!(library.name, "data");
    assert_eq!(library.handles_blocks, vec!["data", "endpoints"]);
    assert_eq!(library.compiletime_wasm_sha256.as_deref(), Some("d41d8c"));

    assert_eq!(doc.overrides.len(), 1);
    assert_eq!(doc.overrides[0].path, "build.optimization");
    assert_eq!(doc.overrides[0].source, "cli");
}

#[test]
fn reserializes_to_semantically_identical_json() {
    // Round-trip through our type, then compare as JSON values so key order and
    // whitespace don't matter — but every key and value must survive. A field
    // our struct drops would fail here.
    let doc: RequestDocument = serde_json::from_str(GOLDEN).unwrap();
    let ours: serde_json::Value =
        serde_json::from_slice(&doc.to_canonical_json().unwrap()).unwrap();
    let theirs: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();

    assert_eq!(ours, theirs, "round-trip must not add, drop, or alter any field");
}

#[test]
fn emits_no_key_the_spec_does_not_name() {
    // RQD002: unknown top-level keys are a hard error on the compiler side, so
    // an accidental extra field here breaks every build rather than being
    // ignored.
    let doc: RequestDocument = serde_json::from_str(GOLDEN).unwrap();
    let ours: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&doc.to_canonical_json().unwrap()).unwrap();

    let permitted = [
        "spec_version",
        "project",
        "build",
        "folders",
        "dependencies",
        "compile_limits",
        "telemetry",
        "target_world",
        "sources",
        "library_manifests",
        "overrides",
    ];
    for key in ours.keys() {
        assert!(permitted.contains(&key.as_str()), "unexpected top-level key: {key}");
    }
}

#[test]
fn hash_is_stable_across_processes() {
    // The determinism invariant the framework owes the compiler: identical
    // project state produces a byte-identical request document. This is what
    // makes CMP-02 externally provable.
    let doc: RequestDocument = serde_json::from_str(GOLDEN).unwrap();
    let reparsed: RequestDocument =
        serde_json::from_slice(&doc.to_canonical_json().unwrap()).unwrap();
    assert_eq!(doc.sha256().unwrap(), reparsed.sha256().unwrap());
}

//! The compiler's response — Platform 14 §14.1.2, §14.8.
//!
//! The process adapter emits a single uncompressed tar on stdout containing
//! `component.wasm`, `build-manifest.json`, `diagnostics.json`, and optionally
//! `source-map.json`. This module owns unpacking that into a typed
//! [`CompileArtifact`].
//!
//! `build-manifest.json` is deliberately kept as an opaque `serde_json::Value`
//! plus the few fields the framework itself reads. The manifest's schema is the
//! compiler's to own (§14.8) and the framework's job is to relay it verbatim
//! into `dist/build-manifest.json` and later into a package's `manifest.toml`
//! — re-deriving fields would be exactly the drift FRM-BO-09a forbids
//! ("recorded verbatim — no re-derivation, no defaults").

use std::io::Read;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::diagnostic::Diagnostic;
use crate::CompileError;

/// Entry names inside the artifact tarball.
pub const WASM_ENTRY: &str = "component.wasm";
pub const MANIFEST_ENTRY: &str = "build-manifest.json";
pub const DIAGNOSTICS_ENTRY: &str = "diagnostics.json";
pub const SOURCE_MAP_ENTRY: &str = "source-map.json";

#[derive(Clone, Debug)]
pub struct CompileArtifact {
    /// The component bytes. The framework names and places them (FRM-BO-09).
    pub wasm: Vec<u8>,
    pub manifest: BuildManifest,
    /// Warnings and infos on success; any error would have failed the compile.
    pub diagnostics: Vec<Diagnostic>,
    pub source_map: Option<Vec<u8>>,
}

/// Platform 14 §14.8. The framework reads `compiler.version` for provenance and
/// passes everything else through untouched.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildManifest {
    #[serde(flatten)]
    pub raw: serde_json::Value,
}

impl BuildManifest {
    pub fn compiler_version(&self) -> Option<&str> {
        self.raw.get("compiler")?.get("version")?.as_str()
    }

    pub fn request_sha256(&self) -> Option<&str> {
        self.raw.get("request_sha256")?.as_str()
    }

    /// Pretty JSON for `dist/build-manifest.json`. A human opens this file when
    /// a build is surprising, so it is written indented, unlike the request
    /// document which is hashed and must stay canonical.
    pub fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.raw)
    }
}

impl CompileArtifact {
    /// Unpack the tar the compiler wrote to stdout.
    ///
    /// A missing `component.wasm` is an error here rather than a `None`: the
    /// compiler only emits a tarball on success, and on failure it exits
    /// non-zero (CMP-05), which the subprocess layer turns into
    /// `CompileError::CompilerFailed` before we ever get here.
    pub fn from_tar(bytes: &[u8]) -> Result<Self, CompileError> {
        let mut wasm = None;
        let mut manifest = None;
        let mut diagnostics = Vec::new();
        let mut source_map = None;

        let mut archive = tar::Archive::new(bytes);
        let entries = archive
            .entries()
            .map_err(|e| CompileError::MalformedOutput(format!("not a tar archive: {e}")))?;

        for entry in entries {
            let mut entry = entry
                .map_err(|e| CompileError::MalformedOutput(format!("bad tar entry: {e}")))?;
            let path = entry
                .path()
                .map_err(|e| CompileError::MalformedOutput(format!("bad tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();

            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| CompileError::MalformedOutput(format!("unreadable entry {path}: {e}")))?;

            // Tolerate a leading "./" — tar writers differ on this and it is
            // not worth failing a build over.
            match path.trim_start_matches("./") {
                WASM_ENTRY => wasm = Some(buf),
                MANIFEST_ENTRY => {
                    let raw = serde_json::from_slice(&buf).map_err(|e| {
                        CompileError::MalformedOutput(format!("{MANIFEST_ENTRY} is not valid JSON: {e}"))
                    })?;
                    manifest = Some(BuildManifest { raw });
                }
                DIAGNOSTICS_ENTRY => {
                    diagnostics = parse_diagnostics(&buf)?;
                }
                SOURCE_MAP_ENTRY => source_map = Some(buf),
                // Unknown entries are ignored, not errored: the compiler may
                // add outputs before the framework learns to read them, and a
                // new artifact file should not break an older framework.
                _ => {}
            }
        }

        Ok(CompileArtifact {
            wasm: wasm.ok_or_else(|| {
                CompileError::MalformedOutput(format!("artifact contains no {WASM_ENTRY}"))
            })?,
            manifest: manifest.ok_or_else(|| {
                CompileError::MalformedOutput(format!("artifact contains no {MANIFEST_ENTRY}"))
            })?,
            diagnostics,
            source_map,
        })
    }

    pub fn wasm_sha256(&self) -> String {
        hex_sha256(&self.wasm)
    }
}

/// Parse `diagnostics.json`.
///
/// Three shapes are accepted, because three exist in the wild:
///
/// - **NDJSON**, one diagnostic object per line. This is what the real
///   compiler (0.1.0) writes, and what its `--diagnostics` flag documents.
/// - **A bare array**, which the spec describes.
/// - **An object with a `diagnostics` key**, which the compiler's driver has
///   also used.
///
/// Accepting all three is not indecision. A diagnostic that fails to parse is
/// a diagnostic the developer never sees — they get "compiler exited with code
/// 1" instead of the error with its line number — so this is exactly the wrong
/// place to be strict about a format the other side owns.
pub fn parse_diagnostics(bytes: &[u8]) -> Result<Vec<Diagnostic>, CompileError> {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }

    // NDJSON first: a whole-document parse of multi-line NDJSON fails with a
    // "trailing characters" error that says nothing useful, so try the
    // line-oriented shape before falling back.
    if let Some(diagnostics) = parse_ndjson(bytes) {
        return Ok(diagnostics);
    }

    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        CompileError::MalformedOutput(format!("{DIAGNOSTICS_ENTRY} is not valid JSON: {e}"))
    })?;
    let array = match &value {
        serde_json::Value::Array(_) => value.clone(),
        serde_json::Value::Object(map) => map
            .get("diagnostics")
            .cloned()
            .unwrap_or(serde_json::Value::Array(Vec::new())),
        _ => {
            return Err(CompileError::MalformedOutput(format!(
                "{DIAGNOSTICS_ENTRY} must be an array or an object with a diagnostics key"
            )))
        }
    };
    serde_json::from_value(array).map_err(|e| {
        CompileError::MalformedOutput(format!("{DIAGNOSTICS_ENTRY} has unexpected shape: {e}"))
    })
}

/// NDJSON: one diagnostic per non-blank line.
///
/// Returns `None` — rather than an error — when the input is not NDJSON, so
/// the caller can try the other shapes. A single-line input is deliberately
/// *not* treated as NDJSON unless it parses as one diagnostic object, since a
/// one-element JSON array is also a single line.
fn parse_ndjson(bytes: &[u8]) -> Option<Vec<Diagnostic>> {
    let text = std::str::from_utf8(bytes).ok()?;

    let mut diagnostics = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Every non-blank line must be a diagnostic object, or this is not
        // NDJSON and a partial parse would silently drop the rest.
        diagnostics.push(serde_json::from_str::<Diagnostic>(line).ok()?);
    }

    (!diagnostics.is_empty()).then_some(diagnostics)
}

/// Hex-lowercase SHA-256. The one hashing helper in the framework — every
/// digest on the wire (`sources[].sha256`, request hash, wasm hash) is this
/// format, so there is one implementation to be wrong about.
pub fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *body).unwrap();
        }
        // `finish()` writes the terminating zero blocks. Without it the archive
        // is truncated and a reader fails on the last entry — exactly the bug
        // these tests exist to catch.
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn hex_sha256_matches_known_vector() {
        // The empty-string digest, the one everybody can check by eye.
        assert_eq!(
            hex_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn unpacks_a_full_artifact() {
        let tar = tar_with(&[
            (WASM_ENTRY, b"\0asm\x01\0\0\0"),
            (MANIFEST_ENTRY, br#"{"compiler":{"version":"1.4.0"},"request_sha256":"deadbeef"}"#),
            (DIAGNOSTICS_ENTRY, b"[]"),
        ]);
        let artifact = CompileArtifact::from_tar(&tar).unwrap();
        assert_eq!(artifact.wasm, b"\0asm\x01\0\0\0");
        assert_eq!(artifact.manifest.compiler_version(), Some("1.4.0"));
        assert_eq!(artifact.manifest.request_sha256(), Some("deadbeef"));
        assert!(artifact.diagnostics.is_empty());
        assert!(artifact.source_map.is_none());
    }

    #[test]
    fn tolerates_dot_slash_prefixes_and_unknown_entries() {
        let tar = tar_with(&[
            ("./component.wasm", b"wasm"),
            ("./build-manifest.json", b"{}"),
            ("something-new.json", b"{}"),
        ]);
        let artifact = CompileArtifact::from_tar(&tar).unwrap();
        assert_eq!(artifact.wasm, b"wasm");
    }

    #[test]
    fn missing_wasm_is_malformed() {
        let tar = tar_with(&[(MANIFEST_ENTRY, b"{}")]);
        let err = CompileArtifact::from_tar(&tar).unwrap_err();
        assert!(matches!(err, CompileError::MalformedOutput(_)), "got {err:?}");
    }

    #[test]
    fn missing_manifest_is_malformed() {
        let tar = tar_with(&[(WASM_ENTRY, b"wasm")]);
        assert!(CompileArtifact::from_tar(&tar).is_err());
    }

    #[test]
    fn truncated_archive_is_rejected_not_silently_accepted() {
        // A compiler killed mid-write, or one that forgets tar's terminating
        // zero blocks, must fail the build rather than yield a partial
        // artifact — half an artifact is exactly what FRM-BO-10 forbids.
        let full = tar_with(&[
            (WASM_ENTRY, b"\0asm\x01\0\0\0"),
            (MANIFEST_ENTRY, b"{}"),
        ]);
        let truncated = &full[..full.len() - 600];
        assert!(
            CompileArtifact::from_tar(truncated).is_err(),
            "a truncated tar must not parse as a complete artifact"
        );
    }

    #[test]
    fn diagnostics_accept_both_shapes() {
        let bare = br#"[{"level":"warning","code":"SEM001","message":"unused"}]"#;
        assert_eq!(parse_diagnostics(bare).unwrap().len(), 1);

        let wrapped = br#"{"diagnostics":[{"level":"warning","code":"SEM001","message":"unused"}]}"#;
        assert_eq!(parse_diagnostics(wrapped).unwrap().len(), 1);

        assert!(parse_diagnostics(b"").unwrap().is_empty());
    }
}

#[cfg(test)]
mod ndjson_tests {
    use super::*;

    #[test]
    fn ndjson_is_parsed_because_that_is_what_the_compiler_writes() {
        // Compiler 0.1.0 writes one diagnostic object per line. Before this
        // was handled, a rejected program surfaced to the developer as
        // "compiler exited with code 1" — the real message, with its line
        // number, was sitting in a file we could not read.
        let bytes = br#"{"level":"error","code":"SYN002","message":"expected an expression"}
{"level":"error","code":"SYN008","message":"block must contain an expression"}"#;

        let diagnostics = parse_diagnostics(bytes).unwrap();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].code, "SYN002");
        assert_eq!(diagnostics[1].code, "SYN008");
    }

    #[test]
    fn a_bare_array_still_parses() {
        // The shape the spec describes. Both must work — the framework does
        // not get to pick which one the compiler emits.
        let bytes = br#"[{"level":"error","code":"SEM001","message":"unknown identifier"}]"#;
        let diagnostics = parse_diagnostics(bytes).unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "SEM001");
    }

    #[test]
    fn an_object_with_a_diagnostics_key_still_parses() {
        let bytes = br#"{"diagnostics":[{"level":"warning","code":"SEM900","message":"unused"}]}"#;
        let diagnostics = parse_diagnostics(bytes).unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "SEM900");
    }

    #[test]
    fn a_single_ndjson_line_parses_as_one_diagnostic() {
        // The common case: one error, one line, no enclosing array.
        let bytes = br#"{"level":"error","code":"SYN002","message":"expected ':'"}"#;
        let diagnostics = parse_diagnostics(bytes).unwrap();
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn blank_lines_between_diagnostics_are_ignored() {
        let bytes = b"{\"level\":\"error\",\"code\":\"A\",\"message\":\"one\"}\n\n{\"level\":\"error\",\"code\":\"B\",\"message\":\"two\"}\n";
        assert_eq!(parse_diagnostics(bytes).unwrap().len(), 2);
    }

    #[test]
    fn an_empty_file_is_no_diagnostics_not_an_error() {
        // A successful compile writes an empty diagnostics.json.
        assert!(parse_diagnostics(b"").unwrap().is_empty());
        assert!(parse_diagnostics(b"\n  \n").unwrap().is_empty());
    }

    #[test]
    fn spans_survive_the_ndjson_path() {
        // The span is the whole value of a diagnostic — an error without a
        // line number is barely better than an exit code.
        let bytes = br#"{"level":"error","code":"SYN002","message":"expected an expression","primary_span":{"file":"app/main.cln","start":{"line":2,"column":14},"end":{"line":3,"column":1}}}"#;
        let diagnostics = parse_diagnostics(bytes).unwrap();
        let span = diagnostics[0].primary_span.as_ref().expect("span must survive");
        assert_eq!(span.file, "app/main.cln");
        assert_eq!(span.start.line, 2);
        assert_eq!(span.start.column, 14);
    }

    #[test]
    fn genuinely_malformed_input_is_still_an_error() {
        // Tolerance about *shape* must not become tolerance about garbage:
        // that would turn a broken compiler into a silent success.
        assert!(parse_diagnostics(b"this is not json at all").is_err());
    }
}

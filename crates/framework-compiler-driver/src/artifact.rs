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

/// `diagnostics.json` is a bare array. Accept an object with a `diagnostics`
/// key too — the compiler's own driver has used both shapes and neither is
/// ambiguous.
pub fn parse_diagnostics(bytes: &[u8]) -> Result<Vec<Diagnostic>, CompileError> {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
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

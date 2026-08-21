//! The request document — Platform 14 §14.1.1.
//!
//! This is the whole contract between framework and compiler. The compiler
//! reads nothing else (CMP-01); the framework puts everything in here
//! (FRM-BO-02). Two rules drive the serde attributes below:
//!
//! - **Unknown top-level keys are a hard error on the compiler side**
//!   (`RQD002`), so we must never emit a key the schema doesn't name. Every
//!   optional section is `skip_serializing_if` — "fields not present in
//!   clean.toml are omitted from the request document" (§11.4).
//! - **The document must be byte-identical for identical project state**, which
//!   is what makes the compiler's CMP-02 externally provable. That means
//!   ordered maps (`BTreeMap`, never `HashMap`) and a sorted `sources[]`
//!   (FRM-BO-06).
//!
//! M0 note: this type lives here rather than in `cln-shared` because promoting
//! it is a three-repo coordination event (manager, framework, compiler) and it
//! would block hello-world on lockstep releases. The golden test in
//! `tests/golden_request.rs` pins the shape against the spec fixture so the
//! promotion later is a move, not a redesign.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Pinned to "1" until the ADR process ships a new version (§11.5).
pub const SPEC_VERSION: &str = "1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestDocument {
    pub spec_version: String,
    pub project: Project,
    pub build: Build,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<String, Vec<String>>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, Dependency>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_limits: Option<CompileLimits>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<Telemetry>,

    /// The host contract this component is compiled against (ADR-0033).
    /// Required — see [`TargetWorld`].
    pub target_world: TargetWorld,

    /// Sorted by `path`, lexicographic byte-wise (FRM-BO-06).
    pub sources: Vec<Source>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_manifests: Vec<LibraryManifest>,

    /// The audit trail of every value that did not come from clean.toml
    /// (FRM-BO-08). Never merged into the lowered config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<Override>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub target: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Memory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_model: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory64: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub tier: String,
}

/// The target host contract, carried by value (ADR-0033, §14.1.1).
///
/// The compiler validates every `host function` call site against this and
/// fetches nothing itself (CMP-01). Three properties are load-bearing and easy
/// to break by "improving" this type:
///
/// - **`wit` is the fetched `host.wit` verbatim**, not an extract of the one
///   world in use. Slicing it here would make the request record what the
///   framework produced rather than what the host published, so `cln repro
///   build` could no longer show the contract as shipped and a bug in our
///   parser would be indistinguishable from a host contract change (Option F
///   in the ADR, rejected).
/// - **`world` names which world inside `wit` applies.** A `host.wit` is a WIT
///   package and may declare several. Without this the compiler would resolve
///   the target-to-world mapping from its own table, making the compiler binary
///   the authority on what a host provides — the coupling BVER-03 exists to
///   prevent.
/// - **`version` is resolved, never a constraint.** `[target].version` may be a
///   range like `0.1.x`; putting that here would let two byte-identical
///   requests denote different hosts, breaking CMP-02 through the cache key.
///
/// All fields are `String`, so serde emits them in declaration order with no
/// map involved — `to_canonical_json` stays byte-stable for free.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TargetWorld {
    /// `[target].host`, e.g. `"clean-server"`.
    pub host: String,
    /// The concrete version pinned in `.cln/lock.toml`.
    pub version: String,
    /// The world within `wit` to validate against, e.g. `"server"`.
    pub world: String,
    /// Hex-lowercase SHA-256 of `wit`, matching the lockfile (BVER-03).
    pub sha256: String,
    /// The host's `host.wit`, verbatim as published.
    pub wit: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Dependency {
    pub version: String,
    /// `"registry" | "path" | "git"` (§11.4). M0 emits none of these — there
    /// are no dependencies until Phase 2 — but the shape is pinned now so the
    /// golden test covers it.
    pub resolved_from: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompileLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_timeout_min: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_import_depth: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub consent_level: String,
}

/// One discovered source file (§11.5).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// POSIX, project-relative.
    pub path: String,
    /// Hex-lowercase SHA-256 of the *decoded* content, not the raw bytes.
    /// The compiler verifies this and refuses the request with `RQD001` on
    /// mismatch.
    pub sha256: String,
    pub content: String,
}

/// One entry of the resolved dependency closure. Empty in M0.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LibraryManifest {
    pub name: String,
    pub version: String,
    pub wit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handles_blocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiletime_wasm_sha256: Option<String>,

    /// The block-handler WASM itself, standard-alphabet base64 **with**
    /// padding (ADR-0004).
    ///
    /// Platform 14 §14.1.1 models only the hash, which cannot work: CMP-01
    /// leaves the compiler nowhere to *fetch* the artifact that hash names, so
    /// the bytes have to travel in the request like every other input. The
    /// compiler carries this as a provisional schema addition; this field is
    /// the framework's half of it.
    ///
    /// Three properties the compiler enforces, all verified against the real
    /// binary rather than inferred:
    ///
    /// - The alphabet is standard and the padding is required — unpadded
    ///   base64 is rejected as "not valid base64".
    /// - `compiletime_wasm_sha256` is the hash of the **decoded** bytes, not
    ///   of this string.
    /// - That hash must be lowercase hex.
    ///
    /// `None` for a library that registers no `handles_blocks`. A library that
    /// registers blocks and omits this cannot have them expanded, and the
    /// compiler says so with `LIB004`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiletime_wasm: Option<String>,
}

impl LibraryManifest {
    /// Attach a compile-time handler: the WASM inline, plus the hash of it.
    ///
    /// The two fields are set together because they are one fact stated twice,
    /// and the compiler rejects the pair when they disagree (`LIB004`).
    /// Letting a caller set either alone is how they drift.
    pub fn with_compiletime_wasm(mut self, wasm: &[u8]) -> Self {
        self.compiletime_wasm_sha256 = Some(sha256_hex(wasm));
        self.compiletime_wasm = Some(base64_standard(wasm));
        self
    }
}

/// Hex-lowercase SHA-256, the spelling every hash in the request uses.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Standard-alphabet base64, padded.
///
/// Hand-rolled rather than pulled in as a dependency: this is the only base64
/// in the workspace, the encoder is twenty lines, and the compiler seam is the
/// place where an extra dependency is least welcome. The padding is not
/// optional — the compiler rejects unpadded input outright.
fn base64_standard(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }

    out
}

/// FRM-BO-08. One entry per overridden value; the compiler applies it and
/// records it verbatim so `cln repro build` can replay the exact build.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Override {
    /// Dotted path into the config, e.g. `build.optimization`.
    pub path: String,
    pub value: String,
    /// `"cli" | "env"`.
    pub source: String,
}

impl RequestDocument {
    /// Canonical JSON bytes: the exact payload written to the compiler's
    /// stdin, and the exact bytes hashed for the build cache key (§11.7).
    ///
    /// `serde_json::to_vec` is deterministic for our types — struct fields
    /// serialize in declaration order and every map is a `BTreeMap` — so this
    /// upholds the framework's determinism invariant without a canonicalizer.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Hex-lowercase SHA-256 of the canonical JSON. The build-cache key in
    /// Phase 5, and the "did anything change?" check in `cln dev` (§6 step 4b).
    pub fn sha256(&self) -> Result<String, serde_json::Error> {
        Ok(crate::artifact::hex_sha256(&self.to_canonical_json()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> RequestDocument {
        RequestDocument {
            spec_version: SPEC_VERSION.to_string(),
            project: Project { name: "hello-world".into(), version: "0.1.0".into() },
            build: Build {
                target: "wasm32-cli".into(),
                optimization: None,
                memory: None,
                strip: None,
                component_model: None,
                memory64: None,
            },
            folders: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            compile_limits: None,
            telemetry: None,
            target_world: TargetWorld {
                host: "clean-cli".into(),
                version: "0.1.0".into(),
                world: "cli".into(),
                sha256: "9f2b1c".into(),
                wit: "package clean:host@0.1.0;\nworld cli {}\n".into(),
            },
            sources: vec![Source {
                path: "app/main.cln".into(),
                sha256: "abc".into(),
                content: "start()\n".into(),
            }],
            library_manifests: Vec::new(),
            overrides: Vec::new(),
        }
    }

    #[test]
    fn absent_sections_are_omitted_not_nulled() {
        // RQD002: the compiler hard-errors on unknown keys, and §11.4 says
        // absent fields are omitted so the compiler applies its own defaults.
        // A `"folders": {}` or `"telemetry": null` would be a contract break.
        let json = String::from_utf8(minimal().to_canonical_json().unwrap()).unwrap();
        for absent in ["folders", "dependencies", "compile_limits", "telemetry",
                       "library_manifests", "overrides", "optimization", "memory64"] {
            assert!(!json.contains(absent), "{absent} must be omitted, got: {json}");
        }
        assert!(!json.contains("null"), "no field may serialize as null: {json}");
    }

    #[test]
    fn canonical_json_is_stable_across_serializations() {
        let doc = minimal();
        assert_eq!(doc.to_canonical_json().unwrap(), doc.to_canonical_json().unwrap());
        assert_eq!(doc.sha256().unwrap(), doc.sha256().unwrap());
    }

    #[test]
    fn folders_serialize_in_sorted_order_regardless_of_insertion() {
        let mut a = minimal();
        a.folders.insert("app/server".into(), vec!["server".into()]);
        a.folders.insert("app/data".into(), vec!["data".into()]);

        let mut b = minimal();
        b.folders.insert("app/data".into(), vec!["data".into()]);
        b.folders.insert("app/server".into(), vec!["server".into()]);

        assert_eq!(a.sha256().unwrap(), b.sha256().unwrap(),
            "insertion order must not affect the request document");
    }

    #[test]
    fn target_world_is_required_not_optional() {
        // ADR-0033 rejected the optional variant explicitly: a safety check
        // that is off when the field is absent is off in exactly the conditions
        // where nobody notices. Deserializing a request without the field must
        // fail rather than default.
        let mut json: serde_json::Value =
            serde_json::from_slice(&minimal().to_canonical_json().unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("target_world");
        assert!(
            serde_json::from_value::<RequestDocument>(json).is_err(),
            "a request without target_world must not deserialize"
        );
    }

    #[test]
    fn target_world_is_always_serialized() {
        // No `skip_serializing_if` — the compiler refuses a request missing it
        // with RQD002, so an omitted-when-empty field would be a contract break
        // that only shows up on a project with an unusual host.
        let json = String::from_utf8(minimal().to_canonical_json().unwrap()).unwrap();
        assert!(json.contains("target_world"), "got {json}");
        assert!(json.contains("\"world\":\"cli\""), "got {json}");
    }

    #[test]
    fn same_world_text_gives_the_same_request_hash() {
        // The determinism check ADR-0033 asks for: a world inlined by value
        // must serialize deterministically, or the build-cache key is unsound.
        let a = minimal();
        let mut b = minimal();
        b.target_world.wit = a.target_world.wit.clone();
        assert_eq!(a.sha256().unwrap(), b.sha256().unwrap());

        // ...and a different world must change it, or the cache would serve a
        // component validated against the wrong contract.
        let mut c = minimal();
        c.target_world.wit.push_str("world extra {}\n");
        assert_ne!(a.sha256().unwrap(), c.sha256().unwrap());
    }

    #[test]
    fn the_world_selector_participates_in_the_hash() {
        // Same file, different world selected, is a different compilation.
        let a = minimal();
        let mut b = minimal();
        b.target_world.world = "server".into();
        assert_ne!(a.sha256().unwrap(), b.sha256().unwrap());
    }

    #[test]
    fn roundtrips_through_json() {
        let doc = minimal();
        let back: RequestDocument =
            serde_json::from_slice(&doc.to_canonical_json().unwrap()).unwrap();
        assert_eq!(doc, back);
    }
}

#[cfg(test)]
mod compiletime_wasm_tests {
    use super::*;

    fn entry() -> LibraryManifest {
        LibraryManifest {
            name: "frame.data".into(),
            version: "1.0.0".into(),
            wit: String::new(),
            handles_blocks: vec!["data".into()],
            compiletime_wasm_sha256: None,
            compiletime_wasm: None,
        }
    }

    /// The three properties the real compiler enforces, verified against
    /// `clean-compiler 0.1.0` by probing it directly: standard alphabet,
    /// padding required, hash over the *decoded* bytes in lowercase hex.
    #[test]
    fn pairs_the_bytes_with_the_hash_of_those_bytes() {
        // A minimal well-formed WASM module: magic plus version.
        let wasm = b"\0asm\x01\0\0\0";
        let entry = entry().with_compiletime_wasm(wasm);

        assert_eq!(entry.compiletime_wasm.as_deref(), Some("AGFzbQEAAAA="));
        assert_eq!(
            entry.compiletime_wasm_sha256.as_deref(),
            Some("93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476"),
            "the hash must be over the decoded bytes, not the base64"
        );
    }

    #[test]
    fn the_encoding_is_padded() {
        // Every input length mod 3 must round-trip to a 4-char group. The
        // compiler rejects unpadded base64 outright, so the `=` matter.
        assert_eq!(base64_standard(b"a"), "YQ==");
        assert_eq!(base64_standard(b"ab"), "YWI=");
        assert_eq!(base64_standard(b"abc"), "YWJj");
        assert_eq!(base64_standard(b""), "");
    }

    #[test]
    fn the_encoding_covers_the_whole_alphabet_including_the_high_bytes() {
        // `+` and `/` are the two characters a url-safe encoder would get
        // wrong, and they only appear for high byte values.
        assert_eq!(base64_standard(&[0xfb, 0xef, 0xbe]), "++++");
        assert_eq!(base64_standard(&[0xff, 0xff, 0xff]), "////");

        let all: Vec<u8> = (0u8..=255).collect();
        let encoded = base64_standard(&all);
        assert_eq!(encoded.len(), 344, "256 bytes encode to 344 padded chars");
        assert!(encoded.ends_with('='), "{encoded}");
    }

    #[test]
    fn a_library_with_no_handler_carries_neither_field() {
        // Both are skipped on the wire, which is what lets a bridge-only
        // library have no compile-time artifact at all.
        let json = serde_json::to_string(&entry()).unwrap();
        assert!(!json.contains("compiletime_wasm"), "{json}");
    }
}

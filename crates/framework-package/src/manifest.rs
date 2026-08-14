//! `manifest.toml` — the archive's self-description (Manager §00.14).
//!
//! This type is the whole reason a package exists rather than a bare `.wasm`.
//! A bare module cannot say which runtime it needs, cannot carry integrity
//! hashes, and cannot name its bridges — so a consumer has to be told those
//! facts out of band, and the two can disagree. Everything the deploy contract
//! checks at upload is read from here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Manifest schema version. Bumped only by a spec change to the layout
/// (Manager §00.14, "Format ownership").
pub const SPEC_VERSION: &str = "1";

/// What kind of artifact this archive holds.
///
/// Both kinds ship as `.clapp`; this field is the discriminator, not the
/// extension. `cln run` has always read the manifest to select the runtime and
/// world before touching the wasm, so the extension never carried this
/// information in the first place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An end-user application: one component, run locally.
    Clapp,
    /// A server deployment bundle: one or more components, plus migrations
    /// and generated host configuration.
    Serve,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Clapp => "clapp",
            Kind::Serve => "serve",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub spec_version: String,
    pub package: Package,
    pub build: Build,
    pub artifact: Artifact,
    pub integrity: Integrity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Provenance. `runtime_version` is an exact pin, not a hint: `cln run`
/// refuses on mismatch (Manager §00.13) and Cloud rejects a bundle whose
/// runtime it cannot provide. A component built against one runtime's host
/// contract has no guarantee against another's.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Build {
    pub compiler_version: String,
    pub framework_version: String,
    pub runtime_version: String,
    pub built_at: String,
    pub built_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: Kind,
    pub worlds: Vec<String>,
    /// `clapp` only — the single component to run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_wasm: Option<String>,
    /// `serve` only — world name to archive-relative wasm path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, String>,
    /// Bridge components carried in this archive, keyed by the WIT interface
    /// they satisfy (`"clean:session/store"`).
    ///
    /// Bridges travel *inside* the package rather than being installed per
    /// node so the artifact stays self-describing, and so an app tested
    /// against one bridge version cannot silently run against another in
    /// production — that failure produces no error, only different behaviour.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bridges: BTreeMap<String, Bridge>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bridge {
    /// Archive-relative path, under `bridges/`.
    pub path: String,
    pub name: String,
    pub version: String,
}

/// Hex-lowercase SHA-256 over every wasm in the archive, keyed by its
/// archive-relative path.
///
/// Verified by `cln run` before execution and by Cloud at upload. The producer
/// computes them once; every consumer compares rather than recomputing, so a
/// corrupted artifact is caught at the door instead of surfacing as a crash
/// loop with the real reason in a log nobody can see.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Integrity {
    pub wasm_sha256: BTreeMap<String, String>,
}

impl Manifest {
    /// Serialize for the archive root.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Every wasm this archive declares, as archive-relative paths.
    ///
    /// Bridges are included: they are components the runtime instantiates, so
    /// a corrupted bridge is exactly as fatal as a corrupted guest, and the
    /// upload check has to cover both.
    pub fn declared_wasm(&self) -> Vec<&str> {
        let mut paths: Vec<&str> = Vec::new();
        if let Some(entry) = self.artifact.entry_wasm.as_deref() {
            paths.push(entry);
        }
        paths.extend(self.artifact.entries.values().map(String::as_str));
        paths.extend(self.artifact.bridges.values().map(|b| b.path.as_str()));
        paths
    }
}

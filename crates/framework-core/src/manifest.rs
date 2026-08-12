//! `clean.toml` — step 2 of §11.2.
//!
//! M0 parses `[project]` and `[build]`, the two sections hello-world needs.
//! Everything else in the schema is captured but not interpreted.
//!
//! **Why unknown sections are kept, not rejected.** The compiler hard-errors on
//! unknown keys in the *request document* (`RQD002`), but `clean.toml` is a
//! deliberate superset of it: `[dev]`, `[security]`, `[mcp.host_runtime]` and
//! others never reach the compiler at all (§11.5 — "anything else the framework
//! needs is kept out of the request document"). Rejecting them here would make
//! a valid project unbuildable. Full CONF-06 validation against the closed
//! schema is a later phase, and it belongs next to the lockfile check, not
//! here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{FrameworkError, HostWitError, ManifestError};

/// The manifest file name. Exactly one per project root (CONF-01).
pub const MANIFEST_FILE: &str = "clean.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub project: ProjectSection,

    #[serde(default)]
    pub build: Option<BuildSection>,

    /// `[target]` — which host contract to build against. Required by the
    /// schema; `Option` here so a missing block produces `CFG001` naming the
    /// section rather than a serde parse error.
    #[serde(default)]
    pub target: Option<TargetSection>,

    /// Glob pattern -> libraries in scope. Carried through lowering verbatim
    /// (§11.4); unused in M0 because there are no libraries yet.
    #[serde(default)]
    pub folders: BTreeMap<String, Vec<String>>,

    /// Left as raw TOML in M0. Turning these into resolved
    /// `dependencies[].resolved_from` entries needs the lockfile, which is
    /// Phase 2.
    #[serde(default)]
    pub dependencies: BTreeMap<String, toml::Value>,

    /// Everything the framework does not interpret yet. Captured so a
    /// round-trip is lossless and so a project using `[dev]` still builds.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSection {
    pub name: String,
    pub version: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuildSection {
    /// Required by the schema when `[build]` is present. Validated in
    /// [`Manifest::validate`] rather than by serde so a missing target
    /// produces `CFG001` with a real message instead of a serde parse error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<bool>,

    /// The schema spells this `component-model` (hyphen); accept the
    /// underscore spelling too, since the request document uses it and users
    /// will reasonably try both.
    #[serde(default, alias = "component-model", skip_serializing_if = "Option::is_none")]
    pub component_model: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory64: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip_checks: Option<bool>,

    /// FRM-BO-05: additional paths to exclude from discovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// `[target]` — the host contract the project builds against
/// ([schema](clean.toml.md), Platform 16 §16.5).
///
/// This names *which* contract to fetch; it never carries one. The contract
/// itself is the host's `host.wit`, fetched at Moment 1 and threaded into the
/// request document as `target_world` (FRM-BO-16).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetSection {
    /// Host name, e.g. `"clean-server"`.
    pub host: String,

    /// Host version **constraint** — may be a range such as `"0.1.x"`. The
    /// concrete version it resolves to is pinned in `.cln/lock.toml`, and that
    /// is what reaches the compiler.
    pub version: String,

    /// Where to fetch `host.wit`. Absent for official hosts, which resolve
    /// from the built-in registry; third-party hosts must supply it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wit_source: Option<String>,
}

/// Which world each built-in target maps to (Platform 07 §7.2). This is the
/// `world` field of `target_world`: the framework selects, the compiler is
/// told. Resolving it compiler-side is what BVER-03 forbids.
pub fn world_for_target(target: &str) -> Option<&'static str> {
    match target {
        "wasm32-server" | "wasm64-server" => Some("server"),
        "wasm32-browser" => Some("browser"),
        "wasm32-cli" => Some("cli"),
        "wasm32-embedded" => Some("embedded"),
        _ => None,
    }
}

/// The built-in targets (Platform 07 §7.4, CONF-02). Third-party targets are
/// declared through Manager, which M0 does not consult — an unknown target is
/// `CFG001` here.
pub const BUILT_IN_TARGETS: &[&str] = &[
    "wasm32-server",
    "wasm32-browser",
    "wasm32-cli",
    "wasm32-embedded",
    "wasm64-server",
];

/// The schema default for `build.entry`.
pub const DEFAULT_ENTRY: &str = "app/main.cln";

impl Manifest {
    /// Read and validate `<project_root>/clean.toml`.
    pub fn load(project_root: &Path) -> Result<Self, FrameworkError> {
        let path = project_root.join(MANIFEST_FILE);
        if !path.exists() {
            return Err(ManifestError::Missing { path }.into());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|source| ManifestError::Unreadable { path: path.clone(), source })?;

        let manifest: Manifest = toml::from_str(&text)
            .map_err(|source| ManifestError::Malformed { path: path.clone(), source })?;

        manifest.validate(&path)?;
        Ok(manifest)
    }

    fn validate(&self, path: &Path) -> Result<(), FrameworkError> {
        if self.project.name.trim().is_empty() {
            return Err(ManifestError::EmptyProjectName { path: path.to_path_buf() }.into());
        }

        // `project.version` is a semver string per the schema. Reject early —
        // it is copied verbatim into the request document and then into every
        // downstream package manifest, so a bad value propagates far.
        if self.project.version.parse::<semver::Version>().is_err() {
            return Err(ManifestError::MalformedProjectVersion {
                path: path.to_path_buf(),
                raw: self.project.version.clone(),
            }
            .into());
        }

        let target = self.target().ok_or_else(|| ManifestError::MissingTarget {
            path: path.to_path_buf(),
        })?;

        if !BUILT_IN_TARGETS.contains(&target) {
            return Err(ManifestError::UnknownTarget {
                path: path.to_path_buf(),
                target: target.to_string(),
            }
            .into());
        }

        // `[target]` is required by the schema (ADR-0033, FRM-BO-16). There is
        // deliberately no default host: guessing would move the developer's
        // first confusing failure from this manifest line to an instantiation
        // error with no line number attached.
        let target_section = self.target.as_ref().ok_or_else(|| HostWitError::MissingSection {
            path: path.to_path_buf(),
        })?;

        if target_section.host.trim().is_empty() {
            return Err(HostWitError::MissingSection { path: path.to_path_buf() }.into());
        }

        // A host we do not know and that gives us no source cannot be fetched.
        // Catching it here means the message arrives before discovery rather
        // than after, and names the manifest rather than a URL.
        if target_section.wit_source.is_none() && !crate::hostwit::is_official(&target_section.host)
        {
            return Err(HostWitError::UnknownHost {
                host: target_section.host.clone(),
                known: crate::hostwit::official_hosts(),
            }
            .into());
        }

        // Every built-in target maps to a world; a target that does not is a
        // gap in `world_for_target`, not a user error — but it must not reach
        // the request document as an empty string.
        if world_for_target(target).is_none() {
            return Err(HostWitError::NoWorldForTarget {
                path: path.to_path_buf(),
                target: target.to_string(),
            }
            .into());
        }

        Ok(())
    }

    /// `build.target`. Required — CONF-02 gives it no default.
    pub fn target(&self) -> Option<&str> {
        self.build.as_ref()?.target.as_deref()
    }

    /// The `[target]` host-contract block. Required by the schema; validated in
    /// [`Manifest::validate`] so the failure names the section.
    pub fn target_section(&self) -> Option<&TargetSection> {
        self.target.as_ref()
    }

    /// The world `build.target` selects, for `target_world.world`.
    pub fn world(&self) -> Option<&'static str> {
        world_for_target(self.target()?)
    }

    /// `build.entry`, defaulted per the schema.
    pub fn entry(&self) -> PathBuf {
        self.build
            .as_ref()
            .and_then(|b| b.entry.clone())
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ENTRY))
    }

    /// Extra excludes from `[build].exclude` (FRM-BO-05).
    pub fn excludes(&self) -> &[String] {
        self.build.as_ref().map(|b| b.exclude.as_slice()).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::write(dir.join(MANIFEST_FILE), body).unwrap();
    }

    /// The smallest manifest that validates. `[target]` is part of "minimal"
    /// because the schema requires it — a project without one cannot be built
    /// (ADR-0033), so a fixture without one would be testing a state that
    /// cannot exist.
    const MINIMAL: &str = r#"
[project]
name = "hello-world"
version = "0.1.0"

[build]
target = "wasm32-cli"

[target]
host = "clean-cli"
version = "0.1.0"
"#;

    #[test]
    fn loads_the_minimal_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), MINIMAL);
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.project.name, "hello-world");
        assert_eq!(m.project.version, "0.1.0");
        assert_eq!(m.target(), Some("wasm32-cli"));
        assert_eq!(m.entry(), PathBuf::from("app/main.cln"));
    }

    #[test]
    fn keeps_unknown_sections_so_a_dev_section_still_builds() {
        // `[dev]` is a real clean.toml section that never reaches the compiler.
        // Rejecting it would make a valid project unbuildable.
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            &format!("{MINIMAL}\n[dev]\nwatch = true\n\n[security]\nallowedHostModules = []\n"),
        );
        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.extra.contains_key("dev"));
        assert!(m.extra.contains_key("security"));
    }

    #[test]
    fn missing_manifest_names_the_expected_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("clean.toml"), "got {err}");
    }

    #[test]
    fn missing_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "[project]\nname = \"x\"\nversion = \"0.1.0\"\n");
        let err = Manifest::load(dir.path()).unwrap_err();
        assert_eq!(err.code(), "CFG001", "got {err}");
    }

    #[test]
    fn unknown_target_is_cfg001() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-toaster\"\n",
        );
        let err = Manifest::load(dir.path()).unwrap_err();
        assert_eq!(err.code(), "CFG001");
        assert!(err.to_string().contains("wasm32-toaster"));
    }

    #[test]
    fn malformed_project_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "[project]\nname = \"x\"\nversion = \"one\"\n[build]\ntarget = \"wasm32-cli\"\n",
        );
        assert!(Manifest::load(dir.path()).is_err());
    }

    #[test]
    fn accepts_both_component_model_spellings() {
        // Spelled out rather than appended to MINIMAL: the key belongs under
        // [build], and MINIMAL now ends with [target], so concatenation would
        // silently file it in the wrong section and test nothing.
        let with_spelling = |spelling: &str, value: bool| {
            format!(
                "[project]\nname = \"x\"\nversion = \"0.1.0\"\n\n\
                 [build]\ntarget = \"wasm32-cli\"\n{spelling} = {value}\n\n\
                 [target]\nhost = \"clean-cli\"\nversion = \"0.1.0\"\n"
            )
        };

        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), &with_spelling("component-model", true));
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.build.unwrap().component_model, Some(true));

        write_manifest(dir.path(), &with_spelling("component_model", false));
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.build.unwrap().component_model, Some(false));
    }

    #[test]
    fn the_target_section_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), MINIMAL);
        let m = Manifest::load(dir.path()).unwrap();
        let target = m.target_section().unwrap();
        assert_eq!(target.host, "clean-cli");
        assert_eq!(target.version, "0.1.0");
        assert_eq!(target.wit_source, None);
    }

    #[test]
    fn a_missing_target_section_is_an_error_not_a_default() {
        // ADR-0033: no default host. Guessing would move the first confusing
        // failure from this manifest line to instantiation time.
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-cli\"\n",
        );
        let err = Manifest::load(dir.path()).unwrap_err();
        assert_eq!(err.code(), "CFG001");
        assert!(err.to_string().contains("[target]"), "got {err}");
        assert!(err.help().unwrap().contains("host"), "must say what to add");
    }

    #[test]
    fn an_unknown_host_without_a_wit_source_is_rejected_early() {
        // Caught at manifest load, so the message names clean.toml rather than
        // a URL that was never going to resolve.
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-cli\"\n\
             [target]\nhost = \"pixel-engine\"\nversion = \"0.1.0\"\n",
        );
        let err = Manifest::load(dir.path()).unwrap_err();
        assert_eq!(err.code(), "CFG001");
        assert!(err.help().unwrap().contains("wit_source"), "got {err:?}");
    }

    #[test]
    fn a_third_party_host_with_a_wit_source_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-cli\"\n\
             [target]\nhost = \"pixel-engine\"\nversion = \"0.1.0\"\n\
             wit_source = \"https://example.test/host.wit\"\n",
        );
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(
            m.target_section().unwrap().wit_source.as_deref(),
            Some("https://example.test/host.wit")
        );
    }

    #[test]
    fn every_built_in_target_maps_to_a_world() {
        // If this fails, a target reaches the request document with an empty
        // world and the compiler checks against nothing.
        for target in BUILT_IN_TARGETS {
            assert!(
                world_for_target(target).is_some(),
                "no world mapped for built-in target {target}"
            );
        }
        assert_eq!(world_for_target("wasm32-toaster"), None);
    }

    #[test]
    fn malformed_toml_reports_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(dir.path(), "[project\nname =");
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("clean.toml"), "got {err}");
    }
}

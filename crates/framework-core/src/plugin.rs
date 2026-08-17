//! `plugin.toml` and `plugin.wasm` — §12, rules FRM-PM-01 through FRM-PM-03.
//!
//! A plugin is a dependency that ships **pre-built WASM** rather than Clean
//! source. The framework never compiles it. What it does instead is check that
//! the plugin is what its manifest claims — and then let the plugin extend the
//! build in two ways the compiler must be told about:
//!
//! - **`[paths].owns`** adds discovery roots (§11.3, FRM-BO-03 item 3), so a
//!   plugin can own a folder like `ui/` and have its files compiled.
//! - **`[paths].patterns`** adds file extensions (§11.4), so a plugin can
//!   claim `.ui.cln` or another suffix its handler understands.
//!
//! # Why the exports are checked against the bytes
//!
//! FRM-PM-03 requires that every function named in `[exports]` actually exist
//! in `plugin.wasm`. It would be cheaper to trust the manifest — but the
//! manifest and the WASM are built at different times by different tools, and
//! a rename that updates one and not the other is the ordinary way this breaks.
//!
//! Left unchecked, the failure lands much later: the compiler emits a call to
//! a function that is not there, and the developer sees an instantiation error
//! naming a symbol they never wrote, in a plugin they did not build. Checking
//! here costs one parse of a file already on disk and names the plugin, the
//! export, and the manifest line instead.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest a plugin ships at its root.
pub const PLUGIN_FILE: &str = "plugin.toml";

/// The pre-built component beside it (FRM-PM-01).
pub const PLUGIN_WASM: &str = "plugin.wasm";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginSection,

    /// Functions this plugin exports, name → descriptor. Validated against
    /// `plugin.wasm` (FRM-PM-03).
    #[serde(default)]
    pub exports: BTreeMap<String, toml::Value>,

    /// How this plugin extends discovery.
    #[serde(default)]
    pub paths: Option<PathsSection>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginSection {
    pub name: String,
    pub version: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// The compiler version this plugin's WASM was built against, when the
    /// plugin records it. Informational here — the framework does not gate on
    /// it, because the compiler is the authority on whether it can load a
    /// component and duplicating that rule would create a second, staler one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_with_compiler: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathsSection {
    /// Project-relative folders this plugin owns, added as discovery roots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owns: Vec<PathBuf>,

    /// Additional file extensions this plugin's sources use, without the
    /// leading dot (`"ui.cln"`, not `".ui.cln"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("no {PLUGIN_FILE} found at {}", .path.display())]
    Missing { path: PathBuf },

    #[error("could not read {}: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse {}: {source}", .path.display())]
    Malformed {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("{}: [plugin].name must not be empty", .path.display())]
    EmptyName { path: PathBuf },

    #[error("{}: [plugin].version '{raw}' is not a semver version", .path.display())]
    MalformedVersion { path: PathBuf, raw: String },

    /// FRM-PM-01: the manifest is only half a plugin.
    #[error("{} declares a plugin but {PLUGIN_WASM} is missing", .path.display())]
    WasmMissing { path: PathBuf },

    #[error("could not read {}: {source}", .path.display())]
    WasmUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// FRM-PM-02: the bytes are not a WASM module or component at all.
    #[error("{} is not valid WebAssembly: {reason}", .path.display())]
    WasmInvalid { path: PathBuf, reason: String },

    /// FRM-PM-03: the manifest names an export the WASM does not have.
    #[error("plugin '{plugin}' declares export '{export}', which {} does not provide", .wasm.display())]
    ExportMissing { plugin: String, export: String, wasm: PathBuf, available: Vec<String> },

    /// An `owns` entry that escapes the project would let a dependency pull
    /// arbitrary files off the developer's disk into the compilation.
    #[error("plugin '{plugin}' owns '{}', which is outside the project", .path.display())]
    OwnsEscapesProject { plugin: String, path: PathBuf },
}

impl PluginError {
    pub fn code(&self) -> &'static str {
        match self {
            PluginError::Missing { .. }
            | PluginError::Unreadable { .. }
            | PluginError::Malformed { .. }
            | PluginError::WasmUnreadable { .. } => "CFG003",

            PluginError::EmptyName { .. }
            | PluginError::MalformedVersion { .. }
            | PluginError::OwnsEscapesProject { .. } => "CFG001",

            // The plugin is internally inconsistent: manifest and WASM
            // disagree, or the WASM is unusable. `FRM007` is the plugin
            // validation code — distinct from a bad manifest, because the fix
            // is to rebuild the plugin, not to edit a file.
            PluginError::WasmMissing { .. }
            | PluginError::WasmInvalid { .. }
            | PluginError::ExportMissing { .. } => "FRM007",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            PluginError::WasmMissing { .. } => {
                Some(format!("a plugin ships {PLUGIN_FILE} and {PLUGIN_WASM} together; rebuild the plugin"))
            }
            PluginError::WasmInvalid { .. } => {
                Some("rebuild the plugin; the shipped .wasm is corrupt or truncated".into())
            }
            PluginError::ExportMissing { available, .. } => {
                if available.is_empty() {
                    Some("the plugin's .wasm exports nothing; rebuild it".into())
                } else {
                    // Naming what IS there turns "it's missing" into a
                    // one-glance diagnosis of a rename or a stale build.
                    Some(format!("the .wasm exports: {}", available.join(", ")))
                }
            }
            PluginError::OwnsEscapesProject { .. } => {
                Some("[paths].owns entries must be relative paths inside the project".into())
            }
            PluginError::MalformedVersion { .. } => {
                Some("use a semver version such as \"1.0.0\"".into())
            }
            _ => None,
        }
    }

    pub fn path(&self) -> &PathBuf {
        match self {
            PluginError::Missing { path }
            | PluginError::Unreadable { path, .. }
            | PluginError::Malformed { path, .. }
            | PluginError::EmptyName { path }
            | PluginError::MalformedVersion { path, .. }
            | PluginError::WasmMissing { path }
            | PluginError::WasmUnreadable { path, .. }
            | PluginError::WasmInvalid { path, .. }
            | PluginError::OwnsEscapesProject { path, .. } => path,
            PluginError::ExportMissing { wasm, .. } => wasm,
        }
    }
}

/// A plugin that has been read and checked against its own WASM.
#[derive(Clone, Debug)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    /// Absolute path to the validated `plugin.wasm`.
    pub wasm_path: PathBuf,
    /// Hex-lowercase SHA-256 of the WASM bytes. Part of build identity: a
    /// plugin that changed produces a different build even when every source
    /// file and manifest is byte-identical.
    pub wasm_sha256: String,
}

impl LoadedPlugin {
    pub fn name(&self) -> &str {
        &self.manifest.plugin.name
    }

    /// Discovery roots this plugin contributes, project-relative.
    pub fn owned_roots(&self) -> &[PathBuf] {
        self.manifest
            .paths
            .as_ref()
            .map(|p| p.owns.as_slice())
            .unwrap_or_default()
    }

    /// File extensions this plugin contributes, without the leading dot.
    pub fn patterns(&self) -> &[String] {
        self.manifest
            .paths
            .as_ref()
            .map(|p| p.patterns.as_slice())
            .unwrap_or_default()
    }
}

impl PluginManifest {
    /// Read and validate `<plugin_root>/plugin.toml` **and** its
    /// `plugin.wasm`, per FRM-PM-01..03.
    ///
    /// The two are loaded together deliberately: a `PluginManifest` on its own
    /// is a set of claims, and every caller in the framework needs the checked
    /// version. Splitting them would make "did anyone validate this?" a
    /// question the type could not answer.
    pub fn load(plugin_root: &Path) -> Result<LoadedPlugin, PluginError> {
        let path = plugin_root.join(PLUGIN_FILE);
        if !path.exists() {
            return Err(PluginError::Missing { path });
        }

        let text = std::fs::read_to_string(&path)
            .map_err(|source| PluginError::Unreadable { path: path.clone(), source })?;

        let manifest: PluginManifest = toml::from_str(&text)
            .map_err(|source| PluginError::Malformed { path: path.clone(), source })?;

        manifest.validate(&path)?;

        // FRM-PM-01: the WASM must be there.
        let wasm_path = plugin_root.join(PLUGIN_WASM);
        if !wasm_path.is_file() {
            return Err(PluginError::WasmMissing { path: path.clone() });
        }

        let bytes = std::fs::read(&wasm_path)
            .map_err(|source| PluginError::WasmUnreadable { path: wasm_path.clone(), source })?;

        // FRM-PM-02 and FRM-PM-03.
        let exported = read_exports(&bytes, &wasm_path)?;
        manifest.check_exports(&exported, &wasm_path)?;

        Ok(LoadedPlugin {
            wasm_sha256: framework_compiler_driver::artifact::hex_sha256(&bytes),
            manifest,
            wasm_path,
        })
    }

    fn validate(&self, path: &Path) -> Result<(), PluginError> {
        if self.plugin.name.trim().is_empty() {
            return Err(PluginError::EmptyName { path: path.to_path_buf() });
        }

        if self.plugin.version.parse::<semver::Version>().is_err() {
            return Err(PluginError::MalformedVersion {
                path: path.to_path_buf(),
                raw: self.plugin.version.clone(),
            });
        }

        // A dependency that can name `../../..` as a folder it owns can pull
        // anything on the disk into the compilation.
        for owned in self.paths.iter().flat_map(|p| p.owns.iter()) {
            if escapes_project(owned) {
                return Err(PluginError::OwnsEscapesProject {
                    plugin: self.plugin.name.clone(),
                    path: owned.clone(),
                });
            }
        }

        Ok(())
    }

    /// FRM-PM-03: every declared export must exist in the WASM.
    fn check_exports(
        &self,
        exported: &BTreeSet<String>,
        wasm_path: &Path,
    ) -> Result<(), PluginError> {
        for declared in self.exports.keys() {
            if !exported.contains(declared) {
                return Err(PluginError::ExportMissing {
                    plugin: self.plugin.name.clone(),
                    export: declared.clone(),
                    wasm: wasm_path.to_path_buf(),
                    available: exported.iter().cloned().collect(),
                });
            }
        }
        Ok(())
    }
}

/// Does this project-relative path leave the project?
///
/// Purely lexical — the path need not exist yet, and resolving symlinks here
/// would make the answer depend on the developer's disk rather than on what
/// the plugin declared.
fn escapes_project(path: &Path) -> bool {
    use std::path::Component;

    if path.is_absolute() {
        return true;
    }

    let mut depth = 0i32;
    for component in path.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::Normal(_) => depth += 1,
            // A leading `/` is caught by `is_absolute`; `.` changes nothing.
            Component::RootDir | Component::Prefix(_) => return true,
            Component::CurDir => {}
        }
    }
    false
}

/// Every function name a WASM module or component exports.
///
/// Handles both shapes: plugins built as core modules and plugins built as
/// components. The framework accepts either because the compiler does — and
/// rejecting one here would make this a second, stricter gate on what a plugin
/// may be.
fn read_exports(bytes: &[u8], path: &Path) -> Result<BTreeSet<String>, PluginError> {
    use wasmparser::{Parser, Payload};

    let mut exports = BTreeSet::new();
    // Component and module sections can nest. Only the outermost export
    // section is the plugin's public surface; a nested module's exports are
    // internal to it and naming one in `[exports]` would be wrong.
    let mut depth = 0u32;

    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.map_err(|e| PluginError::WasmInvalid {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

        match payload {
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => depth += 1,

            Payload::End(_) => depth = depth.saturating_sub(1),

            Payload::ExportSection(section) if depth == 0 => {
                for export in section {
                    let export = export.map_err(|e| PluginError::WasmInvalid {
                        path: path.to_path_buf(),
                        reason: e.to_string(),
                    })?;
                    exports.insert(export.name.to_string());
                }
            }

            Payload::ComponentExportSection(section) if depth == 0 => {
                for export in section {
                    let export = export.map_err(|e| PluginError::WasmInvalid {
                        path: path.to_path_buf(),
                        reason: e.to_string(),
                    })?;
                    // `ComponentExternName` carries an optional `implements`
                    // alongside the name; `[exports]` keys name the export
                    // itself, so that is what is compared.
                    exports.insert(export.name.name.to_string());
                }
            }

            _ => {}
        }
    }

    Ok(exports)
}

/// The smallest valid core module: the 8-byte header alone.
///
/// Shared with [`crate::closure`]'s tests, which need a plugin that loads
/// without caring what it exports.
#[cfg(test)]
pub(crate) const EMPTY_MODULE: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

#[cfg(test)]
mod tests {
    use super::*;

    /// A core module exporting one function named `render`.
    ///
    /// Hand-assembled rather than built with a WASM crate: the bytes are the
    /// point of the test, and a builder would only re-encode what is written
    /// here while adding a dependency.
    fn module_exporting(name: &str) -> Vec<u8> {
        let mut wasm = EMPTY_MODULE.to_vec();

        // Type section: one type, `() -> ()`.
        wasm.extend_from_slice(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00]);
        // Function section: one function, of type 0.
        wasm.extend_from_slice(&[0x03, 0x02, 0x01, 0x00]);

        // Export section: one export, `name` → func 0.
        let mut entry = vec![0x01];
        entry.push(name.len() as u8);
        entry.extend_from_slice(name.as_bytes());
        entry.extend_from_slice(&[0x00, 0x00]);
        wasm.push(0x07);
        wasm.push(entry.len() as u8);
        wasm.extend_from_slice(&entry);

        // Code section: one body, empty.
        wasm.extend_from_slice(&[0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b]);
        wasm
    }

    struct Plugin {
        dir: tempfile::TempDir,
    }

    impl Plugin {
        fn new(manifest: &str, wasm: &[u8]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(PLUGIN_FILE), manifest).unwrap();
            std::fs::write(dir.path().join(PLUGIN_WASM), wasm).unwrap();
            Plugin { dir }
        }

        fn load(&self) -> Result<LoadedPlugin, PluginError> {
            PluginManifest::load(self.dir.path())
        }
    }

    const MINIMAL: &str = "[plugin]\nname = \"frame.ui\"\nversion = \"0.4.0\"\n";

    #[test]
    fn loads_a_plugin_whose_exports_match_its_wasm() {
        let plugin = Plugin::new(
            &format!("{MINIMAL}\n[exports]\nrender = {{ params = [] }}\n"),
            &module_exporting("render"),
        );
        let loaded = plugin.load().unwrap();
        assert_eq!(loaded.name(), "frame.ui");
        assert_eq!(loaded.wasm_sha256.len(), 64);
    }

    #[test]
    fn a_declared_export_the_wasm_lacks_is_refused() {
        // FRM-PM-03. Without this the failure lands at instantiation, naming a
        // symbol the developer never wrote in a plugin they did not build.
        let plugin = Plugin::new(
            &format!("{MINIMAL}\n[exports]\nrender = {{ params = [] }}\n"),
            &module_exporting("draw"),
        );
        let err = plugin.load().unwrap_err();

        assert!(matches!(err, PluginError::ExportMissing { .. }), "got {err}");
        assert!(err.to_string().contains("render"), "must name the export: {err}");
        assert!(err.to_string().contains("frame.ui"), "must name the plugin: {err}");
        // Naming what IS there turns this into a one-glance diagnosis.
        assert!(err.help().unwrap().contains("draw"), "got {err:?}");
        assert_eq!(err.code(), "FRM007");
    }

    #[test]
    fn a_plugin_declaring_no_exports_is_fine() {
        // A plugin may exist purely to own a folder and extend discovery.
        let plugin = Plugin::new(MINIMAL, EMPTY_MODULE);
        assert!(plugin.load().is_ok());
    }

    #[test]
    fn a_manifest_without_a_wasm_is_refused() {
        // FRM-PM-01: the manifest alone is half a plugin.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PLUGIN_FILE), MINIMAL).unwrap();

        let err = PluginManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, PluginError::WasmMissing { .. }), "got {err}");
        assert!(err.help().unwrap().contains(PLUGIN_WASM));
    }

    #[test]
    fn a_corrupt_wasm_is_refused_rather_than_passed_to_the_compiler() {
        // FRM-PM-02.
        let plugin = Plugin::new(MINIMAL, b"this is not wasm at all");
        let err = plugin.load().unwrap_err();
        assert!(matches!(err, PluginError::WasmInvalid { .. }), "got {err}");
        assert!(err.help().unwrap().contains("rebuild"));
    }

    #[test]
    fn a_truncated_wasm_is_refused() {
        // The header alone is valid; a half-written section is not. This is
        // what an interrupted build leaves behind.
        let mut truncated = module_exporting("render");
        truncated.truncate(truncated.len() - 3);
        let plugin = Plugin::new(MINIMAL, &truncated);
        assert!(matches!(plugin.load().unwrap_err(), PluginError::WasmInvalid { .. }));
    }

    #[test]
    fn declared_paths_are_reported_for_discovery() {
        let plugin = Plugin::new(
            &format!("{MINIMAL}\n[paths]\nowns = [\"ui\"]\npatterns = [\"ui.cln\"]\n"),
            EMPTY_MODULE,
        );
        let loaded = plugin.load().unwrap();
        assert_eq!(loaded.owned_roots(), [PathBuf::from("ui")]);
        assert_eq!(loaded.patterns(), ["ui.cln"]);
    }

    #[test]
    fn a_plugin_owning_a_path_outside_the_project_is_refused() {
        // A dependency that can own `../../..` pulls arbitrary files off the
        // developer's disk into the compilation.
        for escape in ["../secrets", "ui/../../elsewhere", "/etc"] {
            let plugin = Plugin::new(
                &format!("{MINIMAL}\n[paths]\nowns = [\"{escape}\"]\n"),
                EMPTY_MODULE,
            );
            let err = plugin.load().unwrap_err();
            assert!(
                matches!(err, PluginError::OwnsEscapesProject { .. }),
                "{escape} must be refused, got {err}"
            );
        }
    }

    #[test]
    fn a_path_that_dips_but_stays_inside_is_allowed() {
        // `ui/../ui` is silly but harmless, and refusing it would mean
        // rejecting a normalized-differently path that names a real folder.
        let plugin = Plugin::new(
            &format!("{MINIMAL}\n[paths]\nowns = [\"ui/../shared\"]\n"),
            EMPTY_MODULE,
        );
        assert!(plugin.load().is_ok());
    }

    #[test]
    fn the_wasm_hash_is_part_of_what_was_loaded() {
        // Build identity: a plugin that changed must produce a different
        // build even when every source file is byte-identical.
        let one = Plugin::new(MINIMAL, &module_exporting("a")).load().unwrap();
        let two = Plugin::new(MINIMAL, &module_exporting("b")).load().unwrap();
        assert_ne!(one.wasm_sha256, two.wasm_sha256);
    }

    #[test]
    fn a_malformed_version_is_refused() {
        let plugin = Plugin::new(
            "[plugin]\nname = \"x\"\nversion = \"latest\"\n",
            EMPTY_MODULE,
        );
        assert!(matches!(plugin.load().unwrap_err(), PluginError::MalformedVersion { .. }));
    }

    #[test]
    fn unknown_sections_are_kept_so_a_newer_plugin_still_loads() {
        let plugin = Plugin::new(&format!("{MINIMAL}\n[lint]\nrules = []\n"), EMPTY_MODULE);
        assert!(plugin.load().unwrap().manifest.extra.contains_key("lint"));
    }

    #[test]
    fn a_missing_plugin_toml_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = PluginManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, PluginError::Missing { .. }));
        assert!(err.to_string().contains(PLUGIN_FILE), "got {err}");
    }
}

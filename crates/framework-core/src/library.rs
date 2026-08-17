//! `library.toml` — a dependency's own manifest, step 4 of §11.2.
//!
//! This is the file a Clean *library* ships at its root. It is not
//! `clean.toml`: a library has no `[build].target` and no `[target]` host
//! block, because a library is not built against a host — it is compiled into
//! whatever application depends on it, against that application's host.
//! Sharing one type between the two would mean a pile of `Option`s that are
//! always `None` on one side, and validation that cannot say which file it is
//! complaining about.
//!
//! What the compiler needs from here is small: the name and version (to bind
//! imports), the WIT the library exports (to type-check against), and the
//! blocks it declares it handles (so pass 6 knows to run its handler). Those
//! three become a `library_manifests[]` entry. Everything else in the file is
//! kept but not interpreted, for the same reason as in [`crate::manifest`]:
//! a library using a section this framework does not know yet must still be
//! usable, not unbuildable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use framework_compiler_driver::request::LibraryManifest as RequestLibrary;
use serde::{Deserialize, Serialize};

/// The manifest file a library ships at its root.
pub const LIBRARY_FILE: &str = "library.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryManifest {
    pub library: LibrarySection,

    /// What this library exposes to its dependents.
    #[serde(default)]
    pub exports: Option<ExportsSection>,

    /// `handles block <name>` declarations, which tell the compiler to run
    /// this library's compile-time handler for those blocks (Phase 3).
    #[serde(default)]
    pub handles: Option<HandlesSection>,

    /// This library's own dependencies. Read for completeness; the *authority*
    /// on the closure is `.cln/lock.toml`, which Manager resolved. Framework
    /// never re-resolves from here — two resolvers would eventually disagree.
    #[serde(default)]
    pub dependencies: BTreeMap<String, toml::Value>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibrarySection {
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
pub struct ExportsSection {
    /// The library's WIT, either inline...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wit: Option<String>,

    /// ...or in a file beside the manifest. Exactly one of the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wit_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HandlesSection {
    /// Block names this library handles at compile time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("no {LIBRARY_FILE} found at {}", .path.display())]
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

    #[error("{}: [library].name must not be empty", .path.display())]
    EmptyName { path: PathBuf },

    #[error("{}: [library].version '{raw}' is not a semver version", .path.display())]
    MalformedVersion { path: PathBuf, raw: String },

    #[error("{}: [exports] sets both `wit` and `wit_file`", .path.display())]
    AmbiguousExports { path: PathBuf },

    #[error("could not read {}: {source}", .path.display())]
    WitUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The manifest says the library handles a block but names it emptily.
    #[error("{}: [handles].blocks contains an empty name", .path.display())]
    EmptyBlockName { path: PathBuf },
}

impl LibraryError {
    pub fn code(&self) -> &'static str {
        match self {
            // The file could not be read at all.
            LibraryError::Missing { .. }
            | LibraryError::Unreadable { .. }
            | LibraryError::Malformed { .. }
            | LibraryError::WitUnreadable { .. } => "CFG003",
            // It parsed, but violates the schema.
            LibraryError::EmptyName { .. }
            | LibraryError::MalformedVersion { .. }
            | LibraryError::AmbiguousExports { .. }
            | LibraryError::EmptyBlockName { .. } => "CFG001",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            LibraryError::Missing { .. } => Some(format!(
                "every Clean library ships a {LIBRARY_FILE}; run `cln fetch` if the checkout is incomplete"
            )),
            LibraryError::AmbiguousExports { .. } => {
                Some("set either `wit` or `wit_file`, not both".into())
            }
            LibraryError::MalformedVersion { .. } => {
                Some("use a semver version such as \"1.0.0\"".into())
            }
            _ => None,
        }
    }

    pub fn path(&self) -> &PathBuf {
        match self {
            LibraryError::Missing { path }
            | LibraryError::Unreadable { path, .. }
            | LibraryError::Malformed { path, .. }
            | LibraryError::EmptyName { path }
            | LibraryError::MalformedVersion { path, .. }
            | LibraryError::AmbiguousExports { path }
            | LibraryError::WitUnreadable { path, .. }
            | LibraryError::EmptyBlockName { path } => path,
        }
    }
}

impl LibraryManifest {
    /// Read and validate `<library_root>/library.toml`, resolving `wit_file`
    /// against `library_root` if the manifest uses one.
    pub fn load(library_root: &Path) -> Result<Self, LibraryError> {
        let path = library_root.join(LIBRARY_FILE);
        if !path.exists() {
            return Err(LibraryError::Missing { path });
        }

        let text = std::fs::read_to_string(&path)
            .map_err(|source| LibraryError::Unreadable { path: path.clone(), source })?;

        let mut manifest: LibraryManifest = toml::from_str(&text)
            .map_err(|source| LibraryError::Malformed { path: path.clone(), source })?;

        manifest.validate(&path)?;
        manifest.inline_wit_file(library_root, &path)?;
        Ok(manifest)
    }

    fn validate(&self, path: &Path) -> Result<(), LibraryError> {
        if self.library.name.trim().is_empty() {
            return Err(LibraryError::EmptyName { path: path.to_path_buf() });
        }

        // Copied verbatim into `library_manifests[]` and from there into the
        // build cache key, so a bad value propagates far.
        if self.library.version.parse::<semver::Version>().is_err() {
            return Err(LibraryError::MalformedVersion {
                path: path.to_path_buf(),
                raw: self.library.version.clone(),
            });
        }

        // Two sources for one value: whichever we picked would be a silent
        // choice, and the other would look like it had taken effect.
        if let Some(exports) = &self.exports {
            if exports.wit.is_some() && exports.wit_file.is_some() {
                return Err(LibraryError::AmbiguousExports { path: path.to_path_buf() });
            }
        }

        if let Some(handles) = &self.handles {
            if handles.blocks.iter().any(|b| b.trim().is_empty()) {
                return Err(LibraryError::EmptyBlockName { path: path.to_path_buf() });
            }
        }

        Ok(())
    }

    /// Replace `wit_file` with its contents, so everything downstream sees one
    /// representation. The request document carries WIT by value — a path
    /// would make the request non-self-contained and break `cln repro build`.
    fn inline_wit_file(
        &mut self,
        library_root: &Path,
        manifest_path: &Path,
    ) -> Result<(), LibraryError> {
        let Some(exports) = self.exports.as_mut() else { return Ok(()) };
        let Some(relative) = exports.wit_file.take() else { return Ok(()) };

        let wit_path = library_root.join(&relative);
        let wit = std::fs::read_to_string(&wit_path).map_err(|source| {
            // Name the WIT file, not the manifest: that is the file to fix.
            let _ = manifest_path;
            LibraryError::WitUnreadable { path: wit_path, source }
        })?;

        exports.wit = Some(wit);
        Ok(())
    }

    /// The library's WIT, or empty if it exports none.
    ///
    /// A library with no `[exports]` is legal — an implementation-only library
    /// that adds no interface of its own, existing to handle a block or to
    /// pull in its own dependencies.
    pub fn wit(&self) -> &str {
        self.exports
            .as_ref()
            .and_then(|e| e.wit.as_deref())
            .unwrap_or_default()
    }

    /// Blocks this library handles, sorted — the request document must not
    /// vary with the order they happened to be written in.
    pub fn handled_blocks(&self) -> Vec<String> {
        let mut blocks = self
            .handles
            .as_ref()
            .map(|h| h.blocks.clone())
            .unwrap_or_default();
        blocks.sort();
        blocks.dedup();
        blocks
    }

    /// Project into the request document's `library_manifests[]` entry.
    ///
    /// `compiletime_wasm_sha256` stays `None`: it is the hash of the compiled
    /// block handler, which Phase 3 produces. Filling it with anything now
    /// would be a claim the framework cannot back.
    pub fn into_request_entry(self) -> RequestLibrary {
        let handles_blocks = self.handled_blocks();
        RequestLibrary {
            name: self.library.name,
            version: self.library.version,
            wit: self.exports.and_then(|e| e.wit).unwrap_or_default(),
            handles_blocks,
            compiletime_wasm_sha256: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) {
        std::fs::write(dir.join(LIBRARY_FILE), body).unwrap();
    }

    const MINIMAL: &str = r#"
[library]
name = "frame.data"
version = "2.1.2"
"#;

    #[test]
    fn loads_a_minimal_library() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), MINIMAL);
        let lib = LibraryManifest::load(dir.path()).unwrap();
        assert_eq!(lib.library.name, "frame.data");
        assert_eq!(lib.library.version, "2.1.2");
        // No [exports] is legal: an implementation-only library.
        assert_eq!(lib.wit(), "");
        assert!(lib.handled_blocks().is_empty());
    }

    #[test]
    fn inline_wit_reaches_the_request_entry() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &format!("{MINIMAL}\n[exports]\nwit = \"interface data {{}}\"\n"),
        );
        let entry = LibraryManifest::load(dir.path()).unwrap().into_request_entry();
        assert_eq!(entry.name, "frame.data");
        assert_eq!(entry.wit, "interface data {}");
        assert_eq!(entry.compiletime_wasm_sha256, None, "Phase 3 fills this, not Phase 2");
    }

    #[test]
    fn a_wit_file_is_inlined_so_the_request_stays_self_contained() {
        // The request document carries WIT by value; a path would break
        // `cln repro build` on another machine.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.wit"), "interface data { get: func(); }").unwrap();
        write(dir.path(), &format!("{MINIMAL}\n[exports]\nwit_file = \"data.wit\"\n"));

        let lib = LibraryManifest::load(dir.path()).unwrap();
        assert_eq!(lib.wit(), "interface data { get: func(); }");
        assert!(lib.exports.as_ref().unwrap().wit_file.is_none(), "path is consumed");
    }

    #[test]
    fn a_missing_wit_file_names_the_wit_file_not_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &format!("{MINIMAL}\n[exports]\nwit_file = \"gone.wit\"\n"));
        let err = LibraryManifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("gone.wit"), "got {err}");
    }

    #[test]
    fn setting_both_wit_and_wit_file_is_refused() {
        // Whichever won would be a silent choice, and the other would look
        // like it had taken effect.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.wit"), "interface data {}").unwrap();
        write(
            dir.path(),
            &format!("{MINIMAL}\n[exports]\nwit = \"interface x {{}}\"\nwit_file = \"data.wit\"\n"),
        );
        let err = LibraryManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, LibraryError::AmbiguousExports { .. }), "got {err}");
        assert_eq!(err.code(), "CFG001");
    }

    #[test]
    fn handled_blocks_are_sorted_and_deduped() {
        // The request document must not vary with the order blocks happened to
        // be written in — that would change the build-cache key for free.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &format!("{MINIMAL}\n[handles]\nblocks = [\"table\", \"index\", \"table\"]\n"),
        );
        let lib = LibraryManifest::load(dir.path()).unwrap();
        assert_eq!(lib.handled_blocks(), vec!["index", "table"]);
    }

    #[test]
    fn a_missing_library_toml_says_which_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = LibraryManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, LibraryError::Missing { .. }));
        assert!(err.to_string().contains(LIBRARY_FILE), "got {err}");
    }

    #[test]
    fn a_malformed_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "[library]\nname = \"x\"\nversion = \"latest\"\n");
        let err = LibraryManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, LibraryError::MalformedVersion { .. }), "got {err}");
    }

    #[test]
    fn an_empty_block_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &format!("{MINIMAL}\n[handles]\nblocks = [\"table\", \"\"]\n"));
        let err = LibraryManifest::load(dir.path()).unwrap_err();
        assert!(matches!(err, LibraryError::EmptyBlockName { .. }), "got {err}");
    }

    #[test]
    fn unknown_sections_are_kept_so_a_newer_library_still_builds() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &format!("{MINIMAL}\n[lint]\nrules = []\n"));
        let lib = LibraryManifest::load(dir.path()).unwrap();
        assert!(lib.extra.contains_key("lint"));
    }

    #[test]
    fn a_librarys_own_dependencies_are_read_but_not_authoritative() {
        // The lockfile is the authority on the closure. This is read so the
        // manifest round-trips, not so the framework can resolve from it.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &format!("{MINIMAL}\n[dependencies]\nfoundation = \"^1.0\"\n"));
        let lib = LibraryManifest::load(dir.path()).unwrap();
        assert!(lib.dependencies.contains_key("foundation"));
    }
}

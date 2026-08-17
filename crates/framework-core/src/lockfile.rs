//! `.cln/lock.toml` — the resolved dependency closure, read-only.
//!
//! **Manager owns this file; the framework only reads it.** `cln add` writes
//! it, `cln fetch` refreshes it. Nothing here ever creates or edits a
//! `[[package]]` entry — the framework's sole write to this file is the
//! `[host.<name>]` pin in [`crate::hostwit::pin_hash`], which is a different
//! section entirely.
//!
//! # The `kind` field
//!
//! PLAN.md open question #1: Manager's `cln add` is overloaded for libraries
//! (Clean source, needs its handler compiled) and plugins (pre-built WASM,
//! loaded as-is). The framework must tell them apart — it compiles one and
//! validates the other — and the two cannot be distinguished by name or
//! version alone.
//!
//! So every entry carries an explicit `kind = "library" | "plugin"`. This is
//! the proposal in the plan, implemented ahead of Manager confirming the
//! schema, and the reason this module exists as its own file: **if Manager
//! lands a different shape, this is the only file that changes.** Everything
//! downstream consumes [`LockedPackage`], not TOML.
//!
//! An entry with no `kind` is refused rather than guessed at (see
//! [`LockError::MissingKind`]). Defaulting it would pick a compile strategy
//! for a package by coin-flip, and the failure would surface as a confusing
//! compiler error about a missing handler rather than a line naming the
//! lockfile.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the lockfile lives, project-relative. Shared with
/// [`crate::hostwit`], which pins the host contract into the same file.
pub const LOCKFILE: &str = ".cln/lock.toml";

/// What a locked package *is*, which decides what the framework does with it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    /// Clean source. Its `library.toml` is read, its `handles block` handlers
    /// are compiled (Phase 3), and it reaches the compiler as a
    /// `library_manifests[]` entry.
    Library,
    /// A pre-built `plugin.wasm` plus `plugin.toml`. Never compiled; its
    /// declared exports are validated against the WASM and its paths extend
    /// discovery (Phase 4).
    Plugin,
}

impl PackageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageKind::Library => "library",
            PackageKind::Plugin => "plugin",
        }
    }

    /// The manifest file this kind of package carries at its root.
    pub fn manifest_file(self) -> &'static str {
        match self {
            PackageKind::Library => "library.toml",
            PackageKind::Plugin => "plugin.toml",
        }
    }
}

/// Where a package's bytes came from. M0/M1 support path and git only —
/// the registry is M3 (PLAN.md §8), so a `registry` source parses but has no
/// resolved location on disk yet and is refused at closure-walk time rather
/// than here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    /// A local directory, relative to the project root (or absolute).
    Path { path: PathBuf },
    /// A git checkout Manager has already materialized on disk. `rev` is the
    /// resolved commit, never a branch — a lockfile that pins a branch pins
    /// nothing.
    Git { url: String, rev: String, path: PathBuf },
    /// Reserved for M3. Parsed so a future lockfile does not fail to read on
    /// an older framework with a confusing TOML error.
    Registry { registry: String },
}

impl PackageSource {
    /// `resolved_from` in the request document (§11.4).
    pub fn as_str(&self) -> &'static str {
        match self {
            PackageSource::Path { .. } => "path",
            PackageSource::Git { .. } => "git",
            PackageSource::Registry { .. } => "registry",
        }
    }

    /// Where the package's files are, when they are on disk. `None` for a
    /// registry source, which M0 cannot fetch.
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            PackageSource::Path { path } => Some(path),
            PackageSource::Git { path, .. } => Some(path),
            PackageSource::Registry { .. } => None,
        }
    }
}

/// One resolved entry of the closure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,

    /// Library or plugin — see the module docs. Deliberately not defaulted.
    pub kind: PackageKind,

    #[serde(flatten)]
    pub source: PackageSource,

    /// Names of this package's own dependencies. The closure is already
    /// flattened by Manager into `[[package]]` entries; this records the edges
    /// so the framework can walk in dependency order without re-resolving.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

/// The parts of `.cln/lock.toml` the framework reads.
///
/// `[host]` is deliberately absent: it is read by [`crate::hostwit`] through
/// its own accessor, and duplicating it here would create two owners of one
/// section.
#[derive(Clone, Debug, Default)]
pub struct Lockfile {
    /// Keyed by package name, so a closure walk can follow `dependencies`
    /// edges by name without a linear scan.
    pub packages: BTreeMap<String, LockedPackage>,
}

/// The raw file shape, before validation. Separate from [`Lockfile`] so the
/// public type can be a map while the file stays an array of tables — the
/// natural TOML spelling and what Manager writes.
#[derive(Deserialize)]
struct RawLockfile {
    #[serde(default)]
    package: Vec<toml::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
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

    /// The distinction the framework cannot proceed without. See module docs.
    #[error("{}: package '{name}' has no `kind`", .path.display())]
    MissingKind { path: PathBuf, name: String },

    #[error("{}: package '{name}' has an unknown kind '{kind}'", .path.display())]
    UnknownKind { path: PathBuf, name: String, kind: String },

    #[error("{}: package '{name}' is malformed: {reason}", .path.display())]
    BadPackage { path: PathBuf, name: String, reason: String },

    #[error("{}: two packages are both named '{name}'", .path.display())]
    DuplicatePackage { path: PathBuf, name: String },
}

impl LockError {
    /// Every variant is a lockfile the framework cannot act on. `CFG002` is
    /// the registry's "lockfile is missing or invalid" code — distinct from
    /// `CFG003` (clean.toml unreadable) so the message points at the right
    /// file and the right remedy.
    pub fn code(&self) -> &'static str {
        match self {
            LockError::Unreadable { .. } => "FRM002",
            _ => "CFG002",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            // Every one of these means the lockfile disagrees with what this
            // framework can read. Re-resolving is the remedy in all cases, and
            // it is Manager's job — the framework never edits `[[package]]`.
            LockError::Malformed { .. }
            | LockError::MissingKind { .. }
            | LockError::UnknownKind { .. }
            | LockError::BadPackage { .. }
            | LockError::DuplicatePackage { .. } => {
                Some("run `cln fetch` to regenerate .cln/lock.toml".into())
            }
            LockError::Unreadable { .. } => {
                Some("check that .cln/ is readable".into())
            }
        }
    }

    pub fn path(&self) -> &PathBuf {
        match self {
            LockError::Unreadable { path, .. }
            | LockError::Malformed { path, .. }
            | LockError::MissingKind { path, .. }
            | LockError::UnknownKind { path, .. }
            | LockError::BadPackage { path, .. }
            | LockError::DuplicatePackage { path, .. } => path,
        }
    }
}

impl Lockfile {
    /// Read `<project_root>/.cln/lock.toml`.
    ///
    /// A missing lockfile is `Ok(None)`, not an error: a project that has
    /// never resolved is the normal state before the first `cln fetch`, and
    /// distinguishing "absent" from "empty" is what lets the caller decide
    /// whether to trigger a resolve (§00.8). A lockfile that *exists* but does
    /// not parse is always an error — treating it as absent would silently
    /// build against no dependencies at all.
    pub fn load(project_root: &Path) -> Result<Option<Self>, LockError> {
        let path = project_root.join(LOCKFILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(LockError::Unreadable { path, source }),
        };
        Self::parse(&text, &path).map(Some)
    }

    /// Parse lockfile text. `path` is used only for error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, LockError> {
        let raw: RawLockfile = toml::from_str(text)
            .map_err(|source| LockError::Malformed { path: path.to_path_buf(), source })?;

        let mut packages = BTreeMap::new();
        for entry in raw.package {
            let package = parse_package(entry, path)?;
            if let Some(existing) = packages.insert(package.name.clone(), package) {
                // Two entries for one name means the closure is ambiguous:
                // whichever we kept would decide the build, silently.
                return Err(LockError::DuplicatePackage {
                    path: path.to_path_buf(),
                    name: existing.name,
                });
            }
        }

        Ok(Lockfile { packages })
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&LockedPackage> {
        self.packages.get(name)
    }

    /// Every entry of the given kind, in name order.
    pub fn of_kind(&self, kind: PackageKind) -> impl Iterator<Item = &LockedPackage> {
        self.packages.values().filter(move |p| p.kind == kind)
    }
}

/// Validate one `[[package]]` table.
///
/// Hand-rolled rather than `#[derive(Deserialize)]` on [`LockedPackage`]
/// because serde's error for a missing `kind` is "missing field `kind`" with
/// no package name attached — and in a lockfile with thirty entries, the name
/// is the only part of that message that helps.
fn parse_package(value: toml::Value, path: &Path) -> Result<LockedPackage, LockError> {
    let bad = |name: &str, reason: &str| LockError::BadPackage {
        path: path.to_path_buf(),
        name: name.to_string(),
        reason: reason.to_string(),
    };

    let table = value.as_table().ok_or_else(|| bad("<unnamed>", "not a table"))?;

    let name = table
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad("<unnamed>", "missing `name`"))?
        .to_string();

    let version = table
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| bad(&name, "missing `version`"))?
        .to_string();

    let kind = match table.get("kind").and_then(|v| v.as_str()) {
        Some("library") => PackageKind::Library,
        Some("plugin") => PackageKind::Plugin,
        Some(other) => {
            return Err(LockError::UnknownKind {
                path: path.to_path_buf(),
                name,
                kind: other.to_string(),
            })
        }
        None => return Err(LockError::MissingKind { path: path.to_path_buf(), name }),
    };

    let source = parse_source(table, &name, path)?;

    let dependencies = match table.get("dependencies") {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad(&name, "`dependencies` must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(bad(&name, "`dependencies` must be an array")),
    };

    Ok(LockedPackage { name, version, kind, source, dependencies })
}

/// Determine the source from which keys are present.
///
/// The keys are mutually exclusive by construction: Manager writes exactly one
/// of `path`, `git`, or `registry` per entry. An entry with none of them is
/// unresolved, which is precisely the state that must not reach a build.
fn parse_source(
    table: &toml::Table,
    name: &str,
    path: &Path,
) -> Result<PackageSource, LockError> {
    let bad = |reason: &str| LockError::BadPackage {
        path: path.to_path_buf(),
        name: name.to_string(),
        reason: reason.to_string(),
    };

    if let Some(git) = table.get("git").and_then(|v| v.as_str()) {
        // A git entry without a resolved rev pins nothing — the same lockfile
        // would produce different builds on different days.
        let rev = table
            .get("rev")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("a `git` package needs a resolved `rev`"))?;
        let checkout = table
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| bad("a `git` package needs the `path` of its checkout"))?;
        return Ok(PackageSource::Git {
            url: git.to_string(),
            rev: rev.to_string(),
            path: PathBuf::from(checkout),
        });
    }

    if let Some(local) = table.get("path").and_then(|v| v.as_str()) {
        return Ok(PackageSource::Path { path: PathBuf::from(local) });
    }

    if let Some(registry) = table.get("registry").and_then(|v| v.as_str()) {
        return Ok(PackageSource::Registry { registry: registry.to_string() });
    }

    Err(bad("no `path`, `git`, or `registry` source"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> PathBuf {
        PathBuf::from(".cln/lock.toml")
    }

    fn parse(text: &str) -> Result<Lockfile, LockError> {
        Lockfile::parse(text, &at())
    }

    const TWO_PACKAGES: &str = r#"
[[package]]
name = "frame.data"
version = "2.1.2"
kind = "library"
path = "vendor/frame.data"

[[package]]
name = "frame.ui"
version = "0.4.0"
kind = "plugin"
path = "vendor/frame.ui"
"#;

    #[test]
    fn reads_libraries_and_plugins_apart() {
        // The whole reason `kind` exists: these two entries look identical
        // apart from it, and the framework does completely different things
        // with them.
        let lock = parse(TWO_PACKAGES).unwrap();
        assert_eq!(lock.packages.len(), 2);
        assert_eq!(lock.get("frame.data").unwrap().kind, PackageKind::Library);
        assert_eq!(lock.get("frame.ui").unwrap().kind, PackageKind::Plugin);

        let libraries: Vec<_> = lock.of_kind(PackageKind::Library).map(|p| &p.name).collect();
        assert_eq!(libraries, vec!["frame.data"]);
    }

    #[test]
    fn a_package_without_a_kind_is_refused_not_guessed() {
        // Defaulting would pick a compile strategy by coin-flip; the failure
        // would then surface as a missing-handler error from the compiler.
        let err = parse(
            "[[package]]\nname = \"frame.data\"\nversion = \"2.1.2\"\npath = \"vendor/x\"\n",
        )
        .unwrap_err();
        assert!(matches!(err, LockError::MissingKind { .. }));
        assert!(err.to_string().contains("frame.data"), "must name it: {err}");
        assert_eq!(err.code(), "CFG002");
    }

    #[test]
    fn an_unknown_kind_names_the_package_and_the_kind() {
        let err = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"widget\"\npath = \"v\"\n",
        )
        .unwrap_err();
        assert!(matches!(err, LockError::UnknownKind { .. }));
        assert!(err.to_string().contains("widget"), "got {err}");
        assert!(err.to_string().contains("'x'"), "got {err}");
    }

    #[test]
    fn a_missing_lockfile_is_absent_not_an_error() {
        // The normal state before the first `cln fetch`. The caller decides
        // whether to trigger a resolve; that decision needs `None`, not `[]`.
        let dir = tempfile::tempdir().unwrap();
        assert!(Lockfile::load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn an_empty_lockfile_is_present_and_empty() {
        // Distinct from absent: this project resolved, and resolved to nothing.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
        std::fs::write(dir.path().join(LOCKFILE), "").unwrap();
        let lock = Lockfile::load(dir.path()).unwrap().unwrap();
        assert!(lock.is_empty());
    }

    #[test]
    fn a_corrupt_lockfile_is_an_error_not_an_empty_closure() {
        // Treating it as empty would silently build against no dependencies.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
        std::fs::write(dir.path().join(LOCKFILE), "[[package]\nname =").unwrap();
        let err = Lockfile::load(dir.path()).unwrap_err();
        assert!(matches!(err, LockError::Malformed { .. }));
    }

    #[test]
    fn the_host_pin_does_not_look_like_a_package() {
        // hostwit writes `[host.<name>]` into this same file. Reading it must
        // not produce a phantom package, and must not fail.
        let lock = parse(
            "[host.clean-cli]\nversion = \"0.1.0\"\nsha256 = \"abc\"\n\n\
             [[package]]\nname = \"frame.data\"\nversion = \"2.1.2\"\n\
             kind = \"library\"\npath = \"v\"\n",
        )
        .unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert!(lock.get("frame.data").is_some());
    }

    #[test]
    fn git_sources_need_a_resolved_rev() {
        // A branch name pins nothing: the same lockfile would produce
        // different builds on different days.
        let err = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
             git = \"https://example.test/x.git\"\npath = \"vendor/x\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("rev"), "got {err}");
    }

    #[test]
    fn git_sources_carry_url_rev_and_checkout() {
        let lock = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
             git = \"https://example.test/x.git\"\nrev = \"a1b2c3\"\npath = \"vendor/x\"\n",
        )
        .unwrap();
        let package = lock.get("x").unwrap();
        assert_eq!(package.source.as_str(), "git");
        assert_eq!(package.source.local_path(), Some(Path::new("vendor/x")));
    }

    #[test]
    fn a_package_with_no_source_is_refused() {
        // An unresolved entry must not reach a build.
        let err = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"library\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("source"), "got {err}");
    }

    #[test]
    fn a_registry_source_parses_but_has_no_local_path() {
        // M3 shape. Parsing it now means a newer lockfile fails at the closure
        // walk with a real message rather than as a TOML error.
        let lock = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
             registry = \"clean\"\n",
        )
        .unwrap();
        assert_eq!(lock.get("x").unwrap().source.local_path(), None);
    }

    #[test]
    fn duplicate_package_names_are_refused() {
        // Whichever entry we kept would silently decide the build.
        let err = parse(
            "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nkind = \"library\"\npath = \"a\"\n\
             [[package]]\nname = \"x\"\nversion = \"2.0.0\"\nkind = \"library\"\npath = \"b\"\n",
        )
        .unwrap_err();
        assert!(matches!(err, LockError::DuplicatePackage { .. }));
    }

    #[test]
    fn dependency_edges_are_recorded() {
        let lock = parse(
            "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nkind = \"library\"\npath = \"a\"\n\
             dependencies = [\"b\"]\n\
             [[package]]\nname = \"b\"\nversion = \"1.0.0\"\nkind = \"library\"\npath = \"b\"\n",
        )
        .unwrap();
        assert_eq!(lock.get("a").unwrap().dependencies, vec!["b"]);
        assert!(lock.get("b").unwrap().dependencies.is_empty());
    }

    #[test]
    fn manifest_file_follows_the_kind() {
        assert_eq!(PackageKind::Library.manifest_file(), "library.toml");
        assert_eq!(PackageKind::Plugin.manifest_file(), "plugin.toml");
    }
}

//! Steps 3 and 4 of §11.2 — walk the resolved closure and read each package's
//! own manifest.
//!
//! The framework does not *resolve* anything here. Manager already did that
//! and wrote the answer to `.cln/lock.toml`; this module reads that answer,
//! loads the `library.toml` beside each package's source, and projects the
//! result into the two request-document fields the compiler consumes:
//! `dependencies` (name → version + where it came from) and
//! `library_manifests[]` (the WIT each library exports, plus the blocks it
//! handles).
//!
//! # Why the walk is ordered, and why it must terminate
//!
//! `library_manifests[]` reaches the compiler as a list, and the request
//! document must be byte-identical for identical project state (CMP-02) — so
//! the order cannot depend on lockfile line order or filesystem iteration. It
//! is dependency order: a package appears after everything it depends on, and
//! ties break by name. That makes the list stable *and* meaningful.
//!
//! A dependency cycle would make that order impossible. Manager should never
//! write one, but "should never" is not a guarantee the framework can build
//! on — an unchecked walk would recurse until the stack ran out, and the user
//! would get a crash instead of a message naming the cycle.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use framework_compiler_driver::request::{Dependency, LibraryManifest as RequestLibrary};

use crate::library::{LibraryError, LibraryManifest};
use crate::lockfile::{LockError, LockedPackage, Lockfile, PackageKind, PackageSource};
use crate::plugin::{LoadedPlugin, PluginError, PluginManifest};

/// The closure, resolved and read, ready to lower.
#[derive(Clone, Debug, Default)]
pub struct ResolvedClosure {
    /// `dependencies` in the request document, keyed by name (a `BTreeMap`, so
    /// serialization order is name order regardless of walk order).
    pub dependencies: BTreeMap<String, Dependency>,

    /// `library_manifests[]`, in dependency order. See the module docs.
    pub libraries: Vec<RequestLibrary>,

    /// Plugin packages, validated against their own `plugin.wasm`, in
    /// dependency order alongside the libraries. Their declared paths extend
    /// discovery (§11.3 item 3, §11.4).
    pub plugins: Vec<LoadedPlugin>,
}

impl ResolvedClosure {
    pub fn is_empty(&self) -> bool {
        self.dependencies.is_empty() && self.libraries.is_empty() && self.plugins.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClosureError {
    #[error(transparent)]
    Lock(#[from] LockError),

    #[error(transparent)]
    Library(#[from] LibraryError),

    /// Boxed: `PluginError::ExportMissing` carries the full export list, and
    /// inlining it would grow every `Result` in the crate.
    #[error(transparent)]
    Plugin(#[from] Box<PluginError>),

    #[error(transparent)]
    Resolve(#[from] crate::resolver::ResolveError),

    /// The resolver reported success, but the lockfile still accounts for no
    /// dependencies. Naming it here beats letting the build continue and
    /// surface it as unresolved imports with no mention of resolution.
    #[error(
        "{} still resolves no dependencies for {}",
        crate::lockfile::LOCKFILE,
        .project_root.display()
    )]
    NotResolved { project_root: PathBuf },

    /// The lockfile names a package whose files are not where it says.
    #[error("dependency '{name}' is not at {}", .path.display())]
    Missing { name: String, path: PathBuf },

    /// An entry naming a dependency that has no `[[package]]` of its own. The
    /// closure Manager wrote is incomplete — building would silently omit it.
    #[error("dependency '{missing}' (required by '{required_by}') is not in the lockfile")]
    Dangling { missing: String, required_by: String },

    #[error("dependency cycle: {}", .cycle.join(" -> "))]
    Cycle { cycle: Vec<String> },

    /// M3 shape reaching an M0/M1 framework.
    #[error("dependency '{name}' comes from a registry, which is not supported yet")]
    RegistryUnsupported { name: String },
}

/// Box on the way in, so call sites keep writing `?` against a bare
/// `PluginError` while the enum itself stays small.
impl From<PluginError> for ClosureError {
    fn from(error: PluginError) -> Self {
        ClosureError::Plugin(Box::new(error))
    }
}

impl ClosureError {
    pub fn code(&self) -> &'static str {
        match self {
            ClosureError::Lock(e) => e.code(),
            ClosureError::Library(e) => e.code(),
            // All of these mean the lockfile does not describe a buildable
            // closure — the same class of failure as a malformed lockfile.
            ClosureError::Missing { .. }
            | ClosureError::Dangling { .. }
            | ClosureError::Cycle { .. }
            | ClosureError::RegistryUnsupported { .. } => "CFG002",
            ClosureError::Plugin(e) => e.code(),
            ClosureError::Resolve(e) => e.code(),
            // The resolver ran and produced nothing: a resolution failure, not
            // a bad lockfile — there is no lockfile to be bad.
            ClosureError::NotResolved { .. } => "FRM006",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            ClosureError::Lock(e) => e.help(),
            ClosureError::Library(e) => e.help(),
            ClosureError::Missing { .. } => {
                Some("run `cln fetch` to re-materialize the dependency".into())
            }
            ClosureError::Dangling { .. } => {
                Some("run `cln fetch` to regenerate a complete .cln/lock.toml".into())
            }
            // Framework cannot fix this: the edges come from the packages'
            // own manifests, so the fix is in one of those manifests.
            ClosureError::Cycle { .. } => {
                Some("break the cycle by removing one of the dependencies".into())
            }
            ClosureError::RegistryUnsupported { .. } => {
                Some("use a path or git dependency until registry support lands".into())
            }
            ClosureError::Plugin(e) => e.help(),
            ClosureError::Resolve(e) => e.help(),
            ClosureError::NotResolved { .. } => {
                Some("run `cln fetch` and check that it writes .cln/lock.toml".into())
            }
        }
    }
}

/// Read the closure for a project.
///
/// `Ok(None)` when there is no lockfile at all — the caller decides whether
/// that warrants triggering a resolve (§00.8). An empty-but-present lockfile
/// yields an empty closure, which is a different thing: the project resolved,
/// and resolved to nothing.
pub fn resolve(project_root: &Path) -> Result<Option<ResolvedClosure>, ClosureError> {
    let Some(lockfile) = Lockfile::load(project_root)? else {
        return Ok(None);
    };
    read_closure(project_root, &lockfile).map(Some)
}

/// Read every package in `lockfile` from disk and project it.
pub fn read_closure(
    project_root: &Path,
    lockfile: &Lockfile,
) -> Result<ResolvedClosure, ClosureError> {
    let order = walk_order(lockfile)?;

    let mut closure = ResolvedClosure::default();

    for name in &order {
        let package = lockfile
            .get(name)
            .expect("walk_order only yields names present in the lockfile");

        closure.dependencies.insert(
            package.name.clone(),
            Dependency {
                version: package.version.clone(),
                resolved_from: package.source.as_str().to_string(),
            },
        );

        match package.kind {
            PackageKind::Library => {
                let root = package_root(project_root, package)?;
                let manifest = LibraryManifest::load(&root)?;
                closure.libraries.push(manifest.into_request_entry());
            }
            // Read and checked against its own WASM (FRM-PM-01..03) before it
            // can extend discovery. A plugin whose manifest disagrees with its
            // bytes must not get to add roots to the compilation.
            PackageKind::Plugin => {
                let root = package_root(project_root, package)?;
                let loaded = PluginManifest::load(&root)?;

                // A plugin participates in the compilation, so its bytes are
                // part of build identity: rebuilding a plugin without touching
                // a single source file must not be served a cached component
                // built against the old one (§11.7).
                //
                // It rides in `library_manifests[]` because that is the only
                // slot the request document has for a dependency's own
                // manifest — a new top-level key would be an `RQD002` hard
                // error. `wit` is empty (a plugin publishes no Clean
                // interface) and `compiletime_wasm_sha256` carries the hash,
                // which is exactly what that field means: the WASM this
                // dependency contributes at compile time.
                closure.libraries.push(RequestLibrary {
                    name: loaded.name().to_string(),
                    version: loaded.manifest.plugin.version.clone(),
                    wit: String::new(),
                    handles_blocks: Vec::new(),
                    compiletime_wasm_sha256: Some(loaded.wasm_sha256.clone()),
                });

                closure.plugins.push(loaded);
            }
        }
    }

    Ok(closure)
}

/// Where a package's files are, verified to exist.
fn package_root(
    project_root: &Path,
    package: &LockedPackage,
) -> Result<PathBuf, ClosureError> {
    let relative = match &package.source {
        PackageSource::Registry { .. } => {
            return Err(ClosureError::RegistryUnsupported { name: package.name.clone() })
        }
        source => source
            .local_path()
            .expect("only a registry source lacks a local path"),
    };

    // An absolute path in a lockfile is unusual but legal — Manager may
    // materialize a git checkout outside the project.
    let root = if relative.is_absolute() {
        relative.to_path_buf()
    } else {
        project_root.join(relative)
    };

    if !root.is_dir() {
        return Err(ClosureError::Missing { name: package.name.clone(), path: root });
    }

    Ok(root)
}

/// Package names in dependency order: every package after everything it
/// depends on, ties broken by name.
///
/// An iterative depth-first post-order rather than the obvious recursion —
/// a deep closure must not be able to blow the stack, and the explicit stack
/// is what makes the cycle path reportable.
fn walk_order(lockfile: &Lockfile) -> Result<Vec<String>, ClosureError> {
    /// What the traversal is doing with a node when it is popped.
    enum Step<'a> {
        /// Descend into this node's dependencies.
        Enter(&'a str),
        /// Every dependency is done; emit it.
        Emit(&'a str),
    }

    let mut order = Vec::with_capacity(lockfile.packages.len());
    let mut done: BTreeSet<&str> = BTreeSet::new();
    // The nodes on the current root-to-here path. Membership here (rather than
    // in `done`) is what distinguishes a cycle from a diamond: a package
    // reached twice by different paths is fine, a package reached from inside
    // its own subtree is not.
    let mut on_path: Vec<&str> = Vec::new();

    // Roots in name order, so the whole traversal is deterministic.
    for root in lockfile.packages.keys() {
        if done.contains(root.as_str()) {
            continue;
        }

        let mut stack = vec![Step::Enter(root.as_str())];

        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(name) => {
                    if done.contains(name) {
                        continue;
                    }
                    if on_path.contains(&name) {
                        // Report the cycle as the path that closes it, so the
                        // message names the packages the user must look at.
                        let start = on_path.iter().position(|n| *n == name).unwrap_or(0);
                        let mut cycle: Vec<String> =
                            on_path[start..].iter().map(|s| s.to_string()).collect();
                        cycle.push(name.to_string());
                        return Err(ClosureError::Cycle { cycle });
                    }

                    let package = lockfile.get(name).expect("caller verified presence");

                    on_path.push(name);
                    stack.push(Step::Emit(name));

                    // Reversed: the stack pops last-pushed-first, so pushing in
                    // reverse name order visits in name order.
                    let mut deps: Vec<&str> =
                        package.dependencies.iter().map(String::as_str).collect();
                    deps.sort_unstable();
                    for dep in deps.into_iter().rev() {
                        // Resolve the name now so a dangling edge is reported
                        // against the package that declares it.
                        if lockfile.get(dep).is_none() {
                            return Err(ClosureError::Dangling {
                                missing: dep.to_string(),
                                required_by: name.to_string(),
                            });
                        }
                        stack.push(Step::Enter(dep));
                    }
                }
                Step::Emit(name) => {
                    on_path.pop();
                    if done.insert(name) {
                        order.push(name.to_string());
                    }
                }
            }
        }
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::EMPTY_MODULE;

    /// Build a lockfile from `(name, kind, deps)` triples, materializing each
    /// package directory so `package_root` finds it.
    fn project(
        packages: &[(&str, PackageKind, &[&str])],
    ) -> (tempfile::TempDir, Lockfile) {
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::new();

        for (name, kind, deps) in packages {
            let root = dir.path().join("vendor").join(name);
            std::fs::create_dir_all(&root).unwrap();

            match kind {
                PackageKind::Library => std::fs::write(
                    root.join("library.toml"),
                    format!(
                        "[library]\nname = \"{name}\"\nversion = \"1.0.0\"\n\
                         [exports]\nwit = \"interface {} {{}}\"\n",
                        name.replace(['.', '-'], "_")
                    ),
                )
                .unwrap(),

                // A plugin is only a plugin if both files are there — the
                // closure walk validates it (FRM-PM-01..03).
                PackageKind::Plugin => {
                    std::fs::write(
                        root.join("plugin.toml"),
                        format!("[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n"),
                    )
                    .unwrap();
                    std::fs::write(root.join("plugin.wasm"), EMPTY_MODULE).unwrap();
                }
            }

            let deps_line = if deps.is_empty() {
                String::new()
            } else {
                let quoted: Vec<String> = deps.iter().map(|d| format!("\"{d}\"")).collect();
                format!("dependencies = [{}]\n", quoted.join(", "))
            };

            text.push_str(&format!(
                "[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\nkind = \"{}\"\n\
                 path = \"vendor/{name}\"\n{deps_line}\n",
                kind.as_str()
            ));
        }

        let lockfile = Lockfile::parse(&text, Path::new(".cln/lock.toml")).unwrap();
        (dir, lockfile)
    }

    #[test]
    fn dependencies_come_before_the_packages_that_need_them() {
        // `app` needs `mid`, `mid` needs `base`. The compiler sees a library
        // only after everything it builds on.
        let (dir, lock) = project(&[
            ("app", PackageKind::Library, &["mid"]),
            ("mid", PackageKind::Library, &["base"]),
            ("base", PackageKind::Library, &[]),
        ]);
        let closure = read_closure(dir.path(), &lock).unwrap();
        let names: Vec<&str> = closure.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["base", "mid", "app"]);
    }

    #[test]
    fn the_order_does_not_depend_on_lockfile_line_order() {
        // CMP-02: identical project state must produce a byte-identical
        // request. Two lockfiles listing the same closure in different orders
        // are the same project state.
        let forward = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
                       path = \"vendor/a\"\ndependencies = [\"b\"]\n\
                       [[package]]\nname = \"b\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
                       path = \"vendor/b\"\n";
        let backward = "[[package]]\nname = \"b\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
                        path = \"vendor/b\"\n\
                        [[package]]\nname = \"a\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
                        path = \"vendor/a\"\ndependencies = [\"b\"]\n";

        let at = Path::new(".cln/lock.toml");
        let (dir, _) = project(&[
            ("a", PackageKind::Library, &["b"]),
            ("b", PackageKind::Library, &[]),
        ]);

        let one = read_closure(dir.path(), &Lockfile::parse(forward, at).unwrap()).unwrap();
        let two = read_closure(dir.path(), &Lockfile::parse(backward, at).unwrap()).unwrap();

        let names = |c: &ResolvedClosure| -> Vec<String> {
            c.libraries.iter().map(|l| l.name.clone()).collect()
        };
        assert_eq!(names(&one), names(&two));
        assert_eq!(names(&one), vec!["b", "a"]);
    }

    #[test]
    fn a_diamond_yields_each_package_once() {
        // `top` needs both `left` and `right`; both need `base`. `base` is
        // reached twice and must appear once — a duplicate would be handed to
        // the compiler as two libraries with the same name.
        let (dir, lock) = project(&[
            ("top", PackageKind::Library, &["left", "right"]),
            ("left", PackageKind::Library, &["base"]),
            ("right", PackageKind::Library, &["base"]),
            ("base", PackageKind::Library, &[]),
        ]);
        let closure = read_closure(dir.path(), &lock).unwrap();
        let names: Vec<&str> = closure.libraries.iter().map(|l| l.name.as_str()).collect();

        assert_eq!(names.len(), 4, "each package exactly once: {names:?}");
        let base = names.iter().position(|n| *n == "base").unwrap();
        for dependent in ["left", "right", "top"] {
            assert!(
                base < names.iter().position(|n| *n == dependent).unwrap(),
                "base must precede {dependent} in {names:?}"
            );
        }
    }

    #[test]
    fn a_cycle_names_the_packages_involved() {
        // Manager should never write one. An unchecked walk would recurse
        // until the stack ran out and the user would get a crash.
        let (dir, lock) = project(&[
            ("a", PackageKind::Library, &["b"]),
            ("b", PackageKind::Library, &["a"]),
        ]);
        let err = read_closure(dir.path(), &lock).unwrap_err();
        assert!(matches!(err, ClosureError::Cycle { .. }), "got {err}");
        assert!(err.to_string().contains('a') && err.to_string().contains('b'), "got {err}");
    }

    #[test]
    fn a_package_depending_on_itself_is_a_cycle() {
        let (dir, lock) = project(&[("a", PackageKind::Library, &["a"])]);
        let err = read_closure(dir.path(), &lock).unwrap_err();
        assert!(matches!(err, ClosureError::Cycle { .. }), "got {err}");
    }

    #[test]
    fn an_edge_to_a_package_that_is_not_locked_is_refused() {
        // Building would silently omit it, and the failure would surface as an
        // unresolved import with no mention of the lockfile.
        let (dir, _) = project(&[("a", PackageKind::Library, &[])]);
        let lock = Lockfile::parse(
            "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
             path = \"vendor/a\"\ndependencies = [\"ghost\"]\n",
            Path::new(".cln/lock.toml"),
        )
        .unwrap();

        let err = read_closure(dir.path(), &lock).unwrap_err();
        assert!(matches!(err, ClosureError::Dangling { .. }), "got {err}");
        assert!(err.to_string().contains("ghost"), "got {err}");
        assert!(err.to_string().contains('a'), "must name who needs it: {err}");
    }

    #[test]
    fn a_package_missing_from_disk_names_the_path() {
        let (dir, lock) = project(&[("a", PackageKind::Library, &[])]);
        std::fs::remove_dir_all(dir.path().join("vendor/a")).unwrap();

        let err = read_closure(dir.path(), &lock).unwrap_err();
        assert!(matches!(err, ClosureError::Missing { .. }), "got {err}");
        assert!(err.to_string().contains("vendor/a"), "got {err}");
    }

    #[test]
    fn a_plugin_reaches_the_compiler_by_its_wasm_hash_not_its_wit() {
        // A plugin publishes no Clean interface, so it has no `wit` — but its
        // bytes participate in the compilation, so they must reach the request
        // document or a rebuilt plugin would produce an identical one.
        let (dir, lock) = project(&[
            ("frame.data", PackageKind::Library, &[]),
            ("frame.ui", PackageKind::Plugin, &[]),
        ]);
        let closure = read_closure(dir.path(), &lock).unwrap();

        assert_eq!(closure.plugins.len(), 1);
        assert_eq!(closure.plugins[0].name(), "frame.ui");

        let entry = closure
            .libraries
            .iter()
            .find(|l| l.name == "frame.ui")
            .expect("the plugin must reach library_manifests[]");
        assert!(entry.wit.is_empty(), "a plugin publishes no Clean interface");
        assert_eq!(
            entry.compiletime_wasm_sha256.as_deref().map(str::len),
            Some(64),
            "the plugin's bytes must be part of build identity"
        );

        // The library keeps its own shape: WIT, and no compiled handler yet.
        let library = closure.libraries.iter().find(|l| l.name == "frame.data").unwrap();
        assert!(!library.wit.is_empty());
        assert_eq!(library.compiletime_wasm_sha256, None, "Phase 3 fills this");

        // Both are dependencies of the project regardless of kind.
        assert_eq!(closure.dependencies.len(), 2);
    }

    #[test]
    fn dependencies_record_where_each_package_came_from() {
        let (dir, lock) = project(&[("a", PackageKind::Library, &[])]);
        let closure = read_closure(dir.path(), &lock).unwrap();
        let dependency = closure.dependencies.get("a").unwrap();
        assert_eq!(dependency.version, "1.0.0");
        assert_eq!(dependency.resolved_from, "path");
    }

    #[test]
    fn a_registry_dependency_is_refused_with_a_real_message() {
        let dir = tempfile::tempdir().unwrap();
        let lock = Lockfile::parse(
            "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
             registry = \"clean\"\n",
            Path::new(".cln/lock.toml"),
        )
        .unwrap();

        let err = read_closure(dir.path(), &lock).unwrap_err();
        assert!(matches!(err, ClosureError::RegistryUnsupported { .. }), "got {err}");
        assert!(err.help().unwrap().contains("path"), "got {err:?}");
    }

    #[test]
    fn no_lockfile_is_none_and_an_empty_one_is_an_empty_closure() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(dir.path()).unwrap().is_none());

        std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
        std::fs::write(dir.path().join(crate::lockfile::LOCKFILE), "").unwrap();
        assert!(resolve(dir.path()).unwrap().unwrap().is_empty());
    }

    #[test]
    fn a_deep_chain_does_not_exhaust_the_stack() {
        // The reason the walk is iterative. A recursive one dies here.
        let names: Vec<String> = (0..2000).map(|i| format!("p{i:04}")).collect();
        let mut text = String::new();
        let dir = tempfile::tempdir().unwrap();

        for (i, name) in names.iter().enumerate() {
            let root = dir.path().join("vendor").join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("library.toml"),
                format!("[library]\nname = \"{name}\"\nversion = \"1.0.0\"\n"),
            )
            .unwrap();

            let deps = match names.get(i + 1) {
                Some(next) => format!("dependencies = [\"{next}\"]\n"),
                None => String::new(),
            };
            text.push_str(&format!(
                "[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\nkind = \"library\"\n\
                 path = \"vendor/{name}\"\n{deps}\n"
            ));
        }

        let lock = Lockfile::parse(&text, Path::new(".cln/lock.toml")).unwrap();
        let closure = read_closure(dir.path(), &lock).unwrap();
        assert_eq!(closure.libraries.len(), 2000);
        // Deepest first.
        assert_eq!(closure.libraries[0].name, "p1999");
    }
}

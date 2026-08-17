//! File discovery — §11.3, rules FRM-BO-03 through FRM-BO-07.
//!
//! Roots #2 (`app/`) and #3 (plugin-declared `[paths].owns`, added by
//! [`DiscoveryPlan::with_plugins`]). Root #1 (`[folders]` keys) still needs a
//! glob matcher and is not wired up; the walk takes a root list, so it is an
//! added entry rather than a rewrite.
//!
//! The rules this module is accountable for:
//!
//! - **FRM-BO-03** roots, in declared order, overlaps read once, missing roots
//!   skipped silently.
//! - **FRM-BO-04** extensions: `.cln`, plus whatever patterns plugins declare.
//! - **FRM-BO-05** excludes: dot-directories, `dist/`, `target/`,
//!   `node_modules/`, `.build/` at any depth, plus `[build].exclude`.
//!   `.gitignore` is deliberately not honoured.
//! - **FRM-BO-06** order: sort by project-relative POSIX path, byte-wise.
//! - **FRM-BO-07** encoding: UTF-8 or `CFG005`, no fallback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use framework_compiler_driver::artifact::hex_sha256;
use framework_compiler_driver::request::Source;

use crate::errors::{DiscoveryError, FrameworkError};
use crate::glob::Glob;

/// The compiler's own extension (FRM-BO-04). Plugins add patterns in Phase 4.
pub const CLN_EXTENSION: &str = "cln";

/// Directory names excluded at any depth (FRM-BO-05).
pub const EXCLUDED_DIRS: &[&str] = &["dist", "target", "node_modules", ".build"];

/// Inputs to a discovery walk. Built by `build.rs` from the manifest.
#[derive(Clone, Debug)]
pub struct DiscoveryPlan {
    pub project_root: PathBuf,
    /// Project-relative roots, in FRM-BO-03 order.
    pub roots: Vec<PathBuf>,
    /// Accepted file extensions, without the dot.
    pub extensions: Vec<String>,
    /// Extra paths to skip, from `[build].exclude` (FRM-BO-05). Globs.
    pub excludes: Vec<Glob>,
}

impl DiscoveryPlan {
    /// The base plan: `app/` only, `.cln` only.
    pub fn m0(project_root: impl Into<PathBuf>, excludes: Vec<String>) -> Self {
        DiscoveryPlan {
            project_root: project_root.into(),
            roots: vec![PathBuf::from("app")],
            extensions: vec![CLN_EXTENSION.to_string()],
            excludes: excludes.iter().map(|e| Glob::new(e)).collect(),
        }
    }

    /// Add the roots named by `[folders]` keys — root #1 of FRM-BO-03.
    ///
    /// `[folders]` maps a path glob to the libraries in scope for files under
    /// it. That mapping is the compiler's business (it travels in the request
    /// document verbatim), but it also *names folders the project compiles*,
    /// which is discovery's business: a project whose only sources live under
    /// a `[folders]` key must not build empty.
    ///
    /// Each pattern contributes its **literal prefix** as a walk root —
    /// `app/server/**` walks `app/server`, `**/model.cln` walks the project
    /// root. Walking the fixed part visits only the subtree that can match,
    /// rather than the whole project, and the per-file exclude rules still
    /// apply to everything found there.
    ///
    /// Roots come after `app/` for the same reason plugin roots do: FRM-BO-03
    /// reads overlapping roots once, first declaration winning, and the
    /// project's own source must not be shadowed.
    pub fn with_folders<'a>(mut self, patterns: impl IntoIterator<Item = &'a str>) -> Self {
        for pattern in patterns {
            // An empty literal prefix means the pattern has no fixed part
            // (`**/model.cln`), so the only honest root is the project itself.
            // `PathBuf::new()` joins to the project root unchanged, and the
            // exclude rules keep `dist/`, `target/` and dot-directories out of
            // that walk exactly as they would anywhere else.
            let root = PathBuf::from(Glob::new(pattern).literal_prefix());

            if !self.roots.contains(&root) {
                self.roots.push(root);
            }
        }
        self
    }

    /// Add the roots and extensions the closure's plugins declare (§11.3
    /// FRM-BO-03 item 3, §11.4).
    ///
    /// Plugin roots come **after** `app/` because FRM-BO-03 makes overlapping
    /// roots read once with the first declaration winning: the project's own
    /// source must not be shadowed by a dependency that claims the same
    /// folder.
    ///
    /// Duplicates are dropped rather than refused. Two plugins owning `ui/`,
    /// or one declaring an extension the compiler already handles, is
    /// harmless — the walk reads each file once either way — and refusing it
    /// would break a build over a coincidence between two dependencies the
    /// developer does not control.
    pub fn with_plugins(mut self, plugins: &[crate::plugin::LoadedPlugin]) -> Self {
        for plugin in plugins {
            for root in plugin.owned_roots() {
                if !self.roots.contains(root) {
                    self.roots.push(root.clone());
                }
            }
            for pattern in plugin.patterns() {
                if !self.extensions.contains(pattern) {
                    self.extensions.push(pattern.clone());
                }
            }
        }
        self
    }
}

/// Walk the plan's roots and return `sources[]`, sorted and hashed, ready to
/// drop into the request document.
pub fn discover(plan: &DiscoveryPlan) -> Result<Vec<Source>, FrameworkError> {
    // Keyed by relative POSIX path: gives FRM-BO-06 ordering for free, and
    // makes "overlapping roots are read once, first declaration wins" a
    // property of the container rather than a hand-rolled dedup.
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();

    for root in &plan.roots {
        let absolute = plan.project_root.join(root);
        if !absolute.is_dir() {
            // FRM-BO-03: roots that do not exist are skipped silently.
            continue;
        }
        walk(&absolute, plan, &mut found)?;
    }

    if found.is_empty() {
        return Err(DiscoveryError::NoSources {
            searched: plan.roots.iter().map(|r| to_posix(r)).collect(),
        }
        .into());
    }

    found
        .into_iter()
        .map(|(path, absolute)| read_source(path, &absolute))
        .collect()
}

fn walk(
    dir: &Path,
    plan: &DiscoveryPlan,
    found: &mut BTreeMap<String, PathBuf>,
) -> Result<(), FrameworkError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| DiscoveryError::Unreadable { path: dir.to_path_buf(), source })?;

    for entry in entries {
        let entry = entry
            .map_err(|source| DiscoveryError::Unreadable { path: dir.to_path_buf(), source })?;
        let path = entry.path();

        let name = entry.file_name();
        let name = name.to_string_lossy();

        let file_type = entry.file_type().map_err(|source| DiscoveryError::Unreadable {
            path: path.clone(),
            source,
        })?;

        // Symlinks are not followed: a link out of the project (or back into
        // it) would break both the determinism invariant and the "sources are
        // project-relative" contract.
        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            if is_excluded_dir(&name) {
                continue;
            }
            if let Some(relative) = relative_posix(&plan.project_root, &path) {
                if is_user_excluded(&relative, &plan.excludes) {
                    continue;
                }
            }
            walk(&path, plan, found)?;
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        if !matches_extension(&path, &plan.extensions) {
            // FRM-BO-04: files not matching any pattern are ignored, not errored.
            continue;
        }

        let Some(relative) = relative_posix(&plan.project_root, &path) else {
            continue;
        };
        if is_user_excluded(&relative, &plan.excludes) {
            continue;
        }

        // First declaration wins (FRM-BO-03).
        found.entry(relative).or_insert(path);
    }

    Ok(())
}

/// FRM-BO-07. Read as UTF-8 or refuse the build with `CFG005`.
fn read_source(relative: String, absolute: &Path) -> Result<Source, FrameworkError> {
    let bytes = std::fs::read(absolute)
        .map_err(|source| DiscoveryError::Unreadable { path: absolute.to_path_buf(), source })?;

    let content = String::from_utf8(bytes)
        .map_err(|_| DiscoveryError::NotUtf8 { path: absolute.to_path_buf() })?;

    // §11.5: the hash is of the *decoded* content, not the raw bytes. For
    // valid UTF-8 these are the same bytes, but stating it here is what keeps
    // it true if a decoding step is ever added.
    let sha256 = hex_sha256(content.as_bytes());

    Ok(Source { path: relative, sha256, content })
}

/// FRM-BO-05: any directory whose name starts with `.`, plus the four
/// unconditional names.
fn is_excluded_dir(name: &str) -> bool {
    name.starts_with('.') || EXCLUDED_DIRS.contains(&name)
}

/// `[build].exclude` entries (FRM-BO-05).
///
/// Matched as globs, so `**/*.test.cln` and `app/scratch` both work. Excluding
/// a directory excludes what is under it — `matches_prefix`, not `matches`:
/// a developer who excludes `app/scratch` means the folder, and having to
/// write `app/scratch/**` to be taken seriously is a trap.
fn is_user_excluded(relative: &str, excludes: &[Glob]) -> bool {
    excludes.iter().any(|exclude| exclude.matches_prefix(relative))
}

/// Does the file name end in one of the accepted extensions?
///
/// Matches on the file name's suffix rather than `Path::extension`, because a
/// plugin pattern may be multi-part: `Path::extension` on `button.ui.cln`
/// returns `cln`, so a plugin declaring `ui.cln` would never match and its own
/// files would be silently skipped.
///
/// The `.` before the suffix is required, so `cln` matches `main.cln` but not
/// a file literally named `cln` — and `ui.cln` matches `button.ui.cln` but not
/// `myui.cln`, which is a different name that merely ends the same way.
fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    extensions.iter().any(|wanted| {
        let suffix = format!(".{wanted}");
        name.len() > suffix.len() && name.ends_with(&suffix)
    })
}

/// Project-relative POSIX form. `None` when the path escapes the root, which
/// should be impossible from a walk rooted inside it.
fn relative_posix(project_root: &Path, path: &Path) -> Option<String> {
    Some(to_posix(path.strip_prefix(project_root).ok()?))
}

fn to_posix(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, body: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn plan(root: &Path) -> DiscoveryPlan {
        DiscoveryPlan::m0(root, Vec::new())
    }

    #[test]
    fn finds_cln_files_recursively_under_app() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "app/server/api.cln", b"get()\n");

        let sources = discover(&plan(dir.path())).unwrap();
        let paths: Vec<_> = sources.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["app/main.cln", "app/server/api.cln"]);
    }

    #[test]
    fn sorts_byte_wise_by_relative_path_frm_bo_06() {
        let dir = tempfile::tempdir().unwrap();
        // Written in an order that a naive walk would preserve.
        for name in ["app/zebra.cln", "app/Apple.cln", "app/beta.cln", "app/alpha/deep.cln"] {
            write(dir.path(), name, b"x\n");
        }
        let sources = discover(&plan(dir.path())).unwrap();
        let paths: Vec<_> = sources.iter().map(|s| s.path.clone()).collect();

        let mut expected = paths.clone();
        expected.sort();
        assert_eq!(paths, expected, "must be byte-wise sorted");
        // Byte-wise means uppercase sorts before lowercase.
        assert_eq!(paths[0], "app/Apple.cln");
    }

    #[test]
    fn ignores_non_cln_files_without_erroring_frm_bo_04() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "app/notes.md", b"# notes\n");
        write(dir.path(), "app/schema.sql", b"SELECT 1;\n");
        write(dir.path(), "app/no-extension", b"x\n");

        let sources = discover(&plan(dir.path())).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, "app/main.cln");
    }

    #[test]
    fn excludes_build_output_and_dotdirs_frm_bo_05() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        for excluded in [
            "app/dist/stale.cln",
            "app/target/stale.cln",
            "app/node_modules/dep/x.cln",
            "app/.build/x.cln",
            "app/.hidden/x.cln",
            "app/.cln/cached.cln",
        ] {
            write(dir.path(), excluded, b"nope\n");
        }

        let sources = discover(&plan(dir.path())).unwrap();
        assert_eq!(sources.len(), 1, "found {:?}", sources.iter().map(|s| &s.path).collect::<Vec<_>>());
    }

    #[test]
    fn honours_build_exclude_but_not_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "app/scratch/wip.cln", b"wip\n");
        // FRM-BO-05 is explicit: .gitignore is NOT honoured, so builds are
        // reproducible across contributors regardless of local VCS config.
        write(dir.path(), ".gitignore", b"app/scratch/\n");

        let all = discover(&plan(dir.path())).unwrap();
        assert_eq!(all.len(), 2, "gitignore must not affect discovery");

        let mut with_exclude = plan(dir.path());
        with_exclude.excludes = vec![Glob::new("app/scratch")];
        let filtered = discover(&with_exclude).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "app/main.cln");
    }

    #[test]
    fn missing_root_is_skipped_silently_frm_bo_03() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");

        let mut p = plan(dir.path());
        p.roots.push(PathBuf::from("does-not-exist"));
        assert_eq!(discover(&p).unwrap().len(), 1);
    }

    #[test]
    fn overlapping_roots_read_a_file_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/server/api.cln", b"get()\n");

        let mut p = plan(dir.path());
        p.roots = vec![PathBuf::from("app"), PathBuf::from("app/server")];
        let sources = discover(&p).unwrap();
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn non_utf8_is_cfg005_frm_bo_07() {
        let dir = tempfile::tempdir().unwrap();
        // 0xFF is not valid UTF-8 in any position.
        write(dir.path(), "app/main.cln", &[0x73, 0x74, 0xFF, 0x0A]);

        let err = discover(&plan(dir.path())).unwrap_err();
        assert_eq!(err.code(), "CFG005", "got {err}");
    }

    #[test]
    fn hashes_the_decoded_content() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"");
        let sources = discover(&plan(dir.path())).unwrap();
        assert_eq!(
            sources[0].sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn empty_project_reports_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        let err = discover(&plan(dir.path())).unwrap_err();
        assert!(err.to_string().contains("app"), "got {err}");
    }

    #[test]
    fn discovery_is_deterministic_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["app/b.cln", "app/a.cln", "app/c/d.cln"] {
            write(dir.path(), name, b"x\n");
        }
        assert_eq!(discover(&plan(dir.path())).unwrap(), discover(&plan(dir.path())).unwrap());
    }
}

#[cfg(test)]
mod plugin_extension_tests {
    use super::*;
    use crate::plugin::{LoadedPlugin, PluginManifest, EMPTY_MODULE};

    fn write(root: &Path, relative: &str, body: &[u8]) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// A loaded plugin declaring the given roots and patterns.
    fn plugin(dir: &Path, name: &str, owns: &[&str], patterns: &[&str]) -> LoadedPlugin {
        let root = dir.join("vendor").join(name);
        std::fs::create_dir_all(&root).unwrap();

        let list = |items: &[&str]| -> String {
            let quoted: Vec<String> = items.iter().map(|i| format!("\"{i}\"")).collect();
            quoted.join(", ")
        };

        std::fs::write(
            root.join("plugin.toml"),
            format!(
                "[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n\
                 [paths]\nowns = [{}]\npatterns = [{}]\n",
                list(owns),
                list(patterns)
            ),
        )
        .unwrap();
        std::fs::write(root.join("plugin.wasm"), EMPTY_MODULE).unwrap();

        PluginManifest::load(&root).unwrap()
    }

    #[test]
    fn a_plugin_owned_folder_becomes_a_discovery_root() {
        // FRM-BO-03 item 3: a plugin can own `ui/` and have its files compiled.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "ui/button.cln", b"button()\n");

        let plugins = [plugin(dir.path(), "frame.ui", &["ui"], &[])];
        let plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);

        let paths: Vec<String> = discover(&plan).unwrap().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["app/main.cln", "ui/button.cln"]);
    }

    #[test]
    fn a_multi_part_pattern_matches_the_files_it_names() {
        // `Path::extension` on `button.ui.cln` returns `cln`, so matching on
        // it alone would silently skip every file the plugin actually owns.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "app/button.ui.cln", b"button()\n");

        let plugins = [plugin(dir.path(), "frame.ui", &[], &["ui.cln"])];
        let plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);

        let paths: Vec<String> = discover(&plan).unwrap().into_iter().map(|s| s.path).collect();
        assert!(paths.contains(&"app/button.ui.cln".to_string()), "got {paths:?}");
    }

    #[test]
    fn a_pattern_matches_a_suffix_after_a_dot_not_a_bare_ending() {
        // `ui.cln` names files like `button.ui.cln`. `myui.cln` merely ends
        // the same way and belongs to nobody.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/button.ui.cln", b"a\n");
        write(dir.path(), "app/myui.cln", b"b\n");

        // Only the plugin's pattern, so `.cln` alone does not sweep both in.
        let plugins = [plugin(dir.path(), "frame.ui", &[], &["ui.cln"])];
        let mut plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);
        plan.extensions.retain(|e| e != CLN_EXTENSION);

        let paths: Vec<String> = discover(&plan).unwrap().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["app/button.ui.cln"]);
    }

    #[test]
    fn a_file_named_only_the_extension_is_not_a_source() {
        // A file literally named `cln` has no name of its own.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "app/cln", b"not a source\n");

        let paths: Vec<String> = discover(&DiscoveryPlan::m0(dir.path(), Vec::new()))
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(paths, ["app/main.cln"]);
    }

    #[test]
    fn the_projects_own_root_wins_over_a_plugin_claiming_it() {
        // FRM-BO-03: overlapping roots are read once, first declaration wins.
        // A dependency must not shadow the project's own source.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");

        let plugins = [plugin(dir.path(), "frame.ui", &["app"], &[])];
        let plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);

        assert_eq!(plan.roots, [PathBuf::from("app")], "app must appear once, first");
        assert_eq!(discover(&plan).unwrap().len(), 1, "the file must be read once");
    }

    #[test]
    fn two_plugins_claiming_the_same_folder_is_not_an_error() {
        // The developer does not control whether two dependencies coincide,
        // and the walk reads each file once either way.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");
        write(dir.path(), "ui/button.cln", b"button()\n");

        let plugins = [
            plugin(dir.path(), "frame.ui", &["ui"], &["ui.cln"]),
            plugin(dir.path(), "frame.widgets", &["ui"], &["ui.cln"]),
        ];
        let plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);

        assert_eq!(plan.roots, [PathBuf::from("app"), PathBuf::from("ui")]);
        assert_eq!(plan.extensions, [CLN_EXTENSION.to_string(), "ui.cln".to_string()]);
        assert_eq!(discover(&plan).unwrap().len(), 2);
    }

    #[test]
    fn a_plugin_owning_a_folder_that_does_not_exist_is_skipped_silently() {
        // FRM-BO-03: missing roots are skipped. A plugin that owns `ui/` in a
        // project that has no `ui/` yet is not an error.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "app/main.cln", b"start()\n");

        let plugins = [plugin(dir.path(), "frame.ui", &["ui"], &[])];
        let plan = DiscoveryPlan::m0(dir.path(), Vec::new()).with_plugins(&plugins);

        assert_eq!(discover(&plan).unwrap().len(), 1);
    }
}

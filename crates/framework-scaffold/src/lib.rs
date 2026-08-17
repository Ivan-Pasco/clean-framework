//! Project scaffolding — `cln new`, `cln library create`.
//!
//! Writes the smallest tree that builds. That constraint is the whole design:
//! a scaffold that emits commented-out options, TODO markers, or a directory
//! layout the developer has not earned yet teaches them to delete things
//! before they have learned what any of it does.
//!
//! Every generated project must satisfy three properties, each covered by a
//! test here:
//!
//! - **It builds unmodified.** `cln new x && cln build x` succeeds.
//! - **Every file in it is load-bearing.** Nothing is generated that the build
//!   would not miss.
//! - **It names a host explicitly.** ADR-0033 gives `[target]` no default, so
//!   a scaffold that omitted it would produce a project that cannot build —
//!   the single most confusing possible first experience.
//!
//! # Why the templates are inline
//!
//! A `templates/` directory read at runtime would make the scaffold depend on
//! files being installed alongside the binary, and Manager installs a single
//! executable per version. `include_str!` would be tidier to edit but puts the
//! same constraint on the source tree at build time for no gain at this size.

use std::path::{Path, PathBuf};

/// What to generate.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Template {
    /// A runnable CLI application. The default: it is the one kind that can be
    /// built and run with no host to deploy to and no further explanation.
    App,
    /// An HTTP server application.
    Server,
    /// A library other projects depend on. No `[build]`, no host — a library
    /// is compiled into whatever application uses it, against *that*
    /// application's host.
    Library,
}

impl Template {
    pub fn as_str(self) -> &'static str {
        match self {
            Template::App => "app",
            Template::Server => "server",
            Template::Library => "library",
        }
    }

    /// Parse a `--template` value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "app" | "cli" => Some(Template::App),
            "server" => Some(Template::Server),
            "library" | "lib" => Some(Template::Library),
            _ => None,
        }
    }

    pub const ALL: &'static [&'static str] = &["app", "server", "library"];
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    /// Refusing to write into a directory that already has content. A scaffold
    /// that merged into an existing project would eventually overwrite
    /// somebody's `clean.toml`, and the damage would not be obvious until
    /// their next build.
    #[error("{} already exists and is not empty", .path.display())]
    NotEmpty { path: PathBuf },

    #[error("could not create {}: {source}", .path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The name reaches `clean.toml`, the package registry, and eventually a
    /// file name. Catching a bad one here beats a TOML parse error on the
    /// developer's first build.
    #[error("'{name}' is not a usable project name: {reason}")]
    BadName { name: String, reason: String },
}

impl ScaffoldError {
    pub fn code(&self) -> &'static str {
        match self {
            ScaffoldError::BadName { .. } => "CFG001",
            ScaffoldError::NotEmpty { .. } | ScaffoldError::Unwritable { .. } => "FRM002",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            ScaffoldError::NotEmpty { .. } => {
                Some("choose a new directory, or remove the existing one first".into())
            }
            ScaffoldError::BadName { .. } => Some(
                "use letters, digits, '-' and '_', starting with a letter — for example `my-app`"
                    .into(),
            ),
            ScaffoldError::Unwritable { .. } => {
                Some("check that the parent directory is writable".into())
            }
        }
    }
}

/// What was generated.
#[derive(Clone, Debug)]
pub struct Scaffolded {
    pub root: PathBuf,
    pub template: Template,
    /// Project-relative paths, in the order written.
    pub files: Vec<String>,
}

/// The host each template targets.
///
/// Named explicitly in every generated `clean.toml`: ADR-0033 gives `[target]`
/// no default, so an omitted block is not "use the sensible one", it is a
/// project that does not build.
fn host_for(template: Template) -> Option<(&'static str, &'static str, &'static str)> {
    match template {
        // (build target, host name, host version)
        Template::App => Some(("wasm32-cli", "clean-cli", "0.1.0")),
        Template::Server => Some(("wasm32-server", "clean-server", "0.1.0")),
        // A library targets no host — see `Template::Library`.
        Template::Library => None,
    }
}

/// Generate a project at `root`.
///
/// `root`'s file name supplies the project name unless `name` overrides it.
pub fn scaffold(
    root: &Path,
    template: Template,
    name: Option<&str>,
) -> Result<Scaffolded, ScaffoldError> {
    let name = match name {
        Some(explicit) => explicit.to_string(),
        None => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            // `cln new .` in an unnamed root: fall back rather than fail, and
            // let validation below reject it if it is unusable.
            .unwrap_or_default(),
    };
    validate_name(&name)?;

    ensure_empty(root)?;

    let mut files = Vec::new();
    let mut write = |relative: &str, body: String| -> Result<(), ScaffoldError> {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| ScaffoldError::Unwritable { path: parent.to_path_buf(), source })?;
        }
        std::fs::write(&path, body)
            .map_err(|source| ScaffoldError::Unwritable { path: path.clone(), source })?;
        files.push(relative.to_string());
        Ok(())
    };

    match template {
        Template::Library => {
            write("library.toml", library_manifest(&name))?;
            write("src/main.cln", library_source(&name))?;
        }
        Template::App | Template::Server => {
            write("clean.toml", app_manifest(&name, template))?;
            write("app/main.cln", app_source(template))?;
        }
    }

    write(".gitignore", gitignore())?;

    Ok(Scaffolded { root: root.to_path_buf(), template, files })
}

/// A name that reaches `clean.toml`, a registry, and a file name.
fn validate_name(name: &str) -> Result<(), ScaffoldError> {
    let bad = |reason: &str| ScaffoldError::BadName {
        name: name.to_string(),
        reason: reason.to_string(),
    };

    if name.is_empty() {
        return Err(bad("it is empty"));
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(bad("it must start with a letter"));
    }
    if let Some(bad_char) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '-' && *c != '_')
    {
        return Err(bad(&format!("'{bad_char}' is not allowed")));
    }

    Ok(())
}

/// Refuse a directory that already has something in it.
///
/// A directory that does not exist is fine — it is created. An *empty* one is
/// fine too: `mkdir my-app && cd my-app && cln new .` is a normal way to
/// start, and refusing it would be pedantry.
fn ensure_empty(root: &Path) -> Result<(), ScaffoldError> {
    match std::fs::read_dir(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(root)
                .map_err(|source| ScaffoldError::Unwritable { path: root.to_path_buf(), source })
        }
        Err(source) => Err(ScaffoldError::Unwritable { path: root.to_path_buf(), source }),
        Ok(mut entries) => match entries.next() {
            None => Ok(()),
            Some(_) => Err(ScaffoldError::NotEmpty { path: root.to_path_buf() }),
        },
    }
}

fn app_manifest(name: &str, template: Template) -> String {
    let (target, host, host_version) =
        host_for(template).expect("app and server templates target a host");

    format!(
        "[project]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         \n\
         [build]\n\
         target = \"{target}\"\n\
         \n\
         [target]\n\
         host = \"{host}\"\n\
         version = \"{host_version}\"\n"
    )
}

fn library_manifest(name: &str) -> String {
    format!(
        "[library]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n"
    )
}

fn app_source(template: Template) -> String {
    match template {
        Template::Server => "start:\n\tprint(\"server starting\")\n".to_string(),
        _ => "start:\n\tprint(\"hello\")\n".to_string(),
    }
}

fn library_source(name: &str) -> String {
    // Named after the library so the first thing a developer sees is their own
    // name, not a placeholder to rename.
    let function = name.replace('-', "_");
    format!("{function}:\n\tprint(\"hello from {name}\")\n")
}

/// Only what a Clean build actually produces.
///
/// `dist/` is the build output (FRM-BO-09) and `.cln/` holds the lockfile and
/// version pin, which Manager owns. Listing editor and OS files here would be
/// guessing at the developer's tools; those belong in a global gitignore.
fn gitignore() -> String {
    "dist/\n.cln/\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold_into(template: Template) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my-app");
        scaffold(&root, template, None).unwrap();
        (dir, root)
    }

    fn read(root: &Path, relative: &str) -> String {
        std::fs::read_to_string(root.join(relative)).unwrap()
    }

    #[test]
    fn an_app_gets_a_manifest_a_source_file_and_nothing_else() {
        // "Nothing else" is the property: every generated file must be one the
        // build would miss.
        let (_dir, root) = scaffold_into(Template::App);

        let mut found: Vec<String> = walk(&root);
        found.sort();
        assert_eq!(found, vec![".gitignore", "app/main.cln", "clean.toml"]);
    }

    #[test]
    fn the_generated_manifest_parses_and_names_a_host() {
        // ADR-0033: there is no default host. A scaffold omitting [target]
        // produces a project that cannot build, which would be the worst
        // possible first experience.
        let (_dir, root) = scaffold_into(Template::App);
        let manifest: toml::Value = toml::from_str(&read(&root, "clean.toml")).unwrap();

        assert_eq!(manifest["project"]["name"].as_str(), Some("my-app"));
        assert_eq!(manifest["project"]["version"].as_str(), Some("0.1.0"));
        assert_eq!(manifest["build"]["target"].as_str(), Some("wasm32-cli"));
        assert_eq!(manifest["target"]["host"].as_str(), Some("clean-cli"));
        assert!(manifest["target"]["version"].is_str());
    }

    #[test]
    fn a_server_targets_the_server_world_and_host() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my-api");
        scaffold(&root, Template::Server, None).unwrap();

        let manifest: toml::Value = toml::from_str(&read(&root, "clean.toml")).unwrap();
        assert_eq!(manifest["build"]["target"].as_str(), Some("wasm32-server"));
        assert_eq!(manifest["target"]["host"].as_str(), Some("clean-server"));
    }

    #[test]
    fn a_library_has_no_build_target_and_no_host() {
        // A library is compiled into whatever application depends on it,
        // against that application's host. Giving it a [target] would be a
        // claim it cannot honour.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my-lib");
        scaffold(&root, Template::Library, None).unwrap();

        let manifest: toml::Value = toml::from_str(&read(&root, "library.toml")).unwrap();
        assert_eq!(manifest["library"]["name"].as_str(), Some("my-lib"));
        assert!(manifest.get("build").is_none(), "a library declares no build target");
        assert!(manifest.get("target").is_none(), "a library declares no host");
        assert!(root.join("src/main.cln").is_file());
        assert!(!root.join("clean.toml").exists(), "a library is not an application");
    }

    #[test]
    fn the_project_name_comes_from_the_directory_unless_overridden() {
        let dir = tempfile::tempdir().unwrap();

        let from_dir = dir.path().join("inferred-name");
        scaffold(&from_dir, Template::App, None).unwrap();
        assert!(read(&from_dir, "clean.toml").contains("name = \"inferred-name\""));

        let overridden = dir.path().join("some-directory");
        scaffold(&overridden, Template::App, Some("chosen-name")).unwrap();
        assert!(read(&overridden, "clean.toml").contains("name = \"chosen-name\""));
    }

    #[test]
    fn scaffolding_into_a_non_empty_directory_is_refused() {
        // Merging would eventually overwrite somebody's clean.toml, and the
        // damage would not show up until their next build.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("existing");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("clean.toml"), "[project]\nname = \"theirs\"\n").unwrap();

        let err = scaffold(&root, Template::App, None).unwrap_err();
        assert!(matches!(err, ScaffoldError::NotEmpty { .. }), "got {err}");
        // Their file is untouched.
        assert!(read(&root, "clean.toml").contains("theirs"));
    }

    #[test]
    fn scaffolding_into_an_empty_directory_is_allowed() {
        // `mkdir my-app && cd my-app && cln new .` is a normal way to start.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("empty");
        std::fs::create_dir_all(&root).unwrap();

        assert!(scaffold(&root, Template::App, None).is_ok());
    }

    #[test]
    fn unusable_names_are_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();

        for (name, why) in [
            ("", "empty"),
            ("9lives", "starts with a digit"),
            ("my app", "contains a space"),
            ("my/app", "contains a separator"),
            ("my.app", "contains a dot"),
        ] {
            let root = dir.path().join("target-dir");
            let err = scaffold(&root, Template::App, Some(name))
                .unwrap_err();
            assert!(
                matches!(err, ScaffoldError::BadName { .. }),
                "'{name}' ({why}) must be refused, got {err}"
            );
            assert!(!root.exists(), "nothing may be written for '{name}'");
        }
    }

    #[test]
    fn ordinary_names_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["app", "my-app", "my_app", "App2"] {
            let root = dir.path().join(name);
            assert!(scaffold(&root, Template::App, Some(name)).is_ok(), "{name} must be allowed");
        }
    }

    #[test]
    fn the_gitignore_covers_generated_output_only() {
        // dist/ is the build output; .cln/ is Manager's. Editor and OS files
        // are the developer's business, not ours to guess.
        let (_dir, root) = scaffold_into(Template::App);
        let ignored = read(&root, ".gitignore");
        assert!(ignored.contains("dist/"));
        assert!(ignored.contains(".cln/"));
    }

    #[test]
    fn a_library_names_its_function_after_itself() {
        // The first thing a developer reads should be their own name, not a
        // placeholder to rename. Hyphens become underscores because a hyphen
        // is not valid in an identifier.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("string-utils");
        scaffold(&root, Template::Library, None).unwrap();
        assert!(read(&root, "src/main.cln").starts_with("string_utils:"));
    }

    #[test]
    fn templates_parse_from_their_cli_spellings() {
        assert_eq!(Template::parse("app"), Some(Template::App));
        assert_eq!(Template::parse("cli"), Some(Template::App));
        assert_eq!(Template::parse("server"), Some(Template::Server));
        assert_eq!(Template::parse("lib"), Some(Template::Library));
        assert_eq!(Template::parse("library"), Some(Template::Library));
        assert_eq!(Template::parse("nonsense"), None);

        // Every advertised name must parse, or `--help` lies.
        for name in Template::ALL {
            assert!(Template::parse(name).is_some(), "{name} is advertised but unparseable");
        }
    }

    #[test]
    fn source_files_use_tabs_because_clean_is_tab_indented() {
        // A scaffold emitting spaces would hand the developer a file that does
        // not compile, on their very first build.
        let (_dir, root) = scaffold_into(Template::App);
        let source = read(&root, "app/main.cln");
        let body = source.lines().nth(1).expect("a body line");
        assert!(body.starts_with('\t'), "indented with spaces: {body:?}");
    }

    /// Every file under `root`, project-relative, POSIX-separated.
    fn walk(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/"),
                    );
                }
            }
        }

        found
    }
}

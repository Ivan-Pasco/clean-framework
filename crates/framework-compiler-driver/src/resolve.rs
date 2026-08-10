//! Finding the compiler binary.
//!
//! PLAN.md open question "how does framework find the compiler path" is
//! answered by Manager §00.2/§00.8: the project pins a compiler version in
//! `.cln/version`, and Manager installs that version at
//! `~/.cln/versions/compiler/<version>/clean-compiler`. We read the pin and
//! resolve the path through `cln-layout`, which is the only crate allowed to
//! string-format paths under `~/.cln/`.
//!
//! Manager normally resolves this before dispatching to us, but the framework
//! must resolve it independently: `framework-core` is callable as a library
//! (PLAN.md §3) and the integration tests drive it without Manager in the loop.

use std::path::{Path, PathBuf};

use cln_layout::Layout;
use cln_shared::ToolchainKind;
use semver::Version;

/// Where a project records its compiler pin (Platform 07 §7.2 — "toolchain
/// versions are NOT in clean.toml").
pub const VERSION_PIN_FILE: &str = ".cln/version";

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("no compiler pinned: {} does not exist", .0.display())]
    NoPin(PathBuf),

    #[error("could not read {}: {source}", .path.display())]
    UnreadablePin {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} contains '{raw}', which is not a semver version: {source}", .path.display())]
    MalformedPin {
        path: PathBuf,
        raw: String,
        #[source]
        source: semver::Error,
    },

    #[error("could not locate the ~/.cln/ directory (no home directory)")]
    NoLayout,

    #[error("compiler {version} is not installed (looked for {})", .expected.display())]
    NotInstalled { version: Version, expected: PathBuf },
}

impl ResolveError {
    /// The `cln` command that fixes this, or `None` when the failure is not
    /// something the user can install their way out of. Every diagnostic the
    /// framework emits for a resolve failure carries this as its `help:` line.
    pub fn remedy(&self) -> Option<String> {
        match self {
            ResolveError::NoPin(_) | ResolveError::MalformedPin { .. } => {
                Some("run `cln pin compiler <version>` to pin a compiler for this project".into())
            }
            ResolveError::NotInstalled { version, .. } => {
                Some(format!("run `cln install compiler {version}`"))
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCompiler {
    pub version: Version,
    pub binary: PathBuf,
}

/// Read `<project_root>/.cln/version` and resolve the installed binary.
pub fn resolve_compiler(project_root: &Path) -> Result<ResolvedCompiler, ResolveError> {
    let layout = Layout::from_home().ok_or(ResolveError::NoLayout)?;
    resolve_compiler_in(project_root, &layout)
}

/// Same, against an explicit layout root. This is what the tests use — they
/// build a fake `~/.cln/` in a tempdir rather than touching the real one.
pub fn resolve_compiler_in(
    project_root: &Path,
    layout: &Layout,
) -> Result<ResolvedCompiler, ResolveError> {
    let version = read_pin(project_root)?;
    let binary = layout.version_binary(ToolchainKind::Compiler, &version);
    if !binary.exists() {
        return Err(ResolveError::NotInstalled { version, expected: binary });
    }
    Ok(ResolvedCompiler { version, binary })
}

/// Parse the compiler pin. The file holds a bare semver string; surrounding
/// whitespace is tolerated because editors add trailing newlines.
pub fn read_pin(project_root: &Path) -> Result<Version, ResolveError> {
    let path = project_root.join(VERSION_PIN_FILE);
    if !path.exists() {
        return Err(ResolveError::NoPin(path));
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|source| ResolveError::UnreadablePin { path: path.clone(), source })?;
    let trimmed = raw.trim();
    trimmed
        .parse::<Version>()
        .map_err(|source| ResolveError::MalformedPin {
            path,
            raw: trimmed.to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_pin(dir: &Path, pin: &str) {
        std::fs::create_dir_all(dir.join(".cln")).unwrap();
        std::fs::write(dir.join(VERSION_PIN_FILE), pin).unwrap();
    }

    fn install_fake_compiler(layout: &Layout, version: &str) -> PathBuf {
        let v: Version = version.parse().unwrap();
        let binary = layout.version_binary(ToolchainKind::Compiler, &v);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        binary
    }

    #[test]
    fn resolves_a_pinned_installed_compiler() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let layout = Layout::new(home.path().join(".cln"));

        project_with_pin(project.path(), "1.4.0\n");
        let expected = install_fake_compiler(&layout, "1.4.0");

        let resolved = resolve_compiler_in(project.path(), &layout).unwrap();
        assert_eq!(resolved.version, Version::new(1, 4, 0));
        assert_eq!(resolved.binary, expected);
    }

    #[test]
    fn missing_pin_names_the_file_and_suggests_pinning() {
        let project = tempfile::tempdir().unwrap();
        let layout = Layout::new(project.path().join("unused-cln-home"));
        let err = resolve_compiler_in(project.path(), &layout).unwrap_err();
        assert!(matches!(err, ResolveError::NoPin(_)), "got {err:?}");
        assert!(err.remedy().unwrap().contains("cln pin compiler"));
    }

    #[test]
    fn uninstalled_compiler_suggests_the_exact_install_command() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let layout = Layout::new(home.path().join(".cln"));
        project_with_pin(project.path(), "2.0.1");

        let err = resolve_compiler_in(project.path(), &layout).unwrap_err();
        assert_eq!(err.remedy().unwrap(), "run `cln install compiler 2.0.1`");
    }

    #[test]
    fn malformed_pin_is_rejected_not_guessed() {
        let project = tempfile::tempdir().unwrap();
        let layout = Layout::new(project.path().join("unused-cln-home"));
        project_with_pin(project.path(), "latest");

        let err = resolve_compiler_in(project.path(), &layout).unwrap_err();
        assert!(matches!(err, ResolveError::MalformedPin { .. }), "got {err:?}");
    }

    #[test]
    fn pin_tolerates_surrounding_whitespace() {
        let project = tempfile::tempdir().unwrap();
        project_with_pin(project.path(), "  1.2.3  \n");
        assert_eq!(read_pin(project.path()).unwrap(), Version::new(1, 2, 3));
    }
}

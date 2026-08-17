//! The resolve callback — `cln fetch --internal` (Manager §00.8).
//!
//! **Resolution lives in Manager, not here.** Manager decides what version of
//! what package satisfies a constraint, materializes it on disk, and writes
//! `.cln/lock.toml`. The framework's entire involvement is asking for that to
//! happen when the lockfile is missing, then reading the result.
//!
//! Keeping it that way is the point. A resolver in the framework would be a
//! second implementation of Manager's version solving, and the two would drift
//! — with the disagreement surfacing as a build that works under `cln build`
//! and fails under `cln add`, or the reverse.
//!
//! PLAN.md open question #3 resolved this as a subprocess, matching both the
//! Manager→framework dispatch direction and the compiler seam. It runs at most
//! once per build (never per keystroke), so process-spawn cost is not worth
//! designing around.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Asks Manager to resolve a project's dependencies.
///
/// A trait so tests can drive the "lockfile appears" and "resolver fails"
/// paths without a real `cln` on PATH, and so `cln dev` can later substitute a
/// resolver that reuses one warm process.
pub trait Resolver: std::fmt::Debug {
    /// Resolve `project_root`, leaving a `.cln/lock.toml` behind on success.
    fn resolve(&self, project_root: &Path) -> Result<(), ResolveError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("could not run `{program} fetch --internal`: {source}")]
    NotRunnable {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("`{program} fetch --internal` failed with exit code {code}")]
    Failed { program: String, code: i32, stderr: String },

    /// Only reachable when a caller wires up [`NoResolver`] — a build that
    /// needs resolution in a context that cannot resolve.
    #[error("dependencies need resolving, but no resolver is available")]
    Unavailable,
}

impl ResolveError {
    /// `FRM006` — the resolve callback could not be run or refused. Distinct
    /// from `CFG002` (the lockfile itself is unreadable): here there is no
    /// lockfile to read, and the remedy is about Manager rather than the file.
    pub fn code(&self) -> &'static str {
        "FRM006"
    }

    pub fn help(&self) -> Option<String> {
        match self {
            ResolveError::NotRunnable { .. } => {
                Some("ensure `cln` is on PATH, or run `cln fetch` manually first".into())
            }
            // Manager already printed why on its own stderr; repeating a
            // generic remedy here would bury it.
            ResolveError::Failed { .. } => {
                Some("run `cln fetch` to see the resolution failure in full".into())
            }
            ResolveError::Unavailable => Some("run `cln fetch` to write .cln/lock.toml".into()),
        }
    }
}

/// The real resolver: spawns `cln fetch --internal --project=<path>`.
#[derive(Clone, Debug)]
pub struct SubprocessResolver {
    program: PathBuf,
}

/// The Manager binary, resolved from PATH. Not a pinned absolute path: unlike
/// the compiler (whose version is pinned per project and must match the
/// lockfile), the resolver *is* the Manager currently driving this build.
pub const MANAGER_PROGRAM: &str = "cln";

impl Default for SubprocessResolver {
    fn default() -> Self {
        SubprocessResolver { program: PathBuf::from(MANAGER_PROGRAM) }
    }
}

impl SubprocessResolver {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        SubprocessResolver { program: program.into() }
    }
}

impl Resolver for SubprocessResolver {
    fn resolve(&self, project_root: &Path) -> Result<(), ResolveError> {
        let program = self.program.display().to_string();

        let output = Command::new(&self.program)
            .arg("fetch")
            .arg("--internal")
            .arg(format!("--project={}", project_root.display()))
            .output()
            .map_err(|source| ResolveError::NotRunnable { program: program.clone(), source })?;

        if !output.status.success() {
            return Err(ResolveError::Failed {
                program,
                // 128 + signal is unavailable portably; -1 marks "killed by a
                // signal" distinctly from any real exit code.
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(())
    }
}

/// A resolver that always refuses.
///
/// For contexts that must never spawn Manager — the determinism test suite,
/// and any caller that wants "build exactly what is locked, or fail".
#[derive(Clone, Copy, Debug, Default)]
pub struct NoResolver;

impl Resolver for NoResolver {
    fn resolve(&self, _project_root: &Path) -> Result<(), ResolveError> {
        Err(ResolveError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_resolver_refuses_rather_than_pretending_to_succeed() {
        // Returning Ok would leave the caller reading a lockfile that was
        // never written, and reporting the absence as something else.
        let err = NoResolver.resolve(Path::new("/tmp/x")).unwrap_err();
        assert!(matches!(err, ResolveError::Unavailable));
        assert_eq!(err.code(), "FRM006");
        assert!(err.help().unwrap().contains("cln fetch"));
    }

    #[test]
    fn a_missing_manager_binary_names_what_could_not_run() {
        let resolver = SubprocessResolver::new("cln-does-not-exist-anywhere");
        let err = resolver.resolve(Path::new("/tmp")).unwrap_err();
        assert!(matches!(err, ResolveError::NotRunnable { .. }), "got {err}");
        assert!(err.to_string().contains("cln-does-not-exist-anywhere"), "got {err}");
        assert!(err.help().unwrap().contains("PATH"));
    }

    #[test]
    fn a_failing_resolver_carries_its_exit_code_and_stderr() {
        // `false` exits 1 and says nothing; enough to prove the failure path
        // reports the code rather than swallowing it.
        let resolver = SubprocessResolver::new("false");
        let err = resolver.resolve(Path::new("/tmp")).unwrap_err();
        match err {
            ResolveError::Failed { code, .. } => assert_eq!(code, 1),
            other => panic!("expected Failed, got {other}"),
        }
    }

    #[test]
    fn a_succeeding_resolver_returns_ok() {
        let resolver = SubprocessResolver::new("true");
        assert!(resolver.resolve(Path::new("/tmp")).is_ok());
    }

    /// A shim that records the argv it was called with, standing in for `cln`.
    #[cfg(unix)]
    fn recording_shim(dir: &Path, log: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let shim = dir.join("cln-shim");
        std::fs::write(
            &shim,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        shim
    }

    #[cfg(unix)]
    #[test]
    fn the_resolver_is_invoked_as_fetch_internal_for_the_given_project() {
        // Manager resolves the project it is told to, not its own cwd —
        // `cln build ./some/app` must resolve ./some/app. The exact argv is
        // the contract with Manager (§00.8), so it is worth pinning.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv.txt");
        let project = dir.path().join("some-app");
        std::fs::create_dir_all(&project).unwrap();

        let resolver = SubprocessResolver::new(recording_shim(dir.path(), &log));
        resolver.resolve(&project).unwrap();

        let argv: Vec<String> =
            std::fs::read_to_string(&log).unwrap().lines().map(str::to_string).collect();
        assert_eq!(
            argv,
            vec![
                "fetch".to_string(),
                "--internal".to_string(),
                format!("--project={}", project.display()),
            ]
        );
    }
}

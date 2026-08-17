//! The subprocess transport — Platform 14 §14.2.2.
//!
//! Protocol, one build per process:
//!
//! ```text
//!   argv:   clean-compiler compile --stdout-tar
//!   stdin:  the request document, as JSON (§14.1.1)
//!   stdout: on success, one uncompressed tar (§14.1.2)
//!   stderr: human-readable log; on failure, diagnostics JSON if the compiler
//!           could produce any
//!   exit:   0 on success, non-zero on failure (CMP-05)
//! ```
//!
//! `--stdout-tar` is passed explicitly. Platform 14 §14.1.2 makes the tarball
//! an opt-in mode of the process adapter ("or to stdout as a single tarball
//! (process adapter, with `--stdout-tar`)"), with a caller-specified output
//! directory as the other mode. We choose stdout so a build never depends on a
//! scratch directory existing and never leaves one behind on failure.
//!
//! Phase 7's warm-process mode (framed multi-request over one long-lived pipe)
//! is a *different* file in this crate, not a change to this one. It needs a
//! compiler-side ADR first — PLAN.md open question #9.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::artifact::{parse_diagnostics, CompileArtifact};
use crate::request::RequestDocument;
use crate::resolve::{resolve_compiler, resolve_compiler_in, ResolvedCompiler};
use crate::{CompileError, Compiler};

use cln_layout::Layout;

/// Invokes a Manager-installed `clean-compiler` binary, one process per build.
#[derive(Clone, Debug)]
pub struct SubprocessCompiler {
    binary: PathBuf,
    version: semver::Version,
}

impl SubprocessCompiler {
    /// Resolve from the project's `.cln/version` pin against the real
    /// `~/.cln/` layout.
    pub fn for_project(project_root: &Path) -> Result<Self, CompileError> {
        Ok(Self::from_resolved(resolve_compiler(project_root)?))
    }

    /// Resolve against an explicit layout root — used by tests.
    pub fn for_project_in(project_root: &Path, layout: &Layout) -> Result<Self, CompileError> {
        Ok(Self::from_resolved(resolve_compiler_in(project_root, layout)?))
    }

    pub fn from_resolved(resolved: ResolvedCompiler) -> Self {
        SubprocessCompiler { binary: resolved.binary, version: resolved.version }
    }

    /// Point at an arbitrary binary. This is how the orchestration tests drive
    /// `fake-compiler` through the *real* transport rather than stubbing the
    /// trait — the seam itself gets tested, per PLAN.md §7 layer B(b).
    pub fn at(binary: impl Into<PathBuf>, version: semver::Version) -> Self {
        SubprocessCompiler { binary: binary.into(), version }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The version from the `.cln/version` pin — i.e. the version Manager was
    /// asked to install. [`Compiler::version`] reports what the binary says
    /// about itself, which is what goes in the build manifest.
    pub fn pinned_version(&self) -> &semver::Version {
        &self.version
    }
}

impl Compiler for SubprocessCompiler {
    fn compile(&self, request: &RequestDocument) -> Result<CompileArtifact, CompileError> {
        self.compile_capturing(request).map(|(artifact, _)| artifact)
    }

    fn compile_capturing(
        &self,
        request: &RequestDocument,
    ) -> Result<(CompileArtifact, Vec<u8>), CompileError> {
        let payload = request.to_canonical_json()?;

        let mut child = Command::new(&self.binary)
            .arg("compile")
            .arg("--stdout-tar")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| CompileError::Spawn { path: self.binary.clone(), source })?;

        // Take stdin so it drops (and the pipe closes) before we wait for the
        // process. A compiler reading to EOF would otherwise deadlock against
        // a parent that never closes the write end.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| CompileError::MalformedOutput("compiler stdin unavailable".into()))?;
            stdin.write_all(&payload)?;
            stdin.flush()?;
        }

        // `wait_with_output` drains stdout and stderr concurrently with the
        // wait. Reading one pipe to completion first would deadlock on a
        // compiler that fills the other.
        let output = child.wait_with_output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(CompileError::CompilerFailed {
                code: output.status.code().unwrap_or(-1),
                diagnostics: diagnostics_from_failure(&output.stdout, &stderr),
                stderr,
            });
        }

        // The bytes go back alongside the parsed artifact so the build cache
        // can store the response verbatim (§11.7).
        let artifact = CompileArtifact::from_tar(&output.stdout)?;
        Ok((artifact, output.stdout))
    }

    fn version(&self) -> Result<String, CompileError> {
        let output = Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .map_err(|source| CompileError::Spawn { path: self.binary.clone(), source })?;

        if !output.status.success() {
            return Err(CompileError::CompilerFailed {
                code: output.status.code().unwrap_or(-1),
                diagnostics: Vec::new(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let text = String::from_utf8_lossy(&output.stdout);
        parse_version_output(&text).ok_or_else(|| {
            CompileError::MalformedOutput(format!(
                "could not parse a version from `clean-compiler --version` output: {text:?}"
            ))
        })
    }
}

/// On failure the compiler emits `diagnostics.json`. It may land on stdout
/// (as bare JSON, since there is no tarball to wrap it in) or on stderr. Try
/// both; a failure with no parseable diagnostics is preserved as raw stderr by
/// the caller rather than being invented here.
fn diagnostics_from_failure(stdout: &[u8], stderr: &str) -> Vec<Diagnostics> {
    if let Ok(diags) = parse_diagnostics(stdout) {
        if !diags.is_empty() {
            return diags;
        }
    }
    parse_diagnostics(stderr.as_bytes()).unwrap_or_default()
}

type Diagnostics = crate::diagnostic::Diagnostic;

/// `clean-compiler --version` prints something like `clean-compiler 1.4.0`.
/// Take the last whitespace-separated token that parses as semver, so a richer
/// banner (`clean-compiler 1.4.0 (abc1234 2026-08-01)`) still works.
fn parse_version_output(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == '(' || c == ')' || c == 'v'))
        .find(|token| token.parse::<semver::Version>().is_ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_version_banner() {
        assert_eq!(parse_version_output("clean-compiler 1.4.0\n").unwrap(), "1.4.0");
    }

    #[test]
    fn parses_a_version_with_build_metadata_banner() {
        assert_eq!(
            parse_version_output("clean-compiler 2.0.1 (abc1234 2026-08-01)\n").unwrap(),
            "2.0.1"
        );
    }

    #[test]
    fn rejects_output_with_no_version() {
        assert!(parse_version_output("unknown command\n").is_none());
    }

    #[test]
    fn missing_binary_is_a_spawn_error_naming_the_path() {
        let compiler = SubprocessCompiler::at("/nonexistent/clean-compiler", semver::Version::new(1, 0, 0));
        let err = compiler.version().unwrap_err();
        match err {
            CompileError::Spawn { path, .. } => {
                assert_eq!(path, PathBuf::from("/nonexistent/clean-compiler"))
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }
}

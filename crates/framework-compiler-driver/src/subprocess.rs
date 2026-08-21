//! The subprocess transport — Platform 14 §14.2.2.
//!
//! Protocol, one build per process:
//!
//! ```text
//!   argv:   clean-compiler --request - --out <dir>
//!   stdin:  the request document, as JSON (§14.1.1)
//!   <dir>:  on success, the artifact set — component.wasm,
//!           build-manifest.json, diagnostics.json, and optionally
//!           source-map.json
//!   stderr: human-readable log
//!   exit:   0 on success, non-zero on failure (CMP-05)
//! ```
//!
//! **This shape was corrected against the real compiler (0.1.0).** It
//! previously invoked `clean-compiler compile --stdout-tar`, reading the
//! artifact set as a tarball on stdout — the mode Platform 14 §14.1.2
//! describes as opt-in. The shipped compiler implements neither that
//! subcommand nor that flag: it exits 2 with `unexpected argument 'compile'`,
//! so every `cln build` against a real compiler failed at the seam. The
//! output-directory mode is the one that exists, so it is the one we use.
//!
//! The directory is created under the system temp dir and removed on the way
//! out, success or failure. That is the cost of this mode over stdout — a
//! scratch directory has to exist — and it is paid here rather than pushed
//! into the project, so a failed build still never writes inside `dist/`
//! (FRM-BO-10).
//!
//! The artifact set is re-packed into an in-memory tarball before being
//! returned. Everything downstream — `CompileArtifact::from_tar`, the build
//! cache that stores the response verbatim (§11.7) — already speaks tar, and
//! a cache entry must stay a self-contained blob rather than a directory that
//! has since been deleted.
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

        // Scratch space for the artifact set, removed on the way out whatever
        // happens. `OutDir` owns that guarantee via `Drop`, so an early return
        // on a failed compile cannot leak a directory.
        let out = OutDir::new()?;

        let mut child = Command::new(&self.binary)
            .arg("--request")
            .arg("-")
            .arg("--out")
            .arg(out.path())
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

            // On a rejected program the compiler still writes
            // `diagnostics.json` into the output directory — that file is the
            // real message for the user, and it is far better than the exit
            // code. Reading it before `out` is dropped is the only chance to
            // get it.
            let mut diagnostics = out.diagnostics();
            if diagnostics.is_empty() {
                diagnostics = diagnostics_from_failure(&output.stdout, &stderr);
            }

            return Err(CompileError::CompilerFailed {
                code: output.status.code().unwrap_or(-1),
                diagnostics,
                stderr,
            });
        }

        // Re-pack into a tarball so everything downstream keeps its existing
        // shape: `from_tar` parses it, and the build cache stores it verbatim
        // as a self-contained blob rather than a directory we are about to
        // delete (§11.7).
        let tarball = out.to_tar()?;
        let artifact = CompileArtifact::from_tar(&tarball)?;
        Ok((artifact, tarball))
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

/// The scratch directory the compiler writes its artifact set into.
///
/// Exists as a type rather than a few lines inline so the cleanup is a `Drop`
/// impl: `compile_capturing` returns early on a rejected program, and a
/// hand-rolled `remove_dir_all` at the end of the happy path would leak a
/// directory on every failed build — which is most builds, during development.
struct OutDir {
    path: PathBuf,
}

impl OutDir {
    fn new() -> Result<Self, CompileError> {
        // Process id and a monotonic counter: two builds in one process (the
        // determinism suite compiles the same project twice) must not share a
        // directory, and neither must two concurrent `cln build` runs.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let path = std::env::temp_dir().join(format!(
            "cln-build-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        // A directory left by a killed process would otherwise hand this build
        // the previous one's `component.wasm`.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;

        Ok(OutDir { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// `diagnostics.json` from a failed compile, if it is readable.
    ///
    /// Never an error: this runs while already reporting a failure, and a
    /// missing or malformed diagnostics file must not replace the compiler's
    /// real exit status with an I/O complaint.
    fn diagnostics(&self) -> Vec<Diagnostics> {
        std::fs::read(self.path.join(crate::artifact::DIAGNOSTICS_ENTRY))
            .ok()
            .and_then(|bytes| parse_diagnostics(&bytes).ok())
            .unwrap_or_default()
    }

    /// Pack the artifact set into an uncompressed tarball.
    ///
    /// Entries are added in a fixed order rather than in `read_dir` order,
    /// which is unspecified and varies by filesystem. The tarball is hashed
    /// nowhere, but it *is* what the build cache stores, and a cache whose
    /// stored bytes vary run-to-run for one compilation would be a puzzle to
    /// debug later.
    fn to_tar(&self) -> Result<Vec<u8>, CompileError> {
        use crate::artifact::{
            DIAGNOSTICS_ENTRY, MANIFEST_ENTRY, SOURCE_MAP_ENTRY, WASM_ENTRY,
        };

        let mut builder = tar::Builder::new(Vec::new());

        for name in [WASM_ENTRY, MANIFEST_ENTRY, DIAGNOSTICS_ENTRY, SOURCE_MAP_ENTRY] {
            let file = self.path.join(name);
            let Ok(bytes) = std::fs::read(&file) else {
                // Absent entries are normal: `source-map.json` is optional, and
                // a diagnostics-only run writes no component. A *required*
                // entry being missing is caught by `from_tar`, which reports it
                // against the artifact contract rather than as a file-not-found.
                continue;
            };

            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            // A fixed mtime: the tarball is the build cache's stored value, and
            // baking the wall clock into it would make two identical
            // compilations produce different cache contents.
            header.set_mtime(0);
            header.set_cksum();

            builder
                .append_data(&mut header, name, bytes.as_slice())
                .map_err(|e| CompileError::MalformedOutput(format!(
                    "could not pack {name} from the compiler's output: {e}"
                )))?;
        }

        builder
            .into_inner()
            .map_err(|e| CompileError::MalformedOutput(format!(
                "could not finish packing the compiler's output: {e}"
            )))
    }
}

impl Drop for OutDir {
    fn drop(&mut self) {
        // Best-effort: a build that succeeded must not fail because a temp
        // directory could not be removed.
        let _ = std::fs::remove_dir_all(&self.path);
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

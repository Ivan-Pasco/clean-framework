//! The single seam between the framework and the Clean compiler.
//!
//! Everything the framework knows about *how* the compiler is invoked lives in
//! this crate. `framework-core` knows only the [`Compiler`] trait: hand it a
//! [`RequestDocument`], get back a [`CompileArtifact`] or diagnostics. When the
//! compiler grows a second transport (JSON-RPC / MCP per Platform 14 §14.2.3,
//! or the framed warm-process mode for `cln dev`), it is an added file here —
//! not a scatter change across the orchestrator.
//!
//! Per PLAN.md's locked decisions, the framework never Cargo-depends on the
//! compiler crate. The compiler is a Manager-installed binary at
//! `~/.cln/versions/compiler/<version>/clean-compiler`, invoked as a subprocess.

pub mod artifact;
pub mod diagnostic;
pub mod request;
pub mod resolve;
pub mod subprocess;

pub use artifact::{BuildManifest, CompileArtifact};
pub use diagnostic::{Diagnostic, Level, Position, Span};
pub use request::{Build, Memory, Override, Project, RequestDocument, Source};
pub use resolve::{resolve_compiler, ResolveError, ResolvedCompiler};
pub use subprocess::SubprocessCompiler;

/// The framework/compiler boundary.
///
/// Note the two failure channels. `Err(CompileError)` means the seam itself
/// broke — the binary was missing, the process died, stdout was not a valid
/// tarball. `Ok(artifact)` with `diagnostics` containing errors means the
/// compiler ran and rejected the program, which is a normal build outcome and
/// must be reported to the user as diagnostics, not as a framework crash.
pub trait Compiler {
    /// Compile one request document. Implementations must not touch the
    /// project directory — everything the compiler needs is in `request`
    /// (FRM-BO-02, CMP-01).
    fn compile(&self, request: &RequestDocument) -> Result<CompileArtifact, CompileError>;

    /// [`Compiler::compile`], also returning the response **exactly as it
    /// arrived** — the undecoded tarball.
    ///
    /// This exists for the build cache (§11.7), which stores those raw bytes
    /// rather than a re-serialized [`CompileArtifact`]. Re-serializing would
    /// risk dropping anything the framework does not model (the build manifest
    /// is deliberately opaque, §14.8); keeping the bytes means a cache hit
    /// replays the same `from_tar` on the same input as a cache miss, so
    /// CMP-06 holds by construction instead of by care.
    ///
    /// The default implementation returns an empty tarball alongside the
    /// artifact, which a cache reads as "nothing worth storing". Wrappers and
    /// test doubles that have no meaningful bytes need not implement it.
    fn compile_capturing(
        &self,
        request: &RequestDocument,
    ) -> Result<(CompileArtifact, Vec<u8>), CompileError> {
        self.compile(request).map(|artifact| (artifact, Vec::new()))
    }

    /// The compiler's self-reported version, for the build manifest.
    /// Read from `clean-compiler --version` per PLAN.md open question #2 —
    /// we never trust the version encoded in the install folder name.
    fn version(&self) -> Result<String, CompileError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("compiler binary not found: {0}")]
    Resolve(#[from] ResolveError),

    #[error("failed to spawn compiler at {path}: {source}")]
    Spawn {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("compiler process failed: {0}")]
    Io(#[from] std::io::Error),

    /// The compiler exited non-zero. Diagnostics are carried, never a
    /// stringly-typed message (Platform 14 §14.2.1, DIA-01). An empty
    /// `diagnostics` here means the compiler failed *without* explaining
    /// itself, which is a compiler bug — `stderr` preserves the evidence.
    #[error("compiler exited with code {code}")]
    CompilerFailed {
        code: i32,
        diagnostics: Vec<Diagnostic>,
        stderr: String,
    },

    #[error("could not parse compiler output: {0}")]
    MalformedOutput(String),

    #[error("could not serialize request document: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl CompileError {
    /// Diagnostics carried by this failure, if any. Lets the CLI render a
    /// compiler rejection the same way it renders a successful build's
    /// warnings, rather than special-casing the error path.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            CompileError::CompilerFailed { diagnostics, .. } => diagnostics,
            _ => &[],
        }
    }
}

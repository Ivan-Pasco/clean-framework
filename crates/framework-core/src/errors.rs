//! Framework-side failures.
//!
//! Two things every variant here owes the caller: a **registry code** (DIA-01
//! — a stringly-typed message must not reach the user) and a **help line**
//! saying what to do next (Platform 13 §1). [`FrameworkError::to_diagnostic`]
//! is how these reach Manager's renderer in the same shape the compiler's own
//! diagnostics arrive in, so Manager has one renderer, not two.
//!
//! Codes used in M0, from the Platform 09 registry:
//!
//! - `CFG001` — schema violation in `clean.toml` (unknown/missing target).
//! - `CFG003` — the manifest itself is missing or unparseable.
//! - `CFG005` — a source file is not valid UTF-8 (TXT-02).
//! - `FRM001` — the pinned compiler cannot be resolved or invoked.
//! - `FRM002` — the framework could not write its outputs.

use std::path::PathBuf;

use framework_compiler_driver::{CompileError, Diagnostic};

#[derive(Debug, thiserror::Error)]
pub enum FrameworkError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    /// The compiler seam failed, or the compiler rejected the program. Both
    /// arrive here; `diagnostics()` distinguishes them.
    #[error(transparent)]
    Compiler(#[from] CompileError),

    #[error("could not write {}: {source}", .path.display())]
    Output {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("no {} found at {}", crate::manifest::MANIFEST_FILE, .path.display())]
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

    #[error("{}: [project].name must not be empty", .path.display())]
    EmptyProjectName { path: PathBuf },

    #[error("{}: [project].version '{raw}' is not a semver version", .path.display())]
    MalformedProjectVersion { path: PathBuf, raw: String },

    #[error("{}: [build].target is required", .path.display())]
    MissingTarget { path: PathBuf },

    #[error("{}: unknown build target '{target}'", .path.display())]
    UnknownTarget { path: PathBuf, target: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("could not read {}: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// TXT-02: the reader validates at the moment it reads. No fallback
    /// encodings — a file decoded with the wrong table produces well-formed
    /// text that is undetectable downstream.
    #[error("{} is not valid UTF-8", .path.display())]
    NotUtf8 { path: PathBuf },

    #[error("no source files found under {}", .searched.join(", "))]
    NoSources { searched: Vec<String> },
}

impl FrameworkError {
    /// The Platform 09 registry code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            FrameworkError::Manifest(e) => e.code(),
            FrameworkError::Discovery(e) => e.code(),
            FrameworkError::Compiler(_) => "FRM001",
            FrameworkError::Output { .. } => "FRM002",
        }
    }

    /// What the user should do next.
    pub fn help(&self) -> Option<String> {
        match self {
            FrameworkError::Manifest(e) => e.help(),
            FrameworkError::Discovery(e) => e.help(),
            FrameworkError::Compiler(CompileError::Resolve(e)) => e.remedy(),
            FrameworkError::Compiler(_) => None,
            FrameworkError::Output { .. } => {
                Some("check that the project directory is writable".into())
            }
        }
    }

    /// Diagnostics carried *by the compiler*. Non-empty only when the compiler
    /// ran and rejected the program — in that case these are the real message
    /// for the user and the framework's own error is just the envelope.
    pub fn compiler_diagnostics(&self) -> &[Diagnostic] {
        match self {
            FrameworkError::Compiler(e) => e.diagnostics(),
            _ => &[],
        }
    }

    /// Render as the wire diagnostic Manager expects.
    ///
    /// When the compiler supplied its own diagnostics we return those verbatim
    /// — they have real spans and the framework's envelope message ("compiler
    /// exited with code 1") would only bury them.
    pub fn to_diagnostics(&self) -> Vec<Diagnostic> {
        let from_compiler = self.compiler_diagnostics();
        if !from_compiler.is_empty() {
            return from_compiler.to_vec();
        }

        let mut diagnostic = Diagnostic::error(self.code(), self.headline());
        if let Some(help) = self.help() {
            diagnostic = diagnostic.with_help(help);
        }
        if let Some(path) = self.source_file() {
            diagnostic = diagnostic.with_file(path);
        }
        // The full `Display` chain carries the underlying cause (the io::Error,
        // the toml parse position). The headline is bounded at 100 chars by
        // DIA-02, so the detail goes in a note rather than being lost.
        let detail = self.to_string();
        if detail != self.headline() {
            diagnostic = diagnostic.with_note(detail);
        }
        vec![diagnostic]
    }

    /// A DIA-02-conformant headline: one line, <= 100 chars, no trailing
    /// punctuation.
    fn headline(&self) -> String {
        let full = self.to_string();
        let first_line = full.lines().next().unwrap_or_default();
        let trimmed = first_line.trim_end_matches(['.', ':', ' ']);
        if trimmed.chars().count() <= 100 {
            return trimmed.to_string();
        }
        let truncated: String = trimmed.chars().take(97).collect();
        format!("{truncated}...")
    }

    /// The file this failure is about, for the diagnostic's primary span.
    fn source_file(&self) -> Option<String> {
        let path = match self {
            FrameworkError::Manifest(e) => e.path(),
            FrameworkError::Discovery(DiscoveryError::Unreadable { path, .. })
            | FrameworkError::Discovery(DiscoveryError::NotUtf8 { path }) => path,
            FrameworkError::Output { path, .. } => path,
            _ => return None,
        };
        Some(path.to_string_lossy().replace('\\', "/"))
    }
}

impl ManifestError {
    pub fn code(&self) -> &'static str {
        match self {
            // The manifest could not be read at all.
            ManifestError::Missing { .. }
            | ManifestError::Unreadable { .. }
            | ManifestError::Malformed { .. } => "CFG003",
            // The manifest parsed but violates the schema (CONF-06).
            ManifestError::EmptyProjectName { .. }
            | ManifestError::MalformedProjectVersion { .. }
            | ManifestError::MissingTarget { .. }
            | ManifestError::UnknownTarget { .. } => "CFG001",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            ManifestError::Missing { .. } => {
                Some("run `cln new <name>` to scaffold a project, or build from a directory containing clean.toml".into())
            }
            ManifestError::MissingTarget { .. } => Some(format!(
                "add `target = \"wasm32-server\"` under [build]; valid targets are {}",
                crate::manifest::BUILT_IN_TARGETS.join(", ")
            )),
            ManifestError::UnknownTarget { .. } => Some(format!(
                "valid targets are {}",
                crate::manifest::BUILT_IN_TARGETS.join(", ")
            )),
            ManifestError::MalformedProjectVersion { .. } => {
                Some("use a semver version such as \"0.1.0\"".into())
            }
            _ => None,
        }
    }

    fn path(&self) -> &PathBuf {
        match self {
            ManifestError::Missing { path }
            | ManifestError::Unreadable { path, .. }
            | ManifestError::Malformed { path, .. }
            | ManifestError::EmptyProjectName { path }
            | ManifestError::MalformedProjectVersion { path, .. }
            | ManifestError::MissingTarget { path }
            | ManifestError::UnknownTarget { path, .. } => path,
        }
    }
}

impl DiscoveryError {
    pub fn code(&self) -> &'static str {
        match self {
            DiscoveryError::NotUtf8 { .. } => "CFG005",
            DiscoveryError::Unreadable { .. } => "FRM002",
            DiscoveryError::NoSources { .. } => "FRM003",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            DiscoveryError::NotUtf8 { .. } => {
                Some("re-save the file as UTF-8; the framework does not attempt fallback encodings".into())
            }
            DiscoveryError::NoSources { .. } => {
                Some("add a .cln file under app/, or set [build].entry to your source root".into())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_is_bounded_and_unpunctuated() {
        let err = FrameworkError::Manifest(ManifestError::Missing {
            path: PathBuf::from("/some/where/clean.toml"),
        });
        let headline = err.headline();
        assert!(headline.chars().count() <= 100);
        assert!(!headline.ends_with('.'));
        assert!(!headline.contains('\n'));
    }

    #[test]
    fn manifest_errors_carry_codes_and_help() {
        let err = FrameworkError::Manifest(ManifestError::UnknownTarget {
            path: PathBuf::from("clean.toml"),
            target: "wasm32-toaster".into(),
        });
        assert_eq!(err.code(), "CFG001");
        assert!(err.help().unwrap().contains("wasm32-server"));

        let diags = err.to_diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "CFG001");
        assert!(!diags[0].helps.is_empty());
        assert_eq!(diags[0].primary_span.as_ref().unwrap().file, "clean.toml");
    }

    #[test]
    fn utf8_failure_is_cfg005_per_txt02() {
        let err = FrameworkError::Discovery(DiscoveryError::NotUtf8 {
            path: PathBuf::from("app/main.cln"),
        });
        assert_eq!(err.code(), "CFG005");
        assert!(err.help().unwrap().contains("UTF-8"));
    }

    #[test]
    fn compiler_diagnostics_replace_the_framework_envelope() {
        // A compiler rejection must surface the compiler's spanned diagnostic,
        // not "compiler exited with code 1".
        let inner = Diagnostic::error("SEM001", "unknown identifier `pritn`");
        let err = FrameworkError::Compiler(CompileError::CompilerFailed {
            code: 1,
            diagnostics: vec![inner.clone()],
            stderr: String::new(),
        });
        assert_eq!(err.to_diagnostics(), vec![inner]);
    }

    #[test]
    fn compiler_failure_without_diagnostics_still_produces_one() {
        let err = FrameworkError::Compiler(CompileError::CompilerFailed {
            code: 101,
            diagnostics: Vec::new(),
            stderr: "thread 'main' panicked".into(),
        });
        let diags = err.to_diagnostics();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "FRM001");
    }
}

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
//! - `CFG001` — schema violation in `clean.toml` (unknown/missing target,
//!   missing `[target]` block, unresolvable host, world not in the contract).
//! - `CFG003` — the manifest itself is missing or unparseable.
//! - `CFG005` — a source file is not valid UTF-8 (TXT-02).
//! - `FRM001` — the pinned compiler cannot be resolved or invoked.
//! - `FRM002` — the framework could not write its outputs, or its caches.
//! - `FRM003` — no source files were discovered.
//! - `FRM004` — the target's `host.wit` could not be obtained (Moment 1).
//! - `FRM005` — the obtained contract disagrees with `.cln/lock.toml`
//!   (BVER-03).

use std::path::PathBuf;

use framework_compiler_driver::{CompileError, Diagnostic};

#[derive(Debug, thiserror::Error)]
pub enum FrameworkError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    /// Boxed: `HostWitError` carries several `PathBuf`-plus-`String` variants,
    /// and inlining the largest of them would grow every `Result` in the crate
    /// — including the hot success paths that never see one.
    #[error(transparent)]
    HostWit(#[from] Box<HostWitError>),

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

/// Failures obtaining or verifying the target's `host.wit` (Moment 1).
///
/// None of these has a "carry on without a world" branch. ADR-0033 refuses a
/// fallback that compiles against an assumed contract, and the whole value of
/// the World Import Check is that it reflects a real host declaration.
#[derive(Debug, thiserror::Error)]
pub enum HostWitError {
    #[error("{}: [target] section is required", .path.display())]
    MissingSection { path: PathBuf },

    #[error("{}: no world is defined for build target '{target}'", .path.display())]
    NoWorldForTarget { path: PathBuf, target: String },

    #[error("unknown host '{host}' and no wit_source given")]
    UnknownHost { host: String, known: Vec<&'static str> },

    #[error("host contract for {host}@{version} declares no world '{world}'")]
    WorldNotInContract { host: String, version: String, world: String },

    #[error("no cached host contract for {host}@{version} and --offline was given")]
    OfflineCacheMiss { host: String, version: String, path: PathBuf },

    /// Reached only if a caller allows the network but supplies no fetcher.
    /// A bug in wiring, not a user error — but a panic here would be worse.
    #[error("no fetcher available to obtain the host contract for {host}")]
    NoFetcher { host: String },

    #[error("could not fetch host contract from {url}: {reason}")]
    FetchFailed { url: String, reason: String },

    #[error("host contract for {host}@{version} does not match the hash in .cln/lock.toml")]
    LockMismatch { host: String, version: String, expected: String, found: String },

    #[error("could not read cached host contract {}: {source}", .path.display())]
    CacheUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write host contract cache {}: {source}", .path.display())]
    CacheUnwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not read {}: {source}", .path.display())]
    LockUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {}: {source}", .path.display())]
    LockUnwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse {}: {source}", .path.display())]
    LockMalformed {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("{}: [host] is not a table", .path.display())]
    LockShape { path: PathBuf },

    #[error("could not serialize {}: {source}", .path.display())]
    LockSerialize {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },

    #[error("cannot locate the host-contract cache: no home directory")]
    NoHomeDirectory,
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
            FrameworkError::HostWit(e) => e.code(),
            FrameworkError::Compiler(_) => "FRM001",
            FrameworkError::Output { .. } => "FRM002",
        }
    }

    /// What the user should do next.
    pub fn help(&self) -> Option<String> {
        match self {
            FrameworkError::Manifest(e) => e.help(),
            FrameworkError::Discovery(e) => e.help(),
            FrameworkError::HostWit(e) => e.help(),
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
            FrameworkError::HostWit(e) => e.path()?,
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

/// Box on the way in, so every call site can still write `?` and `.into()`
/// against a bare `HostWitError` while the enum itself stays small.
impl From<HostWitError> for FrameworkError {
    fn from(error: HostWitError) -> Self {
        FrameworkError::HostWit(Box::new(error))
    }
}

impl HostWitError {
    pub fn code(&self) -> &'static str {
        match self {
            // The manifest is wrong: a missing/unusable [target], an
            // unresolvable host, or a target whose world the contract lacks.
            // All are CONF-06 schema violations the developer fixes in
            // clean.toml.
            HostWitError::MissingSection { .. }
            | HostWitError::NoWorldForTarget { .. }
            | HostWitError::UnknownHost { .. }
            | HostWitError::WorldNotInContract { .. } => "CFG001",

            // The contract could not be obtained.
            HostWitError::OfflineCacheMiss { .. }
            | HostWitError::NoFetcher { .. }
            | HostWitError::FetchFailed { .. } => "FRM004",

            // The contract was obtained but does not match what this project
            // is locked against (BVER-03).
            HostWitError::LockMismatch { .. } => "FRM005",

            // Local I/O around the cache and lockfile.
            HostWitError::CacheUnreadable { .. }
            | HostWitError::CacheUnwritable { .. }
            | HostWitError::LockUnreadable { .. }
            | HostWitError::LockUnwritable { .. }
            | HostWitError::LockMalformed { .. }
            | HostWitError::LockShape { .. }
            | HostWitError::LockSerialize { .. }
            | HostWitError::NoHomeDirectory => "FRM002",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            HostWitError::MissingSection { .. } => Some(
                "add a [target] block naming the host, e.g. host = \"clean-server\" and version = \"0.6.0\""
                    .into(),
            ),
            HostWitError::NoWorldForTarget { target, .. } => Some(format!(
                "'{target}' maps to no component-model world; use one of {}",
                crate::manifest::BUILT_IN_TARGETS.join(", ")
            )),
            HostWitError::UnknownHost { known, .. } => Some(format!(
                "set [target].wit_source to the host's host.wit URL, or use a known host: {}",
                known.join(", ")
            )),
            HostWitError::WorldNotInContract { host, world, .. } => Some(format!(
                "{host} does not declare world '{world}'; check [build].target matches this host"
            )),
            HostWitError::OfflineCacheMiss { .. } => {
                Some("run once without --offline to populate the host-contract cache".into())
            }
            HostWitError::FetchFailed { .. } => {
                Some("check the URL and network, or run with a warm cache and --offline".into())
            }
            // A hash disagreement is either a republished contract or a
            // tampered cache. Deleting the lock entry is the deliberate act
            // that says "I accept the new contract" — we never do it for them.
            HostWitError::LockMismatch { host, .. } => Some(format!(
                "the host republished its contract, or the cache was altered; \
                 review the change and remove the [host.{host}] entry from .cln/lock.toml to re-pin"
            )),
            _ => None,
        }
    }

    fn path(&self) -> Option<&PathBuf> {
        match self {
            HostWitError::MissingSection { path }
            | HostWitError::NoWorldForTarget { path, .. }
            | HostWitError::CacheUnreadable { path, .. }
            | HostWitError::CacheUnwritable { path, .. }
            | HostWitError::LockUnreadable { path, .. }
            | HostWitError::LockUnwritable { path, .. }
            | HostWitError::LockMalformed { path, .. }
            | HostWitError::LockShape { path }
            | HostWitError::LockSerialize { path, .. }
            | HostWitError::OfflineCacheMiss { path, .. } => Some(path),
            _ => None,
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

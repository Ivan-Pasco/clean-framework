//! The seam to the compiler's language-info API (Framework §10.1, §10.5).
//!
//! Language-shaped questions — what the syntax is, what the builtins are, what
//! an error code means — are **not** the framework's to answer. §10.7 Rule 1
//! is explicit: the framework MCP never answers them from its own memory, a
//! cached copy, or an external URL. Every such answer comes from a fresh call
//! to the compiler pinned by the current project.
//!
//! The reason is version-matching (§10.3). Language documentation is compiled
//! *into* each compiler binary, so compiler 1.4 knows the spec for 1.4. A
//! developer pinned to 1.4 must be told 1.4's rules even with 2.0 installed —
//! and the only way to guarantee that is to ask the binary that will actually
//! run.
//!
//! # Why a trait
//!
//! §10.5 specifies two equivalent surfaces: a CLI (`cln --json <query>`) for
//! out-of-process calls, and a linkable Rust library for in-process ones. One
//! trait with two implementations is what lets the tool handlers be written
//! once against either. It is also what makes them testable without a
//! compiler installed.
//!
//! # Today
//!
//! **No shipped compiler implements this API.** `clean-compiler 0.1.0`
//! rejects `--json` outright, and carries none of the bundled documentation
//! §10.3 requires. [`Unavailable`] is therefore the default, and it fails
//! loudly with the exact call it tried — a language tool that silently
//! invented an answer would be the precise drift §10.7 exists to prevent.

use serde_json::Value;

/// One language-info query, as named in §10.2.1.
///
/// A closed enum rather than a free string: these are the queries the tool
/// surface can raise, and a typo should not become a subprocess call that
/// fails at the far end with a message about an unknown argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Query {
    /// Concise syntax and core patterns.
    QuickReference,
    /// A named section of the language specification.
    Specification { section: Option<String> },
    /// Built-in functions with signatures.
    Builtins,
    /// Primitive and built-in complex types.
    Types,
    /// Stdlib functions with signatures and categories.
    Functions,
    /// The numbered error taxonomy.
    ErrorCodes,
    /// The human explanation for one code.
    ExplainError { code: String },
    /// What changed between compiler versions.
    Changelog,
    /// Typecheck a self-contained snippet. No WASM emission.
    Check { source: String },
    /// Parse a snippet and return its AST.
    Parse { source: String },
    /// Validate a snippet against the language rules.
    Validate { source: String },
    /// Format a snippet in canonical style.
    Format { source: String },
    /// Compile a self-contained snippet to WASM.
    Compile { source: String },
}

impl Query {
    /// The `cln --json` subcommand this query maps to (§10.5).
    ///
    /// Named here rather than at the call site so the CLI contract has one
    /// home — this list *is* the request half of what the compiler must
    /// implement.
    pub fn subcommand(&self) -> &'static str {
        match self {
            Query::QuickReference => "get-quick-reference",
            Query::Specification { .. } => "get-specification",
            Query::Builtins => "list-builtins",
            Query::Types => "list-types",
            Query::Functions => "list-functions",
            Query::ErrorCodes => "list-error-codes",
            Query::ExplainError { .. } => "explain-error",
            Query::Changelog => "get-changelog",
            Query::Check { .. } => "check",
            Query::Parse { .. } => "parse",
            Query::Validate { .. } => "validate",
            Query::Format { .. } => "format",
            Query::Compile { .. } => "compile",
        }
    }

    /// The full argv this query would be invoked as, for diagnostics.
    ///
    /// Snippet-carrying queries pass their source on stdin rather than argv,
    /// so it does not appear here — a multi-line program in an error message
    /// would bury the thing that failed.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec!["--json".to_string(), self.subcommand().to_string()];
        match self {
            Query::Specification { section: Some(section) } => {
                argv.push("--section".into());
                argv.push(section.clone());
            }
            Query::ExplainError { code } => argv.push(code.clone()),
            _ => {}
        }
        argv
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LanguageError {
    /// No compiler implements the language-info API yet.
    #[error(
        "the compiler's language-info API is not available: {reason}\n\
         tried: cln {}",
        .attempted.join(" ")
    )]
    Unavailable { reason: String, attempted: Vec<String> },

    /// The call ran and the compiler refused it.
    #[error("the compiler rejected `cln {}`: {message}", .attempted.join(" "))]
    Refused { attempted: Vec<String>, message: String },

    /// The call ran and returned something that is not the agreed schema.
    #[error("the compiler's answer to `cln {}` was not valid JSON: {message}",
            .attempted.join(" "))]
    Malformed { attempted: Vec<String>, message: String },
}

/// The compiler's language-info API, as the MCP consumes it.
///
/// Implementations: [`Unavailable`] (the default today) and, once a compiler
/// ships the API, a subprocess implementation over `cln --json` plus an
/// in-process one over the linkable library (§10.5).
pub trait LanguageInfo: std::fmt::Debug + Send + Sync {
    /// Answer one query against the pinned compiler.
    fn query(&self, query: &Query) -> Result<Value, LanguageError>;

    /// The compiler version answers are coming from, when known.
    ///
    /// `None` when no compiler is resolvable. §10.9 rule 2 calls the
    /// no-project case "unpinned"; every language answer carries this so a
    /// developer can see which compiler spoke.
    fn compiler_version(&self) -> Option<String>;
}

/// The default: no compiler implements the API.
///
/// Every query fails, and the failure names the exact call that was attempted
/// so the gap is legible rather than mysterious. This is deliberately not a
/// fixture or a fallback — §10.7 Rule 1 forbids the framework answering a
/// language question from anywhere but the compiler, and a stub that returned
/// plausible text would violate it in the most damaging possible way: silently.
#[derive(Clone, Debug)]
pub struct Unavailable {
    reason: String,
}

impl Unavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Unavailable { reason: reason.into() }
    }

    /// The state of the world as of compiler 0.1.0.
    pub fn not_implemented() -> Self {
        Unavailable::new(
            "no installed compiler implements `cln --json <query>` \
             (Framework §10.5); clean-compiler 0.1.0 rejects --json and \
             bundles none of the §10.3 documentation",
        )
    }
}

impl LanguageInfo for Unavailable {
    fn query(&self, query: &Query) -> Result<Value, LanguageError> {
        Err(LanguageError::Unavailable {
            reason: self.reason.clone(),
            attempted: query.argv(),
        })
    }

    fn compiler_version(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_query_maps_to_a_distinct_subcommand() {
        // The subcommand list is the request half of the compiler contract.
        // Two queries sharing a name would make one of them unreachable.
        let queries = [
            Query::QuickReference,
            Query::Specification { section: None },
            Query::Builtins,
            Query::Types,
            Query::Functions,
            Query::ErrorCodes,
            Query::ExplainError { code: "SEM021".into() },
            Query::Changelog,
            Query::Check { source: String::new() },
            Query::Parse { source: String::new() },
            Query::Validate { source: String::new() },
            Query::Format { source: String::new() },
            Query::Compile { source: String::new() },
        ];

        let mut seen = std::collections::BTreeSet::new();
        for query in &queries {
            assert!(
                seen.insert(query.subcommand()),
                "duplicate subcommand {}",
                query.subcommand()
            );
        }
        assert_eq!(seen.len(), 13);
    }

    #[test]
    fn a_sectioned_specification_query_carries_the_section() {
        let query = Query::Specification { section: Some("types".into()) };
        assert_eq!(query.argv(), ["--json", "get-specification", "--section", "types"]);
    }

    #[test]
    fn an_explain_error_query_carries_the_code() {
        let query = Query::ExplainError { code: "SEM021".into() };
        assert_eq!(query.argv(), ["--json", "explain-error", "SEM021"]);
    }

    #[test]
    fn a_snippet_query_keeps_the_source_out_of_argv() {
        // Source travels on stdin. In argv it would bury the failure in a
        // diagnostic and, for a large file, overflow the argument limit.
        let query = Query::Check { source: "start:\n\tinteger x = 1\n".into() };
        assert_eq!(query.argv(), ["--json", "check"]);
    }

    #[test]
    fn the_unavailable_implementation_names_the_call_it_tried() {
        let info = Unavailable::not_implemented();
        let error = info.query(&Query::Builtins).unwrap_err();

        let text = error.to_string();
        assert!(text.contains("cln --json list-builtins"), "{text}");
        assert!(text.contains("§10.5"), "{text}");
    }

    #[test]
    fn the_unavailable_implementation_reports_no_version() {
        assert_eq!(Unavailable::not_implemented().compiler_version(), None);
    }
}

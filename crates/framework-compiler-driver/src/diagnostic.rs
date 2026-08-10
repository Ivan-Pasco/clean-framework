//! The diagnostic wire shape, per Platform 13 §2.
//!
//! The framework both *consumes* these (parsing `diagnostics.json` out of the
//! compiler's artifact tarball) and *emits* them (framework-side failures like
//! `CFG005` for invalid UTF-8). Manager renders them, so there is one shape on
//! the wire and one renderer at the top — not two of each.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Info,
    Help,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// 1-based, counted in characters (Platform 13 §2).
    pub line: u32,
    /// 1-based, counted in characters.
    pub column: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// Project-relative, forward-slashed.
    pub file: String,
    pub start: Position,
    /// Exclusive end.
    pub end: Position,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub span: Span,
    pub label: String,
}

/// Platform 13 §2. Optional fields are skipped when empty so a framework-built
/// diagnostic and a compiler-built one serialize to the same minimal shape.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub level: Level,
    /// `PREFIX###` form, resolving to a row in Platform 09 (DIA-01).
    pub code: String,
    /// Single line, <= 100 chars, no trailing punctuation (DIA-02).
    pub message: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_span: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_url: Option<String>,
}

impl Diagnostic {
    /// A framework-side error with no source location — a missing compiler
    /// binary, an unreadable manifest. Spanned diagnostics come from the
    /// compiler, which is the only party that has parsed the source.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        let doc_url = Some(format!("https://errors.cleanlanguage.dev/E/{code}"));
        Diagnostic {
            level: Level::Error,
            code,
            message: message.into(),
            primary_span: None,
            primary_label: None,
            secondary: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            doc_url,
        }
    }

    /// Attach a `help:` line — the "what to do next" a good diagnostic owes
    /// the reader (Platform 13 §1).
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
    }

    /// Attach a `note:` line — context, not action.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Point the diagnostic at a file the framework read but could not use.
    /// Column 1 line 1: the framework knows the file, never the offset.
    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        let file = path.into();
        self.primary_span = Some(Span {
            file,
            start: Position { line: 1, column: 1 },
            end: Position { line: 1, column: 1 },
        });
        self
    }

    pub fn is_error(&self) -> bool {
        matches!(self.level, Level::Error)
    }
}

/// True when any diagnostic in the slice fails the build (Platform 13 §3).
pub fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(Diagnostic::is_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_diagnostic_skips_empty_fields() {
        let d = Diagnostic::error("CFG005", "file is not valid UTF-8");
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("secondary"), "empty vecs must be skipped: {json}");
        assert!(!json.contains("primary_span"), "absent span must be skipped: {json}");
        assert!(json.contains(r#""level":"error""#));
        assert!(json.contains("https://errors.cleanlanguage.dev/E/CFG005"));
    }

    #[test]
    fn roundtrips_through_json() {
        let d = Diagnostic::error("CFG005", "file is not valid UTF-8")
            .with_file("app/main.cln")
            .with_help("re-save the file as UTF-8");
        let back: Diagnostic = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn has_errors_ignores_warnings() {
        let warn = Diagnostic {
            level: Level::Warning,
            ..Diagnostic::error("SEM001", "unused variable")
        };
        assert!(!has_errors(std::slice::from_ref(&warn)));
        assert!(has_errors(&[warn, Diagnostic::error("SEM002", "boom")]));
    }
}

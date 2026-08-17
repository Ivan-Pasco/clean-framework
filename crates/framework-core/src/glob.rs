//! Path globbing for `[folders]` keys and `[build].exclude` entries.
//!
//! Deliberately small. Clean's config uses path globs — `app/server/**`,
//! `src/*.cln` — and nothing here needs brace expansion, character classes, or
//! extglob. A dependency would bring all of that plus its own opinions about
//! case sensitivity and leading dots, and every one of those opinions would
//! silently become part of what `[folders]` means.
//!
//! # The three wildcards
//!
//! | Pattern | Matches | Does not match |
//! | --- | --- | --- |
//! | `*` | any run of characters **within one segment** | `/` |
//! | `**` | any number of whole segments, including none | — |
//! | `?` | exactly one character within one segment | `/` |
//!
//! `**` is only meaningful as a whole segment (`app/**/model.cln`). Written
//! inside a segment (`app/**x`) it degrades to `*`, which is what every other
//! glob implementation does and is less surprising than an error.
//!
//! # Why matching is on POSIX strings
//!
//! Discovery has already converted paths to project-relative POSIX form
//! (FRM-BO-06), and the request document carries them that way. Matching the
//! same strings means a pattern behaves identically on Windows and Unix —
//! matching `Path` components instead would make `app/*` depend on the
//! separator the host happens to use.

/// A compiled path glob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Glob {
    segments: Vec<Segment>,
    /// The pattern as written, for diagnostics.
    source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Segment {
    /// `**` — any number of whole segments.
    AnyDepth,
    /// A single segment, matched literally or with `*`/`?`.
    Single(Vec<Token>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Literal(String),
    /// `*` — any run of characters, not crossing a separator.
    Star,
    /// `?` — exactly one character.
    Any,
}

impl Glob {
    /// Compile a pattern. Never fails: every string is a valid glob, and a
    /// pattern that matches nothing is the developer's answer to give, not a
    /// build error.
    pub fn new(pattern: &str) -> Self {
        // A trailing `/` means "this directory" and is how people habitually
        // write folder patterns. Keeping it would produce a trailing empty
        // segment that matches nothing.
        let trimmed = pattern.trim_end_matches('/');

        let segments = trimmed
            .split('/')
            .map(|segment| {
                if segment == "**" {
                    Segment::AnyDepth
                } else {
                    Segment::Single(tokenize(segment))
                }
            })
            .collect();

        Glob { segments, source: pattern.to_string() }
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Does this glob match `path` exactly?
    ///
    /// `path` is a project-relative POSIX path with no leading slash.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        matches_from(&self.segments, &parts)
    }

    /// Does this glob match `path` **or anything beneath it**?
    ///
    /// This is what a *folder* pattern means. `[folders]` maps a folder to the
    /// libraries in scope there, and `app/server/**` names a subtree — but so
    /// does `app/server`, because a developer writing a folder name means the
    /// folder's contents, not a file of that exact name.
    ///
    /// Used for discovery roots and for `[build].exclude`, where excluding a
    /// directory must exclude what is inside it.
    pub fn matches_prefix(&self, path: &str) -> bool {
        if self.matches(path) {
            return true;
        }

        // Try the pattern against every ancestor: `app/server/api.cln` is
        // covered by `app/server` because the ancestor `app/server` matches.
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        (1..parts.len()).any(|end| matches_from(&self.segments, &parts[..end]))
    }

    /// The longest leading run of literal segments, as a path.
    ///
    /// `app/server/**` yields `app/server`; `**/model.cln` yields `""`. This
    /// is what lets a `[folders]` pattern become a discovery *root* — walking
    /// from the literal prefix visits only the subtree that can possibly
    /// match, instead of the whole project.
    pub fn literal_prefix(&self) -> String {
        let mut prefix = Vec::new();

        for segment in &self.segments {
            match segment {
                Segment::Single(tokens) => {
                    // One token, and it is literal: still a fixed directory
                    // name. Anything else introduces a wildcard and the walk
                    // has to start here.
                    match tokens.as_slice() {
                        [Token::Literal(name)] => prefix.push(name.clone()),
                        _ => break,
                    }
                }
                Segment::AnyDepth => break,
            }
        }

        prefix.join("/")
    }
}

fn tokenize(segment: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut literal = String::new();

    for ch in segment.chars() {
        match ch {
            '*' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                // Collapse `**` inside a segment to one `*` — see module docs.
                if tokens.last() != Some(&Token::Star) {
                    tokens.push(Token::Star);
                }
            }
            '?' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(Token::Any);
            }
            other => literal.push(other),
        }
    }

    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }

    tokens
}

/// Match `segments` against `parts`, both consumed left to right.
///
/// Recursive only on `**`, which is the one construct that needs to try
/// several splits. Depth is bounded by the number of `**` in the pattern, not
/// by the path length, so a deep tree cannot exhaust the stack.
fn matches_from(segments: &[Segment], parts: &[&str]) -> bool {
    match segments.split_first() {
        // Both exhausted: a match. Segments left over: no.
        None => parts.is_empty(),

        Some((Segment::AnyDepth, rest)) => {
            // `**` matches zero or more whole segments, so try every split —
            // including consuming nothing, which is what makes `a/**/b` match
            // `a/b`.
            (0..=parts.len()).any(|skip| matches_from(rest, &parts[skip..]))
        }

        Some((Segment::Single(tokens), rest)) => match parts.split_first() {
            None => false,
            Some((part, remaining)) => {
                matches_segment(tokens, part) && matches_from(rest, remaining)
            }
        },
    }
}

/// Match one segment's tokens against one path component.
///
/// Iterative with backtracking on `*` rather than recursive: a pathological
/// pattern like `*a*a*a*a*` against a long name would otherwise be both
/// exponential and stack-hungry. This is the standard single-pointer
/// backtracking walk, which is linear for every pattern that occurs in
/// practice and never recurses.
fn matches_segment(tokens: &[Token], part: &str) -> bool {
    let chars: Vec<char> = part.chars().collect();

    let (mut t, mut c) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have consumed too little.
    let mut star: Option<(usize, usize)> = None;

    while c < chars.len() {
        match tokens.get(t) {
            Some(Token::Literal(text)) => {
                let literal: Vec<char> = text.chars().collect();
                if chars[c..].starts_with(&literal) {
                    t += 1;
                    c += literal.len();
                    continue;
                }
            }
            Some(Token::Any) => {
                t += 1;
                c += 1;
                continue;
            }
            Some(Token::Star) => {
                // Try consuming nothing first, and remember to come back and
                // consume one more character if the rest fails.
                star = Some((t, c));
                t += 1;
                continue;
            }
            None => {}
        }

        // Mismatch. Back up to the last `*` and let it take one more character.
        match star {
            Some((star_t, star_c)) => {
                star = Some((star_t, star_c + 1));
                t = star_t + 1;
                c = star_c + 1;
                if star_c + 1 > chars.len() {
                    return false;
                }
            }
            None => return false,
        }
    }

    // Input exhausted: anything left must be `*`, which can match empty.
    tokens[t..].iter().all(|token| matches!(token, Token::Star))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, path: &str) -> bool {
        Glob::new(pattern).matches(path)
    }

    #[test]
    fn literal_patterns_match_exactly() {
        assert!(m("app/main.cln", "app/main.cln"));
        assert!(!m("app/main.cln", "app/other.cln"));
        assert!(!m("app/main.cln", "app/deep/main.cln"));
    }

    #[test]
    fn star_stays_within_one_segment() {
        // The distinction that makes `*` and `**` different things.
        assert!(m("app/*.cln", "app/main.cln"));
        assert!(!m("app/*.cln", "app/server/main.cln"));
        assert!(m("app/*", "app/server"));
        assert!(!m("app/*", "app/server/api.cln"));
    }

    #[test]
    fn doublestar_spans_any_number_of_segments() {
        assert!(m("app/**", "app/main.cln"));
        assert!(m("app/**", "app/server/deep/api.cln"));
        // Zero segments: `a/**/b` must match `a/b`.
        assert!(m("app/**/main.cln", "app/main.cln"));
        assert!(m("app/**/main.cln", "app/a/b/c/main.cln"));
        assert!(!m("app/**/main.cln", "app/a/other.cln"));
    }

    #[test]
    fn a_bare_doublestar_matches_everything() {
        assert!(m("**", "main.cln"));
        assert!(m("**", "a/b/c/d.cln"));
    }

    #[test]
    fn question_mark_matches_exactly_one_character() {
        assert!(m("app/v?.cln", "app/v1.cln"));
        assert!(!m("app/v?.cln", "app/v12.cln"));
        assert!(!m("app/v?.cln", "app/v.cln"));
        assert!(!m("app/v?.cln", "app/v/1.cln"));
    }

    #[test]
    fn stars_backtrack_correctly() {
        // The case naive left-to-right matching gets wrong: the first `*` must
        // give characters back for the literal to land.
        assert!(m("*.ui.cln", "button.ui.cln"));
        assert!(m("*a*b*", "xxaxxbxx"));
        assert!(!m("*a*b*", "xxbxxaxx"));
        assert!(m("a*a", "aa"));
        assert!(m("a*a", "abababa"));
        assert!(!m("a*a", "ab"));
    }

    #[test]
    fn a_pathological_pattern_terminates() {
        // Exponential backtracking would hang here rather than fail a test,
        // which is why the segment matcher is iterative.
        let pattern = "*a*a*a*a*a*a*a*a*b";
        let path = "a".repeat(64);
        assert!(!Glob::new(pattern).matches(&path));
    }

    #[test]
    fn a_trailing_slash_still_names_the_folder() {
        // People write `app/server/` habitually; it must not silently match
        // nothing.
        assert!(Glob::new("app/server/").matches("app/server"));
        assert!(Glob::new("app/server/").matches_prefix("app/server/api.cln"));
    }

    #[test]
    fn prefix_matching_covers_a_subtree() {
        // What a *folder* pattern means: naming a folder means its contents.
        let glob = Glob::new("app/server");
        assert!(glob.matches_prefix("app/server"));
        assert!(glob.matches_prefix("app/server/api.cln"));
        assert!(glob.matches_prefix("app/server/deep/nested.cln"));
        assert!(!glob.matches_prefix("app/client/api.cln"));
        // A sibling whose name merely starts the same way is not inside it.
        assert!(!glob.matches_prefix("app/server-extra/api.cln"));
    }

    #[test]
    fn prefix_matching_works_with_wildcards() {
        let glob = Glob::new("app/*/handlers");
        assert!(glob.matches_prefix("app/server/handlers"));
        assert!(glob.matches_prefix("app/server/handlers/get.cln"));
        assert!(!glob.matches_prefix("app/server/models/user.cln"));
    }

    #[test]
    fn literal_prefix_finds_where_to_start_walking() {
        // This is what turns a pattern into a discovery root: walk the fixed
        // part, not the whole project.
        assert_eq!(Glob::new("app/server/**").literal_prefix(), "app/server");
        assert_eq!(Glob::new("app/server/api.cln").literal_prefix(), "app/server/api.cln");
        assert_eq!(Glob::new("app/*/handlers").literal_prefix(), "app");
        assert_eq!(Glob::new("**/model.cln").literal_prefix(), "");
        assert_eq!(Glob::new("*.cln").literal_prefix(), "");
    }

    #[test]
    fn the_source_is_kept_for_diagnostics() {
        // A message about a pattern must quote what the developer wrote, not
        // a normalized form they would not recognize.
        assert_eq!(Glob::new("app/server/").as_str(), "app/server/");
    }

    #[test]
    fn matching_is_separator_agnostic_by_construction() {
        // Discovery hands us POSIX strings on every platform (FRM-BO-06), so a
        // pattern behaves the same everywhere. Guard the assumption.
        assert!(m("app/server/**", "app/server/deep/api.cln"));
    }
}

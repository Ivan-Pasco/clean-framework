//! `host_bridge.cln` — a library's host contract, and the WIT synthesized from
//! it (LBS §8, rule LBS-02).
//!
//! A Clean library never ships hand-written WIT. It declares every host import
//! it needs as a typed `host function` inside a `host interface` block, in one
//! file at the library root, and the framework mechanically derives the WIT
//! package `clean:library/<name>@<version>` from those declarations (§8.1).
//! That derived WIT is what reaches the compiler in `library_manifests[].wit`.
//!
//! # Why the framework parses Clean here
//!
//! The compiler is the authority on Clean syntax everywhere else, so a second
//! parser in the framework deserves justification. The reason is ordering: the
//! WIT has to exist *before* the compiler runs, because it is an input to the
//! compilation, and the compiler is the thing that would otherwise produce it.
//! Handing the compiler a request whose WIT it must first compute would invert
//! the seam — the framework assembles a self-contained request (FRM-BO-02,
//! CMP-01), and "self-contained" has to include the library contracts.
//!
//! What keeps the duplication honest is how little of Clean this reads. It
//! parses one file shape — `host interface`, `requires host worlds`, and
//! `host function` declarations with a mandatory `description` — and refuses
//! everything else. It does not parse expressions, statements, or bodies,
//! because a `host_bridge.cln` has none: LIB019 makes this file *only*
//! declarations. If the language grows records or resources (§8.1), the type
//! mapping below grows; the file shape does not.
//!
//! # The three checks this enables
//!
//! - **LIB019** — declarations outside `host_bridge.cln` are refused. Checked
//!   by [`scan_for_misplaced`], because the rule is about the files that
//!   *don't* hold declarations and so cannot be checked by parsing the one
//!   that does.
//! - **LIB012** — a library whose `requires host worlds` omits the project's
//!   target world cannot be built into it. Checked by [`HostInterface::binds`].
//! - **SYN005-equivalent** — a `host function` without a `description` is
//!   refused here rather than reaching the compiler, so the diagnostic names
//!   the library file the author actually wrote.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The one file in a library that may carry host declarations (§8.2, LIB019).
pub const HOST_BRIDGE_FILE: &str = "host_bridge.cln";

/// The world names a `requires host worlds` list may name (Platform 15 §0.3).
///
/// Closed on purpose. A typo like `"servers"` would otherwise produce a
/// library that binds to no world at all and fails later as "unbound", which
/// reads as a host problem rather than a spelling one.
///
/// Derived from [`crate::world::WORLDS`] rather than listed again here. It was
/// a fixed-size `[&str; 4]`, which made adding a world a *type* change — and
/// until that change was made, a library naming the new world failed to
/// **parse**, blaming the library author for a gap in this crate.
pub fn known_worlds() -> Vec<&'static str> {
    crate::world::names()
}

/// One `host interface` block: the whole content of a `host_bridge.cln`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostInterface {
    /// Lowercase-kebab interface name (`ui`, `canvas`, `storage`).
    pub name: String,
    /// The interface's own version, independent of the library's (Platform 08).
    pub version: String,
    /// Non-empty; validated against [`known_worlds`].
    pub worlds: Vec<String>,
    pub functions: Vec<HostFunction>,
}

/// One `host function` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFunction {
    /// As written in Clean — camelCase by convention (`setInnerHTML`).
    pub name: String,
    pub params: Vec<Param>,
    /// `None` for a declaration with no `returns` clause.
    pub returns: Option<CleanType>,
    /// Mandatory (LBS-02). Becomes the WIT doc comment and MCP hover text.
    pub description: String,
    /// 1-based line of the `host function` keyword, for diagnostics.
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: CleanType,
}

/// The subset of Clean's type grammar a host declaration may use (§8.3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CleanType {
    String,
    /// `integer`, or `integer:32` — the width names what the WIT side uses.
    Integer { width: IntWidth },
    Number,
    Boolean,
    Bytes,
    List(Box<CleanType>),
    Pairs(Box<CleanType>, Box<CleanType>),
    /// `T?` — lowered to `option<T>`.
    Optional(Box<CleanType>),
    /// A Clean function passed as a callback (§8.6). Crosses the bridge as the
    /// integer index the compiler assigns, never as a function reference.
    Handler,
    /// A name this module does not know: a user-defined `class`, which §8.3
    /// maps to a WIT `record`. Carried through as a kebab-cased type name
    /// rather than rejected, because the record's *shape* is the compiler's to
    /// resolve — the framework only needs the name to write the signature.
    Named(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntWidth {
    /// Plain `integer` → `s64`.
    S64,
    /// `integer:32` → `s32`.
    S32,
}

impl CleanType {
    /// The WIT spelling (§8.3 type-mapping table).
    pub fn to_wit(&self) -> String {
        match self {
            CleanType::String => "string".into(),
            CleanType::Integer { width: IntWidth::S64 } => "s64".into(),
            CleanType::Integer { width: IntWidth::S32 } => "s32".into(),
            CleanType::Number => "float64".into(),
            CleanType::Boolean => "bool".into(),
            CleanType::Bytes => "list<u8>".into(),
            CleanType::List(inner) => format!("list<{}>", inner.to_wit()),
            CleanType::Pairs(k, v) => {
                format!("list<tuple<{}, {}>>", k.to_wit(), v.to_wit())
            }
            CleanType::Optional(inner) => format!("option<{}>", inner.to_wit()),
            // §8.6: the host sees the index, not the function.
            CleanType::Handler => "u32".into(),
            CleanType::Named(name) => kebab(name),
        }
    }

    /// Parse one type expression. `depth` bounds nesting so a pathological
    /// `list<list<list<…>>>` cannot recurse the parser into a stack overflow
    /// — this input comes off disk from a dependency, not from us.
    fn parse(raw: &str, depth: usize) -> Result<CleanType, String> {
        const MAX_DEPTH: usize = 8;
        if depth > MAX_DEPTH {
            return Err(format!("type nests deeper than {MAX_DEPTH} levels"));
        }

        let raw = raw.trim();
        if raw.is_empty() {
            return Err("expected a type".into());
        }

        // `T?` binds loosest, so it is peeled first.
        if let Some(inner) = raw.strip_suffix('?') {
            return Ok(CleanType::Optional(Box::new(CleanType::parse(inner, depth + 1)?)));
        }

        if let Some(rest) = raw.strip_prefix("list<") {
            let inner = rest
                .strip_suffix('>')
                .ok_or_else(|| format!("unclosed `list<` in type `{raw}`"))?;
            return Ok(CleanType::List(Box::new(CleanType::parse(inner, depth + 1)?)));
        }

        if let Some(rest) = raw.strip_prefix("pairs<") {
            let inner = rest
                .strip_suffix('>')
                .ok_or_else(|| format!("unclosed `pairs<` in type `{raw}`"))?;
            let (k, v) = split_top_level_comma(inner)
                .ok_or_else(|| format!("`pairs<K, V>` needs two type arguments, got `{inner}`"))?;
            return Ok(CleanType::Pairs(
                Box::new(CleanType::parse(k, depth + 1)?),
                Box::new(CleanType::parse(v, depth + 1)?),
            ));
        }

        Ok(match raw {
            "string" => CleanType::String,
            "integer" => CleanType::Integer { width: IntWidth::S64 },
            "integer:32" => CleanType::Integer { width: IntWidth::S32 },
            "number" => CleanType::Number,
            "boolean" => CleanType::Boolean,
            "bytes" => CleanType::Bytes,
            "handler" => CleanType::Handler,
            other => {
                // A width suffix on anything but `integer` is a
                // misunderstanding of ADR-0019 worth naming precisely.
                if let Some((base, width)) = other.split_once(':') {
                    return Err(format!(
                        "width suffix `:{width}` is only valid on `integer`, not `{base}`"
                    ));
                }
                if !is_identifier(other) {
                    return Err(format!("`{other}` is not a valid type name"));
                }
                CleanType::Named(other.to_string())
            }
        })
    }
}

/// Split `K, V` at the comma that is not inside angle brackets.
fn split_top_level_comma(input: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in input.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some((&input[..i], &input[i + 1..])),
            _ => {}
        }
    }
    None
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `setInnerHTML` → `set-inner-html`, the WIT spelling (§8.7).
///
/// Runs of capitals are kept together, so `setInnerHTML` yields
/// `set-inner-html` rather than `set-inner-h-t-m-l`, and `queryDOM2Node`
/// yields `query-dom2-node`. WIT identifiers are lowercase-kebab, so this
/// mapping has to be total — every accepted Clean name must produce a legal
/// WIT name.
fn kebab(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);

    for i in 0..chars.len() {
        let c = chars[i];

        if c == '_' || c == '-' {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
            continue;
        }

        if c.is_ascii_uppercase() {
            let prev_lower = i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
            // The tail of an acronym, where the next char starts a new word:
            // the `L` in `HTMLPanel`.
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_ascii_lowercase();
            let prev_upper = i > 0 && chars[i - 1].is_ascii_uppercase();

            if !out.is_empty() && !out.ends_with('-') && (prev_lower || (prev_upper && next_lower)) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            continue;
        }

        out.push(c);
    }

    out
}

#[derive(Debug, thiserror::Error)]
pub enum HostBridgeError {
    #[error("could not read {}: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// LIB019: the rule is about *location*, so the diagnostic names the file
    /// that should not have held the declaration.
    #[error(
        "{}:{line}: host declarations are only allowed in {HOST_BRIDGE_FILE}",
        .path.display()
    )]
    Misplaced { path: PathBuf, line: usize },

    #[error("{}:{line}: {message}", .path.display())]
    Syntax { path: PathBuf, line: usize, message: String },

    /// LIB012, checked at build time against the project's target world.
    #[error(
        "library '{library}' declares host interface '{interface}' for worlds [{}], \
         but this project targets world '{target}'",
        .worlds.join(", ")
    )]
    Unbound {
        library: String,
        interface: String,
        worlds: Vec<String>,
        target: String,
    },
}

impl HostBridgeError {
    /// The Platform 09 registry code. Libraries own no prefix of their own
    /// (§1), so these travel as the framework's config/library codes.
    pub fn code(&self) -> &'static str {
        match self {
            HostBridgeError::Unreadable { .. } => "CFG003",
            HostBridgeError::Misplaced { .. } => "LIB019",
            HostBridgeError::Syntax { .. } => "CFG001",
            HostBridgeError::Unbound { .. } => "LIB012",
        }
    }

    pub fn help(&self) -> Option<String> {
        match self {
            HostBridgeError::Misplaced { .. } => Some(format!(
                "move the `host interface` block into {HOST_BRIDGE_FILE} at the library root"
            )),
            HostBridgeError::Unbound { worlds, target, .. } => Some(format!(
                "either add \"{target}\" to `requires host worlds [{}]`, \
                 or drop this library from a {target} build",
                worlds.join(", ")
            )),
            HostBridgeError::Syntax { .. } => Some(
                "see LBS-02: `host function name(p: type) returns type` \
                 followed by an indented `description \"…\"`"
                    .into(),
            ),
            HostBridgeError::Unreadable { .. } => None,
        }
    }
}

impl HostInterface {
    /// Load and parse the `host_bridge.cln` at a library root.
    ///
    /// `Ok(None)` when the file is absent — most libraries import nothing from
    /// the host, and that is not an error.
    pub fn load(library_root: &Path) -> Result<Option<HostInterface>, HostBridgeError> {
        let path = library_root.join(HOST_BRIDGE_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(HostBridgeError::Unreadable { path, source }),
        };
        Self::parse(&text, &path).map(Some)
    }

    /// LIB012: may this interface be built into a component targeting `world`?
    pub fn binds(&self, world: &str) -> bool {
        self.worlds.iter().any(|w| w == world)
    }

    /// Parse the declarations in one `host_bridge.cln`.
    pub fn parse(text: &str, path: &Path) -> Result<HostInterface, HostBridgeError> {
        let syntax = |line: usize, message: String| HostBridgeError::Syntax {
            path: path.to_path_buf(),
            line,
            message,
        };

        let mut header: Option<(String, String, usize)> = None;
        let mut worlds: Vec<String> = Vec::new();
        let mut functions: Vec<HostFunction> = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = strip_comment(raw).trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("host interface") {
                if header.is_some() {
                    // §8.3: one interface per file. Two would make the
                    // synthesized package ambiguous.
                    return Err(syntax(line, "a second `host interface` in one file; \
                                             LBS-02 allows exactly one".into()));
                }
                header = Some(parse_header(rest, line).map_err(|m| syntax(line, m))?);
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("requires host worlds") {
                if header.is_none() {
                    return Err(syntax(
                        line,
                        "`requires host worlds` before any `host interface`".into(),
                    ));
                }
                if !worlds.is_empty() {
                    return Err(syntax(line, "duplicate `requires host worlds`".into()));
                }
                worlds = parse_worlds(rest).map_err(|m| syntax(line, m))?;
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("host function") {
                if header.is_none() {
                    return Err(syntax(
                        line,
                        "`host function` before any `host interface`".into(),
                    ));
                }
                let function = parse_function(rest, line).map_err(|m| syntax(line, m))?;
                functions.push(function);
                continue;
            }

            if let Some(rest) = trimmed.strip_prefix("description") {
                let Some(last) = functions.last_mut() else {
                    return Err(syntax(line, "`description` before any `host function`".into()));
                };
                if !last.description.is_empty() {
                    return Err(syntax(
                        line,
                        format!("`{}` already has a description", last.name),
                    ));
                }
                last.description = parse_quoted(rest).map_err(|m| syntax(line, m))?;
                continue;
            }

            return Err(syntax(
                line,
                format!("unexpected `{trimmed}` in {HOST_BRIDGE_FILE}"),
            ));
        }

        let Some((name, version, header_line)) = header else {
            return Err(syntax(1, format!("{HOST_BRIDGE_FILE} declares no `host interface`")));
        };

        if worlds.is_empty() {
            return Err(syntax(
                header_line,
                "`host interface` needs a non-empty `requires host worlds [...]`".into(),
            ));
        }

        // LBS-02 makes the description mandatory. Checked after the whole file
        // is read, because the description follows its function.
        if let Some(missing) = functions.iter().find(|f| f.description.is_empty()) {
            return Err(syntax(
                missing.line,
                format!("host function `{}` is missing its mandatory description", missing.name),
            ));
        }

        if let Some(duplicate) = first_duplicate(&functions) {
            return Err(syntax(
                duplicate.line,
                format!("host function `{}` is declared twice", duplicate.name),
            ));
        }

        Ok(HostInterface { name, version, worlds, functions })
    }

    /// Synthesize the WIT package this interface becomes (§8.1, §8.4).
    ///
    /// The package is named for the *library*, the interface for the `host
    /// interface` block — a library's WIT package is its identity on the wire,
    /// so it cannot be named for a block it may rename.
    pub fn to_wit(&self, library_name: &str, library_version: &str) -> String {
        let mut out = String::new();

        // The header says "generated" because §8.1 makes this a build artifact
        // that is never hand-edited; someone who finds it in a cache directory
        // should know not to fix it in place.
        let _ = writeln!(
            out,
            "// Generated from {HOST_BRIDGE_FILE} by clean-framework (LBS §8.1). Do not edit."
        );
        let _ = writeln!(
            out,
            "package clean:library/{}@{};",
            kebab(library_name),
            library_version
        );
        out.push('\n');
        let _ = writeln!(out, "interface {} {{", kebab(&self.name));

        for (i, function) in self.functions.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let _ = writeln!(out, "    /// {}", function.description);

            let params = function
                .params
                .iter()
                .map(|p| format!("{}: {}", kebab(&p.name), p.ty.to_wit()))
                .collect::<Vec<_>>()
                .join(", ");

            match &function.returns {
                Some(ret) => {
                    let _ = writeln!(
                        out,
                        "    {}: func({params}) -> {};",
                        kebab(&function.name),
                        ret.to_wit()
                    );
                }
                None => {
                    let _ = writeln!(out, "    {}: func({params});", kebab(&function.name));
                }
            }
        }

        out.push_str("}\n");
        out
    }
}

fn first_duplicate(functions: &[HostFunction]) -> Option<&HostFunction> {
    // Compared on the *WIT* name, not the Clean one: `setHTML` and `setHtml`
    // are distinct in Clean and collide in WIT, and the collision is what
    // breaks the build later.
    let mut seen = std::collections::BTreeSet::new();
    functions.iter().find(|f| !seen.insert(kebab(&f.name)))
}

/// Strip a `//` comment, ignoring one inside a string literal.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_string = !in_string,
            b'\\' if in_string => i += 1,
            b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// ` storage version "0.1.0":` → `("storage", "0.1.0")`.
fn parse_header(rest: &str, line: usize) -> Result<(String, String, usize), String> {
    let rest = rest.trim().strip_suffix(':').ok_or_else(|| {
        "`host interface` header must end with `:`".to_string()
    })?;

    let (name, version_part) = rest
        .split_once("version")
        .ok_or_else(|| "`host interface <name> version \"x.y.z\":` is the required form".to_string())?;

    let name = name.trim();
    if name.is_empty() {
        return Err("`host interface` needs a name".into());
    }
    // §8.3 requires lowercase-kebab. Enforced rather than silently normalized:
    // the name appears in the synthesized WIT, and a library author who wrote
    // `myUI` should be told it becomes `my-ui` rather than discovering it.
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!(
            "interface name `{name}` must be lowercase-kebab (LBS-02)"
        ));
    }

    let version = parse_quoted(version_part)?;
    if version.split('.').count() != 3 {
        return Err(format!("version \"{version}\" is not a three-part semver"));
    }

    Ok((name.to_string(), version, line))
}

/// ` ["server", "cli"]` → `["server", "cli"]`.
fn parse_worlds(rest: &str) -> Result<Vec<String>, String> {
    let rest = rest.trim();
    let inner = rest
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| "`requires host worlds [...]` needs a bracketed list".to_string())?;

    let mut worlds = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let world = parse_quoted(part)?;
        if !crate::world::is_known(&world) {
            return Err(format!(
                "`{world}` is not a host world; expected one of {}",
                known_worlds().join(", ")
            ));
        }
        if worlds.contains(&world) {
            return Err(format!("world `{world}` listed twice"));
        }
        worlds.push(world);
    }

    if worlds.is_empty() {
        return Err("`requires host worlds` list is empty".into());
    }
    Ok(worlds)
}

/// ` write(path: string, data: bytes) returns integer` → a [`HostFunction`].
fn parse_function(rest: &str, line: usize) -> Result<HostFunction, String> {
    let rest = rest.trim();

    let open = rest.find('(').ok_or_else(|| {
        "`host function` needs a parameter list, even if empty: `name()`".to_string()
    })?;
    let close = rest.rfind(')').ok_or_else(|| "unclosed parameter list".to_string())?;
    if close < open {
        return Err("unclosed parameter list".into());
    }

    let name = rest[..open].trim();
    if !is_identifier(name) {
        return Err(format!("`{name}` is not a valid function name"));
    }

    let mut params = Vec::new();
    let params_src = &rest[open + 1..close];
    for part in split_params(params_src) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (pname, pty) = part.split_once(':').ok_or_else(|| {
            format!("parameter `{part}` needs a type: `name: type`")
        })?;
        let pname = pname.trim();
        if !is_identifier(pname) {
            return Err(format!("`{pname}` is not a valid parameter name"));
        }
        if params.iter().any(|p: &Param| p.name == pname) {
            return Err(format!("parameter `{pname}` is declared twice"));
        }
        params.push(Param { name: pname.to_string(), ty: CleanType::parse(pty, 0)? });
    }

    let tail = rest[close + 1..].trim();
    let returns = if tail.is_empty() {
        None
    } else {
        let ty = tail.strip_prefix("returns").ok_or_else(|| {
            format!("expected `returns <type>` after the parameter list, found `{tail}`")
        })?;
        // §8.3: fallible functions return a normal type; `result<>` is the
        // compiler's lowering of `onError`, never written by hand.
        let ty = ty.trim();
        if ty.starts_with("result<") {
            return Err(
                "write the success type; `result<T, error>` is derived from `onError` (LBS-02)"
                    .into(),
            );
        }
        Some(CleanType::parse(ty, 0)?)
    };

    Ok(HostFunction {
        name: name.to_string(),
        params,
        returns,
        description: String::new(),
        line,
    })
}

/// Split a parameter list on commas that are not inside `<…>`, so
/// `pairs<string, integer>` stays one parameter.
fn split_params(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in input.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

/// Pull the contents of the first `"…"` out of `input`.
fn parse_quoted(input: &str) -> Result<String, String> {
    let input = input.trim();
    let open = input.find('"').ok_or_else(|| format!("expected a quoted string, found `{input}`"))?;
    let rest = &input[open + 1..];
    let close = rest.find('"').ok_or_else(|| "unterminated string literal".to_string())?;

    let trailing = rest[close + 1..].trim().trim_end_matches(':').trim();
    if !trailing.is_empty() {
        return Err(format!("unexpected `{trailing}` after the quoted string"));
    }

    Ok(rest[..close].to_string())
}

/// LIB019: refuse host declarations in any library file but `host_bridge.cln`.
///
/// Takes the files rather than walking the tree itself, because discovery
/// already knows which files belong to the library and a second walk could
/// disagree with the first.
pub fn scan_for_misplaced<'a>(
    files: impl IntoIterator<Item = (&'a Path, &'a str)>,
) -> Result<(), HostBridgeError> {
    for (path, contents) in files {
        if path.file_name().is_some_and(|n| n == HOST_BRIDGE_FILE) {
            continue;
        }
        for (index, raw) in contents.lines().enumerate() {
            let trimmed = strip_comment(raw).trim();
            if trimmed.starts_with("host interface") || trimmed.starts_with("host function") {
                return Err(HostBridgeError::Misplaced {
                    path: path.to_path_buf(),
                    line: index + 1,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> PathBuf {
        PathBuf::from("mylib/host_bridge.cln")
    }

    fn parse(text: &str) -> Result<HostInterface, HostBridgeError> {
        HostInterface::parse(text, &at())
    }

    /// The `frame.storage` surface (STOR-01) — the first library we ship, and
    /// the one this whole module exists to make buildable.
    const STORAGE: &str = r#"
// frame.storage — host bridge
host interface storage version "0.1.0":
	requires host worlds ["server"]

	host function write(path: string, data: bytes) returns integer
		description "Write bytes under the storage root. Returns 0 on success."

	host function read(path: string) returns bytes
		description "Read bytes back. Empty when the file is missing."

	host function exists(path: string) returns boolean
		description "Test for existence without reading the bytes."

	host function delete(path: string) returns boolean
		description "Remove a file. True on success."
"#;

    #[test]
    fn parses_the_storage_surface() {
        let iface = parse(STORAGE).unwrap();
        assert_eq!(iface.name, "storage");
        assert_eq!(iface.version, "0.1.0");
        assert_eq!(iface.worlds, vec!["server"]);
        assert_eq!(iface.functions.len(), 4);

        let write = &iface.functions[0];
        assert_eq!(write.name, "write");
        assert_eq!(write.params.len(), 2);
        assert_eq!(write.params[1].ty, CleanType::Bytes);
        assert_eq!(write.returns, Some(CleanType::Integer { width: IntWidth::S64 }));
    }

    #[test]
    fn synthesizes_the_wit_package_for_storage() {
        let wit = parse(STORAGE).unwrap().to_wit("frame.storage", "0.1.0");

        assert!(wit.contains("package clean:library/frame.storage@0.1.0;"), "{wit}");
        assert!(wit.contains("interface storage {"), "{wit}");
        assert!(wit.contains("write: func(path: string, data: list<u8>) -> s64;"), "{wit}");
        assert!(wit.contains("read: func(path: string) -> list<u8>;"), "{wit}");
        assert!(wit.contains("exists: func(path: string) -> bool;"), "{wit}");
        assert!(wit.contains("delete: func(path: string) -> bool;"), "{wit}");
        // The description is the doc comment (§8.3).
        assert!(wit.contains("/// Write bytes under the storage root."), "{wit}");
    }

    /// §8.4 prints the expected output for `ui`; matching it exactly is the
    /// cheapest guard against the synthesizer drifting from the spec.
    #[test]
    fn matches_the_specs_worked_ui_example() {
        let source = r#"
host interface ui version "0.1.0":
	requires host worlds ["browser"]

	host function setInnerHTML(selector: string, html: string) returns integer
		description "Replace inner HTML of first matching element."

	host function onEvent(selector: string, event: string, handlerName: string) returns integer
		description "Attach a delegated event listener."
"#;
        let wit = parse(source).unwrap().to_wit("ui", "0.1.0");

        assert!(
            wit.contains("set-inner-html: func(selector: string, html: string) -> s64;"),
            "{wit}"
        );
        assert!(
            wit.contains(
                "on-event: func(selector: string, event: string, handler-name: string) -> s64;"
            ),
            "{wit}"
        );
    }

    #[test]
    fn maps_every_type_in_the_spec_table() {
        let cases = [
            ("string", "string"),
            ("integer", "s64"),
            ("integer:32", "s32"),
            ("number", "float64"),
            ("boolean", "bool"),
            ("bytes", "list<u8>"),
            ("list<string>", "list<string>"),
            ("pairs<string, integer>", "list<tuple<string, s64>>"),
            ("string?", "option<string>"),
            ("list<bytes>", "list<list<u8>>"),
        ];
        for (clean, wit) in cases {
            assert_eq!(CleanType::parse(clean, 0).unwrap().to_wit(), wit, "{clean}");
        }
    }

    /// §8.6: a `handler` crosses as the index the compiler assigns, so the WIT
    /// must say `u32` — a function type would make the host responsible for
    /// Clean's calling convention.
    #[test]
    fn a_handler_parameter_crosses_as_an_index() {
        let source = r#"
host interface ui version "0.1.0":
	requires host worlds ["browser"]

	host function onEvent(selector: string, callback: handler) returns integer
		description "Attach a listener."
"#;
        let wit = parse(source).unwrap().to_wit("ui", "0.1.0");
        assert!(wit.contains("on-event: func(selector: string, callback: u32) -> s64;"), "{wit}");
    }

    #[test]
    fn a_missing_description_is_refused_by_name() {
        let source = r#"
host interface storage version "0.1.0":
	requires host worlds ["server"]

	host function write(path: string) returns integer
"#;
        let error = parse(source).unwrap_err();
        assert_eq!(error.code(), "CFG001");
        assert!(error.to_string().contains("write"), "{error}");
        assert!(error.to_string().contains("mandatory description"), "{error}");
    }

    #[test]
    fn a_function_with_no_return_clause_is_a_void_wit_function() {
        let source = r#"
host interface log version "0.1.0":
	requires host worlds ["server"]

	host function emit(message: string)
		description "Emit a log line."
"#;
        let iface = parse(source).unwrap();
        assert_eq!(iface.functions[0].returns, None);
        assert!(iface.to_wit("log", "0.1.0").contains("emit: func(message: string);"));
    }

    #[test]
    fn an_unknown_world_names_the_valid_ones() {
        let source = r#"
host interface storage version "0.1.0":
	requires host worlds ["servers"]

	host function write(path: string) returns integer
		description "d"
"#;
        let error = parse(source).unwrap_err();
        assert!(error.to_string().contains("servers"), "{error}");
        assert!(error.to_string().contains("server"), "{error}");
    }

    /// LBS-02 forbids a hand-written `result<>`; the mistake is common enough
    /// that the message points at `onError` rather than just refusing.
    #[test]
    fn a_hand_written_result_type_points_at_on_error() {
        let source = r#"
host interface storage version "0.1.0":
	requires host worlds ["server"]

	host function write(path: string) returns result<integer, error>
		description "d"
"#;
        let error = parse(source).unwrap_err();
        assert!(error.to_string().contains("onError"), "{error}");
    }

    #[test]
    fn two_interfaces_in_one_file_are_refused() {
        let source = r#"
host interface a version "0.1.0":
	requires host worlds ["server"]

host interface b version "0.1.0":
	requires host worlds ["server"]
"#;
        let error = parse(source).unwrap_err();
        assert!(error.to_string().contains("exactly one"), "{error}");
    }

    /// Distinct in Clean, identical in WIT. Caught here, where the message can
    /// name both, rather than in a WIT parser that sees only the collision.
    #[test]
    fn two_functions_that_kebab_to_the_same_wit_name_collide() {
        let source = r#"
host interface ui version "0.1.0":
	requires host worlds ["browser"]

	host function setHTML(s: string) returns integer
		description "d"

	host function setHtml(s: string) returns integer
		description "d"
"#;
        let error = parse(source).unwrap_err();
        assert!(error.to_string().contains("declared twice"), "{error}");
    }

    #[test]
    fn a_width_suffix_on_a_non_integer_is_named_precisely() {
        let error = CleanType::parse("string:32", 0).unwrap_err();
        assert!(error.contains("only valid on `integer`"), "{error}");
    }

    #[test]
    fn deeply_nested_types_are_bounded_rather_than_overflowing() {
        let deep = format!("{}string{}", "list<".repeat(30), ">".repeat(30));
        assert!(CleanType::parse(&deep, 0).is_err());
    }

    #[test]
    fn binds_answers_the_lib012_question() {
        let iface = parse(STORAGE).unwrap();
        assert!(iface.binds("server"));
        assert!(!iface.binds("browser"));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored_but_strings_keep_their_slashes() {
        let source = r#"
// leading comment
host interface storage version "0.1.0":
	requires host worlds ["server"]   // trailing comment

	host function write(path: string) returns integer
		description "Writes to a//b. Returns 0."
"#;
        let iface = parse(source).unwrap();
        assert_eq!(iface.functions[0].description, "Writes to a//b. Returns 0.");
    }

    #[test]
    fn lib019_names_the_file_that_should_not_hold_the_declaration() {
        let stray = PathBuf::from("mylib/src/util.cln");
        let bridge = PathBuf::from("mylib/host_bridge.cln");

        let error = scan_for_misplaced([
            (bridge.as_path(), "host interface storage version \"0.1.0\":\n"),
            (stray.as_path(), "\nhost function sneaky(a: string)\n"),
        ])
        .unwrap_err();

        assert_eq!(error.code(), "LIB019");
        assert!(error.to_string().contains("util.cln"), "{error}");
        assert!(error.help().unwrap().contains(HOST_BRIDGE_FILE));
    }

    #[test]
    fn a_commented_out_declaration_does_not_trip_lib019() {
        let stray = PathBuf::from("mylib/src/util.cln");
        assert!(scan_for_misplaced([(
            stray.as_path(),
            "// host function sneaky(a: string)\n"
        )])
        .is_ok());
    }

    #[test]
    fn kebab_keeps_acronyms_together() {
        assert_eq!(kebab("setInnerHTML"), "set-inner-html");
        assert_eq!(kebab("onEvent"), "on-event");
        assert_eq!(kebab("write"), "write");
        assert_eq!(kebab("HTMLPanel"), "html-panel");
        assert_eq!(kebab("frame.storage"), "frame.storage");
        assert_eq!(kebab("read_bytes"), "read-bytes");
    }
}

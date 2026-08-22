//! The tool surface (Framework §10.2).
//!
//! Every tool the toolchain exposes is served here, because §10.1 makes the
//! framework MCP the only MCP server in the toolchain. What differs between
//! tools is where the *data* comes from:
//!
//! - **Project-backed** (§10.2.2) — answered from framework state:
//!   `clean.toml`, the host WIT cache, the resolved closure.
//! - **Language-backed** (§10.2.1) — forwarded to the compiler's language-info
//!   API, because §10.7 Rule 1 forbids the framework answering a language
//!   question from its own memory.
//!
//! The AI cannot tell which is which, and should not need to (§10.8).
//!
//! # Why the language-backed tools are declared but cannot answer
//!
//! No shipped compiler implements the language-info API. Those tools are still
//! *declared*, so the tool list matches the spec and the compiler team can see
//! exactly what it must serve — and each one fails with the precise `cln
//! --json` call it attempted. Hiding them until the compiler lands would make
//! the contract invisible to the people who have to implement it; faking them
//! would violate Rule 1 silently, which is worse than failing.

use serde_json::{json, Value};

use crate::language::{LanguageInfo, Query};
use crate::project::Project;
use crate::protocol::ToolOutcome;

/// Where a tool's data comes from (§10.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    /// Framework state (§10.2.2).
    Project,
    /// The compiler's language-info API (§10.2.1).
    Language,
}

/// One tool, as advertised in `tools/list`.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub source: Source,
    /// JSON Schema for the arguments. `{}` for a tool that takes none.
    pub schema: fn() -> Value,
}

fn no_arguments() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn source_argument() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "description": "Self-contained Clean source. Not a path — the compiler reads nothing from disk (CMP-01)."
            }
        },
        "required": ["source"]
    })
}

/// The tools this server serves.
///
/// Ordered language-backed first, then project-backed, matching §10.2.1 and
/// §10.2.2 — a reader comparing this list against the spec should be able to
/// go top to bottom.
pub const TOOLS: &[Tool] = &[
    // ---- §10.2.1 language-backed -----------------------------------------
    Tool {
        name: "get_quick_reference",
        description: "Concise syntax and core patterns for the current compiler version.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "get_specification",
        description: "Full language specification section lookup.",
        source: Source::Language,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "section": {
                        "type": "string",
                        "description": "Section to fetch, e.g. \"types\". Omit for the index."
                    }
                }
            })
        },
    },
    Tool {
        name: "list_builtins",
        description: "Built-in functions with signatures.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "list_types",
        description: "Primitive and built-in complex types.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "list_functions",
        description: "Stdlib functions with signatures and categories.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "list_error_codes",
        description: "The numbered error taxonomy.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "explain_error",
        description: "Human explanation for a specific error code, e.g. SEM021.",
        source: Source::Language,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Diagnostic code, e.g. \"SEM021\"." }
                },
                "required": ["code"]
            })
        },
    },
    Tool {
        name: "get_changelog",
        description: "What changed between compiler versions.",
        source: Source::Language,
        schema: no_arguments,
    },
    Tool {
        name: "check",
        description: "Fast typecheck of a self-contained snippet. No WASM emission.",
        source: Source::Language,
        schema: source_argument,
    },
    Tool {
        name: "parse",
        description: "Parse a snippet and return its AST.",
        source: Source::Language,
        schema: source_argument,
    },
    Tool {
        name: "validate",
        description: "Validate that a snippet conforms to the language rules.",
        source: Source::Language,
        schema: source_argument,
    },
    Tool {
        name: "format",
        description: "Format Clean source in canonical style.",
        source: Source::Language,
        schema: source_argument,
    },
    Tool {
        name: "compile",
        description: "Compile a self-contained snippet to WASM.",
        source: Source::Language,
        schema: source_argument,
    },
    // ---- §10.2.2 project-backed ------------------------------------------
    Tool {
        name: "get_target_host",
        description: "Which host and version this project targets, and the world it builds against.",
        source: Source::Project,
        schema: no_arguments,
    },
    Tool {
        name: "get_host_capabilities",
        description:
            "Interfaces and functions the target host provides, read from the cached host.wit.",
        source: Source::Project,
        schema: no_arguments,
    },
    Tool {
        name: "list_libraries",
        description: "Libraries and plugins active in this project, from the resolved closure.",
        source: Source::Project,
        schema: no_arguments,
    },
    Tool {
        name: "get_app_structure",
        description: "The project's folder layout and discovered source files.",
        source: Source::Project,
        schema: no_arguments,
    },
];

/// `tools/list`, in MCP's shape.
pub fn describe_all() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Run one tool.
///
/// `project` is `None` when the working directory is under no `clean.toml`
/// (§10.9 rule 4). Project-backed tools then fail with an explanation rather
/// than guessing at a project.
pub fn call(
    tool: &Tool,
    arguments: &Value,
    project: Option<&Project>,
    language: &dyn LanguageInfo,
) -> ToolOutcome {
    match tool.source {
        Source::Language => call_language(tool, arguments, language),
        Source::Project => match project {
            Some(project) => call_project(tool, project),
            None => ToolOutcome::failed(json!({
                "error": "no project selected",
                "detail":
                    "This tool answers from project state, and the working directory is \
                     not inside a clean.toml project (Framework §10.9 rule 4).",
                "remedy": "Start the MCP server from a directory containing clean.toml.",
            })),
        },
    }
}

fn string_argument(arguments: &Value, key: &str) -> Option<String> {
    arguments.get(key)?.as_str().map(str::to_string)
}

fn call_language(tool: &Tool, arguments: &Value, language: &dyn LanguageInfo) -> ToolOutcome {
    // Build the query, refusing a call whose required argument is absent
    // before spending a subprocess on it.
    let query = match tool.name {
        "get_quick_reference" => Query::QuickReference,
        "get_specification" => Query::Specification {
            section: string_argument(arguments, "section"),
        },
        "list_builtins" => Query::Builtins,
        "list_types" => Query::Types,
        "list_functions" => Query::Functions,
        "list_error_codes" => Query::ErrorCodes,
        "get_changelog" => Query::Changelog,
        "explain_error" => match string_argument(arguments, "code") {
            Some(code) => Query::ExplainError { code },
            None => return missing_argument("code"),
        },
        name @ ("check" | "parse" | "validate" | "format" | "compile") => {
            let Some(source) = string_argument(arguments, "source") else {
                return missing_argument("source");
            };
            match name {
                "check" => Query::Check { source },
                "parse" => Query::Parse { source },
                "validate" => Query::Validate { source },
                "format" => Query::Format { source },
                _ => Query::Compile { source },
            }
        }
        other => {
            return ToolOutcome::failed(json!({
                "error": format!("no handler for language tool `{other}`"),
            }))
        }
    };

    match language.query(&query) {
        Ok(mut value) => {
            // §10.7 Rule 1's visible half: every language answer says which
            // compiler produced it, so a developer can tell whether the
            // guidance matches the compiler that will run.
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "compiler_version".into(),
                    match language.compiler_version() {
                        Some(version) => json!(version),
                        // §10.9 rule 2's wording, kept verbatim.
                        None => json!("unpinned"),
                    },
                );
            }
            ToolOutcome::ok(value)
        }
        Err(error) => ToolOutcome::failed(json!({
            "error": error.to_string(),
            "detail":
                "Language answers come from the compiler, never from the framework \
                 (Framework §10.7 Rule 1). The framework will not substitute its own \
                 answer, because a wrong-but-plausible one is worse than none.",
            "needed": "a compiler implementing the language-info API of Framework §10.5",
        })),
    }
}

fn missing_argument(name: &str) -> ToolOutcome {
    ToolOutcome::failed(json!({
        "error": format!("missing required argument `{name}`"),
    }))
}

fn call_project(tool: &Tool, project: &Project) -> ToolOutcome {
    match tool.name {
        "get_target_host" => project.target_host(),
        "get_host_capabilities" => project.host_capabilities(),
        "list_libraries" => project.libraries(),
        "get_app_structure" => project.app_structure(),
        other => ToolOutcome::failed(json!({
            "error": format!("no handler for project tool `{other}`"),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::Unavailable;

    #[test]
    fn every_tool_name_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in TOOLS {
            assert!(seen.insert(tool.name), "duplicate tool {}", tool.name);
        }
    }

    #[test]
    fn the_spec_10_2_1_language_tools_are_all_present() {
        // §10.2.1's table, verbatim. A tool dropped here silently shrinks the
        // contract the compiler team is implementing against.
        for name in [
            "get_quick_reference",
            "get_specification",
            "list_builtins",
            "list_types",
            "list_functions",
            "list_error_codes",
            "explain_error",
            "check",
            "parse",
            "compile",
            "validate",
            "format",
            "get_changelog",
        ] {
            let tool = find(name).unwrap_or_else(|| panic!("§10.2.1 names {name}"));
            assert_eq!(tool.source, Source::Language, "{name} must be language-backed");
        }
    }

    #[test]
    fn every_declared_schema_is_a_json_schema_object() {
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert!(schema.get("properties").is_some(), "{}", tool.name);
        }
    }

    #[test]
    fn tools_list_advertises_every_tool_with_its_schema() {
        let listed = describe_all();
        let tools = listed["tools"].as_array().unwrap();

        assert_eq!(tools.len(), TOOLS.len());
        for tool in tools {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn a_language_tool_fails_with_the_exact_call_it_tried() {
        // The point of failing loudly: the compiler team can read the missing
        // call straight off the error.
        let tool = find("list_builtins").unwrap();
        let outcome = call(tool, &json!({}), None, &Unavailable::not_implemented());

        assert!(outcome.is_error);
        let error = outcome.value["error"].as_str().unwrap();
        assert!(error.contains("cln --json list-builtins"), "{error}");
    }

    #[test]
    fn a_language_tool_never_invents_an_answer() {
        // §10.7 Rule 1. If this ever returns ok, the framework has started
        // answering language questions from its own memory.
        for name in ["get_quick_reference", "list_types", "list_error_codes"] {
            let tool = find(name).unwrap();
            let outcome = call(tool, &json!({}), None, &Unavailable::not_implemented());
            assert!(outcome.is_error, "{name} answered without a compiler");
        }
    }

    #[test]
    fn a_language_tool_missing_its_argument_says_which_one() {
        let tool = find("explain_error").unwrap();
        let outcome = call(tool, &json!({}), None, &Unavailable::not_implemented());

        assert!(outcome.is_error);
        assert!(
            outcome.value["error"].as_str().unwrap().contains("code"),
            "{:?}",
            outcome.value
        );
    }

    #[test]
    fn a_project_tool_without_a_project_explains_rather_than_guessing() {
        let tool = find("get_target_host").unwrap();
        let outcome = call(tool, &json!({}), None, &Unavailable::not_implemented());

        assert!(outcome.is_error);
        assert_eq!(outcome.value["error"], "no project selected");
    }
}

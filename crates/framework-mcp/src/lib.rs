//! The Clean MCP server — the toolchain's single MCP endpoint (Framework §10).
//!
//! §10.1 is the rule this crate exists to keep: **there is one MCP server, and
//! it is this one.** Every AI session connects here. Behind it, project-shaped
//! questions are answered from framework state and language-shaped ones are
//! forwarded to the compiler's language-info API, which is an ordinary CLI and
//! library surface rather than a second MCP server. The assistant sees one
//! namespace and cannot tell the difference (§10.8).
//!
//! # Layout
//!
//! - [`protocol`] — JSON-RPC over stdio, in MCP's shape.
//! - [`tools`] — the tool surface of §10.2, and the dispatch to a data source.
//! - [`project`] — project-backed answers, read from the same on-disk state
//!   the build path reads (§10.6).
//! - [`language`] — the seam to the compiler. A trait, because §10.5 specifies
//!   two equivalent surfaces (subprocess CLI and in-process library) and the
//!   handlers should not care which is in use.
//!
//! # What works today
//!
//! Project-backed tools answer for real. Language-backed tools are declared
//! and fail, because no shipped compiler implements the language-info API —
//! `clean-compiler 0.1.0` rejects `--json` and carries none of the bundled
//! documentation §10.3 requires.
//!
//! That failure is deliberate and is the honest half of §10.7 Rule 1: the
//! framework must never answer a language question from its own memory. A stub
//! returning plausible syntax would be indistinguishable from a correct answer
//! right up until it was wrong, and version-matched documentation is the whole
//! reason the split exists. So the tools are advertised — the compiler team
//! can read the contract off `tools/list` — and each failure names the exact
//! `cln --json` call it attempted.

pub mod language;
pub mod project;
pub mod protocol;
pub mod tools;

use std::io::{BufRead, Write};

use serde_json::{json, Value};

pub use language::{LanguageError, LanguageInfo, Query, Unavailable};
pub use project::Project;
pub use protocol::{Request, Response, ToolOutcome, PROTOCOL_VERSION};

/// The server's identity in the `initialize` handshake.
pub const SERVER_NAME: &str = "clean";

/// One MCP server over a byte stream.
pub struct Server {
    project: Option<Project>,
    language: Box<dyn LanguageInfo>,
}

impl Server {
    /// A server for the project containing `working_directory`.
    ///
    /// No project is a supported state, not an error: §10.9 rule 2 allows an
    /// ad-hoc session with no `clean.toml`, where language tools answer from
    /// the latest installed compiler and project tools report that there is
    /// nothing to answer about.
    pub fn new(working_directory: &std::path::Path, language: Box<dyn LanguageInfo>) -> Self {
        Server { project: Project::discover(working_directory), language }
    }

    /// The server as it ships today: no compiler implements language-info.
    pub fn with_default_language(working_directory: &std::path::Path) -> Self {
        Server::new(working_directory, Box::new(Unavailable::not_implemented()))
    }

    /// Read messages until the stream closes (§14.2.3 framing: one JSON object
    /// per line).
    ///
    /// Errors on individual messages are answered, not fatal — an assistant
    /// sending one malformed request should get a JSON-RPC error and keep its
    /// session, which is why only an I/O failure on the transport itself ends
    /// the loop.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let Some(response) = self.handle_line(&line) else {
                // A notification. Answering one is a protocol violation.
                continue;
            };

            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            // Flushed per message: a client is waiting on this response before
            // it sends the next request, so buffering would deadlock.
            output.flush()?;
        }
        Ok(())
    }

    /// Handle one line. `None` for a notification, which gets no reply.
    pub fn handle_line(&mut self, line: &str) -> Option<Response> {
        let request: Request = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                // No id is recoverable from an unparseable message, so
                // JSON-RPC says to answer with a null id.
                return Some(Response::error(
                    Value::Null,
                    protocol::error_code::PARSE_ERROR,
                    format!("could not parse message: {error}"),
                ));
            }
        };

        if request.is_notification() {
            return None;
        }

        let id = request.id.clone().unwrap_or(Value::Null);
        Some(self.dispatch(&request, id))
    }

    fn dispatch(&mut self, request: &Request, id: Value) -> Response {
        match request.method.as_str() {
            "initialize" => Response::result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": framework_core::FRAMEWORK_VERSION,
                    },
                }),
            ),

            "tools/list" => Response::result(id, tools::describe_all()),

            "tools/call" => self.call_tool(request, id),

            // `ping` is in the MCP spec and costs nothing to honour.
            "ping" => Response::result(id, json!({})),

            other => Response::error(
                id,
                protocol::error_code::METHOD_NOT_FOUND,
                format!("unknown method `{other}`"),
            ),
        }
    }

    fn call_tool(&mut self, request: &Request, id: Value) -> Response {
        let Some(name) = request.params.get("name").and_then(Value::as_str) else {
            return Response::error(
                id,
                protocol::error_code::INVALID_PARAMS,
                "tools/call requires a `name`",
            );
        };

        let Some(tool) = tools::find(name) else {
            // Naming the alternatives turns a typo into a one-step fix rather
            // than a second round-trip.
            return Response::error_with_data(
                id,
                protocol::error_code::INVALID_PARAMS,
                format!("unknown tool `{name}`"),
                json!({
                    "available": tools::TOOLS.iter().map(|t| t.name).collect::<Vec<_>>(),
                }),
            );
        };

        let arguments = request
            .params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let outcome = tools::call(
            tool,
            &arguments,
            self.project.as_ref(),
            self.language.as_ref(),
        );

        Response::result(id, outcome.to_result())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        // A directory that is deliberately not a project, so these exercise
        // the protocol rather than any project state.
        Server::with_default_language(std::path::Path::new("/"))
    }

    fn call(server: &mut Server, line: &str) -> Value {
        let response = server.handle_line(line).expect("expected a response");
        serde_json::to_value(response).unwrap()
    }

    #[test]
    fn initialize_reports_the_protocol_version_and_tool_capability() {
        let mut server = server();
        let response = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        );

        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(response["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn a_notification_gets_no_reply() {
        // MCP sends notifications/initialized after the handshake. Replying is
        // a protocol violation.
        let mut server = server();
        assert!(server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn tools_list_returns_the_whole_surface() {
        let mut server = server();
        let response = call(&mut server, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);

        let listed = response["result"]["tools"].as_array().unwrap();
        assert_eq!(listed.len(), tools::TOOLS.len());
    }

    #[test]
    fn an_unparseable_message_is_answered_not_fatal() {
        let mut server = server();
        let response = call(&mut server, "{not json");
        assert_eq!(response["error"]["code"], protocol::error_code::PARSE_ERROR);
        assert!(response["id"].is_null());
    }

    #[test]
    fn an_unknown_method_says_so() {
        let mut server = server();
        let response = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#,
        );
        assert_eq!(response["error"]["code"], protocol::error_code::METHOD_NOT_FOUND);
    }

    #[test]
    fn an_unknown_tool_lists_the_real_ones() {
        let mut server = server();
        let response = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}"#,
        );

        assert_eq!(response["error"]["code"], protocol::error_code::INVALID_PARAMS);
        let available = response["error"]["data"]["available"].as_array().unwrap();
        assert!(available.iter().any(|t| t == "get_target_host"));
    }

    #[test]
    fn a_language_tool_fails_as_a_result_not_a_transport_error() {
        // The distinction matters: the model asked a fair question and must be
        // able to read why it could not be answered.
        let mut server = server();
        let response = call(
            &mut server,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_builtins"}}"#,
        );

        assert!(response["error"].is_null(), "{response}");
        assert_eq!(response["result"]["isError"], true);

        let error = response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap();
        assert!(error.contains("cln --json list-builtins"), "{error}");
    }

    #[test]
    fn the_full_handshake_runs_over_a_stream() {
        let session = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );

        let mut output = Vec::new();
        server()
            .serve(std::io::Cursor::new(session), &mut output)
            .unwrap();

        let lines: Vec<&str> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .collect();

        // Two requests, two responses — the notification produced none.
        assert_eq!(lines.len(), 2, "{lines:?}");

        let initialize: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(initialize["id"], 1);
        let listed: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(listed["id"], 2);
        assert!(listed["result"]["tools"].is_array());
    }
}

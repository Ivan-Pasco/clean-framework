# The language-info API the compiler owes the MCP

**From:** framework
**Date:** 2026-08-21
**Status:** contract published, framework side implemented, compiler side absent
**Spec:** Framework §10.5 (surfaces), §10.2.1 (which tools need it), §10.3
(docs ship with the compiler), §10.7 Rule 1 (why the framework must not
answer these itself)

---

## What this is

The Clean MCP server is built and running. It is the toolchain's single MCP
endpoint per §10.1, it serves 17 tools, and the project-backed ones answer for
real against the shipped `clean-server@0.7.0` contract.

The 13 language-backed tools cannot answer, because the compiler does not
implement the API they forward to. This document is the request half of that
API — the exact calls the framework makes — plus the response shape each one
needs, so the compiler side can be implemented without guessing.

**The framework will not work around this.** §10.7 Rule 1 forbids the
framework answering a language question from its own memory, a cache, or a
spec URL, and the reason is §10.3: language documentation is bundled *inside*
each compiler binary so answers are always version-matched to the compiler
that will actually run. A framework-side fallback would be plausible, wrong
whenever the pinned compiler differed, and invisible when it was wrong. So
those tools fail loudly and name the call they tried.

## Current state, verified against `clean-compiler 0.1.0`

- `--json` is not a recognized argument. Every query form is rejected at argv.
- The bundled documentation of §10.3 is not present: no quick-reference text,
  no stdlib reference, and 5 diagnostic codes in the whole binary rather than
  a taxonomy.
- `--serve` **does** exist and speaks MCP, but it is Platform 14 §14.2.3 — a
  JSON-RPC wrapper over `compile`/`check`/`why` for driving compilation. It is
  a different thing from this API and does not satisfy §10.5. (Worth noting
  the naming collision: §14.2.3 gives the compiler an MCP surface while §10.1
  says the framework MCP is the toolchain's only one. Different consumers, but
  the names will confuse people.)

## The two surfaces

§10.5 specifies both. The framework's seam is a trait
(`framework-mcp::LanguageInfo`) so either can be dropped in without touching
the tool handlers.

1. **CLI** — `cln --json <query>`, one JSON document on stdout. Used
   out-of-process. §10.9 rule 1 makes this a **stable public API**: third-party
   linters, formatters, and CI checks may call it directly.
2. **Library** — a linkable Rust crate with typed functions matching the CLI
   queries. Used when the framework hosts the compiler in-process.

Implementing the CLI first is fine; it is what the framework calls today.

## The queries

Exactly as the framework invokes them. Snippet-carrying queries pass source on
**stdin**, not argv — a multi-line program in an argument list would bury the
failure in diagnostics and, for a large file, overflow the argument limit.

| MCP tool | Invocation | Source on stdin |
|---|---|---|
| `get_quick_reference` | `cln --json get-quick-reference` | no |
| `get_specification` | `cln --json get-specification [--section <name>]` | no |
| `list_builtins` | `cln --json list-builtins` | no |
| `list_types` | `cln --json list-types` | no |
| `list_functions` | `cln --json list-functions` | no |
| `list_error_codes` | `cln --json list-error-codes` | no |
| `explain_error` | `cln --json explain-error <CODE>` | no |
| `get_changelog` | `cln --json get-changelog` | no |
| `check` | `cln --json check` | yes |
| `parse` | `cln --json parse` | yes |
| `validate` | `cln --json validate` | yes |
| `format` | `cln --json format` | yes |
| `compile` | `cln --json compile` | yes |

## Response shapes

One JSON object on stdout per call. The framework adds a `compiler_version`
field to every response before returning it to the model, so **do not** include
one — it will be overwritten.

Nothing below is invented: the field names follow the tool descriptions in
§10.2.1. Where a shape is genuinely open, it is marked as the compiler's
choice, and the framework passes it through unaltered.

```jsonc
// list-builtins, list-functions
{ "functions": [ { "name": "…", "signature": "…", "category": "…",
                   "summary": "…" } ] }

// list-types
{ "types": [ { "name": "…", "kind": "primitive" | "complex",
               "summary": "…" } ] }

// list-error-codes
{ "codes": [ { "code": "SEM021", "name": "UndefinedModule",
               "severity": "error", "summary": "…" } ] }

// explain-error
{ "code": "SEM021", "name": "UndefinedModule", "severity": "error",
  "explanation": "…", "example": "…", "remedy": "…" }

// get-quick-reference
{ "sections": [ { "title": "…", "body": "…" } ] }

// get-specification            (no --section returns the index)
{ "section": "types", "title": "…", "body": "…" }
{ "sections": [ { "name": "types", "title": "…" } ] }

// get-changelog
{ "versions": [ { "version": "1.4.0", "changes": ["…"] } ] }

// check, validate              (diagnostics use the existing shape)
{ "failed": false, "diagnostics": [ /* Platform 14 diagnostic objects */ ] }

// parse
{ "failed": false, "ast": { /* compiler's choice */ },
  "diagnostics": [ … ] }

// format
{ "formatted": "…", "changed": true }

// compile
{ "failed": false, "wasm_base64": "…", "diagnostics": [ … ] }
```

Two notes on the snippet queries. They compile a **self-contained** snippet
with no project context — that is what makes them language-backed rather than
project-backed (§10.2.3). And `check`/`compile` here overlap with what `--serve`
already does; if it is cheaper to route these through the existing request-
document path internally, that is invisible to us, provided the argv form and
the response shape hold.

## Failure convention

A query that cannot be answered should exit non-zero with a JSON object on
stdout:

```jsonc
{ "error": "unknown section `widgets`",
  "available": ["types", "syntax", "…"] }
```

The framework surfaces this to the model as a tool failure. Listing the valid
alternatives, where there is a finite set, turns a typo into a one-step fix
rather than a second round-trip — the same reason our own `tools/call` lists
the real tool names when one is misspelled.

## Version reporting

Every response is attributed to the compiler that produced it (§10.7 Rule 1,
§10.9 rule 2). The framework reads the version from the resolved binary and
stamps it, marking it `"unpinned"` when the session has no project. The
compiler does not need to do anything here beyond being resolvable.

## What lands when this does

The 13 language-backed tools start answering with no framework change — they
are already written, declared, and tested against the trait. Dropping in a
subprocess implementation of `LanguageInfo` is roughly fifty lines on our side.

Until then, an assistant connected to the Clean MCP can see the target host,
its capabilities from the real WIT, the resolved library closure, and the app
structure — and gets an honest, actionable failure for anything about the
language itself.

## Trying it

```sh
cd <any project with a clean.toml>
cln mcp
```

Then, on stdin:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_host_capabilities"}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_builtins"}}
```

Tool 3 answers from the cached contract. Tool 4 fails with
`tried: cln --json list-builtins` — which is this document, in one line.

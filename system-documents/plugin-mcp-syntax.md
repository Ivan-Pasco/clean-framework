# `[mcp]` in `plugin.toml` — syntax metadata for AI-assisted development

**Status:** proposed, unimplemented. Written during Phase 4 so Phase 8
(`framework-mcp`) has a concrete target rather than a blank page.

**Owner of the decision:** framework, in agreement with whoever builds the
editor/AI integration that consumes it.

---

## What this is for

A plugin adds syntax to a Clean project. `frame.ui` introduces `button:` and
`stack:`; `frame.data` introduces `table:`. None of that syntax exists in the
compiler — it comes from the plugin — so nothing outside the plugin knows it is
valid, what it accepts, or how to spell it.

That is fine for the compiler, which learns the syntax from the plugin's own
WASM at compile time. It is not fine for an AI assistant writing Clean code: it
has no way to know `button:` exists, and will confidently invent a syntax that
does not compile. The `[mcp]` section is how a plugin publishes what it added,
so `cln mcp` can serve it to the tools a developer actually writes code in.

The section is **documentation, not contract**. The compiler never reads it.
Nothing about a build changes because of it.

---

## Shape

```toml
[plugin]
name = "frame.ui"
version = "0.4.0"

[paths]
owns = ["ui"]
patterns = ["ui.cln"]

# Everything below is for `cln mcp`. The build ignores it entirely.
[mcp]
description = "Declarative UI blocks that compile to a component tree."

[[mcp.syntax]]
name = "button"
kind = "block"
doc = "A clickable button. Emits its `on_click` handler when pressed."
snippet = """
button:
\tlabel: "<text>"
\ton_click: <handler>
"""

[[mcp.syntax]]
name = "stack"
kind = "block"
doc = "Lays its children along one axis."
snippet = """
stack:
\tdirection: vertical
"""

[[mcp.syntax.fields]]
name = "direction"
type = "vertical | horizontal"
required = true
doc = "Which axis children flow along."
```

### Fields

| Key | Required | Meaning |
| --- | --- | --- |
| `mcp.description` | no | One line on what the plugin adds. Shown when an assistant lists what is available in a project. |
| `mcp.syntax[]` | no | One entry per construct the plugin introduces. |
| `.name` | **yes** | The identifier as written in source (`button`). |
| `.kind` | no | `block`, `function`, `attribute`. Defaults to `block`. |
| `.doc` | no | Prose. What it does and when to reach for it. |
| `.snippet` | no | A minimal working example, with `<placeholders>` marking what the developer fills in. |
| `.fields[]` | no | Accepted keys, each with `name`, `type`, `required`, `doc`. |

`snippet` uses real tabs, because Clean is tab-indented and an assistant copying
spaces out of a snippet produces code that does not compile.

---

## Rules

**FRM-MCP-01 — The build ignores `[mcp]` entirely.**
It is not lowered into the request document, it does not reach the compiler, and
it takes no part in the build-cache key. Two plugins differing only in their
`[mcp]` section produce byte-identical builds.

**FRM-MCP-02 — Malformed `[mcp]` never fails a build.**
A missing `name`, an unparseable entry, a `[mcp]` that is not a table: all are
skipped by `cln mcp`, which serves what it could read. The reasoning is the same
one that governs the build cache — a developer whose autocomplete is wrong still
needs to be able to compile, and a plugin author's typo in a doc string must not
break every project that depends on them.

`cln mcp` may warn about what it skipped. `cln build` may not.

**FRM-MCP-03 — Absent `[mcp]` is normal, not deficient.**
Most plugins will not have one at first. A plugin without the section is served
as "exists, adds syntax I cannot describe" — never as an error, and never
omitted from the list of what a project depends on.

**FRM-MCP-04 — The plugin is the only source.**
Syntax metadata is never inferred from the WASM, never hand-maintained in the
framework, and never fetched from anywhere else. A plugin describes itself or it
is not described. Anything else would drift the moment a plugin is republished.

---

## What already works

Nothing in this document needs new parsing to *survive*. `PluginManifest` keeps
every section it does not interpret in its `extra` map, so a plugin carrying
`[mcp]` loads today, and the metadata arrives intact — nested tables, arrays,
docs and all. Verified against the example above during Phase 4.

What Phase 8 adds is a typed reader over that data and the MCP tool surface that
serves it.

---

## Open questions

1. **Does `cln mcp` serve the closure or just direct dependencies?** A project
   depending on `frame.ui`, which depends on `frame.core`, arguably wants both.
   Leaning toward the full closure — an assistant should know about syntax that
   is legal in the file it is editing, and transitive plugins add legal syntax.

2. **Versioning.** When two plugins in one project both define `button`, whose
   documentation wins? Proposal: serve both, qualified by plugin name, and let
   the assistant disambiguate — the compiler already has to resolve this and the
   MCP server should not invent a second, different answer.

3. **Is `[mcp]` the right section name?** It names the transport rather than the
   content. `[syntax]` or `[docs]` would survive the MCP protocol falling out of
   fashion. Low cost to change now, high cost after plugins ship with it.

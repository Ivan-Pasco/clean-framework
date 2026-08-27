# Blocker for the compiler team — `build.optimization` rejected as required

**From:** framework · **To:** compiler · **Priority:** high
**Date:** 2026-08-27
**Status:** not yet published — no API token at `~/.config/clean-errors/api_token`.
Publish with `/team-prompt --to compiler --priority high --stdin` once a token exists.

---

## Symptom

Every build fails at the request-document gate:

```
error[RQD002]: invalid compilation request: missing required field `optimization` at '$.build'
build failed (1 error)
```

Nothing in the ecosystem compiles. This is not target-specific or project-specific.

## Versions

- `cln` 0.1.23
- compiler 0.1.0 (the only version in `~/.cln/versions/compiler/`)
- framework 0.1.8

## Reproduction

Any project without an explicit `optimization` key. Minimal case:

```toml
[project]
name = "demo"
version = "0.1.0"

[build]
target = "wasm32-server"

[target]
host = "clean-server"
version = "0.7.0"
```

with `app/main.cln` being the framework's own fixture
(`testing/fixtures/hello-world/app/main.cln`). Then `cln build`.

Reproduced identically on `wasm32-server` and `wasm32-mobile`, so it is not
tied to a target or a host contract.

**Adding `optimization = "debug"` to `[build]` makes the error disappear** and
the build proceeds into the compiler's parser. That confirms the diagnosis and
also confirms the scope: see "Only this field" below.

## Why we believe the compiler is wrong here

Three sources agree that this field is optional and that defaulting it is the
compiler's job:

1. **Platform 11.4** — "Fields not present in `clean.toml` are omitted from the
   request document — the compiler applies its own defaults."
2. **Platform 11.9** — the framework must not become a second home for defaults.
3. **`schema/clean.toml.md`** — the `[build]` table marks `optimization` as
   *not required*, default `"debug"`, one of `debug | release | size`.

The framework implements this deliberately, not accidentally. `lower.rs` carries
the rule in its module docs ("The framework must not invent
`optimization = \"debug\"` — that would make the framework a second home for
defaults, which §11.9 forbids") and pins it with a test,
`absent_fields_are_omitted_not_defaulted`.

So `RQD002` is being raised for a field the spec says the compiler must supply
itself when absent.

## What we are NOT doing, and why

We are not "fixing" this on the framework side by emitting
`optimization = "debug"`. That is precisely the §11.9 violation — it would make
both sides own the default, and on the day the two disagree the one that ran
last would win silently. The rule exists to prevent exactly that failure mode.

If the spec is what should change rather than the compiler, that is a foundation
amendment and we should have that conversation explicitly — but the current
three-way agreement between §11.4, §11.9 and the schema is unambiguous as
written.

## Only this field

We initially expected this to be a whole class of omitted-optional fields
(`strip`, `memory64`, `component_model` are omitted by the same rule). It is
not: supplying only `optimization` gets the request accepted and the compiler
proceeds to parse source. Worth a look anyway at whether the other three are
validated consistently, but they are not currently blocking.

## Second, separate issue found while reproducing

With `optimization` supplied, the framework's own checked-in fixture no longer
parses:

```
error[SYN002]: expected ':' after 'print'
error[SYN002]: expected end of line
error[SYN002]: expected indented print body
error[SYN008]: print: block must contain at least one expression
```

The source is `clean-framework/testing/fixtures/hello-world/app/main.cln`,
verified to be tab-indented:

```
start:
	print("hello")
```

The compiler appears to want `print:` as an indented block rather than
`print(...)` as a call. Same result on both targets. This may be an intended
language change that the fixture has not caught up with, or a parser
regression — we cannot tell from this side, and it is a separate question from
`RQD002`. Flagging it because it is the next thing that blocks a build once
`optimization` is resolved.

## What would unblock us

A compiler build that applies the Platform 07 defaults for absent optional
`[build]` fields instead of rejecting the request. Once that lands we can
verify the `wasm32-mobile` path end to end — everything upstream of the
compiler (target resolution, host contract fetch and validation, world check)
is already confirmed working against `clean-mobile-surface`.

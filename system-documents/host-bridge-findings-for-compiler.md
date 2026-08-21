# `compiletime_wasm_sha256` is mandatory, and it blocks every host-bridge library

**From:** framework, against compiler 0.1.0
**Date:** 2026-08-20
**Status:** blocking — no library that ships only a host bridge can be built
**Needs:** a compiler-side schema fix; the spec already says what we expect

---

## The short version

The framework now synthesizes a library's WIT from its `host_bridge.cln`, per
[LBS §8](../../foundation/02%20components/framework/09-libraries-specification.md).
That half works: real declarations become a real `clean:library/<name>@<ver>`
package and reach the request document.

The compiler then refuses the request:

```
error[RQD002]: invalid compilation request: missing required field
               `compiletime_wasm_sha256` at '$.library_manifests[0]'
```

Sending `null` is refused too, with `invalid type: null, expected a string`.
Only a real 64-character hex string is accepted.

**So `library_manifests[]` is currently unusable for any library that does not
ship a compile-time block handler** — which is every host-bridge-only library,
including `frame.storage`, the smallest one in the catalog.

## Why we think the field should be optional

Platform 14 §14.4.2 defines the field by what it is *for*:

> **[6] Block Handler Expansion** — For every block whose keyword is a
> library-declared block name, load the library's `compiletime` function (the
> framework-compiled, cached WASM identified by
> `library_manifests[].compiletime_wasm_sha256` …)

A library with no `handles block` declaration has no `compiletime` function, so
there is no WASM, so there is no hash. There is nothing the framework could put
in the field that would be true. Inventing one — a zero hash, or the hash of an
empty file — would name an artifact that does not exist, and pass 6 would
either look for it or skip it depending on internals we cannot see from here.

`frame.storage` (libraries §09, STOR-01) is the clean example: four host
functions, no DSL block, no compile-time handler, explicitly *"a
host-function-only library — no new syntax, no DSL block"*. It cannot be built
today.

## What we are not doing

We are not sending a placeholder hash. A field whose meaning is "the WASM this
dependency contributes at compile time" must not carry a value for a
dependency that contributes none — that is how a cache key starts describing
something that was never built, and Phase 5's build cache is keyed on this
document.

## What we would like

Make `compiletime_wasm_sha256` optional in the request schema — absent, or
`null`, for a library with no compile-time handler. That matches §14.1.1, which
already marks it optional, and it is what the framework emits today
(`skip_serializing_if = "Option::is_none"`).

## How to reproduce

The framework carries this as a Layer C test, currently red:

```sh
cd clean-framework
cargo test -p framework-core --test real_compiler -- --ignored \
  the_real_compiler_accepts_our_synthesized_library_wit
```

It asserts on the specific `RQD002`, so when the field is relaxed the test goes
green on its own and the note in it comes out. The rest of the Layer C suite
passes against 0.1.0, and the default suite is green.

Minimal request fragment that reproduces it — everything else in the document
is a normal `wasm32-server` build:

```json
"library_manifests": [
  {
    "name": "frame.storage",
    "version": "0.1.0",
    "wit": "package clean:library/frame.storage@0.1.0;\n\ninterface storage {\n    write: func(path: string, data: list<u8>) -> s64;\n}\n"
  }
]
```

Adding `"compiletime_wasm_sha256": "<64 hex chars>"` makes the same request
compile, which is how we know this is the only thing in the way.

## Unrelated, and not a complaint

Two smaller things we found while probing 0.1.0, recorded here only so they are
written down somewhere:

- `host interface` / `host function` / `requires host worlds` all parse, and
  the full `frame.storage` surface type-checks against the shipped
  `clean-server@0.7.0` contract. The type grammar accepts `string`, `integer`,
  `integer:32`, `number`, `boolean`, `bytes`, `list<T>`, and `pairs<K,V>`.
  This is more than we expected and it is what made Track 1 possible.
- `description` is enforced as mandatory (`SYN005`), exactly as LBS-02 requires.
  The framework checks it too, so the author sees the library file rather than
  a request-level error.

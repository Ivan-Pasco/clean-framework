# What the compiler still owes libraries

**From:** framework, against the installed compiler 0.1.0
**Date:** 2026-08-21
**Status:** one blocker, three smaller gaps
**Method:** every claim below was produced by driving the shipped
`clean-compiler 0.1.0` binary with real request documents, not by reading
source or inferring from spec.

---

## The short version

The framework now derives a library's WIT from its `host_bridge.cln` and puts
it in `library_manifests[].wit` (LBS §8). That half works.

**A library still cannot be used.** `storage.write(...)` fails with:

```
error[SEM021]: I cannot resolve the module `storage`
```

Library WIT reaches the compiler and is never turned into callable signatures.
So a library can be declared, shipped, and linked — and nothing can call it.
Everything else in this document is small next to that.

---

## 1. BLOCKER — library module calls do not resolve (`SEM021`)

Given a `library_manifests[]` entry whose `wit` declares
`interface storage { write: func(path: string, data: list<u8>) -> s64; }`,
no call form resolves:

| Call site | Result |
|---|---|
| `storage.write("a", "b")` | `SEM021: I cannot resolve the module storage` |
| `write("a", "b")` | `SEM019: I cannot find a function named write` |
| with `folders = {"app": ["frame.storage"]}` | `SEM021`, unchanged |
| with `folders` **and** a matching `dependencies` entry | `SEM021`, unchanged |

We tried the folder-scoping and dependency-declaration paths precisely because
`LIB017` suggested they mattered; once both are satisfied the diagnostic is
unchanged, so neither is the missing piece.

**What this blocks:** every library in the catalog. `frame.storage` is four
host functions and nothing else — it has no other surface to exercise. Same for
the host-facing half of `frame.server`, `frame.auth`, `frame.client`,
`frame.ui`, and `frame.canvas`.

**What we believe is needed:** parse `library_manifests[].wit`, bind the
interface name as a module in scope, and typecheck call sites against the
declared signatures — the same treatment `target_world.wit` already gets for
host imports, applied to the library entries.

The framework has no workaround. We cannot synthesize a call the compiler will
not resolve, and we will not emit a fake module.

---

## 2. FIXED ON OUR SIDE — `compiletime_wasm` was missing from the request

Recorded here because the earlier version of this document asked for the wrong
thing, and the record should not stay wrong.

We originally reported that `compiletime_wasm_sha256` should be optional. That
was half the story. The compiler also needs the **bytes**, inline, in a field
called `compiletime_wasm` — because CMP-01 leaves it nowhere to fetch the
artifact a bare hash names. The framework was sending neither.

The framework now sends both, and the pairing is verified against the real
binary. The contract, confirmed by probing rather than assumed:

- **Standard base64 alphabet, padding required.** Unpadded input is rejected as
  `compiletime_wasm is not valid base64`.
- **`compiletime_wasm_sha256` hashes the decoded bytes**, not the base64 string.
- **Lowercase hex.** Uppercase is rejected by name.
- Both fields absent for a library registering no `handles_blocks`; a library
  that registers blocks and omits the WASM gets `LIB004`.

`the_real_compiler_accepts_a_plugins_inline_wasm` covers this and passes.

**Small ask:** `compiletime_wasm` is absent from Platform 14 §14.1.1, so the
spec still describes a request the compiler will not accept. Worth an amendment
so the next implementer does not repeat our mistake.

**One live incompatibility:** the installed 0.1.0 binary rejects an entry that
omits `compiletime_wasm_sha256` entirely (`RQD002: missing required field`),
which blocks bridge-only libraries independently of §1. The field is optional
in §14.1.1 and, as we understand it, in the compiler's current source — 0.1.0
simply predates that. A release carrying the fix clears it; our Layer C test
asserts on the specific diagnostic and will go green on its own.

---

## 3. `print` does not parse

```
start:
	print("hello")
```

gives `SYN002: expected ':' after 'print'`.

This is what `cln new` generates and what the language spec shows, so a new
project fails on its first build. Small, but it is the first thing every user
sees.

---

## 4. Stdlib modules are absent

`math.abs(-3)` gives `SEM019: I cannot find a function named abs`. Method-style
calls on values do work (`"abc".length()` compiles), so this is specifically
the module-qualified stdlib namespaces — `math.*`, `string.*`, `list.*`,
`time.*`, `json.*` — from Language §15.

Note this is likely the *same* underlying gap as §1: both are
module-qualified name resolution. If so, fixing §1 may fix this for free.

---

## What already works, so nobody spends time on it

Worth stating plainly — we got this wrong ourselves and wasted a day on the
assumption that the language surface was smaller than it is.

- **Functions.** Inside a `functions:` block, with a leading return type:
  `functions:` / `integer add(integer a, integer b)` / `return a + b`. Calls
  from `start:` work. Recursion works. `void` works.
- **Classes.** `class User` with typed fields, at the top level.
- **Control flow.** `if` / `else`, `iterate i in 1 to 3`, comparisons,
  arithmetic, string concatenation.
- **Types.** `string`, `integer`, `boolean`, `bytes`, `number`, `list<T>`,
  literals, `.length()`.
- **`host interface` blocks.** The full LBS-02 grammar parses:
  `host interface` / `version` / `requires host worlds` / `host function` with
  a mandatory `description` (enforced as `SYN005`, exactly per spec). Parameter
  and return types accept `string`, `integer`, `integer:32`, `number`,
  `boolean`, `bytes`, `list<T>`, `pairs<K,V>`.
- **The block-handler path.** A registered `handles_blocks` entry with a valid
  inline WASM reaches the sandbox — our stub was rejected with
  `BLOCK004: handler wasm does not export 'memory'`, which is the sandbox
  loading it and judging it on merit.

---

## Priority, from where we sit

1. **§1, library module calls.** Nothing else changes what is shippable. Until
   it lands, no library in the catalog can be used, and further framework work
   on libraries has nothing to build against.
2. **§2's 0.1.0 incompatibility** — clears bridge-only libraries, and is
   probably already done in source; it needs a release.
3. **§3 `print`** — one afternoon, and it unbreaks `cln new`.
4. **§4 stdlib modules** — possibly free with §1.

## Reproducing any of this

```sh
cd clean-framework
cargo test -p framework-core --test real_compiler -- --ignored
```

`the_real_compiler_accepts_a_plugins_inline_wasm` passes.
`the_real_compiler_accepts_our_synthesized_library_wit` fails on the §2
incompatibility. Neither covers §1 yet — there is no call site to write until
the module resolves.

# Library code is unreachable: the one gap that blocks every `frame.*` library

**From:** framework, against the installed `clean-compiler 0.1.0`
**Date:** 2026-08-21
**Audience:** compiler team
**Method:** every claim was produced by driving the shipped binary with real
request documents. Nothing here is inferred from source or assumed from spec.

---

## The explanation, in one page

The framework can now do its half of the library story. A library declares its
host imports in `host_bridge.cln`, the framework derives the WIT
(`clean:library/<name>@<version>`) and hands it to you in
`library_manifests[].wit`. That works, it is committed, and it is covered by
tests against your real binary.

**What is missing is the other end: nothing can call into a library.**

The WIT arrives, and it is never turned into callable signatures. Today
`library_manifests[].wit` is hashed for the build manifest and otherwise
unread — the only WIT that becomes types is `target_world.wit`. So a library
can be declared, resolved, locked, linked, and hashed, and then no line of
Clean can invoke a single function in it.

Concretely, with a library whose WIT declares
`interface storage { write: func(path: string, data: list<u8>) -> s64; }`,
fully wired — the library in `library_manifests[]`, a matching `dependencies`
entry, and `folders` bringing it into scope for `app/`:

| What the developer writes | What they get |
|---|---|
| `storage.write("a", "b")` | `SEM021: I cannot resolve the module 'storage'` |
| `write("a", "b")` | `SEM019: I cannot find a function named 'write'` |
| `import: storage` | `IMPORT002: I cannot find the module 'storage' in this build` |

Three call forms, three different diagnostics, one cause: the library's WIT was
never read.

**Why this outranks everything else on the library track.** It is not that some
libraries are blocked — it is that the concept is. `frame.storage` is four host
functions and nothing else; it has no other surface. The same is true of the
host-facing half of `frame.server`, `frame.auth`, `frame.client`, `frame.ui`,
and `frame.canvas`. Until a call resolves, every library in the catalog is
declaration-only, and further framework work on libraries has nothing to build
against.

The framework has no way around this. We cannot synthesize a call you will not
resolve, and we will not emit a fake module to paper over it.

## What already works, so no one re-litigates it

We got this wrong ourselves and lost a day to it, so it is worth stating:
**the language surface is in good shape.** Functions (in a `functions:` block
with a leading return type), classes, `if`/`else`, `iterate`, recursion,
`list<T>`, `bytes`, string ops, and the entire LBS-02 `host interface` grammar
all compile today. The block-handler sandbox path works end to end — a
registered handler with valid inline WASM reaches it and is judged on merit.

The gap is narrow and specific. It is not "libraries are unimplemented"; it is
"library WIT is never parsed into scope."

## Three smaller items, while you are in here

1. **`print("hello")` does not parse** — `SYN002: expected ':' after 'print'`.
   This is what `cln new` generates, so a new project fails on its first build.
2. **Module-qualified stdlib is absent** — `math.abs(-3)` gives `SEM019`.
   Method-style calls on values (`"abc".length()`) do work. This may well be
   the same root cause as the main gap; both are module-qualified resolution.
3. **`compiletime_wasm` is missing from Platform 14 §14.1.1.** Your binary
   requires it (correctly — CMP-01 leaves you nowhere to fetch what a bare hash
   names). The framework now sends it. But the spec still describes a request
   you reject, which is how we got this wrong in the first place. Worth an
   amendment.

Also: the installed 0.1.0 rejects an entry omitting `compiletime_wasm_sha256`
with `RQD002: missing required field`, though §14.1.1 marks it optional and we
understand current source agrees. That alone blocks bridge-only libraries. A
release carrying the fix clears it — no code change needed on our side.

## Suggested order

1. **Library module calls.** The only item that changes what is shippable.
2. **A release** carrying the optional-`compiletime_wasm_sha256` fix.
3. **`print`.** Small, and it unbreaks `cln new`.
4. **Stdlib modules.** Possibly free with (1).

## Reproducing

```sh
cd clean-framework
cargo test -p framework-core --test real_compiler -- --ignored
```

`the_real_compiler_accepts_a_plugins_inline_wasm` passes — inline WASM is
correct end to end. `the_real_compiler_accepts_our_synthesized_library_wit`
fails on the 0.1.0 field requirement. Neither covers the main gap yet: there is
no call site worth writing until a module resolves.

Full detail, including the exact encoding contract we had to establish by
probing: [host-bridge-findings-for-compiler.md](host-bridge-findings-for-compiler.md).

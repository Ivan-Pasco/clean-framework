# Prompt for the compiler session

Paste the block below into a session opened in the `clean-language-compiler`
repo. It is written to be self-contained: it states the problem, the evidence,
the spec anchors, and the acceptance criteria, without assuming the reader saw
this conversation.

The four items are ordered by what unblocks the most. Item 1 is the only one
that changes what is shippable — if only one thing gets done, do that.

---

```
Four gaps in the compiler are blocking the library ecosystem. They were found
by driving the shipped `clean-compiler 0.1.0` binary with real request
documents from the framework side, not by reading source. Please verify each
against the binary yourself before and after fixing — the spec and the binary
have diverged in both directions here, so neither alone is authority.

Work them in the order given. Item 1 is the blocker; the rest are small.

--------------------------------------------------------------------------
ITEM 1 (BLOCKER) — library WIT never becomes callable signatures
--------------------------------------------------------------------------

`library_manifests[].wit` arrives in the request, is hashed for the build
manifest, and is otherwise never read. The only WIT that becomes types is
`target_world.wit`. The consequence: a library can be declared, resolved,
locked, linked and hashed, and no line of Clean can call into it.

Reproduce with a request document carrying a library entry whose `wit` is:

    package clean:library/frame.storage@0.1.0;

    interface storage {
        /// Write bytes.
        write: func(path: string, data: list<u8>) -> s64;
    }

...plus a matching `dependencies` entry and `folders = {"app": ["frame.storage"]}`.
Then all three call forms fail:

    storage.write("a", "b")   -> SEM021: I cannot resolve the module `storage`
    write("a", "b")           -> SEM019: I cannot find a function named `write`
    import: storage           -> IMPORT002: I cannot find the module `storage`

Note this reproduces ONLY if the entry also carries a valid
`compiletime_wasm` / `compiletime_wasm_sha256` pair — otherwise 0.1.0 fails
earlier with RQD002 and masks the real behaviour (see item 2). Use a minimal
module (magic + version, `\0asm\x01\0\0\0`) with the sha256 of its DECODED
bytes, lowercase hex, standard-alphabet base64 WITH padding.

What is needed: parse `library_manifests[].wit`, bind the interface as a module
in scope, and typecheck call sites against the declared signatures — the same
treatment `target_world.wit` already gets for host imports, applied to library
entries.

Two questions the spec does not settle, worth deciding explicitly and writing
down, because the framework will generate whatever you choose:

  a) Is a library host function called QUALIFIED (`storage.write(...)`) or
     UNQUALIFIED (`write(...)`)? Libraries Specification §8.5 shows the
     unqualified form inside library source. Both currently fail with different
     diagnostics, which suggests neither path is wired rather than that one was
     chosen.

  b) What brings the module into scope — `clean.toml [folders]`, an explicit
     `import:`, or both? Block resolution has an answer for handlers
     (Language §21.2, BLK-02: explicit import wins, then folder scope). Value-
     level library calls have no equivalent rule written down. Mirroring BLK-02
     is the obvious default; if you pick something else, say so and we will
     match it.

Relevant spec: Libraries Specification §8 (LBS-02, §8.5 call sites), Platform
14 §14.1.1 (request inputs), Platform 14 §14.4.2 pass [9] (World Import Check,
which already reads `library_manifests[].wit` for library-declared imports —
that pass is the natural home).

Acceptance: a Clean program calling a function declared only in a library's
synthesized WIT compiles to a component whose imports name that library's
interface. A call whose signature does not match the WIT is rejected with a
diagnostic naming the library and the function.

--------------------------------------------------------------------------
ITEM 2 — `compiletime_wasm_sha256` must be optional
--------------------------------------------------------------------------

0.1.0 rejects a `library_manifests[]` entry that omits `compiletime_wasm_sha256`:

    RQD002: invalid compilation request: missing required field
            `compiletime_wasm_sha256` at '$.library_manifests[0]'

Platform 14 §14.1.1 marks it optional, and it is meaningless for a library that
ships no compile-time handler — which is every host-bridge-only library,
including `frame.storage`. We believe current source already makes it optional
(`Option<String>`), in which case this needs only a release, not a change.
Please confirm against the binary.

While confirming: `compiletime_wasm` (the inline base64 handler WASM) is absent
from Platform 14 §14.1.1 entirely, though the binary requires it whenever
`handles_blocks` is non-empty. The framework now sends it and it works — the
gap is documentation. Please add it to §14.1.1 with the encoding contract, all
four parts of which we had to establish by probing:

  - standard base64 alphabet, padding REQUIRED (unpadded -> "not valid base64")
  - `compiletime_wasm_sha256` hashes the DECODED bytes, not the base64 string
  - lowercase hex only (uppercase is rejected by name)
  - both fields absent when the library registers no `handles_blocks`;
    registering blocks while omitting the WASM is LIB004

Acceptance: a request whose library entry has neither field compiles. §14.1.1
documents `compiletime_wasm` and its encoding.

--------------------------------------------------------------------------
ITEM 3 — `print` does not parse
--------------------------------------------------------------------------

    start:
    	print("hello")

    -> SYN002: expected ':' after 'print'

This is exactly what `cln new` generates and what the language spec shows, so
every new project fails on its first build. The parser appears to expect a
block form (`print:`), so decide which is canonical and make the spec, the
scaffold template, and the parser agree.

Acceptance: the output of `cln new` compiles unmodified.

--------------------------------------------------------------------------
ITEM 4 — module-qualified stdlib is absent
--------------------------------------------------------------------------

    math.abs(-3)   -> SEM019: I cannot find a function named `abs`

Method-style calls on values DO work (`"abc".length()` compiles), so this is
specifically the module-qualified stdlib namespaces of Language §15 —
`math.*`, `string.*`, `list.*`, `time.*`, `json.*`.

This may share a root cause with item 1: both are module-qualified name
resolution. Check after item 1 lands; it may already be fixed.

Acceptance: the stdlib namespaces in Language §15 resolve and typecheck.

--------------------------------------------------------------------------
CONTEXT WORTH HAVING
--------------------------------------------------------------------------

The language surface is in better shape than these four items suggest, and it
is worth not re-investigating what already works. Verified against 0.1.0:

  - functions, in a `functions:` block with a leading return type:
        functions:
        	integer add(integer a, integer b)
        		return a + b
    calls from `start:` work; recursion works; `void` works
  - classes with typed fields, at top level
  - if/else, iterate, comparisons, arithmetic, string concatenation
  - string, integer, boolean, bytes, number, list<T>, literals, .length()
  - the full LBS-02 `host interface` grammar, including `integer:32`,
    `pairs<K,V>`, and the mandatory `description` (correctly SYN005)
  - the block-handler sandbox: a registered handler with valid inline WASM
    reaches it and is rejected on merit (BLOCK004 for a stub missing 'memory')

The framework side is done and green: `host_bridge.cln` is parsed, WIT is
synthesized into `library_manifests[].wit`, LIB012 world-binding is enforced,
and plugin WASM travels inline. Framework capacity is not the constraint.

Full write-up, with every probe that produced these claims:
  clean-framework/system-documents/host-bridge-findings-for-compiler.md
  clean-framework/system-documents/compiler-brief-library-calls.md
```

# What we found at the framework/compiler seam

**From:** framework, against compiler 0.1.0
**Date:** 2026-08-20
**Status:** informational — framework has already adapted to two of these; two need a decision that isn't ours

Compiler 0.1.0 is the first release the framework has ever been able to run
against. Before it, everything above the seam was tested with a stand-in that
implemented what we *believed* the protocol to be. That belief turned out to be
wrong in three places. This is a report of what the real binary does, what we
changed on our side, and where the two specs genuinely disagree.

The headline: **it works.** The framework compiles Clean source into a real
43KB WASM component through the real compiler, with correct provenance, a
working build cache, and real diagnostics reaching the developer. The rest of
this document is about the friction we hit getting there.

---

## 1. The invocation differed (framework adapted)

We were calling:

```
clean-compiler compile --stdout-tar        # request JSON on stdin,
                                           # tarball back on stdout
```

The real compiler accepts neither the `compile` subcommand nor `--stdout-tar`.
It exits 2 with `unexpected argument 'compile' found`. So every `cln build`
against a real toolchain failed at the very first call across the seam, before
any Clean source was read.

The compiler's actual interface is an output directory:

```
clean-compiler --request - --out <dir>     # request JSON on stdin,
                                           # artifact set written into <dir>
```

We came to the stdout mode from Platform 14 §14.1.2, which describes it as an
opt-in mode of the process adapter ("or to stdout as a single tarball (process
adapter, with `--stdout-tar`)"). We read that as available; it isn't
implemented. **We have switched to the directory mode**, which is the one that
exists, and we're not asking for the other one back — the directory mode works
fine for us.

Two notes on what that cost us, purely so the trade-off is visible if the
question ever comes up again:

- We now create and remove a scratch directory per build. It's owned by a type
  whose `Drop` cleans it up, because the failure path returns early and a
  hand-rolled cleanup would leak a directory on every rejected compile.
- We re-pack the artifact set into an in-memory tarball on the way out, because
  our build cache stores the compiler's response verbatim and a cache entry has
  to be a self-contained blob rather than a directory we're about to delete.

Neither is a complaint. They're just the shape of the adaptation.

**What might be worth doing on your side:** the spec text in Platform 14
§14.1.2 currently promises a mode the binary doesn't have. Either the mode or
the sentence is wrong. We don't need the mode.

---

## 2. `diagnostics.json` is NDJSON, not an array (framework adapted)

The spec describes `diagnostics.json` as an array of diagnostic objects. The
real compiler writes NDJSON — one JSON object per line, no enclosing brackets:

```
{"level":"error","code":"SYN002","message":"expected an expression",...}
{"level":"error","code":"SYN002","message":"expected end of line",...}
```

Our parser accepted arrays only. The consequence was bad out of proportion to
the cause: when the compiler rejected a program, it wrote a **perfectly good
spanned diagnostic** — file, line, column, rendered snippet, doc URL — into the
output directory, and the developer saw:

```
error[FRM001]: compiler exited with code 1
```

The real message was sitting in a file we had just written and could not parse.

**We now accept all three shapes** — NDJSON, a bare array, and an object with a
`diagnostics` key — and the developer gets the real thing:

```
error[SYN002]: expected an expression, found Newline
  --> app/main.cln:2:14
```

We're being deliberately permissive here rather than asking you to change. A
diagnostic that fails to parse is a diagnostic nobody sees, which makes this
exactly the wrong place for us to be strict about a format you own. But the
spec and the implementation do disagree, and someone reading the spec to write
another consumer would hit the same wall.

---

## 3. `optimization` is required, and we think it shouldn't be (needs a decision)

This one we have **not** worked around, because working around it would mean
silently taking a position on a question that belongs to both of us.

The request document we send omits fields the developer didn't set, so that the
compiler applies its own defaults. That's deliberate, and it comes from
framework spec §11.4:

> Fields not present in `clean.toml` are omitted from the request document —
> the compiler applies its own defaults.

and §11.9, which says the framework must not become a second home for defaults.
The reasoning behind those lines is that if both sides carry a default for
`optimization`, they will eventually disagree, and the resulting build will be
correct according to each component and wrong according to the developer.

The compiler rejects such a request outright:

```
error[RQD002]: invalid compilation request: missing required field
               `optimization` at '$.build'
```

Adding `optimization` makes the request fully valid — that's how we got a real
component out — so this is the only thing standing between a default
`clean.toml` and a working build.

**The disagreement in one sentence:** framework spec says absent means "you
decide"; compiler implementation says absent means "invalid".

Three ways this could resolve, and we don't have a preference strong enough to
act unilaterally:

1. **The compiler treats the field as optional** and applies its own default.
   This matches §11.4 and keeps one home for defaults.
2. **The framework always sends a value.** Then the framework owns the default
   for `optimization`, and §11.4/§11.9 need amending to say so — because that's
   a real change in who decides, not a formatting detail.
3. **`clean.toml` requires it**, so it's never absent in the first place. That
   pushes the decision onto every developer, which seems worst, but it is
   internally consistent.

We'd rather one of you tells us which, than pick and have it drift.

---

## 4. Something you may already know: `print` isn't in 0.1.0 yet

Not a bug, and not a complaint — recording it because it surprised us and may
surprise others.

`print("hello")` is what the language spec shows as the canonical first program
(`04 language/09-functions.md`, FNC-01), and it's what `cln new` generates. On
0.1.0 it produces:

```
clean-compiler: program uses 1 construct(s) outside the current milestone surface
```

with exit code 3. We eventually confirmed the milestone surface does include
declarations, arithmetic, and strings — `integer x = 42` compiles to a real
component — which is how we got our end-to-end test working.

The exit-3 message is genuinely good: it's specific, it doesn't pretend to be a
syntax error, and it made the situation obvious once we saw it. The only reason
we mention it at all is that the scaffolded "hello world" a new developer gets
does not currently compile, which is a rough first five minutes. That's ours to
fix on the scaffold side, and we will.

---

## What the framework is doing about all this

- **#1 and #2:** already fixed and committed. We match the real interface, and
  we parse the real diagnostics format.
- **#3:** waiting on the decision above. Right now a project needs an explicit
  `optimization` in `clean.toml` to build, which we don't consider a durable
  answer.
- **#4:** we'll make `cln new` generate something that compiles on the current
  milestone surface, and revisit when `print` lands.

We've also added a test suite that runs the framework against a real
Manager-installed compiler and skips when none is present. Everything in this
document was found by hand this afternoon; from now on it's checked
automatically. If the compiler's interface moves again, we'll see it on the
next test run rather than on some developer's first build.

---

## Detail, if useful

The exact commands we used, so anything here can be reproduced:

```bash
# The failing invocation (exit 2, before this was fixed)
clean-compiler compile --stdout-tar < request.json

# The working one
clean-compiler --request request.json --out ./out
ls out/    # component.wasm  build-manifest.json  diagnostics.json

# The RQD002 case: a request with no `build.optimization`
clean-compiler --request no-optimization.json --out ./out
cat out/diagnostics.json

# The milestone-surface case
printf 'start:\n\tprint("hello")\n'      # exit 3, outside milestone surface
printf 'start:\n\tinteger x = 42\n'      # compiles, 43489 bytes
```

The build manifest 0.1.0 produces looks right to us — `spec_version`,
`compiler` (with a sha256), `request_sha256`, `inputs`, `resolved_config`,
`overrides`, `outputs`, `diagnostics`, `timings`. We relay it into
`dist/build-manifest.json` verbatim and re-read only `compiler.version`, so its
schema stays yours.

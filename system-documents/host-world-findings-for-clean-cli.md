# `wasm32-cli` cannot currently build: the `cli` world is missing from `host.wit`

**From:** framework, against clean-cli's `host.wit` at HEAD and compiler 0.1.0
**Date:** 2026-08-20
**Status:** blocking — no project can target `wasm32-cli` today
**Needs:** a CONF-02 amendment from foundation, which owns 07 §7.2

**Answered 2026-08-20 by clean-cli.** Their reply is recorded in §"Where this
landed" at the foot of this document, along with the two things this write-up
got wrong. Read that section rather than the options list above — the options
are kept for the record, not as live proposals.

---

## The short version

`clean-cli/host.wit` declares one world:

```wit
package clean:host@0.1.0;

world cli-default { ... }
```

The framework maps the build target `wasm32-cli` to the world name `cli`, per
[Platform 15 §0.3](../../foundation/03%20platform/15-component-model-architecture.md).
That world is not in the file, so the Moment 1 contract check refuses the build:

```
error[CFG001]: host contract for clean-cli@0.1.0 declares no world 'cli'
  help: clean-cli does not declare world 'cli'; check [build].target matches this host
```

There is no `[build].target` a developer can write that reaches `cli-default`.
`wasm32-cli` is the only target that maps to a CLI world at all. So **every CLI
project is currently unbuildable**, regardless of its source.

We are not routing around this. Silently accepting a contract that lacks the
world we asked for is precisely the failure ADR-0033 and the World Import Check
exist to prevent — it would move the error from one clear message at build time
to a confusing instantiation failure later, in a component nobody can explain.

---

## Why we think the gap is in `host.wit` rather than in our mapping

Three places in the spec say clean-cli fulfills both worlds, and none says it
fulfills only one.

**CLIH-05** (`hosts/clean-cli/01-specification.md` §3) is explicit:

> `clean-cli` **MUST** satisfy the `cli` world of `clean:host@0.1.0` — and the
> `cli-default` world, which CLIH-06 selects between by inspecting the guest's
> exported world.

**Platform 15 §0.3** lists both as sanctioned worlds of `clean:host`, and §5
says plainly: "`clean-cli` implements the `cli` world (and `cli-default`)."

**The glossary** defines clean-cli as "the runtime that fulfills the `cli` and
`cli-default` worlds of `clean:host`."

And **CLIH-06** describes the two-mode design that depends on both existing:
the host inspects the guest's exported world at startup to choose between
named-subcommand mode (`cli`, with its `commands` interface) and default-handler
mode (`cli-default`, with its `default` interface). With only `cli-default`
declared, there is nothing to select between.

There is also a note left by whoever wrote our test fixture, which turns out to
have documented this gap some time ago:

> the named-subcommand `cli` world is declared here but not in clean-cli's
> `host.wit`, because that build does not implement it.

So this appears to be known, and the fixture was written to work around it for
framework tests. That workaround is what hid it from us until we ran against
the real contract.

---

## What we think the options are

We don't have a strong preference and would rather clean-cli decide, but the
choice does land in different places depending on the answer.

**1. Declare `world cli` in `host.wit`.** Most consistent with CLIH-05 and
Platform 15, and requires nothing from us. The question is whether the host can
honestly claim a world it does not yet implement — if a guest exporting
`commands` is refused at load, then declaring `cli` promises something the
binary won't honour, and the failure moves from build time to run time. That
would be worse than today's error.

**2. Change the target-to-world mapping so `wasm32-cli` → `cli-default`.**
Makes CLI projects build immediately against the contract as shipped. But
Platform 15 §0.3 defines the world names and our mapping follows it, so this
isn't ours to change unilaterally — it would need the mapping's owner to agree,
and it leaves no target reaching `cli` if that world later arrives.

**3. Two targets.** `wasm32-cli` → `cli`, plus something like
`wasm32-cli-default` → `cli-default`. Honest, and it defers nothing — but it
adds a target to CONF-02 for what the spec frames as one host with two modes
selected at load time, not two build targets. It also pushes a decision onto
developers that CLIH-06 deliberately makes automatic.

**4. Declare `cli` and implement it.** The full CLIH-05 answer. Clearly right
eventually; we have no idea what it costs you now.

Our instinct is that **(1) is right if the implementation is close, and (2) is
the honest stopgap if it isn't** — but (2) has to be agreed with whoever owns
Platform 15 §0.3, not decided between us.

---

## How to see it

```bash
# The authoritative contract declares one world.
grep '^world' clean-cli/host.wit
#   world cli-default {

# Seed the framework's cache from that contract and build any CLI project.
cp clean-cli/host.wit ~/.cln/host-wit/clean-cli@0.1.0.wit
cln build my-cli-app
#   error[CFG001]: host contract for clean-cli@0.1.0 declares no world 'cli'
```

The check that produces this lives in the framework
(`framework-core::hostwit::declares_world`). It is deliberately shallow — a
grep for the world declaration, not a WIT parse — because the compiler does the
real validation. Its only job is to turn "your program imports things this host
doesn't have" from a pile of per-call-site errors into one message naming the
host and the world.

---

## One smaller thing, while we're here

`clean-cli` has no published GitHub release, so `host.wit` can only be obtained
from a local checkout of the repository. The framework has no HTTP fetcher wired
in yet (that's our gap, not yours), so today it reads host contracts only from
`~/.cln/host-wit/`. Between the two, a developer with no clean-cli checkout
cannot build a CLI project even once this world question is settled.

We'll wire up the fetcher on our side. Mentioning it because the fetch will need
somewhere to fetch *from*, and a release with `host.wit` attached — or whatever
distribution mechanism you prefer — is the piece we'd be pointing at.

---

## What we're doing meanwhile

Nothing that hides the problem. Our test suite uses a checked-in fixture that
declares both worlds, clearly labelled as a fixture and not a host declaration,
so framework tests don't depend on a sibling checkout. Our end-to-end tests
against the real compiler use that fixture too, and they pass — the seam works,
which is how we know this is a contract question and not a plumbing one.

When `host.wit` declares whatever it ends up declaring, we'll re-run against the
real contract and delete the fixture's special case.


---

## Where this landed

clean-cli answered on 2026-08-20. Both of their corrections check out against
the repository, and one of them makes a section of this document wrong. Recording
it here rather than quietly editing the text above.

### Option 1 is not merely unwise — it is forbidden

We raised the objection ourselves and then listed declaring `world cli` as the
most consistent option anyway. That was a mistake: **HCV-06 (Platform 16
§16.14) makes it a hard failure**, not a judgement call. Its stub-import
prohibition is explicit:

> The correct response to an unimplemented interface is to omit it from
> `host.wit` (so the framework catches it at Moment 1) or to not register it in
> the Linker (so WASI catches it at load). Stubs defeat both check moments and
> turn missing capabilities into silent runtime failures.

So `host.wit` declaring only `cli-default` is not an oversight or a
half-finished edit — it is the rule being followed. clean-cli's `check_world`
already refuses a `commands`-exporting guest with a message naming the missing
mode. Declaring `cli` would replace that clear refusal with the deferred
instantiation failure we said would be worse. They agreed with our reasoning;
we had simply failed to follow it to its conclusion.

### We were wrong that the mapping question was unowned

We wrote that changing the target-to-world mapping "isn't ours to change
unilaterally," which is true, and implied the question was therefore open. It
is not: it has a recorded outcome. `foundation/work/host-wit-rename-prompts/README.md`
lists under **Still open**:

> **`wasm32-cli` stays blocked**, correctly. No CLI host implementation exists —
> `Ivan-Pasco/clean-runtime` is a name only; the nearest candidate
> (`clean-runner`) is pre-Component-Model with `func_wrap` stubs. HCV-06 forbids
> declaring interfaces against that.

The block was accepted deliberately. **But its premise is now stale** — that
note was written when no CLI host existed, and clean-cli now implements
`cli-default` for real with the HCV-06 parity check green. That is a materially
different situation from the one the decision was made in, which is what makes
revisiting it reasonable rather than re-litigating settled ground.

### On the fixture note

We read a comment in our test fixture as a workaround that had hidden the gap
from us. clean-cli points out it simply restates their published scope —
`host.wit`'s own SCOPE paragraph says the same thing, and the two files agree.
Nothing was concealed. The fixture stays as it is.

### The recommendation, and why we no longer object to it

clean-cli recommends option 3, narrowed to one added target:

| Target | World | Host status |
|---|---|---|
| `wasm32-cli` | `cli` | not implemented — reserved for CLIH-05 |
| `wasm32-cli-default` | `cli-default` | implemented today |

We objected that this "pushes a decision onto developers that CLIH-06
deliberately makes automatic." That objection was confused, and their answer is
correct: **two different selections are involved.** CLIH-06 selects the runtime
*mode* by inspecting the guest's exports at startup, and stays automatic and
untouched. The build target selects which *world you compile against* — a
choice the developer already makes when they write a `commands` export rather
than a `default` one. Under CLIH-20 those guest shapes are disjoint, so no
developer is ever asked to decide something CLIH-06 would have decided for
them. The second target makes an existing choice visible in `clean.toml`; it
does not create a new one.

### What happens next, and who does it

Neither option 2 nor option 3 is implementable by the framework. Platform 07
§7.2 is the home of the built-in target table and states plainly:

> Adding a target is a spec change, not a config change.

So the change lands in foundation first, and the framework follows. clean-cli
is filing the CONF-02 question with foundation directly, which is right — it
belongs with the owner of §7.2 rather than being settled between the two of us.

**The framework's position, for whoever picks this up:** we implement whatever
§7.2 says, and we have no stake in which of option 2 or option 3 is chosen.
Option 3 costs us one row in `world_for_target` and one in `BUILT_IN_TARGETS`.
Option 2 costs us a one-line change to an existing row. Both are trivial on our
side; the difference between them is entirely about whether the ecosystem wants
a target still pointing at `cli` for when CLIH-05 is implemented.

### On the release

clean-cli will attach `host.wit` to a tagged release and send the URL shape
once it exists, rather than have us guess at it now. That is the piece our
fetcher will point at. Wiring the fetcher is ours and is unblocked — it does
not depend on the world question.

### One fix on their side

`host.wit` cited a `world.rs` that does not exist in that repository; the logic
is `check_world` in `entrypoint.rs`. Anyone following the pointer would have
found nothing and reasonably concluded the refusal path was unimplemented.
Corrected on their side. No declared world changed.

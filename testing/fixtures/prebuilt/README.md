# Prebuilt components — compiler output, stood in for

These are hand-built WASM components that stand in for what the Clean compiler
will emit once it can. They exist so the framework's own pipeline — compile
seam, `dist/` write, `.clapp` assembly, integrity hashing, host-config
generation — can be exercised against a payload that is *actually runnable*,
before the compiler is ready to produce one.

## Why this is needed at all

`testing/fake-compiler` normally emits an 8-byte component preamble. That is
correct and sufficient for almost every framework test: nothing above the
compiler seam looks past the WASM header, so a preamble exercises every code
path the framework owns.

It is not sufficient for one thing — the deploy contract. A preamble packages
cleanly, hashes cleanly, and verifies cleanly, then fails the instant a runtime
tries to instantiate it. That failure surfaces at deploy time in whichever
component receives the archive, far from where it was introduced. These
fixtures move that failure back into the framework's own test suite.

`crates/framework-cli/tests/cli_real_component.rs` is the consumer. It points
`FAKE_COMPILER_WASM` at `hello-cli.wasm`, asserts the bytes survive the whole
pipeline unaltered, and then unzips the resulting `.clapp` and runs it through
the real `clean-cli` host — asserting stdout is exactly `hello\n` with exit 0.

## `hello-cli.wasm`

Targets the `cli-default` world declared in
[clean-cli's `host.wit`](../../../../clean-cli/host.wit) — the authoritative
contract under HCV-02:

```wit
world cli-default {
    export env;
    import run: func(argv: list<string>, stdin-tty: bool) -> u8;
}
```

Declared from the host's perspective, so the host's `import run` is the
**guest's export**. The guest exports:

```wit
export run: func(argv: list<string>, stdin-tty: bool) -> u8;
```

and writes to stdout through `wasi:cli/stdout@0.3.0`. It is byte-identical to
`clean-cli/testing/hello/hello.wasm`, the reference guest for this contract,
which is the strongest available statement that it targets the real thing.

Source of truth is `gen.py`, which writes `hello-cli.wat`. Both are committed
beside the `.wasm`, so the test suite needs no WASM toolchain to run.

### Why it is authored at the component level

The obvious path — write a core module, run `wasm-tools component embed` then
`component new` — cannot express this contract. WASI 0.3 replaced Preview 2's
byte-oriented `write` with a native `stream<u8>`, so writing to stdout means
creating a stream with `stream.new`, handing its readable end to
`write-via-stream`, and pushing bytes into the writable end. Those are
canonical built-ins that exist only at the component level; a core module has
no way to name them, so there is nothing for `embed` to bind.

`gen.py` therefore emits a full component, with memory split into its own core
module to break the cycle between `stream.write`'s `memory` option and the
module that calls it. Its header comment explains the construction in detail.

### Regenerating

Requires `wasm-tools`. From this directory:

```sh
python3 gen.py
wasm-tools parse hello-cli.wat -o hello-cli.wasm
wasm-tools validate --features all hello-cli.wasm
```

This is byte-reproducible: regenerating from an unchanged `gen.py` yields an
identical `hello-cli.wasm`.

Verify the result declares the contract it should:

```sh
wasm-tools component wit hello-cli.wasm
```

Expected — note `run` as an export, and no `clean:*` imports at all, since this
guest needs no capability bridge:

```wit
world root {
  import wasi:cli/types@0.3.0;
  import wasi:cli/stdout@0.3.0;

  export run: func(argv: list<string>, stdin-tty: bool) -> u8;
}
```

### Running it by hand

```sh
../../../../clean-runtime/target/release/clean-runtime --world=cli \
    hello-cli.wasm --config=<a host.toml pointing at it>
```

`--world=cli` selects the *host*; `[guest] world` in the config is guest
metadata that clean-cli cross-checks against the guest's actual exports.

## Retiring these

When the compiler can emit a `cli-default` component for `hello-world`, the
Layer C tests in `crates/framework-cli/tests/real_compiler.rs` cover this
ground against real output, and these fixtures become redundant. Delete this
directory and `cli_real_component.rs` together at that point — a stale stand-in
that no longer matches what the compiler produces is worse than no stand-in,
because it reads as coverage of a contract it has stopped tracking.

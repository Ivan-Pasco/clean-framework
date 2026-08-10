# Clean Framework

The build orchestrator for the Clean Language ecosystem. It turns a project
directory into `dist/app.wasm` by handing the compiler one self-contained
request document.

The framework has no user-facing CLI. Users type `cln build`; Clean Manager
routes that to the `clean-framework` binary it installed under
`~/.cln/versions/framework/<version>/` (Governance §2.4, Manager §00.4).

**Status: M0.** Hello-world compiles. No dependencies, plugins, caching, watch
mode, or packaging yet — see [PLAN.md](PLAN.md) for the full roadmap.

## Layout

| Crate | Responsibility |
|---|---|
| [crates/framework-core/](crates/framework-core/) | Orchestration: discovery, manifest, lowering, request assembly, output placement. No argv, no compiler transport. |
| [crates/framework-compiler-driver/](crates/framework-compiler-driver/) | The single seam to the compiler. The `RequestDocument` type, the artifact tarball reader, and the subprocess transport. |
| [crates/framework-cli/](crates/framework-cli/) | The `clean-framework` binary. A thin argv adapter over `framework-core`. |
| [testing/fake-compiler/](testing/fake-compiler/) | A compiler stand-in speaking the real subprocess protocol, so framework tests don't depend on compiler stability. |

The compiler seam is deliberately one crate. When the compiler grows a second
transport (JSON-RPC per Platform 14 §14.2.3, or the framed warm-process mode
`cln dev` needs), that is an added file there rather than a scatter change
across the orchestrator.

## Building

The workspace depends on `cln-shared` and `cln-layout` from
[clean-manager](https://github.com/Ivan-Pasco/clean-manager) **by path**, so
check both repos out as siblings:

```
Clean Language/
├── clean-framework/     # this repo
└── clean-manager/
```

These become git pins at M1, once the manager's types stabilize. Until then a
path dependency avoids a push-and-bump cycle for every shared-type tweak. CI
checks out the sibling repo to match; that step disappears with the pin.

> **CI setup:** `clean-manager` is a private repo, and a workflow's default
> `GITHUB_TOKEN` can only read the repository it runs in. Both workflows
> therefore check it out with a read-only **SSH deploy key**, stored here as
> the `CLEAN_MANAGER_DEPLOY_KEY` secret and registered on `clean-manager`.
> A deploy key is used rather than a PAT because it is bound to that one
> repository and is read-only, so a leak exposes nothing else. Without the
> secret, CI fails at the checkout step with `Input required and not
> supplied`. This whole arrangement disappears at M1 with the git pin.

```sh
cargo build --workspace
cargo test --workspace
```

## Testing

Three layers, per [PLAN.md §7](PLAN.md):

- **Layer A — unit tests.** Per module, hermetic, no compiler. `cargo test`.
- **Layer B — orchestration.** Drives the real subprocess transport against
  `fake-compiler`. Every FRM-BO rule has a test here. `cargo test`.
- **Layer C — the real compiler.** Requires a Manager-installed compiler and
  is excluded from the default suite:

  ```sh
  cln install compiler <version>
  cargo test -- --ignored
  ```

  When no compiler is installed, Layer C **fails loudly** rather than skipping
  quietly — a green suite that silently never ran the real compiler reads as
  proof of something it never checked.

## Contracts worth knowing

- **The compiler never sees the filesystem** (FRM-BO-02, CMP-01). If a build
  needs a file, the framework reads it and puts its contents in the request
  document. This is what makes builds reproducible from the request alone.
- **The request document is deterministic.** Identical project state produces
  byte-identical JSON, regardless of where the project lives on disk or which
  OS built it. CI asserts both. This is what makes the compiler's CMP-02
  externally provable.
- **Failure is total** (FRM-BO-10). `dist/` is either fully replaced by a
  successful build or left exactly as the previous successful build left it.
  Outputs are staged in a temp directory and swapped in.
- **Overrides are audited, not merged** (FRM-BO-08). `--override
  build.optimization=debug` does not rewrite the lowered config; it appends to
  `overrides[]` so `cln repro build` can replay the exact build.

# Clean Framework — Implementation Plan

This is a build plan for `clean-framework`, the build orchestrator for the Clean Language ecosystem. It's derived from `foundation/02 components/framework/*`, `foundation/02 components/manager/00-manager.md`, `foundation/02 components/hosts/00-host-model.md`, and `foundation/03 platform/07,14,15`. Where the plan makes a choice that the spec doesn't force, it's called out inline and cross-referenced.

The framework's job, one sentence: **turn a project directory into `dist/app.wasm` by handing the compiler a self-contained request document, then wrap the result into `.clapp` / `.serve` on demand.** Everything else — file discovery, dependency resolution glue, plugin loading, watch mode, dev server — exists to serve that job.

---

## 1. Language and toolchain

**Choice: Rust.**

Rationale, in order of weight:

1. **The compiler is Rust.** Framework and compiler are shipped as independent binaries installed by Manager into `~/.cln/versions/compiler/<version>/` and `~/.cln/versions/framework/<version>/` (Manager §00.2). Framework invokes the compiler as a subprocess per Platform 14 §14.2.2 — request JSON on stdin, artifact tarball on stdout. Both being Rust means we share crates for the `RequestDocument` type, `clean.toml` schema, and diagnostic format via a small `clean-shared` crate (or by both depending on `foundation/schema` bindings), which prevents JSON drift without linking the compiler as a library.
2. **The manager is Rust.** Manager §00.4 dispatches to `clean-framework` as a component binary. Sharing crates for `clean.toml` parsing, lockfile I/O, `~/.cln/` layout, and the `RequestDocument` type between manager and framework avoids two implementations of the same schema drifting.
3. **We need a WASM component-model runtime in-process** (for step 5 of build orchestration — compiling and executing block-handler WASM, and for `cln dev`'s hot-swap into a running dev host). The mature options here (`wasmtime`, `wasm-tools`, `wac-graph`) are Rust-native.
4. **Watch-mode targets are aggressive.** Platform 14 §14.14.3 asks for < 500 ms rebuild on a medium project. GC pauses in a Node/Go orchestrator eat a meaningful chunk of that budget on cold caches; a Rust orchestrator + warm compiler process cleared that in every prior spike.

Counter-argument considered: **Go for a smaller binary and faster iteration.** Rejected — the wasmtime story is worse, and we'd end up shelling to the compiler on every build, giving up the in-process fast path (see §3 below).

Reference-stack choices to lock down in a follow-up ADR (not in this plan): TOML parser (`toml_edit` for round-trip fidelity when we write `clean.toml`), file watcher (`notify` v6+ with debouncing), ZIP (`zip` crate — the mature one, not `async_zip`), HTTP client for git deps (`ureq` or `reqwest` — sync is fine because we're the fast path, not the server), SHA-256 (`sha2`).

---

## 2. Crate / module layout

Single Cargo workspace, one shipping binary (`clean-framework`), several crates so pieces are independently testable and can be pulled in by the manager without pulling in the whole world.

```
clean-framework/
├── Cargo.toml                          # workspace root
├── PLAN.md                             # this file
│
├── crates/
│   ├── framework-core/                 # the orchestration engine — no CLI, no I/O logic beyond fs
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── build.rs                # orchestrates FRM-BO §11.2 steps 1–10 (build orchestration)
│   │       ├── discover.rs             # §11.3 file discovery (roots, extensions, excludes, order, encoding)
│   │       ├── manifest.rs             # clean.toml load + validate (schema/clean.toml.md)
│   │       ├── lower.rs                # §11.4 clean.toml → request-document lowering
│   │       ├── request.rs              # RequestDocument type (mirrors Platform 14 §14.1.1)
│   │       ├── overrides.rs            # FRM-BO-08 override audit trail
│   │       ├── caches.rs               # §11.7 wit-cache + build-cache read/write
│   │       └── errors.rs               # Framework diagnostic codes (FRM-###, CFG###) → 09 error codes
│   │
│   ├── framework-libraries/            # library + plugin resolution and loading (spec §9, §12)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── library_manifest.rs     # library.toml load (schema/library.toml.md)
│   │       ├── plugin_manifest.rs      # plugin.toml load (schema/plugin.toml.md + FRM-PM-01..03)
│   │       ├── resolver.rs             # reads .cln/lock.toml written by Manager; walks the closure
│   │       ├── handler_build.rs        # step 5: compile block handlers, cache to ~/.cln/wit-cache/
│   │       └── host_bridge_wit.rs      # synthesize WIT from `host_bridge.cln` per LBS §8
│   │
│   ├── framework-compiler-driver/      # the single seam to the compiler
│   │   └── src/
│   │       ├── lib.rs                  # trait Compiler { fn compile(&self, req) -> Result<Artifact, Diags> }
│   │       ├── subprocess.rs           # resolves ~/.cln/versions/compiler/<version>/clean-compiler
│   │       │                           #   from .cln/version, spawns with request JSON on stdin,
│   │       │                           #   reads artifact tarball on stdout (Platform 14 §14.2.2)
│   │       └── warm_pool.rs            # keeps a single compiler process alive across `cln dev`
│   │                                   #   rebuilds — see §6, framing = one long-lived stdin pipe
│   │
│   ├── framework-package/              # §11.6 FRM-BO-09a + Manager §00.14 packaging
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── clapp.rs                # .clapp assembly (ZIP, manifest.toml, integrity)
│   │       ├── serve.rs                # .serve assembly
│   │       ├── standalone.rs           # embed ~/.cln/active/runtime into a native binary
│   │       └── raw.rs                  # --raw wasm passthrough
│   │
│   ├── framework-dev/                  # watch mode + hot-reload orchestration
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── watcher.rs              # notify-based watcher over §11.3 roots, with debouncing
│   │       ├── loop.rs                 # rebuild + swap; keeps compiler process warm
│   │       └── runtime_bridge.rs       # spawns/manages the pinned runtime, requests hot-swap
│   │
│   ├── framework-scaffold/             # cln new + cln library create
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── templates.rs            # embedded templates: app, library, minimal
│   │       └── writer.rs               # writes clean.toml + folder skeleton
│   │
│   ├── framework-migrate/              # cln db migrate <verb> (spec §11 owns the entry point)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── discover.rs             # walk app/data/migrations/
│   │       └── driver.rs               # dispatches to db-drivers/ (spec §04)
│   │
│   ├── framework-mcp/                  # cln mcp — the Clean MCP server (spec §10)
│   │   └── src/
│   │       ├── lib.rs
│   │       └── tools.rs                # list_libraries, get_library_examples, get_feature_spec, …
│   │
│   └── framework-cli/                  # thin argv adapter used by Manager
│       └── src/
│           ├── main.rs                 # ships clean-framework binary
│           └── verbs.rs                # dispatches build/dev/package/new/library/db/mcp
│
├── testing/
│   ├── fake-compiler/                  # a compiler stand-in (see §7)
│   ├── fixtures/                       # tiny projects: hello-world, one-plugin, two-plugins-collision
│   └── golden/                         # golden request documents + build manifests
│
└── docs/
    └── (empty initially; add ADRs as we lock decisions)
```

**Why this shape:**

- `framework-core` is pure orchestration — takes a `PathBuf`, returns a `CompileArtifact` (or diagnostics). No knowledge of argv, no knowledge of the compiler transport. Testable in isolation with the fake compiler.
- The compiler seam is *one crate* (`framework-compiler-driver`). If the compiler adds a new transport (MCP/JSON-RPC per Platform 14 §14.2.3), it's an added file here, not a scatter change across the orchestrator.
- `framework-cli` is 200 lines. All the logic is in the other crates; the CLI just wires argv → orchestration entry points. That's what lets Manager call the framework as a library later without carrying the argv parser (see §3).
- `framework-scaffold`, `framework-migrate`, `framework-mcp` are shipped in the same binary but are separate crates because they don't share code with `build.rs` and shouldn't couple to it. If the MCP server grows a heavy dependency (a search index, a semantic-tokens engine), it doesn't pollute `cln build`.

**Spec → module map (the important ones):**

| Spec section | Module |
|---|---|
| FRM-BO-01, FRM-BO-02 (ownership + no-fs-for-compiler) | `framework-core::build` |
| §11.3 file discovery (FRM-BO-03..07) | `framework-core::discover` |
| §11.4 lowering (FRM-BO-08 overrides) | `framework-core::lower`, `framework-core::overrides` |
| §11.5 request payload | `framework-core::request` |
| §11.6 response handling (FRM-BO-09, FRM-BO-10) | `framework-core::build` |
| §11.7 caches | `framework-core::caches` |
| §11.8 watch mode | `framework-dev::loop`, `framework-dev::watcher` |
| §9 library system (`library.toml`, host_bridge synthesis) | `framework-libraries::*` |
| §12 plugin manifest (FRM-PM-01..03) | `framework-libraries::plugin_manifest` |
| Manager §00.14 packaging (.clapp, .serve, --standalone, --raw) | `framework-package::*` |
| §10 MCP server | `framework-mcp::*` |
| §04 database driver ABI (dispatch, not implementation) | `framework-migrate::driver` |

---

## 3. Public API shape

Manager dispatches to the framework binary per Manager §00.4. But because both are Rust and both live in the same organization, we get to choose: **process-per-verb**, or **linked library**. This plan chooses **both, with the linked library as the primary contract.**

**Primary surface: `framework-core::api` — the linked-library entry points.**

Every verb Manager dispatches to the framework has a corresponding function here. Return types are structured `Result` values, not exit codes.

```rust
// framework-core/src/lib.rs

pub struct BuildInputs {
    pub project_root: PathBuf,
    pub overrides: Vec<Override>,        // from --override, CLN_* env vars
    pub target: Option<String>,          // --target=wasm32-server
    pub optimization: Option<String>,    // --optimization debug|release|size
    pub strip_checks: bool,              // --strip-checks
    pub offline: bool,                   // inherit from Manager --offline
}

pub struct BuildOutcome {
    pub artifact: Option<CompileArtifact>,   // present on success
    pub dist_wasm: Option<PathBuf>,          // where the framework wrote it (§11.6 FRM-BO-09)
    pub diagnostics: Vec<Diagnostic>,        // shape from Platform 13; empty on cold-cache hit
    pub build_manifest_path: PathBuf,        // dist/build-manifest.json
}

/// Steps 1–10 of §11.2. Non-blocking on the compiler seam only in the sense that
/// diagnostics are collected before returning; a compiler error still returns Ok(outcome)
/// with `artifact = None` and a non-empty `diagnostics`. Err() is reserved for framework
/// bugs and unrecoverable I/O.
pub fn build(inputs: BuildInputs) -> Result<BuildOutcome, FrameworkError>;

/// Runs passes 1–9 of the compiler (Platform 14 §14.14.4). Same inputs, no wasm emitted.
pub fn check(inputs: BuildInputs) -> Result<BuildOutcome, FrameworkError>;

pub struct PackageInputs {
    pub project_root: PathBuf,
    pub kind: Option<PackageKind>,          // Clapp | Serve; None = auto-detect per §11.6
    pub standalone_targets: Vec<TargetOs>,  // --os=windows,macos,linux; empty = not standalone
    pub raw_target: Option<String>,         // --raw --target=<world>
    pub output_dir: PathBuf,                // where to write .clapp / .serve / native binary
}

pub struct PackageOutcome {
    pub artifacts: Vec<PathBuf>,            // one per produced file
    pub kind: PackageKind,
}

/// FRM-BO-09a. Triggers a build if dist/app.wasm is missing or stale.
pub fn package(inputs: PackageInputs) -> Result<PackageOutcome, FrameworkError>;

pub struct DevInputs {
    pub project_root: PathBuf,
    pub host_address: Option<SocketAddr>,   // dev host bind; default 127.0.0.1:3000
    pub overrides: Vec<Override>,
}

/// Blocks until Ctrl-C. Returns Ok on clean shutdown, Err on unrecoverable failure.
pub fn dev(inputs: DevInputs, shutdown: oneshot::Receiver<()>) -> Result<(), FrameworkError>;

pub fn new_project(template: TemplateRef, path: &Path) -> Result<(), FrameworkError>;
pub fn library_create(name: &str, path: &Path) -> Result<(), FrameworkError>;
pub fn library_publish(project_root: &Path) -> Result<(), FrameworkError>;

pub fn db_migrate(verb: MigrateVerb, project_root: &Path) -> Result<MigrateOutcome, FrameworkError>;

pub fn mcp_serve(project_root: &Path, transport: McpTransport) -> Result<(), FrameworkError>;
```

**Secondary surface: `clean-framework` binary.** The `framework-cli` crate is a thin argv → api translator. It parses flags, calls the corresponding `framework-core::api::*` function, converts the `BuildOutcome` / diagnostics to the format Manager expects on stdout, and sets an appropriate exit code. This is what Manager invokes today (Manager §00.4).

**Why both:** the binary is what makes Manager version-safe — Manager can dispatch to `~/.cln/versions/framework/<version>/clean-framework` and get an ABI-stable argv+stdout contract. The library is what lets `cln` collapse into one binary later if we want to (no version-mismatch story would be needed for the framework/manager pair on that path), and it's what tests hit — no subprocess spawn per test case.

**Diagnostic transport across the binary boundary.** The binary writes `diagnostics.json` (Platform 13 shape) to stdout in a machine-readable envelope. Manager parses it and renders to the terminal. This is the same shape the compiler uses so Manager has one diagnostic renderer, not two.

---

## 4. Build pipeline implementation order

The goal is to compile and run **hello-world end-to-end as early as possible**, then layer capabilities. Each phase below is a working system; nothing is half-implemented and shipped.

**Phase 0 — Skeleton (no builds yet).** `framework-core` crate exists. `clean.toml` parser (`manifest.rs`) round-trips the smallest valid manifest per schema. `RequestDocument` type serializes to the Platform 14 §14.1.1 JSON shape and passes a byte-exact round-trip test against a canned fixture. No compiler seam yet.

**Phase 1 — Hello-world happy path (steps 1, 2, 6, 7, 8, 9, 10 of §11.2, minus plugins).** Discover source files (§11.3, no plugin path extensions yet, `.cln` only). Read `clean.toml`. Skip dependency resolution — assume no `[dependencies]`. Lower to request document. Resolve the pinned compiler binary from `.cln/version` → `~/.cln/versions/compiler/<version>/clean-compiler`; if missing, emit a diagnostic pointing at `cln install compiler <version>` and stop. Spawn the compiler as a subprocess via `framework-compiler-driver::subprocess`, write request JSON to stdin, read artifact tarball from stdout. Write `dist/app.wasm` (FRM-BO-09) or fail totally (FRM-BO-10). No caching, no watch, no packaging.

At the end of this phase: `cln build hello-world` produces `dist/app.wasm` and `cln run dist/app.wasm` (Manager-owned) executes it. **This is the milestone that de-risks everything else.**

**Phase 2 — Dependency resolution glue (step 3, step 4).** Read `.cln/lock.toml` written by Manager. Walk the closure; load each `library.toml` or `plugin.toml`. Pass their manifests into `request.library_manifests[]`. Wire up the Manager callback (`cln fetch --internal`) that lets `cln build` trigger a resolve when the lockfile is absent or stale — per Manager §00.8. Path-only and git-only deps are supported (per user decision — no registry yet).

**Phase 3 — Block-handler compilation (step 5).** For every library with a `handles block` declaration, compile the handler source to WASM (via the compiler seam), cache under `~/.cln/wit-cache/` keyed by SHA-256 (ADR-0004). Pass the cached WASM's hash into the request document as `library_manifests[].compiletime_wasm_sha256`. The compiler executes the handler in its sandbox during pass 6.

**Phase 4 — Plugin `.wasm` loading.** Load `plugin.toml`, validate FRM-PM-01..03. Check that every export named in `[exports]` is present in `plugin.wasm`. Include the plugin manifest in the request document. Register plugin-declared `[paths].owns` folders as additional discovery roots (§11.3 FRM-BO-03 item 3). Register plugin-declared `[paths].patterns` extensions (§11.4).

**Phase 5 — Caching (§11.7).** Build-cache (`~/.cln/build-cache/`) keyed by request-document SHA-256. Consulted before invoking the compiler; a hit skips the compile entirely and byte-identically reproduces the output (Platform 14 CMP-06). Block-handler cache is already present from Phase 3; wire it into a `cln cache` inspection command for Manager to expose later.

**Phase 6 — Packaging (`cln package`, FRM-BO-09a).** `framework-package::clapp`, then `framework-package::serve`. Auto-detect kind from `clean.toml` (single non-server world → clapp; `server` world → serve). Populate `manifest.toml` from the build manifest returned by the compiler. Compute integrity hashes. `--raw` and `--standalone` land in this phase in that order.

**Phase 7 — `cln dev` (§11.8 + hot-reload).** Watch mode. Warm compiler process. `notify`-based file watcher with 50ms debounce. On change: re-run discovery (steps 1–7), call `compile_incremental` (Platform 14 §14.14.3), swap the fresh `component.wasm` into a running dev host instance managed by `framework-dev::runtime_bridge`. Hot-swap protocol is owned by hosts/clean-server/01-server §1.9 — framework just calls its reload endpoint with the new bytes.

**Phase 8 — Everything else that Manager dispatches to us.** `cln new`, `cln library create`, `cln library build`, `cln library publish`, `cln db migrate <verb>`, `cln mcp`. Each is a separate crate (§2) and lands independently. `cln api spec` / `cln api sdk` are owned by the server library per Manager §00.3.5, so framework just proxies the compile artifact to that library's tooling — small.

---

## 5. `cln package` implementation

Handled entirely inside `framework-package`. `cln package` (dispatched by Manager) calls `framework-core::api::package(inputs)`.

**Flow, per FRM-BO-09a:**

1. **Ensure `dist/app.wasm` exists and is current.** Compare the request document that would produce it now against the request document recorded in the last `build-manifest.json`. If they differ (or `dist/` is empty), call `build(inputs.into())` first.

2. **Auto-detect package kind** (unless the caller passed `--kind`). Read the target world declared in `clean.toml [build].target` (or the default). `server` → `.serve`; anything else → `.clapp`. Mixed cases (project declares `server` + `cli` + `worker` — currently unsupported by the spec's single-target model) will need a `--kind` override until multi-world packaging is spec'd.

3. **Assemble.** For `.clapp`:
   - Open a `ZipWriter` at `<output_dir>/<name>-<version>.clapp` (deflate).
   - Write `manifest.toml` — every field from Manager §00.14's shape, populated from the compiler's `build_manifest.json` (`compiler_version`, `framework_version`, `runtime_version`, `built_at`, `built_by`, `worlds`, `entries`) plus the currently-active runtime version resolved via `~/.cln/active/runtime` symlink read.
   - Copy `dist/app.wasm` to `app.wasm` inside the archive.
   - Walk the project's declared asset roots (default `public/`) and bundle under `assets/`.
   - Optionally read `README.md` from project root if present.
   - Compute SHA-256 of `manifest.toml` bytes → `integrity.manifest_sha256`. Compute SHA-256 of `app.wasm` → `integrity.wasm_sha256["app.wasm"]`. Rewrite `manifest.toml` with these fields populated (the first pass writes them empty as placeholders; we do a second serialization with them filled and update the archive entry). Simpler: buffer `manifest.toml` in memory, compute all hashes before writing anything, then write in one pass. **Do the second one.**
   - Optional signature (deferred to Phase 6.5 with the signing story).

   For `.serve`, same shape but with a `wasm/` subdirectory (one wasm per world), a `migrations/` copy from `app/data/migrations/`, and `config/clean.toml.template` written from the project's `clean.toml` with secrets stripped.

4. **`--standalone`.** Load `~/.cln/active/runtime`, embed the runtime binary + the `.clapp` bytes into a native executable per target OS. Cross-compilation is delegated to Manager (which owns `~/.cln/versions/runtime/`); framework asks Manager for the right runtime binary per `--os=<list>` value.

5. **`--raw --target=<world>`.** Copy `dist/app.wasm` to `dist/<name>-<world>.wasm`. Zero transformation — the wrapper is additive per Manager §00.14. Byte-identical to what's inside `.clapp`.

**File paths and function signatures worth pinning:**

- `framework-package/src/clapp.rs::assemble(inputs: &PackageInputs, artifact: &CompileArtifact, runtime_version: &str) -> Result<PathBuf>`
- `framework-package/src/manifest_toml.rs::render(build_manifest: &BuildManifest, runtime_version: &str, kind: PackageKind) -> Result<String>` — returns the TOML string; hashes are patched in by the caller after the wasm is finalized.

---

## 6. `cln dev` architecture

The user-facing feature we cannot get wrong. Loop shape:

```
┌────────────────────────────────────────────────────────────────┐
│  1. Warm compiler subprocess (spawned once via framework-      │
│     compiler-driver::warm_pool; the compiler process stays     │
│     alive across rebuilds, framed request JSON on stdin per    │
│     rebuild, framed artifact tarball on stdout)                │
│                                                                │
│  2. Initial build → dist/app.wasm → runtime instance spawned   │
│     via framework-dev::runtime_bridge; hosts on 127.0.0.1:3000 │
│                                                                │
│  3. framework-dev::watcher (notify crate) subscribes to §11.3  │
│     roots. Debounces bursts (default 50ms) into one FS event   │
│     batch.                                                     │
│                                                                │
│  4. On batch:                                                  │
│     a. Re-run discover+lower → new RequestDocument             │
│        (never mutated in place; §11.8 requires fresh from fs)  │
│     b. Compare request SHA-256 to last built request. If same, │
│        no-op — save/re-save of an unchanged file.              │
│     c. Call compile_incremental(request, previous_artifact) —  │
│        Platform 14 §14.14.3. Framework's request-assembly work │
│        is identical to a full build; compiler decides what to  │
│        re-lower.                                               │
│     d. If diagnostics.errors.is_empty(): send                  │
│        POST /_cln/hot-swap with the fresh wasm bytes to the    │
│        runtime instance (protocol owned by                     │
│        hosts/clean-server/01-server §1.9).                     │
│        Else: emit diagnostics on stdout, keep the previous     │
│        wasm in place (never leave dist/ half-updated —         │
│        FRM-BO-10).                                             │
│                                                                │
│  5. On Ctrl-C: shut down runtime instance, kill compiler       │
│     process, exit clean.                                       │
└────────────────────────────────────────────────────────────────┘
```

**Key correctness invariants:**

- **Request document is regenerated from scratch on every rebuild** (§11.8 — "never edits the request document in place"). No stale-cache surprises.
- **Watcher exclusions match §11.3 FRM-BO-05.** Dotfiles, `dist/`, `target/`, `node_modules/`, `.build/`, `.cln/`. We don't watch our own outputs and cause a rebuild loop.
- **Latency budget.** Watcher debounce (50ms) + discover+hash+lower (~50ms for medium project) + compiler `compile_incremental` (~300ms for a one-file change per §14.9) + hot-swap POST (~50ms). Total ~450ms — under the 500ms §14.14.3 target.
- **Compiler process is warm.** Spawning a fresh subprocess per keystroke burns the budget on OS process creation. We spawn once at `cln dev` startup, use a length-framed protocol (4-byte length prefix + JSON) on stdin/stdout, and reuse the same process for every rebuild. When `.cln/version` changes mid-session (rare — usually only on `cln pin`), we tear down and respawn against the new binary path.
- **Warm-process protocol is a compiler contract.** Platform 14 §14.2.2 defines the one-shot process adapter but not framed multi-request mode. This needs a small ADR before Phase 7 (open question #9 below).

**What we don't build:** state preservation across hot-swap. That's a runtime concern (hosts/clean-server), not ours. We just deliver bytes.

---

## 7. Testing strategy

The framework touches the compiler on every meaningful test. We need a way to write framework tests without a working compiler binary in the loop, or every framework CI run becomes a compiler CI run.

**Three layers:**

**Layer A — unit tests.** Per module. `discover.rs` gets a `tempdir` with a fake project tree; asserts it enumerates the right paths. `lower.rs` gets a `clean.toml` string; asserts the resulting `RequestDocument` matches a golden JSON. `manifest_toml.rs::render` gets a canned `BuildManifest`; asserts output byte-for-byte. Fast, hermetic, no compiler.

**Layer B — orchestration tests with `fake-compiler`.** A test crate (`testing/fake-compiler`) that implements the same trait `framework-compiler-driver` uses (`trait Compiler { fn compile(&self, req: RequestDocument) -> Result<CompileArtifact, Diagnostics>; }`). The fake is used two ways: (a) as a direct in-process trait impl for orchestration unit tests, and (b) as a small standalone binary that speaks the subprocess protocol, so we can also test the subprocess seam itself. It accepts a `HashMap<request_sha256, canned_response>` and returns whatever's pre-registered, or a synthetic "unknown request" diagnostic. This lets us test the whole `framework-core::build` orchestration — discovery + lower + call + write dist + write manifest + FRM-BO-10 failure behavior — deterministically, without the real compiler binary. Every FRM-BO rule gets a test at this layer.

**Layer C — integration tests with the real compiler.** A small suite (5–10 tests, run in CI, not per-commit) that requires a specific pinned compiler version installed in `~/.cln/versions/compiler/<version>/` and runs true end-to-end: hello-world, one-plugin, `.clapp` packaging round-trip, `cln dev` change-detect. CI provisions this by running `cln install compiler <pinned>` before the suite. These are slow (real compiler runs, no canned responses) and gate releases, not day-to-day.

**Why the fake-compiler approach over "just run the compiler":** the compiler is under active development. Its output for a given input will change. Making every framework test depend on compiler stability drags framework CI on every compiler codegen tweak. The `Compiler` trait boundary buys us the freedom to test framework logic against canned artifacts and to test the seam separately.

**Determinism tests.** The framework has its own determinism invariant to uphold: given the same project state, the request document it produces MUST be byte-identical (this is what makes the compiler's CMP-02 externally provable). A determinism suite hashes the request document across two builds of the same fixture on two OSes.

---

## 8. Milestones

**M0 — Hello-world compiles.** *Two-week target once we start writing code.*

Deliverables:
- Cargo workspace set up per §2. `framework-core`, `framework-compiler-driver`, `framework-cli` crates only.
- `clean.toml` parser + minimal `RequestDocument` type + lowering.
- File discovery for `.cln` files only (no plugins).
- In-process compiler seam.
- `clean-framework build <path>` → `dist/app.wasm`.
- One integration test (Layer C) proving `cln build hello-world && cln run dist/app.wasm` prints "hello".

Explicit non-goals: dependencies, plugins, caching, packaging, dev mode, MCP.

**M1 — Real projects build.** *~4 weeks after M0.*

Deliverables:
- `framework-libraries` crate: library.toml + plugin.toml load, FRM-PM-01..03 validation, lockfile read (Manager writes it).
- Path + git dependency resolution (via the Manager callback). No registry.
- Step 5 block-handler compilation + `~/.cln/wit-cache/`.
- Plugin `.wasm` loading + `[paths].owns` extending discovery roots.
- Build cache (`~/.cln/build-cache/`).
- `cln check` (diagnostics-only build).
- Layer A + Layer B test suites; the `fake-compiler` in-tree.

Explicit non-goals: `.clapp` packaging, dev mode, MCP.

**M2 — Ship-ready.** *~6 weeks after M1.*

Deliverables:
- `framework-package`: `.clapp`, `.serve`, `--raw`, `--standalone`.
- `framework-dev`: full watch + hot-reload loop.
- `framework-scaffold`: `cln new app`, `cln new library`, `cln new minimal`.
- `framework-mcp`: the MCP server (§10) with the tool surface listed in §9.1 of the libraries spec.
- `framework-migrate`: `cln db migrate <verb>` dispatching to `~/.cln/db-drivers/`.
- Determinism test suite for the framework's request-document output.
- ADR-0006-equivalent for the framework: reference-stack ADR pinning `toml_edit`, `notify`, `zip`, `wasmtime`, `sha2`, etc.

**M3 — Ecosystem readiness (post-v1).** Registry client (OCI per Manager §00.11.1), library signing verification, workspaces (Manager §00.11.3), a proper `cln doctor` integration hook.

---

## 9. Open questions

Explicit list of things the spec is silent on that we'll hit and need answered before or during implementation.

1. **How does `.cln/lock.toml` handle plugins vs. libraries?** Manager §00.3.2 says `cln add` is overloaded for both. The lockfile schema isn't in `foundation/schema/` yet (I checked). Framework needs to know whether a lock entry is a library (Clean source, needs compile) or a plugin (pre-built WASM, gets loaded as-is). Proposal: lockfile entries carry an explicit `kind = "library" | "plugin"` field. Confirm before Phase 2.

2. **Framework version discovery for `manifest.toml`.** **Resolved:** framework knows its own version via `env!("CARGO_PKG_VERSION")` at build time. Reading from `.cln/frame-version` would be circular — Manager already used that file to pick which framework binary to launch, so the running binary IS the pinned version. Same rule for the compiler: framework parses the compiler's `--version` output (one subprocess call at startup) and records that in the build manifest, rather than trusting the version string in the folder name.

3. **`cln fetch --internal` callback shape.** **Resolved:** subprocess. Framework spawns `cln fetch --internal --project=<path>`, matching the Manager→Framework dispatch direction (which is also subprocess). Consistent with the compiler seam. In-process sharing is a later optimization if we ever measure it as a real cost — currently `cln fetch` runs at most once per build, not per keystroke.

4. **How does the framework find the runtime binary for `--standalone`?** Package step 4 needs `~/.cln/active/runtime` bytes. Does the framework read that symlink directly, or ask Manager for the resolved path? Proposal: framework reads the symlink; it's a fixed on-disk location per Manager §00.2 that we can rely on.

5. **What triggers `.serve` vs `.clapp` when the project declares multiple worlds?** Spec (Manager §00.14) says `.serve` may contain multiple wasms, but our compiler request document currently declares one target. Do we run the compiler N times, once per world, and bundle the results? Or does the compiler grow multi-world emission? Proposal: N compiler invocations, one per world, framework aggregates. Confirm with compiler owner before Phase 6.

6. **MCP transport.** Spec §10 doesn't nail down whether the framework MCP runs over stdio, TCP, or both. `cln mcp` in Manager §00.3.6 doesn't say either. Proposal: stdio for `cln mcp` (matches how Claude/Cursor spawn MCP servers), TCP mode gated behind `--tcp=<port>` for local debugging.

7. **What happens on partial plugin failure during handler compilation (Phase 3)?** If one of ten plugins fails to compile its handler, do we fail the whole build (FRM-BO-10 all-or-nothing) or skip that plugin's blocks and let the compiler report `handles block` misses? Proposal: fail the whole build. Consistent with FRM-BO-10; partial-build states are exactly the debt this spec is trying to avoid.

8. **How do overrides for plugin-declared config work?** `[compile.env]` is a `clean.toml` section, but plugins may want their own config surface (e.g., `frame.data` wanting a DB URL at compile time). Is that a plugin-declared section in `clean.toml`, or a plugin-declared env-var namespace? Not covered in §12. Defer to a later spec pass; parking-lot.

9. **Warm-process protocol for `cln dev`.** Platform 14 §14.2.2 defines the one-shot compiler subprocess: request JSON on stdin, artifact tarball on stdout, then exit. For `cln dev` we need multi-request framing over the same stdin/stdout pipes so we don't pay process-spawn cost per keystroke. Proposal: length-prefixed frames (4-byte big-endian length + payload) in both directions, first frame is a handshake declaring protocol version. Needs a small compiler-side ADR before Phase 7 lands — this is a compiler API surface addition, not a framework-internal choice.

---

## Metadata

- **Author:** framework session (Ivo Pasco, 2026-08-09)
- **Status:** Draft for review
- **Owned decisions locked before writing:** compiler seam = subprocess-only against `~/.cln/versions/compiler/<version>/clean-compiler` installed by Manager (both framework and compiler are Manager-managed binaries, they never link against each other's source); dependency resolution = path+git only in M0/M1 (registry deferred to M3); resolver home = Manager, framework calls back via `cln fetch --internal`; framework version = `env!("CARGO_PKG_VERSION")`, compiler version = read from `clean-compiler --version`.
- **Next step after review:** convert accepted plan into an ADR-0001 for the framework, scaffold the Cargo workspace, land Phase 0.

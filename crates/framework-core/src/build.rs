//! The build orchestrator — §11.2 steps 1, 2, 3, 4, 6, 7, 8, 9, 10.
//!
//! Step 5 (compile block handlers) is Phase 3 work and is absent, not stubbed:
//! a library declaring `handles block` reaches the compiler with its
//! `compiletime_wasm_sha256` unset, which is honest — the framework has not
//! compiled that handler. Filling it with anything would be a claim we cannot
//! back.
//!
//! The one subtle rule here is **FRM-BO-10 — failure is total**. `dist/` is
//! either fully replaced by a successful build or left exactly as the previous
//! successful build left it. There is no half-updated state. This is enforced
//! by writing every output to a sibling temp directory and renaming into place
//! only after the compiler has returned successfully.

use std::path::{Path, PathBuf};

use cln_layout::Layout;
use framework_compiler_driver::artifact::CompileArtifact;
use framework_compiler_driver::{Compiler, Diagnostic, RequestDocument};

use crate::cache::{BuildCache, CachedCompiler};
use crate::closure::{self, ResolvedClosure};
use crate::discover::{discover, DiscoveryPlan};
use crate::errors::{FrameworkError, HostWitError};
use crate::hostwit::{
    self, HostContract, HostWitCache, Network, WitFetcher,
};
use crate::lower::{lower, ConfigOverride};
use crate::manifest::Manifest;
use crate::resolver::{Resolver, SubprocessResolver};

/// Output directory name, project-relative (FRM-BO-09).
pub const DIST_DIR: &str = "dist";
/// The shipped name for an application's component (FRM-BO-09).
pub const APP_WASM: &str = "app.wasm";
/// The build manifest, copied out of the compiler's artifact verbatim.
pub const BUILD_MANIFEST: &str = "build-manifest.json";
/// The generated host configuration (FRM-BO-11). Written beside `app.wasm`,
/// which is what lets its `[guest] wasm` be a bare `app.wasm`.
pub const HOST_TOML: &str = "host.toml";

/// Where a build reads and writes cached artifacts (§11.7).
#[derive(Clone, Debug, Default)]
pub enum Caching {
    /// `~/.cln/build-cache/`, resolved at build time.
    #[default]
    Default,
    /// A specific cache — used by tests, and by any caller that keeps its own.
    At(BuildCache),
    /// `--no-cache`: always compile, store nothing.
    Disabled,
}

#[derive(Clone, Debug)]
pub struct BuildInputs {
    pub project_root: PathBuf,
    /// From `--override` flags and `CLN_*` env vars (FRM-BO-08).
    pub overrides: Vec<ConfigOverride>,
    /// `--offline`: satisfy the host contract from cache or fail (C-18).
    pub network: Network,
    /// Where fetched host contracts live. Defaults to `~/.cln/host-wit/`;
    /// overridden in tests so they never touch the real cache.
    pub host_wit_cache: Option<HostWitCache>,
    /// The toolchain root used to resolve the active runtime when packaging
    /// (FRM-BO-09a). Defaults to `~/.cln/`, or `$CLN_HOME` when set;
    /// overridden in tests so they never read the developer's real install.
    pub toolchain_layout: Option<Layout>,

    /// Who to ask when `[dependencies]` are declared but `.cln/lock.toml` is
    /// absent (§00.8). Defaults to spawning `cln fetch --internal`; swap in
    /// [`crate::resolver::NoResolver`] for "build what is locked or fail".
    ///
    /// `Arc` rather than `Box` so `BuildInputs` stays `Clone` — `cln dev` will
    /// clone it per rebuild.
    pub resolver: std::sync::Arc<dyn Resolver + Send + Sync>,

    /// Where compiled artifacts are cached (§11.7).
    ///
    /// [`Caching::Default`] resolves to `~/.cln/build-cache/` at build time.
    /// Tests pass [`Caching::At`] so they never touch the developer's real
    /// cache — and so none can pass by accident because an earlier run left
    /// the right entry behind.
    pub build_cache: Caching,
}

impl BuildInputs {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        BuildInputs {
            project_root: project_root.into(),
            overrides: Vec::new(),
            network: Network::Allowed,
            host_wit_cache: None,
            toolchain_layout: None,
            resolver: std::sync::Arc::new(SubprocessResolver::default()),
            build_cache: Caching::Default,
        }
    }

    /// Cache compiled artifacts in `cache` instead of `~/.cln/build-cache/`.
    pub fn with_build_cache(mut self, cache: BuildCache) -> Self {
        self.build_cache = Caching::At(cache);
        self
    }

    /// `--no-cache`: compile every time, and store nothing.
    pub fn without_cache(mut self) -> Self {
        self.build_cache = Caching::Disabled;
        self
    }

    /// The build cache to use, or `None` when caching is off.
    ///
    /// A cache that cannot be located is not a build failure — the point of a
    /// cache is to make a build faster, and a developer with no home directory
    /// should still get a component.
    fn resolved_build_cache(&self) -> Option<BuildCache> {
        match &self.build_cache {
            Caching::At(cache) => Some(cache.clone()),
            Caching::Disabled => None,
            Caching::Default => BuildCache::user().ok(),
        }
    }

    /// Use `resolver` instead of spawning `cln fetch --internal`.
    pub fn with_resolver(
        mut self,
        resolver: std::sync::Arc<dyn Resolver + Send + Sync>,
    ) -> Self {
        self.resolver = resolver;
        self
    }

    /// Resolve the active runtime from `root` instead of `~/.cln/`.
    pub fn with_toolchain_layout(mut self, root: impl AsRef<Path>) -> Self {
        self.toolchain_layout = Some(Layout::new(root.as_ref()));
        self
    }

    pub fn with_overrides(mut self, overrides: Vec<ConfigOverride>) -> Self {
        self.overrides = overrides;
        self
    }

    pub fn offline(mut self, offline: bool) -> Self {
        self.network = if offline { Network::Offline } else { Network::Allowed };
        self
    }

    pub fn with_host_wit_cache(mut self, cache: HostWitCache) -> Self {
        self.host_wit_cache = Some(cache);
        self
    }

    fn cache(&self) -> Result<HostWitCache, FrameworkError> {
        match &self.host_wit_cache {
            Some(cache) => Ok(cache.clone()),
            None => HostWitCache::user(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BuildOutcome {
    /// Where the framework wrote the component (FRM-BO-09).
    pub dist_wasm: PathBuf,
    pub build_manifest_path: PathBuf,
    /// The generated `dist/host.toml` (FRM-BO-11) — what `cln run <project>`
    /// passes to the host as `--config`.
    pub dist_host_toml: PathBuf,
    /// Warnings and infos from a successful compile. Errors never reach here —
    /// they arrive as `Err(FrameworkError::Compiler)`.
    pub diagnostics: Vec<Diagnostic>,
    /// Hex-lowercase SHA-256 of the request document. The build-cache key in
    /// Phase 5 and the change-detection key for `cln dev` in Phase 7.
    pub request_sha256: String,
    pub wasm_sha256: String,
}

/// Assemble the request document for a project without invoking the compiler.
///
/// Split out because three callers need exactly this and nothing more: `cln
/// build`, the determinism test suite, and (in Phase 7) the `cln dev` loop
/// deciding whether anything actually changed.
pub fn assemble_request(inputs: &BuildInputs) -> Result<RequestDocument, FrameworkError> {
    assemble_request_with(inputs, default_fetcher().as_deref())
}

/// [`assemble_request`] with an injectable fetcher, for tests and for callers
/// that supply their own transport.
pub fn assemble_request_with(
    inputs: &BuildInputs,
    fetcher: Option<&dyn WitFetcher>,
) -> Result<RequestDocument, FrameworkError> {
    // Step 2 — read the manifest first: it declares the excludes that step 1
    // needs.
    let manifest = Manifest::load(&inputs.project_root)?;

    // Moment 1 — obtain the host contract before anything else that can fail
    // slowly. A project targeting a host it cannot reach should learn that
    // before walking the source tree.
    let contract = resolve_target_world(inputs, &manifest, fetcher)?;

    // Steps 3 and 4 — resolve the closure and read each package's manifest.
    let closure = resolve_closure(inputs, &manifest)?;

    // Step 1 — discover, with the roots and extensions the closure's plugins
    // declare (FRM-BO-03 item 3, §11.4). This runs after step 3 because a
    // plugin cannot extend discovery until it has been read and validated.
    let plan = DiscoveryPlan::m0(&inputs.project_root, manifest.excludes().to_vec())
        .with_plugins(&closure.plugins);
    let sources = discover(&plan)?;

    // Steps 6 and 7 — lower and assemble.
    Ok(lower(&manifest, &contract, &closure, sources, &inputs.overrides))
}

/// Steps 3 and 4: obtain the resolved closure.
///
/// **The framework does not resolve.** Manager owns resolution and writes
/// `.cln/lock.toml`; when that file is absent, the framework asks Manager to
/// produce it (`cln fetch --internal`, §00.8) rather than resolving itself.
/// Two resolvers would eventually disagree, and the one that ran last would
/// win silently.
///
/// A project with no `[dependencies]` is the common case and must not pay for
/// a subprocess: there is nothing to resolve, so the callback is skipped.
///
/// **"Needs resolving" is decided by the closure, not by the file's
/// existence.** Moment 1 has already run by the time this is called, and
/// pinning the host contract (BVER-03) *creates* `.cln/lock.toml` to hold
/// `[host.<name>]`. So on a first build the file always exists and contains no
/// packages. Treating that as "already resolved" would skip the resolver on
/// exactly the builds that need it, and the project would compile without its
/// dependencies.
fn resolve_closure(
    inputs: &BuildInputs,
    manifest: &Manifest,
) -> Result<ResolvedClosure, FrameworkError> {
    let existing = closure::resolve(&inputs.project_root)?;

    // Declares nothing: whatever the lockfile holds, there is nothing to ask
    // Manager for.
    if manifest.dependencies.is_empty() {
        return Ok(existing.unwrap_or_default());
    }

    // Declares dependencies and the lockfile accounts for them.
    if let Some(closure) = existing {
        if !closure.dependencies.is_empty() {
            return Ok(closure);
        }
    }

    // Declared dependencies with no lockfile: ask Manager to resolve, then
    // read what it wrote. Building without them would silently compile a
    // program whose imports cannot resolve.
    inputs
        .resolver
        .resolve(&inputs.project_root)
        .map_err(closure::ClosureError::from)?;

    // The resolver reported success, so the lockfile must now account for the
    // declared dependencies. An empty closure here means it did not — refusing
    // names the real problem, where carrying on would surface it as a pile of
    // unresolved imports with no mention of resolution at all.
    match closure::resolve(&inputs.project_root)? {
        Some(closure) if !closure.dependencies.is_empty() => Ok(closure),
        _ => Err(closure::ClosureError::NotResolved {
            project_root: inputs.project_root.clone(),
        }
        .into()),
    }
}

/// Moment 1 (HCV-01): fetch or read the target's `host.wit`, verify it against
/// the lockfile pin, check it declares the world this target needs, and pin it
/// if it was not pinned before.
///
/// The world check is here rather than left to the compiler because the
/// failure is far more legible at this level: one message naming the host and
/// the world, instead of `COM012` on every single host-function call site.
fn resolve_target_world(
    inputs: &BuildInputs,
    manifest: &Manifest,
    fetcher: Option<&dyn WitFetcher>,
) -> Result<HostContract, FrameworkError> {
    let manifest_path = inputs.project_root.join(crate::manifest::MANIFEST_FILE);

    // Both validated by `Manifest::validate`, so these are unreachable in
    // practice — but they are cheap, and the alternative to an explicit error
    // is an `unwrap` that becomes a panic the day validation changes.
    let target = manifest.target_section().ok_or_else(|| HostWitError::MissingSection {
        path: manifest_path.clone(),
    })?;
    let world = manifest.world().ok_or_else(|| HostWitError::NoWorldForTarget {
        path: manifest_path,
        target: manifest.target().unwrap_or_default().to_string(),
    })?;

    let cache = inputs.cache()?;
    let pinned = hostwit::pinned_hash(&inputs.project_root, &target.host)?;

    let contract = hostwit::resolve_contract(
        &target.host,
        &target.version,
        target.wit_source.as_deref(),
        &cache,
        inputs.network,
        fetcher,
        pinned.as_deref(),
    )?;

    if !hostwit::declares_world(&contract.wit, world) {
        return Err(HostWitError::WorldNotInContract {
            host: contract.host.clone(),
            version: contract.version.clone(),
            world: world.to_string(),
        }
        .into());
    }

    // BVER-03: record the hash so every later build is reproducible against a
    // pinned contract. Only when it was not already pinned — `resolve_contract`
    // has already refused a mismatch, so reaching here with a pin means it
    // matched and rewriting it would be a no-op write on every build.
    if pinned.is_none() {
        hostwit::pin_hash(&inputs.project_root, &contract)?;
    }

    Ok(contract)
}

/// The default transport. `None` until an HTTP client is wired in — a project
/// whose contract is already cached still builds, and one that needs a fetch
/// fails with `FRM004` naming what is missing rather than panicking.
fn default_fetcher() -> Option<Box<dyn WitFetcher>> {
    None
}

/// Run a full build. Steps 1–10, minus block-handler compilation (Phase 3).
pub fn build(inputs: &BuildInputs, compiler: &dyn Compiler) -> Result<BuildOutcome, FrameworkError> {
    let request = assemble_request(inputs)?;
    let request_sha256 = request
        .sha256()
        .map_err(|e| FrameworkError::Compiler(e.into()))?;

    // Step 8 — the single call across the seam, through the build cache
    // (§11.7). A hit skips the compiler entirely; everything downstream is
    // identical either way, because the cache replays the compiler's own
    // response bytes.
    let cached;
    let compiler: &dyn Compiler = match inputs.resolved_build_cache() {
        Some(cache) => {
            cached = CachedCompiler::new(compiler, cache);
            &cached
        }
        None => compiler,
    };

    let artifact = compiler.compile(&request)?;

    // Steps 9 and 10 — receive and write. The manifest is re-read rather than
    // threaded out of `assemble_request`, which returns the lowered request
    // document and not the manifest it was lowered from; a second parse of a
    // small TOML file is cheaper than widening that return type for one caller.
    let manifest = Manifest::load(&inputs.project_root)?;
    let paths = write_dist(&inputs.project_root, &artifact, &manifest)?;

    Ok(BuildOutcome {
        dist_wasm: paths.wasm,
        build_manifest_path: paths.manifest,
        dist_host_toml: paths.host_toml,
        diagnostics: artifact.diagnostics.clone(),
        request_sha256,
        wasm_sha256: artifact.wasm_sha256(),
    })
}

struct DistPaths {
    wasm: PathBuf,
    manifest: PathBuf,
    /// The generated `dist/host.toml` (FRM-BO-11).
    host_toml: PathBuf,
}

/// Step 10, honouring FRM-BO-10.
///
/// Everything is staged in `dist.tmp-<pid>/` and swapped in at the end. A crash
/// or a full disk mid-write leaves the previous `dist/` untouched rather than
/// producing a `dist/` whose `app.wasm` and `build-manifest.json` disagree.
fn write_dist(
    project_root: &Path,
    artifact: &CompileArtifact,
    manifest: &Manifest,
) -> Result<DistPaths, FrameworkError> {
    let dist = project_root.join(DIST_DIR);
    let staging = project_root.join(format!("{DIST_DIR}.tmp-{}", std::process::id()));

    // A staging directory left behind by a killed process would otherwise make
    // every later build fail.
    if staging.exists() {
        remove_dir(&staging)?;
    }
    create_dir(&staging)?;

    let staged_wasm = staging.join(APP_WASM);
    write_file(&staged_wasm, &artifact.wasm)?;

    let staged_manifest = staging.join(BUILD_MANIFEST);
    let manifest_json = artifact
        .manifest
        .to_pretty_json()
        .map_err(|e| FrameworkError::Compiler(e.into()))?;
    write_file(&staged_manifest, manifest_json.as_bytes())?;

    // FRM-BO-11 — the host configuration, generated from project state alone
    // and staged with everything else so a failed build leaves no partial
    // config behind. `Placement::Dist` is what makes `[guest] wasm` resolve:
    // this file lands beside the `app.wasm` written just above.
    let staged_host_toml = staging.join(HOST_TOML);
    let host_toml = crate::hosttoml::generate(manifest, crate::hosttoml::Placement::Dist);
    write_file(&staged_host_toml, host_toml.as_bytes())?;

    // Swap. `rename` over an existing directory fails on every platform we
    // ship, so the old dist is moved aside first and only deleted once the new
    // one is in place — the window where neither exists is a single rename.
    let previous = project_root.join(format!("{DIST_DIR}.old-{}", std::process::id()));
    if previous.exists() {
        remove_dir(&previous)?;
    }

    let had_previous = dist.exists();
    if had_previous {
        rename(&dist, &previous)?;
    }

    if let Err(err) = rename(&staging, &dist) {
        // Put the previous build back rather than leaving the project with no
        // dist at all.
        if had_previous {
            let _ = rename(&previous, &dist);
        }
        let _ = remove_dir(&staging);
        return Err(err);
    }

    if had_previous {
        // Best-effort: the new dist is already in place, so a failure to clean
        // up the old copy must not fail the build.
        let _ = remove_dir(&previous);
    }

    Ok(DistPaths {
        wasm: dist.join(APP_WASM),
        manifest: dist.join(BUILD_MANIFEST),
        host_toml: dist.join(HOST_TOML),
    })
}

fn create_dir(path: &Path) -> Result<(), FrameworkError> {
    std::fs::create_dir_all(path)
        .map_err(|source| FrameworkError::Output { path: path.to_path_buf(), source })
}

fn remove_dir(path: &Path) -> Result<(), FrameworkError> {
    std::fs::remove_dir_all(path)
        .map_err(|source| FrameworkError::Output { path: path.to_path_buf(), source })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), FrameworkError> {
    std::fs::write(path, bytes)
        .map_err(|source| FrameworkError::Output { path: path.to_path_buf(), source })
}

fn rename(from: &Path, to: &Path) -> Result<(), FrameworkError> {
    std::fs::rename(from, to)
        .map_err(|source| FrameworkError::Output { path: to.to_path_buf(), source })
}

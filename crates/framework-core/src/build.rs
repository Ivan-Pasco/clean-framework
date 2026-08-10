//! The build orchestrator — §11.2 steps 1, 2, 6, 7, 8, 9, 10.
//!
//! Steps 3 (resolve dependencies), 4 (read library manifests) and 5 (compile
//! block handlers) are Phase 2/3 work and are absent, not stubbed: a project
//! with `[dependencies]` builds today as if it had none, which is correct for
//! M0's no-dependency scope and will become a hard error the moment the
//! lockfile reader lands.
//!
//! The one subtle rule here is **FRM-BO-10 — failure is total**. `dist/` is
//! either fully replaced by a successful build or left exactly as the previous
//! successful build left it. There is no half-updated state. This is enforced
//! by writing every output to a sibling temp directory and renaming into place
//! only after the compiler has returned successfully.

use std::path::{Path, PathBuf};

use framework_compiler_driver::artifact::CompileArtifact;
use framework_compiler_driver::{Compiler, Diagnostic, RequestDocument};

use crate::discover::{discover, DiscoveryPlan};
use crate::errors::FrameworkError;
use crate::lower::{lower, ConfigOverride};
use crate::manifest::Manifest;

/// Output directory name, project-relative (FRM-BO-09).
pub const DIST_DIR: &str = "dist";
/// The shipped name for an application's component (FRM-BO-09).
pub const APP_WASM: &str = "app.wasm";
/// The build manifest, copied out of the compiler's artifact verbatim.
pub const BUILD_MANIFEST: &str = "build-manifest.json";

#[derive(Clone, Debug)]
pub struct BuildInputs {
    pub project_root: PathBuf,
    /// From `--override` flags and `CLN_*` env vars (FRM-BO-08).
    pub overrides: Vec<ConfigOverride>,
}

impl BuildInputs {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        BuildInputs { project_root: project_root.into(), overrides: Vec::new() }
    }

    pub fn with_overrides(mut self, overrides: Vec<ConfigOverride>) -> Self {
        self.overrides = overrides;
        self
    }
}

#[derive(Clone, Debug)]
pub struct BuildOutcome {
    /// Where the framework wrote the component (FRM-BO-09).
    pub dist_wasm: PathBuf,
    pub build_manifest_path: PathBuf,
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
    // Step 2 — read the manifest first: it declares the excludes that step 1
    // needs.
    let manifest = Manifest::load(&inputs.project_root)?;

    // Step 1 — discover.
    let plan = DiscoveryPlan::m0(&inputs.project_root, manifest.excludes().to_vec());
    let sources = discover(&plan)?;

    // Steps 6 and 7 — lower and assemble.
    Ok(lower(&manifest, sources, &inputs.overrides))
}

/// Run a full build. Steps 1–10 minus the dependency steps.
pub fn build(inputs: &BuildInputs, compiler: &dyn Compiler) -> Result<BuildOutcome, FrameworkError> {
    let request = assemble_request(inputs)?;
    let request_sha256 = request
        .sha256()
        .map_err(|e| FrameworkError::Compiler(e.into()))?;

    // Step 8 — the single call across the seam.
    let artifact = compiler.compile(&request)?;

    // Steps 9 and 10 — receive and write.
    let paths = write_dist(&inputs.project_root, &artifact)?;

    Ok(BuildOutcome {
        dist_wasm: paths.wasm,
        build_manifest_path: paths.manifest,
        diagnostics: artifact.diagnostics.clone(),
        request_sha256,
        wasm_sha256: artifact.wasm_sha256(),
    })
}

struct DistPaths {
    wasm: PathBuf,
    manifest: PathBuf,
}

/// Step 10, honouring FRM-BO-10.
///
/// Everything is staged in `dist.tmp-<pid>/` and swapped in at the end. A crash
/// or a full disk mid-write leaves the previous `dist/` untouched rather than
/// producing a `dist/` whose `app.wasm` and `build-manifest.json` disagree.
fn write_dist(project_root: &Path, artifact: &CompileArtifact) -> Result<DistPaths, FrameworkError> {
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

    Ok(DistPaths { wasm: dist.join(APP_WASM), manifest: dist.join(BUILD_MANIFEST) })
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

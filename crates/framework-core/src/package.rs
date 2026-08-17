//! `cln package` — wrap a built component into a distributable archive
//! (FRM-BO-09a, Manager §00.14).
//!
//! Packaging is a separate step from building, so a build stays fast,
//! deterministic, and cacheable regardless of what shipping shape the
//! developer eventually wants. This module owns one question the packaging
//! crate deliberately does not: **is `dist/` current?**
//!
//! It answers it by comparing the request document a build would produce now
//! against the one recorded in `dist/build-manifest.json`. That is the same
//! hash the build cache and `cln dev` use to decide whether anything changed,
//! so "stale" means exactly one thing across the toolchain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cln_layout::Layout;
use cln_shared::ToolchainKind;
use framework_compiler_driver::Compiler;
use framework_package::{self as pkg, Kind, PackageInputs};

use crate::build::{self, BuildInputs, APP_WASM, BUILD_MANIFEST, DIST_DIR};
use crate::errors::FrameworkError;
use crate::manifest::Manifest;

/// Where packages are written, project-relative.
pub const PACKAGE_DIR: &str = "dist";

pub struct PackageOutcome {
    pub path: PathBuf,
    /// Hex-lowercase SHA-256 over the archive. What a content-addressed store
    /// keys on, and what an upload check compares against.
    pub sha256: String,
    pub kind: Kind,
    /// True when this call had to build first.
    pub rebuilt: bool,
}

/// Package a project, building first if `dist/` is missing or stale.
///
/// The build is triggered here rather than demanded of the caller because the
/// staleness question needs the request document, which only this crate
/// assembles. A caller that wants to avoid the check has already built.
pub fn package(
    inputs: &BuildInputs,
    compiler: &dyn Compiler,
) -> Result<PackageOutcome, FrameworkError> {
    let manifest = Manifest::load(&inputs.project_root)?;
    let rebuilt = ensure_built(inputs, compiler)?;

    // Resolved through `cln-layout` for the same reason the host-wit cache and
    // the compiler resolver are: every process that asks where the toolchain
    // is has to get one answer. Tests inject their own root so they never read
    // the developer's real `~/.cln` — and so the resolution tiers are testable
    // at all, which reading `$HOME` directly would prevent.
    let layout = inputs.toolchain_layout.clone().or_else(Layout::from_home);

    let wasm = read(&inputs.project_root.join(DIST_DIR).join(APP_WASM))?;
    let build_manifest = read_build_manifest(&inputs.project_root)?;

    // The world is what decides the kind: a project targeting `server` ships a
    // server bundle, anything else ships an application. Manager §00.14 leaves
    // the multi-world case to a future `--kind` override; a single-target
    // project has exactly one answer.
    let world = manifest.world().unwrap_or("cli");
    let kind = if world == "server" { Kind::Serve } else { Kind::Clapp };

    let mut components = BTreeMap::new();
    components.insert(world.to_string(), wasm);

    let name = manifest.project.name.clone();
    let version = manifest.project.version.clone();

    let packaged = pkg::package(PackageInputs {
        kind,
        name: name.clone(),
        version,
        description: None,
        components,
        // Bridges and migrations arrive with dependency resolution; a project
        // with no dependencies has neither, which is the whole of M0's scope.
        bridges: BTreeMap::new(),
        // Regenerated for the archive rather than copied from `dist/`.
        // `dist/host.toml` says `wasm = "app.wasm"` because it sits beside the
        // component; inside the archive the config lives one level down in
        // `config/`, and host-core resolves relative paths against the config
        // file's own directory. Copying the dist copy would produce a bundle
        // that parses cleanly and then fails at startup looking for
        // `config/app.wasm`. Generation is total (FRM-BO-11), so regenerating
        // is also what keeps a hand-edited `dist/host.toml` out of a shipped
        // artifact.
        host_toml: Some(
            crate::hosttoml::generate(&manifest, crate::hosttoml::Placement::Archive).into_bytes(),
        ),
        files: collect_assets(&inputs.project_root)?,
        build: pkg::Build {
            compiler_version: build_manifest
                .compiler_version()
                .unwrap_or("unknown")
                .to_string(),
            framework_version: crate::FRAMEWORK_VERSION.to_string(),
            // The runtime the artifact must run against. Resolved, not
            // invented: `cln run` refuses on mismatch and Cloud rejects a
            // bundle whose runtime it cannot provide, so a wrong value here
            // fails loudly at the far end.
            runtime_version: runtime_pin(&inputs.project_root, layout.as_ref()),
            built_at: now_rfc3339(),
            built_by: format!("clean-framework {}", crate::FRAMEWORK_VERSION),
        },
    })
    .map_err(package_error)?;

    let path = inputs
        .project_root
        .join(PACKAGE_DIR)
        .join(pkg::file_name(&name));
    packaged.write_to(&path).map_err(package_error)?;

    Ok(PackageOutcome { path, sha256: packaged.sha256(), kind, rebuilt })
}

/// Build when `dist/` is absent or does not match what the sources would
/// produce now. Returns whether a build ran.
fn ensure_built(inputs: &BuildInputs, compiler: &dyn Compiler) -> Result<bool, FrameworkError> {
    let dist_wasm = inputs.project_root.join(DIST_DIR).join(APP_WASM);
    if !dist_wasm.exists() {
        build::build(inputs, compiler)?;
        return Ok(true);
    }

    let current = build::assemble_request(inputs)?
        .sha256()
        .map_err(|e| FrameworkError::Compiler(e.into()))?;

    let recorded = read_build_manifest(&inputs.project_root)
        .ok()
        .and_then(|m| m.request_sha256().map(str::to_string));

    // An unreadable or hash-less build manifest rebuilds rather than trusting
    // the wasm beside it — the pair must agree, and only a build makes them.
    if recorded.as_deref() == Some(current.as_str()) {
        return Ok(false);
    }

    build::build(inputs, compiler)?;
    Ok(true)
}

fn read_build_manifest(
    project_root: &Path,
) -> Result<framework_compiler_driver::artifact::BuildManifest, FrameworkError> {
    let path = project_root.join(DIST_DIR).join(BUILD_MANIFEST);
    let bytes = read(&path)?;
    serde_json::from_slice(&bytes)
        .map_err(|source| FrameworkError::Output { path, source: source.into() })
}

/// Build time as RFC 3339, or `SOURCE_DATE_EPOCH` when set.
///
/// A wall-clock timestamp makes two packages of identical inputs differ, which
/// costs the byte-reproducibility that lets a content-addressed store
/// deduplicate and lets a rebuild be checked against a published artifact.
/// `SOURCE_DATE_EPOCH` is the cross-ecosystem convention for exactly this
/// tension, so a reproducible build sets it and gets a stable manifest, while
/// an ordinary build still records when it ran.
fn now_rfc3339() -> String {
    let secs = match std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.trim().parse().ok()) {
        Some(epoch) => epoch,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    format_rfc3339(secs)
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Hand-rolled rather than pulling in a date crate: this is the only date the
/// framework formats, and the civil-from-days algorithm is small and exact.
fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let (h, m, s) = (time / 3600, (time % 3600) / 60, time % 60);

    // Howard Hinnant's civil_from_days, shifted to a March-based year so the
    // leap day lands at the end and needs no special case.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// The runtime version this artifact is stamped with, resolved the way
/// FRM-BO-09a specifies: the project pin if there is one, otherwise **the
/// currently-active runtime resolved by Clean Manager**.
///
/// The two tiers are the top two of the `cln run` resolution chain (Manager
/// §00.13), and they are here for the same reason they are there. A project
/// that has pinned a runtime is asking to be built against that one. A project
/// that has not is built against whatever `~/.cln/active/runtime` points at —
/// which is not a guess, it is the runtime the toolchain would actually have
/// used, and the one the developer's own `cln run` will select. Stamping it is
/// how the artifact records what it was built against rather than leaving a
/// consumer to assume.
///
/// Reading the pin file alone was wrong twice over: `cln pin` is not yet wired
/// (Manager PLAN), so the file never exists and every artifact stamped
/// `"unknown"` — and Cloud rejects `"unknown"` outright, because a runtime it
/// cannot name is a runtime it cannot schedule. FRM-BO-09a says these fields
/// take "no defaults"; `"unknown"` survives only for the case where there is
/// genuinely nothing to read, and it is honest there.
fn runtime_pin(project_root: &Path, layout: Option<&Layout>) -> String {
    if let Some(pinned) = project_pin(project_root) {
        return pinned;
    }
    layout
        .and_then(|l| l.active_version(ToolchainKind::Runtime))
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// `.cln/runtime-version`, the project's own pin (Manager §00.2, §00.13 tier 2).
fn project_pin(project_root: &Path) -> Option<String> {
    std::fs::read_to_string(project_root.join(".cln").join("runtime-version"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Static assets, read from `public/` into `assets/public/` in the archive.
fn collect_assets(project_root: &Path) -> Result<BTreeMap<String, Vec<u8>>, FrameworkError> {
    let mut files = BTreeMap::new();
    let root = project_root.join("public");
    if root.is_dir() {
        collect_into(&root, &root, "assets/public", &mut files)?;
    }
    Ok(files)
}

fn collect_into(
    root: &Path,
    dir: &Path,
    prefix: &str,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), FrameworkError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| FrameworkError::Output { path: dir.to_path_buf(), source })?;

    for entry in entries {
        let entry =
            entry.map_err(|source| FrameworkError::Output { path: dir.to_path_buf(), source })?;
        let path = entry.path();

        if path.is_dir() {
            collect_into(root, &path, prefix, out)?;
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path);
        // Archive paths are always `/`-separated, whatever the host OS uses,
        // so a package built on Windows unpacks correctly on Linux.
        let joined = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        out.insert(format!("{prefix}/{joined}"), read(&path)?);
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, FrameworkError> {
    std::fs::read(path).map_err(|source| FrameworkError::Output { path: path.to_path_buf(), source })
}

fn package_error(err: pkg::PackageError) -> FrameworkError {
    FrameworkError::Package(err)
}

#[cfg(test)]
mod runtime_resolution {
    use super::runtime_pin;
    use cln_layout::Layout;
    use cln_shared::ToolchainKind;
    use tempfile::TempDir;

    /// A toolchain root with `runtime` installed and made active, as
    /// `cln install runtime` + `cln use runtime` would leave it.
    fn toolchain_with_active_runtime(version: &str) -> (TempDir, Layout) {
        let home = TempDir::new().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();

        let v: semver::Version = version.parse().unwrap();
        std::fs::create_dir_all(layout.version_dir(ToolchainKind::Runtime, &v)).unwrap();
        std::fs::write(layout.version_binary(ToolchainKind::Runtime, &v), b"stub").unwrap();
        layout.set_active(ToolchainKind::Runtime, &v).unwrap();

        (home, layout)
    }

    fn project_pinned_to(version: &str) -> TempDir {
        let project = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join(".cln")).unwrap();
        std::fs::write(
            project.path().join(".cln").join("runtime-version"),
            format!("{version}\n"),
        )
        .unwrap();
        project
    }

    #[test]
    fn an_unpinned_project_is_stamped_with_the_active_runtime() {
        // The case that matters: no project pin, because `cln pin` is not yet
        // wired. The artifact must still name a real runtime — Cloud schedules
        // on this string, and cannot place a bundle that says "unknown".
        let project = TempDir::new().unwrap();
        let (_home, layout) = toolchain_with_active_runtime("0.7.0");

        assert_eq!(runtime_pin(project.path(), Some(&layout)), "0.7.0");
    }

    #[test]
    fn a_project_pin_outranks_the_active_runtime() {
        // §00.13 orders these deliberately. A project that pinned a runtime is
        // asking to be built against that one, even while a newer one is
        // active — otherwise switching the global toolchain would silently
        // restamp every artifact built from a pinned project.
        let project = project_pinned_to("0.6.1");
        let (_home, layout) = toolchain_with_active_runtime("0.7.0");

        assert_eq!(runtime_pin(project.path(), Some(&layout)), "0.6.1");
    }

    #[test]
    fn nothing_installed_and_nothing_pinned_stays_unknown() {
        // The honest floor. With no pin and no active runtime there is no
        // version to report, and inventing one would produce an artifact
        // claiming a compatibility it was never built against. "unknown" is
        // what makes Cloud's rejection correct rather than obstructive.
        let project = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let layout = Layout::new(home.path());
        layout.ensure_base().unwrap();

        assert_eq!(runtime_pin(project.path(), Some(&layout)), "unknown");
        assert_eq!(runtime_pin(project.path(), None), "unknown");
    }

    #[test]
    fn a_blank_pin_file_falls_through_rather_than_stamping_empty() {
        // A pin file truncated to nothing is absence, not a version. Treating
        // it as a value would put `runtime_version = ""` in the manifest,
        // which parses fine and then matches no node at all.
        let project = TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join(".cln")).unwrap();
        std::fs::write(project.path().join(".cln").join("runtime-version"), "  \n").unwrap();
        let (_home, layout) = toolchain_with_active_runtime("0.7.0");

        assert_eq!(runtime_pin(project.path(), Some(&layout)), "0.7.0");
    }

    #[test]
    fn a_dangling_active_link_does_not_become_a_version() {
        // Uninstalling the active runtime leaves the symlink pointing nowhere.
        // The stamp must not claim a runtime that is no longer installed.
        let (home, layout) = toolchain_with_active_runtime("0.7.0");
        let v: semver::Version = "0.7.0".parse().unwrap();
        std::fs::remove_dir_all(layout.version_dir(ToolchainKind::Runtime, &v)).unwrap();
        drop(home);

        let project = TempDir::new().unwrap();
        assert_eq!(runtime_pin(project.path(), Some(&layout)), "unknown");
    }
}

#[cfg(test)]
mod tests {
    use super::format_rfc3339;

    #[test]
    fn formats_the_epoch_itself() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn formats_a_known_instant() {
        // 2026-08-14T12:00:00Z — the value the packaging tests pin.
        assert_eq!(format_rfc3339(1_786_708_800), "2026-08-14T12:00:00Z");
    }

    #[test]
    fn handles_a_leap_day() {
        // 2024-02-29 is the case a naive day-count gets wrong.
        assert_eq!(format_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn handles_the_turn_of_a_century_that_is_not_a_leap_year() {
        // 1900 is divisible by 4 but not a leap year; 2000 is. Both fall
        // inside the era arithmetic, so a wrong branch shows up here.
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn carries_time_of_day_through() {
        assert_eq!(format_rfc3339(86_399), "1970-01-01T23:59:59Z");
    }
}

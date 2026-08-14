//! Wraps a built component into a `.clapp` distribution archive.
//!
//! This crate is the producer half of the deploy contract. Its output is what
//! Clean Cloud receives, what `cln run` executes, and what a double-click
//! inspects — one artifact, self-describing, verifiable without being run.
//!
//! **Why a package and not a bare `.wasm`.** A bare module cannot state which
//! runtime it needs, cannot carry integrity hashes, cannot carry migrations,
//! and cannot name the bridge components its capabilities require. Every one
//! of those facts would otherwise have to travel out of band, where it can
//! disagree with the artifact it describes. `cln package --raw` still emits a
//! bare component for generic wasm tooling; that is interop, not deployment.
//!
//! **One extension, two kinds.** Both an application and a server bundle ship
//! as `.clapp`. `manifest.toml`'s `kind` field is the discriminator — as it
//! always was, since `cln run` reads the manifest to pick the runtime and
//! world before it touches any wasm.
//!
//! This crate does not decide whether a build is needed. `framework-core` owns
//! that question so it has exactly one owner; here, an absent `dist/app.wasm`
//! is [`PackageError::NotBuilt`].

pub mod archive;
pub mod error;
pub mod manifest;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use archive::{sha256_hex, Entry, MANIFEST_NAME};
pub use error::PackageError;
pub use manifest::{Artifact, Bridge, Build, Integrity, Kind, Manifest, Package, SPEC_VERSION};

/// Archive-relative locations, fixed by Manager §00.14.
pub mod layout {
    /// `clapp` — the single component.
    pub const APP_WASM: &str = "app.wasm";
    /// `serve` — components live under here, one per world.
    pub const WASM_DIR: &str = "wasm";
    /// Bridge components, carried inside the archive.
    pub const BRIDGES_DIR: &str = "bridges";
    /// Static assets served over HTTP.
    pub const ASSETS_DIR: &str = "assets";
    /// Database migrations, run by whoever provisions the database.
    pub const MIGRATIONS_DIR: &str = "migrations";
    /// Generated host configuration. Carries `[bridges]`; the operator
    /// supplies the deployment blocks at deploy time.
    pub const HOST_TOML: &str = "config/host.toml";
}

/// What the caller supplies to produce a package.
pub struct PackageInputs {
    pub kind: Kind,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// World name → component bytes. A `clapp` has exactly one; a `serve`
    /// bundle may have several (`server`, `worker`, `cli`).
    pub components: BTreeMap<String, Vec<u8>>,
    /// Bridge components to carry, keyed by the WIT interface they satisfy.
    pub bridges: BTreeMap<String, BridgeInput>,
    /// Generated `dist/host.toml`, carrying the derived `[bridges]` block.
    pub host_toml: Option<Vec<u8>>,
    /// Archive-relative path → bytes, for assets and migrations.
    pub files: BTreeMap<String, Vec<u8>>,
    pub build: Build,
}

pub struct BridgeInput {
    pub name: String,
    pub version: String,
    pub wasm: Vec<u8>,
}

/// The finished archive, in memory.
pub struct Packaged {
    pub bytes: Vec<u8>,
    pub manifest: Manifest,
}

impl Packaged {
    /// Hex-lowercase SHA-256 over the archive itself.
    ///
    /// This is the identity a content-addressed store keys on, and it is
    /// stable across repeated packaging of identical inputs — see
    /// [`archive::write`] on why entry timestamps are fixed.
    pub fn sha256(&self) -> String {
        sha256_hex(&self.bytes)
    }

    /// Write to `path`, replacing whatever is there.
    ///
    /// Staged and renamed, so an interrupted write leaves the previous package
    /// intact rather than a truncated file that reads as a valid path
    /// (FRM-BO-10, applied to packaging).
    pub fn write_to(&self, path: &Path) -> Result<(), PackageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| PackageError::Output { path: parent.to_path_buf(), source })?;
        }

        let staging = staging_path(path);
        std::fs::write(&staging, &self.bytes)
            .map_err(|source| PackageError::Output { path: staging.clone(), source })?;

        std::fs::rename(&staging, path).map_err(|source| {
            let _ = std::fs::remove_file(&staging);
            PackageError::Output { path: path.to_path_buf(), source }
        })
    }
}

fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(name)
}

/// Build the archive.
///
/// Deterministic: identical inputs produce byte-identical output, which is
/// what lets the receiving side deduplicate by hash and lets a rebuild be
/// checked against a published artifact. The two things that would otherwise
/// vary — entry order and entry timestamps — are pinned here and in
/// [`archive::write`] respectively.
pub fn package(inputs: PackageInputs) -> Result<Packaged, PackageError> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut wasm_sha256: BTreeMap<String, String> = BTreeMap::new();
    let mut entry_paths: BTreeMap<String, String> = BTreeMap::new();
    let mut bridges: BTreeMap<String, Bridge> = BTreeMap::new();

    // Components. `BTreeMap` iteration is ordered, so the archive's layout
    // does not depend on the order the caller inserted worlds.
    for (world, bytes) in &inputs.components {
        let path = match inputs.kind {
            Kind::Clapp => layout::APP_WASM.to_string(),
            Kind::Serve => format!("{}/{world}.wasm", layout::WASM_DIR),
        };
        wasm_sha256.insert(path.clone(), sha256_hex(bytes));
        entry_paths.insert(world.clone(), path.clone());
        entries.push(Entry::new(path, bytes.clone()));
    }

    // Bridges travel with the app so the artifact stays self-describing, and
    // so an app cannot be tested against one bridge version and run against
    // another — a divergence that produces no error, only different
    // behaviour, with nothing in the artifact recording that it happened.
    for (interface, bridge) in &inputs.bridges {
        let path = format!("{}/{}.wasm", layout::BRIDGES_DIR, bridge.name);
        wasm_sha256.insert(path.clone(), sha256_hex(&bridge.wasm));
        bridges.insert(
            interface.clone(),
            Bridge { path: path.clone(), name: bridge.name.clone(), version: bridge.version.clone() },
        );
        entries.push(Entry::new(path, bridge.wasm.clone()));
    }

    if let Some(host_toml) = &inputs.host_toml {
        entries.push(Entry::new(layout::HOST_TOML, host_toml.clone()));
    }

    for (path, bytes) in &inputs.files {
        entries.push(Entry::new(path.clone(), bytes.clone()));
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let worlds: Vec<String> = inputs.components.keys().cloned().collect();
    let artifact = Artifact {
        kind: inputs.kind,
        worlds,
        entry_wasm: match inputs.kind {
            Kind::Clapp => Some(layout::APP_WASM.to_string()),
            Kind::Serve => None,
        },
        entries: match inputs.kind {
            Kind::Clapp => BTreeMap::new(),
            Kind::Serve => entry_paths,
        },
        bridges,
    };

    let manifest = Manifest {
        spec_version: SPEC_VERSION.to_string(),
        package: Package {
            name: inputs.name,
            version: inputs.version,
            description: inputs.description,
        },
        build: inputs.build,
        artifact,
        integrity: Integrity { wasm_sha256 },
    };

    let bytes = archive::write(&manifest, &entries)?;
    Ok(Packaged { bytes, manifest })
}

/// The file name a package is written under.
pub fn file_name(name: &str) -> String {
    format!("{name}.clapp")
}

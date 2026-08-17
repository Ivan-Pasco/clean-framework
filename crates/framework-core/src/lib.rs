//! Clean Framework's orchestration engine.
//!
//! This crate turns a project directory into a request document, hands it
//! across the compiler seam, and places the result. It knows nothing about
//! argv (that is `framework-cli`) and nothing about how the compiler is
//! actually invoked (that is `framework-compiler-driver`).
//!
//! The build flow it implements is §11.2 of the build-orchestration spec:
//! steps 1, 2, 3, 4, 6, 7, 8, 9, 10. Block-handler compilation (step 5) lands
//! in Phase 3.
//!
//! ```no_run
//! use framework_core::{build, BuildInputs};
//! use framework_compiler_driver::SubprocessCompiler;
//!
//! let inputs = BuildInputs::new("./my-app");
//! let compiler = SubprocessCompiler::for_project(&inputs.project_root)?;
//! let outcome = build(&inputs, &compiler)?;
//! println!("wrote {}", outcome.dist_wasm.display());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod build;
pub mod cache;
pub mod closure;
pub mod discover;
pub mod errors;
pub mod hosttoml;
pub mod hostwit;
pub mod library;
pub mod lockfile;
pub mod lower;
pub mod manifest;
pub mod package;
pub mod plugin;
pub mod resolver;

pub use build::{
    assemble_request, build, BuildInputs, BuildOutcome, Caching, APP_WASM, DIST_DIR,
};
pub use cache::{BuildCache, CacheError, CachedCompiler};
pub use closure::{ClosureError, ResolvedClosure};
pub use library::{LibraryError, LibraryManifest};
pub use lockfile::{LockError, LockedPackage, Lockfile, PackageKind, PackageSource};
pub use package::{package, PackageOutcome};
pub use plugin::{LoadedPlugin, PluginError, PluginManifest};
pub use resolver::{NoResolver, ResolveError, Resolver, SubprocessResolver};
pub use errors::{DiscoveryError, FrameworkError, HostWitError, ManifestError};
pub use hostwit::{HostContract, HostWitCache, Network, WitFetcher};
pub use lower::{ConfigOverride, OverrideSource};
pub use manifest::{Manifest, TargetSection};

/// This framework's own version, per PLAN.md open question #2 (resolved).
///
/// Reading `.cln/frame-version` would be circular: Manager already used that
/// file to choose which framework binary to launch, so the running binary *is*
/// the pinned version.
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("could not read {}: {source}", .path.display())]
    Input {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write {}: {source}", .path.display())]
    Output {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize manifest.toml: {0}")]
    ManifestSerialize(#[source] toml::ser::Error),

    #[error("could not build the archive: {0}")]
    Archive(#[source] zip::result::ZipError),

    #[error("could not write {path} into the archive: {source}")]
    ArchiveWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// The build has not run, or ran and failed. Packaging never compiles on
    /// the caller's behalf — `framework-core` decides whether a build is
    /// needed, so that the "is dist stale?" question has exactly one owner.
    #[error("no built component at {} — run a build first", .path.display())]
    NotBuilt { path: PathBuf },

    /// A capability the guest imports has no bridge component to carry.
    ///
    /// Failing here rather than shipping the package is the point: a bundle
    /// missing a bridge deploys cleanly and then refuses to start, reaching
    /// the operator as a deploy that timed out waiting for health, with the
    /// real reason in a log they cannot see (CLNH-18).
    #[error("no bridge component found for {interface} (backend {backend}) at {}", .searched.display())]
    BridgeMissing {
        interface: String,
        backend: String,
        searched: PathBuf,
    },
}

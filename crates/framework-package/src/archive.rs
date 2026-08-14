//! The ZIP writer (Manager §00.14).
//!
//! ZIP rather than tar because every OS opens one natively, existing tooling
//! can inspect a bundle without Clean-specific software, and it is what every
//! mature comparable format chose (`.jar`, `.apk`, `.whl`, `.crate`).

use std::io::{Cursor, Write};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::error::PackageError;
use crate::manifest::Manifest;

/// The manifest's fixed name at the archive root.
pub const MANIFEST_NAME: &str = "manifest.toml";

/// One file destined for the archive.
pub struct Entry {
    /// Archive-relative path, always `/`-separated.
    pub path: String,
    pub bytes: Vec<u8>,
}

impl Entry {
    pub fn new(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Entry { path: path.into(), bytes }
    }
}

/// Hex-lowercase SHA-256, the encoding every manifest field and every
/// consumer's comparison uses.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Write a complete archive to memory.
///
/// In memory rather than streamed to disk because the caller stages the file
/// and renames it into place (FRM-BO-10 — a package is either complete or
/// absent, never half-written), and because the whole archive is bounded by
/// what the compiler just produced.
///
/// Entries are written in the order given. Callers sort before calling; see
/// [`crate::build_entries`], which is what makes the output byte-reproducible.
pub fn write(manifest: &Manifest, entries: &[Entry]) -> Result<Vec<u8>, PackageError> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

    // Deflate rather than stored: wasm compresses well, and the archive
    // travels over the network on every deploy.
    //
    // A fixed timestamp is what makes two packages of identical inputs
    // byte-identical. ZIP records a modification time per entry, and the
    // default is "now" — which would make every package unique and defeat
    // content-addressed deduplication on the receiving side.
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(fixed_timestamp());

    // The manifest goes first so a reader can stat it without scanning.
    let manifest_toml = manifest.to_toml().map_err(PackageError::ManifestSerialize)?;
    add(&mut zip, MANIFEST_NAME, manifest_toml.as_bytes(), options)?;

    for entry in entries {
        add(&mut zip, &entry.path, &entry.bytes, options)?;
    }

    let cursor = zip.finish().map_err(PackageError::Archive)?;
    Ok(cursor.into_inner())
}

fn add(
    zip: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), PackageError> {
    zip.start_file(path, options).map_err(PackageError::Archive)?;
    zip.write_all(bytes)
        .map_err(|source| PackageError::ArchiveWrite { path: path.to_string(), source })?;
    Ok(())
}

/// The DOS epoch — the earliest timestamp the ZIP format can represent.
///
/// Any fixed value works; this one is unmistakably a constant rather than a
/// real build time, so nobody reads it as provenance. The real build time is
/// in `manifest.toml`'s `built_at`, where it can be recorded without making
/// the bytes vary.
fn fixed_timestamp() -> zip::DateTime {
    zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .expect("1980-01-01 is representable in the ZIP date format")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_known_digest_of_the_empty_input() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}

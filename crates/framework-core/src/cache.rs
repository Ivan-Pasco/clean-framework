//! The build cache — §11.7.
//!
//! Keyed by the SHA-256 of the request document, which is the whole input to a
//! compilation (CMP-01: the compiler reads nothing else). Two builds with the
//! same key are the same build, so a hit can skip the compiler entirely and
//! still satisfy CMP-06 — byte-identical output for byte-identical input.
//!
//! # What is stored, and why it is the raw tarball
//!
//! The cache stores the compiler's response **exactly as it came off stdout**,
//! before parsing. Storing the parsed [`CompileArtifact`] instead would mean
//! re-serializing it on write and hoping the round-trip is lossless — and any
//! field the framework does not model (the build manifest is deliberately an
//! opaque `serde_json::Value`, §14.8) would be silently dropped or reordered.
//!
//! Keeping the bytes means a cache hit and a cache miss run through the *same*
//! `CompileArtifact::from_tar` on the *same* bytes. There is no second code
//! path that could diverge, which is what makes "byte-identically reproduces
//! the output" a property rather than an aspiration.
//!
//! # Why this is a `Compiler`
//!
//! [`CachedCompiler`] wraps another [`Compiler`] rather than living inside the
//! build orchestrator. The orchestrator then has no cache-shaped branch in it,
//! and every caller that already takes a `&dyn Compiler` — `cln build`, the
//! test suite, `cln dev` later — gets caching by construction rather than by
//! remembering to ask for it.

use std::path::{Path, PathBuf};

use framework_compiler_driver::artifact::CompileArtifact;
use framework_compiler_driver::{CompileError, Compiler, RequestDocument};

/// Directory name under the toolchain root (`~/.cln/build-cache/`).
pub const BUILD_CACHE_DIR: &str = "build-cache";

/// The stored response's file name inside an entry.
const ARTIFACT_FILE: &str = "artifact.tar";

/// A content-addressed store of compiler responses.
#[derive(Clone, Debug)]
pub struct BuildCache {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("could not read the build cache at {}: {source}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write the build cache at {}: {source}", .path.display())]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot locate the build cache: no home directory")]
    NoHomeDirectory,
}

impl CacheError {
    /// `FRM002` — local I/O around the framework's own storage, the same code
    /// the host-contract cache uses for the same class of failure.
    pub fn code(&self) -> &'static str {
        "FRM002"
    }

    pub fn help(&self) -> Option<String> {
        match self {
            CacheError::NoHomeDirectory => {
                Some("set CLN_HOME to the toolchain root".into())
            }
            _ => Some("check that ~/.cln/ is writable, or build with --no-cache".into()),
        }
    }
}

impl BuildCache {
    /// A cache rooted at `dir`. Tests point this at a temp directory so they
    /// never read or write the developer's real cache.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        BuildCache { root: dir.into() }
    }

    /// The user's cache, `~/.cln/build-cache/` (or `$CLN_HOME/build-cache/`).
    pub fn user() -> Result<Self, CacheError> {
        let layout = cln_layout::Layout::from_home().ok_or(CacheError::NoHomeDirectory)?;
        Ok(BuildCache::at(layout.root().join(BUILD_CACHE_DIR)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an entry lives.
    ///
    /// Sharded on the first two hex characters. A flat directory of every
    /// build a developer has ever run reaches tens of thousands of entries,
    /// which several filesystems handle badly; two levels keep each directory
    /// small at no lookup cost, and it is the same layout git uses.
    fn entry_dir(&self, key: &str) -> PathBuf {
        let (prefix, rest) = key.split_at(2.min(key.len()));
        self.root.join(prefix).join(rest)
    }

    /// The stored compiler response for `key`, if any.
    ///
    /// A cache that cannot be read is **not** an error: the build should
    /// proceed by compiling. Only a corrupt *write* deserves attention, and
    /// that is caught when the entry fails to parse — at which point the entry
    /// is ignored and recompiled over. A cache is an optimization, and an
    /// optimization that can fail a build is a liability.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.entry_dir(key).join(ARTIFACT_FILE)).ok()
    }

    /// Store a compiler response under `key`.
    ///
    /// Written to a temp file and renamed, so a concurrent reader sees either
    /// no entry or a complete one. Two `cln build` runs on one machine (an
    /// editor's build racing a terminal's) would otherwise be able to read a
    /// half-written tarball, and a truncated artifact is far worse than a
    /// miss.
    pub fn put(&self, key: &str, artifact_tar: &[u8]) -> Result<(), CacheError> {
        let dir = self.entry_dir(key);
        std::fs::create_dir_all(&dir)
            .map_err(|source| CacheError::Unwritable { path: dir.clone(), source })?;

        // The temp file is named for the key, not `Random`: two processes
        // storing the *same* key would otherwise leave one file orphaned if
        // one of them died between create and rename. Same key, same temp
        // name, and the loser's rename simply overwrites with identical bytes.
        let staging = dir.join(format!(".{key}.partial"));
        std::fs::write(&staging, artifact_tar)
            .map_err(|source| CacheError::Unwritable { path: staging.clone(), source })?;

        let final_path = dir.join(ARTIFACT_FILE);
        std::fs::rename(&staging, &final_path).map_err(|source| {
            // Leave no debris if the rename failed.
            let _ = std::fs::remove_file(&staging);
            CacheError::Unwritable { path: final_path, source }
        })
    }

    /// Every key currently stored, sorted. For `cln cache` inspection.
    pub fn keys(&self) -> Result<Vec<String>, CacheError> {
        let mut keys = Vec::new();

        let shards = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            // An empty cache is a cache with no entries, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
            Err(source) => {
                return Err(CacheError::Unreadable { path: self.root.clone(), source })
            }
        };

        for shard in shards {
            let shard = shard
                .map_err(|source| CacheError::Unreadable { path: self.root.clone(), source })?;
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let prefix = shard.file_name().to_string_lossy().into_owned();

            let entries = std::fs::read_dir(shard.path())
                .map_err(|source| CacheError::Unreadable { path: shard.path(), source })?;

            for entry in entries {
                let entry = entry
                    .map_err(|source| CacheError::Unreadable { path: shard.path(), source })?;
                // A directory without the artifact file is a half-written or
                // hand-deleted entry; it is not a cached build.
                if !entry.path().join(ARTIFACT_FILE).is_file() {
                    continue;
                }
                keys.push(format!("{prefix}{}", entry.file_name().to_string_lossy()));
            }
        }

        keys.sort();
        Ok(keys)
    }

    /// Total size of the stored artifacts, in bytes. For `cln cache`.
    pub fn size_bytes(&self) -> Result<u64, CacheError> {
        let mut total = 0;
        for key in self.keys()? {
            let path = self.entry_dir(&key).join(ARTIFACT_FILE);
            if let Ok(metadata) = std::fs::metadata(&path) {
                total += metadata.len();
            }
        }
        Ok(total)
    }

    /// Remove every entry. For `cln cache clear`.
    pub fn clear(&self) -> Result<(), CacheError> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CacheError::Unwritable { path: self.root.clone(), source }),
        }
    }
}

/// A [`Compiler`] that consults a [`BuildCache`] before compiling.
pub struct CachedCompiler<'a> {
    inner: &'a dyn Compiler,
    cache: BuildCache,
    /// Set when a compile was served from the cache. Lets a caller report
    /// "cached" without asking the cache a second question whose answer could
    /// have changed in between.
    last_was_hit: std::cell::Cell<bool>,
    /// The inner compiler's version, resolved once — see [`CachedCompiler::key`].
    /// `None` inside the cell means "asked, and it would not say".
    compiler_version: std::cell::OnceCell<Option<String>>,
}

impl<'a> CachedCompiler<'a> {
    pub fn new(inner: &'a dyn Compiler, cache: BuildCache) -> Self {
        CachedCompiler {
            inner,
            cache,
            last_was_hit: std::cell::Cell::new(false),
            compiler_version: std::cell::OnceCell::new(),
        }
    }

    /// Did the most recent [`Compiler::compile`] come from the cache?
    pub fn last_was_hit(&self) -> bool {
        self.last_was_hit.get()
    }

    /// The cache key: the request hash **combined with the compiler's
    /// version**.
    ///
    /// The request document alone is not enough. It describes the *input* to a
    /// compilation, and CMP-06 promises identical output only for an identical
    /// input **through the same compiler**. Keying on the request alone means
    /// two compilers — an upgraded toolchain, or a project pinned to an older
    /// one — read and overwrite each other's entries, and a developer gets a
    /// component built by a compiler they are no longer using, with no way to
    /// tell from the output.
    ///
    /// Returns `None` when the compiler will not report a version. That is not
    /// a build failure: the compile proceeds uncached, because an entry that
    /// cannot be attributed to a compiler is an entry that cannot be safely
    /// reused later.
    ///
    /// The version is asked for once and remembered. `version()` on the real
    /// compiler is a subprocess spawn, and paying it on every compile would
    /// hand back a slice of what the cache exists to save — most of all in
    /// `cln dev`, which compiles on every keystroke.
    fn key(&self, request: &RequestDocument) -> Result<Option<String>, CompileError> {
        let request_sha256 = request.sha256()?;

        let version = self
            .compiler_version
            .get_or_init(|| self.inner.version().ok());

        let Some(version) = version.as_deref() else {
            return Ok(None);
        };

        Ok(Some(framework_compiler_driver::artifact::hex_sha256(
            format!("{request_sha256}\n{version}").as_bytes(),
        )))
    }
}

impl Compiler for CachedCompiler<'_> {
    fn compile(&self, request: &RequestDocument) -> Result<CompileArtifact, CompileError> {
        let Some(key) = self.key(request)? else {
            // The compiler would not name itself, so the entry could not be
            // attributed to it. Compiling uncached is the honest outcome —
            // see `key`.
            self.last_was_hit.set(false);
            return self.inner.compile(request);
        };

        if let Some(bytes) = self.cache.get(&key) {
            // A stored entry that no longer parses means a corrupt or
            // truncated write, or a tar format the current build cannot read.
            // Recompiling is always correct; failing the build over a bad
            // cache entry would not be.
            match CompileArtifact::from_tar(&bytes) {
                Ok(artifact) => {
                    self.last_was_hit.set(true);
                    return Ok(artifact);
                }
                Err(_) => {
                    // Fall through and compile. The entry is overwritten below.
                }
            }
        }

        self.last_was_hit.set(false);
        let (artifact, tarball) = self.inner.compile_capturing(request)?;

        // An empty capture means this compiler cannot hand back its raw
        // response (the trait's default). Storing it would create an entry
        // that fails to parse on every later read — a permanent miss that
        // costs a disk write each build.
        if !tarball.is_empty() {
            // A cache that cannot be written must not fail a build that already
            // succeeded. The compiler produced a valid component; refusing to
            // return it because a disk is full would be strictly worse than
            // running uncached.
            let _ = self.cache.put(&key, &tarball);
        }

        Ok(artifact)
    }

    fn version(&self) -> Result<String, CompileError> {
        self.inner.version()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framework_compiler_driver::request::{
        Build, Project, Source, TargetWorld, SPEC_VERSION,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) fn request(source: &str) -> RequestDocument {
        RequestDocument {
            spec_version: SPEC_VERSION.to_string(),
            project: Project { name: "x".into(), version: "0.1.0".into() },
            build: Build {
                target: "wasm32-cli".into(),
                optimization: None,
                memory: None,
                strip: None,
                component_model: None,
                memory64: None,
            },
            folders: BTreeMap::new(),
            dependencies: BTreeMap::new(),
            compile_limits: None,
            telemetry: None,
            target_world: TargetWorld {
                host: "clean-cli".into(),
                version: "0.1.0".into(),
                world: "cli".into(),
                sha256: "abc".into(),
                wit: "package clean:host@0.1.0;\nworld cli {}\n".into(),
            },
            sources: vec![Source {
                path: "app/main.cln".into(),
                sha256: "abc".into(),
                content: source.into(),
            }],
            library_manifests: Vec::new(),
            overrides: Vec::new(),
        }
    }

    /// A tarball in the shape the compiler emits.
    pub(super) fn artifact_tar(wasm: &[u8]) -> Vec<u8> {
        use framework_compiler_driver::artifact::{
            DIAGNOSTICS_ENTRY, MANIFEST_ENTRY, WASM_ENTRY,
        };

        let mut builder = tar::Builder::new(Vec::new());
        let mut append = |name: &str, bytes: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        };

        append(WASM_ENTRY, wasm);
        append(MANIFEST_ENTRY, br#"{"spec_version":"1","compiler":{"version":"0.0.0"}}"#);
        append(DIAGNOSTICS_ENTRY, b"[]");
        builder.into_inner().unwrap()
    }

    /// Counts compiles, so a test can prove the compiler was not reached.
    pub(super) struct CountingCompiler {
        calls: AtomicUsize,
        wasm: Vec<u8>,
        version: String,
    }

    impl CountingCompiler {
        pub(super) fn new(wasm: &[u8]) -> Self {
            CountingCompiler {
                calls: AtomicUsize::new(0),
                wasm: wasm.to_vec(),
                version: "0.0.0".into(),
            }
        }

        pub(super) fn versioned(wasm: &[u8], version: &str) -> Self {
            CountingCompiler { version: version.into(), ..CountingCompiler::new(wasm) }
        }

        pub(super) fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Compiler for CountingCompiler {
        fn compile(&self, request: &RequestDocument) -> Result<CompileArtifact, CompileError> {
            self.compile_capturing(request).map(|(artifact, _)| artifact)
        }

        fn compile_capturing(
            &self,
            _request: &RequestDocument,
        ) -> Result<(CompileArtifact, Vec<u8>), CompileError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let tarball = artifact_tar(&self.wasm);
            Ok((CompileArtifact::from_tar(&tarball)?, tarball))
        }

        fn version(&self) -> Result<String, CompileError> {
            Ok(self.version.clone())
        }
    }

    pub(super) const WASM: &[u8] = &[0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    pub(super) fn cache() -> (tempfile::TempDir, BuildCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::at(dir.path().join("build-cache"));
        (dir, cache)
    }

    #[test]
    fn a_second_identical_build_does_not_reach_the_compiler() {
        // The whole point of §11.7.
        let (_dir, cache) = cache();
        let inner = CountingCompiler::new(WASM);
        let cached = CachedCompiler::new(&inner, cache);

        let first = cached.compile(&request("start()\n")).unwrap();
        assert_eq!(inner.calls(), 1);
        assert!(!cached.last_was_hit());

        let second = cached.compile(&request("start()\n")).unwrap();
        assert_eq!(inner.calls(), 1, "the compiler must not run again");
        assert!(cached.last_was_hit());

        // CMP-06: byte-identical output.
        assert_eq!(first.wasm, second.wasm);
    }

    #[test]
    fn a_changed_source_misses_the_cache() {
        let (_dir, cache) = cache();
        let inner = CountingCompiler::new(WASM);
        let cached = CachedCompiler::new(&inner, cache);

        cached.compile(&request("start()\n")).unwrap();
        cached.compile(&request("start()\nprint()\n")).unwrap();

        assert_eq!(inner.calls(), 2, "a different request is a different build");
    }

    #[test]
    fn a_hit_reproduces_every_part_of_the_artifact_not_just_the_wasm() {
        // The manifest and diagnostics ride in the same tarball; a cache that
        // restored only the component would silently drop provenance.
        let (_dir, cache) = cache();
        let inner = CountingCompiler::new(WASM);
        let cached = CachedCompiler::new(&inner, cache);

        let first = cached.compile(&request("start()\n")).unwrap();
        let second = cached.compile(&request("start()\n")).unwrap();

        assert_eq!(first.wasm, second.wasm);
        assert_eq!(
            first.manifest.compiler_version(),
            second.manifest.compiler_version()
        );
        assert_eq!(first.diagnostics.len(), second.diagnostics.len());
        assert_eq!(first.source_map, second.source_map);
    }

    #[test]
    fn a_corrupt_entry_recompiles_rather_than_failing_the_build() {
        // A cache is an optimization. An optimization that can fail a build is
        // a liability.
        let (_dir, cache) = cache();
        let inner = CountingCompiler::new(WASM);
        let key = request("start()\n").sha256().unwrap();

        cache.put(&key, b"this is not a tar archive").unwrap();

        let cached = CachedCompiler::new(&inner, cache);
        let artifact = cached.compile(&request("start()\n")).unwrap();

        assert_eq!(inner.calls(), 1, "a bad entry must fall through to the compiler");
        assert_eq!(artifact.wasm, WASM);
        assert!(!cached.last_was_hit());
    }

    #[test]
    fn a_corrupt_entry_is_replaced_by_the_recompile() {
        let (_dir, cache) = cache();
        let inner = CountingCompiler::new(WASM);
        let key = request("start()\n").sha256().unwrap();
        cache.put(&key, b"garbage").unwrap();

        let cached = CachedCompiler::new(&inner, cache.clone());
        cached.compile(&request("start()\n")).unwrap();
        cached.compile(&request("start()\n")).unwrap();

        assert_eq!(inner.calls(), 1, "the repaired entry must serve the next build");
    }

    #[test]
    fn an_unwritable_cache_does_not_fail_a_successful_build() {
        // The compiler produced a valid component; refusing to return it
        // because a disk is full would be strictly worse than running uncached.
        let inner = CountingCompiler::new(WASM);

        // A file where the cache directory should be: every write fails.
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, b"x").unwrap();

        let cached = CachedCompiler::new(&inner, BuildCache::at(&blocked));
        let artifact = cached.compile(&request("start()\n")).unwrap();

        assert_eq!(artifact.wasm, WASM);
        assert_eq!(inner.calls(), 1);
    }

    #[test]
    fn entries_are_sharded_so_no_directory_grows_without_bound() {
        let (_dir, cache) = cache();
        let key = "abcdef0123456789";
        cache.put(key, &artifact_tar(WASM)).unwrap();

        assert!(cache.root().join("ab").join("cdef0123456789").is_dir());
        assert_eq!(cache.keys().unwrap(), vec![key.to_string()]);
    }

    #[test]
    fn a_missing_cache_directory_lists_no_keys_rather_than_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::at(dir.path().join("never-created"));
        assert!(cache.keys().unwrap().is_empty());
        assert_eq!(cache.size_bytes().unwrap(), 0);
    }

    #[test]
    fn a_half_written_entry_is_not_reported_as_cached() {
        // A directory with no artifact file is debris, not a build.
        let (_dir, cache) = cache();
        std::fs::create_dir_all(cache.root().join("ab").join("cdef")).unwrap();
        assert!(cache.keys().unwrap().is_empty());
    }

    #[test]
    fn writing_leaves_no_staging_file_behind() {
        let (_dir, cache) = cache();
        let key = "abcdef";
        cache.put(key, &artifact_tar(WASM)).unwrap();

        let entry = cache.root().join("ab").join("cdef");
        let leftovers: Vec<String> = std::fs::read_dir(&entry)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != ARTIFACT_FILE)
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    #[test]
    fn size_and_clear_report_and_remove_what_was_stored() {
        let (_dir, cache) = cache();
        let tarball = artifact_tar(WASM);
        cache.put("aabbcc", &tarball).unwrap();
        cache.put("ddeeff", &tarball).unwrap();

        assert_eq!(cache.keys().unwrap().len(), 2);
        assert_eq!(cache.size_bytes().unwrap(), (tarball.len() * 2) as u64);

        cache.clear().unwrap();
        assert!(cache.keys().unwrap().is_empty());
        // Clearing twice is not an error — the second is already clear.
        cache.clear().unwrap();
    }

    #[test]
    fn overwriting_an_entry_replaces_it_wholesale() {
        let (_dir, cache) = cache();
        let key = "abcdef";
        cache.put(key, b"first").unwrap();
        cache.put(key, b"second-and-longer").unwrap();
        assert_eq!(cache.get(key).unwrap(), b"second-and-longer");
    }
}

#[cfg(test)]
mod compiler_identity_tests {
    use super::tests::*;
    use super::*;

    #[test]
    fn two_compiler_versions_do_not_share_entries() {
        // The request document describes the *input*. CMP-06 promises
        // identical output only for identical input through the SAME compiler.
        // Without the version in the key, an upgraded toolchain reads the old
        // one's entries and the developer silently gets a component built by a
        // compiler they no longer use.
        let (_dir, cache) = cache();

        let old = CountingCompiler::versioned(WASM, "0.1.0");
        CachedCompiler::new(&old, cache.clone())
            .compile(&request("start()\n"))
            .unwrap();
        assert_eq!(old.calls(), 1);

        let new = CountingCompiler::versioned(WASM, "0.2.0");
        let cached = CachedCompiler::new(&new, cache);
        cached.compile(&request("start()\n")).unwrap();

        assert_eq!(new.calls(), 1, "a different compiler must not reuse the entry");
        assert!(!cached.last_was_hit());
    }

    #[test]
    fn the_same_compiler_version_still_hits() {
        // The other half: adding the version to the key must not defeat the
        // cache for the ordinary case of building twice with one toolchain.
        let (_dir, cache) = cache();

        let first = CountingCompiler::versioned(WASM, "0.1.0");
        CachedCompiler::new(&first, cache.clone())
            .compile(&request("start()\n"))
            .unwrap();

        let second = CountingCompiler::versioned(WASM, "0.1.0");
        let cached = CachedCompiler::new(&second, cache);
        cached.compile(&request("start()\n")).unwrap();

        assert_eq!(second.calls(), 0, "same compiler, same request: must hit");
        assert!(cached.last_was_hit());
    }

    #[test]
    fn a_compiler_that_will_not_name_itself_compiles_uncached() {
        // An entry that cannot be attributed to a compiler cannot be safely
        // reused, so it is not written at all — but the build still succeeds.
        struct Anonymous(std::sync::atomic::AtomicUsize);

        impl Compiler for Anonymous {
            fn compile(
                &self,
                request: &RequestDocument,
            ) -> Result<CompileArtifact, CompileError> {
                self.compile_capturing(request).map(|(a, _)| a)
            }

            fn compile_capturing(
                &self,
                _request: &RequestDocument,
            ) -> Result<(CompileArtifact, Vec<u8>), CompileError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tarball = artifact_tar(WASM);
                Ok((CompileArtifact::from_tar(&tarball)?, tarball))
            }

            fn version(&self) -> Result<String, CompileError> {
                Err(CompileError::MalformedOutput("no version".into()))
            }
        }

        let (_dir, cache) = cache();
        let inner = Anonymous(std::sync::atomic::AtomicUsize::new(0));

        let cached = CachedCompiler::new(&inner, cache.clone());
        cached.compile(&request("start()\n")).unwrap();
        cached.compile(&request("start()\n")).unwrap();

        assert_eq!(inner.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(cache.keys().unwrap().is_empty(), "nothing may be stored");
    }

    #[test]
    fn a_compiler_that_cannot_capture_its_output_is_not_cached() {
        // The trait's default `compile_capturing` returns no bytes. Storing
        // them would create an entry that fails to parse on every read — a
        // permanent miss that costs a disk write per build.
        struct NoCapture;

        impl Compiler for NoCapture {
            fn compile(
                &self,
                _request: &RequestDocument,
            ) -> Result<CompileArtifact, CompileError> {
                CompileArtifact::from_tar(&artifact_tar(WASM))
            }
            fn version(&self) -> Result<String, CompileError> {
                Ok("0.1.0".into())
            }
        }

        let (_dir, cache) = cache();
        CachedCompiler::new(&NoCapture, cache.clone())
            .compile(&request("start()\n"))
            .unwrap();

        assert!(cache.keys().unwrap().is_empty(), "an empty capture must not be stored");
    }
}

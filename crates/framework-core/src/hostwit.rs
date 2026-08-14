//! Obtaining the target's `host.wit` — the Moment 1 obligation (HCV-01,
//! HCV-04), and the source of the request document's `target_world`
//! (FRM-BO-16, ADR-0033).
//!
//! Four rules shape everything here:
//!
//! - **The published `host.wit` is the single declaration** (BVER-03). The
//!   framework never synthesizes a contract, never bundles a fallback, and
//!   never infers one from `build.target`. If it cannot be obtained, the build
//!   fails saying so.
//! - **The text is passed on verbatim.** No extraction of the selected world,
//!   no reformatting, no comment stripping. The request records what the host
//!   published, so `cln repro build` can show the contract as shipped and a bug
//!   in our WIT handling can never masquerade as a host contract change.
//! - **The cache is content-addressed by `<host>@<version>.wit`** under
//!   `~/.cln/host-wit/`, and the hash is pinned in `.cln/lock.toml` (BVER-03).
//!   A cached file whose hash disagrees with the lockfile is a hard error, not
//!   a silent refetch — that disagreement is either tampering or a host
//!   republishing under a version it already used.
//! - **`--offline` means cache-or-fail**, never fetch. C-18 promises every
//!   command works offline after a fetch; the honest failure when the cache is
//!   cold is a message naming what is missing and how to get it.
//!
//! What is deliberately *not* here: any notion of a default host. A project
//! with no `[target]` block is a config error (CFG001), because guessing would
//! put the developer's first confusing failure at instantiation time instead of
//! at the manifest line that is actually wrong.

use std::path::{Path, PathBuf};

use framework_compiler_driver::artifact::hex_sha256;

use cln_layout::Layout;

use crate::errors::{FrameworkError, HostWitError};

/// Cache root for fetched host contracts, relative to the user's home.
/// Canonical location per Platform 16 §16.5 and Manager §00.2.
pub const CACHE_DIR: &str = ".cln/host-wit";

/// The project lockfile that pins the contract hash (BVER-03).
pub const LOCKFILE: &str = ".cln/lock.toml";

/// Official hosts and where their `host.wit` is published.
///
/// A host in this table needs no `wit_source` in `clean.toml`; anything else
/// must supply one (§16.5). The table maps a name to a URL *template* whose
/// `{version}` is substituted — it never maps a name to contract *content*,
/// which would make this table a second declaration and defeat BVER-03.
const OFFICIAL_HOSTS: &[(&str, &str)] = &[
    (
        "clean-server",
        "https://raw.githubusercontent.com/Ivan-Pasco/clean-server/v{version}/host.wit",
    ),
    (
        "clean-browser",
        "https://raw.githubusercontent.com/Ivan-Pasco/clean-framework/v{version}/hosts/clean-browser/host.wit",
    ),
    (
        "clean-cli",
        "https://raw.githubusercontent.com/Ivan-Pasco/clean-manager/v{version}/hosts/clean-cli/host.wit",
    ),
];

/// A host contract, obtained and verified. This is what becomes `target_world`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostContract {
    pub host: String,
    /// The concrete version this resolved to — never the manifest's constraint.
    pub version: String,
    /// Hex-lowercase SHA-256 of `wit`.
    pub sha256: String,
    /// The published `host.wit`, verbatim.
    pub wit: String,
}

/// How a contract may be obtained for this build.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Network {
    /// Cache first, then fetch on a miss.
    Allowed,
    /// Cache or fail (`--offline`). Never fetches.
    Offline,
}

/// Where the cache lives. Injectable so tests never touch the real `~/.cln`.
#[derive(Clone, Debug)]
pub struct HostWitCache {
    root: PathBuf,
}

impl HostWitCache {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        HostWitCache { root: root.into() }
    }

    /// The user-level cache under the toolchain root — `~/.cln/host-wit/`,
    /// or `$CLN_HOME/host-wit/` when that is set.
    ///
    /// Resolved through `cln-layout` rather than from `HOME` directly, for the
    /// same reason the compiler resolver does: every process that locates a
    /// toolchain has to agree on where it is. Reading the environment here
    /// meant a relocated build found its pinned compiler and then failed to
    /// find the host contract beside it — the two answers came from different
    /// code that had already drifted apart.
    pub fn user() -> Result<Self, FrameworkError> {
        let layout = Layout::from_home().ok_or(HostWitError::NoHomeDirectory)?;
        Ok(HostWitCache { root: layout.host_wit_dir() })
    }

    pub fn path_for(&self, host: &str, version: &str) -> PathBuf {
        self.root.join(format!("{host}@{version}.wit"))
    }

    /// Read a cached contract, if present. A cache read that fails for any
    /// reason other than absence is an error — a partially-written or
    /// unreadable cache entry must not silently become a refetch.
    pub fn get(&self, host: &str, version: &str) -> Result<Option<String>, FrameworkError> {
        let path = self.path_for(host, version);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(HostWitError::CacheUnreadable { path, source }.into()),
        }
    }

    /// Write a fetched contract into the cache, atomically.
    ///
    /// Via a temp file and a rename, so a concurrent `cln build` never reads a
    /// half-written contract and takes its hash.
    pub fn put(&self, host: &str, version: &str, wit: &str) -> Result<(), FrameworkError> {
        std::fs::create_dir_all(&self.root)
            .map_err(|source| HostWitError::CacheUnwritable { path: self.root.clone(), source })?;

        let final_path = self.path_for(host, version);
        let staging = self.root.join(format!(".{host}@{version}.wit.tmp-{}", std::process::id()));

        std::fs::write(&staging, wit)
            .map_err(|source| HostWitError::CacheUnwritable { path: staging.clone(), source })?;

        std::fs::rename(&staging, &final_path).map_err(|source| {
            let _ = std::fs::remove_file(&staging);
            HostWitError::CacheUnwritable { path: final_path.clone(), source }
        })?;

        Ok(())
    }
}

/// Something that can retrieve a contract over the network.
///
/// A trait so the fetch path is testable without a network, and so `--offline`
/// can be enforced by *not having* a fetcher rather than by a flag some future
/// code path forgets to check.
pub trait WitFetcher {
    fn fetch(&self, url: &str) -> Result<String, FrameworkError>;
}

/// The Moment 1 obligation: obtain the target's contract, verified.
///
/// `lock` is the hash `.cln/lock.toml` pins for this host, when the project has
/// one. When present it is authoritative: a contract whose hash disagrees is
/// refused rather than accepted or refetched.
pub fn resolve_contract(
    host: &str,
    version: &str,
    wit_source: Option<&str>,
    cache: &HostWitCache,
    network: Network,
    fetcher: Option<&dyn WitFetcher>,
    lock: Option<&str>,
) -> Result<HostContract, FrameworkError> {
    let wit = match cache.get(host, version)? {
        Some(cached) => cached,
        None => match network {
            Network::Offline => {
                return Err(HostWitError::OfflineCacheMiss {
                    host: host.to_string(),
                    version: version.to_string(),
                    path: cache.path_for(host, version),
                }
                .into())
            }
            Network::Allowed => {
                let url = resolve_source(host, version, wit_source)?;
                let fetcher = fetcher.ok_or_else(|| HostWitError::NoFetcher {
                    host: host.to_string(),
                })?;
                let fetched = fetcher.fetch(&url)?;
                cache.put(host, version, &fetched)?;
                fetched
            }
        },
    };

    let sha256 = hex_sha256(wit.as_bytes());

    // BVER-03: the lockfile pin is authoritative. A mismatch means the cached
    // or fetched contract is not the one this project was locked against —
    // either the host republished under the same version or the cache was
    // tampered with. Both need a human, neither is safe to paper over.
    if let Some(pinned) = lock {
        if pinned != sha256 {
            return Err(HostWitError::LockMismatch {
                host: host.to_string(),
                version: version.to_string(),
                expected: pinned.to_string(),
                found: sha256,
            }
            .into());
        }
    }

    Ok(HostContract {
        host: host.to_string(),
        version: version.to_string(),
        sha256,
        wit,
    })
}

/// Where to fetch a host's contract from.
///
/// An explicit `wit_source` always wins; otherwise the host must be official.
/// An unknown host with no source is an error naming both facts, because the
/// fix differs: add the URL, or check the spelling.
pub fn resolve_source(
    host: &str,
    version: &str,
    wit_source: Option<&str>,
) -> Result<String, FrameworkError> {
    if let Some(explicit) = wit_source {
        return Ok(explicit.replace("{version}", version));
    }

    OFFICIAL_HOSTS
        .iter()
        .find(|(name, _)| *name == host)
        .map(|(_, template)| template.replace("{version}", version))
        .ok_or_else(|| {
            HostWitError::UnknownHost {
                host: host.to_string(),
                known: OFFICIAL_HOSTS.iter().map(|(n, _)| *n).collect(),
            }
            .into()
        })
}

/// Whether a host is one of the official set (needs no `wit_source`).
pub fn is_official(host: &str) -> bool {
    OFFICIAL_HOSTS.iter().any(|(name, _)| *name == host)
}

/// The official host names, for error messages that must list them.
pub fn official_hosts() -> Vec<&'static str> {
    OFFICIAL_HOSTS.iter().map(|(name, _)| *name).collect()
}

/// Read the pinned contract hash for a host from `.cln/lock.toml`.
///
/// Absent lockfile, absent section, or absent entry all mean "not pinned yet",
/// which is the normal state of a project that has never built. A lockfile that
/// exists but cannot be parsed is an error — silently treating a corrupt
/// lockfile as unpinned would drop the BVER-03 guarantee exactly when it
/// matters.
pub fn pinned_hash(project_root: &Path, host: &str) -> Result<Option<String>, FrameworkError> {
    let path = project_root.join(LOCKFILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(HostWitError::LockUnreadable { path, source }.into()),
    };

    let parsed: toml::Value = toml::from_str(&text)
        .map_err(|source| HostWitError::LockMalformed { path: path.clone(), source })?;

    Ok(parsed
        .get("host")
        .and_then(|h| h.get(host))
        .and_then(|entry| entry.get("sha256"))
        .and_then(|v| v.as_str())
        .map(str::to_string))
}

/// Record the contract's hash in `.cln/lock.toml` (BVER-03).
///
/// Rewrites only the `[host.<name>]` table, preserving everything else in the
/// file — the lockfile also pins dependencies, and a build must not drop them.
pub fn pin_hash(
    project_root: &Path,
    contract: &HostContract,
) -> Result<(), FrameworkError> {
    let dir = project_root.join(".cln");
    std::fs::create_dir_all(&dir)
        .map_err(|source| HostWitError::LockUnwritable { path: dir.clone(), source })?;

    let path = project_root.join(LOCKFILE);
    let mut doc: toml::Table = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|source| HostWitError::LockMalformed { path: path.clone(), source })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(source) => return Err(HostWitError::LockUnreadable { path, source }.into()),
    };

    let mut entry = toml::Table::new();
    entry.insert("version".into(), contract.version.clone().into());
    entry.insert("sha256".into(), contract.sha256.clone().into());

    let hosts = doc
        .entry("host".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));

    match hosts.as_table_mut() {
        Some(table) => {
            table.insert(contract.host.clone(), toml::Value::Table(entry));
        }
        // `[host]` exists but is not a table — the file is not a lockfile we
        // wrote. Refuse rather than clobber it.
        None => {
            return Err(HostWitError::LockShape { path }.into());
        }
    }

    let rendered = toml::to_string_pretty(&doc)
        .map_err(|source| HostWitError::LockSerialize { path: path.clone(), source })?;

    std::fs::write(&path, rendered)
        .map_err(|source| HostWitError::LockUnwritable { path, source }.into())
        .map(|_| ())
}

/// Does `wit` declare `world <name>`?
///
/// A deliberately shallow check: the framework is a courier, not a WIT
/// compiler, and full parsing belongs to the compiler which does it anyway.
/// What this catches is the mistake worth catching here — a target whose world
/// the fetched contract does not contain, which would otherwise surface as a
/// confusing `COM012` on every call site instead of one message naming the
/// world and the host.
pub fn declares_world(wit: &str, world: &str) -> bool {
    wit.lines()
        .map(strip_line_comment)
        .any(|line| is_world_declaration(line, world))
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn is_world_declaration(line: &str, world: &str) -> bool {
    let rest = match line.trim_start().strip_prefix("world") {
        Some(r) => r,
        None => return false,
    };
    // `world` must be its own token: `worldly-thing` is not a declaration.
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let rest = rest.trim_start();
    let name = rest
        .split(|c: char| c.is_whitespace() || c == '{')
        .next()
        .unwrap_or_default();
    name == world
}


#[cfg(test)]
mod tests {
    use super::*;

    struct Canned(&'static str);
    impl WitFetcher for Canned {
        fn fetch(&self, _url: &str) -> Result<String, FrameworkError> {
            Ok(self.0.to_string())
        }
    }

    struct Exploding;
    impl WitFetcher for Exploding {
        fn fetch(&self, url: &str) -> Result<String, FrameworkError> {
            panic!("must not fetch: {url}");
        }
    }

    const SERVER_WIT: &str = "package clean:host@0.1.0;\n\nworld server {\n  export routing;\n}\n";

    fn cache() -> (tempfile::TempDir, HostWitCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = HostWitCache::at(dir.path());
        (dir, cache)
    }

    #[test]
    fn fetches_on_a_cold_cache_and_caches_the_result() {
        let (_dir, cache) = cache();
        let contract = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Allowed, Some(&Canned(SERVER_WIT)), None,
        )
        .unwrap();

        assert_eq!(contract.wit, SERVER_WIT);
        assert_eq!(contract.version, "0.6.0");

        // Second call must be served from cache — the exploding fetcher proves
        // no network is touched.
        let again = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Allowed, Some(&Exploding), None,
        )
        .unwrap();
        assert_eq!(again, contract);
    }

    #[test]
    fn wit_is_carried_verbatim_including_comments() {
        // ADR-0033 Option F rejected: the framework must not rewrite the
        // contract. Comments and formatting survive so `cln repro build` shows
        // what the host published.
        let source = "// a comment the host wrote\npackage clean:host@0.1.0;\n\nworld server {}\n";
        let (_dir, cache) = cache();
        let contract = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Allowed, Some(&Canned(source)), None,
        )
        .unwrap();
        assert_eq!(contract.wit, source);
    }

    #[test]
    fn offline_with_a_cold_cache_fails_and_never_fetches() {
        let (_dir, cache) = cache();
        let err = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Offline, Some(&Exploding), None,
        )
        .unwrap_err();
        assert_eq!(err.code(), "FRM004");
        assert!(err.to_string().contains("clean-server"), "got {err}");
    }

    #[test]
    fn offline_with_a_warm_cache_succeeds() {
        let (_dir, cache) = cache();
        cache.put("clean-server", "0.6.0", SERVER_WIT).unwrap();
        let contract = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Offline, Some(&Exploding), None,
        )
        .unwrap();
        assert_eq!(contract.wit, SERVER_WIT);
    }

    #[test]
    fn a_lockfile_mismatch_is_refused_not_papered_over() {
        let (_dir, cache) = cache();
        cache.put("clean-server", "0.6.0", SERVER_WIT).unwrap();
        let err = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Allowed, Some(&Canned(SERVER_WIT)),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        assert_eq!(err.code(), "FRM005");
    }

    #[test]
    fn a_matching_lockfile_hash_passes() {
        let (_dir, cache) = cache();
        cache.put("clean-server", "0.6.0", SERVER_WIT).unwrap();
        let expected = hex_sha256(SERVER_WIT.as_bytes());
        let contract = resolve_contract(
            "clean-server", "0.6.0", None, &cache, Network::Allowed, Some(&Exploding),
            Some(&expected),
        )
        .unwrap();
        assert_eq!(contract.sha256, expected);
    }

    #[test]
    fn official_hosts_need_no_wit_source() {
        let url = resolve_source("clean-server", "0.6.0", None).unwrap();
        assert!(url.contains("clean-server"), "got {url}");
        assert!(url.contains("v0.6.0"), "version must be substituted, got {url}");
        assert!(url.ends_with("/host.wit"), "HCV-02: must be the repo-root file, got {url}");
    }

    #[test]
    fn an_unknown_host_without_a_source_names_the_known_ones() {
        let err = resolve_source("pixel-engine", "0.1.0", None).unwrap_err();
        assert_eq!(err.code(), "CFG001");
        assert!(err.to_string().contains("pixel-engine"));
        assert!(err.help().unwrap().contains("clean-server"));
    }

    #[test]
    fn an_explicit_wit_source_wins_for_any_host() {
        let url = resolve_source(
            "pixel-engine", "0.1.0", Some("https://example.test/v{version}/host.wit"),
        )
        .unwrap();
        assert_eq!(url, "https://example.test/v0.1.0/host.wit");
    }

    #[test]
    fn declares_world_finds_the_named_world() {
        assert!(declares_world(SERVER_WIT, "server"));
        assert!(!declares_world(SERVER_WIT, "browser"));
    }

    #[test]
    fn declares_world_is_not_fooled_by_prefixes_or_comments() {
        assert!(!declares_world("world serverless {}\n", "server"));
        assert!(!declares_world("worldly {}\n", "world"));
        assert!(!declares_world("// world server {}\n", "server"));
        assert!(declares_world("world server{}\n", "server"));
        assert!(declares_world("  world   server   {\n", "server"));
    }

    #[test]
    fn declares_world_handles_a_multi_world_contract() {
        // The case the `world` selector exists for.
        let two = "package p:q@0.1.0;\nworld server {}\nworld browser {}\n";
        assert!(declares_world(two, "server"));
        assert!(declares_world(two, "browser"));
        assert!(!declares_world(two, "cli"));
    }

    #[test]
    fn pinning_round_trips_through_the_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let contract = HostContract {
            host: "clean-server".into(),
            version: "0.6.0".into(),
            sha256: "abc123".into(),
            wit: SERVER_WIT.into(),
        };
        pin_hash(dir.path(), &contract).unwrap();
        assert_eq!(pinned_hash(dir.path(), "clean-server").unwrap().as_deref(), Some("abc123"));
        assert_eq!(pinned_hash(dir.path(), "clean-browser").unwrap(), None);
    }

    #[test]
    fn pinning_preserves_other_lockfile_content() {
        // The lockfile also pins dependencies. A build must not drop them.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
        std::fs::write(
            dir.path().join(LOCKFILE),
            "[dependencies.data]\nversion = \"3.1.0\"\n",
        )
        .unwrap();

        let contract = HostContract {
            host: "clean-server".into(),
            version: "0.6.0".into(),
            sha256: "abc123".into(),
            wit: SERVER_WIT.into(),
        };
        pin_hash(dir.path(), &contract).unwrap();

        let text = std::fs::read_to_string(dir.path().join(LOCKFILE)).unwrap();
        assert!(text.contains("3.1.0"), "dependency pin was dropped: {text}");
        assert!(text.contains("abc123"), "host pin missing: {text}");
    }

    #[test]
    fn a_missing_lockfile_means_unpinned_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(pinned_hash(dir.path(), "clean-server").unwrap(), None);
    }

    #[test]
    fn a_corrupt_lockfile_is_an_error_not_a_silent_unpin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cln")).unwrap();
        std::fs::write(dir.path().join(LOCKFILE), "[host\nbroken").unwrap();
        assert!(pinned_hash(dir.path(), "clean-server").is_err());
    }
}

//! Lowering `clean.toml` into the request document — §11.4.
//!
//! "The lowering is mechanical and lossless." Two rules do the real work:
//!
//! - **Fields not present in `clean.toml` are omitted** from the request
//!   document, so the compiler applies its own Platform 07 defaults. The
//!   framework must not invent `optimization = "debug"` — that would make the
//!   framework a second home for defaults, which §11.9 forbids.
//! - **FRM-BO-08: overrides are audited, not merged.** A `--optimization
//!   debug` flag does not rewrite `build.optimization`; it appends an entry to
//!   `overrides[]`. The compiler applies it and records it, so `cln repro
//!   build` can replay the exact build.

use framework_compiler_driver::request::{
    Build, Override, Project, RequestDocument, Source, TargetWorld, SPEC_VERSION,
};

use crate::closure::ResolvedClosure;
use crate::hostwit::HostContract;
use crate::manifest::Manifest;

/// One overridden value, before it becomes an `overrides[]` entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigOverride {
    /// Dotted config path, e.g. `build.optimization`.
    pub path: String,
    pub value: String,
    pub source: OverrideSource,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OverrideSource {
    Cli,
    Env,
}

impl OverrideSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OverrideSource::Cli => "cli",
            OverrideSource::Env => "env",
        }
    }
}

impl ConfigOverride {
    pub fn cli(path: impl Into<String>, value: impl Into<String>) -> Self {
        ConfigOverride {
            path: path.into(),
            value: value.into(),
            source: OverrideSource::Cli,
        }
    }

    pub fn env(path: impl Into<String>, value: impl Into<String>) -> Self {
        ConfigOverride {
            path: path.into(),
            value: value.into(),
            source: OverrideSource::Env,
        }
    }
}

/// Project the manifest and discovered sources into the request document.
///
/// `sources` must already be sorted (FRM-BO-06) — [`crate::discover`]
/// guarantees that, and re-sorting here would hide a bug there.
///
/// `contract` is the host declaration fetched at Moment 1. It is a parameter
/// rather than something this function obtains because §11.4.1 puts it beside
/// the lowering, not in it: every other value here is a projection of
/// `clean.toml`, and this one comes from a different source entirely.
///
/// `closure` is the resolved dependency closure (steps 3 and 4), for the same
/// reason: it is `.cln/lock.toml` plus each package's own manifest, not a
/// projection of `clean.toml`. A project with no lockfile passes
/// `&ResolvedClosure::default()` — an unresolved `[dependencies]` entry is
/// never lowered as if it were resolved, because `resolved_from` would have no
/// honest value.
pub fn lower(
    manifest: &Manifest,
    contract: &HostContract,
    closure: &ResolvedClosure,
    sources: Vec<Source>,
    overrides: &[ConfigOverride],
) -> RequestDocument {
    let build_section = manifest.build.as_ref();

    RequestDocument {
        spec_version: SPEC_VERSION.to_string(),

        project: Project {
            name: manifest.project.name.clone(),
            version: manifest.project.version.clone(),
        },

        build: Build {
            // Validated present by `Manifest::validate`, so the fallback is
            // unreachable in practice; an empty string would fail loudly at
            // the compiler rather than silently building the wrong target.
            target: manifest.target().unwrap_or_default().to_string(),
            optimization: build_section.and_then(|b| b.optimization.clone()),
            // `[memory]` lowering arrives with the memory-tier work; M0
            // projects do not set it and the compiler defaults it.
            memory: None,
            strip: build_section.and_then(|b| b.strip),
            component_model: build_section.and_then(|b| b.component_model),
            memory64: build_section.and_then(|b| b.memory64),
        },

        folders: manifest.folders.clone(),

        // From `.cln/lock.toml`, never from `[dependencies]` in clean.toml:
        // that section carries constraints ("^3.1"), and the request document
        // needs resolved versions plus where each came from.
        dependencies: closure.dependencies.clone(),

        compile_limits: None,
        telemetry: None,

        // FRM-BO-16. The WIT goes across verbatim — no extraction of the
        // selected world, no reformatting. `world` is the selector the
        // compiler needs because a host.wit may declare several; resolving it
        // compiler-side is what BVER-03 forbids.
        //
        // `world` is validated present by `Manifest::validate`, so the fallback
        // is unreachable. An empty string would fail at the compiler with a
        // clear `RQD002` rather than silently checking against nothing.
        target_world: TargetWorld {
            host: contract.host.clone(),
            version: contract.version.clone(),
            world: manifest.world().unwrap_or_default().to_string(),
            sha256: contract.sha256.clone(),
            wit: contract.wit.clone(),
        },

        sources,

        // Already in dependency order, and already deduped, by the closure
        // walk. Re-sorting here would discard that ordering; it is meaningful
        // (a library follows what it builds on) and it is what keeps the
        // request byte-identical across lockfile rewrites.
        library_manifests: closure.libraries.clone(),

        overrides: overrides
            .iter()
            .map(|o| Override {
                path: o.path.clone(),
                value: o.value.clone(),
                source: o.source.as_str().to_string(),
            })
            .collect(),
    }
}

/// Collect `CLN_<SECTION>_<KEY>` environment overrides (CONF-01).
///
/// Only the variables M0 can honestly lower are read. An unknown `CLN_*`
/// variable is ignored rather than guessed at: emitting an `overrides[]` entry
/// with a path the compiler does not recognize would be an `RQD002` hard error
/// on a spelling mistake.
pub fn overrides_from_env<F>(mut get: F) -> Vec<ConfigOverride>
where
    F: FnMut(&str) -> Option<String>,
{
    const KNOWN: &[(&str, &str)] = &[
        ("CLN_BUILD_OPTIMIZATION", "build.optimization"),
        ("CLN_BUILD_TARGET", "build.target"),
        ("CLN_MEMORY_TIER", "build.memory.tier"),
    ];

    KNOWN
        .iter()
        .filter_map(|(var, path)| get(var).map(|value| ConfigOverride::env(*path, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn manifest_from(toml_text: &str) -> Manifest {
        toml::from_str(toml_text).unwrap()
    }

    fn hello_sources() -> Vec<Source> {
        vec![Source {
            path: "app/main.cln".into(),
            sha256: "abc".into(),
            content: "start()\n".into(),
        }]
    }

    const CLI_WIT: &str = "package clean:host@0.1.0;\nworld cli {}\n";

    fn contract() -> HostContract {
        HostContract {
            host: "clean-cli".into(),
            version: "0.1.0".into(),
            sha256: "9f2b1c".into(),
            wit: CLI_WIT.into(),
        }
    }

    #[test]
    fn lowers_project_and_build_verbatim() {
        let m = manifest_from(
            r#"
[project]
name = "hello-world"
version = "0.1.0"

[build]
target = "wasm32-cli"
optimization = "release"
strip = true
"#,
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[]);
        assert_eq!(doc.spec_version, "1");
        assert_eq!(doc.project.name, "hello-world");
        assert_eq!(doc.project.version, "0.1.0");
        assert_eq!(doc.build.target, "wasm32-cli");
        assert_eq!(doc.build.optimization.as_deref(), Some("release"));
        assert_eq!(doc.build.strip, Some(true));
    }

    #[test]
    fn absent_fields_are_omitted_not_defaulted() {
        // §11.4: "Fields not present in clean.toml are omitted from the request
        // document — the compiler applies its own defaults." The framework must
        // not become a second home for defaults (§11.9).
        let m = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-cli\"\n",
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[]);
        assert_eq!(doc.build.optimization, None);
        assert_eq!(doc.build.strip, None);
        assert_eq!(doc.build.memory64, None);
        assert_eq!(doc.build.component_model, None);
    }

    #[test]
    fn overrides_are_audited_not_merged_frm_bo_08() {
        let m = manifest_from(
            r#"
[project]
name = "x"
version = "0.1.0"

[build]
target = "wasm32-cli"
optimization = "release"
"#,
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[ConfigOverride::cli("build.optimization", "debug")]);

        // The lowered config still says what clean.toml says.
        assert_eq!(doc.build.optimization.as_deref(), Some("release"));
        // The override rides alongside for the compiler to apply and record.
        assert_eq!(doc.overrides.len(), 1);
        assert_eq!(doc.overrides[0].path, "build.optimization");
        assert_eq!(doc.overrides[0].value, "debug");
        assert_eq!(doc.overrides[0].source, "cli");
    }

    #[test]
    fn unresolved_dependencies_are_not_lowered() {
        // `[dependencies]` in clean.toml carries *constraints*. Lowering one
        // without the lockfile would force us to invent `resolved_from` and to
        // pass "^3.1" where the compiler expects a resolved version.
        let m = manifest_from(
            r#"
[project]
name = "x"
version = "0.1.0"

[build]
target = "wasm32-server"

[dependencies]
data = "^3.1"
"#,
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[]);
        assert!(doc.dependencies.is_empty());
    }

    #[test]
    fn a_resolved_closure_reaches_the_request_document() {
        use framework_compiler_driver::request::{Dependency, LibraryManifest};

        let m = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n\
             [build]\ntarget = \"wasm32-server\"\n[dependencies]\ndata = \"^3.1\"\n",
        );

        let mut closure = ResolvedClosure::default();
        closure.dependencies.insert(
            "frame.data".into(),
            Dependency { version: "2.1.2".into(), resolved_from: "path".into() },
        );
        closure.libraries.push(LibraryManifest {
            name: "frame.data".into(),
            version: "2.1.2".into(),
            wit: "interface data {}".into(),
            handles_blocks: vec!["table".into()],
            compiletime_wasm_sha256: None,
        });

        let doc = lower(&m, &contract(), &closure, hello_sources(), &[]);

        // The resolved version wins over the "^3.1" constraint in clean.toml.
        assert_eq!(doc.dependencies["frame.data"].version, "2.1.2");
        assert_eq!(doc.dependencies["frame.data"].resolved_from, "path");
        assert_eq!(doc.library_manifests.len(), 1);
        assert_eq!(doc.library_manifests[0].handles_blocks, vec!["table"]);
    }

    #[test]
    fn library_order_survives_lowering() {
        use framework_compiler_driver::request::LibraryManifest;

        // The closure walk put these in dependency order. Lowering must not
        // re-sort them: `base` is a dependency of `app` and must precede it.
        let entry = |name: &str| LibraryManifest {
            name: name.into(),
            version: "1.0.0".into(),
            wit: String::new(),
            handles_blocks: Vec::new(),
            compiletime_wasm_sha256: None,
        };

        let m = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-cli\"\n",
        );
        let closure = ResolvedClosure {
            libraries: vec![entry("base"), entry("app")],
            ..Default::default()
        };

        let doc = lower(&m, &contract(), &closure, hello_sources(), &[]);
        let names: Vec<&str> = doc.library_manifests.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["base", "app"], "dependency order must survive");
    }

    #[test]
    fn env_overrides_are_collected_by_known_name_only() {
        let overrides = overrides_from_env(|var| match var {
            "CLN_BUILD_OPTIMIZATION" => Some("debug".into()),
            "CLN_NOT_A_REAL_KEY" => Some("boom".into()),
            _ => None,
        });
        assert_eq!(overrides, vec![ConfigOverride::env("build.optimization", "debug")]);
    }

    #[test]
    fn target_world_carries_the_contract_verbatim_with_the_selected_world() {
        // FRM-BO-16. The three things that must be true: the WIT is unmodified,
        // the world comes from build.target's mapping, and the version/hash are
        // the contract's resolved values rather than anything from clean.toml.
        let m = manifest_from(
            r#"
[project]
name = "x"
version = "0.1.0"

[build]
target = "wasm32-cli"

[target]
host = "clean-cli"
version = "0.1.x"
"#,
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[]);

        assert_eq!(doc.target_world.wit, CLI_WIT, "the WIT must not be rewritten");
        assert_eq!(doc.target_world.world, "cli");
        assert_eq!(doc.target_world.host, "clean-cli");
        assert_eq!(doc.target_world.sha256, "9f2b1c");
        // The manifest says "0.1.x"; the request must carry the resolved
        // version, or two identical requests could mean different hosts.
        assert_eq!(doc.target_world.version, "0.1.0");
    }

    #[test]
    fn the_world_follows_build_target_not_the_host_name() {
        // Same host contract, different build target, different world. This is
        // the mapping the compiler is forbidden from doing itself (BVER-03).
        let server = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-server\"\n[target]\nhost = \"clean-server\"\nversion = \"0.6.0\"\n",
        );
        assert_eq!(lower(&server, &contract(), &ResolvedClosure::default(), hello_sources(), &[]).target_world.world, "server");

        let browser = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm32-browser\"\n[target]\nhost = \"clean-browser\"\nversion = \"0.1.0\"\n",
        );
        assert_eq!(lower(&browser, &contract(), &ResolvedClosure::default(), hello_sources(), &[]).target_world.world, "browser");

        // wasm64-server shares the server world with wasm32-server.
        let w64 = manifest_from(
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\n[build]\ntarget = \"wasm64-server\"\n[target]\nhost = \"clean-server\"\nversion = \"0.6.0\"\n",
        );
        assert_eq!(lower(&w64, &contract(), &ResolvedClosure::default(), hello_sources(), &[]).target_world.world, "server");
    }

    #[test]
    fn folders_are_carried_through() {
        let m = manifest_from(
            r#"
[project]
name = "x"
version = "0.1.0"

[build]
target = "wasm32-server"

[folders]
"app/server/**" = ["server"]
"#,
        );
        let doc = lower(&m, &contract(), &ResolvedClosure::default(), hello_sources(), &[]);
        assert_eq!(doc.folders.get("app/server/**").unwrap(), &vec!["server".to_string()]);
    }
}

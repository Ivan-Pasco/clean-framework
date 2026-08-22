//! Project-backed answers (Framework §10.2.2).
//!
//! These read the same on-disk state the build path reads — `clean.toml`, the
//! host WIT cache, `.cln/lock.toml` — because §10.6 is explicit that the MCP
//! is "a query surface over shared state, not a keeper of its own." There is
//! no MCP cache and no MCP configuration; if an answer here disagrees with
//! what `cln build` does, the answer is wrong.
//!
//! # Rule 2, and why answers look over-annotated
//!
//! §10.7 Rule 2 requires every host-capability answer to cite the host name,
//! version, and where the WIT came from, so an assistant can show a developer
//! whether its guidance is fresh. That is why these responses carry more
//! envelope than the bare fact — the citation is the point, not decoration.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use framework_core::{closure, hostwit, Manifest};

use crate::protocol::ToolOutcome;

/// A selected project: a directory containing `clean.toml`.
#[derive(Clone, Debug)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    /// Find the project containing `start`, walking upward.
    ///
    /// §10.9 rule 4: the project is chosen from the working directory, and
    /// there is no cross-project auto-switching mid-session. Walking up means
    /// a session started in `src/` still finds the project — the alternative
    /// would report "no project" from inside one.
    pub fn discover(start: &Path) -> Option<Project> {
        let mut current = Some(start);
        while let Some(directory) = current {
            if directory.join("clean.toml").is_file() {
                return Some(Project { root: directory.to_path_buf() });
            }
            current = directory.parent();
        }
        None
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn manifest(&self) -> Result<Manifest, String> {
        Manifest::load(&self.root).map_err(|e| e.to_string())
    }

    /// `get_target_host` — which host this project builds against.
    pub fn target_host(&self) -> ToolOutcome {
        let manifest = match self.manifest() {
            Ok(manifest) => manifest,
            Err(error) => return manifest_unreadable(&error),
        };

        let Some(target) = manifest.target_section() else {
            return ToolOutcome::failed(json!({
                "error": "clean.toml declares no [target] block",
                "remedy": "add [target] with `host` and `version`",
            }));
        };

        // The pinned hash, when the project has been built. Its absence is
        // meaningful rather than an error: it says this project has not
        // resolved a contract yet.
        let pinned = hostwit::pinned_hash(&self.root, &target.host)
            .ok()
            .flatten();

        ToolOutcome::ok(json!({
            "host": target.host,
            "version_constraint": target.version,
            "resolved_version": pinned.as_ref().map(|_| &target.version),
            "build_target": manifest.target(),
            "world": manifest.world(),
            "contract_pinned": pinned.is_some(),
            "wit_sha256": pinned,
        }))
    }

    /// `get_host_capabilities` — what the target host provides.
    ///
    /// Read from the cached `host.wit`, and cited per §10.7 Rule 2. The WIT is
    /// summarized into interfaces and function names rather than returned
    /// whole: a 296-line contract is not a useful tool response, and the
    /// summary is what an assistant actually reasons over.
    pub fn host_capabilities(&self) -> ToolOutcome {
        let manifest = match self.manifest() {
            Ok(manifest) => manifest,
            Err(error) => return manifest_unreadable(&error),
        };

        let Some(target) = manifest.target_section() else {
            return ToolOutcome::failed(json!({
                "error": "clean.toml declares no [target] block",
            }));
        };

        let cache = match hostwit::HostWitCache::user() {
            Ok(cache) => cache,
            Err(error) => {
                return ToolOutcome::failed(json!({
                    "error": format!("no host WIT cache: {error}"),
                }))
            }
        };

        let wit = match cache.get(&target.host, &target.version) {
            Ok(Some(wit)) => wit,
            Ok(None) => {
                return ToolOutcome::failed(json!({
                    "error": format!(
                        "no cached contract for {}@{}",
                        target.host, target.version
                    ),
                    "remedy": "run `cln build` once to fetch and pin the host contract",
                    "looked_in": cache.path_for(&target.host, &target.version).display().to_string(),
                }))
            }
            Err(error) => {
                return ToolOutcome::failed(json!({ "error": error.to_string() }))
            }
        };

        let world = manifest.world();
        ToolOutcome::ok(json!({
            // §10.7 Rule 2 — the citation, without which an assistant cannot
            // tell a developer how fresh this is.
            "host": target.host,
            "version": target.version,
            "wit_source": cache.path_for(&target.host, &target.version).display().to_string(),
            "world": world,
            "declares_world": world.map(|w| hostwit::declares_world(&wit, w)),
            "interfaces": summarize_wit(&wit),
        }))
    }

    /// `list_libraries` — the resolved closure, as the build sees it.
    pub fn libraries(&self) -> ToolOutcome {
        match closure::resolve(&self.root) {
            Ok(Some(closure)) => {
                let libraries: Vec<Value> = closure
                    .libraries
                    .iter()
                    .map(|library| {
                        json!({
                            "name": library.name,
                            "version": library.version,
                            "handles_blocks": library.handles_blocks,
                            // A library declaring host imports has synthesized
                            // WIT; one that does not is pure Clean.
                            "declares_host_bridge": !library.wit.is_empty(),
                        })
                    })
                    .collect();

                let plugins: Vec<Value> = closure
                    .plugins
                    .iter()
                    .map(|plugin| {
                        json!({
                            "name": plugin.name(),
                            "version": plugin.manifest.plugin.version,
                        })
                    })
                    .collect();

                ToolOutcome::ok(json!({
                    "libraries": libraries,
                    "plugins": plugins,
                    "resolved": true,
                }))
            }
            // No lockfile: the project has not resolved. Distinct from having
            // resolved to nothing, and the remedy differs.
            Ok(None) => ToolOutcome::ok(json!({
                "libraries": [],
                "plugins": [],
                "resolved": false,
                "detail": "no .cln/lock.toml; the project has not been resolved yet",
                "remedy": "run `cln fetch`",
            })),
            Err(error) => ToolOutcome::failed(json!({ "error": error.to_string() })),
        }
    }

    /// `get_app_structure` — the folder layout, per the manifest.
    pub fn app_structure(&self) -> ToolOutcome {
        let manifest = match self.manifest() {
            Ok(manifest) => manifest,
            Err(error) => return manifest_unreadable(&error),
        };

        let folders: Vec<Value> = manifest
            .folders
            .iter()
            .map(|(folder, libraries)| json!({ "folder": folder, "libraries": libraries }))
            .collect();

        ToolOutcome::ok(json!({
            "root": self.root.display().to_string(),
            "entry": manifest.entry().display().to_string(),
            "folders": folders,
            "excludes": manifest.excludes(),
        }))
    }
}

fn manifest_unreadable(error: &str) -> ToolOutcome {
    ToolOutcome::failed(json!({
        "error": format!("could not read clean.toml: {error}"),
    }))
}

/// Summarize a `host.wit` into interfaces and the functions they declare.
///
/// A deliberately shallow read of the same shape `hostwit::declares_world`
/// takes: track `interface <name> {` and collect `<name>: func(...)` lines
/// inside it. It is not a WIT parser and does not need to be — this feeds an
/// assistant's sense of what exists, and the authority on whether a call is
/// legal is the compiler's World Import Check, not this summary.
fn summarize_wit(wit: &str) -> Vec<Value> {
    let mut interfaces = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    let mut depth = 0usize;

    for raw in wit.lines() {
        let line = match raw.find("//") {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();

        if current.is_none() {
            if let Some(rest) = line.strip_prefix("interface ") {
                if let Some(name) = rest.split_whitespace().next() {
                    current = Some((name.to_string(), Vec::new()));
                    depth = line.matches('{').count();
                    continue;
                }
            }
            continue;
        }

        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());

        if let Some((name, functions)) = current.take() {
            if depth == 0 {
                interfaces.push(json!({ "interface": name, "functions": functions }));
                continue;
            }

            let mut functions = functions;
            // `write: func(path: string) -> s64;`
            if let Some((left, right)) = line.split_once(':') {
                if right.trim_start().starts_with("func") {
                    let function = left.trim();
                    if !function.is_empty() && !function.contains(' ') {
                        functions.push(function.to_string());
                    }
                }
            }
            current = Some((name, functions));
        }
    }

    interfaces
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_WIT: &str = r#"
package clean:host@0.1.0;

/// Route registration.
interface routing {
    enum method { get, post }
    register: func(m: method, path: string, handler-id: u32);
}

interface response {
    set-status: func(status: u16);
    add-header: func(name: string, value: string);
}

world server {
    export routing;
}
"#;

    fn project_at(dir: &Path, body: &str) -> Project {
        std::fs::write(dir.join("clean.toml"), body).unwrap();
        Project::discover(dir).expect("a directory with clean.toml is a project")
    }

    const MANIFEST: &str = r#"
[project]
name = "demo"
version = "0.1.0"

[build]
target = "wasm32-server"

[target]
host = "clean-server"
version = "0.7.0"
"#;

    #[test]
    fn discovery_walks_upward_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.toml"), MANIFEST).unwrap();
        let nested = dir.path().join("app/server");
        std::fs::create_dir_all(&nested).unwrap();

        let project = Project::discover(&nested).expect("must find the project above");
        assert_eq!(project.root(), dir.path());
    }

    #[test]
    fn discovery_outside_any_project_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Project::discover(dir.path()).is_none());
    }

    #[test]
    fn target_host_reports_the_host_and_world() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_at(dir.path(), MANIFEST);

        let outcome = project.target_host();
        assert!(!outcome.is_error, "{:?}", outcome.value);
        assert_eq!(outcome.value["host"], "clean-server");
        assert_eq!(outcome.value["world"], "server");
        assert_eq!(outcome.value["build_target"], "wasm32-server");
        // Never built, so no contract is pinned — and that is reported, not
        // hidden behind an error.
        assert_eq!(outcome.value["contract_pinned"], false);
    }

    #[test]
    fn app_structure_reports_the_entry_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_at(dir.path(), MANIFEST);

        let outcome = project.app_structure();
        assert!(!outcome.is_error, "{:?}", outcome.value);
        assert!(outcome.value["entry"].as_str().unwrap().ends_with("main.cln"));
    }

    #[test]
    fn an_unresolved_project_says_so_rather_than_erroring() {
        // No lockfile is a normal state for a fresh checkout. Reporting it as
        // a failure would make `list_libraries` look broken on every new
        // project.
        let dir = tempfile::tempdir().unwrap();
        let project = project_at(dir.path(), MANIFEST);

        let outcome = project.libraries();
        assert!(!outcome.is_error, "{:?}", outcome.value);
        assert_eq!(outcome.value["resolved"], false);
        assert_eq!(outcome.value["libraries"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn host_capabilities_without_a_cached_contract_names_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let project = project_at(
            dir.path(),
            // A real host — `Manifest::validate` refuses an unknown one
            // before this code runs — at a version nothing has cached.
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n\
             [build]\ntarget = \"wasm32-server\"\n\n\
             [target]\nhost = \"clean-server\"\nversion = \"99.99.99\"\n",
        );

        let outcome = project.host_capabilities();
        assert!(outcome.is_error);
        assert!(outcome.value["looked_in"].is_string(), "{:?}", outcome.value);
    }

    #[test]
    fn the_wit_summary_finds_interfaces_and_their_functions() {
        let interfaces = summarize_wit(SERVER_WIT);
        assert_eq!(interfaces.len(), 2, "{interfaces:?}");

        assert_eq!(interfaces[0]["interface"], "routing");
        let routing: Vec<&str> = interfaces[0]["functions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap())
            .collect();
        assert_eq!(routing, ["register"], "enum members must not count as functions");

        assert_eq!(interfaces[1]["interface"], "response");
        assert_eq!(interfaces[1]["functions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_wit_summary_ignores_commented_out_declarations() {
        let wit = "interface real {\n  a: func();\n}\n// interface fake {\n";
        let interfaces = summarize_wit(wit);
        assert_eq!(interfaces.len(), 1);
        assert_eq!(interfaces[0]["interface"], "real");
    }
}

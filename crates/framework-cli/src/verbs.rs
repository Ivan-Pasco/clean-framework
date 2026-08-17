//! argv → `framework-core` dispatch.
//!
//! **Output contract with Manager.** Human-readable progress goes to stderr;
//! stdout carries a single machine-readable JSON envelope. Manager parses that
//! envelope and renders the diagnostics with the same renderer it uses for the
//! compiler's own, so there is one diagnostic renderer in the toolchain rather
//! than two (PLAN.md §3).
//!
//! Exit codes: `0` success, `1` the build failed (diagnostics explain why),
//! `2` the framework was invoked wrongly.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use framework_compiler_driver::{Compiler, SubprocessCompiler};
use framework_core::{build, BuildInputs, ConfigOverride, FrameworkError, FRAMEWORK_VERSION};
use framework_scaffold::{scaffold, Template};

#[derive(Parser, Debug)]
#[command(
    name = "clean-framework",
    version = FRAMEWORK_VERSION,
    about = "Clean Framework build orchestrator (invoked by cln, not directly)"
)]
struct Cli {
    #[command(subcommand)]
    command: Verb,
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// Build a project into dist/app.wasm.
    Build(BuildArgs),

    /// Wrap the built component into a distributable .clapp archive.
    ///
    /// Builds first when dist/ is missing or stale, so a caller never has to
    /// sequence the two (FRM-BO-09a).
    Package(BuildArgs),

    /// Report diagnostics without writing dist/.
    ///
    /// Answers "does this compile?" without disturbing a dist/ that a running
    /// dev server or a previous release may be using.
    Check(BuildArgs),

    /// Inspect or clear the build cache (§11.7).
    Cache(CacheArgs),

    /// Scaffold a new project.
    New(NewArgs),
}

#[derive(Parser, Debug)]
struct NewArgs {
    /// Directory to create. Its name becomes the project name unless --name
    /// is given.
    path: PathBuf,

    /// Project name, when it should differ from the directory name.
    #[arg(long)]
    name: Option<String>,

    /// What to generate.
    #[arg(long, default_value = "app", value_parser = clap::builder::PossibleValuesParser::new(Template::ALL))]
    template: String,
}

#[derive(Parser, Debug)]
struct CacheArgs {
    #[command(subcommand)]
    action: CacheAction,
}

#[derive(Subcommand, Debug)]
enum CacheAction {
    /// Report where the cache lives, how many builds it holds, and its size.
    Status(CacheScope),
    /// Remove every cached build.
    Clear(CacheScope),
}

/// Which cache to act on. Attached to each action rather than to `cache`
/// itself so `cln cache status --build-cache <dir>` parses — clap only accepts
/// a parent's options *before* the subcommand, and nobody writes them there.
#[derive(Parser, Debug)]
struct CacheScope {
    /// Build-cache directory, overriding `~/.cln/build-cache/`.
    /// For tests — Manager never passes this.
    #[arg(long, hide = true)]
    build_cache: Option<PathBuf>,
}

impl CacheAction {
    fn scope(&self) -> &CacheScope {
        match self {
            CacheAction::Status(scope) | CacheAction::Clear(scope) => scope,
        }
    }
}

#[derive(Parser, Debug)]
struct BuildArgs {
    /// Project root. Defaults to the current directory.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Audited config override, `section.key=value` (CONF-01, FRM-BO-08).
    #[arg(long = "override", value_name = "PATH=VALUE")]
    overrides: Vec<String>,

    /// Shorthand for `--override build.optimization=<value>`.
    #[arg(long)]
    optimization: Option<String>,

    /// Refuse network operations; satisfy the host contract from the local
    /// cache or fail (C-18, CLI §6).
    #[arg(long)]
    offline: bool,

    /// Path to the compiler binary, bypassing the `.cln/version` pin.
    /// For tests and toolchain development — Manager never passes this.
    #[arg(long, hide = true)]
    compiler: Option<PathBuf>,

    /// Host-contract cache directory, overriding `~/.cln/host-wit/`.
    /// For tests — Manager never passes this.
    #[arg(long, hide = true)]
    host_wit_cache: Option<PathBuf>,

    /// Compile every time and store nothing, ignoring the build cache (§11.7).
    #[arg(long)]
    no_cache: bool,

    /// Build-cache directory, overriding `~/.cln/build-cache/`.
    /// For tests — Manager never passes this.
    #[arg(long, hide = true)]
    build_cache: Option<PathBuf>,
}

pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap writes its own message; --help and --version are not errors.
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion => ExitCode::SUCCESS,
                _ => ExitCode::from(2),
            };
        }
    };

    match cli.command {
        Verb::Build(args) => run_build(args),
        Verb::Package(args) => run_package(args),
        Verb::Check(args) => run_check(args),
        Verb::Cache(args) => run_cache(args),
        Verb::New(args) => run_new(args),
    }
}

fn run_new(args: NewArgs) -> ExitCode {
    // Validated by clap against `Template::ALL`, so an unknown value never
    // reaches here — but an `expect` would turn a future flag-parsing change
    // into a panic rather than a message.
    let Some(template) = Template::parse(&args.template) else {
        eprintln!("error: unknown template '{}'", args.template);
        return ExitCode::from(2);
    };

    match scaffold(&args.path, template, args.name.as_deref()) {
        Ok(outcome) => {
            eprintln!(
                "created {} ({}): {}",
                outcome.root.display(),
                outcome.template.as_str(),
                outcome.files.join(", ")
            );
            let envelope = serde_json::json!({
                "status": "ok",
                "root": outcome.root,
                "template": outcome.template.as_str(),
                "files": outcome.files,
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            let envelope = serde_json::json!({
                "status": "error",
                "diagnostics": [{
                    "code": e.code(),
                    "message": e.to_string(),
                    "helps": e.help().into_iter().collect::<Vec<_>>(),
                }],
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::from(1)
        }
    }
}

fn run_check(args: BuildArgs) -> ExitCode {
    let (inputs, compiler) = match prepare(&args) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    match framework_core::check(&inputs, &compiler) {
        Ok(outcome) => {
            let warnings = outcome.diagnostics.len();
            eprintln!(
                "checked: no errors{}{}",
                if warnings > 0 { format!(", {warnings} warning(s)") } else { String::new() },
                if outcome.cached { " (cached)" } else { "" }
            );
            let envelope = serde_json::json!({
                "status": "ok",
                "request_sha256": outcome.request_sha256,
                "diagnostics": outcome.diagnostics,
                "cached": outcome.cached,
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::SUCCESS
        }
        Err(e) => report_failure(&e),
    }
}

fn run_cache(args: CacheArgs) -> ExitCode {
    let cache = match &args.action.scope().build_cache {
        Some(dir) => framework_core::BuildCache::at(dir),
        None => match framework_core::BuildCache::user() {
            Ok(cache) => cache,
            Err(e) => {
                eprintln!("error: {e}");
                let envelope = serde_json::json!({
                    "status": "error",
                    "diagnostics": [{
                        "code": e.code(),
                        "message": e.to_string(),
                        "helps": e.help().into_iter().collect::<Vec<_>>(),
                    }],
                    "framework_version": FRAMEWORK_VERSION,
                });
                println!("{envelope}");
                return ExitCode::from(1);
            }
        },
    };

    let result = match args.action {
        CacheAction::Status(_) => cache.keys().and_then(|keys| {
            cache.size_bytes().map(|bytes| {
                eprintln!(
                    "{} cached build(s), {:.1} MB at {}",
                    keys.len(),
                    bytes as f64 / 1_048_576.0,
                    cache.root().display()
                );
                serde_json::json!({
                    "status": "ok",
                    "action": "status",
                    "root": cache.root(),
                    "entries": keys.len(),
                    "bytes": bytes,
                    "framework_version": FRAMEWORK_VERSION,
                })
            })
        }),

        CacheAction::Clear(_) => {
            // Counted before clearing: afterwards there is nothing to count,
            // and "removed 0" would be indistinguishable from a no-op failure.
            let before = cache.keys().map(|keys| keys.len());
            before.and_then(|entries| {
                cache.clear().map(|()| {
                    eprintln!("cleared {entries} cached build(s)");
                    serde_json::json!({
                        "status": "ok",
                        "action": "clear",
                        "root": cache.root(),
                        "removed": entries,
                        "framework_version": FRAMEWORK_VERSION,
                    })
                })
            })
        }
    };

    match result {
        Ok(envelope) => {
            println!("{envelope}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            let envelope = serde_json::json!({
                "status": "error",
                "diagnostics": [{
                    "code": e.code(),
                    "message": e.to_string(),
                    "helps": e.help().into_iter().collect::<Vec<_>>(),
                }],
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::from(1)
        }
    }
}

fn run_package(args: BuildArgs) -> ExitCode {
    let (inputs, compiler) = match prepare(&args) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    match framework_core::package(&inputs, &compiler) {
        Ok(outcome) => {
            if outcome.rebuilt {
                eprintln!("built (dist was stale)");
            }
            eprintln!("packaged {}", outcome.path.display());
            let envelope = serde_json::json!({
                "status": "ok",
                "package": outcome.path,
                "package_sha256": outcome.sha256,
                "kind": outcome.kind.as_str(),
                "rebuilt": outcome.rebuilt,
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::SUCCESS
        }
        Err(e) => report_failure(&e),
    }
}

/// Shared setup for the verbs that need a project and a compiler.
///
/// Returns the exit code directly on failure because the two failure modes
/// differ: a bad flag is exit 2 (invoked wrongly), everything else goes
/// through the diagnostic envelope.
fn prepare(args: &BuildArgs) -> Result<(BuildInputs, SubprocessCompiler), ExitCode> {
    let overrides = match collect_overrides(args) {
        Ok(overrides) => overrides,
        Err(message) => {
            eprintln!("error: {message}");
            return Err(ExitCode::from(2));
        }
    };

    let mut inputs = BuildInputs::new(&args.path)
        .with_overrides(overrides)
        .offline(args.offline);
    // `--no-cache` wins over an explicit directory: the flag says "do not
    // reuse anything", and honouring a path alongside it would do the opposite.
    if args.no_cache {
        inputs = inputs.without_cache();
    } else if let Some(cache) = &args.build_cache {
        inputs = inputs.with_build_cache(framework_core::BuildCache::at(cache));
    }

    if let Some(cache) = &args.host_wit_cache {
        inputs = inputs.with_host_wit_cache(framework_core::HostWitCache::at(cache));
    }

    match resolve_compiler(args, &inputs) {
        Ok(compiler) => Ok((inputs, compiler)),
        Err(e) => Err(report_failure(&e)),
    }
}

fn run_build(args: BuildArgs) -> ExitCode {
    let (inputs, compiler) = match prepare(&args) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };

    match build(&inputs, &compiler) {
        Ok(outcome) => {
            eprintln!("built {}", outcome.dist_wasm.display());
            let envelope = serde_json::json!({
                "status": "ok",
                "dist_wasm": outcome.dist_wasm,
                "build_manifest": outcome.build_manifest_path,
                "request_sha256": outcome.request_sha256,
                "wasm_sha256": outcome.wasm_sha256,
                "diagnostics": outcome.diagnostics,
                "framework_version": FRAMEWORK_VERSION,
            });
            println!("{envelope}");
            ExitCode::SUCCESS
        }
        Err(e) => report_failure(&e),
    }
}

fn resolve_compiler(
    args: &BuildArgs,
    inputs: &BuildInputs,
) -> Result<SubprocessCompiler, FrameworkError> {
    let compiler = match &args.compiler {
        // An explicit path skips the pin; ask the binary what it is, since
        // there is no folder name to read a version from.
        Some(path) => {
            let probe = SubprocessCompiler::at(path, semver::Version::new(0, 0, 0));
            let reported = probe.version()?;
            let version = reported.parse().unwrap_or_else(|_| semver::Version::new(0, 0, 0));
            SubprocessCompiler::at(path, version)
        }
        None => SubprocessCompiler::for_project(&inputs.project_root)?,
    };
    Ok(compiler)
}

fn collect_overrides(args: &BuildArgs) -> Result<Vec<ConfigOverride>, String> {
    let mut overrides = framework_core::lower::overrides_from_env(|var| std::env::var(var).ok());

    for raw in &args.overrides {
        let (path, value) = raw
            .split_once('=')
            .ok_or_else(|| format!("--override expects PATH=VALUE, got '{raw}'"))?;
        if path.trim().is_empty() {
            return Err(format!("--override has an empty path: '{raw}'"));
        }
        overrides.push(ConfigOverride::cli(path.trim(), value));
    }

    if let Some(optimization) = &args.optimization {
        overrides.push(ConfigOverride::cli("build.optimization", optimization));
    }

    Ok(overrides)
}

/// Emit the failure envelope. Diagnostics carry the real message — the
/// framework's own error text is only a fallback for failures that produced
/// none.
fn report_failure(error: &FrameworkError) -> ExitCode {
    let diagnostics = error.to_diagnostics();

    for diagnostic in &diagnostics {
        eprintln!("error[{}]: {}", diagnostic.code, diagnostic.message);
        for note in &diagnostic.notes {
            eprintln!("  note: {note}");
        }
        for help in &diagnostic.helps {
            eprintln!("  help: {help}");
        }
    }

    let envelope = serde_json::json!({
        "status": "error",
        "diagnostics": diagnostics,
        "framework_version": FRAMEWORK_VERSION,
    });
    println!("{envelope}");
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> BuildArgs {
        // build, package and check take the same arguments, so any of the
        // three parses here. `cache` does not — it has its own shape.
        match Cli::try_parse_from(args).unwrap().command {
            Verb::Build(a) | Verb::Package(a) | Verb::Check(a) => a,
            Verb::Cache(_) | Verb::New(_) => panic!("this verb does not take BuildArgs"),
        }
    }

    #[test]
    fn build_defaults_to_the_current_directory() {
        let args = parse(&["clean-framework", "build"]);
        assert_eq!(args.path, PathBuf::from("."));
    }

    #[test]
    fn override_flag_parses_into_an_audited_entry() {
        let args = parse(&["clean-framework", "build", "--override", "build.optimization=debug"]);
        let overrides = collect_overrides(&args).unwrap();
        let entry = overrides.iter().find(|o| o.path == "build.optimization").unwrap();
        assert_eq!(entry.value, "debug");
        assert_eq!(entry.source, framework_core::OverrideSource::Cli);
    }

    #[test]
    fn override_without_equals_is_a_usage_error() {
        let args = parse(&["clean-framework", "build", "--override", "nonsense"]);
        assert!(collect_overrides(&args).is_err());
    }

    #[test]
    fn optimization_shorthand_becomes_an_override() {
        let args = parse(&["clean-framework", "build", "--optimization", "size"]);
        let overrides = collect_overrides(&args).unwrap();
        assert!(overrides
            .iter()
            .any(|o| o.path == "build.optimization" && o.value == "size"));
    }

    #[test]
    fn unknown_verb_exits_two() {
        assert_eq!(run(["clean-framework", "teleport"]), ExitCode::from(2));
    }
}

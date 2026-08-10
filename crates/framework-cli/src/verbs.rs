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

    /// Path to the compiler binary, bypassing the `.cln/version` pin.
    /// For tests and toolchain development — Manager never passes this.
    #[arg(long, hide = true)]
    compiler: Option<PathBuf>,
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
    }
}

fn run_build(args: BuildArgs) -> ExitCode {
    let overrides = match collect_overrides(&args) {
        Ok(overrides) => overrides,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };

    let inputs = BuildInputs::new(&args.path).with_overrides(overrides);

    let compiler = match resolve_compiler(&args, &inputs) {
        Ok(compiler) => compiler,
        Err(e) => return report_failure(&e),
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
        match Cli::try_parse_from(args).unwrap().command {
            Verb::Build(a) => a,
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

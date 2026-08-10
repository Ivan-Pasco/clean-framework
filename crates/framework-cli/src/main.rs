//! `clean-framework` — the binary Manager dispatches to (Manager §00.4).
//!
//! This is an argv adapter and nothing else. Every decision lives in
//! `framework-core`; this file translates flags in and diagnostics out. Keeping
//! it thin is what lets Manager call the framework as a library later without
//! carrying the argv parser (PLAN.md §3).
//!
//! The framework has no user-facing CLI of its own (Governance §2.4) — users
//! type `cln build`, Manager routes it here.

mod verbs;

use std::process::ExitCode;

fn main() -> ExitCode {
    verbs::run(std::env::args_os())
}

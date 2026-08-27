//! The mobile world, end to end through the framework's own front door.
//!
//! These assert the seams that would otherwise fail far from their cause: a
//! contract whose world the target does not name fails inside the compiler,
//! and an unregistered host fails at fetch with a network-shaped error. Both
//! are cheap to check here and expensive to diagnose there.

use std::path::Path;

fn read(rel: &str) -> String {
    // Tests run with CWD at the crate root; the contracts live at the repo root.
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn the_mobile_target_resolves_to_the_world_the_contract_declares() {
    let world = framework_core::world::for_target("wasm32-mobile")
        .expect("wasm32-mobile is a built-in target (Platform 07 §7.2)");
    assert_eq!(world.name, "mobile");
    assert_eq!(world.guest_world, "mobile", "no cli-style remap for mobile");

    assert!(
        framework_core::hostwit::declares_world(&read("hosts/clean-mobile-surface/host.wit"), world.name),
        "clean-mobile-surface must declare the world wasm32-mobile maps to, \
         or every mobile build fails our Moment 1 check"
    );
}

#[test]
fn clean_mobile_surface_is_an_official_host() {
    // Officialness is what lets `clean.toml` name the host without a
    // `wit_source` (§16.5).
    assert!(framework_core::hostwit::is_official("clean-mobile-surface"));
}

#[test]
fn the_browser_contract_exists_and_is_empty() {
    let wit = read("hosts/clean-browser/host.wit");
    assert!(
        framework_core::hostwit::declares_world(&wit, "browser"),
        "the file must declare `world browser`, so a browser build gets a \
         contract rather than a fetch failure"
    );

    // Emptiness is the assertion, not an accident. No browser host has
    // shipped, so under HCV-06 there is no interface this file may declare;
    // the first `interface` added here is a claim that a host registers it.
    assert!(
        !wit.lines().map(str::trim_start).any(|l| l.starts_with("interface ")),
        "world browser must stay empty until a browser host registers something"
    );
}

/// Names of every `interface X { ... }` declared at the top level of a
/// contract, in declaration order.
///
/// Deliberately as shallow as `declares_world`, and for the same reason: this
/// is a drift alarm, not a WIT compiler. Comment text is stripped first so a
/// prose mention of `interface foo` in a banner cannot register as one.
fn declared_interfaces(wit: &str) -> Vec<String> {
    wit.lines()
        .map(|l| l.split("//").next().unwrap_or("").trim())
        .filter_map(|l| l.strip_prefix("interface "))
        .filter_map(|rest| rest.split('{').next())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn mobile_carries_every_interface_the_browser_world_declares() {
    // THE DRIFT ALARM. clean-mobile-surface provides "the browser contract
    // plus devices", but it cannot `include` clean-browser: a contract is
    // fetched as ONE file (`resolve_contract`), hashed and pinned by BVER-03,
    // so only mobile's own file ever reaches the compiler. The browser half is
    // therefore copied, and a copy with nothing watching it diverges.
    //
    // What divergence would cost: a guest written against clean-browser stops
    // building for mobile, or worse builds against a mobile contract that
    // silently lacks an interface the browser one has — the exact
    // "portable between browser and mobile unchanged" promise this host is
    // built on, broken quietly.
    //
    // Both worlds are empty today, so this passes trivially. It is written now
    // because the moment it stops being trivial is the moment someone adds an
    // interface to clean-browser without knowing this file exists.
    let browser = declared_interfaces(&read("hosts/clean-browser/host.wit"));
    let mobile = declared_interfaces(&read("hosts/clean-mobile-surface/host.wit"));

    let missing: Vec<_> = browser.iter().filter(|i| !mobile.contains(i)).collect();
    assert!(
        missing.is_empty(),
        "clean-browser declares {missing:?}, which clean-mobile-surface does not carry.\n\
         Mobile provides the browser surface plus devices, and cannot `include` it \n\
         (one file per contract — see the composition note in mobile's banner).\n\
         Copy each interface above into hosts/clean-mobile-surface/host.wit verbatim, \n\
         and add it to `world mobile`'s exports."
    );
}

#[test]
fn mobile_stays_self_contained() {
    // The tripwire for someone "fixing" the duplication with the obvious
    // keyword. `include` is valid WIT and would even parse if `world browser`
    // were inlined here — but it resolves nothing across files, because
    // clean-browser's file is never fetched alongside this one. A dangling
    // include fails inside the compiler, far from the file that caused it.
    let wit = read("hosts/clean-mobile-surface/host.wit");
    let code: String = wit
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("include "),
        "mobile must stay self-contained; see the composition note in its banner"
    );
}

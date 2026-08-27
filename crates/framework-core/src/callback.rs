//! The callback half of a host contract, and whether the host declares it.
//!
//! # The gap this closes
//!
//! A device API has two halves and two owners. The outbound half — "guest asks
//! the host to take a photo" — is declared by a library, as a `host function`
//! in its `host_bridge.cln`, and the framework synthesizes WIT from it
//! ([`crate::hostbridge`]). The inbound half — "host hands the photo back" —
//! can only be declared by the host, as an `import` in its hand-written
//! `host.wit`.
//!
//! Nothing made those two agree. `HostInterface::to_wit` emits exactly one
//! bare `interface` of guest→host `func`s; the grammar has no production for
//! `import`, so a library *cannot* state which callback it expects even if its
//! author wanted to. The host's `import` line is written separately, by
//! someone else, in another repository.
//!
//! So a library could declare
//!
//! ```text
//! host function requestPhoto(quality: integer, callback: handler) returns integer
//! ```
//!
//! against a host whose `world` has no `photo-captured` import — and every
//! check in this framework passes. The compiler validates the guest's imports
//! against the host contract, which the guest satisfies: it imports nothing it
//! should not. The missing piece is not something the guest imports; it is
//! something the host was supposed to call, and never will. The app builds,
//! ships, opens the camera, and waits forever for a callback nobody sends.
//!
//! # What is checkable, and what is not
//!
//! Not much, and being precise about that is the point of this module.
//!
//! A `handler` parameter crosses the boundary as a bare `u32` — the index the
//! compiler assigns (§8.6). By design the host never learns Clean's calling
//! convention. But the erasure that makes the boundary clean also erases the
//! evidence: at the WIT level `callback: u32` and `quality: u32` are the same
//! type, and nothing in the synthesized WIT records that one of them is a
//! callback the host must invoke.
//!
//! What survives is the *Clean* declaration, before the erasure. A parsed
//! [`crate::hostbridge::HostFunction`] still knows a parameter was declared
//! `handler`. That is the signal this module uses: a function taking a
//! `handler` is asking the host to call back, so the host's contract must
//! declare at least one import capable of carrying that call.
//!
//! What this deliberately does NOT do is match a specific function to a
//! specific import. There is no naming convention tying `requestPhoto` to
//! `photo-captured`, and inventing one here would be a rule the spec never
//! stated and a library author could not discover. The check is therefore
//! coarse: it catches the whole-cloth failure — a library expecting callbacks
//! from a host that declares none — and stays silent on the finer question of
//! whether the *right* callback exists.
//!
//! That is a real limit, not an oversight. A coarse check that never cries
//! wolf is worth more than a precise one built on a convention nobody agreed
//! to; the narrower check becomes possible the day the spec gives a library a
//! way to name the callback it expects, and this module is where it would go.

use crate::hostbridge::{CleanType, HostFunction, HostInterface};

/// Does this function ask the host to call back?
///
/// True when any parameter was declared `handler`. Nested positions do not
/// count: `list<handler>` is not expressible today, and reading one as a
/// callback request would be guessing at a shape the spec has not defined.
pub fn expects_callback(function: &HostFunction) -> bool {
    function.params.iter().any(|p| p.ty == CleanType::Handler)
}

/// Every function in this interface that expects the host to call back.
pub fn callback_functions(interface: &HostInterface) -> Vec<&HostFunction> {
    interface.functions.iter().filter(|f| expects_callback(f)).collect()
}

/// Names of the `import`s a host contract's `world` declares.
///
/// As shallow as `hostwit::declares_world`, and for the same stated reason:
/// this crate is a courier, not a WIT compiler. It reads import *names* inside
/// the named world, which is all the check below needs — whether the host can
/// call back at all, not whether a particular signature lines up.
///
/// Comment text is stripped first, so prose mentioning `import foo` in a
/// banner cannot register as a declaration.
pub fn world_imports(wit: &str, world: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut inside = false;

    for raw in wit.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if !inside {
            // `world <name> {` — the same token discipline `declares_world`
            // uses, so `worldly-thing` is not a match.
            if let Some(rest) = line.strip_prefix("world") {
                if rest.starts_with(char::is_whitespace)
                    && rest.trim_start().split(['{', ' ']).next() == Some(world)
                {
                    inside = true;
                    depth = line.matches('{').count() - line.matches('}').count();
                }
            }
            continue;
        }

        // An `import` naming a function: `import photo-captured: func(...)`.
        // `import some-interface;` is a type-only import (what `use` produces)
        // and is not a callback, so the `:` is required.
        if let Some(rest) = line.strip_prefix("import ") {
            if let Some(name) = rest.split(':').next() {
                let name = name.trim();
                if !name.is_empty() && rest.contains(':') {
                    out.push(name.to_string());
                }
            }
        }

        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        if depth == 0 {
            break;
        }
    }

    out
}

/// A library expecting callbacks from a host that declares none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingCallbacks {
    /// The library's interface name, as written in `host_bridge.cln`.
    pub interface: String,
    /// The world checked against.
    pub world: String,
    /// Clean names of the functions that take a `handler`.
    pub functions: Vec<String>,
}

/// Check one library interface against the host contract it will bind to.
///
/// `None` when the pairing is sound, or when there is nothing to check: an
/// interface whose functions take no `handler` asks for no callbacks, and a
/// host declaring imports can carry them.
///
/// The check is one-directional on purpose. A host declaring imports no
/// library uses is not a defect — `app-state-changed` fires whether or not
/// anyone asked for it — so an unused import is never reported.
pub fn check(
    interface: &HostInterface,
    host_wit: &str,
    world: &str,
) -> Option<MissingCallbacks> {
    let wanting = callback_functions(interface);
    if wanting.is_empty() {
        return None;
    }
    if !world_imports(host_wit, world).is_empty() {
        return None;
    }

    Some(MissingCallbacks {
        interface: interface.name.clone(),
        world: world.to_string(),
        functions: wanting.iter().map(|f| f.name.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn iface(src: &str) -> HostInterface {
        HostInterface::parse(src, Path::new("host_bridge.cln")).unwrap()
    }

    const CAMERA_LIB: &str = r#"
host interface device version "0.1.0":
	requires host worlds ["mobile"]

	host function requestPhoto(quality: integer, callback: handler) returns integer
		description "Open the capture UI."
"#;

    const NO_CALLBACK_LIB: &str = r#"
host interface device version "0.1.0":
	requires host worlds ["mobile"]

	host function showNotification(title: string, body: string) returns integer
		description "Show a local notification."
"#;

    /// THE GAP, stated as a test.
    ///
    /// A host that provides the camera but declares no way to hand the photo
    /// back. Every other check in this framework passes: the library is
    /// well-formed, the world exists, the guest imports nothing it should not.
    /// Without this module the app builds, ships, and waits forever.
    #[test]
    fn a_library_wanting_callbacks_from_a_silent_host_is_reported() {
        let host = "package clean:host@0.1.0;\n\
                    interface camera {\n    request-photo: func() -> result<_, string>;\n}\n\
                    world mobile {\n    export camera;\n}\n";

        let found = check(&iface(CAMERA_LIB), host, "mobile")
            .expect("a handler parameter with no host import must be reported");
        assert_eq!(found.functions, vec!["requestPhoto"]);
        assert_eq!(found.world, "mobile");
    }

    #[test]
    fn a_host_that_can_call_back_satisfies_the_check() {
        let host = "package clean:host@0.1.0;\n\
                    world mobile {\n\
                    \x20   export camera;\n\
                    \x20   import photo-captured: func(request-id: u32, image: list<u8>);\n\
                    }\n";
        assert_eq!(check(&iface(CAMERA_LIB), host, "mobile"), None);
    }

    /// The check must stay silent when nothing asked for a callback, or it
    /// becomes noise every library learns to ignore.
    #[test]
    fn a_library_that_wants_no_callback_is_never_reported() {
        let host = "package clean:host@0.1.0;\nworld mobile {\n    export notifications;\n}\n";
        assert_eq!(check(&iface(NO_CALLBACK_LIB), host, "mobile"), None);
    }

    #[test]
    fn only_the_named_worlds_imports_are_read() {
        // `server` declaring imports must not vouch for `mobile`. Reading the
        // whole file instead of the named world would make every multi-world
        // contract silently pass.
        let host = "package clean:host@0.1.0;\n\
                    world server {\n    import handle: func(handler-id: u32);\n}\n\
                    world mobile {\n    export camera;\n}\n";
        assert!(check(&iface(CAMERA_LIB), host, "mobile").is_some());
        assert_eq!(world_imports(host, "server"), vec!["handle"]);
        assert!(world_imports(host, "mobile").is_empty());
    }

    /// `use` emits bare `import <interface>;` lines into a world — mobile's
    /// own contract has two. They carry types, not calls, and counting one as
    /// a callback would make the check pass for a host that cannot call back.
    #[test]
    fn a_type_only_import_does_not_count_as_a_callback() {
        let host = "package clean:host@0.1.0;\n\
                    world mobile {\n\
                    \x20   import location;\n\
                    \x20   use location.{position};\n\
                    \x20   export camera;\n\
                    }\n";
        assert!(world_imports(host, "mobile").is_empty());
        assert!(check(&iface(CAMERA_LIB), host, "mobile").is_some());
    }

    #[test]
    fn commented_out_imports_are_not_declarations() {
        let host = "package clean:host@0.1.0;\n\
                    world mobile {\n\
                    \x20   // import photo-captured: func(request-id: u32);\n\
                    \x20   export camera;\n\
                    }\n";
        assert!(world_imports(host, "mobile").is_empty());
    }

    #[test]
    fn the_real_mobile_contract_can_carry_the_callbacks_it_promises() {
        // The contract shipped in this repo must satisfy its own device
        // library, or the six interfaces it declares are unusable.
        let wit = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hosts/clean-mobile-surface/host.wit"),
        )
        .unwrap();

        let imports = world_imports(&wit, "mobile");
        assert!(
            imports.contains(&"photo-captured".to_string()),
            "expected photo-captured among {imports:?}"
        );
        assert_eq!(check(&iface(CAMERA_LIB), &wit, "mobile"), None);
    }
}

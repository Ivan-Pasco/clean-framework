//! The component-model worlds this framework knows, and everything that
//! follows from choosing one.
//!
//! # Why this module exists
//!
//! A world used to be described in four places that nothing forced to agree:
//! the target→world map and the built-in target list (`manifest`), the
//! vocabulary a library's `requires host worlds` may name (`hostbridge`), the
//! `[guest] world` value written into `host.toml` (`hosttoml`), and the package
//! kind (`package`). Adding a world meant finding all four.
//!
//! Two of them failed quietly. `hosttoml`'s map ended in a `_ => "cli-default"`
//! arm, so an unlisted world produced a `host.toml` that parsed cleanly and
//! then misbehaved at startup — and because `[guest] world` is metadata that
//! the host only warns about (CLIH-06), the symptom was a line on stderr rather
//! than a failed build. `hostbridge`'s list was a fixed-size `[&str; 4]`, so
//! adding a world was a *type* change, and until it was made no library could
//! declare the new world at all: `requires host worlds ["mobile"]` failed to
//! parse, blaming the library author for a gap in this crate.
//!
//! So [`WORLDS`] is now the single declaration and the four sites read from it.
//! The completeness test below is the point of the module: a `World` cannot be
//! added without answering every question a world raises, because the struct
//! has no optional fields.
//!
//! # What is deliberately *not* here
//!
//! `BUILT_IN_TARGETS` is derived from this table, but the rule that governs it
//! is not: **adding a target is a spec change, not a config change** (Platform
//! 07 §7.2, CONF-02). `manifest`'s spec-drift test still hard-asserts the arity
//! it expects, so growing this table without amending §7.2 fails CI. That
//! tripwire is load-bearing — see the note on [`WORLDS`].

use framework_package::Kind;

/// One component-model world, and every framework decision keyed to it.
///
/// No field is optional, which is what makes the completeness test possible:
/// a half-declared world does not compile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct World {
    /// The world name as it appears in a host's `host.wit` and in
    /// `target_world.world`.
    pub name: &'static str,

    /// Every `build.target` triple that selects this world. Many-to-one:
    /// `wasm32-server` and `wasm64-server` both mean `server`.
    pub targets: &'static [&'static str],

    /// The `[guest] world` value for `host.toml`.
    ///
    /// Usually the world's own name. `cli` is the exception, and the reason is
    /// recorded at [`crate::hosttoml`]: clean-cli declares two disjoint CLI
    /// worlds and the target triple cannot say which, so the framework names
    /// the one clean-cli actually implements.
    pub guest_world: &'static str,

    /// What `cln package` produces for a project in this world.
    pub package_kind: Kind,
}

/// Every world the framework knows (Platform 07 §7.2).
///
/// **This table mirrors §7.2 and must not diverge from it.** A target that
/// ought to exist is added to §7.2 first and reflected here second, never the
/// other way round. `manifest`'s `the_target_table_matches_platform_07_section_7_2`
/// asserts the arity this table produces, so an addition here without a spec
/// amendment is a failing build rather than a silent divergence.
///
/// One consequence is visible today: `wasm32-cli` maps to `cli`, and the
/// shipped `clean-cli/host.wit` declares only `cli-default`, so no CLI project
/// builds. That is not a bug on either side — HCV-06 forbids clean-cli
/// declaring a world it has not implemented, and our `CFG001` at Moment 1 is
/// the intended failure. See `system-documents/host-world-findings-for-clean-cli.md`.
pub const WORLDS: &[World] = &[
    World {
        name: "server",
        targets: &["wasm32-server", "wasm64-server"],
        guest_world: "server",
        package_kind: Kind::Serve,
    },
    World {
        name: "browser",
        targets: &["wasm32-browser"],
        guest_world: "browser",
        package_kind: Kind::Clapp,
    },
    World {
        name: "cli",
        targets: &["wasm32-cli"],
        // Not "cli"; see the field doc and `hosttoml`.
        guest_world: "cli-default",
        package_kind: Kind::Clapp,
    },
    World {
        name: "embedded",
        targets: &["wasm32-embedded"],
        guest_world: "embedded",
        package_kind: Kind::Clapp,
    },
];

/// The world a `build.target` selects, or `None` for an unknown target.
pub fn for_target(target: &str) -> Option<&'static World> {
    WORLDS.iter().find(|w| w.targets.contains(&target))
}

/// A world by name, or `None` if this framework does not know it.
pub fn for_name(name: &str) -> Option<&'static World> {
    WORLDS.iter().find(|w| w.name == name)
}

/// Whether a name is a world this framework knows.
///
/// Used by `hostbridge` to reject a typo in `requires host worlds` at parse
/// time, where the message can still name the file and line the author wrote.
pub fn is_known(name: &str) -> bool {
    for_name(name).is_some()
}

/// Every world name, for error messages that must list the valid ones.
pub fn names() -> Vec<&'static str> {
    WORLDS.iter().map(|w| w.name).collect()
}

/// Every built-in target triple (Platform 07 §7.4, CONF-02).
///
/// Ordered by world, then as listed within a world — stable, so error messages
/// do not reorder between builds.
pub fn built_in_targets() -> Vec<&'static str> {
    WORLDS.iter().flat_map(|w| w.targets.iter().copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason this module exists.
    ///
    /// Every world answers every question, and every target resolves back to
    /// the world that claims it. A new `World` cannot skip a field — the struct
    /// forbids it — so this test's job is the *relational* half: that the
    /// derived views agree with the table they are derived from.
    #[test]
    fn every_world_is_completely_declared() {
        for world in WORLDS {
            assert!(!world.name.is_empty(), "a world with no name");
            assert!(
                !world.targets.is_empty(),
                "world '{}' is selected by no target, so nothing can build it",
                world.name
            );
            assert!(
                !world.guest_world.is_empty(),
                "world '{}' has no [guest] world value",
                world.name
            );

            for target in world.targets {
                assert_eq!(
                    for_target(target).map(|w| w.name),
                    Some(world.name),
                    "target '{target}' does not resolve back to world '{}'",
                    world.name
                );
            }

            assert_eq!(
                for_name(world.name).map(|w| w.name),
                Some(world.name),
                "world '{}' is not findable by its own name",
                world.name
            );
        }
    }

    /// Two worlds claiming one target would make `for_target` order-dependent,
    /// and the loser would be unreachable.
    #[test]
    fn no_target_is_claimed_twice() {
        let targets = built_in_targets();
        for (i, target) in targets.iter().enumerate() {
            assert!(
                !targets[i + 1..].contains(target),
                "target '{target}' is claimed by more than one world"
            );
        }
    }

    #[test]
    fn no_world_is_declared_twice() {
        let names = names();
        for (i, name) in names.iter().enumerate() {
            assert!(
                !names[i + 1..].contains(name),
                "world '{name}' is declared more than once"
            );
        }
    }

    /// The old `hosttoml` map ended in `_ => "cli-default"`, so an unlisted
    /// world silently produced a CLI `host.toml`. Nothing falls through now, so
    /// `cli` must be the *only* world naming `cli-default` — any other would be
    /// that bug rather than the deliberate CLIH-20 remap.
    #[test]
    fn only_cli_maps_to_cli_default() {
        for world in WORLDS {
            if world.guest_world == "cli-default" {
                assert_eq!(
                    world.name, "cli",
                    "world '{}' claims cli-default, which is the old silent \
                     fallthrough rather than the deliberate CLIH-20 remap",
                    world.name
                );
            }
        }
    }

    /// `server` is the only world that ships a deployment bundle. Stated as a
    /// test because `package` used to decide it with a `world == "server"`
    /// string compare, and that is exactly the kind of fact that drifts.
    #[test]
    fn only_server_packages_as_a_bundle() {
        for world in WORLDS {
            let expected = if world.name == "server" { Kind::Serve } else { Kind::Clapp };
            assert_eq!(
                world.package_kind, expected,
                "world '{}' packages as the wrong kind",
                world.name
            );
        }
    }

    #[test]
    fn an_unknown_target_or_world_resolves_to_nothing() {
        assert!(for_target("wasm32-toaster").is_none());
        assert!(for_name("toaster").is_none());
        assert!(!is_known("servers"), "a near-miss must not resolve");
    }
}

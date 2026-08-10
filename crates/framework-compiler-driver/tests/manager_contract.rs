//! The contract with Manager, asserted rather than assumed.
//!
//! Manager installs this framework by downloading a release asset, unpacking
//! it into `~/.cln/versions/framework/<version>/`, and later exec'ing a binary
//! by a name it derives from `cln-shared`. Three things must line up, and all
//! three live in different repos:
//!
//! - the binary name in our `Cargo.toml` `[[bin]]`,
//! - the asset filename our release workflow produces,
//! - what `cln-shared` expects for both.
//!
//! Nothing in the type system connects them, so a rename in any one repo
//! produces an install that fails only at `cln build` time on a user's
//! machine. These tests are the tripwire.

use cln_shared::platform::{Arch, Os};
use cln_shared::{Platform, ToolchainKind};

#[test]
fn our_binary_name_is_what_manager_will_exec() {
    // Must match `[[bin]] name` in crates/framework-cli/Cargo.toml and the
    // staged file name in .github/workflows/release.yml.
    assert_eq!(ToolchainKind::Framework.binary_name(), "clean-framework");
}

#[test]
fn our_release_asset_names_match_what_manager_parses() {
    // The exact shape release.yml builds:
    //   clean-framework-<version>-<os>-<arch>.<ext>
    let cases = [
        (Platform { os: Os::Macos, arch: Arch::Arm64 }, "clean-framework-0.1.0-macos-arm64.tar.gz"),
        (Platform { os: Os::Linux, arch: Arch::X86_64 }, "clean-framework-0.1.0-linux-x86_64.tar.gz"),
        (Platform { os: Os::Windows, arch: Arch::X86_64 }, "clean-framework-0.1.0-windows-x86_64.zip"),
    ];

    for (platform, asset) in cases {
        assert!(
            platform.asset_matches(asset),
            "Manager would not recognize {asset} as the asset for {platform}"
        );
    }
}

#[test]
fn the_compiler_lives_where_we_look_for_it() {
    // The path `resolve.rs` builds must be the path Manager installs into
    // (Manager §00.2). Constructing it through cln-layout rather than
    // string-formatting is what keeps this true, but assert the shape anyway
    // so a layout change is a visible break rather than a silent relocation.
    let layout = cln_layout::Layout::new("/tmp/cln-contract-check");
    let binary = layout.version_binary(ToolchainKind::Compiler, &semver::Version::new(1, 4, 0));

    let rendered = binary.to_string_lossy().replace('\\', "/");
    assert!(
        rendered.ends_with("versions/compiler/1.4.0/clean-compiler"),
        "unexpected compiler path shape: {rendered}"
    );
}

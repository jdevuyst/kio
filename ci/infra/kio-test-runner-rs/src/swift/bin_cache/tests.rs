//! Unit tests for the Swift adapter over the shared build cache
//! (`src/swift/bin_cache/`).
//!
//! These cover the **Swift-specific policy** this module owns — the
//! one-level key-input field list, the toolchain subroot path, and the
//! `meta.json` body. The generic machinery (keying determinism,
//! length-prefix framing, locking, atomic-rename, orphan reap, LRU
//! eviction, the miss-then-hit firing of `produce`) is exercised by
//! [`crate::build_cache`]'s own tests, since it is compiler-agnostic.
//!
//! Per-field invalidation is asserted through [`bin_key`]: change one
//! Swift input, the key changes. The end-to-end `swiftc` path +
//! warm-hit firing is covered by the `exec_rlib_cache_*` goldens under
//! `kio@swift` (the per-case `rlib-cache-second-run-hits` check builds
//! twice and asserts the cache subtree fills), so these stay hermetic
//! and fast.

use crate::opt_profile::OptProfile;
use std::path::PathBuf;

use super::key::{self, BinInput};

#[test]
fn swiftc_build_uses_the_shared_outer_observer_shape() {
    let observer = crate::compiler_observer::CompilerObserver::for_test("observe");
    let command = super::swiftc_command(&observer);
    assert_eq!(command.get_program(), "observe");
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["swiftc"]);
}

fn bin_key(input: &BinInput) -> String {
    crate::build_cache::hash_fields(None, &key::bin_fields(input))
}

fn input(files: &[(&str, &[u8])]) -> BinInput {
    BinInput {
        swiftc_identity:
            "Swift version 6.3.2 (swift-6.3.2-RELEASE)\nTarget: x86_64-unknown-linux-gnu".into(),
        build_flags: super::build_flags(OptProfile::Default, "Greeter"),
        build_files: files
            .iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect(),
    }
}

// =========================================================================
// bin key: per-Swift-input invalidation
// =========================================================================

#[test]
fn bin_key_is_deterministic() {
    let i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let a = bin_key(&i);
    let b = bin_key(&i);
    assert_eq!(a, b);
    assert_eq!(a.len(), 64, "blake3 hex");
}

#[test]
fn bin_key_changes_on_swiftc_identity() {
    let mut i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let a = bin_key(&i);
    i.swiftc_identity =
        "Swift version 6.4.0 (swift-6.4.0-RELEASE)\nTarget: x86_64-unknown-linux-gnu".into();
    assert_ne!(a, bin_key(&i), "a toolchain bump rekeys");
}

#[test]
fn bin_key_changes_on_platform() {
    // The target platform is part of the `--version` identity, so a
    // cross-platform cache can't alias.
    let mut i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let a = bin_key(&i);
    i.swiftc_identity =
        "Swift version 6.3.2 (swift-6.3.2-RELEASE)\nTarget: aarch64-apple-macosx14.0".into();
    assert_ne!(a, bin_key(&i));
}

#[test]
fn bin_key_changes_on_source_byte() {
    let a = bin_key(&input(&[("pkg.swift", b"public struct KioUnit {} // a")]));
    let b = bin_key(&input(&[("pkg.swift", b"public struct KioUnit {} // b")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_driver_byte() {
    // The synthesized driver is part of the build-tree file set, so a
    // driver edit (a different protocol/host) re-keys the binary.
    let a = bin_key(&input(&[("main.swift", b"print(1) // v1")]));
    let b = bin_key(&input(&[("main.swift", b"print(1) // v2")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_file_rename() {
    let a = bin_key(&input(&[("a.swift", b"x")]));
    let b = bin_key(&input(&[("b.swift", b"x")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_without_file_prefix_map() {
    // `-file-prefix-map` is folded into the key: were a build path ever
    // embedded, a remapped binary must not alias a non-remapped one.
    let i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags = vec![
        ("-O".to_owned(), "none".to_owned()),
        ("-module-name".to_owned(), "Greeter".to_owned()),
    ];
    assert_ne!(a, bin_key(&i2), "dropping -file-prefix-map rekeys");
}

#[test]
fn bin_key_changes_on_module_name() {
    // `-module-name` affects symbol mangling, so it is folded into the
    // key: a binary built under one module name must not alias another.
    let i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags = vec![
        ("-O".to_owned(), "none".to_owned()),
        ("-module-name".to_owned(), "Other".to_owned()),
        ("-file-prefix-map".to_owned(), super::REMAP_TO.to_owned()),
    ];
    assert_ne!(a, bin_key(&i2), "a module-name change rekeys");
}

#[test]
fn bin_key_changes_on_profile() {
    // The `-O` level is folded into build_flags. Swift reserves its
    // single optimizing level (`-O`) for `optimized`; `default` and
    // `unoptimized` both map to `-Onone` (mild opt costs real compile
    // time under swiftc, so the default stays cheap), so they share a
    // key and `optimized` rekeys off them.
    let i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let default_key = bin_key(&i);
    let mut opt = i.clone();
    opt.build_flags = super::build_flags(OptProfile::Optimized, "Greeter");
    assert_ne!(default_key, bin_key(&opt), "-O rekeys off -Onone");
    let mut unopt = i.clone();
    unopt.build_flags = super::build_flags(OptProfile::Unoptimized, "Greeter");
    assert_eq!(
        default_key,
        bin_key(&unopt),
        "default and unoptimized share swift's -Onone"
    );
}

#[test]
fn bin_key_walk_order_is_irrelevant() {
    // The runner sorts the collected file set before keying
    // (execute_artifact's `files.sort_by`); the in-memory field order does
    // not matter as long as the keyed list is sorted. The fixture is in
    // sorted order (`kio_runtime.swift` < `pkg.swift`), so reversing then
    // re-sorting a copy reproduces it and keys identically.
    let i1 = input(&[("kio_runtime.swift", b"b"), ("pkg.swift", b"a")]);
    let mut i2 = i1.clone();
    i2.build_files.reverse();
    i2.build_files.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(bin_key(&i1), bin_key(&i2));
}

// =========================================================================
// subroot path
// =========================================================================

#[test]
fn subroot_rel_is_swift_seg() {
    let sub = key::subroot_rel(
        "Swift version 6.3.2 (swift-6.3.2-RELEASE)\nTarget: x86_64-unknown-linux-gnu",
    );
    let segs: Vec<_> = sub
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        segs.len(),
        1,
        "platform lives in the identity, no triple seg"
    );
    assert!(segs[0].starts_with("swift-"));
    assert_eq!(segs[0].len(), "swift-".len() + 8, "swift-<hex8>");
}

#[test]
fn subroot_rel_changes_with_toolchain() {
    let a = key::subroot_rel(
        "Swift version 6.3.2 (swift-6.3.2-RELEASE)\nTarget: x86_64-unknown-linux-gnu",
    );
    let b = key::subroot_rel(
        "Swift version 6.4.0 (swift-6.4.0-RELEASE)\nTarget: x86_64-unknown-linux-gnu",
    );
    assert_ne!(a, b, "a toolchain bump moves the subroot");
}

// =========================================================================
// meta.json sidecar
// =========================================================================

#[test]
fn bin_meta_json_carries_key_and_target() {
    let i = input(&[("pkg.swift", b"public struct KioUnit {}")]);
    let meta = key::bin_meta_json(&i);
    assert!(meta.contains("\"kind\": \"bin\""));
    assert!(meta.contains("\"target\": \"swift\""));
    assert!(meta.contains(&format!("\"key\": \"{}\"", bin_key(&i))));
    assert!(meta.contains("\"file_count\": 1"));
    // The `-file-prefix-map` flag is recorded so a user can see the
    // artifact is path-neutral.
    assert!(meta.contains("-file-prefix-map"));
}

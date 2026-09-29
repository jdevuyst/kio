//! Unit tests for the Haskell adapter over the shared build cache
//! (`src/haskell/bin_cache/`).
//!
//! These cover the **Haskell-specific policy** this module owns — the
//! one-level key-input field list, the toolchain subroot path, the
//! `ghc --info` platform parse, and the `meta.json` body. The generic
//! machinery (keying determinism, length-prefix framing, locking,
//! atomic-rename, orphan reap, LRU eviction, the miss-then-hit firing of
//! `produce`) is exercised by [`crate::build_cache`]'s own tests, since
//! it is compiler-agnostic.
//!
//! Per-field invalidation is asserted through [`bin_key`]: change one
//! Haskell input, the key changes. The end-to-end `ghc --make` path +
//! warm-hit firing is covered by the `exec_rlib_cache_*` goldens under
//! `kio@haskell` (the per-case `rlib-cache-second-run-hits` check
//! builds twice and asserts the cache subtree fills), so these stay
//! hermetic and fast.

use crate::opt_profile::OptProfile;
use std::path::PathBuf;

use super::key::{self, BinInput};

#[test]
fn ghc_build_uses_the_shared_outer_observer_shape() {
    let observer = crate::compiler_observer::CompilerObserver::for_test("observe");
    let command = super::ghc_compile_command(&observer);
    assert_eq!(command.get_program(), "observe");
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["ghc"]);
}

fn bin_key(input: &BinInput) -> String {
    crate::build_cache::hash_fields(None, &key::bin_fields(input))
}

fn input(files: &[(&str, &[u8])]) -> BinInput {
    BinInput {
        ghc_identity: "9.10.3\nx86_64-unknown-linux".into(),
        build_flags: super::build_flags(OptProfile::Default),
        build_files: files
            .iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect(),
    }
}

// =========================================================================
// bin key: per-Haskell-input invalidation
// =========================================================================

#[test]
fn bin_key_is_deterministic() {
    let i = input(&[("Greeter.hs", b"module Greeter where")]);
    let a = bin_key(&i);
    let b = bin_key(&i);
    assert_eq!(a, b);
    assert_eq!(a.len(), 64, "blake3 hex");
}

#[test]
fn bin_key_changes_on_ghc_identity() {
    let mut i = input(&[("Greeter.hs", b"module Greeter where")]);
    let a = bin_key(&i);
    i.ghc_identity = "9.12.1\nx86_64-unknown-linux".into();
    assert_ne!(a, bin_key(&i), "a toolchain bump rekeys");
}

#[test]
fn bin_key_changes_on_platform() {
    // The target platform is part of the identity, so a cross-platform
    // cache can't alias.
    let mut i = input(&[("Greeter.hs", b"module Greeter where")]);
    let a = bin_key(&i);
    i.ghc_identity = "9.10.3\naarch64-apple-darwin".into();
    assert_ne!(a, bin_key(&i));
}

#[test]
fn bin_key_changes_on_source_byte() {
    let a = bin_key(&input(&[("Greeter.hs", b"module Greeter where -- a")]));
    let b = bin_key(&input(&[("Greeter.hs", b"module Greeter where -- b")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_profile() {
    // The `-O` level is folded into build_flags. ghc's `default` and
    // `unoptimized` both map to `-O0` (mild opt costs real compile time
    // under ghc, so the default stays cheap), so they share a key;
    // `optimized` (`-O2`) rekeys off them.
    let i = input(&[("Greeter.hs", b"module Greeter where")]);
    let default_key = bin_key(&i);
    let mut opt = i.clone();
    opt.build_flags = super::build_flags(OptProfile::Optimized);
    assert_ne!(default_key, bin_key(&opt), "-O2 rekeys off -O0");
    let mut unopt = i.clone();
    unopt.build_flags = super::build_flags(OptProfile::Unoptimized);
    assert_eq!(
        default_key,
        bin_key(&unopt),
        "default and unoptimized share ghc's -O0"
    );
}

#[test]
fn bin_key_changes_on_driver_byte() {
    // The synthesized driver is part of the build-tree file set, so a
    // driver edit (a different protocol/host) re-keys the binary.
    let a = bin_key(&input(&[("Main.hs", b"module Main where -- v1")]));
    let b = bin_key(&input(&[("Main.hs", b"module Main where -- v2")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_file_rename() {
    let a = bin_key(&input(&[("A.hs", b"x")]));
    let b = bin_key(&input(&[("B.hs", b"x")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_walk_order_is_irrelevant() {
    // `build_files` is keyed in path-sorted order (the runner sorts before
    // keying), so the same file set from any directory-walk order keys the
    // same. Two coexisting facades with namespaces `Foo` and `Foo.Runtime`
    // exercise both a flat path and a nested namespace path. Path order puts
    // `Foo/Runtime.hs` before `Foo.hs`, so `i1` is canonical.
    let i1 = input(&[("Foo/Runtime.hs", b"b"), ("Foo.hs", b"a")]);
    let mut i2 = i1.clone();
    i2.build_files.reverse();
    i2.build_files.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(bin_key(&i1), bin_key(&i2));
}

// =========================================================================
// subroot path
// =========================================================================

#[test]
fn subroot_rel_is_ghc_seg() {
    let sub = key::subroot_rel("9.10.3\nx86_64-unknown-linux");
    let segs: Vec<_> = sub
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        segs.len(),
        1,
        "platform lives in the identity, no triple seg"
    );
    assert!(segs[0].starts_with("ghc-"));
    assert_eq!(segs[0].len(), "ghc-".len() + 8, "ghc-<hex8>");
}

#[test]
fn subroot_rel_changes_with_toolchain() {
    let a = key::subroot_rel("9.10.3\nx86_64-unknown-linux");
    let b = key::subroot_rel("9.12.1\nx86_64-unknown-linux");
    assert_ne!(a, b, "a toolchain bump moves the subroot");
}

// =========================================================================
// ghc --info platform parse
// =========================================================================

#[test]
fn parse_target_platform_from_ghc_info() {
    // A trimmed sample of real `ghc --info` output: a Haskell-rendered
    // list of `("key","value")` pairs, one per line.
    let info = "[(\"Project name\",\"The Glorious Glasgow Haskell Compilation System\")\n\
         ,(\"Target platform\",\"x86_64-unknown-linux\")\n\
         ,(\"Have interpreter\",\"YES\")\n\
         ]";
    assert_eq!(
        super::parse_ghc_target_platform(info),
        Some("x86_64-unknown-linux".to_owned())
    );
}

#[test]
fn parse_target_platform_absent_is_none() {
    let info = "[(\"Project name\",\"GHC\")\n]";
    assert_eq!(super::parse_ghc_target_platform(info), None);
}

// =========================================================================
// meta.json sidecar
// =========================================================================

#[test]
fn bin_meta_json_carries_key_and_target() {
    let i = input(&[("Greeter.hs", b"module Greeter where")]);
    let meta = key::bin_meta_json(&i);
    assert!(meta.contains("\"kind\": \"bin\""));
    assert!(meta.contains("\"target\": \"haskell\""));
    assert!(meta.contains(&format!("\"key\": \"{}\"", bin_key(&i))));
    assert!(meta.contains("\"file_count\": 1"));
}

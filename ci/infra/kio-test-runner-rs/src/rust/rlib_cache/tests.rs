//! Unit tests for the Rust adapter over the shared build cache
//! (`src/rust/rlib_cache/`).
//!
//! These cover the **Rust-specific policy** this module owns — the
//! key-input field lists, the toolchain/target subroot path, the
//! two-level rlib→bin chained key, and the `meta.json` bodies. The
//! generic machinery (keying determinism, length-prefix framing,
//! locking, atomic-rename, orphan reap, LRU eviction) is exercised by
//! [`crate::build_cache`]'s own tests, since it is compiler-agnostic.
//!
//! Per-field invalidation is asserted through [`super::rlib_key`] /
//! the bin key: change one Rust input, the key changes. The
//! end-to-end rustc path (the goldens at
//! `test-data/goldens/00_success/exec_rlib_cache_*`) covers compile +
//! warm-hit separately; these stay hermetic and fast.

use crate::build_cache::hash_fields;
use crate::compiler_admission::CompilerAdmission;
use crate::compiler_observer::{CompilerObserver, ENV as COMPILER_OBSERVER_ENV};
use crate::opt_profile::OptProfile;
use std::fs;
use std::path::PathBuf;

use super::key::{self, BinInput, RlibInput};

fn input(crate_name: &str, files: &[(&str, &[u8])]) -> RlibInput {
    RlibInput {
        rustc_identity: "rustc 1.85.0\ncommit-hash:abcd\nhost:x86_64-unknown-linux-gnu".into(),
        target_triple: "x86_64-unknown-linux-gnu".into(),
        edition: "2024".into(),
        profile: OptProfile::Unoptimized,
        crate_name: crate_name.into(),
        crate_files: files
            .iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect(),
    }
}

fn bin_key(input: &BinInput) -> String {
    hash_fields(Some(b"bin\0"), &key::bin_fields(input))
}

#[test]
fn rustc_command_nests_observer_outside_cache_wrapper() {
    let observer = CompilerObserver::for_test("observe");
    let wrapper = "sccache".into();
    let command = super::rustc_command(&observer, Some(&wrapper));
    assert_eq!(command.get_program(), "observe");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("sccache"),
            std::ffi::OsStr::new("rustc")
        ]
    );
}

#[cfg(unix)]
#[test]
fn observer_fires_for_a_cache_miss_not_a_hit_and_is_not_keyed() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = tempfile::TempDir::new().unwrap();
    let crate_root = fixture.path().join("crate");
    fs::create_dir_all(crate_root.join("src")).unwrap();
    fs::write(
        crate_root.join("src/lib.rs"),
        b"pub fn answer() -> i32 { 42 }\n",
    )
    .unwrap();

    // Deliberately use a compiler-like basename so miss observation itself is
    // name-independent. The scheduler readiness collision has its causal shell
    // coverage in `schedule-entry-selftest.sh`.
    let observer_path = fixture.path().join("rustc");
    fs::write(
        &observer_path,
        b"#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$(dirname \"$0\")/observer.log\"\nexec \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&observer_path, fs::Permissions::from_mode(0o755)).unwrap();
    let observer = CompilerObserver::for_test(observer_path);

    let rustc = std::path::Path::new("rustc");
    let input = RlibInput {
        rustc_identity: super::rustc_identity(rustc).unwrap(),
        target_triple: super::default_target_triple(rustc).unwrap(),
        edition: "2024".into(),
        profile: OptProfile::Unoptimized,
        crate_name: "observer_fixture".into(),
        crate_files: super::collect_crate_files(&crate_root).unwrap(),
    };

    let persistent_root = fixture.path().join("persistent-cache");
    let persistent = super::RlibCache::open(
        persistent_root.clone(),
        None,
        observer,
        None,
        CompilerAdmission::disabled(),
    )
    .unwrap();
    persistent.get_or_compile_rlib(&input, &crate_root).unwrap();
    persistent.get_or_compile_rlib(&input, &crate_root).unwrap();
    let log = fixture.path().join("observer.log");
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);

    // Changing observer identity must not change the artifact identity. A
    // nonexistent alternate observer therefore still resolves this warm hit.
    let alternate = super::RlibCache::open(
        persistent_root,
        None,
        CompilerObserver::for_test(fixture.path().join("missing-observer")),
        None,
        CompilerAdmission::disabled(),
    )
    .unwrap();
    alternate.get_or_compile_rlib(&input, &crate_root).unwrap();

    let meta = key::rlib_meta_json(&input);
    assert!(!meta.contains(COMPILER_OBSERVER_ENV));
}

// =========================================================================
// rlib key: per-Rust-input invalidation
// =========================================================================

#[test]
fn rlib_key_is_deterministic() {
    let i = input("p", &[("src/lib.rs", b"// hi")]);
    let a = super::rlib_key(&i);
    let b = super::rlib_key(&i);
    assert_eq!(a, b);
    assert_eq!(a.len(), 64, "blake3 hex");
}

#[test]
fn rlib_key_changes_on_rustc_identity() {
    let mut i = input("p", &[("src/lib.rs", b"// hi")]);
    let a = super::rlib_key(&i);
    i.rustc_identity = format!("{}-changed", i.rustc_identity);
    assert_ne!(a, super::rlib_key(&i));
}

#[test]
fn rlib_key_changes_on_target_triple() {
    let mut i = input("p", &[("src/lib.rs", b"// hi")]);
    let a = super::rlib_key(&i);
    i.target_triple = "aarch64-apple-darwin".into();
    assert_ne!(a, super::rlib_key(&i));
}

#[test]
fn rlib_key_changes_on_edition() {
    let mut i = input("p", &[("src/lib.rs", b"// hi")]);
    let a = super::rlib_key(&i);
    i.edition = "2018".into();
    assert_ne!(a, super::rlib_key(&i));
}

#[test]
fn rlib_key_changes_on_profile() {
    let mut i = input("p", &[("src/lib.rs", b"// hi")]);
    let a = super::rlib_key(&i);
    i.profile = OptProfile::Optimized;
    assert_ne!(a, super::rlib_key(&i));
}

#[test]
fn rlib_key_changes_on_source_byte() {
    let a = super::rlib_key(&input("p", &[("src/lib.rs", b"// hi")]));
    let b = super::rlib_key(&input("p", &[("src/lib.rs", b"// HI")]));
    assert_ne!(a, b);
}

#[test]
fn rlib_key_changes_on_file_rename() {
    let a = super::rlib_key(&input("p", &[("src/lib.rs", b"x")]));
    let b = super::rlib_key(&input("p", &[("src/main.rs", b"x")]));
    assert_ne!(a, b);
}

#[test]
fn rlib_key_changes_on_crate_name() {
    let a = super::rlib_key(&input("p", &[("src/lib.rs", b"x")]));
    let b = super::rlib_key(&input("q", &[("src/lib.rs", b"x")]));
    assert_ne!(a, b);
}

#[test]
fn rlib_key_walk_order_is_irrelevant() {
    // The walker sorts; the in-memory field order does not matter as
    // long as the collected list is sorted. Build two inputs with
    // reversed manual order, sort both (as the walker would), assert
    // equal keys.
    let i1 = input("p", &[("src/a.rs", b"a"), ("src/b.rs", b"b")]);
    let mut i2 = i1.clone();
    i2.crate_files.reverse();
    i2.crate_files.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(super::rlib_key(&i1), super::rlib_key(&i2));
}

// =========================================================================
// bin key: chains the rlib key + driver, distinct from the rlib key
// =========================================================================

fn bin_input() -> BinInput {
    BinInput {
        rustc_identity: "rustc 1.85.0".into(),
        target_triple: "x86_64-unknown-linux-gnu".into(),
        edition: "2024".into(),
        profile: OptProfile::Unoptimized,
        rlib_key: "0".repeat(64),
        crate_name: "p".into(),
        driver_source: b"fn main() {}".to_vec(),
        linker_flags: vec![("--extern".into(), "p".into())],
    }
}

#[test]
fn bin_key_changes_on_driver_source() {
    let bin = bin_input();
    let a = bin_key(&bin);
    let mut bin2 = bin.clone();
    bin2.driver_source = b"fn main() { println!(); }".to_vec();
    assert_ne!(a, bin_key(&bin2));
}

#[test]
fn bin_key_changes_on_rlib_key() {
    // Same driver, different rlib key (a `host.rs` edit upstream)
    // re-keys the bin — the chained sub-key threads through.
    let bin = bin_input();
    let a = bin_key(&bin);
    let mut bin3 = bin.clone();
    bin3.rlib_key = "1".repeat(64);
    assert_ne!(a, bin_key(&bin3));
}

#[test]
fn bin_key_distinct_from_rlib_key_with_identical_fields() {
    // The `b"bin\0"` domain separator keeps the bin sub-key from
    // colliding with the rlib's even when every shared field matches.
    // Build an rlib whose fields coincide with the bin's leading
    // fields; the keys must still differ.
    let rlib = RlibInput {
        rustc_identity: "rustc 1.85.0".into(),
        target_triple: "x86_64-unknown-linux-gnu".into(),
        edition: "2024".into(),
        profile: OptProfile::Unoptimized,
        crate_name: "p".into(),
        crate_files: Vec::new(),
    };
    assert_ne!(super::rlib_key(&rlib), bin_key(&bin_input()));
}

// =========================================================================
// subroot path
// =========================================================================

#[test]
fn subroot_rel_is_rustc_seg_over_triple() {
    let sub = key::subroot_rel(
        "rustc 1.85.0\ncommit-hash:abcd\nhost:x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
    );
    let segs: Vec<_> = sub
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(segs.len(), 2);
    assert!(segs[0].starts_with("rustc-"));
    assert_eq!(segs[0].len(), "rustc-".len() + 8, "rustc-<hex8>");
    assert_eq!(segs[1], "x86_64-unknown-linux-gnu");
}

#[test]
fn subroot_rel_changes_with_toolchain() {
    let a = key::subroot_rel("rustc 1.85.0", "x86_64-unknown-linux-gnu");
    let b = key::subroot_rel("rustc 1.86.0", "x86_64-unknown-linux-gnu");
    assert_ne!(a, b, "a toolchain bump moves the subroot");
}

// =========================================================================
// meta.json sidecars
// =========================================================================

#[test]
fn rlib_meta_json_carries_key_and_profile() {
    let i = input("p", &[("src/lib.rs", b"x")]);
    let meta = key::rlib_meta_json(&i);
    assert!(meta.contains("\"kind\": \"rlib\""));
    assert!(meta.contains(&format!("\"key\": \"{}\"", super::rlib_key(&i))));
    assert!(meta.contains("\"profile\": \"unoptimized\""));
    assert!(meta.contains("\"file_count\": 1"));
}

#[test]
fn bin_meta_json_carries_key_and_rlib_key() {
    let rlib = input("p", &[("src/lib.rs", b"x")]);
    let bin = bin_input();
    let meta = key::bin_meta_json(&rlib, &bin);
    assert!(meta.contains("\"kind\": \"bin\""));
    assert!(meta.contains(&format!("\"key\": \"{}\"", bin_key(&bin))));
    assert!(meta.contains(&format!("\"rlib_key\": \"{}\"", bin.rlib_key)));
}

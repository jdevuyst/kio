//! Unit tests for the Go adapter over the shared build cache
//! (`src/go/bin_cache/`).
//!
//! These cover the **Go-specific policy** this module owns — the
//! one-level key-input field list, the toolchain subroot path, and the
//! `meta.json` body. The generic machinery (keying determinism,
//! length-prefix framing, locking, atomic-rename, orphan reap, LRU
//! eviction, the miss-then-hit firing of `produce`) is exercised by
//! [`crate::build_cache`]'s own tests, since it is compiler-agnostic.
//!
//! Per-field invalidation is asserted through [`bin_key`]: change one
//! Go input, the key changes. The end-to-end `go build` path + warm-hit
//! firing is covered by the `exec_rlib_cache_*` goldens under
//! `kio@go` (the per-case `rlib-cache-second-run-hits` check builds
//! twice and asserts the cache subtree fills), so these stay hermetic
//! and fast.

use std::path::PathBuf;
use std::process::Command;

use super::key::{self, BinInput};

#[test]
fn go_build_uses_the_shared_outer_observer_shape() {
    let observer = crate::compiler_observer::CompilerObserver::for_test("observe");
    let command = super::go_compile_command(&observer);
    assert_eq!(command.get_program(), "observe");
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["go"]);
}

fn bin_key(input: &BinInput) -> String {
    crate::build_cache::hash_fields(None, &key::bin_fields(input))
}

fn input(files: &[(&str, &[u8])]) -> BinInput {
    BinInput {
        go_identity: "go version go1.26.4 linux/amd64".into(),
        build_flags: super::build_flags("go 1.26"),
        build_files: files
            .iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect(),
    }
}

// =========================================================================
// bin key: per-Go-input invalidation
// =========================================================================

#[test]
fn bin_key_is_deterministic() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let b = bin_key(&i);
    assert_eq!(a, b);
    assert_eq!(a.len(), 64, "blake3 hex");
}

#[test]
fn bin_key_changes_on_go_identity() {
    let mut i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    i.go_identity = "go version go1.27.0 linux/amd64".into();
    assert_ne!(a, bin_key(&i), "a toolchain bump rekeys");
}

#[test]
fn bin_key_changes_on_source_byte() {
    let a = bin_key(&input(&[("greeter/pkg.go", b"package greeter // a")]));
    let b = bin_key(&input(&[("greeter/pkg.go", b"package greeter // b")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_driver_byte() {
    // The synthesized driver is part of the build-tree file set, so a
    // driver edit (a different protocol/host) re-keys the binary.
    let a = bin_key(&input(&[("driver/main.go", b"package main // v1")]));
    let b = bin_key(&input(&[("driver/main.go", b"package main // v2")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_file_rename() {
    let a = bin_key(&input(&[("greeter/a.go", b"x")]));
    let b = bin_key(&input(&[("greeter/b.go", b"x")]));
    assert_ne!(a, b);
}

#[test]
fn bin_key_changes_on_go_directive() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags = super::build_flags("go 1.25");
    assert_ne!(a, bin_key(&i2), "a go-directive change rekeys");
}

#[test]
fn bin_key_changes_without_trimpath() {
    // `-trimpath` changes the emitted binary bytes, so it is folded into
    // the key: a cache built with it must not alias one built without.
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags.retain(|(key, _)| key != "-trimpath");
    assert_ne!(a, bin_key(&i2), "dropping -trimpath rekeys");
}

#[test]
fn bin_key_changes_without_goenv_off() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags.retain(|(key, _)| key != "GOENV");
    assert_ne!(a, bin_key(&i2), "dropping GOENV=off rekeys");
}

#[test]
fn bin_key_changes_without_empty_goexperiment() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags.retain(|(key, _)| key != "GOEXPERIMENT");
    assert_ne!(a, bin_key(&i2), "dropping empty GOEXPERIMENT rekeys");
}

#[test]
fn bin_key_changes_without_local_toolchain_policy() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let a = bin_key(&i);
    let mut i2 = i.clone();
    i2.build_flags.retain(|(key, _)| key != "GOTOOLCHAIN");
    assert_ne!(a, bin_key(&i2), "dropping GOTOOLCHAIN=local rekeys");
}

#[test]
fn build_settings_pin_floor_and_compiler_selection_environment() {
    let settings = super::build_flags("go 1.26");
    assert_eq!(
        settings,
        vec![
            ("-trimpath".to_owned(), String::new()),
            ("GOENV".to_owned(), "off".to_owned()),
            ("GOEXPERIMENT".to_owned(), String::new()),
            ("GOTOOLCHAIN".to_owned(), "local".to_owned()),
            ("go-directive".to_owned(), "go 1.26".to_owned()),
        ]
    );
    assert!(
        !settings.iter().any(|(key, _)| key == "TEST_TELEMETRY_DIR"),
        "a path-varying state directory does not affect artifact identity"
    );
}

#[test]
fn go_commands_redirect_telemetry_with_the_hermetic_environment() {
    let mut cmd = Command::new("go");
    super::apply_hermetic_go_environment(&mut cmd, std::path::Path::new("work/.telemetry"));
    let env = cmd
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(env.get("GOENV"), Some(&Some("off".to_owned())));
    assert_eq!(env.get("GOEXPERIMENT"), Some(&Some(String::new())));
    assert_eq!(env.get("GOTOOLCHAIN"), Some(&Some("local".to_owned())));
    assert_eq!(
        env.get("TEST_TELEMETRY_DIR"),
        Some(&Some("work/.telemetry".to_owned()))
    );
}

#[test]
fn bin_key_walk_order_is_irrelevant() {
    // The collector sorts; the in-memory field order does not matter as
    // long as the collected list is sorted.
    let i1 = input(&[("greeter/a.go", b"a"), ("greeter/b.go", b"b")]);
    let mut i2 = i1.clone();
    i2.build_files.reverse();
    i2.build_files.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(bin_key(&i1), bin_key(&i2));
}

// =========================================================================
// subroot path
// =========================================================================

#[test]
fn subroot_rel_is_go_seg() {
    let sub = key::subroot_rel("go version go1.26.4 linux/amd64");
    let segs: Vec<_> = sub
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        segs.len(),
        1,
        "go cross-compiles via GOOS/GOARCH, no triple seg"
    );
    assert!(segs[0].starts_with("go-"));
    assert_eq!(segs[0].len(), "go-".len() + 8, "go-<hex8>");
}

#[test]
fn subroot_rel_changes_with_toolchain() {
    let a = key::subroot_rel("go version go1.26.4 linux/amd64");
    let b = key::subroot_rel("go version go1.27.0 linux/amd64");
    assert_ne!(a, b, "a toolchain bump moves the subroot");
}

// =========================================================================
// meta.json sidecar
// =========================================================================

#[test]
fn bin_meta_json_carries_key_and_target() {
    let i = input(&[("greeter/pkg.go", b"package greeter")]);
    let meta = key::bin_meta_json(&i);
    assert!(meta.contains("\"kind\": \"bin\""));
    assert!(meta.contains("\"target\": \"go\""));
    assert!(meta.contains(&format!("\"key\": \"{}\"", bin_key(&i))));
    assert!(meta.contains("\"file_count\": 1"));
    // The `-trimpath` flag is recorded so a user can see the artifact is
    // path-neutral. Empty environment assignments retain their `=` so they
    // remain distinct from true valueless flags in the readable sidecar.
    assert!(meta.contains("\"-trimpath\""));
    assert!(meta.contains("\"GOEXPERIMENT=\""));
}

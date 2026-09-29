//! End-to-end tests for the per-module enriched-IR cache.
//!
//! Runs the real `kio` binary as a subprocess against temp package
//! trees and asserts the cache's skip / invalidate behavior:
//!
//! - **Cold then warm.** A first `kio build` writes the
//!   enriched-IR cache; a second build over byte-identical source
//!   hits the cache (the recovery + optimization pass is skipped
//!   per module).
//! - **Source edit invalidates.** Any edit to a module's body
//!   moves the typed-AST shape, so the next build re-runs recovery
//!   and optimization for that module — the postcard-encoded
//!   `Module<Prime>` is the key's per-module input.
//! - **Cache disabled.** When the package file's build block
//!   declares `cache ();`, the cache directory is never written and
//!   the recovery + optimization sweep runs uncached every time.
//! - **Determinism / behavior preservation.** The emitted output
//!   bytes are identical across cold and warm runs.
//!
//! Gated on `feature = "surface"` — the `kio` binary
//! (`CARGO_BIN_EXE_kio`) only exists in builds that include it.

#![cfg(feature = "surface")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use support::test_binary;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temp package directory, removed on drop.
struct TempPkg(PathBuf);

impl TempPkg {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("kio-enriched-cache-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp package dir");
        TempPkg(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Write `content` to `rel` under the package dir, creating
    /// parent directories as needed.
    fn write(&self, rel: &str, content: &str) {
        let p = self.0.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(p, content).expect("write package file");
    }
}

impl Drop for TempPkg {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run `kio build` in `dir` with `KIO_DEBUG_ENRICHED_CACHE=1` so
/// the cache hit / miss / store-fail status lands on stderr.
/// Returns `(exit_code, combined stdout+stderr)`.
fn kio_build(dir: &Path) -> (i32, String) {
    let out = Command::new(test_binary!("kio"))
        .arg("build")
        .arg("js")
        .current_dir(dir)
        .env("KIO_DEBUG_ENRICHED_CACHE", "1")
        .output()
        .expect("run `kio build`");
    let code = out.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (code, combined)
}

fn clear_artifact_cache(dir: &Path) {
    let _ = fs::remove_dir_all(dir.join(".cache").join("artifacts"));
}

/// Scaffold a minimal single-module package whose package file's
/// `build { ... }` block declares a `cache "<cache>"` field.
/// Returns the package dir; the cache directory lives at
/// `<dir>/.cache/`.
fn scaffold_with_cache(pkg: &TempPkg) {
    pkg.write(
        "pkg.pkg.kio",
        "package pkg;\n\n\
         build {\n  cache \".cache/\";\n\n  target js { out \"out/\"; }\n}\n\n\
         bridge {\n  main;\n}\n",
    );
    pkg.write("main.kio", "module main;\n\npub fn main() -> . { () }\n");
}

/// Scaffold a minimal single-module package whose package file's
/// `build { ... }` block declares `cache ();` — the disabled-cache
/// form.
fn scaffold_with_disabled_cache(pkg: &TempPkg) {
    pkg.write(
        "pkg.pkg.kio",
        "package pkg;\n\n\
         build {\n  cache ();\n\n  target js { out \"out/\"; }\n}\n\n\
         bridge {\n  main;\n}\n",
    );
    pkg.write("main.kio", "module main;\n\npub fn main() -> . { () }\n");
}

#[test]
fn unchanged_package_hits_cache_on_second_build() {
    // First `kio build` writes one entry per module; a second
    // build over byte-identical source hits the cache and skips
    // recovery + optimization per module.
    let pkg = TempPkg::new();
    scaffold_with_cache(&pkg);

    let (code1, log1) = kio_build(pkg.path());
    assert_eq!(code1, 0, "first build should succeed: {log1}");
    assert!(
        log1.contains("enriched-cache: miss"),
        "first build should miss the cache (no prior entry): {log1}"
    );
    clear_artifact_cache(pkg.path());

    let (code2, log2) = kio_build(pkg.path());
    assert_eq!(code2, 0, "second build should succeed: {log2}");
    assert!(
        log2.contains("enriched-cache: hit"),
        "second build over unchanged source should hit the cache: {log2}"
    );
    assert!(
        !log2.contains("enriched-cache: miss"),
        "second build should have no misses: {log2}"
    );
}

#[test]
fn source_edit_invalidates_cache() {
    // Any edit to a module's body moves the typed `Module<Prime>`
    // bytes, so the next build misses on that module — the
    // per-module key includes the typed-module fingerprint.
    let pkg = TempPkg::new();
    scaffold_with_cache(&pkg);

    let (code1, _) = kio_build(pkg.path());
    assert_eq!(code1, 0);

    pkg.write(
        "main.kio",
        "module main;\n\npub fn main() -> . { let x = (); x }\n",
    );
    let (code2, log2) = kio_build(pkg.path());
    assert_eq!(code2, 0, "edited package should still build: {log2}");
    assert!(
        log2.contains("enriched-cache: miss"),
        "edited module should miss the cache: {log2}"
    );
    clear_artifact_cache(pkg.path());

    // Third build over the (now-stable) edited source hits again.
    let (code3, log3) = kio_build(pkg.path());
    assert_eq!(code3, 0);
    assert!(
        log3.contains("enriched-cache: hit"),
        "third build over stable source should hit again: {log3}"
    );
}

#[test]
fn disabled_cache_always_misses_and_writes_no_entries() {
    // With `cache ();` the build proceeds normally through the
    // direct uncached path and never writes a cache directory.
    let pkg = TempPkg::new();
    scaffold_with_disabled_cache(&pkg);

    let (code1, log1) = kio_build(pkg.path());
    assert_eq!(code1, 0, "first build should succeed: {log1}");

    let (code2, log2) = kio_build(pkg.path());
    assert_eq!(code2, 0, "second build should succeed: {log2}");
    // No `.cache/` directory should have appeared.
    let cache_dir = pkg.path().join(".cache");
    assert!(
        !cache_dir.exists(),
        "disabled cache must not create the cache directory: {}",
        cache_dir.display()
    );
}

#[test]
fn warm_and_cold_builds_emit_byte_identical_output() {
    // The cache's contract is "the cached `Module<Enriched>` is
    // byte-identical to a fresh computation." Smoke-test the
    // observable end of that: cold (cache empty) and warm (cache
    // hot) builds emit the same JS bytes.
    let pkg = TempPkg::new();
    scaffold_with_cache(&pkg);

    // Cold build.
    let (code1, _) = kio_build(pkg.path());
    assert_eq!(code1, 0);
    let cold_bytes = fs::read(pkg.path().join("out").join("pkg.js"))
        .expect("cold build should produce out/pkg.js");

    // Wipe the output directory; the cache directory survives so
    // the next build is warm. Clear the outer artifact cache too:
    // this test is specifically about the enriched-IR layer.
    fs::remove_dir_all(pkg.path().join("out")).expect("clean output");
    clear_artifact_cache(pkg.path());

    // Warm build.
    let (code2, log2) = kio_build(pkg.path());
    assert_eq!(code2, 0);
    assert!(
        log2.contains("enriched-cache: hit"),
        "warm build should hit the cache: {log2}"
    );
    let warm_bytes = fs::read(pkg.path().join("out").join("pkg.js"))
        .expect("warm build should produce out/pkg.js");

    assert_eq!(
        cold_bytes, warm_bytes,
        "warm-cache emit must be byte-identical to cold-cache emit"
    );
}

#[test]
fn cache_directory_has_gitignore_after_first_write() {
    // Mirrors the rlib-cache convention: on first write to a
    // cache root, drop a `.gitignore` containing `*` so the
    // contents stay out of `git status` regardless of where the
    // user placed the directory.
    let pkg = TempPkg::new();
    scaffold_with_cache(&pkg);

    let (code, log) = kio_build(pkg.path());
    assert_eq!(code, 0, "first build should succeed: {log}");

    let gitignore = pkg.path().join(".cache").join(".gitignore");
    assert!(
        gitignore.is_file(),
        "expected {} to exist after first write",
        gitignore.display()
    );
    let text = fs::read_to_string(&gitignore).unwrap();
    assert!(text.contains("*"), "gitignore should ignore everything");
    assert!(
        text.contains("!.gitignore"),
        "gitignore should preserve itself"
    );
}

#[test]
fn enriched_ir_subdir_appears_under_cache_root() {
    // The cache lives at `<cache>/enriched-ir/`.
    // Smoke-check that the sub-namespacing happens.
    let pkg = TempPkg::new();
    scaffold_with_cache(&pkg);
    let (code, _) = kio_build(pkg.path());
    assert_eq!(code, 0);
    let schema_dir = pkg.path().join(".cache").join("enriched-ir");
    assert!(
        schema_dir.is_dir(),
        "expected {} to exist after a build",
        schema_dir.display()
    );
    // At least one cache entry should be present (the package has
    // at least one module body whose enriched IR was written).
    let entry_count = fs::read_dir(&schema_dir)
        .expect("read enriched-ir schema directory")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.ends_with(".bin"))
                .unwrap_or(false)
        })
        .count();
    assert!(
        entry_count > 0,
        "expected at least one `.bin` entry under {}",
        schema_dir.display()
    );
}

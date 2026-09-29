//! End-to-end tests for `kio cache` management commands.
//!
//! Runs the real `kio` binary against a temp package tree and
//! asserts that cache management:
//!
//! - Removes the contents of the `build { cache "<path>"; }`
//!   directory in the package's package file.
//! - Preserves the `rlib/` subdirectory (content-addressed by
//!   rustc inputs, not gated by `--no-cache`).
//! - Preserves the cache root's `.gitignore`.
//! - Exits 0 with `no cache configured` when the build block declares
//!   `cache ();`.
//! - Exits with the usage code when no `<name>.pkg.kio` is found.
//! - Garbage-collects old semantic entries while preserving recent
//!   entries and host-toolchain caches.
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

/// A uniquely-named temp directory that drops itself.
struct TempPkg(PathBuf);

impl TempPkg {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kio-cache-clear-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp package dir");
        TempPkg(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPkg {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run `kio cache clear` in `dir`. Returns `(exit_code,
/// stdout, stderr)`.
fn kio_cache_clear(dir: &Path) -> (i32, String, String) {
    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .arg("clear")
        .current_dir(dir)
        .output()
        .expect("run `kio cache clear`");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

fn kio_cache_gc(dir: &Path) -> (i32, String, String) {
    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .arg("gc")
        .current_dir(dir)
        .output()
        .expect("run `kio cache gc`");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

/// Write a `pkg.pkg.kio` carrying a `build { ... }` block with
/// the given body (the `cache` / `target` lines).
fn write_pkg(dir: &Path, build_body: &str) {
    let src = format!("package pkg;\n\nbuild {{\n{build_body}}}\n");
    fs::write(dir.join("pkg.pkg.kio"), src).expect("write package file");
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_access_stamp(cache: &Path, family: &str, rel: &str, seconds: u64) {
    let path = cache
        .join(".gc")
        .join("access")
        .join(family)
        .join(format!("{rel}.stamp"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create access stamp parent");
    }
    fs::write(path, format!("{seconds}\n")).expect("write access stamp");
}

/// Populated cache: the contents are cleared, the `rlib/` subdir
/// and the cache root's `.gitignore` are preserved.
#[test]
fn clear_removes_kio_semantic_caches_and_preserves_rlib() {
    let pkg = TempPkg::new();
    write_pkg(
        pkg.path(),
        "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
    );
    let cache = pkg.path().join(".cache");
    fs::create_dir_all(cache.join("enriched-ir")).unwrap();
    fs::write(cache.join("enriched-ir/entry.bin"), b"stale").unwrap();
    fs::create_dir_all(cache.join("package-check/pkg")).unwrap();
    fs::write(cache.join("package-check/pkg/full.bin"), b"stale").unwrap();
    fs::create_dir_all(cache.join("doc")).unwrap();
    fs::write(cache.join("doc/entry.bin"), b"stale").unwrap();
    fs::create_dir_all(cache.join("rlib/v1")).unwrap();
    fs::write(cache.join("rlib/v1/keep.bin"), b"keepme").unwrap();
    fs::write(cache.join(".gitignore"), "*\n!.gitignore\n").unwrap();

    let (code, stdout, stderr) = kio_cache_clear(pkg.path());
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("cleared"),
        "expected `cleared <N> entries`: stdout={stdout}"
    );

    assert!(
        !cache.join("enriched-ir").exists(),
        "enriched-ir/ should be cleared"
    );
    assert!(
        !cache.join("package-check").exists(),
        "package-check/ should be cleared"
    );
    assert!(!cache.join("doc").exists(), "doc/ should be cleared");
    assert!(cache.join("rlib").is_dir(), "rlib/ must be preserved");
    assert!(
        cache.join("rlib/v1/keep.bin").is_file(),
        "rlib/ contents must survive"
    );
    assert!(
        cache.join(".gitignore").is_file(),
        ".gitignore must be preserved"
    );
}

/// `cache ();` — exit 0 with `no cache configured`, touches
/// nothing.
#[test]
fn disabled_cache_prints_no_cache_configured() {
    let pkg = TempPkg::new();
    write_pkg(pkg.path(), "  cache ();\n  target js { out \"out/\"; }\n");
    let (code, stdout, stderr) = kio_cache_clear(pkg.path());
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("no cache configured"),
        "expected `no cache configured`: stdout={stdout}"
    );
}

/// Cache directory doesn't yet exist (fresh checkout): exit 0 with
/// the `already empty` message.
#[test]
fn missing_cache_dir_prints_already_empty() {
    let pkg = TempPkg::new();
    write_pkg(
        pkg.path(),
        "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
    );
    let (code, stdout, stderr) = kio_cache_clear(pkg.path());
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("already empty"),
        "expected `already empty`: stdout={stdout}"
    );
}

/// Empty cache directory: exit 0 with `already empty`; directory
/// stays on disk.
#[test]
fn empty_cache_dir_prints_already_empty() {
    let pkg = TempPkg::new();
    write_pkg(
        pkg.path(),
        "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
    );
    let cache = pkg.path().join(".cache");
    fs::create_dir_all(&cache).unwrap();

    let (code, stdout, stderr) = kio_cache_clear(pkg.path());
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("already empty"),
        "expected `already empty`: stdout={stdout}"
    );
    assert!(cache.is_dir(), "cache dir should survive an empty clear");
}

#[test]
fn gc_prunes_old_semantic_entries_and_preserves_recent_and_rlib() {
    let pkg = TempPkg::new();
    write_pkg(
        pkg.path(),
        "  cache \".cache/\";\n  target js { out \"out/\"; }\n",
    );
    let cache = pkg.path().join(".cache");
    let doc = cache.join("doc");
    fs::create_dir_all(&doc).unwrap();
    fs::write(doc.join("old.bin"), b"old").unwrap();
    fs::write(doc.join("recent.bin"), b"recent").unwrap();
    fs::create_dir_all(cache.join("rlib/v1")).unwrap();
    fs::write(cache.join("rlib/v1/keep.bin"), b"keepme").unwrap();
    fs::create_dir_all(cache.join("unknown")).unwrap();
    fs::write(cache.join("unknown/keep.txt"), b"keep").unwrap();

    let now = unix_now_secs();
    write_access_stamp(&cache, "doc", "old.bin", now - 4 * 24 * 60 * 60);
    write_access_stamp(&cache, "doc", "recent.bin", now);

    let (code, stdout, stderr) = kio_cache_gc(pkg.path());
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("garbage-collected"),
        "expected garbage collection summary: stdout={stdout}"
    );
    assert!(
        !doc.join("old.bin").exists(),
        "old doc entry should be pruned"
    );
    assert!(
        doc.join("recent.bin").is_file(),
        "recent doc entry should survive"
    );
    assert!(
        cache.join("rlib/v1/keep.bin").is_file(),
        "rlib/ contents must survive"
    );
    assert!(
        cache.join("unknown/keep.txt").is_file(),
        "unknown top-level cache entries must survive"
    );
}

/// No package file at the cwd: exit with the usage code (2). The
/// error message points the user at running from a package
/// directory.
#[test]
fn no_package_file_exits_usage() {
    let pkg = TempPkg::new();
    // No `*.pkg.kio` at all.
    let (code, _stdout, stderr) = kio_cache_clear(pkg.path());
    assert_eq!(code, 2, "expected usage exit code: stderr={stderr}");
    assert!(
        stderr.contains("no `<name>.pkg.kio`"),
        "expected package-file-missing message: stderr={stderr}"
    );
}

/// `kio cache` with no subcommand exits with the usage code.
#[test]
fn cache_without_subcommand_exits_usage() {
    let pkg = TempPkg::new();
    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .current_dir(pkg.path())
        .output()
        .expect("run `kio cache`");
    assert_eq!(out.status.code(), Some(2));
}

/// `kio cache --help` and child help surfaces succeed.
#[test]
fn cache_help_succeeds() {
    let pkg = TempPkg::new();
    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .arg("--help")
        .current_dir(pkg.path())
        .output()
        .expect("run `kio cache --help`");
    assert_eq!(out.status.code(), Some(0));

    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .arg("clear")
        .arg("--help")
        .current_dir(pkg.path())
        .output()
        .expect("run `kio cache clear --help`");
    assert_eq!(out.status.code(), Some(0));

    let out = Command::new(test_binary!("kio"))
        .arg("cache")
        .arg("gc")
        .arg("--help")
        .current_dir(pkg.path())
        .output()
        .expect("run `kio cache gc --help`");
    assert_eq!(out.status.code(), Some(0));
}

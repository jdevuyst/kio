//! End-to-end test for `kio init`.
//!
//! Runs the real `kio` binary to scaffold a package in a temp
//! directory, then runs `kio check` in that same directory and
//! asserts it exits 0. This guards the init template against drifting
//! out of sync with the language: a freshly-init'd package must use
//! the host/bridge model (`host type` / `host fn` in a module, a
//! package `bridge { … }` glob list) and check clean, not the old
//! `env {}` / `bridge <name> {}` / `export {}` model that `kio check`
//! now rejects.
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
        let dir = std::env::temp_dir().join(format!("kio-init-check-{}-{n}", std::process::id()));
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

/// Run `kio <args...>` in `dir`. Returns `(exit_code, stdout, stderr)`.
fn kio(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(test_binary!("kio"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run `kio {}`: {e}", args.join(" ")));
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

/// `kio init` scaffolds a package whose generated files check clean.
/// This is the regression for the init template emitting an old-model
/// package file (`env {}` / `export {}`) that `kio check` rejects.
#[test]
fn init_scaffolds_a_package_that_checks_clean() {
    let pkg = TempPkg::new();

    let (code, stdout, stderr) = kio(pkg.path(), &["init", "demo"]);
    assert_eq!(
        code, 0,
        "`kio init` failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        pkg.path().join("demo.pkg.kio").is_file(),
        "expected demo.pkg.kio to be written"
    );
    assert!(
        pkg.path().join("main.kio").is_file(),
        "expected main.kio to be written"
    );

    let (code, stdout, stderr) = kio(pkg.path(), &["check"]);
    assert_eq!(
        code, 0,
        "`kio check` on a freshly-init'd package must exit 0: stdout={stdout} stderr={stderr}"
    );
}

/// The generated source is already canonically formatted: `kio fmt
/// --check` passes without reformatting. Guards the template against
/// drifting away from the formatter's output shape.
#[test]
fn init_output_is_canonically_formatted() {
    let pkg = TempPkg::new();

    let (code, stdout, stderr) = kio(pkg.path(), &["init", "demo"]);
    assert_eq!(
        code, 0,
        "`kio init` failed: stdout={stdout} stderr={stderr}"
    );

    let (code, stdout, stderr) = kio(pkg.path(), &["fmt", "--check"]);
    assert_eq!(
        code, 0,
        "`kio fmt --check` on freshly-init'd source must exit 0: stdout={stdout} stderr={stderr}"
    );
}

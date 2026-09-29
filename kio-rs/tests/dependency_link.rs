//! End-to-end tests for local `path` dependency materialization.
//!
//! Builds a two-package fixture in a temp directory — a consumer
//! package with a `<local>.dep.kio` pointing at a sibling host-free
//! library package — and runs the real `kio` binary over the consumer,
//! asserting on the process exit code. Materialization is the explicit
//! `kio dep fetch` step
//! (`package_collection::materialize_dependencies`, driven at the
//! command level): it resolves, re-roots, and writes the dependency's
//! modules to disk under the consumer tree. `kio check` then consumes
//! those pre-materialized modules with its ordinary
//! (dependency-agnostic) walk and never re-materializes. The tests run
//! `kio dep fetch` followed by `kio check` to exercise that materialize
//! step, plus the exit-code mapping for its error tier
//! (`ExitCode::Dep` = 30), which surfaces at the `dep fetch` command.
//!
//! Scope of the slice under test: **direct, host-free, local `path`
//! dependencies, materialized nested dependency trees, no sealing** —
//! every `pub` item of the dependency is importable by the consumer.
//! Auto-materializing a dependency's own dependencies, sealing, and
//! host/env composition are later slices and are not exercised here.
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
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kio-dep-link-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    fs::write(path, contents).expect("write fixture file");
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

/// Write a host-free library package at `<root>/lib`: `package lib;`
/// with a single module `lib` exporting a polymorphic `helper`.
fn write_library(root: &Path) {
    write(
        &root.join("lib/lib.pkg.kio"),
        "package lib;\n\nbridge {\n  lib;\n}\n",
    );
    write(
        &root.join("lib/lib.kio"),
        "module lib;\n\npub fn helper[A](x: A) -> A { x }\n",
    );
}

/// A consumer package that declares a `lib` dependency by relative
/// `path` and imports `helper` across the re-rooted edge typechecks
/// clean: the dependency's `lib` module re-roots to `lib/lib`, and
/// `import lib/lib(helper);` resolves to its `pub fn helper`.
#[test]
fn consumer_depending_on_local_library_typechecks() {
    let tmp = TempDir::new();
    let root = tmp.path();
    write_library(root);

    write(
        &root.join("consumer/consumer.pkg.kio"),
        "package consumer;\n\nbridge {\n  main;\n}\n",
    );
    write(
        &root.join("consumer/lib.dep.kio"),
        "dependency lib;\n\nsource {\n  path \"../lib/lib.pkg.kio\";\n}\n",
    );
    write(
        &root.join("consumer/main.kio"),
        "module main;\n\nimport lib/lib(helper);\n\npub fn run[A](x: A) -> A { helper(x) }\n",
    );

    let consumer = root.join("consumer");
    let (fetch_code, fetch_out, fetch_err) = kio(&consumer, &["dep", "fetch"]);
    assert_eq!(
        fetch_code, 0,
        "`kio dep fetch` on a consumer depending on a local library must exit 0: \
         stdout={fetch_out} stderr={fetch_err}"
    );

    let (code, stdout, stderr) = kio(&consumer, &["check"]);
    assert_eq!(
        code, 0,
        "`kio check` on a consumer with its local library materialized must exit 0: \
         stdout={stdout} stderr={stderr}"
    );
}

/// A dependency whose own modules cross-reference each other (module
/// `core` imports `helper`) keeps resolving after the dependency is
/// re-rooted: re-rooting must rewrite the intra-dependency `import helper(h);`
/// to `import <local>/helper(h);`, not only the `module`
/// declarations. Without that, `helper` resolves to a non-existent root
/// module of the merged package and the consumer's `check` fails with
/// `module \`helper\` not found in package` (exit 12).
#[test]
fn intra_dependency_import_resolves_after_rerooting() {
    let tmp = TempDir::new();
    let root = tmp.path();

    // A two-module dependency `dep`: `core` imports `h` from its sibling
    // `helper` and re-exports it through `run`.
    write(
        &root.join("dep/dep.pkg.kio"),
        "package dep;\n\nbridge {\n  core;\n  helper;\n}\n",
    );
    write(
        &root.join("dep/core.kio"),
        "module core;\n\nimport helper(h);\n\npub fn run[A](x: A) -> A { h(x) }\n",
    );
    write(
        &root.join("dep/helper.kio"),
        "module helper;\n\npub fn h[A](x: A) -> A { x }\n",
    );

    write(
        &root.join("consumer/consumer.pkg.kio"),
        "package consumer;\n\nbridge {\n  main;\n}\n",
    );
    write(
        &root.join("consumer/dep.dep.kio"),
        "dependency dep;\n\nsource {\n  path \"../dep/dep.pkg.kio\";\n}\n",
    );
    write(
        &root.join("consumer/main.kio"),
        "module main;\n\nimport dep/core(run);\n\npub fn use_it[A](x: A) -> A { run(x) }\n",
    );

    let consumer = root.join("consumer");
    let (fetch_code, fetch_out, fetch_err) = kio(&consumer, &["dep", "fetch"]);
    assert_eq!(
        fetch_code, 0,
        "`kio dep fetch` materializing a dependency whose modules cross-reference must exit 0: \
         stdout={fetch_out} stderr={fetch_err}"
    );

    let (code, stdout, stderr) = kio(&consumer, &["check"]);
    assert_eq!(
        code, 0,
        "a dependency whose modules cross-reference must keep typechecking after re-rooting \
         (intra-dependency import paths re-rooted with the modules): stdout={stdout} stderr={stderr}"
    );
}

/// A dependency whose local name equals the first segment of one of the
/// consumer's own modules is rejected (exit 30): a `*.dep.kio` may only
/// *add* an importable `<local>/…` root, never shadow a local module,
/// so `import <local>/…` stays unambiguous (open-world).
#[test]
fn dependency_name_colliding_with_local_module_is_a_dep_error() {
    let tmp = TempDir::new();
    let root = tmp.path();
    write_library(root);

    write(
        &root.join("consumer/consumer.pkg.kio"),
        "package consumer;\n\nbridge {\n  main;\n  lib;\n}\n",
    );
    write(
        &root.join("consumer/lib.dep.kio"),
        "dependency lib;\n\nsource {\n  path \"../lib/lib.pkg.kio\";\n}\n",
    );
    write(
        &root.join("consumer/main.kio"),
        "module main;\n\npub fn run[A](x: A) -> A { x }\n",
    );
    // A local module `lib` whose first segment collides with the
    // dependency's local name.
    write(
        &root.join("consumer/lib.kio"),
        "module lib;\n\npub fn local_thing[A](x: A) -> A { x }\n",
    );

    // The collision is detected during materialization, so it surfaces at
    // `kio dep fetch` (not at the later, dependency-agnostic `check`).
    let (code, stdout, stderr) = kio(&root.join("consumer"), &["dep", "fetch"]);
    assert_eq!(
        code, 30,
        "a dependency name colliding with a local module root must exit 30 (ExitCode::Dep) \
         at `kio dep fetch`: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("collides with the local module root"),
        "collision diagnostic expected; got stderr={stderr}"
    );
}

/// A dependency whose `path` does not resolve to an existing
/// `*.pkg.kio` file is rejected (exit 30).
#[test]
fn dependency_path_that_does_not_resolve_is_a_dep_error() {
    let tmp = TempDir::new();
    let root = tmp.path();

    write(
        &root.join("consumer/consumer.pkg.kio"),
        "package consumer;\n\nbridge {\n  main;\n}\n",
    );
    write(
        &root.join("consumer/lib.dep.kio"),
        "dependency lib;\n\nsource {\n  path \"../nonexistent/lib.pkg.kio\";\n}\n",
    );
    write(
        &root.join("consumer/main.kio"),
        "module main;\n\npub fn run[A](x: A) -> A { x }\n",
    );

    // Path resolution happens during materialization, so an unresolvable
    // `path` surfaces at `kio dep fetch` (not at the later `check`).
    let (code, stdout, stderr) = kio(&root.join("consumer"), &["dep", "fetch"]);
    assert_eq!(
        code, 30,
        "an unresolvable dependency path must exit 30 (ExitCode::Dep) at `kio dep fetch`: \
         stdout={stdout} stderr={stderr}"
    );
    assert!(
        stderr.contains("cannot be resolved"),
        "unresolvable-path diagnostic expected; got stderr={stderr}"
    );
}

/// A dependency that itself declares a `*.dep.kio` is accepted when its
/// own dependency has already been materialized in its tree. The nested
/// materialized modules re-root with their parent dependency.
#[test]
fn materialized_nested_dependency_re_roots_with_dependency() {
    let tmp = TempDir::new();
    let root = tmp.path();

    // A leaf library `deeplib`.
    write(
        &root.join("deeplib/deeplib.pkg.kio"),
        "package deeplib;\n\nbridge {\n  deeplib;\n}\n",
    );
    write(
        &root.join("deeplib/deeplib.kio"),
        "module deeplib;\n\npub fn deep[A](x: A) -> A { x }\n",
    );
    // `lib` depends on `deeplib`; its local materialized tree mirrors
    // what `kio dep fetch` in `lib` would have written.
    write(
        &root.join("lib/lib.pkg.kio"),
        "package lib;\n\nbridge {\n  lib;\n}\n",
    );
    write(
        &root.join("lib/lib.kio"),
        "module lib;\n\nimport deeplib/deeplib(deep);\n\npub fn helper[A](x: A) -> A { deep(x) }\n",
    );
    write(
        &root.join("lib/deeplib.dep.kio"),
        "dependency deeplib;\n\nsource {\n  path \"../deeplib/deeplib.pkg.kio\";\n}\n",
    );
    write(
        &root.join("lib/deeplib/deeplib.kio"),
        "module deeplib/deeplib;\n\npub fn deep[A](x: A) -> A { x }\n",
    );
    // The consumer depends on `lib`.
    write(
        &root.join("consumer/consumer.pkg.kio"),
        "package consumer;\n\nbridge {\n  main;\n}\n",
    );
    write(
        &root.join("consumer/lib.dep.kio"),
        "dependency lib;\n\nsource {\n  path \"../lib/lib.pkg.kio\";\n}\n",
    );
    write(
        &root.join("consumer/main.kio"),
        "module main;\n\nimport lib/lib(helper);\n\npub fn run[A](x: A) -> A { helper(x) }\n",
    );

    let consumer = root.join("consumer");
    let (fetch_code, fetch_out, fetch_err) = kio(&consumer, &["dep", "fetch"]);
    assert_eq!(
        fetch_code, 0,
        "`kio dep fetch` materializing a dependency with an already-materialized nested \
         dependency must exit 0: stdout={fetch_out} stderr={fetch_err}"
    );

    let (code, stdout, stderr) = kio(&consumer, &["check"]);
    assert_eq!(
        code, 0,
        "a dependency with an already-materialized nested dependency must typecheck after \
         both trees re-root: stdout={stdout} stderr={stderr}"
    );
}

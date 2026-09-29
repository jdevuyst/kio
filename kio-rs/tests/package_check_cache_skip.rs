//! End-to-end tests for the `kio check` package-check cache.
//!
//! Runs the real `kio` binary as a subprocess against temp
//! package trees and asserts the cache's skip / invalidate
//! behavior: an unchanged package skips re-typechecking on the
//! next check, source edits re-check the edited package, and
//! consumer packages skip when the dependency surface they import is
//! unchanged.
//!
//! Gated on `feature = "surface"` and `feature = "cli"` — the `kio`
//! binary (`CARGO_BIN_EXE_kio`) only exists in builds that include both.

#![cfg(all(feature = "surface", feature = "cli"))]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use support::test_binary;

use kio_lang::cache::keys::{PackageName, PipelineTag};
use kio_lang::pass::full::FullPipeline;
use kio_lang::pipeline::Pipeline;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temp package directory, removed on drop.
struct TempPkg(PathBuf);

impl TempPkg {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("kio-package-check-{}-{n}", std::process::id()));
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

/// Run `kio check` in `dir` with `KIO_DEBUG_PACKAGE_CHECK_CACHE=1` so the
/// cache hit / miss / first-write status lands on stderr.
/// Returns `(exit_code, combined stdout+stderr)`.
fn kio_check(dir: &Path) -> (i32, String) {
    kio_check_with_frontend_timing(dir, false)
}

fn kio_check_with_frontend_timing(dir: &Path, frontend_timing: bool) -> (i32, String) {
    kio_check_with_extra_env(dir, frontend_timing, &[])
}

fn kio_check_with_extra_env(
    dir: &Path,
    frontend_timing: bool,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    let mut cmd = Command::new(test_binary!("kio"));
    cmd.arg("check")
        .current_dir(dir)
        .env("KIO_DEBUG_PACKAGE_CHECK_CACHE", "1");
    if frontend_timing {
        cmd.env("KIO_DEBUG_TIMING", "frontend");
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run `kio check`");
    let code = out.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (code, combined)
}

fn kio_check_no_cache_with_extra_env(dir: &Path, extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(test_binary!("kio"));
    cmd.args(["--no-cache", "check"]).current_dir(dir);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run `kio --no-cache check`");
    let code = out.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (code, combined)
}

fn kio_build_prime_with_extra_env(dir: &Path, extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(test_binary!("kio"));
    cmd.args(["build", "kio-prime"]).current_dir(dir);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run `kio build kio-prime`");
    let code = out.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (code, combined)
}

/// Run `kio test` in `dir` with `KIO_DEBUG_PACKAGE_CHECK_CACHE=1` so the
/// no-equiv fast path's package-check cache hit / miss / first-write
/// status lands on stderr alongside the `no equiv blocks found` line.
/// Returns `(exit_code, combined stdout+stderr)`.
fn kio_test(dir: &Path) -> (i32, String) {
    let out = Command::new(test_binary!("kio"))
        .arg("test")
        .current_dir(dir)
        .env("KIO_DEBUG_PACKAGE_CHECK_CACHE", "1")
        .output()
        .expect("run `kio test`");
    let code = out.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (code, combined)
}

fn package_file(name: &str, rest: &str) -> String {
    format!("package {name};\n\nbuild {{\n  cache \"out/.kio-cache/\";\n}}\n\n{rest}")
}

fn package_file_with_cache(name: &str, cache: &str, rest: &str) -> String {
    format!("package {name};\n\nbuild {{\n  cache \"{cache}\";\n}}\n\n{rest}")
}

fn package_file_with_cache_and_target(
    name: &str,
    cache: &str,
    target: &str,
    out: &str,
    rest: &str,
) -> String {
    format!(
        "package {name};\n\nbuild {{\n  cache \"{cache}\";\n\n  target {target} {{\n    out \"{out}\";\n  }}\n}}\n\n{rest}"
    )
}

fn package_file_no_cache(name: &str, rest: &str) -> String {
    format!("package {name};\n\nbuild {{\n  cache ();\n}}\n\n{rest}")
}

fn package_file_no_cache_with_prime_target(name: &str, rest: &str) -> String {
    format!(
        "package {name};\n\nbuild {{\n  cache ();\n\n  target kio-prime {{\n    out \"out/kio-prime/\";\n  }}\n}}\n\n{rest}"
    )
}

fn pkg_root_module() -> &'static str {
    "module pkg;\n"
}

fn pkg_root_string_env_module() -> &'static str {
    "module pkg;\n\n\
     host type String role(str);\n"
}

fn package_check_cache_path(root: &Path, package_name: &str) -> PathBuf {
    kio_lang::cache::package_check::cache_path(
        &root.join("out").join(".kio-cache"),
        &PackageName::new(package_name),
        PipelineTag::new(FullPipeline::CACHE_TAG),
    )
}

fn identity_user_elaborator_module() -> &'static str {
    "module elaborators;\n\n\
     import __intrinsics__;\n\n\
     import __comptime__;\n\n\
     pub type Target_request = __Type__ | .;\n\n\
     pub pure fn id_impl(_ct: __Comptime__, _source: __Type__, value: __Checked_term__, _target: Target_request) -> __Checked_term__ {\n\
       value\n\
     }\n\n\
     pub elab id_user : [Source] Source -> [Target] Target { impl id_impl; };\n"
}

fn second_value_user_elaborator_module() -> &'static str {
    "module elaborators;\n\n\
     import __intrinsics__;\n\n\
     import __comptime__;\n\n\
     pub type Target_request = __Type__ | .;\n\n\
     pub pure fn second_impl(_ct: __Comptime__, _first_source: __Type__, _first: __Checked_term__, _second_source: __Type__, second: __Checked_term__, _target: Target_request) -> __Checked_term__ {\n\
       second\n\
     }\n\n\
     pub elab second_value : [First] First -> [Second] Second -> [Target] Target { impl second_impl; };\n"
}

fn source_infer_user_elaborator_module() -> &'static str {
    "module elaborators;\n\n\
     import __intrinsics__;\n\n\
     import __comptime__;\n\n\
     pub type Target_request = __Type__ | .;\n\n\
     pub pure fn id_default_impl(_ct: __Comptime__, _source: __Type__, value: __Checked_term__, target: Target_request) -> __Checked_term__ {\n\
       __either__(\n\
         , __Type__\n\
         , .\n\
         , __Checked_term__\n\
         , target\n\
         , .(_target: __Type__) -> __Checked_term__ { value }\n\
         , .(_empty: .) -> __Checked_term__ { value }\n\
         )\n\
     }\n\n\
     pub elab id_default : [Source] Source -> [Target] Target { impl id_default_impl; };\n"
}

fn unit_identity_user_elaborator_module() -> &'static str {
    "module elaborators;\n\n\
     import __intrinsics__;\n\n\
     import __comptime__;\n\n\
     pub type Target_request = __Type__ | .;\n\n\
     pub pure fn id_impl(ct: __Comptime__, _source: __Type__, _value: __Checked_term__, _target: Target_request) -> __Checked_term__ {\n\
       __term_unit__(ct)\n\
     }\n\n\
     pub elab id_user : [Source] Source -> [Target] Target { impl id_impl; };\n"
}

fn unit_infer_user_elaborator_module() -> &'static str {
    "module elaborators;\n\n\
     import __intrinsics__;\n\n\
     import __comptime__;\n\n\
     pub type Target_request = __Type__ | .;\n\n\
     pub pure fn id_default_impl(ct: __Comptime__, _source: __Type__, value: __Checked_term__, target: Target_request) -> __Checked_term__ {\n\
       __either__(\n\
         , __Type__\n\
         , .\n\
         , __Checked_term__\n\
         , target\n\
         , .(_target: __Type__) -> __Checked_term__ { value }\n\
         , .(_empty: .) -> __Checked_term__ { __term_unit__(ct) }\n\
         )\n\
     }\n\n\
     pub elab id_default : [Source] Source -> [Target] Target { impl id_default_impl; };\n"
}

#[test]
fn unchanged_package_skips_on_second_check() {
    // First `kio check` writes the cache; a second check over
    // byte-identical source skips Phase 4 typechecking entirely.
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    assert!(
        log1.contains("first-write"),
        "first check should write the cache: {log1}"
    );
    assert!(
        package_check_cache_path(pkg.path(), "pkg").exists(),
        "package-check cache should live under the build-block cache root"
    );
    assert!(
        !pkg.path().join("pkg.pkg.cache").exists(),
        "package-check cache must not be written next to the package source"
    );

    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "second check should succeed: {log2}");
    assert!(
        log2.contains("hit") && log2.contains("typecheck skipped"),
        "second check over unchanged source should skip: {log2}"
    );
}

#[test]
fn typed_modules_reuse_one_semantic_entry_across_package_names() {
    let root = TempPkg::new();
    let provider = "module shared;\n\n\
        host type Host_value;\n\n\
        host fn host_value() -> Host_value;\n\n\
        pub newtype Token : . { pub constructor mk_token; pub projector un_token; };\n\n\
        fn secret() -> . { () }\n\n\
        pub fn run() -> Token { Token.mk_token(()) }\n";
    let consumer = "module consumer;\n\n\
        import shared as s;\n\n\
        pub fn call() -> s.Token { s.run() }\n";
    root.write(
        "a/package_a.pkg.kio",
        &package_file_with_cache_and_target(
            "package_a",
            "../shared-cache/",
            "js",
            "out/a-js/",
            "bridge {\n  consumer;\n  shared;\n}\n",
        ),
    );
    root.write("a/shared.kio", provider);
    root.write("a/consumer.kio", consumer);
    root.write(
        "b/package_b.pkg.kio",
        &package_file_with_cache_and_target(
            "package_b",
            "../shared-cache/",
            "kio-prime",
            "out/b-prime/",
            "bridge {\n  unrelated;\n  shared;\n  consumer;\n}\n",
        ),
    );
    root.write("b/shared.kio", provider);
    root.write("b/consumer.kio", consumer);
    root.write(
        "b/unrelated.kio",
        "module unrelated;\n\npub fn extra() -> . { () }\n",
    );

    let debug_typed_cache = [
        ("KIO_DEBUG_TYPED_CACHE", "1"),
        ("KIO_DEBUG_WRITE_TYPED_CACHE", "1"),
    ];
    let (code_a, log_a) =
        kio_check_with_extra_env(&root.path().join("a"), false, &debug_typed_cache);
    assert_eq!(code_a, 0, "first package should typecheck: {log_a}");
    assert!(
        log_a.contains("typed-cache: write package_a shared")
            && log_a.contains("typed-cache: write package_a consumer"),
        "first package should populate both shared semantic entries: {log_a}"
    );

    let (code_b, log_b) =
        kio_check_with_extra_env(&root.path().join("b"), false, &debug_typed_cache);
    assert_eq!(code_b, 0, "second package should typecheck: {log_b}");
    assert!(
        log_b.contains("typed-cache: hit package_b shared")
            && log_b.contains("typed-cache: hit package_b consumer")
            && log_b.contains("typed-cache: write package_b unrelated"),
        "the provider's nominal type and host capability plus the consumer using that exact identity should reuse entries despite different build configuration and a manifest with an added unrelated module: {log_b}"
    );

    let provider_with_addition = format!("{provider}\npub fn unrelated() -> . {{ () }}\n");
    root.write("b/shared.kio", &provider_with_addition);
    let (edited_code, edited_log) =
        kio_check_with_extra_env(&root.path().join("b"), false, &debug_typed_cache);
    assert_eq!(
        edited_code, 0,
        "adding an unrelated declaration must preserve the existing consumer's meaning: {edited_log}"
    );
    assert!(
        edited_log.contains("typed-cache: write package_b shared")
            && edited_log.contains("typed-cache: write package_b consumer"),
        "a changed public surface must invalidate both the provider and its consumer before accepting the open-world addition: {edited_log}"
    );

    root.write(
        "visibility/package_visibility.pkg.kio",
        &package_file_with_cache(
            "package_visibility",
            "../shared-cache/",
            "bridge {\n  shared;\n  consumer;\n}\n",
        ),
    );
    root.write("visibility/shared.kio", &provider_with_addition);
    root.write(
        "visibility/consumer.kio",
        "module consumer;\n\n\
         import shared as s;\n\n\
         pub fn expose_secret() -> . { s.secret() }\n",
    );
    let (visibility_code, visibility_log) =
        kio_check_with_extra_env(&root.path().join("visibility"), false, &debug_typed_cache);
    assert_eq!(
        visibility_code, 14,
        "a cached provider must not make its private declarations visible: {visibility_log}"
    );
    assert!(
        visibility_log.contains("no `pub fn` named `secret`")
            && visibility_log.contains("typed-cache: hit package_visibility shared"),
        "the reused provider must retain the ordinary visibility boundary: {visibility_log}"
    );
    assert!(
        !visibility_log.contains("typed-cache: write package_visibility consumer"),
        "a consumer that violates visibility must not be published: {visibility_log}"
    );

    root.write(
        "invalid/package_invalid.pkg.kio",
        &package_file_with_cache(
            "package_invalid",
            "../shared-cache/",
            "bridge {\n  absent;\n}\n",
        ),
    );
    root.write("invalid/shared.kio", provider);
    root.write("invalid/consumer.kio", consumer);
    let (invalid_code, invalid_log) =
        kio_check_with_extra_env(&root.path().join("invalid"), false, &debug_typed_cache);
    assert_eq!(invalid_code, 20, "unexpected package error: {invalid_log}");
    assert!(
        invalid_log.contains("bridge glob `absent` matches no module"),
        "the current package manifest must still be validated: {invalid_log}"
    );
    assert!(
        !invalid_log.contains("typed-cache:"),
        "package-boundary validation must precede typed-cache lookup and writes: {invalid_log}"
    );
}

#[test]
fn typed_cache_debug_root_shares_only_explicitly_enabled_package_entries() {
    let root = TempPkg::new();
    let source = "module shared;\n\npub fn run() -> . { () }\n";
    for (dir, name, cache) in [
        ("a", "package_a", Some("out/a-cache/")),
        ("b", "package_b", Some("out/b-cache/")),
        ("disabled", "package_disabled", None),
    ] {
        let package = match cache {
            Some(cache) => package_file_with_cache(name, cache, "bridge {\n  shared;\n}\n"),
            None => package_file_no_cache(name, "bridge {\n  shared;\n}\n"),
        };
        root.write(&format!("{dir}/{name}.pkg.kio"), &package);
        root.write(&format!("{dir}/shared.kio"), source);
    }

    let shared_root = root.path().join("run-shared-typed-cache");
    let shared_root = shared_root.to_str().expect("temporary path is UTF-8");
    let env = [
        ("KIO_DEBUG_TYPED_CACHE", "1"),
        ("KIO_DEBUG_WRITE_TYPED_CACHE", "1"),
        ("KIO_DEBUG_TYPED_CACHE_ROOT", shared_root),
    ];
    let (code_a, log_a) = kio_check_with_extra_env(&root.path().join("a"), false, &env);
    assert_eq!(code_a, 0, "first package should typecheck: {log_a}");
    assert!(
        log_a.contains("typed-cache: write package_a shared"),
        "first package should populate the run-shared root: {log_a}"
    );

    let (no_cache_code, no_cache_log) =
        kio_check_no_cache_with_extra_env(&root.path().join("b"), &env);
    assert_eq!(
        no_cache_code, 0,
        "the globally cache-disabled package should still typecheck: {no_cache_log}"
    );
    assert!(
        !no_cache_log.contains("typed-cache:"),
        "the debug root must not override the global --no-cache policy: {no_cache_log}"
    );

    let (code_b, log_b) = kio_check_with_extra_env(&root.path().join("b"), false, &env);
    assert_eq!(code_b, 0, "second package should typecheck: {log_b}");
    assert!(
        log_b.contains("typed-cache: hit package_b shared"),
        "the debug root should override distinct enabled package roots: {log_b}"
    );

    let (disabled_code, disabled_log) =
        kio_check_with_extra_env(&root.path().join("disabled"), false, &env);
    assert_eq!(
        disabled_code, 0,
        "the cache-disabled package should still typecheck: {disabled_log}"
    );
    assert!(
        !disabled_log.contains("typed-cache:"),
        "the debug root must not enable a package whose build block disables caching: {disabled_log}"
    );
}

#[test]
fn typed_cache_debug_root_ignores_relative_paths() {
    let root = TempPkg::new();
    root.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  shared;\n}\n"),
    );
    root.write("shared.kio", "module shared;\n\npub fn run() -> . { () }\n");

    let relative_root = "relative-typed-cache-root";
    let relative_path = root.path().join(relative_root);
    let env = [
        ("KIO_DEBUG_TYPED_CACHE", "1"),
        ("KIO_DEBUG_WRITE_TYPED_CACHE", "1"),
        ("KIO_DEBUG_TYPED_CACHE_ROOT", relative_root),
    ];
    let (code, log) = kio_check_with_extra_env(root.path(), false, &env);

    assert_eq!(code, 0, "package should typecheck: {log}");
    assert!(
        log.contains("typed-cache: write pkg shared"),
        "ignoring the invalid override must retain the configured package cache: {log}"
    );
    assert!(
        !relative_path.exists(),
        "a relative debug root must not create a cache under the command cwd"
    );
}

#[test]
fn imported_operator_callable_edit_invalidates_an_importing_consumer() {
    let root = TempPkg::new();
    root.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  consumer;\n  owner;\n  provider;\n}\n"),
    );
    root.write(
        "provider.kio",
        "module provider;\n\npub fn step(left: ., right: .) -> . { left }\n",
    );
    root.write(
        "owner.kio",
        "module owner;\n\nimport provider as provider;\n\npub op _ + _ { impl provider.step; };\n",
    );
    root.write(
        "consumer.kio",
        "module consumer;\n\nimport owner(op _ + _);\n\npub fn run() -> . { () + () }\n",
    );
    let env = [
        ("KIO_DEBUG_TYPED_CACHE", "1"),
        ("KIO_DEBUG_WRITE_TYPED_CACHE", "1"),
    ];

    let (cold_code, cold_log) = kio_check_with_extra_env(root.path(), false, &env);
    assert_eq!(cold_code, 0, "cold package should typecheck: {cold_log}");
    assert!(
        cold_log.contains("typed-cache: write pkg consumer"),
        "cold check should publish the operator consumer: {cold_log}"
    );

    root.write(
        "provider.kio",
        "module provider;\n\npub fn step(left: ., right: .) -> . & . { (left, right) }\n",
    );
    let (edited_code, edited_log) = kio_check_with_extra_env(root.path(), false, &env);
    assert_eq!(
        edited_code, 14,
        "the changed callable result must make the consumer fail freshly: {edited_log}"
    );
    assert!(
        edited_log.contains("typed-cache: miss pkg owner")
            && edited_log.contains("typed-cache: miss pkg consumer")
            && !edited_log.contains("typed-cache: hit pkg owner")
            && !edited_log.contains("typed-cache: hit pkg consumer"),
        "an imported callable provider edit must move the owner and importing consumer keys: {edited_log}"
    );
}

#[test]
fn root_cache_hit_skips_post_parse_frontend_work() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");

    let (code2, log2) = kio_check_with_frontend_timing(pkg.path(), true);
    assert_eq!(code2, 0, "second check should succeed: {log2}");
    let timing = log2
        .lines()
        .find(|line| line.starts_with("frontend-timing: pkg "))
        .unwrap_or_else(|| panic!("expected frontend timing line: {log2}"));
    assert!(
        timing.contains("forced_modules=0")
            && timing.contains("lower_resolve_ms=0.000")
            && timing.contains("typecheck_ms=0.000")
            && timing.contains("pipeline_typecheck_ms=0.000")
            && timing.contains("prime_validation_ms=0.000")
            && timing.contains("package_check_skipped=1")
            && timing.contains("import_body_type_ms=0.000"),
        "root cache hit should skip post-parse frontend work: {timing}"
    );
}

#[test]
fn frontend_scheduler_reports_independent_modules_ready_together() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write("pkg/a.kio", "module pkg/a;\n\npub fn a() -> . { () }\n");
    pkg.write("pkg/b.kio", "module pkg/b;\n\npub fn b() -> . { () }\n");

    let (code, log) = kio_check_with_frontend_timing(pkg.path(), true);
    assert_eq!(code, 0, "check should succeed: {log}");
    let timing = log
        .lines()
        .find(|line| line.starts_with("frontend-timing: pkg "))
        .unwrap_or_else(|| panic!("expected frontend timing line: {log}"));
    assert!(
        timing.contains("forced_modules=3")
            && timing.contains("summary_levels=1")
            && timing.contains("summary_max_width=3"),
        "independent modules should be ready in the same scheduler level: {timing}"
    );
}

#[test]
fn frontend_scheduler_keeps_independent_user_elaborator_consumers_parallel() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", identity_user_elaborator_module());
    pkg.write(
        "pkg/a.kio",
        "module pkg/a;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_user);\n\n\
         pub fn a(s: String) -> String { id_user!(s, String) }\n",
    );
    pkg.write(
        "pkg/b.kio",
        "module pkg/b;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_user);\n\n\
         pub fn b(s: String) -> String { id_user!(s, String) }\n",
    );

    let (code, log) = kio_check_with_frontend_timing(pkg.path(), true);
    assert_eq!(code, 0, "check should succeed: {log}");
    let timing = log
        .lines()
        .find(|line| line.starts_with("frontend-timing: pkg "))
        .unwrap_or_else(|| panic!("expected frontend timing line: {log}"));
    assert!(
        timing.contains("forced_modules=4")
            && timing.contains("summary_levels=2")
            && timing.contains("summary_max_width=2")
            && timing.contains("user_elaborator_template_eval_ms=")
            && timing.contains("user_elaborator_template_replay_ms=")
            && timing.contains("user_elaborator_template_batch_obligations=")
            && timing.contains("user_elaborator_template_batch_unique=")
            && timing.contains("user_elaborator_template_batch_duplicates="),
        "independent user-elaborator consumers should fan out after the elaborator module: {timing}"
    );
}

#[test]
fn user_elaborator_batch_timing_reports_unique_template_keys() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", identity_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_user);\n\n\
         pub fn run(s: String) -> String {\n\
           let first = id_user!(s, String);\n\
           id_user!(first, String)\n\
         }\n",
    );

    let (code, log) = kio_check_with_frontend_timing(pkg.path(), true);
    assert_eq!(code, 0, "check should succeed: {log}");
    let timing = log
        .lines()
        .find(|line| line.starts_with("frontend-timing: pkg "))
        .unwrap_or_else(|| panic!("expected frontend timing line: {log}"));
    assert!(
        timing.contains("user_elaborator_template_batch_obligations=2")
            && timing.contains("user_elaborator_template_batch_unique=1")
            && timing.contains("user_elaborator_template_batch_duplicates=1"),
        "duplicate deferred user-elaborator shapes should be collapsed before compute: {timing}"
    );
}

#[test]
fn typed_cache_misses_when_transitive_same_package_surface_moves() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write("pkg/c.kio", "module pkg/c;\n\npub type Shared = .;\n");
    pkg.write(
        "pkg/b.kio",
        "module pkg/b;\n\nimport pkg/c(Shared);\n\npub fn id_shared(x: Shared) -> Shared { x }\n",
    );
    pkg.write(
        "pkg/a.kio",
        "module pkg/a;\n\n\
         import pkg/c(Shared);\n\n\
         import pkg/b(id_shared);\n\n\
         pub fn use_shared(x: Shared) -> . { id_shared(x) }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "second check should warm-hit: {log2}");
    assert!(
        log2.contains("package-check-cache hit: pkg"),
        "second check should establish the warm cache path: {log2}"
    );

    pkg.write(
        "pkg/c.kio",
        "module pkg/c;\n\n\
         pub newtype Box : . { pub constructor mk_box; pub projector un_box; };\n\n\
         pub type Shared = Box;\n",
    );
    let (code3, log3) = kio_check(pkg.path());
    assert_eq!(
        code3, 14,
        "changing C.Shared must force A to re-typecheck, not accept A's stale typed cache: {log3}"
    );
}

#[test]
fn user_elaborator_template_batch_dedupes_repeated_shape_before_memo_lookup() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", identity_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_user);\n\n\
         pub fn run(s: String) -> String {\n\
           let first = id_user!(s, String);\n\
           id_user!(first, String)\n\
         }\n",
    );

    let (code, log) = kio_check_with_extra_env(pkg.path(), false, &[("KIO_DEBUG_MEMO", "1")]);
    assert_eq!(code, 0, "check should succeed: {log}");
    let user_elaborator_line = log
        .lines()
        .find(|line| {
            line.starts_with("memo user-elaborator: ")
                && line.contains("hits=0")
                && line.contains("misses=1")
                && line.contains("first-writes=1")
        })
        .unwrap_or_else(|| panic!("expected user-elaborator memo stats: {log}"));
    assert!(
        user_elaborator_line.contains("hits=0")
            && user_elaborator_line.contains("misses=1")
            && user_elaborator_line.contains("first-writes=1"),
        "duplicate user-elaborator shapes in one deferred batch should not enter memo lookup twice: {user_elaborator_line}"
    );
}

#[test]
fn user_elaborator_template_memo_replays_across_forced_modules() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache_with_prime_target(
            "pkg",
            "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n",
        ),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", second_value_user_elaborator_module());
    pkg.write(
        "pkg/a.kio",
        "module pkg/a;\n\n\
         import pkg(String);\n\n\
         import elaborators(second_value);\n\n\
         pub fn a(left: String, from_a: String) -> String { second_value!(left, from_a, String) }\n",
    );
    pkg.write(
        "pkg/b.kio",
        "module pkg/b;\n\n\
         import pkg(String);\n\n\
         import elaborators(second_value);\n\n\
         import pkg/a(a);\n\n\
         pub fn b(left: String, from_b: String) -> String { second_value!(a(left, left), from_b, String) }\n",
    );

    let (code, log) = kio_build_prime_with_extra_env(
        pkg.path(),
        &[("KIO_DEBUG_MEMO", "1"), ("KIO_DEBUG_MEMO_VERIFY", "1")],
    );
    assert_eq!(code, 0, "Kio' build should succeed: {log}");
    let user_elaborator_line = log
        .lines()
        .find(|line| {
            line.starts_with("memo user-elaborator: ")
                && line.contains("hits=1")
                && line.contains("misses=1")
                && line.contains("first-writes=1")
        })
        .unwrap_or_else(|| panic!("expected cross-module user-elaborator memo stats: {log}"));
    assert!(
        user_elaborator_line.contains("hits=1")
            && user_elaborator_line.contains("misses=1")
            && user_elaborator_line.contains("first-writes=1"),
        "same elaborator shape should replay from the in-process package memo in a dependent forced module: {user_elaborator_line}"
    );
    let emitted_b = fs::read_to_string(pkg.path().join("out/kio-prime/pkg/b.kio"))
        .expect("read emitted pkg/b Kio'");
    assert!(
        emitted_b.contains("pub fn b(left: String, from_b: String) -> String { from_b }"),
        "memo replay should substitute the second value from the current module: {emitted_b}"
    );
}

#[test]
fn user_elaborator_infer_branch_memo_hits_repeated_shape() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", source_infer_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_default);\n\n\
         pub fn run(s: String) -> String {\n\
           let first = id_default!(s, _);\n\
           let second = id_default!(first, _);\n\
           second\n\
         }\n",
    );

    let (code, log) = kio_check_with_extra_env(pkg.path(), false, &[("KIO_DEBUG_MEMO", "1")]);
    assert_eq!(code, 0, "check should succeed: {log}");
    let memo_line = log
        .lines()
        .find(|line| {
            line.starts_with("memo user-elaborator: ")
                && line.contains("hits=1")
                && line.contains("misses=1")
                && line.contains("first-writes=1")
        })
        .unwrap_or_else(|| panic!("expected user-elaborator memo stats: {log}"));
    assert!(
        memo_line.contains("hits=1")
            && memo_line.contains("misses=1")
            && memo_line.contains("first-writes=1"),
        "repeated inferred-target user-elaborator shape should reuse one result: {memo_line}"
    );
}

#[test]
fn user_elaborator_rechecks_do_not_write_persistent_template_cache() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", source_infer_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_default);\n\n\
         pub fn run(s: String) -> String { id_default!(s, _) }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_default);\n\n\
         pub fn run(s: String) -> String { id_default!(s, _) }\n\n\
         pub fn force_recheck() -> . { () }\n",
    );
    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "forced recheck should succeed: {log2}");

    let cache_dir = pkg.path().join("out/.kio-cache/user-elaborator");
    let template = cache_dir.join("template.bin");
    assert!(
        !cache_dir.exists() && !template.exists(),
        "user-elaborator templates must remain in-process only; found {} or {}",
        cache_dir.display(),
        template.display(),
    );
}

#[test]
fn typed_cache_misses_when_imported_user_elaborator_impl_changes() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", identity_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_user);\n\n\
         pub fn run(s: String) -> String { id_user!(s, String) }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "second check should warm-hit: {log2}");
    assert!(
        log2.contains("package-check-cache hit: pkg"),
        "second check should establish the warm cache path: {log2}"
    );

    pkg.write("elaborators.kio", unit_identity_user_elaborator_module());
    let (code3, log3) = kio_check(pkg.path());
    assert_ne!(
        code3, 0,
        "changing only the private elaborator helper must force pkg/main to re-typecheck: {log3}"
    );
    assert!(
        log3.contains("checked term of type `.`")
            && log3.contains("declared result type was `String`"),
        "the fresh user-elaborator error should be visible, not hidden by typed cache: {log3}"
    );
}

#[test]
fn typed_cache_misses_when_imported_user_elaborator_infer_branch_changes() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n  elaborators;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_string_env_module());
    pkg.write("elaborators.kio", source_infer_user_elaborator_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\n\
         import pkg(String);\n\n\
         import elaborators(id_default);\n\n\
         pub fn run(s: String) -> String {\n\
           let selected = id_default!(s, _);\n\
           selected\n\
         }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "second check should warm-hit: {log2}");
    assert!(
        log2.contains("package-check-cache hit: pkg"),
        "second check should establish the warm cache path: {log2}"
    );

    pkg.write("elaborators.kio", unit_infer_user_elaborator_module());
    let (code3, log3) = kio_check(pkg.path());
    assert_ne!(
        code3, 0,
        "changing only the private infer branch must force pkg/main to re-typecheck: {log3}"
    );
    assert!(
        log3.contains("expected `String`") || log3.contains("found `()`"),
        "the fresh user-elaborator infer-branch error should be visible, not hidden by typed cache: {log3}"
    );
}

#[test]
fn disabled_cache_rechecks_without_writing() {
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file_no_cache("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );

    let (code1, log1) = kio_check(pkg.path());
    assert_eq!(code1, 0, "first check should succeed: {log1}");
    assert!(
        !log1.contains("first-write") && !log1.contains("hit"),
        "disabled cache should not read or write: {log1}"
    );

    let (code2, log2) = kio_check(pkg.path());
    assert_eq!(code2, 0, "second check should succeed: {log2}");
    assert!(
        !log2.contains("first-write") && !log2.contains("hit"),
        "disabled cache should recheck silently: {log2}"
    );
    assert!(
        !pkg.path().join("out").join(".kio-cache").exists(),
        "disabled cache should not create the cache root"
    );
}

#[test]
fn source_edit_re_typechecks_the_package() {
    // Any source edit moves the package's content hash, so the
    // next check re-typechecks it rather than trusting the cache.
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );
    assert_eq!(kio_check(pkg.path()).0, 0);

    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { let x = (); x }\n",
    );
    let (code, log) = kio_check(pkg.path());
    assert_eq!(code, 0, "edited package should still typecheck: {log}");
    assert!(
        !log.contains("hit"),
        "edited package must not be skipped: {log}"
    );
    assert!(
        log.contains("re-checked"),
        "edited package should re-check: {log}"
    );

    // A third check over the (now-stable) edited source skips
    // again — the cache was refreshed by the re-check.
    let (code3, log3) = kio_check(pkg.path());
    assert_eq!(code3, 0);
    assert!(
        log3.contains("hit"),
        "third check over stable source should skip again: {log3}"
    );
}

#[test]
fn cache_hit_never_masks_a_fresh_error() {
    // A package validated and cached, then edited to introduce
    // an error, must surface that error — the edit moves the
    // content hash, so the cache is not consulted as a pass.
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );
    assert_eq!(kio_check(pkg.path()).0, 0);

    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { undefined_name }\n",
    );
    let (code, log) = kio_check(pkg.path());
    assert_eq!(
        code, 13,
        "the name-resolution error must surface, not be masked by the cache: {log}"
    );
}

#[test]
fn no_equiv_test_reuses_package_check_cache_when_warm() {
    // A no-equiv `kio test` reduces to the `kio check` pipeline plus the
    // step-5 "no equiv blocks found" line, so it warms the same
    // package-check cache and a warm second run skips typechecking —
    // this is what keeps a warm no-equiv `kio test` about as cheap as
    // `kio check` instead of re-typechecking every time.
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );

    let (code1, log1) = kio_test(pkg.path());
    assert_eq!(code1, 0, "first test should succeed: {log1}");
    assert!(
        log1.contains("no equiv blocks found"),
        "no-equiv package reports the step-5 line: {log1}"
    );

    let (code2, log2) = kio_test(pkg.path());
    assert_eq!(code2, 0, "second test should succeed: {log2}");
    assert!(
        log2.contains("no equiv blocks found"),
        "no-equiv package still reports the step-5 line: {log2}"
    );
    assert!(
        log2.contains("hit") && log2.contains("typecheck skipped"),
        "a warm no-equiv `kio test` must skip typechecking via the package-check cache: {log2}"
    );
}

#[test]
fn no_equiv_test_reuses_cache_warmed_by_check() {
    // Cross-command reuse: `kio check` warms the package-check cache, then
    // `kio test` over the same unchanged, equiv-free package skips
    // typechecking — `kio test` is fast after `kio check`.
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );

    let (code_check, log_check) = kio_check(pkg.path());
    assert_eq!(code_check, 0, "check should warm the cache: {log_check}");

    let (code_test, log_test) = kio_test(pkg.path());
    assert_eq!(code_test, 0, "test after check should succeed: {log_test}");
    assert!(
        log_test.contains("hit") && log_test.contains("typecheck skipped"),
        "`kio test` should reuse the cache `kio check` warmed: {log_test}"
    );
    assert!(
        log_test.contains("no equiv blocks found"),
        "no-equiv package reports the step-5 line: {log_test}"
    );
}

#[test]
fn no_equiv_test_surfaces_fresh_error_not_no_equiv_line() {
    // The no-equiv fast path runs the check pipeline, so a compile error
    // in an equiv-free package surfaces with its category code — it is
    // never masked as "no equiv blocks found" (spec cli.md § `kio test`
    // step 1 runs before step 5).
    let pkg = TempPkg::new();
    pkg.write(
        "pkg.pkg.kio",
        &package_file("pkg", "bridge {\n  pkg;\n  pkg/**;\n}\n"),
    );
    pkg.write("pkg.kio", pkg_root_module());
    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { () }\n",
    );
    assert_eq!(kio_test(pkg.path()).0, 0);

    pkg.write(
        "pkg/main.kio",
        "module pkg/main;\n\npub fn run() -> . { undefined_name }\n",
    );
    let (code, log) = kio_test(pkg.path());
    assert_eq!(
        code, 13,
        "the name-resolution error must surface from `kio test`: {log}"
    );
    assert!(
        !log.contains("no equiv blocks found"),
        "a compile error must not be masked by the no-equiv report: {log}"
    );
}

#[test]
fn no_equiv_test_fast_path_still_reports_skipped_dependency_equivs() {
    // The consumer declares no equiv of its own — so the no-equiv fast
    // path handles it — but a materialized dependency module carries one.
    // The default run holds the dependency block back, and the fast path
    // must still emit the one-line skip note (the census counts the
    // dependency block as skipped, not as in scope).
    let pkg = TempPkg::new();
    pkg.write(
        "app.pkg.kio",
        &package_file("app", "bridge {\n  main;\n}\n"),
    );
    pkg.write(
        "libdep.dep.kio",
        "dependency libdep;\n\nsource {\n  path \"../libdep/libdep.pkg.kio\";\n}\n",
    );
    pkg.write(
        "libdep/lib.kio",
        "module libdep/lib;\n\n\
         pub fn unit() -> . { () }\n\n\
         equiv lib_unit_law() {\n  unit();\n  ()\n}\n",
    );
    pkg.write(
        "main.kio",
        "module main;\n\nimport libdep/lib as lib;\n\npub fn run() -> . { lib.unit() }\n",
    );

    let (code, log) = kio_test(pkg.path());
    assert_eq!(
        code, 0,
        "an equiv-free consumer with an equiv-bearing dependency should pass: {log}"
    );
    assert!(
        log.contains("no equiv blocks found"),
        "the consumer's own report is the step-5 line: {log}"
    );
    assert!(
        log.contains(
            "note: 1 equiv block in dependency modules skipped; pass --include-deps to include"
        ),
        "the fast path must still report the held-back dependency equiv block: {log}"
    );
}

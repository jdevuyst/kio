//! End-to-end tests for `kio build` workspace discovery and target selection.
//!
//! These use the real `kio` and `kio-prime` binaries because the contract is
//! the CLI-to-filesystem route: package discovery, independent output roots,
//! and unsupported-target selection cannot be proved by an emitter unit test.

#![cfg(all(feature = "surface", feature = "prime", feature = "cli"))]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use support::test_binary;

use tempfile::TempDir;

fn binaries() -> [(&'static str, PathBuf); 2] {
    [
        ("kio", test_binary!("kio")),
        ("kio-prime", test_binary!("kio-prime")),
    ]
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("create fixture directory");
    fs::write(path, contents).expect("write fixture file");
}

fn run(binary: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| panic!("run `{}`: {error}", args.join(" ")))
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed: status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn public_files(dir: &Path) -> Vec<String> {
    let mut files = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .filter_map(|entry| {
            let entry = entry.expect("read output entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.') && entry.path().is_file()).then_some(name)
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn write_prime_package(root: &Path, package: &str, main: &str) {
    write(
        &root.join(format!("{package}.pkg.kio")),
        &format!(
            "package {package};\n\n\
             build {{\n\
             \x20 cache ();\n\n\
             \x20 target kio-prime {{\n\
             \x20   out \"out/kio-prime/\";\n\
             \x20 }}\n\
             }}\n\n\
             bridge {{\n\
             \x20 main;\n\
             }}\n"
        ),
    );
    write(&root.join("main.kio"), main);
}

fn write_skip_package(root: &Path, package: &str, targets: &str) {
    write(
        &root.join(format!("{package}.pkg.kio")),
        &format!(
            "package {package};\n\n\
             build {{\n\
             \x20 cache ();\n\n\
             {targets}\
             }}\n\n\
             bridge {{\n\
             \x20 main;\n\
             }}\n"
        ),
    );
    write(
        &root.join("main.kio"),
        "module main;\n\nfn empty() -> . { () }\n",
    );
}

fn warning_count(stderr: &str) -> usize {
    stderr
        .matches("warning: skipping target 'wasm': no backend in this kio build")
        .count()
}

#[test]
fn build_without_selectors_recursively_builds_every_discovered_package() {
    for (label, binary) in binaries() {
        let workspace = TempDir::new().expect("create workspace");
        let pkg_a = workspace.path().join("apps/pkg_a");
        let pkg_b = workspace.path().join("libs/nested/pkg_b");

        write_prime_package(
            &pkg_a,
            "pkg_a",
            "module main;\n\nhost type Int role(i32);\nhost fn show(p0: Int) -> .;\n",
        );
        write_prime_package(
            &pkg_b,
            "pkg_b",
            "module main;\n\nhost type Str role(str);\nhost fn emit(p0: Str) -> .;\n",
        );
        write(
            &workspace.path().join("distractor/README.txt"),
            "this directory is not a package\n",
        );

        assert!(!pkg_a.join("out").exists());
        assert!(!pkg_b.join("out").exists());

        let output = run(&binary, workspace.path(), &["build", "--no-cache"]);
        assert_success(label, &output);

        for (root, package, sentinel) in [
            (&pkg_a, "pkg_a", "host type Int role(i32);"),
            (&pkg_b, "pkg_b", "host type Str role(str);"),
        ] {
            let emitted = root.join("out/kio-prime");
            assert_eq!(
                public_files(&emitted),
                vec!["main.kio".to_owned(), format!("{package}.pkg.kio")],
                "{label} emitted an unexpected public file set for {package}",
            );
            let main =
                fs::read_to_string(emitted.join("main.kio")).expect("read emitted main module");
            assert!(main.contains(sentinel), "{label} lost {package}'s sentinel");
            let manifest = fs::read_to_string(emitted.join(format!("{package}.pkg.kio")))
                .expect("read emitted package file");
            assert!(
                manifest.starts_with(&format!("package {package};")),
                "{label} emitted the wrong package manifest for {package}"
            );
        }

        assert!(!workspace.path().join("out").exists());
        assert!(!workspace.path().join("distractor/out").exists());
    }
}

#[test]
fn skip_unsupported_with_supported_target_builds_only_supported_output() {
    for (label, binary) in binaries() {
        let package = TempDir::new().expect("create package");
        write_skip_package(
            package.path(),
            "skip_mixed",
            "  target js {\n    out \"out/js/\";\n  };\n\n  target wasm {\n    out \"out/wasm/\";\n  }\n",
        );

        let output = run(
            &binary,
            package.path(),
            &["build", "--skip-unsupported-targets", "--no-cache"],
        );
        assert_success(label, &output);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(warning_count(&stderr), 1, "{label}: stderr={stderr}");
        assert_eq!(
            public_files(&package.path().join("out/js")),
            vec!["skip_mixed.js".to_owned()],
        );
        assert!(!package.path().join("out/wasm").exists());
    }
}

#[test]
fn skip_unsupported_allows_zero_built_targets() {
    for (label, binary) in binaries() {
        let package = TempDir::new().expect("create package");
        write_skip_package(
            package.path(),
            "skip_all",
            "  target wasm {\n    out \"out/wasm/\";\n  }\n",
        );

        let output = run(
            &binary,
            package.path(),
            &["build", "--skip-unsupported-targets", "--no-cache"],
        );
        assert_success(label, &output);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(warning_count(&stderr), 1, "{label}: stderr={stderr}");
        assert!(!package.path().join("out").exists());
    }
}

#[test]
fn skip_unsupported_does_not_apply_to_explicit_target() {
    for (label, binary) in binaries() {
        let package = TempDir::new().expect("create package");
        write_skip_package(
            package.path(),
            "skip_explicit",
            "  target wasm {\n    out \"out/wasm/\";\n  }\n",
        );

        let output = run(
            &binary,
            package.path(),
            &["build", "wasm", "--skip-unsupported-targets", "--no-cache"],
        );
        assert_eq!(
            output.status.code(),
            Some(40),
            "{label}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("warning: skipping target"));
        assert!(!package.path().join("out").exists());
    }
}

fn assert_build_rejected(label: &str, output: &Output, cause: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(40), "{label}: {stderr}");
    assert!(stderr.contains(cause), "{label}: {stderr}");
}

#[test]
fn every_backend_rejects_output_containing_source_before_writes() {
    for (label, binary) in binaries() {
        for target in [
            "js",
            "ts",
            "go",
            "python",
            "java",
            "swift",
            "haskell",
            "rust",
            "kio-prime",
        ] {
            let package = TempDir::new().unwrap();
            write_skip_package(
                package.path(),
                "app",
                &format!("target {target} {{ out \"src\"; }}"),
            );
            let source = "module src/extra;\n";
            write(&package.path().join("src/extra.kio"), source);
            write(&package.path().join("src/sentinel.txt"), "keep me");
            let output = run(&binary, package.path(), &["build"]);
            assert_build_rejected(label, &output, "contains input");
            assert_eq!(
                fs::read_to_string(package.path().join("src/extra.kio")).unwrap(),
                source
            );
            assert_eq!(
                fs::read_to_string(package.path().join("src/sentinel.txt")).unwrap(),
                "keep me"
            );
        }
    }
}

#[test]
fn selected_output_overlap_is_global_but_unselected_targets_are_ignored() {
    for (label, binary) in binaries() {
        for second in ["out", "out/nested", "out/unused/../nested/.."] {
            let package = TempDir::new().unwrap();
            write_skip_package(
                package.path(),
                "app",
                &format!("target js {{ out \"out\"; }}; target rust {{ out \"{second}\"; }}"),
            );
            write(&package.path().join("out/sentinel.txt"), "keep me");
            assert_build_rejected(
                label,
                &run(&binary, package.path(), &["build"]),
                "conflicting output directories",
            );
            assert_eq!(
                fs::read_to_string(package.path().join("out/sentinel.txt")).unwrap(),
                "keep me"
            );
            assert_success(label, &run(&binary, package.path(), &["build", "js", "js"]));
            assert!(package.path().join("out/app.js").is_file());
        }
    }
}

#[test]
fn cross_package_preflight_protects_outputs_and_other_package_inputs() {
    for (label, binary) in binaries() {
        let workspace = TempDir::new().unwrap();
        let a = workspace.path().join("a");
        let b = workspace.path().join("b");
        let safe = workspace.path().join("safe");
        write_skip_package(&a, "a", "target js { out \"../shared\"; }");
        write_skip_package(&b, "b", "target js { out \"../shared/child\"; }");
        write_skip_package(&safe, "safe", "target js { out \"out\"; }");
        write(&safe.join("out/sentinel.txt"), "keep safe output");
        write(&workspace.path().join("shared/sentinel.txt"), "keep me");
        assert_build_rejected(
            label,
            &run(&binary, workspace.path(), &["build"]),
            "conflicting output directories",
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("shared/sentinel.txt")).unwrap(),
            "keep me"
        );
        assert_eq!(
            fs::read_to_string(safe.join("out/sentinel.txt")).unwrap(),
            "keep safe output"
        );
        assert_success(label, &run(&binary, workspace.path(), &["build", "./a"]));
        assert!(workspace.path().join("shared/a.js").is_file());
        assert_eq!(
            fs::read_to_string(safe.join("out/sentinel.txt")).unwrap(),
            "keep safe output"
        );
        write_skip_package(&a, "a", "target js { out \"../b\"; }");
        let manifest = fs::read(b.join("b.pkg.kio")).unwrap();
        assert_build_rejected(
            label,
            &run(&binary, workspace.path(), &["build"]),
            "contains input",
        );
        assert_eq!(fs::read(b.join("b.pkg.kio")).unwrap(), manifest);
        assert!(!b.join("out").exists());
    }
}

#[test]
fn independent_package_error_keeps_safe_sibling_builds_and_external_output() {
    for (label, binary) in binaries() {
        let workspace = TempDir::new().unwrap();
        let a = workspace.path().join("a");
        let b = workspace.path().join("b");
        write_skip_package(&a, "a", "target js { out \"../external\"; }");
        write(&b.join("b.pkg.kio"), "package b; build { invalid }");
        let output = run(&binary, workspace.path(), &["build"]);
        assert_eq!(
            output.status.code(),
            Some(11),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(workspace.path().join("external/a.js").is_file());
    }
}

#[test]
fn warm_artifact_restore_cannot_erase_newly_collected_input() {
    for (label, binary) in binaries() {
        let package = TempDir::new().unwrap();
        write_skip_package(package.path(), "app", "target js { out \"out/js\"; }");
        let manifest = package.path().join("app.pkg.kio");
        write(
            &manifest,
            &fs::read_to_string(&manifest)
                .unwrap()
                .replace("cache ();", "cache \"cache\";"),
        );
        let timed_build = || {
            Command::new(&binary)
                .arg("build")
                .env("KIO_DEBUG_TIMING", "build")
                .current_dir(package.path())
                .output()
                .unwrap()
        };
        assert_success(label, &timed_build());
        let warm = timed_build();
        assert_success(label, &warm);
        let stderr = String::from_utf8_lossy(&warm.stderr);
        assert!(
            stderr.contains("artifact_hit=1"),
            "{label}: expected a real artifact hit: {stderr}"
        );
        let source = "module out/js/preserved;\n";
        write(&package.path().join("out/js/preserved.kio"), source);
        let output = timed_build();
        assert_build_rejected(label, &output, "contains input");
        assert_eq!(
            fs::read_to_string(package.path().join("out/js/preserved.kio")).unwrap(),
            source
        );
        assert!(package.path().join("out/js/app.js").is_file());
    }
}

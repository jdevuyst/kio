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

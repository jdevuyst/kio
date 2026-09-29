//! Imported signature requirements through full and Prime compiler CLIs.

#![cfg(all(feature = "surface", feature = "prime", feature = "cli"))]

mod support;

use std::{fs, path::Path, process::Command};
use support::test_binary;
use tempfile::TempDir;

fn check_control(binary: &Path, source: &str) -> String {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("probe.pkg.kio"),
        "package probe;\nbridge { main; }\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("provider.kio"),
        concat!(
            "module provider;\n",
            "pub newtype W : . { pub constructor mk; pub projector get; };\n",
            "pub fn take(value: W) -> . { () }\n",
            "pub fn poly[A](value: W) -> . { () }\n",
            "pub fn pass[A](value: A) -> A { value }\n",
            "pub fn later(value: .) -> W -> . { .(x: W) -> . { () } }\n",
        ),
    )
    .unwrap();
    fs::write(fixture.path().join("main.kio"), source).unwrap();
    let output = Command::new(binary)
        .args(["check", "--no-cache"])
        .current_dir(fixture.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(14), "{stderr}");
    stderr
}

#[test]
fn imported_parameter_mismatch_names_the_signature_file_not_the_type_definition() {
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        for (import, body) in [
            ("import provider as p;", "p.take(())"),
            ("import provider as p;", "let f = p.take; f(())"),
            ("import provider(take);", "let f = take; f(())"),
        ] {
            let fixture = TempDir::new().unwrap();
            fs::write(
                fixture.path().join("probe.pkg.kio"),
                "package probe;\nbridge { main; }\n",
            )
            .unwrap();
            fs::write(
                fixture.path().join("origin.kio"),
                "module origin;\npub newtype W : . { pub constructor mk; pub projector get; };\n",
            )
            .unwrap();
            let provider = "module provider;\nimport origin as o;\npub type W = o.W;\n\npub fn take(value: W) -> . { () }\n";
            fs::write(fixture.path().join("provider.kio"), provider).unwrap();
            fs::write(fixture.path().join("main.kio"), format!("module main;\n{import}\n// Caller-only padding makes a foreign annotation offset land on unrelated source text instead of the declaration.\nfn caller() -> . {{ {body} }}\n")).unwrap();
            let output = Command::new(&binary)
                .args(["check", "--no-cache"])
                .current_dir(fixture.path())
                .output()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(14), "{stderr}");
            assert!(output.stdout.is_empty());
            assert!(stderr.contains("--> provider.kio:5:20"), "{stderr}");
            assert!(stderr.contains("pub fn take(value: W)"), "{stderr}");
            assert!(!stderr.contains("Caller-only padding"), "{stderr}");
            assert!(
                !stderr.contains("--> origin.kio:"),
                "the nominal definition did not establish this requirement: {stderr}"
            );
        }
    }
}

#[test]
fn imported_requirement_sites_do_not_replace_local_or_argument_internal_constraints() {
    for (binary, body) in [
        (test_binary!("kio"), "let .(f: p.W -> .) = p.take; f(())"),
        (test_binary!("kio"), "p.take(local(()))"),
        (
            test_binary!("kio"),
            "let f = p.pass(p.W -> ., p.take); f(())",
        ),
        (test_binary!("kio-prime"), "p.take(local(()))"),
        (
            test_binary!("kio-prime"),
            "let f = p.pass(p.W -> ., p.take); f(())",
        ),
    ] {
        let source = format!(
            "module main;\nimport provider as p;\nfn local(value: . -> .) -> p.W {{ p.W.mk(()) }}\nfn caller() -> . {{ {body} }}\n"
        );
        let stderr = check_control(&binary, &source);
        assert!(
            !stderr.contains("--> provider.kio:"),
            "a local constraint must not be attributed to the imported callee: {stderr}"
        );
        assert!(
            stderr.contains("because of this"),
            "the useful known context must remain: {stderr}"
        );
    }
}

#[test]
fn imported_requirement_site_survives_a_written_residual_function_layer() {
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        let stderr = check_control(
            &binary,
            "module main;\nimport provider as p;\n// Caller padding makes a foreign source span visibly unrelated to its annotation.\nfn caller() -> . { let f = p.later(()); f(()) }\n",
        );
        assert!(stderr.contains("--> provider.kio:"), "{stderr}");
        assert!(
            stderr.contains("pub fn later"),
            "the written residual signature establishes this requirement: {stderr}"
        );
    }
}

#[test]
fn imported_requirement_site_survives_explicit_universal_arguments() {
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        let stderr = check_control(
            &binary,
            "module main;\nimport provider as p;\n// Caller padding makes a foreign source span visibly unrelated to its annotation.\nfn caller() -> . { let f = p.poly; f(., ()) }\n",
        );
        assert!(
            stderr.contains("--> provider.kio:"),
            "{}: {stderr}",
            binary.display()
        );
        assert!(stderr.contains("pub fn poly"), "{stderr}");
    }
}

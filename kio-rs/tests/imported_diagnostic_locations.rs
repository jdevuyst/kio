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

fn check_host_control(binary: &Path, provider: &str, source: &str, expected_exit: i32) -> String {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("probe.pkg.kio"),
        "package probe;\nbridge { main; provider; origin; }\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("origin.kio"),
        "module origin;\npub host type W;\n",
    )
    .unwrap();
    fs::write(fixture.path().join("provider.kio"), provider).unwrap();
    fs::write(fixture.path().join("main.kio"), source).unwrap();
    let output = Command::new(binary)
        .args(["check", "--no-cache"])
        .current_dir(fixture.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(expected_exit), "{stderr}");
    assert!(output.stdout.is_empty());
    stderr
}

#[test]
fn host_parameter_requirements_name_the_written_signature_through_aliases() {
    let provider = "module provider;\nimport origin as o;\npub type W = o.W;\n\npub host fn take(value: W) -> .;\n";
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        for (import, body) in [
            ("import provider as p;", "p.take(())"),
            ("import provider as p;", "let f = p.take; f(())"),
            ("import provider(take);", "let f = take; f(())"),
        ] {
            let source = format!(
                "module main;\n{import}\n// Caller-only padding places its call beyond the short provider's declaration.\nfn caller() -> . {{ {body} }}\n"
            );
            let stderr = check_host_control(&binary, provider, &source, 14);
            assert!(stderr.contains("--> provider.kio:5:25"), "{stderr}");
            assert!(stderr.contains("pub host fn take(value: W)"), "{stderr}");
            assert!(!stderr.contains("--> origin.kio:"), "{stderr}");
            assert!(!stderr.contains("Caller-only padding"), "{stderr}");
        }
    }
}

#[test]
fn host_nullary_requirement_uses_the_provider_header_not_foreign_call_offsets() {
    let provider = "module provider;\npub host fn tick() -> .;\n";
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        let source = "module main;\nimport provider as p;\n// Caller-only padding is deliberately much longer than the entire provider file.\nfn caller() -> . { p.tick(p.tick) }\n";
        let stderr = check_host_control(&binary, provider, source, 14);
        assert!(stderr.contains("--> provider.kio:2:10"), "{stderr}");
        assert!(stderr.contains("pub host fn tick() -> .;"), "{stderr}");
        assert!(!stderr.contains("Caller-only padding"), "{stderr}");
        check_host_control(
            &binary,
            provider,
            &source.replace("p.tick(p.tick)", "p.tick()"),
            0,
        );
    }
}

#[test]
fn ordinary_nullary_requirement_uses_the_available_provider_header() {
    let provider = "module provider;\npub fn tick() -> . { () }\n";
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        let source = "module main;\nimport provider as p;\n// Caller-only padding is deliberately much longer than the entire provider file.\nfn caller() -> . { p.tick(p.tick) }\n";
        let stderr = check_host_control(&binary, provider, source, 14);
        assert!(stderr.contains("--> provider.kio:2:"), "{stderr}");
        assert!(stderr.contains("pub fn tick() -> . { () }"), "{stderr}");
        assert!(!stderr.contains("source unavailable"), "{stderr}");
        check_host_control(
            &binary,
            provider,
            &source.replace("p.tick(p.tick)", "p.tick()"),
            0,
        );
    }
}

#[test]
fn host_requirement_context_survives_generic_and_returned_function_layers() {
    let provider = concat!(
        "module provider;\nimport origin as o;\npub type W = o.W;\n",
        "pub host fn poly[A](value: W) -> .;\n",
        "pub host fn later(value: .) -> W -> .;\n",
    );
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        for (body, declaration) in [
            ("let f = p.poly; f(., ())", "pub host fn poly"),
            ("let f = p.later(()); f(())", "pub host fn later"),
        ] {
            let source = format!(
                "module main;\nimport provider as p;\n// Caller padding keeps call offsets distinct from every provider annotation.\nfn caller() -> . {{ {body} }}\n"
            );
            let stderr = check_host_control(&binary, provider, &source, 14);
            assert!(stderr.contains("--> provider.kio:"), "{stderr}");
            assert!(stderr.contains(declaration), "{stderr}");
        }
    }
}

#[test]
fn host_requirements_do_not_override_local_or_argument_internal_constraints() {
    let provider = concat!(
        "module provider;\nimport origin as o;\npub type W = o.W;\n",
        "pub host fn take(value: W) -> .;\n",
        "pub host fn make(value: .) -> W;\n",
        "pub host fn pass[A](value: A) -> A;\n",
    );
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        for body in [
            "p.take(local(()))",
            "let f = p.pass(p.W -> ., p.take); f(())",
            "let f = wrapper(()); f(())",
        ] {
            let source = format!(
                "module main;\nimport provider as p;\nfn local(value: . -> .) -> p.W {{ p.make(()) }}\nfn wrapper(value: .) -> p.W -> . {{ p.take }}\nfn caller() -> . {{ {body} }}\n"
            );
            let stderr = check_host_control(&binary, provider, &source, 14);
            assert!(!stderr.contains("--> provider.kio:"), "{stderr}");
            assert!(stderr.contains("because of this"), "{stderr}");
        }
    }
    let source = "module main;\nimport provider as p;\nfn caller() -> . { let .(f: p.W -> .) = p.take; f(()) }\n";
    let stderr = check_host_control(&test_binary!("kio"), provider, source, 14);
    assert!(!stderr.contains("--> provider.kio:"), "{stderr}");
}

#[test]
fn same_module_host_requirement_retains_its_written_annotation() {
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        let source = "module main;\nhost type W;\nhost fn take(value: W) -> .;\n\nfn caller() -> . { take(()) }\n";
        let stderr = check_host_control(&binary, "module provider;\n", source, 14);
        assert!(stderr.contains("host fn take(value: W)"), "{stderr}");
        assert!(!stderr.contains("--> provider.kio:"), "{stderr}");
    }
}

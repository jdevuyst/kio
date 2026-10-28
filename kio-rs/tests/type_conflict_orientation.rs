//! Inferred conflicts describe the type at the primary expression.

#![cfg(all(feature = "surface", feature = "cli"))]

mod support;

use std::{fs, process::Command};
use support::test_binary;
use tempfile::TempDir;

fn check(source: &str) -> (i32, String) {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("probe.pkg.kio"),
        "package probe;\nbridge { main; control; }\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("control.kio"),
        include_str!("../../test-data/poc/macros/workdir/control.kio"),
    )
    .unwrap();
    fs::write(fixture.path().join("main.kio"), source).unwrap();
    let output = Command::new(test_binary!("kio"))
        .args(["check", "--no-cache"])
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert!(output.stdout.is_empty());
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn independent_results_report_the_incoming_expression_as_found() {
    for (first, second) in [("Left", "Right"), ("Right", "Left")] {
        let first_value = first.to_ascii_lowercase();
        let second_value = second.to_ascii_lowercase();
        let source = format!(
            "module main;\nimport control(Bool, macro if);\nhost type Left;\nhost type Right;\nhost fn left() -> Left;\nhost fn right() -> Right;\nfn caller(c: Bool) {{ let result = if(c) {{ {first_value}() }} else {{ {second_value}() }}; () }}\n"
        );
        let (status, stderr) = check(&source);
        assert_eq!(status, 14, "{stderr}");
        assert!(
            stderr.contains(&format!("expected `{first}`, found `{second}`")),
            "{stderr}"
        );
        let line = source.lines().last().unwrap();
        let column = line.rfind(&format!("{second_value}()")).unwrap() + 1;
        let line_number = source.lines().count();
        assert!(
            stderr.starts_with(&format!("main.kio:{line_number}:{column}: error:")),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!("expected `{first}` because of this")),
            "{stderr}"
        );
    }
}

#[test]
fn ordinary_shared_arguments_keep_their_directional_requirement() {
    for (arguments, expected, found) in [("(), W.mk(())", ".", "W"), ("W.mk(()), ()", "W", ".")] {
        let source = format!(
            "module main;\nnewtype W : . {{ constructor mk; projector get; }};\nfn same[A](a: A, b: A) -> A {{ a }}\nfn caller() {{ let result = same({arguments}); () }}\n"
        );
        let (status, stderr) = check(&source);
        assert_eq!(status, 14, "{stderr}");
        assert!(
            stderr.contains(&format!("expected `{expected}`, found `{found}`")),
            "{stderr}"
        );
        let line = source.lines().last().unwrap();
        let first_column = line.find("same(").unwrap() + "same(".len();
        let second_column = first_column + arguments.split_once(", ").unwrap().0.len() + 2;
        assert!(
            stderr.starts_with(&format!("main.kio:4:{}: error:", second_column + 1)),
            "{stderr}"
        );
        let cause = stderr
            .lines()
            .find(|line| line.ends_with(&format!("expected `{expected}` because of this")))
            .unwrap_or_else(|| panic!("missing expected-type source: {stderr}"));
        assert!(
            cause.starts_with(&format!("  | {}-", " ".repeat(first_column))),
            "{stderr}"
        );
    }
}

#[test]
fn matching_results_and_arguments_remain_valid() {
    for source in [
        "module main;\nimport control(Bool, macro if);\nhost type Left;\nhost fn left() -> Left;\nfn caller(c: Bool) { let result = if(c) { left() } else { left() }; () }\n",
        "module main;\nfn same[A](a: A, b: A) -> A { a }\nfn caller() { same((), ()) }\n",
    ] {
        let (status, stderr) = check(source);
        assert_eq!(status, 0, "{stderr}");
    }
}

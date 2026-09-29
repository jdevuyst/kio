//! Source excerpts and their coordinates through the real compiler CLI.

#![cfg(all(feature = "surface", feature = "prime", feature = "cli"))]

mod support;

use std::fs;
use std::process::Command;
use support::test_binary;

use tempfile::TempDir;

#[test]
fn tabbed_type_mismatch_excerpts_align_without_changing_source_columns() {
    for binary in [test_binary!("kio"), test_binary!("kio-prime")] {
        for (annotation, body, expected_column) in [("W", "\t()", 2), ("\tW", " \t\t()", 4)] {
            let fixture = TempDir::new().expect("create diagnostic fixture");
            fs::write(
                fixture.path().join("probe.pkg.kio"),
                "package probe;\nbridge { main; }\n",
            )
            .unwrap();
            fs::write(
                fixture.path().join("main.kio"),
                format!(
                    "module main;\nnewtype W : . {{ constructor mk; projector get; }};\nfn caller() -> {annotation} {{\n{body}\n}}\n"
                ),
            ).unwrap();
            let output = Command::new(&binary)
                .args(["check", "--no-cache"])
                .current_dir(fixture.path())
                .output()
                .expect("run compiler");
            let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
            assert_eq!(output.status.code(), Some(14), "{stderr}");
            assert!(output.stdout.is_empty(), "diagnostics belong on stderr");
            assert!(!stderr.contains('\x1b'), "captured output must be plain");
            assert!(
                !stderr.contains('\t'),
                "tabs must not depend on the terminal: {stderr}"
            );
            assert!(
                stderr.contains(&format!("main.kio:4:{expected_column}: error:")),
                "{stderr}"
            );
            let lines = stderr.lines().collect::<Vec<_>>();
            let primary = lines
                .iter()
                .position(|line| line.starts_with("4 | "))
                .unwrap();
            assert_eq!(
                lines[primary].find("()"),
                lines[primary + 1].find("^^"),
                "{stderr}"
            );
            let secondary = lines
                .iter()
                .position(|line| line.starts_with("3 | "))
                .unwrap();
            assert_eq!(
                lines[secondary].find('W'),
                lines[secondary + 1].find('-'),
                "{stderr}"
            );
        }
    }
}

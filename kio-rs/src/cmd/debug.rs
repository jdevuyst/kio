//! Internal `kio debug` subcommand namespace.
//!
//! Hosts the dumpers that CI utilities and downstream highlighter
//! implementations consume: `tokens` provides the canonical token-kind
//! vocabulary, `kio-prime-roundtrip-package` prepares copied package
//! manifests for the Kio' artifact check, and surface builds also provide
//! the generated `builtin-docs` reference.
//!
//! **Internal-only** — same convention as `kio-prime`. Not
//! documented in `specs/cli.md`, not advertised in `--help` for
//! the public `kio` binary; the dispatch carries it for tooling that
//! knows the spelling.

use std::fs;
use std::path::PathBuf;

use crate::ast::{TargetBlock, TargetEntry};
use crate::exit_code::ExitCode;
use crate::package_collection::parse_module_file_with_file_context;
use crate::pass::parser::parse_package_file;
use crate::pretty::pretty_package_file;
use crate::tokens;

#[cfg(feature = "surface")]
const HELP: &str = "\
Usage: kio debug <subcommand> [args]

Internal dumpers for CI and tooling. Not part of the public CLI
surface (specs/cli.md does not specify these); the spellings may
change without notice.

Subcommands:
  builtin-docs   Print the generated Markdown reference for
                 compiler-provided builtin modules.
  kio-prime-roundtrip-package <file> <target>
                 Prepare a copied package manifest for the internal
                 Kio' artifact round-trip check.
  tokens <file>   Print the file's source tokenization as a
                  JSON array of { start, end, kind } records,
                  classified against the canonical token-kind
                  vocabulary. Used as the reference tokenization
                  downstream highlighter implementations are checked
                  against.

Options:
  -h, --help      Show this help and exit.";

#[cfg(not(feature = "surface"))]
const HELP: &str = "\
Usage: kio debug <subcommand> [args]

Internal dumpers for CI and tooling. Not part of the public CLI
surface (specs/cli.md does not specify these); the spellings may
change without notice.

Subcommands:
  kio-prime-roundtrip-package <file> <target>
                 Prepare a copied package manifest for the internal
                 Kio' artifact round-trip check.
  tokens <file>   Print the file's source tokenization as a
                  JSON array of { start, end, kind } records,
                  classified against the canonical token-kind
                  vocabulary. Used as the reference tokenization
                  downstream highlighter implementations are checked
                  against.

Options:
  -h, --help      Show this help and exit.";

const TOKENS_HELP: &str = "\
Usage: kio debug tokens <file>

Print the file's source tokenization as a JSON array of
{ start, end, kind } records — one entry per line, sorted by
start byte. The kind vocabulary is defined by the `TokenKind`
enum in `kio-rs/src/tokens.rs`.

Internal subcommand: not part of the public CLI surface.";

const ROUNDTRIP_TARGET_OUT: &str = "__kio_roundtrip_target/";
const ROUNDTRIP_PRIME_OUT: &str = "__kio_roundtrip_prime/";

pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return ExitCode::Success;
    }
    let Some(sub) = args.first() else {
        eprintln!("{HELP}");
        return ExitCode::Usage;
    };
    match sub.as_str() {
        #[cfg(feature = "surface")]
        "builtin-docs" => run_builtin_docs(&args[1..]),
        "kio-prime-roundtrip-package" => run_kio_prime_roundtrip_package(&args[1..]),
        "tokens" => run_tokens(&args[1..]),
        other => {
            eprintln!("error: unknown debug subcommand: {other}");
            eprintln!();
            eprintln!("{HELP}");
            ExitCode::Usage
        }
    }
}

fn run_kio_prime_roundtrip_package(args: &[String]) -> ExitCode {
    let [path, target] = args else {
        eprintln!("Usage: kio debug kio-prime-roundtrip-package <file> <target>");
        return ExitCode::Usage;
    };
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("error: cannot read {path}: {error}");
            return ExitCode::Internal;
        }
    };
    let stem = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(crate::file_kind::package_stem);
    match prepare_kio_prime_roundtrip_package(&source, stem, target) {
        Ok(Some(rendered)) => {
            print!("{rendered}");
            ExitCode::Success
        }
        Ok(None) => ExitCode::Success,
        Err(error) => {
            let (span, message) = error.diag();
            let (line, col) = line_col(&source, span.start);
            eprintln!("{path}:{line}:{col}: {message}");
            error.exit_code()
        }
    }
}

fn prepare_kio_prime_roundtrip_package(
    source: &str,
    stem: Option<&str>,
    selected_target: &str,
) -> Result<Option<String>, crate::error::Error> {
    let mut package = parse_package_file(source, stem)?;
    if selected_target == "kio-prime" {
        return Ok(None);
    }
    let Some(build) = package.build.as_mut() else {
        return Ok(None);
    };
    let Some(target) = build
        .targets
        .iter_mut()
        .find(|target| target.id == selected_target)
    else {
        return Ok(None);
    };
    set_target_output(target, ROUNDTRIP_TARGET_OUT);

    if let Some(target) = build
        .targets
        .iter_mut()
        .find(|target| target.id == "kio-prime")
    {
        set_target_output(target, ROUNDTRIP_PRIME_OUT);
    } else {
        build.targets.push(TargetBlock {
            trailing_trivia: Vec::new(),
            id: "kio-prime".to_owned(),
            entries: vec![TargetEntry {
                key: "out".to_owned(),
                value: ROUNDTRIP_PRIME_OUT.to_owned(),
                span: build.span,
                leading_trivia: Vec::new(),
            }],
            span: build.span,
            leading_trivia: Vec::new(),
        });
    }

    Ok(Some(pretty_package_file(&package)))
}

fn set_target_output(target: &mut TargetBlock, output: &str) {
    if let Some(entry) = target.entries.iter_mut().find(|entry| entry.key == "out") {
        entry.value = output.to_owned();
    } else {
        target.entries.push(TargetEntry {
            key: "out".to_owned(),
            value: output.to_owned(),
            span: target.span,
            leading_trivia: Vec::new(),
        });
    }
}

#[cfg(feature = "surface")]
fn run_builtin_docs(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("Usage: kio debug builtin-docs");
        println!();
        println!("Print the generated Markdown reference for compiler-provided builtin modules.");
        return ExitCode::Success;
    }
    if !args.is_empty() {
        eprintln!("error: `kio debug builtin-docs` takes no arguments");
        return ExitCode::Usage;
    }
    print!("{}", crate::builtin_docs::render_markdown_guide());
    ExitCode::Success
}

fn run_tokens(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{TOKENS_HELP}");
        return ExitCode::Success;
    }
    let path = match args {
        [p] => p,
        _ => {
            eprintln!("error: `kio debug tokens` requires exactly one <file> argument");
            eprintln!();
            eprintln!("{TOKENS_HELP}");
            return ExitCode::Usage;
        }
    };
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::Internal;
        }
    };
    let selected = PathBuf::from(path);
    let file_path = if selected.is_absolute() {
        selected
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&selected))
            .unwrap_or(selected)
    };
    let tokens = match classify_file_tokens(&source, &file_path) {
        Ok(ts) => ts,
        Err(err) => {
            // Lex failure routes through the same diagnostic shape
            // the regular pipeline uses, so editor integrations can
            // pick up the line/col for the broken byte. Exit code is
            // the lexer's own error code (parse-error tier per
            // specs/exit-codes.md).
            let (span, message) = err.diag();
            let (line, col) = line_col(&source, span.start);
            eprintln!("{path}:{line}:{col}: {message}");
            return err.exit_code();
        }
    };
    print!("{}", tokens::to_json(&tokens));
    ExitCode::Success
}

fn classify_file_tokens(
    source: &str,
    file_path: &std::path::Path,
) -> Result<Vec<tokens::ClassifiedToken>, crate::error::Error> {
    match parse_module_file_with_file_context(source, file_path) {
        Ok(file) => tokens::dump_with_module(source, &file.module),
        Err(_) => tokens::dump(source),
    }
}

/// Map a byte offset inside `source` to a 1-based (line, column)
/// pair. Mirror of [`crate::cmd::check::line_col`] kept here to avoid
/// pulling the check module into this internal dispatch path.
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let upto = (offset as usize).min(source.len());
    let prefix = &source[..upto];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..upto].chars().count() + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn help_returns_success() {
        assert_eq!(run(&argv(&["-h"])), ExitCode::Success);
        assert_eq!(run(&argv(&["--help"])), ExitCode::Success);
        assert_eq!(run(&argv(&["tokens", "--help"])), ExitCode::Success);
    }

    #[test]
    fn unknown_subcommand_returns_usage() {
        assert_eq!(run(&argv(&["nonexistent"])), ExitCode::Usage);
    }

    #[test]
    fn tokens_without_file_returns_usage() {
        assert_eq!(run(&argv(&["tokens"])), ExitCode::Usage);
    }

    #[test]
    fn tokens_with_too_many_args_returns_usage() {
        assert_eq!(run(&argv(&["tokens", "a.kio", "b.kio"])), ExitCode::Usage);
    }

    #[test]
    fn empty_argv_returns_usage() {
        assert_eq!(run(&[]), ExitCode::Usage);
    }

    #[test]
    fn roundtrip_package_uses_the_package_ast_and_is_silent_when_inapplicable() {
        let fixtures = [
            (
                "closing brace in comment",
                "package app;\nbuild {\n  target ts {\n    // }\n    out \"out/ts/\";\n  };\n  target kio-prime {}\n}\n",
            ),
            (
                "opening brace in string",
                "package app;\nbuild { target ts { out \"out/{ts}/\"; }; target kio-prime { out \"old/\"; } }\n",
            ),
            (
                "commented target decoy",
                "package app;\nbuild {\n  // target kio-prime {\n  target ts {}\n}\n",
            ),
        ];
        for (name, source) in fixtures {
            let rendered = prepare_kio_prime_roundtrip_package(source, Some("app"), "ts")
                .unwrap_or_else(|error| panic!("{name}: {error:?}"))
                .expect("fixture is applicable");
            let package = parse_package_file(&rendered, Some("app"))
                .unwrap_or_else(|error| panic!("{name}: {error:?}\n{rendered}"));
            let build = package.build.expect("prepared build block");
            for (id, output) in [
                ("ts", ROUNDTRIP_TARGET_OUT),
                ("kio-prime", ROUNDTRIP_PRIME_OUT),
            ] {
                let targets: Vec<_> = build
                    .targets
                    .iter()
                    .filter(|target| target.id == id)
                    .collect();
                assert_eq!(targets.len(), 1, "{name}: target {id}\n{rendered}");
                let outs: Vec<_> = targets[0]
                    .entries
                    .iter()
                    .filter(|entry| entry.key == "out")
                    .map(|entry| entry.value.as_str())
                    .collect();
                assert_eq!(outs, [output], "{name}: target {id}\n{rendered}");
            }
        }

        for (source, target) in [
            ("package app;\n", "ts"),
            (
                "package app;\nbuild { target js { out \"out/js/\"; } }\n",
                "ts",
            ),
            (
                "package app;\nbuild { target kio-prime { out \"out/prime/\"; } }\n",
                "kio-prime",
            ),
        ] {
            assert!(
                prepare_kio_prime_roundtrip_package(source, Some("app"), target)
                    .expect("valid inapplicable package")
                    .is_none()
            );
        }

        let temp = tempfile::tempdir().expect("temporary package directory");
        let args = [
            "kio-prime-roundtrip-package".to_owned(),
            temp.path().join("missing.pkg.kio").display().to_string(),
            "ts".to_owned(),
        ];
        assert_eq!(run(&args), ExitCode::Internal);
    }

    #[test]
    fn file_import_grammar_classifies_without_provider() {
        let temp = tempfile::tempdir().expect("source tree");
        let consumer = temp.path().join("a/b/c.kio");
        let provider = temp.path().join("a/d/e.kio");
        let source = "module a/b/c; import a/d/e(varop [% %]); fn make(x: A) -> A { x } fn run(value: A) -> A { let identity = .(inner: A) -> A { inner }; identity([% (make(value), value) %]) }";
        let offset = source.find("inner").expect("lambda parameter") as u32;
        std::fs::create_dir_all(provider.parent().unwrap()).unwrap();
        for present in [false, true] {
            if present {
                std::fs::write(&provider, "broken provider").unwrap();
            }
            let (tokens, attempts) = crate::package_collection::import_grammar_with_denied_reads(
                [provider.clone()],
                || classify_file_tokens(source, &consumer),
            );
            assert!(attempts.is_empty());
            let token = tokens
                .unwrap()
                .into_iter()
                .find(|token| token.span.start == offset)
                .unwrap();
            assert_eq!(token.kind, tokens::TokenKind::VariableParameter);
        }
    }
}

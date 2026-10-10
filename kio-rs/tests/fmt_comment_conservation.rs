#![cfg(all(feature = "surface", feature = "cli"))]

use std::fs;
use std::path::{Path, PathBuf};

use kio_lang::cmd::fmt::format_source;
use kio_lang::pass::lexer::{TokenKind, Trivia, lex_with_trailing};

const MARKER: &str = "kio_fmt_raw_";

fn marker_ids(source: &str) -> Vec<u64> {
    let (tokens, trailing) = lex_with_trailing(source).expect("comment probe must lex");
    tokens
        .into_iter()
        .flat_map(|token| token.leading_trivia)
        .chain(trailing)
        .filter_map(|trivia| match trivia {
            Trivia::LineComment { text, .. } => text
                .trim()
                .strip_prefix(MARKER)
                .and_then(|id| id.parse().ok()),
            _ => None,
        })
        .collect()
}

fn check_conserved(
    filename: &str,
    source: &str,
    expected: &[u64],
    ordered: bool,
) -> Result<(), String> {
    if marker_ids(source) != expected {
        return Err("injection produced incorrect markers".to_owned());
    }
    let formatted =
        format_source(filename, source).map_err(|error| format!("format failed: {error:?}"))?;
    let again = format_source(filename, &formatted)
        .map_err(|error| format!("reparse failed: {error:?}"))?;
    let mut actual = marker_ids(&formatted);
    if !ordered {
        actual.sort_unstable();
    }
    if actual != expected {
        return Err(format!("expected markers {expected:?}, found {actual:?}"));
    }
    if formatted != again {
        return Err("formatter did not reach a fixpoint".to_owned());
    }
    Ok(())
}

fn marker_contexts(source: &str) -> String {
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(MARKER))
        .map(|(index, _)| {
            let start = index.saturating_sub(1);
            let end = (index + 2).min(lines.len());
            lines[start..end]
                .iter()
                .enumerate()
                .map(|(offset, line)| format!("  {}: {line}", start + offset + 1))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_conserved(filename: &str, source: &str, expected: &[u64], ordered: bool) {
    check_conserved(filename, source, expected, ordered)
        .unwrap_or_else(|error| panic!("{filename}: {error}\n{}", marker_contexts(source)));
}

fn structural(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::Comma
            | TokenKind::Semicolon
    ) || matches!(kind, TokenKind::SymbolRun(run) if matches!(run.as_str(), "=" | ":" | "->"))
}

fn injectable_gaps(source: &str) -> Vec<usize> {
    let (tokens, _) = lex_with_trailing(source).expect("generated source must lex");
    tokens
        .windows(2)
        .filter_map(|pair| {
            let previous = &pair[0];
            let next = &pair[1];
            let gap = &source[previous.span.end as usize..next.span.start as usize];
            // Whole lexer tokens protect symbol runs and signed literals. Flush
            // identifier/symbol boundaries also protect `name!` and `.stem.`.
            (gap.chars().any(char::is_whitespace)
                || structural(&previous.kind)
                || structural(&next.kind))
            .then_some(next.span.start as usize)
        })
        .collect()
}

fn inject(source: &str, gaps: &[usize]) -> String {
    let mut injected = String::new();
    let mut previous = 0;
    for (index, &gap) in gaps.iter().enumerate() {
        injected.push_str(&source[previous..gap]);
        injected.push_str(&format!(" // {MARKER}{}\n", index + 1));
        previous = gap;
    }
    injected.push_str(&source[previous..]);
    let kinds = |text: &str| {
        lex_with_trailing(text)
            .expect("injection must retain lexical boundaries")
            .0
            .into_iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(source), kinds(&injected), "injection changed tokens");
    injected
}

fn kio_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read generated directory") {
        let entry = entry.expect("read generated entry");
        let kind = entry.file_type().expect("generated entry type");
        if kind.is_dir() {
            kio_files(&entry.path(), files);
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "kio") {
            files.push(entry.path());
        }
    }
}

#[test]
fn raw_generated_sources_conserve_sparse_seeded_comments() {
    let Some(directory) = std::env::var_os("KIO_DEBUG_FMT_RAW_GENERATED_DIR") else {
        return;
    };
    let seed = std::env::var("KIO_DEBUG_FMT_RAW_SEED")
        .expect("raw formatter probe seed")
        .parse::<u64>()
        .expect("raw formatter probe seed must be an unsigned integer");
    let mut state = seed;
    let mut files = Vec::new();
    kio_files(Path::new(&directory), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no generated Kio source files");
    let file_count = files.len();
    let mut checked = 0;
    let mut failures = Vec::new();
    for file in files {
        let filename = file.to_str().expect("generated path must be UTF-8");
        let relative = file.strip_prefix(Path::new(&directory)).unwrap().display();
        let source = fs::read_to_string(&file).expect("read raw generated source");
        if let Err(error) = format_source(filename, &source) {
            failures.push(format!("{relative}: initial format failed: {error:?}"));
            continue;
        }
        let candidates = injectable_gaps(&source);
        assert!(!candidates.is_empty(), "no comment gaps in {filename}");
        for variant in 0..3 {
            let mut available = candidates.clone();
            let count = available.len().min(variant + 1);
            let mut selected = Vec::new();
            for _ in 0..count {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                selected.push(available.swap_remove((state >> 32) as usize % available.len()));
            }
            selected.sort_unstable();
            let injected = inject(&source, &selected);
            let expected = (1..=count as u64).collect::<Vec<_>>();
            checked += 1;
            if let Err(error) = check_conserved(filename, &injected, &expected, false) {
                failures.push(format!(
                    "{relative}, variant {}: {error}\n{}",
                    variant + 1,
                    marker_contexts(&injected),
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures across {checked} variants in {file_count} files (seed {seed}):\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

#[test]
fn rewriting_boundaries_conserve_locally_ordered_comments() {
    for body in [
        "rec(loop // kio_fmt_raw_1\n) fn f() { // kio_fmt_raw_2\n() }",
        "fn f() -> (// kio_fmt_raw_1\n. // kio_fmt_raw_2\n) { () }",
        "host fn f() -> (// kio_fmt_raw_1\n. // kio_fmt_raw_2\n);",
        "fn f() { {ready = (// kio_fmt_raw_1\nready // kio_fmt_raw_2\n)} }",
        "fn f() { {ready = (// kio_fmt_raw_1\n) // kio_fmt_raw_2\n} }",
        "fn f() { row.!{ready = (// kio_fmt_raw_1\nother // kio_fmt_raw_2\n)} }",
        "fn f() { row.!{ready = (// kio_fmt_raw_1\n) // kio_fmt_raw_2\n} }",
        "fn f() { (row.?{ // kio_fmt_raw_1\n} // kio_fmt_raw_2\n).?{ready} }",
        "fn f() { (row.!{ // kio_fmt_raw_1\n} // kio_fmt_raw_2\n).?{ready} }",
        "fn f() { let .({ // kio_fmt_raw_1\nready // kio_fmt_raw_2\n}) = row; ready }",
        "fn f() { let .(// kio_fmt_raw_1\nx // kio_fmt_raw_2\n) = (); x }",
        "fn f((x: . // kio_fmt_raw_1\n, y: . // kio_fmt_raw_2\n)) { x }",
        "fn f() { .((x: . // kio_fmt_raw_1\n, y: . // kio_fmt_raw_2\n)) { x } }",
    ] {
        assert_conserved("m.kio", &format!("module m; {body}"), &[1, 2], true);
    }
}

#[test]
fn injection_reaches_delimiters_without_splitting_lexical_units() {
    let rec_header = "rec(loop)";
    assert!(injectable_gaps(rec_header).contains(&rec_header.find(')').unwrap()));
    let source = "name!(.stem., -12, \"a b ( )\") // existing (comment)\nleft ++ right";
    let gaps = injectable_gaps(source);
    for protected in [
        "name!",
        ".stem.",
        "-12",
        "\"a b ( )\"",
        "// existing (comment)",
        "++",
    ] {
        let start = source.find(protected).unwrap();
        let end = start + protected.len();
        assert!(
            !gaps.iter().any(|&gap| start < gap && gap < end),
            "split {protected}"
        );
    }
    inject(source, &gaps);
}

#[test]
fn marker_oracle_uses_exact_comment_ids_and_allows_import_reordering() {
    let source = "module m; // kio_fmt_raw_1\nimport z as z; \
                  // kio_fmt_raw_2\nimport a as a; \
                  literal message = \"kio_fmt_raw_3\";";
    assert_conserved("m.kio", source, &[1, 2], false);
    assert_eq!(marker_ids("// kio_fmt_raw_1\n// kio_fmt_raw_1\n"), [1, 1]);
    assert!(marker_ids("// kio_fmt_raw_1suffix\n").is_empty());
}

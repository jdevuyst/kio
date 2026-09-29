//! Implementation of `kio fmt`.
//!
//! Formats Kio-family source files to the canonical style emitted by the
//! pretty-printer. Runs in one of three modes:
//!
//! - **Rewrite** (default) — for each file the formatter visits, write
//!   the canonical form back to disk in place when it differs from
//!   what's there. Modified paths are listed on stdout, one per line.
//! - **Check** (`--check`) — never write. List paths whose canonical
//!   form differs from disk on stdout, one per line, and exit `60`
//!   if any do (`0` if the input is already canonical). Code `60`
//!   per the `6x` fmt-check tier in `specs/exit-codes.md`; differs
//!   from `gofmt -l` / `cargo fmt --check` (both `1`).
//! - **Stdin** (sole arg `-`) — read source from stdin, write canonical
//!   form to stdout. The file shape is inferred from the lone arg
//!   `-` — both `--check` and `-` consume the entire input as a regular
//!   `*.kio` module. Useful for editor integration.
//!
//! Path arguments select what to visit:
//!
//! - **No paths.** Walk the current directory recursively, formatting
//!   every Kio-family source file.
//! - **One or more files** — format each file independently.
//! - **One or more directories** — recursively format every Kio-family
//!   source file under each.
//! - **Mixed** — each arg processed independently in argv order.
//!
//! Modules are formatted from their written import grammars without reading
//! provider declarations. Selected file paths retain their declared module
//! context; stdin is formatted independently of any provider source tree.
//!
//! Per `specs/cli.md`: "Kio's formatting is opinionated and non-
//! configurable: there is one canonical style, and `kio fmt` produces
//! it." The canonical style is specified in `specs/style.md` (the
//! width-driven A1 comma layout, import-block ordering, literal
//! canonicalization, and the per-file-kind shapes for package,
//! signature, dependency, and lock files); this crate's pretty-printer
//! produces it.
//!
//! **Atomic writes.** When rewriting a file, the formatter writes the
//! canonical form to a sibling `<name>.kio.tmp-<pid>` file first and
//! then renames it over the original. A `kio fmt` killed mid-run
//! never leaves a half-written file.
//!
//! **Encoding.** Source must be valid UTF-8. CRLF / mixed line
//! endings normalize to LF on rewrite — a CRLF file is reported as
//! "would change" under `--check` even when no other reformatting is
//! needed.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::path_display::DisplayPath;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(feature = "surface")]
use crate::ast::KioFileKind;
use crate::error::Error;
use crate::exit_code::ExitCode;
use crate::package_collection::parse_module_file_with_file_context;
use crate::pass::parser::{
    parse_dependency_file, parse_lock_file, parse_module_file, parse_package_file,
    parse_signature_file,
};
use crate::pretty::{pretty_dependency_file, pretty_lock_file, pretty_module, pretty_package_file};
#[cfg(feature = "prime")]
use crate::prime::lower::{
    lower_module as prime_lower_module, lower_package_file as prime_lower_package_file,
};

const HELP_TEMPLATE: &str = "\
Usage: kio fmt [--check] [-] [<path>...]

Format Kio-family source files in place to the canonical style.

With no arguments, walks the current directory and formats every
Kio-family source file. Each file rewritten is listed on stdout, one
path per line. Idempotent: running twice
produces no further changes.

Path arguments may be files or directories; directories are walked
recursively. Mixed args are processed independently in argv order.

Modes:
  --check       Don't write. List paths whose canonical form
                differs from disk to stdout (one per line), and
                exit 60 if any do (0 if all are already canonical).
                Differs from gofmt -l / cargo fmt --check (both 1);
                see {base}/specs/exit-codes.md for the 6x fmt-check tier.
  -             Read source from stdin, write canonical form to
                stdout. The lone `-` is read as a regular *.kio
                module. Cannot be mixed with file/directory args.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 when every visited file
is canonical (rewrite mode also exits 0 after writing); 60 in --check
mode when at least one file is not canonical; the matching parse-error
category code for malformed input.

See {base}/specs/cli.md#kio-fmt for the underlying subcommand behavior.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Rewrite mode: write canonical form back to disk; list rewritten
    /// paths on stdout.
    Rewrite,
    /// Check mode: write nothing; list paths whose canonical form
    /// differs from disk on stdout; exit 60 if any do.
    Check,
}

pub fn run(args: &[String], prime_only: bool) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }

    // Parse flags + collect path args. `-` (stdin) cannot mix with
    // any other path; `--check` is compatible with both rewrite and
    // path args.
    let mut mode = Mode::Rewrite;
    let mut paths: Vec<String> = Vec::new();
    let mut stdin_mode = false;
    for a in args {
        match a.as_str() {
            "--check" => mode = Mode::Check,
            "-" => stdin_mode = true,
            other if other.starts_with('-') && other != "-" => {
                eprintln!("error: unknown flag: {other}");
                eprintln!();
                eprintln!(
                    "{}",
                    HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
                );
                return ExitCode::Usage;
            }
            other => paths.push(other.to_owned()),
        }
    }

    if stdin_mode && !paths.is_empty() {
        eprintln!("error: `-` (stdin) cannot be mixed with file or directory arguments");
        return ExitCode::Usage;
    }

    if stdin_mode {
        return run_stdin(prime_only, mode);
    }

    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };

    let mut targets: Vec<PathBuf> = Vec::new();
    if paths.is_empty() {
        if let Err(e) = collect_kio_files(&cwd, &mut targets) {
            eprintln!("error: walking source tree: {e}");
            return ExitCode::Internal;
        }
        targets.sort();
    } else {
        for p in &paths {
            let selected = PathBuf::from(p);
            let path = if selected.is_absolute() {
                selected
            } else {
                cwd.join(selected)
            };
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: cannot stat {}: {}", DisplayPath(&path), e);
                    return ExitCode::Internal;
                }
            };
            if metadata.is_dir() {
                let start = targets.len();
                if let Err(e) = collect_kio_files(&path, &mut targets) {
                    eprintln!("error: walking {}: {}", DisplayPath(&path), e);
                    return ExitCode::Internal;
                }
                targets[start..].sort();
            } else if has_kio_extension(&path) {
                targets.push(path);
            } else {
                eprintln!(
                    "error: {} is not a Kio-family source file",
                    DisplayPath(&path)
                );
                return ExitCode::Usage;
            }
        }
    }
    let mut seen = HashSet::new();
    targets.retain(|path| seen.insert(path.clone()));

    // Per-file work is read + parse + format + (rewrite-only) write.
    // Each file is independent — stable deduplication above guarantees no
    // two workers race on the same path, and `atomic_write` uses a
    // pid-based temp name that's safe across concurrent processes.
    // `par_iter().collect()` preserves input order so the dirty-path
    // listing on stdout and the first reported error are deterministic
    // (the first selected path, not the first to lose the race).
    //
    // Mirrors the `kio check` parse fan-out: failed runs may have
    // reformatted some later files before the first error was caught,
    // since workers commit writes locally before the post-fan-out walk
    // surfaces the error. That's the same trade-off `cargo fmt` makes
    // and the win in wall time on large trees pays for it.
    let results: Vec<FileResult> = crate::maybe_par_iter!(targets)
        .map(|path| {
            let display_path = path
                .strip_prefix(&cwd)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.clone());
            let bytes = match fs::read(path) {
                Ok(b) => b,
                Err(e) => return FileResult::IoErr(display_path, e),
            };
            let source = match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => return FileResult::Utf8Err(display_path),
            };
            let formatted = match format_one_with_file_context(path, &source, prime_only) {
                Ok(s) => s,
                Err(err) => return FileResult::ParseErr(display_path, source, err),
            };
            if formatted == source {
                return FileResult::Clean;
            }
            if mode == Mode::Rewrite
                && let Err(e) = atomic_write(path, formatted.as_bytes())
            {
                return FileResult::WriteErr(display_path, e);
            }
            FileResult::Dirty(display_path)
        })
        .collect();

    let mut any_dirty = false;
    for r in results {
        match r {
            FileResult::Clean => {}
            FileResult::Dirty(display) => {
                any_dirty = true;
                println!("{}", DisplayPath(&display));
            }
            FileResult::IoErr(display, e) => {
                eprintln!("error: cannot read {}: {}", DisplayPath(&display), e);
                return ExitCode::Internal;
            }
            FileResult::Utf8Err(display) => {
                eprintln!(
                    "error: {} is not valid UTF-8 (kio fmt requires UTF-8 source)",
                    DisplayPath(&display)
                );
                return ExitCode::Internal;
            }
            FileResult::ParseErr(display, source, err) => {
                eprint_parse_error(&display, &source, &err);
                return err.exit_code();
            }
            FileResult::WriteErr(display, e) => {
                eprintln!("error: cannot write {}: {}", DisplayPath(&display), e);
                return ExitCode::Internal;
            }
        }
    }

    if mode == Mode::Check && any_dirty {
        return ExitCode::FmtDiff;
    }

    ExitCode::Success
}

/// Outcome of formatting a single file during the rayon fan-out.
/// Errors carry the display path (and source, for parse errors) so the
/// post-fan-out walk renders the diagnostic with the same detail the
/// serial path used to.
enum FileResult {
    Clean,
    Dirty(PathBuf),
    IoErr(PathBuf, std::io::Error),
    Utf8Err(PathBuf),
    ParseErr(PathBuf, String, Error),
    WriteErr(PathBuf, std::io::Error),
}

/// Read every byte of stdin, format, and write to stdout. The input
/// is parsed as a `*.kio` module file — there is no way for the
/// stream to identify itself as a package file.
fn run_stdin(prime_only: bool, mode: Mode) -> ExitCode {
    let mut buf = Vec::new();
    if let Err(e) = io::stdin().read_to_end(&mut buf) {
        eprintln!("error: cannot read stdin: {e}");
        return ExitCode::Internal;
    }
    let source = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("error: stdin is not valid UTF-8 (kio fmt requires UTF-8 source)");
            return ExitCode::Internal;
        }
    };
    let display = PathBuf::from("<stdin>");
    let formatted = match format_one(&display, &source, prime_only) {
        Ok(s) => s,
        Err(err) => {
            eprint_parse_error(&display, &source, &err);
            return err.exit_code();
        }
    };
    match mode {
        Mode::Check => {
            if formatted != source {
                // A formatting difference is a user-input outcome, not a
                // compiler bug: report it under the fmt-check tier (exit
                // 60), matching the file-path path's `any_dirty` branch.
                // See specs/exit-codes.md § 6x and specs/cli.md § kio fmt.
                println!("<stdin>");
                return ExitCode::FmtDiff;
            }
        }
        Mode::Rewrite => {
            let mut out = io::stdout().lock();
            if let Err(e) = out.write_all(formatted.as_bytes()) {
                eprintln!("error: cannot write to stdout: {e}");
                return ExitCode::Internal;
            }
        }
    }
    ExitCode::Success
}

/// True iff the path names a Kio-family source file. The formatter
/// routes by suffix in [`format_one`]; this helper is for path-arg
/// validation.
fn has_kio_extension(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(crate::file_kind::has_kio_extension)
}

/// Format `source` as though it came from a file named `filename`.
///
/// This is the context-free library entry point for the formatter.
/// `filename` is used *only* to
/// determine the file kind (`.pkg.kio` package, `.dep.kio` dependency
/// declaration, `.sig.kio` changelog, or plain `.kio` module); the file
/// is never read or written. Pass an empty string or any non-extension
/// string to get plain-module treatment (the default for stdin-like /
/// untitled editor buffers).
///
/// Returns the canonical-form source on success, or a parse error. A
/// regular module's imported operator grammar is supplied by its own import
/// list, so formatting does not read or resolve provider modules.
/// Does not perform `prime_only` Kio' validation — callers that need
/// that gate should call [`format_one`] directly.
pub fn format_source(filename: &str, source: &str) -> Result<String, Error> {
    format_one(Path::new(filename), source, false)
}

#[cfg(feature = "lsp")]
pub(crate) fn format_source_with_file_context(path: &Path, source: &str) -> Result<String, Error> {
    format_one_with_file_context(path, source, false)
}

/// Format `source` as an explicitly selected Kio-family file kind.
/// Unlike [`format_source`], this does not infer a filename stem, so it
/// is suitable for Kiodoc snippets whose header names are part of the
/// snippet body rather than an on-disk path.
#[cfg(feature = "surface")]
pub(crate) fn format_source_as_kind(kind: KioFileKind, source: &str) -> Result<String, Error> {
    match kind {
        KioFileKind::Module => {
            let module_file = parse_module_file(source)?;
            Ok(pretty_module(&module_file.module))
        }
        KioFileKind::Package => {
            let package_file = parse_package_file(source, None)?;
            Ok(pretty_package_file(&package_file))
        }
        KioFileKind::Signature => {
            let sig = parse_signature_file(source, None)?;
            Ok(crate::sig::emit_signature_file(&sig))
        }
        KioFileKind::Dependency => {
            let dep = parse_dependency_file(source, None)?;
            Ok(pretty_dependency_file(&dep))
        }
        KioFileKind::Lock => {
            let lock = parse_lock_file(source, None)?;
            Ok(pretty_lock_file(&lock))
        }
    }
}

/// Pick the parser/pretty-printer pair based on the filename suffix.
/// Returns the canonical-form source on success, or a parse error.
///
/// When `prime_only` is true (the `kio-prime` binary), the parsed AST
/// is also run through [`crate::prime::lower`] purely for its
/// Kio'-only validation — surface-only forms in the source error out
/// before any formatting happens. The Surface AST itself is what
/// feeds the pretty-printer; the Prime output is discarded.
fn format_one(path: &Path, source: &str, prime_only: bool) -> Result<String, Error> {
    format_one_with_context(path, source, prime_only, ModuleFormatContext::Source)
}

enum ModuleFormatContext {
    Source,
    File,
}

fn format_one_with_file_context(
    path: &Path,
    source: &str,
    prime_only: bool,
) -> Result<String, Error> {
    format_one_with_context(path, source, prime_only, ModuleFormatContext::File)
}

fn format_one_with_context(
    path: &Path,
    source: &str,
    prime_only: bool,
    module_context: ModuleFormatContext,
) -> Result<String, Error> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if crate::file_kind::is_sig_file(name) {
        let sig = parse_signature_file(source, crate::file_kind::sig_stem(name))?;
        return Ok(crate::sig::emit_signature_file(&sig));
    }
    if crate::file_kind::is_dep_file(name) {
        // A `.dep.kio` is a surface file, not Kio', so the `prime_only`
        // Kio'-validation gate the sibling `.pkg.kio` arm runs is absent.
        let dep = parse_dependency_file(source, crate::file_kind::dep_stem(name))?;
        return Ok(pretty_dependency_file(&dep));
    }
    if crate::file_kind::is_lock_file(name) {
        // A `<local>.lock.kio` is a committed pin (the only `*.kio` a
        // build *writes back*); it formats through the lock pretty-printer
        // like its sibling `.dep.kio`, never the module/package one.
        let lock = parse_lock_file(source, crate::file_kind::lock_stem(name))?;
        return Ok(pretty_lock_file(&lock));
    }
    if let Some(stem) = name.strip_suffix(".pkg.kio") {
        let package_file = parse_package_file(source, Some(stem))?;
        #[cfg(feature = "prime")]
        if prime_only {
            prime_lower_package_file(package_file.clone())?;
        }
        let _ = prime_only; // avoid unused-binding warning in full-only builds
        Ok(pretty_package_file(&package_file))
    } else {
        let module_file = match module_context {
            ModuleFormatContext::Source => parse_module_file(source)?,
            ModuleFormatContext::File => parse_module_file_with_file_context(source, path)?,
        };
        #[cfg(feature = "prime")]
        if prime_only {
            prime_lower_module(module_file.module.clone())?;
        }
        let _ = prime_only;
        Ok(pretty_module(&module_file.module))
    }
}

/// Walk `dir` recursively and append every Kio-family source file under
/// it to `out`. Skips build-artifact (`out`, `target`) and hidden
/// (`.*`) directories so a regular `kio fmt` in a Kio package doesn't
/// reach into emitted JS, Cargo build output, or the `.git` tree.
fn collect_kio_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() {
            if name == "target" || name == "out" || name.starts_with('.') {
                continue;
            }
            collect_kio_files(&path, out)?;
        } else if has_kio_extension(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Write `bytes` to `target` via a sibling temp file + rename, so an
/// interrupted `kio fmt` never leaves a half-written file at the
/// target path. Delegates to the shared
/// [`crate::cmd::atomic_write::write_atomic`] helper (temp-then-fsync-
/// then-rename); the temp-file name embeds the current pid to keep
/// concurrent invocations from clobbering each other.
fn atomic_write(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::cmd::atomic_write::write_atomic(target, bytes)
}

fn eprint_parse_error(path: &Path, source: &str, err: &Error) {
    let (span, message) = err.diag();
    let (line, col) = line_col(source, span.start);
    eprintln!("{}:{}:{}: {}", DisplayPath(&path), line, col, message);
}

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

    #[test]
    fn import_grammar_provider_denied_all_contexts() {
        use crate::package_collection::import_grammar_with_denied_reads;
        use crate::pass::parser::{self, IMPORT_GRAMMAR_CONSUMER, import_grammar_assert_consumer};
        let dir = tempfile::tempdir().unwrap();
        let provider = dir.path().join("syntax.kio");
        std::fs::write(&provider, "malformed provider").unwrap();
        let (contexts, attempts) = import_grammar_with_denied_reads([provider], || {
            let outputs = [
                format_source("", IMPORT_GRAMMAR_CONSUMER).unwrap(),
                format_source("main.kio", IMPORT_GRAMMAR_CONSUMER).unwrap(),
                #[cfg(feature = "surface")]
                format_source_as_kind(KioFileKind::Module, IMPORT_GRAMMAR_CONSUMER).unwrap(),
                format_one(Path::new("main.kio"), IMPORT_GRAMMAR_CONSUMER, false).unwrap(),
                format_one_with_file_context(
                    &dir.path().join("app/main.kio"),
                    IMPORT_GRAMMAR_CONSUMER,
                    false,
                )
                .unwrap(),
            ];
            for output in &outputs {
                import_grammar_assert_consumer(&parser::parse(output).unwrap());
                assert_eq!(format_source("", output).unwrap(), *output);
                assert_eq!(*output, outputs[0]);
            }
            outputs.len()
        });
        assert!(attempts.is_empty());
        eprintln!("formatter denial: contexts={contexts} provider_attempts=0");
    }
    use super::*;

    #[test]
    fn has_kio_extension_accepts_kio_files() {
        assert!(has_kio_extension(Path::new("foo.kio")));
        assert!(has_kio_extension(Path::new("foo.pkg.kio")));
        assert!(has_kio_extension(Path::new("foo.sig.kio")));
        assert!(has_kio_extension(Path::new("foo.dep.kio")));
        assert!(has_kio_extension(Path::new("foo.lock.kio")));
        assert!(has_kio_extension(Path::new("/abs/path/main.kio")));
    }

    #[test]
    fn has_kio_extension_rejects_non_kio_files() {
        assert!(!has_kio_extension(Path::new("foo.rs")));
        assert!(!has_kio_extension(Path::new("foo.js")));
        assert!(!has_kio_extension(Path::new("kio")));
        assert!(!has_kio_extension(Path::new("foo")));
        assert!(!has_kio_extension(Path::new("foo.kio.bak")));
    }

    /// `line_col` is 1-indexed and uses `\n` as the line separator.
    /// First column is 1; first line is 1.
    #[test]
    fn line_col_origin_is_one_one() {
        assert_eq!(line_col("hello", 0), (1, 1));
        assert_eq!(line_col("hello", 4), (1, 5));
    }

    #[test]
    fn line_col_counts_newlines() {
        let src = "abc\ndef\nghi";
        assert_eq!(line_col(src, 4), (2, 1)); // 'd'
        assert_eq!(line_col(src, 7), (2, 4)); // newline after 'def' counted as col 4
        assert_eq!(line_col(src, 8), (3, 1)); // 'g'
    }

    /// Offsets past the source length clamp to source.len() rather
    /// than panicking — important because `Span::end` can sit at the
    /// end of the source.
    #[test]
    fn line_col_clamps_overshoot_to_source_end() {
        let src = "ab";
        // Offset way past the end — clamped to len(src) = 2.
        assert_eq!(line_col(src, 999), (1, 3));
    }

    /// `format_one` round-trips a regular module's source through
    /// the parser + pretty-printer. Exercises the suffix dispatch
    /// for the `.kio` (regular module) path.
    #[test]
    fn format_one_handles_regular_module() {
        let src = "module a;\n\nfn id[A](x: A) -> A { x }\n";
        let out = format_one(Path::new("a.kio"), src, false).expect("parse + format");
        // The canonical form keeps the signature inline at this
        // width; the A1 width-driven layout only breaks when the
        // flat form overflows the 100-column budget.
        assert!(out.contains("module a;"));
        assert!(out.contains("fn id"));
    }

    #[test]
    fn format_one_preserves_recursive_type_group_outer_docs() {
        let source = concat!(
            "module docs;\n",
            "\n",
            "/// outer group docs\n",
            "rec {\n",
            "  /// alias member docs\n",
            "  type A = B;\n",
            "  /// newtype member docs\n",
            "  newtype B : A { constructor mk_b; projector un_b }\n",
            "}\n",
            "\n",
            "/// standalone docs\n",
            "type Control = .;\n",
        );

        assert_eq!(
            format_one(Path::new("docs.kio"), source, false).expect("parse + format"),
            source
        );
    }

    /// `format_one` reports a parse error rather than panicking on
    /// invalid input.
    #[test]
    fn format_one_returns_parse_error_on_garbage() {
        let src = "module a;\n\nfn ()))(())";
        let err = format_one(Path::new("a.kio"), src, false);
        assert!(err.is_err(), "expected parse error, got {err:?}");
    }

    #[test]
    fn imported_multislot_fold_formats_with_package_provider_context() {
        let temp = tempfile::tempdir().expect("temp package");
        std::fs::write(temp.path().join("test.pkg.kio"), "package test;\n")
            .expect("write package file");
        std::fs::write(
            temp.path().join("syntax.kio"),
            "module syntax;\npub varop [% %] { foldr cons nil; };\n",
        )
        .expect("write operator provider");
        let source = "module main;\nimport syntax(varop [% %]);\nfn pairs(k1:A,v1:A,k2:A,v2:A)->A{[% (k1,v1),(k2,v2) %]}\n";
        let expected = "module main;\n\nimport syntax(varop [% %]);\n\nfn pairs(k1: A, v1: A, k2: A, v2: A) -> A { [% (k1, v1), (k2, v2) %] }\n";
        let formatted =
            format_one(Path::new("main.kio"), source, false).expect("package-aware format");
        assert_eq!(formatted, expected);

        assert_eq!(
            format_source("main.kio", source).expect("standalone import grammar"),
            expected
        );
    }

    #[test]
    fn imported_multislot_fold_formats_from_the_selected_file_context() {
        let temp = tempfile::tempdir().expect("temp source tree");
        let consumer = temp.path().join("a/b/c.kio");
        let provider = temp.path().join("a/d/e.kio");
        std::fs::create_dir_all(consumer.parent().expect("consumer parent"))
            .expect("consumer dirs");
        std::fs::create_dir_all(provider.parent().expect("provider parent"))
            .expect("provider dirs");
        std::fs::write(
            &provider,
            "module a/d/e;\npub varop [% %] { foldr cons nil; };\n",
        )
        .expect("operator provider");
        let source = "module a/b/c;\nimport a/d/e(varop [% %]);\nfn pairs(k1:A,v1:A,k2:A,v2:A)->A{[% (k1,v1),(k2,v2) %]}\n";
        let expected = "module a/b/c;\n\nimport a/d/e(varop [% %]);\n\nfn pairs(k1: A, v1: A, k2: A, v2: A) -> A { [% (k1, v1), (k2, v2) %] }\n";
        assert_eq!(
            format_one_with_file_context(&consumer, source, false).expect("file-context format"),
            expected
        );

        std::fs::write(temp.path().join("a/near.pkg.kio"), "package near;\n")
            .expect("misleading package marker");
        assert!(
            format_one_with_file_context(&consumer, source, false).is_ok(),
            "a package marker must not redefine the file-derived source root"
        );
    }

    #[test]
    fn package_aware_formatting_leaves_operator_origins_to_resolution() {
        let temp = tempfile::tempdir().expect("temp package");
        std::fs::write(temp.path().join("test.pkg.kio"), "package test;\n")
            .expect("write package file");
        std::fs::write(
            temp.path().join("binary.kio"),
            "module binary; pub fn choose(a: ., b: .) -> . { a } pub op _ ? _ { impl choose; };",
        )
        .expect("write binary provider");
        std::fs::write(
            temp.path().join("ternary.kio"),
            "module ternary; pub fn choose(a: ., b: ., c: .) -> . { a } pub op _ ? _ : _ { impl choose; };",
        )
        .expect("write ternary provider");

        let ambiguous = "module main; import binary(op _ ? _); import ternary(op _ ? _ : _); fn broken() -> . { ( }";
        let error = format_one(Path::new("main.kio"), ambiguous, false)
            .expect_err("formatting must report the malformed body");
        assert!(matches!(error, Error::Parse(_)));
        assert!(error.diag().1.contains("unterminated `(`"));

        let ambiguous_valid = "module main; import binary(op _ ? _); import ternary(op _ ? _ : _); fn choose(a: ., b: .) -> . { a ? b }";
        format_one(Path::new("main.kio"), ambiguous_valid, false)
            .expect("provider-origin conflicts belong to semantic resolution");

        let repeated = "module main; import binary(op _ ? _); import binary(op _ ? _); fn choose(a: ., b: .) -> . { a ? b }";
        format_one(Path::new("main.kio"), repeated, false)
            .expect("duplicate imports belong to semantic resolution");
    }

    #[test]
    fn imported_fold_from_materialized_dependency_formats() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let dependency = temp.path().join("provider");
        let consumer = temp.path().join("consumer");
        std::fs::create_dir_all(&dependency).expect("create dependency");
        std::fs::create_dir_all(&consumer).expect("create consumer");
        std::fs::write(dependency.join("provider.pkg.kio"), "package provider;\n")
            .expect("write dependency package");
        std::fs::write(
            dependency.join("syntax.kio"),
            "module syntax;\npub varop [% %] { foldr cons nil; };\n",
        )
        .expect("write dependency provider");
        std::fs::write(consumer.join("consumer.pkg.kio"), "package consumer;\n")
            .expect("write consumer package");
        std::fs::write(
            consumer.join("ops.dep.kio"),
            "dependency ops;\n\nsource {\n  path \"../provider/provider.pkg.kio\";\n}\n",
        )
        .expect("write dependency declaration");
        crate::package_collection::materialize_dependencies(&consumer)
            .expect("materialize re-rooted provider");

        let source = "module main;\nimport ops/syntax(varop [% %]);\nfn pairs(k1:A,v1:A,k2:A,v2:A)->A{[% (k1,v1),(k2,v2) %]}\n";
        let formatted = format_one(Path::new("main.kio"), source, false)
            .expect("format through re-rooted dependency provider");

        assert!(formatted.contains("import ops/syntax(varop [% %]);"));
        assert!(formatted.contains("[% (k1, v1), (k2, v2) %]"));
    }

    /// `format_one` routes a `.sig.kio` signature changelog through the
    /// signature parser/canonical emitter, not through the module parser
    /// and not through a byte-identical no-op.
    #[test]
    fn format_one_formats_signature_file() {
        let src = "signature   app   v(1) ;\n";
        let out = format_one(Path::new("app.sig.kio"), src, false).expect("parse + format");
        assert_eq!(out, "signature app v(1);\n");
    }

    /// A package file's `build { ... }` block formats inline:
    /// the package-file parser/pretty-printer pair carries it,
    /// with bare-identifier target ids.
    #[test]
    fn format_one_formats_inline_build_block() {
        let src =
            "package a;\n\nbuild {\n  cache ();\n\n  target js {\n    out \"out/js/\";\n  }\n}\n";
        let out = format_one(Path::new("a.pkg.kio"), src, false).expect("parse + format");
        assert!(out.contains("package a;"));
        assert!(out.contains("build {"));
        assert!(out.contains("cache ();"));
        assert!(out.contains("target js {"));
        assert!(out.contains("out/js/"));
    }

    /// `format_one` routes a `<local>.dep.kio` dependency declaration to
    /// the dependency parser/pretty-printer pair and returns its canonical
    /// form. The filename stem feeds the local-name coherence check, so
    /// the `dependency <local>;` header must match the stem.
    #[test]
    fn format_one_formats_dependency_file() {
        let messy = "dependency mathlib  ;\n\nsource   {\n    path  \"../lib/m.pkg.kio\" ;\n}\n";
        let out = format_one(Path::new("mathlib.dep.kio"), messy, false).expect("parse + format");
        assert_eq!(
            out, "dependency mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\"\n}\n",
            "messy .dep.kio did not canonicalise:\n{out}"
        );
    }

    /// The local-path-dependency golden's `mathlib.dep.kio` is already
    /// canonical — formatting it is a fixpoint (the byte content is kept
    /// in sync with `test-data/goldens/00_success/exec_local_path_dependency/`).
    #[test]
    fn format_one_is_fixpoint_on_canonical_dependency_file() {
        let canonical =
            "dependency mathlib;\n\nsource {\n  path \"../library/mathlib/mathlib.pkg.kio\"\n}\n";
        let out =
            format_one(Path::new("mathlib.dep.kio"), canonical, false).expect("parse + format");
        assert_eq!(
            out, canonical,
            "canonical .dep.kio is not a fmt fixpoint:\n{out}"
        );
    }

    /// `format_one` routes a git `<local>.dep.kio` through the dependency
    /// pair and canonicalises the `git` / `ref` pair.
    #[test]
    fn format_one_formats_git_dependency_file() {
        let messy = "dependency foo ;\n\nsource  {\n  git  \"https://example.com/foo.git\" ;\n  ref \"main\";\n}\n";
        let out = format_one(Path::new("foo.dep.kio"), messy, false).expect("parse + format");
        assert_eq!(
            out,
            "dependency foo;\n\nsource {\n  git \"https://example.com/foo.git\";\n  ref \"main\"\n}\n",
            "messy git .dep.kio did not canonicalise:\n{out}"
        );
    }

    /// `format_one` routes a `<local>.lock.kio` through the lock pair and
    /// canonicalises the `resolved { … }` block.
    #[test]
    fn format_one_formats_lock_file() {
        let messy = "lock foo ;\n\nresolved  {\n  git \"u\" ;\n  ref \"r\";\n  commit \"abc\";\n  sig \"d\";\n}\n";
        let out = format_one(Path::new("foo.lock.kio"), messy, false).expect("parse + format");
        assert_eq!(
            out,
            "lock foo;\n\nresolved {\n  git \"u\";\n  ref \"r\";\n  commit \"abc\";\n  sig \"d\"\n}\n",
            "messy .lock.kio did not canonicalise:\n{out}"
        );
    }
}

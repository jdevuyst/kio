//! Snippet validation — substitute the snippet's body into its
//! referenced harness (or pass it through for the standalone form),
//! then drive `kio check` against the synthesized source.
//!
//! The validator constructs a per-snippet scratch directory and lays
//! out a minimal Kio package inside it:
//!
//! ```text
//! <scratch>/
//!   <pkg>.pkg.kio          — package sections extracted from the program text
//!   *.kio                  — package-root support modules imported by
//!                             the assembled program
//!   <pkg>/
//!     main.kio             — the program body (less the package sections)
//! ```
//!
//! The split is the simplest convention that lets Kiodoc's
//! single-blob harness shape coexist with Kio's
//! file-split-package model:
//!
//! - Any leading `build { ... }` or `bridge { ... }` section is
//!   siphoned out into the package file.
//! - The remainder is written into `<pkg>/main.kio` — the file the
//!   `module <pkg>/main;` declaration's path implies relative to the
//!   package root, per `specs/package.md` § Module-name rules. If
//!   the body doesn't start with a `module …;` line, one is
//!   prepended.
//!
//! `kio check` is invoked in-process via
//! [`crate::cmd::check::compile_workspace_at_buffered`], which takes the
//! scratch directory as an explicit argument. The validator never mutates
//! the process's current working directory, so concurrent snippet
//! validations under a rayon `par_iter` (the parent
//! [`super::run`] driver fans out across snippets) don't race.
//!
//! The `_buffered` form captures the typechecker's diagnostic instead of
//! letting it reach the process's real stderr. A snippet declaring a
//! non-zero `check_exit_code` *expects* to be rejected, so its diagnostic
//! is not an error to report; and the parallel fan-out would interleave
//! per-worker stderr writes with the driver's ordered error replay. The
//! validator surfaces the captured diagnostic only when the exit code
//! contradicts the snippet's declaration, alongside its own message —
//! which names the markdown file + line and prints the assembled source,
//! so the author can correlate the scratch path the diagnostic cites.

use crate::path_display::DisplayPath;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::exit_code::ExitCode;
use crate::span::Span;

use super::cache::{CachedResult, DocCache, DocCacheKey};
use super::document::{Document, DocumentFileHeader, Harness, Snippet, SnippetPlaceholder};
use super::scratch::create_scratch_dir;
use crate::ast::KioFileKind;

/// Validation-level error. Carries enough information for an
/// author-facing diagnostic; print via [`Self::eprint`].
#[derive(Debug)]
pub struct ValidationError {
    pub span: Span,
    pub message: String,
    /// The assembled source the validator handed to `kio check`,
    /// printed alongside the diagnostic so authors can see what was
    /// actually compiled. `None` when the validation failed before
    /// reaching the typechecker.
    pub assembled: Option<AssembledProgram>,
    /// What `kio check` reported, captured rather than printed so a
    /// snippet whose exit code matches its declared `check_exit_code`
    /// stays silent — an expected failure is a validation *success*.
    /// `None` when the typechecker never ran (cache hit, or a failure
    /// before it was reached) or when it accepted the snippet.
    pub diagnostic: Option<String>,
}

#[derive(Debug)]
pub struct AssembledProgram {
    pub files: Vec<AssembledFile>,
}

#[derive(Debug, Clone)]
pub struct AssembledFile {
    pub path: String,
    pub body: String,
}

impl ValidationError {
    /// Print the diagnostic and the assembled source.
    pub fn eprint(&self, path: &Path, source: &str) {
        eprint!("{}", self.render(path, source));
    }

    /// As [`Self::eprint`], but returns the rendered diagnostic (with a
    /// trailing newline) so the multi-package fan-out can buffer it.
    pub fn render(&self, path: &Path, _source: &str) -> String {
        use std::fmt::Write as _;
        let mut out = format!("{}: {}\n", DisplayPath(&path), self.message);
        if let Some(diag) = &self.diagnostic {
            let _ = writeln!(out, "--- kio check diagnostic ---");
            let _ = writeln!(out, "{}", diag.trim_end_matches('\n'));
        }
        if let Some(asm) = &self.assembled {
            for file in &asm.files {
                let _ = writeln!(out, "--- assembled file ({}) ---", file.path);
                let _ = writeln!(out, "{}", file.body.trim_end_matches('\n'));
            }
        }
        out
    }
}

/// Validate one snippet against `kio check`. The validator
/// substitutes the snippet body into its harness (or takes the
/// standalone form as-is), synthesizes a minimal Kio package on
/// disk, invokes `kio check`, and compares the exit code against
/// the snippet's declared `check_exit_code`.
///
/// `cache` is consulted before the scratch-dir / `kio check` work:
/// a cache hit short-circuits to the cached exit code, skipping
/// the typechecker call entirely. A miss runs the full pipeline
/// and writes the result on the way out. The disabled cache
/// (`cache ();` in the build block, or no build block at all) is
/// represented as a [`DocCache`] whose every lookup misses and
/// every store is a no-op, so the cached and uncached paths share
/// one code path.
pub fn validate_snippet(
    md_path: &Path,
    md_source: &str,
    document: &Document,
    snippet: &Snippet,
    support_files: &[AssembledFile],
    cache: &DocCache,
) -> Result<(), ValidationError> {
    let _ = md_source;
    // Non-module variants are checked in isolation via the parser
    // for that file kind. They do not engage the harness
    // mechanism (the document layer rejects `@NAME` paired with
    // `variant=`).
    if snippet.variant != KioFileKind::Module {
        return validate_variant_snippet(snippet, cache);
    }
    let assembled = match assemble(document, snippet) {
        Ok(a) => a,
        Err(msg) => {
            return Err(ValidationError {
                span: snippet.span,
                message: msg,
                assembled: None,
                diagnostic: None,
            });
        }
    };

    // Cache key built from the inputs `kio check` would see — the
    // harness body (verbatim, so the marker position is part of it),
    // the validation body, and the marker offset. The header fields
    // (compiler cache identity, schema tag) are folded in by the
    // key constructor.
    let support_files = select_package_support_files(&assembled, support_files);
    let assembled = assembled_with_support_bridges(assembled, &support_files);
    let key = make_cache_key(document, snippet, &support_files);
    let (actual_i32, diagnostic) = match cache.lookup(&key) {
        Some(cached) => (cached.check_exit_code, None),
        None => {
            let scratch = match create_scratch_dir(md_path, snippet.open_line) {
                Ok(p) => p,
                Err(e) => {
                    return Err(ValidationError {
                        span: snippet.span,
                        message: format!("could not create scratch directory: {e}"),
                        assembled: Some(assembled),
                        diagnostic: None,
                    });
                }
            };

            let (exit, diagnostic) =
                match run_kio_check(&scratch, &assembled, &support_files, md_path, snippet) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(ValidationError {
                            span: snippet.span,
                            message: format!(
                                "internal error setting up snippet scratch package: {e}"
                            ),
                            assembled: Some(assembled),
                            diagnostic: None,
                        });
                    }
                };

            let _ = fs::remove_dir_all(&scratch);

            let exit_i32 = exit.as_i32();
            // Persist on the miss path so the next run is warm.
            // Disabled-cache backends silently no-op here.
            cache.store(
                &key,
                &CachedResult {
                    check_exit_code: exit_i32,
                },
            );
            (exit_i32, Some(diagnostic))
        }
    };

    let expected = snippet.check_exit_code;
    if actual_i32 == expected {
        Ok(())
    } else {
        Err(ValidationError {
            span: snippet.span,
            message: format!(
                "snippet at line {}: `kio check` exited {} but {} was expected \
                 (via {})",
                snippet.open_line,
                actual_i32,
                expected,
                if expected == 0 {
                    "default `check_exit_code=0`".to_owned()
                } else {
                    format!("`check_exit_code={expected}`")
                }
            ),
            assembled: Some(assembled),
            diagnostic: diagnostic.filter(|d| !d.trim().is_empty()),
        })
    }
}

/// Validate one accumulating harness's aggregate program. The
/// runner concatenates the bodies of every member snippet (in
/// document order) and substitutes the concatenation into the
/// harness once. The result is fed to `kio check` as a single
/// program.
///
/// A harness with no members is a no-op (validates as success);
/// see [`specs/kiodoc.md`](../../../specs/kiodoc.md)
/// § Accumulating harnesses.
pub fn validate_aggregate(
    md_path: &Path,
    harness: &Harness,
    members: &[Snippet],
    support_files: &[AssembledFile],
    cache: &DocCache,
) -> Result<(), ValidationError> {
    if members.is_empty() {
        // No members → no aggregate to validate. The spec calls this
        // out explicitly so authors can declare a harness ahead of
        // its first use without tripping a "missing member" error.
        return Ok(());
    }
    let concatenated = aggregate_member_bodies(members);
    let program = substitute_into_harness(&harness.body, &harness.placeholder, &concatenated);
    let (package_body, module_body) = split_package_and_module(&program);
    let package_name = synthesize_pkg_name(harness.fence_index);
    let module_header = format!("module {package_name}/main;\n");
    let needs_header = !body_declares_module(&module_body);
    let module_file = if needs_header {
        format!("{module_header}\n{module_body}")
    } else {
        module_body
    };
    let package_file = synthesize_package_file(&package_name, &package_body, &module_file);
    let assembled = assembled_implicit_package(package_name, package_file, module_file);
    let support_files = select_package_support_files(&assembled, support_files);
    let assembled = assembled_with_support_bridges(assembled, &support_files);

    // Cache key: every member body folded into the harness shape.
    // A change to *any* member's body changes the key, so warm
    // aggregates re-run when membership shifts.
    let marker_pos = harness.body.find(&harness.placeholder);
    let mut harness_seed = harness.body.clone();
    append_support_files_seed(&mut harness_seed, &support_files);
    let key = DocCacheKey::new(&harness_seed, &concatenated, marker_pos);
    let (actual_i32, diagnostic) = match cache.lookup(&key) {
        Some(cached) => (cached.check_exit_code, None),
        None => {
            let scratch = match create_scratch_dir(md_path, harness.fence_index) {
                Ok(p) => p,
                Err(e) => {
                    return Err(ValidationError {
                        span: harness.span,
                        message: format!(
                            "could not create scratch directory for accumulating harness \
                             `{}`: {e}",
                            harness.name
                        ),
                        assembled: Some(assembled),
                        diagnostic: None,
                    });
                }
            };
            let (exit, diagnostic) =
                match run_kio_check_aggregate(&scratch, &assembled, &support_files) {
                    Ok(x) => x,
                    Err(e) => {
                        return Err(ValidationError {
                            span: harness.span,
                            message: format!(
                                "internal error setting up aggregate scratch package for \
                                 harness `{}`: {e}",
                                harness.name
                            ),
                            assembled: Some(assembled),
                            diagnostic: None,
                        });
                    }
                };
            let _ = fs::remove_dir_all(&scratch);
            let exit_i32 = exit.as_i32();
            cache.store(
                &key,
                &CachedResult {
                    check_exit_code: exit_i32,
                },
            );
            (exit_i32, Some(diagnostic))
        }
    };
    if actual_i32 == 0 {
        Ok(())
    } else {
        Err(ValidationError {
            span: harness.span,
            message: format!(
                "accumulating harness `{}`: `kio check` exited {} validating the \
                 aggregate of {} member{}",
                harness.name,
                actual_i32,
                members.len(),
                if members.len() == 1 { "" } else { "s" }
            ),
            assembled: Some(assembled),
            diagnostic: diagnostic.filter(|d| !d.trim().is_empty()),
        })
    }
}

/// Validate one non-`Module` variant snippet by routing the body
/// through the parser for that file kind. The snippet is
/// standalone: no harness wrapping, no module-body split, no
/// package-file siphon.
fn validate_variant_snippet(snippet: &Snippet, cache: &DocCache) -> Result<(), ValidationError> {
    let body = validation_body(snippet);
    // Cache key — empty harness body, the variant's full snippet
    // body, no marker. Variant is folded in implicitly via the
    // body's syntactic shape (a package body won't parse as a module,
    // etc.).
    let key = DocCacheKey::new("", &body, None);
    let actual_i32 = match cache.lookup(&key) {
        Some(cached) => cached.check_exit_code,
        None => {
            let parse_result: Result<(), crate::error::Error> = match snippet.variant {
                KioFileKind::Module => {
                    unreachable!("variant=module routes through assemble path")
                }
                KioFileKind::Package => {
                    crate::pass::parser::parse_package_file(&body, None).map(|_| ())
                }
                KioFileKind::Signature => {
                    crate::pass::parser::parse_signature_file(&body, None).map(|_| ())
                }
                KioFileKind::Dependency => {
                    crate::pass::parser::parse_dependency_file(&body, None).map(|_| ())
                }
                KioFileKind::Lock => crate::pass::parser::parse_lock_file(&body, None).map(|_| ()),
            };
            let exit = match parse_result {
                Ok(()) => ExitCode::Success,
                Err(err) => err.exit_code(),
            };
            let exit_i32 = exit.as_i32();
            cache.store(
                &key,
                &CachedResult {
                    check_exit_code: exit_i32,
                },
            );
            exit_i32
        }
    };
    let expected = snippet.check_exit_code;
    if actual_i32 == expected {
        Ok(())
    } else {
        Err(ValidationError {
            span: snippet.span,
            message: format!(
                "variant snippet at line {}: parser exited {} but {} was expected \
                 (via {})",
                snippet.open_line,
                actual_i32,
                expected,
                if expected == 0 {
                    "default `check_exit_code=0`".to_owned()
                } else {
                    format!("`check_exit_code={expected}`")
                }
            ),
            assembled: None,
            diagnostic: None,
        })
    }
}

/// Concatenate the bodies of every accumulating harness's members
/// in document order, separated by `\n` so each member starts on
/// its own line.
fn aggregate_member_bodies(members: &[Snippet]) -> String {
    let mut out = String::new();
    for (i, s) in members.iter().enumerate() {
        if i > 0 && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&validation_body(s));
    }
    out
}

/// `kio check` over an aggregate program. Same plumbing as
/// [`run_kio_check`], diagnostic buffering included; factored out
/// because the aggregate path doesn't carry an originating per-member
/// span (the diagnostic rolls up under the harness's own span).
fn run_kio_check_aggregate(
    scratch: &Path,
    assembled: &AssembledProgram,
    support_files: &[AssembledFile],
) -> std::io::Result<(ExitCode, String)> {
    write_support_files(scratch, support_files)?;
    write_assembled_program(scratch, assembled)?;
    let mut diagnostic = String::new();
    let exit = match crate::cmd::check::compile_workspace_at_buffered(
        scratch,
        false,
        true,
        &mut diagnostic,
    ) {
        Ok(_) => ExitCode::Success,
        Err(code) => code,
    };
    Ok((exit, diagnostic))
}

/// Compute the cache key for one snippet. The harness body
/// (if any) feeds the hash verbatim; the marker position is
/// the byte offset of the declared placeholder inside the harness
/// body (or `None` for a standalone snippet).
fn make_cache_key(
    document: &Document,
    snippet: &Snippet,
    support_files: &[AssembledFile],
) -> DocCacheKey {
    let validation_body = validation_body(snippet);
    let (mut harness_body, marker_pos): (String, Option<usize>) = match &snippet.harness_ref {
        Some(name) => match document.harnesses.get(name) {
            Some(h) => {
                let pos = h.body.find(&h.placeholder);
                if h.file.is_some() {
                    (file_backed_cache_seed(document, h), pos)
                } else {
                    (h.body.clone(), pos)
                }
            }
            // Should be unreachable — the document builder rejects
            // snippets referencing an undeclared harness. Fall
            // back to a key that won't hit a real entry rather
            // than panic; the validator will surface the real
            // error on the assemble path immediately below.
            None => (String::new(), None),
        },
        None => (String::new(), None),
    };
    append_support_files_seed(&mut harness_body, support_files);
    DocCacheKey::new(&harness_body, &validation_body, marker_pos)
}

fn append_support_files_seed(seed: &mut String, support_files: &[AssembledFile]) {
    if support_files.is_empty() {
        return;
    }
    seed.push_str("\n-- package support files\n");
    for file in support_files {
        seed.push_str("-- file ");
        seed.push_str(&file.path);
        seed.push('\n');
        seed.push_str(&file.body);
        if !file.body.ends_with('\n') {
            seed.push('\n');
        }
    }
}

fn select_package_support_files(
    assembled: &AssembledProgram,
    support_files: &[AssembledFile],
) -> Vec<AssembledFile> {
    if support_files.is_empty() {
        return Vec::new();
    }
    let support_by_module: BTreeMap<&str, &AssembledFile> = support_files
        .iter()
        .filter_map(|file| module_file_identity(&file.path).map(|module| (module, file)))
        .collect();
    if support_by_module.is_empty() {
        return Vec::new();
    }
    let assembled_roots: BTreeSet<&str> = assembled
        .files
        .iter()
        .filter_map(|file| module_file_root(&file.path))
        .collect();
    let mut enqueued = BTreeSet::new();
    let mut queue = VecDeque::new();
    for file in &assembled.files {
        enqueue_module_imports(&mut queue, &mut enqueued, &file.body);
    }
    let mut selected = BTreeSet::new();
    while let Some(module) = queue.pop_front() {
        if assembled_roots.contains(module.split('/').next().expect("module path")) {
            continue;
        }
        let Some(file) = support_by_module.get(module.as_str()) else {
            continue;
        };
        selected.insert(module);
        enqueue_module_imports(&mut queue, &mut enqueued, &file.body);
    }
    support_files
        .iter()
        .filter(|file| {
            module_file_identity(&file.path).is_some_and(|module| selected.contains(module))
        })
        .cloned()
        .collect()
}

fn assembled_with_support_bridges(
    mut assembled: AssembledProgram,
    support_files: &[AssembledFile],
) -> AssembledProgram {
    let modules: BTreeSet<String> = support_files
        .iter()
        .chain(&assembled.files)
        .filter_map(|file| module_file_identity(&file.path).map(str::to_owned))
        .collect();
    for file in &mut assembled.files {
        if !crate::file_kind::is_package_file(&file.path) {
            continue;
        }
        let Ok(package) = crate::pass::parser::parse_package_file(&file.body, None) else {
            continue;
        };
        for module in &modules {
            let covered = package.bridge.as_ref().is_some_and(|bridge| {
                bridge
                    .globs
                    .iter()
                    .any(|glob| crate::pass::resolve::glob_matches(&glob.segments, module))
            });
            if !covered {
                file.body = package_file_with_inserted_glob(&file.body, module);
            }
        }
    }
    assembled
}

/// True when the package body's `bridge { … }` block already lists a
/// glob whose leading literal segment is `root` (a `root;` or
/// `root/**;` entry).
pub(super) fn package_file_has_bridge_glob(package_file: &str, root: &str) -> bool {
    package_file.lines().map(str::trim_start).any(|line| {
        let line = line.trim_end_matches(';').trim_end();
        line == root
            || line
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Insert a glob entry `<glob>;` into the package body's `bridge { … }`
/// block, synthesizing the block when absent.
pub(super) fn package_file_with_inserted_glob(package_file: &str, glob: &str) -> String {
    let glob_line = format!("  {glob};");
    if let Some((close_at, separator_at)) = bridge_block_insertion_offsets(package_file) {
        let mut before = package_file[..close_at].trim_end().to_owned();
        if let Some(separator_at) = separator_at {
            before.insert(separator_at, ';');
        }
        let after = &package_file[close_at..];
        format!("{before}\n{glob_line}\n{after}")
    } else {
        format!(
            "{}\n\nbridge {{\n{glob_line}\n}}\n",
            package_file.trim_end()
        )
    }
}

/// Closing-brace offset and any missing final-entry separator for the
/// first top-level bridge block. Token spans keep comments outside the edit.
fn bridge_block_insertion_offsets(package_file: &str) -> Option<(usize, Option<usize>)> {
    use crate::pass::lexer::{TokenKind, lex};

    let tokens = lex(package_file).ok()?;
    let mut depth = 0usize;
    let mut in_bridge = false;
    for (index, token) in tokens.iter().enumerate() {
        match &token.kind {
            TokenKind::LBrace => {
                if depth == 0 {
                    in_bridge = index > 0
                        && matches!(&tokens[index - 1].kind, TokenKind::Ident(name) if name == "bridge");
                }
                depth += 1;
            }
            TokenKind::RBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && in_bridge {
                    let previous = &tokens[index - 1];
                    let separator_at =
                        (!matches!(previous.kind, TokenKind::LBrace | TokenKind::Semicolon))
                            .then_some(previous.span.end as usize);
                    return Some((token.span.start as usize, separator_at));
                }
            }
            _ => {}
        }
    }
    None
}

fn enqueue_module_imports(
    queue: &mut VecDeque<String>,
    enqueued: &mut BTreeSet<String>,
    source: &str,
) {
    for module in module_import_paths(source) {
        if enqueued.insert(module.clone()) {
            queue.push_back(module);
        }
    }
}

fn module_file_identity(path: &str) -> Option<&str> {
    if !crate::file_kind::is_module_file(path) {
        return None;
    }
    path.strip_suffix(crate::file_kind::KIO_SUFFIX)
        .filter(|path| !path.is_empty())
}

fn module_file_root(path: &str) -> Option<&str> {
    module_file_identity(path)?
        .split('/')
        .next()
        .filter(|root| !root.is_empty())
}

fn module_import_paths(source: &str) -> BTreeSet<String> {
    let Ok(file) = crate::pass::parser::parse_module_file_lazy(source) else {
        return BTreeSet::new();
    };
    file.module
        .imports
        .iter()
        .filter_map(|import| match &import.kind {
            crate::ast::ImportKind::Selective { from: path, .. }
            | crate::ast::ImportKind::Qualified { path, .. } => Some(path.segments.join("/")),
            crate::ast::ImportKind::Intrinsics | crate::ast::ImportKind::Comptime => None,
        })
        .collect()
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

/// Substitute the snippet body into its harness (if any) and split
/// the assembled program into a package file and a module file.
fn assemble(document: &Document, snippet: &Snippet) -> Result<AssembledProgram, String> {
    let snippet_body = validation_body(snippet);
    assemble_with_snippet_body(document, snippet, &snippet_body)
}

pub(super) fn assemble_with_snippet_body(
    document: &Document,
    snippet: &Snippet,
    snippet_body: &str,
) -> Result<AssembledProgram, String> {
    if let Some(name) = &snippet.harness_ref {
        let harness = document
            .harnesses
            .get(name)
            .ok_or_else(|| format!("internal: harness `{name}` not found"))?;
        if harness.file.is_some() {
            return assemble_file_backed(document, harness, snippet_body);
        }
    }
    // Step 1: assemble the raw program text — harness substitution
    // (or pass-through for standalone).
    let program = match &snippet.harness_ref {
        Some(name) => {
            let harness = document
                .harnesses
                .get(name)
                .ok_or_else(|| format!("internal: harness `{name}` not found"))?;
            substitute_into_harness(&harness.body, &harness.placeholder, snippet_body)
        }
        None => snippet_body.to_owned(),
    };

    // Step 2: split into (package-file, module-file).
    let (package_body, module_body) = split_package_and_module(&program);

    // Step 3: ensure the module file starts with `module <pkg>/main;`.
    let package_name = synthesize_pkg_name(snippet.open_line);
    let module_header = format!("module {package_name}/main;\n");
    let needs_header = !body_declares_module(&module_body);
    let module_file = if needs_header {
        format!("{module_header}\n{module_body}")
    } else {
        module_body
    };

    // The synthesized package file always carries the required
    // `package <pkg>;` directive, ahead of any extracted package
    // sections.
    let package_file = synthesize_package_file(&package_name, &package_body, &module_file);

    Ok(assembled_implicit_package(
        package_name,
        package_file,
        module_file,
    ))
}

fn assemble_file_backed(
    document: &Document,
    harness: &Harness,
    snippet_body: &str,
) -> Result<AssembledProgram, String> {
    let header = harness
        .file
        .as_ref()
        .ok_or_else(|| format!("internal: harness `{}` is not file-backed", harness.name))?;
    let mut files: BTreeMap<String, String> = document
        .files
        .iter()
        .map(|(path, file)| (path.clone(), file.body.clone()))
        .collect();
    let substituted = substitute_into_harness(&harness.body, &harness.placeholder, snippet_body);
    if files.insert(header.path.clone(), substituted).is_some() {
        return Err(format!(
            "internal: file-backed harness `{}` collides with document file `{}`",
            harness.name, header.path
        ));
    }
    synthesize_missing_package_files(document, Some(header), &mut files);
    Ok(AssembledProgram {
        files: files
            .into_iter()
            .map(|(path, body)| AssembledFile { path, body })
            .collect(),
    })
}

fn assembled_implicit_package(
    package_name: String,
    package_file: String,
    module_file: String,
) -> AssembledProgram {
    let module_path = module_file_path_for_source(&module_file, &package_name);
    let root_path = format!("{package_name}.kio");
    let mut files = vec![AssembledFile {
        path: format!("{package_name}.pkg.kio"),
        body: package_file,
    }];
    if module_path != root_path {
        files.push(AssembledFile {
            path: root_path,
            body: format!("module {package_name};\n"),
        });
    }
    files.push(AssembledFile {
        path: module_path,
        body: module_file,
    });
    AssembledProgram { files }
}

fn module_file_path_for_source(module_file: &str, package_name: &str) -> String {
    for line in module_file.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("module ") else {
            continue;
        };
        let Some(end) = rest.find(';') else {
            continue;
        };
        return module_file_path_for_module_path(rest[..end].trim());
    }
    format!("{package_name}/main.kio")
}

fn module_file_path_for_module_path(module_path: &str) -> String {
    let mut segments: Vec<&str> = module_path.split('/').filter(|s| !s.is_empty()).collect();
    let stem = segments.pop().unwrap_or("main");
    if segments.is_empty() {
        format!("{stem}.kio")
    } else {
        format!("{}/{}.kio", segments.join("/"), stem)
    }
}

fn synthesize_missing_package_files(
    document: &Document,
    harness_file: Option<&DocumentFileHeader>,
    files: &mut BTreeMap<String, String>,
) {
    // Root names that appear as their own module file in the document
    // (`<root>.kio`). Such a root is a plain module — bridged into a
    // package via `assembled_with_support_bridges`, not a package of its
    // own — so a `<root>/<sub>;` submodule file must not make us
    // synthesize a competing `<root>.pkg.kio`.
    let module_roots: BTreeSet<&str> = document
        .files
        .keys()
        .filter_map(|path| module_file_root(path))
        .filter(|root| files.contains_key(&format!("{root}.kio")))
        .collect();

    let mut packages: BTreeSet<String> = BTreeSet::new();
    for file in document.files.values() {
        if let Some(package) = &file.header.package_name
            && !module_roots.contains(package.as_str())
        {
            packages.insert(package.clone());
        }
    }
    if let Some(header) = harness_file
        && let Some(package) = &header.package_name
    {
        packages.insert(package.clone());
    }
    for package in packages {
        let path = format!("{package}.pkg.kio");
        files
            .entry(path)
            .or_insert_with(|| synthesize_package_file(&package, "", ""));
        let root_path = format!("{package}.kio");
        files
            .entry(root_path)
            .or_insert_with(|| format!("module {package};\n"));
    }
}

fn file_backed_cache_seed(document: &Document, harness: &Harness) -> String {
    let mut seed = String::new();
    for file in document.files.values() {
        seed.push_str("-- file ");
        seed.push_str(&file.header.path);
        seed.push('\n');
        seed.push_str(&file.body);
        if !file.body.ends_with('\n') {
            seed.push('\n');
        }
    }
    if let Some(header) = &harness.file {
        seed.push_str("-- harness file ");
        seed.push_str(&header.path);
        seed.push('\n');
    }
    seed.push_str(&harness.body);
    seed
}

pub(super) fn validation_body(snippet: &Snippet) -> String {
    if snippet.placeholders.is_empty() {
        return snippet.body.clone();
    }
    apply_placeholders(&snippet.body, &snippet.placeholders)
}

pub(super) fn apply_placeholders(source: &str, placeholders: &[SnippetPlaceholder]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut idx = 0usize;
    while idx < source.len() {
        let rest = &source[idx..];
        if let Some(placeholder) = placeholders
            .iter()
            .find(|placeholder| rest.starts_with(&placeholder.from))
        {
            out.push_str(&placeholder.to);
            idx += placeholder.from.len();
        } else {
            let ch = rest
                .chars()
                .next()
                .expect("idx is in-bounds, so rest has a first char");
            out.push(ch);
            idx += ch.len_utf8();
        }
    }
    out
}

/// Substitute `snippet_body` into `harness_body` at the literal
/// position of `placeholder`. The snippet's own leading whitespace is
/// preserved verbatim.
fn substitute_into_harness(harness_body: &str, placeholder: &str, snippet_body: &str) -> String {
    if let Some(marker_idx) = harness_body.find(placeholder) {
        let before = &harness_body[..marker_idx];
        let after = &harness_body[marker_idx + placeholder.len()..];
        format!("{before}{snippet_body}{after}")
    } else {
        // A harness without the marker is caught by the document
        // builder; reaching here implies a logic bug.
        unreachable!(
            "harness body must contain its placeholder — \
             document::Document::build is required to reject harness \
             declarations without the marker"
        )
    }
}

/// Split a program text into (package-file content, module-file body).
///
/// Convention: any leading `build { ... }` or `bridge { ... }`
/// package section moves to the package file. Everything from the
/// first non-package-section line onward becomes the module file.
fn split_package_and_module(program: &str) -> (String, String) {
    let mut package_chunks: Vec<&str> = Vec::new();
    let mut module_start_byte: usize = 0;
    let mut idx = 0usize;
    while idx < program.len() {
        let rest = &program[idx..];
        let line_len = rest.find('\n').map(|n| n + 1).unwrap_or(rest.len());
        let line = &rest[..line_len];
        let trimmed = line.trim();
        let is_passthrough = trimmed.is_empty() || trimmed.starts_with("//");
        let is_module_line = trimmed.starts_with("module ");
        if is_module_line {
            module_start_byte = idx;
            break;
        }
        if is_package_section_start(trimmed) {
            let section_len = package_section_len(rest);
            package_chunks.push(&rest[..section_len]);
            idx += section_len;
            continue;
        }
        if is_passthrough && package_chunks.is_empty() {
            // Leading blank/comment prologue before any package section —
            // leave it in the module file so module-level comments
            // survive.
            module_start_byte = idx;
            break;
        }
        if is_passthrough {
            // Blank/comment line *between* package sections — keep with
            // the package file to preserve formatting.
            package_chunks.push(line);
            idx += line_len;
            continue;
        }
        // First non-package-section line that isn't a module header.
        module_start_byte = idx;
        break;
    }
    if module_start_byte == 0 && package_chunks.is_empty() {
        return (String::new(), program.to_owned());
    }
    if module_start_byte == 0 {
        module_start_byte = program.len();
    }
    let package_file: String = package_chunks.into_iter().collect();
    let module_body = program[module_start_byte..].to_owned();
    (package_file, module_body)
}

fn package_body_with_root_bridge(package_name: &str, package_body: &str) -> String {
    if package_file_has_bridge_glob(package_body, package_name) {
        return package_body.to_owned();
    }
    if package_body.trim().is_empty() {
        return format!("bridge {{\n  {package_name};\n}}\n");
    }
    package_file_with_inserted_glob(package_body, package_name)
}

/// True if `source` begins with a `module` header line once leading
/// blank lines and `//` line comments are skipped. A `variant=module`
/// snippet that opens with a `// filename.kio` provenance comment still
/// carries its own header — the same way the token-based `{file}` path
/// (which lexes comments away before reading the header keyword) treats
/// it. Without skipping the comment, such a snippet gets a synthetic
/// `module …/main;` prepended and fails with a duplicate-`module` error.
pub(super) fn body_declares_module(source: &str) -> bool {
    source
        .lines()
        .map(str::trim_start)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .is_some_and(|line| line.starts_with("module "))
}

fn synthesize_package_file(package_name: &str, package_body: &str, _module_file: &str) -> String {
    let body = package_body_with_root_bridge(package_name, package_body);
    if body.trim().is_empty() {
        format!("package {package_name};\n")
    } else {
        format!("package {package_name};\n\n{body}")
    }
}

fn is_package_section_start(trimmed: &str) -> bool {
    starts_keyword(trimmed, "build") || starts_keyword(trimmed, "bridge")
}

fn starts_keyword(source: &str, keyword: &str) -> bool {
    let Some(rest) = source.strip_prefix(keyword) else {
        return false;
    };
    rest.chars().next().is_none_or(|ch| !is_ident_char(ch))
}

fn package_section_len(source: &str) -> usize {
    let mut depth = 0usize;
    let mut saw_open = false;
    let mut in_line_comment = false;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in source.char_indices() {
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
                if saw_open && depth == 0 {
                    return idx + ch.len_utf8();
                }
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == '/' && source[idx + ch.len_utf8()..].starts_with('/') {
            in_line_comment = true;
            continue;
        }
        match ch {
            '{' => {
                saw_open = true;
                depth += 1;
            }
            '}' if saw_open => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let rest = &source[idx + ch.len_utf8()..];
                    let newline_len = rest.find('\n').map(|n| n + 1).unwrap_or(rest.len());
                    return idx + ch.len_utf8() + newline_len;
                }
            }
            _ => {}
        }
    }
    source.len()
}

/// Package name the validator uses inside each per-snippet scratch
/// directory. Constant across snippets so authors can write
/// predictable package-local paths in their harnesses;
/// scratch-dir isolation prevents collisions across snippets.
const SCRATCH_PKG_NAME: &str = "kiodoc";

/// Return the validator's per-snippet package name. The argument is
/// unused today (snippet open-line); kept for forward compatibility
/// in case a per-snippet differentiation becomes useful (e.g. when
/// snippets within one batch reuse a single scratch root).
fn synthesize_pkg_name(_open_line: usize) -> String {
    SCRATCH_PKG_NAME.to_owned()
}

/// Run `kio check` against the synthesized package inside `scratch`.
///
/// Uses [`crate::cmd::check::compile_workspace_at_buffered`] with the
/// scratch directory as the explicit workspace root, so the validator
/// never mutates the process's cwd. Concurrent validations against
/// different scratch directories are race-free at the file-system
/// level (each scratch is per-snippet, see [`create_scratch_dir`]).
///
/// Returns the exit code and the diagnostic `kio check` produced (empty
/// when it accepted the snippet). The diagnostic is buffered rather than
/// printed: a rejection is the *expected* outcome for a snippet declaring
/// a non-zero `check_exit_code`, and snippets are checked in parallel, so
/// letting the typechecker write straight to stderr both cried wolf on
/// success and interleaved worker output. `validate_snippet` decides
/// whether the buffer is worth showing.
fn run_kio_check(
    scratch: &Path,
    assembled: &AssembledProgram,
    support_files: &[AssembledFile],
    md_path: &Path,
    snippet: &Snippet,
) -> std::io::Result<(ExitCode, String)> {
    write_support_files(scratch, support_files)?;
    write_assembled_program(scratch, assembled)?;

    // The buffered diagnostic names the synthesized scratch path. The
    // surrounding caller — `validate_snippet` — prefixes any failure
    // with its own message naming the markdown file + line and
    // prints the assembled program, so the operator can correlate.
    let _ = md_path;
    let _ = snippet;
    // `prime_only = false`: kio doc validates surface-language Kio.
    // `skip_ok = true`: discard the typed workspace, only the exit
    // code matters here.
    let mut diagnostic = String::new();
    let exit = match crate::cmd::check::compile_workspace_at_buffered(
        scratch,
        false,
        true,
        &mut diagnostic,
    ) {
        Ok(_) => ExitCode::Success,
        Err(code) => code,
    };
    Ok((exit, diagnostic))
}

fn write_assembled_program(scratch: &Path, assembled: &AssembledProgram) -> std::io::Result<()> {
    for file in &assembled.files {
        write_assembled_file(scratch, file)?;
    }
    Ok(())
}

fn write_support_files(scratch: &Path, support_files: &[AssembledFile]) -> std::io::Result<()> {
    for file in support_files {
        write_assembled_file(scratch, file)?;
    }
    Ok(())
}

fn write_assembled_file(scratch: &Path, file: &AssembledFile) -> std::io::Result<()> {
    let path = scratch.join(&file.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(path)?;
    f.write_all(file.body.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_no_package_sections() {
        let (e, m) = split_package_and_module("module x/main;\nlet x = 1;\n");
        assert!(e.is_empty());
        assert!(m.contains("module x/main"));
    }

    #[test]
    fn split_package_sections_extracted() {
        // A leading `bridge { … }` package section moves to the package
        // file; the module body starts at the first `module` line. (Host
        // items live in modules now, so `env` is no longer a package
        // section — see `is_package_section_start`.)
        let prog = "bridge {\n  x/main;\n}\n\nmodule x/main;\nlet x = 1;\n";
        let (e, m) = split_package_and_module(prog);
        assert!(e.contains("bridge {"));
        assert!(e.contains("x/main;"));
        assert!(m.starts_with("module x/main"));
        assert!(!m.contains("bridge {"));
    }

    #[test]
    fn substitute_into_harness_inserts_at_marker() {
        let h = "package x\n__INSERT_CODE_HERE__\nend";
        let out = substitute_into_harness(h, "__INSERT_CODE_HERE__", "let x = 1");
        assert!(out.contains("let x = 1"));
        assert!(out.contains("package x"));
        assert!(out.contains("end"));
    }

    #[test]
    fn aggregate_member_bodies_concatenates_in_order() {
        use crate::kiodoc::document::Snippet;
        let mk = |body: &str| Snippet {
            harness_ref: Some("m".to_owned()),
            ignored: false,
            check_exit_code: 0,
            expects_stdout: false,
            expects_stderr: false,
            run_exit_code: None,
            placeholders: Vec::new(),
            body: body.to_owned(),
            span: Span::new(0, 0),
            open_line: 1,
            body_offset: 0,
            stdout_fence: None,
            stderr_fence: None,
            variant: KioFileKind::Module,
        };
        let members = vec![mk("fn a() -> . { () }"), mk("fn b() -> . { () }")];
        let out = aggregate_member_bodies(&members);
        assert!(out.contains("fn a"));
        assert!(out.contains("fn b"));
        // Members joined with at least one newline so concatenated
        // top-level forms don't run together.
        let a_idx = out.find("fn a").unwrap();
        let b_idx = out.find("fn b").unwrap();
        assert!(a_idx < b_idx);
        assert!(out[a_idx..b_idx].contains('\n'));
    }

    #[test]
    fn aggregate_member_bodies_inserts_newline_when_missing() {
        use crate::kiodoc::document::Snippet;
        let mk = |body: &str| Snippet {
            harness_ref: Some("m".to_owned()),
            ignored: false,
            check_exit_code: 0,
            expects_stdout: false,
            expects_stderr: false,
            run_exit_code: None,
            placeholders: Vec::new(),
            body: body.to_owned(),
            span: Span::new(0, 0),
            open_line: 1,
            body_offset: 0,
            stdout_fence: None,
            stderr_fence: None,
            variant: KioFileKind::Module,
        };
        // Body does not end in a newline — aggregator must insert one
        // before the next member.
        let members = vec![mk("fn a() -> . { () }"), mk("fn b() -> . { () }")];
        let out = aggregate_member_bodies(&members);
        // The boundary between `fn a` and `fn b` must include a newline.
        let boundary = out.find("fn b").expect("second member should be present");
        let before = &out[..boundary];
        assert!(before.ends_with('\n'));
    }

    #[test]
    fn module_import_paths_use_complete_grammar_and_defer_bodies() {
        let source = "module c; import helpers/a(op _ (=) _); \
            import helpers/b(varop [% %]); \
            import helpers/c as c; import __intrinsics__; \
            fn broken() -> . { let x = ; () }";
        crate::pass::parser::parse_module_file_lazy(source)
            .expect("valid complete import headers with a deferred function body");
        let body_error = crate::pass::parser::parse_module_file(source)
            .expect_err("the eager parser must reject the malformed function body");
        assert!(body_error.diagnostic().span.start as usize >= source.find("fn broken").unwrap());
        assert_eq!(
            module_import_paths(source),
            BTreeSet::from([
                "helpers/a".to_owned(),
                "helpers/b".to_owned(),
                "helpers/c".to_owned()
            ])
        );
    }

    #[test]
    fn inserted_bridge_glob_preserves_separators_and_comments() {
        for (body, expected) in [
            ("bridge {\n  pkg\n}\n", "bridge {\n  pkg;\n  support;\n}\n"),
            ("bridge {\n  pkg;\n}\n", "bridge {\n  pkg;\n  support;\n}\n"),
            (
                "bridge { pkg; pkg/nested }\n",
                "bridge { pkg; pkg/nested;\n  support;\n}\n",
            ),
            (
                "bridge { pkg // final glob }\n}\n",
                "bridge { pkg; // final glob }\n  support;\n}\n",
            ),
            (
                "bridge { pkg; // final glob }\n} // after block }\n",
                "bridge { pkg; // final glob }\n  support;\n} // after block }\n",
            ),
            (
                "bridge { // empty block }\n}\n",
                "bridge { // empty block }\n  support;\n}\n",
            ),
            ("bridge {}\n", "bridge {\n  support;\n}\n"),
            ("bridge {; pkg }\n", "bridge {; pkg;\n  support;\n}\n"),
            ("", "\n\nbridge {\n  support;\n}\n"),
        ] {
            for prefix in ["", "package pkg; ", "package pkg;\n"] {
                let source = format!("{prefix}{body}");
                let original =
                    crate::pass::parser::parse_package_file(&format!("package pkg;\n{body}"), None)
                        .unwrap();
                let output = package_file_with_inserted_glob(&source, "support");
                let expected = if body.is_empty() {
                    format!("{}{}", prefix.trim_end(), expected)
                } else {
                    format!("{prefix}{expected}")
                };
                assert_eq!(output, expected, "source: {source:?}");
                let parsed_source = if prefix.is_empty() {
                    format!("package pkg;\n{output}")
                } else {
                    output
                };
                let parsed = crate::pass::parser::parse_package_file(&parsed_source, None)
                    .unwrap_or_else(|error| panic!("{parsed_source:?}: {error:?}"));
                let mut expected_globs: Vec<_> = original
                    .bridge
                    .into_iter()
                    .flat_map(|bridge| bridge.globs.into_iter().map(|glob| glob.segments))
                    .collect();
                expected_globs.push(vec![crate::ast::BridgeGlobSegment::Literal(
                    "support".to_owned(),
                )]);
                let actual_globs: Vec<_> = parsed
                    .bridge
                    .unwrap()
                    .globs
                    .into_iter()
                    .map(|glob| glob.segments)
                    .collect();
                assert_eq!(actual_globs, expected_globs);
            }
        }
    }

    #[test]
    fn support_files_add_bridge_globs_to_package_files() {
        let assembled = AssembledProgram {
            files: vec![AssembledFile {
                path: "kiodoc.pkg.kio".to_owned(),
                body: "package kiodoc;\n\nbridge {\n  kiodoc;\n}\n".to_owned(),
            }],
        };
        let support_files = vec![AssembledFile {
            path: "show.kio".to_owned(),
            body: "module show;\nhost type String role(str);\nhost fn print(s: String) -> .;\n"
                .to_owned(),
        }];

        let assembled = assembled_with_support_bridges(assembled, &support_files);
        let package_file = &assembled.files[0].body;
        assert!(package_file.contains("show;"), "got:\n{package_file}");
        assert!(
            crate::pass::parser::parse_package_file(package_file, None).is_ok(),
            "synthesized package file did not parse:\n{package_file}"
        );
    }

    #[test]
    fn support_files_add_empty_glob_bridge() {
        let assembled = AssembledProgram {
            files: vec![AssembledFile {
                path: "kiodoc.pkg.kio".to_owned(),
                body: "package kiodoc;\n\nbridge {\n  kiodoc;\n}\n".to_owned(),
            }],
        };
        let support_files = vec![AssembledFile {
            path: "match.kio".to_owned(),
            body: "module match;\nfn id[A](x: A) -> A { x }\n".to_owned(),
        }];

        let assembled = assembled_with_support_bridges(assembled, &support_files);
        assert!(assembled.files[0].body.contains("match;"));
    }

    #[test]
    fn document_root_files_add_glob_bridges_to_package_files() {
        let assembled = AssembledProgram {
            files: vec![
                AssembledFile {
                    path: "pkg.pkg.kio".to_owned(),
                    body: "package pkg;\n\nbridge {\n  pkg;\n}\n".to_owned(),
                },
                AssembledFile {
                    path: "utils.kio".to_owned(),
                    body: "module utils;\n\
                           pub fn id[A](x: A) -> A { x }\n"
                        .to_owned(),
                },
            ],
        };

        let assembled = assembled_with_support_bridges(assembled, &[]);
        assert!(assembled.files[0].body.contains("utils;"));
    }

    #[test]
    fn selects_transitive_package_support_files() {
        let assembled = AssembledProgram {
            files: vec![AssembledFile {
                path: "kiodoc/main.kio".to_owned(),
                body: "module kiodoc/main;\nimport match(match);\n".to_owned(),
            }],
        };
        let support_files = vec![
            AssembledFile {
                path: "match.kio".to_owned(),
                body: "module match;\nimport spine(fit);\n".to_owned(),
            },
            AssembledFile {
                path: "show.kio".to_owned(),
                body: "module show;\n".to_owned(),
            },
            AssembledFile {
                path: "spine.kio".to_owned(),
                body: "module spine;\n".to_owned(),
            },
        ];
        let selected = select_package_support_files(&assembled, &support_files);
        let paths: Vec<_> = selected.into_iter().map(|file| file.path).collect();
        assert_eq!(paths, vec!["match.kio".to_owned(), "spine.kio".to_owned()]);
    }

    #[test]
    fn assembled_root_files_shadow_package_support_files() {
        let assembled = AssembledProgram {
            files: vec![
                AssembledFile {
                    path: "kiodoc/main.kio".to_owned(),
                    body: "module kiodoc/main;\nimport utils/list_ops(fold);\n".to_owned(),
                },
                AssembledFile {
                    path: "utils.kio".to_owned(),
                    body: "module utils;\nimport helper(helper);\n".to_owned(),
                },
            ],
        };
        let support_files = vec![
            AssembledFile {
                path: "helper.kio".to_owned(),
                body: "module helper;\n".to_owned(),
            },
            AssembledFile {
                path: "utils.kio".to_owned(),
                body: "module utils;\nimport stale(stale);\n".to_owned(),
            },
            AssembledFile {
                path: "stale.kio".to_owned(),
                body: "module stale;\n".to_owned(),
            },
        ];
        let selected = select_package_support_files(&assembled, &support_files);
        let paths: Vec<_> = selected.into_iter().map(|file| file.path).collect();
        assert_eq!(paths, vec!["helper.kio".to_owned()]);
    }

    #[test]
    fn selected_nested_modules_follow_each_exact_import_closure() {
        let assembled = AssembledProgram {
            files: vec![AssembledFile {
                path: "guide/main.kio".to_owned(),
                body: "module guide/main; import helpers/a as a; import helpers/b as b;".to_owned(),
            }],
        };
        let support_files = [
            (
                "helpers/a.kio",
                "module helpers/a; import left/item as item;",
            ),
            (
                "helpers/b.kio",
                "module helpers/b; import right/item as item;",
            ),
            ("left/item.kio", "module left/item;"),
            ("right/item.kio", "module right/item;"),
            ("helpers/unselected.kio", "not a valid module"),
            ("unrelated.kio", "not a valid module"),
        ]
        .into_iter()
        .map(|(path, body)| AssembledFile {
            path: path.to_owned(),
            body: body.to_owned(),
        })
        .collect::<Vec<_>>();
        let selected = select_package_support_files(&assembled, &support_files);
        let paths: Vec<_> = selected.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "helpers/a.kio",
                "helpers/b.kio",
                "left/item.kio",
                "right/item.kio"
            ]
        );
    }

    #[test]
    fn nested_support_bridges_match_modules_not_path_prefixes() {
        for (existing, needs_exact) in [("helpers/a/b", true), ("helpers/**", false)] {
            let assembled = AssembledProgram {
                files: vec![AssembledFile {
                    path: "pkg.pkg.kio".to_owned(),
                    body: format!("package pkg;\nbridge {{\n  {existing};\n}}\n"),
                }],
            };
            let support_files = vec![AssembledFile {
                path: "helpers/a.kio".to_owned(),
                body: "module helpers/a;".to_owned(),
            }];
            let result = assembled_with_support_bridges(assembled, &support_files);
            let body = &result.files[0].body;
            assert_eq!(body.contains("  helpers/a;"), needs_exact, "{body}");
            assert!(!body.contains("  helpers;"), "{body}");
            let package = crate::pass::parser::parse_package_file(body, None).unwrap();
            assert!(
                package.bridge.unwrap().globs.iter().any(|glob| {
                    crate::pass::resolve::glob_matches(&glob.segments, "helpers/a")
                })
            );
        }
    }

    #[test]
    fn validation_body_applies_placeholders_without_cascading() {
        use crate::kiodoc::document::{Snippet, SnippetPlaceholder};
        let snippet = Snippet {
            harness_ref: None,
            ignored: false,
            check_exit_code: 0,
            expects_stdout: false,
            expects_stderr: false,
            run_exit_code: None,
            placeholders: vec![
                SnippetPlaceholder {
                    from: "...".to_owned(),
                    to: "???".to_owned(),
                },
                SnippetPlaceholder {
                    from: "???".to_owned(),
                    to: "fallback()".to_owned(),
                },
            ],
            body: "fn f() -> String { ... }\nfn g() -> String { ??? }\n".to_owned(),
            span: Span::new(0, 0),
            open_line: 1,
            body_offset: 0,
            stdout_fence: None,
            stderr_fence: None,
            variant: KioFileKind::Module,
        };
        let out = validation_body(&snippet);
        assert!(out.contains("fn f() -> String { ??? }"));
        assert!(out.contains("fn g() -> String { fallback() }"));
    }
}

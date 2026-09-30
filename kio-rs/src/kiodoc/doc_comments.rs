//! Doc-comment input surface for `kio doc`.
//!
//! Extends the Kiodoc validation to `///` doc-comments attached to
//! module-level declarations inside `.kio` source files. See
//! [`specs/kiodoc.md`](../../../../specs/kiodoc.md) §
//! "Doc-comment input surface" for the contract.
//!
//! ## Pipeline
//!
//! 1. [`extract_doc_snippets`] parses the `.kio` source file,
//!    collects every doc-comment attached to a top-level decl or
//!    the module node itself, and returns a flat list of
//!    [`DocCommentSnippet`] values — one per `kio` fence (non-ignored)
//!    inside a doc-comment body.
//! 2. The caller fans those snippets out over rayon (same pattern as
//!    the `.md` file path in the main `doc.rs` driver).
//! 3. [`validate_doc_comment_snippet`] handles each snippet: for a
//!    `{@}` snippet it builds the augmented module source (original
//!    source + synthetic fn), writes it to a scratch directory, and
//!    runs `kio check` in-process.
//!
//! ## Fence modes inside `///` doc-comments
//!
//! - **`{@}`** — the surrounding module is the harness. The snippet
//!   body is wrapped in a synthetic top-level fn, appended to the
//!   module, and typechecked.
//! - **`{ignore}`** — skip the snippet. Identical to the `.md` form.
//! - **`{@NAME}`, `{}`** — rejected with a clear diagnostic.
//!   Authors who need a custom harness or a standalone program write
//!   a tutorial in a `.md` file instead.
//! - **`harness=NAME`** — rejected for the same reason.

use crate::path_display::DisplayPath;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::scratch::create_scratch_dir;
use crate::ast::{DocComment, Item, Module};
use crate::exit_code::ExitCode;
use crate::kiodoc::cache::{CachedResult, DocCache, DocCacheKey};
use crate::kiodoc::directives;
use crate::kiodoc::parse;
use crate::kiodoc::refs;
use crate::span::Span;

// ---- public types -----------------------------------------------------------

/// A single `kio` fence extracted from a `///` doc-comment, ready for
/// validation.
#[derive(Debug, Clone)]
pub struct DocCommentSnippet {
    /// Absolute path to the source `.kio` file.
    pub kio_path: PathBuf,
    /// Original source text of the `.kio` file — used to build the
    /// augmented source and map errors back to the source line.
    pub kio_source: String,
    /// The module name component (e.g., `fmt_doc_comments.main`).
    pub module_name: String,
    /// Name of the documented item — empty string for the module-level
    /// doc-comment.
    pub item_name: String,
    /// 0-based index of this snippet among all snippets on the same
    /// item (one item may carry multiple fences).
    pub snippet_index: usize,
    /// The fence body text.
    pub body: String,
    /// Byte offset of the opening fence within the doc-comment body
    /// (after leading-`///` stripping). Used for span-mapping
    /// diagnostics.
    pub fence_span: Span,
    /// 1-based line number of the opening fence marker within the
    /// doc-comment body text.
    pub open_line_in_doc: usize,
    /// `check_exit_code` declared on the fence (default `0`).
    pub check_exit_code: i32,
}

/// A doc-comment-level error.
#[derive(Debug)]
pub struct DocCommentError {
    /// Byte offset (into `kio_source`) of the start of the
    /// doc-comment or fence that caused the error.
    pub span: Span,
    pub message: String,
    /// Assembled source (module + synthetic fn) printed on snippet
    /// check failure, so the author can see what was typechecked.
    pub assembled: Option<String>,
    /// What `kio check` reported, captured rather than printed so a
    /// snippet whose exit code matches its declared `check_exit_code`
    /// stays silent — an expected failure is a validation *success*.
    /// `None` when the typechecker never ran (cache hit, extraction-level
    /// error) or when it accepted the snippet.
    pub diagnostic: Option<String>,
}

impl DocCommentError {
    pub fn eprint(&self, kio_path: &Path, kio_source: &str) {
        eprint!("{}", self.render(kio_path, kio_source));
    }

    /// As [`Self::eprint`], but returns the rendered diagnostic (with a
    /// trailing newline) so the multi-package fan-out can buffer it.
    pub fn render(&self, kio_path: &Path, kio_source: &str) -> String {
        use std::fmt::Write as _;
        let (line, col) = line_col(kio_source, self.span.start);
        let mut out = format!(
            "{}:{}:{}: {}\n",
            DisplayPath(&kio_path),
            line,
            col,
            self.message
        );
        if let Some(diag) = &self.diagnostic {
            let _ = writeln!(out, "--- kio check diagnostic ---");
            let _ = writeln!(out, "{}", diag.trim_end_matches('\n'));
        }
        if let Some(asm) = &self.assembled {
            let _ = writeln!(out, "--- assembled program ---");
            let _ = writeln!(out, "{}", asm.trim_end_matches('\n'));
        }
        out
    }
}

// ---- extraction -------------------------------------------------------------

/// Parse `kio_source` as a regular Kio module and extract every
/// `kio` fence from every doc-comment attached to the module node
/// and to each top-level decl.
///
/// Returns a list of [`DocCommentSnippet`] values (non-ignored only)
/// and a list of [`DocCommentError`] values for any doc-comment-level
/// violations encountered during extraction (e.g. `{@NAME}` /
/// `{harness=N}` inside a doc-comment, bare ` ```kio ` with no attrs).
///
/// Also validates any `` [`name`] `` intra-doc references found in
/// the doc-comment prose against the module's name-resolution scope.
/// Broken references are reported as [`DocCommentError`] values.
///
/// Does not run `kio check` — that is the caller's responsibility.
pub fn extract_doc_snippets(
    kio_path: &Path,
    kio_source: &str,
) -> Result<Vec<DocCommentSnippet>, Vec<DocCommentError>> {
    let module = match crate::pass::parser::parse(kio_source) {
        Ok(m) => m,
        Err(_) => {
            // Parse failures are reported by `kio check`; we don't
            // duplicate them here. Return empty — no snippets to
            // validate.
            return Ok(Vec::new());
        }
    };

    extract_doc_snippets_from_module(kio_path, kio_source, &module)
}

fn extract_doc_snippets_from_module(
    kio_path: &Path,
    kio_source: &str,
    module: &Module,
) -> Result<Vec<DocCommentSnippet>, Vec<DocCommentError>> {
    let module_name = module.path.segments.join("/");
    let mut snippets: Vec<DocCommentSnippet> = Vec::new();
    let mut errors: Vec<DocCommentError> = Vec::new();

    // Build the module-level name scope once; shared across all
    // doc-comments in this file.
    let scope = refs::module_scope_from_surface(module);

    // Module-level doc-comment.
    if let Some(doc) = &module.doc {
        extract_from_doc(
            kio_path,
            kio_source,
            &module_name,
            "",
            doc,
            &mut snippets,
            &mut errors,
        );
        // Validate intra-doc refs in the module-level doc-comment prose.
        check_doc_comment_refs(kio_source, doc, &scope, &mut errors);
    }

    // Per-item doc-comments.
    for item in &module.items {
        if let Item::RecGroup(group, _) = item {
            for member in &group.members {
                if let Some(doc) = &member.doc {
                    extract_from_doc(
                        kio_path,
                        kio_source,
                        &module_name,
                        &member.name,
                        doc,
                        &mut snippets,
                        &mut errors,
                    );
                    check_doc_comment_refs(kio_source, doc, &scope, &mut errors);
                }
            }
        } else if let Item::TypeRecGroup(group) = item {
            if let Some(doc) = &group.doc {
                extract_from_doc(
                    kio_path,
                    kio_source,
                    &module_name,
                    "",
                    doc,
                    &mut snippets,
                    &mut errors,
                );
                check_doc_comment_refs(kio_source, doc, &scope, &mut errors);
            }
            for member in &group.members {
                let (item_name, doc) = match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => {
                        (alias.name.as_str(), alias.doc.as_ref())
                    }
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        (newtype.name.as_str(), newtype.doc.as_ref())
                    }
                    crate::ast::TypeRecMember::Labels(labels, _) => (
                        labels.type_alias_name.as_deref().unwrap_or(""),
                        labels.doc.as_ref(),
                    ),
                };
                if let Some(doc) = doc {
                    extract_from_doc(
                        kio_path,
                        kio_source,
                        &module_name,
                        item_name,
                        doc,
                        &mut snippets,
                        &mut errors,
                    );
                    check_doc_comment_refs(kio_source, doc, &scope, &mut errors);
                }
            }
        } else {
            let (item_name, doc) = item_name_and_doc(item);
            if let Some(doc) = doc {
                extract_from_doc(
                    kio_path,
                    kio_source,
                    &module_name,
                    item_name.as_ref(),
                    doc,
                    &mut snippets,
                    &mut errors,
                );
                check_doc_comment_refs(kio_source, doc, &scope, &mut errors);
            }
        }
    }

    if errors.is_empty() {
        Ok(snippets)
    } else {
        Err(errors)
    }
}

/// Scan the prose text of a doc-comment for `` [`name`] `` intra-doc
/// references and `` [`@KEYWORD(term)`] `` directives, validating each
/// against the module scope. Appends any errors to `errors`.
///
/// "Prose" here means the doc-comment body text with fence bodies
/// excluded — we only validate refs in narrative text, not in Kio
/// source fences (those are validated as snippets).
fn check_doc_comment_refs(
    kio_source: &str,
    doc: &DocComment,
    scope: &refs::ModuleScope,
    errors: &mut Vec<DocCommentError>,
) {
    let doc_body = doc.lines.join("\n");
    // Collection excludes fenced examples internally.
    let overrides = refs::collect_ref_overrides(&doc_body);

    let prose = parse::blank_fences(&doc_body);

    // Names for "did you mean?" suggestions — shared by both checks.
    let all_names: Vec<String> = scope
        .top_level
        .iter()
        .chain(scope.imported.iter())
        .cloned()
        .collect();

    // Validate plain `` [`name`] `` intra-doc refs.
    let ref_errors = refs::check_refs_in_module(&prose, scope, &overrides);
    for re in ref_errors {
        let span_start =
            doc_offset_to_source_offset(kio_source, &doc_body, doc.span.start, re.offset as usize);
        let suggestion = refs::closest_name(&re.name, &all_names);
        let message = refs::format_unresolved_ref_message(&re.name, suggestion);
        errors.push(DocCommentError {
            span: crate::span::Span::new(span_start, span_start),
            message,
            assembled: None,
            diagnostic: None,
        });
    }

    // Validate `` [`@KEYWORD term`] `` directives.
    let (unknown_kw_errors, unresolved_term_errors, type_level_errors) =
        directives::check_directives_in_module(&prose, scope, &overrides);

    for uke in unknown_kw_errors {
        let span_start =
            doc_offset_to_source_offset(kio_source, &doc_body, doc.span.start, uke.offset as usize);
        let message = directives::format_unknown_directive_message(&uke.keyword);
        errors.push(DocCommentError {
            span: crate::span::Span::new(span_start, span_start),
            message,
            assembled: None,
            diagnostic: None,
        });
    }

    for ute in unresolved_term_errors {
        let span_start =
            doc_offset_to_source_offset(kio_source, &doc_body, doc.span.start, ute.offset as usize);
        let suggestion = refs::closest_name(&ute.term, &all_names);
        let message =
            directives::format_unresolved_directive_message(ute.keyword, &ute.term, suggestion);
        errors.push(DocCommentError {
            span: crate::span::Span::new(span_start, span_start),
            message,
            assembled: None,
            diagnostic: None,
        });
    }

    for tle in type_level_errors {
        let span_start =
            doc_offset_to_source_offset(kio_source, &doc_body, doc.span.start, tle.offset as usize);
        let message = directives::format_type_level_directive_message(&tle.term, tle.kind);
        errors.push(DocCommentError {
            span: crate::span::Span::new(span_start, span_start),
            message,
            assembled: None,
            diagnostic: None,
        });
    }
}

/// Map a byte offset within `doc_body` to a byte offset in `kio_source`.
///
/// `doc_start` is the byte offset of the doc-comment's first `///` in
/// `kio_source`. `offset_in_prose` is the byte offset within `doc_body`
/// (after leading-`///` stripping) where the error was found.
fn doc_offset_to_source_offset(
    kio_source: &str,
    doc_body: &str,
    doc_start: u32,
    offset_in_prose: usize,
) -> u32 {
    let doc_start_line = line_number(kio_source, doc_start);
    let ref_line_in_doc = line_number_in(doc_body, offset_in_prose);
    let source_line = doc_start_line + ref_line_in_doc.saturating_sub(1);
    line_start_offset(kio_source, source_line)
}

/// 1-based line number of byte `offset` within `text`.
fn line_number_in(text: &str, offset: usize) -> usize {
    let upto = offset.min(text.len());
    text[..upto].matches('\n').count() + 1
}

/// The name and doc-comment of an [`Item`], if it has one.
fn item_name_and_doc(item: &Item) -> (std::borrow::Cow<'_, str>, Option<&DocComment>) {
    let (name, doc) = match item {
        Item::FnDef(f) => (f.name.as_str(), f.doc.as_ref()),
        Item::TypeAlias(a) => (a.name.as_str(), a.doc.as_ref()),
        Item::Newtype(n) => (n.name.as_str(), n.doc.as_ref()),
        Item::LiteralAlias(l, _) => (l.name.as_str(), l.doc.as_ref()),
        Item::Labels(t, _) => (t.type_alias_name.as_deref().unwrap_or(""), t.doc.as_ref()),
        Item::LabelForward(forward, _) => {
            return (format!("{{{}}}", forward.name).into(), forward.doc.as_ref());
        }
        Item::Op(o, _) => ("", o.doc.as_ref()),
        Item::VariadicOperator(f, _) => ("", f.doc.as_ref()),
        Item::Elaborator(s, _) => (s.name.as_str(), s.doc.as_ref()),
        Item::HostType(h) => (h.name.as_str(), h.doc.as_ref()),
        Item::HostFn(h) => (h.name.as_str(), h.doc.as_ref()),
        Item::RecGroup(_, _) => ("", None),
        Item::TypeRecGroup(_) => ("", None),
        Item::Equiv(_, _) => ("", None),
    };
    (name.into(), doc)
}

/// Walk the doc-comment body, run the fence scanner over it, and
/// collect `kio` fences as snippets (or errors).
fn extract_from_doc(
    kio_path: &Path,
    kio_source: &str,
    module_name: &str,
    item_name: &str,
    doc: &DocComment,
    snippets: &mut Vec<DocCommentSnippet>,
    errors: &mut Vec<DocCommentError>,
) {
    // Reassemble the doc-comment body as a contiguous string —
    // the fence scanner expects a block of text.
    let doc_body = doc.lines.join("\n");
    let fences = parse::scan(&doc_body);

    // Count how many kio snippets we've seen for this item so far
    // (for the `_<NN>` suffix in the synthetic fn name). The
    // per-item counter resets per call, so it's relative to the
    // item.
    let first_idx = snippets
        .iter()
        .filter(|s| s.module_name == module_name && s.item_name == item_name)
        .count();
    let mut per_item = 0usize;

    // Compute the 1-based line number of the doc-comment's first `///`
    // in the kio source — used to map fence.open_line (1-based within
    // doc_body) back to a source line number and byte offset.
    let doc_start_line = line_number(kio_source, doc.span.start);

    for fence in fences {
        if fence.lang != "kio" {
            // Non-kio fences in doc-comments are opaque prose.
            continue;
        }

        // Map the fence's open_line (1-based within doc_body) to a
        // byte offset in the original kio source. Diagnostics use
        // this offset so the error message names the right `.kio`
        // file line.
        //
        // Each line in `doc_body` corresponds to one `/// ...` line
        // in the source: line 1 of doc_body = line `doc_start_line`
        // in kio_source, line 2 = `doc_start_line + 1`, etc.
        let source_line = doc_start_line + fence.open_line.saturating_sub(1);
        let fence_kio_span = Span::new(
            line_start_offset(kio_source, source_line),
            line_start_offset(kio_source, source_line),
        );

        // Parse the fence's attribute list.
        let attrs = match &fence.attrs_text {
            Some(text) => match crate::kiodoc::attrs::parse(text) {
                Ok(a) => a,
                Err(err) => {
                    errors.push(DocCommentError {
                        span: fence_kio_span,
                        message: format!(
                            "invalid fence attribute list in doc-comment: {}",
                            err.message
                        ),
                        assembled: None,
                        diagnostic: None,
                    });
                    continue;
                }
            },
            None => {
                // Bare ` ```kio ` with no attribute list.
                errors.push(DocCommentError {
                    span: fence_kio_span,
                    message: "bare ```kio fence in doc-comment: write `{@}` to use the \
                              surrounding module as harness, or `{ignore}` to skip"
                        .to_owned(),
                    assembled: None,
                    diagnostic: None,
                });
                continue;
            }
        };

        // Check for disallowed modes inside doc-comments.
        if let Some(name) = &attrs.harness_ref {
            errors.push(DocCommentError {
                span: fence_kio_span,
                message: format!(
                    "inside a `///` doc-comment, the harness is the surrounding \
                     module — use `{{@}}` instead of `{{@{name}}}`"
                ),
                assembled: None,
                diagnostic: None,
            });
            continue;
        }
        // `{}` standalone form — rejected in doc-comments. Standalone
        // means: no `@NAME` harness-ref, no self-ref, no `ignore`.
        // Authors needing a standalone snippet write a `.md` tutorial.
        if !attrs.self_ref
            && attrs.harness_ref.is_none()
            && !attrs.bare.iter().any(|b| b.name == "ignore")
        {
            errors.push(DocCommentError {
                span: fence_kio_span,
                message: "inside a `///` doc-comment, use `{@}` to wrap the snippet \
                          in the surrounding module — `{}` standalone is not supported \
                          in doc-comments (use a `.md` tutorial for standalone snippets)"
                    .to_owned(),
                assembled: None,
                diagnostic: None,
            });
            continue;
        }
        if attrs.kvs.iter().any(|kv| kv.key == "harness") {
            errors.push(DocCommentError {
                span: fence_kio_span,
                message: "inside a `///` doc-comment, the harness is the surrounding \
                          module — `harness=NAME` is not supported here"
                    .to_owned(),
                assembled: None,
                diagnostic: None,
            });
            continue;
        }

        // `{ignore}` — skip.
        let ignore = attrs.bare.iter().any(|b| b.name == "ignore");
        if ignore {
            continue;
        }

        // Track whether any error was found for this fence so we don't
        // emit a snippet when there were attribute errors.
        let errors_before = errors.len();

        // Parse `check_exit_code`.
        let mut check_exit_code: i32 = 0;
        for kv in &attrs.kvs {
            match kv.key.as_str() {
                "check_exit_code" => match kv.value.parse::<i32>() {
                    Ok(n) => check_exit_code = n,
                    Err(_) => {
                        errors.push(DocCommentError {
                            span: fence_kio_span,
                            message: format!(
                                "`check_exit_code=N` requires an integer value, got `{}`",
                                kv.value
                            ),
                            assembled: None,
                            diagnostic: None,
                        });
                    }
                },
                other => {
                    errors.push(DocCommentError {
                        span: fence_kio_span,
                        message: format!(
                            "unknown key-value attribute `{other}` on a `kio` fence \
                             in doc-comment"
                        ),
                        assembled: None,
                        diagnostic: None,
                    });
                }
            }
        }

        // Unknown bare attrs?
        for bare in &attrs.bare {
            if bare.name != "ignore" {
                errors.push(DocCommentError {
                    span: fence_kio_span,
                    message: format!(
                        "unknown bare attribute `{}` on a `kio` fence in doc-comment",
                        bare.name
                    ),
                    assembled: None,
                    diagnostic: None,
                });
            }
        }

        // `{@}` snippet — collect it (only when no attribute errors).
        if attrs.self_ref && errors.len() == errors_before {
            let idx = first_idx + per_item;
            per_item += 1;
            snippets.push(DocCommentSnippet {
                kio_path: kio_path.to_path_buf(),
                kio_source: kio_source.to_owned(),
                module_name: module_name.to_owned(),
                item_name: item_name.to_owned(),
                snippet_index: idx,
                body: fence.body.clone(),
                fence_span: fence_kio_span,
                open_line_in_doc: fence.open_line,
                check_exit_code,
            });
        }
    }
}

// ---- validation -------------------------------------------------------------

/// Validate one `{@}` doc-comment snippet. Builds an augmented module
/// source (the original module source with a synthetic fn appended),
/// writes it to a scratch directory, and runs `kio check`.
///
/// The `cache` is consulted identically to the `.md` snippet path.
pub fn validate_doc_comment_snippet(
    snippet: &DocCommentSnippet,
    cache: &DocCache,
) -> Result<(), DocCommentError> {
    let synthetic_fn_name = make_synthetic_fn_name(snippet);
    let augmented = build_augmented_source(snippet, &synthetic_fn_name);

    let key = make_cache_key(&augmented, snippet.check_exit_code);
    let (actual_i32, diagnostic) = match cache.lookup(&key) {
        Some(cached) => (cached.check_exit_code, None),
        None => {
            let scratch = match create_scratch_dir(&snippet.kio_path, snippet.open_line_in_doc) {
                Ok(p) => p,
                Err(e) => {
                    return Err(DocCommentError {
                        span: snippet.fence_span,
                        message: format!("could not create scratch directory: {e}"),
                        assembled: Some(augmented),
                        diagnostic: None,
                    });
                }
            };

            let (exit, diagnostic) = match run_kio_check(&scratch, snippet, &augmented) {
                Ok(x) => x,
                Err(e) => {
                    return Err(DocCommentError {
                        span: snippet.fence_span,
                        message: format!("internal error setting up snippet scratch package: {e}"),
                        assembled: Some(augmented),
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

    let expected = snippet.check_exit_code;
    if actual_i32 == expected {
        Ok(())
    } else {
        Err(DocCommentError {
            span: snippet.fence_span,
            message: format!(
                "doc-comment snippet for `{}` at line {}: `kio check` exited {} \
                 but {} was expected (via {})",
                if snippet.item_name.is_empty() {
                    snippet.module_name.as_str()
                } else {
                    snippet.item_name.as_str()
                },
                doc_comment_kio_line(&snippet.kio_source, snippet.fence_span.start),
                actual_i32,
                expected,
                if expected == 0 {
                    "default `check_exit_code=0`".to_owned()
                } else {
                    format!("`check_exit_code={expected}`")
                }
            ),
            assembled: Some(augmented),
            diagnostic: diagnostic.filter(|d| !d.trim().is_empty()),
        })
    }
}

// ---- helpers ----------------------------------------------------------------

/// Build the synthetic fn name.
///
/// `fn kiodoc_example_m<MODULE>_d<DECL>_i<NN>() -> . { … }`
///
/// Module and item identities use separate exact letter-encoded words, so
/// paths, operators, affixes, and type case remain distinct. The prefix is
/// chosen to be a valid Kio user identifier (does not start with
/// `__`, which is reserved for compiler-generated names).
fn make_synthetic_fn_name(snippet: &DocCommentSnippet) -> String {
    let mod_part = crate::naming::encode_name_component(&snippet.module_name);
    let decl_part = crate::naming::encode_name_component(&snippet.item_name);
    format!(
        "kiodoc_example_m{mod_part}_d{decl_part}_i{}",
        snippet.snippet_index
    )
}

/// Append the synthetic fn to the module source.
///
/// The synthetic fn has no parameters and a fixed `.` (unit) return
/// type — it is emitted as `fn <name>() -> . { <body> }`. We append it
/// after the last line of the existing source.
fn build_augmented_source(snippet: &DocCommentSnippet, fn_name: &str) -> String {
    let mut out = snippet.kio_source.clone();
    // Ensure there's at least one newline before the synthetic fn.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // Emit the synthetic fn as a block expression.
    out.push_str(&format!(
        "\nfn {fn_name}() -> . {{\n{body}\n}}\n",
        body = snippet.body
    ));
    out
}

/// Build a doc-cache key for a doc-comment snippet.  The inputs are
/// the augmented source text (deterministic given the module source +
/// snippet body) and the expected exit code.
fn make_cache_key(augmented_source: &str, check_exit_code: i32) -> DocCacheKey {
    // Use the augmented source as the "harness body" slot (it's the full
    // thing the typer sees) and the check_exit_code serialised as the
    // "snippet body" slot for differentiation.
    let exit_str = check_exit_code.to_string();
    DocCacheKey::new(augmented_source, &exit_str, None)
}

/// Run `kio check` against the augmented source inside `scratch`.
///
/// The augmented source is written as a single module file. The
/// original package's package file is located and copied to the
/// scratch directory, so env and bridge declarations are available.
///
/// Returns the exit code and the diagnostic `kio check` produced (empty
/// when it accepted the snippet), buffered for the same reason as the
/// `.md` snippet path — see [`super::validate`]'s `run_kio_check`.
fn run_kio_check(
    scratch: &Path,
    snippet: &DocCommentSnippet,
    augmented: &str,
) -> std::io::Result<(ExitCode, String)> {
    // Derive the package name and module path from the module name.
    let pkg_name = snippet.module_name.split('/').next().unwrap_or("kiodoc");
    let mod_segments: Vec<&str> = snippet.module_name.split('/').collect();

    // Locate the original package's package file alongside the source
    // file. The package file lives in the *package root* directory
    // (the directory containing the package's module subdirectory).
    // For a module at `<root>/<pkg>/main.kio`, the package file is
    // `<root>/<pkg>.pkg.kio`. We find it by searching upward
    // from `snippet.kio_path`'s parent directory.
    let package_file_content = find_and_read_package_file(&snippet.kio_path, pkg_name);

    // Write the package file. When the original package carries one,
    // its content already opens with the required
    // `package <pkg>;` directive. When there is none (a
    // doc-comment-only package with no host), synthesize a
    // bare package file that still carries the directive.
    let package_file_content = if package_file_content.trim().is_empty() {
        with_root_bridge_globs(&format!("package {pkg_name};\n"), pkg_name)
    } else {
        with_root_bridge_globs(&package_file_content, pkg_name)
    };
    let package_file_path = scratch.join(format!("{pkg_name}.pkg.kio"));
    {
        let mut f = fs::File::create(&package_file_path)?;
        f.write_all(package_file_content.as_bytes())?;
    }

    let root_module_path = scratch.join(format!("{pkg_name}.kio"));
    {
        let mut f = fs::File::create(&root_module_path)?;
        f.write_all(format!("module {pkg_name};\n").as_bytes())?;
    }

    // Write the augmented module file at the path the module
    // declaration implies, relative to the package root (the
    // scratch directory). Per `specs/package.md` § Module-name
    // rules, the declared segments equal the file's path relative
    // to the package root — the package name is no longer stripped
    // at the declaration. So `module pkg/main;` lands at
    // `<scratch>/pkg/main.kio` (a host-using module under the
    // package namespace), and `module main;` lands at
    // `<scratch>/main.kio`.
    let mut mod_path = scratch.to_path_buf();
    for seg in &mod_segments[..mod_segments.len().saturating_sub(1)] {
        mod_path.push(seg);
    }
    mod_path.push(format!(
        "{}.kio",
        mod_segments.last().copied().unwrap_or("main")
    ));
    if let Some(parent) = mod_path.parent() {
        fs::create_dir_all(parent)?;
    }
    {
        let mut f = fs::File::create(&mod_path)?;
        f.write_all(augmented.as_bytes())?;
    }

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

/// Ensure the package body's `bridge { … }` block selects the package's
/// root module and its submodules, so the assembled doc-comment module
/// (`module <pkg>;` or `module <pkg>/…;`) is bridged. Inserts the
/// `<pkg>;` and `<pkg>/**;` globs that aren't already present,
/// synthesizing the `bridge { … }` block when the package file carries
/// none. Reuses the glob-insertion machinery the `.md` snippet
/// assembler uses (`super::validate`).
fn with_root_bridge_globs(package_file: &str, package_name: &str) -> String {
    let mut out = package_file.to_owned();
    if !super::validate::package_file_has_bridge_glob(&out, package_name) {
        out = super::validate::package_file_with_inserted_glob(&out, package_name);
    }
    let submodule_glob = format!("{package_name}/**");
    if !bridge_lists_glob_literal(&out, &submodule_glob) {
        out = super::validate::package_file_with_inserted_glob(&out, &submodule_glob);
    }
    out
}

/// True when the package body's `bridge { … }` block lists the exact
/// glob spelling `glob` (e.g. `pkg/**`). Distinct from
/// `package_file_has_bridge_glob`, which matches on the leading literal
/// segment only — here we need the `/**` submodule glob specifically.
fn bridge_lists_glob_literal(package_file: &str, glob: &str) -> bool {
    package_file
        .lines()
        .map(str::trim_start)
        .any(|line| line.trim_end_matches(';').trim_end() == glob)
}

/// Try to find and read the package's package file next to the given
/// `.kio` source path. Looks for `<pkg_name>.pkg.kio` starting
/// in `kio_path`'s parent directory, then one level up (in case
/// `kio_path` is in a subdirectory like `<root>/<pkg>/main.kio` and
/// the package file is at `<root>/<pkg>.pkg.kio`).
///
/// Returns the package file's contents, or an empty string if not
/// found or unreadable. An empty package file is valid Kio — the
/// `compile_workspace_at` call just won't resolve any host items.
fn find_and_read_package_file(kio_path: &Path, pkg_name: &str) -> String {
    let export_name = format!("{pkg_name}.pkg.kio");

    // Check the immediate parent directory first.
    if let Some(parent) = kio_path.parent() {
        let candidate = parent.join(&export_name);
        if candidate.is_file() {
            return fs::read_to_string(&candidate).unwrap_or_default();
        }
        // Check one level up (the package root, for modules in a
        // subdirectory like `<root>/<pkg>/main.kio`).
        if let Some(grandparent) = parent.parent() {
            let candidate2 = grandparent.join(&export_name);
            if candidate2.is_file() {
                return fs::read_to_string(&candidate2).unwrap_or_default();
            }
        }
    }
    String::new()
}

/// Return the 1-based line number of byte `offset` in `source`.
fn line_number(source: &str, offset: u32) -> usize {
    let upto = (offset as usize).min(source.len());
    source[..upto].matches('\n').count() + 1
}

/// Return the byte offset of the start of the given 1-based `line`
/// in `source`. Returns `source.len()` when `line` exceeds the source.
fn line_start_offset(source: &str, line: usize) -> u32 {
    if line <= 1 {
        return 0;
    }
    let mut current_line = 1usize;
    for (i, ch) in source.char_indices() {
        if ch == '\n' {
            current_line += 1;
            if current_line == line {
                return (i + 1) as u32;
            }
        }
    }
    source.len() as u32
}

/// Convert a byte offset into `source` to a 1-based line number.
fn doc_comment_kio_line(source: &str, offset: u32) -> usize {
    line_number(source, offset)
}

/// Map a byte offset inside `source` to a 1-based `(line, column)`.
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let upto = (offset as usize).min(source.len());
    let prefix = &source[..upto];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..upto].chars().count() + 1;
    (line, col)
}

// ---- walker helper ----------------------------------------------------------

/// Walk `dir` recursively, appending every `.kio` source file
/// (excluding package files) to `out`. Skips
/// build-artifact (`out`, `target`) and hidden (`.*`) directories —
/// mirrors `collect_md_files` in `doc.rs`.
pub fn collect_kio_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // Un-followed file type so a directory symlink is treated as a
        // leaf, never descended — a symlink loop would otherwise recurse
        // unbounded. Matches `package_collection`'s discipline.
        if entry.file_type()?.is_dir() {
            if name == "target" || name == "out" || name.starts_with('.') {
                continue;
            }
            collect_kio_files(&path, out)?;
        } else if is_regular_kio_module(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// True iff `path` is a regular Kio module file (`.kio` extension,
/// not a package file and not a signature changelog).
pub fn is_regular_kio_module(path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    crate::file_kind::is_module_file(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snippet(module_name: &str, item_name: &str, idx: usize) -> DocCommentSnippet {
        DocCommentSnippet {
            kio_path: PathBuf::from("test.kio"),
            kio_source: format!("module {module_name}.main;\n"),
            module_name: format!("{module_name}.main"),
            item_name: item_name.to_owned(),
            snippet_index: idx,
            body: "()".to_owned(),
            fence_span: Span::new(0, 0),
            open_line_in_doc: 1,
            check_exit_code: 0,
        }
    }

    #[test]
    fn synthetic_fn_name_module_only() {
        let s = make_snippet("pkg", "", 0);
        let name = make_synthetic_fn_name(&s);
        assert_eq!(name, "kiodoc_example_mhaglghcogngbgjgo_d_i0");
    }

    #[test]
    fn synthetic_fn_name_with_item() {
        let s = make_snippet("pkg", "my_fn", 2);
        let name = make_synthetic_fn_name(&s);
        assert_eq!(name, "kiodoc_example_mhaglghcogngbgjgo_dgnhjfpgggo_i2");
    }

    #[test]
    fn synthetic_names_preserve_case_paths_and_affixes() {
        let mut names = std::collections::BTreeSet::new();
        for item in ["a.b", "a_b", "A_b", "_a", "a_", "a__", "(+)"] {
            let name = make_synthetic_fn_name(&make_snippet("pkg", item, 0));
            assert!(crate::naming::is_value_name(&name), "{name}");
            assert!(names.insert(name), "{item}");
        }
    }

    #[test]
    fn build_augmented_appends_fn() {
        let s = make_snippet("pkg", "f", 0);
        let name = "kiodoc_example_mpkg_df_i0";
        let asm = build_augmented_source(&s, name);
        assert!(asm.contains(name));
        assert!(asm.contains("fn kiodoc_example_mpkg_df_i0() -> ."));
    }

    #[test]
    fn collect_kio_files_excludes_package_files() {
        let dir = tempdir();
        write(&dir, "main.kio", "");
        write(&dir, "pkg.pkg.kio", "");
        let mut files = Vec::new();
        collect_kio_files(&dir, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("main.kio"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_no_doc_comments_empty() {
        let src = "module pkg/main;\nfn f() -> . { () }\n";
        let result = extract_doc_snippets(Path::new("t.kio"), src);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn extract_ignore_fence_skipped() {
        let src =
            "module pkg/main;\n/// ```kio {ignore}\n/// let x = 1\n/// ```\nfn f() -> . { () }\n";
        let result = extract_doc_snippets(Path::new("t.kio"), src);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn extract_rec_group_member_doc_uses_exact_member_name() {
        let src = concat!(
            "module pkg/main;\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { rec second(value) };\n",
            "  /// ```kio {@}\n",
            "  /// ()\n",
            "  /// ```\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let snippets = extract_doc_snippets(Path::new("t.kio"), src).expect("docs extract");
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].item_name, "second");
    }

    #[test]
    fn extract_named_ref_produces_error() {
        let src =
            "module pkg/main;\n/// ```kio {@main}\n/// let x = 1\n/// ```\nfn f() -> . { () }\n";
        let result = extract_doc_snippets(Path::new("t.kio"), src);
        assert!(result.is_err());
        let errs = result.unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("use `{@}`"));
    }

    #[test]
    fn forwarding_docs_keep_braced_owner_identity_and_validate_their_own_references() {
        let source = concat!(
            "module pkg/main;\n",
            "pub labels { foo: . };\n",
            "/// See [`{field}`].\n",
            "/// ```kio {@}\n",
            "/// ()\n",
            "/// ```\n",
            "pub type {field} = {foo};\n",
            "/// ```kio {@}\n",
            "/// ()\n",
            "/// ```\n",
            "pub fn field() -> . { () }\n",
        );
        let snippets = extract_doc_snippets(Path::new("t.kio"), source).expect("both docs extract");
        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].item_name, "{field}");
        assert_eq!(snippets[1].item_name, "field");
        assert_ne!(
            make_synthetic_fn_name(&snippets[0]),
            make_synthetic_fn_name(&snippets[1])
        );

        let invalid_ref = source.replace("[`{field}`]", "[`{missing}`]");
        let errors = extract_doc_snippets(Path::new("t.kio"), &invalid_ref).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("not in scope"));

        let invalid_fence = source.replacen("```kio {@}", "```kio", 1);
        let errors = extract_doc_snippets(Path::new("t.kio"), &invalid_fence).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("bare ```kio fence"));
    }

    #[test]
    fn standalone_newtype_docs_validate_refs_and_extract_exact_owner() {
        let source = concat!(
            "module pkg/main;\n",
            "/// See [`Wrapped`].\n",
            "/// ```kio {@}\n",
            "/// ()\n",
            "/// ```\n",
            "pub newtype Wrapped : . { constructor make; projector read; };\n",
        );
        let snippets = extract_doc_snippets(Path::new("t.kio"), source).expect("docs extract");
        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].item_name, "Wrapped");

        let invalid_ref = source.replace("[`Wrapped`]", "[`missing`]");
        let errors = extract_doc_snippets(Path::new("t.kio"), &invalid_ref).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("`missing` is not in scope"));

        let invalid_fence = source.replace("```kio {@}", "```kio");
        let errors = extract_doc_snippets(Path::new("t.kio"), &invalid_fence).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("bare ```kio fence"));
    }

    #[test]
    fn extract_standalone_produces_error() {
        let src = "module pkg/main;\n/// ```kio {}\n/// let x = 1\n/// ```\nfn f() -> . { () }\n";
        let result = extract_doc_snippets(Path::new("t.kio"), src);
        assert!(result.is_err());
        let errs = result.unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("{@}"));
    }

    #[test]
    fn doc_comment_refs_ignore_every_fence_language() {
        let src = concat!(
            "module pkg/main;\n",
            "/// See [`known`].\n",
            "/// ```text\n",
            "/// [`missing`] and [`@signature missing`]\n",
            "/// ```\n",
            "/// <!--markdown\n",
            "/// [`also_missing`]\n",
            "/// -->\n",
            "fn known() -> . { () }\n",
        );
        let result = extract_doc_snippets(Path::new("t.kio"), src);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn fenced_reference_definition_does_not_override_doc_comment_prose() {
        let src = concat!(
            "module pkg/main;\n",
            "/// See [`missing`].\n",
            "/// ```markdown\n",
            "/// [missing]: https://example.com\n",
            "/// ```\n",
            "fn known() -> . { () }\n",
        );
        let errors = extract_doc_snippets(Path::new("t.kio"), src).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("`missing` is not in scope"));
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!(
            "kio-doc-comment-test-{nonce}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
    }
}

//! Document model — one pass over the fence stream from
//! [`super::parse`] that builds the harness / snippet / output
//! structure described in
//! [`specs/kiodoc.md`](../../../specs/kiodoc.md).
//!
//! The model enforces every contract violation the spec lists:
//!
//! - Harness names are unique within a document.
//! - `@NAME` references must point at a harness declared earlier in
//!   source order (no forward references).
//! - Snippets declaring `{stdout}` / `{stderr}` must be followed by
//!   the corresponding output fence(s) before any other fence
//!   appears.
//! - Output fences (`stdout` / `stderr` on a non-`kio` fence) must
//!   have a preceding snippet that declared the matching attribute.
//! - Unknown attributes, multi-`@NAME`, repeated attributes, bare
//!   ` ```kio ` without `{}` — all surface here.
//!
//! Snippet validation against `kio check` lives in
//! [`super::validate`]; this module just decides *which* snippets to
//! pass it.

use crate::path_display::DisplayPath;
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::ast::KioFileKind;
use crate::span::Span;

use super::attrs::{self, AttrError};
use super::parse::Fence;

/// Document-level view of a parsed markdown file.
#[derive(Debug)]
pub struct Document {
    pub harnesses: HashMap<String, Harness>,
    pub files: BTreeMap<String, DocumentFile>,
    pub fences: Vec<DocumentFence>,
}

#[derive(Debug, Clone)]
pub struct Harness {
    pub name: String,
    pub body: String,
    pub placeholder: String,
    pub file: Option<DocumentFileHeader>,
    /// Span of the declaring fence in the markdown source.
    pub span: Span,
    /// Position in the document's fence vector — used for the
    /// forward-reference check.
    pub fence_index: usize,
    /// Whether this harness aggregates its `{@NAME}` members into one
    /// program (set by the bare `accumulate` flag on declaration).
    pub accumulate: bool,
    /// Substitution-position classification: where the harness
    /// placeholder sits inside the harness template.
    /// Computed once at declaration via brace-balance over the
    /// harness body prefix.
    pub marker_position: MarkerPosition,
}

/// Substitution-position classification for a declared placeholder
/// inside a harness template. The runner determines this by
/// brace-balance over the prefix of the harness body up to the
/// marker (see [`specs/kiodoc.md`](../../../specs/kiodoc.md)
/// § Substitution position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerPosition {
    /// Marker sits outside every `{...}` group — module scope. A
    /// substituted body contributes top-level items.
    TopLevel,
    /// Marker sits inside one or more `{...}` groups — typically a
    /// `fn` body. A substituted body contributes block contents.
    Block,
}

/// One position in the document's fence vector. Output fences
/// surface here so the pairing-rule walker can locate them by
/// index; ignored fences and non-`kio` non-output fences are
/// represented as `Ignored`.
#[derive(Debug, Clone)]
pub enum DocumentFence {
    Harness(Harness),
    File(DocumentFile),
    Snippet(Snippet),
    Output(OutputFence),
    Ignored,
}

#[derive(Debug, Clone)]
pub struct DocumentFile {
    pub header: DocumentFileHeader,
    pub body: String,
    pub span: Span,
    pub open_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentFileHeader {
    pub path: String,
    pub kind: KioFileKind,
    pub package_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Snippet {
    pub harness_ref: Option<String>,
    pub ignored: bool,
    pub check_exit_code: i32,
    /// Snippet declares `{stdout}`; `kio doc check` enforces the
    /// paired `{stdout}` output fence structurally.
    pub expects_stdout: bool,
    pub expects_stderr: bool,
    pub run_exit_code: Option<i32>,
    pub placeholders: Vec<SnippetPlaceholder>,
    pub body: String,
    pub span: Span,
    /// 1-based line of the opening marker in the markdown source —
    /// used for diagnostics referring back to the originating
    /// snippet.
    pub open_line: usize,
    /// Byte offset of the snippet body's first line in the markdown
    /// source. Lets the validator translate a snippet-relative line
    /// number back to a markdown-source line.
    pub body_offset: u32,
    /// Resolved output fences paired with this snippet — set during
    /// [`Document::build`]'s pairing-rule walk.
    pub stdout_fence: Option<usize>,
    pub stderr_fence: Option<usize>,
    /// What file kind the snippet body parses as. Set by the
    /// `variant=KIND` key-value attribute on the fence (or
    /// [`KioFileKind::Module`] when absent — the regular Kio
    /// module-body parser path).
    pub variant: KioFileKind,
}

#[derive(Debug, Clone)]
pub struct SnippetPlaceholder {
    /// Exact text shown in the rendered snippet.
    pub from: String,
    /// Replacement text compiled by the validator.
    pub to: String,
}

#[derive(Debug, Clone)]
pub struct OutputFence {
    pub kind: OutputKind,
    pub body: String,
    pub span: Span,
    pub open_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Stdout,
    Stderr,
}

/// One Kiodoc-level error, with its location in the markdown source.
#[derive(Debug, Clone)]
pub struct DocError {
    pub span: Span,
    pub message: String,
}

impl DocError {
    /// Print the diagnostic in the standard `<path>:<line>:<col>:
    /// <message>` shape used elsewhere in the implementation.
    pub fn eprint(&self, path: &Path, source: &str) {
        eprint!("{}", self.render(path, source));
    }

    /// As [`Self::eprint`], but returns the rendered diagnostic (with a
    /// trailing newline) so the multi-package fan-out can buffer it.
    pub fn render(&self, path: &Path, source: &str) -> String {
        let (line, col) = line_col(source, self.span.start);
        format!(
            "{}:{}:{}: {}\n",
            DisplayPath(&path),
            line,
            col,
            self.message
        )
    }
}

impl Document {
    /// Walk `fences` in source order, building the document model.
    /// Returns the first contract violation encountered, or the
    /// fully-built `Document` on success.
    pub fn build(_path: &Path, _source: &str, fences: Vec<Fence>) -> Result<Self, DocError> {
        let mut doc = Document {
            harnesses: HashMap::new(),
            files: BTreeMap::new(),
            fences: Vec::with_capacity(fences.len()),
        };
        let mut file_paths: HashMap<String, Span> = HashMap::new();

        // Non-`kio` fences that are *not* output fences are ignored;
        // `kio-repl` is opaque prose.
        for fence in fences {
            let entry = classify_fence(
                fence,
                &mut doc.harnesses,
                &mut doc.files,
                &mut file_paths,
                doc.fences.len(),
            )?;
            doc.fences.push(entry);
        }

        // Pairing-rule walk: for each snippet declaring stdout/stderr,
        // find the matching output fence(s) before any other fence
        // appears. Also flag orphans.
        doc.resolve_pairings()?;
        Ok(doc)
    }

    /// Every snippet in source order. Includes ignored snippets so
    /// callers can decide how to handle them; check the `ignored`
    /// flag before validating.
    pub fn snippets(&self) -> impl Iterator<Item = &Snippet> {
        self.fences.iter().filter_map(|f| match f {
            DocumentFence::Snippet(s) => Some(s),
            _ => None,
        })
    }

    /// Resolve `{stdout}` / `{stderr}` pairings and detect orphans.
    fn resolve_pairings(&mut self) -> Result<(), DocError> {
        let n = self.fences.len();
        let mut pairings: Vec<(usize, OutputKind, usize)> = Vec::new(); // (snippet_idx, kind, output_idx)
        let mut output_paired = vec![false; n];

        for i in 0..n {
            let DocumentFence::Snippet(s) = &self.fences[i] else {
                continue;
            };
            if s.ignored {
                continue;
            }
            let needs_stdout = s.expects_stdout;
            let needs_stderr = s.expects_stderr;
            if !needs_stdout && !needs_stderr {
                continue;
            }
            let mut got_stdout = false;
            let mut got_stderr = false;
            // Scan forward fence-by-fence until both pairings are
            // found or another non-output fence appears.
            let mut j = i + 1;
            while j < n {
                match &self.fences[j] {
                    DocumentFence::Output(o) => match o.kind {
                        OutputKind::Stdout => {
                            if !needs_stdout || got_stdout {
                                return Err(DocError {
                                    span: o.span,
                                    message: "orphan {stdout} output fence: \
                                             not paired with a preceding snippet \
                                             declaring {stdout}"
                                        .to_owned(),
                                });
                            }
                            pairings.push((i, OutputKind::Stdout, j));
                            output_paired[j] = true;
                            got_stdout = true;
                        }
                        OutputKind::Stderr => {
                            if !needs_stderr || got_stderr {
                                return Err(DocError {
                                    span: o.span,
                                    message: "orphan {stderr} output fence: \
                                             not paired with a preceding snippet \
                                             declaring {stderr}"
                                        .to_owned(),
                                });
                            }
                            pairings.push((i, OutputKind::Stderr, j));
                            output_paired[j] = true;
                            got_stderr = true;
                        }
                    },
                    DocumentFence::Snippet(_)
                    | DocumentFence::Harness(_)
                    | DocumentFence::File(_) => {
                        // Another non-output fence appeared before
                        // pairings completed.
                        break;
                    }
                    DocumentFence::Ignored => {
                        // `kio {ignore}` and other-language non-output
                        // fences are still "another fence" for the
                        // pairing rule — they cannot intervene
                        // between a snippet and its output.
                        break;
                    }
                }
                j += 1;
            }
            if needs_stdout && !got_stdout {
                return Err(DocError {
                    span: snippet_span(&self.fences[i]),
                    message: "snippet declares {stdout} but no matching output \
                         fence follows before another fence appears"
                        .to_owned(),
                });
            }
            if needs_stderr && !got_stderr {
                return Err(DocError {
                    span: snippet_span(&self.fences[i]),
                    message: "snippet declares {stderr} but no matching output \
                         fence follows before another fence appears"
                        .to_owned(),
                });
            }
        }

        // Any output fence that didn't get paired in the walk above
        // is an orphan. (The walk catches orphans that follow a
        // snippet without the matching expect; this pass catches
        // ones with no preceding snippet at all.)
        for (i, fence) in self.fences.iter().enumerate() {
            if let DocumentFence::Output(o) = fence
                && !output_paired[i]
            {
                let kind = match o.kind {
                    OutputKind::Stdout => "{stdout}",
                    OutputKind::Stderr => "{stderr}",
                };
                return Err(DocError {
                    span: o.span,
                    message: format!(
                        "orphan {kind} output fence: no preceding snippet \
                         declares {kind}"
                    ),
                });
            }
        }

        // Patch the snippets with their resolved pairings.
        for (snippet_idx, kind, output_idx) in pairings {
            if let DocumentFence::Snippet(s) = &mut self.fences[snippet_idx] {
                match kind {
                    OutputKind::Stdout => s.stdout_fence = Some(output_idx),
                    OutputKind::Stderr => s.stderr_fence = Some(output_idx),
                }
            }
        }
        Ok(())
    }
}

/// Source span of the snippet at `fence` (caller has verified the
/// variant is `Snippet`).
fn snippet_span(fence: &DocumentFence) -> Span {
    match fence {
        DocumentFence::Snippet(s) => s.span,
        _ => unreachable!("snippet_span called on non-snippet fence"),
    }
}

/// Decide how to interpret a fence. Returns `Ignored` for fences
/// that contribute nothing to validation (non-`kio` fences without
/// `stdout`/`stderr`, `kio-repl` opaque prose, fences whose info
/// string the contract doesn't recognize).
fn classify_fence(
    fence: Fence,
    harnesses: &mut HashMap<String, Harness>,
    files: &mut BTreeMap<String, DocumentFile>,
    file_paths: &mut HashMap<String, Span>,
    fence_index: usize,
) -> Result<DocumentFence, DocError> {
    // `kio-repl` is opaque prose.
    if fence.lang == "kio-repl" {
        return Ok(DocumentFence::Ignored);
    }

    let is_kio = fence.lang == "kio";

    let attrs_owned;
    let parsed = if let Some(text) = &fence.attrs_text {
        match attrs::parse(text) {
            Ok(mut a) => {
                a.span = fence.span;
                attrs_owned = a;
                Some(&attrs_owned)
            }
            Err(err) => {
                return Err(attr_error_to_doc(&fence, err));
            }
        }
    } else if is_kio {
        // Bare ` ```kio ` is a runner error per the contract.
        return Err(DocError {
            span: fence.span,
            message: "bare ```kio fence: write `kio {}` for standalone, \
                      `kio {@harness}` for harness-wrapped, or `kio {ignore}` \
                      to skip"
                .to_owned(),
        });
    } else {
        None
    };

    if !is_kio {
        // Non-`kio` fences: only stdout/stderr output fences are
        // recognized; anything else is opaque prose.
        if let Some(p) = parsed {
            let mut output_kind: Option<OutputKind> = None;
            for bare in &p.bare {
                let kind = match bare.name.as_str() {
                    "stdout" => OutputKind::Stdout,
                    "stderr" => OutputKind::Stderr,
                    _ => {
                        return Err(DocError {
                            span: fence.span,
                            message: format!(
                                "unknown attribute `{}` on non-`kio` fence \
                                 (only `stdout`/`stderr` are recognized as \
                                 output-fence markers)",
                                bare.name
                            ),
                        });
                    }
                };
                if output_kind.is_some() {
                    return Err(DocError {
                        span: fence.span,
                        message: "output fence may carry at most one of \
                             `stdout` / `stderr`"
                            .to_owned(),
                    });
                }
                output_kind = Some(kind);
            }
            if !p.kvs.is_empty() {
                return Err(DocError {
                    span: fence.span,
                    message: format!(
                        "key-value attribute `{}` is not valid on a non-`kio` \
                         fence",
                        p.kvs[0].key
                    ),
                });
            }
            if p.harness_ref.is_some() {
                return Err(DocError {
                    span: fence.span,
                    message: "`@NAME` is only valid on a `kio` fence".to_owned(),
                });
            }
            if let Some(kind) = output_kind {
                return Ok(DocumentFence::Output(OutputFence {
                    kind,
                    body: fence.body,
                    span: fence.span,
                    open_line: fence.open_line,
                }));
            }
        }
        return Ok(DocumentFence::Ignored);
    }

    // `kio` fence — attrs are present per the bare-fence guard above.
    let p = parsed.expect("kio fence has attrs by the bare-fence guard");

    // The bare `@` names the snippet's surrounding module as its
    // harness. A `.md` file has no surrounding module, so the form is
    // meaningless here — see `specs/kiodoc.md` § Doc-comment input
    // surface, which admits it only inside a `///` doc-comment.
    if p.self_ref {
        return Err(DocError {
            span: fence.span,
            message: "`{@}` wraps a snippet in its surrounding module, which a \
                      `.md` file does not have — reference a declared harness \
                      with `{@NAME}`, or write `{}` for a standalone snippet"
                .to_owned(),
        });
    }

    let file_flag = p.bare.iter().any(|bare| bare.name == "file");

    // Harness declaration? Must carry `harness=NAME`.
    let mut harness_decl: Option<String> = None;
    let mut harness_placeholder_raw: Option<String> = None;
    for kv in &p.kvs {
        match kv.key.as_str() {
            "harness" => {
                if harness_decl.is_some() {
                    return Err(DocError {
                        span: fence.span,
                        message: "duplicate `harness=` attribute".to_owned(),
                    });
                }
                harness_decl = Some(kv.value.clone());
            }
            "placeholder" => harness_placeholder_raw = Some(kv.value.clone()),
            _ => {}
        }
    }
    if let Some(name) = harness_decl {
        // Harness declaration: admissible attributes are
        // `harness=NAME`, `placeholder=JSON_STRING`, and optionally
        // the bare `accumulate` / `file` flags. No other bare flags,
        // no other kv attributes, no `@NAME`.
        let allowed_kvs = p
            .kvs
            .iter()
            .all(|kv| kv.key == "harness" || kv.key == "placeholder");
        let mut accumulate_flag = false;
        for bare in &p.bare {
            if bare.name == "accumulate" {
                accumulate_flag = true;
            } else if bare.name == "file" {
                // handled below after the placeholder checks
            } else {
                return Err(DocError {
                    span: fence.span,
                    message: format!(
                        "harness-declaration fence must carry only `harness=NAME` \
                         plus `placeholder=JSON_STRING` \
                         (and optionally `accumulate` / `file`) — got bare attribute `{}`",
                        bare.name
                    ),
                });
            }
        }
        if !allowed_kvs || p.harness_ref.is_some() {
            return Err(DocError {
                span: fence.span,
                message: "harness-declaration fence must carry only `harness=NAME` \
                     plus `placeholder=JSON_STRING` (and optionally `accumulate` / `file`) — \
                     no other attributes are permitted"
                    .to_owned(),
            });
        }
        if file_flag && accumulate_flag {
            return Err(DocError {
                span: fence.span,
                message: "file-backed harnesses cannot be accumulating harnesses".to_owned(),
            });
        }
        let placeholder_raw = harness_placeholder_raw.ok_or_else(|| DocError {
            span: fence.span,
            message: format!(
                "harness `{name}` must declare `placeholder=JSON_STRING` naming \
                 the substitution marker"
            ),
        })?;
        if !is_attr_name(&name) {
            return Err(DocError {
                span: fence.span,
                message: format!(
                    "`harness=NAME` requires an unquoted attribute name, got `{name}`"
                ),
            });
        }
        let placeholder = parse_harness_placeholder(&placeholder_raw, fence.span, &name)?;
        let marker_count = fence.body.matches(&placeholder).count();
        if marker_count != 1 {
            return Err(DocError {
                span: fence.span,
                message: format!(
                    "harness `{name}` body must contain its placeholder `{placeholder}` \
                     exactly once; found {marker_count}"
                ),
            });
        }
        if harnesses.contains_key(&name) {
            return Err(DocError {
                span: fence.span,
                message: format!("harness `{name}` is declared more than once"),
            });
        }
        let file = if file_flag {
            let header = infer_document_file_header(&fence.body, fence.span)?;
            reserve_file_path(file_paths, &header.path, fence.span)?;
            Some(header)
        } else {
            None
        };
        let marker_position = classify_marker_position(&fence.body, &placeholder);
        let h = Harness {
            name: name.clone(),
            body: fence.body,
            placeholder,
            file,
            span: fence.span,
            fence_index,
            accumulate: accumulate_flag,
            marker_position,
        };
        harnesses.insert(name, h.clone());
        return Ok(DocumentFence::Harness(h));
    }

    if file_flag {
        if p.harness_ref.is_some() || !p.kvs.is_empty() || !p.bare.iter().all(|b| b.name == "file")
        {
            return Err(DocError {
                span: fence.span,
                message: "`{file}` fences must carry only the bare `file` attribute".to_owned(),
            });
        }
        let header = infer_document_file_header(&fence.body, fence.span)?;
        reserve_file_path(file_paths, &header.path, fence.span)?;
        let file = DocumentFile {
            header: header.clone(),
            body: fence.body,
            span: fence.span,
            open_line: fence.open_line,
        };
        files.insert(header.path.clone(), file.clone());
        return Ok(DocumentFence::File(file));
    }

    // Snippet. Walk the bare/kv lists, accept only the recognized
    // vocabulary.
    let mut ignore_flag = false;
    let mut expects_stdout = false;
    let mut expects_stderr = false;
    let mut check_exit_code: i32 = 0;
    let mut run_exit_code: Option<i32> = None;
    let mut variant = KioFileKind::Module;
    let mut snippet_placeholder_raw: Option<String> = None;

    for bare in &p.bare {
        match bare.name.as_str() {
            "ignore" => ignore_flag = true,
            "stdout" => expects_stdout = true,
            "stderr" => expects_stderr = true,
            other => {
                return Err(DocError {
                    span: fence.span,
                    message: format!("unknown bare attribute `{other}` on a `kio` fence"),
                });
            }
        }
    }
    for kv in &p.kvs {
        match kv.key.as_str() {
            "check_exit_code" => match kv.value.parse::<i32>() {
                Ok(n) => check_exit_code = n,
                Err(_) => {
                    return Err(DocError {
                        span: fence.span,
                        message: format!(
                            "`check_exit_code=N` requires an integer value, got `{}`",
                            kv.value
                        ),
                    });
                }
            },
            "run_exit_code" => match kv.value.parse::<i32>() {
                Ok(n) => run_exit_code = Some(n),
                Err(_) => {
                    return Err(DocError {
                        span: fence.span,
                        message: format!(
                            "`run_exit_code=N` requires an integer value, got `{}`",
                            kv.value
                        ),
                    });
                }
            },
            "variant" => {
                variant =
                    KioFileKind::from_variant_name(kv.value.as_str()).ok_or_else(|| DocError {
                        span: fence.span,
                        message: format!(
                            "`variant=KIND` requires one of \
                             `module`/`package`/`signature`/`dependency`/`lock`, got `{}`",
                            kv.value
                        ),
                    })?;
            }
            "placeholder" => snippet_placeholder_raw = Some(kv.value.clone()),
            other => {
                return Err(DocError {
                    span: fence.span,
                    message: format!("unknown key-value attribute `{other}` on a `kio` fence"),
                });
            }
        }
    }

    if ignore_flag {
        // `ignore` skips validation. It may carry `variant=KIND`
        // so a skipped package-file fragment can still declare what
        // kind of Kio file it illustrates, but no validation/run
        // behavior can compose with a skipped snippet.
        let only_allowed_kvs = p.kvs.iter().all(|kv| kv.key == "variant");
        if !p.bare.iter().all(|b| b.name == "ignore")
            || !only_allowed_kvs
            || p.harness_ref.is_some()
        {
            return Err(DocError {
                span: fence.span,
                message: "`{ignore}` may combine only with `variant=KIND`".to_owned(),
            });
        }
    }

    let placeholders = match snippet_placeholder_raw {
        Some(raw) => parse_snippet_placeholders(&raw, &fence.body, fence.span)?,
        None => Vec::new(),
    };

    if let Some(name) = &p.harness_ref
        && !harnesses.contains_key(name)
    {
        return Err(DocError {
            span: fence.span,
            message: format!(
                "snippet references harness `{name}`, but no harness with \
                 that name has been declared earlier in the document"
            ),
        });
    }

    // Variant-snippet constraints. Non-module variants are always
    // standalone — they describe a package's surface, not a module
    // body, so they can't be wrapped in a harness.
    if variant != KioFileKind::Module && p.harness_ref.is_some() {
        return Err(DocError {
            span: fence.span,
            message: "`variant=` is exclusive with `@NAME` — non-module variant \
                      snippets are always standalone, not harness members"
                .to_owned(),
        });
    }

    // Accumulating-harness member constraints. Members can't carry
    // per-snippet exit-code / run-trigger attributes — the aggregate
    // is one program with one run, so per-member assertions have no
    // aggregate meaning.
    if let Some(name) = &p.harness_ref
        && let Some(h) = harnesses.get(name)
        && h.accumulate
    {
        if check_exit_code != 0 {
            return Err(DocError {
                span: fence.span,
                message: format!(
                    "`check_exit_code=N` is not allowed on a member of accumulating \
                     harness `{name}` — the aggregate validates as one program"
                ),
            });
        }
        if expects_stdout || expects_stderr || run_exit_code.is_some() {
            return Err(DocError {
                span: fence.span,
                message: format!(
                    "run-trigger attributes (`stdout`/`stderr`/`run_exit_code=N`) \
                     are not allowed on a member of accumulating harness `{name}` — \
                     the aggregate has one execution, not per-member runs"
                ),
            });
        }
    }

    Ok(DocumentFence::Snippet(Snippet {
        harness_ref: p.harness_ref.clone(),
        ignored: ignore_flag,
        check_exit_code,
        expects_stdout,
        expects_stderr,
        run_exit_code,
        placeholders,
        body: fence.body,
        span: fence.span,
        open_line: fence.open_line,
        body_offset: fence.body_offset,
        stdout_fence: None,
        stderr_fence: None,
        variant,
    }))
}

fn parse_harness_placeholder(raw: &str, span: Span, name: &str) -> Result<String, DocError> {
    let placeholder: String = serde_json::from_str(raw).map_err(|e| DocError {
        span,
        message: format!("harness `{name}` placeholder must be a JSON string literal: {e}"),
    })?;
    if placeholder.is_empty() {
        return Err(DocError {
            span,
            message: format!("harness `{name}` placeholder must not be empty"),
        });
    }
    Ok(placeholder)
}

fn is_attr_name(value: &str) -> bool {
    crate::naming::is_value_name(value)
}

fn reserve_file_path(
    paths: &mut HashMap<String, Span>,
    path: &str,
    span: Span,
) -> Result<(), DocError> {
    if paths.insert(path.to_owned(), span).is_some() {
        return Err(DocError {
            span,
            message: format!("document-scoped Kio file `{path}` is declared more than once"),
        });
    }
    Ok(())
}

fn infer_document_file_header(body: &str, span: Span) -> Result<DocumentFileHeader, DocError> {
    let header_src = first_header_clause(body).ok_or_else(|| DocError {
        span,
        message: "`{file}` fence body must start with a Kio file header".to_owned(),
    })?;
    let tokens = crate::pass::lexer::lex(header_src).map_err(|e| DocError {
        span,
        message: format!("could not parse `{{file}}` fence header: {e:?}"),
    })?;
    if tokens.is_empty() {
        return Err(DocError {
            span,
            message: "`{file}` fence body must start with a Kio file header".to_owned(),
        });
    }

    use crate::pass::lexer::TokenKind;

    let kind = match tokens.first().map(|t| &t.kind) {
        Some(TokenKind::Ident(first)) => {
            KioFileKind::from_variant_name(first).ok_or_else(|| DocError {
                span,
                message: "`{file}` fence body must start with `module`, `package`, \
                          `signature`, `dependency`, or `lock`"
                    .to_owned(),
            })?
        }
        _ => {
            return Err(DocError {
                span,
                message: "`{file}` fence body must start with `module`, `package`, \
                          `signature`, `dependency`, or `lock`"
                    .to_owned(),
            });
        }
    };

    let segments = if kind.accepts_module_path() {
        parse_header_path_segments(&tokens[1..], span)?
    } else {
        parse_non_module_header_stem(&tokens[1..], span)?
    };
    if segments.is_empty() {
        return Err(DocError {
            span,
            message: "`{file}` fence header must name a module path".to_owned(),
        });
    }
    if !kind.accepts_module_path() && segments.len() != 1 {
        return Err(DocError {
            span,
            message: "non-module file headers must name a root-level stem, \
                      not a slash-qualified module path"
                .to_owned(),
        });
    }

    let package_name = kind.package_name(&segments).map(str::to_owned);
    let path = kind.inferred_path(&segments).ok_or_else(|| DocError {
        span,
        message: "`{file}` fence header must name a module path".to_owned(),
    })?;

    Ok(DocumentFileHeader {
        path,
        kind,
        package_name,
    })
}

fn first_header_clause(body: &str) -> Option<&str> {
    let start = first_meaningful_byte(body)?;
    let rest = body.get(start..)?;
    let end = rest.find(';')?;
    Some(&rest[..=end])
}

fn first_meaningful_byte(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        match bytes[pos] {
            b' ' | b'\t' | b'\r' | b'\n' => pos += 1,
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                let is_doc = bytes.get(pos + 2) == Some(&b'/');
                let after_marker = pos + if is_doc { 3 } else { 2 };
                if bytes
                    .get(after_marker)
                    .is_some_and(|b| !matches!(*b, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    return Some(pos);
                }
                pos = after_marker;
                while bytes.get(pos).is_some_and(|b| *b != b'\n') {
                    pos += 1;
                }
            }
            _ => return Some(pos),
        }
    }
    None
}

fn parse_header_path_segments(
    tokens: &[crate::pass::lexer::Token],
    span: Span,
) -> Result<Vec<String>, DocError> {
    use crate::pass::lexer::TokenKind;

    let mut idx = 0usize;
    let mut segments = Vec::new();
    loop {
        let Some(tok) = tokens.get(idx) else {
            return Err(DocError {
                span,
                message: "`{file}` fence header path is incomplete".to_owned(),
            });
        };
        match &tok.kind {
            TokenKind::Ident(segment) => segments.push(segment.clone()),
            _ => {
                return Err(DocError {
                    span,
                    message: "`{file}` fence header path must contain module-name segments"
                        .to_owned(),
                });
            }
        }
        idx += 1;

        match tokens.get(idx).map(|t| &t.kind) {
            Some(TokenKind::Semicolon) => {
                idx += 1;
                break;
            }
            Some(TokenKind::SymbolRun(sym)) if sym == "/" => {
                idx += 1;
            }
            Some(_) => {
                return Err(DocError {
                    span,
                    message: "`{file}` fence header path must use `/` between segments and \
                              end with `;`"
                        .to_owned(),
                });
            }
            None => {
                return Err(DocError {
                    span,
                    message: "`{file}` fence header path must end with `;`".to_owned(),
                });
            }
        }
    }

    if idx != tokens.len() {
        return Err(DocError {
            span,
            message: "`{file}` fence header must contain only one file-header clause".to_owned(),
        });
    }
    Ok(segments)
}

fn parse_non_module_header_stem(
    tokens: &[crate::pass::lexer::Token],
    span: Span,
) -> Result<Vec<String>, DocError> {
    use crate::pass::lexer::TokenKind;

    let Some(first) = tokens.first() else {
        return Err(DocError {
            span,
            message: "`{file}` fence header path is incomplete".to_owned(),
        });
    };
    let TokenKind::Ident(stem) = &first.kind else {
        return Err(DocError {
            span,
            message: "`{file}` fence header path must start with a root-level stem".to_owned(),
        });
    };
    if matches!(
        tokens.get(1).map(|t| &t.kind),
        Some(TokenKind::SymbolRun(sym)) if sym == "/"
    ) {
        return Err(DocError {
            span,
            message: "non-module file headers must name a root-level stem, \
                      not a slash-qualified module path"
                .to_owned(),
        });
    }
    Ok(vec![stem.clone()])
}

fn parse_snippet_placeholders(
    raw: &str,
    body: &str,
    span: Span,
) -> Result<Vec<SnippetPlaceholder>, DocError> {
    let parsed: JsonValue = serde_json::from_str(raw).map_err(|e| DocError {
        span,
        message: format!("snippet placeholder must be a JSON object of strings: {e}"),
    })?;
    let JsonValue::Object(map) = parsed else {
        return Err(DocError {
            span,
            message:
                "snippet placeholder must be a JSON object mapping visible text to validation text"
                    .to_owned(),
        });
    };
    if map.is_empty() {
        return Err(DocError {
            span,
            message: "snippet placeholder object must contain at least one mapping".to_owned(),
        });
    }
    let mut out = Vec::new();
    for (from, value) in map {
        if from.is_empty() {
            return Err(DocError {
                span,
                message: "snippet placeholder keys must not be empty".to_owned(),
            });
        }
        let JsonValue::String(to) = value else {
            return Err(DocError {
                span,
                message: format!(
                    "snippet placeholder replacement for `{from}` must be a JSON string"
                ),
            });
        };
        if !body.contains(&from) {
            return Err(DocError {
                span,
                message: format!(
                    "snippet placeholder `{from}` did not match any text in the snippet body"
                ),
            });
        }
        out.push(SnippetPlaceholder { from, to });
    }
    out.sort_by(|a, b| {
        b.from
            .len()
            .cmp(&a.from.len())
            .then_with(|| a.from.cmp(&b.from))
    });
    Ok(out)
}

fn attr_error_to_doc(fence: &Fence, err: AttrError) -> DocError {
    DocError {
        span: fence.span,
        message: format!("invalid fence attribute list: {}", err.message),
    }
}

/// Map a byte offset inside `source` to a 1-based (line, column).
/// Column is in UTF-8 scalar values, not bytes.
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let upto = (offset as usize).min(source.len());
    let prefix = &source[..upto];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = source[line_start..upto].chars().count() + 1;
    (line, col)
}

/// Classify the substitution position of `placeholder` in a
/// harness body by brace-balance over the prefix up to the marker.
///
/// The scanner respects Kio's `//` line comments and string-literal
/// `"…"` quotes so a `{` inside a string or comment doesn't count.
/// Block-comment `/* … */` is also skipped for the same reason — the
/// language supports it (per `specs/grammar.md`), and a `{` inside
/// one shouldn't influence classification.
///
/// When the marker isn't present in the body, the caller has already
/// rejected the declaration; the function still returns a sensible
/// default ([`MarkerPosition::TopLevel`]) to keep the type total.
pub fn classify_marker_position(body: &str, placeholder: &str) -> MarkerPosition {
    let marker_idx = match body.find(placeholder) {
        Some(i) => i,
        None => return MarkerPosition::TopLevel,
    };
    let prefix = &body[..marker_idx];
    let bytes = prefix.as_bytes();
    let mut depth: i64 = 0;
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        // Line comment: `//…\n` — skip to end of line.
        if b == b'/' && idx + 1 < bytes.len() && bytes[idx + 1] == b'/' {
            idx += 2;
            while idx < bytes.len() && bytes[idx] != b'\n' {
                idx += 1;
            }
            continue;
        }
        // Block comment: `/*…*/` — skip until the closing pair.
        if b == b'/' && idx + 1 < bytes.len() && bytes[idx + 1] == b'*' {
            idx += 2;
            while idx + 1 < bytes.len() {
                if bytes[idx] == b'*' && bytes[idx + 1] == b'/' {
                    idx += 2;
                    break;
                }
                idx += 1;
            }
            continue;
        }
        // String literal: `"…"` — skip until the closing quote,
        // honoring `\"` escapes inside.
        if b == b'"' {
            idx += 1;
            while idx < bytes.len() {
                if bytes[idx] == b'\\' && idx + 1 < bytes.len() {
                    idx += 2;
                    continue;
                }
                if bytes[idx] == b'"' {
                    idx += 1;
                    break;
                }
                idx += 1;
            }
            continue;
        }
        // Brace tracking.
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
        }
        idx += 1;
    }
    if depth > 0 {
        MarkerPosition::Block
    } else {
        MarkerPosition::TopLevel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiodoc::parse;
    use std::path::PathBuf;

    fn build_from(src: &str) -> Result<Document, DocError> {
        let p = PathBuf::from("x.md");
        Document::build(&p, src, parse::scan(src))
    }

    #[test]
    fn empty_doc_builds() {
        let d = build_from("just prose\n").unwrap();
        assert!(d.harnesses.is_empty());
        assert!(d.snippets().next().is_none());
    }

    #[test]
    fn harness_and_snippet_pair() {
        let src = "```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\nm\n__INSERT_CODE_HERE__\n```\n\n```kio {@main}\nlet x = 1\n```\n";
        let d = build_from(src).unwrap();
        assert!(d.harnesses.contains_key("main"));
        assert_eq!(
            d.harnesses.get("main").unwrap().placeholder,
            "__INSERT_CODE_HERE__"
        );
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert_eq!(snip[0].harness_ref.as_deref(), Some("main"));
    }

    #[test]
    fn forward_reference_to_harness_errors() {
        let src = "```kio {@main}\nlet x = 1\n```\n\n```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\nm\n__INSERT_CODE_HERE__\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("references harness"));
    }

    #[test]
    fn duplicate_harness_errors() {
        let src = "```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\nm\n__INSERT_CODE_HERE__\n```\n\n```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\nn\n__INSERT_CODE_HERE__\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("declared more than once"));
    }

    #[test]
    fn harness_without_marker_errors() {
        let src = "```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\njust a comment, no marker\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("__INSERT_CODE_HERE__"));
    }

    #[test]
    fn harness_without_placeholder_attr_errors() {
        let src = "```kio {harness=main}\n__INSERT_CODE_HERE__\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("placeholder=JSON_STRING"));
    }

    #[test]
    fn harness_name_must_be_unquoted_name() {
        let src = "```kio {harness=\"main\" placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("harness=NAME"));
        assert!(err.message.contains("unquoted"));
    }

    #[test]
    fn custom_harness_placeholder_records_position() {
        let src =
            "```kio {harness=main placeholder=\"__SNIPPET__\"}\nmodule x/main;\n__SNIPPET__\n```\n";
        let d = build_from(src).unwrap();
        let h = d.harnesses.get("main").unwrap();
        assert_eq!(h.placeholder, "__SNIPPET__");
        assert_eq!(h.marker_position, MarkerPosition::TopLevel);
    }

    #[test]
    fn standalone_snippet_recognized() {
        let src = "```kio {}\nbody\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert!(snip[0].harness_ref.is_none());
        assert!(!snip[0].ignored);
    }

    #[test]
    fn ignore_snippet_recognized() {
        let src = "```kio {ignore}\nbody\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert!(snip[0].ignored);
    }

    #[test]
    fn unknown_bare_attr_on_kio_fence_errors() {
        let src = "```kio {weird}\nbody\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("unknown bare attribute"));
    }

    #[test]
    fn check_exit_code_parsed() {
        let src = "```kio {harness=main placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n\n```kio {@main check_exit_code=14}\nx\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert_eq!(snip[0].check_exit_code, 14);
    }

    #[test]
    fn snippet_placeholders_parse_multiple_mappings() {
        let src = "```kio {placeholder={\"...\":\"dummy_code()\",\"???\":\"fallback()\"}}\nfn f() -> String { ... }\nfn g() -> String { ??? }\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert_eq!(snip[0].placeholders.len(), 2);
        assert!(snip[0].placeholders.iter().any(|p| p.from == "..."));
        assert!(snip[0].placeholders.iter().any(|p| p.from == "???"));
    }

    #[test]
    fn snippet_placeholder_must_match_body() {
        let src = "```kio {placeholder={\"...\":\"dummy_code()\"}}\nfn f() -> . { () }\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("did not match"));
    }

    #[test]
    fn ignore_may_combine_with_variant() {
        let src = "```kio {variant=package ignore}\npackage pkg;\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert_eq!(snip.len(), 1);
        assert!(snip[0].ignored);
        assert_eq!(snip[0].variant, KioFileKind::Package);
    }

    #[test]
    fn ignore_may_not_combine_with_placeholder() {
        let src = "```kio {ignore placeholder={\"...\":\"dummy_code()\"}}\n...\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("may combine only with `variant=KIND`"));
    }

    #[test]
    fn file_fence_infers_path_from_header() {
        let src = "```kio {file}\nmodule core;\nfn id[A](x: A) -> A { x }\n```\n";
        let d = build_from(src).unwrap();
        let file = d.files.get("core.kio").unwrap();
        assert_eq!(file.header.kind, KioFileKind::Module);
        assert_eq!(file.header.package_name, None);
        assert!(d.snippets().next().is_none());
    }

    #[test]
    fn file_fence_infers_paths_from_non_module_headers() {
        for (header, path, kind) in [
            ("signature pkg v(1);", "pkg.sig.kio", KioFileKind::Signature),
            ("dependency dep;", "dep.dep.kio", KioFileKind::Dependency),
            ("lock dep;", "dep.lock.kio", KioFileKind::Lock),
        ] {
            let src = format!("```kio {{file}}\n{header}\n```\n");
            let d = build_from(&src).unwrap();
            let file = d.files.get(path).unwrap();
            assert_eq!(file.header.kind, kind);
        }
    }

    #[test]
    fn file_fence_infers_path_after_leading_comments() {
        let src =
            "```kio {file}\n// ; not a header\n/// also ; not a header\nsignature pkg v(1);\n```\n";
        let d = build_from(src).unwrap();
        let file = d.files.get("pkg.sig.kio").unwrap();
        assert_eq!(file.header.kind, KioFileKind::Signature);
    }

    #[test]
    fn file_fence_infers_non_module_path_without_parsing_header_tail() {
        let src = "```kio {file}\nsignature pkg not_the_real_sig_header;\n```\n";
        let d = build_from(src).unwrap();
        let file = d.files.get("pkg.sig.kio").unwrap();
        assert_eq!(file.header.kind, KioFileKind::Signature);
    }

    #[test]
    fn file_backed_harness_infers_insertion_path() {
        let src = "```kio {harness=m file placeholder=\"__SNIPPET__\"}\nmodule pkg/main;\n__SNIPPET__\n```\n\n```kio {@m}\nfn f() -> . { () }\n```\n";
        let d = build_from(src).unwrap();
        let h = d.harnesses.get("m").unwrap();
        assert_eq!(h.file.as_ref().unwrap().path, "pkg/main.kio");
        assert_eq!(h.file.as_ref().unwrap().kind, KioFileKind::Module);
        assert_eq!(d.snippets().count(), 1);
    }

    #[test]
    fn snippet_with_stdout_paired_with_text_fence() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n\n```kio {@m stdout}\nprint(1)\n```\n\n```text {stdout}\n1\n```\n";
        let d = build_from(src).unwrap();
        let snip: Vec<_> = d.snippets().collect();
        assert!(snip[0].stdout_fence.is_some());
    }

    #[test]
    fn snippet_with_stdout_no_output_fence_errors() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n\n```kio {@m stdout}\nprint(1)\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("no matching output fence"));
    }

    #[test]
    fn orphan_stdout_output_errors() {
        let src = "```text {stdout}\n1\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("orphan"));
    }

    #[test]
    fn intervening_fence_between_snippet_and_output_errors() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n\n```kio {@m stdout}\np\n```\n\n```kio {ignore}\nother\n```\n\n```text {stdout}\n1\n```\n";
        let err = build_from(src).unwrap_err();
        // The walker stops at the intervening `kio {ignore}` fence;
        // the snippet's stdout pairing then fails. The output fence
        // also surfaces as an orphan; either message is the
        // contract.
        assert!(err.message.contains("no matching output fence") || err.message.contains("orphan"));
    }

    #[test]
    fn bare_kio_fence_without_attrs_errors() {
        let src = "```kio\nlet x = 1\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("bare ```kio"));
    }

    // ---- Position auto-detection ----------------------------------------

    #[test]
    fn classify_top_level_marker() {
        let body = "module kio/main;\nhost type S role(str);\n\n__INSERT_CODE_HERE__\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::TopLevel
        );
    }

    #[test]
    fn classify_block_marker_inside_fn() {
        let body = "module kio/main;\n\npub fn main() -> . { __INSERT_CODE_HERE__ }\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::Block
        );
    }

    #[test]
    fn classify_block_marker_nested_groups() {
        let body =
            "module kio/main;\nfn outer() -> . { let inner = .() { __INSERT_CODE_HERE__ }; () }\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::Block
        );
    }

    #[test]
    fn classify_top_level_ignores_brace_in_line_comment() {
        // A `{` inside a `//` comment must not start a block.
        let body = "// note { not a real brace\nmodule x/main;\n\n__INSERT_CODE_HERE__\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::TopLevel
        );
    }

    #[test]
    fn classify_top_level_ignores_brace_in_string() {
        let body = "fn s() -> String { \"a { not real\" }\n__INSERT_CODE_HERE__\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::TopLevel
        );
    }

    #[test]
    fn classify_top_level_ignores_brace_in_block_comment() {
        let body = "/* { not a real brace */\nmodule x/main;\n\n__INSERT_CODE_HERE__\n";
        assert_eq!(
            classify_marker_position(body, "__INSERT_CODE_HERE__"),
            MarkerPosition::TopLevel
        );
    }

    #[test]
    fn harness_records_top_level_position_by_default() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\nmodule x/main;\n__INSERT_CODE_HERE__\n```\n";
        let d = build_from(src).unwrap();
        let h = d.harnesses.get("m").unwrap();
        assert_eq!(h.marker_position, MarkerPosition::TopLevel);
        assert!(!h.accumulate);
    }

    #[test]
    fn harness_records_block_position_when_wrapped() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\nmodule x/main;\npub fn main() -> . { __INSERT_CODE_HERE__ }\n```\n";
        let d = build_from(src).unwrap();
        let h = d.harnesses.get("m").unwrap();
        assert_eq!(h.marker_position, MarkerPosition::Block);
    }

    // ---- accumulate flag ------------------------------------------------

    #[test]
    fn harness_records_accumulate_flag() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\" accumulate}\nmodule x/main;\n__INSERT_CODE_HERE__\n```\n";
        let d = build_from(src).unwrap();
        let h = d.harnesses.get("m").unwrap();
        assert!(h.accumulate);
    }

    #[test]
    fn harness_rejects_unknown_bare_flag() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\" weird}\nmodule x/main;\n__INSERT_CODE_HERE__\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("bare attribute `weird`"));
    }

    #[test]
    fn accumulating_member_with_check_exit_code_errors() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\" accumulate}\nmodule x/main;\n__INSERT_CODE_HERE__\n```\n\n```kio {@m check_exit_code=14}\nfn f() -> . { () }\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("check_exit_code"));
        assert!(err.message.contains("accumulating harness"));
    }

    #[test]
    fn accumulating_member_with_stdout_errors() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\" accumulate}\nmodule x/main;\n__INSERT_CODE_HERE__\n```\n\n```kio {@m stdout}\nfn f() -> . { () }\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("run-trigger"));
        assert!(err.message.contains("accumulating harness"));
    }

    // ---- variant= ------------------------------------------------------

    #[test]
    fn variant_default_module() {
        let src = "```kio {}\nmodule x/main;\nfn f() -> . { () }\n```\n";
        let d = build_from(src).unwrap();
        let s = d.snippets().next().unwrap();
        assert_eq!(s.variant, KioFileKind::Module);
    }

    #[test]
    fn variant_package_recognized() {
        let src = "```kio {variant=package}\npackage pkg;\n```\n";
        let d = build_from(src).unwrap();
        let s = d.snippets().next().unwrap();
        assert_eq!(s.variant, KioFileKind::Package);
    }

    #[test]
    fn non_module_file_variants_recognized() {
        for (name, expected) in [
            ("signature", KioFileKind::Signature),
            ("dependency", KioFileKind::Dependency),
            ("lock", KioFileKind::Lock),
        ] {
            let src = format!("```kio {{variant={name}}}\nbody\n```\n");
            let d = build_from(&src).unwrap();
            let s = d.snippets().next().unwrap();
            assert_eq!(s.variant, expected);
        }
    }

    #[test]
    fn variant_unknown_value_errors() {
        let src = "```kio {variant=weird}\n\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("variant=KIND"));
    }

    #[test]
    fn variant_with_harness_ref_errors() {
        let src = "```kio {harness=m placeholder=\"__INSERT_CODE_HERE__\"}\n__INSERT_CODE_HERE__\n```\n\n```kio {@m variant=package}\npackage pkg;\n```\n";
        let err = build_from(src).unwrap_err();
        assert!(err.message.contains("variant="));
        assert!(err.message.contains("standalone"));
    }
}

//! Pretty-printer for the Kio AST — the back end of `kio fmt`.
//!
//! Produces the canonical-style output specified in
//! [`specs/style.md`](../../specs/style.md): two-space indent, A1
//! width-driven leading-comma multi-line layout for every comma-
//! separated list, the two-block import ordering with within-block
//! ASCII codepoint sort, and lowercase-hex / lowercase-`e` literal
//! canonicalisation. Top-level item / import leading comments survive
//! the round-trip, with blank-line separation between paragraphs
//! preserved and empty-`//` runs collapsed to one.
//!
//! **Idempotence is the contract**: for any source `s` that parses,
//! `pretty(parse(s)) == pretty(parse(pretty(parse(s))))`. The unit
//! tests at the bottom check this for every construct in the AST.
//!
//! ## Architecture
//!
//! The printer builds a [`crate::doc::Doc`] tree (Wadler-Leijen
//! combinators) and renders it with [`crate::doc::render`] at the
//! canonical 100-column budget. [`comma_list`](crate::doc::comma_list)
//! drives the width-driven A1 break for every comma-separated list
//! (parameter lists, member lists, call args, tuples, label sugar,
//! elaborator / `equiv` arm lists, etc.) — flat when it fits, leading-
//! comma multi-line when it doesn't or when an inner unconditional
//! break cascades out. [`align`](crate::doc::align) pins continuation
//! chunks of a reflowed string literal to the column of the first
//! `"`. Comments are preserved everywhere they are legal: at node /
//! container / element boundaries they emit in place, and genuinely
//! interstitial comments are hoisted to the nearest canonical line —
//! never dropped (per `specs/style.md` § Comments). Trailing and
//! end-of-block / end-of-file comments ride a phase-erased
//! `meta.trailing_trivia` slot the parser stashes from the closing
//! delimiter's leading run.

use crate::ast::FnPurityExt;
use crate::ast::{
    BridgeBlock, BridgeGlobSegment, BuildBlock, BuildBlockCache, CallArg, DependencyFile,
    DocComment, ElaboratorCall, Equiv, Expr, FieldAccessLabel, FieldUpdateLabel, FnDef, HostFn,
    HostFnParam, HostType, Import, ImportItem, ImportKind, Item, LabelForward, LabelSugarLabel,
    Labels, LiteralAlias, LiteralAliasValue, LockFile, Module, ModulePath, Newtype, Op, OpBody,
    OpPart, PackageFile, RowLetEntry, Signature, SignatureGroupRef, SignatureParam, SourceBlock,
    SourceOrigin, TargetBlock, TargetEntry, Type, TypeAlias, TypeParam, VariadicOperator,
};
use crate::doc::{
    Doc, align, comma_list, concat, concat_all, empty, flat_alt, group, hardline, join, line, nest,
    render, text,
};
use crate::pass::lexer::Trivia;

/// Canonical line-width budget used by every entry point, consumed by
/// [`crate::doc::Group`] fits-checks: `comma_list`'s width-driven A1
/// break flattens a comma-separated list when the flat form fits this
/// budget and breaks to the leading-comma layout when it doesn't
/// (`specs/style.md`'s 100-column rules).
const WIDTH: usize = 100;

pub fn pretty_module(m: &Module) -> String {
    render(&doc_module(m), WIDTH)
}

pub fn pretty_package_file(e: &PackageFile) -> String {
    render(&doc_package_file(e), WIDTH)
}

pub fn pretty_dependency_file(dep: &DependencyFile) -> String {
    render(&doc_dependency_file(dep), WIDTH)
}

pub fn pretty_lock_file(lock: &LockFile) -> String {
    render(&doc_lock_file(lock), WIDTH)
}

/// Render a single top-level item to its canonical source form,
/// **without** any attached `///` doc-comment. Used by Kiodoc's
/// `` [`@source(term)`] `` directive, which embeds the item's source
/// into rendered prose — the doc-comment is excluded because it would
/// recursively contain the very directive that triggered the render.
pub fn pretty_item_source(item: &Item) -> String {
    render(&doc_item(&strip_item_doc(item)), WIDTH)
}

pub(crate) fn pretty_item_block_entry(item: &Item) -> String {
    render(
        &concat(
            doc_item_leading_trivia(item.meta().leading_trivia.as_slice()),
            doc_item_entry(item),
        ),
        WIDTH,
    )
}

pub(crate) fn pretty_export_fn_block_entry(export: &crate::ast::SigExportFn) -> String {
    let head = if export.purity.is_pure() {
        "pub pure fn "
    } else {
        "pub fn "
    };
    render(
        &concat(
            doc_item_leading_trivia(export.function.meta.leading_trivia.as_slice()),
            doc_bodyless_fn(&export.function, head),
        ),
        WIDTH,
    )
}

/// Render a single top-level item's **signature** — the declaration
/// header with no body and no doc-comment. Used by Kiodoc's
/// `` [`@signature(term)`] `` directive. For `fn` this is
/// `fn name[type-params](value-params) -> Ret`; for `type` /
/// `literal` / `labels` / `op` / `newtype` the declaration is itself header-shaped, so the
/// whole (doc-comment-stripped) form is the signature.
pub fn pretty_item_signature(item: &Item) -> String {
    let stripped = strip_item_doc(item);
    let doc = match &stripped {
        Item::FnDef(d) => doc_fn_def_header(d),
        Item::RecGroup(g, _) if g.members.len() == 1 => {
            let loop_path = value_path_surface_str(&g.loop_path);
            let member = &g.members[0];
            let visibility = doc_vis(&member.vis);
            let mut rendered_member = member.clone();
            rendered_member.vis = crate::ast::Visibility::Private;
            concat_all([
                visibility,
                text(format!("rec({loop_path}) ")),
                doc_fn_def_header(&rendered_member),
            ])
        }
        Item::TypeRecGroup(group) => doc_type_rec_group(group),
        other => doc_item(other),
    };
    render(&doc, WIDTH)
}

/// Canonical public-cache identity for a user elaborator declaration.
///
/// Captures constrain visibility; ordered trailing descriptors determine block
/// projection. The private implementation target and its execution schedule are
/// deliberately absent: same-package elaborator consumers track those via
/// the separate implementation-source fingerprint.
#[cfg(feature = "cli")]
pub(crate) fn pretty_elaborator_public_signature(
    elaborator: &crate::ast::UserElaboratorDef,
) -> String {
    let entries = doc_elaborator_public_entries(elaborator);
    let has_entries = !entries.is_empty();
    render(
        &concat_all([
            doc_vis(&elaborator.vis),
            text(format!("elab {}", elaborator.name)),
            text(" : "),
            doc_type(&elaborator.call_ty),
            text(" { "),
            join(text("; "), entries),
            text(if has_entries { " }" } else { "}" }),
        ]),
        WIDTH,
    )
}

/// Render a single top-level item's **bound-value type** — the type
/// of the value the item binds, with no `fn` keyword, item name, or
/// value-binder names. Used by Kiodoc's `` [`@type term`] `` directive
/// and `kio repl`'s `:type`.
///
/// Only the value-binding kinds carry a bound value: a `fn` renders
/// its function type. `type` / `labels` / `newtype` are type-level
/// names — they bind no value — so this returns `None`; the caller
/// reports a kind-aware error. `literal` has no single standalone type,
/// because each use site can infer or annotate one. An `op` is an
/// operator binding, not a plain value query, and likewise yields `None`.
pub fn pretty_item_type(item: &Item) -> Option<String> {
    match item {
        Item::FnDef(d) => {
            let ty = d.sig.signature_ty(d.ret.clone(), d.meta.span);
            Some(pretty_type(&ty))
        }
        Item::HostFn(h) => {
            let sig = host_fn_signature(h);
            let ty = Type::synth_scheme_from_signature(&sig, h.ret.clone(), h.meta.span);
            Some(pretty_type(&ty))
        }
        Item::TypeAlias(_)
        | Item::LiteralAlias(_, _)
        | Item::Labels(_, _)
        | Item::LabelForward(_, _)
        | Item::Newtype(_)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::RecGroup(_, _)
        | Item::TypeRecGroup(_)
        | Item::Elaborator(_, _)
        | Item::HostType(_)
        | Item::Equiv(_, _) => None,
    }
}

/// Build a [`Signature`] from a [`HostFn`]'s grouped params — used by
/// [`pretty_item_type`] to render a host fn's declared function type.
fn host_fn_signature(h: &HostFn) -> Signature {
    let params = h
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| match p {
            HostFnParam::Type(tp) => SignatureParam::Type(tp.clone()),
            HostFnParam::Value(vp) => SignatureParam::Value(crate::ast::Param {
                name: vp.name.clone().unwrap_or_else(|| format!("_p{i}")),
                ty: Some(vp.ty.clone()),
                pattern: None,
                meta: vp.meta.clone(),
            }),
        })
        .collect();
    Signature::from_parts(params, h.param_groups.clone())
}

/// Render a Surface-phase [`Type`] to its canonical surface form.
///
/// Unlike [`crate::pass::typecheck_core::display_type`] — which requires a
/// post-label-elab phase (`TypeLabelSugar = Never`) — this renders a
/// `Type<Surface>` directly, so it can show a type read straight off
/// a parsed declaration's signature. Used by `kio repl`'s `:t` to
/// print a function's declared type.
pub fn pretty_type(ty: &Type) -> String {
    render(&doc_type(ty), WIDTH)
}

/// Render a single `import` clause to its canonical surface form. Used by
/// the `*.sig.kio` changelog emitter (`crate::sig::emit`), whose module
/// sections carry the same `import` grammar as a regular module.
pub fn pretty_import(u: &Import) -> String {
    render(&doc_import(u), WIDTH)
}

pub(crate) fn pretty_import_block_entry(u: &Import) -> String {
    render(&doc_import_entry(u), WIDTH)
}

pub(crate) fn pretty_leading_trivia(trivia: &[Trivia]) -> String {
    render(&doc_item_leading_trivia(trivia), WIDTH)
}

/// Clone `item` with its doc-comment cleared. The renderer entry
/// points above strip the doc-comment so it doesn't appear in the
/// embedded output.
fn strip_item_doc(item: &Item) -> Item {
    let mut item = item.clone();
    match &mut item {
        Item::FnDef(d) => d.doc = None,
        Item::TypeAlias(a) => a.doc = None,
        Item::LiteralAlias(l, _) => l.doc = None,
        Item::Newtype(n) => n.doc = None,
        Item::Labels(t, _) => t.doc = None,
        Item::LabelForward(t, _) => t.doc = None,
        Item::Op(o, _) => o.doc = None,
        Item::VariadicOperator(f, _) => f.doc = None,
        Item::Elaborator(s, _) => s.doc = None,
        Item::RecGroup(g, _) => {
            for member in &mut g.members {
                member.doc = None;
            }
        }
        Item::TypeRecGroup(group) => {
            group.doc = None;
            for member in &mut group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(alias) => alias.doc = None,
                    crate::ast::TypeRecMember::Newtype(newtype) => newtype.doc = None,
                    crate::ast::TypeRecMember::Labels(labels, _) => labels.doc = None,
                }
            }
        }
        Item::HostType(h) => h.doc = None,
        Item::HostFn(h) => h.doc = None,
        // `equiv` carries no doc field.
        Item::Equiv(_, _) => {}
    }
    item
}

/// Render an item's visibility prefix: nothing for `Private`, `pub ` for
/// `Public`, `pub(a/b) ` for `PublicIn`. The trailing space matches
/// the bare-`pub` spelling the call sites used.
pub fn pretty_visibility(vis: &crate::ast::Visibility) -> String {
    render(&doc_vis(vis), WIDTH)
}

fn doc_vis(vis: &crate::ast::Visibility) -> Doc {
    match vis {
        crate::ast::Visibility::Private => empty(),
        crate::ast::Visibility::Public => text("pub "),
        crate::ast::Visibility::PublicIn(p) => {
            text(format!("pub({}) ", module_path_surface_str(&p.segments)))
        }
    }
}

/// Render a `fn` definition's header only — everything `doc_fn_def`
/// emits except the doc-comment prefix and the block body.
fn doc_fn_def_header(d: &FnDef) -> Doc {
    let pubword = doc_vis(&d.vis);
    let pureword = if d.purity.is_pure_fn() {
        text("pure ")
    } else {
        empty()
    };
    let ret_doc = if d.ret_elided {
        empty()
    } else {
        doc_chain_body_layout(&d.ret, doc_type(&d.ret), " -> ", " ->", empty(), empty())
    };
    concat_all([
        pubword,
        pureword,
        text(format!("fn {}", d.name)),
        doc_signature(&d.sig),
        ret_doc,
    ])
}

// ---- top level ---------------------------------------------------------

fn doc_module(m: &Module) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    // Emit the module's own leading line comments — a `//` file header
    // above the `module` line — before the `///` doc-comment and the
    // header. `extract_doc_comment` pulled the trailing `///` run into
    // `m.doc`, so `leading_trivia` holds only the non-doc comments;
    // there is no double-emission. Without this, a plain `//` header
    // would be silently dropped on format (top-level *items* already
    // emit their leading trivia below).
    parts.push(doc_item_leading_trivia(m.meta.leading_trivia.as_slice()));
    if let Some(dc) = &m.doc {
        parts.push(doc_doc_comment(dc));
    }
    parts.extend([
        text("module ".to_owned()),
        doc_module_path(&m.path),
        text(";"),
        hardline(),
    ]);

    if !m.imports.is_empty() {
        parts.push(hardline());
        parts.push(doc_import_blocks(&m.imports));
    }

    if !m.items.is_empty() {
        parts.push(hardline());
        for (i, item) in m.items.iter().enumerate() {
            if i > 0 {
                parts.push(hardline());
            }
            // Emit any preserved leading line comments at column 0
            // before the item. Newlines in the trivia are dropped —
            // blank-line policy is already enforced by the
            // surrounding emit (one blank line between top-level
            // items). An empty-comment-line
            // run (`//\n//\n`) collapses to a single `//`.
            let trivia = item.meta().leading_trivia.as_slice();
            parts.push(doc_item_leading_trivia(trivia));
            parts.push(doc_item(item));
            parts.push(hardline());
        }
    }
    // Comments after the last item, before end-of-file, ride
    // `m.meta.trailing_trivia` (stashed by the parser). Emit them at
    // column 0 below the body, separated by one blank line like a
    // top-level item, so a trailing-after-item or end-of-file comment
    // survives the round-trip.
    let trailing = m.meta.trailing_trivia.as_slice();
    if has_comments(trailing) {
        parts.push(hardline());
        parts.push(doc_item_leading_trivia(trailing));
    }
    concat_all(parts)
}

/// Emit the comment portion of a leading-trivia run before a top-
/// level item. Each `LineComment` emits on its own line at column 0,
/// in source order. Two layout rules apply, both per
/// `specs/style.md`:
///
/// - **Blank-line preservation between comment paragraphs.** A run
///   of two or more consecutive `Newline` trivia between two
///   comments emits as a single blank line in the output. A run of
///   one newline emits as no blank line. This lets authors keep
///   distinct paragraphs of docstring above an item separated by
///   blank lines without the formatter merging them.
/// - **Empty-comment-line collapse.** A run of two or more empty
///   `//` lines (each comment body empty after trailing-whitespace
///   stripping) collapses to a single empty `//` line.
///
/// Trailing newlines after the last comment are dropped — the
/// surrounding emit attaches the trivia directly above the item.
fn doc_item_leading_trivia(trivia: &[Trivia]) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_empty_comment = false;
    let mut newline_run: usize = 0;
    let mut emitted_first = false;
    for entry in trivia {
        match entry {
            Trivia::Newline => newline_run += 1,
            Trivia::LineComment { text: body, .. } => {
                let is_empty = body.is_empty();
                if is_empty && last_was_empty_comment {
                    newline_run = 0;
                    continue;
                }
                if emitted_first && newline_run >= 2 {
                    // Blank line between two comment paragraphs.
                    parts.push(hardline());
                }
                parts.push(text(format!("//{body}")));
                parts.push(hardline());
                last_was_empty_comment = is_empty;
                emitted_first = true;
                newline_run = 0;
            }
            // Doc-comment lines in the raw leading-trivia vector are
            // emitted here for any item that carries them in raw trivia
            // (rather than through the `DocComment` field). This handles
            // the round-trip for items that the parser hasn't yet plumbed
            // doc-attachment through, and for `import` statements.
            Trivia::DocCommentLine { text: body, .. } => {
                if emitted_first && newline_run >= 2 {
                    parts.push(hardline());
                }
                // Re-emit with normalized prefix: `/// ` + body, or `///`
                // for a blank doc line (empty body).
                let line = if body.is_empty() {
                    "///".to_owned()
                } else {
                    format!("/// {body}")
                };
                parts.push(text(line));
                parts.push(hardline());
                last_was_empty_comment = false;
                emitted_first = true;
                newline_run = 0;
            }
        }
    }
    concat_all(parts)
}

/// Render a `DocComment` as a sequence of `/// line\n` lines.
/// Each line is emitted with the normalized prefix: `///` for a blank
/// doc line, `/// ` + content otherwise. Per `specs/style.md`.
fn doc_doc_comment(dc: &DocComment) -> Doc {
    let mut parts: Vec<Doc> = Vec::with_capacity(dc.lines.len() * 2);
    for line_text in &dc.lines {
        if line_text.is_empty() {
            parts.push(text("///"));
        } else {
            parts.push(text(format!("/// {line_text}")));
        }
        parts.push(hardline());
    }
    concat_all(parts)
}

/// Render a pure module reference: every segment joined with the
/// module-path separator `/` (`module a/b/c;`, `import a/b/c(…);`,
/// `module m = a/b/c;` in an export block).
fn module_path_surface_str<S: AsRef<str>>(segs: &[S]) -> String {
    segs.iter()
        .map(|s| s.as_ref())
        .collect::<Vec<_>>()
        .join("/")
}

/// Render a type path: the leading module segments joined with `/`
/// and the final type name reached with `.` (`a/b/c.Type`). A single
/// segment renders bare (a local type, `Type`).
fn type_path_surface_str<S: AsRef<str>>(segs: &[S]) -> String {
    match segs.split_last() {
        None => String::new(),
        Some((item, [])) => item.as_ref().to_owned(),
        Some((item, module)) => {
            format!("{}.{}", module_path_surface_str(module), item.as_ref())
        }
    }
}

/// Render an ordinary lexical value path (`f`, `m.f`, `m.Type.member`).
fn value_path_surface_str<S: AsRef<str>>(segs: &[S]) -> String {
    segs.iter()
        .map(|segment| segment.as_ref())
        .collect::<Vec<_>>()
        .join(".")
}

fn doc_module_path(p: &ModulePath) -> Doc {
    text(module_path_surface_str(&p.segments))
}

/// Emit a module's `import` statements as canonical blocks separated by
/// exactly one blank line and each sorted
/// lexicographically by the full rendered string of the import
/// statement. Within a single selective `import` form, the imported
/// names are also sorted lexicographically (handled inside
/// [`doc_import`]). Per specs/style.md.
///
/// Each import's leading line comments live on `Import::leading_trivia`
/// and survive into the canonical output, attached to the
/// corresponding `import` statement after sorting — so
/// `// docstring` above `import path(foo);` stays above the
/// `import` regardless of where the sort places it within its block.
fn doc_import_blocks(imports: &[Import]) -> Doc {
    // Each entry: (rendered import text, comment-only trivia run).
    // The trivia is reduced to LineComments only — newlines drop
    // since the canonical import-block layout dictates spacing.
    let mut intrinsics: Vec<(String, Vec<String>)> = Vec::new();
    let mut others: Vec<(String, Vec<String>)> = Vec::new();
    for u in imports {
        // Render this import's Doc to a string for sorting and re-emission.
        let buf = render(&doc_import(u), WIDTH);
        let mut comments: Vec<String> = Vec::new();
        let mut last_was_empty = false;
        for t in &u.leading_trivia {
            if let Trivia::LineComment { text: body, .. } = t {
                let is_empty = body.is_empty();
                if is_empty && last_was_empty {
                    continue;
                }
                comments.push(body.clone());
                last_was_empty = is_empty;
            }
        }
        let entry = (buf, comments);
        match &u.kind {
            ImportKind::Intrinsics => intrinsics.push(entry),
            _ => others.push(entry),
        }
    }
    // Within each block: ASCII codepoint sort (no locale, no case-
    // folding) on the full rendered string. Stable cross-platform.
    intrinsics.sort_by(|a, b| a.0.cmp(&b.0));
    others.sort_by(|a, b| a.0.cmp(&b.0));

    let mut parts: Vec<Doc> = Vec::new();
    let mut wrote_block = false;
    for block in [&intrinsics, &others] {
        if block.is_empty() {
            continue;
        }
        if wrote_block {
            // Exactly one blank line between non-empty blocks.
            parts.push(hardline());
        }
        for (rendered, comments) in block {
            for body in comments {
                parts.push(text(format!("//{body}")));
                parts.push(hardline());
            }
            parts.push(text(rendered.clone()));
            parts.push(hardline());
        }
        wrote_block = true;
    }
    concat_all(parts)
}

fn doc_import(u: &Import) -> Doc {
    concat(doc_import_entry(u), text(";"))
}

fn doc_import_entry(u: &Import) -> Doc {
    match &u.kind {
        ImportKind::Selective { items, from } => {
            let mut sorted: Vec<(&ImportItem, String)> = items
                .iter()
                .map(|item| (item, import_item_display(item)))
                .collect();
            sorted.sort_by(|a, b| a.1.cmp(&b.1));
            let provider = module_path_surface_str(&from.segments);
            let item_trivia: Vec<Vec<Trivia>> = sorted
                .iter()
                .map(|(item, _)| item.leading_trivia().to_vec())
                .collect();
            concat(
                text(format!("import {provider}")),
                doc_comma_list_with_trivia(
                    text("("),
                    sorted.into_iter().map(|(_, name)| text(name)).collect(),
                    &item_trivia,
                    &u.trailing_trivia,
                    text(")"),
                ),
            )
        }
        ImportKind::Qualified { path, alias } => text(format!(
            "import {} as {alias}",
            module_path_surface_str(&path.segments)
        )),
        ImportKind::Intrinsics => text("import __intrinsics__"),
        ImportKind::Comptime => text("import __comptime__"),
    }
}

fn import_item_display(item: &crate::ast::ImportItem) -> String {
    match item {
        ImportItem::Name { name, .. } => name.clone(),
        ImportItem::Label { name, .. } => format!("{{{name}}}"),
        ImportItem::OperatorPattern { grammar, .. } => grammar.render(),
    }
}

fn doc_item(item: &Item) -> Doc {
    if let Item::TypeAlias(alias) = item {
        return doc_type_alias_with_terminator(alias, true);
    }
    let body = doc_item_entry(item);
    if matches!(
        item,
        Item::LiteralAlias(..)
            | Item::Labels(..)
            | Item::LabelForward(..)
            | Item::HostType(_)
            | Item::HostFn(_)
    ) {
        concat(body, text(";"))
    } else {
        body
    }
}

fn doc_item_entry(item: &Item) -> Doc {
    match item {
        Item::FnDef(d) => doc_fn_def(d),
        Item::TypeAlias(a) => doc_type_alias(a),
        Item::LiteralAlias(a, _) => doc_literal_alias(a),
        Item::Newtype(d) => doc_newtype(d),
        Item::Labels(d, _) => doc_labels(d),
        Item::LabelForward(d, _) => doc_label_forward(d),
        Item::Equiv(e, _) => doc_equiv(e),
        Item::Elaborator(s, _) => doc_elaborator_item(s),
        Item::RecGroup(g, _) => doc_rec_group(g),
        Item::TypeRecGroup(g) => doc_type_rec_group(g),
        Item::Op(d, _) => doc_op(d),
        Item::VariadicOperator(d, _) => doc_fold(d),
        Item::HostType(h) => doc_host_type(h),
        Item::HostFn(h) => doc_host_fn(h),
    }
}

fn doc_rec_group(g: &crate::ast::RecGroup) -> Doc {
    let loop_path = value_path_surface_str(&g.loop_path);
    if g.members.len() == 1 && !has_comments(g.meta.trailing_trivia.as_slice()) {
        let member = &g.members[0];
        let doc_prefix = member
            .doc
            .as_ref()
            .map(doc_doc_comment)
            .unwrap_or_else(empty);
        let pubword = doc_vis(&member.vis);
        let mut rendered_member = member.clone();
        rendered_member.vis = crate::ast::Visibility::Private;
        rendered_member.doc = None;
        return concat_all([
            doc_item_leading_trivia(member.meta.leading_trivia.as_slice()),
            doc_prefix,
            pubword,
            text(format!("rec({loop_path}) ")),
            doc_fn_def(&rendered_member),
        ]);
    }
    let member_docs: Vec<Doc> = g
        .members
        .iter()
        .map(|member| {
            concat_all([
                doc_item_leading_trivia(member.meta.leading_trivia.as_slice()),
                doc_fn_def(member),
            ])
        })
        .collect();
    concat_all([
        text(format!("rec({loop_path}) {{")),
        nest(
            2,
            concat(hardline(), join(concat(text(";"), hardline()), member_docs)),
        ),
        if has_comments(g.meta.trailing_trivia.as_slice()) {
            nest(
                2,
                concat(
                    hardline(),
                    doc_trailing_comments(g.meta.trailing_trivia.as_slice()),
                ),
            )
        } else {
            empty()
        },
        hardline(),
        text("}"),
    ])
}

fn doc_type_rec_group(g: &crate::ast::TypeRecGroup) -> Doc {
    let doc_prefix = g.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let members = g.members.iter().map(|member| {
        let leading = doc_item_leading_trivia(member.meta().leading_trivia.as_slice());
        let declaration = match member {
            crate::ast::TypeRecMember::TypeAlias(alias) => doc_type_alias(alias),
            crate::ast::TypeRecMember::Newtype(newtype) => doc_newtype(newtype),
            crate::ast::TypeRecMember::Labels(labels, _) => doc_labels(labels),
        };
        concat_all([leading, declaration])
    });
    let trailing = g.meta.trailing_trivia.as_slice();
    let trailing = if has_comments(trailing) {
        concat(hardline(), doc_item_leading_trivia(trailing))
    } else {
        empty()
    };
    concat_all([
        doc_prefix,
        text("rec {"),
        nest(
            2,
            concat(hardline(), join(concat(text(";"), hardline()), members)),
        ),
        nest(2, trailing),
        hardline(),
        text("}"),
    ])
}

fn doc_fn_def(d: &FnDef) -> Doc {
    let doc_prefix = d.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&d.vis);
    let pureword = if d.purity.is_pure_fn() {
        text("pure ")
    } else {
        empty()
    };
    // Return type is width-driven: a chain return that doesn't
    // fit inline breaks to the leading-operator multi-line shape.
    // The body block follows the broken return; its own group
    // makes its own break decision independently.
    let ret_doc = if d.ret_elided {
        empty()
    } else {
        doc_chain_body_layout(&d.ret, doc_type(&d.ret), " -> ", " ->", empty(), empty())
    };
    concat_all([
        doc_prefix,
        pubword,
        pureword,
        text(format!("fn {}", d.name)),
        doc_signature(&d.sig),
        ret_doc,
        group(doc_block_body_expr(&d.body, true)),
    ])
}

fn doc_elaborator_item(s: &crate::ast::UserElaboratorDef) -> Doc {
    let doc_prefix = s.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&s.vis);
    let mut entries = doc_elaborator_public_entries(s);
    entries.push(concat_all([
        text(match s.schedule {
            crate::ast::ElaboratorSchedule::Late => "impl ",
            crate::ast::ElaboratorSchedule::Fills => "impl(fills) ",
        }),
        text(value_path_surface_str(s.implementation.segments())),
    ]));
    concat_all([
        doc_prefix,
        pubword,
        text(format!("elab {}", s.name)),
        text(" : "),
        doc_type(&s.call_ty),
        doc_compact_declaration_body(&s.body_trivia, join(text("; "), entries)),
    ])
}

fn doc_elaborator_public_entries(s: &crate::ast::UserElaboratorDef) -> Vec<Doc> {
    let mut entries = Vec::new();
    if !s.captures.is_empty() {
        entries.push(doc_elaborator_captures(s));
    }
    for block in &s.trailing_blocks {
        entries.push(text(format!(
            "trailing {}{}",
            match block.exposure {
                crate::ast::BlockExposure::Product => "product",
                crate::ast::BlockExposure::Thunk => "thunk",
                crate::ast::BlockExposure::Sequence => "sequence",
            },
            block
                .label
                .as_ref()
                .map_or_else(String::new, |label| format!(" {}", label.name)),
        )));
    }
    entries
}

fn doc_elaborator_captures(s: &crate::ast::UserElaboratorDef) -> Doc {
    if s.captures.is_empty() {
        empty()
    } else if s.captures.len() == 1 {
        concat_all([text("captures "), doc_capture_path(&s.captures[0])])
    } else {
        concat_all([
            text("captures ("),
            join(text(", "), s.captures.iter().map(doc_capture_path)),
            text(")"),
        ])
    }
}

fn doc_capture_path(capture: &crate::ast::UserElaboratorCapture) -> Doc {
    text(
        capture
            .segments
            .iter()
            .map(|segment| segment.as_str())
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// Render a block body from its `Expr`, appending any comments
/// stranded between the body's last token and the closing `}` (the
/// parser stashed them on `body.meta.trailing_trivia`; see
/// [`crate::pass::parser`]). A dangling comment forces the block to
/// the broken multi-line layout — a comment can't sit inline before
/// `}` — so the flat form is suppressed whenever one is present. This
/// is the single choke point for block / lambda / `match!`-clause
/// bodies; `allow_flat` mirrors the width-driven vs. always-break
/// distinction the two former entry points carried.
fn doc_block_body_expr(body: &Expr, allow_flat: bool) -> Doc {
    let content = doc_expr_with_own_meta_trivia(body);
    doc_block_body_with_trailing(content, body.meta().trailing_trivia.as_slice(), allow_flat)
}

/// Block-body layout for a pre-rendered content `Doc` plus its
/// closing-boundary trailing trivia. Shared by [`doc_block_body_expr`]
/// and the placeholder-lambda path. A dangling comment forces the broken
/// layout.
fn doc_block_body_with_trailing(content: Doc, trailing: &[Trivia], allow_flat: bool) -> Doc {
    if has_comments(trailing) {
        let with_dangling = concat_all([content, hardline(), doc_trailing_comments(trailing)]);
        doc_braced_block(empty(), Some(with_dangling), false)
    } else {
        doc_braced_block(empty(), Some(content), allow_flat)
    }
}

fn doc_braced_block(header: Doc, body: Option<Doc>, allow_flat: bool) -> Doc {
    let Some(body) = body else {
        return concat_all([header, text(" {}")]);
    };
    let broken = concat_all([
        header.clone(),
        text(" {"),
        nest(2, concat(hardline(), body.clone())),
        hardline(),
        text("}"),
    ]);
    if allow_flat {
        flat_alt(concat_all([header, text(" { "), body, text(" }")]), broken)
    } else {
        broken
    }
}

/// Width-driven flat-or-broken layout for a "head LEAD body TRAIL"
/// position whose body is a `&`/`|` chain. The flat form is the
/// standard one-liner `head LEAD body TRAIL`; the broken form
/// lays the body on its own line at +2 indent with `LEAD` ending
/// the head's line and `TRAIL` on its own line at the same +2
/// indent — the leading-operator A1 analogue. Non-chain bodies
/// stay inline so a wide type-app body breaks its OWN args (A1
/// leading-comma) rather than this enclosing form.
///
/// The break is body-only: the head's own group decisions (e.g.,
/// the type-param list breaking to A1) are independent of the
/// body's break.
fn doc_chain_body_layout(
    body_ast: &Type,
    body_doc: Doc,
    lead_inline: &'static str,
    lead_break: &'static str,
    trail_inline: Doc,
    trail_break: Doc,
) -> Doc {
    let body_is_chain = matches!(body_ast, Type::Product { .. } | Type::Sum { .. });
    if !body_is_chain {
        return concat_all([text(lead_inline), body_doc, trail_inline]);
    }
    let flat = concat_all([text(lead_inline), body_doc.clone(), trail_inline]);
    let broken = nest(
        2,
        concat_all([
            text(lead_break),
            hardline(),
            body_doc,
            hardline(),
            trail_break,
        ]),
    );
    group(flat_alt(flat, broken))
}

fn doc_type_alias(a: &TypeAlias) -> Doc {
    doc_type_alias_with_terminator(a, false)
}

fn doc_type_alias_with_terminator(a: &TypeAlias, terminated: bool) -> Doc {
    let doc_prefix = a.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&a.vis);
    let head = concat_all([pubword, text(format!("type {}", a.name))]);
    let params = if a.type_params.is_empty() {
        empty()
    } else {
        doc_type_params(&a.type_params)
    };
    concat_all([
        doc_prefix,
        head,
        params,
        // Outer aliases keep the broken chain's own-line terminator;
        // nested aliases leave punctuation to their enclosing sequence.
        if terminated {
            doc_chain_body_layout(
                &a.body,
                doc_type(&a.body),
                " = ",
                " =",
                text(";"),
                text(";"),
            )
        } else if matches!(&a.body, Type::Product { .. } | Type::Sum { .. }) {
            let body = doc_type(&a.body);
            group(flat_alt(
                concat(text(" = "), body.clone()),
                nest(2, concat_all([text(" ="), hardline(), body])),
            ))
        } else {
            concat(text(" = "), doc_type(&a.body))
        },
    ])
}

fn doc_label_forward(forward: &LabelForward) -> Doc {
    let doc_prefix = forward
        .doc
        .as_ref()
        .map(doc_doc_comment)
        .unwrap_or_else(empty);
    let head = text(format!("type {{{}}} =", forward.name));
    let declaration = if has_comments(&forward.body_trivia) {
        concat(
            head,
            doc_compact_declaration_body(&forward.body_trivia, text(forward.target.clone())),
        )
    } else {
        group(concat_all([
            head,
            nest(2, concat(line(), text(format!("{{{}}}", forward.target)))),
        ]))
    };
    concat_all([doc_prefix, doc_vis(&forward.vis), declaration])
}

fn doc_literal_alias(a: &LiteralAlias) -> Doc {
    let doc_prefix = a.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&a.vis);
    concat_all([
        doc_prefix,
        pubword,
        text(format!("literal {} = ", a.name)),
        doc_literal_alias_value(&a.value),
    ])
}

fn doc_literal_alias_value(value: &LiteralAliasValue) -> Doc {
    match value {
        LiteralAliasValue::Str { value, .. } => doc_string_literal(value),
        LiteralAliasValue::Int { digits, .. } => text(digits.clone()),
        LiteralAliasValue::Float { digits, .. } => {
            let canonical: String = digits
                .chars()
                .filter(|&c| c != '+')
                .map(|c| if c == 'E' { 'e' } else { c })
                .collect();
            text(canonical)
        }
        LiteralAliasValue::Bool { value, .. } => text(if *value { ".t" } else { ".f" }),
    }
}

fn doc_newtype(d: &Newtype) -> Doc {
    let doc_prefix = d.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&d.vis);
    let recword = if d.rec_span.is_some() {
        text("rec ")
    } else {
        empty()
    };
    let head = concat_all([pubword, recword, text(format!("newtype {}", d.name))]);
    let params = if d.type_params.is_empty() {
        empty()
    } else {
        doc_type_params(&d.type_params)
    };
    // Constructor and projector retain their comments when canonical order
    // differs from source order.
    let cons_pub = doc_vis(&d.constructor.vis);
    let proj_pub = doc_vis(&d.projector.vis);
    let cons_doc = concat_all([
        doc_inline_leading_trivia(&d.constructor.leading_trivia),
        cons_pub,
        text("constructor "),
        text(d.constructor.name.clone()),
    ]);
    let proj_doc = concat_all([
        doc_inline_leading_trivia(&d.projector.leading_trivia),
        proj_pub,
        text("projector "),
        text(d.projector.name.clone()),
    ]);
    let members_trailing = d.meta.trailing_trivia.as_slice();
    let dangling = has_comments(members_trailing);
    let inline_after_open = concat_all([
        text(" "),
        cons_doc.clone(),
        text("; "),
        proj_doc.clone(),
        text(" }"),
    ]);
    let mut broken_parts: Vec<Doc> = vec![hardline(), cons_doc, text(";"), hardline(), proj_doc];
    if dangling {
        broken_parts.push(hardline());
        broken_parts.push(doc_trailing_comments(members_trailing));
    }
    broken_parts.push(hardline());
    broken_parts.push(text("}"));
    let broken_after_open = nest(2, concat_all(broken_parts));
    let after_open = if dangling {
        broken_after_open
    } else {
        group(flat_alt(inline_after_open, broken_after_open))
    };
    // Existential binders live on the newtype header: emit them as
    // whitespace-separated `<U> <V>` atoms between the universal
    // parameter list and the `:`.
    let existentials = if d.existential_params.is_empty() {
        empty()
    } else {
        concat(text(" "), doc_existential_params(&d.existential_params))
    };
    // Width-driven flat-or-broken for the payload type — when
    // it's a chain that's too wide, break to the leading-operator
    // multi-line layout. The trail is ` {` (the member-list
    // opener); when the payload breaks, `{` lands on its own line
    // at the same +2 indent as the chain items. The member list
    // body itself is width-driven inside `after_open`.
    let payload_part = doc_chain_body_layout(
        &d.payload,
        doc_type(&d.payload),
        " : ",
        " :",
        text(" {"),
        text("{"),
    );
    concat_all([
        doc_prefix,
        head,
        params,
        existentials,
        payload_part,
        after_open,
    ])
}

fn doc_op(d: &Op) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    if let Some(dc) = &d.doc {
        parts.push(doc_doc_comment(dc));
    }
    parts.push(doc_vis(&d.vis));
    parts.push(text("op"));
    match &d.body {
        OpBody::Normal { pattern, function } => {
            // Emit `( … )` around maximal runs of `lenient = true`
            // parts so the declaration-only slot-grouping marker
            // round-trips through `kio fmt`. Per-token `quoted =
            // true` (operator-token quotation `(tok)`) is emitted
            // inline as `(tok)` instead.
            let mut in_group = false;
            for part in pattern {
                let part_lenient = part.is_lenient();
                if part_lenient && !in_group {
                    parts.push(text(" ("));
                    in_group = true;
                } else if !part_lenient && in_group {
                    parts.push(text(" )"));
                    in_group = false;
                }
                parts.push(text(" "));
                match part {
                    OpPart::SlotPlain { .. } => parts.push(text("_")),
                    OpPart::SlotRecursive { .. } => parts.push(text("__")),
                    OpPart::SlotGreedy { .. } => parts.push(text("___")),
                    OpPart::Token {
                        content, quoted, ..
                    } => {
                        if *quoted {
                            parts.push(text(format!("({content})")));
                        } else {
                            parts.push(text(content.clone()));
                        }
                    }
                }
            }
            if in_group {
                parts.push(text(" )"));
            }
            parts.push(doc_compact_declaration_body(
                &d.body_trivia,
                concat_all([text("impl "), text(value_path_surface_str(function))]),
            ));
        }
    }
    concat_all(parts)
}

fn doc_fold(d: &VariadicOperator) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    if let Some(dc) = &d.doc {
        parts.push(doc_doc_comment(dc));
    }
    parts.push(doc_vis(&d.vis));
    parts.push(text("varop"));
    doc_fold_pattern_and_body(&mut parts, &d.open, &d.spec, &d.body_trivia);
    concat_all(parts)
}

fn doc_fold_pattern_and_body(
    parts: &mut Vec<Doc>,
    open: &[String],
    spec: &crate::ast::VariadicSpec,
    body_trivia: &[Trivia],
) {
    for tok in open {
        parts.push(text(" "));
        parts.push(text(tok.clone()));
    }
    for tok in &spec.close {
        parts.push(text(" "));
        parts.push(text(tok.clone()));
    }
    let mode = spec.mode.keyword();
    let mut body = vec![
        text(format!("{mode} ")),
        doc_callable_spec(&spec.step),
        text(" "),
        doc_callable_spec(&spec.initializer),
    ];
    if let Some(fin) = &spec.finalize {
        body.push(text("; finalize "));
        body.push(doc_callable_spec(fin));
    }
    parts.push(doc_compact_declaration_body(body_trivia, concat_all(body)));
}

/// Keep comments captured from a compact declaration inside its braces. A
/// comment forces the body onto lines so an interior `///` cannot be reparsed
/// as documentation for the declaration that follows it.
fn doc_compact_declaration_body(trivia: &[Trivia], body: Doc) -> Doc {
    if has_comments(trivia) {
        concat_all([
            text(" {"),
            nest(
                2,
                concat_all([hardline(), doc_item_leading_trivia(trivia), body]),
            ),
            hardline(),
            text("}"),
        ])
    } else {
        concat_all([text(" { "), body, text(" }")])
    }
}

/// Render a suffix-free named step or finalize callable.
fn doc_callable_spec(spec: &crate::ast::CallableSpec) -> Doc {
    text(value_path_surface_str(&spec.path))
}

fn doc_equiv(e: &Equiv) -> Doc {
    // `equiv name[A](x: A) { term1; term2 }`. Arm lists are always
    // multi-line at 2+ items; each non-final arm carries `;`, and the
    // final arm omits it.
    let head = concat_all([text(format!("equiv {}", e.name)), doc_signature(&e.sig)]);
    if e.terms.is_empty() {
        return concat(head, text(" {}"));
    }
    if e.terms.len() == 1 {
        let term = &e.terms[0];
        return concat(
            head,
            concat_all([text(" { "), doc_expr(&term.body), text(" }")]),
        );
    }
    let mut body: Vec<Doc> = Vec::new();
    for (i, term) in e.terms.iter().enumerate() {
        body.push(hardline());
        body.push(doc_inline_leading_trivia(&term.meta.leading_trivia));
        body.push(doc_expr(&term.body));
        if i + 1 < e.terms.len() {
            body.push(text(";"));
        }
    }
    // A comment dangling before the closing `}` rides the last arm's
    // trailing slot; emit it on its own line before the closer.
    if let Some(last) = e.terms.last() {
        let trailing = last.meta.trailing_trivia.as_slice();
        if has_comments(trailing) {
            body.push(hardline());
            body.push(doc_trailing_comments(trailing));
        }
    }
    concat_all([
        head,
        text(" {"),
        nest(2, concat_all(body)),
        hardline(),
        text("}"),
    ])
}

fn doc_labels(d: &Labels) -> Doc {
    let doc_prefix = d.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&d.vis);
    let recword = if d.rec_span.is_some() {
        text("rec ")
    } else {
        empty()
    };
    let mut head: Vec<Doc> = vec![doc_prefix, pubword, recword, text("labels")];
    if let Some(name) = &d.type_alias_name {
        head.push(text(format!(" {name}")));
        if !d.type_alias_params.is_empty() {
            head.push(doc_type_params(&d.type_alias_params));
        }
        head.push(text(" ="));
    }
    let arms: Vec<&[crate::ast::LabelEntry]> = d
        .type_alias_arms
        .as_ref()
        .map(|arms| arms.iter().map(|arm| arm.entries.as_slice()).collect())
        .unwrap_or_else(|| vec![d.entries.as_slice()]);
    let any_entry_comments = arms.iter().flat_map(|entries| entries.iter()).any(|entry| {
        has_comments(&entry.meta.leading_trivia) || has_comments(&entry.meta.trailing_trivia)
    });
    let body = if arms.iter().all(|entries| entries.is_empty()) {
        text(" {}")
    } else if arms.len() == 1 && arms[0].len() == 1 && !any_entry_comments {
        concat_all([text(" { "), doc_label_entry(&arms[0][0]), text(" }")])
    } else if any_entry_comments {
        // Any captured comment forces the multi-line layout so each
        // comment can sit on its own line above its entry; a trailing
        // comment on the last entry sits below it before the `}`.
        let mut rendered_arms: Vec<Doc> = Vec::new();
        for entries in arms {
            let mut inner: Vec<Doc> = Vec::new();
            for entry in entries {
                inner.push(hardline());
                inner.push(doc_inline_leading_trivia(&entry.meta.leading_trivia));
                inner.push(text(", "));
                // Same item-content nesting as the shared A1 layouts:
                // the `", "` prefix advances the column, the nest keeps
                // a multi-line entry anchored at its own start.
                inner.push(nest(2, doc_label_entry(entry)));
            }
            if let Some(last) = entries.last() {
                let trailing = last.meta.trailing_trivia.as_slice();
                if has_comments(trailing) {
                    inner.push(hardline());
                    inner.push(doc_trailing_comments(trailing));
                }
            }
            inner.push(hardline());
            inner.push(text("}"));
            rendered_arms.push(concat(text("{"), nest(2, concat_all(inner))));
        }
        concat_all([text(" "), join(text(" | "), rendered_arms)])
    } else {
        // Width-driven. A single arm is a standard A1 comma list:
        // flat ` { e1, e2 }` when it fits, else the leading-comma
        // layout with the `{` kept on the introducing line — the
        // same shape the comment-forced branch above produces, so
        // both paths anchor a multi-line entry at its content
        // column. Multi-arm alternation stays flat when it fits and
        // otherwise breaks arm-per-line; each arm then makes its own
        // width decision, with the `"| "` prefix mirrored as a
        // nest(2) like a comma prefix.
        let rendered_arms: Vec<Doc> = arms.iter().map(|entries| doc_labels_arm(entries)).collect();
        if rendered_arms.len() == 1 {
            let arm = rendered_arms.into_iter().next().unwrap_or_else(empty);
            group(concat_all([text(" "), arm]))
        } else {
            let flat = concat_all([text(" "), join(text(" | "), rendered_arms.clone())]);
            let mut broken_inner: Vec<Doc> = Vec::new();
            for (index, arm) in rendered_arms.into_iter().enumerate() {
                broken_inner.push(hardline());
                if index > 0 {
                    broken_inner.push(text("| "));
                    broken_inner.push(nest(2, group(arm)));
                } else {
                    broken_inner.push(group(arm));
                }
            }
            let broken = nest(2, concat_all(broken_inner));
            group(flat_alt(flat, broken))
        }
    };
    concat(concat_all(head), body)
}

/// One labels arm as an A1 comma list — flat `{ e1, e2 }` when the
/// enclosing group fits, else the leading-comma layout with each
/// entry nested at its content column (the same nest(2) mirror of
/// the `", "` prefix as [`comma_list`]). Returned ungrouped so the
/// caller decides the group boundary; the multi-arm broken layout groups
/// each arm to let it break independently.
fn doc_labels_arm(entries: &[crate::ast::LabelEntry]) -> Doc {
    let entries: Vec<Doc> = entries.iter().map(doc_label_entry).collect();
    if entries.len() <= 1 {
        // A1: 0 or 1 items always single-line, no width check.
        return concat_all([text("{ "), join(text(", "), entries), text(" }")]);
    }
    let flat = concat_all([text("{ "), join(text(", "), entries.clone()), text(" }")]);
    let mut broken_inner = Doc::Empty;
    for entry in &entries {
        broken_inner = concat(
            broken_inner,
            concat(hardline(), concat(text(", "), nest(2, entry.clone()))),
        );
    }
    broken_inner = concat(broken_inner, hardline());
    let broken = concat(text("{"), nest(2, concat(broken_inner, text("}"))));
    flat_alt(flat, broken)
}

// Render one label entry (without any leading `, ` or trivia). The
// entry's payload, type-params, and existential binders make their
// own width-driven decisions; the separator widens to ` : ` when
// existential params are present so the binder list (`<U> <V>`) reads
// as a distinct phrase from the payload-introducing `:`.
fn doc_label_entry(entry: &crate::ast::LabelEntry) -> Doc {
    let mut parts: Vec<Doc> = vec![text(entry.name.clone())];
    if !entry.type_params.is_empty() {
        parts.push(doc_type_params(&entry.type_params));
    }
    if !entry.existential_params.is_empty() {
        parts.push(text(" "));
        parts.push(doc_existential_params(&entry.existential_params));
    }
    parts.push(text(if entry.existential_params.is_empty() {
        ": "
    } else {
        " : "
    }));
    parts.push(doc_type(&entry.payload));
    concat_all(parts)
}

/// Render a universal binder `[A]`, prefixing one `*` per arrow in
/// the binder's kind (`[*F]` for kind `*→*`, `[**G]` for `*→*→*`;
/// see [`specs/grammar.md` § Kind grammar]). An unannotated / kind-`*`
/// binder renders with no stars.
fn type_binder_text(p: &TypeParam) -> String {
    let stars = "*".repeat(p.effective_kind().arity());
    format!("[{stars}{}]", p.name)
}

fn doc_type_params(params: &[TypeParam]) -> Doc {
    concat_all(params.iter().map(|p| text(type_binder_text(p))))
}

/// `<L>` or `<L> <R> ...` — whitespace-separated angle-bracket atoms,
/// used for existential binders on `newtype` declarations and
/// `labels` entries. Always single-line (existential binder lists
/// are short by construction).
fn doc_existential_params(params: &[TypeParam]) -> Doc {
    let parts: Vec<String> = params.iter().map(|p| format!("<{}>", p.name)).collect();
    text(parts.join(" "))
}

fn doc_signature(sig: &Signature) -> Doc {
    let groups = sig.canonical_groups();
    if groups.is_empty() {
        return text("()");
    }
    let mut saw_value_group = false;
    let mut parts = Vec::new();
    for group in groups {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(tp) = param else {
                        unreachable!("signature type group contains only type params")
                    };
                    parts.push(text(type_binder_text(tp)));
                }
            }
            SignatureGroupRef::Value(params) => {
                saw_value_group = true;
                let docs: Vec<Doc> = params.iter().map(doc_signature_param).collect();
                // Per-param leading trivia and the last param's
                // trailing run flow through the trivia-aware list so a
                // comment above or trailing a value parameter survives.
                let trivia: Vec<Vec<Trivia>> =
                    params.iter().map(signature_param_leading_trivia).collect();
                let last_trailing = params
                    .last()
                    .map(signature_param_trailing_trivia)
                    .unwrap_or(&[]);
                parts.push(doc_comma_list_with_trivia(
                    text("("),
                    docs,
                    &trivia,
                    last_trailing,
                    text(")"),
                ));
            }
        }
    }
    if !saw_value_group {
        parts.push(text("()"));
    }
    concat_all(parts)
}

/// The leading trivia captured before a signature value parameter.
/// Type-binder params carry no trivia slot, so they contribute an
/// empty run (the parser only stashes trivia on value params).
fn signature_param_leading_trivia(p: &SignatureParam) -> Vec<Trivia> {
    match p {
        SignatureParam::Type(_) => Vec::new(),
        SignatureParam::Value(vp) => vp.meta.leading_trivia.clone(),
    }
}

/// The trailing run stashed on a signature value parameter (the last
/// param's run before `)`); empty for a type-binder param.
fn signature_param_trailing_trivia(p: &SignatureParam) -> &[Trivia] {
    match p {
        SignatureParam::Type(_) => &[],
        SignatureParam::Value(vp) => vp.meta.trailing_trivia.as_slice(),
    }
}

fn doc_signature_param(p: &SignatureParam) -> Doc {
    match p {
        SignatureParam::Type(tp) => text(type_binder_text(tp)),
        SignatureParam::Value(vp) => match &vp.pattern {
            // Destructuring pattern present — emit the surface
            // pattern syntax (bare or as-pattern) and ignore the
            // synthesized `ty` (the desugar pass would have set it
            // from the pattern's structure, but only after parsing
            // — the formatter operates on the Surface AST before
            // desugar runs and so always sees `ty: None` here).
            Some(pat) if vp.name.starts_with("__pat_param_") => doc_param_pattern_tuple(pat),
            Some(pat) => concat(text(format!("{}: ", vp.name)), doc_param_pattern_tuple(pat)),
            None => match &vp.ty {
                None => text(vp.name.clone()),
                Some(ty) => concat(text(format!("{}: ", vp.name)), doc_type(ty)),
            },
        },
    }
}

/// Render a [`ParamPattern`] as `(elem, elem, …)`. Each element
/// is rendered in the same comma-list shape the surrounding fn
/// signature uses, so the whole layout cascades to multi-line
/// when its enclosing list does.
fn doc_param_pattern_tuple(pat: &crate::ast::ParamPattern) -> Doc {
    let elems: Vec<Doc> = pat.elems.iter().map(doc_param_pattern_elem).collect();
    comma_list(text("("), elems, text(")"))
}

fn doc_param_pattern_elem(e: &crate::ast::ParamPatternElem) -> Doc {
    use crate::ast::ParamPatternElem;
    match e {
        ParamPatternElem::Bind {
            name,
            name_span,
            ty,
        } => {
            // A written `_` can distinguish a pattern from a type;
            // only the binder-span inference node denotes an omitted type.
            if matches!(ty, crate::ast::Type::Infer { .. }) && ty.span() == *name_span {
                text(name.clone())
            } else {
                concat(text(format!("{}: ", name)), doc_type(ty))
            }
        }
        ParamPatternElem::Tuple(inner) => doc_param_pattern_tuple(inner),
        ParamPatternElem::BindTuple { name, inner, .. } => {
            concat(text(format!("{}: ", name)), doc_param_pattern_tuple(inner))
        }
    }
}

// ---- types -------------------------------------------------------------

/// Operator of an `&` / `|` chain. Used by the chain printer to
/// decide when an outer paren-wrap is load-bearing (mixing rule)
/// and to walk the right-associated tree into a flat item list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChainOp {
    Amp,
    Pipe,
}

impl ChainOp {
    fn separator(self) -> &'static str {
        match self {
            ChainOp::Amp => " & ",
            ChainOp::Pipe => " | ",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            ChainOp::Amp => "& ",
            ChainOp::Pipe => "| ",
        }
    }
}

/// Flatten a right-associated same-op chain rooted at `t`. The
/// parser folds chains into right-associated `Product` / `Sum`
/// nodes, so `A & B & C` lives in the AST as
/// `Product(A, Product(B, C))`; this walks the right child while
/// it has the same operator and collects the items.
fn flatten_chain(t: &Type, op: ChainOp) -> Vec<&Type> {
    let mut items = Vec::new();
    let mut cur = t;
    loop {
        match (cur, op) {
            (Type::Product { left, right, .. }, ChainOp::Amp) => {
                items.push(left.as_ref());
                cur = right.as_ref();
            }
            (Type::Sum { left, right, .. }, ChainOp::Pipe) => {
                items.push(left.as_ref());
                cur = right.as_ref();
            }
            _ => {
                items.push(cur);
                return items;
            }
        }
    }
}

/// Render a Type as the child of an operator chain. Wraps the
/// rendering in parens when the load-bearing rule applies:
///
/// - A *same-op* `Product` / `Sum` child: it must be **left-
///   leaning**, since the flattener only walks the right
///   spine; without parens we would print it indistinguishably
///   from the right-leaning chain (`(A & B) & C` vs `A & B & C`),
///   collapsing two distinct AST shapes.
/// - A *different-op* chain (mixing): `(A & B) | C`.
/// - A function-type or `Forall` child of any chain:
///   `(A -> B) | C` — without surrounding parens, the arrow
///   would reparse as the chain's tail.
fn doc_type_chain_child(t: &Type, parent_op: ChainOp) -> Doc {
    let needs_parens = matches!(
        (t, parent_op),
        (Type::Product { .. }, _)
            | (Type::Sum { .. }, _)
            | (Type::Function { .. }, _)
            | (Type::Forall { .. }, _)
    );
    if needs_parens {
        concat_all([text("("), doc_type(t), text(")")])
    } else {
        doc_type(t)
    }
}

/// Build a chain doc from rendered items (≥2). Flat single-line:
/// `A & B & C`. Broken multi-line when the enclosing group can't
/// fit: each item on its own line prefixed with `& ` (or `| `) at
/// the surrounding nest's indent — the leading-operator A1
/// analogue. Mirrors `comma_list`'s flat-vs-broken structure.
///
fn doc_chain(items: Vec<Doc>, op: ChainOp) -> Doc {
    doc_chain_inner(items, op, false)
}

/// Like [`doc_chain`] but parenthesizes the **broken** (leading-
/// operator) layout. The broken form begins with a leading operator
/// (`| A` / `& A`), which Kio' admits only inside parentheses (see the
/// `parse_chain_layouts` golden). Most positions that hold a chain are
/// self-delimiting — a `type` body (`= … ;`), a function return — so
/// the bare leading-operator layout round-trips there and needs no
/// parens. A chain at a *bare* position (a call argument, an intrinsic
/// form's type operand) has no such delimiter, so its broken form must
/// supply the parens. The flat form carries no leading operator and is
/// never parenthesized, so a chain that fits on one line is unchanged.
fn doc_chain_paren_when_broken(items: Vec<Doc>, op: ChainOp) -> Doc {
    doc_chain_inner(items, op, true)
}

fn doc_chain_inner(items: Vec<Doc>, op: ChainOp, paren_broken: bool) -> Doc {
    debug_assert!(items.len() >= 2);
    let flat = join(text(op.separator()), items.clone());
    let mut broken = Doc::Empty;
    let mut first = true;
    for item in items.into_iter() {
        if !first {
            broken = concat(broken, hardline());
        }
        broken = concat(broken, concat(text(op.prefix()), item));
        first = false;
    }
    let broken = if paren_broken {
        concat_all([text("("), broken, text(")")])
    } else {
        broken
    };
    group(flat_alt(flat, broken))
}

/// Render a `Sum` / `Product` type at a **bare** position (a call
/// argument or intrinsic-form type operand), where a broken chain's
/// leading operator needs enclosing parens to round-trip. Falls back to
/// the ordinary [`doc_type`] for non-chain types.
fn doc_type_bare_position(t: &Type) -> Doc {
    match t {
        Type::Product { .. } => {
            let items = flatten_chain(t, ChainOp::Amp);
            let docs: Vec<Doc> = items
                .iter()
                .map(|item| doc_type_chain_child(item, ChainOp::Amp))
                .collect();
            doc_chain_paren_when_broken(docs, ChainOp::Amp)
        }
        Type::Sum { .. } => {
            let items = flatten_chain(t, ChainOp::Pipe);
            let docs: Vec<Doc> = items
                .iter()
                .map(|item| doc_type_chain_child(item, ChainOp::Pipe))
                .collect();
            doc_chain_paren_when_broken(docs, ChainOp::Pipe)
        }
        _ => doc_type(t),
    }
}

fn collect_forall_prefix(mut cur: &Type) -> (Vec<&TypeParam>, &Type) {
    let mut params = Vec::new();
    while let Type::Forall { param, body, .. } = cur {
        params.push(param);
        cur = body.as_ref();
    }
    (params, cur)
}

fn doc_paren_type(t: &Type) -> Doc {
    concat_all([text("("), doc_type(t), text(")")])
}

fn doc_function_domain(t: &Type) -> Doc {
    match t {
        Type::Product { .. } | Type::Sum { .. } | Type::Function { .. } | Type::Forall { .. } => {
            doc_paren_type(t)
        }
        _ => doc_type(t),
    }
}

fn doc_forall_type(t: &Type) -> Doc {
    let (params, body) = collect_forall_prefix(t);
    let binders = concat_all(
        params
            .into_iter()
            .map(|param| text(type_binder_text(param))),
    );
    concat_all([binders, text(" "), doc_type(body)])
}

fn doc_type(t: &Type) -> Doc {
    match t {
        Type::Path { segments, args, .. } => {
            let head = text(type_path_surface_str(segments));
            if args.is_empty() {
                head
            } else {
                let inner = args.iter().map(doc_type).collect::<Vec<_>>();
                let args_trivia: Vec<Vec<crate::pass::lexer::Trivia>> = args
                    .iter()
                    .map(|a| a.meta().leading_trivia.clone())
                    .collect();
                let last_trailing = args
                    .last()
                    .map(|a| a.meta().trailing_trivia.as_slice())
                    .unwrap_or(&[]);
                // Expression-shape (use-site) type-application
                // argument list: stays single-line if it fits within
                // the 100-column budget, otherwise breaks into A1.
                concat(
                    head,
                    doc_comma_list_with_trivia(
                        text("("),
                        inner,
                        &args_trivia,
                        last_trailing,
                        text(")"),
                    ),
                )
            }
        }
        Type::Unit { .. } => text("."),
        Type::Bottom { .. } => text("!"),
        Type::Function { param, ret, .. } => concat(
            doc_function_domain(param),
            doc_chain_body_layout(ret, doc_type(ret), " -> ", " ->", empty(), empty()),
        ),
        Type::Product {
            left: _, right: _, ..
        } => {
            let items = flatten_chain(t, ChainOp::Amp);
            let docs: Vec<Doc> = items
                .iter()
                .map(|item| doc_type_chain_child(item, ChainOp::Amp))
                .collect();
            doc_chain(docs, ChainOp::Amp)
        }
        Type::Sum {
            left: _, right: _, ..
        } => {
            let items = flatten_chain(t, ChainOp::Pipe);
            let docs: Vec<Doc> = items
                .iter()
                .map(|item| doc_type_chain_child(item, ChainOp::Pipe))
                .collect();
            doc_chain(docs, ChainOp::Pipe)
        }
        Type::LabelSugar { labels, .. } => {
            let label_trivia: Vec<Vec<crate::pass::lexer::Trivia>> = labels
                .iter()
                .map(|l| l.meta.leading_trivia.clone())
                .collect();
            doc_label_sugar(labels, &label_trivia)
        }
        Type::Forall { .. } => doc_forall_type(t),
        Type::Infer { .. } => text("_".to_owned()),
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn doc_label_sugar(labels: &[LabelSugarLabel], label_trivia: &[Vec<Trivia>]) -> Doc {
    let label_docs: Vec<Doc> = labels
        .iter()
        .map(|label| {
            let head = text(label.label.clone());
            match &label.payload {
                None => head,
                Some(payload) => concat(head, concat(text(": "), doc_type(payload))),
            }
        })
        .collect();
    doc_comma_list_with_trivia(text("{"), label_docs, label_trivia, &[], text("}"))
}

// ---- expressions -------------------------------------------------------

fn doc_expr(e: &Expr) -> Doc {
    if let Some(doc) = doc_source_existential_let(e) {
        return doc;
    }
    match e {
        Expr::Path { segments, .. } => text(segments.join(".")),
        Expr::Call { callee, args, .. } => {
            let arg_docs: Vec<Doc> = args.iter().map(doc_call_arg).collect();
            let arg_trivia = call_arg_trivia(args);
            let head = doc_expr_for_call_callee(callee);
            doc_call(head, arg_docs, &arg_trivia, call_arg_last_trailing(args))
        }
        Expr::RecCall {
            modes,
            callee,
            args,
            ..
        } => {
            let arg_docs: Vec<Doc> = args.iter().map(doc_call_arg).collect();
            let arg_trivia = call_arg_trivia(args);
            let head = if modes.is_empty() {
                format!("rec {}", callee.name)
            } else {
                let mut modes = modes.clone();
                modes.sort_by_key(|mode| mode.fmt_rank());
                let names = modes
                    .into_iter()
                    .map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("rec({names}) {}", callee.name)
            };
            doc_call(
                text(head),
                arg_docs,
                &arg_trivia,
                call_arg_last_trailing(args),
            )
        }
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            let mut parts: Vec<Doc> = Vec::with_capacity(4);
            parts.push(text("."));
            parts.push(doc_signature(sig));
            if let Some(ty) = ret_ty {
                parts.push(text(" -> "));
                parts.push(doc_type(ty));
            }
            // Body emits with its own leading-trivia honored — a
            // comment captured between the fn body's `{` and the
            // first expression flows onto `body.meta.leading_trivia`
            // and must emit above the body so the round-trip
            // preserves it. A comment dangling before the closing `}`
            // rides `body.meta.trailing_trivia` and emits before it.
            parts.push(group(doc_block_body_expr(body, true)));
            concat_all(parts)
        }
        Expr::FnPlaceholder { stem, body, .. } => concat(
            text(format!(".{}.", stem.name)),
            group(doc_block_body_expr(body, true)),
        ),
        Expr::Let {
            name,
            ty,
            pattern,
            value,
            body,
            meta,
            ..
        } => doc_let(
            name,
            ty.as_ref(),
            pattern.as_ref(),
            value,
            body,
            &meta.leading_trivia,
        ),
        Expr::RowLet {
            entries,
            value,
            body,
            meta,
            ..
        } => doc_row_let(entries, value, body, &meta.leading_trivia),
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => doc_seq(value, body, &meta.leading_trivia),
        Expr::Unit { .. } => text("()"),
        Expr::StrLit {
            value, annotation, ..
        } => concat(
            doc_string_literal(value),
            doc_literal_annotation(annotation),
        ),
        Expr::IntLit {
            digits, annotation, ..
        } => concat(text(digits.clone()), doc_literal_annotation(annotation)),
        Expr::FloatLit {
            digits, annotation, ..
        } => {
            // Canonical float emission: lowercase `e`, drop leading
            // `+` after `e`. The lexer strips `_` separators already.
            let canonical: String = digits
                .chars()
                .filter(|&c| c != '+')
                .map(|c| if c == 'E' { 'e' } else { c })
                .collect();
            concat(text(canonical), doc_literal_annotation(annotation))
        }
        Expr::BoolLit {
            value, annotation, ..
        } => concat(
            text(if *value { ".t" } else { ".f" }),
            doc_literal_annotation(annotation),
        ),
        Expr::Tuple { items, .. } => {
            let item_docs: Vec<Doc> = items.iter().map(doc_expr).collect();
            let item_trivia: Vec<Vec<crate::pass::lexer::Trivia>> = items
                .iter()
                .map(|item| item.meta().leading_trivia.clone())
                .collect();
            let last_trailing = items
                .last()
                .map(|item| item.meta().trailing_trivia.as_slice())
                .unwrap_or(&[]);
            doc_comma_list_with_trivia(text("("), item_docs, &item_trivia, last_trailing, text(")"))
        }
        Expr::LabelValue { labels, .. } => {
            let label_docs: Vec<Doc> = labels
                .iter()
                .map(|label| doc_label_payload(&label.label, &label.value))
                .collect();
            let label_trivia: Vec<Vec<crate::pass::lexer::Trivia>> = labels
                .iter()
                .map(|label| label.meta.leading_trivia.clone())
                .collect();
            let last_trailing = labels
                .last()
                .map(|label| label.meta.trailing_trivia.as_slice())
                .unwrap_or(&[]);
            doc_comma_list_with_trivia(
                text("{"),
                label_docs,
                &label_trivia,
                last_trailing,
                text("}"),
            )
        }
        Expr::Elaborator { call, .. } => match call {
            ElaboratorCall::FieldAccess { receiver, labels } => doc_field_access(receiver, labels),
            ElaboratorCall::FieldUpdate { receiver, updates } => {
                doc_field_update(receiver, updates)
            }
        },
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { name, args, .. } => doc_user_elaborator_form(name, args),
        Expr::BlockCall {
            head,
            prefix,
            blocks,
            ..
        } => {
            let mut parts = vec![text(format!("{}!", head.name))];
            if let [value] = prefix.as_slice()
                && !has_comments(&value.meta().leading_trivia)
                && !has_comments(&value.meta().trailing_trivia)
                && block_prefix_starts_bare(value)
                && !block_prefix_has_exposed_block(value)
            {
                parts.extend([text(" "), doc_expr(value)]);
            } else if let [Expr::Unit { meta, .. }] = prefix.as_slice() {
                let mut trivia = meta.leading_trivia.clone();
                trivia.extend(meta.trailing_trivia.clone());
                parts.push(doc_comma_list_with_trivia(
                    text("("),
                    Vec::new(),
                    &[],
                    &trivia,
                    text(")"),
                ));
            } else if !prefix.is_empty() {
                parts.push(doc_comma_list_with_trivia(
                    text("("),
                    prefix.iter().map(doc_expr).collect(),
                    &prefix
                        .iter()
                        .map(|value| value.meta().leading_trivia.clone())
                        .collect::<Vec<_>>(),
                    prefix.last().unwrap().meta().trailing_trivia.as_slice(),
                    text(")"),
                ));
            }
            for (block_index, block) in blocks.iter().enumerate() {
                if has_comments(&block.leading) {
                    parts.extend([hardline(), doc_inline_leading_trivia(&block.leading)]);
                }
                if let Some(label) = &block.label {
                    parts.push(text(format!(" {}", label.name)));
                }
                let item_docs = block
                    .items
                    .iter()
                    .map(|item| match item {
                        crate::ast::NeutralItem::Expression { value, .. } => {
                            doc_expr_with_own_meta_trivia(value)
                        }
                        crate::ast::NeutralItem::Binding {
                            name,
                            ty,
                            pattern,
                            value,
                            bind,
                            meta,
                            ..
                        } => concat_all([
                            doc_inline_leading_trivia(&meta.leading_trivia),
                            text("let "),
                            doc_neutral_let_binder(name, ty.as_ref(), pattern.as_ref()),
                            text(if *bind { " <- " } else { " = " }),
                            doc_expr_with_own_meta_trivia(value),
                            if has_comments(&meta.trailing_trivia) {
                                concat(hardline(), doc_inline_leading_trivia(&meta.trailing_trivia))
                            } else {
                                empty()
                            },
                        ]),
                        crate::ast::NeutralItem::RowBinding {
                            entries,
                            value,
                            meta,
                        } => concat_all([
                            doc_inline_leading_trivia(&meta.leading_trivia),
                            doc_comma_list_with_trivia(
                                text("let .({"),
                                entries.iter().map(doc_row_let_entry).collect(),
                                &entries
                                    .iter()
                                    .map(|entry| entry.meta.leading_trivia.clone())
                                    .collect::<Vec<_>>(),
                                entries
                                    .last()
                                    .map_or(&[], |entry| entry.meta.trailing_trivia.as_slice()),
                                text("}) = "),
                            ),
                            doc_expr_with_own_meta_trivia(value),
                            if has_comments(&meta.trailing_trivia) {
                                concat(hardline(), doc_inline_leading_trivia(&meta.trailing_trivia))
                            } else {
                                empty()
                            },
                        ]),
                        crate::ast::NeutralItem::ExistentialBinding {
                            type_params,
                            name,
                            pattern,
                            value,
                            meta,
                            ..
                        } => concat_all([
                            doc_inline_leading_trivia(&meta.leading_trivia),
                            text("let .("),
                            doc_existential_params(type_params),
                            text(" "),
                            pattern
                                .as_ref()
                                .map_or_else(|| text(name.clone()), doc_param_pattern_tuple),
                            text(") = "),
                            doc_expr_with_own_meta_trivia(value),
                            if has_comments(&meta.trailing_trivia) {
                                concat(hardline(), doc_inline_leading_trivia(&meta.trailing_trivia))
                            } else {
                                empty()
                            },
                        ]),
                    })
                    .collect::<Vec<_>>();
                let mut content = Vec::new();
                for (index, item) in item_docs.into_iter().enumerate() {
                    content.push(item);
                    if index + 1 < block.items.len()
                        || !matches!(
                            block.items[index],
                            crate::ast::NeutralItem::Expression { .. }
                        )
                    {
                        content.push(text(";"));
                    }
                    if index + 1 < block.items.len() {
                        content.push(hardline());
                    }
                }
                let elide = block_index + 1 == blocks.len()
                    && block.label.is_some()
                    && !has_comments(&block.leading)
                    && !has_comments(&block.trailing)
                    && block
                        .separators
                        .iter()
                        .all(|separator| !has_comments(&separator.leading))
                    && matches!(
                        block.items.as_slice(),
                        [crate::ast::NeutralItem::Expression {
                            value: Expr::BlockCall { meta, .. },
                            ..
                        }] if !has_comments(&meta.leading_trivia)
                            && !has_comments(&meta.trailing_trivia)
                    );
                if elide {
                    parts.extend([text(" "), concat_all(content)]);
                } else if block.items.is_empty() && !has_comments(&block.trailing) {
                    parts.push(text(" {}"));
                } else {
                    parts.push(doc_block_body_with_trailing(
                        concat_all(content),
                        &block.trailing,
                        false,
                    ));
                }
            }
            concat_all(parts)
        }
        Expr::Ufcs { flavor, .. } if flavor.callee_on_right() => doc_ufcs_chain(e),
        Expr::Ufcs { .. } => doc_left_call_splice(e),
        Expr::OpChain { kind, .. } => doc_op_chain(kind),
        // Statically uninhabited at `Surface`: structural recovery first
        // produces these variants at `Enriched`, and resolution lowering
        // carries them through to `Routed`.
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn block_prefix_starts_bare(expr: &Expr) -> bool {
    match expr {
        Expr::Path { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. }
        | Expr::RecCall { .. }
        | Expr::UserElaborator { .. } => true,
        Expr::Call { callee, .. } => {
            !call_callee_needs_parens(callee) && block_prefix_starts_bare(callee)
        }
        Expr::Elaborator { call, .. } => {
            let receiver = match call {
                ElaboratorCall::FieldAccess { receiver, .. }
                | ElaboratorCall::FieldUpdate { receiver, .. } => receiver,
            };
            !matches!(receiver.as_ref(), Expr::OpChain { .. }) && block_prefix_starts_bare(receiver)
        }
        Expr::Ufcs {
            receiver, flavor, ..
        } => {
            !flavor.callee_on_right()
                || (!matches!(receiver.as_ref(), Expr::OpChain { .. })
                    && block_prefix_starts_bare(receiver))
        }
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, slots },
            ..
        } => {
            !matches!(pattern.first(), Some(OpPart::Token { .. }))
                && slots.first().is_some_and(|first| {
                    !op_chain_slot_needs_parens(first, &pattern[0], pattern)
                        && block_prefix_starts_bare(first)
                })
        }
        _ => false,
    }
}

// Only follow edges printed without an expression boundary. Call arguments,
// function bodies and explicit operator continuations already own their braces.
fn block_prefix_has_exposed_block(expr: &Expr) -> bool {
    match expr {
        Expr::BlockCall { .. } => true,
        Expr::Call { callee, .. } => {
            !call_callee_needs_parens(callee) && block_prefix_has_exposed_block(callee)
        }
        Expr::Elaborator { call, .. } => {
            let receiver = match call {
                ElaboratorCall::FieldAccess { receiver, .. }
                | ElaboratorCall::FieldUpdate { receiver, .. } => receiver,
            };
            !matches!(receiver.as_ref(), Expr::OpChain { .. })
                && block_prefix_has_exposed_block(receiver)
        }
        Expr::Ufcs {
            receiver, flavor, ..
        } => {
            if flavor.callee_on_right() {
                !matches!(receiver.as_ref(), Expr::OpChain { .. })
                    && block_prefix_has_exposed_block(receiver)
            } else {
                matches!(receiver.as_ref(), Expr::Call { .. })
                    && block_prefix_has_exposed_block(receiver)
            }
        }
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, slots },
            ..
        } => {
            let mut slots = slots.iter();
            pattern.iter().enumerate().any(|(index, part)| {
                if matches!(part, OpPart::Token { .. }) {
                    return false;
                }
                let slot = slots.next().expect("operator slot expression");
                !matches!(pattern.get(index + 1), Some(OpPart::Token { .. }))
                    && !op_chain_slot_needs_parens(slot, part, pattern)
                    && block_prefix_has_exposed_block(slot)
            })
        }
        _ => false,
    }
}

fn doc_source_existential_let(expr: &Expr) -> Option<Doc> {
    let opening = crate::pass::parser::source_existential_let(expr)?;
    let type_params: Option<Vec<_>> = opening
        .type_params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(param) => Some(param.clone()),
            SignatureParam::Value(_) => None,
        })
        .collect();
    Some(doc_let_with(
        concat_all([
            text("let .("),
            doc_existential_params(&type_params?),
            text(" "),
            opening.binder.pattern.as_ref().map_or_else(
                || text(opening.binder.name.clone()),
                doc_param_pattern_tuple,
            ),
            text(") ="),
        ]),
        opening.value,
        opening.body,
        &[],
        &doc_expr_no_own_trivia,
        &doc_expr_with_own_meta_trivia,
    ))
}

fn doc_expr_for_call_callee(callee: &Expr) -> Doc {
    let rendered = doc_expr(callee);
    if call_callee_needs_parens(callee) {
        concat_all([text("("), rendered, text(")")])
    } else {
        rendered
    }
}

fn call_callee_needs_parens(callee: &Expr) -> bool {
    let bare_literal = matches!(
        callee,
        Expr::StrLit {
            annotation: None,
            ..
        } | Expr::IntLit {
            annotation: None,
            ..
        } | Expr::FloatLit {
            annotation: None,
            ..
        } | Expr::BoolLit {
            annotation: None,
            ..
        }
    );
    let left_splice = matches!(callee, Expr::Ufcs { flavor, .. } if !flavor.callee_on_right());
    let normal_operator = matches!(
        callee,
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { .. },
            ..
        }
    );
    bare_literal || left_splice || normal_operator
}

/// Render an expression that owns a following postfix receiver suffix.
/// Operator chains need grouping here: without it, the suffix becomes part of
/// the chain's final operand when reparsed. This boundary is shared by field
/// access/update and receiver-first UFCS.
fn doc_expr_for_postfix_receiver(receiver: &Expr) -> Doc {
    let rendered = doc_expr(receiver);
    if matches!(receiver, Expr::OpChain { .. }) {
        concat_all([text("("), rendered, text(")")])
    } else {
        rendered
    }
}

/// Render an operator-chain placeholder back to its source-level
/// form. The chain is self-describing — the `Normal` variant
/// carries the matched op pattern (slots interleaved with
/// op-tokens) and the parsed operand expressions; the `Variadic`
/// variant carries the OPEN / CLOSE delimiters and element expressions. The
/// pretty-printer never consults the operator scope.
fn doc_op_chain(kind: &crate::ast::OpChainKind) -> Doc {
    match kind {
        crate::ast::OpChainKind::Normal { pattern, slots } => doc_normal_op_chain(pattern, slots),
        crate::ast::OpChainKind::Variadic {
            open_tokens,
            close_tokens,
            elements,
            empty_trivia,
        } => doc_variadic_op_chain(open_tokens, close_tokens, elements, empty_trivia),
    }
}

fn doc_normal_op_chain(pattern: &[OpPart], slots: &[Expr]) -> Doc {
    let mut segments = Vec::new();
    append_normal_op_segments(pattern, slots, &mut segments);
    let flat = join(text(" "), segments.clone());
    if segments.len() <= 1 {
        return flat;
    }

    let mut segments = segments.into_iter();
    let first = segments
        .next()
        .unwrap_or_else(|| unreachable!("a multi-segment operator chain has a first segment"));
    let mut rest = Vec::new();
    for segment in segments {
        rest.push(hardline());
        rest.push(segment);
    }
    let broken = concat(first, nest(2, concat_all(rest)));
    align(group(flat_alt(flat, broken)))
}

fn append_normal_op_segments(pattern: &[OpPart], slots: &[Expr], segments: &mut Vec<Doc>) {
    let pattern_slots = pattern
        .iter()
        .filter(|part| {
            matches!(
                part,
                OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. }
            )
        })
        .count();
    assert_eq!(
        pattern_slots,
        slots.len(),
        "normal operator chain must carry exactly one expression per pattern slot"
    );

    let mut pending_tokens = Vec::new();
    let mut slot_idx = 0usize;
    for (part_idx, part) in pattern.iter().enumerate() {
        match part {
            OpPart::Token { content, .. } => pending_tokens.push(text(content.clone())),
            OpPart::SlotPlain { .. } | OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. } => {
                let slot = &slots[slot_idx];
                let same_pattern_child = match (part, slot) {
                    (
                        OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. },
                        Expr::OpChain {
                            kind:
                                crate::ast::OpChainKind::Normal {
                                    pattern: child_pattern,
                                    slots: child_slots,
                                },
                            ..
                        },
                    ) if op_patterns_equal(pattern, child_pattern) => {
                        Some((child_pattern.as_slice(), child_slots.as_slice()))
                    }
                    _ => None,
                };

                if let Some((child_pattern, child_slots)) = same_pattern_child {
                    let tokens = std::mem::take(&mut pending_tokens);
                    if tokens.is_empty() {
                        append_normal_op_segments(child_pattern, child_slots, segments);
                    } else if slot_idx == 0 {
                        // A recursive prefix application contributes one leading
                        // operator segment before the child application; joining
                        // the run to the child's first segment would hide that
                        // application boundary in the broken layout.
                        segments.push(doc_op_segment(tokens, None));
                        append_normal_op_segments(child_pattern, child_slots, segments);
                    } else {
                        let child_start = segments.len();
                        append_normal_op_segments(child_pattern, child_slots, segments);
                        if let Some(child_first) = segments.get(child_start).cloned() {
                            segments[child_start] = doc_op_segment(tokens, Some(child_first));
                        } else {
                            segments.push(doc_op_segment(tokens, None));
                        }
                    }
                } else {
                    let slot_doc = doc_op_chain_slot(slot, part, pattern);
                    segments.push(doc_op_segment(
                        std::mem::take(&mut pending_tokens),
                        Some(slot_doc),
                    ));
                }
                let trailing = slot.meta().trailing_trivia.as_slice();
                if matches!(pattern.get(part_idx + 1), Some(OpPart::Token { .. }))
                    && has_comments(trailing)
                {
                    // A flattened recursive operand still owns its boundary
                    // comments after its last segment. The hardline also keeps
                    // the following operator out of the final line comment.
                    let last = segments
                        .last_mut()
                        .expect("operator operand contributes a segment");
                    *last = concat_all([last.clone(), hardline(), doc_trailing_comments(trailing)]);
                }
                slot_idx += 1;
            }
        }
    }
    if !pending_tokens.is_empty() {
        segments.push(doc_op_segment(pending_tokens, None));
    }
}

fn doc_op_segment(tokens: Vec<Doc>, value: Option<Doc>) -> Doc {
    let has_tokens = !tokens.is_empty();
    let token_run = join(text(" "), tokens);
    match (has_tokens, value) {
        (false, Some(value)) => value,
        (true, Some(value)) => concat_all([token_run, text(" "), align(value)]),
        (_, None) => token_run,
    }
}

fn doc_variadic_op_chain(
    open_tokens: &[String],
    close_tokens: &[String],
    elements: &[Expr],
    empty_trivia: &[Trivia],
) -> Doc {
    let item_docs = elements.iter().map(doc_expr).collect();
    let item_trivia: Vec<_> = elements
        .iter()
        .map(|item| item.meta().leading_trivia.clone())
        .collect();
    let last_trailing = elements
        .last()
        .map(|item| item.meta().trailing_trivia.as_slice())
        .unwrap_or(empty_trivia);
    doc_comma_list_with_trivia_and_padding(
        text(open_tokens.join(" ")),
        item_docs,
        &item_trivia,
        last_trailing,
        text(close_tokens.join(" ")),
        true,
    )
}

/// Render an op-chain slot. Wrap in load-bearing parens when the slot
/// is itself an op-chain that the surrounding slot position wouldn't
/// otherwise admit, per `specs/language.md` § Operators:
///
/// - `SlotPlain` (non-lenient `_`): atom or paren-expr only — any
///   inner op-chain needs outer parens.
/// - `SlotPlain` lenient (declared inside `( … )`): admits a
///   single-op chain at the use site — no outer parens needed.
/// - `SlotRecursive` (`__`): drives same-op chain continuation on
///   re-parse. Same pattern as parent ⇒ no parens; different
///   pattern ⇒ needs parens (recursion can't span). Lenient widens
///   to admit any single-op chain — no parens.
/// - `SlotGreedy` (`___`): admits any single-op chain — no parens.
fn doc_op_chain_slot(
    slot: &Expr,
    slot_part: &crate::ast::OpPart,
    parent_pattern: &[crate::ast::OpPart],
) -> Doc {
    if op_chain_slot_needs_parens(slot, slot_part, parent_pattern) {
        concat_all([text("("), doc_expr(slot), text(")")])
    } else {
        doc_expr(slot)
    }
}

fn op_chain_slot_needs_parens(
    slot: &Expr,
    slot_part: &crate::ast::OpPart,
    parent_pattern: &[crate::ast::OpPart],
) -> bool {
    use crate::ast::OpPart;
    let inner_pattern = match slot {
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, .. },
            ..
        } => Some(pattern),
        _ => None,
    };
    match (slot_part, inner_pattern) {
        (_, None) => false,
        (OpPart::SlotGreedy { .. }, _) => false,
        (OpPart::SlotPlain { lenient: true, .. }, _)
        | (OpPart::SlotRecursive { lenient: true, .. }, _) => false,
        (OpPart::SlotPlain { lenient: false, .. }, _) => true,
        (OpPart::SlotRecursive { lenient: false, .. }, Some(inner)) => {
            !op_patterns_equal(inner, parent_pattern)
        }
        (OpPart::Token { .. }, _) => false,
    }
}

/// Two op-patterns are equal for round-trip purposes when their
/// shape (token contents, slot kinds in order) matches. `lenient`
/// and `quoted` flags affect declaration-side round-trip but not
/// use-site shape, so they're not compared.
fn op_patterns_equal(a: &[crate::ast::OpPart], b: &[crate::ast::OpPart]) -> bool {
    use crate::ast::OpPart;
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
        (OpPart::Token { content: c1, .. }, OpPart::Token { content: c2, .. }) => c1 == c2,
        (OpPart::SlotPlain { .. }, OpPart::SlotPlain { .. }) => true,
        (OpPart::SlotRecursive { .. }, OpPart::SlotRecursive { .. }) => true,
        (OpPart::SlotGreedy { .. }, OpPart::SlotGreedy { .. }) => true,
        _ => false,
    })
}

/// Build a `Vec<Vec<Trivia>>` parallel to `args` by cloning each
/// arg's `meta.leading_trivia`.
fn call_arg_trivia(args: &[CallArg]) -> Vec<Vec<Trivia>> {
    args.iter()
        .map(|a| a.meta().leading_trivia.clone())
        .collect()
}

/// The last arg's closing-boundary trailing trivia (the comment run
/// the parser stashed before the `)`), or an empty slice when the
/// list is empty. The comma-list emitter reads it to keep a
/// last-element trailing / dangling comment in place.
fn call_arg_last_trailing(args: &[CallArg]) -> &[Trivia] {
    args.last()
        .map(|a| a.meta().trailing_trivia.as_slice())
        .unwrap_or(&[])
}

fn doc_call_arg(arg: &CallArg) -> Doc {
    match arg {
        // A type passed as a call argument (an intrinsic form's type
        // operand — `__if_then_else__(T, …)`, `__left__(T, v)`, …) sits
        // at a bare position: the comma-list provides no delimiter, so a
        // wide `Sum` / `Product` chain that breaks to the leading-operator
        // layout must parenthesize itself or its leading operator would
        // reparse as a stray operator at the start of a type expression.
        // `doc_type_bare_position` adds those parens to the broken layout
        // only — a flat chain stays bare.
        CallArg::Type(t) => doc_type_bare_position(t),
        CallArg::Value(v) => doc_expr(v),
    }
}

/// Pretty-print a UFCS chain `receiver.>f(...).>>g(...).>h(...)`
/// per `specs/style.md` § UFCS chains. Width-driven, all-or-
/// nothing: either every segment is flat on one line, or the
/// innermost receiver lives on its own line and each `.>callee
/// (args)` lands on its own line at +2 indent.
///
/// `top` must be the outermost `Expr::Ufcs` of the chain (the
/// node the caller's `doc_expr` recursion landed on). The walk
/// follows the receiver chain to the innermost non-Ufcs node and
/// collects segments left-to-right in source order. Inner Ufcs
/// nodes are not re-rendered as chains — only this top-level call
/// drives the layout decision.
///
/// Zero-arg segments (`r.>f` with empty `args`) emit without
/// parentheses in either layout. Wide args within a single
/// segment are handled independently by `doc_call`'s own A1
/// leading-comma break — no new rule here.
fn doc_ufcs_chain(top: &Expr) -> Doc {
    struct Segment<'a> {
        callee_segments: &'a [crate::ast::PathSegment],
        args: &'a [CallArg],
        arg_trivia: Vec<Vec<Trivia>>,
        flavor: crate::ast::UfcsFlavor,
        /// True when this segment is the elaborator-UFCS form
        /// (`r.>iso!(T)` etc.). The pretty-printer appends `!`
        /// after the callee name to round-trip the user-authored
        /// spelling.
        bang: bool,
        /// Comments that trailed the segment's receiver (the
        /// preceding chain step, or the innermost receiver for the
        /// first segment). The lexer attaches a receiver-trailing
        /// `// comment` to the following `.>` token's leading trivia;
        /// the parser stashes it on the receiver's
        /// `meta.trailing_trivia`, which we read here so the chain
        /// layout keeps it instead of dropping it (§ Comments).
        leading_trivia: &'a [Trivia],
    }
    let mut segments: Vec<Segment> = Vec::new();
    let mut cur = top;
    while let Expr::Ufcs {
        receiver,
        callee_segments,
        args,
        flavor,
        bang,
        ..
    } = cur
    {
        if !flavor.callee_on_right() {
            break;
        }
        segments.push(Segment {
            callee_segments,
            args,
            arg_trivia: call_arg_trivia(args),
            flavor: *flavor,
            bang: bang.is_some(),
            leading_trivia: receiver.meta().trailing_trivia.as_slice(),
        });
        cur = receiver.as_ref();
    }
    segments.reverse();
    let innermost = cur;
    let seg_doc = |seg: &Segment| -> Doc {
        let callee = text(seg.callee_segments.join("."));
        let mut head = concat(text(seg.flavor.token()), callee);
        if seg.bang {
            head = concat(head, text("!"));
        }
        if seg.args.is_empty() {
            head
        } else {
            let arg_docs: Vec<Doc> = seg.args.iter().map(doc_call_arg).collect();
            doc_call(
                head,
                arg_docs,
                &seg.arg_trivia,
                call_arg_last_trailing(seg.args),
            )
        }
    };
    let receiver_doc = doc_expr_for_postfix_receiver(innermost);
    let seg_docs: Vec<Doc> = segments.iter().map(seg_doc).collect();
    // A receiver-trailing comment can't sit inline before the next
    // `.>`, so any captured comment forces the broken layout (the
    // cascading-break rule, § Comments / § UFCS chains).
    let has_step_comments = segments.iter().any(|seg| has_comments(seg.leading_trivia));
    let mut broken_inner: Vec<Doc> = Vec::with_capacity(seg_docs.len() * 3);
    for (seg, sd) in segments.iter().zip(seg_docs.iter()) {
        broken_inner.push(hardline());
        if has_comments(seg.leading_trivia) {
            broken_inner.push(doc_inline_leading_trivia(seg.leading_trivia));
        }
        broken_inner.push(sd.clone());
    }
    let broken = concat(receiver_doc.clone(), nest(2, concat_all(broken_inner)));
    if has_step_comments {
        return broken;
    }
    let mut flat_parts: Vec<Doc> = Vec::with_capacity(seg_docs.len() + 1);
    flat_parts.push(receiver_doc);
    for sd in seg_docs {
        flat_parts.push(sd);
    }
    let flat = concat_all(flat_parts);
    group(flat_alt(flat, broken))
}

fn doc_left_call_splice(top: &Expr) -> Doc {
    let Expr::Ufcs {
        receiver,
        callee_segments,
        args,
        flavor,
        bang,
        ..
    } = top
    else {
        unreachable!("doc_left_call_splice called on non-UFCS expression")
    };
    let mut callee = text(callee_segments.join("."));
    if bang.is_some() {
        callee = concat(callee, text("!"));
    }
    let lhs = if args.is_empty() {
        callee
    } else {
        let arg_docs: Vec<Doc> = args.iter().map(doc_call_arg).collect();
        let arg_trivia = call_arg_trivia(args);
        doc_call(callee, arg_docs, &arg_trivia, call_arg_last_trailing(args))
    };
    concat_all([
        lhs,
        text(flavor.token()),
        doc_expr_for_call_splice_arg(receiver),
    ])
}

fn doc_expr_for_call_splice_arg(e: &Expr) -> Doc {
    let rendered = doc_expr(e);
    if matches!(
        e,
        Expr::Path { .. }
            | Expr::Call { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::Tuple { .. }
            | Expr::LabelValue { .. }
    ) {
        rendered
    } else {
        concat_all([text("("), rendered, text(")")])
    }
}

/// Pretty-print one of the elaborator forms with the optional
/// trailing target type-arg: `iso!(e)` or `iso!(e, T)`. Uses the
/// width-driven A1 layout so wide arguments break to leading-
/// comma multi-line, mirroring how regular calls render. The
/// `name` parameter already includes the trailing `!` (e.g.,
/// `"iso!"`); the open paren comes from `comma_list`.
fn doc_user_elaborator_form(name: &str, args: &[CallArg]) -> Doc {
    let arg_docs: Vec<Doc> = args.iter().map(doc_call_arg).collect();
    let args_trivia = call_arg_trivia(args);
    concat(
        text(format!("{name}!")),
        doc_comma_list_with_trivia(
            text("("),
            arg_docs,
            &args_trivia,
            call_arg_last_trailing(args),
            text(")"),
        ),
    )
}

/// Pretty-print the `match!` elaborator form: `match!(v, (<arms>), B)`.
fn doc_field_access(receiver: &Expr, labels: &[FieldAccessLabel]) -> Doc {
    let label_docs: Vec<Doc> = labels
        .iter()
        .map(|label| text(label.label.clone()))
        .collect();
    let label_trivia: Vec<Vec<Trivia>> = labels
        .iter()
        .map(|label| label.meta.leading_trivia.clone())
        .collect();
    let last_trailing = labels
        .last()
        .map(|label| label.meta.trailing_trivia.as_slice())
        .unwrap_or(&[]);
    concat(
        doc_expr_for_postfix_receiver(receiver),
        doc_comma_list_with_trivia(
            text(".?{"),
            label_docs,
            &label_trivia,
            last_trailing,
            text("}"),
        ),
    )
}

fn doc_field_update(receiver: &Expr, updates: &[FieldUpdateLabel]) -> Doc {
    let update_docs: Vec<Doc> = updates
        .iter()
        .map(|update| doc_label_payload(&update.label, &update.value))
        .collect();
    let update_trivia: Vec<Vec<Trivia>> = updates
        .iter()
        .map(|update| update.meta.leading_trivia.clone())
        .collect();
    let last_trailing = updates
        .last()
        .map(|update| update.meta.trailing_trivia.as_slice())
        .unwrap_or(&[]);
    concat(
        doc_expr_for_postfix_receiver(receiver),
        doc_comma_list_with_trivia(
            text(".!{"),
            update_docs,
            &update_trivia,
            last_trailing,
            text("}"),
        ),
    )
}

fn doc_label_payload(label: &str, value: &Expr) -> Doc {
    if matches!(value, Expr::Unit { .. }) {
        text(format!("{label}="))
    } else if expr_is_bare_label_payload(label, value) {
        text(label.to_owned())
    } else {
        concat(text(format!("{label} = ")), doc_expr(value))
    }
}

fn expr_is_bare_label_payload(label: &str, value: &Expr) -> bool {
    let Expr::Path { segments, .. } = value else {
        return false;
    };
    segments.len() == 1 && segments[0].as_str() == last_label_segment(label)
}

fn last_label_segment(label: &str) -> &str {
    label.rsplit('.').next().unwrap_or(label)
}

/// Render a neutral let-binder: an identifier (or `_`), or an
/// explicit rich pattern `.(a: A, b: B)` / `.(name: (a: A, b: B))`.
/// The as-pattern is detected by the conjunction of a
/// `Some(pat)` pattern with a user-given (non-generated)
/// name; the bare destructuring uses the synthesized prefix.
fn doc_neutral_let_binder(
    name: &str,
    ty: Option<&crate::ast::Type>,
    pattern: Option<&crate::ast::ParamPattern>,
) -> Doc {
    match pattern {
        Some(pat) => {
            if name.starts_with("__pat_param_") {
                concat(text("."), doc_param_pattern_tuple(pat))
            } else {
                concat_all([
                    text(format!(".({name}: ")),
                    doc_param_pattern_tuple(pat),
                    text(")"),
                ])
            }
        }
        None => match ty {
            Some(ty) => concat_all([text(format!(".({name}: ")), doc_type(ty), text(")")]),
            None => text(name.to_owned()),
        },
    }
}

/// Pretty-print a `let` binding. Leading trivia (comments captured
/// before the `let` keyword) emits on its own line(s) above the
/// `let`; the value's `meta.leading_trivia` (comments captured
/// between `=` and the value expression) emits on its own line(s)
/// between `=` and the value, at the surrounding column +2. Either
/// flavor of trivia forces the surrounding block body into
/// multi-line layout because their `hardline`s break out of any
/// enclosing group. Chains of `let`s either all fit on one line or
/// all stack vertically; the layout is unified by the enclosing
/// `doc_block_body`'s group.
///
/// The value-and-`;` block has a width-driven flat/broken switch:
/// flat is `" value;"`, broken is `"\n  value;"` at the surrounding
/// column +2. When trivia is captured between `=` and the value,
/// the broken form fires unconditionally (the trivia includes its
/// own hardlines); otherwise the choice is driven by whether the
/// flat form fits in the remaining 100-column budget. Both triggers
/// — width pressure and trivia capture — share one canonical broken
/// shape (per `specs/style.md` § Comments inside expression bodies).
fn doc_let(
    name: &str,
    ty: Option<&crate::ast::Type>,
    pattern: Option<&crate::ast::ParamPattern>,
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
) -> Doc {
    if let Some(pat) = pattern {
        return doc_let_pattern(
            &pat.elems,
            if name.starts_with("__pat_let_") {
                None
            } else {
                Some(name)
            },
            value,
            body,
            leading_trivia,
        );
    }
    let binder = match ty {
        Some(ty) => concat_all([text(format!("let .({name}: ")), doc_type(ty), text(") =")]),
        None => text(format!("let {name} =")),
    };
    doc_let_with(
        binder,
        value,
        body,
        leading_trivia,
        &doc_expr_no_own_trivia,
        &doc_expr_with_own_meta_trivia,
    )
}

fn doc_row_let(
    entries: &[RowLetEntry],
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
) -> Doc {
    let entry_docs: Vec<Doc> = entries.iter().map(doc_row_let_entry).collect();
    let entry_trivia: Vec<Vec<Trivia>> = entries
        .iter()
        .map(|entry| entry.meta.leading_trivia.clone())
        .collect();
    let entry_last_trailing = entries
        .last()
        .map(|entry| entry.meta.trailing_trivia.as_slice())
        .unwrap_or(&[]);
    let binder_doc = doc_comma_list_with_trivia(
        text("let .({"),
        entry_docs,
        &entry_trivia,
        entry_last_trailing,
        text("})"),
    );
    let value_leading_trivia = value.meta().leading_trivia.as_slice();
    let value_expr = doc_expr_no_own_trivia(value);
    let value_and_semi = if has_comments(value_leading_trivia) {
        nest(
            2,
            concat_all([
                hardline(),
                doc_inline_leading_trivia(value_leading_trivia),
                value_expr,
                text(";"),
            ]),
        )
    } else {
        let flat = concat_all([text(" "), value_expr.clone(), text(";")]);
        let broken = nest(2, concat_all([hardline(), value_expr, text(";")]));
        group(flat_alt(flat, broken))
    };
    concat_all([
        doc_inline_leading_trivia(leading_trivia),
        binder_doc,
        text(" ="),
        value_and_semi,
        line(),
        doc_expr_with_own_meta_trivia(body),
    ])
}

fn doc_row_let_entry(entry: &RowLetEntry) -> Doc {
    if entry.alias_explicit || entry.local != last_label_segment(&entry.label) {
        text(format!("{} as {}", entry.label, entry.local))
    } else {
        text(entry.label.clone())
    }
}

fn doc_let_pattern(
    elems: &[crate::ast::ParamPatternElem],
    as_pattern_name: Option<&str>,
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
) -> Doc {
    let elem_docs: Vec<Doc> = elems.iter().map(doc_param_pattern_elem).collect();
    let pattern_doc = comma_list(text("("), elem_docs, text(")"));
    let binder_doc = match as_pattern_name {
        None => concat(text("."), pattern_doc),
        Some(name) => concat_all([text(format!(".({name}: ")), pattern_doc, text(")")]),
    };
    let value_leading_trivia = value.meta().leading_trivia.as_slice();
    let value_expr = doc_expr_no_own_trivia(value);
    let value_and_semi = if has_comments(value_leading_trivia) {
        nest(
            2,
            concat_all([
                hardline(),
                doc_inline_leading_trivia(value_leading_trivia),
                value_expr,
                text(";"),
            ]),
        )
    } else {
        let flat = concat_all([text(" "), value_expr.clone(), text(";")]);
        let broken = nest(2, concat_all([hardline(), value_expr, text(";")]));
        group(flat_alt(flat, broken))
    };
    concat_all([
        doc_inline_leading_trivia(leading_trivia),
        text("let "),
        binder_doc,
        text(" ="),
        value_and_semi,
        line(),
        doc_expr_with_own_meta_trivia(body),
    ])
}

/// Variant of [`doc_let`] that delegates sub-expression rendering to
/// caller-supplied closures, so a caller can interpose its own
/// rendering for `let`'s value and body positions.
fn doc_let_with(
    binder: Doc,
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
    render_value: &dyn Fn(&Expr) -> Doc,
    render_body: &dyn Fn(&Expr) -> Doc,
) -> Doc {
    let value_leading_trivia = value.meta().leading_trivia.as_slice();
    let value_expr = render_value(value);
    let value_and_semi = if has_comments(value_leading_trivia) {
        nest(
            2,
            concat_all([
                hardline(),
                doc_inline_leading_trivia(value_leading_trivia),
                value_expr,
                text(";"),
            ]),
        )
    } else {
        let flat = concat_all([text(" "), value_expr.clone(), text(";")]);
        let broken = nest(2, concat_all([hardline(), value_expr, text(";")]));
        group(flat_alt(flat, broken))
    };
    concat_all([
        doc_inline_leading_trivia(leading_trivia),
        binder,
        value_and_semi,
        line(),
        render_body(body),
    ])
}

/// Pretty-print an `Expr::Seq` (`e;` followed by the rest of the
/// block). Same layout principles as `doc_let`: trivia capture
/// before the value and before the body; chain of statements
/// either all fits flat or all breaks. The flat form is
/// `value; body`; the broken form puts each statement on its own
/// line at the surrounding +2 indent.
fn doc_seq(value: &Expr, body: &Expr, leading_trivia: &[Trivia]) -> Doc {
    doc_seq_with(
        value,
        body,
        leading_trivia,
        &doc_expr_no_own_trivia,
        &doc_expr_with_own_meta_trivia,
    )
}

/// Variant of [`doc_seq`] that delegates sub-expression rendering to
/// caller-supplied closures. Same role as [`doc_let_with`], for the
/// expression-statement's value and trailing body.
fn doc_seq_with(
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
    render_value: &dyn Fn(&Expr) -> Doc,
    render_body: &dyn Fn(&Expr) -> Doc,
) -> Doc {
    let value_leading_trivia = value.meta().leading_trivia.as_slice();
    let value_doc = if has_comments(value_leading_trivia) {
        concat_all([
            doc_inline_leading_trivia(value_leading_trivia),
            render_value(value),
        ])
    } else {
        render_value(value)
    };
    concat_all([
        doc_inline_leading_trivia(leading_trivia),
        value_doc,
        text(";"),
        line(),
        render_body(body),
    ])
}

/// Pretty-print an `Expr` whose `meta.leading_trivia` the caller has
/// already emitted. For `Let` / `RowLet` / `Seq` this re-enters the
/// statement printer with empty leading trivia so the meta-trivia
/// doesn't double-emit; for all other variants this is the same as
/// [`doc_expr`] (those arms don't read meta-trivia).
fn doc_expr_no_own_trivia(e: &Expr) -> Doc {
    match e {
        Expr::Let {
            name,
            ty,
            pattern,
            value,
            body,
            meta: _,
            ..
        } => doc_let(name, ty.as_ref(), pattern.as_ref(), value, body, &[]),
        Expr::RowLet {
            entries,
            value,
            body,
            meta: _,
            ..
        } => doc_row_let(entries, value, body, &[]),
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta: _,
        } => doc_seq(value, body, &[]),
        _ => doc_expr(e),
    }
}

/// Pretty-print an `Expr` and emit its `meta.leading_trivia`. For
/// `Let` / `RowLet` / `Seq`, `doc_expr` already reads
/// `meta.leading_trivia` itself via the statement printers, so just
/// delegate; for other variants the trivia isn't picked up by their
/// `doc_expr` arms, so emit it explicitly.
fn doc_expr_with_own_meta_trivia(e: &Expr) -> Doc {
    if matches!(e, Expr::Let { .. } | Expr::RowLet { .. } | Expr::Seq { .. }) {
        return doc_expr(e);
    }
    let trivia = e.meta().leading_trivia.as_slice();
    if has_comments(trivia) {
        concat_all([doc_inline_leading_trivia(trivia), doc_expr(e)])
    } else {
        doc_expr(e)
    }
}

/// Pretty-print a call site. When any arg carries a leading comment
/// — or the last arg a trailing / dangling comment before `)` —
/// force a multi-line broken layout. Otherwise fall back to the
/// standard width-driven A1 `comma_list`. Thin wrapper over
/// [`doc_comma_list_with_trivia`] that prepends the callee.
///
/// `arg_trivia` must be parallel to `args` (same length). The
/// parser's `call_arg_list` preserves this; every synthesized
/// `Expr::Call` site allocates a matching `vec![Vec::new();
/// arg_count]` so the invariant holds across passes. `last_trailing`
/// is the trailing run stashed on the last arg's `meta` (empty when
/// no args or no trailing comment).
fn doc_call(
    callee: Doc,
    args: Vec<Doc>,
    arg_trivia: &[Vec<Trivia>],
    last_trailing: &[Trivia],
) -> Doc {
    concat(
        callee,
        doc_comma_list_with_trivia(text("("), args, arg_trivia, last_trailing, text(")")),
    )
}

/// Pretty-print a comma-separated list with per-item leading
/// trivia and the last item's closing-boundary trailing trivia.
/// When any item carries a leading `LineComment` — or the last item
/// a trailing / dangling comment before the closer — force the
/// multi-line A1 broken layout (open on the head's line, items at
/// +2 indent each preceded by their trivia comments and a leading-
/// comma `, `, a multi-line item's continuation anchored at the
/// item's content column, the last item's trailing comment on its
/// own line, closer at the same +2 column). Otherwise fall back to
/// the standard width-driven [`comma_list`].
///
/// `item_trivia` must be parallel to `items`; the parser captures
/// real trivia for surface forms (calls, tuples, label-value
/// sugar) and synthesized sites use empty vectors. `last_trailing`
/// is the trailing run the parser stashed on the last element's
/// `meta` (empty for an empty list or no trailing comment).
fn doc_comma_list_with_trivia(
    open: Doc,
    items: Vec<Doc>,
    item_trivia: &[Vec<Trivia>],
    last_trailing: &[Trivia],
    close: Doc,
) -> Doc {
    doc_comma_list_with_trivia_and_padding(open, items, item_trivia, last_trailing, close, false)
}

fn doc_comma_list_with_trivia_and_padding(
    open: Doc,
    items: Vec<Doc>,
    item_trivia: &[Vec<Trivia>],
    last_trailing: &[Trivia],
    close: Doc,
    padded: bool,
) -> Doc {
    debug_assert_eq!(
        items.len(),
        item_trivia.len(),
        "item_trivia must be parallel to items"
    );
    let any_leading = item_trivia.iter().any(|t| has_comments(t));
    let trailing_comment = has_comments(last_trailing);
    if !any_leading && !trailing_comment {
        return crate::doc::comma_list_with_padding(open, items, close, padded);
    }
    let last_index = items.len().saturating_sub(1);
    let mut inner = empty();
    if items.is_empty() && trailing_comment {
        inner = concat(hardline(), doc_trailing_comments(last_trailing));
    }
    for (i, (item, trivia)) in items.iter().zip(item_trivia).enumerate() {
        inner = concat(inner, hardline());
        inner = concat(inner, doc_inline_leading_trivia(trivia));
        inner = concat(inner, text(", "));
        // nest(2) mirrors the `", "` prefix so a multi-line item's
        // continuation lines anchor at the item's content column, not
        // at the comma column — same as `comma_list`'s broken layout.
        inner = concat(inner, nest(2, item.clone()));
        // The last element's trailing / dangling comment emits on its
        // own line below it, before the closer, so it is preserved in
        // place rather than dropped.
        if i == last_index && trailing_comment {
            inner = concat(inner, hardline());
            inner = concat(inner, doc_trailing_comments(last_trailing));
        }
    }
    inner = concat(inner, hardline());
    concat(open, nest(2, concat(inner, close)))
}

/// True when `trivia` contains at least one `LineComment` or
/// `DocCommentLine` (after empty-comment-line collapse). Used by
/// callers to decide whether to prepend a leading hardline before a
/// comment block.
fn has_comments(trivia: &[Trivia]) -> bool {
    trivia.iter().any(|t| {
        matches!(
            t,
            Trivia::LineComment { .. } | Trivia::DocCommentLine { .. }
        )
    })
}

/// Emit comments captured as leading trivia before an inline position, for
/// example `let` bindings inside expression bodies and `match!` clauses inside
/// a multi-clause `match!`. Each
/// `LineComment` is followed by a [`hardline`]; intermediate
/// blank-line paragraph separators between two comment paragraphs
/// are preserved; runs of empty `//` lines collapse to one.
///
/// **No leading hardline.** Callers are responsible for placing a
/// hardline above the first comment if they need one (e.g.,
/// `doc_let` does this when comments are present; the multi-
/// clause loop's own intro hardline already handles it
/// for clauses).
///
/// If `trivia` contains no `LineComment`s, returns
/// [`empty()`](Doc::Empty) — no layout effect.
fn doc_inline_leading_trivia(trivia: &[Trivia]) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_empty_comment = false;
    let mut newline_run: usize = 0;
    let mut emitted_first = false;
    for entry in trivia {
        match entry {
            Trivia::Newline => newline_run += 1,
            Trivia::LineComment { text: body, .. } => {
                let is_empty = body.is_empty();
                if is_empty && last_was_empty_comment {
                    newline_run = 0;
                    continue;
                }
                if emitted_first && newline_run >= 2 {
                    // Blank line between two comment paragraphs.
                    parts.push(hardline());
                }
                parts.push(text(format!("//{body}")));
                parts.push(hardline());
                last_was_empty_comment = is_empty;
                emitted_first = true;
                newline_run = 0;
            }
            // Doc-comment lines are only expected in top-level trivia;
            // inside expression bodies they don't attach to anything,
            // so skip them silently (they'll never appear in valid programs
            // since the parser enforces attachment rules).
            Trivia::DocCommentLine { .. } => {}
        }
    }
    concat_all(parts)
}

/// Emit comments stranded at a *closing* boundary — the trailing /
/// dangling run before a `}` / `)` or end-of-file (stored on a
/// container's `meta.trailing_trivia`). Each comment lands on its own
/// line, in source order, with the empty-`//`-run collapse and the
/// ≤1-blank-line-between-paragraphs rule that leading comments use.
/// Unlike [`doc_inline_leading_trivia`] this emits **no** trailing
/// hardline after the last comment: the caller closes the line (the
/// closer or item separator follows). Leading newlines (separating
/// the run from the last token) are dropped.
///
/// Returns [`empty()`](Doc::Empty) when the run carries no comments.
fn doc_trailing_comments(trivia: &[Trivia]) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_empty_comment = false;
    let mut newline_run: usize = 0;
    let mut emitted_first = false;
    for entry in trivia {
        match entry {
            Trivia::Newline => newline_run += 1,
            Trivia::LineComment { text: body, .. } => {
                let is_empty = body.is_empty();
                if is_empty && last_was_empty_comment {
                    newline_run = 0;
                    continue;
                }
                if emitted_first {
                    // Separator before this comment: a blank line
                    // between paragraphs (run of ≥2 newlines) collapses
                    // to one, a single newline to none.
                    parts.push(hardline());
                    if newline_run >= 2 {
                        parts.push(hardline());
                    }
                }
                parts.push(text(format!("//{body}")));
                last_was_empty_comment = is_empty;
                emitted_first = true;
                newline_run = 0;
            }
            // A `///` at a closing boundary has nothing to document; the
            // parser never produces a valid program with one there, so
            // there is no faithful position. Skip rather than mis-emit.
            Trivia::DocCommentLine { .. } => {}
        }
    }
    concat_all(parts)
}

/// Target chunk width for [`doc_string_literal`]'s reflow. Per
/// `specs/style.md` § "String literal reflow": values longer than
/// 72 characters split into adjacent string literals, each chunk
/// ≤ 72 characters of value (not line position).
const STRING_CHUNK_TARGET: usize = 72;

/// The optional trailing `(Type)` of a `LiteralCall`. Empty for a
/// bare literal; `(Type)` — no inner padding — when annotated.
fn doc_literal_annotation(annotation: &Option<Type>) -> Doc {
    match annotation {
        Some(ty) => concat(concat(text("("), doc_type(ty)), text(")")),
        None => text(""),
    }
}

fn doc_string_literal(value: &str) -> Doc {
    let chunks = chunk_string_value(value, STRING_CHUNK_TARGET);
    if chunks.len() == 1 {
        return text(escape_string(&chunks[0]));
    }
    // Multi-chunk: emit each as a literal on its own line, aligned
    // with the first chunk's opening quote (per `specs/style.md`).
    // `align` makes the inner `hardline`s indent to the current
    // column when the doc fires.
    let mut chunk_docs: Vec<Doc> = Vec::with_capacity(chunks.len() * 2 - 1);
    for (i, c) in chunks.iter().enumerate() {
        if i > 0 {
            chunk_docs.push(hardline());
        }
        chunk_docs.push(text(escape_string(c)));
    }
    align(concat_all(chunk_docs))
}

fn doc_string_field_entry(leading: Doc, key: &str, value: &str) -> Doc {
    concat_all([leading, text(format!("{key} ")), doc_string_literal(value)])
}

/// Canonical string-literal escape: short forms for the named
/// controls (`\n`, `\t`, `\r`, `\b`, `\f`), `\\` for backslash,
/// `\"` for the double-quote, and lowercase-hex `\uXXXX` for any
/// other code point < 0x20.
fn escape_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Split `value` into chunks of ≤ `target` characters, breaking at
/// whitespace word boundaries. Trailing whitespace at a split point
/// belongs to the preceding chunk. A single word longer than
/// `target` overflows in its own chunk (no further splitting —
/// per `specs/style.md`'s "Unbreakable strings overflow" rule).
///
/// If `value` is ≤ `target` characters, returns one chunk equal to
/// the whole value. Otherwise the chunks are non-empty.
fn chunk_string_value(value: &str, target: usize) -> Vec<String> {
    if value.chars().count() <= target {
        return vec![value.to_owned()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len: usize = 0;

    let mut iter = value.chars().peekable();
    while iter.peek().is_some() {
        // Read a word (maximal non-whitespace run) plus the
        // trailing whitespace after it.
        let mut word = String::new();
        while let Some(&c) = iter.peek() {
            if c.is_whitespace() {
                break;
            }
            word.push(c);
            iter.next();
        }
        let mut ws = String::new();
        while let Some(&c) = iter.peek() {
            if !c.is_whitespace() {
                break;
            }
            ws.push(c);
            iter.next();
        }
        let word_len = word.chars().count();
        let ws_len = ws.chars().count();

        if current_len + word_len <= target || current.is_empty() {
            // Fits in the current chunk (or the chunk is empty
            // and a single overflowing word goes here regardless).
            current.push_str(&word);
            current.push_str(&ws);
            current_len += word_len + ws_len;
        } else {
            // Start a new chunk with this word.
            chunks.push(current);
            current = word;
            current.push_str(&ws);
            current_len = word_len + ws_len;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

// =========================================================================
// Package files
// =========================================================================

fn doc_package_file(e: &PackageFile) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    parts.push(doc_item_leading_trivia(e.meta.leading_trivia.as_slice()));
    parts.push(text(format!("package {};", e.name)));
    parts.push(hardline());
    if let Some(build) = &e.build {
        parts.push(hardline());
        parts.push(doc_build_block(build));
        parts.push(hardline());
    }
    if let Some(bridge) = &e.bridge {
        parts.push(hardline());
        parts.push(doc_item_leading_trivia(bridge.leading_trivia.as_slice()));
        parts.push(doc_bridge_block(bridge));
        parts.push(hardline());
    }
    parts.push(doc_item_leading_trivia(e.meta.trailing_trivia.as_slice()));
    concat_all(parts)
}

/// Render the package `bridge { <glob>; … }` block — one glob per line.
fn doc_bridge_block(bridge: &BridgeBlock) -> Doc {
    let body = if bridge.globs.is_empty() {
        None
    } else {
        let mut lines: Vec<Doc> = Vec::new();
        for (idx, glob) in bridge.globs.iter().enumerate() {
            if idx > 0 {
                lines.push(concat(text(";"), hardline()));
            }
            lines.push(doc_item_leading_trivia(glob.leading_trivia.as_slice()));
            lines.push(text(doc_bridge_glob(glob)));
        }
        Some(concat_all(lines))
    };
    let body = append_block_trailing(body, bridge.trailing_trivia.as_slice());
    doc_braced_block(text("bridge"), body, false)
}

fn doc_bridge_glob(glob: &crate::ast::BridgeGlob) -> String {
    glob.segments
        .iter()
        .map(|s| match s {
            BridgeGlobSegment::Literal(name) => name.as_str(),
            BridgeGlobSegment::Star => "*",
            BridgeGlobSegment::DoubleStar => "**",
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Render a `<local>.dep.kio` dependency file: the `dependency <local>;`
/// header, a blank line, then the `source { … }` block. Mirrors
/// [`doc_package_file`]'s leading-trivia / blank-line discipline.
fn doc_dependency_file(dep: &DependencyFile) -> Doc {
    let mut parts = vec![
        doc_item_leading_trivia(dep.meta.leading_trivia.as_slice()),
        text(format!("dependency {};", dep.name)),
        hardline(),
        hardline(),
        doc_item_leading_trivia(dep.source.leading_trivia.as_slice()),
        doc_source_block(&dep.source),
        hardline(),
    ];
    // `rehost` statements follow the source block, separated by one blank
    // line, each on its own line in canonical (sorted) order. `retype`
    // statements follow the `rehost` group the same way.
    if !dep.rehost.is_empty() {
        parts.push(hardline());
        parts.push(doc_rehost_statements(&dep.rehost));
    }
    if !dep.retype.is_empty() {
        parts.push(hardline());
        parts.push(doc_retype_statements(&dep.retype));
    }
    parts.push(doc_item_leading_trivia(dep.meta.trailing_trivia.as_slice()));
    concat_all(parts)
}

/// Render local `path`, or remote `git`, `ref`, and optional `path` in canonical order.
fn doc_source_block(source: &SourceBlock) -> Doc {
    let body = match &source.origin {
        SourceOrigin::Path {
            path,
            path_leading_trivia,
            ..
        } => doc_string_field_entry(
            doc_item_leading_trivia(path_leading_trivia.as_slice()),
            "path",
            path,
        ),
        SourceOrigin::Git(source) => {
            let mut fields = vec![
                doc_string_field_entry(
                    doc_item_leading_trivia(&source.url_leading_trivia),
                    "git",
                    &source.url,
                ),
                doc_string_field_entry(
                    doc_item_leading_trivia(&source.ref_leading_trivia),
                    "ref",
                    &source.git_ref,
                ),
            ];
            if let Some(selector) = &source.manifest_path {
                fields.push(doc_string_field_entry(
                    doc_item_leading_trivia(&selector.leading_trivia),
                    "path",
                    &selector.path,
                ));
            }
            join(concat(text(";"), hardline()), fields)
        }
    };
    doc_braced_block(
        text("source"),
        append_block_trailing(Some(body), &source.trailing_trivia),
        false,
    )
}

/// Render a dependency file's `rehost <dep>/<mod> to <local>/<mod>;`
/// statements, one per line, sorted lexicographically by `from` then
/// `to`. Source order is not significant, so the formatter imposes a
/// canonical order the same way it sorts a module's imports.
fn doc_rehost_statements(rehost: &[crate::ast::RehostDecl]) -> Doc {
    let mut sorted: Vec<&crate::ast::RehostDecl> = rehost.iter().collect();
    sorted.sort_by(|a, b| {
        module_path_surface_str(&a.from.segments)
            .cmp(&module_path_surface_str(&b.from.segments))
            .then_with(|| {
                module_path_surface_str(&a.to.segments)
                    .cmp(&module_path_surface_str(&b.to.segments))
            })
    });
    concat_all(sorted.into_iter().map(|r| {
        concat_all([
            doc_item_leading_trivia(r.leading_trivia.as_slice()),
            text("rehost "),
            doc_module_path(&r.from),
            text(" to "),
            doc_module_path(&r.to),
            text(";"),
            hardline(),
        ])
    }))
}

/// Render a dependency file's `retype <dep>/<mod> to <local>/<mod>;`
/// statements (the per-type form spelled `<dep>/<mod>.T to <local>/<mod>.T`),
/// one per line, sorted lexicographically by `from` then `to` then the
/// per-type name. Source order is not significant, so the formatter
/// imposes a canonical order the same way [`doc_rehost_statements`] does.
fn doc_retype_statements(retype: &[crate::ast::RetypeDecl]) -> Doc {
    let mut sorted: Vec<&crate::ast::RetypeDecl> = retype.iter().collect();
    sorted.sort_by(|a, b| {
        module_path_surface_str(&a.from.segments)
            .cmp(&module_path_surface_str(&b.from.segments))
            .then_with(|| {
                module_path_surface_str(&a.to.segments)
                    .cmp(&module_path_surface_str(&b.to.segments))
            })
            .then_with(|| a.type_name.cmp(&b.type_name))
    });
    concat_all(sorted.into_iter().map(|r| {
        let type_suffix = |doc: Doc| match &r.type_name {
            Some(name) => concat_all([doc, text("."), text(name.clone())]),
            None => doc,
        };
        concat_all([
            doc_item_leading_trivia(r.leading_trivia.as_slice()),
            text("retype "),
            type_suffix(doc_module_path(&r.from)),
            text(" to "),
            type_suffix(doc_module_path(&r.to)),
            text(";"),
            hardline(),
        ])
    }))
}

/// Render a `<local>.lock.kio` lock file: the `lock <local>;` header, a
/// blank line, then the `resolved` fields. Mirrors
/// [`doc_dependency_file`]'s leading-trivia / blank-line discipline.
fn doc_lock_file(lock: &LockFile) -> Doc {
    let leading = |key: &str| {
        doc_item_leading_trivia(
            lock.field_leading_trivia
                .get(key)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )
    };
    let mut fields = vec![
        doc_string_field_entry(leading("git"), "git", &lock.url),
        doc_string_field_entry(leading("ref"), "ref", &lock.git_ref),
    ];
    if let Some(path) = &lock.manifest_path {
        fields.push(doc_string_field_entry(leading("path"), "path", path));
    }
    fields.extend([
        doc_string_field_entry(leading("commit"), "commit", &lock.commit),
        doc_string_field_entry(leading("sig"), "sig", &lock.sig),
    ]);
    let body = join(concat(text(";"), hardline()), fields);
    concat_all([
        doc_item_leading_trivia(lock.meta.leading_trivia.as_slice()),
        text(format!("lock {};", lock.name)),
        hardline(),
        hardline(),
        doc_item_leading_trivia(&lock.resolved_leading_trivia),
        doc_braced_block(
            text("resolved"),
            append_block_trailing(Some(body), &lock.trailing_trivia),
            false,
        ),
        hardline(),
        doc_item_leading_trivia(lock.meta.trailing_trivia.as_slice()),
    ])
}

/// Append a closing-boundary comment run to a `doc_braced_block` body
/// (a blank line separates it from the last declaration, matching the
/// block's top-level-item spacing). Returns `body` unchanged when the
/// run carries no comments; synthesizes a body from the comments alone
/// when `body` is `None` (an otherwise-empty block with only a
/// dangling comment).
fn append_block_trailing(body: Option<Doc>, trailing: &[Trivia]) -> Option<Doc> {
    if !has_comments(trailing) {
        return body;
    }
    // `doc_trailing_comments` emits no trailing newline — the enclosing
    // `doc_braced_block` supplies the line before `}`.
    let comments = doc_trailing_comments(trailing);
    match body {
        Some(b) => Some(concat_all([b, hardline(), hardline(), comments])),
        None => Some(comments),
    }
}

fn doc_host_type(h: &HostType) -> Doc {
    let doc_prefix = h.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let head = text(format!("host type {}", h.name));
    let params = if h.type_params.is_empty() {
        empty()
    } else {
        doc_type_params(&h.type_params)
    };
    let role = match h.role {
        Some(role) => text(format!(" role({})", role.role.as_str())),
        None => empty(),
    };
    let owned = if h.owned { text(" { owned }") } else { empty() };
    concat_all([doc_prefix, head, params, role, owned])
}

fn doc_host_fn(h: &HostFn) -> Doc {
    doc_bodyless_fn(h, "host fn ")
}

fn doc_bodyless_fn(h: &HostFn, prefix: &str) -> Doc {
    let doc_prefix = h.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let head = text(format!("{prefix}{}", h.name));
    let params = doc_host_fn_signature(h);
    concat_all([doc_prefix, head, params, text(" -> "), doc_type(&h.ret)])
}

fn doc_host_fn_signature(h: &HostFn) -> Doc {
    let groups = crate::ast::host_fn_group_refs(&h.params, &h.param_groups);
    if groups.is_empty() {
        return text("()");
    }
    let mut saw_value_group = false;
    let mut parts = Vec::new();
    for group in groups {
        match group {
            crate::ast::HostFnGroupRef::Type(params) => {
                for param in params {
                    let HostFnParam::Type(tp) = param else {
                        unreachable!("host fn type group contains only type params")
                    };
                    parts.push(text(type_binder_text(tp)));
                }
            }
            crate::ast::HostFnGroupRef::Value(params) => {
                saw_value_group = true;
                let docs: Vec<Doc> = params.iter().map(doc_host_fn_def_param).collect();
                parts.push(comma_list(text("("), docs, text(")")));
            }
        }
    }
    if !saw_value_group {
        parts.push(text("()"));
    }
    concat_all(parts)
}

fn doc_host_fn_def_param(p: &HostFnParam) -> Doc {
    match p {
        HostFnParam::Type(tp) => text(type_binder_text(tp)),
        HostFnParam::Value(vp) => match &vp.name {
            None => doc_type(&vp.ty),
            Some(name) => concat(text(format!("{name}: ")), doc_type(&vp.ty)),
        },
    }
}

// =========================================================================
// Build files
// =========================================================================

/// Render the `build { ... }` block: the `build` header, then the
/// body (mandatory `cache`, optional `docs`, then each `target`
/// block) at +2 indent, closing `}` back at the outer column.
fn doc_build_block(b: &BuildBlock) -> Doc {
    let mut body: Vec<Doc> = Vec::new();
    // Mandatory `cache` declaration emits first, separated from the
    // optional `docs` block and any `target` blocks by a blank line.
    body.push(doc_cache_decl(&b.cache));
    if let Some(docs) = &b.docs {
        body.push(doc_docs_block(docs));
    }
    for t in &b.targets {
        body.push(doc_target_block(t));
    }
    doc_braced_block(
        concat(doc_item_leading_trivia(&b.leading_trivia), text("build")),
        append_block_trailing(
            Some(join(concat_all([text(";"), hardline(), hardline()]), body)),
            &b.trailing_trivia,
        ),
        false,
    )
}

/// Canonicalized layout for the `docs` block: `md` first, then
/// repeatable `support` entries, then the optional `md_out` and `html`
/// keys in that fixed order, so authors who write the keys in a
/// different order don't see spurious diffs.
fn doc_docs_block(d: &crate::ast::BuildBlockDocs) -> Doc {
    let mut entries: Vec<Doc> = Vec::new();
    entries.push(doc_string_field_entry(
        doc_inline_leading_trivia(&d.md_leading_trivia),
        "md",
        &d.md,
    ));
    for (i, support) in d.support.iter().enumerate() {
        entries.push(doc_string_field_entry(
            doc_inline_leading_trivia(
                d.support_leading_trivia
                    .get(i)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            ),
            "support",
            support,
        ));
    }
    if let Some(md_out) = &d.md_out {
        entries.push(doc_string_field_entry(
            doc_inline_leading_trivia(&d.md_out_leading_trivia),
            "md_out",
            md_out,
        ));
    }
    if let Some(html) = &d.html {
        entries.push(doc_string_field_entry(
            doc_inline_leading_trivia(&d.html_leading_trivia),
            "html",
            html,
        ));
    }
    doc_braced_block(
        concat(doc_inline_leading_trivia(&d.leading_trivia), text("docs")),
        append_block_trailing(
            Some(join(concat(text(";"), hardline()), entries)),
            &d.trailing_trivia,
        ),
        false,
    )
}

fn doc_cache_decl(c: &BuildBlockCache) -> Doc {
    match c {
        BuildBlockCache::Path {
            path,
            leading_trivia,
            ..
        } => doc_string_field_entry(doc_inline_leading_trivia(leading_trivia), "cache", path),
        BuildBlockCache::Disabled { leading_trivia, .. } => {
            concat_all([doc_inline_leading_trivia(leading_trivia), text("cache ()")])
        }
    }
}

fn doc_target_block(t: &TargetBlock) -> Doc {
    // Entries at +2 indent, closing `}` back at the block's indent.
    // The target id is a bare identifier (`target rust`), not a
    // quoted string.
    let mut entries: Vec<Doc> = Vec::new();
    let mut ordered_entries: Vec<&TargetEntry> = t.entries.iter().collect();
    ordered_entries.sort_by(|a, b| target_entry_order(a).cmp(&target_entry_order(b)));
    for entry in ordered_entries {
        entries.push(doc_target_entry(entry));
    }
    doc_braced_block(
        concat(
            doc_inline_leading_trivia(&t.leading_trivia),
            text(format!("target {}", t.id)),
        ),
        append_block_trailing(
            (!entries.is_empty()).then(|| join(concat(text(";"), hardline()), entries)),
            &t.trailing_trivia,
        ),
        false,
    )
}

fn doc_target_entry(e: &TargetEntry) -> Doc {
    doc_string_field_entry(
        doc_inline_leading_trivia(&e.leading_trivia),
        &e.key,
        &e.value,
    )
}

fn target_entry_order(e: &TargetEntry) -> (u8, &str) {
    match e.key.as_str() {
        "out" => (0, ""),
        other => (1, other),
    }
}

// =========================================================================
// Tests — pretty-printer round-trips for every AST construct.
//
// Property: parse → pretty → parse → pretty produces the same string
// as parse → pretty (i.e., pretty is idempotent under parse). This
// proves that pretty's output is a canonical form for parse-equivalent
// inputs without needing to compare AST structures across spans.
// =========================================================================

#[cfg(test)]
mod tests {

    #[test]
    fn semicolon_owners_preserve_outer_terminators_and_nested_separators() {
        let source = "module m;
            fn id(x: .) -> . { x };
            newtype Box : . { ; constructor make; projector take; };
            op _ + _ { ; impl add; };
            varop [% %] { ; foldl step initial; finalize finish; };
            elab identity : . -> . { ; captures id; impl identity_impl; };
            host type Text role(str) { owned };
            type Alias = .;
            labels Choice = { left: . } | { right: . };
            rec(loop) { ; fn first(x: .) -> . { x }; fn second(x: .) -> . { x }; };
            rec { ; type Left = Right; newtype Right : Left { constructor make; projector take }; };";
        let formatted = roundtrip(source);
        for expected in [
            "fn id(x: .) -> . { x }\n",
            "newtype Box : . { constructor make; projector take }\n",
            "op _ + _ { impl add }\n",
            "varop [% %] { foldl step initial; finalize finish }\n",
            "elab identity : . -> . { captures id; impl identity_impl }\n",
            "host type Text role(str) { owned };",
            "type Alias = .;",
            "labels Choice = { left: . } | { right: . };",
            "fn first(x: .) -> . { x };\n",
            "type Left = Right;\n",
        ] {
            assert!(
                formatted.contains(expected),
                "missing {expected:?}:\n{formatted}"
            );
        }
    }

    #[test]
    fn semicolon_config_comments_follow_sorted_fields_and_survive_closers() {
        let source = "build { ;
            // cache note
            cache ();
            // docs note
            docs { ; // html note
                html \"out/html\";
                // md note
                md \"docs\";
                // docs closer
            };
            // target note
            target js { ; // module note
                module_format \"esm\";
                // out note
                out \"out/js\";
                // target closer
            };
            // build closer
        }; // file end
        bridge { ; api; // bridge closer
        }; // package end
        ";
        let formatted = package_roundtrip(source);
        for comment in [
            "cache note",
            "docs note",
            "html note",
            "md note",
            "docs closer",
            "target note",
            "module note",
            "out note",
            "target closer",
            "build closer",
            "file end",
            "bridge closer",
            "package end",
        ] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        assert!(formatted.find("// md note").unwrap() < formatted.find("// html note").unwrap());
        assert!(formatted.find("// out note").unwrap() < formatted.find("// module note").unwrap());
        assert!(
            formatted.contains("  };\n\n  // target note"),
            "{formatted}"
        );
        assert!(!formatted.contains("html \"out/html\";"), "{formatted}");
        assert!(!formatted.contains("module_format \"esm\";"), "{formatted}");
    }

    #[test]
    fn removed_outer_semicolon_comments_stay_outside_declaration_bodies() {
        let formatted = roundtrip(
            "module m;
            newtype Box : . { constructor make; projector take; }
            // before outer token
            ; // after outer token
            type Alias = .;
            fn run() -> . { () }
            // before final token
            ; // after final token
        ",
        );
        for comment in [
            "before outer token",
            "after outer token",
            "before final token",
            "after final token",
        ] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        let first_comment = formatted.find("// before outer token").unwrap();
        assert!(formatted[..first_comment].ends_with("}\n\n"), "{formatted}");
        assert!(
            first_comment < formatted.find("type Alias").unwrap(),
            "{formatted}"
        );
        assert!(
            formatted.find("fn run() -> . { () }").unwrap()
                < formatted.find("// before final token").unwrap()
        );
    }

    #[test]
    fn semicolon_dependency_and_lock_fields_keep_boundary_comments() {
        let dep = dependency_roundtrip(
            "dependency dep;
            // origin
            source { ; // ref note
                ref \"main\";
                // git note
                git \"https://example.test/repo\";
                // source closer
            }; // dependency end
        ",
        );
        for comment in [
            "origin",
            "ref note",
            "git note",
            "source closer",
            "dependency end",
        ] {
            assert_eq!(dep.matches(comment).count(), 1, "{dep}");
        }
        let lock = lock_roundtrip(
            "lock dep;
            // resolved note
            resolved { ; // sig note
                sig \"digest\";
                // ref note
                ref \"main\";
                // git note
                git \"https://example.test/repo\";
                // commit note
                commit \"abcdef\";
                // lock closer
            }; // lock end
        ",
        );
        for comment in [
            "resolved note",
            "sig note",
            "ref note",
            "git note",
            "commit note",
            "lock closer",
            "lock end",
        ] {
            assert_eq!(lock.matches(comment).count(), 1, "{lock}");
        }
        assert!(lock.find("// git note").unwrap() < lock.find("// ref note").unwrap());
        assert!(lock.find("// commit note").unwrap() < lock.find("// sig note").unwrap());
        assert!(!lock.contains("sig \"digest\";"), "{lock}");
    }

    #[test]
    fn contextual_let_calls_preserve_expression_meaning() {
        let source = "module m; fn let(x: .) -> . { x } \
            fn left(x: ., y: .) -> . { x } op _ <- _ { impl left; }; \
            fn run(f: ., x: .) -> . { (let(f) <- x); f }";
        let before = parse(source).unwrap();
        let formatted = roundtrip(source);
        let after = parse(&formatted).unwrap();
        for module in [&before, &after] {
            let Expr::Seq { value, .. } = function_body(module, "run") else {
                panic!("ordinary call/operator statement became a binding");
            };
            assert_eq!(op_topology(value), "normal[_ `<-` _](call[let](f), x)");
        }
    }

    #[test]
    fn rich_let_patterns_round_trip_with_explicit_introducer() {
        for (binder, expected) in [
            ("x", "let x ="),
            ("_", "let _ ="),
            (".(x: Int)", "let .(x: Int) ="),
            (".(x, y)", "let .(x, y) ="),
            (
                ".(whole: (x: Int, y: Int))",
                "let .(whole: (x: Int, y: Int)) =",
            ),
            (".({foo as x})", "let .({foo as x}) ="),
        ] {
            let source = format!("module m; fn run() {{ let {binder} = value; () }}");
            let formatted = roundtrip(&source);
            assert!(formatted.contains(expected), "{formatted}");
        }
        for connective in ["=", "<-"] {
            for binder in [
                "x",
                "_",
                ".(x: Int)",
                ".(x, y)",
                ".(whole: (x: Int, y: Int))",
            ] {
                roundtrip(&format!(
                    "module m; fn run() {{ do! m {{ let {binder} {connective} value; result }} }}"
                ));
            }
        }
        roundtrip("module m; fn run() { let .(<U> x) = value; x }");
    }

    #[test]
    fn removed_let_forms_are_not_binding_aliases() {
        for binder in [
            "x: Int",
            "(x, y)",
            "whole: (x: Int, y: Int)",
            "<U> x",
            ".(.{foo})",
        ] {
            let source = format!("module m; fn run() {{ let {binder} = value; () }}");
            assert!(parse(&source).is_err(), "{source}");
        }
        for binder in ["x", "(x, y)", "{foo}"] {
            let source = format!("module m; fn run() {{ .{binder} = value; () }}");
            assert!(parse(&source).is_err(), "{source}");
        }
    }

    #[test]
    fn import_grammar_import_a1_comments() {
        let source = "module c; // provider comment\nimport z( // list comment\n , zebra, // item comment\n alpha, {field}, op _ + _); import a as alias;";
        let module = crate::pass::parser::parse(source).expect("consumer grammar");
        let formatted = pretty_module(&module);
        assert!(
            formatted.find("import a as alias;").unwrap() < formatted.find("import z(").unwrap()
        );
        for comment in ["provider comment", "list comment", "item comment"] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        assert!(!formatted.contains("z ("));
        assert_eq!(
            pretty_module(&crate::pass::parser::parse(&formatted).unwrap()),
            formatted
        );
        let long = crate::pass::parser::parse("module c; import long_provider_name(first_long_selected_name, second_long_selected_name, third_long_selected_name, varop [% %]);").unwrap();
        let rendered = pretty_module(&long);
        assert!(
            rendered.contains("import long_provider_name(\n  , "),
            "{rendered}"
        );
        assert!(rendered.contains("\n  );"));
        assert_eq!(
            pretty_module(&crate::pass::parser::parse(&rendered).unwrap()),
            rendered
        );
    }

    #[test]
    fn import_grammar_quotation_and_run_boundaries() {
        for signature in [
            "op _ (=) (_)",
            "op _ && ++ _",
            "varop [<... ...<]",
            "op _ ? (_ : _)",
        ] {
            let source = format!("module c; import p({signature});");
            let parsed = crate::pass::parser::parse(&source).expect("signature");
            let formatted = pretty_module(&parsed);
            let reparsed = crate::pass::parser::parse(&formatted).expect("formatted signature");
            assert_eq!(pretty_module(&reparsed), formatted);
            let crate::ast::ImportKind::Selective { items: a, .. } = &parsed.imports[0].kind else {
                panic!("selective")
            };
            let crate::ast::ImportKind::Selective { items: b, .. } = &reparsed.imports[0].kind
            else {
                panic!("selective")
            };
            let crate::ast::ImportItem::OperatorPattern { grammar: a, .. } = &a[0] else {
                panic!("operator")
            };
            let crate::ast::ImportItem::OperatorPattern { grammar: b, .. } = &b[0] else {
                panic!("operator")
            };
            assert_eq!(a, b);
        }
    }
    use super::*;
    use crate::pass::parser::parse;

    #[test]
    fn selective_import_comments_follow_items_and_container_boundaries() {
        let source = r#"module c;
// statement
import z( // opening selection
  , // zebra comment
    zebra
  , // alpha comment
    alpha
  , { // label comment
      field}
  , op _ // operator comment
      + _
  // list closer
  );
import a as a;
"#;
        let module = parse(source).unwrap();
        let formatted = pretty_module(&module);
        assert!(
            formatted
                .contains("import a as a;\n// statement\nimport z(\n  // alpha comment\n  , alpha"),
            "{formatted}"
        );
        assert!(
            formatted.contains("  // operator comment\n  , op _ + _"),
            "{formatted}"
        );
        assert!(
            formatted.contains("  // opening selection\n  // zebra comment\n  , zebra"),
            "{formatted}"
        );
        assert!(
            formatted.contains("  // label comment\n  , {field}"),
            "{formatted}"
        );
        assert!(
            formatted.contains("\n  // list closer\n  );"),
            "{formatted}"
        );
        for comment in [
            "statement",
            "opening selection",
            "alpha comment",
            "zebra comment",
            "operator comment",
            "label comment",
            "list closer",
        ] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        assert_eq!(pretty_module(&parse(&formatted).unwrap()), formatted);
    }

    #[test]
    fn selective_import_repeated_comma_comments_keep_the_next_item() {
        let source = r#"module c;
import p( // opening selection
  , // zebra
    zebra
  // next slot
  , // alpha
  , alpha,
  // dangling
  );
"#;
        let formatted = roundtrip(source);
        assert!(
            formatted.contains("import p(\n  // next slot\n  // alpha\n  , alpha"),
            "{formatted}"
        );
        assert!(
            formatted.contains("  // opening selection\n  // zebra\n  , zebra"),
            "{formatted}"
        );
        assert!(formatted.contains("\n  // dangling\n  );"), "{formatted}");
        for comment in [
            "opening selection",
            "next slot",
            "// alpha",
            "// zebra",
            "dangling",
        ] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
    }

    #[test]
    fn selective_import_width_and_comment_paths_indent_the_closer() {
        for source in [
            "module c; import p(first_long_selected_name, second_long_selected_name, third_long_selected_name, fourth_long_selected_name);",
            "module c; import p(, // attached\n short);",
        ] {
            let formatted = roundtrip(source);
            assert!(formatted.contains("\n  , "), "{formatted}");
            assert!(formatted.contains("\n  );"), "{formatted}");
            assert!(!formatted.contains("\n);"), "{formatted}");
        }
        assert!(roundtrip("module c; import p(short);").contains("import p(short);"));
    }

    #[test]
    fn variadic_import_delimiter_comments_belong_to_one_selection() {
        for (open, close) in [("[%", "%]"), ("[<...", "...<]")] {
            let source = format!("module c; import p(varop {open} // delimiter\n {close}, beta);");
            let module = parse(&source).unwrap();
            let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
                panic!("selective import");
            };
            assert_eq!(items.len(), 2);
            let ImportItem::OperatorPattern { grammar, .. } = &items[0] else {
                panic!("variadic selection");
            };
            let formatted = pretty_module(&module);
            assert!(
                formatted.contains(&format!("  // delimiter\n  , varop {open} {close}")),
                "{formatted}"
            );
            assert_eq!(formatted.matches("// delimiter").count(), 1, "{formatted}");
            let reparsed = parse(&formatted).unwrap();
            let ImportKind::Selective { items, .. } = &reparsed.imports[0].kind else {
                panic!("selective import");
            };
            assert_eq!(items.len(), 2);
            let ImportItem::OperatorPattern { grammar: after, .. } = &items[1] else {
                panic!("sorted variadic selection");
            };
            assert_eq!(after, grammar);
            assert_eq!(pretty_module(&reparsed), formatted);
        }
    }

    /// Assert that `pretty(parse(src))` is idempotent under `parse → pretty`.
    /// Returns the canonical form (the first pretty-print output) for
    /// inspection by callers that want to make further claims about it.
    fn roundtrip(src: &str) -> String {
        let m1 = parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        let pp1 = pretty_module(&m1);
        let m2 = parse(&pp1)
            .unwrap_or_else(|e| panic!("parse of pretty output failed: {e:?}\noutput was:\n{pp1}"));
        let pp2 = pretty_module(&m2);
        assert_eq!(
            pp1, pp2,
            "pretty-printer is not idempotent under parse for input:\n{src}"
        );
        pp1
    }

    #[test]
    fn existential_let_preserves_source_spelling_and_owned_comments() {
        let source = "module main;
            fn open() -> . {
                // opening
                let .(<Hidden> payload) =
                    // source
                    rhs;
                // continuation
                payload
            }";
        let printed = roundtrip(source);
        assert!(printed.contains("let .(<Hidden> payload) ="), "{printed}");
        for comment in ["// opening", "// source", "// continuation"] {
            assert_eq!(printed.matches(comment).count(), 1, "{printed}");
        }
        assert!(printed.find("// opening") < printed.find("let .("));
        assert!(printed.find("// source") < printed.find("rhs;"));
        assert!(printed.find("rhs;") < printed.find("// continuation"));
        let reparsed = parse(&printed).expect("formatted opening parses");
        let Expr::Call {
            callee, args, meta, ..
        } = function_body(&reparsed, "open")
        else {
            panic!("opening retains ordinary call representation");
        };
        assert!(
            matches!(callee.as_ref(), Expr::Path { segments, .. } if segments.join(".") == "rhs")
        );
        let CallArg::Value(Expr::FnExpr {
            sig,
            body,
            meta: continuation,
            ..
        }) = &args[1]
        else {
            panic!("opening retains continuation");
        };
        assert_eq!(meta.span, continuation.span);
        assert!(matches!(&sig.params[0], SignatureParam::Type(param) if param.name == "Hidden"));
        assert!(matches!(&sig.params[1], SignatureParam::Value(param) if param.name == "payload"));
        assert!(
            matches!(body.as_ref(), Expr::Path { segments, .. } if segments.join(".") == "payload")
        );
    }

    #[test]
    fn existential_let_nested_openings_and_ordinary_call_inverse() {
        let printed = roundtrip(
            "module main; fn open() -> . {
            let ordinary = seed;
            let .(<Left> <Right> pair) = unpack(ordinary);
            let .(<Hidden> value) = unpack_again(pair);
            value
        }",
        );
        assert!(
            printed.contains("let .(<Left> <Right> pair) = unpack(ordinary);"),
            "{printed}"
        );
        assert!(
            printed.contains("let .(<Hidden> value) = unpack_again(pair);"),
            "{printed}"
        );
        for source in [
            "module main; fn explicit() -> . { rhs(_, .[Hidden](payload) { payload }) }",
            "module main; fn explicit() -> . { (rhs)(_, .[Hidden](payload) { payload }) }",
            "module main; fn explicit() -> . {
                // authored call
                rhs(_, .[Hidden](payload) { let local = payload; local })
            }",
        ] {
            let printed = roundtrip(source);
            assert!(!printed.contains("let .(<"), "{printed}");
            assert!(printed.contains("rhs("), "{printed}");
            assert!(printed.contains(".[Hidden](payload)"), "{printed}");
            if source.contains("// authored call") {
                assert_eq!(printed.matches("// authored call").count(), 1, "{printed}");
            }
        }
    }

    #[test]
    fn existential_let_typed_patterns_preserve_spelling_and_owned_comments() {
        for pattern in [
            "(left: Hidden, right: Hidden)",
            "(left: _, right: _)",
            "((left: Hidden, _: Hidden), right: Hidden)",
            "(whole: (left: Hidden, right: Hidden), _: Hidden)",
        ] {
            let source = format!(
                "module main; fn open() -> . {{
                // opening
                let .(<Hidden> {pattern}) =
                    // source
                    rhs;
                // continuation
                left
            }}"
            );
            let printed = roundtrip(&source);
            assert!(
                printed.contains(&format!("let .(<Hidden> {pattern}) =")),
                "{printed}"
            );
            for comment in ["// opening", "// source", "// continuation"] {
                assert_eq!(printed.matches(comment).count(), 1, "{printed}");
            }
            let reparsed = parse(&printed).expect("typed opening reparses");
            let Expr::Call { args, .. } = function_body(&reparsed, "open") else {
                panic!("typed opening retains ordinary call representation");
            };
            let CallArg::Value(Expr::FnExpr { sig, body, .. }) = &args[1] else {
                panic!("typed opening retains continuation");
            };
            assert!(
                matches!(&sig.params[1], SignatureParam::Value(param) if param.pattern.is_some())
            );
            assert!(
                matches!(body.as_ref(), Expr::Path { segments, .. } if segments.join(".") == "left")
            );
        }
        for source in [
            "module main; fn explicit() -> . { rhs(_, .[Hidden]((left: Hidden, right: Hidden)) { left }) }",
            "module main; fn explicit() -> . { (rhs)(_, .[Hidden]((left: _, right: _)) { left }) }",
        ] {
            let printed = roundtrip(source);
            assert!(!printed.contains("let .(<"), "{printed}");
            assert!(printed.contains("rhs("), "{printed}");
        }
        assert!(
            parse("module main; fn open() -> . { let .(<Hidden> (left, right)) = rhs; left }")
                .is_err()
        );
    }

    #[test]
    fn pattern_disambiguating_written_inference_annotations_are_preserved() {
        let printed = roundtrip(
            "module main; fn open() -> . {
            let .(whole: (left: _, right: _)) = rhs;
            let .(first, second) = whole;
            first
        }",
        );
        assert!(
            printed.contains("let .(whole: (left: _, right: _)) = rhs;"),
            "{printed}"
        );
        assert!(
            printed.contains("let .(first, second) = whole;"),
            "{printed}"
        );
    }

    fn function_body<'a>(module: &'a Module, name: &str) -> &'a Expr {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(&def.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("function `{name}` not found"))
    }

    fn op_topology(expr: &Expr) -> String {
        match expr {
            Expr::Path { segments, .. } => segments.join("."),
            Expr::Call { callee, args, .. } => {
                let args = args
                    .iter()
                    .map(|arg| match arg {
                        CallArg::Value(value) => op_topology(value),
                        CallArg::Type(ty) => format!("type({ty:?})"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("call[{}]({args})", op_topology(callee))
            }
            Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { pattern, slots },
                ..
            } => {
                let pattern = pattern
                    .iter()
                    .map(|part| match part {
                        OpPart::SlotPlain { .. } => "_".to_owned(),
                        OpPart::SlotRecursive { .. } => "__".to_owned(),
                        OpPart::SlotGreedy { .. } => "___".to_owned(),
                        OpPart::Token { content, .. } => format!("`{content}`"),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let slots = slots.iter().map(op_topology).collect::<Vec<_>>().join(", ");
                format!("normal[{pattern}]({slots})")
            }
            Expr::OpChain {
                kind:
                    crate::ast::OpChainKind::Variadic {
                        open_tokens,
                        close_tokens,
                        elements,
                        ..
                    },
                ..
            } => {
                let elements = elements
                    .iter()
                    .map(op_topology)
                    .collect::<Vec<_>>()
                    .join("; ");
                format!(
                    "variadic[{}; {}]({elements})",
                    open_tokens.join(" "),
                    close_tokens.join(" ")
                )
            }
            other => panic!("expected operator-chain topology node, got {other:?}"),
        }
    }

    #[test]
    fn operator_boundary_comments_preserve_order_and_topology() {
        for (pattern, expression) in [
            ("_ + _", "left // marker1\n + right"),
            ("__ + _", "left // marker1\n + middle // marker2\n + right"),
            ("_ + __", "left // marker1\n + middle // marker2\n + right"),
            ("__ ++", "left // marker1\n ++ // marker2\n ++"),
            ("_ + ~ _", "left // marker1\n + // marker2\n ~ right"),
            ("_ + _", "(left // marker1\n) // marker2\n + right"),
            ("_ + _", "call( // marker1\n left) // marker2\n + right"),
            ("_ + _", "(left + middle) // marker1\n + right"),
            ("_ + _", "left // marker1\n\n // marker2\n + right"),
        ] {
            let source = format!(
                "module sample;\nop {pattern} {{ impl pick }}\nfn example() -> . {{ {expression} }}"
            );
            let before = parse(&source).unwrap();
            let output = roundtrip(&source);
            let after = parse(&output).unwrap();
            assert_eq!(
                op_topology(function_body(&before, "example")),
                op_topology(function_body(&after, "example")),
                "operator topology changed for {source}"
            );
            let comments = |text: &str| {
                text.lines()
                    .filter_map(|line| {
                        line.split_once("//")
                            .map(|(_, body)| body.trim().to_owned())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&output), "{output}");
        }
    }

    fn assert_op_expr_layout_and_topology(
        module_prefix: &str,
        function_head: &str,
        function_name: &str,
        expression: &str,
        width: usize,
        expected: &str,
    ) {
        let source = format!("{module_prefix} {function_head} {{ {expression} }}");
        let before = parse(&source).unwrap_or_else(|e| panic!("parse failed: {e:?}\n{source}"));
        let before_topology = op_topology(function_body(&before, function_name));
        let rendered = render(&doc_expr(function_body(&before, function_name)), width);
        assert_eq!(rendered, expected);

        let reparsed_source = format!("{module_prefix} {function_head} {{ {rendered} }}");
        let after = parse(&reparsed_source)
            .unwrap_or_else(|e| panic!("formatted parse failed: {e:?}\n{reparsed_source}"));
        assert_eq!(
            op_topology(function_body(&after, function_name)),
            before_topology,
            "operator layout changed the parsed topology"
        );
    }

    // ---- top-level / module --------------------------------------------

    #[test]
    fn empty_module() {
        let pp = roundtrip("module foo;");
        assert_eq!(pp, "module foo;\n");
    }

    #[test]
    fn nested_module_path() {
        roundtrip("module pkg/utils/string;");
    }

    #[test]
    fn pure_function_and_rec_modifiers_render_canonical_order() {
        let pp = roundtrip(
            "module x; \
             pure pub fn f() -> . { () } \
             pub type T = .; \
             pub newtype N : . { pub constructor mk_n; pub projector un_n; }; \
             pub labels L = { a: . }; \
             pub elab checked : [A] A -> A { impl checked_impl; }; \
             pub op _ + _ { impl plus; }; \
             pub varop [* *] { foldr append empty; }; \
             rec(loop) pub fn g() -> . { () }",
        );
        assert!(pp.contains("pub pure fn f() -> ."));
        assert!(pp.contains("pub type T = .;"));
        assert!(pp.contains("pub newtype N : ."));
        assert!(pp.contains("pub labels L ="));
        assert!(pp.contains("pub elab checked :"));
        assert!(pp.contains("pub op _ + _ { impl plus }"));
        assert!(pp.contains("pub varop [* *] { foldr append empty }"));
        assert!(pp.contains("pub rec(loop) fn g() -> ."));
    }

    #[test]
    fn fills_elaborator_schedule_has_a_stable_canonical_spelling() {
        let pp = roundtrip("module x; elab demo : . -> . { impl (fills) demo_impl; };");
        assert!(pp.contains("{ impl(fills) demo_impl }"), "got:\n{pp}");
    }

    #[test]
    fn declaration_callable_paths_round_trip_all_dotted_shapes() {
        let pp = roundtrip(
            "module x; \
             op _ + _ { impl helpers.add; }; \
             varop [* *] { \
               foldr helpers.Box.push Box.empty; \
               finalize finish; \
             }; \
             elab demo : . -> . { impl(fills) helpers.demo_impl; };",
        );
        for expected in [
            "impl helpers.add }",
            "foldr helpers.Box.push Box.empty;",
            "finalize finish }",
            "impl(fills) helpers.demo_impl }",
        ] {
            assert!(pp.contains(expected), "missing {expected} in:\n{pp}");
        }
    }

    #[test]
    fn declaration_callable_path_comments_stay_once_in_source_order() {
        let source = r#"module x;
op _ + _ {
  impl helpers // op owner
    . // op separator
    add;
};
varop [* *] {
  foldr helpers // step owner
    .push helpers // base owner
    . // base separator
    empty // base path
    ;
  finalize helpers // finalize owner
    .finish;
};
elab demo : . -> . {
  impl( // schedule open
    fills // schedule marker
  ) helpers // elaborator owner
    .demo_impl;
};
"#;
        let out = roundtrip(source);
        let comments = [
            "// op owner",
            "// op separator",
            "// step owner",
            "// base owner",
            "// base separator",
            "// base path",
            "// finalize owner",
            "// schedule open",
            "// schedule marker",
            "// elaborator owner",
        ];
        let mut previous = 0;
        for comment in comments {
            assert_eq!(out.matches(comment).count(), 1, "got:\n{out}");
            let position = out.find(comment).unwrap();
            assert!(position >= previous, "comment order changed; got:\n{out}");
            previous = position;
        }
    }

    #[test]
    fn compact_declaration_keeps_body_comments_separate_from_item_docs() {
        let source = r#"module x;

/// declaration docs
elab demo : Box(
  // nested call type
  A
) -> . {
  impl
  /// interstitial implementation
  helpers.demo_impl;
};
"#;
        let parsed = parse(source).unwrap_or_else(|e| panic!("parse failed: {e:?}\n{source}"));
        let crate::ast::Item::Elaborator(elaborator, _) = &parsed.items[0] else {
            panic!("expected elaborator")
        };
        let original_doc = elaborator.doc.clone();
        assert_eq!(
            original_doc
                .as_ref()
                .expect("declaration doc")
                .lines
                .as_slice(),
            ["declaration docs"]
        );
        let original_body_comments = elaborator
            .body_trivia
            .iter()
            .filter_map(|trivia| match trivia {
                Trivia::LineComment { text, .. } => Some(("line", text.as_str())),
                Trivia::DocCommentLine { text, .. } => Some(("doc", text.as_str())),
                Trivia::Newline => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            original_body_comments,
            [
                ("line", " nested call type"),
                ("doc", "interstitial implementation"),
            ]
        );
        let once = pretty_module(&parsed);

        for comment in [
            "/// declaration docs",
            "// nested call type",
            "/// interstitial implementation",
        ] {
            assert_eq!(
                once.matches(comment).count(),
                1,
                "each source comment must have one formatter owner; got:\n{once}"
            );
        }
        let positions = [
            "/// declaration docs",
            "// nested call type",
            "/// interstitial implementation",
        ]
        .map(|comment| once.find(comment).expect("comment survives"));
        assert!(
            positions[0] < positions[1] && positions[1] < positions[2],
            "comment source order changed; got:\n{once}"
        );

        let reparsed =
            parse(&once).unwrap_or_else(|e| panic!("parse of pretty output failed: {e:?}\n{once}"));
        let crate::ast::Item::Elaborator(elaborator, _) = &reparsed.items[0] else {
            panic!("expected elaborator after reparse")
        };
        assert_eq!(elaborator.doc, original_doc);
        let reparsed_body_comments = elaborator
            .body_trivia
            .iter()
            .filter_map(|trivia| match trivia {
                Trivia::LineComment { text, .. } => Some(("line", text.as_str())),
                Trivia::DocCommentLine { text, .. } => Some(("doc", text.as_str())),
                Trivia::Newline => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reparsed_body_comments, original_body_comments);
        assert_eq!(
            pretty_module(&reparsed),
            once,
            "compact-declaration formatting must be immediately idempotent"
        );
    }

    #[test]
    fn variadic_modes_preserve_direction_and_seed_mode() {
        for (source, expected) in [
            (
                "module x/right; varop [* *] { foldr append empty; };",
                "foldr append empty }",
            ),
            (
                "module x/left; varop [* *] { foldl append_left empty; };",
                "foldl append_left empty }",
            ),
            (
                "module x/positive; varop [% %] { foldr1 push seed; };",
                "foldr1 push seed }",
            ),
        ] {
            assert!(roundtrip(source).contains(expected));
        }
    }

    #[test]
    fn singleton_rec_braces_expand_to_visibility_preserving_shorthand() {
        for (source, expected) in [
            (
                "module x/y; rec(loop) { fn local() -> . { () } }",
                "rec(loop) fn local() -> .",
            ),
            (
                "module x/y; rec(loop) { pub(x) fn scoped() -> . { () } }",
                "pub(x) rec(loop) fn scoped() -> .",
            ),
            (
                "module x/y; rec(loop) { pub fn exported() -> . { () } }",
                "pub rec(loop) fn exported() -> .",
            ),
        ] {
            let pp = roundtrip(source);
            assert!(pp.contains(expected), "got:\n{pp}");
            assert!(!pp.contains("rec(loop) {"), "got:\n{pp}");
        }
    }

    #[test]
    fn singleton_rec_modifier_orders_share_one_canonical_form() {
        for source in [
            "module x/y; pub(x) rec(loop) fn f() -> . { () }",
            "module x/y; rec(loop) pub(x) fn f() -> . { () }",
        ] {
            let pp = roundtrip(source);
            assert!(pp.contains("pub(x) rec(loop) fn f() -> ."), "got:\n{pp}");
        }
    }

    #[test]
    fn singleton_rec_signature_keeps_visibility_before_rec() {
        let module = parse("module x/y; rec(loop) { pub(x) fn f(value: .) -> . { value } }")
            .expect("fixture parses");
        assert_eq!(
            pretty_item_signature(&module.items[0]),
            "pub(x) rec(loop) fn f(value: .) -> ."
        );
    }

    // ---- import statements ---------------------------------------------

    #[test]
    fn import_selective_single() {
        roundtrip("module x; import a/b(foo);");
    }

    #[test]
    fn import_selective_multi() {
        roundtrip("module x; import pkg/mod(foo, bar, baz);");
    }

    #[test]
    fn import_selective_label_namespace() {
        roundtrip("module x; import pkg/mod(Item, item, {item});");
    }

    #[test]
    fn import_qualified_multi_segment() {
        roundtrip("module x; import a/b/c as m;");
    }

    #[test]
    fn import_qualified_single_segment() {
        roundtrip("module x; import a as m;");
    }

    #[test]
    fn import_selective_round_trips() {
        roundtrip("module x; import pkg/mod(Foo, Bar);");
        roundtrip("module x; import match(match);");
    }

    #[test]
    fn import_intrinsics() {
        roundtrip("module x; import __intrinsics__;");
    }

    // ---- fn ----------------------------------------------------------

    #[test]
    fn fn_def_zero_arg_unit() {
        roundtrip("module x; fn main() -> . { () }");
    }

    #[test]
    fn fn_def_pub() {
        roundtrip("module x; pub fn main() -> . { () }");
    }

    #[test]
    fn fn_def_identity_function() {
        roundtrip("module x; pub fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn fn_def_comma_binder_group_canonicalizes() {
        let pp = roundtrip("module x; pub fn pair[A, B](x: A, y: B) -> A { x }");
        assert!(
            pp.contains("pub fn pair[A][B](x: A, y: B) -> A { x }"),
            "got:\n{pp}"
        );
    }

    #[test]
    fn fn_def_intermixed_params() {
        roundtrip("module x; fn f[A](x: A)[B](y: B) -> A { x }");
    }

    #[test]
    fn fn_def_elided_unit_return_round_trips() {
        // language.md § Function definitions: `.> ()` may be omitted.
        // The pretty printer must NOT re-emit the elided arrow, so the
        // source round-trips byte-for-byte.
        roundtrip("module x; fn say_hi() { () }");
    }

    #[test]
    fn fn_def_explicit_unit_return_round_trips() {
        // The dual: an explicit `-> .` must come back out as `-> .`.
        roundtrip("module x; fn say_hi() -> . { () }");
    }

    #[test]
    fn op_call_multi_token_run_keeps_token_separator() {
        roundtrip(
            "module x; newtype A : . { constructor mk_a; projector un_a; }; \
             fn cat(a: A, b: A) -> A { a } \
             op _ && ++ _ { impl cat; }; \
             fn main(x: A, y: A) -> A { x && ++ y }",
        );
    }

    #[test]
    fn operator_forms_round_trip_with_run_boundaries() {
        let fused = roundtrip(
            "module fused; fn pick(a: A, b: A) -> A { a } \
             op _ <! __ !> { impl pick; }; \
             fn run(a: A, b: A) -> A { a <! b !> }",
        );
        assert!(fused.contains("op _ <! __ !> { impl pick }"));
        assert!(fused.contains("a <! b !>"));

        let separated = roundtrip(
            "module separated; fn pick(a: A, b: A) -> A { a } \
             op _ < ! __ ! > { impl pick; }; \
             fn run(a: A, b: A) -> A { a < ! b ! > }",
        );
        assert!(separated.contains("op _ < ! __ ! > { impl pick }"));
        assert!(separated.contains("a < ! b ! >"));

        let fold = roundtrip(
            "module folded; fn empty() -> A { value } \
             fn push(a: A, b: A) -> A { a } \
             varop [! !] { foldr push empty; }; \
             fn run(a: A, b: A) -> A { [! a, b !] }",
        );
        assert!(fold.contains("varop [! !] { foldr push empty }"));
        assert!(fold.contains("[! a, b !]"));

        let import_source = "module imported; import syntax(op _ < !, op _ <!, op < ! _, op <! _);";
        let first =
            crate::pass::parser::parse_lazy(import_source).expect("operator-import header parses");
        let rendered = pretty_module(first.module());
        assert!(rendered.contains("import syntax(op < ! _, op <! _, op _ < !, op _ <!);"));
        crate::pass::parser::parse_lazy(&rendered)
            .expect("formatted operator-import header reparses");
    }

    #[test]
    fn op_chain_layout_preserves_right_left_and_greedy_topology() {
        for pattern in ["_ + __", "__ + _", "_ + ___"] {
            let module_prefix = format!(
                "module x; fn combine(a: A, b: A) -> A {{ a }} \
                 op {pattern} {{ impl combine; }};"
            );
            assert_op_expr_layout_and_topology(
                &module_prefix,
                "fn use_op() -> A",
                "use_op",
                "a + b + c",
                8,
                "a\n  + b\n  + c",
            );
            assert_op_expr_layout_and_topology(
                &module_prefix,
                "fn flat_op() -> A",
                "flat_op",
                "a + b + c",
                80,
                "a + b + c",
            );
        }
    }

    #[test]
    fn op_chain_layout_preserves_prefix_and_postfix_topology() {
        for (pattern, expression, expected) in [
            ("~ __", "~ ~ ~ value", "~\n  ~\n  ~ value"),
            ("__ ~", "value ~ ~ ~", "value\n  ~\n  ~\n  ~"),
        ] {
            let module_prefix = format!(
                "module x; fn unary(value: A) -> A {{ value }} \
                 op {pattern} {{ impl unary; }};"
            );
            assert_op_expr_layout_and_topology(
                &module_prefix,
                "fn use_op() -> A",
                "use_op",
                expression,
                10,
                expected,
            );
        }
    }

    #[test]
    fn op_chain_layout_preserves_higher_arity_matched_pair_and_token_runs() {
        for (implementation, implementation_name, pattern, expression, width, expected) in [
            (
                "fn choose(a: A, b: A, c: A) -> A { a }",
                "choose",
                "_ ? _ : __",
                "a ? b : c ? d : e",
                10,
                "a\n  ? b\n  : c\n  ? d\n  : e",
            ),
            (
                "fn index(a: A, b: A) -> A { a }",
                "index",
                "_ < __ >",
                "a < b < c > >",
                10,
                "a\n  < b\n  < c\n  >\n  >",
            ),
            (
                "fn combine(a: A, b: A) -> A { a }",
                "combine",
                "_ && ++ __",
                "a && ++ b && ++ c",
                12,
                "a\n  && ++ b\n  && ++ c",
            ),
        ] {
            let module_prefix = format!(
                "module x; {implementation} \
                 op {pattern} {{ impl {implementation_name}; }};"
            );
            assert_op_expr_layout_and_topology(
                &module_prefix,
                "fn use_op() -> A",
                "use_op",
                expression,
                width,
                expected,
            );
        }
    }

    #[test]
    fn op_chain_layout_keeps_different_pattern_parentheses() {
        let module_prefix = "module x; \
             fn add(a: A, b: A) -> A { a } \
             fn maybe(a: A) -> A { a } \
             op _ + __ { impl add; }; \
             op __ ? { impl maybe; };";
        assert_op_expr_layout_and_topology(
            module_prefix,
            "fn use_op() -> A",
            "use_op",
            "(a ?) + b + c",
            12,
            "(a ?)\n  + b\n  + c",
        );
    }

    #[test]
    fn op_chain_layout_anchors_nested_inline_expressions() {
        let normal_prefix =
            "module x; fn combine(a: A, b: A) -> A { a } op _ + __ { impl combine; };";
        assert_op_expr_layout_and_topology(
            normal_prefix,
            "fn nested_op() -> A",
            "nested_op",
            "wrap(a + b + c)",
            12,
            "wrap(a\n       + b\n       + c)",
        );

        let variadic_prefix = "module x; varop [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            variadic_prefix,
            "fn nested_list() -> A",
            "nested_list",
            "wrap([! a, b, c !])",
            12,
            "wrap([!\n  , a\n  , b\n  , c\n  !])",
        );
    }

    #[test]
    fn variadic_op_layout_preserves_elements_delimiters_and_topology() {
        let list_prefix = "module x; varop [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            list_prefix,
            "fn list() -> A",
            "list",
            "[! a, b, c !]",
            12,
            "[!\n  , a\n  , b\n  , c\n  !]",
        );
        assert_op_expr_layout_and_topology(
            list_prefix,
            "fn flat_list() -> A",
            "flat_list",
            "[! a, b, c !]",
            80,
            "[! a, b, c !]",
        );
        assert_op_expr_layout_and_topology(
            list_prefix,
            "fn empty_list() -> A",
            "empty_list",
            "[! !]",
            1,
            "[! !]",
        );
        assert_op_expr_layout_and_topology(
            list_prefix,
            "fn singleton_list() -> A",
            "singleton_list",
            "[! a !]",
            1,
            "[! a !]",
        );

        let entries_prefix =
            "module x; op _ => _ { impl pair; }; varop [% %] { foldr insert empty; };";
        assert_op_expr_layout_and_topology(
            entries_prefix,
            "fn entries() -> A",
            "entries",
            "[% a => b, c => d %]",
            16,
            "[%\n  , a => b\n  , c => d\n  %]",
        );

        let repeated_comma_prefix = "module x; varop [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            repeated_comma_prefix,
            "fn separated() -> A",
            "separated",
            "[! , a,, b,, c, !]",
            12,
            "[!\n  , a\n  , b\n  , c\n  !]",
        );

        assert_op_expr_layout_and_topology(
            entries_prefix,
            "fn broken_entry() -> A",
            "broken_entry",
            "[% key => build(alpha, beta, gamma), other => value %]",
            16,
            "[%\n  , key\n      => build(\n           , alpha\n           , beta\n           , gamma\n           )\n  , other\n      => value\n  %]",
        );
    }

    // ---- alias -----------------------------------------------------

    #[test]
    fn type_alias_nullary() {
        roundtrip("module x; type Logger = String -> .;");
    }

    #[test]
    fn type_alias_parametric_pub() {
        roundtrip("module x; pub type Pair[A][B] = (A & B);");
    }

    #[test]
    fn type_alias_comma_binder_group_canonicalizes() {
        let pp = roundtrip("module x; type F[A, B] = [A, B] A -> B;");
        assert!(pp.contains("type F[A][B] = [A][B] A -> B;"), "got:\n{pp}");
    }

    // ---- newtype -------------------------------------------------------

    #[test]
    fn newtype_nullary() {
        roundtrip(
            "module x; newtype Celsius : . { pub constructor mk_celsius; pub projector to_unit; };",
        );
    }

    #[test]
    fn newtype_recursive() {
        roundtrip(
            "module x; rec newtype List[A] : (. | (A & List(A))) { \
             pub constructor cons; pub projector un_list; };",
        );
    }

    #[test]
    fn newtype_with_single_existential_header_roundtrip() {
        // Existential binders live in the newtype header, emitted as
        // `<U>` atoms between the universal-parameter list and `:`.
        roundtrip(
            "module x; newtype Pack[A] <U> : (A & U) { \
             pub constructor mk_pack; pub projector un_pack; };",
        );
    }

    #[test]
    fn newtype_with_multi_existential_header_roundtrip() {
        // Multi-binder existential header — whitespace-separated atoms.
        roundtrip(
            "module x; newtype Box <A> <B> : (A & B) { \
             pub constructor mk_box; pub projector un_box; };",
        );
    }

    #[test]
    fn newtype_member_comments_follow_canonical_member_order() {
        let pp = roundtrip(
            "module x; newtype Box : . { \
             // projector comment\n\
             projector get; \
             // constructor comment\n\
             constructor mk; \
             };",
        );
        let constructor_comment = pp.find("// constructor comment").unwrap();
        let constructor = pp.find("constructor mk;").unwrap();
        let projector_comment = pp.find("// projector comment").unwrap();
        let projector = pp.find("projector get").unwrap();
        assert!(
            constructor_comment < constructor
                && constructor < projector_comment
                && projector_comment < projector,
            "expected member comments to follow constructor/projector order; got:\n{pp}"
        );
    }

    // ---- labels -------------------------------------------------------

    #[test]
    fn label_forward_round_trip_preserves_visibility_and_exact_path() {
        let formatted = roundtrip("module x; pub(x) type { local } = { provider . original };");
        assert!(formatted.contains("pub(x) type {local} = {provider.original};"));
        assert!(!formatted.contains("newtype"));
    }

    #[test]
    fn label_forward_interstitial_comments_survive_once_in_order() {
        let formatted = roundtrip(
            "module x;\n/// Forwarded family.\npub type {\n// local binding\nlocal\n// after local\n} = {\n// target module\nprovider.\n// target label\noriginal\n// after target\n};",
        );
        let comments = [
            "// local binding",
            "// after local",
            "// target module",
            "// target label",
            "// after target",
        ];
        let mut previous = 0;
        for comment in comments {
            assert_eq!(formatted.matches(comment).count(), 1);
            let position = formatted.find(comment).expect("preserved comment");
            assert!(position >= previous);
            previous = position;
        }
        assert_eq!(formatted.matches("/// Forwarded family.").count(), 1);
        assert!(formatted.contains("pub type {local} = {"));
        assert!(formatted.contains("provider.original"));
    }

    #[test]
    fn label_forward_keeps_interior_doc_comments_separate_from_declaration_docs() {
        for external_doc in ["", "/// External family documentation.\n"] {
            let source = format!(
                "module x;\n{external_doc}type {{\n/// Interior binding note.\nlocal}} = {{provider.\n/// Interior target note.\noriginal}};"
            );
            let parsed = parse(&source).expect("parse source");
            let Item::LabelForward(before, ()) = &parsed.items[0] else {
                panic!("expected label forwarding declaration");
            };
            assert_eq!(before.doc.is_some(), !external_doc.is_empty());
            let formatted = pretty_module(&parsed);
            let reparsed = parse(&formatted).expect("parse formatted declaration");
            let Item::LabelForward(after, ()) = &reparsed.items[0] else {
                panic!("expected label forwarding declaration after formatting");
            };
            assert_eq!(
                before.doc.as_ref().map(|doc| &doc.lines),
                after.doc.as_ref().map(|doc| &doc.lines),
            );
            let comment_bodies = |trivia: &[Trivia]| {
                trivia
                    .iter()
                    .filter_map(|item| match item {
                        Trivia::LineComment { text, .. } => Some((false, text.clone())),
                        Trivia::DocCommentLine { text, .. } => Some((true, text.clone())),
                        Trivia::Newline => None,
                    })
                    .collect::<Vec<_>>()
            };
            let before_comments = comment_bodies(&before.body_trivia);
            assert_eq!(before_comments.len(), 2);
            assert!(before_comments.iter().all(|(is_doc, _)| *is_doc));
            assert_eq!(before_comments, comment_bodies(&after.body_trivia));
            assert_eq!(formatted, pretty_module(&reparsed));
        }
    }

    #[test]
    fn labels_anonymous_round_trip() {
        roundtrip("module x; labels { foo : . };");
    }

    #[test]
    fn labels_anonymous_multi_round_trip() {
        roundtrip("module x; labels { foo : ., bar : I32 };");
    }

    #[test]
    fn labels_named_round_trip() {
        roundtrip("module x; labels T = { foo : . };");
    }

    #[test]
    fn labels_named_parametric_round_trip() {
        roundtrip("module x; labels T[A] = { foo[A] : A };");
    }

    #[test]
    fn labels_reuse_marker_round_trip() {
        roundtrip(
            "module x; labels { value[*F][A] : F(A) }; labels Choice[*G][B] = { value[*G][B] : _, other : . };",
        );
    }

    #[test]
    fn labels_pub_round_trip() {
        roundtrip("module x; pub labels { foo : I32, bar : Bool };");
    }

    #[test]
    fn labels_recursive_round_trip() {
        roundtrip("module x; rec labels { list[A] : (. | (A & List(A))) };");
    }

    #[test]
    fn recursive_type_group_round_trip() {
        let formatted = roundtrip(
            "module x; rec { pub type A = B; pub newtype B : A { pub constructor mk_b; pub projector un_b; }; }",
        );
        assert!(formatted.contains("rec {\n  pub type A = B;"));
    }

    #[test]
    fn recursive_type_group_member_and_closer_comments_round_trip() {
        let formatted = roundtrip(
            "module x; rec {\n\
             // group-leading context\n\
             /// Alias docs.\n\
             pub type A = B;\n\
             /// Nominal docs.\n\
             pub newtype B : A { pub constructor mk_b; pub projector un_b; };\n\
             // group-trailing context\n\
             }",
        );
        for preserved in [
            "// group-leading context",
            "/// Alias docs.",
            "/// Nominal docs.",
            "// group-trailing context",
        ] {
            assert!(
                formatted.contains(preserved),
                "lost {preserved:?}:\n{formatted}"
            );
        }
    }

    // ---- equiv ---------------------------------------------------------

    #[test]
    fn equiv_two_term_round_trip() {
        roundtrip(
            "module x; fn id[A](v: A) -> A { v } \
             equiv id_id[A](x: A) { id(x); id(id(x)) }",
        );
    }

    #[test]
    fn equiv_three_term_round_trip() {
        roundtrip("module x; equiv three { (); (); () }");
    }

    #[test]
    fn equiv_no_params_round_trip() {
        roundtrip("module x; equiv simple { (); () }");
    }

    #[test]
    fn equiv_extra_semicolons_canonicalize() {
        let pp = roundtrip("module x; equiv simple { ;; ();;; ();;; }");
        assert!(
            pp.contains("equiv simple() {\n  ();\n  ()\n}"),
            "got:\n{pp}"
        );
        assert!(!pp.contains(";;"), "got:\n{pp}");
    }

    #[test]
    fn equiv_clause_comment_survives_round_trip() {
        let pp = roundtrip("module x; equiv simple { (); // second\n () }");
        assert!(
            pp.contains("equiv simple() {\n  ();\n  // second\n  ()\n}"),
            "expected comment before second equiv arm; got:\n{pp}"
        );
    }

    // ---- type expressions ----------------------------------------------

    #[test]
    fn ordinary_and_marked_type_names_round_trip_identically() {
        let output = roundtrip(
            "module x; type Alias[A] = A; type _Alias[_A] = _A; \
             newtype Box[B] : B { constructor mk_box; projector un_box; }; \
             newtype _Box[_B] : _B { constructor mk_marked; projector un_marked; }; \
             fn id[T](x: T) -> T { x } \
             fn marked[_A](x: _A) -> _A { id(_A, x) }",
        );
        for retained in [
            "type _Alias[_A] = _A;",
            "newtype _Box[_B] : _B",
            "fn marked[_A](x: _A) -> _A",
            "id(_A, x)",
        ] {
            assert!(
                output.contains(retained),
                "formatter dropped or rewrote `{retained}`:\n{output}"
            );
        }
    }

    #[test]
    fn type_function_zero_param() {
        roundtrip("module x; type F = . -> Int;");
    }

    #[test]
    fn type_function_zero_param_after_binder_round_trips() {
        roundtrip("module x; type F = [T] . -> T;");
    }

    #[test]
    fn compact_forall_runs_remain_compact_after_formatting() {
        let output = roundtrip(
            "module x; type Poly = [A][*F] F(A) -> A; \
             fn make() -> . { .[*F](x: F(.)) -> F(.) { x } }",
        );
        assert!(output.contains("[A][*F] F(A) -> A"));
        assert!(output.contains(".[*F](x: F(.)) -> F(.)"));
    }

    #[test]
    fn placeholder_stems_and_shadowing_round_trip() {
        for expression in [
            ".x. { pair(x2, x1, x2) }",
            ".arg. { let arg2 = arg1; arg2 }",
            "._x. { _x1.>apply(._y. { _y1 }) }",
        ] {
            let output = roundtrip(&format!("module m; fn make() {{ {expression} }}"));
            assert!(output.contains(expression), "{output}");
        }
    }

    #[test]
    fn type_function_grouped_unit_domain_canonicalizes() {
        let pp = roundtrip("module x; type F = (.) -> Int;");
        assert!(pp.contains("type F = . -> Int;"), "got:\n{pp}");
    }

    #[test]
    fn type_function_grouped_atomic_domain_canonicalizes() {
        let pp = roundtrip("module x; type F = (A) -> Int;");
        assert!(pp.contains("type F = A -> Int;"), "got:\n{pp}");
    }

    #[test]
    fn type_function_multi_param() {
        roundtrip("module x; type F = (Int & Bool & String) -> .;");
    }

    #[test]
    fn type_function_curried_return_stays_curried() {
        roundtrip("module x; type F = A -> B -> C;");
    }

    #[test]
    fn type_alias_mixed_signature_binders_round_trip() {
        roundtrip("module x; type F = A -> [B] B -> [C] C;");
    }

    #[test]
    fn type_alias_trailing_signature_binder_round_trip() {
        roundtrip("module x; type F = A -> [B] B;");
    }

    #[test]
    fn value_param_rank_n_type_round_trips() {
        roundtrip("module x; fn use[A](f: A -> [B] B -> A) -> . { () }");
    }

    #[test]
    fn type_unit_and_bottom() {
        roundtrip("module x; type U = .; type Z = !;");
    }

    #[test]
    fn type_sum_and_product() {
        roundtrip("module x; type S = (A | B); type P = (A & B);");
    }

    #[test]
    fn type_product_chain_three_round_trip() {
        let pp = roundtrip("module x; type T = (A & B & C);");
        assert!(
            pp.contains("A & B & C") && !pp.contains("(A & "),
            "expected bare chain form, got:\n{pp}"
        );
    }

    #[test]
    fn type_sum_chain_three_round_trip() {
        let pp = roundtrip("module x; type T = (A | B | C);");
        assert!(
            pp.contains("A | B | C") && !pp.contains("(A | "),
            "expected bare chain form, got:\n{pp}"
        );
    }

    #[test]
    fn type_left_leaning_keeps_explicit_parens() {
        // `(A & B) & C` is structurally distinct from `A & B & C`
        // (left-leaning vs right-leaning). The printer flattens
        // right-leaning runs, so the inner left-leaning chain stays
        // wrapped in load-bearing parens to preserve the AST shape.
        let pp = roundtrip("module x; type T = ((A & B) & C);");
        assert!(pp.contains("(A & B) & C"), "got:\n{pp}");
    }

    #[test]
    fn type_bare_chain_two_elements_round_trip() {
        // No-paren input round-trips identically with the printer's
        // bare-chain emission — both inputs collapse to the same
        // AST.
        let pp = roundtrip("module x; type T = A & B;");
        assert!(
            pp.contains("type T = A & B;"),
            "expected bare chain output, got:\n{pp}"
        );
    }

    #[test]
    fn type_chain_mixing_prints_with_load_bearing_parens() {
        // `(A & B) | C` keeps inner parens because the inner chain
        // is a child of the outer (different-op) chain.
        let pp = roundtrip("module x; type T = ((A & B) | C);");
        assert!(
            pp.contains("(A & B) | C"),
            "expected mixing parens preserved, got:\n{pp}"
        );
    }

    #[test]
    fn type_chain_function_child_keeps_parens() {
        // A function-type child of a `|` chain needs surrounding
        // parens, otherwise `A -> B | C` reparses as a function
        // returning a sum.
        let pp = roundtrip("module x; type T = ((A -> B) | C);");
        assert!(
            pp.contains("(A -> B) | C"),
            "expected function parens preserved, got:\n{pp}"
        );
    }

    #[test]
    fn type_function_param_chain_no_extra_parens() {
        // The function-arrow's parameter list already supplies
        // `(...)` for its single chain parameter; no extra parens
        // around the chain itself.
        let pp = roundtrip("module x; type T = (A & B) -> C;");
        assert!(
            pp.contains("(A & B) -> C"),
            "expected single layer of parens for fn-param chain, got:\n{pp}"
        );
    }

    #[test]
    fn type_alias_wide_sum_chain_breaks_to_leading_op_layout() {
        // A type-alias body that's a wide `|` chain breaks to the
        // leading-operator multi-line layout: `=` ends the head
        // line, items at +2 prefixed with `| `, `;` on its own
        // line at +2.
        let src = "module x; type Event = Click_event_kind_one_pretty_long_name_here \
                   | Hover_event_kind_two_also_pretty_long | Focus_event_kind_three_lengthy \
                   | Drag_event_kind_four_yet_more;";
        let pp = roundtrip(src);
        assert!(
            pp.contains("type Event =\n  | Click_event_kind_one_pretty_long_name_here"),
            "expected leading-op break for first chain item; got:\n{pp}"
        );
        assert!(
            pp.contains("\n  ;"),
            "expected trailing `;` on its own line at +2; got:\n{pp}"
        );
    }

    #[test]
    fn newtype_wide_chain_payload_breaks_to_leading_op_layout() {
        // A newtype payload that's a wide `|` chain breaks to the
        // leading-operator layout: `:` ends the head's line, the
        // chain items at +2, `{` lands on its own line at the same
        // indent before the member-list block.
        let src = "module x; newtype Token : Int_lit_token_a_pretty_long_name \
                   | String_lit_token_b_also_pretty_long_indeed | Bool_lit_token_c_lengthy_too \
                   | Float_lit_token_d_yet_more { pub constructor mk_token; pub projector un_token; };";
        let pp = roundtrip(src);
        assert!(
            pp.contains("newtype Token :\n  | Int_lit_token_a_pretty_long_name"),
            "expected leading-op break for first chain item; got:\n{pp}"
        );
        assert!(
            pp.contains("\n  {"),
            "expected `{{` on its own line at +2; got:\n{pp}"
        );
    }

    #[test]
    fn fn_def_wide_chain_return_breaks() {
        let src = "module x; fn f() -> Long_event_kind_one_pretty_long_name \
                   | Hover_event_kind_two_pretty_long | Focus_event_kind_three_lengthy \
                   | Drag_event_kind_four_more_chars { f() }";
        let pp = roundtrip(src);
        assert!(
            pp.contains("fn f() ->\n  | Long_event_kind_one_pretty_long_name"),
            "expected leading-op break for fn return; got:\n{pp}"
        );
    }

    #[test]
    fn rec_call_annotations_print_in_canonical_order() {
        let pp = roundtrip("module x; rec(loop) fn f() -> . { rec(escape, cont, poly) f() }");
        assert!(
            pp.contains("rec(poly, cont, escape) f()"),
            "expected canonical rec-call annotation order; got:\n{pp}"
        );
    }

    #[test]
    fn fn_type_wide_chain_return_breaks() {
        // A function-type return that's a wide chain breaks to
        // the leading-operator multi-line layout inside the
        // function-type doc.
        let src = "module x; type F = A -> Long_event_kind_one_pretty_long_name \
                   | Hover_event_kind_two_pretty_long | Focus_event_kind_three_lengthy \
                   | Drag_event_kind_four_more_chars;";
        let pp = roundtrip(src);
        assert!(
            pp.contains("A ->\n  | Long_event_kind_one_pretty_long_name"),
            "expected leading-op break for fn-type return; got:\n{pp}"
        );
    }

    #[test]
    fn newtype_short_payload_stays_inline() {
        let pp = roundtrip(
            "module x; newtype Pair[A][B] : (A & B) \
             { pub constructor mk_pair; pub projector un_pair; };",
        );
        assert!(
            pp.contains(": A & B {"),
            "expected inline payload; got:\n{pp}"
        );
        assert!(
            !pp.contains("\n  & "),
            "did not expect chain break; got:\n{pp}"
        );
    }

    #[test]
    fn type_alias_short_chain_stays_inline() {
        // Short chain bodies stay inline: `type T = A & B;`.
        let pp = roundtrip("module x; type T = A & B;");
        assert!(
            pp.contains("type T = A & B;"),
            "expected inline chain; got:\n{pp}"
        );
        // No multi-line break for the short chain.
        assert!(!pp.contains("\n  & "), "did not expect break; got:\n{pp}");
    }

    #[test]
    fn type_function_return_chain_no_parens() {
        // A chain in the function-return position takes no wrapping
        // parens — `A -> B & C` parses to function returning the
        // chain.
        let pp = roundtrip("module x; type T = A -> B & C;");
        assert!(
            pp.contains("A -> B & C"),
            "expected bare chain in return, got:\n{pp}"
        );
    }

    // ---- value expressions ---------------------------------------------

    #[test]
    fn fn_round_trips_without_annotations() {
        roundtrip("module x; fn f() -> . { .(x) { x }(0) }");
    }

    #[test]
    fn let_round_trips_without_annotation() {
        roundtrip("module x; fn f() -> . { let x = (); x }");
    }

    #[test]
    fn dotted_path_value_position() {
        roundtrip("module x; fn f() -> . { Foo.mk_foo() }");
    }

    #[test]
    fn call_with_mixed_args() {
        roundtrip(r#"module x; fn f() -> . { id(String, "hi") }"#);
    }

    #[test]
    fn block_label_value_prefix_keeps_required_grouping() {
        let output = roundtrip("module x; fn f() -> . { do!({field = value}) { final_value } }");
        assert!(
            output.contains("do!({field = value}) {"),
            "label-value receiver lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn bare_literal_callee_keeps_required_grouping() {
        let output = roundtrip("module x; fn f() -> . { (\"literal\")(argument) }");
        assert!(
            output.contains("(\"literal\")(argument)"),
            "bare literal callee lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn composite_callee_keeps_required_grouping() {
        let output = roundtrip(
            "module x; \
             fn prefix_impl(value: Formtype) -> Formtype { value } \
             op - __ { impl prefix_impl; }; \
             fn operator_call() -> . { (- form_value)(context_arg) } \
             fn splice_call() -> . { (context_callee.<form_receiver)(context_arg) }",
        );
        assert!(
            output.contains("(- form_value)(context_arg)"),
            "operator callee lost its disambiguating parentheses:\n{output}"
        );
        assert!(
            output.contains("(context_callee.<form_receiver)(context_arg)"),
            "left-splice callee lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn left_splice_boolean_argument_keeps_required_grouping() {
        let output = roundtrip("module x; fn f() -> . { context_callee.<(.t) }");
        assert!(
            output.contains("context_callee.<(.t)"),
            "boolean left-splice argument lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn operator_right_splice_receiver_keeps_required_grouping() {
        let output = roundtrip(
            "module x; \
             fn prefix_impl(value: Formtype) -> Formtype { value } \
             op - __ { impl prefix_impl; }; \
             varop [% %] { foldr form_push form_empty; }; \
             fn normal_splice() -> . { (- form_value).>context_callee } \
             fn variadic_splice() -> . { ([% form_left, form_right %]).>context_callee }",
        );
        assert!(
            output.contains("(- form_value).>context_callee"),
            "normal-operator receiver lost its disambiguating parentheses:\n{output}"
        );
        assert!(
            output.contains("([% form_left, form_right %]).>context_callee"),
            "variadic-operator receiver lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn operator_field_receiver_keeps_required_grouping() {
        let output = roundtrip(
            "module x; \
             fn prefix_impl(value: Formtype) -> Formtype { value } \
             op - __ { impl prefix_impl; }; \
             varop [% %] { foldr form_push form_empty; }; \
             fn normal_access() -> . { (- form_value).?{form_field} } \
             fn normal_update() -> . { (- form_value).!{form_field = form_replacement} } \
             fn variadic_access() -> . { ([% form_left, form_right %]).?{form_field} } \
             fn variadic_update() -> . { ([% form_left, form_right %]).!{form_field = form_replacement} }",
        );
        for expected in [
            "(- form_value).?{form_field}",
            "(- form_value).!{form_field = form_replacement}",
            "([% form_left, form_right %]).?{form_field}",
            "([% form_left, form_right %]).!{form_field = form_replacement}",
        ] {
            assert!(
                output.contains(expected),
                "operator field receiver lost its disambiguating parentheses around `{expected}`:\n{output}"
            );
        }
    }

    #[test]
    fn literals_int_float_bool_string() {
        roundtrip(
            r#"module x; fn f() -> . { let s = "hi"; let n = 42; let f = 3.14; let b = .t; () }"#,
        );
    }

    // ---- string-escape round-trip --------------------------------------

    #[test]
    fn string_escapes_round_trip() {
        roundtrip(r#"module x; fn f() -> . { let s = "a\nb\t\"c\"\\dé中"; () }"#);
    }

    #[test]
    fn string_unicode_escape_emits_lowercase_hex() {
        let module = parse("module x; fn f() -> . { let s = \"\u{0001}\"; () }").unwrap();
        let out = pretty_module(&module);
        assert!(
            out.contains("\\u0001"),
            "expected lowercase hex escape `\\u0001`; got: {out}"
        );
    }

    #[test]
    fn float_exponent_canonicalises_to_lowercase_e_and_drops_plus() {
        let module =
            parse("module x; fn f() -> . { let a = 1.5E+10; let b = 1.5e-10; () }").unwrap();
        let out = pretty_module(&module);
        assert!(
            out.contains("1.5e10"),
            "expected `1.5e10` (lowercase e, no `+`); got: {out}"
        );
        assert!(
            out.contains("1.5e-10"),
            "expected `1.5e-10` (negative sign preserved); got: {out}"
        );
        assert!(
            !out.contains('E'),
            "uppercase `E` should not survive; got: {out}"
        );
        assert!(
            !out.contains("e+"),
            "leading `+` after `e` should be dropped; got: {out}"
        );
    }

    // ---- trivia preservation -------------------------------------------

    #[test]
    fn item_leading_comments_survive_round_trip() {
        let src = "module x;\n\
                   \n\
                   // docstring for `id`\n\
                   fn id() -> . { () }\n";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        assert!(
            out.contains("// docstring for `id`"),
            "expected leading comment to survive; got:\n{out}"
        );
        let m2 = parse(&out).unwrap();
        let out2 = pretty_module(&m2);
        assert_eq!(out, out2, "trivia round-trip must be idempotent");
    }

    #[test]
    fn import_leading_comments_survive_round_trip() {
        let src = "module x;\n\
                   \n\
                   // pull in the unit type\n\
                   import a/b(T);\n";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        assert!(
            out.contains("// pull in the unit type"),
            "expected leading comment on import to survive; got:\n{out}"
        );
        let m2 = parse(&out).unwrap();
        let out2 = pretty_module(&m2);
        assert_eq!(out, out2, "import-trivia round-trip must be idempotent");
    }

    #[test]
    fn comment_paragraphs_keep_blank_line_separator() {
        let src = "module x;\n\
                   \n\
                   // First paragraph.\n\
                   \n\
                   // Second paragraph.\n\
                   fn id() -> . { () }\n";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        assert!(out.contains("// First paragraph."), "got:\n{out}");
        assert!(out.contains("// Second paragraph."), "got:\n{out}");
        let lines: Vec<&str> = out.lines().collect();
        let first_idx = lines.iter().position(|l| l.contains("First")).unwrap();
        let second_idx = lines.iter().position(|l| l.contains("Second")).unwrap();
        assert!(
            second_idx == first_idx + 2 && lines[first_idx + 1].is_empty(),
            "expected one blank line between comment paragraphs; got:\n{out}"
        );
        let m2 = parse(&out).unwrap();
        assert_eq!(pretty_module(&m2), out);
    }

    #[test]
    fn empty_comment_lines_collapse() {
        let src = "module x;\n\
                   \n\
                   //\n\
                   //\n\
                   //\n\
                   fn id() -> . { () }\n";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        let count = out.lines().filter(|l| l.trim() == "//").count();
        assert_eq!(count, 1, "empty comment run should collapse; got:\n{out}");
    }

    // ---- hello_world module shape --------------------------------------

    #[test]
    fn hello_world_module_file() {
        roundtrip(
            r#"module hello/main;

host type String role(str);

host fn print(s: String) -> .;

pub fn main() -> . { print("Hello, world!") }"#,
        );
    }

    #[test]
    fn host_type_and_fn_roundtrip() {
        let pp =
            roundtrip("module x; host type Map[K][V]; host fn get[K][V](m: Map(K, V), k: K) -> V;");
        assert!(pp.contains("host type Map[K][V];"), "got:\n{pp}");
        assert!(
            pp.contains("host fn get[K][V](m: Map(K, V), k: K) -> V;"),
            "got:\n{pp}"
        );
    }

    #[test]
    fn host_pub_canonicalizes_to_plain_host() {
        let pp = roundtrip("module x; pub host type Foo;");
        assert!(pp.contains("host type Foo;"), "got:\n{pp}");
        assert!(!pp.contains("pub host"), "got:\n{pp}");
    }

    // ---- package-file round-trips ---------------------------------------

    fn package_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_package_file;
        let full = format!("package pkg;\n{src}");
        let e1 = parse_package_file(&full, None)
            .unwrap_or_else(|e| panic!("parse failed for {full:?}: {e:?}"));
        let pp1 = pretty_package_file(&e1);
        let e2 = parse_package_file(&pp1, None)
            .unwrap_or_else(|e| panic!("re-parse failed: {e:?}\noutput was:\n{pp1}"));
        let pp2 = pretty_package_file(&e2);
        assert_eq!(pp1, pp2, "package pretty-print not idempotent for:\n{src}");
        pp1
    }

    #[test]
    fn package_empty() {
        package_roundtrip("");
    }

    #[test]
    fn package_bridge_single_glob() {
        let pp = package_roundtrip("bridge { main; }");
        assert!(pp.contains("bridge {"), "got:\n{pp}");
        assert!(pp.contains("main\n"), "got:\n{pp}");
    }

    #[test]
    fn package_bridge_nested_and_wildcard_globs() {
        let pp = package_roundtrip("bridge { app/api; lib/*; vendor/**; }");
        assert!(pp.contains("app/api;"), "got:\n{pp}");
        assert!(pp.contains("lib/*;"), "got:\n{pp}");
        assert!(pp.contains("vendor/**\n"), "got:\n{pp}");
    }

    #[test]
    fn package_empty_bridge() {
        let pp = package_roundtrip("bridge {}");
        assert!(pp.contains("bridge {}"), "got:\n{pp}");
    }

    #[test]
    fn package_canonical_hello_world() {
        package_roundtrip(
            "build {\n  cache ();\n\n  target js { out \"out/js/\"; }\n}\n\nbridge { hello; }",
        );
    }

    // ---- dependency-file round-trips ------------------------------------

    fn dependency_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_dependency_file;
        let d1 = parse_dependency_file(src, None)
            .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        let pp1 = pretty_dependency_file(&d1);
        let d2 = parse_dependency_file(&pp1, None)
            .unwrap_or_else(|e| panic!("re-parse failed: {e:?}\noutput was:\n{pp1}"));
        let pp2 = pretty_dependency_file(&d2);
        assert_eq!(
            pp1, pp2,
            "dependency pretty-print not idempotent for:\n{src}"
        );
        pp1
    }

    /// The canonical `.dep.kio` layout is fixed: `dependency <local>;`,
    /// one blank line, the `source { … }` block with its single
    /// `path "<rel>"` line at +2 indent, and a trailing newline.
    #[test]
    fn dependency_canonical_layout_is_exact() {
        let pp = dependency_roundtrip(
            "dependency mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\";\n}\n",
        );
        assert_eq!(
            pp, "dependency mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\"\n}\n",
            "canonical .dep.kio layout drifted:\n{pp}"
        );
    }

    /// A messy-but-valid `.dep.kio` (extra blank lines, odd spacing,
    /// trailing separators) canonicalises to the fixed layout.
    #[test]
    fn dependency_messy_canonicalises() {
        let pp = dependency_roundtrip(
            "dependency mathlib  ;\n\n\n\nsource   {\n\n    path    \"../lib/m.pkg.kio\"  ;\n\n}\n",
        );
        assert_eq!(
            pp, "dependency mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\"\n}\n",
            "messy .dep.kio did not canonicalise:\n{pp}"
        );
    }

    /// A `//` header comment above the `dependency` line and a `//`
    /// comment above the `path` line survive the round-trip (the parser
    /// stashes them in `meta.leading_trivia` / `path_leading_trivia`).
    #[test]
    fn dependency_preserves_leading_comments() {
        let pp = dependency_roundtrip(
            "// pin the local math library\ndependency mathlib;\n\nsource {\n  // relative to the consumer package root\n  path \"../lib/m.pkg.kio\";\n}\n",
        );
        assert!(pp.contains("// pin the local math library\n"), "got:\n{pp}");
        assert!(
            pp.contains("  // relative to the consumer package root\n"),
            "got:\n{pp}"
        );
    }

    /// A `git`/`ref` `.dep.kio` has a fixed canonical layout: the `git`
    /// line then the `ref` line at +2 indent, in that order.
    #[test]
    fn dependency_git_canonical_layout_is_exact() {
        let pp = dependency_roundtrip(
            "dependency foo;\n\nsource {\n  git \"https://example.com/foo.git\";\n  ref \"main\"\n}\n",
        );
        assert_eq!(
            pp,
            "dependency foo;\n\nsource {\n  git \"https://example.com/foo.git\";\n  ref \"main\"\n}\n",
            "canonical git .dep.kio layout drifted:\n{pp}"
        );
    }

    /// A messy git `.dep.kio` canonicalises to the fixed layout (and the
    /// `git` / `ref` source order is preserved by construction — the AST
    /// stores them as named fields, not a key order).
    #[test]
    fn dependency_git_messy_canonicalises() {
        let pp = dependency_roundtrip(
            "dependency foo ;\n\n\nsource  {\n\n  git   \"file:///srv/foo.git\"  ;\n  ref \"v1.2.3\" ;\n\n}\n",
        );
        assert_eq!(
            pp,
            "dependency foo;\n\nsource {\n  git \"file:///srv/foo.git\";\n  ref \"v1.2.3\"\n}\n",
            "messy git .dep.kio did not canonicalise:\n{pp}"
        );
    }

    /// Comments above the `git` and `ref` lines survive the round-trip.
    #[test]
    fn dependency_git_preserves_leading_comments() {
        let pp = dependency_roundtrip(
            "dependency foo;\n\nsource {\n  // the upstream mirror\n  git \"https://example.com/foo.git\";\n  // track the release branch\n  ref \"release\";\n}\n",
        );
        assert!(pp.contains("  // the upstream mirror\n"), "got:\n{pp}");
        assert!(pp.contains("  // track the release branch\n"), "got:\n{pp}");
    }

    // ---- lock-file round-trips ------------------------------------------

    fn lock_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_lock_file;
        let l1 = parse_lock_file(src, None)
            .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        let pp1 = pretty_lock_file(&l1);
        let l2 = parse_lock_file(&pp1, None)
            .unwrap_or_else(|e| panic!("re-parse failed: {e:?}\noutput was:\n{pp1}"));
        let pp2 = pretty_lock_file(&l2);
        assert_eq!(pp1, pp2, "lock pretty-print not idempotent for:\n{src}");
        pp1
    }

    /// The canonical `.lock.kio` layout is fixed: `lock <local>;`, one
    /// blank line, then the `resolved` fields at
    /// +2 indent in that order, and a trailing newline.
    #[test]
    fn lock_canonical_layout_is_exact() {
        let pp = lock_roundtrip(
            "lock foo;\n\nresolved {\n  git \"https://example.com/foo.git\";\n  ref \"main\";\n  commit \"0123456789abcdef0123456789abcdef01234567\";\n  sig \"deadbeef\"\n}\n",
        );
        assert_eq!(
            pp,
            "lock foo;\n\nresolved {\n  git \"https://example.com/foo.git\";\n  ref \"main\";\n  commit \"0123456789abcdef0123456789abcdef01234567\";\n  sig \"deadbeef\"\n}\n",
            "canonical .lock.kio layout drifted:\n{pp}"
        );
    }

    /// A messy `.lock.kio` canonicalises to the fixed layout.
    #[test]
    fn lock_messy_canonicalises() {
        let pp = lock_roundtrip(
            "lock foo ;\n\n\nresolved  {\n  git \"u\" ;\n\n  ref   \"r\"  ;\n  commit \"c\";\n  sig \"d\";\n}\n",
        );
        assert_eq!(
            pp,
            "lock foo;\n\nresolved {\n  git \"u\";\n  ref \"r\";\n  commit \"c\";\n  sig \"d\"\n}\n",
            "messy .lock.kio did not canonicalise:\n{pp}"
        );
    }

    // ---- build-block round-trips ---------------------------------------

    /// Round-trip a `build { ... }` block body (the `cache` / `docs` /
    /// `target` sequence, without the enclosing braces) through the
    /// standalone body parser + `doc_build_block` and assert
    /// idempotence.
    fn build_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_build_block_body;
        let b1 = parse_build_block_body(src)
            .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        let pp1 = render(&doc_build_block(&b1), WIDTH);
        // The rendered form carries the `build { ... }` braces; the
        // body parser wants the inner body, so re-parse via the
        // package-file parser to confirm the rendered block is
        // well-formed and stable.
        let wrapped = format!("package pkg;\n\n{pp1}\n");
        let e2 = crate::pass::parser::parse_package_file(&wrapped, Some("pkg"))
            .unwrap_or_else(|e| panic!("re-parse failed: {e:?}\noutput was:\n{wrapped}"));
        let b2 = e2
            .build
            .expect("rendered package file carries the build block");
        let pp2 = render(&doc_build_block(&b2), WIDTH);
        assert_eq!(pp1, pp2, "build pretty-print not idempotent for:\n{src}");
        pp1
    }

    #[test]
    fn build_only_cache() {
        // Cache-only build block is legal grammar (target blocks are
        // optional even though `kio build` rejects a target-less
        // package).
        build_roundtrip("cache ();");
    }

    #[test]
    fn build_single_target() {
        build_roundtrip("cache ();\ntarget js { out \"out/js/\"; }");
    }

    #[test]
    fn build_cache_path_emits_first() {
        let pp = build_roundtrip(
            "cache \"out/.kio-cache/\";\n\
             target js { out \"out/js/\"; }",
        );
        assert!(pp.contains("cache \"out/.kio-cache/\";"), "got:\n{pp}");
    }

    #[test]
    fn build_multiple_targets_with_extra_keys() {
        build_roundtrip(
            "cache ();\n\
             target js { out \"out/js/\"; module_format \"esm\"; }; \
             target wasm { out \"out/wasm/\"; }",
        );
    }

    #[test]
    fn build_target_keys_are_canonicalized() {
        let pp = build_roundtrip(
            "cache ();\n\
             target rust { thread_safety \"send_sync\"; namespace \"demo\"; out \"out/rust/\"; }",
        );
        let out_at = pp.find("out \"out/rust/\";").unwrap();
        let namespace_at = pp.find("namespace \"demo\";").unwrap();
        let thread_safety_at = pp.find("thread_safety \"send_sync\"").unwrap();
        assert!(
            out_at < namespace_at && namespace_at < thread_safety_at,
            "got:\n{pp}"
        );
    }

    #[test]
    fn build_target_comments_follow_canonical_key_order() {
        let pp = build_roundtrip(
            "cache ();\n\
             target rust { \
               thread_safety \"send_sync\"; \
               // output path\n\
               out \"out/rust/\"; \
               namespace \"demo\"; \
             }",
        );
        let output_comment = pp.find("// output path").unwrap();
        let out_at = pp.find("out \"out/rust/\";").unwrap();
        let namespace_at = pp.find("namespace \"demo\";").unwrap();
        let thread_safety_at = pp.find("thread_safety \"send_sync\"").unwrap();
        assert!(
            output_comment < out_at && out_at < namespace_at && namespace_at < thread_safety_at,
            "expected target entry comment to move with `out`; got:\n{pp}"
        );
    }

    #[test]
    fn build_hyphenated_target_id_roundtrips() {
        // `kio-prime` spells as a single bare kebab id.
        let pp = build_roundtrip("cache ();\ntarget kio-prime { out \"out/kio-prime/\"; }");
        assert!(pp.contains("target kio-prime {"), "got:\n{pp}");
    }

    #[test]
    fn build_docs_block_full() {
        let pp = build_roundtrip(
            "cache ();\n\
             docs { md \"docs\"; support \"../support\"; md_out \"out/docs-md/\"; html \"out/docs/\"; };\n\
             target js { out \"out/js/\"; }",
        );
        assert!(pp.contains("docs {"), "got:\n{pp}");
        assert!(pp.contains("md \"docs\""), "got:\n{pp}");
        assert!(pp.contains("support \"../support\";"), "got:\n{pp}");
    }

    #[test]
    fn build_docs_block_md_only() {
        // `md_out` / `html` omitted — only `md` emits.
        let pp = build_roundtrip("cache ();\ndocs { md \"docs\"; }");
        assert!(pp.contains("md \"docs\"\n"), "got:\n{pp}");
        assert!(!pp.contains("md_out"), "got:\n{pp}");
        assert!(!pp.contains("html"), "got:\n{pp}");
    }

    #[test]
    fn build_docs_block_key_order_canonicalized() {
        // Author writes html before support and md_out; canonical output reorders.
        let pp = build_roundtrip(
            "cache ();\ndocs { html \"out/docs/\"; support \"s\"; md \"d\"; md_out \"out/m/\"; };",
        );
        let md_at = pp.find("md \"").unwrap();
        let support_at = pp.find("support \"").unwrap();
        let md_out_at = pp.find("md_out \"").unwrap();
        let html_at = pp.find("html \"").unwrap();
        assert!(
            md_at < support_at && support_at < md_out_at && md_out_at < html_at,
            "got:\n{pp}"
        );
    }

    #[test]
    fn build_docs_comments_follow_canonical_key_order() {
        let pp = build_roundtrip(
            "cache ();\n\
             docs { \
               html \"out/docs/\"; \
               // markdown root\n\
               md \"docs\"; \
               // support root\n\
               support \"support\"; \
               md_out \"out/docs-md/\"; \
            }",
        );
        let md_comment = pp.find("// markdown root").unwrap();
        let md_at = pp.find("md \"docs\";").unwrap();
        let support_comment = pp.find("// support root").unwrap();
        let support_at = pp.find("support \"support\";").unwrap();
        let md_out_at = pp.find("md_out \"out/docs-md/\";").unwrap();
        let html_at = pp.find("html \"out/docs/\"").unwrap();
        assert!(
            md_comment < md_at
                && md_at < support_comment
                && support_comment < support_at
                && support_at < md_out_at
                && md_out_at < html_at,
            "expected docs comments to follow canonical key order; got:\n{pp}"
        );
    }

    // ---- width-driven A1 breaks for comma-separated lists -------------

    /// A short call argument list fits on one line — the
    /// `comma_list` Doc helper picks the flat layout.
    #[test]
    fn call_args_fit_on_one_line() {
        let pp = roundtrip(r#"module x; fn f() -> . { id(String, "hi") }"#);
        assert!(pp.contains("id(String, \"hi\")"), "got:\n{pp}");
    }

    /// A call argument list whose flat width exceeds the 100-column
    /// budget breaks into the leading-comma A1 layout. The fn
    /// body itself also breaks to multi-line (it can't stay
    /// single-line if the call inside it doesn't fit), so the
    /// call args land at +4 indent from column 0 (fn body's
    /// nest 2 + call's nest 2).
    #[test]
    fn call_args_overflow_breaks_to_a1() {
        let src = r#"module x;
fn f() -> . {
  call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta, arg_epsilon)
}"#;
        let pp = roundtrip(src);
        assert!(
            pp.contains("\n    , arg_alpha"),
            "expected leading-comma break for arg_alpha at +4 indent; got:\n{pp}"
        );
        assert!(
            pp.contains("\n    , arg_epsilon"),
            "expected leading-comma break for arg_epsilon at +4 indent; got:\n{pp}"
        );
    }

    /// A type-application argument list at a use site honours the
    /// same width-driven rule.
    #[test]
    fn type_app_args_overflow_breaks_to_a1() {
        let src = "module x;\n\
                   type Result = Outer(\
                   Inner_one, Inner_two, Inner_three, Inner_four, Inner_five, Inner_six, Inner_seven);";
        let pp = roundtrip(src);
        assert!(
            pp.contains("\n  , Inner_one"),
            "expected leading-comma break for Inner_one; got:\n{pp}"
        );
    }

    /// A multi-line item in a broken A1 list anchors at its own
    /// content column: a lambda item's block body sits at +2 from
    /// the lambda header (not level with the list items) and its
    /// closing `}` returns to the item's content column (not the
    /// comma column). The list's own closer stays at the comma
    /// column. Regression: the `", "` prefix used to advance the
    /// column without the indent level, so the body landed level
    /// with the item content and the `}` level with the commas.
    #[test]
    fn lambda_item_block_body_nests_inside_the_lambda() {
        let src = "module x;\n\
                   fn filter[A](keep: A -> Bool, xs: A, whole: A) -> A {\n\
                     reverse(fold_list(xs, whole, .(carry: A, x: A) -> A { if! keep(x) { cons(x, carry) } else { carry } }))\n\
                   }\n";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            "module x;\n\
             \n\
             fn filter[A](keep: A -> Bool, xs: A, whole: A) -> A {\n\
             \x20 reverse(fold_list(\n\
             \x20   , xs\n\
             \x20   , whole\n\
             \x20   , .(carry: A, x: A) -> A {\n\
             \x20       if! keep(x) {\n\
             \x20         cons(x, carry)\n\
             \x20       } else {\n\
             \x20         carry\n\
             \x20       }\n\
             \x20     }\n\
             \x20   ))\n\
             }\n"
        );
    }

    /// The same item-content anchoring through the `match!` clause
    /// path: a clause lambda is an item of the clause tuple, itself
    /// an item of the `match!` argument list, so each `, ` level
    /// nests its content and a broken clause body sits at +2 from
    /// its clause header.
    #[test]
    fn match_clause_bodies_nest_inside_their_clause() {
        let src = "module x;\n\
                   import match(match);\n\
                   fn step[A](v: A | A, dflt: A) -> A {\n\
                     match!(v, (.(l: A) { if! is_good(l) { l } else { dflt } }, .(r: A) { r }))\n\
                   }\n";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            "module x;\n\
             \n\
             import match(match);\n\
             \n\
             fn step[A](v: A | A, dflt: A) -> A {\n\
             \x20 match!(\n\
             \x20   , v\n\
             \x20   , (\n\
             \x20       , .(l: A) {\n\
             \x20           if! is_good(l) {\n\
             \x20             l\n\
             \x20           } else {\n\
             \x20             dflt\n\
             \x20           }\n\
             \x20         }\n\
             \x20       , .(r: A) { r }\n\
             \x20       )\n\
             \x20   )\n\
             }\n"
        );
    }

    /// The width-driven `labels` arm is the same A1 comma list as
    /// every other list: a too-wide arm breaks to the leading-comma
    /// layout with the `{` kept on the introducing line, and a
    /// multi-line entry anchors at its content column — the entry's
    /// broken payload items sit at +2 from the entry's own start,
    /// not at the comma column. Regression: the arm used to flat-
    /// join its entries with no broken layout, hanging the whole
    /// arm on the next line and leaving a wide payload to break
    /// mid-line anchored two columns left of the entry.
    #[test]
    fn labels_wide_arm_breaks_to_leading_comma_at_content_column() {
        let src = "module x;\n\
                   labels Wide = { first_long_label_name: Someextremelylongtypeconstructorname(Aaaaaaaa, Bbbbbbbb, Gggggggg, Hhhhhhhh), second_label: I32 };\n";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            "module x;\n\
             \n\
             labels Wide = {\n\
             \x20 , first_long_label_name: Someextremelylongtypeconstructorname(\n\
             \x20     , Aaaaaaaa\n\
             \x20     , Bbbbbbbb\n\
             \x20     , Gggggggg\n\
             \x20     , Hhhhhhhh\n\
             \x20     )\n\
             \x20 , second_label: I32\n\
             \x20 };\n"
        );
    }

    /// The width-driven and comment-forced `labels` layouts agree:
    /// the same declaration with and without an entry comment
    /// renders identically apart from the comment line itself.
    #[test]
    fn labels_width_and_comment_paths_agree() {
        let plain = "module x;\n\
                     labels Wide = { first_long_label_name: Someextremelylongtypeconstructorname(Aaaaaaaa, Bbbbbbbb, Gggggggg, Hhhhhhhh), second_label: I32 };\n";
        let commented = "module x;\n\
                         labels Wide = {\n\
                         \x20 // keep me\n\
                         \x20 , first_long_label_name: Someextremelylongtypeconstructorname(Aaaaaaaa, Bbbbbbbb, Gggggggg, Hhhhhhhh)\n\
                         \x20 , second_label: I32\n\
                         \x20 };\n";
        let plain_pp = roundtrip(plain);
        let commented_pp = roundtrip(commented);
        assert!(
            commented_pp.contains("\n  // keep me\n"),
            "entry comment survived: {commented_pp}"
        );
        assert_eq!(
            commented_pp.replace("  // keep me\n", ""),
            plain_pp,
            "labels layouts diverge once the comment line is removed"
        );
    }

    // ---- string-literal reflow at 72 chars -----------------------------

    /// Short string literals (value ≤ 72 chars) emit single-line.
    #[test]
    fn short_string_stays_single_line() {
        let pp = roundtrip(r#"module x; fn f() -> . { let s = "hello world"; () }"#);
        assert!(pp.contains(r#""hello world""#), "got:\n{pp}");
        // No reflow → no "..." "..." sequence on adjacent lines.
        assert!(
            !pp.contains("\"hello\"\n"),
            "short string shouldn't reflow; got:\n{pp}"
        );
    }

    /// A string literal whose value exceeds 72 chars reflows into
    /// adjacent literals at word boundaries.
    #[test]
    fn long_string_reflows_at_word_boundaries() {
        // 80-char value with spaces — should split at 72 cap.
        let value =
            "the quick brown fox jumps over the lazy dog and then continues running to home";
        assert!(value.chars().count() > 72);
        let src = format!(r#"module x; fn f() -> . {{ let s = "{value}"; () }}"#);
        let pp = roundtrip(&src);
        // The output should contain at least two adjacent string
        // literals on consecutive lines (the canonical reflow shape).
        let lines: Vec<&str> = pp.lines().collect();
        let any_adjacent_strings = lines
            .windows(2)
            .any(|pair| pair[0].trim_end().ends_with('"') && pair[1].trim_start().starts_with('"'));
        assert!(
            any_adjacent_strings,
            "expected adjacent string-literal reflow; got:\n{pp}"
        );
    }

    /// A short fn body stays on a single line — `... -> R { body }`
    /// is canonical for bodies that fit. The 2-item params list and
    /// the body each make their own width-driven decision; here both
    /// fit, so the whole declaration stays inline.
    #[test]
    fn short_fn_def_body_stays_single_line() {
        let pp = roundtrip("module x; fn id[A](x: A) -> A { x }");
        assert!(
            pp.contains(") -> A { x }"),
            "short fn body should stay inline; got:\n{pp}"
        );
    }

    #[test]
    fn final_block_semicolon_canonicalizes_away() {
        let pp = roundtrip("module x; fn f() -> . { (); }");
        assert!(pp.contains("fn f() -> . { () }"), "got:\n{pp}");
        assert!(!pp.contains("();"), "got:\n{pp}");
    }

    #[test]
    fn scope_call_round_trips_as_block_expression() {
        let pp = roundtrip("module x; fn f() -> . { scope! { let x = (); x } }");
        assert!(pp.contains("scope! {\n"), "got:\n{pp}");
        assert!(pp.contains("let x = ();"), "got:\n{pp}");
    }

    #[test]
    fn scope_call_comment_survives_round_trip() {
        let pp = roundtrip(
            "module x; fn f() -> . { scope! { \
             // local binding\n\
             let x = (); x } }",
        );
        assert!(
            pp.contains("scope! {\n    // local binding\n    let x = ();"),
            "expected scope comment before local let; got:\n{pp}"
        );
    }

    #[test]
    fn sequence_call_comment_survives_round_trip() {
        let pp = roundtrip(
            "module x; fn bind[A][B](x: A, k: A -> B) -> B { k(x) } \
             fn f() -> . { do! bind { \
               // sequence step\n\
               (); () } }",
        );
        assert!(
            pp.contains("do! bind {\n    // sequence step\n    ();"),
            "expected sequence comment before expression; got:\n{pp}"
        );
    }

    /// A comment captured before a `match!` clause survives the
    /// round-trip: the comment emits on its own line above the
    /// clause inside the multi-line arms tuple.
    #[test]
    fn within_body_comment_before_match_clause_survives_round_trip() {
        // Use single-clause `match!` first to verify trivia
        // capture works for the basic case. Multi-clause is the
        // intended target — a baseline check below confirms the
        // comment-free multi-clause parses, then the with-comment
        // form is round-tripped.
        let bare = "module x; import match(match); fn f[A](v: A) -> . { match!(v, .() { () }) }";
        parse(bare).expect("single-clause baseline parses");
        // The comment-free multi-clause control must parse before
        // checking the same shape with interstitial trivia.
        let multi_bare = "module x; import match(match); fn f[A](v: A) -> . { match!(v, (.() { () }, .[B](n: B) { () })) }";
        parse(multi_bare).expect("multi-clause baseline parses");
        let src = "module x; import match(match); fn f[A](v: A) -> . { match!(v, (.() { () }, // sec\n.[B](n: B) { () })) }";
        let m = parse(src)
            .unwrap_or_else(|e| panic!("with-comment parse failed: {e:?}\nsource:\n{src}"));
        let out = pretty_module(&m);
        assert!(
            out.contains("// sec"),
            "expected leading comment on the second clause to survive; got:\n{out}"
        );
        let m2 = parse(&out).unwrap();
        assert_eq!(pretty_module(&m2), out);
    }

    /// A comment captured before a `let` binding in a fn body
    /// survives the round-trip: the comment emits on its own line
    /// above the `let`, and the surrounding `{ body }` breaks to
    /// multi-line so the comment can sit there.
    #[test]
    fn within_body_comment_before_let_survives_round_trip() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     // initialize x\n\
                     let x = ();\n\
                     x\n\
                   }\n";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        assert!(
            out.contains("// initialize x"),
            "expected leading comment to survive; got:\n{out}"
        );
        // Body broke to multi-line because the trivia forced it.
        assert!(
            out.contains("{\n"),
            "expected multi-line block body; got:\n{out}"
        );
        // Idempotent.
        let m2 = parse(&out).unwrap();
        assert_eq!(pretty_module(&m2), out);
    }

    #[test]
    fn within_body_comment_before_final_call_emits_once() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     // finish\n\
                     g()\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(
            out.matches("// finish").count(),
            1,
            "comment before the final call must emit exactly once; got:\n{out}"
        );
    }

    #[test]
    fn within_closure_body_comment_before_final_call_emits_once() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     .() {\n\
                       // finish\n\
                       g()\n\
                     }()\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(
            out.matches("// finish").count(),
            1,
            "comment before the closure-body final call must emit exactly once; got:\n{out}"
        );
    }

    /// A comment before a `let` whose RHS is a multi-line block,
    /// when that `let` is the body of a closure passed as a
    /// positional call argument, survives the round-trip and is
    /// emitted exactly once. Regression guard: the `fn`-literal
    /// block-body parser used to capture the trivia between `{` and
    /// the first statement and inject it onto the body, while the
    /// inner `let` independently captured the same trivia onto its
    /// own `meta.leading_trivia` — so the comment landed twice and
    /// the pretty-printer emitted it twice.
    #[test]
    fn within_closure_body_comment_before_let_emits_once() {
        let src = "module x;\n\
                   import match(match);\n\
                   fn apply[A](x: A, f: A -> A) -> A { f(x) }\n\
                   fn g[A](p: A & A) -> A & A {\n\
                     apply(A & A, p, .(q) {\n\
                       // keep q\n\
                       let kept = match!(q, .(l: A, r: A) { (l, r) });\n\
                       kept\n\
                     })\n\
                   }\n";
        let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e:?}\nsource:\n{src}"));
        let out = pretty_module(&m);
        assert_eq!(
            out.matches("// keep q").count(),
            1,
            "comment before the closure-body let must emit exactly once; got:\n{out}"
        );
        // Idempotent under parse → pretty.
        let m2 = parse(&out).unwrap();
        assert_eq!(pretty_module(&m2), out);
    }

    /// A comment dangling between a block body's last token and the
    /// closing `}` rides `body.meta.trailing_trivia` and survives.
    /// Trivia is leading-only and a closer's leading run is discarded,
    /// so the trailing-trivia channel is the only thing that keeps it.
    #[test]
    fn trailing_dangling_block_comment_survives() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     ()\n\
                     // dangling\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(
            out.matches("// dangling").count(),
            1,
            "dangling end-of-block comment must survive exactly once; got:\n{out}"
        );
    }

    /// A trailing comment on the final expression (`e // c` before
    /// `}`) survives.
    #[test]
    fn trailing_comment_on_final_expr_survives() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     () // the result\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// the result").count(), 1, "got:\n{out}");
    }

    /// A trailing comment on the last call argument, before the `)`,
    /// survives — it rides the last arg's `meta.trailing_trivia`.
    #[test]
    fn trailing_comment_on_last_call_arg_survives() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     g(\n\
                       a,\n\
                       b // last arg\n\
                     )\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// last arg").count(), 1, "got:\n{out}");
    }

    /// A comment after the last item, before end-of-file, survives —
    /// it rides the module's `meta.trailing_trivia` (the lexer's
    /// end-of-file trivia run).
    #[test]
    fn trailing_comment_after_last_item_survives() {
        let src = "module x;\n\
                   fn f() -> . { () } // after item\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// after item").count(), 1, "got:\n{out}");
    }

    /// An end-of-file-only comment (no trailing item attachment)
    /// survives.
    #[test]
    fn end_of_file_comment_survives() {
        let src = "module x;\n\
                   fn f() -> . { () }\n\
                   // file footer\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// file footer").count(), 1, "got:\n{out}");
    }

    /// A comment dangling at the end of a trailing block, before `}`,
    /// survives.
    #[test]
    fn trailing_comment_in_scope_call_survives() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     scope! {\n\
                       ()\n\
                       // dangling in do\n\
                     }\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// dangling in do").count(), 1, "got:\n{out}");
    }

    /// The interstitial-hoist fallback: a comment wedged on a token
    /// that doesn't start an AST node (here between `=` and a `;` is
    /// covered elsewhere; this exercises a comment that the parser
    /// folds into a leading run) survives and the second pass is a
    /// fixpoint. The contract is *never dropped* + idempotent after
    /// the first pass, which `roundtrip` already asserts; this case
    /// pins that an interstitial comment is not lost.
    #[test]
    fn interstitial_comment_hoisted_and_idempotent() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     let y =\n\
                       // between = and value\n\
                       ();\n\
                     y\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(
            out.matches("// between = and value").count(),
            1,
            "interstitial comment must survive exactly once; got:\n{out}"
        );
        // Idempotence beyond the first pass: a second round is a no-op.
        let m2 = parse(&out).unwrap();
        let out2 = pretty_module(&m2);
        assert_eq!(out2, out, "second pass must be a fixpoint; got:\n{out2}");
    }

    /// A short `let X = E;` stays on one line — the value-and-`;`
    /// group fits flat in the budget so no break fires.
    #[test]
    fn short_let_rhs_stays_inline() {
        let pp = roundtrip("module x; fn f() -> . { let x = (); x }");
        assert!(
            pp.contains("let x = ();"),
            "short let should stay inline; got:\n{pp}"
        );
    }

    /// A `let X = E;` whose flat form overflows the 100-column
    /// budget breaks at `=`: the value sits on its own line at +2
    /// indent, with `;` trailing the value's last token.
    #[test]
    fn wide_let_rhs_breaks_at_equals() {
        let src = r#"module x;
fn f() -> . {
  let result =
    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta);
  result
}"#;
        let pp = roundtrip(src);
        assert!(
            pp.contains("let result =\n"),
            "expected break at `=`; got:\n{pp}"
        );
        assert!(
            pp.contains("\n    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta);"),
            "expected value on its own line at +4 indent, `;` trailing the call's close paren; got:\n{pp}"
        );
    }

    /// Width-driven and trivia-driven breaks of the same RHS produce
    /// the same shape (modulo the comment line) — the two triggers
    /// share one canonical broken shape.
    #[test]
    fn wide_let_and_trivia_let_share_broken_shape() {
        // Width-driven version (no trivia).
        let width_src = r#"module x;
fn f() -> . {
  let result =
    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta);
  result
}"#;
        let width_pp = roundtrip(width_src);
        // Trivia-driven version (short value, but a comment forces break).
        let trivia_src = "module x;\n\
                          fn f() -> . {\n\
                            let result =\n\
                              // explainer\n\
                              compute();\n\
                            result\n\
                          }\n";
        let trivia_pp = roundtrip(trivia_src);
        // Both render the let with `=\n` followed by an indented
        // value line and a trailing `;` on that line.
        assert!(
            width_pp.contains("let result =\n"),
            "width-driven let should break at `=`; got:\n{width_pp}"
        );
        assert!(
            trivia_pp.contains("let result =\n"),
            "trivia-driven let should break at `=`; got:\n{trivia_pp}"
        );
        assert!(
            trivia_pp.contains("// explainer"),
            "trivia comment should survive; got:\n{trivia_pp}"
        );
    }

    /// A let-chain where one let's flat form overflows pulls the
    /// chain into the vertical-stack layout per § Comments inside
    /// expression bodies. Each let renders on its own line; the
    /// wide let additionally breaks internally at `=`.
    #[test]
    fn wide_let_in_chain_stacks_and_breaks_internally() {
        let src = r#"module x;
fn f() -> . {
  let a = ();
  let big_result =
    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta);
  let z = ();
  big_result
}"#;
        let pp = roundtrip(src);
        // Chain stacks: each let on its own line.
        assert!(
            pp.contains("\n  let a = ();\n"),
            "expected let a on its own line; got:\n{pp}"
        );
        assert!(
            pp.contains("\n  let z = ();\n"),
            "expected let z on its own line; got:\n{pp}"
        );
        // The wide let additionally breaks internally.
        assert!(
            pp.contains("let big_result =\n"),
            "wide let should break at `=`; got:\n{pp}"
        );
    }

    /// A wide value whose own internal layout breaks (e.g., a wide
    /// call that broke to A1) composes with the let-`=` break: the
    /// let breaks first, the call's A1 break fires inside the +2
    /// nest, and `;` trails the call's `)` close-token.
    #[test]
    fn wide_let_with_internally_broken_rhs_composes() {
        let src = r#"module x;
fn f() -> . {
  let result =
    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta, arg_epsilon);
  result
}"#;
        let pp = roundtrip(src);
        // Outer let breaks at `=`.
        assert!(
            pp.contains("let result =\n"),
            "let should break at `=`; got:\n{pp}"
        );
        // Inner call breaks to A1 with leading commas.
        assert!(
            pp.contains("\n      , arg_alpha"),
            "expected A1 break for call args; got:\n{pp}"
        );
        // The `;` closes the call at the close-paren's line.
        assert!(
            pp.contains("\n      );"),
            "expected `;` trailing the call's close paren; got:\n{pp}"
        );
    }

    // ---- UFCS chain layout --------------------------------------------

    /// A short UFCS chain fits on one line: `r.>f(x).>g(y).>h(z)`.
    #[test]
    fn short_ufcs_chain_stays_flat() {
        let pp = roundtrip("module x; fn f() -> . { let r = receiver.>f(x).>g(y).>h(z); () }");
        assert!(
            pp.contains("receiver.>f(x).>g(y).>h(z)"),
            "short chain should stay flat; got:\n{pp}"
        );
    }

    /// A wide UFCS chain breaks per § UFCS chains: receiver on its
    /// own line, each `.>seg(args)` on its own line at +2 indent.
    /// Idempotent round-trip.
    #[test]
    fn wide_ufcs_chain_breaks_to_leading_arrow_layout() {
        let src = r#"module x;
fn f() -> . {
  let r =
    some_long_receiver_expression_value
      .>first_step_with_a_long_name(arg_one, arg_two)
      .>second_step_with_a_long_name(arg_three)
      .>final_step_with_a_long_name(arg_four);
  r
}"#;
        let pp = roundtrip(src);
        // Receiver on its own line; first segment at +2 indent
        // (block 2 + let 2 + chain 2 = 6).
        assert!(
            pp.contains("\n    some_long_receiver_expression_value\n"),
            "expected receiver on its own line at +4; got:\n{pp}"
        );
        assert!(
            pp.contains("\n      .>first_step_with_a_long_name(arg_one, arg_two)"),
            "expected first .>seg at +6 indent; got:\n{pp}"
        );
        assert!(
            pp.contains("\n      .>second_step_with_a_long_name(arg_three)"),
            "expected second .>seg at +6 indent; got:\n{pp}"
        );
        assert!(
            pp.contains("\n      .>final_step_with_a_long_name(arg_four);"),
            "expected last .>seg trailing `;` at +6 indent; got:\n{pp}"
        );
    }

    /// A zero-arg UFCS segment (`.>f` with no parens) emits without
    /// parens whether at the head of a chain or standalone.
    #[test]
    fn ufcs_zero_arg_segment_omits_parens() {
        let flat = roundtrip("module x; fn f() -> . { let r = r.>bare; () }");
        assert!(
            flat.contains("r.>bare"),
            "single bare UFCS should have no parens; got:\n{flat}"
        );
        let chain = roundtrip("module x; fn f() -> . { let r = r.>bare.>next(x); () }");
        assert!(
            chain.contains("r.>bare.>next(x)"),
            "bare segment inside a flat chain should have no parens; got:\n{chain}"
        );
    }

    #[test]
    fn elaborator_left_call_splice_preserves_bang() {
        let pp = roundtrip("module x; fn f() -> . { let r = iso!(T).<<value; () }");
        assert!(
            pp.contains("iso!(T).<<value"),
            "elaborator left-call splice should preserve `!`; got:\n{pp}"
        );
    }

    #[test]
    fn bare_and_explicit_unit_ufcs_forms_roundtrip() {
        let source = r#"module x;
fn f() -> . {
  receiver.>call;
  receiver.>>call;
  call.<receiver;
  call.<<receiver;
  receiver.>T.member;
  receiver.>>T.member;
  T.member.<receiver;
  T.member.<<receiver;
  receiver.>transform!;
  receiver.>>transform!;
  transform!.<receiver;
  transform!.<<receiver;
  receiver.>call(());
  call(()).<receiver;
  ()
}"#;
        let formatted = roundtrip(source);
        for spelling in [
            "receiver.>call",
            "receiver.>>call",
            "call.<receiver",
            "call.<<receiver",
            "receiver.>T.member",
            "receiver.>>T.member",
            "T.member.<receiver",
            "T.member.<<receiver",
            "receiver.>transform!",
            "receiver.>>transform!",
            "transform!.<receiver",
            "transform!.<<receiver",
            "receiver.>call(())",
            "call(()).<receiver",
        ] {
            assert!(
                formatted.contains(spelling),
                "formatter lost `{spelling}`:\n{formatted}"
            );
        }
    }

    /// Wide args inside a single chain segment break to A1
    /// independently of the surrounding chain break — the chain
    /// breaks at `.>`, and `doc_call` handles the inner arg list.
    #[test]
    fn ufcs_wide_args_segment_falls_back_to_a1() {
        let src = r#"module x;
fn f() -> . {
  let r =
    receiver
      .>one(x)
      .>two(arg_one_with_a_long_name, arg_two_with_a_long_name, arg_three_with_a_long_name, arg_four_with_a_long_name)
      .>three(y);
  r
}"#;
        let pp = roundtrip(src);
        // Chain broke at `.>`.
        assert!(
            pp.contains("\n      .>one(x)\n"),
            "expected first .>seg on its own line; got:\n{pp}"
        );
        assert!(
            pp.contains("\n      .>three(y)"),
            "expected last .>seg on its own line; got:\n{pp}"
        );
        // The wide segment's args broke to A1.
        assert!(
            pp.contains(".>two(\n"),
            "expected wide segment's args to break to A1; got:\n{pp}"
        );
        assert!(
            pp.contains("\n        , arg_one_with_a_long_name"),
            "expected leading-comma arg at +8; got:\n{pp}"
        );
    }

    /// A comment trailing the receiver of a UFCS chain — captured on
    /// the following `.>` token's leading trivia — survives a `kio
    /// fmt` round-trip and forces the chain to break (the cascading-
    /// break rule, § Comments / § UFCS chains). The comment can't sit
    /// inline before the next `.>`, so it lands on its own line above
    /// the segment it precedes.
    #[test]
    fn ufcs_receiver_trailing_comment_survives_round_trip() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     let r = recv // before the step\n\
                       .>step(arg);\n\
                     ()\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// before the step").count(), 1, "got:\n{out}");
        assert!(
            out.contains("recv\n"),
            "comment forces the chain to break; got:\n{out}"
        );
    }

    /// The generative shape behind the comment-conservation gate: a
    /// multi-line comma-list element whose value is a UFCS chain, with
    /// a comment trailing the chain's receiver on the element's first
    /// physical line. Both that receiver-trailing comment and the
    /// element-trailing comment survive in source order.
    #[test]
    fn ufcs_chain_in_comma_list_element_keeps_step_comment() {
        let src = "module x;\n\
                   fn f() -> . {\n\
                     g(\n\
                       , receiver_with_a_name // step comment\n\
                         .>chain_step_with_a_long_name(some_argument_value) // element comment\n\
                       )\n\
                   }\n";
        let out = roundtrip(src);
        assert_eq!(out.matches("// step comment").count(), 1, "got:\n{out}");
        assert_eq!(out.matches("// element comment").count(), 1, "got:\n{out}");
        let step_at = out.find("// step comment").unwrap();
        let elem_at = out.find("// element comment").unwrap();
        assert!(
            step_at < elem_at,
            "source order must be preserved (step before element); got:\n{out}"
        );
    }

    /// A long fn body breaks the surrounding `{ body }` to
    /// multi-line, with body at +2 indent and the closing `}`
    /// back at column 0.
    #[test]
    fn long_fn_def_body_breaks_to_multi_line() {
        let src = r#"module x;
fn f() -> . {
  call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta, arg_epsilon)
}"#;
        let pp = roundtrip(src);
        // The body is on its own line at +2 indent, closing `}`
        // at column 0.
        assert!(
            pp.contains("fn f() -> . {\n  "),
            "expected `{{` at end of line then body at +2 indent; got:\n{pp}"
        );
        assert!(
            pp.ends_with("\n}\n"),
            "expected closing `}}` at column 0; got:\n{pp}"
        );
    }

    /// A string with no whitespace overflows in a single chunk
    /// rather than mid-word splitting.
    #[test]
    fn long_url_string_overflows_in_one_chunk() {
        // 100-char URL-like string with no whitespace.
        let value = "https://example.com/api/v1/some/very/long/path/that/has/many/segments/and/no/spaces/at/all/yes";
        assert!(value.chars().count() > 72);
        assert!(!value.chars().any(|c| c.is_whitespace()));
        let src = format!(r#"module x; fn f() -> . {{ let s = "{value}"; () }}"#);
        let pp = roundtrip(&src);
        // The whole URL appears as a single literal — overflow is
        // accepted per `specs/style.md`'s "Unbreakable strings
        // overflow" rule.
        assert!(
            pp.contains(&format!("\"{value}\"")),
            "long URL should stay one chunk; got:\n{pp}"
        );
    }
}

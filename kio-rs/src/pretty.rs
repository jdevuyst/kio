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
//! Formatting preserves meaning and comments, follows the specified canonical
//! rules, and reaches a fixpoint after one pass. Idempotence checks establish
//! that fixpoint; output, structure, and comment assertions check the other
//! obligations independently.
//!
//! ## Architecture
//!
//! The printer builds a [`crate::doc::Doc`] tree (Wadler-Leijen
//! combinators) and renders it with [`crate::doc::render`] at the
//! canonical 100-column budget. [`comma_list`](crate::doc::comma_list)
//! drives the width-driven A1 break for every comma-separated list
//! (parameter lists, member lists, call args, tuples,
//! macro / `equiv` arm lists, etc.) — flat when it fits, leading-
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
    BridgeBlock, BridgeGlobSegment, BuildBlock, BuildBlockCache, CallArg, CallStage,
    DependencyFile, DocComment, Equiv, Expr, FnDef, HostFn, HostFnParam, HostType, Import,
    ImportItem, ImportKind, Item, Labels, LiteralAlias, LiteralAliasValue, LockFile, Module,
    ModulePath, Newtype, Op, OpBody, OpPart, PackageFile, Signature, SignatureGroupRef,
    SignatureParam, SourceBlock, SourceOrigin, Surface, TargetBlock, Type, TypeAlias, TypeParam,
    VariadicOperator,
};
use crate::ast::{DelimitedTrivia, SignatureTriviaPosition, append_relocated_trivia};
use crate::doc::{
    Doc, align, broken_comma_list, comma_list, concat, concat_all, empty, executable_block,
    flat_alt, group, hardline, join, line, nest, render, text,
};
use crate::pass::lexer::Trivia;

/// Canonical line-width budget used by every entry point, consumed by
/// [`crate::doc::Group`] fits-checks: `comma_list`'s width-driven A1
/// break flattens a comma-separated list when the flat form fits this
/// budget and breaks to the leading-comma layout when it doesn't
/// (`specs/style.md`'s 100-column rules).
const WIDTH: usize = 100;

pub(crate) fn canonical_float_digits(digits: &str) -> String {
    digits
        .chars()
        .filter(|&c| c != '+')
        .map(|c| if c == 'E' { 'e' } else { c })
        .collect()
}

pub fn pretty_module(m: &Module) -> String {
    render(&doc_module(m), WIDTH)
}

pub fn pretty_package_file(e: &PackageFile) -> String {
    render(&doc_package_file(e, true), WIDTH)
}

/// Internal fixture preparation preserves selectors unrelated to its chosen target.
#[cfg(feature = "cli")]
pub(crate) fn pretty_package_file_preserving_baselines(e: &PackageFile) -> String {
    render(&doc_package_file(e, false), WIDTH)
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
/// literal aliases, compact nominal types, or operators the declaration is itself header-shaped, so the
/// whole (doc-comment-stripped) form is the signature.
pub fn pretty_item_signature(item: &Item) -> String {
    let stripped = strip_item_doc(item);
    let doc = match &stripped {
        Item::FnDef(d) => doc_fn_def_header(d),
        Item::RecGroup(g, _) if g.members.len() == 1 => {
            let member = &g.members[0];
            let visibility = doc_vis(&member.vis);
            let mut rendered_member = member.clone();
            rendered_member.vis = crate::ast::Visibility::Private;
            concat_all([
                visibility,
                doc_rec_intro(g),
                doc_fn_def_header(&rendered_member),
            ])
        }
        Item::TypeRecGroup(group) => doc_type_rec_group(group),
        other => doc_item(other),
    };
    render(&doc, WIDTH)
}

/// Render a function header without copying its body.
#[cfg(any(feature = "lsp", feature = "repl-core"))]
pub(crate) fn pretty_fn_signature(def: &FnDef<Surface>) -> String {
    render(&doc_fn_def_header(def), WIDTH)
}

/// Render a recursive function member without copying its body.
#[cfg(any(feature = "lsp", feature = "repl-core"))]
pub(crate) fn pretty_rec_fn_signature(def: &FnDef<Surface, ()>) -> String {
    render(&doc_fn_def_header(def), WIDTH)
}

/// Canonical public-cache identity for a user macro declaration.
///
/// Captures constrain visibility; ordered trailing descriptors determine block
/// projection. The private implementation target and its execution schedule are
/// deliberately absent: same-package macro consumers track those via
/// the separate implementation-source fingerprint.
#[cfg(feature = "cli")]
pub(crate) fn pretty_macro_public_signature(macro_def: &crate::ast::UserMacroDef) -> String {
    let entries = doc_macro_public_entries(macro_def);
    let has_entries = !entries.is_empty();
    render(
        &concat_all([
            doc_vis(&macro_def.vis),
            text(format!("macro {}", macro_def.name)),
            text(" : "),
            doc_type(&macro_def.call_ty),
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
/// reports a kind-aware error. `lit` has no single standalone type,
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
        | Item::Newtype(_)
        | Item::Op(_, _)
        | Item::VariadicOperator(_, _)
        | Item::RecGroup(_, _)
        | Item::TypeRecGroup(_)
        | Item::Macro(_, _)
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
/// This renders a `Type<Surface>` directly, so it can show a type read straight off
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

        Item::Op(o, _) => o.doc = None,
        Item::VariadicOperator(f, _) => f.doc = None,
        Item::Macro(s, _) => s.doc = None,
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
    let mut prefix = render(&doc_vis(vis), WIDTH);
    // Source rendering strips line-end spaces; a prefix is followed by a token.
    if !prefix.is_empty() {
        prefix.push(' ');
    }
    prefix
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
fn doc_fn_def_header(d: &FnDef<Surface, impl FnPurityExt>) -> Doc {
    let pubword = doc_vis(&d.vis);
    let pureword = if d.purity.is_pure_fn() {
        text("pure ")
    } else {
        empty()
    };
    let ret_doc = if d.ret_elided || can_omit_unit_return(&d.ret) {
        empty()
    } else {
        doc_function_return(&d.ret, true)
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
    // above the `mod` line — before the `///` doc-comment and the
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
        text("mod ".to_owned()),
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
    doc_comment_run(trivia, CommentRunContext::ItemLeading)
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
/// module-path separator `/` (`mod a/b/c;`, `import a/b/c { … }`,
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
/// `// docstring` above `import path { fn foo }` stays above the
/// `import` regardless of where the sort places it within its block.
fn doc_import_blocks(imports: &[Import]) -> Doc {
    let mut intrinsics: Vec<(String, Doc)> = Vec::new();
    let mut others: Vec<(String, Doc)> = Vec::new();
    for u in imports {
        // Render this import's Doc to a string for sorting and re-emission.
        let buf = render(&doc_import(u), WIDTH);
        let comments = doc_comment_run(&u.leading_trivia, CommentRunContext::ImportLeading);
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
    for block in [intrinsics, others] {
        if block.is_empty() {
            continue;
        }
        if wrote_block {
            // Exactly one blank line between non-empty blocks.
            parts.push(hardline());
        }
        for (rendered, comments) in block {
            parts.push(comments);
            parts.push(text(rendered));
            parts.push(hardline());
        }
        wrote_block = true;
    }
    concat_all(parts)
}

fn doc_import(u: &Import) -> Doc {
    if matches!(u.kind, ImportKind::Selective { .. }) {
        doc_import_entry(u)
    } else {
        concat(doc_import_entry(u), text(";"))
    }
}

fn doc_import_entry(u: &Import) -> Doc {
    match &u.kind {
        ImportKind::Selective { items, from } => {
            let mut sorted: Vec<(&ImportItem, String)> = items
                .iter()
                .map(|item| (item, import_item_display(item)))
                .collect();
            sorted.sort_by_key(|a| import_item_sort_key(a.0));
            let provider = module_path_surface_str(&from.segments);
            let mut lines = Vec::new();
            for (index, (item, rendered)) in sorted.iter().enumerate() {
                if index > 0 {
                    lines.push(concat(text(";"), line()));
                }
                lines.push(doc_item_leading_trivia(item.leading_trivia()));
                lines.push(text(rendered.clone()));
            }
            let body = append_block_trailing(Some(concat_all(lines)), &u.trailing_trivia);
            group(doc_braced_block(
                text(format!("import {provider}")),
                body,
                true,
            ))
        }
        ImportKind::Qualified { path, alias } => text(format!(
            "import {} as {alias}",
            module_path_surface_str(&path.segments)
        )),
        ImportKind::Intrinsics => text("import __intrinsics__"),
        ImportKind::Comptime => text("import __comptime__"),
    }
}

fn import_item_sort_key(item: &ImportItem) -> (u8, String) {
    match item {
        ImportItem::Name { kind, name, .. } => (
            if *kind == crate::ast::ImportNameKind::Macro {
                1
            } else {
                0
            },
            name.clone(),
        ),
        ImportItem::OperatorPattern { grammar, .. } => {
            let group = if matches!(grammar, crate::ast::OperatorGrammar::Fixed { .. }) {
                2
            } else {
                3
            };
            let spelling = grammar.render();
            (
                group,
                spelling
                    .split_once(' ')
                    .map_or("", |(_, pattern)| pattern)
                    .to_owned(),
            )
        }
    }
}

fn import_item_display(item: &crate::ast::ImportItem) -> String {
    match item {
        ImportItem::Name {
            kind, name, alias, ..
        } => {
            let prefix = format!("{} ", kind.spelling());
            alias.as_ref().map_or_else(
                || format!("{prefix}{name}"),
                |alias| format!("{prefix}{name} as {}", alias.local),
            )
        }
        ImportItem::OperatorPattern { grammar, alias, .. } => {
            let mut rendered = grammar.render();
            if let Some(alias) = alias {
                let local = alias.local.render();
                let tag = match &alias.local {
                    crate::ast::OperatorGrammar::Fixed {
                        kind: crate::ast::FixedOperatorKind::Macro,
                        ..
                    } => "op_macro ",
                    crate::ast::OperatorGrammar::Fixed { .. } => "op ",
                    crate::ast::OperatorGrammar::Variadic { .. } => "op_var ",
                };
                let spelling = local
                    .strip_prefix(tag)
                    .expect("operator rendering has a namespace tag");
                rendered.push_str(" as ");
                rendered.push_str(spelling);
            }
            rendered
        }
    }
}

fn doc_item(item: &Item) -> Doc {
    if let Item::TypeAlias(alias) = item {
        return doc_type_alias_with_terminator(alias, true);
    }
    if let Item::Labels(labels, _) = item {
        return doc_labels_with_terminator(labels, labels.type_alias_name.is_some());
    }
    let body = doc_item_entry(item);
    if matches!(
        item,
        Item::LiteralAlias(..) | Item::HostType(_) | Item::HostFn(_)
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

        Item::Equiv(e, _) => doc_equiv(e),
        Item::Macro(s, _) => doc_macro_item(s),
        Item::RecGroup(g, _) => doc_rec_group(g),
        Item::TypeRecGroup(g) => doc_type_rec_group(g),
        Item::Op(d, _) => doc_op(d),
        Item::VariadicOperator(d, _) => doc_fold(d),
        Item::HostType(h) => doc_host_type(h),
        Item::HostFn(h) => doc_host_fn(h),
    }
}

fn doc_rec_intro(g: &crate::ast::RecGroup) -> Doc {
    let loop_path = value_path_surface_str(&g.loop_path);
    let Some(trivia) = &g.source_trivia else {
        return text(format!("rec({loop_path}) "));
    };
    let header = if has_comments(&trivia.leading) || has_comments(&trivia.closing) {
        let mut content = vec![
            hardline(),
            doc_inline_leading_trivia(&trivia.leading),
            text(loop_path),
        ];
        if has_comments(&trivia.closing) {
            content.extend([hardline(), doc_trailing_comments(&trivia.closing)]);
        }
        concat_all([
            text("rec("),
            nest(2, concat_all(content)),
            hardline(),
            text(")"),
        ])
    } else {
        text(format!("rec({loop_path})"))
    };
    if has_comments(&trivia.after) {
        concat_all([header, hardline(), doc_inline_leading_trivia(&trivia.after)])
    } else {
        concat(header, text(" "))
    }
}

fn doc_rec_group(g: &crate::ast::RecGroup) -> Doc {
    let outer_doc = (g.members.len() == 1)
        .then(|| g.members[0].doc.as_ref())
        .flatten()
        .filter(|doc| doc.span.end <= g.meta.span.start);
    let member_comments_follow_header = g.source_trivia.is_some()
        && g.members.first().is_some_and(|member| {
            (member.doc.is_some() && outer_doc.is_none())
                || has_comments(&member.meta.leading_trivia)
        });
    if g.members.len() == 1
        && !has_comments(g.meta.trailing_trivia.as_slice())
        && !member_comments_follow_header
    {
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
            doc_rec_intro(g),
            doc_fn_def(&rendered_member),
        ]);
    }
    let member_docs: Vec<Doc> = g
        .members
        .iter()
        .map(|member| {
            let mut rendered_member = member.clone();
            if outer_doc.is_some() {
                rendered_member.doc = None;
            }
            concat_all([
                doc_item_leading_trivia(member.meta.leading_trivia.as_slice()),
                doc_fn_def(&rendered_member),
            ])
        })
        .collect();
    concat_all([
        outer_doc.map(doc_doc_comment).unwrap_or_else(empty),
        doc_rec_intro(g),
        text("{"),
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
    // Delimiter trivia stays on its side of the opening brace: hoisting an
    // interior `///` before `rec` would turn it into group documentation.
    let boundary_trivia = g.meta.trailing_trivia.as_slice();
    let opening_end = g.open_brace_span.map_or(0, |open| {
        boundary_trivia
            .iter()
            .rposition(|trivia| match trivia {
                Trivia::LineComment { span, .. } | Trivia::DocCommentLine { span, .. } => {
                    span.end <= open.start
                }
                Trivia::Newline => false,
            })
            .map_or(0, |index| index + 1)
    });
    let (opening, trailing) = boundary_trivia.split_at(opening_end);
    let intro = if has_comments(opening) {
        concat_all([
            text("rec"),
            hardline(),
            doc_item_leading_trivia(opening),
            text("{"),
        ])
    } else {
        text("rec {")
    };
    let trailing = if has_comments(trailing) {
        concat(hardline(), doc_item_leading_trivia(trailing))
    } else {
        empty()
    };
    concat_all([
        doc_prefix,
        intro,
        nest(
            2,
            concat(hardline(), join(concat(text(";"), hardline()), members)),
        ),
        nest(2, trailing),
        hardline(),
        text("}"),
    ])
}

fn doc_fn_def(d: &FnDef<Surface, impl FnPurityExt>) -> Doc {
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
    let ret_doc = if d.ret_elided || can_omit_unit_return(&d.ret) {
        empty()
    } else {
        doc_function_return(&d.ret, true)
    };
    concat_all([
        doc_prefix,
        pubword,
        pureword,
        text(format!("fn {}", d.name)),
        doc_signature(&d.sig),
        ret_doc,
        group(doc_block_body_expr(&d.body)),
    ])
}

fn doc_macro_item(s: &crate::ast::UserMacroDef) -> Doc {
    let doc_prefix = s.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&s.vis);
    let mut entries = doc_macro_public_entries(s);
    entries.push(concat_all([
        text(match s.schedule {
            crate::ast::MacroSchedule::Late => "impl ",
            crate::ast::MacroSchedule::Fills => "impl(fills) ",
        }),
        text(value_path_surface_str(s.implementation.segments())),
    ]));
    concat_all([
        doc_prefix,
        pubword,
        text(format!("macro {}", s.name)),
        text(" : "),
        doc_type(&s.call_ty),
        doc_compact_declaration_body(&s.body_trivia, join(text("; "), entries)),
    ])
}

fn doc_macro_public_entries(s: &crate::ast::UserMacroDef) -> Vec<Doc> {
    let mut entries = Vec::new();
    if !s.captures.is_empty() {
        entries.push(doc_macro_captures(s));
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

fn doc_macro_captures(s: &crate::ast::UserMacroDef) -> Doc {
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

fn doc_capture_path(capture: &crate::ast::UserMacroCapture) -> Doc {
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
/// is shared by every executable body.
fn doc_block_body_expr(body: &Expr) -> Doc {
    let content = doc_expr_with_own_meta_trivia(body);
    doc_executable_block(Some(content))
}

/// Block-body layout for pre-rendered content and its separately owned closer.
/// A dangling comment forces the broken layout.
fn doc_block_body_with_trailing(content: Doc, trailing: &[Trivia]) -> Doc {
    if has_comments(trailing) {
        let with_dangling = concat_all([content, hardline(), doc_trailing_comments(trailing)]);
        doc_executable_block(Some(with_dangling))
    } else {
        doc_executable_block(Some(content))
    }
}

fn doc_executable_block(body: Option<Doc>) -> Doc {
    let layout = match body {
        Some(body) => doc_braced_block(empty(), Some(body), true),
        None => flat_alt(text(" {}"), concat_all([text(" {"), hardline(), text("}")])),
    };
    executable_block(layout)
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

fn doc_literal_alias(a: &LiteralAlias) -> Doc {
    let doc_prefix = a.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let pubword = doc_vis(&a.vis);
    concat_all([
        doc_prefix,
        pubword,
        text("lit"),
        if has_comments(&a.meta.trailing_trivia) {
            concat_all([
                hardline(),
                doc_inline_leading_trivia(&a.meta.trailing_trivia),
            ])
        } else {
            text(" ")
        },
        text(format!("{} = ", a.name)),
        doc_literal_alias_value(&a.value),
    ])
}

fn doc_literal_alias_value(value: &LiteralAliasValue) -> Doc {
    match value {
        LiteralAliasValue::Str { value, .. } => doc_string_literal(value),
        LiteralAliasValue::Int { digits, .. } => text(digits.clone()),
        LiteralAliasValue::Float { digits, .. } => {
            let canonical = canonical_float_digits(digits);
            text(canonical)
        }
        LiteralAliasValue::Bool { value, .. } => text(if *value { ".t" } else { ".f" }),
    }
}

fn doc_newtype(d: &Newtype) -> Doc {
    let entry = crate::ast::LabelEntry {
        name: d.name.clone(),
        name_span: d.name_span,
        type_params: d.type_params.clone(),
        existential_params: d.existential_params.clone(),
        payload: d.payload.clone(),
        members: Some(crate::ast::NominalMembers {
            constructor: d.constructor.clone(),
            projector: d.projector.clone(),
            trailing_trivia: d.meta.trailing_trivia.clone(),
        }),
        meta: crate::ast::Meta::new(d.meta.span),
    };
    concat_all([
        d.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty),
        doc_vis(&d.vis),
        if d.rec_span.is_some() {
            text("rec ")
        } else {
            empty()
        },
        text("type "),
        doc_labels_arm(&[entry]),
    ])
}

fn doc_op(d: &Op) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    if let Some(dc) = &d.doc {
        parts.push(doc_doc_comment(dc));
    }
    parts.push(doc_vis(&d.vis));
    match &d.body {
        OpBody::Normal {
            kind,
            pattern,
            function,
        } => {
            parts.push(text(if *kind == crate::ast::FixedOperatorKind::Macro {
                "op_macro"
            } else {
                "op"
            }));
            for part in pattern {
                parts.push(text(" "));
                match part {
                    OpPart::SlotPlain { .. }
                    | OpPart::SlotRecursive { .. }
                    | OpPart::SlotGreedy { .. } => parts.push(text(part.slot_spelling().unwrap())),
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
    parts.push(text("op_var"));
    doc_fold_pattern_and_body(
        &mut parts,
        &d.open,
        &d.spec,
        &d.body_trivia,
        d.finalize_keyword_span.is_some(),
    );
    concat_all(parts)
}

fn doc_fold_pattern_and_body(
    parts: &mut Vec<Doc>,
    open: &[String],
    spec: &crate::ast::VariadicSpec,
    body_trivia: &[Trivia],
    explicit_finalizer: bool,
) {
    parts.push(text(" "));
    parts.push(text(if open == ["["] && spec.close == ["]"] {
        "[]".to_owned()
    } else {
        format!("{} {}", open.join(" "), spec.close.join(" "))
    }));
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
    } else if explicit_finalizer {
        body.push(text("; finalize ()"));
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
    doc_labels_with_terminator(d, false)
}

fn doc_labels_with_terminator(d: &Labels, terminated: bool) -> Doc {
    let mut head = vec![
        d.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty),
        doc_vis(&d.vis),
        if d.rec_span.is_some() {
            text("rec ")
        } else {
            empty()
        },
        text("type "),
    ];
    if let Some(name) = &d.type_alias_name {
        head.push(text(name.clone()));
        head.push(doc_type_params(&d.type_alias_params));
        let rhs = d.type_alias_rhs.as_ref().expect("named compact RHS");
        let body = doc_nominal_rhs(
            rhs,
            d.type_alias_arms.as_deref().expect("named compact groups"),
        );
        let terminator = if terminated { text(";") } else { empty() };
        let flat = concat_all([text(" = "), body.clone(), terminator.clone()]);
        head.push(
            if matches!(
                rhs,
                crate::ast::NominalRhs::Product(..) | crate::ast::NominalRhs::Sum(..)
            ) {
                group(flat_alt(
                    flat,
                    nest(
                        2,
                        concat_all([
                            text(" ="),
                            hardline(),
                            body,
                            if terminated { hardline() } else { empty() },
                            terminator,
                        ]),
                    ),
                ))
            } else {
                flat
            },
        );
    } else {
        head.push(doc_labels_arm(&d.entries));
    }
    concat_all(head)
}

fn doc_nominal_rhs(rhs: &crate::ast::NominalRhs, arms: &[crate::ast::LabelsArm]) -> Doc {
    use crate::ast::NominalRhs;
    match rhs {
        NominalRhs::Group(index) => doc_labels_arm(&arms[*index].entries),
        NominalRhs::Type(ty) => {
            if matches!(
                ty,
                Type::Function { .. }
                    | Type::Forall { .. }
                    | Type::Product { .. }
                    | Type::Sum { .. }
            ) {
                concat_all([text("("), doc_type(ty), text(")")])
            } else {
                doc_type(ty)
            }
        }
        NominalRhs::Product(..) => doc_nominal_chain(rhs, arms, ChainOp::Amp),
        NominalRhs::Sum(..) => doc_nominal_chain(rhs, arms, ChainOp::Pipe),
        NominalRhs::Grouped(inner, meta) => concat_all([
            doc_inline_leading_trivia(&meta.leading_trivia),
            text("("),
            doc_nominal_rhs(inner, arms),
            if has_comments(&meta.trailing_trivia) {
                concat_all([
                    hardline(),
                    doc_trailing_comments(&meta.trailing_trivia),
                    hardline(),
                ])
            } else {
                empty()
            },
            text(")"),
        ]),
    }
}

fn doc_nominal_chain(
    rhs: &crate::ast::NominalRhs,
    arms: &[crate::ast::LabelsArm],
    op: ChainOp,
) -> Doc {
    use crate::ast::NominalRhs;
    let mut docs = Vec::new();
    let mut trailing = Vec::new();
    let mut current = rhs;
    loop {
        let (left, right, meta) = match (current, op) {
            (NominalRhs::Product(left, right, meta), ChainOp::Amp)
            | (NominalRhs::Sum(left, right, meta), ChainOp::Pipe) => (left, right, meta),
            _ => {
                docs.push(doc_nominal_rhs(current, arms));
                break;
            }
        };
        docs.push(concat_all([
            doc_inline_leading_trivia(&meta.leading_trivia),
            doc_nominal_rhs(left, arms),
        ]));
        if has_comments(&meta.trailing_trivia) {
            trailing.push(doc_trailing_comments(&meta.trailing_trivia));
        }
        current = right;
    }
    if !trailing.is_empty() {
        let last = docs.pop().expect("chain operands");
        docs.push(concat_all(std::iter::once(last).chain(
            trailing.into_iter().rev().flat_map(|doc| [hardline(), doc]),
        )));
    }
    let flat = join(text(op.separator()), docs.clone());
    let broken = join(
        hardline(),
        docs.into_iter().map(|doc| concat(text(op.prefix()), doc)),
    );
    group(flat_alt(flat, broken))
}

fn doc_labels_arm(entries: &[crate::ast::LabelEntry]) -> Doc {
    let comments = entries.iter().any(|entry| {
        has_comments(&entry.meta.leading_trivia) || has_comments(&entry.meta.trailing_trivia)
    });
    let flat = concat_all([
        text("{ "),
        join(
            text(" "),
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| doc_label_entry(entry, index + 1 < entries.len())),
        ),
        text(" }"),
    ]);
    let mut body = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        body.push(hardline());
        body.push(doc_inline_leading_trivia(&entry.meta.leading_trivia));
        body.push(doc_label_entry(entry, index + 1 < entries.len()));
        if has_comments(&entry.meta.trailing_trivia) {
            body.push(hardline());
            body.push(doc_trailing_comments(&entry.meta.trailing_trivia));
        }
    }
    let broken = concat_all([text("{"), nest(2, concat_all(body)), hardline(), text("}")]);
    if comments {
        broken
    } else {
        group(flat_alt(flat, broken))
    }
}

fn doc_nominal_members(members: &crate::ast::NominalMembers) -> Doc {
    let member = |value: &crate::ast::TypeMember| {
        concat_all([
            doc_inline_leading_trivia(&value.leading_trivia),
            doc_vis(&value.vis),
            text(value.name.clone()),
        ])
    };
    let constructor = member(&members.constructor);
    let projector = member(&members.projector);
    let flat = concat_all([
        text("{ "),
        constructor.clone(),
        text("; "),
        projector.clone(),
        text(" }"),
    ]);
    let mut body = vec![hardline(), constructor, text(";"), hardline(), projector];
    if has_comments(&members.trailing_trivia) {
        body.push(hardline());
        body.push(doc_trailing_comments(&members.trailing_trivia));
    }
    let broken = concat_all([text("{"), nest(2, concat_all(body)), hardline(), text("}")]);
    if has_comments(&members.constructor.leading_trivia)
        || has_comments(&members.projector.leading_trivia)
        || has_comments(&members.trailing_trivia)
    {
        broken
    } else {
        group(flat_alt(flat, broken))
    }
}

fn doc_label_entry(entry: &crate::ast::LabelEntry, terminated: bool) -> Doc {
    let mut parts = vec![
        text(entry.name.clone()),
        doc_type_params(&entry.type_params),
    ];
    if !entry.existential_params.is_empty() {
        parts.push(text(" "));
        parts.push(doc_existential_params(&entry.existential_params));
    }
    let payload = doc_type(&entry.payload);
    let body = if let Some(members) = &entry.members {
        let suffix = concat_all([
            doc_nominal_members(members),
            if terminated { text(";") } else { empty() },
        ]);
        doc_chain_body_layout(
            &entry.payload,
            payload,
            ": ",
            ":",
            concat(text(" "), suffix.clone()),
            suffix,
        )
    } else if terminated {
        doc_chain_body_layout(&entry.payload, payload, ": ", ":", text(";"), text(";"))
    } else if matches!(entry.payload, Type::Product { .. } | Type::Sum { .. }) {
        group(flat_alt(
            concat(text(": "), payload.clone()),
            nest(2, concat_all([text(":"), hardline(), payload])),
        ))
    } else {
        concat(text(": "), payload)
    };
    parts.push(body);
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

fn signature_boundary_trivia(
    source: &Option<Box<crate::ast::SignatureTrivia>>,
    index: usize,
    position: SignatureTriviaPosition,
) -> &[Trivia] {
    source
        .as_ref()
        .and_then(|trivia| {
            trivia
                .anchors
                .iter()
                .find(|anchor| anchor.group == index && anchor.position == position)
        })
        .map_or(&[], |anchor| anchor.trivia.as_slice())
}

fn doc_signature_type_binder(tp: &TypeParam, opening: &[Trivia], closing: &[Trivia]) -> Doc {
    if !has_comments(opening) && !has_comments(closing) {
        return text(type_binder_text(tp));
    }
    let binder = type_binder_text(tp);
    let mut inner = vec![
        hardline(),
        doc_inline_leading_trivia(opening),
        text(binder[1..binder.len() - 1].to_owned()),
    ];
    if has_comments(closing) {
        inner.extend([hardline(), doc_trailing_comments(closing)]);
    }
    inner.extend([hardline(), text("]")]);
    concat(text("["), nest(2, concat_all(inner)))
}

fn doc_signature_group(group: Doc, before: &[Trivia]) -> Doc {
    if has_comments(before) {
        nest(
            2,
            concat_all([hardline(), doc_inline_leading_trivia(before), group]),
        )
    } else {
        group
    }
}

fn doc_signature(sig: &Signature) -> Doc {
    let groups = sig.canonical_groups();
    if groups.is_empty() {
        return text("()");
    }
    let mut parts = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let before = signature_boundary_trivia(
            &sig.source_trivia,
            index,
            SignatureTriviaPosition::BeforeGroup,
        );
        let opening = signature_boundary_trivia(
            &sig.source_trivia,
            index,
            SignatureTriviaPosition::InsideOpen,
        );
        let closing = signature_boundary_trivia(
            &sig.source_trivia,
            index,
            SignatureTriviaPosition::InsideClose,
        );
        let group_doc = match group {
            SignatureGroupRef::Type(params) => {
                let [SignatureParam::Type(tp)] = params else {
                    unreachable!("canonical signature type group has one binder")
                };
                doc_signature_type_binder(tp, opening, closing)
            }
            SignatureGroupRef::Value(params) => {
                let docs = params.iter().map(doc_signature_param).collect();
                let leading: Vec<_> = params.iter().map(signature_param_leading_trivia).collect();
                let trailing: Vec<_> = params
                    .iter()
                    .map(|p| signature_param_trailing_trivia(p).to_vec())
                    .collect();
                doc_comma_list_with_boundaries(
                    text("("),
                    docs,
                    &leading,
                    &trailing,
                    closing,
                    text(")"),
                    false,
                )
            }
        };
        parts.push(doc_signature_group(group_doc, before));
    }
    concat_all(parts)
}

fn can_omit_unit_return(ty: &Type) -> bool {
    matches!(ty, Type::Unit { .. })
        && !has_comments(&ty.meta().leading_trivia)
        && !has_comments(&ty.meta().trailing_trivia)
}

fn doc_function_return(ty: &Type, chain_layout: bool) -> Doc {
    if chain_layout {
        doc_chain_body_layout(ty, doc_type(ty), " -> ", " ->", empty(), empty())
    } else {
        concat(text(" -> "), doc_type(ty))
    }
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

/// A parameter's own trailing run, separate from the signature group's closer;
/// empty for a type-binder parameter.
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
    match &pat.source_trivia {
        Some(trivia) => doc_comma_list_with_trivia(
            text("("),
            elems,
            &trivia.elem_leading,
            &trivia.closing,
            text(")"),
        ),
        None => comma_list(text("("), elems, text(")")),
    }
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
        if !items.is_empty()
            && (has_comments(&cur.meta().leading_trivia)
                || has_comments(&cur.meta().trailing_trivia))
        {
            items.push(cur);
            return items;
        }
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
fn doc_type_chain_child(t: &Type, parent_op: ChainOp, leading: &[Trivia]) -> Doc {
    if has_comments(leading) || has_comments(&t.meta().trailing_trivia) {
        return doc_type_with_boundary_trivia(t, leading, &t.meta().trailing_trivia);
    }
    let needs_parens = matches!(
        (t, parent_op),
        (Type::Product { .. }, _)
            | (Type::Sum { .. }, _)
            | (Type::Function { .. }, _)
            | (Type::Forall { .. }, _)
    );
    if needs_parens {
        concat_all([text("("), doc_type_core(t), text(")")])
    } else {
        doc_type_core(t)
    }
}

/// Build a chain doc from rendered items (≥2). Flat single-line:
/// `A & B & C`. Broken multi-line when the enclosing group can't
/// fit: each item on its own line prefixed with `& ` (or `| `) at
/// the surrounding nest's indent — the leading-operator A1
/// analogue. Mirrors `comma_list`'s flat-vs-broken structure.
///
fn doc_chain(items: Vec<&Type>, op: ChainOp) -> Doc {
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
fn doc_chain_paren_when_broken(items: Vec<&Type>, op: ChainOp) -> Doc {
    doc_chain_inner(items, op, true)
}

fn doc_chain_inner(items: Vec<&Type>, op: ChainOp, paren_broken: bool) -> Doc {
    debug_assert!(items.len() >= 2);
    let docs: Vec<Doc> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            // Later operands own comments on their preceding separator;
            // the first operand has no such boundary outside its grouping.
            let leading = if index == 0 {
                item.meta().leading_trivia.as_slice()
            } else {
                &[]
            };
            doc_type_chain_child(item, op, leading)
        })
        .collect();
    let flat = join(text(op.separator()), docs.clone());
    let has_trivia = items.iter().any(|item| {
        has_comments(&item.meta().leading_trivia) || has_comments(&item.meta().trailing_trivia)
    });
    let mut broken = Doc::Empty;
    let mut first = true;
    for (item, doc) in items.into_iter().zip(docs) {
        if !first {
            broken = concat_all([
                broken,
                hardline(),
                doc_inline_leading_trivia(&item.meta().leading_trivia),
            ]);
        }
        broken = concat_all([broken, text(op.prefix()), doc]);
        first = false;
    }
    let broken = if paren_broken {
        concat_all([text("("), broken, text(")")])
    } else {
        broken
    };
    if has_trivia {
        broken
    } else {
        group(flat_alt(flat, broken))
    }
}

/// Render a `Sum` / `Product` type at a **bare** position (a call
/// argument or intrinsic-form type operand), where a broken chain's
/// leading operator needs enclosing parens to round-trip. Falls back to
/// the ordinary [`doc_type`] for non-chain types.
fn doc_type_bare_position(t: &Type) -> Doc {
    match t {
        Type::Product { .. } => {
            let items = flatten_chain(t, ChainOp::Amp);
            doc_chain_paren_when_broken(items, ChainOp::Amp)
        }
        Type::Sum { .. } => {
            let items = flatten_chain(t, ChainOp::Pipe);
            doc_chain_paren_when_broken(items, ChainOp::Pipe)
        }
        _ => doc_type_core(t),
    }
}

fn collect_forall_prefix(mut cur: &Type) -> (Vec<&TypeParam>, &Type) {
    let mut params = Vec::new();
    while let Type::Forall { param, body, .. } = cur {
        params.push(param);
        cur = body.as_ref();
        if has_comments(&cur.meta().leading_trivia) || has_comments(&cur.meta().trailing_trivia) {
            break;
        }
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
    doc_type_with_boundary_trivia(t, &t.meta().leading_trivia, &t.meta().trailing_trivia)
}

fn doc_type_with_boundary_trivia(t: &Type, leading: &[Trivia], trailing: &[Trivia]) -> Doc {
    let body = doc_type_core(t);
    if !has_comments(leading) && !has_comments(trailing) {
        return body;
    }
    let mut inner = vec![hardline(), doc_inline_leading_trivia(leading), body];
    if has_comments(trailing) {
        inner.extend([hardline(), doc_trailing_comments(trailing)]);
    }
    inner.extend([hardline(), text(")")]);
    concat(text("("), nest(2, concat_all(inner)))
}

fn doc_type_core(t: &Type) -> Doc {
    match t {
        Type::Path { segments, args, .. } => concat_all(
            std::iter::once(text(type_path_surface_str(segments)))
                .chain(args.iter().map(doc_type_argument)),
        ),
        Type::Unit { .. } => text("."),
        Type::Bottom { .. } => text("!"),
        Type::Function { param, ret, .. } => concat(
            doc_function_domain(param),
            if matches!(ret.as_ref(), Type::Product { .. } | Type::Sum { .. }) {
                doc_chain_body_layout(ret, doc_type(ret), " -> (", " -> (", text(")"), text(")"))
            } else {
                concat(text(" -> "), doc_type(ret))
            },
        ),
        Type::Product { .. } => {
            let items = flatten_chain(t, ChainOp::Amp);
            doc_chain(items, ChainOp::Amp)
        }
        Type::Sum { .. } => {
            let items = flatten_chain(t, ChainOp::Pipe);
            doc_chain(items, ChainOp::Pipe)
        }
        Type::Forall { .. } => doc_forall_type(t),
        Type::Infer { .. } => text("_".to_owned()),
        Type::Goal { ext, .. } => match *ext {},
    }
}

// ---- expressions -------------------------------------------------------

fn doc_expr(e: &Expr) -> Doc {
    let doc = doc_expr_layout(e);
    if matches!(e, Expr::BlockCall { .. }) {
        group(doc)
    } else {
        doc
    }
}

/// Cuddled trailing blocks share this layout decision, including a final
/// labelled call whose wrapper braces are elided. Their bodies retain their
/// own groups, so expanding the arms does not expand nested expressions.
fn doc_expr_layout(e: &Expr) -> Doc {
    if let Some(doc) = doc_source_existential_let(e) {
        return doc;
    }
    match e {
        Expr::Path { segments, .. } => text(segments.join(".")),
        Expr::Call {
            callee,
            args,
            source_trivia,
            ..
        } => {
            let head = doc_expr_for_call_callee(callee);
            doc_call(head, args, delimited_closing(source_trivia))
        }
        Expr::RecCall {
            modes,
            callee,
            stages,
            ..
        } => {
            let head = if modes.is_empty() {
                format!("recur {}", callee.name)
            } else {
                let names = crate::ast::RecCallMode::canonical_modes(modes)
                    .into_iter()
                    .map(|mode| mode.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("recur({names}) {}", callee.name)
            };
            doc_call_stages(text(head), stages)
        }
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            let mut parts: Vec<Doc> = Vec::with_capacity(4);
            parts.push(text("."));
            parts.push(doc_signature(sig));
            if let Some(ty) = ret_ty {
                parts.push(doc_function_return(ty, false));
            }
            // Body emits with its own leading-trivia honored — a
            // comment captured between the fn body's `{` and the
            // first expression flows onto `body.meta.leading_trivia`
            // and must emit above the body so the round-trip
            // preserves it. A comment dangling before the closing `}`
            // rides `body.meta.trailing_trivia` and emits before it.
            parts.push(group(doc_block_body_expr(body)));
            concat_all(parts)
        }
        Expr::FnPlaceholder { stem, body, .. } => concat(
            text(format!(".{}.", stem.name)),
            group(doc_block_body_expr(body)),
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
            &[],
        ),
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => doc_seq(value, body, &meta.leading_trivia, &[]),
        Expr::Unit { source_trivia, .. } => doc_comma_list_with_trivia(
            text("("),
            Vec::new(),
            &[],
            delimited_closing(source_trivia),
            text(")"),
        ),
        Expr::ComptimeLiteral {
            value,
            proof,
            source_trivia,
            ..
        } => {
            let literal = match value {
                crate::ast::ComptimeLiteralValue::Bool(value) => {
                    text(if *value { ".t" } else { ".f" })
                }
                crate::ast::ComptimeLiteralValue::Str(value) => text(escape_string(value)),
            };
            doc_comma_list_with_trivia(
                concat_all([text("__"), literal, text("__(")]),
                vec![doc_expr_no_own_trivia(proof)],
                &[proof.meta().leading_trivia.clone()],
                delimited_closing(source_trivia),
                text(")"),
            )
        }
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
            let canonical = canonical_float_digits(digits);
            concat(text(canonical), doc_literal_annotation(annotation))
        }
        Expr::BoolLit {
            value, annotation, ..
        } => concat(
            text(if *value { ".t" } else { ".f" }),
            doc_literal_annotation(annotation),
        ),
        Expr::Tuple { items, .. } => {
            let item_docs: Vec<Doc> = items.iter().map(doc_expr_no_own_trivia).collect();
            let item_trivia: Vec<Vec<crate::pass::lexer::Trivia>> = items
                .iter()
                .map(|item| item.meta().leading_trivia.clone())
                .collect();
            let tails: Vec<_> = items
                .iter()
                .map(|item| item.meta().trailing_trivia.clone())
                .collect();
            doc_comma_list_with_boundaries(
                text("("),
                item_docs,
                &item_trivia,
                &tails,
                &[],
                text(")"),
                false,
            )
        }
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserMacro { name, stages, .. } => doc_call_stages(text(name.clone()), stages),
        Expr::BlockCall {
            head,
            prefix,
            blocks,
            splice,
            ..
        } => {
            let (prefix, splice) = block_splice_parts(prefix, *splice);
            let mut parts = Vec::new();
            if let Some((receiver, flavor)) = splice
                && flavor.callee_on_right()
            {
                parts.extend([
                    doc_expr_for_postfix_receiver(receiver),
                    text(flavor.token()),
                ]);
            }
            parts.push(text(head.name.clone()));
            for (stage, args) in prefix {
                parts.push(doc_call(
                    text(""),
                    args,
                    delimited_closing(&stage.source_trivia),
                ));
            }
            for (block_index, block) in blocks.iter().enumerate() {
                if has_comments(&block.leading) {
                    parts.extend([hardline(), doc_inline_leading_trivia(&block.leading)]);
                }
                if let Some(label) = &block.label {
                    parts.push(text(format!(" {}", label.name)));
                }
                let elide = block_index + 1 == blocks.len()
                    && splice.is_none_or(|(_, flavor)| flavor.callee_on_right())
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
                            value: Expr::BlockCall { meta, splice: None, .. },
                            ..
                        }] if !has_comments(&meta.leading_trivia)
                            && !has_comments(&meta.trailing_trivia)
                    );
                let item_docs = block
                    .items
                    .iter()
                    .map(|item| match item {
                        crate::ast::NeutralItem::Expression { value, .. } => {
                            if elide {
                                doc_expr_layout(value)
                            } else {
                                doc_expr_with_own_meta_trivia(value)
                            }
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
                        content.push(line());
                    }
                }
                if elide {
                    parts.extend([text(" "), concat_all(content)]);
                } else if block.items.is_empty() && !has_comments(&block.trailing) {
                    parts.push(doc_executable_block(None));
                } else {
                    parts.push(doc_block_body_with_trailing(
                        concat_all(content),
                        &block.trailing,
                    ));
                }
            }
            if let Some((receiver, flavor)) = splice
                && !flavor.callee_on_right()
            {
                parts.extend([text(flavor.token()), doc_expr_for_call_splice_arg(receiver)]);
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

fn doc_call_stages(mut head: Doc, stages: &[CallStage]) -> Doc {
    for stage in stages {
        head = doc_call(head, &stage.args, delimited_closing(&stage.source_trivia));
    }
    head
}

type BlockSpliceParts<'a> = (
    Vec<(&'a CallStage, &'a [CallArg])>,
    Option<(&'a Expr, crate::ast::UfcsFlavor)>,
);

fn block_splice_parts(
    prefix: &[CallStage],
    splice: Option<crate::ast::UfcsFlavor>,
) -> BlockSpliceParts<'_> {
    let Some(flavor) = splice else {
        return (
            prefix
                .iter()
                .map(|stage| (stage, stage.args.as_slice()))
                .collect(),
            None,
        );
    };
    let target = if flavor.callee_on_right() {
        0
    } else {
        prefix.len() - 1
    };
    let mut receiver = None;
    let written = prefix
        .iter()
        .enumerate()
        .filter_map(|(index, stage)| {
            if index != target {
                return Some((stage, stage.args.as_slice()));
            }
            let (arg, args) = if flavor.inserts_first() {
                stage.args.split_first()
            } else {
                stage.args.split_last()
            }
            .expect("a spliced block prefix contains its receiver");
            let CallArg::Value(value) = arg else {
                unreachable!("UFCS inserts a value")
            };
            receiver = Some((value, flavor));
            (!args.is_empty()).then_some((stage, args))
        })
        .collect();
    (written, receiver)
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
        &doc_expr_statement_value,
        &doc_expr_with_own_meta_trivia,
    ))
}

fn doc_expr_for_call_callee(callee: &Expr) -> Doc {
    doc_expr_grouped_boundary(callee, call_callee_needs_parens(callee), true, true)
}

fn expr_boundary_has_comments(expr: &Expr, leading: bool, trailing: bool) -> bool {
    (leading && has_comments(&expr.meta().leading_trivia))
        || (trailing && has_comments(&expr.meta().trailing_trivia))
}

/// Retain the grouping boundary when a following suffix or semicolon would
/// otherwise consume the expression's comments on the next parse.
fn doc_expr_grouped_boundary(expr: &Expr, required: bool, leading: bool, trailing: bool) -> Doc {
    let before = if leading {
        expr.meta().leading_trivia.as_slice()
    } else {
        &[]
    };
    let after = if trailing {
        expr.meta().trailing_trivia.as_slice()
    } else {
        &[]
    };
    let rendered = doc_expr_no_own_trivia(expr);
    if expr_boundary_has_comments(expr, leading, trailing) {
        concat(
            text("("),
            nest(
                2,
                concat_all([
                    hardline(),
                    doc_inline_leading_trivia(before),
                    rendered,
                    if has_comments(after) {
                        concat(hardline(), doc_trailing_comments(after))
                    } else {
                        empty()
                    },
                    hardline(),
                    text(")"),
                ]),
            ),
        )
    } else if required {
        concat_all([text("("), rendered, text(")")])
    } else {
        rendered
    }
}

fn doc_expr_statement_value(expr: &Expr) -> Doc {
    doc_expr_grouped_boundary(expr, false, false, true)
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
    let owns_written_suffixes = matches!(
        callee,
        Expr::Ufcs { .. } | Expr::BlockCall { .. } | Expr::RecCall { .. }
    );
    let normal_operator = operator_expr_needs_grouping(callee);
    bare_literal || owns_written_suffixes || normal_operator
}

/// Render an expression that owns a following postfix receiver suffix.
/// Operand-ending chains need grouping here so the suffix does not attach to
/// the chain's final operand. This boundary is shared by field
/// access/update. UFCS consumes its receiver tail at the splice instead.
fn doc_expr_for_postfix_receiver(receiver: &Expr) -> Doc {
    doc_expr_grouped_boundary(receiver, operator_expr_needs_grouping(receiver), true, true)
}

/// Render an operator-chain placeholder back to its source-level
/// form. The chain is self-describing — the `Normal` variant
/// carries the matched op pattern (slots interleaved with
/// op-tokens) and the parsed operand expressions; the `Variadic`
/// variant carries the OPEN / CLOSE delimiters and element expressions. The
/// pretty-printer never consults the operator scope.
fn doc_op_chain(kind: &crate::ast::OpChainKind) -> Doc {
    match kind {
        crate::ast::OpChainKind::Normal { pattern, slots, .. } => {
            doc_normal_op_chain(pattern, slots)
        }
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
    let mut boundary_cache = OperatorBoundaryCache::default();
    append_normal_op_segments(pattern, slots, &mut segments, &mut boundary_cache);
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

fn append_normal_op_segments<'a>(
    pattern: &'a [OpPart],
    slots: &'a [Expr],
    segments: &mut Vec<Doc>,
    boundary_cache: &mut OperatorBoundaryCache<'a>,
) {
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
                let followed_by_token =
                    matches!(pattern.get(part_idx + 1), Some(OpPart::Token { .. }));
                let needs_parens = operator_slot_grouping(slot, part_idx, pattern, boundary_cache);
                let same_pattern_child = match (part, slot) {
                    (
                        OpPart::SlotRecursive { .. } | OpPart::SlotGreedy { .. },
                        Expr::OpChain {
                            kind:
                                crate::ast::OpChainKind::Normal {
                                    pattern: child_pattern,
                                    slots: child_slots,
                                    ..
                                },
                            ..
                        },
                    ) if op_patterns_equal(pattern, child_pattern)
                        && !needs_parens
                        && !expr_boundary_has_comments(slot, true, !followed_by_token) =>
                    {
                        Some((child_pattern.as_slice(), child_slots.as_slice()))
                    }
                    _ => None,
                };

                if let Some((child_pattern, child_slots)) = same_pattern_child {
                    let tokens = std::mem::take(&mut pending_tokens);
                    if tokens.is_empty() {
                        append_normal_op_segments(
                            child_pattern,
                            child_slots,
                            segments,
                            boundary_cache,
                        );
                    } else if slot_idx == 0 {
                        // A recursive prefix application contributes one leading
                        // operator segment before the child application; joining
                        // the run to the child's first segment would hide that
                        // application boundary in the broken layout.
                        segments.push(doc_op_segment(tokens, None));
                        append_normal_op_segments(
                            child_pattern,
                            child_slots,
                            segments,
                            boundary_cache,
                        );
                    } else {
                        let child_start = segments.len();
                        append_normal_op_segments(
                            child_pattern,
                            child_slots,
                            segments,
                            boundary_cache,
                        );
                        if let Some(child_first) = segments.get(child_start).cloned() {
                            segments[child_start] = doc_op_segment(tokens, Some(child_first));
                        } else {
                            segments.push(doc_op_segment(tokens, None));
                        }
                    }
                } else {
                    let slot_doc = doc_op_chain_slot(slot, needs_parens, !followed_by_token);
                    segments.push(doc_op_segment(
                        std::mem::take(&mut pending_tokens),
                        Some(slot_doc),
                    ));
                }
                let trailing = slot.meta().trailing_trivia.as_slice();
                if followed_by_token && has_comments(trailing) {
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
    let marked = open_tokens != ["["];
    let item_docs = elements
        .iter()
        .enumerate()
        .map(|(index, element)| {
            let at_first_edge = index == 0;
            let at_last_edge = index + 1 == elements.len();
            let (starts_word, ends_word) = expr_symbol_edges(element);
            let grouped = (at_first_edge && starts_word) || (marked && at_last_edge && ends_word);
            if grouped {
                doc_expr_grouped_boundary(element, true, false, false)
            } else {
                doc_expr(element)
            }
        })
        .collect();
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
        marked,
    )
}

fn expr_symbol_edges(expr: &Expr) -> (bool, bool) {
    match expr {
        Expr::BlockCall {
            prefix,
            splice: Some(flavor),
            ..
        } if flavor.callee_on_right() => {
            let (_, Some((receiver, _))) = block_splice_parts(prefix, Some(*flavor)) else {
                unreachable!()
            };
            (
                !operator_expr_needs_grouping(receiver) && expr_symbol_edges(receiver).0,
                false,
            )
        }
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, slots, .. },
            ..
        } => {
            let starts = matches!(pattern.first(), Some(OpPart::Token { .. }))
                || slots.first().is_some_and(|slot| expr_symbol_edges(slot).0);
            let ends = matches!(pattern.last(), Some(OpPart::Token { .. }))
                || slots.last().is_some_and(|slot| expr_symbol_edges(slot).1);
            (starts, ends)
        }
        Expr::Call { callee, .. } if !call_callee_needs_parens(callee) => {
            (expr_symbol_edges(callee).0, false)
        }
        Expr::Ufcs { receiver, .. } if !operator_expr_needs_grouping(receiver) => {
            (expr_symbol_edges(receiver).0, false)
        }
        _ => (false, false),
    }
}

fn operator_expr_needs_grouping(expr: &Expr) -> bool {
    matches!(expr,
        Expr::OpChain { kind: crate::ast::OpChainKind::Normal { pattern, .. }, .. }
            if !matches!(pattern.last(), Some(OpPart::Token { .. })))
}

/// Render an op-chain slot. Wrap in load-bearing parens when the slot
/// is itself an op-chain that the surrounding slot position wouldn't
/// otherwise admit, per `specs/language.md` § Operators:
///
/// - `SlotPlain` (`_`): atoms, including fixed-word-ending operators.
/// - `SlotRecursive` (`__`): drives same-op chain continuation on
///   re-parse. A non-atomic child of another pattern needs parentheses.
/// - `SlotGreedy` (`___`): admits an operator chain of any pattern.
///
/// Slot admission does not erase grouping needed to protect exposed chain
/// claims or a containing frame's pending operator word.
fn doc_op_chain_slot(slot: &Expr, needs_parens: bool, trailing: bool) -> Doc {
    doc_expr_grouped_boundary(slot, needs_parens, true, trailing)
}

#[derive(Default)]
struct OperatorBoundaryCache<'a> {
    slots: std::collections::HashMap<(usize, usize, usize), bool>,
    edges: std::collections::HashMap<(usize, u8, OperatorBoundaryTarget<'a>), bool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum OperatorBoundaryTarget<'a> {
    GreedyOnly,
    Pattern(OperatorPatternShape<'a>),
    Word(&'a str),
}

#[derive(Clone, Copy)]
struct OperatorPatternShape<'a>(&'a [OpPart]);

impl PartialEq for OperatorPatternShape<'_> {
    fn eq(&self, other: &Self) -> bool {
        op_patterns_equal(self.0, other.0)
    }
}

impl Eq for OperatorPatternShape<'_> {}

impl std::hash::Hash for OperatorPatternShape<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for part in self.0 {
            part.delayed().hash(state);
            match part {
                OpPart::SlotPlain { .. } => 0u8.hash(state),
                OpPart::SlotRecursive { .. } => 1u8.hash(state),
                OpPart::SlotGreedy { .. } => 2u8.hash(state),
                OpPart::Token { content, .. } => {
                    3u8.hash(state);
                    content.hash(state);
                }
            }
        }
    }
}

fn operator_slot_grouping<'a>(
    slot: &'a Expr,
    slot_index: usize,
    parent_pattern: &'a [OpPart],
    cache: &mut OperatorBoundaryCache<'a>,
) -> bool {
    let key = (
        slot as *const Expr as usize,
        parent_pattern.as_ptr() as usize,
        slot_index,
    );
    if let Some(result) = cache.slots.get(&key) {
        return *result;
    }
    let result = operator_slot_grouping_inner(slot, slot_index, parent_pattern, cache);
    cache.slots.insert(key, result);
    result
}

fn operator_slot_grouping_inner<'a>(
    slot: &'a Expr,
    slot_index: usize,
    parent_pattern: &'a [OpPart],
    cache: &mut OperatorBoundaryCache<'a>,
) -> bool {
    use crate::ast::OpPart;
    let slot_part = &parent_pattern[slot_index];
    if (slot_index == 0
        && exposes_operator_boundary(slot, OperatorBoundary::RightClaim(parent_pattern), cache))
        || (slot_index + 1 == parent_pattern.len()
            && exposes_operator_boundary(slot, OperatorBoundary::LeftClaim(parent_pattern), cache))
    {
        return true;
    }
    if slot_index != 0
        && let Some(OpPart::Token { content, .. }) = parent_pattern.get(slot_index + 1)
        && exposes_operator_boundary(slot, OperatorBoundary::FollowingWord(content), cache)
    {
        return true;
    }
    let inner_pattern = match slot {
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, .. },
            ..
        } => Some(pattern),
        _ => None,
    };
    if !operator_expr_needs_grouping(slot) {
        return false;
    }
    match (slot_part, inner_pattern) {
        (_, None) => false,
        (OpPart::SlotGreedy { .. }, _) => false,
        (OpPart::SlotPlain { .. }, _) => true,
        (OpPart::SlotRecursive { .. }, Some(inner)) => !op_patterns_equal(inner, parent_pattern),
        (OpPart::Token { .. }, _) => false,
    }
}

#[derive(Clone, Copy)]
enum OperatorBoundary<'a> {
    LeftClaim(&'a [OpPart]),
    RightClaim(&'a [OpPart]),
    FollowingWord(&'a str),
}

fn exposes_operator_boundary<'a>(
    expr: &'a Expr,
    boundary: OperatorBoundary<'a>,
    cache: &mut OperatorBoundaryCache<'a>,
) -> bool {
    use OperatorBoundaryTarget::{GreedyOnly, Pattern, Word};
    let (kind, target) = match boundary {
        // A same-chain claim requires the target's corresponding slot
        // to be recursive too. All other targets admit only greedy claims,
        // independent of their words. Keep traversal and grouping unchanged.
        OperatorBoundary::LeftClaim(pattern) => (
            0,
            if matches!(pattern.first(), Some(OpPart::SlotRecursive { .. })) {
                Pattern(OperatorPatternShape(pattern))
            } else {
                GreedyOnly
            },
        ),
        OperatorBoundary::RightClaim(pattern) => (
            1,
            if matches!(pattern.last(), Some(OpPart::SlotRecursive { .. })) {
                Pattern(OperatorPatternShape(pattern))
            } else {
                GreedyOnly
            },
        ),
        OperatorBoundary::FollowingWord(word) => (2, Word(word)),
    };
    let key = (expr as *const Expr as usize, kind, target);
    if let Some(result) = cache.edges.get(&key) {
        return *result;
    }
    let result = exposes_operator_boundary_inner(expr, boundary, cache);
    cache.edges.insert(key, result);
    result
}

fn exposes_operator_boundary_inner<'a>(
    expr: &'a Expr,
    boundary: OperatorBoundary<'a>,
    cache: &mut OperatorBoundaryCache<'a>,
) -> bool {
    match expr {
        Expr::BlockCall {
            prefix,
            splice: Some(flavor),
            ..
        } if !matches!(boundary, OperatorBoundary::RightClaim(_)) && flavor.callee_on_right() => {
            let (_, Some((receiver, _))) = block_splice_parts(prefix, Some(*flavor)) else {
                unreachable!()
            };
            !operator_expr_needs_grouping(receiver)
                && !expr_boundary_has_comments(receiver, true, true)
                && exposes_operator_boundary(receiver, boundary, cache)
        }
        Expr::Call { callee, .. }
            if !matches!(boundary, OperatorBoundary::RightClaim(_))
                && !call_callee_needs_parens(callee)
                && !expr_boundary_has_comments(callee, true, true) =>
        {
            exposes_operator_boundary(callee, boundary, cache)
        }
        Expr::Ufcs {
            receiver, flavor, ..
        } if !matches!(boundary, OperatorBoundary::RightClaim(_))
            && flavor.callee_on_right()
            && !operator_expr_needs_grouping(receiver)
            && !expr_boundary_has_comments(receiver, true, false) =>
        {
            exposes_operator_boundary(receiver, boundary, cache)
        }
        Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, slots, .. },
            ..
        } => {
            let is_slot = |part: Option<&OpPart>| {
                part.is_some_and(|part| !matches!(part, OpPart::Token { .. }))
            };
            let claims = |part: Option<&OpPart>, target: &[OpPart]| match part {
                Some(OpPart::SlotGreedy { .. }) => true,
                Some(OpPart::SlotRecursive { .. }) => op_patterns_equal(pattern, target),
                _ => false,
            };
            let direct = match boundary {
                OperatorBoundary::LeftClaim(target) => claims(pattern.first(), target),
                OperatorBoundary::RightClaim(target) => claims(pattern.last(), target),
                OperatorBoundary::FollowingWord(word) => {
                    is_slot(pattern.first())
                        && matches!(pattern.get(1), Some(OpPart::Token { content, .. }) if content == word)
                }
            };
            if direct {
                return true;
            }
            let mut exposed_slot = |slot: &'a Expr, index| {
                !operator_slot_grouping(slot, index, pattern, cache)
                    && !expr_boundary_has_comments(slot, true, index + 1 == pattern.len())
                    && exposes_operator_boundary(slot, boundary, cache)
            };
            (!matches!(boundary, OperatorBoundary::RightClaim(_))
                && is_slot(pattern.first())
                && exposed_slot(&slots[0], 0))
                || (is_slot(pattern.last())
                    && exposed_slot(
                        slots.last().expect("fixed operator has a value slot"),
                        pattern.len() - 1,
                    ))
        }
        _ => false,
    }
}

/// Two op-patterns are equal for round-trip purposes when their
/// shape (token contents, slot kinds in order) matches. The `quoted`
/// flag affects declaration-side round-trip but not
/// use-site shape, so they're not compared.
fn op_patterns_equal(a: &[crate::ast::OpPart], b: &[crate::ast::OpPart]) -> bool {
    use crate::ast::OpPart;
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x.delayed() == y.delayed()
            && match (x, y) {
                (OpPart::Token { content: c1, .. }, OpPart::Token { content: c2, .. }) => c1 == c2,
                (OpPart::SlotPlain { .. }, OpPart::SlotPlain { .. }) => true,
                (OpPart::SlotRecursive { .. }, OpPart::SlotRecursive { .. }) => true,
                (OpPart::SlotGreedy { .. }, OpPart::SlotGreedy { .. }) => true,
                _ => false,
            }
    })
}

/// Build a `Vec<Vec<Trivia>>` parallel to `args` by cloning each
/// arg's `meta.leading_trivia`.
fn call_arg_trivia(args: &[CallArg]) -> Vec<Vec<Trivia>> {
    args.iter()
        .map(|a| a.meta().leading_trivia.clone())
        .collect()
}

fn doc_call_arg(arg: &CallArg) -> Doc {
    match arg {
        // A type passed as a call argument (an intrinsic form's type
        // operand — `__if_then_else__(B, T, …)`, `__left__(T, v)`, …) sits
        // at a bare position: the comma-list provides no delimiter, so a
        // wide `Sum` / `Product` chain that breaks to the leading-operator
        // layout must parenthesize itself or its leading operator would
        // reparse as a stray operator at the start of a type expression.
        // `doc_type_bare_position` adds those parens to the broken layout
        // only — a flat chain stays bare.
        CallArg::Type(t) => doc_type_bare_position(t),
        CallArg::Value(v) => doc_expr_no_own_trivia(v),
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
        callee: &'a Expr,
        args: &'a [CallArg],
        closing: &'a [Trivia],
        flavor: crate::ast::UfcsFlavor,
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
        callee,
        args,
        flavor,
        source_trivia,
        ..
    } = cur
    {
        if !flavor.callee_on_right() {
            break;
        }
        segments.push(Segment {
            callee,
            args,
            closing: delimited_closing(source_trivia),
            flavor: *flavor,
            leading_trivia: receiver.meta().trailing_trivia.as_slice(),
        });
        cur = receiver.as_ref();
    }
    segments.reverse();
    let innermost = cur;
    let seg_doc = |seg: &Segment| -> Doc {
        let callee = doc_expr_for_call_callee(seg.callee);
        let head = concat(text(seg.flavor.token()), callee);
        if seg.args.is_empty() && !has_comments(seg.closing) {
            head
        } else {
            doc_call(head, seg.args, seg.closing)
        }
    };
    let receiver_doc = doc_expr_grouped_boundary(
        innermost,
        operator_expr_needs_grouping(innermost),
        true,
        false,
    );
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
        callee,
        args,
        flavor,
        source_trivia,
        ..
    } = top
    else {
        unreachable!("doc_left_call_splice called on non-UFCS expression")
    };
    let callee = doc_expr_for_call_callee(callee);
    let lhs = if args.is_empty() && !has_comments(delimited_closing(source_trivia)) {
        callee
    } else {
        doc_call(callee, args, delimited_closing(source_trivia))
    };
    concat_all([
        lhs,
        text(flavor.token()),
        doc_expr_for_call_splice_arg(receiver),
    ])
}

fn doc_expr_for_call_splice_arg(e: &Expr) -> Doc {
    let bare = matches!(
        e,
        Expr::Path { .. }
            | Expr::Call { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::Tuple { .. }
    );
    doc_expr_grouped_boundary(e, !bare || call_splice_arg_starts_with_dot(e), true, true)
}

// A dot-led call head would fuse with the preceding `.<` or `.<<` token.
fn call_splice_arg_starts_with_dot(expr: &Expr) -> bool {
    match expr {
        Expr::FnExpr { .. } | Expr::FnPlaceholder { .. } | Expr::BoolLit { .. } => true,
        Expr::Call { callee, .. } => {
            !call_callee_needs_parens(callee)
                && !has_comments(&callee.meta().leading_trivia)
                && !has_comments(&callee.meta().trailing_trivia)
                && call_splice_arg_starts_with_dot(callee)
        }
        _ => false,
    }
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
        Some(pat) if pat.numbered.is_some() => {
            text(format!(".{}.", pat.numbered.as_ref().unwrap().stem.name))
        }
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
            Some(ty) => doc_annotated_let_binder(name, ty),
            None => text(name.to_owned()),
        },
    }
}

fn doc_annotated_let_binder(name: &str, ty: &Type) -> Doc {
    doc_comma_list_with_trivia(
        text(".("),
        vec![concat(text(format!("{name}: ")), doc_type_core(ty))],
        &[ty.meta().leading_trivia.clone()],
        &ty.meta().trailing_trivia,
        text(")"),
    )
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
    trailing_trivia: &[Trivia],
) -> Doc {
    if let Some(numbered) = pattern.and_then(|pat| pat.numbered.as_ref()) {
        return doc_let_with(
            text(format!("let .{}. =", numbered.stem.name)),
            value,
            body,
            leading_trivia,
            &doc_expr_statement_value,
            &|body| doc_expr_with_outer_trivia(body, trailing_trivia),
        );
    }
    if let Some(pat) = pattern {
        return doc_let_pattern(
            pat,
            if name.starts_with("__pat_let_") {
                None
            } else {
                Some(name)
            },
            value,
            body,
            leading_trivia,
            trailing_trivia,
        );
    }
    let binder = match ty {
        Some(ty) => concat_all([text("let "), doc_annotated_let_binder(name, ty), text(" =")]),
        None => text(format!("let {name} =")),
    };
    doc_let_with(
        binder,
        value,
        body,
        leading_trivia,
        &doc_expr_statement_value,
        &|body| doc_expr_with_outer_trivia(body, trailing_trivia),
    )
}

fn doc_let_pattern(
    pattern: &crate::ast::ParamPattern,
    as_pattern_name: Option<&str>,
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
    trailing_trivia: &[Trivia],
) -> Doc {
    let pattern_doc = doc_param_pattern_tuple(pattern);
    let binder_doc = match as_pattern_name {
        None => concat(text("."), pattern_doc),
        Some(name) => concat_all([text(format!(".({name}: ")), pattern_doc, text(")")]),
    };
    let value_leading_trivia = value.meta().leading_trivia.as_slice();
    let value_expr = doc_expr_statement_value(value);
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
        doc_expr_with_outer_trivia(body, trailing_trivia),
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
fn doc_seq(
    value: &Expr,
    body: &Expr,
    leading_trivia: &[Trivia],
    trailing_trivia: &[Trivia],
) -> Doc {
    doc_seq_with(
        value,
        body,
        leading_trivia,
        &doc_expr_statement_value,
        &|body| doc_expr_with_outer_trivia(body, trailing_trivia),
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
/// already emitted. For `Let` / `Seq` this re-enters the
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
            ..
        } => doc_let(name, ty.as_ref(), pattern.as_ref(), value, body, &[], &[]),
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta: _,
        } => doc_seq(value, body, &[], &[]),
        _ => doc_expr(e),
    }
}

/// Consume this expression's boundary trivia. Statement continuations carry
/// outer closing comments down to the final expression so nested tails precede
/// the enclosing closer, without emitting either run twice.
fn doc_expr_with_own_meta_trivia(e: &Expr) -> Doc {
    doc_expr_with_outer_trivia(e, &[])
}

fn doc_expr_with_outer_trivia(e: &Expr, outer: &[Trivia]) -> Doc {
    let leading = e.meta().leading_trivia.as_slice();
    let mut trailing = e.meta().trailing_trivia.clone();
    append_relocated_trivia(&mut trailing, outer.to_vec());
    match e {
        Expr::Let {
            name,
            ty,
            pattern,
            value,
            body,
            ..
        } => doc_let(
            name,
            ty.as_ref(),
            pattern.as_ref(),
            value,
            body,
            leading,
            &trailing,
        ),
        Expr::Seq { value, body, .. } => doc_seq(value, body, leading, &trailing),
        _ => concat_all([
            doc_inline_leading_trivia(leading),
            doc_expr_no_own_trivia(e),
            if has_comments(&trailing) {
                concat(hardline(), doc_trailing_comments(&trailing))
            } else {
                empty()
            },
        ]),
    }
}

/// Render argument boundaries separately from the call's own closer.
fn doc_call(callee: Doc, args: &[CallArg], closing: &[Trivia]) -> Doc {
    let type_count = args
        .iter()
        .take_while(|arg| matches!(arg, CallArg::Type(_)))
        .count();
    let mut parts = vec![callee];
    for arg in &args[..type_count] {
        let CallArg::Type(ty) = arg else {
            unreachable!()
        };
        parts.push(doc_type_argument(ty));
    }
    if type_count < args.len() || args.is_empty() {
        let values = &args[type_count..];
        assert!(
            values.iter().all(|arg| matches!(arg, CallArg::Value(_))),
            "a call stage cannot interleave type and value arguments"
        );
        let docs = values.iter().map(doc_call_arg).collect();
        let leading = call_arg_trivia(values);
        let trailing = values
            .iter()
            .map(|arg| arg.meta().trailing_trivia.clone())
            .collect::<Vec<_>>();
        parts.push(doc_comma_list_with_boundaries(
            text("("),
            docs,
            &leading,
            &trailing,
            closing,
            text(")"),
            false,
        ));
    }
    concat_all(parts)
}

/// One type per bracket, with comments owned by that exact type argument.
fn doc_type_argument(ty: &Type) -> Doc {
    let leading = &ty.meta().leading_trivia;
    let trailing = &ty.meta().trailing_trivia;
    if !has_comments(leading) && !has_comments(trailing) {
        return concat_all([text("["), doc_type_core(ty), text("]")]);
    }
    let mut inner = vec![
        hardline(),
        doc_inline_leading_trivia(leading),
        doc_type_core(ty),
    ];
    if has_comments(trailing) {
        inner.extend([hardline(), doc_trailing_comments(trailing)]);
    }
    concat_all([text("["), nest(2, concat_all(inner)), hardline(), text("]")])
}

fn delimited_closing(trivia: &Option<Box<DelimitedTrivia>>) -> &[Trivia] {
    trivia
        .as_ref()
        .map_or(&[], |trivia| trivia.closing.as_slice())
}

/// Pretty-print a comma-separated list with per-item leading
/// trivia and the last item's closing-boundary trailing trivia.
/// When any item carries a leading `LineComment` — or the last item
/// a trailing / dangling comment before the closer — force the
/// multi-line A1 broken layout (open on the head's line, items at
/// its base column each preceded by their trivia comments and a leading-
/// comma `, `, a multi-line item's continuation anchored at the
/// item's content column, the last item's trailing comment on its
/// own line, closer at the same base column). Otherwise fall back to
/// the standard width-driven [`comma_list`].
///
/// `item_trivia` must be parallel to `items`; the parser captures
/// real trivia for surface calls and tuples; synthesized sites use empty
/// vectors. `last_trailing`
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
    doc_comma_list_with_boundaries(open, items, item_trivia, &[], last_trailing, close, padded)
}

fn doc_comma_list_with_boundaries(
    open: Doc,
    items: Vec<Doc>,
    item_trivia: &[Vec<Trivia>],
    item_tails: &[Vec<Trivia>],
    closing: &[Trivia],
    close: Doc,
    padded: bool,
) -> Doc {
    debug_assert_eq!(
        items.len(),
        item_trivia.len(),
        "item_trivia must be parallel to items"
    );
    let any_leading = item_trivia.iter().any(|t| has_comments(t));
    debug_assert!(item_tails.is_empty() || item_tails.len() == items.len());
    let trailing_comment =
        has_comments(closing) || item_tails.iter().any(|tail| has_comments(tail));
    if !any_leading && !trailing_comment {
        return crate::doc::comma_list_with_padding(open, items, close, padded);
    }
    let mut annotated_items = Vec::new();
    let mut pending = Vec::new();
    for (i, (item, trivia)) in items.iter().zip(item_trivia).enumerate() {
        append_relocated_trivia(&mut pending, trivia.clone());
        annotated_items.push((doc_inline_leading_trivia(&pending), item.clone()));
        pending = item_tails.get(i).cloned().unwrap_or_default();
    }
    append_relocated_trivia(&mut pending, closing.to_vec());
    broken_comma_list(
        open,
        annotated_items,
        doc_trailing_comments(&pending),
        close,
    )
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
/// example `let` bindings inside expression bodies and `match` clauses inside
/// a multi-clause `match`. Each
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
    doc_comment_run(trivia, CommentRunContext::InlineLeading)
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
    doc_comment_run(trivia, CommentRunContext::Trailing)
}

enum CommentRunContext {
    ItemLeading,
    InlineLeading,
    Trailing,
    ImportLeading,
}

fn doc_comment_run(trivia: &[Trivia], context: CommentRunContext) -> Doc {
    let (include_docs, keep_paragraphs, final_hardline) = match context {
        CommentRunContext::ItemLeading => (true, true, true),
        CommentRunContext::InlineLeading => (false, true, true),
        CommentRunContext::Trailing => (false, true, false),
        CommentRunContext::ImportLeading => (false, true, true),
    };
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_empty_comment = false;
    let mut newline_run: usize = 0;
    let mut emitted_first = false;
    for entry in trivia {
        let (line, is_empty_comment) = match entry {
            Trivia::Newline => {
                newline_run += 1;
                continue;
            }
            Trivia::LineComment { text: body, .. } => {
                let is_empty = body.is_empty();
                if is_empty && last_was_empty_comment {
                    newline_run = 0;
                    continue;
                }
                (format!("//{body}"), is_empty)
            }
            Trivia::DocCommentLine { text: body, .. } if include_docs => (
                if body.is_empty() {
                    "///".to_owned()
                } else {
                    format!("/// {body}")
                },
                false,
            ),
            Trivia::DocCommentLine { .. } => continue,
        };
        if emitted_first {
            parts.push(hardline());
            if keep_paragraphs && newline_run >= 2 {
                parts.push(hardline());
            }
        }
        parts.push(text(line));
        last_was_empty_comment = is_empty_comment;
        emitted_first = true;
        newline_run = 0;
    }
    if final_hardline && emitted_first {
        parts.push(hardline());
    }
    concat_all(parts)
}

/// Target chunk width for [`doc_string_literal`]'s reflow. Per
/// `specs/style.md` § "String literal reflow": values longer than
/// 72 characters split into adjacent string literals, each chunk
/// ≤ 72 characters of value (not line position).
const STRING_CHUNK_TARGET: usize = 72;

/// A literal's optional explicit carrier type.
fn doc_literal_annotation(annotation: &Option<Type>) -> Doc {
    annotation.as_ref().map_or_else(empty, doc_type_argument)
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

fn doc_package_file(e: &PackageFile, materialize_baselines: bool) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    parts.push(doc_item_leading_trivia(e.meta.leading_trivia.as_slice()));
    parts.push(text(format!("pkg {};", e.name)));
    parts.push(hardline());
    if let Some(build) = &e.build {
        parts.push(hardline());
        parts.push(doc_build_block(build, materialize_baselines));
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
        text(format!("dep {};", dep.name)),
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
            let mut fields: Vec<ConfigEntry<'_>> = vec![
                (
                    "git",
                    crate::ast::BlockFieldValue::Str(source.url.clone()),
                    &source.url_leading_trivia,
                ),
                (
                    "ref",
                    crate::ast::BlockFieldValue::Str(source.git_ref.clone()),
                    &source.ref_leading_trivia,
                ),
            ];
            if let Some(selector) = &source.manifest_path {
                fields.push((
                    "path",
                    crate::ast::BlockFieldValue::Str(selector.path.clone()),
                    &selector.leading_trivia,
                ));
            } else if let Some(trivia) = &source.absent_path_leading_trivia {
                fields.push(("path", crate::ast::BlockFieldValue::Unit, trivia));
            }
            join(
                concat(text(";"), hardline()),
                fields.into_iter().map(doc_config_entry),
            )
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
    let mut path_fields = lock
        .manifest_path
        .as_ref()
        .map(|path| {
            vec![(
                "path",
                crate::ast::BlockFieldValue::Str(path.clone()),
                &[][..],
            )]
        })
        .unwrap_or_default();
    if path_fields.is_empty() && lock.field_leading_trivia.contains_key("path") {
        path_fields.push(("path", crate::ast::BlockFieldValue::Unit, &[]));
    }
    fields.extend(
        path_fields
            .into_iter()
            .map(|field| concat(leading("path"), doc_config_entry(field))),
    );
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
    let head = if has_comments(&h.meta.trailing_trivia) {
        concat(
            text("host type"),
            nest(
                2,
                concat_all([
                    hardline(),
                    doc_inline_leading_trivia(&h.meta.trailing_trivia),
                    text(h.name.clone()),
                ]),
            ),
        )
    } else {
        text(format!("host type {}", h.name))
    };
    let params = if h.type_params.is_empty() {
        empty()
    } else {
        doc_type_params(&h.type_params)
    };
    let role = match h.role {
        Some(role) => text(format!(" role({})", role.role.as_str())),
        None => empty(),
    };
    concat_all([doc_prefix, head, params, role])
}

fn doc_host_fn(h: &HostFn) -> Doc {
    doc_bodyless_fn(h, "host fn ")
}

fn doc_bodyless_fn(h: &HostFn, prefix: &str) -> Doc {
    let doc_prefix = h.doc.as_ref().map(doc_doc_comment).unwrap_or_else(empty);
    let head = text(format!("{prefix}{}", h.name));
    let params = doc_host_fn_signature(h);
    let ret = if can_omit_unit_return(&h.ret) {
        empty()
    } else {
        doc_function_return(&h.ret, false)
    };
    let trailing = if has_comments(&h.meta.trailing_trivia) {
        concat_all([
            nest(
                2,
                concat(hardline(), doc_trailing_comments(&h.meta.trailing_trivia)),
            ),
            hardline(),
        ])
    } else {
        empty()
    };
    concat_all([doc_prefix, head, params, ret, trailing])
}

fn doc_host_fn_signature(h: &HostFn) -> Doc {
    let groups = crate::ast::host_fn_group_refs(&h.params, &h.param_groups);
    if groups.is_empty() {
        return text("()");
    }
    let mut parts = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let before = signature_boundary_trivia(
            &h.source_trivia,
            index,
            SignatureTriviaPosition::BeforeGroup,
        );
        let opening =
            signature_boundary_trivia(&h.source_trivia, index, SignatureTriviaPosition::InsideOpen);
        let closing = signature_boundary_trivia(
            &h.source_trivia,
            index,
            SignatureTriviaPosition::InsideClose,
        );
        let group_doc = match group {
            crate::ast::HostFnGroupRef::Type(params) => concat_all(params.iter().map(|param| {
                let HostFnParam::Type(tp) = param else {
                    unreachable!("host fn type group contains only type params")
                };
                doc_signature_type_binder(tp, opening, closing)
            })),
            crate::ast::HostFnGroupRef::Value(params) => {
                let docs: Vec<Doc> = params.iter().map(doc_host_fn_def_param).collect();
                let leading: Vec<_> = params
                    .iter()
                    .map(|param| match param {
                        HostFnParam::Value(value) => value.meta.leading_trivia.clone(),
                        HostFnParam::Type(_) => {
                            unreachable!("host fn value group contains only value params")
                        }
                    })
                    .collect();
                let trailing: Vec<_> = params
                    .iter()
                    .map(|param| match param {
                        HostFnParam::Value(value) => value.meta.trailing_trivia.clone(),
                        HostFnParam::Type(_) => {
                            unreachable!("host fn value group contains only value params")
                        }
                    })
                    .collect();
                doc_comma_list_with_boundaries(
                    text("("),
                    docs,
                    &leading,
                    &trailing,
                    closing,
                    text(")"),
                    false,
                )
            }
        };
        parts.push(doc_signature_group(group_doc, before));
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
/// body (optional `cache`, optional `docs`, then each `target`
/// block) at +2 indent, closing `}` back at the outer column.
fn doc_build_block(b: &BuildBlock, materialize_baselines: bool) -> Doc {
    let mut body: Vec<Doc> = Vec::new();
    if let Some(cache) = &b.cache {
        body.push(doc_cache_decl(cache));
    }
    if let Some(docs) = &b.docs {
        body.push(doc_docs_block(docs));
    }
    for t in &b.targets {
        body.push(doc_target_block(t, materialize_baselines));
    }
    doc_braced_block(
        concat(doc_item_leading_trivia(&b.leading_trivia), text("build")),
        append_block_trailing(
            (!body.is_empty()).then(|| join(concat_all([text(";"), hardline(), hardline()]), body)),
            &b.trailing_trivia,
        ),
        false,
    )
}

/// Canonicalized layout for the `docs` block: `md` first, then
/// repeatable `support` entries, then any written `md_out` and `html`
/// keys in that fixed order, so authors who write the keys in a
/// different order don't see spurious diffs.
fn doc_docs_block(d: &crate::ast::BuildBlockDocs) -> Doc {
    use crate::ast::BlockFieldValue;
    let mut fields: Vec<ConfigEntry<'_>> = vec![(
        "md",
        BlockFieldValue::Str(d.md.clone()),
        &d.md_leading_trivia,
    )];
    for (i, support) in d.support.iter().enumerate() {
        fields.push((
            "support",
            BlockFieldValue::Str(support.clone()),
            d.support_leading_trivia
                .get(i)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ));
    }
    if let Some(md_out) = &d.md_out {
        fields.push(("md_out", md_out.clone(), &d.md_out_leading_trivia));
    }
    if let Some(html) = &d.html {
        fields.push(("html", html.clone(), &d.html_leading_trivia));
    }
    let catalog = crate::config_fields::DOCS_FIELDS;
    fields.sort_by_key(|(name, _, _)| catalog.iter().position(|field| field.name == *name));
    let entries: Vec<Doc> = fields.into_iter().map(doc_config_entry).collect();
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

fn doc_target_block(t: &TargetBlock, materialize_baselines: bool) -> Doc {
    let mut fields: Vec<ConfigEntry<'_>> = t
        .entries
        .iter()
        .map(|entry| {
            (
                entry.key.as_str(),
                entry.value.clone(),
                entry.leading_trivia.as_slice(),
            )
        })
        .collect();
    if materialize_baselines
        && let Some(baseline) = crate::build_target::BuildTarget::from_id(&t.backend)
            .and_then(|backend| backend.latest_baseline())
    {
        if let Some((_, value, _)) = fields.iter_mut().find(|(key, _, _)| *key == "baseline") {
            if matches!(value, crate::ast::BlockFieldValue::Unit) {
                *value = crate::ast::BlockFieldValue::Word(baseline.to_owned());
            }
        } else {
            fields.push((
                "baseline",
                crate::ast::BlockFieldValue::Word(baseline.to_owned()),
                &[],
            ));
        }
    }
    fields.sort_by(|a, b| (a.0 != "out", a.0).cmp(&(b.0 != "out", b.0)));
    let entries: Vec<Doc> = fields.into_iter().map(doc_config_entry).collect();
    doc_braced_block(
        concat(
            doc_inline_leading_trivia(&t.leading_trivia),
            match &t.name {
                Some(name) => concat_all([
                    text(format!("target ({}, ", t.backend)),
                    doc_string_literal(name),
                    text(")"),
                ]),
                None => text(format!("target {}", t.backend)),
            },
        ),
        append_block_trailing(
            (!entries.is_empty()).then(|| join(concat(text(";"), hardline()), entries)),
            &t.trailing_trivia,
        ),
        false,
    )
}

type ConfigEntry<'a> = (&'a str, crate::ast::BlockFieldValue, &'a [Trivia]);

fn doc_config_entry((name, value, trivia): ConfigEntry<'_>) -> Doc {
    let leading = doc_inline_leading_trivia(trivia);
    match value {
        crate::ast::BlockFieldValue::Str(value) => doc_string_field_entry(leading, name, &value),
        crate::ast::BlockFieldValue::Unit => concat(leading, text(format!("{name} ()"))),
        crate::ast::BlockFieldValue::Word(value) => {
            concat(leading, text(format!("{name} {value}")))
        }
        _ => unreachable!("configuration fields are validated before formatting"),
    }
}

// =========================================================================
// Tests — canonical output, preserved structure and comments, and fixpoints.
//
// Property: parse → pretty → parse → pretty produces the same string
// as parse → pretty (i.e., pretty is idempotent under parse). Exact output
// and structure/comment assertions separately pin the canonical style and
// conservation rules; a stable output alone establishes only a fixpoint.
// =========================================================================

#[cfg(test)]
mod tests {

    #[test]
    fn import_sorting_compares_fixed_patterns_across_kinds() {
        let source = "mod m; import syntax { op_macro _ + _; op _ * _; type Zebra; lit alpha; fn beta; macro gamma }";
        let rendered = roundtrip(source);
        let alpha = rendered.find("lit alpha").unwrap();
        let beta = rendered.find("fn beta").unwrap();
        let zebra = rendered.find("type Zebra").unwrap();
        let gamma = rendered.find("macro gamma").unwrap();
        let multiply = rendered.find("op _ * _").unwrap();
        let add = rendered.find("op_macro _ + _").unwrap();
        assert!(
            zebra < alpha && alpha < beta && beta < gamma && gamma < multiply && multiply < add,
            "{rendered}"
        );
    }

    #[test]
    fn semicolon_owners_preserve_outer_terminators_and_nested_separators() {
        let source = "mod m;
            fn id(x: .) -> . { x };
            type { Box : . { ; mk; get; }; };
            op _ + _ { ; impl add; };
            op_var [% %] { ; foldl step initial; finalize finish; };
            macro identity : . -> . { ; captures id; impl identity_impl; };
            host type Text role(str);
            type Alias = .;
            type Choice = { Left: . } | { Right: . };
            rec(loop) { ; fn first(x: .) -> . { x }; fn second(x: .) -> . { x }; };
            rec { ; type Left = Right; type { Right : Left { mk; get }; }; };";
        let formatted = roundtrip(source);
        for expected in [
            "fn id(x: .) { x }\n",
            "type { Box: . { mk; get } }\n",
            "op _ + _ { impl add }\n",
            "op_var [% %] { foldl step initial; finalize finish }\n",
            "macro identity : . -> . { captures id; impl identity_impl }\n",
            "host type Text role(str);",
            "type Alias = .;",
            "type Choice = { Left: . } | { Right: . };",
            "fn first(x: .) { x };\n",
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
        assert!(!formatted.contains("namespace \"pkg\";"), "{formatted}");
    }

    #[test]
    fn removed_outer_semicolon_comments_stay_outside_declaration_bodies() {
        let formatted = roundtrip(
            "mod m;
            type { Box : . { mk; get; }; }
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
            formatted.find("fn run() { () }").unwrap()
                < formatted.find("// before final token").unwrap()
        );
    }

    #[test]
    fn semicolon_dependency_and_lock_fields_keep_boundary_comments() {
        let dep = dependency_roundtrip(
            "dep dep;
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
        let source = "mod m; fn let(x: .) -> . { x } fn left(x: ., y: .) -> . { x } op _ <- _ { impl left; }; fn run(f: ., x: .) -> . { (let(f) <- x); f }";
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
        ] {
            let source = format!("mod m; fn run() {{ let {binder} = value; () }}");
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
                    "mod m; fn run() {{ do(m) {{ let {binder} {connective} value; result }} }}"
                ));
            }
        }
        roundtrip("mod m; fn run() { let .(<U> x) = value; x }");
    }

    #[test]
    fn removed_let_forms_are_not_binding_aliases() {
        for binder in ["x: Int", "(x, y)", "whole: (x: Int, y: Int)", "<U> x"] {
            let source = format!("mod m; fn run() {{ let {binder} = value; () }}");
            assert!(parse(&source).is_err(), "{source}");
        }
        for binder in ["x", "(x, y)"] {
            let source = format!("mod m; fn run() {{ .{binder} = value; () }}");
            assert!(parse(&source).is_err(), "{source}");
        }
    }

    #[test]
    fn import_grammar_import_a1_comments() {
        let source = "mod c; // provider comment\nimport z { // list comment\n ; fn zebra; // item comment\n fn alpha; type Field; op _ + _} import a as alias;";
        let module = crate::pass::parser::parse(source).expect("consumer grammar");
        let formatted = pretty_module(&module);
        assert!(
            formatted.find("import a as alias;").unwrap() < formatted.find("import z {").unwrap()
        );
        for comment in ["provider comment", "list comment", "item comment"] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        assert!(!formatted.contains("z ("));
        assert_eq!(
            pretty_module(&crate::pass::parser::parse(&formatted).unwrap()),
            formatted
        );
        let long = crate::pass::parser::parse("mod c; import long_provider_name {fn first_long_selected_name; fn second_long_selected_name; fn third_long_selected_name; op_var [% %]}").unwrap();
        let rendered = pretty_module(&long);
        assert!(
            rendered.contains("import long_provider_name {\n  "),
            "{rendered}"
        );
        assert!(rendered.contains("\n}"));
        assert_eq!(
            pretty_module(&crate::pass::parser::parse(&rendered).unwrap()),
            rendered
        );
    }

    #[test]
    fn selective_alias_formatting_keeps_explicit_identity_and_comments() {
        let source = "mod c; import p {fn foo // source\n as // separator\n foo; type Bar as Baz; op _ + __ as _ ++ __; op_var [* *] as [% %]}";
        let parsed = crate::pass::parser::parse(source).expect("alias source");
        let formatted = pretty_module(&parsed);
        assert!(formatted.contains("foo as foo"), "{formatted}");
        assert!(formatted.contains("Bar as Baz"), "{formatted}");
        assert!(formatted.contains("op _ + __ as _ ++ __"), "{formatted}");
        assert!(formatted.contains("op_var [* *] as [% %]"), "{formatted}");
        for comment in ["// source", "// separator"] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        let reparsed = crate::pass::parser::parse(&formatted).expect("formatted aliases");
        assert_eq!(pretty_module(&reparsed), formatted);
        let crate::ast::ImportKind::Selective { items, .. } = &reparsed.imports[0].kind else {
            panic!("selective")
        };
        assert!(items.iter().any(|item| matches!(item,
            ImportItem::Name { name, alias: Some(alias), .. } if name == "foo" && alias.local == "foo")));
    }

    #[test]
    fn import_grammar_quotation_and_run_boundaries() {
        for signature in [
            "op _ (=) _",
            "op _ &&++ _",
            "op_var [<** **<]",
            "op _ ? ___ : _",
        ] {
            let source = format!("mod c; import p {{{signature}}}");
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
        let source = r#"mod c;
// statement
import z { // opening selection
  ; // zebra comment
    fn zebra
  ; // alpha comment
    fn alpha
  ; // type comment
    type Field
  ; op _ // operator comment
      + _
  // list closer
  }
import a as a;
"#;
        let module = parse(source).unwrap();
        let formatted = pretty_module(&module);
        assert!(formatted.find("import a as a;").unwrap() < formatted.find("import z {").unwrap());
        for (comment, item) in [
            ("// type comment", "type Field"),
            ("// alpha comment", "fn alpha"),
            ("// zebra comment", "fn zebra"),
            ("// operator comment", "op _ + _"),
        ] {
            assert!(
                formatted.find(comment).unwrap() < formatted.find(item).unwrap(),
                "{formatted}"
            );
        }
        assert!(formatted.contains("// list closer"), "{formatted}");
        for comment in [
            "statement",
            "opening selection",
            "alpha comment",
            "zebra comment",
            "operator comment",
            "type comment",
            "list closer",
        ] {
            assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
        }
        assert_eq!(pretty_module(&parse(&formatted).unwrap()), formatted);
    }

    #[test]
    fn selective_import_repeated_comma_comments_keep_the_next_item() {
        let source = r#"mod c;
import p { // opening selection
  ; // zebra
    fn zebra
  // next slot
  ; // alpha
  ; fn alpha;
  // dangling
  }
"#;
        let formatted = roundtrip(source);
        assert!(formatted.find("// alpha").unwrap() < formatted.find("fn alpha").unwrap());
        assert!(formatted.find("// zebra").unwrap() < formatted.find("fn zebra").unwrap());
        assert!(formatted.contains("// dangling"), "{formatted}");
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
    fn selective_import_width_and_comment_paths_keep_the_base_column() {
        for source in [
            "mod c; import p {fn first_long_selected_name; fn second_long_selected_name; fn third_long_selected_name; fn fourth_long_selected_name}",
            "mod c; import p {; // attached\n fn short}",
        ] {
            let formatted = roundtrip(source);
            assert!(formatted.contains("import p {\n"), "{formatted}");
            assert!(formatted.contains("\n}"), "{formatted}");
        }
        assert!(roundtrip("mod c; import p {fn short}").contains("import p { fn short }"));
    }

    #[test]
    fn variadic_import_delimiter_comments_belong_to_one_selection() {
        for (open, close) in [("[%", "%]"), ("[<**", "**<]")] {
            let source =
                format!("mod c; import p {{op_var {open} // delimiter\n {close}; fn beta}}");
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
                formatted.find("// delimiter").unwrap()
                    < formatted.find(&format!("op_var {open} {close}")).unwrap(),
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
    /// Returns the first pretty-print output for
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
    fn fixed_operator_grouping_survives_native_suffixes_and_pending_words() {
        for (imports, expression, expected) in [
            ("op _ $ ___; op ___ < _ >", "a $ (b < k >)", "a $ (b < k >)"),
            ("op _ $ ___; op ___ < _ >", "(a $ b) < k >", "(a $ b) < k >"),
            (
                "op _ + _; op ___ < _ >",
                "a + ((b < k >)(arg))",
                "a + (b < k >(arg))",
            ),
            (
                "op _ + _; op ___ < _ >",
                "a + ((b < k >).>f)",
                "a + (b < k >.>f)",
            ),
            (
                "op _ + ___; op ___ < _ >; op _ * _",
                "a + ((b < k >) * c)",
                "a + (b < k > * c)",
            ),
            (
                "op _ ? ___ : _; op ! ___; op _ : _",
                "a ? (! (b : c)) : d",
                "a ? (! b : c) : d",
            ),
            (
                "op _ ? ___ ? _",
                "a ? (b ? c ? d) ? e",
                "a ? (b ? c ? d) ? e",
            ),
            ("op _ ?", "a ? ?", "a ? ?"),
            ("op __ + _", "a + b + c", "a + b + c"),
        ] {
            let source = format!("mod m; import ops {{{imports}}} fn run() {{ {expression} }}");
            let formatted = roundtrip(&source);
            assert!(formatted.contains(expected), "{expression}: {formatted}");
            assert_eq!(
                parsed_structure(&source)["items"],
                parsed_structure(&formatted)["items"],
                "{expression}: {formatted}"
            );
        }
    }

    #[test]
    fn operator_boundary_cache_uses_semantic_contexts() {
        let module = parse("mod m; import ops {op _ + __} fn run() { a + b + c }").unwrap();
        let expr = function_body(&module, "run");
        let Expr::OpChain {
            kind: crate::ast::OpChainKind::Normal { pattern, .. },
            ..
        } = expr
        else {
            panic!("operator");
        };
        let cloned_pattern = pattern.clone();
        let first_word = String::from("+");
        let second_word = String::from("+");
        let mut cache = OperatorBoundaryCache::default();
        assert!(exposes_operator_boundary(
            expr,
            OperatorBoundary::RightClaim(pattern),
            &mut cache
        ));
        let entries = cache.edges.len();
        assert!(exposes_operator_boundary(
            expr,
            OperatorBoundary::RightClaim(&cloned_pattern),
            &mut cache
        ));
        assert_eq!(cache.edges.len(), entries);
        assert!(exposes_operator_boundary(
            expr,
            OperatorBoundary::FollowingWord(&first_word),
            &mut cache
        ));
        let entries = cache.edges.len();
        assert!(exposes_operator_boundary(
            expr,
            OperatorBoundary::FollowingWord(&second_word),
            &mut cache
        ));
        assert_eq!(cache.edges.len(), entries);

        for size in [32, 64, 128] {
            for (imports, expression) in [
                ("op ! ___".to_owned(), format!("{}value", "! ".repeat(size))),
                ("op _ + __".to_owned(), vec!["value"; size + 1].join(" + ")),
                (
                    (0..size)
                        .map(|index| format!("op ~{} ___", "!".repeat(index + 1)))
                        .collect::<Vec<_>>()
                        .join("; "),
                    format!(
                        "{} value",
                        (0..size)
                            .map(|index| format!("~{}", "!".repeat(index + 1)))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                ),
                (
                    (0..size)
                        .map(|index| format!("op _ ~{} ___", "!".repeat(index + 1)))
                        .collect::<Vec<_>>()
                        .join("; "),
                    format!(
                        "value{}",
                        (0..size)
                            .map(|index| format!(" ~{} value", "!".repeat(index + 1)))
                            .collect::<String>()
                    ),
                ),
            ] {
                let module = parse(&format!(
                    "mod m; import ops {{{imports}}} fn run() {{ {expression} }}"
                ))
                .unwrap();
                let Expr::OpChain {
                    kind: crate::ast::OpChainKind::Normal { pattern, slots, .. },
                    ..
                } = function_body(&module, "run")
                else {
                    panic!("operator");
                };
                let mut cache = OperatorBoundaryCache::default();
                assert!(!operator_slot_grouping(
                    slots.last().unwrap(),
                    pattern.len() - 1,
                    pattern,
                    &mut cache
                ));
                assert!(
                    cache.edges.len() <= 6 * size,
                    "size={size}, edge queries={}",
                    cache.edges.len()
                );
                assert!(
                    cache.slots.len() <= 2 * size,
                    "size={size}, slot queries={}",
                    cache.slots.len()
                );
            }
        }
    }

    #[test]
    fn executable_blocks_unfold_one_level_at_a_time() {
        for (body, expected) in [
            (
                "if(ready) { run() } else { wait() }",
                "if(ready) { run() } else { wait() }",
            ),
            (
                "if(ready) { do(bind) { run() } } else { wait() }",
                "if(ready) {\n  do(bind) { run() }\n} else {\n  wait()\n}",
            ),
            (
                "choose(ready) { nested { run() } } otherwise { wait() }",
                "choose(ready) {\n  nested { run() }\n} otherwise {\n  wait()\n}",
            ),
            ("do(bind) { run() }", "do(bind) { run() }"),
            ("do(bind) { run(); wait() }", "do(bind) { run(); wait() }"),
            ("scope { let x = run(); x }", "scope { let x = run(); x }"),
        ] {
            let parsed = parse(&format!("mod x; fn f() {{ {body} }}")).unwrap();
            let Item::FnDef(function) = &parsed.items[0] else {
                panic!("function")
            };
            assert_eq!(render(&doc_expr(&function.body), WIDTH), expected);
            let output = roundtrip(&format!("mod x; fn f() {{ {body} }}"));
            assert!(output.contains("fn f() {\n"), "{output}");
        }
    }

    #[test]
    fn trailing_block_chains_share_width_depth_and_comment_choices() {
        for (body, width, expected) in [
            (
                "if(a) { x } else if(b) { y } else { z }",
                100,
                "if(a) { x } else if(b) { y } else { z }",
            ),
            (
                "if(a) { x } else if(b) { y } else { z }",
                30,
                "if(a) {\n  x\n} else if(b) {\n  y\n} else {\n  z\n}",
            ),
            (
                "choose(a) { x } otherwise choose(b) { nested { y } } otherwise { z }",
                100,
                "choose(a) {\n  x\n} otherwise choose(b) {\n  nested { y }\n} otherwise {\n  z\n}",
            ),
            (
                "if(a) { x } else if(b) { // keep\ny } else { z }",
                100,
                "if(a) {\n  x\n} else if(b) {\n  // keep\n  y\n} else {\n  z\n}",
            ),
            (
                "if(a) { x } else { y // tail\n}",
                100,
                "if(a) {\n  x\n} else {\n  y\n  // tail\n}",
            ),
            (
                "choose(a) { nested {} } otherwise {}",
                100,
                "choose(a) {\n  nested {}\n} otherwise {\n}",
            ),
        ] {
            let parsed = parse(&format!("mod x; fn f() {{ {body} }}")).unwrap();
            let Item::FnDef(function) = &parsed.items[0] else {
                panic!("function")
            };
            let actual = render(&doc_expr(&function.body), width);
            assert_eq!(actual, expected);
            roundtrip(&format!("mod x; fn f() {{ {actual} }}"));
        }
    }

    #[test]
    fn calling_a_ufcs_result_preserves_the_call_boundary() {
        for callee in [
            "vault.>Vault.open",
            "vault.>>open",
            "open.<vault",
            "open.<<vault",
        ] {
            for arguments in ["", "()", "value", "value, other"] {
                let invocation = format!("({callee})({arguments})");
                let source = format!("mod x; fn run() {{ {invocation} }}");
                let output = roundtrip(&source);
                assert!(output.contains(&invocation), "{output}");
                let reparsed = parse(&output).unwrap();
                let Item::FnDef(function) = &reparsed.items[0] else {
                    panic!("function")
                };
                let Expr::Call { callee, .. } = &function.body else {
                    panic!("callback invocation became a splice: {output}");
                };
                assert!(matches!(callee.as_ref(), Expr::Ufcs { .. }));
            }
        }
    }

    #[test]
    fn existential_let_preserves_source_spelling_and_owned_comments() {
        let source = "mod main;
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
            "mod main; fn open() -> . {
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
            "mod main; fn explicit() -> . { rhs[_](.[Hidden](payload) { payload }) }",
            "mod main; fn explicit() -> . { (rhs)[_](.[Hidden](payload) { payload }) }",
            "mod main; fn explicit() -> . {
                // authored call
                rhs[_](.[Hidden](payload) { let local = payload; local })
            }",
        ] {
            let printed = roundtrip(source);
            assert!(!printed.contains("let .(<"), "{printed}");
            assert!(printed.contains("rhs[_]("), "{printed}");
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
                "mod main; fn open() -> . {{
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
            "mod main; fn explicit() -> . { rhs[_](.[Hidden]((left: Hidden, right: Hidden)) { left }) }",
            "mod main; fn explicit() -> . { (rhs)[_](.[Hidden]((left: _, right: _)) { left }) }",
        ] {
            let printed = roundtrip(source);
            assert!(!printed.contains("let .(<"), "{printed}");
            assert!(printed.contains("rhs[_]("), "{printed}");
        }
        assert!(
            parse("mod main; fn open() -> . { let .(<Hidden> (left, right)) = rhs; left }")
                .is_err()
        );
    }

    fn parsed_structure(source: &str) -> serde_json::Value {
        source_structure(&parse(source).unwrap())
    }

    fn source_structure(ast: &impl serde::Serialize) -> serde_json::Value {
        fn normalize(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    if fields.len() == 2
                        && fields.contains_key("start")
                        && fields.contains_key("end")
                    {
                        fields.insert("start".to_owned(), 0.into());
                        fields.insert("end".to_owned(), 0.into());
                        return;
                    }
                    fields.retain(|key, _| {
                        !matches!(
                            key.as_str(),
                            "meta"
                                | "span"
                                | "source_trivia"
                                | "source_layout"
                                | "ret_elided"
                                | "trivia"
                                | "separators"
                                | "leading"
                                | "trailing"
                                | "id"
                                | "ext"
                                | "match_id"
                        ) && !key.ends_with("_span")
                            && !key.ends_with("_trivia")
                    });
                    for child in fields.values_mut() {
                        normalize(child);
                    }
                }
                serde_json::Value::Array(items) => {
                    for child in items {
                        normalize(child);
                    }
                }
                _ => {}
            }
        }
        let mut value = serde_json::to_value(ast).unwrap();
        normalize(&mut value);
        value
    }

    #[test]
    fn let_header_comments_survive_ordinary_and_neutral_bindings() {
        let source = include_str!(
            "../../test-data/goldens/00_success/fmt_let_header_comments/workdir/main.kio"
        );
        let expected = include_str!(
            "../../test-data/goldens/00_success/fmt_let_header_comments/expected.stdout"
        );
        assert_eq!(roundtrip(source), expected);
        assert_eq!(parsed_structure(source), parsed_structure(expected));

        for name in ["value", "_"] {
            let source = format!(
                "mod m; fn f() {{ let // binder
{name} // connector
= // value
(); () }}"
            );
            let expected = format!(
                "mod m;

fn f() {{
  // binder
  let {name} =
    // connector

    // value
    ();
  ()
}}
"
            );
            assert_eq!(roundtrip(&source), expected);
            assert_eq!(parsed_structure(&source), parsed_structure(&expected));
            assert_eq!(
                roundtrip(&format!("mod m; fn f() {{ let {name} = (); () }}")),
                format!("mod m;\n\nfn f() {{ let {name} = (); () }}\n")
            );

            for connective in ["=", "<-"] {
                let source = format!(
                    "mod m; fn f() {{ do(bind) {{ let // binder
{name} // connector
{connective} // value
(); () }} }}"
                );
                let expected = format!(
                    "mod m;

fn f() {{
  do(bind) {{
    // binder
    let {name} {connective} // connector

    // value
    ();
    ()
  }}
}}
"
                );
                assert_eq!(roundtrip(&source), expected);
                assert_eq!(parsed_structure(&source), parsed_structure(&expected));
                assert_eq!(
                    roundtrip(&format!(
                        "mod m; fn f() {{ do(bind) {{ let {name} {connective} (); () }} }}"
                    )),
                    format!(
                        "mod m;\n\nfn f() {{\n  do(bind) {{ let {name} {connective} (); () }}\n}}\n"
                    )
                );
            }
        }
    }

    #[test]
    fn let_header_comments_around_dot_patterns_survive() {
        for intro in ["let // binder\n.", "let . // binder\n"] {
            let source = format!("mod m; fn f() {{ {intro}(first, second) = packet; first }}");
            let expected = "mod m;

fn f() {
  // binder
  let .(first, second) = packet;
  first
}
";
            assert_eq!(roundtrip(&source), expected);
            assert_eq!(parsed_structure(&source), parsed_structure(expected));

            for connective in ["=", "<-"] {
                let source = format!(
                    "mod m; fn f() {{ do(bind) {{ {intro}(first, second) {connective} packet; first }} }}"
                );
                let expected = format!(
                    "mod m;

fn f() {{
  do(bind) {{
    // binder
    let .(first, second) {connective} packet;
    first
  }}
}}
"
                );
                assert_eq!(roundtrip(&source), expected);
                assert_eq!(parsed_structure(&source), parsed_structure(&expected));
            }
        }
    }

    #[test]
    fn pattern_opener_and_existential_closer_comments_stay_ordered() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for pattern in [
            "(first, second)",
            "(single)",
            "(single: .)",
            "(<Hidden> packet // closing\n)",
            "(<Hidden> (first: Hidden, second: . // inner closing\n) // closing\n)",
        ] {
            for (prefix, suffix) in [("", ""), ("do(bind) {", "}")] {
                let source = format!(
                    "mod m; fn f() {{ {prefix} let // before dot
. // before opener
{pattern} // equals
= // value
packet; () {suffix} }}"
                );
                let formatted = roundtrip(&source);
                assert_eq!(
                    comments(&source),
                    comments(&formatted),
                    "{source}\n{formatted}"
                );
                assert_eq!(parsed_structure(&source), parsed_structure(&formatted));
            }
        }

        for source in [
            "mod m; fn f() { . // signature opener\n(pair: (x: ., y: .)) { x } }",
            "mod m; fn f() { . // signature opener\n(// pattern opener\n(x: ., y: .)) { x } }",
            "mod m; fn f() { . // type opener\n[A] // value opener\n(value: A) { value } }",
        ] {
            let formatted = roundtrip(source);
            assert_eq!(
                comments(source),
                comments(&formatted),
                "{source}\n{formatted}"
            );
            assert_eq!(parsed_structure(source), parsed_structure(&formatted));
        }
    }

    #[test]
    fn pattern_comments_survive_all_binding_consumers() {
        fn comments(source: &str) -> Vec<String> {
            let (tokens, trailing) = crate::pass::lexer::lex_with_trailing(source).unwrap();
            tokens
                .into_iter()
                .flat_map(|token| token.leading_trivia)
                .chain(trailing)
                .filter_map(|trivia| match trivia {
                    Trivia::LineComment { text, .. } | Trivia::DocCommentLine { text, .. } => {
                        Some(text)
                    }
                    Trivia::Newline => None,
                })
                .collect()
        }

        for pattern in [
            "(// first\nx // colon\n: // annotation\n. // comma\n, \
             // next\nwhole // named colon\n: // inner opener\n(\
             // inner first\na: _, b: . // inner close\n), \
             (// bare first\nc: ., _: . // bare close\n) // outer close\n)",
            "(// singleton first\nx // singleton colon\n: \
             // singleton type\nBox[// type argument\n. \
             // type close\n] // singleton close\n)",
            "(// inferred first\nx // inferred close\n)",
            "(// explicit first\nx: _ // explicit close\n)",
            "(// leading comma\n, // repeated comma\n, x: . \
             // trailing comma\n, // final close\n)",
        ] {
            for source in [
                format!("mod m; fn example({pattern}) {{ () }}"),
                format!("mod m; fn example(whole: {pattern}) {{ () }}"),
                format!("mod m; equiv example({pattern}) {{ (); () }}"),
                format!("mod m; fn example() {{ .({pattern}) {{ () }} }}"),
                format!("mod m; fn example() {{ let .{pattern} // after pattern\n= value; () }}"),
                format!("mod m; fn example() {{ do(m) {{ let .{pattern} = value; () }} }}"),
                format!("mod m; fn example() {{ do(m) {{ let .{pattern} <- value; () }} }}"),
                format!("mod m; fn example() {{ let .(<Hidden> {pattern}) = value; () }}"),
                format!(
                    "mod m; fn example() {{ do(m) {{ let .(<Hidden> {pattern}) = value; () }} }}"
                ),
            ] {
                // A bare single name after `whole:` denotes a type, not a pattern.
                if pattern.contains("inferred first")
                    && (source.contains("example(whole:") || source.contains("<Hidden>"))
                {
                    continue;
                }
                let formatted = roundtrip(&source);
                assert_eq!(
                    comments(&source),
                    comments(&formatted),
                    "{source}\n{formatted}"
                );
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&formatted),
                    "{source}\n{formatted}"
                );
            }
        }
    }

    #[test]
    fn pattern_disambiguating_written_inference_annotations_are_preserved() {
        let printed = roundtrip(
            "mod main; fn open() -> . {
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
                kind: crate::ast::OpChainKind::Normal { pattern, slots, .. },
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
            ("_ ++", "left // marker1\n ++ // marker2\n ++"),
            ("_ +~ _", "left // marker1\n +~ // marker2\n right"),
            ("_ + _", "(left // marker1\n) // marker2\n + right"),
            ("_ + _", "call( // marker1\n left) // marker2\n + right"),
            ("_ + _", "(left + middle) // marker1\n + right"),
            ("_ + _", "left // marker1\n\n // marker2\n + right"),
        ] {
            let source = format!(
                "mod sample;\nop {pattern} {{ impl pick }}\nfn example() -> . {{ {expression} }}"
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
        let pp = roundtrip("mod foo;");
        assert_eq!(pp, "mod foo;\n");
    }

    #[test]
    fn nested_module_path() {
        roundtrip("mod pkg/utils/string;");
    }

    #[test]
    fn pure_function_and_rec_modifiers_render_canonical_order() {
        let pp = roundtrip(
            "mod x; pure pub fn f() -> . { () } pub type T = .; pub type { N : . { pub mk; pub get; }; }; pub type L = { A: . }; pub macro checked : [A] A -> A { impl checked_impl; }; pub op _ + _ { impl plus; }; pub op_var [* *] { foldr append empty; }; rec(loop) pub fn g() -> . { () }",
        );
        assert!(pp.contains("pub pure fn f() {"));
        assert!(pp.contains("pub type T = .;"));
        assert!(pp.contains("pub type { N: ."));
        assert!(pp.contains("pub type L ="));
        assert!(pp.contains("pub macro checked :"));
        assert!(pp.contains("pub op _ + _ { impl plus }"));
        assert!(pp.contains("pub op_var [* *] { foldr append empty }"));
        assert!(pp.contains("pub rec(loop) fn g() {"));
    }

    #[test]
    fn fills_macro_schedule_has_a_stable_canonical_spelling() {
        let pp = roundtrip("mod x; macro demo : . -> . { impl (fills) demo_impl; };");
        assert!(pp.contains("{ impl(fills) demo_impl }"), "got:\n{pp}");
    }

    #[test]
    fn declaration_callable_paths_round_trip_all_dotted_shapes() {
        let pp = roundtrip(
            "mod x; op _ + _ { impl helpers.add; }; op_var [* *] { foldr helpers.Box.push Box.empty; finalize finish; }; macro demo : . -> . { impl(fills) helpers.demo_impl; };",
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
        let source = r#"mod x;
op _ + _ {
  impl helpers // op owner
    . // op separator
    add;
};
op_var [* *] {
  foldr helpers // step owner
    .push helpers // base owner
    . // base separator
    empty // base path
    ;
  finalize helpers // finalize owner
    .finish;
};
macro demo : . -> . {
  impl( // schedule open
    fills // schedule marker
  ) helpers // macro owner
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
            "// macro owner",
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
        let source = r#"mod x;

/// declaration docs
macro demo : Box[
  // nested call type
  A
] -> . {
  impl
  /// interstitial implementation
  helpers.demo_impl;
};
"#;
        let parsed = parse(source).unwrap_or_else(|e| panic!("parse failed: {e:?}\n{source}"));
        let crate::ast::Item::Macro(macro_def, _) = &parsed.items[0] else {
            panic!("expected macro")
        };
        let original_doc = macro_def.doc.clone();
        assert_eq!(
            original_doc
                .as_ref()
                .expect("declaration doc")
                .lines
                .as_slice(),
            ["declaration docs"]
        );
        let original_body_comments = macro_def
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
        let crate::ast::Item::Macro(macro_def, _) = &reparsed.items[0] else {
            panic!("expected macro after reparse")
        };
        assert_eq!(macro_def.doc, original_doc);
        let reparsed_body_comments = macro_def
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
                "mod x/right; op_var [* *] { foldr append empty; };",
                "foldr append empty }",
            ),
            (
                "mod x/left; op_var [* *] { foldl append_left empty; };",
                "foldl append_left empty }",
            ),
            (
                "mod x/positive; op_var [% %] { foldr1 push seed; };",
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
                "mod x/y; rec(loop) { fn local() -> . { () } }",
                "rec(loop) fn local() {",
            ),
            (
                "mod x/y; rec(loop) { pub(x) fn scoped() -> . { () } }",
                "pub(x) rec(loop) fn scoped() {",
            ),
            (
                "mod x/y; rec(loop) { pub fn exported() -> . { () } }",
                "pub rec(loop) fn exported() {",
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
            "mod x/y; pub(x) rec(loop) fn f() -> . { () }",
            "mod x/y; rec(loop) pub(x) fn f() -> . { () }",
        ] {
            let pp = roundtrip(source);
            assert!(pp.contains("pub(x) rec(loop) fn f() {"), "got:\n{pp}");
        }
    }

    #[test]
    fn singleton_rec_signature_keeps_visibility_before_rec() {
        let module = parse("mod x/y; rec(loop) { pub(x) fn f(value: .) -> . { value } }")
            .expect("fixture parses");
        assert_eq!(
            pretty_item_signature(&module.items[0]),
            "pub(x) rec(loop) fn f(value: .)"
        );
    }

    // ---- import statements ---------------------------------------------

    #[test]
    fn import_selective_single() {
        roundtrip("mod x; import a/b {fn foo}");
    }

    #[test]
    fn import_selective_multi() {
        roundtrip("mod x; import pkg/mod {fn foo; fn bar; fn baz}");
    }

    #[test]
    fn import_selective_type_and_value_names() {
        roundtrip("mod x; import pkg/mod {type Item; fn item; type Field}");
    }

    #[test]
    fn import_qualified_multi_segment() {
        roundtrip("mod x; import a/b/c as m;");
    }

    #[test]
    fn import_qualified_single_segment() {
        roundtrip("mod x; import a as m;");
    }

    #[test]
    fn import_selective_round_trips() {
        roundtrip("mod x; import pkg/mod {type Foo; type Bar}");
        roundtrip("mod x; import match {macro match}");
    }

    #[test]
    fn import_intrinsics() {
        roundtrip("mod x; import __intrinsics__;");
    }

    // ---- fn ----------------------------------------------------------

    #[test]
    fn fn_def_zero_arg_unit() {
        roundtrip("mod x; fn main() -> . { () }");
    }

    #[test]
    fn fn_def_pub() {
        roundtrip("mod x; pub fn main() -> . { () }");
    }

    #[test]
    fn fn_def_identity_function() {
        roundtrip("mod x; pub fn id[A](x: A) -> A { x }");
    }

    #[test]
    fn fn_def_rejects_comma_binder_group() {
        assert!(parse("mod x; pub fn pair[A, B](x: A, y: B) -> A { x }").is_err());
    }

    #[test]
    fn fn_def_intermixed_params() {
        roundtrip("mod x; fn f[A](x: A)[B](y: B) -> A { x }");
    }

    #[test]
    fn fn_def_elided_unit_return_round_trips() {
        roundtrip("mod x; fn say_hi() { () }");
    }

    #[test]
    fn named_unit_results_are_canonical_defaults() {
        let pp = roundtrip("mod x; host fn print(s: String) -> .; fn say_hi() -> . { () }");
        assert!(pp.contains("host fn print(s: String);"), "{pp}");
        assert!(pp.contains("fn say_hi() { () }"), "{pp}");
        assert!(!pp.contains("->"), "{pp}");
    }

    #[test]
    fn unit_default_does_not_collapse_function_results_or_lambda_annotations() {
        let pp = roundtrip(
            "mod x; type Unit = .; host fn factory() -> . -> .; fn alias() -> Unit { () } fn factory() -> . -> . { .(value: .) -> . { value } }",
        );
        assert!(pp.contains("host fn factory() -> . -> .;"), "{pp}");
        assert!(pp.contains("fn alias() -> Unit"), "{pp}");
        assert!(pp.contains("fn factory() -> . -> ."), "{pp}");
        assert!(pp.contains(".(value: .) -> ."), "{pp}");
    }

    #[test]
    fn host_unit_result_comments_survive_formatting() {
        for source in [
            "mod x; host fn tick() // arrow\n -> .;",
            "mod x; host fn tick() -> // result\n .;",
            "mod x; host fn tick() -> . // end\n;",
            "mod x; host fn tick() // end\n;",
        ] {
            let pp = roundtrip(source);
            for comment in ["// arrow", "// result", "// end"] {
                assert_eq!(
                    pp.matches(comment).count(),
                    source.matches(comment).count(),
                    "{pp}"
                );
            }
        }
    }

    #[test]
    fn op_call_symbol_word_keeps_internal_punctuation() {
        let output = roundtrip(
            "mod x; type { A : . { mk; get; }; }; fn cat(a: A, b: A) -> A { a } op _ &&++ _ { impl cat; }; fn main(x: A, y: A) -> A { x&&++y }",
        );
        assert!(output.contains("x &&++ y"), "{output}");
    }

    #[test]
    fn operator_forms_round_trip_with_run_boundaries() {
        let fused = roundtrip(
            "mod fused; fn pick(a: A, b: A) -> A { a } op _ <! _ !> { impl pick; }; fn run(a: A, b: A) -> A { a <! b !> }",
        );
        assert!(fused.contains("op _ <! _ !> { impl pick }"));
        assert!(fused.contains("a <! b !>"));

        let compact = roundtrip(
            "mod separated; fn pick(a: A, b: A) -> A { a } op _ <! _ !> { impl pick; }; fn run(a: A, b: A) -> A { a<!b !> }",
        );
        assert!(compact.contains("op _ <! _ !> { impl pick }"));
        assert!(compact.contains("a <! b !>"));
        assert!(parse("mod separated; import syntax {op _ < ! _ ! >}").is_err());

        let fold = roundtrip(
            "mod folded; fn empty() -> A { value } fn push(a: A, b: A) -> A { a } op_var [! !] { foldr push empty; }; fn run(a: A, b: A) -> A { [! a, b !] }",
        );
        assert!(fold.contains("op_var [! !] { foldr push empty }"));
        assert!(fold.contains("[! a, b !]"));

        let import_source = "mod imported; import syntax {op _ <; op _ <!; op < _; op <! _}";
        let first =
            crate::pass::parser::parse_lazy(import_source).expect("operator-import header parses");
        let rendered = pretty_module(first.module());
        assert!(rendered.contains("import syntax { op < _; op <! _; op _ <; op _ <! }"));
        crate::pass::parser::parse_lazy(&rendered)
            .expect("formatted operator-import header reparses");
    }

    #[test]
    fn delayed_operator_headers_canonicalize_dots_and_preserve_comments() {
        for head in ["op", "op_macro"] {
            let source = format!(
                "mod m; {head} . // first operand\n _ && . // chain operand\n __ {{ impl target; }} fn run(left: A, right: A) {{ left && right }}"
            );
            let formatted = roundtrip(&source);
            assert!(
                formatted.contains(&format!("{head} ._ && .__")),
                "{formatted}"
            );
            for comment in ["first operand", "chain operand"] {
                assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
            }
            let source = format!(
                "mod m; import absent {{{head} . // import operand\n _ && . __ as . _ %% . __}}"
            );
            let formatted = roundtrip(&source);
            assert!(
                formatted.contains(&format!("{head} ._ && .__ as ._ %% .__")),
                "{formatted}"
            );
            assert_eq!(
                formatted.matches("import operand").count(),
                1,
                "{formatted}"
            );
        }
    }

    #[test]
    fn op_chain_layout_preserves_right_left_and_greedy_topology() {
        for pattern in ["_ + __", "__ + _", "_ + ___"] {
            let module_prefix = format!(
                "mod x; fn combine(a: A, b: A) -> A {{ a }} op {pattern} {{ impl combine; }};"
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
            ("_ ~", "value ~ ~ ~", "value ~ ~\n  ~"),
        ] {
            let module_prefix = format!(
                "mod x; fn unary(value: A) -> A {{ value }} op {pattern} {{ impl unary; }};"
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
                "_ < _ >",
                "a < b < c > >",
                10,
                "a\n  < b\n      < c\n      >\n  >",
            ),
            (
                "fn combine(a: A, b: A) -> A { a }",
                "combine",
                "_ &&++ __",
                "a &&++ b &&++ c",
                12,
                "a\n  &&++ b\n  &&++ c",
            ),
        ] {
            let module_prefix =
                format!("mod x; {implementation} op {pattern} {{ impl {implementation_name}; }};");
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
        let module_prefix = "mod x; fn add(a: A, b: A) -> A { a } fn maybe(a: A, b: A) -> A { a } op _ + __ { impl add; }; op _ ? _ { impl maybe; };";
        assert_op_expr_layout_and_topology(
            module_prefix,
            "fn use_op() -> A",
            "use_op",
            "(a ? z) + b + c",
            12,
            "(a ? z)\n  + b\n  + c",
        );
    }

    #[test]
    fn op_chain_layout_anchors_nested_inline_expressions() {
        let normal_prefix = "mod x; fn combine(a: A, b: A) -> A { a } op _ + __ { impl combine; };";
        assert_op_expr_layout_and_topology(
            normal_prefix,
            "fn nested_op() -> A",
            "nested_op",
            "wrap(a + b + c)",
            12,
            "wrap(a\n       + b\n       + c)",
        );

        let variadic_prefix = "mod x; op_var [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            variadic_prefix,
            "fn nested_list() -> A",
            "nested_list",
            "wrap([! a, b, c !])",
            12,
            "wrap([!\n, a\n, b\n, c\n!])",
        );
    }

    #[test]
    fn variadic_op_layout_preserves_elements_delimiters_and_topology() {
        let list_prefix = "mod x; op_var [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            list_prefix,
            "fn list() -> A",
            "list",
            "[! a, b, c !]",
            12,
            "[!\n, a\n, b\n, c\n!]",
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
            "mod x; op _ => _ { impl pair; }; op_var [% %] { foldr insert empty; };";
        assert_op_expr_layout_and_topology(
            entries_prefix,
            "fn entries() -> A",
            "entries",
            "[% a => b, c => d %]",
            16,
            "[%\n, a => b\n, c => d\n%]",
        );

        let repeated_comma_prefix = "mod x; op_var [! !] { foldr push empty; };";
        assert_op_expr_layout_and_topology(
            repeated_comma_prefix,
            "fn separated() -> A",
            "separated",
            "[! , a,, b,, c, !]",
            12,
            "[!\n, a\n, b\n, c\n!]",
        );

        assert_op_expr_layout_and_topology(
            entries_prefix,
            "fn broken_entry() -> A",
            "broken_entry",
            "[% key => build(alpha, beta, gamma), other => value %]",
            16,
            "[%\n, key\n    => build(\n       , alpha\n       , beta\n       , gamma\n       )\n, other => value\n%]",
        );
    }

    // ---- alias -----------------------------------------------------

    #[test]
    fn type_alias_nullary() {
        roundtrip("mod x; type Logger = String -> .;");
    }

    #[test]
    fn type_alias_parametric_pub() {
        roundtrip("mod x; pub type Pair[A][B] = (A & B);");
    }

    #[test]
    fn type_alias_rejects_comma_binder_group() {
        assert!(parse("mod x; type F[A, B] = [A, B] A -> B;").is_err());
    }

    // ---- newtype -------------------------------------------------------

    #[test]
    fn newtype_nullary() {
        roundtrip("mod x; type { Celsius : . { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_recursive() {
        roundtrip("mod x; rec type { List[A] : (. | (A & List[A])) { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_with_single_existential_header_roundtrip() {
        // Existential binders live in the newtype header, emitted as
        // `<U>` atoms between the universal-parameter list and `:`.
        roundtrip("mod x; type { Pack[A] <U> : (A & U) { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_with_multi_existential_header_roundtrip() {
        // Multi-binder existential header — whitespace-separated atoms.
        roundtrip("mod x; type { Box <A> <B> : (A & B) { pub mk; pub get; }; };");
    }

    #[test]
    fn newtype_member_comments_follow_canonical_member_order() {
        let pp = roundtrip(
            "mod x; type { Box : . { // projector comment
get; // constructor comment
mk; }; };",
        );
        let constructor_comment = pp.find("// constructor comment").unwrap();
        let constructor = pp.find("mk;").unwrap();
        let projector_comment = pp.find("// projector comment").unwrap();
        let projector = pp.find("get").unwrap();
        assert!(
            constructor_comment < constructor
                && constructor < projector_comment
                && projector_comment < projector,
            "expected member comments to follow constructor/projector order; got:\n{pp}"
        );
    }

    // ---- labels -------------------------------------------------------

    #[test]
    fn ordinary_alias_round_trip_preserves_visibility_and_exact_path() {
        let formatted = roundtrip("mod x; pub(x) type Local = provider.Original;");
        assert!(formatted.contains("pub(x) type Local = provider.Original;"));
    }

    #[test]
    fn ordinary_alias_target_comments_survive_once_in_order() {
        let formatted = roundtrip(
            "mod x;\n/// Alias family.\npub type Local = (\n// target module\nprovider.\n// target type\nOriginal\n// after target\n);",
        );
        let mut previous = 0;
        for comment in ["// target module", "// target type", "// after target"] {
            assert_eq!(formatted.matches(comment).count(), 1);
            let position = formatted.find(comment).expect("preserved comment");
            assert!(position >= previous);
            previous = position;
        }
        assert_eq!(formatted.matches("/// Alias family.").count(), 1);
        assert!(formatted.contains("provider.Original"));
    }

    #[test]
    fn ordinary_alias_keeps_body_comments_separate_from_declaration_docs() {
        for external_doc in ["", "/// External alias documentation.\n"] {
            let source = format!(
                "mod x;\n{external_doc}type Local = (\n// Interior target note.\nprovider.Original &\n// Interior unit note.\n.);"
            );
            let parsed = parse(&source).expect("parse source");
            let Item::TypeAlias(before) = &parsed.items[0] else {
                panic!("expected ordinary alias");
            };
            assert_eq!(before.doc.is_some(), !external_doc.is_empty());
            let formatted = pretty_module(&parsed);
            let reparsed = parse(&formatted).expect("parse formatted declaration");
            let Item::TypeAlias(after) = &reparsed.items[0] else {
                panic!("expected ordinary alias after formatting");
            };
            assert_eq!(
                before.doc.as_ref().map(|doc| &doc.lines),
                after.doc.as_ref().map(|doc| &doc.lines),
            );
            for note in ["// Interior target note.", "// Interior unit note."] {
                assert_eq!(formatted.matches(note).count(), 1);
            }
            assert_eq!(formatted, pretty_module(&reparsed));
        }
    }

    #[test]
    fn compact_nominal_anonymous_round_trip() {
        roundtrip("mod x; type { Foo : . };");
    }

    #[test]
    fn compact_nominal_anonymous_multi_round_trip() {
        roundtrip("mod x; type { Foo : .; Bar : I32 };");
    }

    #[test]
    fn compact_nominal_named_round_trip() {
        roundtrip("mod x; type T = { Foo : . };");
    }

    #[test]
    fn compact_nominal_named_parametric_round_trip() {
        roundtrip("mod x; type T[A] = { Foo[A] : A };");
    }

    #[test]
    fn nominal_rhs_references_round_trip() {
        roundtrip(
            "mod x; type { Value[*F][A]: F[A]; }; type Choice[*G][B] = Value[G][B] & { Other: .; };",
        );
    }

    #[test]
    fn compact_nominal_pub_round_trip() {
        roundtrip("mod x; pub type { Foo : I32; Bar : Bool };");
    }

    #[test]
    fn nominal_declaration_headers_preserve_comments() {
        let source = include_str!(
            "../../test-data/goldens/00_success/fmt_label_declaration_comments/workdir/main.kio"
        );
        let expected = include_str!(
            "../../test-data/goldens/00_success/fmt_label_declaration_comments/expected.stdout"
        );
        assert_eq!(roundtrip(source), expected);
        assert_eq!(parsed_structure(source), parsed_structure(expected));

        for declaration in [
            "type // 1\n{ Field // 2\n: // 3\n. } // 4\n;",
            "pub type // 1\nChoice // 2\n[// 3\nA // 4\n] // 5\n= // 6\n{ First // 7\n[// 8\nB // 9\n] // 10\n< // 11\nHidden // 12\n> // 13\n: // 14\n(B & Hidden) // 15\n} // 16\n| // 17\n(// 18\nSecond // 19\n) // 20\n;",
            "rec type // 1\n{ List[A] // 2\n: // 3\n(. | (A & List[A])) };",
            "rec { /// Labels documentation.\npub type // 1\nChoice = // 2\n{ Field // 3\n: // 4\n. }; }",
        ] {
            let source = format!("mod m; {declaration}");
            let printed = roundtrip(&source);
            let comments = |text: &str| {
                let (tokens, trailing) = crate::pass::lexer::lex_with_trailing(text).unwrap();
                tokens
                    .into_iter()
                    .flat_map(|token| token.leading_trivia)
                    .chain(trailing)
                    .filter_map(|trivia| match trivia {
                        Trivia::LineComment { text, .. } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&printed),
                "{source}"
            );
            let lazy = crate::pass::parser::parse_lazy(&source)
                .unwrap()
                .force_all()
                .unwrap();
            assert_eq!(pretty_module(&lazy), printed);
        }
    }

    #[test]
    fn nominal_rhs_separator_comments_preserve_operand_order() {
        let comments = |source: &str| {
            let (tokens, trailing) = crate::pass::lexer::lex_with_trailing(source).unwrap();
            tokens
                .into_iter()
                .flat_map(|token| token.leading_trivia)
                .chain(trailing)
                .filter_map(|trivia| match trivia {
                    Trivia::LineComment { text, .. } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for op in ["&", "|"] {
            for rhs in [
                "// 1\nOP // 2\n{ // 3\nFirst: . } // 4\nOP // 5\nOP // 6\nExisting // 7\nOP // 8\n{ // 9\nLast: . } // 10\nOP // 11\nOP // 12\n",
                "Existing // 1\nOP // 2\n(// 3\n{ Middle: . } // 4\nOP // 5\n{ Last: . } // 6\n) // 7\nOP // 8\nFinal // 9\nOP // 10\n",
                "// 1\nOP // 2\n{ // 3\nOnly: . } // 4\nOP // 5\n",
            ] {
                let source = format!("mod m; type Choice = {};", rhs.replace("OP", op));
                let formatted = roundtrip(&source);
                assert_eq!(
                    comments(&source),
                    comments(&formatted),
                    "{source}\n{formatted}"
                );
                assert_eq!(parsed_structure(&source), parsed_structure(&formatted));
                let lazy = crate::pass::parser::parse_lazy(&source)
                    .unwrap()
                    .force_all()
                    .unwrap();
                assert_eq!(pretty_module(&lazy), formatted);
            }
        }
    }

    #[test]
    fn call_argument_comments_survive_expression_boundaries() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, text)| text.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for payload in [
            "(// before unit\n)",
            "(() // after unit\n)",
            "// before unit\n()",
            "(// before value\nready)",
            "(ready // after value\n)",
            "// before value\nready",
            "// before value\nother",
            "(other // after value\n)",
            "(// before value\nother // after value\n)",
            "((// before inner\nother // after inner\n) // after outer\n)",
            "(call(// call argument\nother) // after call\n)",
            "(.() { // function body\n() } // after function\n)",
        ] {
            for (prefix, suffix) in [("", ""), ("", ", next"), ("first, ", "")] {
                let source = format!("mod x; fn f() {{ consume({prefix}{payload}{suffix}) }}");
                let printed = roundtrip(&source);
                assert_eq!(comments(&printed), comments(&source), "{source}\n{printed}");
                assert_eq!(
                    parsed_structure(&printed),
                    parsed_structure(&source),
                    "comments changed the argument structure: {source}\n{printed}"
                );
            }
        }
    }

    #[test]
    fn call_arguments_keep_closing_comments() {
        for entries in ["ready", "ready, other", "ready,"] {
            let body = format!("target({entries} // close\n)");
            let source = format!("mod x; fn f() {{ {body} }}");
            let printed = roundtrip(&source);
            assert_eq!(
                printed.matches("// close").count(),
                1,
                "{source}\n{printed}"
            );
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
        }
    }

    #[test]
    fn empty_brace_unit_keeps_inside_and_outside_comments_distinct() {
        let expression = "{ // inside\n}";
        for body in [
            expression.to_owned(),
            format!("({expression} // outside\n)"),
            format!("call({expression})"),
            format!("({expression}, value)"),
        ] {
            let source = format!("mod x; fn f() {{ {body} }}");
            let printed = roundtrip(&source);
            assert_eq!(
                printed.matches("// inside").count(),
                1,
                "{source}\n{printed}"
            );
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
            let inside = printed.find("// inside").unwrap();
            let open = printed[..inside].rfind('(').expect("unit opener");
            let close = inside + printed[inside..].find(')').expect("unit closer");
            assert!(open < inside && inside < close, "{source}\n{printed}");
            if source.contains("// outside") {
                assert_eq!(printed.matches("// outside").count(), 1);
                assert!(
                    close < printed.find("// outside").unwrap(),
                    "{source}\n{printed}"
                );
            }
        }
        assert_eq!(
            roundtrip("mod x; fn f() { {} }"),
            "mod x;\n\nfn f() { () }\n"
        );
    }

    #[test]
    fn qualified_path_comments_hoist_once_in_source_order() {
        let path = "m // before dot\n. // before segment\nready";
        for body in [
            format!("{path} // before call\n()"),
            format!("call({path} // before close\n)"),
        ] {
            let source = format!("mod x; fn f() {{ {body} }}");
            let printed = roundtrip(&source);
            let comments = |source: &str| {
                source
                    .lines()
                    .filter_map(|line| line.split_once("//").map(|(_, text)| text.trim()))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
        }
    }

    #[test]
    fn ufcs_splice_boundaries_keep_comments_in_order() {
        let bodies = [
            "row // receiver\n.> // callee\nselect",
            "row // first receiver\n.> // first callee\nselect // next receiver\n.> // next callee\nfinish",
        ];
        for body in bodies {
            let source = format!("mod x; fn f() {{ {body} }}");
            let printed = roundtrip(&source);
            let comments = |source: &str| {
                source
                    .lines()
                    .filter_map(|line| line.split_once("//").map(|(_, text)| text.trim()))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
        }
    }

    #[test]
    fn compact_nominal_recursive_round_trip() {
        roundtrip("mod x; rec type { List[A] : (. | (A & List[A])) };");
    }

    #[test]
    fn recursive_type_group_round_trip() {
        let formatted =
            roundtrip("mod x; rec { pub type A = B; pub type { B : A { pub mk; pub get; }; }; }");
        assert!(formatted.contains("rec {\n  pub type A = B;"));
    }

    #[test]
    fn declaration_headers_and_host_signatures_preserve_comments() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, text)| text.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for source in [
            include_str!("../../test-data/goldens/00_success/fmt_declaration_header_comments/workdir/main.kio").to_owned(),
            "mod m; /// Function docs.\npure // purity\npub // before scope\n( // scope open\nm // scope close\n) // before fn\nfn // before name\nf // before params\n() { () }".to_owned(),
            "mod m; rec(loop) { /// Member docs.\npub // visibility\nfn // name\nf() { () } }".to_owned(),
            "mod m; /// Recursive docs.\npub // visibility\nrec(loop) fn // name\nf() { () }".to_owned(),
            "mod m; equiv // name\ne[ // binder\nA](x: A) // body\n{ x; x }".to_owned(),
            "mod m; equiv e() // body\n{ (); () }".to_owned(),
            "mod m; /// Host docs.\nhost pub // modifier\nfn f[ // first binder\nA // first\n][ // second binder\nB // second\n] // empty group\n( // inside empty\n) // next group\n[C](value: C) -> C;".to_owned(),
            "mod m; host fn f( // leading\n, value // colon\n: // type\n. // first comma\n, // repeated\n, next: . , // trailing\n);".to_owned(),
            "mod m; host fn f[ // opening\n* // kind\nF // closing\n]( // empty\n);".to_owned(),
            "mod m; /// Labels docs.\npub // modifier\nrec type List = {Nil: .} | {Cons: List};".to_owned(),
            "mod m; /// Literal docs.\npub // modifier\nlit // name\nanswer // equals\n= // value\n42 // terminator\n;".to_owned(),
        ] {
            let printed = roundtrip(&source);
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            assert_eq!(parsed_structure(&source), parsed_structure(&printed), "{source}\n{printed}");
            let lazy = crate::pass::parser::parse_lazy(&source).unwrap().force_all().unwrap();
            assert_eq!(pretty_module(&lazy), printed);
            let tooling = crate::pass::parser::probe_tooling(&source, Some(crate::ast::KioFileKind::Module), None, None);
            let Some(crate::pass::parser::ToolingSyntax::Module(module)) = tooling.syntax else {
                panic!("tooling parse failed: {source}");
            };
            assert_eq!(pretty_module(&module), printed, "{source}");
        }
        for declaration in [
            "pub pure fn f() { () }",
            "host fn f[A][B](value: A) -> B;",
            "pub lit answer = 42;",
        ] {
            assert_eq!(
                roundtrip(&format!("mod m; {declaration}")),
                format!("mod m;\n\n{declaration}\n")
            );
        }
    }

    #[test]
    fn bodyless_signature_comments_leave_contract_digest_unchanged() {
        for declaration in [
            "/// Export docs.\npub // visibility\npure // purity\nfn // name\nf[ // first binder\nA // first\n][ // second binder\nB // second\n] // empty value args\n( // inside empty\n) // type application\n[C](value // colon\n: // type\nC // close value\n) -> C;",
            "/// Host docs.\nhost // modifier\nfn // name\nf[ // first binder\nA // first\n][ // second binder\nB // second\n] // empty value args\n( // inside empty\n) // type application\n[C](value // colon\n: // type\nC // close value\n) -> C;",
            "pub // modifier\nfn f[ // binder\nA];",
        ] {
            let source = format!(
                "sig app v(1); v(1) {{ nonbreaking {{ add {{ mod api {{ {declaration} }} }} }} }}"
            );
            let parsed = crate::pass::parser::parse_signature_file(&source, None).unwrap();
            let printed = crate::sig::emit_signature_file(&parsed);
            let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
            assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
            assert_eq!(source_structure(&parsed), source_structure(&reparsed));
            let comments = |source: &str| {
                source
                    .lines()
                    .filter_map(|line| {
                        line.split_once("//")
                            .map(|(_, text)| text.trim().to_owned())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            let original = crate::sig::replay(&parsed).unwrap();
            let formatted = crate::sig::replay(&reparsed).unwrap();
            assert_eq!(
                crate::sig::contract_digest(&original.current),
                crate::sig::contract_digest(&formatted.current)
            );
            let uncommented = source
                .lines()
                .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
                .collect::<Vec<_>>()
                .join("\n");
            let plain = crate::pass::parser::parse_signature_file(&uncommented, None).unwrap();
            let plain_contract = crate::sig::replay(&plain).unwrap();
            assert_eq!(
                crate::sig::contract_digest(&original.current),
                crate::sig::contract_digest(&plain_contract.current)
            );
            #[cfg(all(feature = "cli", feature = "surface"))]
            assert_eq!(
                crate::sig::emit_signature_comparison(&parsed),
                crate::sig::emit_signature_comparison(&plain)
            );
        }
    }

    #[test]
    fn recursive_function_header_comments_round_trip() {
        let header = "rec // after rec\n(// after open\nprovider \
            // before dot\n. // after dot\nloop // before close\n) // after close\n";
        for declaration in [
            format!("{header}fn f() {{ // first body\n() }}"),
            format!("pub(m) {header}fn f() {{ // first body\n() }}"),
            format!("{header}{{ fn f() {{ // first body\n() }} }}"),
            format!("{header}{{ // member boundary\nfn f() {{ // first body\n() }} }}"),
            format!("{header}{{ /// Member documentation.\nfn f() {{ // first body\n() }} }}"),
            format!(
                "{header}{{ fn f() {{ // first body\n() }}; fn g() {{ // second body\n() }} }}"
            ),
        ] {
            let source = format!("mod m; {declaration}");
            let formatted = roundtrip(&source);
            let mut previous = 0;
            for comment in source
                .lines()
                .filter_map(|line| line.split_once("//").map(|(_, text)| text.trim_end()))
            {
                let comment = format!("//{comment}");
                assert_eq!(formatted.matches(&comment).count(), 1, "{formatted}");
                let position = formatted.find(&comment).unwrap();
                assert!(position >= previous, "{formatted}");
                previous = position;
            }
            assert_eq!(parsed_structure(&source), parsed_structure(&formatted));
            let lazy = crate::pass::parser::parse_lazy(&source)
                .unwrap()
                .force_all()
                .unwrap();
            assert_eq!(pretty_module(&lazy), formatted);
            let tooling = crate::pass::parser::probe_tooling(
                &source,
                Some(crate::ast::KioFileKind::Module),
                None,
                None,
            );
            let Some(crate::pass::parser::ToolingSyntax::Module(module)) = tooling.syntax else {
                panic!("tooling parse failed");
            };
            assert_eq!(pretty_module(&module), formatted);
        }
    }

    #[test]
    fn recursive_type_group_member_and_closer_comments_round_trip() {
        let formatted = roundtrip(
            "mod x; rec {
// group-leading context
/// Alias docs.
pub type A = B;
/// Nominal docs.
pub type { B : A { pub mk; pub get; }; };
// group-trailing context
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
        roundtrip("mod x; fn id[A](v: A) -> A { v } equiv id_id[A](x: A) { id(x); id(id(x)) }");
    }

    #[test]
    fn equiv_three_term_round_trip() {
        roundtrip("mod x; equiv three { (); (); () }");
    }

    #[test]
    fn equiv_no_params_round_trip() {
        roundtrip("mod x; equiv simple { (); () }");
    }

    #[test]
    fn equiv_extra_semicolons_canonicalize() {
        let pp = roundtrip("mod x; equiv simple { ;; ();;; ();;; }");
        assert!(
            pp.contains("equiv simple() {\n  ();\n  ()\n}"),
            "got:\n{pp}"
        );
        assert!(!pp.contains(";;"), "got:\n{pp}");
    }

    #[test]
    fn equiv_clause_comment_survives_round_trip() {
        let pp = roundtrip("mod x; equiv simple { (); // second\n () }");
        assert!(
            pp.contains("equiv simple() {\n  ();\n  // second\n  ()\n}"),
            "expected comment before second equiv arm; got:\n{pp}"
        );
    }

    // ---- type expressions ----------------------------------------------

    #[test]
    fn ordinary_and_marked_type_names_round_trip_identically() {
        let output = roundtrip(
            "mod x; type Alias[A] = A; type _Alias[_A] = _A; type { Box[B] : B { mk; get; }; }; type { _Box[_B] : _B { mk; get; }; }; fn id[T](x: T) -> T { x } fn marked[_A](x: _A) -> _A { id[_A](x) }",
        );
        for retained in [
            "type _Alias[_A] = _A;",
            "type { _Box[_B]: _B",
            "fn marked[_A](x: _A) -> _A",
            "id[_A](x)",
        ] {
            assert!(
                output.contains(retained),
                "formatter dropped or rewrote `{retained}`:\n{output}"
            );
        }
    }

    #[test]
    fn type_function_zero_param() {
        roundtrip("mod x; type F = . -> Int;");
    }

    #[test]
    fn type_function_zero_param_after_binder_round_trips() {
        roundtrip("mod x; type F = [T] . -> T;");
    }

    #[test]
    fn compact_forall_runs_remain_compact_after_formatting() {
        let output = roundtrip(
            "mod x; type Poly = [A][*F] F[A] -> A; fn make() -> . { .[*F](x: F[.]) -> F[.] { x } }",
        );
        assert!(output.contains("[A][*F] F[A] -> A"));
        assert!(output.contains(".[*F](x: F[.]) -> F[.]"));
    }

    #[test]
    fn placeholder_stems_and_shadowing_round_trip() {
        for expression in [
            ".x. { pair(x2, x1, x2) }",
            ".arg. { let arg2 = arg1; arg2 }",
            "._x. {\n    _x1.>apply(._y. { _y1 })\n  }",
        ] {
            let output = roundtrip(&format!("mod m; fn make() {{ {expression} }}"));
            assert!(output.contains(expression), "{output}");
        }
    }

    #[test]
    fn type_function_grouped_unit_domain_canonicalizes() {
        let pp = roundtrip("mod x; type F = (.) -> Int;");
        assert!(pp.contains("type F = . -> Int;"), "got:\n{pp}");
    }

    #[test]
    fn type_function_grouped_atomic_domain_canonicalizes() {
        let pp = roundtrip("mod x; type F = (A) -> Int;");
        assert!(pp.contains("type F = A -> Int;"), "got:\n{pp}");
    }

    #[test]
    fn type_function_multi_param() {
        roundtrip("mod x; type F = (Int & Bool & String) -> .;");
    }

    #[test]
    fn type_function_curried_return_stays_curried() {
        roundtrip("mod x; type F = A -> B -> C;");
    }

    #[test]
    fn type_alias_mixed_signature_binders_round_trip() {
        roundtrip("mod x; type F = A -> [B] B -> [C] C;");
    }

    #[test]
    fn type_alias_trailing_signature_binder_round_trip() {
        roundtrip("mod x; type F = A -> [B] B;");
    }

    #[test]
    fn value_param_rank_n_type_round_trips() {
        roundtrip("mod x; fn use[A](f: A -> [B] B -> A) -> . { () }");
    }

    #[test]
    fn type_unit_and_bottom() {
        roundtrip("mod x; type U = .; type Z = !;");
    }

    #[test]
    fn type_sum_and_product() {
        roundtrip("mod x; type S = (A | B); type P = (A & B);");
    }

    #[test]
    fn type_product_chain_three_round_trip() {
        let pp = roundtrip("mod x; type T = (A & B & C);");
        assert!(
            pp.contains("A & B & C") && !pp.contains("(A & "),
            "expected bare chain form, got:\n{pp}"
        );
    }

    #[test]
    fn type_sum_chain_three_round_trip() {
        let pp = roundtrip("mod x; type T = (A | B | C);");
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
        let pp = roundtrip("mod x; type T = ((A & B) & C);");
        assert!(pp.contains("(A & B) & C"), "got:\n{pp}");
    }

    #[test]
    fn type_bare_chain_two_elements_round_trip() {
        // No-paren input round-trips identically with the printer's
        // bare-chain emission — both inputs collapse to the same
        // AST.
        let pp = roundtrip("mod x; type T = A & B;");
        assert!(
            pp.contains("type T = A & B;"),
            "expected bare chain output, got:\n{pp}"
        );
    }

    #[test]
    fn type_chain_mixing_prints_with_load_bearing_parens() {
        // `(A & B) | C` keeps inner parens because the inner chain
        // is a child of the outer (different-op) chain.
        let pp = roundtrip("mod x; type T = ((A & B) | C);");
        assert!(
            pp.contains("(A & B) | C"),
            "expected mixing parens preserved, got:\n{pp}"
        );
    }

    #[test]
    fn type_chain_function_child_keeps_parens() {
        // A function-type child of a `|` chain needs surrounding
        // parens to delimit the function-valued chain item.
        let pp = roundtrip("mod x; type T = ((A -> B) | C);");
        assert!(
            pp.contains("(A -> B) | C"),
            "expected function parens preserved, got:\n{pp}"
        );
    }

    #[test]
    fn grouped_type_comments_survive_all_type_positions() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for ty in [
            "(// first\n. // last\n)",
            "(// first\nA // last\n)",
            "(// first\n! // last\n)",
            "(// first\n& // second\n& // last\n)",
            "(// first\n| // second\n| // last\n)",
            "(// first\nA & B // last\n)",
            "(// first\n& // second\nA & B // third\n& // last\n)",
            "(// first\nA -> B // last\n)",
            "(// first\n[A] A -> A // last\n)",
            "(// first\n(// second\nA // third\n) // last\n)",
            "(A // first\n&.)",
            "(A // first\n|.)",
            "(// first\n&. // last\n)",
            "(// first\n|. // last\n)",
        ] {
            for declaration in [
                format!("fn f() -> {ty} {{ () }}"),
                format!("host fn f() -> {ty};"),
                format!("type T = {ty};"),
                format!("type T = {ty} -> .;"),
                format!("type T = . -> {ty};"),
                format!("type T = Pair[{ty}][.];"),
                format!("type T = Pair[.][{ty}];"),
                format!("fn f(value: {ty}) {{ () }}"),
                format!("fn f() {{ g[{ty}] }}"),
                format!("type T = ({ty} & .);"),
                format!("type T = (. & {ty});"),
                format!("type {{ T : {ty} {{ mk; get }}; }}"),
            ] {
                let source = format!("mod x; {declaration}");
                let printed = roundtrip(&source);
                assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&printed),
                    "type structure changed: {source}\n{printed}"
                );
            }
        }
        assert_eq!(
            roundtrip("mod x; fn f() -> ((.)) { () }"),
            roundtrip("mod x; fn f() { () }")
        );
        assert_eq!(
            roundtrip("mod x; type T = ((A));"),
            roundtrip("mod x; type T = A;")
        );
    }

    #[test]
    fn bare_type_chain_boundary_comments_survive() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for op in ["&", "|"] {
            for chain in [
                format!("{op} // first\nA"),
                format!("{op} // first\n{op} // last\nA"),
                format!("{op} // first\n{op}"),
                format!("A // first\n{op}"),
                format!("A // first\n{op} // last\n{op}"),
                format!("{op} // first\nA // second\n{op} B // third\n{op} // last\n{op}"),
                format!("{op} // first\n.{op}"),
                format!("{op} // final\n"),
                format!("A {op} // final\n"),
                format!("{op} // first\n{op} // final\n"),
            ] {
                for ty in [chain.clone(), format!("({chain})")] {
                    for declaration in [
                        format!("type T = {ty};"),
                        format!("host fn f() -> {ty};"),
                        format!("fn f(value: {ty}) {{ () }}"),
                        format!("type T = Pair[{ty}][.];"),
                        format!("type T = Pair[.][{ty}];"),
                    ] {
                        let source = format!("mod x; {declaration}");
                        let printed = roundtrip(&source);
                        assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                        assert_eq!(
                            parsed_structure(&source),
                            parsed_structure(&printed),
                            "type structure changed: {source}\n{printed}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn type_expression_boundary_comments_survive() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        assert!(parse("mod x; type T = Box(// first\n, // last\n);").is_err());
        for ty in [
            "[// first\nA // second\n] // last\nA",
            "[A] // first\n[B] // last\nA -> B",
            "[* // first\nF // second\n][A // third\n] // last\nF[A]",
            "[A] (// first\n[B] B // last\n)",
            "A // first\n-> // last\nB",
            "(A) // first\n-> // last\nB",
            "(A & B) // first\n-> // last\nC",
            "A -> [// first\nB] // last\nB",
            "Box // first\n[// second\nA // last\n]",
            "module_path // first\n/nested // last\n.Type",
            "module_path // path\n/nested.Type[// first\nA // last\n]",
            "Map[A] // between\n[B]",
            "Box[[A] // first\nA -> // last\nA]",
        ] {
            for declaration in [
                format!("type T = {ty};"),
                format!("type T = Box[{ty}];"),
                format!("fn f(value: {ty}) {{ () }}"),
                format!("host fn f(value: {ty});"),
                format!("host fn f() -> {ty};"),
                format!("type {{ T : {ty} {{ mk; get }}; }}"),
            ] {
                let source = format!("mod x; {declaration}");
                let printed = roundtrip(&source);
                assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&printed),
                    "{source}\n{printed}"
                );
            }
        }
        assert!(parse("mod x; type T = Box();").is_err());
        assert!(parse("mod x; type T = [A,B] ((A)) -> B;").is_err());
        assert_eq!(
            roundtrip("mod x; type T = [A][B] ((A)) -> B;"),
            roundtrip("mod x; type T = [A][B] A -> B;")
        );
    }

    #[test]
    fn type_declaration_header_comments_survive() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for declaration in [
            "host type // first\nT // last\n;",
            "pub // first\nhost // second\ntype // third\nT // last\n;",
            "host type T // first\n[// second\nA // third\n] // last\n;",
            "host type T // first\nrole // second\n(// third\nstr // fourth\n) // last\n;",
            "type // first\n{ T // second\n[A] // third\n: // fourth\nA // last\n{ mk; get }; }",
            "pub // first\ntype { T[// second\nA // third\n] <// fourth\nB // fifth\n> // sixth\n: // seventh\n(A & B) // last\n{ mk; get }; }",
            "type { T : . { pub // first\n// second\nmk; pub // third\n// last\nget }; }",
            "type { T : . { pub(// first\nx // second\n) // third\n// fourth\nmk; // last\nget }; }",
        ] {
            let source = format!("mod x;\n/// Type documentation.\n{declaration}");
            let printed = roundtrip(&source);
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&printed),
                "{source}\n{printed}"
            );
        }
    }

    #[test]
    #[cfg(feature = "surface")]
    fn signature_type_declaration_comments_survive() {
        for declaration in [
            "host type // first\nT // second\nrole(// third\nstr // last\n)",
            "host type T[// first\nA // last\n]",
            "pub type // first\n{ T : // second\n. // third\n{ pub // fourth\n// fifth\nmk; pub // last\nget }; }",
            "type T = [// first\nA] A // second\n-> // last\nA",
        ] {
            let source = format!(
                "sig app v(1); v(1) {{ nonbreaking {{ add {{ mod api {{\n/// Type documentation.\n{declaration}\n}} }} }} }}"
            );
            let parsed = crate::pass::parser::parse_signature_file(&source, None).unwrap();
            let printed = crate::sig::emit_signature_file(&parsed);
            let comments = |source: &str| {
                source
                    .lines()
                    .filter_map(|line| {
                        line.split_once("//")
                            .map(|(_, comment)| comment.trim().to_owned())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
            let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
            assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
            let before = crate::sig::replay(&parsed).unwrap();
            let after = crate::sig::replay(&reparsed).unwrap();
            assert_eq!(
                crate::sig::contract_digest(&before.current),
                crate::sig::contract_digest(&after.current)
            );
        }
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "cli"))]
    fn signature_recursive_newtype_docs_survive_projection() {
        let source = "sig app v(1); v(1) { with { mod api {
rec {
/// First member.
pub type { A : . | B { pub mk; pub get }; };
/// Second member.
pub type { B : . | A { pub mk; pub get }; }
} } }; nonbreaking { add { api.A; api.B } } }";
        let parsed = crate::pass::parser::parse_signature_file(source, None).unwrap();
        let printed = crate::sig::emit_signature_file(&parsed);
        assert_eq!(printed.matches("/// First member.").count(), 1, "{printed}");
        assert_eq!(
            printed.matches("/// Second member.").count(),
            1,
            "{printed}"
        );
        assert!(
            printed.find("/// First member.").unwrap()
                < printed.find("/// Second member.").unwrap(),
            "{printed}"
        );
        let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
        assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
        let comparison = crate::sig::emit_signature_comparison(&parsed);
        assert!(!comparison.contains("///"), "{comparison}");
        assert_eq!(comparison, crate::sig::emit_signature_comparison(&reparsed));
        let before = crate::sig::replay(&parsed).unwrap();
        let after = crate::sig::replay(&reparsed).unwrap();
        assert_eq!(
            crate::sig::contract_digest(&before.current),
            crate::sig::contract_digest(&after.current)
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "cli"))]
    fn signature_newtype_docs_do_not_restore_private_facts() {
        let declaration = "/// Public documentation.\n\
            pub type { T <Hidden> : Hidden {\n\
            // private constructor comment\nmk;\n\
            // private projector comment\nget }; }";
        let module = parse(&format!("mod api; {declaration}")).unwrap();
        let Item::Newtype(newtype) = &module.items[0] else {
            panic!("fixture declares a newtype");
        };
        let projected = crate::sig::project_newtype_declaration(newtype.clone());
        assert!(projected.doc.is_none());
        assert!(projected.existential_params.is_empty());
        assert!(matches!(projected.payload, Type::Unit { .. }));
        assert_eq!(projected.constructor.name, "mk");
        assert_eq!(projected.constructor.vis, crate::ast::Visibility::Private);
        assert_eq!(projected.projector.name, "get");
        assert_eq!(projected.projector.vis, crate::ast::Visibility::Private);
        assert!(projected.constructor.leading_trivia.is_empty());
        assert!(projected.projector.leading_trivia.is_empty());

        let source = format!(
            "sig app v(1); v(1) {{ nonbreaking {{ add {{ mod api {{ {declaration} }} }} }} }}"
        );
        let parsed = crate::pass::parser::parse_signature_file(&source, None).unwrap();
        let printed = crate::sig::emit_signature_file(&parsed);
        assert_eq!(
            printed.matches("/// Public documentation.").count(),
            1,
            "{printed}"
        );
        for omitted in [
            "Hidden",
            "pub mk",
            "pub get",
            "private constructor comment",
            "private projector comment",
        ] {
            assert!(!printed.contains(omitted), "{printed}");
        }
        let comparison = crate::sig::emit_signature_comparison(&parsed);
        assert!(!comparison.contains("//"), "{comparison}");
        let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
        assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
        assert_eq!(comparison, crate::sig::emit_signature_comparison(&reparsed));
        let before = crate::sig::replay(&parsed).unwrap();
        let after = crate::sig::replay(&reparsed).unwrap();
        assert_eq!(
            crate::sig::contract_digest(&before.current),
            crate::sig::contract_digest(&after.current)
        );
    }

    #[test]
    fn type_alias_header_and_body_boundary_comments_survive() {
        let comments = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, comment)| comment.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        for declaration in [
            "type // first\nT = A;",
            "type T // first\n= A;",
            "type T = // first\nA;",
            "type T = // first\n& A;",
            "type T = // first\n|;",
            "type T = // first\n(// second\nA // last\n);",
            "type T // first\n[A] // last\n= A;",
            "type T[// first\n* // second\nF // third\n][A // last\n] = F[A];",
            "pub // first\ntype // second\nT // third\n[A] // fourth\n= // fifth\nA // last\n;",
            "type T = // first\n[A] A -> A;",
            "type T = // first\nmodule_path.A;",
        ] {
            for source in [
                format!("mod x;\n/// Alias documentation.\n{declaration}"),
                format!("mod x; rec {{\n/// Alias documentation.\n{declaration}\n}}"),
            ] {
                let printed = roundtrip(&source);
                assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&printed),
                    "type structure changed: {source}\n{printed}"
                );
            }
        }
        assert!(parse("mod x; type T[,,*F,, A,,] = F[A];").is_err());
        assert_eq!(
            roundtrip("mod x; type T[*F][A] = (F[A]);"),
            roundtrip("mod x; type T[*F][A] = F[A];")
        );
    }

    #[test]
    #[cfg(feature = "surface")]
    fn signature_alias_header_comments_survive() {
        let source = "sig app v(1); v(1) { nonbreaking { add { mod api {
/// Alias documentation.
pub // first
type // second
T[// third
A // fourth
] // fifth
= // last
A;
} } } }";
        let parsed = crate::pass::parser::parse_signature_file(source, None).unwrap();
        let printed = crate::sig::emit_signature_file(&parsed);
        let mut previous = 0;
        for comment in [
            "/// Alias documentation.",
            "// first",
            "// second",
            "// third",
            "// fourth",
            "// fifth",
            "// last",
        ] {
            assert_eq!(printed.matches(comment).count(), 1, "{printed}");
            let offset = printed.find(comment).unwrap();
            assert!(offset >= previous, "{printed}");
            previous = offset;
        }
        let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
        assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
        let before = crate::sig::replay(&parsed).unwrap();
        let after = crate::sig::replay(&reparsed).unwrap();
        assert_eq!(
            crate::sig::contract_digest(&before.current),
            crate::sig::contract_digest(&after.current)
        );
    }

    #[test]
    #[cfg(feature = "surface")]
    fn grouped_signature_return_comments_survive() {
        for declaration in ["pub fn exported", "host fn supplied"] {
            let source = format!(
                "sig app v(1); v(1) {{ nonbreaking {{ add {{ mod api {{ {declaration}() -> (// before result\n. // after result\n); }} }} }} }}"
            );
            let parsed = crate::pass::parser::parse_signature_file(&source, None).unwrap();
            let printed = crate::sig::emit_signature_file(&parsed);
            for comment in ["// before result", "// after result"] {
                assert_eq!(printed.matches(comment).count(), 1, "{printed}");
            }
            assert!(printed.contains("->"), "{printed}");
            let reparsed = crate::pass::parser::parse_signature_file(&printed, None).unwrap();
            assert_eq!(printed, crate::sig::emit_signature_file(&reparsed));
            let before = crate::sig::replay(&parsed).unwrap();
            let after = crate::sig::replay(&reparsed).unwrap();
            assert_eq!(
                crate::sig::contract_digest(&before.current),
                crate::sig::contract_digest(&after.current)
            );
        }
    }

    #[test]
    fn type_chain_later_operand_comments_keep_separator_ownership() {
        for op in ["&", "|"] {
            let source = format!("mod x; type T = A // next operand\n{op} B;");
            let printed = roundtrip(&source);
            assert_eq!(
                printed,
                format!("mod x;\n\ntype T =\n  {op} A\n  // next operand\n  {op} B\n  ;\n")
            );
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));

            for ty in [
                format!("(// first operand\nA) {op} B"),
                format!("A {op} (// second operand\nB // operand end\n)"),
            ] {
                let source = format!("mod x; type T = {ty};");
                let printed = roundtrip(&source);
                let comments = |text: &str| {
                    text.lines()
                        .filter_map(|line| {
                            line.split_once("//")
                                .map(|(_, comment)| comment.trim().to_owned())
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                assert_eq!(parsed_structure(&source), parsed_structure(&printed));
            }
        }
    }

    #[test]
    fn type_chain_separator_comments_survive_in_source_order() {
        fn without_comments(source: &str) -> String {
            source
                .lines()
                .map(|line| line.split("//").next().unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        }
        for op in ["&", "|"] {
            for leading in ["", op] {
                for chain in [
                    format!("{leading}A // first\n{op} B // second\n{op} C"),
                    format!(
                        "{leading}A // first\n{op} Box[ // second\nB // third\n] // fourth\n{op} C"
                    ),
                    format!("{leading}A // first\n{op} (B // second\n{op} C)"),
                ] {
                    for source in [
                        format!("mod x; type T = {chain};"),
                        format!("mod x; fn f() -> ({chain}) -> D {{ () }}"),
                        format!("mod x; fn f(x: Container[({chain}) -> D]) {{ () }}"),
                    ] {
                        let printed = roundtrip(&source);
                        let comments = |text: &str| {
                            text.lines()
                                .filter_map(|line| {
                                    line.split_once("//")
                                        .map(|(_, body)| body.trim().to_owned())
                                })
                                .collect::<Vec<_>>()
                        };
                        assert_eq!(comments(&printed), comments(&source), "{source}\n{printed}");
                        assert_eq!(
                            roundtrip(&without_comments(&printed)),
                            roundtrip(&without_comments(&source)),
                            "formatting comments changed type structure: {source}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn type_function_param_chain_no_extra_parens() {
        // The function-arrow's parameter list already supplies
        // `(...)` for its single chain parameter; no extra parens
        // around the chain itself.
        let pp = roundtrip("mod x; type T = (A & B) -> C;");
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
        let src = "mod x; type Event = Click_event_kind_one_pretty_long_name_here | Hover_event_kind_two_also_pretty_long | Focus_event_kind_three_lengthy | Drag_event_kind_four_yet_more;";
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
        // A nominal payload that's a wide `|` chain breaks to the
        // leading-operator layout: `:` ends the head's line, the
        // chain items at +2, `{` lands on its own line at the same
        // indent before the member-list block.
        let src = "mod x; type { Token : Int_lit_token_a_pretty_long_name | String_lit_token_b_also_pretty_long_indeed | Bool_lit_token_c_lengthy_too | Float_lit_token_d_yet_more { pub mk; pub get; }; };";
        let pp = roundtrip(src);
        assert_eq!(parsed_structure(src), parsed_structure(&pp));
        assert!(
            pp.contains("Token:\n    | Int_lit_token_a_pretty_long_name"),
            "expected leading-op break for first chain item; got:\n{pp}"
        );
        assert!(
            pp.contains("\n    { pub mk; pub get }\n}"),
            "expected `{{` on its own line at +2; got:\n{pp}"
        );
    }

    #[test]
    fn nominal_payload_chain_layout_preserves_members_and_separators() {
        for op in ["&", "|"] {
            let payload = format!(
                "First_payload_type_with_a_deliberately_long_name {op} Second_payload_type_with_another_long_name {op} Third_payload_type_with_a_long_name"
            );
            for members in ["", " { pub mk; get; }"] {
                for nonfinal in [false, true] {
                    let following = if nonfinal { "; Next: ." } else { "" };
                    let source =
                        format!("mod x; type {{ Token: {payload}{members}{following}; }};");
                    let printed = roundtrip(&source);
                    assert_eq!(parsed_structure(&source), parsed_structure(&printed));
                    assert!(
                        printed.contains(&format!("  Token:\n    {op} First_payload")),
                        "{printed}"
                    );
                    assert!(
                        printed.contains(&format!("\n    {op} Second_payload")),
                        "{printed}"
                    );
                    assert!(
                        printed.contains(&format!("\n    {op} Third_payload")),
                        "{printed}"
                    );
                    assert_eq!(
                        printed.matches("mk").count(),
                        usize::from(!members.is_empty())
                    );
                    assert_eq!(
                        printed.matches("get").count(),
                        usize::from(!members.is_empty())
                    );
                    assert_eq!(
                        printed.matches(';').count(),
                        1 + usize::from(!members.is_empty()) + usize::from(nonfinal),
                        "{printed}"
                    );
                    if members.is_empty() {
                        let ending = if nonfinal {
                            "\n    ;\n  Next: .\n}\n"
                        } else {
                            "_long_name\n}\n"
                        };
                        assert!(printed.ends_with(ending), "{printed}");
                    } else {
                        let suffix = if nonfinal {
                            " { pub mk; get };\n  Next: ."
                        } else {
                            " { pub mk; get }\n}"
                        };
                        assert!(printed.contains(&format!("\n   {suffix}")), "{printed}");
                    }
                }
            }
        }
    }

    #[test]
    fn nominal_payload_chain_comments_preserve_order_and_structure() {
        let source = "mod x; type { // entry
Token: First_payload_type_with_a_deliberately_long_name // first payload
| Second_payload_type_with_another_long_name // last payload
{ // before mk
pub mk // after mk
; // before get
get // after get
} // before separator
; // next entry
Next: . // after next
};";
        let printed = roundtrip(source);
        assert_eq!(parsed_structure(source), parsed_structure(&printed));
        let comments = |text: &str| {
            text.lines()
                .filter_map(|line| {
                    line.split_once("//")
                        .map(|(_, body)| body.trim().to_owned())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(comments(source), comments(&printed), "{printed}");
        assert_eq!(printed.matches(';').count(), 3, "{printed}");
        assert!(printed.contains("  Token:\n"), "{printed}");
        assert!(printed.contains("\n    {\n"), "{printed}");
    }

    #[test]
    fn nominal_payload_member_boundary_comments_survive_each_declaration_shape() {
        for payload in [".", "A & B", "[A] A -> A", "Box[.]"] {
            for declaration in [
                format!("type {{ Token: {payload} // payload boundary\n{{ mk; get }} }}"),
                format!("type {{ Token: {payload} // payload boundary\n{{ mk; get }}; Other: . }}"),
                format!("type {{ Other: .; Token: {payload} // payload boundary\n{{ mk; get }} }}"),
                format!(
                    "type Choice = {{ Token: {payload} // payload boundary\n{{ mk; get }} }} | {{ Other: . }};"
                ),
            ] {
                let source = format!("mod x; {declaration}");
                let printed = roundtrip(&source);
                assert_eq!(parsed_structure(&source), parsed_structure(&printed));
                assert_eq!(
                    printed.matches("// payload boundary").count(),
                    1,
                    "{printed}"
                );
                let boundary = printed.find("// payload boundary").unwrap();
                let members = printed.find("{ mk; get }").expect("explicit member body");
                assert!(boundary < members, "{printed}");
            }
        }
    }

    #[test]
    fn named_nominal_rhs_wide_payloads_preserve_compact_groups() {
        for op in ["&", "|"] {
            let source = format!(
                "mod x; type Choice[A] = {{ First: First_payload_type_with_a_deliberately_long_name {op} Second_payload_type_with_another_long_name {op} Third_payload_type_with_a_long_name {{ mk; pub get; }}; Another: A; }} | (Existing & {{ Last: .; }});"
            );
            let printed = roundtrip(&source);
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
            assert!(
                printed.contains(&format!("First:\n      {op} First_payload")),
                "{printed}"
            );
            assert_eq!(printed.matches("type Choice[A]").count(), 1, "{printed}");
            assert_eq!(printed.matches("mk;").count(), 1, "{printed}");
            assert_eq!(printed.matches("pub get").count(), 1, "{printed}");
            assert_eq!(printed.matches(';').count(), 4, "{printed}");
            assert!(printed.contains("(Existing & { Last: . })"), "{printed}");
        }
    }

    #[test]
    fn fn_def_wide_chain_return_breaks() {
        let src = "mod x; fn f() -> Long_event_kind_one_pretty_long_name | Hover_event_kind_two_pretty_long | Focus_event_kind_three_lengthy | Drag_event_kind_four_more_chars { f() }";
        let pp = roundtrip(src);
        assert!(
            pp.contains("fn f() ->\n  | Long_event_kind_one_pretty_long_name"),
            "expected leading-op break for fn return; got:\n{pp}"
        );
    }

    #[test]
    fn rec_call_annotations_print_in_canonical_order() {
        let pp = roundtrip("mod x; rec(loop) fn f() -> . { recur(escape, cont, poly) f() }");
        assert!(
            pp.contains("recur(poly, escape) f()"),
            "expected canonical rec-call annotation order; got:\n{pp}"
        );
    }

    #[test]
    fn fn_type_wide_chain_return_breaks() {
        // A function-type return that's a wide chain breaks to
        // the leading-operator multi-line layout inside the
        // function-type doc.
        let src = "mod x; type F = A -> (Long_event_kind_one_pretty_long_name | Hover_event_kind_two_pretty_long | Focus_event_kind_three_lengthy | Drag_event_kind_four_more_chars);";
        let pp = roundtrip(src);
        assert!(
            pp.contains("A -> (\n  | Long_event_kind_one_pretty_long_name"),
            "expected leading-op break for fn-type return; got:\n{pp}"
        );
    }

    #[test]
    fn newtype_short_payload_stays_inline() {
        let pp = roundtrip("mod x; type { Pair[A][B] : (A & B) { pub mk; pub get; }; };");
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
        let pp = roundtrip("mod x; type T = A & B;");
        assert!(
            pp.contains("type T = A & B;"),
            "expected inline chain; got:\n{pp}"
        );
        // No multi-line break for the short chain.
        assert!(!pp.contains("\n  & "), "did not expect break; got:\n{pp}");
    }

    #[test]
    fn arrow_result_chains_keep_load_bearing_parentheses() {
        for operator in ["&", "|"] {
            let pp = roundtrip(&format!("mod x; type T = A -> (B {operator} C);"));
            assert!(pp.contains(&format!("A -> (B {operator} C)")), "{pp}");
            let signatures = roundtrip(&format!(
                "mod x; fn f() -> B {operator} C {{ () }} fn g() {{ .() -> B {operator} C {{ () }} }} fn h() -> A -> (B {operator} C) {{ () }}"
            ));
            assert!(
                signatures.contains(&format!("fn f() -> B {operator} C")),
                "{signatures}"
            );
            assert!(
                signatures.contains(&format!(".() -> B {operator} C")),
                "{signatures}"
            );
            assert!(
                signatures.contains(&format!("fn h() -> A -> (B {operator} C)")),
                "{signatures}"
            );
            let wide = format!(
                "mod x; type T = A -> (Very_long_first_result_type {operator} Very_long_second_result_type {operator} Very_long_third_result_type);"
            );
            roundtrip(&wide);
            let commented = format!("mod x; type T = A -> (B {operator} // result\n C);");
            let formatted = roundtrip(&commented);
            assert_eq!(formatted.matches("// result").count(), 1, "{formatted}");
        }
    }

    // ---- value expressions ---------------------------------------------

    #[test]
    fn fn_round_trips_without_annotations() {
        roundtrip("mod x; fn f() -> . { .(x) { x }(0) }");
    }

    #[test]
    fn let_round_trips_without_annotation() {
        roundtrip("mod x; fn f() -> . { let x = (); x }");
    }

    #[test]
    fn dotted_path_value_position() {
        roundtrip("mod x; fn f() -> . { Foo.mk_foo() }");
    }

    #[test]
    fn call_with_mixed_args() {
        roundtrip(r#"mod x; fn f() -> . { id[String]("hi") }"#);
    }

    #[test]
    fn block_unit_prefix_keeps_required_grouping() {
        let output = roundtrip("mod x; fn f() -> . { do({}) { final_value } }");
        assert!(
            output.contains("do(()) {"),
            "unit prefix lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn block_prefix_comments_keep_required_grouping() {
        for prefix in [
            "factory // boundary\n(())",
            "(// boundary\nfactory)(())",
            "factory(()) // boundary\n(())",
            "factory // boundary\n(()).>select",
            "(// boundary\nreceiver).>select",
            "(// boundary\nreceiver).>select.>finish",
            "(// boundary\nreceiver).>>select",
            "(// boundary\nreceiver).>select.>>finish",
            "factory // boundary\n(()) + value",
        ] {
            let source = format!(
                "mod x; op _ + _ {{ impl add; }}; fn f() {{ choose ({prefix}) {{ // body
() }} }}"
            );
            let output = roundtrip(&source);
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&output),
                "{source}"
            );
            let comments = output
                .lines()
                .filter_map(|line| line.find("//").map(|start| line[start..].trim()))
                .collect::<Vec<_>>();
            assert_eq!(comments, ["// boundary", "// body"], "{output}");
        }
        for prefix in [
            "receiver // boundary\n.>select.>finish",
            "receiver // boundary\n.>>select.>finish",
            "select.<(receiver // boundary\n)",
            "select.<<(receiver // boundary\n)",
            "value + factory(// boundary\n())",
        ] {
            let source = format!(
                "mod x; op _ + _ {{ impl add; }}; fn f() {{ choose({prefix}) {{ // body
() }} }}"
            );
            let output = roundtrip(&source);
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&output),
                "{source}"
            );
            assert!(output.contains("choose("), "{output}");
            let comments = output
                .lines()
                .filter_map(|line| line.find("//").map(|start| line[start..].trim()))
                .collect::<Vec<_>>();
            assert_eq!(comments, ["// boundary", "// body"], "{output}");
        }
    }

    #[test]
    fn operator_operand_comments_preserve_boundaries_and_structure() {
        for expression in [
            "left + (// first\nright // second\n)",
            "(// first\nleft // second\n) + right",
            "left :: (// first\nmiddle :: right // second\n)",
            "(// first\nleft ++ middle // second\n) ++ right",
            "left + (// first\nmiddle :: right // second\n)",
            "(// first\nleft // second\n) %%",
            "left ?? (// first\nmiddle ?? right // second\n)",
            "left @@ (// first\nmiddle :: right // second\n)",
            "choose ((// first\nleft) + right) { // second\n() }",
        ] {
            let source = format!(
                "mod x; op _ + _ {{ impl add; }}; op _ :: __ {{ impl cons; }}; op __ ++ _ {{ impl append; }}; op _ %% {{ impl finish; }}; op _ ?? ___ {{ impl select; }}; op _ @@ ___ {{ impl pick; }}; fn f() {{ {expression} }}"
            );
            let output = roundtrip(&source);
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&output),
                "{source}"
            );
            let comments = output
                .lines()
                .filter_map(|line| line.find("//").map(|start| line[start..].trim()))
                .collect::<Vec<_>>();
            assert_eq!(comments, ["// first", "// second"], "{output}");
        }
    }

    #[test]
    fn bare_literal_callee_keeps_required_grouping() {
        let output = roundtrip("mod x; fn f() -> . { (\"literal\")(argument) }");
        assert!(
            output.contains("(\"literal\")(argument)"),
            "bare literal callee lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn composite_callee_keeps_required_grouping() {
        let output = roundtrip(
            "mod x; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; fn operator_call() -> . { (- form_value)(context_arg) } fn splice_call() -> . { (context_callee.<form_receiver)(context_arg) }",
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
    fn recursive_callee_keeps_ordinary_result_stages() {
        for expression in [
            "(recur(cont) step(value))(other)",
            "(recur(poly, cont) step[A](value))[B](other)",
            "(recur(escape) step())(())",
        ] {
            let source = format!("mod x; fn subject() {{ {expression} }}");
            let output = roundtrip(&source);
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&output),
                "recursive header and ordinary result stages must stay separate: {output}"
            );
            assert!(output.contains(expression), "{output}");
        }
    }

    #[test]
    fn left_splice_boolean_argument_keeps_required_grouping() {
        let output = roundtrip("mod x; fn f() -> . { context_callee.<(.t) }");
        assert!(
            output.contains("context_callee.<(.t)"),
            "boolean left-splice argument lost its disambiguating parentheses:\n{output}"
        );
    }

    #[test]
    fn literal_annotation_comments_keep_order_and_structure() {
        for literal in ["915", "9.15", "\"value\"", ".t", ".f"] {
            for annotation in [
                "// 1\n[ // 2\nTarget // 3\n]",
                "// 1\n[ // 2\n(( // 3\nTarget // 4\n) // 5\n) // 6\n]",
            ] {
                let source = format!("mod x; fn f() {{ ({literal} {annotation}).>>id }}");
                let printed = roundtrip(&source);
                let comments = |text: &str| {
                    text.lines()
                        .filter_map(|line| {
                            line.find("//").map(|start| line[start..].trim().to_owned())
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(comments(&source), comments(&printed), "{source}\n{printed}");
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&printed),
                    "{source}"
                );
            }
        }
        let source = "mod x; fn f() { 915 // before\n[I16] }";
        assert_eq!(
            roundtrip(source),
            "mod x;\n\nfn f() {\n  915[\n    // before\n    I16\n  ]\n}\n"
        );
        let source = "mod x; fn f() { 915 // before\n[ // inside\nI16] }";
        assert_eq!(
            roundtrip(source),
            "mod x;\n\nfn f() {\n  915[\n    // before\n\n    // inside\n    I16\n  ]\n}\n"
        );
    }

    #[test]
    fn left_splice_called_dot_forms_keep_required_grouping() {
        for splice in [".<", ".<<"] {
            for argument in [
                ".() { () }()",
                ".[A](x: A) { x }((), ())",
                ".x. { x1 }(())",
                ".t[Bool](())",
                ".() { .() { () } }()()",
                "(.() { () } // callee\n)()",
            ] {
                let source = format!("mod x; fn f() {{ fit(id{splice}({argument})) }}");
                let printed = roundtrip(&source);
                assert_eq!(
                    parsed_structure(&source),
                    parsed_structure(&printed),
                    "{source}\n{printed}"
                );
            }
            let source = format!("mod x; fn f() {{ id{splice}(.() {{ () }}()) }}");
            assert_eq!(
                roundtrip(&source),
                format!("mod x;\n\nfn f() {{\n  id{splice}(.() {{ () }}())\n}}\n")
            );
            let source = format!("mod x; fn f() {{ id{splice}(g()()) }}");
            assert_eq!(
                roundtrip(&source),
                format!("mod x;\n\nfn f() {{ id{splice}g()() }}\n")
            );
        }
    }

    #[test]
    fn operator_right_splice_receiver_keeps_required_grouping() {
        let output = roundtrip(
            "mod x; fn prefix_impl(value: Formtype) -> Formtype { value } op - __ { impl prefix_impl; }; op_var [% %] { foldr form_push form_empty; }; fn normal_splice() -> . { (- form_value).>context_callee } fn variadic_splice() -> . { ([% form_left, form_right %]).>context_callee }",
        );
        assert!(
            output.contains("(- form_value).>context_callee"),
            "normal-operator receiver lost its disambiguating parentheses:\n{output}"
        );
        assert!(
            output.contains("[% form_left, form_right %].>context_callee"),
            "structural variadic receiver retained unnecessary parentheses:\n{output}"
        );
    }

    #[test]
    fn literals_int_float_bool_string() {
        roundtrip(
            r#"mod x; fn f() -> . { let s = "hi"; let n = 42; let f = 3.14; let b = .t; () }"#,
        );
    }

    // ---- string-escape round-trip --------------------------------------

    #[test]
    fn string_escapes_round_trip() {
        roundtrip(r#"mod x; fn f() -> . { let s = "a\nb\t\"c\"\\dé中"; () }"#);
    }

    #[test]
    fn string_unicode_escape_emits_lowercase_hex() {
        let module = parse("mod x; fn f() -> . { let s = \"\u{0001}\"; () }").unwrap();
        let out = pretty_module(&module);
        assert!(
            out.contains("\\u0001"),
            "expected lowercase hex escape `\\u0001`; got: {out}"
        );
    }

    #[test]
    fn float_exponent_canonicalises_to_lowercase_e_and_drops_plus() {
        let module = parse("mod x; fn f() -> . { let a = 1.5E+10; let b = 1.5e-10; () }").unwrap();
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
        let src = "mod x;

// docstring for `id`
fn id() -> . { () }
";
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
        let src = "mod x;

// pull in the unit type
import a/b {type T}
";
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
        let src = "mod x;

// First paragraph.

// Second paragraph.
fn id() -> . { () }
";
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
    fn hoisted_header_comments_keep_body_paragraph_boundary() {
        for (source, expected_body) in [
            (
                "fn f() -> // header\n. { // body\n() }",
                "  // header\n\n  // body\n  ()\n",
            ),
            ("fn f() -> // header\n. { () }", "  // header\n  ()\n"),
            (
                "fn f() -> . { // body\n// continuation\n() }",
                "  // body\n  // continuation\n  ()\n",
            ),
            (
                "fn f() -> // header\n. // before body\n{ // body\n() }",
                "  // header\n\n  // before body\n\n  // body\n  ()\n",
            ),
            (
                "fn f() -> // header\n. { // body\n\n\n// paragraph\n() }",
                "  // header\n\n  // body\n\n  // paragraph\n  ()\n",
            ),
            (
                "fn f() -> // header\n. { // binding\nlet x = (); x }",
                "  // header\n\n  // binding\n  let x = ();\n  x\n",
            ),
        ] {
            let source = format!("mod x;\n/// Attached documentation.\n{source}\n");
            let out = roundtrip(&source);
            assert_eq!(
                out,
                format!("mod x;\n\n/// Attached documentation.\nfn f() {{\n{expected_body}}}\n"),
            );
        }
    }

    #[test]
    fn source_container_comments_have_stable_owners() {
        for (source, expected) in [
            (
                "fn f() { g( // close\n) }",
                "fn f() {\n  g(\n  // close\n  )\n}",
            ),
            (
                "fn f() { ( // unit\n) }",
                "fn f() {\n  (\n  // unit\n  )\n}",
            ),
            (
                "fn f() { { // unit\n} }",
                "fn f() {\n  (\n  // unit\n  )\n}",
            ),
            (
                "fn f() { ( // first\n, // second\n, // last\n) }",
                "fn f() {\n  (\n  // first\n\n  // second\n\n  // last\n  )\n}",
            ),
            (
                "fn f() { g( // first\n, // second\n, // last\n) }",
                "fn f() {\n  g(\n  // first\n\n  // second\n\n  // last\n  )\n}",
            ),
            (
                "fn f() { g( // first\n())( // second\n) }",
                "fn f() {\n  g(\n  // first\n  , ()\n  )(\n  // second\n  )\n}",
            ),
            (
                "fn f() { g(h( // inner\n) // outer\n) // block\n}",
                "fn f() {\n  g(\n  , h(\n    // inner\n    )\n  // outer\n  )\n  // block\n}",
            ),
            (
                "fn f( // first\n)( // second\nx: .) { x }",
                "fn f(\n// first\n)(\n// second\n, x: .\n) { x }",
            ),
            (
                "fn f(x // A\n: Pair[ // B\n.]) { x }",
                "fn f(\n// A\n, x: Pair[\n    // B\n    .\n  ]\n) { x }",
            ),
            (
                "fn f( // leading\nx // colon\n: // type\n. // trailing\n) { x }",
                "fn f(\n// leading\n\n// colon\n\n// type\n, x: .\n// trailing\n) { x }",
            ),
            (
                "fn f() // arrow\n-> // return type\nPair[ // first type\n.][ // second type\n.] // body opener\n{ // body note\n() }",
                "fn f() -> (\n  // arrow\n\n  // return type\n  Pair[\n    // first type\n    .\n  ][\n    // second type\n    .\n  ]\n  ) {\n  // body opener\n\n  // body note\n  ()\n}",
            ),
            (
                "fn f() { .() // arrow\n-> // return type\n. // body opener\n{ // body note\n() } }",
                "fn f() {\n  .() -> . {\n    // arrow\n\n    // return type\n\n    // body opener\n\n    // body note\n    ()\n  }\n}",
            ),
        ] {
            let printed = roundtrip(&format!("mod x; {source}"));
            assert_eq!(
                printed,
                format!("mod x;\n\n{expected}\n"),
                "source: {source}"
            );
        }
    }

    #[test]
    fn collapsed_group_comments_keep_order_and_suffix_boundaries() {
        for (source, expected) in [
            (
                "fn f(){ let a=(); ((a // inner\n) // outer\n) // block\n}",
                "fn f() {\n  let a = ();\n  a\n  // inner\n\n  // outer\n\n  // block\n}",
            ),
            (
                "fn f(){ ((a // inner\n) // outer\n) // block\n}",
                "fn f() {\n  a\n  // inner\n\n  // outer\n\n  // block\n}",
            ),
            (
                "fn f(){ (a // inner\n\n) // outer\n}",
                "fn f() {\n  a\n  // inner\n\n  // outer\n}",
            ),
            (
                "fn f(){ (a // inner\n)\n\n// outer\n}",
                "fn f() {\n  a\n  // inner\n\n  // outer\n}",
            ),
            (
                "fn f(){ g((x // inner\n), y) }",
                "fn f() {\n  g(\n  , x\n  // inner\n  , y\n  )\n}",
            ),
            (
                "fn f(){ (g() // callee\n)() }",
                "fn f() {\n  (\n    g()\n    // callee\n    )()\n}",
            ),
            (
                "fn f(){ (g() // inner\n) // step\n.>use }",
                "fn f() {\n  g()\n    // inner\n\n    // step\n    .>use\n}",
            ),
            (
                "fn f(){ let a=(g() // value\n); a }",
                "fn f() {\n  let a =\n    (\n      g()\n      // value\n      );\n  a\n}",
            ),
        ] {
            let printed = roundtrip(&format!("mod x; {source}"));
            assert_eq!(
                printed,
                format!("mod x;\n\n{expected}\n"),
                "source: {source}"
            );
        }
    }

    #[test]
    fn relocated_comment_blocks_preserve_paragraphs_and_documentation() {
        for (source, expected) in [
            (
                include_str!(
                    "../../test-data/goldens/00_success/fmt_relocated_comment_blocks/workdir/main.kio"
                ),
                include_str!(
                    "../../test-data/goldens/00_success/fmt_relocated_comment_blocks/expected.stdout"
                ),
            ),
            (
                include_str!(
                    "../../test-data/goldens/00_success/fmt_comment_paragraphs/workdir/main.kio"
                ),
                include_str!(
                    "../../test-data/goldens/00_success/fmt_comment_paragraphs/expected.stdout"
                ),
            ),
        ] {
            assert_eq!(roundtrip(source), expected);
            assert_eq!(parsed_structure(source), parsed_structure(expected));
        }

        // Distinct source gaps become distinct blocks; multiple lines already
        // sharing one gap remain together, including an empty comment line.
        for declaration in [
            "type // first\nT = // second\n.;",
            "host // first\nfn // second\nf();",
            "fn f() { (g // first\n) // second\n() }",
            "fn f() { m // first\n. // second\nfield() }",
            "type T = A // first\n-> // second\nB;",
            "import // first\nm // second\nas alias;",
            "op _ + // first\n_ { impl // second\nadd }",
        ] {
            let source = format!("mod m; {declaration}");
            let printed = roundtrip(&source);
            let lines: Vec<_> = printed.lines().map(str::trim).collect();
            let first = lines.iter().position(|line| *line == "// first").unwrap();
            assert_eq!(
                &lines[first..first + 3],
                &["// first", "", "// second"],
                "{printed}"
            );
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
        }
        for comments in [
            "// first\n// second\n",
            "// first\n//\n// second\n",
            "// first\n\n// second\n",
        ] {
            let source = format!("mod m; type T = {comments}.;");
            let printed = roundtrip(&source);
            let normalized = printed
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(normalized.contains(comments.trim_end()), "{printed}");
            assert_eq!(parsed_structure(&source), parsed_structure(&printed));
        }
        assert_eq!(
            roundtrip("mod m; fn f() { // only block\n() }"),
            "mod m;\n\nfn f() {\n  // only block\n  ()\n}\n"
        );
    }

    #[cfg(feature = "cli")]
    #[test]
    fn redundant_file_terminators_preserve_comment_blocks() {
        for (filename, source) in [
            ("main.kio", "mod main; fn f() { () }"),
            ("app.pkg.kio", "pkg app; build { cache (); }"),
            ("lib.dep.kio", "dep lib; source { path \".\"; }"),
            (
                "lib.lock.kio",
                "lock lib; resolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; }",
            ),
            (
                "app.sig.kio",
                "sig app v(1); v(1) { nonbreaking { add { mod api { pub fn f(); } } } }",
            ),
        ] {
            let source = format!("{source} // before terminator\n; // after terminator\n");
            let printed = crate::cmd::fmt::format_source(filename, &source).unwrap();
            assert!(
                printed.ends_with("// before terminator\n\n// after terminator\n"),
                "{filename}\n{printed}"
            );
            assert_eq!(
                crate::cmd::fmt::format_source(filename, &printed).unwrap(),
                printed,
                "{filename}"
            );
        }
    }

    #[test]
    fn expression_suffix_comments_preserve_call_stages_and_statement_ownership() {
        for source in [
            "fn f(){ io.print // 1\n(label) }",
            "fn f(){ bounce(()) // 1\n() }",
            "fn f(){ use.<bounce // 1\n(()) }",
            "fn f(){ g // 1\n(a).<x }",
            "fn f(){ g // 1\n(a).<<x }",
            "fn f(){ ( // 1\ng // 2\n) // 3\n( // 4\na // 5\n).<x }",
            "fn f(){ ( // 1\ng // 2\n) // 3\n( // 4\na // 5\n).<<x }",
            "fn f(){ (bounce(()) // 1\n) // 2\n() }",
            "fn f(){ (bounce(()) // 1\n\n) // 2\n() }",
            "fn f(){ g() // 1\n; // 2\nh() // 3\n; // 4\n() }",
            "fn f(){ g() // 1\n; // 2\n; // 3\n}",
            "fn f(){ let x = g() // 1\n; // 2\nx }",
            "fn f(){ let _ = g() // 1\n; () }",
            "fn f(){ let .(x: .) = g() // 1\n; x }",
            "fn f(){ let .(x, y) = g() // 1\n; x }",
            "fn f(){ let .(<Hidden> payload) = g() // 1\n; payload }",
            "fn f(){ let .x. = g() // 1\n; x1 }",
        ] {
            let source = format!("mod x; {source}");
            let printed = roundtrip(&source);
            let comments = |text: &str| {
                text.lines()
                    .filter_map(|line| line.find("//").map(|start| line[start..].trim().to_owned()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&printed), comments(&source), "{source}\n{printed}");
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&printed),
                "{source}"
            );
        }
    }

    #[test]
    fn call_head_comments_preserve_arguments_and_recursive_modes() {
        for source in [
            "fn f(){ x.>g[A] // 1\n(y) }",
            "fn f(){ x.>>g // 1\n( // 2\ny // 3\n) }",
            "fn f(){ x.>g // 1\n(y) }",
            "fn f(){ x.>>g // 1\n( // 2\ny // 3\n) }",
            "fn f(){ g // 1\n(y) }",
            "fn f(){ g // 1\n( // 2\n) }",
            "fn f(){ g // 1\n(y).<x }",
            "fn f(){ g // 1\n(y).<<x }",
            "fn f(){ recur g // 1\n(y) }",
            "fn f(){ recur g // 1\n( // 2\n) }",
            "fn f(){ recur // 1\ng(y) }",
            "fn f(){ recur // 1\n(poly // 2\n) // 3\ng // 4\n() }",
            "fn f(){ recur( // 1\npoly // 2\n, // 3\ncont // 4\n) // 5\ng(y) }",
            "fn f(){ g.< // 1\n(x) }",
            "fn f(){ g.<< // 1\n(x) }",
            "fn f(){ g.< // 1\n(x) }",
            "fn f(){ g.<< // 1\n(x) }",
            "fn f(){ g // 1\n.< // 2\n(x) }",
            "fn f(){ g(y) // 1\n.<< // 2\n(x) }",
            "fn f(){ g // 1\n.< // 2\n(x) }",
            "fn f(){ g(y) // 1\n.<< // 2\n(x) }",
            "fn f(){ g.< // 1\n( // 2\nh // 3\n)(x) }",
            "fn f(){ g // 1\n( // 2\nx // 3\n) }",
            "fn f(){ g // 1\n(x) { () } }",
        ] {
            let source = format!("mod x; {source}");
            let printed = roundtrip(&source);
            let comments = |text: &str| {
                text.lines()
                    .filter_map(|line| line.find("//").map(|start| line[start..].trim().to_owned()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(comments(&printed), comments(&source), "{source}\n{printed}");
            assert_eq!(
                parsed_structure(&source),
                parsed_structure(&printed),
                "{source}"
            );
        }
        assert_eq!(
            roundtrip("mod x; fn f(){ x.>g // before\n( // inside\ny) }"),
            "mod x;\n\nfn f() {\n  x\n    .>g(\n    // before\n\n    // inside\n    , y\n    )\n}\n"
        );
        assert_eq!(
            roundtrip("mod x; fn f(){ g // before\n.< // after\n(x) }"),
            "mod x;\n\nfn f() {\n  g.<(\n    // before\n\n    // after\n    x\n    )\n}\n"
        );
        for source in [
            "x.>g // comment\n()",
            "x.>>g // comment\n()",
            "g // comment\n().<x",
        ] {
            assert!(parse(&format!("mod x; fn f(){{ {source} }}")).is_err());
        }
    }

    #[test]
    fn delimited_comment_owners_cover_call_consumers_and_type_groups() {
        for source in [
            "fn f() { recur f( // 1\n) }",
            "fn f() { e( // 1\n) }",
            "fn f() { x.>g(y // 1\n) }",
            "fn f() { x.>e(y // 1\n) }",
            "fn f() { g(y // 1\n).<x }",
            "fn f() { e(y // 1\n).<x }",
            "fn f() { e( // 1\n) { () } }",
            "fn f() { e((x // 1\n), y // 2\n) { () } }",
            "fn f[ // 1\nA // 2\n][ // 3\nB // 4\n]() { () }",
            "fn f() // 1\n[A] // 2\n( // 3\nx: A) { x }",
            "fn f() { .( // 1\nx // 2\n: // 3\n. // 4\n) { x } }",
            "fn f(){ ( // 1\ng() // 2\n)() }",
            "fn f(){ ( // 1\n)( // 2\n) }",
            "fn f(){ (g() // 1\n); () }",
            "fn f(){ let .(a, b) = (g() // 1\n); (a // 2\n) // 3\n}",
            "fn f(){ g((x // 1\n), // 2\ny // 3\n) // 4\n}",
            "fn f(x // 1\n: (a: .)) { a }",
            "rec(loop) fn f( // 1\n) // 2\n-> // 3\n. { () }",
        ] {
            let printed = roundtrip(&format!("mod x; {source}"));
            let comments = |text: &str| {
                text.lines()
                    .filter_map(|line| line.find("//").map(|start| line[start..].trim().to_owned()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                comments(&printed),
                comments(source),
                "source: {source}\n{printed}"
            );
        }
    }

    #[test]
    fn comment_run_contexts_keep_canonical_output() {
        let comments = "//\n///\n///\n/// raw documentation\n//\n//\n\n// paragraph\n";
        let cases = [
            (
                format!("mod m;\n{comments}fn f() {{ () }}\n"),
                "mod m;\n\n//\n///\n///\n/// raw documentation\n//\n\n// paragraph\nfn f() { () }\n",
            ),
            (
                format!("mod m;\nfn f() {{\n{comments}() }}\n"),
                "mod m;\n\nfn f() {\n  //\n\n  // paragraph\n  ()\n}\n",
            ),
            (
                format!("mod m;\nfn f() {{ ()\n{comments}}}\n"),
                "mod m;\n\nfn f() {\n  ()\n  //\n\n  // paragraph\n}\n",
            ),
            (
                "mod m;\n//\n//\n\n// paragraph\nimport z as z;\n// first import\nimport a as a;\n"
                    .to_owned(),
                "mod m;\n\n// first import\nimport a as a;\n//\n\n// paragraph\nimport z as z;\n",
            ),
            (
                "mod m;\nfn f() {\n///\n/// ignored\n() }\n".to_owned(),
                "mod m;\n\nfn f() { () }\n",
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(roundtrip(&source), expected, "source:\n{source}");
        }
    }

    #[test]
    fn empty_comment_lines_collapse() {
        let src = "mod x;

//
//
//
fn id() -> . { () }
";
        let m = parse(src).unwrap();
        let out = pretty_module(&m);
        let count = out.lines().filter(|l| l.trim() == "//").count();
        assert_eq!(count, 1, "empty comment run should collapse; got:\n{out}");
    }

    // ---- hello_world module shape --------------------------------------

    #[test]
    fn hello_world_module_file() {
        roundtrip(
            r#"mod hello/main;

host type String role(str);

host fn print(s: String) -> .;

pub fn main() -> . { print("Hello, world") }"#,
        );
    }

    #[test]
    fn host_type_and_fn_roundtrip() {
        let pp =
            roundtrip("mod x; host type Map[K][V]; host fn get[K][V](m: Map[K][V], k: K) -> V;");
        assert!(pp.contains("host type Map[K][V];"), "got:\n{pp}");
        assert!(
            pp.contains("host fn get[K][V](m: Map[K][V], k: K) -> V;"),
            "got:\n{pp}"
        );
    }

    #[test]
    fn host_pub_canonicalizes_to_plain_host() {
        let pp = roundtrip("mod x; pub host type Foo;");
        assert!(pp.contains("host type Foo;"), "got:\n{pp}");
        assert!(!pp.contains("pub host"), "got:\n{pp}");
    }

    // ---- package-file round-trips ---------------------------------------

    fn package_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_package_file;
        let full = format!("pkg pkg;\n{src}");
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
    fn module_header_comments_keep_path_and_documentation() {
        let source = "// file note\n/// Module documentation.\nmod // keyword\napp // slash\n/ // segment\nmain // terminator\n;\n/// Function documentation.\nfn f() { () }";
        let printed = roundtrip(source);
        assert_eq!(parsed_structure(source), parsed_structure(&printed));
        let mut previous = 0;
        for comment in ["file note", "keyword", "slash", "segment", "terminator"] {
            let marker = format!("// {comment}\n");
            assert_eq!(printed.matches(&marker).count(), 1, "{printed}");
            let position = printed.find(&marker).unwrap();
            assert!(position >= previous, "{printed}");
            previous = position;
        }
        assert!(
            printed.contains("/// Module documentation.\nmod app/main;"),
            "{printed}"
        );
        assert!(
            printed.contains("/// Function documentation.\nfn f()"),
            "{printed}"
        );
    }

    #[test]
    fn package_header_and_config_value_comments_survive() {
        use crate::pass::parser::parse_package_file;
        for cache in ["\"out/cache/\"", "(// cache interior\n)"] {
            let source = format!(
                "pkg // package name
pkg // package terminator
;
build // build opener
{{
cache // cache value
{cache};
docs // docs opener
{{
md // md value
\"do\" // string chunk
\"cs\";
support // support value
\"support\";
md_out // md_out value
\"out/md/\";
html // html value
\"out/html/\"
}};
target rust {{ out // target value
\"out/rust/\"; baseline v1_85; namespace \"pkg\"; thread_safety () }}
}}
bridge // bridge opener
{{ main }}"
            );
            let before = parse_package_file(&source, Some("pkg")).unwrap();
            let printed = pretty_package_file(&before);
            let after = parse_package_file(&printed, Some("pkg")).unwrap();
            assert_eq!(printed, pretty_package_file(&after));
            assert_eq!(source_structure(&before), source_structure(&after));
            let (tokens, _) = crate::pass::lexer::lex_with_trailing(&source).unwrap();
            let mut previous = 0;
            for comment in tokens.into_iter().flat_map(|token| token.leading_trivia) {
                if let Trivia::LineComment { text, .. } = comment {
                    let marker = format!("//{text}\n");
                    assert_eq!(printed.matches(&marker).count(), 1, "{printed}");
                    let position = printed.find(&marker).unwrap();
                    assert!(position >= previous, "{printed}");
                    previous = position;
                }
            }
            assert!(printed.contains("md \"docs\";"), "{printed}");
        }
    }

    #[test]
    fn config_value_comments_follow_their_reordered_keys() {
        let printed = package_roundtrip(
            "build { target rust { namespace // namespace value\n\"demo\"; \
             out // output value\n\"out/rust/\" }; cache // cache value\n(); }",
        );
        let markers = [
            "// cache value\n  cache ();",
            "// output value\n    out \"out/rust/\";",
            "// namespace value\n    namespace \"demo\"",
        ];
        let mut previous = 0;
        for marker in markers {
            assert_eq!(printed.matches(marker).count(), 1, "{printed}");
            let position = printed.find(marker).unwrap();
            assert!(position >= previous, "{printed}");
            previous = position;
        }
        let dependency = dependency_roundtrip(
            "dep local; source { path // dependency path\n\"../other.pkg.kio\" }",
        );
        assert!(
            dependency.contains("// dependency path\n  path \"../other.pkg.kio\""),
            "{dependency}"
        );
        let lock = lock_roundtrip(
            "lock local; resolved { git // git value\n\"repo\"; ref \"main\"; commit \"revision\"; sig \"digest\" }",
        );
        assert!(lock.contains("// git value\n  git \"repo\";"), "{lock}");
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
        let pp =
            dependency_roundtrip("dep mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\";\n}\n");
        assert_eq!(
            pp, "dep mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\"\n}\n",
            "canonical .dep.kio layout drifted:\n{pp}"
        );
    }

    /// A messy-but-valid `.dep.kio` (extra blank lines, odd spacing,
    /// trailing separators) canonicalises to the fixed layout.
    #[test]
    fn dependency_messy_canonicalises() {
        let pp = dependency_roundtrip(
            "dep mathlib  ;\n\n\n\nsource   {\n\n    path    \"../lib/m.pkg.kio\"  ;\n\n}\n",
        );
        assert_eq!(
            pp, "dep mathlib;\n\nsource {\n  path \"../lib/m.pkg.kio\"\n}\n",
            "messy .dep.kio did not canonicalise:\n{pp}"
        );
    }

    /// A `//` header comment above the `dep` line and a `//`
    /// comment above the `path` line survive the round-trip (the parser
    /// stashes them in `meta.leading_trivia` / `path_leading_trivia`).
    #[test]
    fn dependency_preserves_leading_comments() {
        let pp = dependency_roundtrip(
            "// pin the local math library\ndep mathlib;\n\nsource {\n  // relative to the consumer package root\n  path \"../lib/m.pkg.kio\";\n}\n",
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
            "dep foo;\n\nsource {\n  git \"https://example.com/foo.git\";\n  ref \"main\"\n}\n",
        );
        assert_eq!(
            pp, "dep foo;\n\nsource {\n  git \"https://example.com/foo.git\";\n  ref \"main\"\n}\n",
            "canonical git .dep.kio layout drifted:\n{pp}"
        );
    }

    /// A messy git `.dep.kio` canonicalises to the fixed layout (and the
    /// `git` / `ref` source order is preserved by construction — the AST
    /// stores them as named fields, not a key order).
    #[test]
    fn dependency_git_messy_canonicalises() {
        let pp = dependency_roundtrip(
            "dep foo ;\n\n\nsource  {\n\n  git   \"file:///srv/foo.git\"  ;\n  ref \"v1.2.3\" ;\n\n}\n",
        );
        assert_eq!(
            pp, "dep foo;\n\nsource {\n  git \"file:///srv/foo.git\";\n  ref \"v1.2.3\"\n}\n",
            "messy git .dep.kio did not canonicalise:\n{pp}"
        );
    }

    /// Comments above the `git` and `ref` lines survive the round-trip.
    #[test]
    fn dependency_git_preserves_leading_comments() {
        let pp = dependency_roundtrip(
            "dep foo;\n\nsource {\n  // the upstream mirror\n  git \"https://example.com/foo.git\";\n  // track the release branch\n  ref \"release\";\n}\n",
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

    #[test]
    fn configuration_preserves_omitted_and_explicit_settings() {
        let output = package_roundtrip(
            "build { docs { md \"docs\" }; target rust { out \"out/rust\" }; target haskell { out \"out/haskell\" }; target kio-prime { out \"out/prime\" } }",
        );
        for field in [
            "cache ()",
            "md_out \"out/docs-md/\"",
            "html \"out/docs/\"",
            "namespace ()",
            "thread_safety ()",
        ] {
            assert!(!output.contains(field), "invented {field}:\n{output}");
        }
        let parsed = crate::pass::parser::parse_package_file(&output, Some("pkg")).unwrap();
        let targets = parsed.build.unwrap().targets;
        assert_eq!(targets[0].entries.len(), 2);
        assert_eq!(targets[1].entries.len(), 2);
        assert_eq!(targets[2].entries.len(), 1);
        assert!(!package_roundtrip("").contains("build"));
        assert!(!build_roundtrip("target js { out \"js\" }").contains("docs"));
        assert!(
            !dependency_roundtrip("dep dep; source { git \"url\"; ref \"main\" }")
                .contains("path ()")
        );
        assert!(
            !lock_roundtrip(
                "lock dep; resolved { git \"url\"; ref \"main\"; commit \"sha\"; sig \"digest\" }"
            )
            .contains("path ()")
        );
        assert!(
            !dependency_roundtrip("dep dep; source { path \"dep.pkg.kio\" }").contains("path ()")
        );
        let explicit = package_roundtrip(
            "build { cache (); docs { md \"docs\"; md_out (); html \"out/docs/\" }; target rust { out \"out\"; namespace (); thread_safety ((send_sync)) } }",
        );
        for field in [
            "cache ()",
            "md_out ()",
            "html \"out/docs/\"",
            "namespace ()",
            "thread_safety send_sync",
        ] {
            assert!(explicit.contains(field), "lost {field}:\n{explicit}");
        }
        assert!(
            dependency_roundtrip("dep dep; source { git \"url\"; ref \"main\"; path () }")
                .contains("path ()")
        );
        assert!(lock_roundtrip("lock dep; resolved { git \"url\"; ref \"main\"; path (); commit \"sha\"; sig \"digest\" }").contains("path ()"));
    }

    #[test]
    fn configuration_grouping_canonicalizes_without_losing_comments() {
        let sources = [
            "build { cache (); target (rust, \"native\") { out \"out/rust\"; thread_safety () } }",
            "build { cache (((()))); target (rust, ((\"na\" \"tive\"))) { out ((\"out/\" \"rust\")); thread_safety ((())) } }",
        ];
        assert_eq!(package_roundtrip(sources[0]), package_roundtrip(sources[1]));
        let source = "build { cache ( // outer\n( // inner\n) // close\n); target (rust, ( // name\n\"native\")) { out ( // output\n\"out\" // end\n); thread_safety ( // absent\n()) } }";
        let output = package_roundtrip(source);
        for comment in ["outer", "inner", "close", "name", "output", "end", "absent"] {
            assert_eq!(
                output.matches(&format!("// {comment}\n")).count(),
                1,
                "{output}"
            );
        }
        let dependency = dependency_roundtrip(
            "dep dep; source { path ( // selection\n()); git (\"url\"); ref ((\"main\")) }",
        );
        assert!(
            dependency.contains("// selection\n  path ()"),
            "{dependency}"
        );
        let lock = lock_roundtrip(
            "lock dep; resolved { git (\"url\"); ref (\"main\"); path ( // selection\n()); commit (\"sha\"); sig (\"digest\") }",
        );
        assert!(lock.contains("// selection\n  path ()"), "{lock}");
    }

    /// Round-trip a `build { ... }` block body (the `cache` / `docs` /
    /// `target` sequence, without the enclosing braces) through the
    /// standalone body parser + `doc_build_block` and assert
    /// idempotence.
    fn build_roundtrip(src: &str) -> String {
        use crate::pass::parser::parse_build_block_body;
        let b1 = parse_build_block_body(src)
            .unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"));
        let pp1 = render(&doc_build_block(&b1, true), WIDTH);
        // The rendered form carries the `build { ... }` braces; the
        // body parser wants the inner body, so re-parse via the
        // package-file parser to confirm the rendered block is
        // well-formed and stable.
        let wrapped = format!("pkg pkg;\n\n{pp1}\n");
        let e2 = crate::pass::parser::parse_package_file(&wrapped, Some("pkg"))
            .unwrap_or_else(|e| panic!("re-parse failed: {e:?}\noutput was:\n{wrapped}"));
        let b2 = e2
            .build
            .expect("rendered package file carries the build block");
        let pp2 = render(&doc_build_block(&b2, true), WIDTH);
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
             target rust { thread_safety send_sync; namespace \"demo\"; out \"out/rust/\"; }",
        );
        let out_at = pp.find("out \"out/rust/\";").unwrap();
        let namespace_at = pp.find("namespace \"demo\";").unwrap();
        let thread_safety_at = pp.find("thread_safety send_sync").unwrap();
        assert!(
            out_at < namespace_at && namespace_at < thread_safety_at,
            "got:\n{pp}"
        );
    }

    #[test]
    fn build_baselines_materialize_only_implicit_selection() {
        for backend in crate::build_target::BuildTarget::ALL.iter().copied() {
            let Some(baseline) = backend.latest_baseline() else {
                continue;
            };
            let expected = format!(
                "build {{\n  target ({}, \"named\") {{\n    out \"out\";\n    baseline {baseline}\n  }}\n}}",
                backend.id()
            );
            for setting in [
                String::new(),
                "baseline ()".to_owned(),
                format!("baseline (({baseline}))"),
            ] {
                let source = format!(
                    "target ({}, \"named\") {{ out \"out\"; {setting} }}",
                    backend.id()
                );
                assert_eq!(build_roundtrip(&source), expected);
            }
            let floating = build_roundtrip(&format!(
                "target {} {{ out \"out\"; baseline ((latest)) }}",
                backend.id()
            ));
            assert!(floating.contains("baseline latest"), "{floating}");
            assert!(
                !floating.contains(&format!("baseline {baseline}")),
                "{floating}"
            );
        }
        let sparse = build_roundtrip("target rust { out \"out\" }");
        assert!(
            !sparse.contains("namespace") && !sparse.contains("thread_safety"),
            "{sparse}"
        );
        let prime = build_roundtrip("target kio-prime { out \"out\" }");
        assert!(!prime.contains("baseline"), "{prime}");
    }

    #[test]
    fn build_baseline_materialization_preserves_grouping_comments() {
        let source =
            "target js { out \"out\"; // choice\n baseline ( // outer\n( // unit\n) // close\n) }";
        let printed = build_roundtrip(source);
        for comment in ["// choice", "// outer", "// unit", "// close"] {
            assert_eq!(printed.matches(comment).count(), 1, "{printed}");
        }
        assert!(printed.contains("baseline es2020"), "{printed}");
    }

    #[test]
    fn build_target_comments_follow_canonical_key_order() {
        let pp = build_roundtrip(
            "cache ();\n\
             target rust { \
               thread_safety send_sync; \
               // output path\n\
               out \"out/rust/\"; \
               namespace \"demo\"; \
             }",
        );
        let output_comment = pp.find("// output path").unwrap();
        let out_at = pp.find("out \"out/rust/\";").unwrap();
        let namespace_at = pp.find("namespace \"demo\";").unwrap();
        let thread_safety_at = pp.find("thread_safety send_sync").unwrap();
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
    fn build_named_target_headers_roundtrip() {
        let pp = build_roundtrip(
            r#"target(js,"web") { namespace "demo"; out "web" };
            target (kio-prime, "archive") {}; target (js, "js") {}"#,
        );
        assert!(pp.contains(r#"target (js, "web") {"#), "{pp}");
        assert!(pp.contains(r#"target (kio-prime, "archive") {"#), "{pp}");
        assert!(pp.contains(r#"target (js, "js") {"#), "{pp}");
    }

    #[test]
    fn build_named_target_header_comments_survive() {
        let pp = build_roundtrip(
            "target // pair\n (// backend\n js // comma\n, // name\n \"w\" // chunk\n \"eb\" // close\n) { out \"web\" }",
        );
        for comment in ["pair", "backend", "comma", "name", "chunk", "close"] {
            assert_eq!(pp.matches(&format!("// {comment}\n")).count(), 1, "{pp}");
        }
        assert!(pp.contains(r#"target (js, "web")"#), "{pp}");
        let grouped = build_roundtrip("target (js, // name\n((\"web\"))) { out \"web\" }");
        assert_eq!(grouped.matches("// name\n").count(), 1, "{grouped}");
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
        let pp = roundtrip(r#"mod x; fn f() -> . { id[String]("hi") }"#);
        assert!(pp.contains("id[String](\"hi\")"), "got:\n{pp}");
    }

    /// A call argument list whose flat width exceeds the 100-column
    /// budget breaks into the leading-comma A1 layout. The fn
    /// body itself also breaks to multi-line (it can't stay
    /// single-line if the call inside it doesn't fit), so the
    /// call args land at +4 indent from column 0 (fn body's
    /// nest 2 + call's nest 2).
    #[test]
    fn call_args_overflow_breaks_to_a1() {
        let src = r#"mod x;
fn f() -> . {
  call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta, arg_epsilon)
}"#;
        let pp = roundtrip(src);
        assert!(
            pp.contains("\n  , arg_alpha"),
            "expected leading-comma break for arg_alpha at the body base; got:\n{pp}"
        );
        assert!(
            pp.contains("\n  , arg_epsilon"),
            "expected leading-comma break for arg_epsilon at the body base; got:\n{pp}"
        );
    }

    /// Successive type-argument brackets retain their singleton stages.
    #[test]
    fn type_app_args_use_separate_bracket_stages() {
        let src = "mod x;
type Result = Outer[Inner_one][Inner_two][Inner_three][Inner_four][Inner_five][Inner_six][Inner_seven];";
        let pp = roundtrip(src);
        assert!(
            pp.contains("Outer[Inner_one][Inner_two][Inner_three][Inner_four][Inner_five][Inner_six][Inner_seven]"),
            "type arguments stay in their individual bracket stages; got:\n{pp}"
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
        let src = "mod x;
fn filter[A](keep: A -> Bool, xs: A, whole: A) -> A {
reverse(fold_list(xs, whole, .(carry: A, x: A) -> A { if(keep(x)) { cons(x, carry) } else { carry } }))
}
";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            "mod x;

fn filter[A](keep: A -> Bool, xs: A, whole: A) -> A {
\x20 reverse(fold_list(
\x20 , xs
\x20 , whole
\x20 , .(carry: A, x: A) -> A {
\x20     if(keep(x)) { cons(x, carry) } else { carry }
\x20   }
\x20 ))
}
"
        );
    }

    /// The same item-content anchoring through the `match` clause
    /// path: a clause lambda is an item of the clause tuple, itself
    /// an item of the `match` argument list, so each `, ` level
    /// nests its content and a broken clause body sits at +2 from
    /// its clause header.
    #[test]
    fn match_clause_bodies_nest_inside_their_clause() {
        let src = "mod x;
import match {macro match}
fn step[A](v: A | A, dflt: A) -> A {
match(v, (.(l: A) { if(is_good(l)) { l } else { dflt } }, .(r: A) { r }))
}
";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            "mod x;

import match { macro match }

fn step[A](v: A | A, dflt: A) -> A {
\x20 match(
\x20 , v
\x20 , (
\x20   , .(l: A) {
\x20       if(is_good(l)) { l } else { dflt }
\x20     }
\x20   , .(r: A) { r }
\x20   )
\x20 )
}
"
        );
    }

    /// A wide compact group uses indented semicolon entries and keeps
    /// its opening brace on the declaration line.
    #[test]
    fn compact_nominal_wide_group_uses_indented_semicolon_entries() {
        let src = "mod x; type Wide = { First_long_label_name: Someextremelylongtypeconstructorname[Aaaaaaaa][Bbbbbbbb][Gggggggg][Hhhhhhhh]; Second_label: I32 };";
        let pp = roundtrip(src);
        assert_eq!(
            pp,
            r#"mod x;

type Wide = {
  First_long_label_name: Someextremelylongtypeconstructorname[Aaaaaaaa][Bbbbbbbb][Gggggggg][Hhhhhhhh];
  Second_label: I32
};
"#
        );
    }

    /// Width-driven and comment-forced compact layouts differ only by the comment.
    #[test]
    fn compact_nominal_width_and_comment_paths_agree() {
        let plain = "mod x; type Wide = { First_long_label_name: Someextremelylongtypeconstructorname[Aaaaaaaa][Bbbbbbbb][Gggggggg][Hhhhhhhh]; Second_label: I32 };";
        let commented = r#"mod x;
type Wide = {
  // keep me
  First_long_label_name: Someextremelylongtypeconstructorname[Aaaaaaaa][Bbbbbbbb][Gggggggg][Hhhhhhhh];
  Second_label: I32;
};
"#;
        let plain_pp = roundtrip(plain);
        let commented_pp = roundtrip(commented);
        assert!(
            commented_pp.contains("\n  // keep me\n"),
            "entry comment survived: {commented_pp}"
        );
        assert_eq!(
            commented_pp.replace("  // keep me\n", ""),
            plain_pp,
            "compact layouts diverge once the comment line is removed"
        );
    }

    // ---- string-literal reflow at 72 chars -----------------------------

    /// Short string literals (value ≤ 72 chars) emit single-line.
    #[test]
    fn short_string_stays_single_line() {
        let pp = roundtrip(r#"mod x; fn f() -> . { let s = "hello world"; () }"#);
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
        let src = format!(r#"mod x; fn f() -> . {{ let s = "{value}"; () }}"#);
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
        let pp = roundtrip("mod x; fn id[A](x: A) -> A { x }");
        assert!(
            pp.contains(") -> A { x }"),
            "short fn body should stay inline; got:\n{pp}"
        );
    }

    #[test]
    fn final_block_semicolon_canonicalizes_away() {
        let pp = roundtrip("mod x; fn f() -> . { (); }");
        assert!(pp.contains("fn f() { () }"), "got:\n{pp}");
        assert!(!pp.contains("();"), "got:\n{pp}");
    }

    #[test]
    fn scope_call_round_trips_as_block_expression() {
        let pp = roundtrip("mod x; fn f() -> . { scope { let x = (); x } }");
        assert!(pp.contains("scope { let x = (); x }"), "got:\n{pp}");
        assert!(pp.contains("let x = ();"), "got:\n{pp}");
    }

    #[test]
    fn scope_call_comment_survives_round_trip() {
        let pp = roundtrip(
            "mod x; fn f() -> . { scope { // local binding
let x = (); x } }",
        );
        assert!(
            pp.contains("scope {\n    // local binding\n    let x = ();"),
            "expected scope comment before local let; got:\n{pp}"
        );
    }

    #[test]
    fn sequence_call_comment_survives_round_trip() {
        let pp = roundtrip(
            "mod x; fn bind[A][B](x: A, k: A -> B) -> B { k(x) } fn f() -> . { do(bind) { // sequence step
(); () } }",
        );
        assert!(
            pp.contains("do(bind) {\n    // sequence step\n    ();"),
            "expected sequence comment before expression; got:\n{pp}"
        );
    }

    /// A comment captured before a `match` clause survives the
    /// round-trip: the comment emits on its own line above the
    /// clause inside the multi-line arms tuple.
    #[test]
    fn within_body_comment_before_match_clause_survives_round_trip() {
        // Use single-clause `match` first to verify trivia
        // capture works for the basic case. Multi-clause is the
        // intended target — a baseline check below confirms the
        // comment-free multi-clause parses, then the with-comment
        // form is round-tripped.
        let bare = "mod x; import match {macro match} fn f[A](v: A) -> . { match(v, .() { () }) }";
        parse(bare).expect("single-clause baseline parses");
        // The comment-free multi-clause control must parse before
        // checking the same shape with interstitial trivia.
        let multi_bare = "mod x; import match {macro match} fn f[A](v: A) -> . { match(v, (.() { () }, .[B](n: B) { () })) }";
        parse(multi_bare).expect("multi-clause baseline parses");
        let src = "mod x; import match {macro match} fn f[A](v: A) -> . { match(v, (.() { () }, // sec\n.[B](n: B) { () })) }";
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
        let src = "mod x;
fn f() -> . {
// initialize x
let x = ();
x
}
";
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
        let src = "mod x;
fn f() -> . {
// finish
g()
}
";
        let out = roundtrip(src);
        assert_eq!(
            out.matches("// finish").count(),
            1,
            "comment before the final call must emit exactly once; got:\n{out}"
        );
    }

    #[test]
    fn within_closure_body_comment_before_final_call_emits_once() {
        let src = "mod x;
fn f() -> . {
.() {
// finish
g()
}()
}
";
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
        let src = "mod x;
import match {macro match}
fn apply[A](x: A, f: A -> A) -> A { f(x) }
fn g[A](p: A & A) -> A & A {
apply[A & A](p, .(q) {
// keep q
let kept = match(q, .(l: A, r: A) { (l, r) });
kept
})
}
";
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
        let src = "mod x;
fn f() -> . {
()
// dangling
}
";
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
        let src = "mod x;
fn f() -> . {
() // the result
}
";
        let out = roundtrip(src);
        assert_eq!(out.matches("// the result").count(), 1, "got:\n{out}");
    }

    /// A trailing comment on the last call argument, before the `)`,
    /// survives in the call container's closing trivia.
    #[test]
    fn trailing_comment_on_last_call_arg_survives() {
        let src = "mod x;
fn f() -> . {
g(
a,
b // last arg
)
}
";
        let out = roundtrip(src);
        assert_eq!(out.matches("// last arg").count(), 1, "got:\n{out}");
    }

    /// A comment after the last item, before end-of-file, survives —
    /// it rides the module's `meta.trailing_trivia` (the lexer's
    /// end-of-file trivia run).
    #[test]
    fn trailing_comment_after_last_item_survives() {
        let src = "mod x;
fn f() -> . { () } // after item
";
        let out = roundtrip(src);
        assert_eq!(out.matches("// after item").count(), 1, "got:\n{out}");
    }

    /// An end-of-file-only comment (no trailing item attachment)
    /// survives.
    #[test]
    fn end_of_file_comment_survives() {
        let src = "mod x;
fn f() -> . { () }
// file footer
";
        let out = roundtrip(src);
        assert_eq!(out.matches("// file footer").count(), 1, "got:\n{out}");
    }

    /// A comment dangling at the end of a trailing block, before `}`,
    /// survives.
    #[test]
    fn trailing_comment_in_scope_call_survives() {
        let src = "mod x;
fn f() -> . {
scope {
()
// dangling in do
}
}
";
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
        let src = "mod x;
fn f() -> . {
let y =
// between = and value
();
y
}
";
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
        let pp = roundtrip("mod x; fn f() -> . { let x = (); x }");
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
        let src = r#"mod x;
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
        let width_src = r#"mod x;
fn f() -> . {
  let result =
    call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta);
  result
}"#;
        let width_pp = roundtrip(width_src);
        // Trivia-driven version (short value, but a comment forces break).
        let trivia_src = "mod x;
fn f() -> . {
let result =
// explainer
compute();
result
}
";
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
        let src = r#"mod x;
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
        let src = r#"mod x;
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
            pp.contains("\n    , arg_alpha"),
            "expected A1 break for call args; got:\n{pp}"
        );
        // The `;` closes the call at the close-paren's line.
        assert!(
            pp.contains("\n    );"),
            "expected `;` trailing the call's close paren; got:\n{pp}"
        );
    }

    // ---- UFCS chain layout --------------------------------------------

    /// A short UFCS chain fits on one line: `r.>f(x).>g(y).>h(z)`.
    #[test]
    fn short_ufcs_chain_stays_flat() {
        let pp = roundtrip("mod x; fn f() -> . { let r = receiver.>f(x).>g(y).>h(z); () }");
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
        let src = r#"mod x;
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
        let flat = roundtrip("mod x; fn f() -> . { let r = r.>bare; () }");
        assert!(
            flat.contains("r.>bare"),
            "single bare UFCS should have no parens; got:\n{flat}"
        );
        let chain = roundtrip("mod x; fn f() -> . { let r = r.>bare.>next(x); () }");
        assert!(
            chain.contains("r.>bare.>next(x)"),
            "bare segment inside a flat chain should have no parens; got:\n{chain}"
        );
    }

    #[test]
    fn macro_block_prefixes_keep_their_parentheses() {
        for head in ["select", "let", "recur"] {
            for argument in ["value", "compute(value)", ".t", "-1"] {
                let source = format!(
                    "mod x; import provider {{macro {head}}} fn run(value: .) -> . {{ {head}({argument}) {{ () }} }}"
                );
                let output = roundtrip(&source);
                assert!(
                    output.contains(&format!("{head}({argument}) {{")),
                    "{output}"
                );
            }
        }
    }

    #[test]
    fn macro_block_result_call_keeps_its_callee_grouped() {
        let output = roundtrip(
            "mod x; import provider {macro choose} fn run(value: .) -> . { (choose { value })(()) }",
        );
        assert!(output.contains("(choose { value })(())"), "{output}");
    }

    #[test]
    fn macro_left_call_splice_preserves_selected_import() {
        let pp = roundtrip(
            "mod x; import palette {macro iso} fn f() -> . { let r = iso[T].<<value; () }",
        );
        assert!(
            pp.contains("iso[T].<<value"),
            "macro left-call splice should preserve its call spelling; got:\n{pp}"
        );
    }

    #[test]
    fn bare_and_explicit_unit_ufcs_forms_roundtrip() {
        let source = r#"mod x;
fn f() -> . {
  receiver.>call;
  receiver.>>call;
  call.<receiver;
  call.<<receiver;
  receiver.>T.member;
  receiver.>>T.member;
  T.member.<receiver;
  T.member.<<receiver;
  receiver.>transform;
  receiver.>>transform;
  transform.<receiver;
  transform.<<receiver;
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
            "receiver.>transform",
            "receiver.>>transform",
            "transform.<receiver",
            "transform.<<receiver",
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
        let src = r#"mod x;
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
            pp.contains("\n      , arg_one_with_a_long_name"),
            "expected leading-comma arg at the segment base; got:\n{pp}"
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
        let src = "mod x;
fn f() -> . {
let r = recv // before the step
.>step(arg);
()
}
";
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
        let src = "mod x;
fn f() -> . {
g(
, receiver_with_a_name // step comment
.>chain_step_with_a_long_name(some_argument_value) // element comment
)
}
";
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
        let src = r#"mod x;
fn f() -> . {
  call_with_long_name_to_push_over_a_hundred_columns(arg_alpha, arg_beta, arg_gamma, arg_delta, arg_epsilon)
}"#;
        let pp = roundtrip(src);
        // The body is on its own line at +2 indent, closing `}`
        // at column 0.
        assert!(
            pp.contains("fn f() {\n  "),
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
        let src = format!(r#"mod x; fn f() -> . {{ let s = "{value}"; () }}"#);
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

#[cfg(test)]
mod nominal_format_tests {
    #[test]
    fn nominal_format_roundtrips_all_declaration_shapes() {
        for declaration in [
            "type { _Name: .; Age: .; }",
            "pub type { Token[A]<X>: A & X { mk; pub get; }; }",
            "type Result = { Success: .; } | (Name & Age);",
            "type Result = (Name & Age) | { Failure: .; };",
            "type Result = { Tag: .; } & (Existing | { Empty: .; });",
            "type Result = (Name -> Age) | { Failure: .; };",
            "type Result = ([A] A -> A) | { Failure: .; };",
            "rec type Tree = { Leaf: .; } | { Branch: Tree & Tree; };",
            "rec type { Node: Node { mk; get; }; }",
        ] {
            let source = format!("mod x; {declaration}");
            let parsed = crate::pass::parser::parse(&source).unwrap();
            let formatted = super::pretty_module(&parsed);
            let reparsed = crate::pass::parser::parse(&formatted).unwrap_or_else(|error| {
                panic!("formatted declaration failed: {formatted}\n{error:?}")
            });
            assert_eq!(super::pretty_module(&reparsed), formatted);
            let lazy = crate::pass::parser::parse_lazy(&source)
                .unwrap()
                .force_all()
                .unwrap();
            assert_eq!(super::pretty_module(&lazy), formatted);
        }
    }

    #[test]
    fn nominal_rhs_chains_follow_the_type_chain_width_layout() {
        let source = "mod x; type Result = { Field: .; } & Alpha_payload_type & Beta_payload_type & Gamma_payload_type & Delta_payload_type & Epsilon_payload_type & Zeta_payload_type & Eta_payload_type & Theta_payload_type;";
        let parsed = crate::pass::parser::parse(source).unwrap();
        let formatted = super::pretty_module(&parsed);
        assert!(
            formatted.lines().all(|line| line.len() <= 100),
            "{formatted}"
        );
        assert!(
            formatted.contains("type Result =\n  & { Field: . }\n"),
            "{formatted}"
        );
        assert!(formatted.ends_with("  ;\n"), "{formatted}");
        let reparsed = crate::pass::parser::parse(&formatted).unwrap();
        assert_eq!(super::pretty_module(&reparsed), formatted);
    }

    #[test]
    fn nominal_rhs_group_comments_roundtrip_once() {
        for (source, comments) in [
            (
                "mod x; type Result = { First: .; } | // next-operand\n(Existing & { Other: .; });",
                &["next-operand"][..],
            ),
            (
                "mod x; type Result = { First: .; } | (// group-body\nExisting & { Other: .; } // group-end\n);",
                &["group-body", "group-end"][..],
            ),
        ] {
            let parsed = crate::pass::parser::parse(source).unwrap();
            let formatted = super::pretty_module(&parsed);
            for comment in comments {
                assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
            }
            let reparsed = crate::pass::parser::parse(&formatted).unwrap();
            assert_eq!(super::pretty_module(&reparsed), formatted);
        }
    }

    #[test]
    fn nominal_format_preserves_comment_ownership() {
        let source = "mod x; pub type { // entry-start\n Token: . { // member-start\n pub // visibility\n get; // member-between\n mk; // member-end\n }; // entry-end\n }";
        let formatted = super::pretty_module(&crate::pass::parser::parse(source).unwrap());
        for comment in [
            "entry-start",
            "member-start",
            "visibility",
            "member-between",
            "member-end",
            "entry-end",
        ] {
            assert_eq!(
                formatted.matches(comment).count(),
                1,
                "{comment} in {formatted}"
            );
        }
        let reparsed = crate::pass::parser::parse(&formatted).unwrap();
        assert_eq!(super::pretty_module(&reparsed), formatted);
    }
}

//! Directive and intra-doc-link rewriting for Kiodoc rendering.
//!
//! `kio doc check` only *validates* the Kiodoc directives; `kio doc
//! build` rewrites them into the rendered output. This module is the
//! rewriting pass — and it rewrites Kiodoc forms back into **plain
//! Markdown**. A `` [`@signature term`] `` becomes an inline code
//! span `` `sig` `` carrying the resolved item's pretty-printed
//! declaration header; a `` [`@type term`] `` becomes an inline code
//! span carrying the item's bound-value type; a `` [`@source term`] ``
//! becomes a fenced ` ```kio ` block on its own line carrying the
//! item's full source (doc-comment stripped); a `` [`name`] ``
//! becomes a Markdown link to the page + anchor that documents
//! `name`, or a plain code span when `name` is not documented in
//! this package (see [`specs/kiodoc.md`](../../../../specs/kiodoc.md)
//! § "Resolved-link rendering").
//!
//! Rewriting to Markdown rather than directly to HTML means the
//! Markdown emitter (`kio doc build --md`) consumes the rewriter's
//! output verbatim, and the HTML emitter runs it through the shared
//! [`super::markdown`] renderer — one rewriter, no format-specific
//! escaping bugs.
//!
//! A `[name]: url` Markdown reference-link definition overrides
//! auto-resolution for that key — the same override semantics
//! `kio doc check` honors. Overridden names are left exactly as
//! written so the author's explicit link wins.

use std::collections::HashSet;

use super::site::SymbolIndex;

pub enum LinkTarget {
    Exact(String),
    Plain,
    LegacyName,
}

/// Resolution scope for one page's directives — supplies the
/// pretty-printed signature / source for a directive term.
///
/// A module page resolves terms against that module's items; a
/// tutorial `.md` page resolves against the package's package file.
/// The renderer builds one [`DirectiveScope`] per page.
pub trait DirectiveScope {
    fn link_target(&self, term: &str) -> LinkTarget;
    /// Render the declaration header of the item named `term`, or
    /// `None` if `term` does not resolve in this scope.
    fn signature(&self, term: &str) -> Option<String>;
    /// Render the full source of the item named `term`, or `None`.
    fn source(&self, term: &str) -> Option<String>;
    /// Render the bound-value type of the item named `term`, or
    /// `None` when `term` does not resolve or names a type-level name
    /// (which binds no value — `kio doc check` rejects `@type`
    /// against one before rendering is reached).
    fn ty(&self, term: &str) -> Option<String>;
}

/// Rewrite the Kiodoc directives and intra-doc links in `prose` into
/// plain Markdown. `url_prefix` is prepended to every in-package
/// link target (e.g. `../` for a nested module page); `link_ext` is
/// the page extension to link to (`.html` or `.md`); `index`
/// resolves `` [`name`] `` references; `scope` resolves
/// `@signature` / `@source` terms; `overrides` is the set of
/// reference-link keys the author defined by hand.
pub fn rewrite(
    prose: &str,
    url_prefix: &str,
    link_ext: &str,
    index: &SymbolIndex,
    scope: &dyn DirectiveScope,
    overrides: &HashSet<String>,
) -> String {
    let fences = crate::kiodoc::parse::scan(prose);
    if fences.is_empty() {
        return rewrite_prose_segment(prose, url_prefix, link_ext, index, scope, overrides);
    }

    let mut out = String::with_capacity(prose.len());
    let mut cursor = 0usize;
    for fence in fences {
        let start = fence.span.start as usize;
        let end = fence.span.end as usize;
        out.push_str(&rewrite_prose_segment(
            &prose[cursor..start],
            url_prefix,
            link_ext,
            index,
            scope,
            overrides,
        ));
        out.push_str(&prose[start..end]);
        cursor = end;
    }
    out.push_str(&rewrite_prose_segment(
        &prose[cursor..],
        url_prefix,
        link_ext,
        index,
        scope,
        overrides,
    ));
    out
}

fn rewrite_prose_segment(
    prose: &str,
    url_prefix: &str,
    link_ext: &str,
    index: &SymbolIndex,
    scope: &dyn DirectiveScope,
    overrides: &HashSet<String>,
) -> String {
    let bytes = prose.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if let Some(end) = crate::kiodoc::parse::inline_literal_end(prose, i) {
            out.push_str(&prose[i..end]);
            i = end;
            continue;
        }
        if bytes[i] != b'[' {
            push_char(&mut out, prose, &mut i);
            continue;
        }
        // Try to parse a `` [`...`] `` Kiodoc bracket form starting
        // here. If it doesn't match, emit the `[` verbatim.
        match parse_kiodoc_bracket(&prose[i..]) {
            Some(parsed) => {
                let consumed = parsed.consumed;
                // A trailing `(` or `[` makes this a plain Markdown
                // link, not a Kiodoc form — leave it verbatim.
                let after = prose[i + consumed..].trim_start();
                if after.starts_with('(') || after.starts_with('[') {
                    push_char(&mut out, prose, &mut i);
                    continue;
                }
                let rendered =
                    render_bracket(&parsed, url_prefix, link_ext, index, scope, overrides);
                out.push_str(&rendered);
                i += consumed;
            }
            None => push_char(&mut out, prose, &mut i),
        }
    }
    out
}

/// Copy the next UTF-8 character of `s` (starting at `*i`) to `out`,
/// advancing `*i`.
fn push_char(out: &mut String, s: &str, i: &mut usize) {
    let ch = s[*i..].chars().next().unwrap();
    out.push(ch);
    *i += ch.len_utf8();
}

/// A parsed `` [`...`] `` Kiodoc bracket form.
struct KiodocBracket {
    kind: BracketKind,
    /// Total bytes consumed, including both brackets and backticks.
    consumed: usize,
}

enum BracketKind {
    /// `` [`@signature term`] ``, `` [`@source term`] ``, or
    /// `` [`@type term`] ``.
    Directive { keyword: String, term: String },
    /// `` [`name`] ``.
    Ref { name: String },
}

/// Parse a `` [`...`] `` form at the start of `s`. The payload is
/// either a directive (`@KEYWORD term`) or a plain ref name.
fn parse_kiodoc_bracket(s: &str) -> Option<KiodocBracket> {
    let bytes = s.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'[' || bytes[1] != b'`' {
        return None;
    }
    // The payload runs from byte 2 to the closing backtick.
    let mut j = 2;
    while j < bytes.len() && bytes[j] != b'`' && bytes[j] != b']' && bytes[j] != b'\n' {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b'`' {
        return None;
    }
    let payload = &s[2..j];
    // Closing backtick then `]`.
    if j + 1 >= bytes.len() || bytes[j + 1] != b']' {
        return None;
    }
    let consumed = j + 2;
    if payload.is_empty() {
        return None;
    }
    // Directive form: `@KEYWORD term` — keyword and term separated by
    // one or more whitespace characters.
    if let Some(rest) = payload.strip_prefix('@') {
        let space = rest.find(char::is_whitespace)?;
        let keyword = &rest[..space];
        let term = rest[space..].trim();
        // The keyword must be a non-empty run of letters, and the
        // term must be non-empty after trimming.
        if keyword.is_empty()
            || keyword.chars().any(|c| !c.is_ascii_alphabetic())
            || term.is_empty()
        {
            return None;
        }
        return Some(KiodocBracket {
            kind: BracketKind::Directive {
                keyword: keyword.to_owned(),
                term: term.to_owned(),
            },
            consumed,
        });
    }
    Some(KiodocBracket {
        kind: BracketKind::Ref {
            name: payload.to_owned(),
        },
        consumed,
    })
}

/// Render one parsed Kiodoc bracket to Markdown.
fn render_bracket(
    parsed: &KiodocBracket,
    url_prefix: &str,
    link_ext: &str,
    index: &SymbolIndex,
    scope: &dyn DirectiveScope,
    overrides: &HashSet<String>,
) -> String {
    match &parsed.kind {
        BracketKind::Directive { keyword, term } => {
            // An overridden term keeps the directive verbatim — the
            // author's reference-link definition is the rendered link.
            if overrides.contains(term) {
                return verbatim_bracket(parsed);
            }
            match keyword.as_str() {
                "signature" => match scope.signature(term) {
                    Some(sig) => format!("`{}`", sig.replace('`', "")),
                    None => verbatim_bracket(parsed),
                },
                "type" => match scope.ty(term) {
                    Some(ty) => format!("`{}`", ty.replace('`', "")),
                    None => verbatim_bracket(parsed),
                },
                "source" => match scope.source(term) {
                    Some(src) => format!("\n\n```kio\n{src}\n```\n\n"),
                    None => verbatim_bracket(parsed),
                },
                // `kio doc check` already rejected unknown keywords;
                // a render-time unknown keyword is left verbatim.
                _ => verbatim_bracket(parsed),
            }
        }
        BracketKind::Ref { name } => {
            if overrides.contains(name) {
                return verbatim_bracket(parsed);
            }
            let target = scope.link_target(name);
            let destination = match &target {
                LinkTarget::Exact(anchor) => index.lookup_anchor(anchor),
                LinkTarget::Plain => None,
                LinkTarget::LegacyName => index.lookup(name),
            };
            match destination {
                Some((url_stem, anchor)) => {
                    let href = format!("{url_prefix}{url_stem}{link_ext}#{anchor}");
                    format!("[`{name}`]({href})")
                }
                // Not documented in this package — render as a plain
                // code span per spec.
                None => format!("`{name}`"),
            }
        }
    }
}

/// Reconstruct the literal `` [`...`] `` text for a bracket the
/// rewriter chose not to transform.
fn verbatim_bracket(parsed: &KiodocBracket) -> String {
    match &parsed.kind {
        BracketKind::Directive { keyword, term } => {
            format!("[`@{keyword} {term}`]")
        }
        BracketKind::Ref { name } => format!("[`{name}`]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestScope;
    impl DirectiveScope for TestScope {
        fn link_target(&self, _term: &str) -> LinkTarget {
            LinkTarget::LegacyName
        }

        fn signature(&self, term: &str) -> Option<String> {
            if term == "add" {
                Some("fn add(x: Int) -> Int".to_owned())
            } else {
                None
            }
        }
        fn source(&self, term: &str) -> Option<String> {
            if term == "add" {
                Some("fn add(x: Int) -> Int { x }".to_owned())
            } else {
                None
            }
        }
        fn ty(&self, term: &str) -> Option<String> {
            if term == "add" {
                Some("Int -> Int".to_owned())
            } else {
                None
            }
        }
    }

    fn idx() -> SymbolIndex {
        let mut i = SymbolIndex::default();
        i.insert("add", "pkg/main", "item-add");
        i
    }

    #[test]
    fn ref_to_in_package_symbol_links() {
        let out = rewrite(
            "see [`add`] here",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "see [`add`](pkg/main.html#item-add) here");
    }

    #[test]
    fn inline_code_shields_reference_and_directive_rewriting() {
        let literal = "`[` or `]`; `` [`add`] [`@signature add`] ``";
        let out = rewrite(
            &format!("{literal}; [`add`]; [`@signature add`]"),
            "",
            ".md",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(
            out,
            format!("{literal}; [`add`](pkg/main.md#item-add); `fn add(x: Int) -> Int`")
        );
    }

    #[test]
    fn ref_to_unknown_symbol_is_code_span() {
        let out = rewrite(
            "see [`mystery`] here",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "see `mystery` here");
    }

    #[test]
    fn signature_directive_inlines() {
        let out = rewrite(
            "the [`@signature add`] form",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "the `fn add(x: Int) -> Int` form");
    }

    #[test]
    fn type_directive_inlines() {
        let out = rewrite(
            "the [`@type add`] form",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "the `Int -> Int` form");
    }

    #[test]
    fn source_directive_promotes_block() {
        let out = rewrite(
            "x [`@source add`] y",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert!(out.contains("```kio\nfn add(x: Int) -> Int { x }\n```"));
    }

    #[test]
    fn old_paren_directive_form_is_not_rewritten() {
        // The pre-paren-free spelling is no longer a directive — the
        // keyword/term separator is whitespace, not `(`. The `@`
        // prefix with no whitespace is not a plain ref either, so the
        // whole bracket form is left verbatim.
        let out = rewrite(
            "[`@signature(add)`]",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "[`@signature(add)`]");
    }

    #[test]
    fn url_prefix_applied() {
        let out = rewrite("[`add`]", "../", ".md", &idx(), &TestScope, &HashSet::new());
        assert_eq!(out, "[`add`](../pkg/main.md#item-add)");
    }

    #[test]
    fn override_keeps_directive_verbatim() {
        let mut overrides = HashSet::new();
        overrides.insert("add".to_owned());
        let out = rewrite(
            "see [`add`] and [`@signature add`]",
            "",
            ".md",
            &idx(),
            &TestScope,
            &overrides,
        );
        assert_eq!(out, "see [`add`] and [`@signature add`]");
    }

    #[test]
    fn markdown_inline_link_not_rewritten() {
        // `` [`add`](url) `` is a plain Markdown link — left alone.
        let out = rewrite(
            "[`add`](http://x.com)",
            "",
            ".html",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "[`add`](http://x.com)");
    }

    #[test]
    fn unresolved_directive_term_left_verbatim() {
        let out = rewrite(
            "[`@signature mystery`]",
            "",
            ".md",
            &idx(),
            &TestScope,
            &HashSet::new(),
        );
        assert_eq!(out, "[`@signature mystery`]");
    }

    #[test]
    fn fenced_forms_stay_literal_while_prose_is_rewritten() {
        let input = concat!(
            "prose [`add`] and [`@signature add`]\n",
            "```text\n",
            "[`add`] [`@signature add`]\n",
            "```\n",
            "<!--kio {ignore}\n",
            "[`add`] [`@source add`]\n",
            "-->\n",
        );
        let out = rewrite(input, "", ".html", &idx(), &TestScope, &HashSet::new());
        assert!(out.starts_with("prose [`add`](pkg/main.html#item-add) and `fn add"));
        assert!(out.contains("```text\n[`add`] [`@signature add`]\n```"));
        assert!(out.contains("<!--kio {ignore}\n[`add`] [`@source add`]\n-->"));
    }
}

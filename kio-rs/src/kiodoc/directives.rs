//! `@signature`, `@source`, and `@type` code-embedding directives for
//! Kiodoc.
//!
//! These directives embed live code into Kiodoc prose:
//!
//! - `` [`@signature term`] `` — resolves `term` in the active scope
//!   and, when rendered, inlines the item's pretty-printed declaration
//!   header. For `kio doc check`, validation only: the term must
//!   resolve.
//!
//! - `` [`@source term`] `` — resolves `term` in the active scope and,
//!   when rendered, block-promotes the item's full source (with its
//!   `///` doc-comment stripped). For `kio doc check`, validation
//!   only: the term must resolve.
//!
//! - `` [`@type term`] `` — resolves `term` in the active scope and,
//!   when rendered, inlines the *type of the named item's bound
//!   value*. For `kio doc check`, validation also checks that `term`
//!   names a value binding (`fn`, host fn, or exported fn): a
//!   type-level name (`type` / `labels` / `newtype`) binds no value and
//!   is a Kiodoc contract error.
//!
//! All three share the resolver from the `refs` module: the `term`
//! argument uses the same two-form name grammar (`simple-name` or
//! `qualified-path`) that `` [`name`] `` intra-doc links accept. The
//! keyword and the term are separated by one or more whitespace
//! characters — the same space-separated `KW term` argument shape the
//! `kio repl` meta-commands use; the two surfaces differ only by the
//! `@` / `:` sigil.
//!
//! Unknown directive keywords (e.g. `` [`@eval foo`] ``,
//! `` [`@signaure foo`] ``) are runner errors — they are not silently
//! ignored, which catches typos at the first opportunity.
//!
//! See [`specs/kiodoc.md`](../../../../specs/kiodoc.md)
//! § "Code-embedding directives" for the full contract.

use crate::span::Span;

/// The set of valid directive keywords in the `` [`@KEYWORD term`] ``
/// family. Extend this list when a new directive is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveKeyword {
    /// `` [`@signature term`] `` — embeds the item's declaration
    /// header inline.
    Signature,
    /// `` [`@source term`] `` — block-promotes the item's full source.
    Source,
    /// `` [`@type term`] `` — embeds the type of the item's bound
    /// value inline.
    Type,
}

impl DirectiveKeyword {
    /// Parse a keyword string, returning `None` for unknown keywords.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "signature" => Some(DirectiveKeyword::Signature),
            "source" => Some(DirectiveKeyword::Source),
            "type" => Some(DirectiveKeyword::Type),
            _ => None,
        }
    }

    /// The canonical lowercase spelling of the directive keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            DirectiveKeyword::Signature => "signature",
            DirectiveKeyword::Source => "source",
            DirectiveKeyword::Type => "type",
        }
    }
}

/// One `` [`@KEYWORD term`] `` directive found in prose text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// The resolved directive keyword.
    pub keyword: DirectiveKeyword,
    /// The `term` argument after the keyword.
    pub term: String,
    /// Byte offset of the `` [`@KEYWORD term`] `` pattern within the
    /// prose text block it was found in (not the overall source file).
    pub offset: u32,
}

/// An unknown directive keyword error — the keyword was not recognized.
/// This is a runner error (not a "term not found" error).
#[derive(Debug, Clone)]
pub struct UnknownDirective {
    /// The `@KEYWORD` text (without the `@`).
    pub keyword: String,
    /// The full term argument found after the keyword, or empty if
    /// the term was missing.
    pub term: String,
    /// Byte offset of the `` [`@KEYWORD …`] `` pattern.
    pub offset: u32,
}

/// An unresolved directive term — the keyword was recognized but the
/// `term` could not be resolved in the active scope.
#[derive(Debug, Clone)]
pub struct UnresolvedDirective {
    pub keyword: DirectiveKeyword,
    pub term: String,
    pub offset: u32,
}

// ---- Scanner ----------------------------------------------------------------

/// Scan `text` for `` [`@KEYWORD term`] `` directive patterns.
///
/// Returns two lists:
/// - `directives`: fully-parsed, keyword-recognized directives.
/// - `unknown`: patterns whose keyword is not in the known vocabulary.
///   These are always runner errors and must be reported to the author.
///
/// A directive is `` [`@KEYWORD term`] `` — bracket, backtick, `@`,
/// an alphabetic keyword, one or more whitespace characters, the term
/// (trimmed, running to the closing backtick), then `` `] ``.
/// Patterns that look like directives but are structurally malformed
/// (no `@`, no whitespace after the keyword, no closing backtick,
/// etc.) are silently skipped — they do not match the scanner's
/// pattern and are treated as prose.
///
/// The scanner is text-local and does not identify fences. Document-level
/// callers pass text through [`super::parse::blank_fences`] first.
pub fn scan_directives(text: &str) -> (Vec<Directive>, Vec<UnknownDirective>) {
    let mut directives = Vec::new();
    let mut unknown = Vec::new();

    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    while i < len {
        if let Some(end) = super::parse::inline_literal_end(text, i) {
            i = end;
            continue;
        }
        // Look for `[`.
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let bracket_start = i;
        i += 1;
        // Must be followed by a backtick.
        if i >= len || bytes[i] != b'`' {
            continue;
        }
        i += 1;
        // Must be followed by `@`.
        if i >= len || bytes[i] != b'@' {
            // Not a directive; rewind to let the ref scanner pick up
            // a plain `` [`name`] `` if applicable. We just skip past
            // the `[` — the ref scanner is separate.
            continue;
        }
        let at_pos = i;
        i += 1; // consume `@`

        // Scan the keyword: letters only.
        let kw_start = i;
        while i < len && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let kw_end = i;
        let keyword_str = &text[kw_start..kw_end];

        if keyword_str.is_empty() {
            // `[`@`] with no keyword — not a directive pattern.
            i = at_pos + 1;
            continue;
        }

        // Must be followed by at least one whitespace character —
        // this separates the keyword from the term.
        if i >= len || (bytes[i] != b' ' && bytes[i] != b'\t') {
            // `[`@KEYWORD…`]` with no space after the keyword — not a
            // directive pattern.
            continue;
        }
        // Consume the run of separating whitespace.
        while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }

        // Scan the term to the closing backtick, allowing any
        // characters except newline and `]`.
        let arg_start = i;
        while i < len && bytes[i] != b'`' && bytes[i] != b']' && bytes[i] != b'\n' {
            i += 1;
        }
        if i >= len || bytes[i] != b'`' {
            // No closing backtick — not a directive pattern.
            continue;
        }
        let arg_end = i;
        i += 1; // consume the closing backtick

        // Must be followed by `]`.
        if i >= len || bytes[i] != b']' {
            continue;
        }
        i += 1;

        let term = text[arg_start..arg_end].trim().to_owned();
        if term.is_empty() {
            // `` [`@KEYWORD   `] `` — whitespace but no term. Not a
            // directive pattern.
            continue;
        }

        // Skip if followed by `(` or `[` — those are standard Markdown
        // inline/reference links that just happen to start with
        // `` [`@... ``.
        let peek = text[i..].trim_start();
        if peek.starts_with('(') || peek.starts_with('[') {
            continue;
        }

        let offset = bracket_start as u32;

        match DirectiveKeyword::parse(keyword_str) {
            Some(keyword) => {
                directives.push(Directive {
                    keyword,
                    term,
                    offset,
                });
            }
            None => {
                unknown.push(UnknownDirective {
                    keyword: keyword_str.to_owned(),
                    term,
                    offset,
                });
            }
        }
    }

    (directives, unknown)
}

// ---- Resolution wrappers ---------------------------------------------------

/// Check directive terms in `text` against a module scope, returning
/// one error per unresolved term, one per unknown keyword, and one
/// per `@type` directive against a type-level name.
///
/// `overrides` is the set of reference-link definition keys — keys
/// present there are skipped (same override semantics as intra-doc
/// refs, see [`super::refs::check_refs_in_module`]).
///
/// Returns `(unknown_keyword_errors, unresolved_term_errors,
/// type_level_errors)`.
pub fn check_directives_in_module(
    text: &str,
    scope: &super::refs::ModuleScope,
    overrides: &std::collections::HashSet<String>,
) -> (
    Vec<UnknownDirectiveError>,
    Vec<UnresolvedDirectiveError>,
    Vec<TypeLevelDirectiveError>,
) {
    let (dirs, unk) = scan_directives(text);
    let mut unknown_errs: Vec<UnknownDirectiveError> = unk
        .into_iter()
        .map(|u| UnknownDirectiveError {
            keyword: u.keyword,
            offset: u.offset,
        })
        .collect();
    let mut unresolved_errs: Vec<UnresolvedDirectiveError> = Vec::new();
    let mut type_level_errs: Vec<TypeLevelDirectiveError> = Vec::new();

    for d in dirs {
        // Override key is the full term, prefix-less.
        if overrides.contains(&d.term) {
            continue;
        }
        match super::refs::resolve_in_module(&d.term, scope) {
            super::refs::RefOutcome::Resolved | super::refs::RefOutcome::Overridden => {
                // A resolved `@type` term must name a value binding,
                // not a type-level name.
                if d.keyword == DirectiveKeyword::Type
                    && let Some(kind) = super::refs::type_level_kind_in_module(&d.term, scope)
                {
                    type_level_errs.push(TypeLevelDirectiveError {
                        term: d.term,
                        kind,
                        offset: d.offset,
                    });
                }
            }
            super::refs::RefOutcome::Unresolved => {
                unresolved_errs.push(UnresolvedDirectiveError {
                    keyword: d.keyword,
                    term: d.term,
                    offset: d.offset,
                });
            }
        }
    }
    // Sort all lists for deterministic output.
    unknown_errs.sort_by_key(|e| e.offset);
    unresolved_errs.sort_by_key(|e| e.offset);
    type_level_errs.sort_by_key(|e| e.offset);
    (unknown_errs, unresolved_errs, type_level_errs)
}

/// Check directive terms in `text` against a package scope.
/// Returns `(unknown_keyword_errors, unresolved_term_errors,
/// type_level_errors)`.
pub fn check_directives_in_package(
    text: &str,
    scope: &super::refs::PackageScope,
    overrides: &std::collections::HashSet<String>,
) -> (
    Vec<UnknownDirectiveError>,
    Vec<UnresolvedDirectiveError>,
    Vec<TypeLevelDirectiveError>,
) {
    let (dirs, unk) = scan_directives(text);
    let mut unknown_errs: Vec<UnknownDirectiveError> = unk
        .into_iter()
        .map(|u| UnknownDirectiveError {
            keyword: u.keyword,
            offset: u.offset,
        })
        .collect();
    let mut unresolved_errs: Vec<UnresolvedDirectiveError> = Vec::new();
    let mut type_level_errs: Vec<TypeLevelDirectiveError> = Vec::new();

    for d in dirs {
        if overrides.contains(&d.term) {
            continue;
        }
        match super::refs::resolve_in_package(&d.term, scope) {
            super::refs::RefOutcome::Resolved | super::refs::RefOutcome::Overridden => {
                if d.keyword == DirectiveKeyword::Type
                    && let Some(kind) = super::refs::type_level_kind_in_package(&d.term, scope)
                {
                    type_level_errs.push(TypeLevelDirectiveError {
                        term: d.term,
                        kind,
                        offset: d.offset,
                    });
                }
            }
            super::refs::RefOutcome::Unresolved => {
                unresolved_errs.push(UnresolvedDirectiveError {
                    keyword: d.keyword,
                    term: d.term,
                    offset: d.offset,
                });
            }
        }
    }
    unknown_errs.sort_by_key(|e| e.offset);
    unresolved_errs.sort_by_key(|e| e.offset);
    type_level_errs.sort_by_key(|e| e.offset);
    (unknown_errs, unresolved_errs, type_level_errs)
}

// ---- Error types -----------------------------------------------------------

/// An unknown directive keyword — `` [`@KEYWORD term`] `` where
/// `KEYWORD` is not in the recognized vocabulary.
#[derive(Debug, Clone)]
pub struct UnknownDirectiveError {
    /// The unrecognized keyword (without the `@`).
    pub keyword: String,
    /// Byte offset within the prose text block.
    pub offset: u32,
}

/// An unresolved directive term — the keyword was recognized but
/// `term` could not be found in the active scope.
#[derive(Debug, Clone)]
pub struct UnresolvedDirectiveError {
    pub keyword: DirectiveKeyword,
    pub term: String,
    pub offset: u32,
}

/// A `` [`@type term`] `` directive whose `term` resolves to a
/// type-level name (`type` / `labels` / `newtype` / host type /
/// exported type). A type-level name binds no value, so `@type`
/// against one is a Kiodoc contract error.
#[derive(Debug, Clone)]
pub struct TypeLevelDirectiveError {
    /// The type-level term `@type` was applied to.
    pub term: String,
    /// The kind of the type-level name — names the kind in the
    /// diagnostic.
    pub kind: super::refs::TypeLevelKind,
    /// Byte offset within the prose text block.
    pub offset: u32,
}

// ---- Diagnostics -----------------------------------------------------------

/// Format the "unknown directive keyword" message.
pub fn format_unknown_directive_message(keyword: &str) -> String {
    format!(
        "unknown Kiodoc directive `@{keyword}`: \
         expected `@signature`, `@source`, or `@type`"
    )
}

/// Format the "unresolved directive term" message. Consistent with the
/// unresolved intra-doc reference format from [`super::refs`].
pub fn format_unresolved_directive_message(
    keyword: DirectiveKeyword,
    term: &str,
    suggestion: Option<&str>,
) -> String {
    let base = format!(
        "unresolved `@{}` directive: `{}` is not in scope",
        keyword.as_str(),
        term
    );
    match suggestion {
        Some(s) => format!("{base}; did you mean `{s}`?"),
        None => base,
    }
}

/// Format the "`@type` on a type-level name" message. Names the kind
/// found and what `@type` expects.
pub fn format_type_level_directive_message(term: &str, kind: super::refs::TypeLevelKind) -> String {
    format!(
        "`{term}` is {}; `@type` expects a value binding (fn, host fn, exported fn)",
        kind.describe()
    )
}

/// Byte span for a directive, computed from a base offset plus the
/// directive's intra-text offset.
pub fn directive_span(base_offset: u32, directive_offset: u32) -> Span {
    Span::new(
        base_offset + directive_offset,
        base_offset + directive_offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_signature_directive() {
        let (dirs, unk) = scan_directives("See [`@signature foo`] for details.");
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].keyword, DirectiveKeyword::Signature);
        assert_eq!(dirs[0].term, "foo");
        assert!(unk.is_empty());
    }

    #[test]
    fn inline_code_shields_directive_patterns() {
        let text = "`` [`@signature missing`] [`@unknown missing`] ``; [`@signature present`]";
        let (dirs, unknown) = scan_directives(text);
        assert!(unknown.is_empty(), "{unknown:?}");
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        assert_eq!(dirs[0].term, "present");
        assert_eq!(
            dirs[0].offset,
            text.find("[`@signature present`]").unwrap() as u32
        );
    }

    #[test]
    fn scan_source_directive() {
        let (dirs, unk) = scan_directives("Code: [`@source bar.baz`].");
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].keyword, DirectiveKeyword::Source);
        assert_eq!(dirs[0].term, "bar.baz");
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_type_directive() {
        let (dirs, unk) = scan_directives("Type: [`@type foo`].");
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].keyword, DirectiveKeyword::Type);
        assert_eq!(dirs[0].term, "foo");
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_unknown_keyword() {
        let (dirs, unk) = scan_directives("[`@eval foo`]");
        assert!(dirs.is_empty());
        assert_eq!(unk.len(), 1);
        assert_eq!(unk[0].keyword, "eval");
        assert_eq!(unk[0].term, "foo");
    }

    #[test]
    fn scan_unknown_typo() {
        let (dirs, unk) = scan_directives("[`@signaure foo`]");
        assert!(dirs.is_empty());
        assert_eq!(unk.len(), 1);
        assert_eq!(unk[0].keyword, "signaure");
    }

    #[test]
    fn scan_plain_intraref_not_picked_up() {
        // Plain `` [`name`] `` without `@` should not be picked up as a
        // directive.
        let (dirs, unk) = scan_directives("[`foo`]");
        assert!(dirs.is_empty());
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_multiple_directives() {
        let text = "Use [`@signature f`] and [`@source g`].";
        let (dirs, unk) = scan_directives(text);
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0].keyword, DirectiveKeyword::Signature);
        assert_eq!(dirs[0].term, "f");
        assert_eq!(dirs[1].keyword, DirectiveKeyword::Source);
        assert_eq!(dirs[1].term, "g");
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_whitespace_trimmed_in_term() {
        let (dirs, _) = scan_directives("[`@signature   my_fn  `]");
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].term, "my_fn");
    }

    #[test]
    fn scan_tab_between_keyword_and_term() {
        let (dirs, _) = scan_directives("[`@signature\tmy_fn`]");
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].term, "my_fn");
    }

    #[test]
    fn scan_inline_link_skipped() {
        // [`@signature foo`](url) is a Markdown inline link, not a directive.
        let (dirs, unk) = scan_directives("[`@signature foo`](http://example.com)");
        assert!(dirs.is_empty());
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_no_space_not_a_directive() {
        // `` [`@signature`] `` with no whitespace / term is not a
        // directive — the paren-free form requires `keyword term`.
        let (dirs, unk) = scan_directives("[`@signature`]");
        assert!(dirs.is_empty());
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_old_paren_form_not_a_directive() {
        // The pre-paren-free spelling `` [`@signature(foo)`] `` is no
        // longer a directive: there is no whitespace after the
        // keyword, so the scanner skips it.
        let (dirs, unk) = scan_directives("[`@signature(foo)`]");
        assert!(dirs.is_empty());
        assert!(unk.is_empty());
    }

    #[test]
    fn scan_whitespace_only_term_not_a_directive() {
        let (dirs, unk) = scan_directives("[`@signature   `]");
        assert!(dirs.is_empty());
        assert!(unk.is_empty());
    }

    #[test]
    fn format_unknown_message() {
        let msg = format_unknown_directive_message("eval");
        assert!(msg.contains("@eval"));
        assert!(msg.contains("@signature"));
        assert!(msg.contains("@source"));
        assert!(msg.contains("@type"));
    }

    #[test]
    fn format_unresolved_message_no_suggestion() {
        let msg = format_unresolved_directive_message(DirectiveKeyword::Signature, "foo", None);
        assert!(msg.contains("@signature"));
        assert!(msg.contains("`foo`"));
    }

    #[test]
    fn format_unresolved_message_with_suggestion() {
        let msg = format_unresolved_directive_message(DirectiveKeyword::Source, "fo", Some("foo"));
        assert!(msg.contains("did you mean `foo`"));
    }

    #[test]
    fn format_type_level_message_names_kind() {
        use super::super::refs::TypeLevelKind;
        let msg = format_type_level_directive_message("Cnf", TypeLevelKind::TypeAlias);
        assert!(msg.contains("`Cnf`"));
        assert!(msg.contains("type alias"));
        assert!(msg.contains("value binding"));
        // The `labels` kind names itself distinctly.
        let labels_msg = format_type_level_directive_message("Color", TypeLevelKind::Labels);
        assert!(labels_msg.contains("`labels`"));
    }

    #[test]
    fn type_directive_against_value_binding_resolves() {
        let scope = super::super::refs::ModuleScope {
            top_level: vec!["compute".to_owned()],
            ..Default::default()
        };
        let (unk, unresolved, type_level) =
            check_directives_in_module("[`@type compute`]", &scope, &Default::default());
        assert!(unk.is_empty());
        assert!(unresolved.is_empty());
        assert!(type_level.is_empty());
    }

    #[test]
    fn type_directive_against_type_level_name_errors() {
        use super::super::refs::TypeLevelKind;
        let scope = super::super::refs::ModuleScope {
            top_level: vec!["Cnf".to_owned()],
            type_level: vec![("Cnf".to_owned(), TypeLevelKind::TypeAlias)],
            ..Default::default()
        };
        let (unk, unresolved, type_level) =
            check_directives_in_module("[`@type Cnf`]", &scope, &Default::default());
        assert!(unk.is_empty());
        assert!(unresolved.is_empty());
        assert_eq!(type_level.len(), 1);
        assert_eq!(type_level[0].term, "Cnf");
        assert_eq!(type_level[0].kind, TypeLevelKind::TypeAlias);
    }

    #[test]
    fn forwarding_directives_select_the_declaration_without_a_value_type() {
        let module = crate::pass::parser::parse(
            "module api; fn field() -> . { () } type {field} = {original};",
        )
        .unwrap();
        let scope = super::super::refs::module_scope_from_surface(&module);
        let text =
            "[`@signature {field}`]. [`@source {field}`]. [`@type {field}`]. [`@type field`].";
        let (directives, unknown) = scan_directives(text);
        assert_eq!(directives.len(), 4);
        assert!(unknown.is_empty());
        let (unknown, unresolved, nonvalue) =
            check_directives_in_module(text, &scope, &Default::default());
        assert!(unknown.is_empty());
        assert!(unresolved.is_empty());
        assert_eq!(nonvalue.len(), 1);
        assert_eq!(nonvalue[0].term, "{field}");
        assert_eq!(
            format_type_level_directive_message(&nonvalue[0].term, nonvalue[0].kind),
            "`{field}` is a label declaration; `@type` expects a value binding (fn, host fn, exported fn)"
        );
    }

    #[test]
    fn signature_directive_against_type_level_name_is_fine() {
        use super::super::refs::TypeLevelKind;
        // `@signature` on a type-level name is *not* an error — only
        // `@type` rejects type-level names.
        let scope = super::super::refs::ModuleScope {
            top_level: vec!["Cnf".to_owned()],
            type_level: vec![("Cnf".to_owned(), TypeLevelKind::TypeAlias)],
            ..Default::default()
        };
        let (unk, unresolved, type_level) =
            check_directives_in_module("[`@signature Cnf`]", &scope, &Default::default());
        assert!(unk.is_empty());
        assert!(unresolved.is_empty());
        assert!(type_level.is_empty());
    }
}

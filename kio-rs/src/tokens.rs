//! The production token classifier for Kio source highlighting.
//!
//! Walks the lexer output, lifts comment-line trivia into first-class
//! tokens, and classifies each emitted token against the canonical
//! token-kind vocabulary defined by [`TokenKind`] below. This is the
//! single richest classifier in the tree: `kio doc`'s semantic HTML
//! highlighting, the LSP semantic-tokens provider, and the REPL
//! truecolor highlighter all consume this classifier (the latter two as lossy
//! projections), and the JSON dump (`kio debug tokens`) is the
//! operational source of truth other highlighter implementations
//! (tree-sitter, TextMate, …) are checked against.
//!
//! [`TokenKind`] *is* the vocabulary: every variant maps to one
//! dot-separated kebab string in the JSON dump (see [`TokenKind::as_str`])
//! and a hyphenated `kio-`-prefixed CSS class (see
//! [`TokenKind::css_class`]); adding or retiring a row means editing
//! this enum and regenerating the `test-data/highlight-corpus/`
//! fixtures.
//!
//! The classifier lives here rather than in [`crate::pass::lexer`]
//! because the lexer's `TokenKind` (imported as [`LexTokenKind`]) is a
//! parser-facing enum and the highlighter's kind taxonomy is
//! cross-cutting (bang-call elaborator fusion, parser-contextual mapping of
//! forall `[` / `]` bytes inside operator runs, etc.) that doesn't belong on
//! the parser-facing type.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::ast;
use crate::error::Error;
use crate::pass::lexer::{Token as LexToken, TokenKind as LexTokenKind, Trivia, lex};
use crate::span::Span;

/// One classified token: a source span paired with its [`TokenKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedToken {
    pub span: Span,
    pub kind: TokenKind,
}

#[derive(Debug)]
#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) struct RecMemberTokenFact {
    pub name: String,
    pub name_span: Span,
    pub calls: Vec<(String, Span)>,
}

/// Canonical token-kind vocabulary. Each variant maps to one
/// dot-separated kebab string in the JSON dump (see [`as_str`]) and one
/// hyphenated, `kio-`-prefixed CSS class for `kio doc`'s highlighted
/// HTML (see [`css_class`]). The two spellings are locked together so a
/// future kind add/retire moves the wire string and the CSS class in
/// lockstep.
///
/// Adding or retiring a variant is the contract change — downstream
/// highlighter implementations and the fixtures under
/// `test-data/highlight-corpus/` must move in lockstep. Per
/// `TESTING.md` § Tokenization or vocabulary change, regenerate the
/// fixtures with `sh ci/checks/orchestrators/highlight-tokens.sh -u` in the same
/// change.
///
/// [`as_str`]: TokenKind::as_str
/// [`css_class`]: TokenKind::css_class
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    KeywordControl,
    KeywordDeclaration,
    KeywordElaborator,
    Identifier,
    OperatorBuiltin,
    OperatorUser,
    LiteralString,
    LiteralNumber,
    LiteralBool,
    CommentLine,
    CommentDoc,
    PunctuationBracket,
    PunctuationSeparator,
    Slot,
    /// Module-path segments (the IDENTs in `module a/b/c;`, in
    /// `import a/b(items);`'s path, in `import a/b/c as m;`, and the
    /// alias target). Reserved builtin providers such as
    /// `import __intrinsics__;` occupy the same module-name slot.
    EntityNameModule,
    /// Function definition names (`fn foo` / `host fn foo`).
    EntityNameFunction,
    /// References to callable values: call-site callee names and declaration
    /// targets such as an `op` implementation or a variadic step. This retains
    /// the same wire/CSS class as a function definition while allowing LSP
    /// semantic tokens to omit the definition-only `declaration` modifier.
    EntityNameFunctionReference,
    /// Type definition names (`newtype Box`, `type List`, `host type Foo`).
    /// The classifier keys off definition sites: a type *reference* in
    /// code (a `Box` in `let x: Box(Int) = …`) is not classified here
    /// and stays `Identifier`.
    EntityNameType,
    /// Label spellings inside `labels { foo: T, bar: U }` blocks — the
    /// closest analogue to Rust's struct field names.
    EntityNameLabel,
    /// A label reference: a reuse marker (`foo: _`), a selective import, or
    /// an unqualified label in an expression. This keeps the static token
    /// wire vocabulary identical to [`EntityNameLabel`](Self::EntityNameLabel),
    /// while letting semantic tokens distinguish references from declarations
    /// without parsing the source a second time.
    EntityNameLabelReference,
    /// A label reference whose parsed path carries an explicit module
    /// qualifier. The wire and presentation kinds remain
    /// `entity.name.label`; LSP binder repair uses the parsed distinction
    /// so trivia between the qualifier and leaf cannot erase identity.
    EntityNameQualifiedLabelReference,
    /// Function-parameter binders (the `x` in `fn foo(x: T)`).
    /// Parameter *uses* in the body stay `Identifier`.
    VariableParameter,
}

impl TokenKind {
    /// Wire-form spelling — the dot-separated string that appears in the
    /// JSON dump and is the stable contract shared with the TextMate /
    /// tree-sitter highlighters.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenKind::KeywordControl => "keyword.control",
            TokenKind::KeywordDeclaration => "keyword.declaration",
            TokenKind::KeywordElaborator => "keyword.elaborator",
            TokenKind::Identifier => "identifier",
            TokenKind::OperatorBuiltin => "operator.builtin",
            TokenKind::OperatorUser => "operator.user",
            TokenKind::LiteralString => "literal.string",
            TokenKind::LiteralNumber => "literal.number",
            TokenKind::LiteralBool => "literal.bool",
            TokenKind::CommentLine => "comment.line",
            TokenKind::CommentDoc => "comment.doc",
            TokenKind::PunctuationBracket => "punctuation.bracket",
            TokenKind::PunctuationSeparator => "punctuation.separator",
            TokenKind::Slot => "slot",
            TokenKind::EntityNameModule => "entity.name.module",
            TokenKind::EntityNameFunction => "entity.name.function",
            TokenKind::EntityNameFunctionReference => "entity.name.function",
            TokenKind::EntityNameType => "entity.name.type",
            TokenKind::EntityNameLabel => "entity.name.label",
            TokenKind::EntityNameLabelReference => "entity.name.label",
            TokenKind::EntityNameQualifiedLabelReference => "entity.name.label",
            TokenKind::VariableParameter => "variable.parameter",
        }
    }

    /// CSS class for `kio doc`'s highlighted HTML — the [`as_str`] wire
    /// string with each `.` rewritten to `-` and a `kio-` prefix
    /// (`entity.name.function` → `kio-entity-name-function`). The
    /// default stylesheet themes these via `--kio-tok-*` custom
    /// properties; the class scheme is the rendering contract the
    /// website's stylesheet targets.
    ///
    /// Co-located with [`as_str`] so the wire vocabulary and the CSS
    /// vocabulary stay locked together.
    ///
    /// [`as_str`]: TokenKind::as_str
    pub fn css_class(self) -> &'static str {
        match self {
            TokenKind::KeywordControl => "kio-keyword-control",
            TokenKind::KeywordDeclaration => "kio-keyword-declaration",
            TokenKind::KeywordElaborator => "kio-keyword-elaborator",
            TokenKind::Identifier => "kio-identifier",
            TokenKind::OperatorBuiltin => "kio-operator-builtin",
            TokenKind::OperatorUser => "kio-operator-user",
            TokenKind::LiteralString => "kio-literal-string",
            TokenKind::LiteralNumber => "kio-literal-number",
            TokenKind::LiteralBool => "kio-literal-bool",
            TokenKind::CommentLine => "kio-comment-line",
            TokenKind::CommentDoc => "kio-comment-doc",
            TokenKind::PunctuationBracket => "kio-punctuation-bracket",
            TokenKind::PunctuationSeparator => "kio-punctuation-separator",
            TokenKind::Slot => "kio-slot",
            TokenKind::EntityNameModule => "kio-entity-name-module",
            TokenKind::EntityNameFunction => "kio-entity-name-function",
            TokenKind::EntityNameFunctionReference => "kio-entity-name-function",
            TokenKind::EntityNameType => "kio-entity-name-type",
            TokenKind::EntityNameLabel => "kio-entity-name-label",
            TokenKind::EntityNameLabelReference => "kio-entity-name-label",
            TokenKind::EntityNameQualifiedLabelReference => "kio-entity-name-label",
            TokenKind::VariableParameter => "kio-variable-parameter",
        }
    }
}

/// Run the lexer over `source` and produce the highlighter's token
/// stream.
///
/// CLI tooling consumes the parser's full syntax or its retained prefix facts.
/// Contextual keywords are colored only at positions proved by that parser;
/// other identifiers stay neutral. Output remains in source order, including
/// comments and tokens beyond a parse failure.
pub fn dump(source: &str) -> Result<Vec<ClassifiedToken>, Error> {
    let raw = lex(source)?;
    #[cfg(any(test, feature = "cli"))]
    {
        let probe = crate::pass::parser::probe_tooling_source(source, None);
        Ok(dump_with_tooling(&raw, &probe))
    }
    #[cfg(not(any(test, feature = "cli")))]
    {
        if let Ok((module, _errors)) = crate::pass::parser::parse_module(source, true) {
            return Ok(dump_with_ast(&raw, &module, source));
        }
        if let Ok(module) = crate::pass::parser::parse_lazy(source) {
            return Ok(dump_with_ast(&raw, module.module(), source));
        }
        Ok(dump_with_ast_overrides(
            &raw,
            source,
            &AstOverrides::default(),
        ))
    }
}

#[cfg(any(test, feature = "cli"))]
fn dump_with_tooling(
    raw: &[LexToken],
    probe: &crate::pass::parser::ToolingProbe<'_>,
) -> Vec<ClassifiedToken> {
    use crate::pass::parser::{KeywordRole, SourceNameRole, ToolingSyntax};

    let mut overrides = AstOverrides::default();
    // Prefix roles are refined by completed AST carriers below: for example,
    // a label head becomes a reference when its payload is a reuse marker.
    for fact in &probe.facts.source_names {
        overrides.insert(
            fact.span.start,
            match fact.role {
                SourceNameRole::Module => TokenKind::EntityNameModule,
                SourceNameRole::Function => TokenKind::EntityNameFunction,
                SourceNameRole::Type => TokenKind::EntityNameType,
                SourceNameRole::Label => TokenKind::EntityNameLabel,
                SourceNameRole::LabelReference => TokenKind::EntityNameLabelReference,
                SourceNameRole::QualifiedLabelReference => {
                    TokenKind::EntityNameQualifiedLabelReference
                }
                SourceNameRole::FunctionReference => TokenKind::EntityNameFunctionReference,
                SourceNameRole::Parameter => TokenKind::VariableParameter,
            },
        );
    }
    if let Some(syntax) = &probe.syntax {
        match syntax {
            ToolingSyntax::Module(module) => walk_module(module, raw, &mut overrides),
            ToolingSyntax::Declarations { imports, items } => {
                for import in imports {
                    walk_import(import, raw, &mut overrides);
                }
                for item in items {
                    walk_item(item, raw, &mut overrides);
                }
            }
            ToolingSyntax::Package(package) => {
                mark_file_name(
                    &package.name,
                    "package",
                    package.meta.span,
                    raw,
                    &mut overrides,
                );
            }
            ToolingSyntax::Signature(signature) => {
                walk_signature_file(signature, raw, &mut overrides);
            }
            ToolingSyntax::Dependency(dependency) => {
                mark_file_name(
                    &dependency.name,
                    "dependency",
                    dependency.meta.span,
                    raw,
                    &mut overrides,
                );
                for mapping in &dependency.rehost {
                    mark_module_path(&mapping.from, &mut overrides);
                    mark_module_path(&mapping.to, &mut overrides);
                }
                for mapping in &dependency.retype {
                    mark_module_path(&mapping.from, &mut overrides);
                    mark_module_path(&mapping.to, &mut overrides);
                }
            }
            ToolingSyntax::Lock(lock) => {
                mark_file_name(&lock.name, "lock", lock.meta.span, raw, &mut overrides);
            }
            ToolingSyntax::Expression(expression) => walk_expr(expression, raw, &mut overrides),
        }
    } else {
        if let Some(path) = &probe.facts.module_path {
            mark_module_path(path, &mut overrides);
        }
        for import in &probe.facts.prefix_imports {
            walk_import(import, raw, &mut overrides);
        }
        for item in &probe.facts.prefix_items {
            walk_item(item, raw, &mut overrides);
        }
    }
    for fact in &probe.facts.keywords {
        overrides.insert(
            fact.span.start,
            match fact.role {
                KeywordRole::Declaration => TokenKind::KeywordDeclaration,
                KeywordRole::Control => TokenKind::KeywordControl,
            },
        );
    }
    overrides
        .structural_brackets
        .extend(&probe.facts.structural_forall.bracket_offsets);
    dump_with_ast_overrides(raw, probe.source, &overrides)
}

#[cfg(any(test, feature = "cli"))]
fn mark_file_name(
    name: &str,
    keyword: &str,
    span: Span,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    if let Some(span) = find_name_after_kw(raw, span, keyword, name) {
        overrides.insert(span.start, TokenKind::Identifier);
    }
}

#[cfg(any(test, feature = "cli"))]
fn mark_module_path(path: &ast::ModulePath, overrides: &mut AstOverrides) {
    for segment in &path.segments {
        overrides.insert(segment.span.start, TokenKind::EntityNameModule);
    }
}

#[cfg(any(test, feature = "cli"))]
fn walk_signature_file(
    signature: &ast::SignatureFile,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    for version in &signature.versions {
        for section in &version.with {
            walk_signature_module(section, raw, overrides);
        }
        for changes in version.breaking.iter().chain(&version.nonbreaking) {
            for section in changes.add.iter().chain(&changes.modify) {
                walk_signature_module(section, raw, overrides);
            }
            for section in &changes.remove {
                mark_module_path(&section.path, overrides);
            }
            for reference in changes
                .add_refs
                .iter()
                .chain(&changes.modify_refs)
                .chain(&changes.remove_refs)
            {
                mark_module_path(&reference.path, overrides);
            }
        }
    }
}

#[cfg(any(test, feature = "cli"))]
fn walk_signature_module(
    section: &ast::SigModuleSection,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    mark_module_path(&section.path, overrides);
    for import in &section.imports {
        walk_import(import, raw, overrides);
    }
    for item in &section.items {
        match item {
            ast::SigItem::HostType(host) => walk_host_type(host, raw, overrides),
            ast::SigItem::HostFn(host) => walk_host_fn(host, raw, overrides),
            ast::SigItem::ExportFn(export) => walk_host_fn(&export.function, raw, overrides),
            ast::SigItem::TypeAlias(alias) => walk_alias(alias, raw, overrides),
            ast::SigItem::Newtype(newtype) => walk_newtype(newtype, raw, overrides),
            ast::SigItem::TypeRecGroup(group) => walk_type_rec_group(group, raw, overrides),
        }
    }
}

/// Classify a body-less declaration signature using the parser's declaration
/// root. Completed header facts remain usable when the omitted body prevents
/// a complete item; no synthetic module or body contributes source authority.
#[cfg(all(feature = "surface", feature = "cli"))]
pub(crate) fn dump_item_signature(signature: &str) -> Result<Vec<ClassifiedToken>, Error> {
    dump(signature)
}

/// Return parser-backed classifications only when either the eager parser or
/// its lazy-header counterpart accepts the complete module. The lazy path is
/// important for declarations whose deferred bodies are malformed; falling back to [`dump`] here would make lexical output look
/// like parser confirmation.
#[cfg(test)]
fn dump_if_module_parses(source: &str) -> Result<Option<Vec<ClassifiedToken>>, Error> {
    if let Ok(module) = crate::pass::parser::parse(source) {
        let raw = lex(source)?;
        return Ok(Some(dump_with_ast(&raw, &module, source)));
    }
    if let Ok(module) = crate::pass::parser::parse_lazy(source) {
        let raw = lex(source)?;
        return Ok(Some(dump_with_ast(&raw, module.module(), source)));
    }
    Ok(None)
}

/// Classify a live REPL line, parsing only the command's expression slice as
/// a fragment when the whole `:command ...` line is not Kio syntax. This keeps
/// compact forall brackets structural without teaching the language-level
/// token dumper about CLI command names.
#[cfg(feature = "repl-core")]
pub(crate) fn dump_repl_input(
    source: &str,
    expression_start: usize,
    context: Option<&crate::pass::parser::ExpressionParseContext>,
) -> Result<Vec<ClassifiedToken>, Error> {
    let Some(fragment) = source.get(expression_start..) else {
        return dump(source);
    };
    let prefix = &source[..expression_start];
    let mut out = dump_lexical(&lex(prefix)?, prefix);
    let raw = lex(fragment)?;
    let probe = crate::pass::parser::probe_tooling(fragment, None, None, context);
    out.extend(
        dump_with_tooling(&raw, &probe)
            .into_iter()
            .map(|mut token| {
                token.span.start += expression_start as u32;
                token.span.end += expression_start as u32;
                token
            }),
    );
    Ok(out)
}

/// Classify a standalone type through the type grammar, including its forall
/// binders. Type-producing REPL surfaces select this explicit root.
#[cfg(feature = "repl-core")]
pub(crate) fn dump_type_fragment(source: &str) -> Result<Vec<ClassifiedToken>, Error> {
    let raw = lex(source)?;
    let ty = crate::pass::parser::parse_type_fragment(source)?;
    let mut overrides = AstOverrides::default();
    walk_type(&ty, &raw, &mut overrides);
    let mut out = dump_lexical(&raw, source);
    for token in &mut out {
        if let Some(kind) = overrides.get(&token.span.start) {
            token.kind = *kind;
        }
    }
    Ok(split_structural_brackets(
        out,
        &overrides.structural_brackets,
        source,
    ))
}

/// Classify `source` using a module parsed with its surrounding package
/// context. Tooling that already resolved imported operator declarations must
/// use this entry point so a valid operator-bearing body does not degrade to
/// lexical classification.
pub fn dump_with_module(
    source: &str,
    module: &ast::Module<ast::Surface>,
) -> Result<Vec<ClassifiedToken>, Error> {
    let raw = lex(source)?;
    Ok(dump_with_ast(&raw, module, source))
}

/// Classify a source buffer against an already-cached Surface parse. LSP
/// analysis uses this entry point so label-navigation indexing shares the
/// document parse instead of parsing again through [`dump`].
#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn dump_module(
    source: &str,
    module: &ast::Module<ast::Surface>,
) -> Result<Vec<ClassifiedToken>, Error> {
    let raw = lex(source)?;
    Ok(dump_with_ast(&raw, module, source))
}

#[cfg(all(feature = "surface", feature = "lsp"))]
pub(crate) fn dump_module_with_rec_facts(
    source: &str,
    module: &ast::Module<ast::Surface>,
) -> Result<(Vec<ClassifiedToken>, Vec<RecMemberTokenFact>), Error> {
    let raw = lex(source)?;
    Ok(dump_with_ast_and_rec_facts(&raw, module, source))
}

/// Lex-only classification retains neutral identifiers without grammar claims.
#[cfg(any(test, feature = "repl-core"))]
fn dump_lexical(raw: &[LexToken], source: &str) -> Vec<ClassifiedToken> {
    dump_with_ast_overrides(raw, source, &AstOverrides::default())
}

fn is_ident_named(token: &LexToken, expected: &str) -> bool {
    ident_text(token) == Some(expected)
}

fn ident_text(token: &LexToken) -> Option<&str> {
    match &token.kind {
        LexTokenKind::Ident(name) => Some(name),
        _ => None,
    }
}

/// Parser-driven classification — the primary path. Walks the
/// parsed `Module` and builds a side-table of byte → role overrides,
/// then walks the raw tokens applying those overrides where set and
/// falling back to per-kind classification otherwise.
///
/// What gets an override:
///
/// - Module-path segments (top-level `module` decl, `import …(items)`
///   path, `import … as` alias, `import __intrinsics__`) → `entity.name.module`.
/// - Function definition names (`fn foo`, including the `pub`'d form)
///   → `entity.name.function`.
/// - Function-call callee names (the trailing IDENT of a path used as
///   `Call.callee`) → `entity.name.function`. This is approximate —
///   the path's actual target isn't resolved at parse time — but it
///   matches the visual convention readers expect.
/// - Type definition names (`newtype Box`, `type _List`)
///   → `entity.name.type`. The newtype's `constructor` / `projector`
///   member names → `entity.name.function`.
/// - Label names inside `labels { foo: T, bar: U }` → `entity.name.label`.
/// - Function-parameter binders → `variable.parameter`.
///
/// Keyword roles are recovered from the represented declaration or expression
/// boundaries. Ordinary parameter uses and other unclassified names remain
/// identifiers; keyword-shaped spellings alone confer no role.
fn dump_with_ast(
    raw: &[LexToken],
    module: &ast::Module<ast::Surface>,
    source: &str,
) -> Vec<ClassifiedToken> {
    let overrides = ast_overrides(module, raw);
    dump_with_ast_overrides(raw, source, &overrides)
}

#[cfg(all(feature = "surface", feature = "lsp"))]
fn dump_with_ast_and_rec_facts(
    raw: &[LexToken],
    module: &ast::Module<ast::Surface>,
    source: &str,
) -> (Vec<ClassifiedToken>, Vec<RecMemberTokenFact>) {
    let overrides = ast_overrides(module, raw);
    let out = dump_with_ast_overrides(raw, source, &overrides);
    (out, overrides.rec_members)
}

fn dump_with_ast_overrides(
    raw: &[LexToken],
    source: &str,
    overrides: &AstOverrides,
) -> Vec<ClassifiedToken> {
    let mut out: Vec<ClassifiedToken> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        for t in &raw[i].leading_trivia {
            match t {
                Trivia::LineComment { span, .. } => {
                    out.push(ClassifiedToken {
                        span: *span,
                        kind: TokenKind::CommentLine,
                    });
                }
                Trivia::DocCommentLine { span, .. } => {
                    out.push(ClassifiedToken {
                        span: *span,
                        kind: TokenKind::CommentDoc,
                    });
                }
                Trivia::Newline => {}
            }
        }

        if let Some(fused) = try_fuse_elaborator_bang(raw, i) {
            out.push(fused);
            i += 2;
            continue;
        }

        if let Some(fused) = try_fuse_dot_bool(raw, i) {
            out.push(fused);
            i += 2;
            continue;
        }

        if let Some((elaborator, suffix)) =
            try_split_confirmed_elaborator_bang(raw, i, overrides, source)
        {
            out.push(elaborator);
            out.push(suffix);
            i += 2;
            continue;
        }

        let kind = overrides
            .get(&raw[i].span.start)
            .copied()
            .unwrap_or_else(|| classify(&raw[i].kind));
        out.push(ClassifiedToken {
            span: raw[i].span,
            kind,
        });
        i += 1;
    }

    let tail_start = raw.last().map(|t| t.span.end as usize).unwrap_or(0);
    collect_trailing_line_comments(source, tail_start, &mut out);

    split_structural_brackets(out, &overrides.structural_brackets, source)
}

#[derive(Default)]
struct AstOverrides {
    kinds: HashMap<u32, TokenKind>,
    /// Starts of `!` bytes that the parser confirmed as elaborator suffixes.
    /// This lets the final stream split a greedy `!.>` / `!.<` lexer run
    /// without guessing that an arbitrary bang-led operator is an elaborator.
    elaborator_bangs: HashSet<u32>,
    /// Byte offsets of square brackets consumed as forall delimiters. A raw
    /// `SymbolRun` may contain both such a delimiter and an ordinary operator
    /// suffix or prefix, so the final classified stream splits at these bytes.
    structural_brackets: HashSet<u32>,
    #[cfg(all(feature = "surface", feature = "lsp"))]
    rec_members: Vec<RecMemberTokenFact>,
    #[cfg(all(feature = "surface", feature = "lsp"))]
    rec_calls: Vec<(String, Span)>,
}

impl std::ops::Deref for AstOverrides {
    type Target = HashMap<u32, TokenKind>;

    fn deref(&self) -> &Self::Target {
        &self.kinds
    }
}

impl std::ops::DerefMut for AstOverrides {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.kinds
    }
}

fn ast_overrides(module: &ast::Module<ast::Surface>, raw: &[LexToken]) -> AstOverrides {
    let mut overrides = AstOverrides::default();
    walk_module(module, raw, &mut overrides);
    overrides
}

/// Scan the tail of `source` (from `from` to end-of-file) for `//`
/// line comments the lexer dropped because they don't precede a
/// meaningful token. Pushes one `comment.line` token per comment.
///
/// Only line comments matter here — every other byte after the last
/// meaningful token is horizontal whitespace, newlines, or a
/// trailing `//` comment run. Anything else would have produced a
/// meaningful token, contradicting the assumption that we're past
/// the last one.
fn collect_trailing_line_comments(source: &str, from: usize, out: &mut Vec<ClassifiedToken>) {
    let bytes = source.as_bytes();
    let mut pos = from;
    while pos < bytes.len() {
        match bytes[pos] {
            b' ' | b'\t' | b'\r' | b'\n' => pos += 1,
            b'/' if pos + 1 < bytes.len() && bytes[pos + 1] == b'/' => {
                let comment_start = pos;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                out.push(ClassifiedToken {
                    span: Span::new(comment_start as u32, pos as u32),
                    kind: TokenKind::CommentLine,
                });
            }
            _ => {
                // Any other byte past the last meaningful token would
                // have produced a token of its own — getting here
                // means the lexer's invariant changed. Bail rather
                // than guess what to emit.
                break;
            }
        }
    }
}

/// If `raw[i]` and `raw[i+1]` together form a bang-call elaborator
/// (a value-reference name plus contiguous `!`, with no trivia between them), return the
/// fused token. The contiguity check is the invariant: a space between
/// (`name !`) breaks the fusion because `!` could just as well be a
/// user-defined unary op.
fn try_fuse_elaborator_bang(raw: &[LexToken], i: usize) -> Option<ClassifiedToken> {
    let head = &raw[i];
    let next = raw.get(i + 1)?;
    let LexTokenKind::Ident(name) = &head.kind else {
        return None;
    };
    if !crate::naming::is_value_reference_name(name) || !next.kind.is_sym("!") {
        return None;
    }
    // Adjacency: no trivia on the `!` token, and the ident's end
    // byte equals the `!`'s start byte. Either condition alone is
    // enough in practice (the lexer would have to produce one to
    // produce the other), but checking both keeps the fusion rule
    // local and easy to reason about.
    if !next.leading_trivia.is_empty() || head.span.end != next.span.start {
        return None;
    }
    Some(ClassifiedToken {
        span: Span::new(head.span.start, next.span.end),
        kind: TokenKind::KeywordElaborator,
    })
}

/// Split an AST-confirmed elaborator `!` from a greedily fused following UFCS
/// arrow. The lexer intentionally emits `!.>`, `!.>>`, `!.<`, or `!.<<` as one
/// symbol run. A parsed [`ast::Expr::Ufcs`] retains the exact `!` span, so the
/// highlighter can safely emit `name!` plus the builtin arrow without treating
/// an unrelated bang-led user operator as an elaborator call.
fn try_split_confirmed_elaborator_bang(
    raw: &[LexToken],
    i: usize,
    overrides: &AstOverrides,
    source: &str,
) -> Option<(ClassifiedToken, ClassifiedToken)> {
    let head = &raw[i];
    let next = raw.get(i + 1)?;
    let LexTokenKind::Ident(_) = &head.kind else {
        return None;
    };
    let LexTokenKind::SymbolRun(run) = &next.kind else {
        return None;
    };
    if !overrides.elaborator_bangs.contains(&next.span.start)
        || !run.starts_with('!')
        || run.len() == 1
        || !next.leading_trivia.is_empty()
        || head.span.end != next.span.start
    {
        return None;
    }

    let bang_end = next.span.start + 1;
    let suffix = &source[bang_end as usize..next.span.end as usize];
    if !matches!(suffix, ".>" | ".>>" | ".<" | ".<<") {
        return None;
    }
    Some((
        ClassifiedToken {
            span: Span::new(head.span.start, bang_end),
            kind: TokenKind::KeywordElaborator,
        },
        ClassifiedToken {
            span: Span::new(bang_end, next.span.end),
            kind: classify_symbol_run(suffix),
        },
    ))
}

/// If `raw[i..i+2]` spells `.t` or `.f` with no whitespace, return
/// one fused boolean-literal token. The lexer deliberately keeps the
/// spelling as `SymbolRun(".")` + `Ident("t"|"f")`; the parser and
/// highlighter recognize the expression-level literal form.
fn try_fuse_dot_bool(raw: &[LexToken], i: usize) -> Option<ClassifiedToken> {
    let dot = &raw[i];
    let name = raw.get(i + 1)?;
    if !dot.kind.is_sym(".") {
        return None;
    }
    let LexTokenKind::Ident(word) = &name.kind else {
        return None;
    };
    if word != "t" && word != "f" {
        return None;
    }
    if !name.leading_trivia.is_empty() || dot.span.end != name.span.start {
        return None;
    }
    if raw
        .get(i + 2)
        .is_some_and(|next| next.kind.is_sym(".") && next.span.start == name.span.end)
    {
        return None;
    }
    Some(ClassifiedToken {
        span: Span::new(dot.span.start, name.span.end),
        kind: TokenKind::LiteralBool,
    })
}

/// Map one lexer [`TokenKind`] to its canonical [`TokenKind`].
/// Bang-call elaborator fusion happens earlier in [`dump`]; by the time
/// we get here, every token is classified standalone.
fn classify(kind: &LexTokenKind) -> TokenKind {
    match kind {
        LexTokenKind::Ident(_) => TokenKind::Identifier,
        LexTokenKind::StrLit(_) => TokenKind::LiteralString,
        LexTokenKind::IntLit { .. } | LexTokenKind::FloatLit { .. } => TokenKind::LiteralNumber,
        LexTokenKind::BoolLit(_) => TokenKind::LiteralBool,
        LexTokenKind::LParen
        | LexTokenKind::RParen
        | LexTokenKind::LBrace
        | LexTokenKind::RBrace => TokenKind::PunctuationBracket,
        LexTokenKind::Comma | LexTokenKind::Semicolon => TokenKind::PunctuationSeparator,
        LexTokenKind::SymbolRun(run) => classify_symbol_run(run),
        LexTokenKind::Slot1 | LexTokenKind::Slot2 | LexTokenKind::Slot3 => TokenKind::Slot,
    }
}

/// Classify a [`SymbolRun`](LexTokenKind::SymbolRun) by content.
///
/// Square brackets retain operator coloring here. Parser-driven classification
/// splits bytes that delimit forall binders into `punctuation.bracket`.
/// A standalone `.` is the
/// member-access / `DotPath` separator and classifies as
/// `punctuation.separator`. The reserved op-tokens (`.>`, `.>>`,
/// `.<`, `.<<`, `&`, `|`, `=`, `:`, `!`, `->`) are `operator.builtin`; everything
/// else (including `<` / `>`, which the lexer treats as fixed-
/// role symbols but which can also appear as user-declared
/// comparison operators) is `operator.user`.
fn classify_symbol_run(run: &str) -> TokenKind {
    match run {
        "." => TokenKind::PunctuationSeparator,
        ".>" | ".>>" | ".<" | ".<<" | "&" | "|" | "=" | ":" | "!" | "->" => {
            TokenKind::OperatorBuiltin
        }
        _ => TokenKind::OperatorUser,
    }
}

// ---- Parser-driven walker ---------------------------------------
//
// The walker visits the parsed AST in source order and registers
// `(token-start-byte, refined-role)` entries in `overrides`. The
// `dump_with_ast` token loop consults this table per token, applying
// the override when present and falling back to per-kind classify
// otherwise.
//
// Helper invariants this code relies on:
//
// - `raw` is sorted by `span.start`.
// - Tokens within an AST node's `meta.span` (or equivalent) appear as
//   a contiguous slice of `raw` (the lexer is total over the source).
// - When an AST node carries a `name: String` but no `name_span`,
//   the name is recoverable as the IDENT *immediately after* the
//   leading keyword of the declaration form. The two helpers below
//   ([`find_ident_in_span`] and [`find_name_after_kw`]) implement
//   that recovery.

fn walk_module(m: &ast::Module<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_keyword(
        raw,
        Span::new(m.meta.span.start, m.path.span.start),
        "module",
        TokenKind::KeywordDeclaration,
        overrides,
    );
    for seg in &m.path.segments {
        overrides.insert(seg.span.start, TokenKind::EntityNameModule);
    }
    for u in &m.imports {
        walk_import(u, raw, overrides);
    }
    for item in &m.items {
        walk_item(item, raw, overrides);
    }
}

fn walk_import(u: &ast::Import, raw: &[LexToken], overrides: &mut AstOverrides) {
    if let Some(keyword) = find_ident_in_span(raw, u.span, "import") {
        overrides.insert(keyword.start, TokenKind::KeywordDeclaration);
    }
    match &u.kind {
        ast::ImportKind::Selective { items, from } => {
            for seg in &from.segments {
                overrides.insert(seg.span.start, TokenKind::EntityNameModule);
            }
            // Ordinary names retain their default identifier classification.
            // A braced label item is classified identically to label
            // references in expressions.
            for item in items {
                match item {
                    ast::ImportItem::Name { span, .. } => {
                        overrides.insert(span.start, TokenKind::Identifier);
                    }
                    ast::ImportItem::Label { name, span, .. } => {
                        if let Some(name_span) = find_ident_in_span(raw, *span, name) {
                            overrides.insert(name_span.start, TokenKind::EntityNameLabelReference);
                        }
                    }
                    ast::ImportItem::OperatorPattern { grammar, span, .. } => {
                        mark_operator_head(
                            raw,
                            *span,
                            matches!(grammar, ast::OperatorGrammar::Variadic { .. }),
                            overrides,
                        );
                    }
                }
            }
        }
        ast::ImportKind::Qualified { path, alias } => {
            for seg in &path.segments {
                overrides.insert(seg.span.start, TokenKind::EntityNameModule);
            }
            // The alias is the final matching IDENT after the provider path.
            // Selecting from the end keeps `import app/as as as;` unambiguous:
            // the provider segment, separator keyword, and alias are three
            // distinct roles despite sharing one spelling.
            let alias_range = Span::new(path.span.end, u.span.end);
            if let Some(alias_span) = find_last_ident_in_span(raw, alias_range, alias) {
                overrides.insert(alias_span.start, TokenKind::EntityNameModule);
                if let Some(keyword_span) =
                    find_ident_in_span(raw, Span::new(path.span.end, alias_span.start), "as")
                {
                    overrides.insert(keyword_span.start, TokenKind::KeywordDeclaration);
                }
            }
        }
        ast::ImportKind::Intrinsics => {
            // The grammar-owned builtin provider occupies the module-name slot.
            if let Some(s) = find_ident_in_span(raw, u.span, "__intrinsics__") {
                overrides.insert(s.start, TokenKind::EntityNameModule);
            }
        }
        ast::ImportKind::Comptime => {
            if let Some(s) = find_ident_in_span(raw, u.span, "__comptime__") {
                overrides.insert(s.start, TokenKind::EntityNameModule);
            }
        }
    }
}

fn walk_item(item: &ast::Item<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    match item {
        ast::Item::FnDef(f) => walk_fn_def(f, raw, overrides),
        ast::Item::TypeAlias(a) => walk_alias(a, raw, overrides),
        ast::Item::LiteralAlias(a, _) => walk_literal_alias(a, raw, overrides),
        ast::Item::Newtype(n) => walk_newtype(n, raw, overrides),
        ast::Item::Labels(t, _) => walk_labels(t, raw, overrides),
        ast::Item::LabelForward(forward, _) => {
            mark_declaration_keyword(raw, forward.meta.span, "type", overrides);
            overrides.insert(forward.name_span.start, TokenKind::EntityNameLabel);
            if let Some(name) = forward.target.rsplit('.').next()
                && let Some(span) = find_last_ident_in_span(raw, forward.target_span, name)
            {
                overrides.insert(span.start, label_reference_kind(&forward.target));
            }
        }
        ast::Item::Equiv(e, _) => walk_equiv(e, raw, overrides),
        ast::Item::Elaborator(s, _) => {
            mark_declaration_keyword(raw, s.meta.span, "elab", overrides);
            overrides.insert(s.name_span.start, TokenKind::EntityNameFunction);
            walk_type(&s.call_ty, raw, overrides);
            for block in &s.trailing_blocks {
                mark_leading_keyword(
                    raw,
                    block.meta.span,
                    "trailing",
                    TokenKind::KeywordDeclaration,
                    overrides,
                );
                let kind = match block.exposure {
                    ast::BlockExposure::Product => "product",
                    ast::BlockExposure::Thunk => "thunk",
                    ast::BlockExposure::Sequence => "sequence",
                };
                mark_keyword(
                    raw,
                    block.meta.span,
                    kind,
                    TokenKind::KeywordDeclaration,
                    overrides,
                );
                if let Some(label) = &block.label {
                    overrides.insert(label.span.start, TokenKind::KeywordControl);
                }
            }
            if let Some(span) = find_last_ident_in_span(
                raw,
                Span::new(s.call_ty.meta().span.end, s.implementation.span().start),
                "impl",
            ) {
                overrides.insert(span.start, TokenKind::KeywordDeclaration);
            }
            if !s.captures.is_empty() {
                mark_keyword(
                    raw,
                    Span::new(s.call_ty.meta().span.end, s.captures[0].span.start),
                    "captures",
                    TokenKind::KeywordDeclaration,
                    overrides,
                );
            }
            if s.schedule == ast::ElaboratorSchedule::Fills {
                let end = s.implementation.span().start;
                if let Some(span) =
                    find_last_ident_in_span(raw, Span::new(s.call_ty.meta().span.end, end), "fills")
                {
                    overrides.insert(span.start, TokenKind::KeywordDeclaration);
                }
            }
            mark_callable_path(&s.implementation, overrides);
        }
        ast::Item::RecGroup(g, _) => {
            mark_declaration_keyword(raw, g.meta.span, "rec", overrides);
            for member in &g.members {
                #[cfg(all(feature = "surface", feature = "lsp"))]
                let first_call = overrides.rec_calls.len();
                walk_fn_def(member, raw, overrides);
                #[cfg(all(feature = "surface", feature = "lsp"))]
                let calls = overrides.rec_calls.split_off(first_call);
                #[cfg(all(feature = "surface", feature = "lsp"))]
                if let Some(name_span) =
                    find_name_after_kw(raw, member.meta.span, "fn", &member.name)
                {
                    overrides.rec_members.push(RecMemberTokenFact {
                        name: member.name.clone(),
                        name_span,
                        calls,
                    });
                }
            }
        }
        ast::Item::TypeRecGroup(group) => {
            walk_type_rec_group(group, raw, overrides);
        }
        ast::Item::Op(o, _) => walk_op(o, raw, overrides),
        ast::Item::VariadicOperator(f, _) => walk_variadic_operator(f, raw, overrides),
        ast::Item::HostType(h) => walk_host_type(h, raw, overrides),
        ast::Item::HostFn(h) => walk_host_fn(h, raw, overrides),
    }
}

fn walk_type_rec_group(group: &ast::TypeRecGroup, raw: &[LexToken], overrides: &mut AstOverrides) {
    if let Some(span) = group.rec_span {
        mark_declaration_keyword(raw, span, "rec", overrides);
    }
    for member in &group.members {
        match member {
            ast::TypeRecMember::TypeAlias(alias) => walk_alias(alias, raw, overrides),
            ast::TypeRecMember::Newtype(newtype) => walk_newtype(newtype, raw, overrides),
            ast::TypeRecMember::Labels(labels, _) => walk_labels(labels, raw, overrides),
        }
    }
}

fn walk_host_type(h: &ast::HostType<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, h.meta.span, "type", overrides);
    if let Some(role) = h.role {
        mark_keyword(
            raw,
            role.span,
            "role",
            TokenKind::KeywordDeclaration,
            overrides,
        );
        mark_keyword(
            raw,
            role.span,
            role.role.as_str(),
            TokenKind::KeywordDeclaration,
            overrides,
        );
    }
    if h.owned
        && let Some(span) = find_last_ident_in_span(raw, h.meta.span, "owned")
    {
        overrides.insert(span.start, TokenKind::KeywordDeclaration);
    }
    if let Some(s) = find_name_after_kw(raw, h.meta.span, "type", &h.name) {
        overrides.insert(s.start, TokenKind::EntityNameType);
    }
    for tp in &h.type_params {
        mark_type_param(tp, raw, overrides);
    }
}

fn walk_host_fn(h: &ast::HostFn<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, h.meta.span, "fn", overrides);
    if let Some(s) = find_name_after_kw(raw, h.meta.span, "fn", &h.name) {
        overrides.insert(s.start, TokenKind::EntityNameFunction);
    }
    for p in &h.params {
        match p {
            ast::HostFnParam::Type(tp) => mark_type_param(tp, raw, overrides),
            ast::HostFnParam::Value(v) => {
                if let Some(name) = &v.name
                    && let Some(s) = find_ident_in_span(raw, v.meta.span, name)
                {
                    overrides.insert(s.start, TokenKind::VariableParameter);
                }
                walk_type(&v.ty, raw, overrides);
            }
        }
    }
    walk_type(&h.ret, raw, overrides);
}

fn walk_fn_def(f: &ast::FnDef<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, f.meta.span, "fn", overrides);
    if let Some(s) = find_name_after_kw(raw, f.meta.span, "fn", &f.name) {
        overrides.insert(s.start, TokenKind::EntityNameFunction);
    }
    walk_signature(&f.sig, raw, overrides, true);
    walk_type(&f.ret, raw, overrides);
    walk_expr(&f.body, raw, overrides);
}

fn walk_alias(a: &ast::TypeAlias<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, a.meta.span, "type", overrides);
    if let Some(s) = find_name_after_kw(raw, a.meta.span, "type", &a.name) {
        overrides.insert(s.start, TokenKind::EntityNameType);
    }
    for tp in &a.type_params {
        mark_type_param(tp, raw, overrides);
    }
    walk_type(&a.body, raw, overrides);
}

fn walk_literal_alias(
    a: &ast::LiteralAlias<ast::Surface>,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    mark_declaration_keyword(raw, a.meta.span, "literal", overrides);
    if let Some(s) = find_name_after_kw(raw, a.meta.span, "literal", &a.name) {
        overrides.insert(s.start, TokenKind::Identifier);
    }
}

fn walk_newtype(n: &ast::Newtype<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, n.meta.span, "newtype", overrides);
    if let Some(span) = n.rec_span {
        mark_declaration_keyword(raw, span, "rec", overrides);
    }
    if let Some(s) = find_name_after_kw(raw, n.meta.span, "newtype", &n.name) {
        overrides.insert(s.start, TokenKind::EntityNameType);
    }
    // A member span includes its visibility prefix. The final name follows
    // the member keyword, even when either spelling also occurs in a scope path.
    for member in [&n.constructor, &n.projector] {
        if let Some(name) = find_last_ident_in_span(raw, member.span, &member.name) {
            if let Some(index) = first_token_at_or_after(raw, name.start).checked_sub(1)
                && let Some(keyword) = ident_text(&raw[index])
            {
                mark_declaration_keyword(raw, raw[index].span, keyword, overrides);
            }
            overrides.insert(name.start, TokenKind::EntityNameFunction);
        }
    }
    for tp in &n.type_params {
        mark_type_param(tp, raw, overrides);
    }
    for tp in &n.existential_params {
        mark_type_param(tp, raw, overrides);
    }
    walk_type(&n.payload, raw, overrides);
}

fn walk_labels(t: &ast::Labels<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, t.meta.span, "labels", overrides);
    if let Some(span) = t.rec_span {
        mark_declaration_keyword(raw, span, "rec", overrides);
    }
    // Named form: `labels T = { … };` — the T is the alias type name.
    if let Some(s) = t.type_alias_span {
        overrides.insert(s.start, TokenKind::EntityNameType);
    }
    for tp in &t.type_alias_params {
        mark_type_param(tp, raw, overrides);
    }
    for entry in &t.entries {
        let kind = if entry.is_reuse_marker() {
            TokenKind::EntityNameLabelReference
        } else {
            TokenKind::EntityNameLabel
        };
        overrides.insert(entry.name_span.start, kind);
        for tp in &entry.type_params {
            mark_type_param(tp, raw, overrides);
        }
        for tp in &entry.existential_params {
            mark_type_param(tp, raw, overrides);
        }
        walk_type(&entry.payload, raw, overrides);
    }
}

fn walk_equiv(e: &ast::Equiv<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, e.meta.span, "equiv", overrides);
    // `equiv` declarations carry a function-shaped signature; treat
    // the name like a function definition.
    overrides.insert(e.name_span.start, TokenKind::EntityNameFunction);
    walk_signature(&e.sig, raw, overrides, true);
    for term in &e.terms {
        walk_expr(&term.body, raw, overrides);
    }
}

fn mark_operator_head(raw: &[LexToken], span: Span, variadic: bool, overrides: &mut AstOverrides) {
    let keyword = if variadic { "varop" } else { "op" };
    let Some(index) = raw.iter().position(|token| {
        span.start <= token.span.start
            && token.span.end <= span.end
            && is_ident_named(token, keyword)
    }) else {
        return;
    };
    overrides.insert(raw[index].span.start, TokenKind::KeywordDeclaration);
}

fn walk_op(o: &ast::Op<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    mark_declaration_keyword(raw, o.meta.span, "op", overrides);
    let ast::OpBody::Normal { function, .. } = &o.body;
    mark_variadic_clause_head(raw, function, overrides);
    mark_callable_path(function, overrides);
}

fn walk_variadic_operator(
    f: &ast::VariadicOperator<ast::Surface>,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    mark_declaration_keyword(raw, f.meta.span, "varop", overrides);
    mark_variadic_clause_head(raw, &f.spec.step.path, overrides);
    mark_callable_path(&f.spec.initializer.path, overrides);
    mark_callable_path(&f.spec.step.path, overrides);
    if let Some(finalize) = &f.spec.finalize {
        mark_variadic_clause_head(raw, &finalize.path, overrides);
        mark_callable_path(&finalize.path, overrides);
    }
}

/// A clause's first callable immediately follows its grammar-owned head.
/// Use that boundary, not a spelling search through callable segments.
fn mark_variadic_clause_head(
    raw: &[LexToken],
    path: &ast::LexicalCallablePath,
    overrides: &mut AstOverrides,
) {
    if let Some(first) = path.first()
        && let Some(index) = raw
            .iter()
            .position(|token| token.span.start == first.span.start)
        && let Some(keyword) = index.checked_sub(1).and_then(|previous| raw.get(previous))
    {
        overrides.insert(keyword.span.start, TokenKind::KeywordDeclaration);
    }
}

fn mark_callable_path(path: &ast::LexicalCallablePath, overrides: &mut AstOverrides) {
    if let Some(last) = path.last() {
        overrides.insert(last.span.start, TokenKind::EntityNameFunctionReference);
    }
}

fn walk_signature(
    sig: &ast::Signature<ast::Surface>,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
    pattern_binders_are_parameters: bool,
) {
    for p in &sig.params {
        match p {
            ast::SignatureParam::Type(tp) => {
                mark_type_param(tp, raw, overrides);
            }
            ast::SignatureParam::Value(v) => {
                // `Param { name, ty, meta }` — the value param's
                // meta.span covers the whole `name: T` form. The
                // name IDENT is the first occurrence of `v.name`
                // within that span.
                if let Some(s) = find_ident_in_span(raw, v.meta.span, &v.name) {
                    overrides.insert(s.start, TokenKind::VariableParameter);
                }
                if let Some(ty) = &v.ty {
                    walk_type(ty, raw, overrides);
                }
                if let Some(pattern) = &v.pattern {
                    walk_param_pattern(pattern, raw, overrides, pattern_binders_are_parameters);
                }
            }
        }
    }
}

fn walk_param_pattern(
    pattern: &ast::ParamPattern,
    raw: &[LexToken],
    overrides: &mut AstOverrides,
    binders_are_parameters: bool,
) {
    for element in &pattern.elems {
        if binders_are_parameters {
            match element {
                ast::ParamPatternElem::Bind {
                    name, name_span, ..
                }
                | ast::ParamPatternElem::BindTuple {
                    name, name_span, ..
                } if name != "_" => {
                    overrides.insert(name_span.start, TokenKind::VariableParameter);
                }
                _ => {}
            }
        }
        match element {
            ast::ParamPatternElem::Bind { ty, .. } => walk_type(ty, raw, overrides),
            ast::ParamPatternElem::Tuple(inner)
            | ast::ParamPatternElem::BindTuple { inner, .. } => {
                walk_param_pattern(inner, raw, overrides, binders_are_parameters);
            }
        }
    }
}

/// Walk a type expression and mark exact type-name `Path` segments as
/// `EntityNameType`. Recurses into all sub-types so positions deep inside
/// `(A & B) -> C`,
/// `Foo(Bar)`, etc. all get the refinement.
fn walk_type(ty: &ast::Type<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    use ast::Type::*;
    match ty {
        Path { segments, args, .. } => {
            for seg in segments {
                if crate::naming::is_type_reference_name(&seg.name) {
                    overrides.insert(seg.span.start, TokenKind::EntityNameType);
                }
            }
            for arg in args {
                walk_type(arg, raw, overrides);
            }
        }
        Function { param, ret, .. } => {
            walk_type(param, raw, overrides);
            walk_type(ret, raw, overrides);
        }
        Product { left, right, .. } | Sum { left, right, .. } => {
            walk_type(left, raw, overrides);
            walk_type(right, raw, overrides);
        }
        LabelSugar { labels, .. } => {
            for label in labels {
                if let Some(payload) = &label.payload {
                    walk_type(payload, raw, overrides);
                }
            }
        }
        Forall { param, body, .. } => {
            mark_type_param(param, raw, overrides);
            walk_type(body, raw, overrides);
        }
        Unit { .. } | Bottom { .. } | Infer { .. } => {}
        Goal { ext, .. } => match *ext {},
    }
}

/// Set the variable.parameter override on the IDENT inside a
/// `TypeParam`'s span. `TypeParam.span` covers the whole bracket
/// pair (`[A]` or `<U>`); the inner IDENT is the part we want to
/// classify.
fn mark_type_param(tp: &ast::TypeParam, raw: &[LexToken], overrides: &mut AstOverrides) {
    let Some(name_span) = find_ident_in_span(raw, tp.span, &tp.name) else {
        return;
    };
    overrides.insert(name_span.start, TokenKind::VariableParameter);

    let name_index = first_token_at_or_after(raw, name_span.start);
    let mut before = name_index;
    while let Some(index) = before.checked_sub(1) {
        let token = &raw[index];
        match &token.kind {
            LexTokenKind::Comma => before = index,
            LexTokenKind::SymbolRun(run)
                if !run.is_empty() && run.bytes().all(|byte| byte == b'*') =>
            {
                before = index;
            }
            LexTokenKind::SymbolRun(run) => {
                if let Some(offset) = run.rfind('[')
                    && run[offset + 1..].bytes().all(|byte| byte == b'*')
                {
                    overrides
                        .structural_brackets
                        .insert(token.span.start + offset as u32);
                }
                break;
            }
            _ => break,
        }
    }

    let mut after = name_index + 1;
    while let Some(token) = raw.get(after) {
        match &token.kind {
            LexTokenKind::Comma => after += 1,
            LexTokenKind::SymbolRun(run) if run.starts_with(']') => {
                overrides.structural_brackets.insert(token.span.start);
                break;
            }
            _ => break,
        }
    }
}

/// Split parser-confirmed forall brackets out of classified symbol runs.
/// Remaining fragments are reclassified by their own spelling: `]->` becomes
/// punctuation `]` followed by builtin operator `->`, while an unmarked `[!`
/// remains one ordinary user-operator token.
fn split_structural_brackets(
    tokens: Vec<ClassifiedToken>,
    structural: &HashSet<u32>,
    source: &str,
) -> Vec<ClassifiedToken> {
    if structural.is_empty() {
        return tokens;
    }

    let mut offsets = structural.iter().copied().collect::<Vec<_>>();
    offsets.sort_unstable();
    let mut next_offset = 0usize;
    let mut out = Vec::with_capacity(tokens.len() + offsets.len());
    for token in tokens {
        while offsets
            .get(next_offset)
            .is_some_and(|offset| *offset < token.span.start)
        {
            next_offset += 1;
        }
        let first_cut = next_offset;
        while offsets
            .get(next_offset)
            .is_some_and(|offset| *offset < token.span.end)
        {
            next_offset += 1;
        }
        if first_cut == next_offset {
            out.push(token);
            continue;
        }

        let mut start = token.span.start;
        for &offset in &offsets[first_cut..next_offset] {
            if start < offset {
                let spelling = &source[start as usize..offset as usize];
                out.push(ClassifiedToken {
                    span: Span::new(start, offset),
                    kind: classify_symbol_run(spelling),
                });
            }
            out.push(ClassifiedToken {
                span: Span::new(offset, offset + 1),
                kind: TokenKind::PunctuationBracket,
            });
            start = offset + 1;
        }
        if start < token.span.end {
            let spelling = &source[start as usize..token.span.end as usize];
            out.push(ClassifiedToken {
                span: Span::new(start, token.span.end),
                kind: classify_symbol_run(spelling),
            });
        }
    }
    out
}

fn walk_expr(e: &ast::Expr<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    use ast::Expr::*;
    if let Some(opening) = crate::pass::parser::source_existential_let(e) {
        mark_leading_keyword(
            raw,
            e.span(),
            "let",
            TokenKind::KeywordDeclaration,
            overrides,
        );
        for param in opening.type_params {
            if let ast::SignatureParam::Type(param) = param {
                mark_type_param(param, raw, overrides);
            }
        }
        if let Some(pattern) = &opening.binder.pattern {
            walk_param_pattern(pattern, raw, overrides, false);
        }
        walk_expr(opening.value, raw, overrides);
        walk_expr(opening.body, raw, overrides);
        return;
    }
    match e {
        BlockCall {
            head,
            prefix,
            blocks,
            ..
        } => {
            overrides.insert(head.span.start, TokenKind::EntityNameFunctionReference);
            overrides.elaborator_bangs.insert(head.span.end);
            for value in prefix {
                walk_expr(value, raw, overrides);
            }
            for block in blocks {
                if let Some(label) = &block.label {
                    overrides.insert(label.span.start, TokenKind::KeywordControl);
                }
                for item in &block.items {
                    match item {
                        ast::NeutralItem::Binding {
                            ty, pattern, meta, ..
                        } => {
                            mark_leading_keyword(
                                raw,
                                meta.span,
                                "let",
                                TokenKind::KeywordDeclaration,
                                overrides,
                            );
                            if let Some(ty) = ty {
                                walk_type(ty, raw, overrides);
                            }
                            if let Some(pattern) = pattern {
                                walk_param_pattern(pattern, raw, overrides, false);
                            }
                        }
                        ast::NeutralItem::RowBinding { entries, meta, .. } => {
                            mark_leading_keyword(
                                raw,
                                meta.span,
                                "let",
                                TokenKind::KeywordDeclaration,
                                overrides,
                            );
                            walk_row_let_entries(entries, raw, overrides);
                        }
                        ast::NeutralItem::ExistentialBinding {
                            type_params,
                            pattern,
                            meta,
                            ..
                        } => {
                            mark_leading_keyword(
                                raw,
                                meta.span,
                                "let",
                                TokenKind::KeywordDeclaration,
                                overrides,
                            );
                            for param in type_params {
                                mark_type_param(param, raw, overrides);
                            }
                            if let Some(pattern) = pattern {
                                walk_param_pattern(pattern, raw, overrides, false);
                            }
                        }
                        ast::NeutralItem::Expression { .. } => {}
                    }
                    walk_expr(item.value(), raw, overrides);
                }
            }
        }
        Path { .. } => {
            // A bare path in expression position. Its target may be
            // a function value, a binder, or a constructor — we
            // can't tell at parse time without name resolution.
            // Keep at default `identifier`.
        }
        Call { callee, args, .. } => {
            // Call.callee is most often a `Path` (`foo(x)`,
            // `Box.mk_box(v)`, `a.b.c.foo()`). The trailing path
            // segment is the function name at the call site;
            // classify it as a function reference. Preceding
            // segments are the qualifying prefix; they stay at
            // identifier (the VSCode overlay applies casing-based
            // namespace/type classification on top — keeping the
            // discrimination out of the reference avoids the
            // tree-sitter lexer ambiguity that the keyword-shaped
            // `module`/`fn`/`let` tokens have with case-classified
            // identifier regex alternatives). For non-path callees
            // (lambdas, sub-expressions), recurse into the callee.
            if let Path { segments, .. } = callee.as_ref() {
                if let Some(last) = segments.last() {
                    overrides.insert(last.span.start, TokenKind::EntityNameFunctionReference);
                }
            } else {
                walk_expr(callee, raw, overrides);
            }
            for arg in args {
                walk_call_arg(arg, raw, overrides);
            }
        }
        RecCall {
            modes,
            callee,
            args,
            meta,
            ..
        } => {
            if let Some(span) = find_ident_in_span(raw, meta.span, "rec") {
                overrides.insert(span.start, TokenKind::KeywordDeclaration);
            }
            for mode in modes {
                mark_keyword(
                    raw,
                    Span::new(meta.span.start, callee.span.start),
                    mode.as_str(),
                    TokenKind::KeywordControl,
                    overrides,
                );
            }
            overrides.insert(callee.span.start, TokenKind::EntityNameFunctionReference);
            #[cfg(all(feature = "surface", feature = "lsp"))]
            overrides.rec_calls.push((callee.name.clone(), callee.span));
            for arg in args {
                walk_call_arg(arg, raw, overrides);
            }
        }
        FnExpr {
            sig,
            ret_ty,
            body,
            meta,
            ..
        } => {
            // Existential-opening lets also carry a continuation FnExpr;
            // only a written dot-lambda gives pattern leaves parameter roles.
            let intro = first_token_at_or_after(raw, meta.span.start);
            let written_lambda = raw.get(intro).is_some_and(|token| {
                token.span.start == meta.span.start
                    && match &token.kind {
                        LexTokenKind::SymbolRun(run) if run.starts_with(".[") => true,
                        LexTokenKind::SymbolRun(run) if run == "." => {
                            raw.get(intro + 1).is_some_and(|next| match &next.kind {
                                LexTokenKind::LParen => true,
                                LexTokenKind::SymbolRun(run) => run.starts_with('['),
                                _ => false,
                            })
                        }
                        _ => false,
                    }
            });
            walk_signature(sig, raw, overrides, written_lambda);
            if let Some(ty) = ret_ty {
                walk_type(ty, raw, overrides);
            }
            walk_expr(body, raw, overrides);
        }
        Let {
            ty,
            pattern,
            value,
            body,
            meta,
            ..
        } => {
            mark_leading_keyword(
                raw,
                meta.span,
                "let",
                TokenKind::KeywordDeclaration,
                overrides,
            );
            if let Some(ty) = ty {
                walk_type(ty, raw, overrides);
            }
            if let Some(pattern) = pattern {
                walk_param_pattern(pattern, raw, overrides, false);
            }
            walk_expr(value, raw, overrides);
            walk_expr(body, raw, overrides);
        }
        RowLet {
            entries,
            value,
            body,
            meta,
            ..
        } => {
            mark_leading_keyword(
                raw,
                meta.span,
                "let",
                TokenKind::KeywordDeclaration,
                overrides,
            );
            walk_row_let_entries(entries, raw, overrides);
            walk_expr(value, raw, overrides);
            walk_expr(body, raw, overrides);
        }
        Seq { value, body, .. } => {
            walk_expr(value, raw, overrides);
            walk_expr(body, raw, overrides);
        }
        Elaborator { call, .. } => match call {
            ast::ElaboratorCall::FieldAccess { receiver, labels } => {
                walk_expr(receiver, raw, overrides);
                for label in labels {
                    if let Some(name) = label.label.rsplit('.').next()
                        && let Some(s) = find_last_ident_in_span(raw, label.label_span, name)
                    {
                        overrides.insert(s.start, label_reference_kind(&label.label));
                    }
                }
            }
            ast::ElaboratorCall::FieldUpdate { receiver, updates } => {
                walk_expr(receiver, raw, overrides);
                for update in updates {
                    if let Some(name) = update.label.rsplit('.').next()
                        && let Some(s) = find_last_ident_in_span(raw, update.label_span, name)
                    {
                        overrides.insert(s.start, label_reference_kind(&update.label));
                    }
                    walk_expr(&update.value, raw, overrides);
                }
            }
        },
        RecOrder { ext, .. } | RecQuote { ext, .. } => match *ext {},
        UserElaborator { name, args, .. } => {
            if let Some(s) = find_ident_in_span(raw, e.meta().span, name) {
                overrides.insert(s.start, TokenKind::EntityNameFunctionReference);
            }
            for arg in args {
                match arg {
                    ast::CallArg::Value(value) => walk_expr(value, raw, overrides),
                    ast::CallArg::Type(ty) => walk_type(ty, raw, overrides),
                }
            }
        }
        Tuple { items, .. } => {
            for el in items {
                walk_expr(el, raw, overrides);
            }
        }
        FnPlaceholder { stem, body, .. } => {
            overrides.insert(stem.span.start, TokenKind::VariableParameter);
            walk_expr(body, raw, overrides);
        }
        Ufcs {
            receiver,
            callee_segments,
            args,
            bang,
            ..
        } => {
            walk_expr(receiver, raw, overrides);
            // The trailing callee segment is the dispatched method's
            // name — classify like a Call's callee tail.
            if let Some(last) = callee_segments.last() {
                overrides.insert(last.span.start, TokenKind::EntityNameFunctionReference);
            }
            if let Some(span) = bang {
                overrides.elaborator_bangs.insert(span.start);
            }
            for arg in args {
                walk_call_arg(arg, raw, overrides);
            }
        }
        Unit { .. } => {}
        StrLit { annotation, .. }
        | IntLit { annotation, .. }
        | FloatLit { annotation, .. }
        | BoolLit { annotation, .. } => {
            if let Some(ty) = annotation {
                walk_type(ty, raw, overrides);
            }
        }
        LabelValue { labels, .. } => {
            for label in labels {
                if let Some(name) = label.label.rsplit('.').next()
                    && let Some(s) = find_last_ident_in_span(raw, label.label_span, name)
                {
                    overrides.insert(s.start, label_reference_kind(&label.label));
                }
                walk_expr(&label.value, raw, overrides);
            }
        }
        OpChain { kind, .. } => match kind {
            ast::OpChainKind::Normal { slots, .. } => {
                for slot in slots {
                    walk_expr(slot, raw, overrides);
                }
            }
            ast::OpChainKind::Variadic { elements, .. } => {
                for element in elements {
                    walk_expr(element, raw, overrides);
                }
            }
        },
        // The Enriched* variants are introduced post-Prime
        // (structural-recovery pass); their `ext` witness is
        // `Never` at Surface, so the parser never produces
        // them and this match arm discharges via `match *ext {}`.
        EnrichedTuple { ext, .. }
        | EnrichedProject { ext, .. }
        | EnrichedInject { ext, .. }
        | EnrichedMatch { ext, .. }
        | EnrichedConditional { ext, .. }
        | EnrichedRecord { ext, .. }
        | EnrichedFieldGet { ext, .. } => match *ext {},
        LowHostCall { ext, .. }
        | LowModuleCall { ext, .. }
        | LowQualifiedModuleCall { ext, .. }
        | LowQualifiedNewtypeMember { ext, .. }
        | LowNewtypeCtor { ext, .. }
        | LowNewtypeProj { ext, .. }
        | LowClosureCall { ext, .. }
        | LowIndirectCall { ext, .. }
        | LowTypeApplication { ext, .. }
        | LowAbsurdCall { ext, .. }
        | LowCpsProjectorApply { ext, .. }
        | LowBoundRef { ext, .. }
        | LowHostFnValueRef { ext, .. }
        | LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn walk_row_let_entries(
    entries: &[ast::RowLetEntry],
    raw: &[LexToken],
    overrides: &mut AstOverrides,
) {
    for entry in entries {
        if let Some(name) = entry.label.rsplit('.').next()
            && let Some(span) = find_last_ident_in_span(raw, entry.label_span, name)
        {
            overrides.insert(span.start, label_reference_kind(&entry.label));
        }
        if let Some(span) = find_ident_in_span(raw, entry.local_span, &entry.local) {
            overrides.insert(span.start, TokenKind::VariableParameter);
        }
        if entry.alias_explicit
            && let Some(keyword) = find_ident_in_span(
                raw,
                Span::new(entry.label_span.end, entry.local_span.start),
                "as",
            )
        {
            overrides.insert(keyword.start, TokenKind::KeywordDeclaration);
        }
    }
}

fn walk_call_arg(arg: &ast::CallArg<ast::Surface>, raw: &[LexToken], overrides: &mut AstOverrides) {
    match arg {
        ast::CallArg::Value(value)
            if crate::pass::typecheck_core::value_arg_looks_like_type_arg(value) =>
        {
            match crate::pass::typecheck_core::expr_to_type_arg(value) {
                Ok(ty) => walk_type(&ty, raw, overrides),
                Err(_) => walk_expr(value, raw, overrides),
            }
        }
        ast::CallArg::Value(value) => walk_expr(value, raw, overrides),
        ast::CallArg::Type(ty) => walk_type(ty, raw, overrides),
    }
}

fn label_reference_kind(label: &str) -> TokenKind {
    if label.contains('.') {
        TokenKind::EntityNameQualifiedLabelReference
    } else {
        TokenKind::EntityNameLabelReference
    }
}

// ---- LexToken-stream lookup helpers --------------------------------

/// Some expression owners also admit keyword-free forms. Only the token at
/// the owner's start may be their introducer, never a later binder name.
fn mark_leading_keyword(
    raw: &[LexToken],
    span: Span,
    word: &str,
    kind: TokenKind,
    overrides: &mut AstOverrides,
) {
    if let Some(token) = raw.get(first_token_at_or_after(raw, span.start))
        && token.span.start == span.start
        && is_ident_named(token, word)
    {
        overrides.insert(token.span.start, kind);
    }
}

fn mark_keyword(
    raw: &[LexToken],
    span: Span,
    word: &str,
    kind: TokenKind,
    overrides: &mut AstOverrides,
) {
    if let Some(span) = find_ident_in_span(raw, span, word) {
        overrides.insert(span.start, kind);
    }
}

/// A represented declaration authenticates its leading keyword and the
/// immediately preceding modifiers. Scoped visibility paths remain names.
fn mark_declaration_keyword(
    raw: &[LexToken],
    span: Span,
    keyword: &str,
    overrides: &mut AstOverrides,
) {
    let Some(keyword_span) = find_ident_in_span(raw, span, keyword) else {
        return;
    };
    overrides.insert(keyword_span.start, TokenKind::KeywordDeclaration);
    let mut index = first_token_at_or_after(raw, keyword_span.start);
    while let Some(previous) = index.checked_sub(1) {
        if ident_text(&raw[previous]).is_some_and(|word| matches!(word, "pub" | "pure" | "host")) {
            overrides.insert(raw[previous].span.start, TokenKind::KeywordDeclaration);
            index = previous;
        } else if matches!(raw[previous].kind, LexTokenKind::RParen) {
            let mut open = previous;
            while open > 0 && !matches!(raw[open].kind, LexTokenKind::LParen) {
                open -= 1;
            }
            let Some(head) = open.checked_sub(1) else {
                break;
            };
            if !is_ident_named(&raw[head], "pub") {
                break;
            }
            overrides.insert(raw[head].span.start, TokenKind::KeywordDeclaration);
            for token in &raw[open + 1..previous] {
                if matches!(token.kind, LexTokenKind::Ident(_)) {
                    overrides.insert(token.span.start, TokenKind::EntityNameModule);
                }
            }
            index = head;
        } else {
            break;
        }
    }
}

/// Index of the first token in `raw` whose span starts at or after
/// `byte`. Linear scan over `raw` is `O(n)`; binary search via
/// `partition_point` keeps this `O(log n)` for the AST walker's
/// repeated calls.
fn first_token_at_or_after(raw: &[LexToken], byte: u32) -> usize {
    raw.partition_point(|t| t.span.start < byte)
}

/// Locate the first IDENT token in `raw` within `range` whose text
/// matches `name`. `None` if no such token exists — the caller
/// silently no-ops in that case (a missing override falls back to
/// the lex-only classification, which is the right behavior for
/// invariant violations).
fn find_ident_in_span(raw: &[LexToken], range: Span, name: &str) -> Option<Span> {
    let mut i = first_token_at_or_after(raw, range.start);
    while i < raw.len() && raw[i].span.start < range.end {
        if let LexTokenKind::Ident(n) = &raw[i].kind
            && n == name
        {
            return Some(raw[i].span);
        }
        i += 1;
    }
    None
}

/// Locate the final matching IDENT within `range`. Parsed label paths carry
/// one span for the whole `qualifier.leaf` spelling, so the last match is the
/// leaf even when the qualifier repeats its name (`item.item`).
fn find_last_ident_in_span(raw: &[LexToken], range: Span, name: &str) -> Option<Span> {
    let mut i = first_token_at_or_after(raw, range.start);
    let mut found = None;
    while i < raw.len() && raw[i].span.start < range.end {
        if let LexTokenKind::Ident(n) = &raw[i].kind
            && n == name
        {
            found = Some(raw[i].span);
        }
        i += 1;
    }
    found
}

/// Locate the IDENT named `name` that follows `keyword` within
/// `range`. This is the standard "find the name after the
/// declaration keyword" recovery used when an AST node carries a
/// `name: String` but no `name_span` field — the parser's position
/// invariant is "name follows keyword," so walk past the keyword
/// then find the matching IDENT.
fn find_name_after_kw(raw: &[LexToken], range: Span, keyword: &str, name: &str) -> Option<Span> {
    let mut i = first_token_at_or_after(raw, range.start);
    // Find the keyword.
    while i < raw.len() && raw[i].span.start < range.end {
        if let LexTokenKind::Ident(n) = &raw[i].kind
            && n == keyword
        {
            i += 1;
            // Find the matching name IDENT past the keyword.
            while i < raw.len() && raw[i].span.start < range.end {
                if let LexTokenKind::Ident(m) = &raw[i].kind
                    && m == name
                {
                    return Some(raw[i].span);
                }
                i += 1;
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Emit `tokens` as a JSON array — one entry per line so goldens
/// diff cleanly, with stable key order (`start`, `end`, `kind`) so
/// the rendering doesn't depend on a HashMap iteration order.
///
/// Empty input renders as `[]\n` rather than `[]\n` — keeps the
/// goldens file shape uniform whether the source is empty or full.
pub fn to_json(tokens: &[ClassifiedToken]) -> String {
    if tokens.is_empty() {
        return "[]\n".to_owned();
    }
    let mut out = String::new();
    out.push_str("[\n");
    for (i, t) in tokens.iter().enumerate() {
        let _ = write!(
            out,
            "  {{\"start\": {}, \"end\": {}, \"kind\": \"{}\"}}",
            t.span.start,
            t.span.end,
            t.kind.as_str()
        );
        if i + 1 < tokens.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_patterns_classify_written_signature_binders() {
        let cases: &[(&str, &[&str])] = &[
            (
                "module m; fn f((rec, value)) { let(helper.rec) }",
                &["rec", "value"],
            ),
            (
                "module m; fn f(scalar: ., whole: (first: ., (second: ., third: .), pair: (fourth: ., fifth: .), _: .)) { (scalar, whole, first, second, third, pair, fourth, fifth) }",
                &[
                    "scalar", "whole", "first", "second", "third", "pair", "fourth", "fifth",
                ],
            ),
            (
                "module m; fn f() { .((first: ., pair: (second: ., third: .), _: .), scalar: .) { (first, pair, second, third, scalar) } }",
                &["first", "pair", "second", "third", "scalar"],
            ),
            (
                "module m; equiv same((first: ., pair: (second: ., third: .), _: .), scalar: .) { first; first }",
                &["first", "pair", "second", "third", "scalar"],
            ),
        ];
        let mut failures = Vec::new();
        for &(source, names) in cases {
            let module = crate::pass::parser::parse(source).expect(source);
            let cached = dump_with_module(source, &module).unwrap();
            let probed = dump(source).unwrap();
            assert_eq!(cached, probed, "cached/probe parity: {source}");
            for (route, tokens) in [("cached", &cached), ("probe", &probed)] {
                for token in tokens {
                    let text = &source[token.span.start as usize..token.span.end as usize];
                    let expected = if names.contains(&text) {
                        Some(if source.find(text).unwrap() == token.span.start as usize {
                            TokenKind::VariableParameter
                        } else {
                            TokenKind::Identifier
                        })
                    } else if text == "_" {
                        Some(TokenKind::Slot)
                    } else {
                        None
                    };
                    if let Some(expected) = expected
                        && token.kind != expected
                    {
                        failures.push(format!(
                            "{route}: {source}: {text}@{:?}: {:?} != {expected:?}",
                            token.span, token.kind
                        ));
                    }
                }
                for name in names {
                    assert!(
                        tokens.iter().any(|token| source
                            [token.span.start as usize..token.span.end as usize]
                            == **name),
                        "unvisited binder {name}"
                    );
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn explicit_row_and_existential_lets_keep_written_source_roles() {
        let source = "module m;
            fn run[A](input: A) {
                let .(whole: (first: A, second: A)) = pair;
                let .({field as alias, short}) = row;
                let .(<Hidden> payload) = rhs;
                let .(<Other> (left: Other, right: Other)) = rhs_again;
                let .(<Last> last) = fetch(input);
                (payload, left, right, last)
            }";
        let module = crate::pass::parser::parse(source).expect("source roles require valid syntax");
        let cached = dump_with_module(source, &module).unwrap();
        let probed = dump(source).unwrap();
        assert_eq!(cached, probed, "cached/probe parity");
        for (needle, offset, expected) in [
            ("let .({", 0, TokenKind::KeywordDeclaration),
            ("as alias", 3, TokenKind::VariableParameter),
            ("short}", 0, TokenKind::VariableParameter),
            ("let .(<Hidden>", 0, TokenKind::KeywordDeclaration),
            ("<Hidden>", 1, TokenKind::VariableParameter),
            ("payload) =", 0, TokenKind::Identifier),
            ("rhs;", 0, TokenKind::Identifier),
            ("let .(<Other>", 0, TokenKind::KeywordDeclaration),
            ("left: Other", 0, TokenKind::Identifier),
            ("right: Other", 0, TokenKind::Identifier),
            ("rhs_again;", 0, TokenKind::Identifier),
            ("fetch(input)", 0, TokenKind::EntityNameFunctionReference),
        ] {
            let start = (source.find(needle).unwrap() + offset) as u32;
            assert_eq!(
                cached
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected,
                "{needle}",
            );
        }
    }

    #[test]
    fn existential_source_roles_do_not_reclassify_authored_calls() {
        for callee in ["rhs", "(rhs)"] {
            let source = format!(
                "module m; fn explicit() {{
                {callee}(_, .[Hidden](payload) {{ payload }})
            }}"
            );
            let module = crate::pass::parser::parse(&source).unwrap();
            let cached = dump_with_module(&source, &module).unwrap();
            assert_eq!(cached, dump(&source).unwrap());
            for (needle, expected) in [
                ("rhs", TokenKind::EntityNameFunctionReference),
                ("payload)", TokenKind::VariableParameter),
            ] {
                let start = source.find(needle).unwrap() as u32;
                assert_eq!(
                    cached
                        .iter()
                        .find(|token| token.span.start == start)
                        .unwrap()
                        .kind,
                    expected,
                    "{source}: {needle}",
                );
            }
        }
    }

    #[test]
    fn parameter_patterns_keep_local_bindings_and_unpacked_continuations_neutral() {
        let cases: &[(&str, &[&str])] = &[
            (
                "module m; fn f(input: .) { let .(first: ., pair: (second: ., third: .), _: .) = input; (first, pair, second, third) }",
                &["first", "pair", "second", "third"],
            ),
            (
                "module m; fn f(input: .) { do! input { let .(first: ., second: .) <- input; let .(third: ., fourth: .) = input; (first, second, third, fourth) } }",
                &["first", "second", "third", "fourth"],
            ),
            (
                "module m; fn f(input: .) { let .(<A> (first: A, pair: (second: A, third: A))) = input; (first, pair, second, third) }",
                &["first", "pair", "second", "third"],
            ),
        ];
        for &(source, names) in cases {
            let module = crate::pass::parser::parse(source).expect(source);
            for tokens in [
                dump_with_module(source, &module).unwrap(),
                dump(source).unwrap(),
            ] {
                for name in names {
                    let occurrences: Vec<_> = tokens
                        .iter()
                        .filter(|token| {
                            &source[token.span.start as usize..token.span.end as usize] == *name
                        })
                        .collect();
                    assert!(occurrences.len() >= 2, "binder and body use: {name}");
                    for token in occurrences {
                        assert_eq!(
                            token.kind,
                            TokenKind::Identifier,
                            "{source}: {name}@{:?}",
                            token.span
                        );
                    }
                }
                for token in tokens.iter().filter(|token| {
                    &source[token.span.start as usize..token.span.end as usize] == "_"
                }) {
                    assert_eq!(token.kind, TokenKind::Slot, "{source}");
                }
            }
        }
    }

    #[test]
    fn parameter_patterns_retain_only_validated_parameter_prefix_names() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            (
                "module m; fn f((first: ., second: ",
                &["first", "second"],
                &[],
            ),
            (
                "module m; fn f() { .((first: ., pair: (second: ., ",
                &["first", "pair", "second"],
                &[],
            ),
            (
                "module m; equiv same((first: ., second: ",
                &["first", "second"],
                &[],
            ),
            ("module m; fn f((first: ., Bad: .", &["first"], &["Bad"]),
            (
                "module m; fn f() { let .(first: ., second: ",
                &[],
                &["first", "second"],
            ),
            (
                "module m; fn f(input: .) { do! input { let .(first: ., second: ",
                &[],
                &["first", "second"],
            ),
            (
                "module m; fn f(input: .) { let .(<A> (first: A, second: ",
                &[],
                &["first", "second"],
            ),
        ];
        let mut failures = Vec::new();
        for &(source, parameters, neutral) in cases {
            let probe = crate::pass::parser::probe_tooling_source(source, None);
            assert!(probe.syntax.is_none(), "complete AST: {source}");
            assert!(probe.parse_error.is_some(), "accepted prefix: {source}");
            let tokens = dump(source).unwrap();
            for (names, expected) in [
                (parameters, TokenKind::VariableParameter),
                (neutral, TokenKind::Identifier),
            ] {
                for name in names {
                    let start = source.find(name).unwrap() as u32;
                    let token = tokens
                        .iter()
                        .find(|token| token.span.start == start)
                        .expect(name);
                    if token.kind != expected {
                        failures.push(format!(
                            "{source}: {name}@{start}: {:?} != {expected:?}",
                            token.kind
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn parameter_patterns_distinguish_dot_lambdas_from_rich_let_patterns() {
        let cases = [
            (
                "module m; fn f(input: .) { let . (first: A, second: A) = input; (first, second) }\n",
                false,
            ),
            (
                "module m; fn f(input: .) { let .(first: A, second: A) = input; (first, second) }\n",
                false,
            ),
            (
                "module m; fn f() { . [A]((first: A, second: A)) { (first, second) } }\n",
                true,
            ),
            (
                "module m; fn f() { .[A]((first: A, second: A)) { (first, second) } }\n",
                true,
            ),
        ];
        let mut failures = Vec::new();
        for (source, parameter_owner) in cases {
            let module = crate::pass::parser::parse(source).expect(source);
            for (route, tokens) in [
                ("cached", dump_with_module(source, &module).unwrap()),
                ("probe", dump(source).unwrap()),
            ] {
                for name in ["first", "second"] {
                    let occurrences: Vec<_> = tokens
                        .iter()
                        .filter(|token| {
                            &source[token.span.start as usize..token.span.end as usize] == name
                        })
                        .collect();
                    assert_eq!(occurrences.len(), 2, "binder and body use: {name}");
                    for token in occurrences {
                        let expected = if parameter_owner
                            && token.span.start as usize == source.find(name).unwrap()
                        {
                            TokenKind::VariableParameter
                        } else {
                            TokenKind::Identifier
                        };
                        if token.kind != expected {
                            failures.push(format!(
                                "{route}: {source}: {name}@{:?}: {:?} != {expected:?}",
                                token.span, token.kind
                            ));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn tooling_bridge_cached_block_call_marks_only_the_elaborator_head() {
        let source = "module app; fn run(do: .) { do!(do) { let value <- (); value } }";
        let module = crate::pass::parser::parse(source).unwrap();
        let cached = dump_with_module(source, &module).unwrap();
        let probed = dump(source).unwrap();
        assert_eq!(cached, probed);
        let keyword = source.find("do!(do)").unwrap() as u32;
        for (start, expected) in [
            (keyword, TokenKind::KeywordElaborator),
            (keyword + 4, TokenKind::Identifier),
        ] {
            assert_eq!(
                cached
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected
            );
        }
    }

    #[test]
    fn tooling_bridge_cached_recursive_call_annotations_match_native_roles() {
        let source = "module app; rec(loop) fn run(poly: .) -> . { rec(poly, cont) run(poly) }";
        let module = crate::pass::parser::parse(source).unwrap();
        let cached = dump_with_module(source, &module).unwrap();
        let probed = dump(source).unwrap();
        assert_eq!(cached, probed);
        let call = source.find("rec(poly").unwrap() as u32;
        for (start, expected) in [
            (call, TokenKind::KeywordDeclaration),
            (call + 4, TokenKind::KeywordControl),
            (call + 10, TokenKind::KeywordControl),
        ] {
            assert_eq!(
                cached
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected
            );
        }
        let loop_name = source.find("loop").unwrap() as u32;
        assert_eq!(
            cached
                .iter()
                .find(|token| token.span.start == loop_name)
                .unwrap()
                .kind,
            TokenKind::Identifier
        );
    }

    #[test]
    fn tooling_bridge_contextual_let_binders_are_not_promoted_to_keywords() {
        for source in [
            "module app; fn run() { let let = (); let }",
            "module app; fn run() { let .(let: .) = (); let }",
            "module app; fn run() { let .(let: ., other: .) = ((), ()); let }",
            "module app; fn run() { let .(let: (left: ., right: .)) = ((), ()); let }",
            "module app; fn run() { do! bind { let let = (); let } }",
            "module app; fn run() { do! bind { let let <- (); let } }",
            "module app; fn run() { do! bind { let .(let: ., other: .) <- (); let } }",
        ] {
            let module = crate::pass::parser::parse(source).unwrap();
            let cached = dump_with_module(source, &module).unwrap();
            let probed = dump(source).unwrap();
            let start = source.match_indices("let").nth(1).unwrap().0 as u32;
            for tokens in [&cached, &probed] {
                assert_eq!(
                    tokens
                        .iter()
                        .find(|token| token.span.start == start)
                        .unwrap()
                        .kind,
                    TokenKind::Identifier,
                    "{source}"
                );
            }
            assert_eq!(cached, probed, "{source}");
        }
    }

    #[test]
    fn tooling_bridge_cached_module_matches_probe_with_keyword_shaped_names() {
        let source = "module keyword/module;\n\
            import other/host as package;\n\
            host type Text role(str);\n\
            pub pure fn module(module: .) -> . { let build = module; scope! { build } }\n\
            fn branch(if: .) -> . { if! .t { if } else { module(if) } }\n\
            newtype Box : . { pub(keyword/module) constructor constructor; projector projector; };\n\
            pub(keyword/module) rec newtype Ring : . { constructor ring; projector unring; };\n\
            elab macro : . -> . { captures helper.impl; impl(fills) helper.fills; };\n\
            labels { reused: . }; labels { reused: _ };";
        let module = crate::pass::parser::parse(source).expect("ordinary contextual names");
        let probed = dump(source).unwrap();
        let cached = dump_with_module(source, &module).unwrap();
        assert_eq!(cached.len(), probed.len());
        for (cached, probed) in cached.iter().zip(&probed) {
            assert_eq!(
                cached,
                probed,
                "cached/probe role for {:?}",
                &source[probed.span.start as usize..probed.span.end as usize]
            );
        }
        let at = |needle: &str, offset: usize| {
            let start = (source.find(needle).unwrap() + offset) as u32;
            probed
                .iter()
                .find(|token| token.span.start == start)
                .unwrap()
                .kind
        };
        assert_eq!(at("module(module", 7), TokenKind::VariableParameter);
        assert_eq!(at("= module", 2), TokenKind::Identifier);
        assert_eq!(at("let build", 0), TokenKind::KeywordDeclaration);
        assert_eq!(at("scope! {", 0), TokenKind::KeywordElaborator);
        assert_eq!(at("if! .t", 0), TokenKind::KeywordElaborator);
        assert_eq!(at("{ if }", 2), TokenKind::Identifier);
        assert_eq!(
            at("constructor constructor", 0),
            TokenKind::KeywordDeclaration
        );
        assert_eq!(
            at("constructor constructor", 12),
            TokenKind::EntityNameFunction
        );
    }

    #[test]
    fn tooling_bridge_file_roots_keep_field_keywords_and_inverse_calls_separate() {
        for (source, keyword) in [
            (
                "package package; build { target js { out \"out\"; namespace \"demo\"; } }",
                "target",
            ),
            (
                "signature signature v(1); v(1) { with { module host/type { host type Text role(str); } }; nonbreaking { add { host/type.Text; } } }",
                "with",
            ),
            (
                "dependency dependency; source { path \"package.pkg.kio\"; }",
                "path",
            ),
            (
                "lock lock; resolved { git \"url\"; ref \"ref\"; commit \"commit\"; sig \"sig\"; }",
                "commit",
            ),
        ] {
            let probe = crate::pass::parser::probe_tooling_source(source, None);
            assert!(probe.syntax.is_some(), "{source}: {:?}", probe.parse_error);
            let tokens = dump(source).unwrap();
            let start = source.find(keyword).unwrap() as u32;
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                TokenKind::KeywordDeclaration,
                "{source}"
            );
            let mut words = source.split_whitespace();
            let head = words.next().unwrap();
            let name_start = head.len() as u32 + 1;
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == name_start)
                    .unwrap()
                    .kind,
                TokenKind::Identifier,
                "file name {head}"
            );
        }
        for word in [
            "module",
            "package",
            "signature",
            "dependency",
            "lock",
            "build",
            "target",
            "source",
            "resolved",
            "role",
            "constructor",
            "varop",
        ] {
            let source = format!("{word}(())");
            let tokens = dump(&source).unwrap();
            assert_eq!(
                tokens[0].kind,
                TokenKind::EntityNameFunctionReference,
                "inverse call {word}"
            );
        }
    }

    #[test]
    fn tooling_bridge_failed_body_retains_proved_prefix_without_claiming_suffix() {
        let source = "module app; fn first() { () } fn broken[A](build: A) { ? package } fn trailing() { () }";
        let probe = crate::pass::parser::probe_tooling_source(source, None);
        assert!(probe.parse_error.is_some());
        let tokens = dump(source).unwrap();
        for (word, kind) in [
            ("first", TokenKind::EntityNameFunction),
            ("broken", TokenKind::EntityNameFunction),
            ("build", TokenKind::VariableParameter),
            ("package", TokenKind::Identifier),
            ("trailing", TokenKind::EntityNameFunction),
        ] {
            let start = source.find(word).unwrap() as u32;
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                kind,
                "prefix/suffix {word}"
            );
        }
    }

    #[test]
    #[cfg(feature = "repl-core")]
    fn tooling_bridge_repl_fragment_uses_expression_ast_and_shifts_exact_spans() {
        let expression = ".(module: .) { scope! { module } }";
        let source = format!(":normalize {expression}");
        let offset = ":normalize ".len();
        let tokens = dump_repl_input(&source, offset, None).unwrap();
        let expression_tokens = dump(expression).unwrap();
        let shifted = tokens
            .into_iter()
            .filter(|token| token.span.start >= offset as u32)
            .map(|mut token| {
                token.span.start -= offset as u32;
                token.span.end -= offset as u32;
                token
            })
            .collect::<Vec<_>>();
        assert_eq!(shifted, expression_tokens);
        assert!(
            shifted
                .iter()
                .any(|token| token.kind == TokenKind::VariableParameter)
        );
        assert!(
            shifted
                .iter()
                .any(|token| token.kind == TokenKind::KeywordElaborator)
        );
    }

    #[test]
    fn tooling_bridge_retains_newtype_member_and_row_let_roles_before_body_errors() {
        for (source, needle, offset, expected) in [
            (
                "module app; newtype Box : . { constructor constructor; ? }",
                "constructor constructor",
                12,
                TokenKind::EntityNameFunction,
            ),
            (
                "module app; fn run(row: .) { let .({provider.field as module}) = row; ? }",
                "provider.field",
                9,
                TokenKind::EntityNameQualifiedLabelReference,
            ),
            (
                "module app; fn run(row: .) { let .({field as module}) = row; ? }",
                "field as",
                0,
                TokenKind::EntityNameLabelReference,
            ),
            (
                "module app; fn run(row: .) { let .({field as module}) = row; ? }",
                "as module",
                3,
                TokenKind::VariableParameter,
            ),
        ] {
            let probe = crate::pass::parser::probe_tooling_source(source, None);
            assert!(probe.parse_error.is_some(), "malformed suffix");
            let start = (source.find(needle).unwrap() + offset) as u32;
            let tokens = dump(source).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected,
                "{source}"
            );
        }
    }

    /// Helper: classify a source snippet and return the
    /// `(span, kind-as-str)` pairs in source order. Used by every
    /// test so the assertion shape stays uniform.
    fn dumped(src: &str) -> Vec<(u32, u32, &'static str)> {
        dump(src)
            .unwrap()
            .into_iter()
            .map(|t| (t.span.start, t.span.end, t.kind.as_str()))
            .collect()
    }

    fn dumped_text(src: &str) -> Vec<(&str, &'static str)> {
        dump(src)
            .unwrap()
            .into_iter()
            .map(|t| {
                (
                    &src[t.span.start as usize..t.span.end as usize],
                    t.kind.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn signature_module_paths_follow_exact_ast_spans() {
        let source = "signature app v(2);\n\
            v(1) {\n\
              with { module context/if { pub type Item = .; } };\n\
              breaking {\n\
                add { added/with.Item; module added/body { pub fn item() -> .; } };\n\
                modify { modified/with.Item; module modified/body { pub fn item() -> .; } };\n\
                remove { removed/with.item; module removed/body { item; removed; } }\n\
              }\n\
            }\n\
            v(2) { nonbreaking { remove { second/with.item; } } }\n\
            // removed/body is a comment, not a module path\n";
        crate::pass::parser::parse_signature_file(source, None).expect("complete signature syntax");
        let tokens = dump(source).expect("signature token stream");
        let mut expected = Vec::new();
        for path in [
            "context/if",
            "added/with",
            "added/body",
            "modified/with",
            "modified/body",
            "removed/with",
            "removed/body",
            "second/with",
        ] {
            let start = source.find(path).expect("module path") as u32;
            let slash = path.find('/').expect("two path segments") as u32;
            expected.push(Span::new(start, start + slash));
            expected.push(Span::new(start + slash + 1, start + path.len() as u32));
        }
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.kind == TokenKind::EntityNameModule)
                .map(|token| token.span)
                .collect::<Vec<_>>(),
            expected,
        );
        for (needle, offset, word) in [
            ("removed/with.item", "removed/with.".len(), "item"),
            ("item; removed;", "item; ".len(), "removed"),
        ] {
            let start = (source.find(needle).unwrap() + offset) as u32;
            assert!(tokens.iter().any(|token| {
                token.span == Span::new(start, start + word.len() as u32)
                    && token.kind == TokenKind::Identifier
            }));
        }
    }

    #[test]
    fn malformed_import_keeps_sibling_and_next_declaration_roles() {
        let source = "module c; import missing/provider(op _ = _, {kept}); fn after(value: .) -> . { value }";
        assert!(crate::pass::parser::parse(source).is_err());
        assert!(crate::pass::parser::parse_lazy(source).is_err());
        let (_, errors) = crate::pass::parser::parse_recover_imports(source)
            .expect("recover malformed import selection");
        assert!(!errors.is_empty());
        assert!(dump_if_module_parses(source).unwrap().is_none());

        let classified = dump(source).expect("token dump after import recovery");
        for (spelling, expected) in [
            ("kept", TokenKind::EntityNameLabelReference),
            ("value", TokenKind::VariableParameter),
        ] {
            let start = source.find(spelling).expect("retained token") as u32;
            assert_eq!(
                classified
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected,
                "retained role for {spelling}"
            );
        }
        let value_start = source.find("value").unwrap() as u32;
        let lexical = dump_lexical(&lex(source).unwrap(), source);
        assert_ne!(
            lexical
                .iter()
                .find(|token| token.span.start == value_start)
                .unwrap()
                .kind,
            TokenKind::VariableParameter
        );
    }

    #[test]
    fn imported_operator_ast_keeps_parameter_classification() {
        let source = "module app/main; import app/syntax(varop [% %], op _ => _); fn run(value: .) -> . { [% value => value %] }";
        let value_start = source.find("value").expect("parameter binder") as u32;
        let module =
            crate::pass::parser::parse(source).expect("provider-independent consumer parse");
        let token = dump_with_module(source, &module)
            .expect("token dump")
            .into_iter()
            .find(|token| token.span.start == value_start)
            .expect("parameter token");
        assert_eq!(token.kind, TokenKind::VariableParameter);
    }

    #[test]
    fn declaration_callable_path_leaves_are_function_references() {
        let source = "module x; \
            op _ + _ { impl local; }; \
            varop [* *] { \
              foldr Box.push helpers.empty; \
              finalize helpers.Box.finish; \
            }; \
            elab demo : . -> . { impl helper.run; };";
        let classified = dump(source).expect("token dump");

        for leaf in ["local", "empty", "push", "finish", "run"] {
            assert!(
                classified.iter().any(|token| {
                    &source[token.span.start as usize..token.span.end as usize] == leaf
                        && token.kind == TokenKind::EntityNameFunctionReference
                }),
                "callable leaf `{leaf}` was not classified as a function: {classified:?}"
            );
        }
    }

    #[test]
    fn ordinary_ufcs_and_recursive_callees_are_function_references() {
        let source = "module x; \
            host fn loop[S][R](step: S -> S | R, state: S) -> R; \
            fn call_target(x: .) -> . { x } \
            fn ufcs_target(x: .) -> . { x } \
            rec(loop) fn recursive(x: .) -> . { \
              let _ = call_target(x); \
              let _ = x.>ufcs_target; \
              rec recursive(x) \
            }";
        let probe = crate::pass::parser::probe_tooling_source(source, None);
        assert!(
            probe.syntax.is_some(),
            "error: {:?}; facts: {:?}",
            probe.parse_error,
            probe.facts.source_names
        );
        let classified = dump(source).expect("token dump");

        for name in ["call_target", "ufcs_target", "recursive"] {
            let start = source.rfind(name).expect("callee occurrence") as u32;
            let token = classified
                .iter()
                .find(|token| token.span.start == start)
                .unwrap_or_else(|| panic!("missing callee token `{name}`"));
            assert_eq!(
                token.kind,
                TokenKind::EntityNameFunctionReference,
                "callee `{name}`"
            );
        }
    }

    #[test]
    fn incomplete_function_like_declarations_keep_definition_classification() {
        for name in [
            "foo", "op", "import", "variadic", "foldl1", "finalize", "fold", "role", "if",
        ] {
            let source = format!("module m;\nfn {name}(");
            let definition = dump(&source)
                .expect("lexical fallback token dump")
                .into_iter()
                .find(|token| &source[token.span.start as usize..token.span.end as usize] == name)
                .unwrap_or_else(|| panic!("function-name token `{name}`"));

            assert_eq!(
                definition.kind,
                TokenKind::EntityNameFunction,
                "contextual name `{name}`"
            );
        }

        let source = "module m;\nequiv law(";
        let law = dump(source)
            .expect("lexical fallback token dump")
            .into_iter()
            .find(|token| &source[token.span.start as usize..token.span.end as usize] == "law")
            .expect("equivalence-name token");
        assert_eq!(law.kind, TokenKind::EntityNameFunction);

        let source = "module m;\nfn op[A]";
        let op = dump(source)
            .expect("lexical fallback token dump")
            .into_iter()
            .find(|token| &source[token.span.start as usize..token.span.end as usize] == "op")
            .expect("contextual function-name token");
        assert_eq!(op.kind, TokenKind::EntityNameFunction);
    }

    #[test]
    fn invalid_empty_ufcs_keeps_proved_callees_without_claiming_later_body_calls() {
        let source = "module x; \
            host fn loop[S][R](step: S -> S | R, state: S) -> R; \
            fn call_target(x: .) -> . { x } \
            fn ufcs_target(x: .) -> . { x } \
            rec(loop) fn recursive(x: .) -> . { \
              let _ = call_target(x); \
              let _ = x.>ufcs_target(); \
              rec recursive(x) \
            }";
        let probe = crate::pass::parser::probe_tooling_source(source, None);
        assert!(
            matches!(probe.parse_error, Some(Error::Parse(ref diagnostic)) if diagnostic.message.contains("explicitly empty UFCS"))
        );
        let tokens = dump(source).unwrap();
        for (name, expected) in [
            ("call_target", TokenKind::EntityNameFunctionReference),
            ("ufcs_target", TokenKind::EntityNameFunctionReference),
            ("recursive", TokenKind::Identifier),
        ] {
            let start = source.rfind(name).unwrap() as u32;
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == start)
                    .unwrap()
                    .kind,
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn flat_keyword_inventory_does_not_invent_incomplete_declarations() {
        let source = "fn newtype type equiv elab module";
        let classified = dump(source).expect("lexical fallback token dump");

        for (name, kind) in [
            ("newtype", TokenKind::EntityNameFunction),
            ("elab", TokenKind::KeywordDeclaration),
        ] {
            let token = classified
                .iter()
                .find(|token| &source[token.span.start as usize..token.span.end as usize] == name)
                .unwrap_or_else(|| panic!("keyword token `{name}`"));
            assert_eq!(token.kind, kind, "keyword `{name}`");
        }
    }

    #[test]
    fn item_signature_probe_uses_lazy_header_without_reclassifying_deferred_body() {
        let source = "module kio_doc;\n\
            import syntax(varop [* *]);\n\
            pub fn combine[A](left: A, right: A) -> A { [* left, right ? left *] }";
        assert!(
            crate::pass::parser::parse(source).is_err(),
            "malformed body must not supply eager structural facts"
        );
        assert!(
            crate::pass::parser::parse_lazy(source).is_ok(),
            "lazy parsing must retain the declaration header"
        );

        let tokens = dump_if_module_parses(source)
            .expect("signature probe lexes")
            .expect("lazy parser supplies structural facts");
        let header_open = source.find("[A]").expect("forall open") as u32;
        let header_close = header_open + 2;
        let body_open = source.rfind("[* left").expect("body operator") as u32;
        let kind_at = |offset| {
            tokens
                .iter()
                .find(|token| token.span.start == offset)
                .map(|token| token.kind)
        };
        assert_eq!(kind_at(header_open), Some(TokenKind::PunctuationBracket));
        assert_eq!(kind_at(header_close), Some(TokenKind::PunctuationBracket));
        assert_eq!(kind_at(body_open), Some(TokenKind::OperatorUser));
    }

    #[test]
    fn empty_source_yields_no_tokens() {
        assert!(dump("").unwrap().is_empty());
        assert_eq!(to_json(&[]), "[]\n");
    }

    #[test]
    fn lexical_keyword_spellings_are_neutral_without_parser_facts() {
        let source = "if else match fn newtype type literal labels alias op varop fold equiv module import use from pub pure package pkg bridge host upstream downstream source build dependency lock resolved signature with breaking nonbreaking add modify remove auto let";
        let tokens = dump_lexical(&lex(source).unwrap(), source);
        assert!(
            tokens
                .iter()
                .all(|token| token.kind == TokenKind::Identifier)
        );
    }

    #[test]
    fn unknown_idents_classify_as_identifier() {
        // Anything not in the keyword table is a plain identifier —
        // `case` retired as a contextual keyword when `match!` clauses
        // became ordinary expressions of function type, so it lexes
        // as a plain ident now.
        let got = dumped("foo Bar _baz qux_2 typename as target rec role constructor case");
        let kinds: Vec<&str> = got.iter().map(|(_, _, k)| *k).collect();
        // Every entry above is `identifier`.
        assert!(kinds.iter().all(|k| *k == "identifier"), "got: {kinds:?}");
    }

    #[test]
    fn qualified_import_alias_matching_the_provider_leaf_is_a_module_name() {
        let source = "module app/main; import app/value as value;";
        let tokens = dump(source).expect("token dump");
        let occurrences = source
            .match_indices("value")
            .map(|(start, _)| {
                tokens
                    .iter()
                    .find(|token| token.span.start == start as u32)
                    .map(|token| token.kind)
                    .unwrap_or_else(|| panic!("missing value token at {start}"))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            occurrences,
            [TokenKind::EntityNameModule, TokenKind::EntityNameModule]
        );
    }

    #[test]
    fn as_is_a_keyword_only_in_grammar_positions() {
        let source = concat!(
            "module contextual/keywords;\n",
            "import app/as as as;\n",
            "fn identity(as: .) -> . {\n",
            "  as\n",
            "}\n",
            "fn row(row: .) -> . { let .({field as local}) = row; as }\n",
        );
        let tokens = dump(source).expect("token dump");
        let kinds_for = |spelling: &str| {
            source
                .match_indices(spelling)
                .filter(|(start, _)| {
                    source[..*start]
                        .chars()
                        .next_back()
                        .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
                        && source[*start + spelling.len()..]
                            .chars()
                            .next()
                            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
                })
                .map(|(start, _)| {
                    tokens
                        .iter()
                        .find(|token| token.span.start == start as u32)
                        .unwrap_or_else(|| panic!("missing `{spelling}` token at {start}"))
                        .kind
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            kinds_for("as"),
            [
                TokenKind::EntityNameModule,
                TokenKind::KeywordDeclaration,
                TokenKind::EntityNameModule,
                TokenKind::VariableParameter,
                TokenKind::Identifier,
                TokenKind::KeywordDeclaration,
                TokenKind::Identifier,
            ]
        );
    }

    #[test]
    fn variadic_words_are_keywords_only_at_their_grammar_positions() {
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            let source = format!(
                "module variadic/pkg;
                import variadic/helpers as helper;
                fn variadic(value: .) -> . {{ value }}
                fn ordinary() -> . {{
                    let bare = variadic;
                    let member = helper.variadic;
                    variadic(())
                }}
                varop [* *] {{
                    {mode} helper.{mode} helper.variadic;
                    finalize helper.finalize;
                }};"
            );
            let module = crate::pass::parser::parse(&source).expect("contextual grammar");
            let tokens = dump_with_module(&source, &module).expect("token dump");
            let at = |start: usize| {
                tokens
                    .iter()
                    .find(|token| token.span.start == start as u32)
                    .map(|token| token.kind)
                    .expect("token at source position")
            };
            for (needle, offset, kind) in [
                ("module variadic", 7, TokenKind::EntityNameModule),
                ("import variadic", 7, TokenKind::EntityNameModule),
                ("fn variadic", 3, TokenKind::EntityNameFunction),
                ("bare = variadic", 7, TokenKind::Identifier),
                ("member = helper.variadic", 16, TokenKind::Identifier),
                ("variadic(())", 0, TokenKind::EntityNameFunctionReference),
                ("varop [*", 0, TokenKind::KeywordDeclaration),
                ("finalize helper", 0, TokenKind::KeywordDeclaration),
                ("helper.finalize", 7, TokenKind::EntityNameFunctionReference),
            ] {
                assert_eq!(
                    at(source.find(needle).expect(needle) + offset),
                    kind,
                    "{mode}: {needle}"
                );
            }
            let clause = format!("{mode} helper.{mode}");
            assert_eq!(
                at(source.find(&clause).expect("primary clause")),
                TokenKind::KeywordDeclaration
            );
            assert_eq!(
                at(source.find(&format!("helper.{mode}")).expect("callable") + 7),
                TokenKind::EntityNameFunctionReference
            );
        }
    }

    #[test]
    fn import_tags_do_not_reclassify_ordinary_selected_names() {
        let source = "module app;
            import providers/op(op, import, variadic, foldl, foldr1, finalize, {op},
                op _ + _, varop [% %]);
            fn op(import: .) -> . { import }
            fn import(value: .) -> . { value }";
        let module = crate::pass::parser::parse(source).expect("all explicit namespaces");
        let tokens = dump_with_module(source, &module).expect("token dump");
        let at = |needle: &str, offset: usize| {
            let start = source.find(needle).expect(needle) as u32 + offset as u32;
            tokens
                .iter()
                .find(|token| token.span.start == start)
                .expect(needle)
                .kind
        };
        assert_eq!(at("import providers", 0), TokenKind::KeywordDeclaration);
        assert_eq!(at("/op(", 1), TokenKind::EntityNameModule);
        assert_eq!(at("(op,", 1), TokenKind::Identifier);
        for name in ["import,", "variadic,", "foldl,", "foldr1,", "finalize,"] {
            assert_eq!(at(name, 0), TokenKind::Identifier, "{name}");
        }
        assert_eq!(at("{op}", 1), TokenKind::EntityNameLabelReference);
        assert_eq!(at("op _ +", 0), TokenKind::KeywordDeclaration);
        assert_eq!(at("varop [%", 0), TokenKind::KeywordDeclaration);
        assert_eq!(at("fn op", 3), TokenKind::EntityNameFunction);
        assert_eq!(at("op(import:", 3), TokenKind::VariableParameter);
        assert_eq!(at("{ import }", 2), TokenKind::Identifier);
        assert_eq!(at("fn import", 3), TokenKind::EntityNameFunction);
    }

    #[test]
    fn lexical_variadic_heads_preserve_contextual_names() {
        let function = dumped_text("fn op() -> . { () }");
        assert_eq!(
            function.iter().find(|(text, _)| *text == "op").unwrap().1,
            "entity.name.function"
        );
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            let source = format!(
                "varop [% %] {{ {mode} helper.{mode} helper.variadic; finalize helper.finalize; }};"
            );
            let tokens = dumped_text(&source);
            assert_eq!(
                tokens.iter().find(|(text, _)| *text == "varop").unwrap().1,
                "keyword.declaration"
            );
            let modes = tokens
                .iter()
                .filter(|(text, _)| *text == mode)
                .map(|(_, kind)| *kind)
                .collect::<Vec<_>>();
            assert_eq!(modes, ["keyword.declaration", "entity.name.function"]);
            let finalizers = tokens
                .iter()
                .filter(|(text, _)| *text == "finalize")
                .map(|(_, kind)| *kind)
                .collect::<Vec<_>>();
            assert_eq!(finalizers, ["keyword.declaration", "entity.name.function"]);
        }
        for word in [
            "op", "import", "variadic", "foldl", "foldr", "foldl1", "foldr1", "finalize", "fold",
            "use", "from",
        ] {
            let source = format!("left {word} right");
            assert_eq!(dumped_text(&source)[1], (word, "identifier"));
        }
    }

    #[test]
    fn lexical_fragments_highlight_function_defs_and_calls() {
        let src =
            "// module header intentionally omitted\npub fn main() -> . { print(\"hello\\n\") }";
        let got = dumped_text(src);
        assert!(
            got.contains(&("main", "entity.name.function")),
            "got: {got:?}"
        );
        assert!(
            got.contains(&("print", "entity.name.function")),
            "got: {got:?}"
        );
    }

    #[test]
    fn lexical_call_heads_override_keyword_shaped_names() {
        let src = "fn double(n: Int) -> Int { add(n, n) }";
        let got = dumped_text(src);
        assert!(
            got.contains(&("double", "entity.name.function")),
            "got: {got:?}"
        );
        assert!(
            got.contains(&("add", "entity.name.function")),
            "got: {got:?}"
        );
    }

    #[test]
    fn recursive_group_lead_is_a_keyword_not_a_call() {
        let got = dumped_text("host type String role(str); rec(loop) fn walk() -> . { () }");
        assert!(
            got.contains(&("role", "keyword.declaration")),
            "got: {got:?}"
        );
        assert!(
            got.contains(&("rec", "keyword.declaration")),
            "got: {got:?}"
        );
        assert!(
            got.contains(&("walk", "entity.name.function")),
            "got: {got:?}"
        );
    }

    #[test]
    fn lexical_fallback_keeps_rec_contextual() {
        let source = "rec value; rec(loop); rec { }; rec newtype Cell; rec labels Rows;";
        let raw = lex(source).expect("lexical fixture");
        let got = dump_lexical(&raw, source)
            .into_iter()
            .filter(|token| &source[token.span.start as usize..token.span.end as usize] == "rec")
            .map(|token| token.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
                TokenKind::Identifier,
            ]
        );
    }

    #[test]
    #[cfg(all(feature = "surface", feature = "lsp"))]
    fn recursive_call_inside_an_operator_operand_remains_a_member_fact() {
        let source = concat!(
            "module pkg/main;\n",
            "host fn loop[S][R](step: S -> S | R, state: S) -> R;\n",
            "fn choose(left: ., right: .) -> . { left }\n",
            "op _ + _ { impl choose; };\n",
            "rec(loop) {\n",
            "  fn first(value: .) -> . { rec(cont) second(value) + value };\n",
            "  fn second(value: .) -> . { rec first(value) }\n",
            "}\n",
        );
        let module = crate::pass::parser::parse(source).expect("operator-aware module parse");
        let (_, members) =
            dump_module_with_rec_facts(source, &module).expect("parser-driven token facts");
        let first = members
            .iter()
            .find(|member| member.name == "first")
            .expect("first recursive member");
        let second_call = first
            .calls
            .iter()
            .find(|(name, _)| name == "second")
            .expect("recursive second call inside operator operand");
        assert_eq!(
            &source[second_call.1.start as usize..second_call.1.end as usize],
            "second"
        );
    }

    #[test]
    fn signature_version_heads_are_keywords_not_call_heads() {
        let got = dumped_text("signature app v(1);\nv(1) { nonbreaking { add {} } }");
        let version_heads = got
            .iter()
            .filter(|(text, _)| *text == "v")
            .map(|(_, kind)| *kind)
            .collect::<Vec<_>>();
        assert_eq!(
            version_heads,
            ["keyword.declaration", "keyword.declaration"]
        );
    }

    #[test]
    fn representative_elaborator_bang_calls_fuse_into_one_keyword_elaborator() {
        // Contiguous identifier-plus-bang tokens fuse into one
        // `keyword.elaborator` span covering both bytes. This loop samples
        // the elaborator names used across the corpus.
        for src in [
            "iso!",
            "into!",
            "onto!",
            "align!",
            "ease!",
            "atom!",
            "reorder_sum!",
            "reorder_prod!",
            "narrow_sum!",
            "narrow_prod!",
            "widen_sum!",
            "widen_prod!",
            "flatten_sum!",
            "flatten_prod!",
            "one_sum!",
            "one_prod!",
            "fit!",
            "match!",
            "derive!",
        ] {
            let got = dumped(src);
            assert_eq!(got.len(), 1, "for {src:?}: got {got:?}");
            let (start, end, kind) = got[0];
            assert_eq!(start, 0);
            assert_eq!(end as usize, src.len());
            assert_eq!(kind, "keyword.elaborator");
        }
    }

    #[test]
    fn parser_confirmed_bare_bang_ufcs_splits_greedy_symbol_runs() {
        let src = "module x;\n\
                   fn run(r: ., s: .) -> . {\n\
                     transform!.<r;\n\
                     transform!.<<r;\n\
                     r.>transform!.>next;\n\
                     s.>transform!.>>next;\n\
                     ()\n\
                   }";
        let interesting = dumped_text(src)
            .into_iter()
            .filter(|(text, _)| matches!(*text, "transform!" | ".>" | ".>>" | ".<" | ".<<"))
            .collect::<Vec<_>>();
        assert_eq!(
            interesting,
            [
                ("transform!", "keyword.elaborator"),
                (".<", "operator.builtin"),
                ("transform!", "keyword.elaborator"),
                (".<<", "operator.builtin"),
                (".>", "operator.builtin"),
                ("transform!", "keyword.elaborator"),
                (".>", "operator.builtin"),
                (".>", "operator.builtin"),
                ("transform!", "keyword.elaborator"),
                (".>>", "operator.builtin"),
            ]
        );

        assert_eq!(
            dumped("foo!=bar"),
            [
                (0, 3, "identifier"),
                (3, 5, "operator.user"),
                (5, 8, "identifier"),
            ],
            "lexical fallback must not guess that a bang-led user operator is an elaborator"
        );
    }

    #[test]
    fn elaborator_bang_call_does_not_fuse_with_intervening_space() {
        // `iso !` — the space breaks contiguity, so we don't fuse.
        // `iso` becomes plain identifier and `!` the operator.builtin
        // standalone.
        let got = dumped("iso !");
        assert_eq!(got, vec![(0, 3, "identifier"), (4, 5, "operator.builtin"),]);
    }

    #[test]
    fn arbitrary_idents_followed_by_bang_fuse_as_elaborators() {
        let got = dumped("foo!");
        assert_eq!(got, vec![(0, 4, "keyword.elaborator")]);

        let got = dumped("else!");
        assert_eq!(got, vec![(0, 5, "keyword.elaborator")]);
    }

    #[test]
    fn bang_fusion_uses_the_exact_value_reference_word_role() {
        for source in ["foo1_bar2!", "_foo__!", "__foo_bar!", "___foo_bar__!"] {
            assert_eq!(
                dumped(source),
                [(0, source.len() as u32, "keyword.elaborator")],
                "{source}"
            );
        }
        for source in [
            "Foo!",
            "_Foo!",
            "__Foo!",
            "fooBar!",
            "a1b!",
            "foo_1!",
            "foo__bar!",
            "_!",
        ] {
            let tokens = dumped(source);
            assert_eq!(tokens.len(), 2, "{source}: {tokens:?}");
            assert!(
                tokens
                    .iter()
                    .all(|(_, _, kind)| *kind != "keyword.elaborator"),
                "{source}: {tokens:?}"
            );
        }
    }

    #[test]
    fn bool_literals_classify_as_literal_bool() {
        let got = dumped(".t .f");
        assert_eq!(got, vec![(0, 2, "literal.bool"), (3, 5, "literal.bool"),]);
    }

    #[test]
    fn bool_words_classify_as_identifiers() {
        let got = dumped("true false");
        assert_eq!(got, vec![(0, 4, "identifier"), (5, 10, "identifier"),]);
    }

    #[test]
    fn number_literals_classify_as_literal_number() {
        let got = dumped("0 42 3.14 1.0e-9");
        let kinds: Vec<&str> = got.iter().map(|(_, _, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                "literal.number",
                "literal.number",
                "literal.number",
                "literal.number"
            ]
        );
    }

    #[test]
    fn string_literal_classifies_as_literal_string() {
        let got = dumped(r#""hello""#);
        assert_eq!(got, vec![(0, 7, "literal.string")]);
    }

    #[test]
    fn structural_punctuation_and_separators() {
        let got = dumped("( ) { } , ; .");
        let kinds: Vec<&str> = got.iter().map(|(_, _, k)| *k).collect();
        assert_eq!(
            kinds,
            vec![
                "punctuation.bracket",
                "punctuation.bracket",
                "punctuation.bracket",
                "punctuation.bracket",
                "punctuation.separator",
                "punctuation.separator",
                "punctuation.separator",
            ]
        );
    }

    #[test]
    fn square_brackets_classify_by_parser_context() {
        let src = "module x; fn id[, *F,][,, **G,,,][A](f: F(A), x: A) -> A { x }";
        let got = dumped_text(src);
        let binder_symbols: Vec<_> = got
            .into_iter()
            .filter(|(text, _)| matches!(*text, "[" | "]") || text.bytes().all(|byte| byte == b'*'))
            .collect();
        assert_eq!(
            binder_symbols,
            vec![
                ("[", "punctuation.bracket"),
                ("*", "operator.user"),
                ("]", "punctuation.bracket"),
                ("[", "punctuation.bracket"),
                ("**", "operator.user"),
                ("]", "punctuation.bracket"),
                ("[", "punctuation.bracket"),
                ("]", "punctuation.bracket"),
            ]
        );

        let operators = dumped_text("[! [ ! ]] ] ]");
        assert_eq!(
            operators,
            vec![
                ("[!", "operator.user"),
                ("[", "operator.user"),
                ("!", "operator.builtin"),
                ("]]", "operator.user"),
                ("]", "operator.user"),
                ("]", "operator.user"),
            ]
        );
    }

    #[test]
    fn reserved_operators_classify_as_operator_builtin() {
        // The reserved op-token set: `.>`, `.>>`, `.<`, `.<<`, `&`, `|`, `=`, `:`, `!`, `->`.
        let got = dumped(".> .>> .< .<< & | = : ! ->");
        let kinds: Vec<&str> = got.iter().map(|(_, _, k)| *k).collect();
        assert!(
            kinds.iter().all(|k| *k == "operator.builtin"),
            "got: {kinds:?}"
        );
    }

    #[test]
    fn user_operator_runs_classify_as_operator_user() {
        // Anything not in the reserved set is `operator.user` — even
        // `<` / `>`, which the grammar does pin to type-arg brackets
        // in some positions but which can also be user-declared
        // comparison operators (open-world means the lexer can't
        // commit either way).
        let got = dumped("+ ++ <= < > ~ ? [ ] [! ]] ]-");
        let kinds: Vec<&str> = got.iter().map(|(_, _, k)| *k).collect();
        assert!(
            kinds.iter().all(|k| *k == "operator.user"),
            "got: {kinds:?}"
        );
    }

    #[test]
    fn slot_tokens_classify_as_slot() {
        let got = dumped("_ __ ___");
        assert_eq!(got, vec![(0, 1, "slot"), (2, 4, "slot"), (5, 8, "slot"),]);
    }

    #[test]
    fn hash_has_ordinary_operator_coloring() {
        let got = dumped("# #1 #42");
        assert_eq!(
            got,
            vec![
                (0, 1, "operator.user"),
                (2, 3, "operator.user"),
                (3, 4, "literal.number"),
                (5, 6, "operator.user"),
                (6, 8, "literal.number"),
            ]
        );
    }

    #[test]
    fn placeholder_family_uses_ordinary_identifier_roles() {
        let source = "module x; import m(op _ + _); fn main() -> . { .t. { print(t1) } + () }";
        let module = crate::pass::parser::parse(source).expect("complete module parses");
        let raw = lex(source).expect("lex");
        let start = source.find("t1").expect("numbered reference") as u32;
        for tokens in [
            dump_lexical(&raw, source),
            dump_with_ast(&raw, &module, source),
        ] {
            let at_index = tokens
                .iter()
                .filter(|token| token.span.start >= start && token.span.start < start + 2)
                .collect::<Vec<_>>();
            assert_eq!(at_index.len(), 1);
            assert_eq!(at_index[0].span, Span::new(start, start + 2));
            assert_eq!(at_index[0].kind, TokenKind::Identifier);
        }
        let intro = source.find(".t.").unwrap() as u32;
        let tokens = dump_with_ast(&raw, &module, source);
        assert!(
            tokens
                .iter()
                .any(|token| token.span == Span::new(intro + 1, intro + 2)
                    && token.kind == TokenKind::VariableParameter)
        );
        assert!(
            !tokens
                .iter()
                .any(|token| token.kind == TokenKind::LiteralBool)
        );
    }

    #[test]
    fn line_comments_emit_as_comment_line_tokens() {
        // Comments are lexer trivia, not standalone tokens, but the
        // highlighter lifts them into the dump with the span the
        // lexer recorded (covering `// …` up to but not including the
        // newline). Trailing whitespace is part of the comment's
        // span — only `text` strips it.
        let src = "// hello\nfoo // tail\nbar";
        let got = dumped(src);
        assert_eq!(
            got,
            vec![
                (0, 8, "comment.line"),   // `// hello`
                (9, 12, "identifier"),    // `foo`
                (13, 20, "comment.line"), // `// tail`
                (21, 24, "identifier"),   // `bar`
            ]
        );
    }

    #[test]
    fn doc_comments_emit_as_comment_doc_tokens() {
        // `///` doc-comment lines emit as `comment.doc`. (A fourth
        // slash — `////` — is a lex error, not a ruler; see
        // `four_slashes_are_a_lex_error`.)
        let src = "/// doc line\n/// another\nfoo\nbar";
        let got = dumped(src);
        assert_eq!(
            got,
            vec![
                (0, 12, "comment.doc"),  // `/// doc line`
                (13, 24, "comment.doc"), // `/// another`
                (25, 28, "identifier"),  // `foo`
                (29, 32, "identifier"),  // `bar`
            ]
        );
    }

    #[test]
    fn four_slashes_are_a_lex_error() {
        // `////` is not a comment ruler — the `///` marker must be
        // followed by whitespace or end-of-line, and a fourth `/` is
        // rejected (the `//…` operator family is reserved).
        assert!(dump("//// ruler\n").is_err());
        // The whitespace-after-marker rule also rejects a marker
        // flush against a non-slash, non-whitespace character.
        assert!(dump("//no-space\n").is_err());
        assert!(dump("///doc-no-space\n").is_err());
    }

    #[test]
    fn trailing_eof_comment_is_recovered() {
        // A comment at the very end of the file (no trailing
        // newline) is leading trivia of no meaningful token, so the
        // lexer drops it. The dump runs a tail scan to recover any
        // trailing line comments so editors don't see a silent gap
        // at file end.
        assert_eq!(dumped("// trailing"), vec![(0, 11, "comment.line")]);
        // Multiple trailing comments separated by newlines.
        assert_eq!(
            dumped("foo\n// one\n// two\n"),
            vec![
                (0, 3, "identifier"),
                (4, 10, "comment.line"),
                (11, 17, "comment.line"),
            ]
        );
        // All-comments file (no meaningful tokens at all).
        assert_eq!(
            dumped("// header\n// body"),
            vec![(0, 9, "comment.line"), (10, 17, "comment.line"),]
        );
    }

    #[test]
    fn json_format_matches_canonical_shape() {
        // Hand-roll a tiny dump and check the JSON output's exact
        // shape — one entry per line, key order `start`, `end`,
        // `kind`, trailing newline.
        let toks = vec![
            ClassifiedToken {
                span: Span::new(0, 2),
                kind: TokenKind::KeywordDeclaration,
            },
            ClassifiedToken {
                span: Span::new(3, 6),
                kind: TokenKind::Identifier,
            },
        ];
        let want = "[\n  \
            {\"start\": 0, \"end\": 2, \"kind\": \"keyword.declaration\"},\n  \
            {\"start\": 3, \"end\": 6, \"kind\": \"identifier\"}\n\
            ]\n";
        assert_eq!(to_json(&toks), want);
    }

    #[test]
    fn end_to_end_smoke_module_header() {
        // A small slice of a regular module file, checked end-to-end:
        // module declaration, import statement, fn definition with body.
        let src = "module foo;\nimport baz(bar);\nfn id(x) { x }\n";
        let json = to_json(&dump(src).unwrap());
        // Module decl: `module` kw.decl, `foo` ident, `;` separator.
        assert!(
            json.contains("\"start\": 0, \"end\": 6, \"kind\": \"keyword.declaration\""),
            "missing `module` kw.decl in:\n{json}"
        );
        // Import: the declaration keyword and module-owned provider have distinct roles.
        assert!(
            json.contains("\"kind\": \"keyword.declaration\""),
            "missing keyword.declaration in:\n{json}"
        );
        // Body: braces as punctuation.bracket.
        assert!(
            json.contains("\"kind\": \"punctuation.bracket\""),
            "missing brackets in:\n{json}"
        );
    }

    // ---- Parser-driven classification --------------------------
    //
    // The tests below exercise the AST walker. Each test starts
    // with a snippet that parses cleanly, so the parser-driven
    // path runs; the `parse_failure_falls_back_to_lexical` test
    // covers the fallback case.

    fn find_kind_at(toks: &[(u32, u32, &'static str)], start: u32) -> Option<&'static str> {
        toks.iter()
            .find(|(s, _, _)| *s == start)
            .map(|(_, _, k)| *k)
    }

    #[test]
    fn module_path_segments_classify_as_entity_name_module() {
        // `module a/b;` — both path segments are module names; the
        // leading `module` keyword stays at keyword.declaration.
        let src = "module a/b;\n";
        let toks = dumped(src);
        assert_eq!(find_kind_at(&toks, 0), Some("keyword.declaration")); // module
        assert_eq!(find_kind_at(&toks, 7), Some("entity.name.module")); // a
        assert_eq!(find_kind_at(&toks, 9), Some("entity.name.module")); // b
    }

    #[test]
    fn import_path_classifies_as_entity_name_module() {
        // Every written provider segment has the module-name role.
        let src = "module m;\nimport bar/baz(foo);\n";
        let toks = dumped(src);
        for name in ["bar", "baz"] {
            let start = src.find(name).expect("provider segment") as u32;
            assert_eq!(find_kind_at(&toks, start), Some("entity.name.module"));
        }
    }

    #[test]
    fn import_intrinsics_target_classifies_as_entity_name_module() {
        // The reserved builtin provider shares the ordinary module-name role.
        let src = "module m;\nimport __intrinsics__;\n";
        let toks = dumped(src);
        let target = src.find("__intrinsics__").expect("builtin provider") as u32;
        assert_eq!(find_kind_at(&toks, target), Some("entity.name.module"));
    }

    #[test]
    fn fn_def_name_and_params_classify() {
        // `fn id[A](x: A) -> A { x }` — `id` is
        // entity.name.function, the type binder `A` and value param
        // `x` are variable.parameter, and the `x` body reference
        // stays identifier (parameter uses aren't refined in this
        // pass). The return-type `A` reference is a type reference.
        let src = "module m;\nfn id[A](x: A) -> A { x }\n";
        let toks = dumped(src);
        // `id` at byte 13 (after `fn ` at 10-12 + space).
        assert_eq!(find_kind_at(&toks, 13), Some("entity.name.function"));
        // `A` (type binder inside `[A]`) at byte 16.
        assert_eq!(find_kind_at(&toks, 16), Some("variable.parameter"));
        // `x` value param at byte 19.
        assert_eq!(find_kind_at(&toks, 19), Some("variable.parameter"));
        // `x` body reference at byte 32 — stays identifier.
        assert_eq!(find_kind_at(&toks, 32), Some("identifier"));
    }

    #[test]
    fn newtype_name_and_members_classify() {
        // Ordinary and marked type names receive the same semantic roles.
        // Trailing `;` is part of the newtype-decl grammar.
        let src = "module m;\npub newtype Box[A] : A { pub constructor mk_box; pub projector un_box; };\npub newtype _Box[_A] : _A { pub constructor mk_marked; pub projector un_marked; };\n";
        let toks = dumped_text(src);
        for name in ["Box", "_Box"] {
            assert!(
                toks.contains(&(name, "entity.name.type")),
                "{name}: {toks:?}"
            );
        }
        for name in ["A", "_A"] {
            assert!(
                toks.contains(&(name, "variable.parameter")),
                "{name}: {toks:?}"
            );
            assert!(
                toks.contains(&(name, "entity.name.type")),
                "{name}: {toks:?}"
            );
        }
        for name in ["mk_box", "un_box", "mk_marked", "un_marked"] {
            assert!(
                toks.contains(&(name, "entity.name.function")),
                "{name}: {toks:?}"
            );
        }
    }

    #[test]
    fn raw_call_type_arguments_classify_as_types_by_exact_spelling() {
        let src = "module m; fn id[A](x: A) -> A { x } \
                   fn use_types[A][_A](x: A, y: _A) -> _A { \
                     let _ = id(A, x); id(_A, y) \
                   }";
        let toks = dumped(src);
        for call in ["id(A, x)", "id(_A, y)"] {
            let type_arg = src.find(call).unwrap() + "id(".len();
            assert_eq!(
                find_kind_at(&toks, type_arg as u32),
                Some("entity.name.type"),
                "{call}"
            );
        }
    }

    #[test]
    fn labels_entry_names_classify_as_entity_name_label() {
        // `labels { foo: Int, bar: String };` — entry names are labels.
        // Trailing `;` is part of the labels-decl grammar.
        let src = "module m;\nlabels { foo: Int, bar: String };\n";
        let toks = dumped(src);
        // `foo` at byte 19.
        assert_eq!(find_kind_at(&toks, 19), Some("entity.name.label"));
        // `bar` at byte 29.
        assert_eq!(find_kind_at(&toks, 29), Some("entity.name.label"));
    }

    #[test]
    fn label_value_labels_distinguish_declarations_from_references() {
        let src = "module m;\nlabels { red: ., blue: . };\nfn pick() -> Red { {red = ()} }\n";
        let toks = dump(src).expect("token dump");
        assert_eq!(
            toks.iter()
                .find(|token| token.span.start == 19)
                .map(|token| token.kind),
            Some(TokenKind::EntityNameLabel)
        );
        assert_eq!(
            toks.iter()
                .find(|token| token.span.start == 27)
                .map(|token| token.kind),
            Some(TokenKind::EntityNameLabel)
        );
        assert_eq!(
            toks.iter()
                .find(|token| token.span.start == 58)
                .map(|token| token.kind),
            Some(TokenKind::EntityNameLabelReference)
        );
    }

    #[test]
    fn forwarded_label_tokens_distinguish_the_local_binding_and_target_leaf() {
        let source = "module m; type {item} = {item.item}; type {other} = {item};";
        let tokens = dump(source).expect("forwarding token dump");
        let kind_at = |start: usize| {
            tokens
                .iter()
                .find(|token| token.span.start == start as u32)
                .map(|token| token.kind)
        };
        let local = source.find("{item}").unwrap() + 1;
        let qualified = source.find("item.item").unwrap();
        let bare = source.rfind("{item}").unwrap() + 1;
        assert_eq!(kind_at(local), Some(TokenKind::EntityNameLabel));
        assert_eq!(kind_at(qualified), Some(TokenKind::Identifier));
        assert_eq!(
            kind_at(qualified + "item.".len()),
            Some(TokenKind::EntityNameQualifiedLabelReference)
        );
        assert_eq!(kind_at(bare), Some(TokenKind::EntityNameLabelReference));
    }

    #[test]
    fn repeated_qualified_label_name_classifies_the_leaf_for_every_form() {
        let src = concat!(
            "module m;\n",
            "fn forms(row: Foo) -> Foo {\n",
            "  let .({item.item as local}) = row;\n",
            "  let accessed = row.?{item.item};\n",
            "  let updated = row.!{item.item = accessed};\n",
            "  {item.item = updated}\n",
            "}\n",
        );
        let toks = dumped(src);
        let paths: Vec<_> = src
            .match_indices("item.item")
            .map(|(start, _)| start)
            .collect();
        assert_eq!(paths.len(), 4, "row-let, access, update, and value");
        for start in paths {
            assert_eq!(
                find_kind_at(&toks, start as u32),
                Some("identifier"),
                "qualifier at byte {start} must not be rebound as the label leaf"
            );
            assert_eq!(
                find_kind_at(&toks, start as u32 + "item.".len() as u32),
                Some("entity.name.label"),
                "final label segment at byte {start} was not classified"
            );
        }
    }

    #[test]
    fn call_site_callee_classifies_as_entity_name_function() {
        // `fn main() -> . { print("hi") }` — `print` is the call's
        // callee path; classify the trailing segment as
        // entity.name.function.
        let src = "module m;\nfn main() -> . { print(\"hi\") }\n";
        let toks = dumped(src);
        // `print` at byte 27.
        assert_eq!(find_kind_at(&toks, 27), Some("entity.name.function"));
    }

    #[test]
    fn parse_failure_falls_back_to_lexical() {
        // A standalone elaborator bang call doesn't parse as a regular
        // module, so the dump falls back to dump_lexical and produces the
        // lex-only result.
        let src = "iso!";
        let toks = dumped(src);
        assert_eq!(toks, vec![(0, 4, "keyword.elaborator")]);
    }

    #[test]
    fn malformed_imported_operator_body_keeps_lazy_header_roles() {
        let src = "module x;\n\nimport m(op _ + _, op _ ? _, foo);\n\nfn main() -> . { .x. { print(x1) } + () ? }\n";
        assert!(
            crate::pass::parser::parse(src).is_err(),
            "the incomplete final operator must keep the body deferred"
        );
        assert!(crate::pass::parser::parse_lazy(src).is_ok());

        let toks = dumped(src);
        let declared = src.find('x').expect("declared module") as u32;
        let provider = (src.find("import m").expect("provider module") + "import ".len()) as u32;
        let call = src.find("print").expect("call head") as u32;
        let indexed = src.find("x1").expect("numbered reference") as u32;
        assert_eq!(find_kind_at(&toks, declared), Some("entity.name.module"));
        assert_eq!(find_kind_at(&toks, provider), Some("entity.name.module"));
        assert_eq!(find_kind_at(&toks, call), Some("entity.name.function"));
        assert!(
            toks.contains(&(indexed, indexed + 2, "identifier")),
            "numbered reference keeps ordinary identifier coloring: {toks:?}"
        );
    }
}

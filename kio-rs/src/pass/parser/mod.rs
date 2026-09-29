//! Recursive-descent parser for Kio'.
//!
//! Consumes the token stream from [`crate::pass::lexer`] and produces an [`ast::Module`]
//! covering the regular-module shape: `module <path>;` declaration, `import`
//! statements, and top-level `fn` / `type` / `newtype` items, plus
//! the value- and type-expression forms in Kio'.
//!
//! Surface-only constructs (`match!`, `if` / `else`, `labels`, the
//! `.@` elaborator prefix) are recognized at the lexer level and rejected here
//! with a parse error, so a Kio' program containing any of them fails to
//! compile rather than being silently accepted.
//!
//! Naming conventions are also enforced at this layer (per spec, the
//! compiler enforces them, not just convention): type names must match
//! `_?[A-Z][a-z0-9_]*`, value-shaped names (function definitions and params,
//! members, module segments) must match `[a-z_][a-z0-9_]*`, and no user
//! identifier may begin with `__` (reserved for compiler-generated
//! names).

#[cfg(any(test, feature = "repl-core"))]
use crate::ast::Type;
use crate::ast::{BuildBlock, DependencyFile, LockFile, Module, PackageFile, SignatureFile};
use crate::error::Error;
use crate::pass::lexer::{lex, lex_with_trailing};

pub(crate) struct SourceExistentialLet<'a> {
    pub(crate) value: &'a crate::ast::Expr,
    pub(crate) type_params: &'a [crate::ast::SignatureParam],
    pub(crate) binder: &'a crate::ast::Param,
    pub(crate) body: &'a crate::ast::Expr,
}

pub(crate) fn source_existential_let(expr: &crate::ast::Expr) -> Option<SourceExistentialLet<'_>> {
    use crate::ast::{CallArg, Expr, SignatureParam, Type};
    let Expr::Call {
        callee, args, meta, ..
    } = expr
    else {
        return None;
    };
    let [
        CallArg::Type(Type::Infer { meta: hole, .. }),
        CallArg::Value(Expr::FnExpr {
            sig,
            ret_ty: None,
            body,
            meta: continuation,
            ..
        }),
    ] = args.as_slice()
    else {
        return None;
    };
    // A parsed opening shares its span with the synthetic continuation;
    // an authored lambda starts inside the enclosing call instead.
    if continuation.span != meta.span
        || hole.span != callee.span()
        || meta.span.start >= callee.span().start
    {
        return None;
    }
    let (SignatureParam::Value(binder), type_params) = sig.params.split_last()? else {
        return None;
    };
    if type_params.is_empty()
        || !type_params
            .iter()
            .all(|param| matches!(param, SignatureParam::Type(_)))
        || binder.ty.is_some()
        || binder.name == "_"
    {
        return None;
    }
    Some(SourceExistentialLet {
        value: callee,
        type_params,
        binder,
        body,
    })
}

/// Lex `source` and build its tree-skeleton, carrying the
/// end-of-file trivia run (the comments / newlines after the last
/// token) onto the skeleton so the formatter can preserve an
/// end-of-file comment. Used by the full-file parse entry points;
/// the token-stream-only helpers that don't build a `Module` keep
/// the plain [`lex`] + [`crate::pass::tree_skeleton::build`] path.
fn lex_and_build(source: &str) -> Result<crate::pass::tree_skeleton::SkeletonFile, Error> {
    let (tokens, trailing) = lex_with_trailing(source)?;
    Ok(crate::pass::tree_skeleton::build_with_trailing(
        tokens, trailing,
    ))
}

#[cfg(any(test, feature = "cli"))]
#[allow(clippy::large_enum_variant)] // a single-file probe owns one AST inline, not a recursive collection
pub(crate) enum ToolingSyntax {
    Module(Module),
    Package(PackageFile),
    Signature(SignatureFile),
    Dependency(DependencyFile),
    Lock(LockFile),
    Declarations {
        imports: Vec<crate::ast::Import>,
        items: Vec<crate::ast::Item>,
    },
    Expression(crate::ast::Expr),
}

#[cfg(any(test, feature = "cli"))]
pub(crate) struct ToolingProbe<'a> {
    pub(crate) source: &'a str,
    #[cfg(test)]
    pub(crate) kind: Option<crate::ast::KioFileKind>,
    pub(crate) facts: core::ParserFacts,
    #[cfg(any(test, feature = "lsp"))]
    pub(crate) lexical_error: Option<Error>,
    #[cfg(any(test, feature = "lsp"))]
    pub(crate) parse_error: Option<Error>,
    pub(crate) syntax: Option<ToolingSyntax>,
}

#[cfg(any(test, feature = "lsp"))]
pub(crate) fn probe_tooling<'a>(
    source: &'a str,
    kind: Option<crate::ast::KioFileKind>,
    cursor: Option<u32>,
    expression_context: Option<&ExpressionParseContext>,
) -> ToolingProbe<'a> {
    probe_tooling_root(source, kind, cursor, expression_context, false)
}

#[cfg(any(test, feature = "cli"))]
pub(crate) fn probe_tooling_source(source: &str, cursor: Option<u32>) -> ToolingProbe<'_> {
    probe_tooling_root(source, None, cursor, None, true)
}

#[cfg(any(test, feature = "cli"))]
fn probe_tooling_root<'a>(
    source: &'a str,
    mut kind: Option<crate::ast::KioFileKind>,
    cursor: Option<u32>,
    expression_context: Option<&ExpressionParseContext>,
    infer_root: bool,
) -> ToolingProbe<'a> {
    use crate::ast::KioFileKind;
    let crate::pass::lexer::LexPrefix {
        tokens,
        trailing,
        error: lexical_error,
        frontier,
    } = crate::pass::lexer::lex_prefix(source);
    let suppression = cursor.and_then(|cursor| {
        tokens
            .iter()
            .find_map(|token| core::token_cursor_suppression(token, cursor))
            .or_else(|| core::trivia_cursor_suppression(&trailing, cursor))
            .or_else(|| {
                (lexical_error.is_some() && frontier <= cursor).then_some(core::CursorSuppression {
                    span: crate::span::Span::new(frontier, source.len() as u32),
                    kind: core::CursorSuppressionKind::LexicalError,
                })
            })
    });
    let skeleton = crate::pass::tree_skeleton::build_with_trailing(tokens, trailing);
    let mut parser = if kind.is_none() {
        expression_parser(&skeleton, frontier as usize, expression_context)
    } else {
        Parser::new(&skeleton, frontier as usize)
    };
    let declarations = if infer_root {
        let root = parser.tooling_source_root();
        kind = root.0;
        root.1
    } else {
        false
    };
    parser.enable_tooling(
        cursor.filter(|cursor| {
            *cursor < frontier || (lexical_error.is_none() && *cursor == frontier)
        }),
    );
    parser.set_cursor_suppression(suppression);
    let parsed = match kind {
        Some(KioFileKind::Module) => parser.module_tooling().map(ToolingSyntax::Module),
        Some(KioFileKind::Package) => parser.package_file(None).map(ToolingSyntax::Package),
        Some(KioFileKind::Signature) => parser.signature_file(None).map(ToolingSyntax::Signature),
        Some(KioFileKind::Dependency) => {
            parser.dependency_file(None).map(ToolingSyntax::Dependency)
        }
        Some(KioFileKind::Lock) => parser.lock_file(None).map(ToolingSyntax::Lock),
        None if declarations => parser
            .tooling_declarations()
            .map(|(imports, items)| ToolingSyntax::Declarations { imports, items }),
        None => parser.expr().map(ToolingSyntax::Expression),
    }
    .and_then(|syntax| parser.expect_eof().map(|()| syntax));
    let (syntax, _parse_error) = match parsed {
        Ok(syntax) => (Some(syntax), None),
        Err(error) => (None, Some(error)),
    };
    ToolingProbe {
        source,
        #[cfg(test)]
        kind,
        facts: parser.take_tooling_facts(),
        #[cfg(any(test, feature = "lsp"))]
        lexical_error,
        #[cfg(any(test, feature = "lsp"))]
        parse_error: _parse_error,
        syntax,
    }
}

/// Lex and parse a regular Kio' module file. Builds the
/// tree-skeleton CST first, then drives the recursive-descent
/// parser over it.
pub fn parse(source: &str) -> Result<Module, Error> {
    parse_module(source, false).map(|(module, _)| module)
}

pub(crate) fn parse_module(
    source: &str,
    recover_import_errors: bool,
) -> Result<(Module, Vec<Error>), Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let parsed = if recover_import_errors {
        parser.module_recover_imports()?
    } else {
        (parser.module()?, Vec::new())
    };
    parser.expect_eof()?;
    Ok(parsed)
}

#[cfg(test)]
pub(crate) fn parse_recover_imports(source: &str) -> Result<(Module, Vec<Error>), Error> {
    parse_module(source, true)
}

/// Lex and parse the header/signature surface of a regular module,
/// leaving function and `equiv` bodies as deferred skeleton-backed
/// handles. Calling [`LazyModule::force_all`] materializes the same
/// [`Module`] that [`parse`] returns.
pub fn parse_lazy(source: &str) -> Result<LazyModule, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new_lazy(&skeleton, source.len());
    let module = parser.module_lazy()?;
    parser.expect_eof()?;
    Ok(module)
}

/// Lex and parse a `.kio` module file: a single `module <path>;`
/// module.
pub fn parse_module_file(source: &str) -> Result<ModuleFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let modules = parser.module_file()?;
    parser.expect_eof()?;
    Ok(modules)
}

/// Lex and parse a `.kio` module file while deferring the module
/// body — the per-item bodies are forced lazily on demand.
pub fn parse_module_file_lazy(source: &str) -> Result<ModuleFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new_lazy(&skeleton, source.len());
    let modules = parser.module_file_lazy()?;
    parser.expect_eof()?;
    Ok(modules)
}

/// Lex a regular Kio' module file and build its tree-skeleton
/// CST per [`crate::pass::tree_skeleton`]. Exposed as a distinct entry
/// point so callers can opt in to building the CST for
/// IDE-style outline / folding tools without paying the
/// full-parse cost; the regular [`parse`] entry point also
/// builds and consumes the CST internally.
pub fn parse_tree_skeleton(
    source: &str,
) -> Result<crate::pass::tree_skeleton::SkeletonFile, Error> {
    let tokens = lex(source)?;
    Ok(crate::pass::tree_skeleton::build(tokens))
}

/// Collect the exact local and imported operator grammar for standalone
/// expression parsing in `module`'s scope.
#[cfg(any(test, feature = "lsp"))]
pub(crate) fn expression_parse_context(module: &Module) -> Result<ExpressionParseContext, Error> {
    ExpressionParseContext::from_module(module)
}

#[cfg(any(test, feature = "cli"))]
fn expression_parser<'a>(
    skeleton: &'a crate::pass::tree_skeleton::SkeletonFile,
    source_len: usize,
    context: Option<&ExpressionParseContext>,
) -> Parser<'a> {
    match context {
        Some(context) => Parser::new_with_expression_parse_context(skeleton, source_len, context),
        None => Parser::new(skeleton, source_len),
    }
}

/// Structural square-bracket events committed by one ordinary expression-
/// parse attempt. The parse may fail for live incomplete input; delimiters
/// already confirmed by its selected grammar path remain useful to
/// continuation and highlighting consumers.
#[cfg(any(test, feature = "repl"))]
pub(crate) struct ExpressionFragmentProbe {
    #[cfg(test)]
    pub(crate) structural_bracket_offsets: Vec<u32>,
    #[cfg(any(test, feature = "repl"))]
    pub(crate) unclosed_structural_forall_at_eof: bool,
}

#[cfg(any(test, feature = "repl"))]
pub(crate) fn probe_expression_fragment(
    source: &str,
    context: Option<&ExpressionParseContext>,
) -> Result<ExpressionFragmentProbe, Error> {
    let tokens = lex(source)?;
    let skeleton = crate::pass::tree_skeleton::build(tokens);
    let mut parser = expression_parser(&skeleton, source.len(), context);
    parser.enable_structural_forall_facts();
    let _parse_result = parser.expr().and_then(|expr| {
        parser.expect_eof()?;
        Ok(expr)
    });
    let facts = parser.structural_forall_facts();
    Ok(ExpressionFragmentProbe {
        #[cfg(test)]
        structural_bracket_offsets: facts.bracket_offsets,
        #[cfg(any(test, feature = "repl"))]
        unclosed_structural_forall_at_eof: facts.unclosed_at_eof,
    })
}

/// Parse one standalone type fragment through the ordinary type parser.
#[cfg(any(test, feature = "repl-core"))]
pub(crate) fn parse_type_fragment(source: &str) -> Result<Type, Error> {
    let tokens = lex(source)?;
    let skeleton = crate::pass::tree_skeleton::build(tokens);
    let mut parser = Parser::new(&skeleton, source.len());
    let ty = parser.type_expr()?;
    parser.expect_eof()?;
    Ok(ty)
}

/// Whether parsing `source` as one expression entered a real forall-binder
/// group and reached end-of-input before its closing `]`. The exact optional
/// context is an opaque snapshot of the loaded module's resolved operator
/// grammar, so the probe follows ordinary parser context without guessing an
/// operator shape from its spelling.
#[cfg(any(test, feature = "repl"))]
pub(crate) fn expression_has_unclosed_structural_forall(
    source: &str,
    context: Option<&ExpressionParseContext>,
) -> bool {
    probe_expression_fragment(source, context)
        .is_ok_and(|probe| probe.unclosed_structural_forall_at_eof)
}

/// Lex and parse the **body** of a `build { ... }` block — the
/// `cache …;` declaration, optional `docs { … };` block, and
/// zero or more `target <id> { ... }` blocks — without the
/// surrounding `build { }` braces.
///
/// The body is **not** a language surface — no types, no values, no
/// `module` declaration. The `cache`, `docs`, and `target` tokens are
/// contextual keywords recognized only here. The package-file
/// parser consumes the same body inline (with the braces) via
/// [`Parser::package_file`]; this standalone entry point is for
/// focused parser and formatter tests.
pub fn parse_build_block_body(source: &str) -> Result<BuildBlock, Error> {
    let tokens = lex(source)?;
    let skeleton = crate::pass::tree_skeleton::build(tokens);
    let mut parser = Parser::new(&skeleton, source.len());
    let build = parser.build_block_body()?;
    parser.expect_eof()?;
    Ok(build)
}

/// Lex and parse a `<name>.pkg.kio` package file.
pub fn parse_package_file(source: &str, stem: Option<&str>) -> Result<PackageFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let package_file = parser.package_file(stem)?;
    parser.expect_eof()?;
    Ok(package_file)
}

/// Lex and parse a `<name>.sig.kio` package-signature changelog. The
/// `signature <pkg> v(<N>);` header's `<pkg>` must match `stem` when
/// given (the filename-stem coherence check, like `parse_package_file`).
pub fn parse_signature_file(source: &str, stem: Option<&str>) -> Result<SignatureFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let signature_file = parser.signature_file(stem)?;
    parser.expect_eof()?;
    Ok(signature_file)
}

/// Lex and parse a `<local>.dep.kio` dependency declaration. The
/// `dependency <local>;` header's `<local>` must match `stem` when given
/// (the filename-stem coherence check, like `parse_package_file`).
pub fn parse_dependency_file(source: &str, stem: Option<&str>) -> Result<DependencyFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let dependency_file = parser.dependency_file(stem)?;
    parser.expect_eof()?;
    Ok(dependency_file)
}

/// Lex and parse a `<local>.lock.kio` dependency lock file. The
/// `lock <local>;` header's `<local>` must match `stem` when given (the
/// filename-stem coherence check, like `parse_dependency_file`).
pub fn parse_lock_file(source: &str, stem: Option<&str>) -> Result<LockFile, Error> {
    let skeleton = lex_and_build(source)?;
    let mut parser = Parser::new(&skeleton, source.len());
    let lock_file = parser.lock_file(stem)?;
    parser.expect_eof()?;
    Ok(lock_file)
}

#[cfg(test)]
pub(crate) const IMPORT_GRAMMAR_CONSUMER: &str = "module app/main;
import syntax(op _ + _, op _ => _, varop [% %]);
labels { field: . };
fn run(a: ., b: .) -> . { [% a => (a + b), b => a %] }
rec(loop) { fn nested(a: ., b: .) -> . { a + b } }";

#[cfg(test)]
pub(crate) fn import_grammar_assert_consumer(module: &Module) {
    use crate::ast::{Expr, Item, OpChainKind};
    assert_eq!(module.imports.len(), 1);
    assert_eq!(module.items.len(), 3);
    let Item::FnDef(run) = &module.items[1] else {
        panic!("run body missing")
    };
    let Expr::OpChain {
        kind: OpChainKind::Variadic { elements, .. },
        ..
    } = &run.body
    else {
        panic!("variadic body missing: {:?}", run.body)
    };
    assert_eq!(elements.len(), 2);
    assert!(elements.iter().all(|element| matches!(
        element,
        Expr::OpChain {
            kind: OpChainKind::Normal { .. },
            ..
        }
    )));
    let Expr::OpChain {
        kind: OpChainKind::Normal { pattern, slots },
        ..
    } = &elements[0]
    else {
        panic!("first pair element missing")
    };
    assert_eq!(
        crate::ast::OperatorDispatchKey::from_pattern(pattern).leading_run,
        ["=>"]
    );
    assert_eq!(slots.len(), 2);
    let Expr::OpChain {
        kind: OpChainKind::Normal { pattern, slots },
        ..
    } = &slots[1]
    else {
        panic!("nested fixed operator body missing")
    };
    assert_eq!(
        crate::ast::OperatorDispatchKey::from_pattern(pattern).leading_run,
        ["+"]
    );
    assert_eq!(slots.len(), 2);
    let Item::RecGroup(rec, _) = &module.items[2] else {
        panic!("recursive body missing")
    };
    assert!(matches!(rec.members[0].body, Expr::OpChain { .. }));
}

mod core;
#[cfg(test)]
mod field_order_tests;
#[cfg(test)]
mod semicolon_tests;
#[cfg(test)]
mod varop_tests;
#[cfg(any(test, feature = "lsp"))]
pub(crate) use core::BlockCursorRegion;
#[cfg(feature = "lsp")]
pub(crate) use core::CursorContext;
#[cfg(any(test, feature = "lsp"))]
pub(crate) use core::CursorSlot;
#[cfg(any(test, feature = "cli"))]
pub(crate) use core::ExpressionParseContext;
#[cfg(any(test, feature = "lsp"))]
pub(crate) use core::ImportSelectionKind;
use core::Parser;
#[cfg(any(test, feature = "lsp"))]
pub(crate) use core::ScopeSyntax;
#[cfg(any(test, feature = "cli"))]
pub(crate) use core::{KeywordRole, SourceNameRole};
pub use core::{LazyModule, ModuleFile};
#[cfg(test)]
pub(crate) use core::{PathSeparator, ScopeOwnerKind};
// Canonical public grammar for operator-focused tooling.
#[cfg(feature = "repl-core")]
pub(crate) use core::parse_operator_grammar;
#[cfg(any(feature = "repl", feature = "repl-core"))]
pub(crate) use core::{op_name, variadic_name};

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod compositionality_tests;

#[cfg(test)]
mod block_call_tests;

#[cfg(test)]
mod diagnostic_repair_tests;

#[cfg(test)]
mod operator_suffix_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn tooling_keywords(probe: &ToolingProbe<'_>) -> Vec<(String, crate::span::Span)> {
        probe
            .facts
            .keywords
            .iter()
            .map(|fact| {
                (
                    probe.source[fact.span.start as usize..fact.span.end as usize].to_owned(),
                    fact.span,
                )
            })
            .collect()
    }

    #[test]
    fn tooling_complete_qualified_leaf_has_its_own_atom() {
        for (source, leaf, slot) in [
            (
                "module app; fn run() { provider.item }",
                "item",
                core::CursorSlot::Value,
            ),
            (
                "module app; type Alias = provider/other.Name;",
                "Name",
                core::CursorSlot::Type,
            ),
        ] {
            let start = source.find(leaf).unwrap() as u32;
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(start + 2),
                None,
            );
            let context = probe.facts.cursor.expect(source);
            assert_eq!(context.slot, slot);
            let path = context
                .path
                .expect("qualified leaf owns its written path prefix");
            assert_eq!(path.separator_kind, PathSeparator::Dot);
            assert_eq!(path.prefix[0].name, "provider");
            assert_eq!(
                context.atom.replacement,
                crate::span::Span::new(start, start + leaf.len() as u32)
            );
        }
    }

    #[test]
    fn tooling_complete_module_reference_gap_is_owned() {
        let source = "module app; import provider/";
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        let context = probe.facts.cursor.expect("module path continuation");
        assert_eq!(context.slot, CursorSlot::ImportProvider);
        assert_eq!(context.path.unwrap().separator_kind, PathSeparator::Slash);
        assert!(context.keywords.is_empty());
        assert_eq!(
            context.atom.replacement,
            crate::span::Span::new(source.len() as u32, source.len() as u32)
        );
    }

    #[test]
    fn tooling_complete_header_and_member_names_are_binder_slots() {
        for (source, binder) in [
            ("module app; type Alias[A] = A;", "Alias"),
            (
                "module app; newtype Name : . { constructor make; projector unwrap; }",
                "make",
            ),
            ("module app; host type Name;", "Name"),
            ("module app;", "app"),
        ] {
            let cursor = source.find(binder).unwrap() as u32 + 1;
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(cursor),
                None,
            );
            assert_eq!(
                probe.facts.suppression.expect(source).kind,
                core::CursorSuppressionKind::Binder
            );
            assert!(probe.facts.cursor.is_none());
        }
    }

    #[test]
    fn tooling_complete_incomplete_type_alias_retains_its_type_parameters() {
        let source = "module app; type Alias[A] = ";
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(
            probe
                .facts
                .scope_prefix
                .iter()
                .any(|prefix| prefix.owner_kind == ScopeOwnerKind::Declaration)
        );
    }

    #[test]
    fn tooling_complete_incomplete_return_type_retains_signature_type_parameters() {
        let source = "module app; fn run[T](value: T) -> ";
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(
            probe
                .facts
                .scope_prefix
                .iter()
                .any(|prefix| prefix.owner_kind == core::ScopeOwnerKind::Signature)
        );
    }

    #[test]
    fn tooling_complete_nested_forall_is_not_lost_to_outer_body_prefix() {
        let source = "module app; fn run() { .(f: [A] A -> A) { f } }";
        let cursor = source.find("A ->").unwrap() as u32 + 1;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(cursor),
            None,
        );
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        assert!(
            probe
                .facts
                .scope_prefix
                .iter()
                .any(|prefix| prefix.owner_kind == core::ScopeOwnerKind::Type)
        );
    }

    #[test]
    fn tooling_complete_equiv_keeps_its_written_signature_on_arm_failure() {
        let source = "module app; equiv test[A](value: A) { value; f([B] ";
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(probe.facts.recovered_group);
        assert!(!probe.facts.regions.is_empty());
        assert!(
            probe
                .facts
                .scope_prefix
                .iter()
                .any(|prefix| matches!(prefix.syntax, core::ScopeSyntax::Signature(_)))
        );
    }

    #[test]
    fn tooling_complete_rec_names_survive_a_member_body_error() {
        let source = "module app; rec(loop) { fn first(value: .) { rec second(value); ? } fn second(x: .) { x } }";
        let cursor = source.find("rec second").unwrap() as u32 + 6;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(cursor),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(
            probe
                .facts
                .scope_prefix
                .iter()
                .any(|prefix| prefix.owner_kind == core::ScopeOwnerKind::RecursiveGroup)
        );
    }

    #[test]
    fn tooling_complete_signature_module_roles_survive_wrong_file_tail() {
        let source = "signature app v(1); v(1) { breaking { add { module api { pub fn run() -> .; } } } } dependency tail;";
        let probe = probe_tooling(source, Some(crate::ast::KioFileKind::Signature), None, None);
        assert!(probe.parse_error.is_some());
        let start = source.find("api").unwrap() as u32;
        assert!(
            probe
                .facts
                .source_names
                .iter()
                .any(|fact| fact.span == crate::span::Span::new(start, start + 3)
                    && fact.role == core::SourceNameRole::Module)
        );
    }

    #[test]
    fn tooling_retained_prefix_signature_export_name_survives_later_errors() {
        use crate::span::Span;
        use crate::tokens::TokenKind;

        let complete = "signature app v(1); v(1) { with { module api { pub fn run() -> .; } }; nonbreaking { add { api.run; } } }";
        for source in [
            format!("{complete} dependency lib;"),
            "signature app v(1); v(1) { with { module api { pub fn run(".to_owned(),
        ] {
            let probe = probe_tooling_source(&source, None);
            assert!(probe.parse_error.is_some());
            assert!(probe.syntax.is_none());
            let start = source.find("run").unwrap() as u32;
            let span = Span::new(start, start + 3);
            assert!(
                probe
                    .facts
                    .source_names
                    .iter()
                    .any(|fact| fact.span == span && fact.role == core::SourceNameRole::Function),
                "validated signature export binder lost: {source}"
            );
            assert!(
                crate::tokens::dump(&source)
                    .unwrap()
                    .iter()
                    .any(|token| token.span == span && token.kind == TokenKind::EntityNameFunction)
            );
        }

        assert!(probe_tooling_source(complete, None).parse_error.is_none());
        let invalid = "signature app v(1); v(1) { with { module api { pub fn Run(";
        let probe = probe_tooling_source(invalid, None);
        assert!(probe.parse_error.is_some());
        let start = invalid.find("Run").unwrap() as u32;
        assert!(
            !probe
                .facts
                .source_names
                .iter()
                .any(|fact| fact.span == Span::new(start, start + 3)
                    && fact.role == core::SourceNameRole::Function)
        );
    }

    #[test]
    fn tooling_retained_prefix_completed_items_survive_later_errors_without_cursor() {
        use crate::ast::Item;
        use crate::span::Span;
        use crate::tokens::TokenKind;

        let prefix = "module app; op _ + _ { impl add; }; elab choose: . { impl select; };";
        assert!(probe_tooling_source(prefix, None).parse_error.is_none());
        let source = format!("{prefix} fn broken() {{ ? }} fn later() {{ () }}");
        let probe = probe_tooling_source(&source, None);
        assert!(probe.parse_error.is_some());
        assert!(probe.syntax.is_none());
        assert!(matches!(probe.facts.prefix_items.as_slice(),
            [Item::Op(..), Item::Elaborator(..), Item::FnDef(function)] if function.name == "later"));
        let tokens = crate::tokens::dump(&source).unwrap();
        for (name, kind) in [
            ("add", TokenKind::EntityNameFunctionReference),
            ("choose", TokenKind::EntityNameFunction),
            ("select", TokenKind::EntityNameFunctionReference),
            ("later", TokenKind::EntityNameFunction),
        ] {
            let start = source.find(name).unwrap() as u32;
            let span = Span::new(start, start + name.len() as u32);
            assert!(
                tokens
                    .iter()
                    .any(|token| token.span == span && token.kind == kind),
                "completed declaration role lost for {name}: {tokens:?}"
            );
        }

        let cursor = source.find("elab").unwrap() as u32 - 1;
        let before_elab = probe_tooling_source(&source, Some(cursor));
        assert!(before_elab.parse_error.is_some());
        assert!(matches!(
            before_elab.facts.prefix_items.as_slice(),
            [Item::Op(..)]
        ));
    }

    #[test]
    fn named_entry_missing_required_fields_report_their_owning_close() {
        let mut failures = Vec::new();
        for declaration in [
            "elab make: . -> . {}",
            "elab make: . -> . { captures saved; }",
            "varop [* *] {}",
            "varop [* *] { finalize finish; }",
        ] {
            let source = format!("module app; {declaration}; fn following() {{ () }}");
            let close = source.find('}').unwrap() as u32;
            let error = parse(&source).expect_err("required field is missing");
            let expected = crate::span::Span::new(close, close + 1);
            if error.diagnostic().span != expected {
                failures.push(format!(
                    "{source}: {:?}, expected {expected:?}",
                    error.diagnostic()
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn tooling_ordinary_argument_heads_retain_their_written_call_owner() {
        let cases = [
            ("identity(I|", "identity", false),
            ("choose(I|)", "choose", false),
            ("identity(|", "identity", false),
            ("identity(, |", "identity", false),
            ("identity((), I|", "identity", true),
            ("identity((), |", "identity", true),
            ("factory()(I|", "factory()", false),
            ("outer(inner(I|", "inner", false),
            ("outer(inner(I|)).< value", "inner", false),
        ];
        let mut failures = Vec::new();
        for (marked, expected, has_prior_argument) in cases {
            let cursor = marked.find('|').unwrap();
            let source = marked.replace('|', "");
            let probe = probe_tooling(&source, None, Some(cursor as u32), None);
            let call = probe
                .facts
                .cursor
                .as_ref()
                .and_then(|context| context.call.as_ref());
            if let Some(call) = call {
                let span = call.callee.span();
                let actual = &source[span.start as usize..span.end as usize];
                if actual != expected || call.has_prior_argument != has_prior_argument {
                    failures.push(format!(
                        "{marked}: {call:?}; expected {expected}, prior={has_prior_argument}"
                    ));
                }
            } else {
                failures.push(format!(
                    "{marked}: no ordinary call owner: {:?}",
                    probe.facts.cursor
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn tooling_other_argument_and_nested_type_owners_do_not_inherit_direct_calls() {
        let mut failures = Vec::new();
        for marked in [
            "value.>identity(I|",
            "choose!(I|",
            "rec(poly) again(I|",
            "identity(I|).< value",
            "outer(.(x: I|) { x })",
            "outer((I|))",
        ] {
            let cursor = marked.find('|').unwrap();
            let source = marked.replace('|', "");
            let probe = probe_tooling(&source, None, Some(cursor as u32), None);
            let context = probe.facts.cursor.expect(marked);
            if context.call.is_some() {
                failures.push(format!("{marked}: {context:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn tooling_named_entries_follow_occupancy_in_every_written_order() {
        use crate::ast::KioFileKind::{Dependency, Module, Package, Signature};
        let cases = [
            (Package, "package app; bridge {} ", vec!["build"]),
            (Package, "package app; bridge {} build {} ", vec![]),
            (
                Dependency,
                "dependency lib; ",
                vec!["source", "rehost", "retype"],
            ),
            (
                Dependency,
                "dependency lib; retype lib/api to app/api; ",
                vec!["source", "rehost", "retype"],
            ),
            (
                Dependency,
                "dependency lib; rehost lib/api to app/api; source { path \"lib.pkg.kio\"; } ",
                vec!["rehost", "retype"],
            ),
            (
                Module,
                "module app; elab make: . { impl run; ",
                vec!["captures", "trailing"],
            ),
            (
                Module,
                "module app; elab make: . { impl run; captures name; ",
                vec!["trailing"],
            ),
            (
                Module,
                "module app; varop [* *] { finalize finish; ",
                vec!["foldl", "foldr", "foldl1", "foldr1"],
            ),
            (
                Module,
                "module app; varop [* *] { finalize finish; foldr step base; ",
                vec![],
            ),
            (
                Signature,
                "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } } ",
                vec!["with", "breaking"],
            ),
            (
                Signature,
                "signature app v(1); v(1) { breaking { add { module api { host type A; } } } ",
                vec!["with", "nonbreaking"],
            ),
            (
                Signature,
                "signature app v(1); v(1) { breaking { remove { module api { A; } } ",
                vec!["add", "modify"],
            ),
        ];
        let mut failures = Vec::new();
        for (kind, source, expected) in cases {
            let probe = probe_tooling(source, Some(kind), Some(source.len() as u32), None);
            if let Some(context) = probe.facts.cursor {
                if context.slot != core::CursorSlot::Grammar || context.keywords != expected {
                    failures.push(format!("{source}: {context:?}; expected {expected:?}"));
                }
            } else {
                failures.push(format!("{source}: missing cursor context"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));

        let source =
            "package app; build { target | {} cache (); docs { md \"docs\"; } target rust {} }";
        let cursor = source.find('|').unwrap();
        let source = source.replace('|', "");
        let probe = probe_tooling(&source, Some(Package), Some(cursor as u32), None);
        assert!(probe.parse_error.is_some());
        let context = probe
            .facts
            .cursor
            .expect("edited target survives sibling recovery");
        assert_eq!(context.slot, core::CursorSlot::TargetId);
        assert_eq!(context.target_ids, ["rust"]);
    }

    #[test]
    fn tooling_complete_remaining_fields_use_their_actual_productions() {
        for (source, expected) in [
            ("module app; host type Text ", vec!["role"]),
            ("module app; host type Text { ", vec!["owned"]),
            ("module app; op _ + _ { ", vec!["impl"]),
            (
                "module app; elab make: . { ",
                vec!["captures", "impl", "trailing"],
            ),
            (
                "module app; elab make: . { captures name; ",
                vec!["impl", "trailing"],
            ),
            (
                "module app; varop [* *] { ",
                vec!["foldl", "foldr", "foldl1", "foldr1", "finalize"],
            ),
            (
                "module app; varop [* *] { foldl step init; ",
                vec!["finalize"],
            ),
            ("module app; fn run() { rec(poly, ", vec!["cont"]),
        ] {
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(source.len() as u32),
                None,
            );
            assert_eq!(
                probe.facts.cursor.expect(source).keywords,
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_closed_row_let_prefix_is_owned_only_by_a_block_statement() {
        let source = "module app; labels { field: . }; fn run(value: Field) { let .({field as local}) = value; local }";
        let cursor = source.find("field as").unwrap() as u32 + 6;
        assert!(parse(source).is_ok());
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(cursor),
            None,
        );
        assert!(probe.parse_error.is_none());
        assert_eq!(probe.facts.cursor.unwrap().keywords, ["as"]);

        let expression = "module app; fn run() { call(let .({field ";
        let probe = probe_tooling(
            expression,
            Some(crate::ast::KioFileKind::Module),
            Some(expression.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(
            probe
                .facts
                .cursor
                .is_none_or(|context| !context.keywords.contains(&"as"))
        );

        for (kind, source, expected) in [
            (
                crate::ast::KioFileKind::Module,
                "module app; host type A role(str) { owned }; ",
                "fn",
            ),
            (
                crate::ast::KioFileKind::Signature,
                "signature app v(1); v(1) { breaking { remove { module api { run; } } } ",
                "nonbreaking",
            ),
            (
                crate::ast::KioFileKind::Signature,
                "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } } } ",
                "v",
            ),
        ] {
            let probe = probe_tooling(source, Some(kind), Some(source.len() as u32), None);
            assert!(
                probe
                    .facts
                    .cursor
                    .expect(source)
                    .keywords
                    .contains(&expected),
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_closed_owners_keep_their_required_choices_or_explicit_empty_set() {
        use crate::ast::KioFileKind::{Module, Signature};

        let mut cases = vec![
            (Module, "module app; rec ".to_owned(), vec!["labels", "newtype"]),
            (Module, "module app; pub rec ".to_owned(), vec!["labels", "newtype"]),
            (Module, "module app; rec { ".to_owned(), vec!["labels", "newtype", "pub", "type"]),
            (Module, "module app; rec { pub ".to_owned(), vec!["labels", "newtype", "type"]),
            (Module, "module app; rec(loop) { pub ".to_owned(), vec!["fn"]),
            (Module, "module app; host type A role(str) { owned ".to_owned(), vec![]),
            (
                Module,
                "module app; elab make: . { impl run; ".to_owned(),
                vec!["captures", "trailing"],
            ),
            (Module, "module app; labels { field: . }; fn run(value: Field) { let .({field ".to_owned(), vec!["as"]),
            (Signature, "signature app v(1); v(1) { nonbreaking { add { module api { pub type A = .; } } } ".to_owned(), vec!["breaking", "with"]),
            (Signature, "signature app v(1); v(1) { breaking { remove { module api { run; } } ".to_owned(), vec!["add", "modify"]),
        ];
        for mode in ["foldl", "foldl1", "foldr", "foldr1"] {
            cases.push((
                Module,
                format!("module app; varop [* *] {{ {mode} step base; finalize finish; "),
                vec![],
            ));
        }
        for operation in ["add", "modify", "remove"] {
            cases.push((
                Signature,
                format!("signature app v(1); v(1) {{ breaking {{ {operation} {{ "),
                vec!["module"],
            ));
        }
        let mut failures = Vec::new();
        for (kind, source, expected) in cases {
            let probe = probe_tooling(&source, Some(kind), Some(source.len() as u32), None);
            let Some(context) = probe.facts.cursor else {
                failures.push(format!("{source:?}: unavailable, expected {expected:?}"));
                continue;
            };
            let mut actual = context.keywords;
            actual.sort_unstable();
            if actual != expected {
                failures.push(format!("{source:?}: {actual:?}, expected {expected:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn tooling_block_label_cursor_uses_the_marked_head_without_fixed_labels() {
        let source = "module app; fn run() { choose! { () } ";
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        let cursor = probe.facts.cursor.expect("trailing block label context");
        assert_eq!(cursor.slot, core::CursorSlot::BlockLabel);
        assert!(cursor.keywords.is_empty());
        let block = probe.facts.block_cursor.expect("marked block call owner");
        assert_eq!(block.head.name, "choose");
        assert!(matches!(block.region, core::BlockCursorRegion::Label(1)));
        assert_eq!(block.labels, [None]);
    }

    #[test]
    fn tooling_context_edges_target_id_keeps_its_slot_after_an_earlier_target() {
        let source = "package app; build { target rust {} target ";
        let cursor = source.len() as u32;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Package),
            Some(cursor),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(probe.facts.cursor.is_some(), "{:?}", probe.facts);
        let context = probe.facts.cursor.unwrap();
        assert_eq!(context.slot, core::CursorSlot::TargetId);
        assert_eq!(
            context.atom.replacement,
            crate::span::Span::new(cursor, cursor)
        );
        assert_eq!(context.target_ids, ["rust"]);
    }

    #[test]
    fn tooling_context_edges_target_id_partial_kebab_excludes_only_the_edited_header() {
        for (marked, expected) in [
            ("package app; build { target ru|st- {} }", vec![]),
            (
                "package app; build { target ru|st- {} target rust {} }",
                vec!["rust"],
            ),
        ] {
            let cursor = marked.find('|').unwrap() as u32;
            let source = marked.replacen('|', "", 1);
            let probe = probe_tooling(
                &source,
                Some(crate::ast::KioFileKind::Package),
                Some(cursor),
                None,
            );
            assert!(probe.parse_error.is_some());
            let context = probe.facts.cursor.expect(marked);
            assert_eq!(context.slot, core::CursorSlot::TargetId);
            assert_eq!(
                context.atom.replacement,
                crate::span::Span::new(cursor - 2, cursor + 3)
            );
            assert_eq!(context.target_ids, expected, "{marked}");
        }
    }

    #[test]
    fn tooling_context_edges_target_ids_exclude_the_edited_header_not_other_siblings() {
        use crate::span::Span;
        for (marked, expected) in [
            ("package app; build { target ru|st {} }", vec![]),
            (
                "package app; build { target ru|st {} target js {} }",
                vec!["js"],
            ),
            (
                "package app; build { target | {} target rust {} }",
                vec!["rust"],
            ),
            (
                "package app; build { target | {} target rust { broken; } target js {} }",
                vec!["rust", "js"],
            ),
            (
                "package app; build { target | {} target js { nested { target rust {} } } }",
                vec!["js"],
            ),
        ] {
            let cursor = marked.find('|').unwrap() as u32;
            let source = marked.replacen('|', "", 1);
            let probe = probe_tooling(
                &source,
                Some(crate::ast::KioFileKind::Package),
                Some(cursor),
                None,
            );
            let context = probe.facts.cursor.expect(marked);
            assert_eq!(context.slot, core::CursorSlot::TargetId, "{marked}");
            assert_eq!(context.target_ids, expected, "{marked}");
            if marked.contains("ru|st") {
                assert_eq!(context.atom.replacement, Span::new(cursor - 2, cursor + 2));
                assert_eq!(context.atom.prefix, Span::new(cursor - 2, cursor));
            } else {
                assert_eq!(context.atom.replacement, Span::new(cursor, cursor));
            }
            let ordinary =
                probe_tooling(&source, Some(crate::ast::KioFileKind::Package), None, None);
            assert_eq!(
                probe.parse_error, ordinary.parse_error,
                "recovery must retain the original error: {marked}"
            );
        }
    }

    #[test]
    fn tooling_context_edges_underscore_owns_the_atom_and_argument_namespace() {
        use crate::span::Span;

        for (source, slot, start) in [
            (".(x: _", core::CursorSlot::Type, 5),
            ("identity(_", core::CursorSlot::Argument, 9),
        ] {
            let cursor = source.len() as u32;
            let probe = probe_tooling(source, None, Some(cursor), None);
            let ordinary = probe_tooling(source, None, None, None);
            assert_eq!(probe.parse_error, ordinary.parse_error);
            let context = probe.facts.cursor.expect(source);
            assert_eq!(
                context.atom.replacement,
                Span::new(start, cursor),
                "{source}"
            );
            assert_eq!(context.atom.prefix, Span::new(start, cursor), "{source}");
            assert_eq!(context.slot, slot, "{source}");
        }

        let complete = probe_tooling("identity(_)", None, None, None);
        let Some(ToolingSyntax::Expression(crate::ast::Expr::Call { args, .. })) = complete.syntax
        else {
            panic!("ordinary type-inference argument must still parse");
        };
        assert!(matches!(
            args.as_slice(),
            [crate::ast::CallArg::Type(crate::ast::Type::Infer { .. })]
        ));
    }

    #[test]
    fn tooling_context_edges_headerless_import_keeps_proved_sibling_roles() {
        use crate::span::Span;
        use crate::tokens::TokenKind;

        for prefix in ["", "module app;\n"] {
            let source = format!("{prefix}import app\nfn neighbor() -> . {{ () }}");
            let probe = probe_tooling_source(&source, None);
            assert!(probe.parse_error.is_some());
            assert!(probe.syntax.is_none());
            let start = source.find("fn neighbor").unwrap() as u32;
            assert_eq!(
                probe.parse_error.as_ref().unwrap().diag(),
                (
                    Span::new(start, start + 2),
                    "expected `(` after the import provider"
                )
            );
            assert!(
                probe
                    .facts
                    .keywords
                    .iter()
                    .any(|fact| fact.span == Span::new(start, start + 2)
                        && fact.role == core::KeywordRole::Declaration),
                "{source}"
            );
            let tokens = crate::tokens::dump(&source).unwrap();
            assert!(
                tokens
                    .iter()
                    .any(|token| token.span == Span::new(start + 3, start + 11)
                        && token.kind == TokenKind::EntityNameFunction),
                "{source}"
            );
            let ordinary_source =
                format!("{prefix}import app as imported;\nfn neighbor() -> . {{ () }}");
            let ordinary = probe_tooling_source(&ordinary_source, None);
            assert!(ordinary.parse_error.is_none());
        }

        let inverse = "import(app, neighbor())";
        let probe = probe_tooling_source(inverse, None);
        assert!(probe.parse_error.is_none());
        assert!(probe.facts.keywords.is_empty());
    }

    #[test]
    fn tooling_required_named_continuations_use_actual_grammar() {
        use crate::ast::KioFileKind;

        for (kind, source, expected) in [
            (
                KioFileKind::Module,
                "module app; host ",
                vec!["fn", "pub", "type"],
            ),
            (
                KioFileKind::Module,
                "module app; pub host ",
                vec!["fn", "type"],
            ),
            (
                KioFileKind::Module,
                "module app; host pub ",
                vec!["fn", "type"],
            ),
            (KioFileKind::Module, "module app; pure host ", vec![]),
            (
                KioFileKind::Module,
                "module app; elab make: . { impl(",
                vec!["fills"],
            ),
            (
                KioFileKind::Module,
                "module app; elab make: . { impl(fi",
                vec!["fills"],
            ),
            (
                KioFileKind::Dependency,
                "dependency app; source { path \"app.pkg.kio\"; } rehost dep/api ",
                vec!["to"],
            ),
            (
                KioFileKind::Dependency,
                "dependency app; source { path \"app.pkg.kio\"; } retype dep/api.Type ",
                vec!["to"],
            ),
        ] {
            let probe = probe_tooling(source, Some(kind), Some(source.len() as u32), None);
            assert!(probe.parse_error.is_some(), "{source}");
            let mut keywords = probe
                .facts
                .cursor
                .as_ref()
                .map_or_else(Vec::new, |cursor| cursor.keywords.clone());
            keywords.sort_unstable();
            assert_eq!(keywords, expected, "{source}");
            if !expected.is_empty() {
                assert_eq!(
                    probe.facts.cursor.as_ref().unwrap().slot,
                    core::CursorSlot::Grammar
                );
            }
        }

        for source in [
            "module app; host fn run() -> .;",
            "module app; host type Text;",
            "module app; host pub type Text;",
            "module app; pub host fn run() -> .;",
            "module app; elab make: . { impl(fills) make; };",
            "dependency app; source { path \"app.pkg.kio\"; } rehost dep/api to app/api;",
            "dependency app; source { path \"app.pkg.kio\"; } retype dep/api.Type to app/api.Type;",
        ] {
            assert!(
                probe_tooling_source(source, None).parse_error.is_none(),
                "{source}"
            );
        }

        let incomplete_host = probe_tooling_source("module app; host ", None);
        assert!(
            !tooling_keywords(&incomplete_host)
                .iter()
                .any(|(keyword, _)| keyword == "host")
        );
        let inverse = probe_tooling_source("module app; fn run() { host() }", None);
        assert!(inverse.parse_error.is_none());
        assert!(
            !tooling_keywords(&inverse)
                .iter()
                .any(|(keyword, _)| keyword == "host")
        );
    }

    #[test]
    fn tooling_required_host_pub_keeps_the_current_atom_before_the_next_slot() {
        use crate::span::Span;

        let source = "module app; host pub";
        let start = source.rfind("pub").unwrap() as u32;
        let end = source.len() as u32;
        let observations: Vec<_> = [end - 1, end]
            .into_iter()
            .map(|cursor| {
                let probe = probe_tooling_source(source, Some(cursor));
                probe.facts.cursor.map(|context| {
                    (
                        context.atom.replacement,
                        context.atom.prefix,
                        context.keywords.contains(&"pub"),
                    )
                })
            })
            .collect();
        assert_eq!(
            observations,
            vec![
                Some((Span::new(start, end), Span::new(start, end - 1), true)),
                Some((Span::new(start, end), Span::new(start, end), true)),
            ]
        );

        let after = format!("{source} ");
        let cursor = after.len() as u32;
        let probe = probe_tooling_source(&after, Some(cursor));
        let context = probe.facts.cursor.unwrap();
        assert_eq!(context.atom.replacement, Span::new(cursor, cursor));
        assert_eq!(context.keywords, vec!["fn", "type"]);
    }

    #[test]
    fn tooling_complete_host_and_label_headers_retain_real_type_parameters() {
        for (source, expected) in [
            ("module app; host fn run[A](value: ", vec!["A"]),
            ("module app; host fn run[A](value: A) -> ", vec!["A"]),
            (
                "module app; labels Choice[A] = { item[B] <C>: ",
                vec!["A", "B", "C"],
            ),
        ] {
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(source.len() as u32),
                None,
            );
            let names: Vec<_> = probe
                .facts
                .scope_prefix
                .iter()
                .flat_map(|prefix| match &prefix.syntax {
                    core::ScopeSyntax::TypeParameters(params) => {
                        params.iter().map(|param| param.name.as_str()).collect()
                    }
                    _ => Vec::new(),
                })
                .collect();
            assert_eq!(names, expected, "{source}");
        }
    }

    #[test]
    fn tooling_complete_recursive_shorthand_retains_its_written_member() {
        let source = "module app; rec(loop) fn run(value: .) { rec run(value) }";
        let cursor = source.find("rec run").unwrap() as u32 + 6;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(cursor),
            None,
        );
        let names: Vec<_> = probe
            .facts
            .scope_prefix
            .iter()
            .flat_map(|prefix| match &prefix.syntax {
                core::ScopeSyntax::RecursiveMembers { names, .. } => {
                    names.iter().map(|name| name.name.as_str()).collect()
                }
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(names, vec!["run"]);
    }

    #[test]
    fn tooling_complete_all_file_binders_suppress_candidates() {
        use crate::ast::KioFileKind;
        for (source, kind, binder) in [
            ("package app;", KioFileKind::Package, "app"),
            ("signature app v(1);", KioFileKind::Signature, "app"),
            ("dependency app;", KioFileKind::Dependency, "app"),
            ("lock app;", KioFileKind::Lock, "app"),
            (
                "module app; literal value = 1;",
                KioFileKind::Module,
                "value",
            ),
            (
                "module app; elab name: . { impl identity; };",
                KioFileKind::Module,
                "name",
            ),
            (
                "module app; equiv name { (); () }",
                KioFileKind::Module,
                "name",
            ),
            (
                "module app; host fn name(value: .) -> .;",
                KioFileKind::Module,
                "name",
            ),
            (
                "module app; labels Name = {label: .};",
                KioFileKind::Module,
                "label:",
            ),
        ] {
            let cursor = source.find(binder).unwrap() as u32 + 1;
            let probe = probe_tooling(source, Some(kind), Some(cursor), None);
            assert_eq!(
                probe.facts.suppression.expect(source).kind,
                core::CursorSuppressionKind::Binder
            );
        }
    }

    #[test]
    fn tooling_complete_recovery_keeps_the_first_member_diagnostic() {
        let source = "module app; rec(loop) { fn first() { ? }; fn broken( }";
        let ordinary = parse(source).unwrap_err();
        let probe = probe_tooling(source, Some(crate::ast::KioFileKind::Module), None, None);
        assert_eq!(
            format!("{:?}", probe.parse_error.unwrap()),
            format!("{ordinary:?}")
        );
    }

    #[test]
    fn tooling_placeholder_missing_outer_brace_retains_neighbor() {
        for body in [
            "()",
            ".x. { x1 }",
            ".(value) { value }",
            ".x. { x1",
            ".(value) { value",
        ] {
            let source = format!("module probe; fn run() {{ {body} fn neighbor() {{ () }}");
            let probe = probe_tooling(&source, Some(crate::ast::KioFileKind::Module), None, None);
            assert_eq!(
                probe.parse_error.unwrap().diag().0.start as usize,
                source.find("fn neighbor").unwrap(),
                "{source}"
            );
            let names: Vec<_> = probe
                .facts
                .prefix_items
                .iter()
                .filter_map(|item| match item {
                    Item::FnDef(def) => Some(def.name.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                names.contains(&"neighbor"),
                "{source}: retained functions: {names:?}"
            );
        }
        let source =
            "module probe; fn run() { .x. { x1 } fn nested() { () } } fn neighbor() { () }";
        let probe = probe_tooling(source, Some(crate::ast::KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_some());
        assert_eq!(probe.facts.prefix_items.len(), 1);
        assert!(matches!(&probe.facts.prefix_items[0], Item::FnDef(def) if def.name == "neighbor"));
        let source = "module probe; fn run() { .x. { x1 } fn broken() { ? } fn neighbor() { () }";
        let probe = probe_tooling(source, Some(crate::ast::KioFileKind::Module), None, None);
        assert_eq!(
            probe.parse_error.unwrap().diag().0.start as usize,
            source.find("fn broken").unwrap()
        );
        assert!(
            probe
                .facts
                .prefix_items
                .iter()
                .any(|item| matches!(item, Item::FnDef(def) if def.name == "neighbor"))
        );
    }

    #[test]
    fn tooling_complete_refactored_roots_preserve_ordinary_ast_and_errors() {
        for source in [
            "module app; fn run[A](value: A) -> A { value }",
            "module app; rec(loop) fn run(value: .) { rec run(value) }",
            "module app; rec(loop) { fn first(value: .) { rec second(value) } fn second(value: .) { value } }",
            "module app; equiv same[A](value: A) { value; value }",
            "module app; equiv same[A](value: A) { value; f([B] ) }",
            "module app; host fn run[A](value: A) -> A;",
            "module app; labels Choice[A] = { item[B] <C>: A & B & C };",
            "module app; rec { type Alias = ; newtype Later : . { constructor make; projector unwrap; }; }",
        ] {
            let cursor = source
                .find("= ;")
                .map_or(source.len() as u32, |index| index as u32 + 2);
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(cursor),
                None,
            );
            match (parse(source), probe.syntax, probe.parse_error) {
                (Ok(ordinary), Some(ToolingSyntax::Module(tooling)), None) => {
                    assert_eq!(ordinary, tooling, "{source}")
                }
                (Err(ordinary), None, Some(tooling)) => {
                    assert_eq!(format!("{ordinary:?}"), format!("{tooling:?}"), "{source}")
                }
                other => panic!("different source acceptance: {source}: {}", other.0.is_ok()),
            }
            if source.contains("type Alias = ;") {
                eprintln!(
                    "recursive type prefix witness: cursor={cursor}, slot={:?}, items={}, carriers={:?}",
                    probe.facts.cursor.as_ref().map(|context| context.slot),
                    probe.facts.prefix_items.len(),
                    probe.facts.scope_prefix
                );
            }
        }
    }

    #[test]
    fn tooling_context_revision_file_headers_are_kind_owned() {
        use crate::ast::KioFileKind;
        for (kind, keyword) in [
            (KioFileKind::Module, "module"),
            (KioFileKind::Package, "package"),
            (KioFileKind::Signature, "signature"),
            (KioFileKind::Dependency, "dependency"),
            (KioFileKind::Lock, "lock"),
        ] {
            let probe = probe_tooling("", Some(kind), Some(0), None);
            assert_eq!(probe.facts.cursor.expect(keyword).keywords, vec![keyword]);
        }
    }

    #[test]
    fn tooling_context_revision_field_choices_follow_validator_occupancy() {
        use crate::ast::KioFileKind;
        for (source, kind, expected) in [
            (
                "package app; build { docs { ",
                KioFileKind::Package,
                vec!["md", "support", "md_out", "html"],
            ),
            (
                "package app; build { docs { md \"docs\"; ",
                KioFileKind::Package,
                vec!["support", "md_out", "html"],
            ),
            (
                "dependency app; source { path \"app.pkg.kio\"; ",
                KioFileKind::Dependency,
                vec!["git", "ref"],
            ),
            (
                "dependency app; source { git \"url\"; ",
                KioFileKind::Dependency,
                vec!["path", "ref"],
            ),
            (
                "dependency app; source { ref \"main\"; ",
                KioFileKind::Dependency,
                vec!["path", "git"],
            ),
            (
                "dependency app; source { path \"app.pkg.kio\"; git \"url\"; ",
                KioFileKind::Dependency,
                vec!["ref"],
            ),
            (
                "dependency app; source { path \"app.pkg.kio\"; ref \"main\"; ",
                KioFileKind::Dependency,
                vec!["git"],
            ),
            (
                "dependency app; source { git \"url\"; ref \"main\"; ",
                KioFileKind::Dependency,
                vec!["path"],
            ),
            (
                "dependency app; source { git \"url\"; ref \"main\"; path \"app.pkg.kio\"; ",
                KioFileKind::Dependency,
                vec![],
            ),
            (
                "lock app; resolved { git \"url\"; ",
                KioFileKind::Lock,
                vec!["ref", "path", "commit", "sig"],
            ),
            (
                "lock app; resolved { path \"app.pkg.kio\"; ",
                KioFileKind::Lock,
                vec!["git", "ref", "commit", "sig"],
            ),
        ] {
            let probe = probe_tooling(source, Some(kind), Some(source.len() as u32), None);
            assert_eq!(
                probe.facts.cursor.expect(source).keywords,
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_context_revision_field_keyword_survives_unfinished_value() {
        use crate::ast::KioFileKind;
        for (source, kind, keyword) in [
            (
                "package app; build { docs { md ",
                KioFileKind::Package,
                "md",
            ),
            (
                "dependency app; source { path ",
                KioFileKind::Dependency,
                "path",
            ),
            ("lock app; resolved { ref ", KioFileKind::Lock, "ref"),
        ] {
            let probe = probe_tooling(source, Some(kind), None, None);
            assert!(probe.parse_error.is_some());
            assert!(
                tooling_keywords(&probe)
                    .iter()
                    .any(|(word, span)| word == keyword
                        && span.start == source.rfind(keyword).unwrap() as u32),
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_context_revision_control_roles_follow_their_production() {
        use crate::ast::KioFileKind;
        let source = "module app; rec(loop) { fn run(x: .) { let local = x; rec run(local) } }";
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none());
        let recs: Vec<_> = probe
            .facts
            .keywords
            .iter()
            .filter(|fact| &source[fact.span.start as usize..fact.span.end as usize] == "rec")
            .collect();
        assert_eq!(recs[0].role, core::KeywordRole::Declaration);
        assert_eq!(recs[1].role, core::KeywordRole::Declaration);
        let local = probe
            .facts
            .keywords
            .iter()
            .find(|fact| &source[fact.span.start as usize..fact.span.end as usize] == "let")
            .unwrap();
        assert_eq!(local.role, core::KeywordRole::Declaration);
    }

    #[test]
    fn tooling_context_revision_signature_occupancy_is_owned_by_sections() {
        use crate::ast::KioFileKind;
        for (source, expected) in [
            (
                "signature app v(1); v(1) { ",
                vec!["with", "breaking", "nonbreaking"],
            ),
            (
                "signature app v(1); v(1) { with { module api { host type A; } } ",
                vec!["breaking", "nonbreaking"],
            ),
            (
                "signature app v(1); v(1) { breaking { ",
                vec!["add", "modify", "remove"],
            ),
            (
                "signature app v(1); v(1) { breaking { add { module api { pub fn run() -> .; } } ",
                vec!["modify", "remove"],
            ),
        ] {
            let probe = probe_tooling(
                source,
                Some(KioFileKind::Signature),
                Some(source.len() as u32),
                None,
            );
            assert_eq!(
                probe.facts.cursor.expect(source).keywords,
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_context_revision_module_item_and_modifier_choices() {
        use crate::ast::KioFileKind;
        let source = "module app; ";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        let keywords = probe.facts.cursor.expect("module body").keywords;
        assert!(
            keywords.contains(&"import") && keywords.contains(&"fn") && keywords.contains(&"type")
        );
        let source = "module app; pure ";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert_eq!(
            probe.facts.cursor.expect("after pure").keywords,
            vec!["pub", "fn"]
        );
        let source = "module app; pub pure fn run() { () } host type A;";
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        let words: Vec<_> = tooling_keywords(&probe)
            .into_iter()
            .map(|(word, _)| word)
            .collect();
        assert!(
            words.contains(&"pub".to_owned())
                && words.contains(&"pure".to_owned())
                && words.contains(&"host".to_owned())
        );
        assert!(words.contains(&"type".to_owned()));
    }

    #[test]
    fn tooling_context_revision_closer_does_not_own_following_cursor() {
        let source = "f()";
        let probe = probe_tooling(source, None, Some(source.len() as u32), None);
        assert!(probe.facts.cursor.is_none(), "{:?}", probe.facts.cursor);
        let source = "package app; build { docs { md \"docs\"; } }";
        let cursor = source.find(" } }").unwrap() as u32 + 2;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Package),
            Some(cursor),
            None,
        );
        assert!(
            probe
                .facts
                .cursor
                .as_ref()
                .is_none_or(|context| !context.keywords.contains(&"support")),
            "{:?}",
            probe.facts.cursor
        );
    }

    #[test]
    fn tooling_context_revision_keyword_end_is_not_the_next_binder() {
        let source = "module app; fn run() { () }";
        let cursor = source.find("fn").unwrap() as u32 + 2;
        let probe = probe_tooling(
            source,
            Some(crate::ast::KioFileKind::Module),
            Some(cursor),
            None,
        );
        let context = probe
            .facts
            .cursor
            .expect("replace the just-written keyword");
        assert_eq!(
            context.atom.replacement,
            crate::span::Span::new(cursor - 2, cursor)
        );
        assert!(probe.facts.suppression.is_none());
    }

    #[test]
    fn tooling_context_revision_empty_sections_use_their_declaration_context() {
        use crate::ast::KioFileKind;
        for (source, kind, expected) in [
            (
                "signature app v(1); v(1) { with { module api { ",
                KioFileKind::Signature,
                vec!["import", "pub", "pure", "host", "rec", "type", "newtype"],
            ),
            (
                "signature app v(1); v(1) { breaking { add { module api { ",
                KioFileKind::Signature,
                vec!["import", "pub", "pure", "host", "type", "newtype"],
            ),
            (
                "module app; rec { ",
                KioFileKind::Module,
                vec!["pub", "type", "newtype", "labels"],
            ),
            (
                "signature app v(1); v(1) { with { module api { rec { ",
                KioFileKind::Signature,
                vec!["pub", "type", "newtype"],
            ),
        ] {
            let probe = probe_tooling(source, Some(kind), Some(source.len() as u32), None);
            assert_eq!(
                probe.facts.cursor.expect(source).keywords,
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_context_revision_signature_host_roles_are_exact() {
        let source = "signature app v(1); v(1) { with { module api { host type A; host fn fn() -> A; } }; breaking { add { module api { pub fn run() -> .; } } } }";
        let probe = probe_tooling(source, Some(crate::ast::KioFileKind::Signature), None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        let keywords = tooling_keywords(&probe);
        for word in ["type", "fn"] {
            let start = source.find(&format!("host {word}")).unwrap() as u32 + 5;
            assert!(keywords.contains(&(
                word.to_owned(),
                crate::span::Span::new(start, start + word.len() as u32)
            )));
            assert!(
                !keywords
                    .iter()
                    .any(|(_, span)| span.start == start + word.len() as u32 + 1)
            );
        }
    }

    #[test]
    fn tooling_context_revision_invalid_modifiers_have_no_remaining_choice() {
        for source in [
            "module app; pure host fn run() { () }",
            "module app; rec { pure ",
        ] {
            let cursor = source.find("fn").unwrap_or(source.len()) as u32;
            let probe = probe_tooling(
                source,
                Some(crate::ast::KioFileKind::Module),
                Some(cursor),
                None,
            );
            assert!(probe.parse_error.is_some());
            assert!(
                probe.facts.cursor.expect(source).keywords.is_empty(),
                "{source}"
            );
        }
    }

    fn tooling_syntax_json(syntax: &ToolingSyntax) -> serde_json::Value {
        match syntax {
            ToolingSyntax::Module(ast) => serde_json::to_value(ast),
            ToolingSyntax::Package(ast) => serde_json::to_value(ast),
            ToolingSyntax::Signature(ast) => serde_json::to_value(ast),
            ToolingSyntax::Dependency(ast) => serde_json::to_value(ast),
            ToolingSyntax::Lock(ast) => serde_json::to_value(ast),
            ToolingSyntax::Declarations { imports, items } => {
                serde_json::to_value((imports, items))
            }
            ToolingSyntax::Expression(ast) => serde_json::to_value(ast),
        }
        .expect("source AST serializes")
    }

    #[test]
    fn tooling_scope_revision_incomplete_body_retains_written_prefix() {
        use super::ScopeSyntax;
        use crate::ast::KioFileKind;
        let source = "module app; import provider(value); type Earlier = .; fn run(parameter: .) { let local = parameter; f(   ";
        let cursor = source.len() as u32;
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor), None);
        assert!(probe.facts.recovered_group);
        assert_eq!(
            probe.facts.cursor.as_ref().expect("argument gap").slot,
            CursorSlot::Argument
        );
        let region = probe.facts.regions.last().expect("function body region");
        assert!(region.recovered);
        assert_eq!(region.interior.end, cursor);
        assert_eq!(
            &source[region.open.start as usize..region.open.end as usize],
            "{"
        );
        let [signature, local] = probe.facts.scope_prefix.as_slice() else {
            panic!("{:?}", probe.facts.scope_prefix);
        };
        assert_eq!(signature.owner, region.open);
        assert_eq!(local.owner, region.open);
        assert!(
            matches!(&signature.syntax, ScopeSyntax::Signature(sig) if matches!(&sig.params[0], crate::ast::SignatureParam::Value(param) if param.name == "parameter"))
        );
        assert!(
            matches!(&local.syntax, ScopeSyntax::Binding {name: Some((name, _)), ..} if name == "local")
        );
        assert_eq!(source.as_bytes()[local.interval.start as usize - 1], b';');
        let module = match probe.syntax {
            Some(ToolingSyntax::Module(module)) => module,
            _ => panic!("a recovered parse is marked separately, not discarded"),
        };
        assert_eq!(module.imports.len(), 1);
        assert!(
            matches!(&module.items[0], crate::ast::Item::TypeAlias(alias) if alias.name == "Earlier")
        );
    }

    #[test]
    fn tooling_scope_revision_failed_body_preserves_prefix_without_fake_ast() {
        use crate::ast::KioFileKind;
        let source = "module app; import provider(value); type Earlier = .; fn run(parameter: .) { let local = parameter;   ";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(probe.syntax.is_none());
        assert!(probe.facts.regions[0].body.is_none());
        assert_eq!(probe.facts.scope_prefix.len(), 2);
        assert_eq!(probe.facts.prefix_imports.len(), 1);
        assert!(
            matches!(&probe.facts.prefix_items[0], crate::ast::Item::TypeAlias(alias) if alias.name == "Earlier")
        );
    }

    #[test]
    fn tooling_scope_revision_regions_are_cursor_local_and_binder_ordered() {
        use super::core::ScopeSyntax;
        use crate::ast::KioFileKind;
        let source = "module app; fn before(other: .) { other } fn run(parameter: .) { let outer = parameter; scope! { let inner = outer; inner } } fn after(other: .) { other }";
        for marker in ["= parameter", "= outer", "inner }", "other }"] {
            let cursor = source.find(marker).unwrap() as u32 + 2;
            let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor), None);
            let bindings: Vec<_> = probe
                .facts
                .scope_prefix
                .iter()
                .filter_map(|prefix| match &prefix.syntax {
                    ScopeSyntax::Binding {
                        name: Some((name, _)),
                        ..
                    } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let expected: &[&str] = match marker {
                "= outer" => &["outer"],
                "inner }" => &["outer", "inner"],
                _ => &[],
            };
            assert_eq!(bindings, expected, "{marker}");
            for prefix in &probe.facts.scope_prefix {
                assert!(
                    probe
                        .facts
                        .regions
                        .iter()
                        .any(|region| region.open == prefix.owner)
                );
                assert!(prefix.interval.start <= cursor && cursor <= prefix.interval.end);
            }
        }
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.facts.regions.is_empty());
        assert!(probe.facts.scope_prefix.is_empty());
        assert_eq!(
            tooling_syntax_json(probe.syntax.as_ref().unwrap()),
            serde_json::to_value(parse(source).unwrap()).unwrap()
        );
    }

    #[test]
    fn tooling_scope_revision_retains_patterns_and_row_lets_as_ast_carriers() {
        use super::core::ScopeSyntax;
        use crate::ast::KioFileKind;
        let source =
            "module app; fn run((left: ., right: .)) { let .(a: ., b: .) = (left, right); ";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        let [signature, binding] = probe.facts.scope_prefix.as_slice() else {
            panic!("{:?}", probe.facts.scope_prefix)
        };
        assert!(
            matches!(&signature.syntax, ScopeSyntax::Signature(sig) if matches!(&sig.params[0], crate::ast::SignatureParam::Value(param) if param.pattern.as_ref().is_some_and(|p| p.elems.len() == 2)))
        );
        assert!(
            matches!(&binding.syntax, ScopeSyntax::Binding { name: None, pattern: Some(pattern), .. } if pattern.elems.len() == 2)
        );
        let source = "module app; fn run(record: .) { let .({field as renamed}) = record; ";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.parse_error.is_some());
        assert!(
            matches!(&probe.facts.scope_prefix[1].syntax, ScopeSyntax::RowLet(entries) if entries.len() == 1)
        );
    }

    #[test]
    fn tooling_scope_revision_retained_success_ast_matches_ordinary_file_roots() {
        use crate::ast::KioFileKind;
        let cases = [
            ("module app; fn run() { () }", Some(KioFileKind::Module)),
            ("package app;", Some(KioFileKind::Package)),
            ("signature app v(1);", Some(KioFileKind::Signature)),
            (
                "dependency app; source { path \"app.pkg.kio\"; }",
                Some(KioFileKind::Dependency),
            ),
            (
                "lock app; resolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; }",
                Some(KioFileKind::Lock),
            ),
        ];
        for (source, kind) in cases {
            let probe = probe_tooling(source, kind, None, None);
            let expected = match kind.unwrap() {
                KioFileKind::Module => serde_json::to_value(parse(source).unwrap()),
                KioFileKind::Package => {
                    serde_json::to_value(parse_package_file(source, None).unwrap())
                }
                KioFileKind::Signature => {
                    serde_json::to_value(parse_signature_file(source, None).unwrap())
                }
                KioFileKind::Dependency => {
                    serde_json::to_value(parse_dependency_file(source, None).unwrap())
                }
                KioFileKind::Lock => serde_json::to_value(parse_lock_file(source, None).unwrap()),
            }
            .unwrap();
            assert_eq!(
                tooling_syntax_json(probe.syntax.as_ref().unwrap()),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn tooling_atom_revision_replaces_only_the_current_identifier() {
        use crate::span::Span;
        for (source, cursor, replacement, prefix) in [
            ("f(  alphabet)", 8, Span::new(4, 12), Span::new(4, 8)),
            ("f(  alphabet)", 4, Span::new(4, 12), Span::new(4, 4)),
            ("f(  alphabet)", 12, Span::new(4, 12), Span::new(4, 12)),
            ("f(  alphabet)", 3, Span::new(3, 3), Span::new(3, 3)),
            ("f(   ", 5, Span::new(5, 5), Span::new(5, 5)),
        ] {
            let probe = probe_tooling(source, None, Some(cursor), None);
            let context = probe.facts.cursor.expect(source);
            assert_eq!(context.atom.replacement, replacement, "{source}@{cursor}");
            assert_eq!(context.atom.prefix, prefix, "{source}@{cursor}");
        }
    }

    #[test]
    fn tooling_atom_revision_split_arrow_advances_the_grammar_gap() {
        use crate::ast::KioFileKind;
        let source = "module app; fn run() ->! { () }";
        let cursor = source.find('!').unwrap() as u32;
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor), None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        let context = probe.facts.cursor.expect("return type");
        assert_eq!(context.span.start, cursor);
        assert_eq!(
            context.atom.replacement,
            crate::span::Span::new(cursor, cursor + 1)
        );
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor - 1), None);
        assert!(probe.facts.cursor.is_none());
        assert_eq!(
            probe.facts.suppression.unwrap().kind,
            core::CursorSuppressionKind::Structural
        );
    }

    #[test]
    fn tooling_atom_revision_suppression_is_explicit() {
        use super::core::CursorSuppressionKind;
        use crate::ast::KioFileKind;
        for (source, marker, kind, expected) in [
            (
                "module app; fn run(parameter: .) { () }",
                "parameter",
                Some(KioFileKind::Module),
                CursorSuppressionKind::Binder,
            ),
            (
                "f(.[Binder](value) { value })",
                "Binder",
                None,
                CursorSuppressionKind::Binder,
            ),
            (
                "f(\"literal\")",
                "literal",
                None,
                CursorSuppressionKind::Literal,
            ),
            (
                "// comment",
                "comment",
                None,
                CursorSuppressionKind::Comment,
            ),
            (
                "f(\"invalid\\q\")",
                "invalid",
                None,
                CursorSuppressionKind::LexicalError,
            ),
        ] {
            let cursor = source.find(marker).unwrap() as u32 + 2;
            let probe = probe_tooling(source, kind, Some(cursor), None);
            assert!(probe.facts.cursor.is_none(), "{source}");
            assert_eq!(probe.facts.suppression.expect(source).kind, expected);
        }
    }

    #[test]
    fn tooling_review_direct_recovered_open_is_visible() {
        use crate::ast::KioFileKind;
        let source = "module app; rec(loop) { fn run() -> . { () }";
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        assert!(
            probe.facts.recovered_group,
            "the recursion-group opener was recovered"
        );
        let closed = format!("{source}}}");
        let probe = probe_tooling(&closed, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none());
        assert!(!probe.facts.recovered_group);
    }

    #[test]
    fn tooling_review_failed_token_is_not_a_completion_gap() {
        for (source, cursor) in [(r#""text\q""#, 2), (r#"f("text\q")"#, 4)] {
            let probe = probe_tooling(source, None, Some(cursor), None);
            let error = probe.lexical_error.as_ref().expect("invalid string escape");
            assert_eq!(error.diag(), lex(source).unwrap_err().diag());
            assert!(
                probe.facts.cursor.is_none(),
                "{source}: {:?}",
                probe.facts.cursor
            );
        }
    }

    #[test]
    fn tooling_review_trailing_comments_suppress_cursor() {
        use crate::ast::KioFileKind;
        for (source, kind) in [
            ("// comment", None),
            ("// comment\n  ", None),
            ("// comment\n\"text\\q\"", None),
            ("// comment\n//bad", None),
            (
                "module app; fn run() { // comment",
                Some(KioFileKind::Module),
            ),
        ] {
            let cursor = source.find("comment").unwrap() as u32 + 2;
            let probe = probe_tooling(source, kind, Some(cursor), None);
            assert!(
                probe.facts.cursor.is_none(),
                "{source}: {:?}",
                probe.facts.cursor
            );
        }
        let source = "// comment\n  ";
        let probe = probe_tooling(source, None, Some(source.len() as u32), None);
        assert_eq!(
            probe.facts.cursor.expect("after comment").slot,
            core::CursorSlot::Value
        );
    }

    #[test]
    fn tooling_review_failed_item_cannot_claim_a_later_cursor() {
        use crate::ast::KioFileKind;
        let source = "module app; type Broken = \n fn later(x: .) -> . { x }";
        let cursor = source.rfind('x').unwrap() as u32;
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor), None);
        assert!(probe.parse_error.is_some());
        assert_eq!(
            probe.facts.cursor.expect("later function body").slot,
            core::CursorSlot::Value
        );
        let alias_cursor = source.find("= ").unwrap() as u32 + 2;
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(alias_cursor), None);
        assert_eq!(
            probe.facts.cursor.expect("unfinished alias").slot,
            core::CursorSlot::Type
        );
    }

    #[test]
    fn tooling_review_failed_item_cannot_claim_next_items_comment() {
        use crate::ast::KioFileKind;
        let source = "module app; type Broken = \n // comment\n fn later(x: .) -> . { x }";
        let cursor = source.find("comment").unwrap() as u32 + 2;
        let probe = probe_tooling(source, Some(KioFileKind::Module), Some(cursor), None);
        assert!(probe.parse_error.is_some());
        assert!(probe.facts.cursor.is_none(), "{:?}", probe.facts.cursor);
    }

    #[test]
    fn tooling_roles_are_exact_and_survive_a_malformed_sibling() {
        use crate::ast::KioFileKind;
        let source = "module app; newtype Box : . { constructor constructor; projector projector; }; host type A role(i32); fn broken(";
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert_eq!(probe.kind, Some(KioFileKind::Module));
        assert!(probe.lexical_error.is_none());
        assert!(probe.parse_error.is_some());
        let keywords = tooling_keywords(&probe);
        for word in ["constructor", "projector", "role"] {
            let positions: Vec<_> = keywords
                .iter()
                .filter(|(text, _)| text == word)
                .map(|(_, span)| span.start as usize)
                .collect();
            assert_eq!(positions, vec![source.find(word).unwrap()], "{word}");
        }
    }

    #[test]
    fn tooling_package_prefix_and_omitted_cache_are_distinct() {
        use crate::ast::KioFileKind;
        for source in [
            "package app; build { cache (); target rust {",
            "package app; build { target rust {",
        ] {
            let probe = probe_tooling(source, Some(KioFileKind::Package), None, None);
            assert!(parse_package_file(source, None).is_err());
            assert!(probe.facts.recovered_group);
            let keywords = tooling_keywords(&probe);
            assert_eq!(
                keywords.iter().filter(|(word, _)| word == "cache").count(),
                usize::from(source.contains("cache"))
            );
            assert_eq!(
                keywords.iter().filter(|(word, _)| word == "target").count(),
                1
            );
            assert!(
                probe
                    .facts
                    .keywords
                    .iter()
                    .all(|fact| fact.span.start < fact.span.end)
            );
        }
    }

    #[test]
    fn tooling_build_choices_follow_occupancy_and_retract_after_edits() {
        use crate::ast::KioFileKind;
        for (prefix, expected) in [
            ("package app; build { ", vec!["cache", "docs", "target"]),
            ("package app; build { cache (); ", vec!["docs", "target"]),
            (
                "package app; build { docs { md \"docs\"; } ",
                vec!["cache", "target"],
            ),
            (
                "package app; build { target rust {} ",
                vec!["cache", "docs", "target"],
            ),
            (
                "package app; build { target rust {} docs { md \"docs\"; } cache (); ",
                vec!["target"],
            ),
        ] {
            let source = format!("{prefix}}}");
            let probe = probe_tooling(
                &source,
                Some(KioFileKind::Package),
                Some(prefix.len() as u32),
                None,
            );
            let context = probe.facts.cursor.expect(prefix);
            assert_eq!(context.slot, core::CursorSlot::BuildField);
            assert_eq!(context.keywords, expected, "{prefix}");
        }
    }

    #[test]
    fn tooling_newtype_choices_use_member_occupancy() {
        use crate::ast::KioFileKind;
        for (prefix, expected) in [
            (
                "module a; newtype A : . { ",
                vec!["pub", "constructor", "projector"],
            ),
            (
                "module a; newtype A : . { constructor mk; ",
                vec!["pub", "projector"],
            ),
            (
                "module a; newtype A : . { projector get; pub ",
                vec!["constructor"],
            ),
            (
                "module a; newtype A : . { constructor mk; projector get; ",
                vec![],
            ),
        ] {
            let source = format!("{prefix}}};");
            let probe = probe_tooling(
                &source,
                Some(KioFileKind::Module),
                Some(prefix.len() as u32),
                None,
            );
            let context = probe.facts.cursor.expect(prefix);
            assert_eq!(context.slot, core::CursorSlot::NewtypeMember);
            assert_eq!(context.keywords, expected, "{prefix}");
        }
    }

    #[test]
    fn tooling_lexical_failure_preserves_only_its_proven_prefix() {
        use crate::ast::KioFileKind;
        let source = "module app; host type A role(i32); fn broken() { \"unterminated";
        let probe = probe_tooling(
            source,
            Some(KioFileKind::Module),
            Some(source.len() as u32),
            None,
        );
        assert!(probe.lexical_error.is_some());
        assert!(probe.facts.cursor.is_none());
        assert!(
            tooling_keywords(&probe)
                .iter()
                .any(|(word, _)| word == "role")
        );
        assert!(lex(source).is_err());
    }

    #[test]
    fn tooling_module_body_uses_consumer_operator_grammar() {
        use crate::ast::KioFileKind;
        let source = "module app; import syntax(op _ + _, varop [% %]); fn run(a: .) -> . { if! a + a { do! a { let x = a; x } } else { [% a, a %] } }";
        parse(source).expect("ordinary imported grammar");
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        for word in ["let", "else"] {
            assert!(
                tooling_keywords(&probe)
                    .iter()
                    .any(|(text, span)| text == word
                        && span.start as usize == source.find(word).unwrap()),
                "{word}"
            );
        }
        for head in ["if", "do"] {
            assert!(
                !tooling_keywords(&probe)
                    .iter()
                    .any(|(word, _)| word == head)
            );
        }
    }

    #[test]
    fn tooling_recursive_calls_and_ordinary_parameters_keep_separate_roles() {
        use crate::ast::KioFileKind;
        let source = "module app; fn ordinary(rec: .) -> . { () } rec(loop) fn run() -> . { rec\n run\n () }";
        parse(source).expect("multiline recursive call");
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        let parameter = source.find("rec:").unwrap() as u32;
        assert!(
            probe
                .facts
                .keywords
                .iter()
                .all(|fact| fact.span.start != parameter)
        );
        let call = source.find("rec\n").unwrap() as u32;
        assert!(
            probe
                .facts
                .keywords
                .iter()
                .any(|fact| fact.span.start == call)
        );
    }

    #[test]
    fn tooling_signature_sections_do_not_promote_removed_names() {
        use crate::ast::KioFileKind;
        let source = "signature app v(1); v(1) { with { module api { host type A; } }; breaking { add { module api { pub fn run() -> .; } }; remove { module api { type; pub; fn; } } } }";
        parse_signature_file(source, None).expect("signature removal grammar");
        let probe = probe_tooling(source, Some(KioFileKind::Signature), None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        let removed_start = source.find("type; pub; fn;").unwrap() as u32;
        assert!(
            probe
                .facts
                .keywords
                .iter()
                .all(|fact| fact.span.start < removed_start)
        );
        for word in ["with", "add", "remove"] {
            assert!(
                tooling_keywords(&probe)
                    .iter()
                    .any(|(text, _)| text == word)
            );
        }
    }

    #[test]
    fn tooling_literals_comments_and_wrong_parent_words_stay_neutral() {
        use crate::ast::KioFileKind;
        let source = "module app; fn run(if: ., constructor: ., role: .) -> . { if }";
        let probe = probe_tooling(source, Some(KioFileKind::Module), None, None);
        assert!(probe.parse_error.is_none());
        assert!(
            tooling_keywords(&probe)
                .iter()
                .all(|(word, _)| !matches!(word.as_str(), "if" | "constructor" | "role"))
        );
        for (source, cursor) in [
            ("\"text\"", 3),
            ("123", 2),
            (".t", 2),
            ("// comment\nvalue", 5),
        ] {
            let probe = probe_tooling(source, None, Some(cursor), None);
            assert!(probe.facts.cursor.is_none(), "{source}");
        }
    }

    #[test]
    fn tooling_selected_type_argument_discards_value_cursor_and_forall_probe_facts() {
        let source = "consume((A) -> B)";
        let probe = probe_tooling(source, None, Some(source.find('A').unwrap() as u32), None);
        assert!(probe.parse_error.is_none());
        assert_eq!(
            probe.facts.cursor.expect("type argument context").slot,
            core::CursorSlot::Type
        );
        for source in [".[A]foo", ".[! { [!1 }"] {
            let probe = probe_tooling(source, None, None, None);
            assert!(
                probe.facts.structural_forall.bracket_offsets.is_empty(),
                "{source}"
            );
        }
        let source = "consume(if! .t { () } else { () })";
        let probe = probe_tooling(source, None, None, None);
        assert!(probe.parse_error.is_none(), "{:?}", probe.parse_error);
        assert_eq!(
            tooling_keywords(&probe)
                .iter()
                .filter(|(word, _)| word == "if")
                .count(),
            0
        );
        let start = source.find("if!").unwrap() as u32;
        assert_eq!(
            probe
                .facts
                .source_names
                .iter()
                .filter(|fact| {
                    fact.span == crate::span::Span::new(start, start + 2)
                        && fact.role == core::SourceNameRole::FunctionReference
                })
                .count(),
            1
        );
        assert_eq!(
            crate::tokens::dump(source)
                .unwrap()
                .iter()
                .find(|token| token.span.start == start)
                .unwrap()
                .kind,
            crate::tokens::TokenKind::KeywordElaborator
        );
    }

    fn import_grammar_import_grammar(signature: &str) -> OperatorGrammar {
        let module = p(&format!("module consumer; import provider({signature});"));
        let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective")
        };
        let ImportItem::OperatorPattern { grammar, .. } = &items[0] else {
            panic!("operator")
        };
        grammar.clone()
    }

    fn import_grammar_declaration_grammar(declaration: &str) -> OperatorGrammar {
        let module = p(&format!("module provider; {declaration}"));
        match &module.items[0] {
            Item::Op(op, _) => {
                let OpBody::Normal { pattern, .. } = &op.body;
                OperatorGrammar::fixed(pattern)
            }
            Item::VariadicOperator(fold, _) => OperatorGrammar::variadic(&fold.open, &fold.spec),
            other => panic!("operator declaration: {other:?}"),
        }
    }

    fn import_grammar_roundtrip(grammar: &OperatorGrammar) {
        let rendered = grammar.render();
        let parsed = import_grammar_import_grammar(&rendered);
        assert_eq!(&parsed, grammar, "{rendered}");
        assert_eq!(parsed.render(), rendered);
        let tokens = lex(&rendered).expect("canonical grammar tokens");
        let mut commented = String::new();
        for (index, token) in tokens.iter().enumerate() {
            commented.push_str(&format!("// grammar boundary {index}\n"));
            commented.push_str(&rendered[token.span.start as usize..token.span.end as usize]);
            commented.push(' ');
        }
        let module = p(&format!("module c; import p({commented});"));
        let canonical = crate::pretty::pretty_module(&module);
        for index in 0..tokens.len() {
            assert_eq!(
                canonical
                    .matches(&format!("// grammar boundary {index}\n"))
                    .count(),
                1
            );
        }
        let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
            panic!("selective")
        };
        let ImportItem::OperatorPattern {
            grammar: commented_grammar,
            ..
        } = &items[0]
        else {
            panic!("operator")
        };
        assert_eq!(commented_grammar, grammar);
    }

    #[test]
    fn variadic_open_is_one_maximal_run_independent_of_registered_prefixes() {
        for declarations in [
            "varop [%? ?%] { foldl append empty; }; \
             varop [%?:+- -+:?%] { foldl append empty; };",
            "import syntax(varop [%? ?%], varop [%?:+- -+:?%]);",
            "",
        ] {
            let source =
                format!("module consumer; {declarations} fn run() -> . {{ [%?:+- (), () -+:?%] }}");
            let eager = parse(&source).expect("structural opening run");
            assert_eq!(parse_lazy(&source).unwrap().force_all().unwrap(), eager);
            let Item::FnDef(function) = eager.items.last().unwrap() else {
                panic!("function")
            };
            let Expr::OpChain {
                kind:
                    OpChainKind::Variadic {
                        open_tokens,
                        elements,
                        ..
                    },
                ..
            } = &function.body
            else {
                panic!("variadic chain")
            };
            assert_eq!(open_tokens, &["[%?:+-"]);
            assert_eq!(elements.len(), 2);
            let formatted = crate::pretty::pretty_module(&eager);
            assert_eq!(
                crate::pretty::pretty_module(&parse(&formatted).unwrap()),
                formatted
            );

            let incomplete = source.replace("[%?:+- (),", "[%?:+ (),");
            assert!(parse(&incomplete).is_err());
            assert!(parse_lazy(&incomplete).unwrap().force_all().is_err());
        }
    }

    #[test]
    fn import_grammar_fixed_grammar_matrix() {
        let admitted = [
            "+ _",
            "_ !",
            "_ + _",
            "__ + _",
            "_ + __",
            "___ + _",
            "_ + ___",
            "_ ? _ : _",
            "< _ : _ : _ >",
            "< ( _ : _ ) >",
            "_ + (_)",
            "(__) + _",
            "_ (=) _",
            "_ && ++ _",
            "++",
            "_ ... _",
            "<% _ %>",
            "< ! _ ! >",
        ];
        for pattern in admitted {
            let grammar =
                import_grammar_declaration_grammar(&format!("op {pattern} {{ impl f; }};"));
            import_grammar_roundtrip(&grammar);
        }
        let rejected = [
            "_ _",
            "_ + __ + __",
            "_ (;) _",
            "_ = _",
            "_ . _",
            "_ + (___)",
            "_ + ____",
            "[ _ ]",
            "[ ! _ ! ]",
            "_ [! _ !]",
        ];
        for pattern in rejected {
            assert!(
                parse(&format!("module p; op {pattern} {{ impl f; }};")).is_err(),
                "{pattern}"
            );
            assert!(
                parse(&format!("module c; import p(op {pattern});")).is_err(),
                "{pattern}"
            );
        }
        eprintln!(
            "fixed: candidates={} admitted={} rejected={} projected={}",
            admitted.len() + rejected.len(),
            admitted.len(),
            rejected.len(),
            admitted.len()
        );
    }

    #[test]
    fn import_grammar_variadic_grammar_matrix() {
        let mut count = 0;
        for (open, close) in [
            ("[%", "%]"),
            ("[...", "...]"),
            ("[<", "<]"),
            ("+[?", "?]+"),
            ("[[*", "*]]"),
            ("[!", "!]"),
        ] {
            for whitespace in [" ", "\t", "\n", "\r\n"] {
                for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
                    let declaration = format!(
                        "varop {open}{whitespace}{close} {{ {mode} step base; finalize finish; }};"
                    );
                    let grammar = import_grammar_declaration_grammar(&declaration);
                    import_grammar_roundtrip(&grammar);
                    let signature = grammar.render();
                    let siblings = p(&format!(
                        "module c; import p({signature}, z, {{field}}, op _ + _, varop [~ ~]);"
                    ));
                    let ImportKind::Selective { items, .. } = &siblings.imports[0].kind else {
                        panic!("selective")
                    };
                    assert_eq!(items.len(), 5);
                    count += 1;
                }
            }
        }
        assert_eq!(count, 96);
        eprintln!("variadic: candidates=96 admitted=96 rejected=0 projected=96 sibling_lists=96");
    }

    #[test]
    fn import_grammar_projection_roundtrip() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, RngSeed, TestRunner};
        let config = Config {
            cases: 192,
            rng_seed: RngSeed::Fixed(0x0408_2026_0908),
            failure_persistence: None,
            ..Config::default()
        };
        let strategy = (
            proptest::collection::vec(
                prop::sample::select(vec!["+", "?", "&&", "..", "<%", "%>"]),
                1..17,
            ),
            0usize..6,
        );
        let count = std::cell::Cell::new(0);
        let classes = std::cell::RefCell::new([0usize; 6]);
        let mut runner = TestRunner::new(config);
        runner
            .run(&strategy, |(runs, class)| {
                let run = runs.join(" ");
                let pattern = match class {
                    0 => format!("_ {run} _"),
                    1 => format!("{run} _"),
                    2 => format!("_ {run}"),
                    3 => format!("__ {run} _"),
                    4 => format!("_ {run} ___"),
                    _ => format!("_ {run} (_)"),
                };
                let grammar =
                    import_grammar_declaration_grammar(&format!("op {pattern} {{ impl f; }};"));
                import_grammar_roundtrip(&grammar);
                count.set(count.get() + 1);
                classes.borrow_mut()[class] += 1;
                Ok(())
            })
            .expect("all generated declarations project without filtering");
        assert_eq!(count.get(), 192);
        assert!(classes.borrow().iter().all(|count| *count > 0));
        eprintln!("property structural classes={:?}", classes.borrow());
        eprintln!(
            "property: seed=0x040820260908 candidates=192 admitted=192 rejected=0 projected=192"
        );
    }

    #[test]
    fn import_grammar_projection_injective() {
        let pairs = [
            ("op _ + _", "op __ + _"),
            ("op _ + _", "op _ + ___"),
            ("op _ + _", "op _ + (_)"),
            ("op _ ? _", "op _ ? _ : _"),
            ("op _ && ++ _", "op _ &&++ _"),
            ("op <% _ %>", "varop [% %]"),
            ("varop [% %]", "varop [: :]"),
            ("varop [% %]", "varop [%% %%]"),
            ("varop [% %]", "varop %[% %]%"),
        ];
        for (left, right) in pairs {
            let a = import_grammar_import_grammar(left);
            let b = import_grammar_import_grammar(right);
            assert_ne!(a, b);
            assert_ne!(a.render(), b.render(), "{left} / {right}");
            import_grammar_roundtrip(&a);
            import_grammar_roundtrip(&b);
            if !left.contains("&& ++") && !right.starts_with("varop") {
                assert_eq!(a.dispatch_key(), b.dispatch_key());
            } else {
                assert_ne!(a.dispatch_key(), b.dispatch_key());
            }
        }
        eprintln!("injectivity: pairs=9 same_key_pairs=4 distinct_renderings=9");
    }

    #[test]
    fn import_grammar_import_recovery_boundaries() {
        for tail in [
            "import m",
            "import m(",
            "import m(a,",
            "import m(varop [%);",
            "import m(varop [% ]%);",
        ] {
            let source = format!("module c; {tail}");
            assert!(parse(&source).is_err(), "complete parser rejects {tail}");
            let (_, errors) = parse_recover_imports(&source).expect("recovered module");
            assert!(!errors.is_empty(), "{tail}");
            assert!(
                !lex_and_build(&source)
                    .expect("syntax tree")
                    .children
                    .is_empty()
            );
        }
        for bad in ["op _ = _", "varop [%", "varop [% ]%"] {
            let source = format!(
                "module c; import m({bad}, // sibling once\n good); fn after() -> . {{ () }}"
            );
            assert!(parse(&source).is_err());
            let (module, errors) =
                parse_recover_imports(&source).expect("recover sibling and next declaration");
            assert!(!errors.is_empty());
            let ImportKind::Selective { items, .. } = &module.imports[0].kind else {
                panic!("selective")
            };
            assert!(
                items
                    .iter()
                    .any(|item| matches!(item, ImportItem::Name { name, .. } if name == "good"))
            );
            assert!(matches!(&module.items[0], Item::FnDef(d) if d.name == "after"));
            assert_eq!(
                crate::pretty::pretty_module(&module)
                    .matches("sibling once")
                    .count(),
                1
            );
        }
        for declaration in [
            "type A = .;",
            "newtype A : . { constructor make; projector take; };",
            "labels { field: . };",
            "pub fn after() -> . { () }",
            "rec newtype A : . | A { constructor make; projector take; };",
        ] {
            let source = format!("module c; import m\n {declaration}");
            assert!(parse(&source).is_err());
            let (module, errors) =
                parse_recover_imports(&source).expect("recover declaration head");
            assert!(!errors.is_empty(), "{declaration}");
            assert_eq!(module.items.len(), 1, "{declaration}");
            let expected = p(&format!("module c; {declaration}"));
            assert_eq!(
                crate::pretty::pretty_module(&module),
                crate::pretty::pretty_module(&expected)
            );
        }
        eprintln!("recovery: incomplete=5 malformed_with_siblings=3 declaration_heads=5");
    }

    #[test]
    fn import_grammar_eager_lazy_equivalence() {
        let eager = parse(IMPORT_GRAMMAR_CONSUMER).expect("eager");
        import_grammar_assert_consumer(&eager);
        let lazy = parse_lazy(IMPORT_GRAMMAR_CONSUMER).expect("lazy");
        let forced = lazy.force_all().expect("force without provider");
        import_grammar_assert_consumer(&forced);
        assert_eq!(
            crate::pretty::pretty_module(&eager),
            crate::pretty::pretty_module(&forced)
        );
        import_grammar_assert_consumer(&parse_module_file(IMPORT_GRAMMAR_CONSUMER).unwrap().module);
        import_grammar_assert_consumer(
            &parse_module_file_lazy(IMPORT_GRAMMAR_CONSUMER)
                .unwrap()
                .lazy
                .unwrap()
                .force_all()
                .unwrap(),
        );
    }
    use crate::ast::*;
    use crate::error::{Applicability, Diagnostic};
    use crate::span::Span;

    fn p(src: &str) -> Module {
        parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e:?}"))
    }

    #[test]
    fn contextual_rec_ordinary_values_and_calls_keep_their_ast() {
        for expression in ["rec", "(rec)"] {
            let module = p(&format!("module app; fn use(rec: .) {{ {expression} }}"));
            let Item::FnDef(def) = &module.items[0] else {
                panic!()
            };
            assert!(
                matches!(&def.body, Expr::Path { segments, .. } if segments == &vec!["rec".to_owned()]),
                "{expression}: {:?}",
                def.body
            );
        }
        for expression in [
            "rec()",
            "rec(value)",
            "rec(cont)",
            "rec(poly)",
            "rec(escape)",
            "rec(value, other)",
            "rec(())",
        ] {
            let module = p(&format!("module app; fn use() {{ {expression} }}"));
            let Item::FnDef(def) = &module.items[0] else {
                panic!()
            };
            let Expr::Call { callee, .. } = &def.body else {
                panic!("{expression}: {:?}", def.body)
            };
            assert!(
                matches!(callee.as_ref(), Expr::Path { segments, .. } if segments == &vec!["rec".to_owned()])
            );
            let source = format!("module app; fn use() {{ {expression} }}");
            let probe = probe_tooling_source(&source, None);
            assert!(!probe.facts.keywords.iter().any(|fact| &probe.source
                [fact.span.start as usize..fact.span.end as usize]
                == "rec"));
        }
    }

    #[test]
    fn contextual_rec_real_calls_and_incomplete_prefixes_keep_their_roles() {
        for expression in [
            "rec again(value)",
            "rec(poly) again(value)",
            "rec(cont, poly) again(value)",
        ] {
            let module = p(&format!("module app; fn use() {{ {expression} }}"));
            let Item::FnDef(def) = &module.items[0] else {
                panic!()
            };
            assert!(matches!(&def.body, Expr::RecCall { callee, .. } if callee.name == "again"));
        }
        for (tail, slot) in [
            ("rec |", CursorSlot::RecursiveCallee),
            ("rec(|", CursorSlot::RecursiveAnnotation),
            ("rec(po|", CursorSlot::RecursiveAnnotation),
            ("rec(cont, |", CursorSlot::RecursiveAnnotation),
            ("rec(cont) |", CursorSlot::RecursiveCallee),
            ("rec ag|ain(value)", CursorSlot::RecursiveCallee),
        ] {
            let marked = format!("module app; rec(loop) fn again[A](value: A) {{ {tail}");
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let probe = probe_tooling_source(&source, Some(cursor as u32));
            assert_eq!(
                probe.facts.cursor.as_ref().map(|context| context.slot),
                Some(slot),
                "{source}: {:?}",
                probe.facts.cursor
            );
        }
    }

    #[test]
    fn contextual_rec_ordinary_call_gaps_preserve_completed_syntax() {
        for expression in ["rec(value)", "rec()", "rec(cont, cont)", "rec(escape)"] {
            let marked = format!("module app; rec(loop) fn run(value: .) {{ {expression} | }}");
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let probe = probe_tooling_source(&source, Some(cursor as u32));
            assert!(
                probe.parse_error.is_none(),
                "{source}: {:?}",
                probe.parse_error
            );
            let Some(ToolingSyntax::Module(module)) = probe.syntax else {
                panic!("{source}")
            };
            let Item::RecGroup(group, _) = &module.items[0] else {
                panic!()
            };
            assert!(
                matches!(group.members[0].body, Expr::Call { .. }),
                "{source}"
            );
        }
    }

    #[test]
    fn contextual_rec_duplicate_annotations_keep_parser_envelope_roles() {
        for mode in ["poly", "cont"] {
            let source = format!("module app; fn run() {{ rec({mode}, {mode}) again() }}");
            let duplicate = source.rfind(mode).unwrap() as u32;
            let expected_span = Span::new(duplicate, duplicate + mode.len() as u32);
            let expected_message = format!("duplicate `rec` call annotation `{mode}`");
            let error = parse(&source).unwrap_err();
            assert_eq!(error.diag(), (expected_span, expected_message.as_str()));
            let probe = probe_tooling_source(&source, None);
            assert!(probe.syntax.is_none());
            assert_eq!(probe.parse_error.as_ref().unwrap().diag(), error.diag());
            let modes: Vec<_> = probe
                .facts
                .keywords
                .iter()
                .filter(|fact| &source[fact.span.start as usize..fact.span.end as usize] == mode)
                .collect();
            assert_eq!(modes.len(), 2, "{source}: {modes:?}");
            assert!(modes.iter().all(|fact| fact.role == KeywordRole::Control));
            let callee = source.find("again").unwrap() as u32;
            assert!(
                probe
                    .facts
                    .source_names
                    .iter()
                    .any(|fact| fact.span == Span::new(callee, callee + 5)
                        && fact.role == SourceNameRole::FunctionReference)
            );
        }
    }

    #[test]
    fn contextual_rec_duplicate_annotations_keep_public_token_roles() {
        let source = "module app; fn run() { rec(poly, poly) again() }";
        let tokens = crate::tokens::dump(source).unwrap();
        let kinds = |name: &str| {
            tokens
                .iter()
                .filter(|token| &source[token.span.start as usize..token.span.end as usize] == name)
                .map(|token| token.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds("poly"),
            vec![crate::tokens::TokenKind::KeywordControl; 2]
        );
        assert_eq!(
            kinds("again"),
            vec![crate::tokens::TokenKind::EntityNameFunctionReference]
        );
    }

    #[test]
    fn contextual_rec_unknown_annotations_keep_parser_envelope_roles() {
        for (annotations, message, marker) in [
            (
                "value, poly",
                "unknown `rec` call annotation `value`",
                "value",
            ),
            (
                "value, poly, poly",
                "unknown `rec` call annotation `value`",
                "value",
            ),
            (
                "poly, poly, value",
                "duplicate `rec` call annotation `poly`",
                "poly, value",
            ),
            (
                "value, other, poly",
                "unknown `rec` call annotation `value`",
                "value",
            ),
        ] {
            let source = format!("module app; fn run() {{ rec({annotations}) again() }}");
            let offset = source.find(marker).unwrap() as u32;
            let width = marker.split(',').next().unwrap().len() as u32;
            let probe = probe_tooling_source(&source, None);
            assert!(probe.syntax.is_none());
            assert_eq!(
                probe.parse_error.as_ref().unwrap().diag(),
                (Span::new(offset, offset + width), message)
            );
            assert_eq!(
                parse(&source).unwrap_err().diag(),
                probe.parse_error.as_ref().unwrap().diag()
            );
            let known: Vec<_> = probe
                .facts
                .keywords
                .iter()
                .filter(|fact| &source[fact.span.start as usize..fact.span.end as usize] == "poly")
                .collect();
            assert_eq!(
                known.len(),
                annotations.matches("poly").count(),
                "{source}: {known:?}"
            );
            assert!(known.iter().all(|fact| fact.role == KeywordRole::Control));
            assert!(probe.facts.keywords.iter().all(|fact| {
                !["value", "other"]
                    .contains(&&source[fact.span.start as usize..fact.span.end as usize])
            }));
            let callee = source.find("again").unwrap() as u32;
            assert!(
                probe
                    .facts
                    .source_names
                    .iter()
                    .any(|fact| fact.span == Span::new(callee, callee + 5)
                        && fact.role == SourceNameRole::FunctionReference)
            );
        }
    }

    #[test]
    fn contextual_rec_unknown_annotations_keep_public_token_roles() {
        let source = "module app; fn run() { rec(value, poly) again() }";
        let tokens = crate::tokens::dump(source).unwrap();
        for (name, expected) in [
            ("value", crate::tokens::TokenKind::Identifier),
            ("poly", crate::tokens::TokenKind::KeywordControl),
            (
                "again",
                crate::tokens::TokenKind::EntityNameFunctionReference,
            ),
        ] {
            assert_eq!(
                tokens
                    .iter()
                    .find(
                        |token| &source[token.span.start as usize..token.span.end as usize] == name
                    )
                    .unwrap()
                    .kind,
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn contextual_rec_annotation_role_recovery_stops_at_grammar_errors() {
        for ordinary in [
            "module app; fn run() { rec(poly, poly) }",
            "module app; fn run() { rec(value, poly) }",
        ] {
            let probe = probe_tooling_source(ordinary, None);
            assert!(probe.parse_error.is_none());
            assert!(
                probe.facts.keywords.iter().all(|fact| &ordinary
                    [fact.span.start as usize..fact.span.end as usize]
                    != "poly")
            );
        }
        for (tail, message, has_callee) in [
            (
                "rec(poly] again()",
                "expected `)` closing `rec` call annotations",
                false,
            ),
            ("rec(poly, 123) again()", "expected identifier", true),
            (
                "rec(unknown, 123) again()",
                "unknown `rec` call annotation `unknown`",
                true,
            ),
        ] {
            let source = format!("module app; fn run() {{ {tail} }}");
            let probe = probe_tooling_source(&source, None);
            assert!(probe.parse_error.is_some());
            assert!(probe.syntax.is_none());
            assert!(probe.facts.keywords.iter().all(|fact| &source
                [fact.span.start as usize..fact.span.end as usize]
                != "unknown"));
            assert_eq!(
                probe.facts.source_names.iter().any(|fact| &source
                    [fact.span.start as usize..fact.span.end as usize]
                    == "again"
                    && fact.role == SourceNameRole::FunctionReference),
                has_callee,
            );
            assert_eq!(probe.parse_error.as_ref().unwrap().diag().1, message);
            assert!(parse(&source).is_err());
        }
    }

    #[test]
    fn contextual_rec_invalid_items_keep_same_list_sibling_roles() {
        for (annotations, marker, message) in [
            ("(poly, poly), cont", "(poly", "expected identifier"),
            ("{ poly, poly }, cont", "{", "expected identifier"),
            ("123, cont", "123", "expected identifier"),
            (
                "poly(), cont",
                "()",
                "expected `)` closing `rec` call annotations",
            ),
            (
                "value, (poly), cont",
                "value",
                "unknown `rec` call annotation `value`",
            ),
            (
                "value poly, cont",
                "value",
                "unknown `rec` call annotation `value`",
            ),
            (
                "poly, poly, (poly), cont",
                "poly, (",
                "duplicate `rec` call annotation `poly`",
            ),
        ] {
            let source = format!("module app; fn run() {{ rec({annotations}) again() }}");
            let probe = probe_tooling_source(&source, None);
            let error = probe.parse_error.as_ref().unwrap();
            assert!(probe.syntax.is_none());
            assert_eq!(error.diag().1, message, "{source}");
            assert_eq!(
                error.diag().0.start as usize,
                source.find("rec(").unwrap() + 4 + annotations.find(marker).unwrap()
            );
            assert_eq!(parse(&source).unwrap_err().diag(), error.diag());
            let cont = source.rfind("cont").unwrap() as u32;
            assert!(
                probe.facts.keywords.iter().any(|fact| {
                    fact.span == Span::new(cont, cont + 4) && fact.role == KeywordRole::Control
                }),
                "{source}: {:?}",
                probe.facts.keywords
            );
            if let Some(nested) = annotations.find("(poly") {
                let nested = source.find("rec(").unwrap() + 4 + nested;
                assert!(
                    probe
                        .facts
                        .keywords
                        .iter()
                        .all(|fact| { fact.span.start != nested as u32 + 1 }),
                    "nested mode promoted: {source}"
                );
            }
        }
    }

    #[test]
    fn contextual_rec_invalid_items_keep_public_sibling_tokens() {
        for (source, poly_count) in [
            (
                "module app; fn run() { rec((poly, poly), cont) again() }",
                2,
            ),
            ("module app; fn run() { rec(value poly, cont) again() }", 1),
        ] {
            let tokens = crate::tokens::dump(source).unwrap();
            for (name, expected, count) in [
                ("poly", crate::tokens::TokenKind::Identifier, poly_count),
                ("cont", crate::tokens::TokenKind::KeywordControl, 1),
                (
                    "again",
                    crate::tokens::TokenKind::EntityNameFunctionReference,
                    1,
                ),
            ] {
                let actual: Vec<_> = tokens
                    .iter()
                    .filter(|token| {
                        &source[token.span.start as usize..token.span.end as usize] == name
                    })
                    .map(|token| token.kind)
                    .collect();
                assert_eq!(actual, vec![expected; count], "{name}");
            }
        }
    }

    #[test]
    fn contextual_rec_invalid_varop_items_shield_their_commas() {
        for (annotations, has_continuation) in [
            ("[* poly, poly *], cont", true),
            ("[* poly, [! poly, poly !] *], cont", true),
            ("[* (poly, poly), poly *], cont", true),
            ("[* poly, poly !], cont", false),
            ("[* poly, poly, cont", false),
        ] {
            let source = format!("module app; fn run() {{ rec({annotations}) again() }}");
            let probe = probe_tooling_source(&source, None);
            assert!(probe.syntax.is_none());
            let error = probe.parse_error.as_ref().unwrap();
            assert_eq!(error.diag().1, "expected identifier");
            assert_eq!(parse(&source).unwrap_err().diag(), error.diag());
            let tokens = crate::tokens::dump(&source).unwrap();
            let poly: Vec<_> = tokens
                .iter()
                .filter(|token| {
                    &source[token.span.start as usize..token.span.end as usize] == "poly"
                })
                .map(|token| token.kind)
                .collect();
            assert_eq!(
                poly,
                vec![crate::tokens::TokenKind::Identifier; annotations.matches("poly").count()],
                "{source}"
            );
            for (name, kind) in [
                (
                    "cont",
                    if has_continuation {
                        crate::tokens::TokenKind::KeywordControl
                    } else {
                        crate::tokens::TokenKind::Identifier
                    },
                ),
                (
                    "again",
                    crate::tokens::TokenKind::EntityNameFunctionReference,
                ),
            ] {
                assert_eq!(
                    tokens
                        .iter()
                        .find(|token| {
                            &source[token.span.start as usize..token.span.end as usize] == name
                        })
                        .unwrap()
                        .kind,
                    kind,
                    "{source}: {name}"
                );
            }
        }
    }

    #[test]
    fn contextual_rec_annotation_comma_runs_preserve_modes_and_format() {
        let canonical = p("module app; fn run() { rec(cont, poly) again() }");
        for annotations in [
            ",cont,poly",
            "cont,,,poly",
            "cont,poly,,,",
            ",,,cont,,,poly,,,",
        ] {
            let source = format!("module app; fn run() {{ rec({annotations}) again() }}");
            let module = p(&source);
            assert_eq!(
                crate::pretty::pretty_module(&module),
                crate::pretty::pretty_module(&canonical)
            );
            let Item::FnDef(def) = &module.items[0] else {
                panic!()
            };
            assert!(matches!(&def.body, Expr::RecCall { modes, .. }
                if modes == &[RecCallMode::Cont, RecCallMode::Poly]));
        }
    }

    #[test]
    fn contextual_rec_annotation_comma_prefixes_and_empty_controls() {
        for (tail, slot) in [
            ("rec(,,, |", CursorSlot::RecursiveAnnotation),
            ("rec(cont,,, |", CursorSlot::RecursiveAnnotation),
            ("rec(,,,cont,,,poly,,,) |", CursorSlot::RecursiveCallee),
        ] {
            let marked = format!("module app; rec(loop) fn again[A](value: A) {{ {tail}");
            let cursor = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let probe = probe_tooling_source(&source, Some(cursor as u32));
            assert_eq!(
                probe.facts.cursor.as_ref().map(|context| context.slot),
                Some(slot),
                "{source}"
            );
        }
        for annotations in ["", ",", ",,,"] {
            let source = format!("module app; fn run() {{ rec({annotations}) again() }}");
            assert_eq!(
                parse(&source).unwrap_err().diag().1,
                "`rec` call annotation list cannot be empty"
            );
        }
        for expression in ["rec((poly), cont)", "rec(,,,cont,,,poly,,,)", "rec(,,,)"] {
            let source = format!("module app; fn run() {{ {expression} }}");
            let probe = probe_tooling_source(&source, None);
            assert!(
                probe.parse_error.is_none(),
                "{source}: {:?}",
                probe.parse_error
            );
            assert!(probe.facts.keywords.iter().all(|fact| {
                !["rec", "cont", "poly"]
                    .contains(&&source[fact.span.start as usize..fact.span.end as usize])
            }));
        }
    }

    #[test]
    fn contextual_rec_outer_envelope_keeps_callee_after_invalid_annotation_groups() {
        for annotation in ["", "()", "(poly)", "helper(helper(helper(value)))", "123"] {
            let source = format!("module app; fn run() {{ rec({annotation}) again() }}");
            let probe = probe_tooling_source(&source, None);
            assert!(probe.parse_error.is_some());
            assert!(probe.syntax.is_none());
            let callee = source.find("again").unwrap() as u32;
            assert_eq!(
                probe
                    .facts
                    .source_names
                    .iter()
                    .filter(|fact| fact.span == Span::new(callee, callee + 5)
                        && fact.role == SourceNameRole::FunctionReference)
                    .count(),
                1,
                "{source}"
            );
            assert!(
                probe.facts.keywords.iter().all(|fact| &source
                    [fact.span.start as usize..fact.span.end as usize]
                    != "poly")
            );
            let tokens = crate::tokens::dump(&source).unwrap();
            assert_eq!(
                tokens
                    .iter()
                    .find(|token| token.span.start == callee)
                    .unwrap()
                    .kind,
                crate::tokens::TokenKind::EntityNameFunctionReference
            );
        }
        for (source, target) in [
            ("module app; fn run() { rec(poly, 123) Again() }", "Again"),
            ("module app; fn run() { rec(poly, 123) _() }", "_"),
            ("module app; fn run() { rec(poly, again()", "again"),
        ] {
            let probe = probe_tooling_source(source, None);
            assert!(probe.parse_error.is_some());
            assert!(probe.facts.source_names.iter().all(|fact| &source
                [fact.span.start as usize..fact.span.end as usize]
                != target
                || fact.role != SourceNameRole::FunctionReference));
        }
    }

    fn p_header(src: &str) -> Module {
        parse_lazy(src)
            .unwrap_or_else(|e| panic!("lazy header parse failed for {src:?}: {e:?}"))
            .module()
            .clone()
    }

    fn p_err(src: &str) -> String {
        match parse(src) {
            Err(err) => err.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {src:?}"),
        }
    }

    fn p_expr_with_context(
        src: &str,
        context: Option<&ExpressionParseContext>,
    ) -> Result<Expr, Error> {
        let tokens = lex(src)?;
        let skeleton = crate::pass::tree_skeleton::build(tokens);
        let mut parser = expression_parser(&skeleton, src.len(), context);
        let expr = parser.expr()?;
        parser.expect_eof()?;
        Ok(expr)
    }

    fn local_expression_context(module_src: &str) -> ExpressionParseContext {
        expression_parse_context(&p(module_src)).expect("local expression parse context")
    }

    #[test]
    fn value_expression_rejects_direct_fqn_without_slash_operator() {
        assert!(
            p_expr_with_context("provider/path.item()", None).is_err(),
            "a slash-qualified FQN must not parse as a value expression"
        );
        for source in [
            "module main; fn run() -> . { provider/path.item() }",
            "module main; fn sink(value: .) -> . { value } \
             fn run() -> . { sink(provider/path.item()) }",
            "module main; fn run() -> . { let value = provider/path.item(); value }",
            "module main; fn run() -> . { \
             if! provider/path.item() { () } else { () } }",
            "module main; fn run(value: .) -> . { \
             value.>provider/path.item() }",
        ] {
            let error = p_err(source);
            assert!(
                error.contains("slash-qualified item paths are not value expressions"),
                "nested direct FQN did not keep the focused diagnostic: {error}"
            );
        }
    }

    #[test]
    fn unbound_slash_operator_is_not_diagnosed_as_an_item_fqn() {
        for source in [
            "module main; fn run() -> . { left / right }",
            "module main; fn run() -> . { left() / right.item }",
        ] {
            let error = p_err(source);
            assert!(error.contains("expected `}`"), "{error}");
            assert!(
                !error.contains("slash-qualified item paths"),
                "an ordinary unbound slash operator was misclassified: {error}"
            );
        }
    }

    #[test]
    fn slash_operator_keeps_dotted_rhs_as_an_expression_path() {
        let context = local_expression_context(
            "module syntax; fn choose[A](left: A, right: A) -> A { right } \
             op _ / _ { impl choose; };",
        );
        let expr = p_expr_with_context("left/alias.item", Some(&context))
            .expect("the registered slash operator should parse");
        let Expr::OpChain {
            kind: OpChainKind::Normal { slots, .. },
            ..
        } = expr
        else {
            panic!("expected a slash operator chain")
        };
        let [
            Expr::Path { segments: left, .. },
            Expr::Path {
                segments: right, ..
            },
        ] = slots.as_slice()
        else {
            panic!("expected path operands")
        };
        assert_eq!(
            left.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["left"]
        );
        assert_eq!(
            right.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["alias", "item"]
        );
        p_expr_with_context("left()/alias.item", Some(&context))
            .expect("a call result followed by slash and a dotted RHS should parse as an operator");

        let ufcs_error = p_expr_with_context("receiver.>provider/path.item()", Some(&context))
            .expect_err("a UFCS callee path must not admit a direct item FQN");
        assert!(
            ufcs_error
                .diag()
                .1
                .contains("slash-qualified item paths are not value expressions"),
            "an imported slash operator masked the UFCS-callee diagnostic: {ufcs_error:?}"
        );
    }

    #[test]
    fn direct_fqns_remain_in_type_paths() {
        p("module main; \
             fn keep(value: provider/path.Value) -> provider/path.Value { value }");
    }

    #[test]
    fn callable_targets_reject_direct_fqns_uniformly() {
        for source in [
            "module main; op _ + _ { impl provider/path.add; };",
            "module main; varop [* *] { foldr cons provider/path.empty; };",
            "module main; varop [* *] { foldr provider/path.cons empty; };",
            "module main; varop [* *] { foldr cons empty; finalize provider/path.finish; };",
            "module main; elab inspect : . -> . { impl provider/path.inspect; };",
            "module main; elab inspect : . -> . { impl(fills) provider/path.inspect; };",
        ] {
            for error in [
                parse(source).expect_err("eager parser must reject a direct callable FQN"),
                parse_lazy(source).expect_err("lazy parser must reject a direct callable FQN"),
            ] {
                assert!(
                    error
                        .diag()
                        .1
                        .contains("slash-qualified item paths are not lexical callable targets"),
                    "unexpected diagnostic for {source:?}: {error:?}",
                );
                assert_eq!(
                    error.diagnostic().help(),
                    Some(
                        "import the item or its module, then use the local name or dotted module alias"
                    )
                );
            }
        }
    }

    #[test]
    fn capture_and_rec_loop_paths_use_imported_or_dotted_value_paths() {
        p("module main; \
           elab inspect : . -> . { captures runtime.helper; impl helper; }; \
           rec(runtime.loop) fn run() -> . { () }");

        assert!(
            parse(
                "module main; \
                 elab inspect : . -> . { captures provider/path.helper; impl helper; };"
            )
            .is_err(),
            "capture paths are dotted value paths, not direct item FQNs"
        );
        assert!(
            parse("module main; rec(provider/path.loop) fn run() -> . { () }").is_err(),
            "rec(loop) names an ordinary value path, not a direct item FQN"
        );
    }

    #[test]
    fn repl_forall_probe_uses_parser_context() {
        for source in [".[", ".[A", ".[ *F", "f([A"] {
            assert!(
                expression_has_unclosed_structural_forall(source, None),
                "expected an unclosed structural binder in {source:?}"
            );
        }
        for source in ["a [ B", "a [! b", "a [ b", "xs[0", "[", ".[! { [!1 }"] {
            assert!(
                !expression_has_unclosed_structural_forall(source, None),
                "ordinary bracket spelling became structural in {source:?}"
            );
        }

        let plus_context = local_expression_context(
            "module x; fn add(a: A, b: A) -> A { a } op _ + _ { impl add; };",
        );
        assert!(!expression_has_unclosed_structural_forall(
            "value + .[A",
            None,
        ));
        assert!(expression_has_unclosed_structural_forall(
            "value + .[A",
            Some(&plus_context),
        ));
    }

    #[test]
    fn expression_context_distinguishes_selected_and_unrelated_fixed_operators() {
        let local_prefix = local_expression_context(
            "module local; fn prefix(a: A) -> A { a } op <% _ { impl prefix; };",
        );
        assert!(p_expr_with_context("<% A", Some(&local_prefix)).is_ok());
        assert!(!expression_has_unclosed_structural_forall(
            "<% A",
            Some(&local_prefix),
        ));
        let no_context_call =
            probe_expression_fragment("f([ A", None).expect("unresolved call fragment probe");
        assert_eq!(no_context_call.structural_bracket_offsets, vec![2]);
        assert!(no_context_call.unclosed_structural_forall_at_eof);
        let prefix_call = probe_expression_fragment("f(<% A", Some(&local_prefix))
            .expect("prefix-operator call fragment probe");
        assert!(prefix_call.structural_bracket_offsets.is_empty());
        assert!(!prefix_call.unclosed_structural_forall_at_eof);

        let consumer = parse("module consumer; import syntax(op _ <% _);")
            .expect("consumer-declared consumer parse");
        let imported =
            expression_parse_context(&consumer).expect("imported expression parse context");
        assert!(p_expr_with_context("a <% B", Some(&imported)).is_ok());
        assert!(!expression_has_unclosed_structural_forall(
            "a <% B",
            Some(&imported),
        ));

        let unrelated = local_expression_context("module unrelated;");
        assert!(p_expr_with_context("a <% B", Some(&unrelated)).is_err());
        let unrelated_probe = probe_expression_fragment("a <% B", Some(&unrelated))
            .expect("unselected operator fragment probe");
        assert!(unrelated_probe.structural_bracket_offsets.is_empty());
        assert!(!unrelated_probe.unclosed_structural_forall_at_eof);
    }

    #[test]
    fn call_arg_forall_shape_is_independent_of_operator_grammar() {
        let no_operator = p_expr_with_context("consume([A] Poly)", None)
            .expect("polytype argument without an operator");
        let Expr::Call { args, .. } = no_operator else {
            panic!("expected call")
        };
        assert!(matches!(args.as_slice(), [CallArg::Type(_)]));

        let bracket = local_expression_context("module x; varop [* *] { foldr step base; };");
        let with_operator = p_expr_with_context("consume([A] Poly)", Some(&bracket))
            .expect("same spelling with local varop");
        let Expr::Call { args, .. } = with_operator else {
            panic!("expected call")
        };
        assert!(matches!(args.as_slice(), [CallArg::Type(_)]));
        let probe = probe_expression_fragment("consume([A] Poly)", Some(&bracket))
            .expect("forall call probe with local varop");
        assert_eq!(probe.structural_bracket_offsets, [8, 10]);
        assert!(!probe.unclosed_structural_forall_at_eof);

        let consumer =
            parse("module consumer; import syntax(varop [* *]);").expect("selective varop import");
        let imported =
            expression_parse_context(&consumer).expect("imported varop expression context");
        let imported_arg = p_expr_with_context("consume([A] Poly)", Some(&imported))
            .expect("same spelling with selectively imported varop");
        let Expr::Call { args, .. } = imported_arg else {
            panic!("expected imported-context call")
        };
        assert!(matches!(args.as_slice(), [CallArg::Type(_)]));
        let imported_probe = probe_expression_fragment("consume([A] Poly)", Some(&imported))
            .expect("forall call probe with imported varop");
        assert_eq!(imported_probe.structural_bracket_offsets, [8, 10]);
        assert!(!imported_probe.unclosed_structural_forall_at_eof);
        for source in [
            "module x; op [ _ ] _ { impl bracket; };",
            "module x; import syntax(op [ _ ] _);",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn expression_probe_retains_only_committed_forall_facts() {
        for source in [".[A](", ".[A](x"] {
            let probe = probe_expression_fragment(source, None).expect("fragment probe");
            assert_eq!(probe.structural_bracket_offsets, vec![1, 3], "{source}");
            assert!(!probe.unclosed_structural_forall_at_eof, "{source}");
        }
        for source in [".[A] {}", ".[A]foo", ".[! { [!1 }"] {
            let probe = probe_expression_fragment(source, None).expect("fragment probe");
            assert!(probe.structural_bracket_offsets.is_empty(), "{source}");
            assert!(!probe.unclosed_structural_forall_at_eof, "{source}");
        }
        assert!(p_expr_with_context(".[] { []1 }", None).is_err());

        let call = probe_expression_fragment("f([A", None).expect("call fragment probe");
        assert_eq!(call.structural_bracket_offsets, vec![2]);
        assert!(call.unclosed_structural_forall_at_eof);
        assert!(p_expr_with_context("f([A] A -> A)", None).is_ok());
    }

    #[test]
    fn lazy_module_parse_defers_function_body_errors_until_force() {
        let source = "\
module lazy;

pub fn bad() -> . { let x = ; () }
";
        let lazy = parse_lazy(source).expect("lazy header parse");
        let Item::FnDef(defn) = &lazy.module().items[0] else {
            panic!("expected fn item");
        };
        assert!(matches!(defn.body, Expr::Unit { .. }));
        let err = lazy.force_all().expect_err("body force should parse body");
        assert!(err.diag().1.contains("expected expression"));
    }

    #[test]
    fn lazy_module_force_matches_eager_parse_with_operator_body() {
        let source = "\
module lazy;

fn add(a: I32, b: I32) -> I32 { a }

op _ + _ { impl add; };

pub fn main() -> I32 { 1 + 2 }
";
        let eager = parse(source).expect("eager parse");
        let forced = parse_lazy(source)
            .expect("lazy header parse")
            .force_all()
            .expect("force bodies");
        assert_eq!(forced, eager);
    }

    #[test]
    fn module_operator_scope_rejects_later_declaration_eagerly() {
        let source = "\
module module_scope;

fn add(a: I32, b: I32) -> I32 { a }

fn before_declaration() -> I32 { 1 + 2 }

op _ + _ { impl add; };
";

        let error = parse(source).expect_err("a later operator is not in the body's scope");
        assert_eq!(error.diag().0.start as usize, source.find('+').unwrap());
    }

    #[test]
    fn module_operator_scope_rejects_later_declaration_when_forced() {
        let source = "module module_scope;\n\
            fn add(a: ., b: .) -> . { a }\n\
            fn before_declaration() -> . { () + () }\n\
            op _ + _ { impl add; };";
        let lazy = parse_lazy(source).expect("the unforced body remains deferred");
        let Item::FnDef(before) = &lazy.module().items[1] else {
            panic!("expected the earlier function");
        };
        assert!(matches!(before.body, Expr::Unit { .. }));
        let error = lazy
            .force_all()
            .expect_err("forcing preserves the body's source scope");
        assert_eq!(error.diag().0.start as usize, source.find('+').unwrap());
    }

    #[test]
    fn module_operator_scope_imported_grammar_preserves_source_order() {
        for body in [
            "fn before() -> . { (() * ()) + () }",
            "rec(loop) { fn before() -> . { (() * ()) + () } }",
        ] {
            let prefix = "module consumer;\n\
                import syntax(op _ * _);\n\
                fn combine(a: ., b: .) -> . { a }\n";
            let operator = "op _ + _ { impl combine; };\n";
            let source = format!("{prefix}{body}\n{operator}");
            let eager =
                parse(&source).expect_err("imported operators do not expose later local operators");
            let lazy = parse_lazy(&source).expect("header-only parse");
            let forced = lazy.force_all().expect_err("force source-ordered body");
            let expected = source.find('+').unwrap();
            assert_eq!(eager.diag().0.start as usize, expected);
            assert_eq!(forced.diag(), eager.diag());

            let preceding = format!("{prefix}{operator}{body}\n");
            let eager =
                parse(&preceding).expect("preceding local and imported operators are available");
            let lazy = parse_lazy(&preceding).expect("header-only preceding control");
            assert_eq!(lazy.force_all().expect("force preceding control"), eager);
            let formatted = crate::pretty::pretty_module(&eager);
            let reparsed = parse(&formatted).expect("formatted source preserves operator order");
            assert_eq!(crate::pretty::pretty_module(&reparsed), formatted);
        }
    }

    #[test]
    fn lazy_module_parse_accepts_signature_only_host_fn() {
        // A `host fn` is signature-only — it has no body to defer. The
        // lazy item dispatcher must not treat it as a body-bearing `fn`
        // (which would skip a `{ … }` / `->` body group that isn't there
        // and fail with "expected `->` or `{`"). The host modifier gates
        // the lazy fn-deferral, so the signature parses eagerly even in
        // lazy mode and round-trips against the eager parse.
        let source = "\
module host_lazy;

host fn foo(p0: String) -> .;
";
        let lazy = parse_lazy(source).expect("lazy header parse of host fn");
        assert!(
            matches!(lazy.module().items[0], Item::HostFn(_)),
            "lazy parse should classify a signature-only `host fn` as HostFn"
        );
        let forced = lazy.force_all().expect("force host-fn module bodies");
        let eager = parse(source).expect("eager parse of host fn");
        assert_eq!(forced, eager);
    }

    // ---- Module declaration ----------------------------------------------

    #[test]
    fn empty_module_declaration() {
        let m = p("module foo;");
        assert_eq!(m.path.segments, vec!["foo"]);
        assert!(m.imports.is_empty());
        assert!(m.items.is_empty());
    }

    #[test]
    fn parse_uses_per_item_rayon_fanout_for_multi_item_modules() {
        // A module with several fn_defs + a op. The parser
        // routes the op through the sequential pre-scan and
        // the fn_defs through rayon. The resulting `Module<Surface>`
        // must match a fully sequential parse — same item count,
        // same item kinds, same names, and the same FnDef body
        // shapes (variant discriminants compared, since the
        // parallel path's NodeIds use task-offset ranges and
        // would differ from a hypothetical purely-sequential
        // build).
        let source = "\
module sample;

fn helper(a: I32, b: I32) -> I32 { a }

op _ + _ { impl helper; };

fn user_one() -> . { let n = 1 + 2; () }

fn user_two() -> I32 { (1 + 2) + 3 }

pub fn main() -> . { () }
";
        let module = parse(source).expect("parse");
        // Five top-level items: helper, op, user_one,
        // user_two, main. The `+` op is the third item in
        // source order; user_one / user_two / main each consume
        // it. The parallel pipeline must preserve source order.
        assert_eq!(module.items.len(), 5);
        let item_kinds: Vec<&str> = module
            .items
            .iter()
            .map(|i| match i {
                Item::FnDef(_) => "fn",
                Item::RecGroup(_, _) => "rec",
                Item::TypeRecGroup(_) => "type_rec",
                Item::Op(_, _) => "op",
                Item::VariadicOperator(_, _) => "fold",
                Item::TypeAlias(_) => "type",
                Item::LiteralAlias(_, _) => "literal",
                Item::Newtype(_) => "newtype",
                Item::Labels(_, _) => "labels",
                Item::LabelForward(_, _) => "label_forward",
                Item::Equiv(_, _) => "equiv",
                Item::Elaborator(_, _) => "elaborator",
                Item::HostType(_) => "host_type",
                Item::HostFn(_) => "host_fn",
            })
            .collect();
        assert_eq!(item_kinds, vec!["fn", "op", "fn", "fn", "fn"]);
        let fn_def_names: Vec<&str> = module
            .items
            .iter()
            .filter_map(|i| match i {
                Item::FnDef(d) => Some(d.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(fn_def_names, vec!["helper", "user_one", "user_two", "main"]);
    }

    #[test]
    fn malformed_brace_body_yields_localized_parse_error() {
        // A fn body whose `{` has no matching `}` is captured by
        // the tree-skeleton builder as a `recovered = true` group.
        // The parser surfaces a parse error scoped to the group's
        // source range — not "expected `}` at EOF" cascading
        // through the rest of the file.
        let source = "module x;\nfn broken() -> . { (\nfn ok() -> . { () }";
        let err = parse(source).expect_err("parse should fail");
        let (span, msg) = err.diag();
        assert!(
            msg.contains("unterminated"),
            "expected `unterminated` in: {msg:?}"
        );
        // The error span is bounded by the malformed group — it
        // ends before the source ends (the source includes the
        // `fn ok` text past the broken body).
        assert!(
            span.start as usize >= source.find('{').unwrap(),
            "span starts at or after the brace: span = {span:?}"
        );
        assert!(
            err.exit_code() == crate::exit_code::ExitCode::Parse,
            "exit code is Parse"
        );
    }

    #[test]
    fn malformed_brace_body_in_package_style_module() {
        // Same case as above but with a package-qualified module
        // path (`module a/b/c/main`) and longer surrounding text.
        // Verifies the recovery error span is still anchored to
        // the malformed `{` and not to position 0.
        let source = "\
module parser_recovery_localized/main;

fn broken() -> . { (

fn ok() -> . { () }";
        let err = parse(source).expect_err("parse should fail");
        let (span, _msg) = err.diag();
        let brace = source.find('{').expect("source has a `{`");
        assert!(
            span.start as usize >= brace,
            "span starts at or after the malformed brace: span = {span:?}, brace at {brace}"
        );
    }

    #[test]
    fn malformed_brace_body_with_leading_comments() {
        // Same case but with line comments above the fn — the
        // golden uses this layout. Comments are lexical trivia, so
        // they should not affect parser recovery. Matches the
        // shape of `test-data/goldens/11_parse_error/parser_recovery_localized/src/main.kio`,
        // including the trailing newline.
        let source = "\
module parser_recovery_localized/main;

// `broken`'s body opens with `{` but never closes before the next
// top-level `fn`. The tree-skeleton builder marks the group as
// recovered and the parser surfaces a parse error scoped to that
// group's source range — not \"expected `}` at EOF\" cascading
// through the rest of the file.

fn broken() -> . { (

fn ok() -> . { () }
";
        let err = parse(source).expect_err("parse should fail");
        let (span, msg) = err.diag();
        let brace = source.find(". { (").expect("source has the broken brace");
        assert!(
            span.start as usize >= brace,
            "span starts at or after the malformed brace: span = {span:?}, msg = {msg:?}"
        );
    }

    #[test]
    fn nested_module_declaration() {
        let m = p("module a/b/c;");
        assert_eq!(m.path.segments, vec!["a", "b", "c"]);
    }

    #[test]
    fn module_declaration_required() {
        let msg = p_err("fn foo() -> . { () }");
        assert!(msg.contains("module"));
    }

    // ---- import statements --------------------------------------------------

    #[test]
    fn import_selective_single() {
        let m = p("module x; import a/b(foo);");
        let u = &m.imports[0];
        match &u.kind {
            ImportKind::Selective { items, from } => {
                let names: Vec<&str> = items
                    .iter()
                    .filter_map(crate::ast::ImportItem::as_name)
                    .collect();
                assert_eq!(names, vec!["foo"]);
                assert_eq!(from.segments, vec!["a", "b"]);
            }
            other => panic!("expected Selective, got {other:?}"),
        }
    }

    #[test]
    fn import_selective_multi() {
        let m = p("module x; import pkg/mod(foo, bar, baz);");
        match &m.imports[0].kind {
            ImportKind::Selective { items, .. } => {
                let names: Vec<&str> = items
                    .iter()
                    .filter_map(crate::ast::ImportItem::as_name)
                    .collect();
                assert_eq!(names, vec!["foo", "bar", "baz"]);
            }
            other => panic!("expected Selective, got {other:?}"),
        }
    }

    #[test]
    fn import_selective_namespaces_are_structurally_distinct() {
        let m = p("module x; import pkg/mod(item, Item, {item});");
        let items = selective_items(&m.imports[0]);
        assert!(matches!(&items[0], ImportItem::Name { name, .. } if name == "item"));
        assert!(matches!(&items[1], ImportItem::Name { name, .. } if name == "Item"));
        assert!(matches!(
            &items[2],
            ImportItem::Label { name, .. } if name == "item"
        ));
        assert_eq!(items[2].as_name(), None);
        assert_eq!(items[2].as_label().map(|(name, _)| name), Some("item"));
    }

    #[test]
    fn import_label_item_requires_a_label_name() {
        let msg = p_err("module x; import pkg/mod({Item});");
        assert!(
            msg.contains("label name `Item` must have a lowercase first letter"),
            "got: {msg}"
        );
    }

    #[test]
    fn import_qualified_multi_segment() {
        let m = p("module x; import a/b/c as m;");
        match &m.imports[0].kind {
            ImportKind::Qualified { path, alias, .. } => {
                assert_eq!(path.segments, vec!["a", "b", "c"]);
                assert_eq!(alias, "m");
            }
            other => panic!("expected Qualified, got {other:?}"),
        }
    }

    #[test]
    fn import_qualified_alias_requires_a_value_name() {
        let m = p("module x; import a/b as lower_alias;");
        assert!(matches!(
            &m.imports[0].kind,
            ImportKind::Qualified { alias, .. } if alias == "lower_alias"
        ));

        let msg = p_err("module x; import a/b as UpperAlias;");
        assert!(msg.contains("value name"), "got: {msg}");
    }

    #[test]
    fn import_qualified_single_segment() {
        let m = p("module x; import a as m;");
        match &m.imports[0].kind {
            ImportKind::Qualified { path, alias, .. } => {
                assert_eq!(path.segments, vec!["a"]);
                assert_eq!(alias, "m");
            }
            other => panic!("expected Qualified, got {other:?}"),
        }
    }

    #[test]
    fn import_fn_modifier_rejected() {
        let msg = p_err("module x; import elaborators(fn checked);");
        assert!(msg.contains("expected"), "got: {msg}");
    }

    #[test]
    fn import_intrinsics() {
        let m = p("module x; import __intrinsics__;");
        assert_eq!(m.imports[0].kind, ImportKind::Intrinsics);
    }

    // ---- import grammar items --------------------------------------------

    fn imported_grammar(item: &ImportItem) -> &OperatorGrammar {
        match item {
            ImportItem::OperatorPattern { grammar, .. } => grammar,
            other => panic!("expected OperatorPattern, got {other:?}"),
        }
    }

    fn selective_items(import: &Import) -> &[ImportItem] {
        match &import.kind {
            ImportKind::Selective { items, .. } => items,
            other => panic!("expected Selective, got {other:?}"),
        }
    }

    #[test]
    fn import_operator_items_retain_the_complete_grammar() {
        for grammar in [
            "op _ + _",
            "op _ + __",
            "op _ $ ___",
            "op + _",
            "op _ !",
            "op _ ? _ : __",
            "op _ <% _ %>",
            "op <% _ %>",
            "varop [* *]",
            "varop [! !]",
        ] {
            let module = p_header(&format!("module x; import syntax({grammar});"));
            let items = selective_items(&module.imports[0]);
            assert_eq!(items.len(), 1);
            assert_eq!(imported_grammar(&items[0]).render(), grammar);
        }
    }

    #[test]
    fn import_operator_projection_is_delimited_from_sibling_items() {
        let module = p_header(
            "module x; import syntax(Mytype, my_function, op _ + __, varop [* *], {field});",
        );
        let items = selective_items(&module.imports[0]);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].as_name(), Some("Mytype"));
        assert_eq!(items[1].as_name(), Some("my_function"));
        assert_eq!(imported_grammar(&items[2]).render(), "op _ + __");
        assert_eq!(imported_grammar(&items[3]).render(), "varop [* *]");
        assert_eq!(items[4].as_label().map(|(name, _)| name), Some("field"));
    }

    #[test]
    fn imported_prefix_operator_carries_fixed_tail() {
        let m = parse(
            "module x; \
             import syntax(op ? _ : _); \
             fn choose(a: A, b: A) -> A { ? a : b }",
        )
        .expect("consumer-declared parse");
        let Item::FnDef(def) = &m.items[0] else {
            panic!("expected function definition");
        };
        let Expr::OpChain {
            kind: OpChainKind::Normal { pattern, slots },
            ..
        } = &def.body
        else {
            panic!("expected fixed operator chain");
        };
        assert_eq!(pattern.len(), 4);
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn imported_variadic_operator_contains_ordinary_operator_elements() {
        let m = parse(
            "module x; \
             import syntax(varop [% %], op _ => _); \
             fn pairs(k1: A, v1: A, k2: A, v2: A) -> A { \
               [% k1 => v1, k2 => v2 %] \
             }",
        )
        .expect("consumer-declared parse");
        let Item::FnDef(def) = &m.items[0] else {
            panic!("expected function definition");
        };
        let Expr::OpChain {
            kind:
                OpChainKind::Variadic {
                    close_tokens,
                    elements,
                    ..
                },
            ..
        } = &def.body
        else {
            panic!("expected variadic operator chain");
        };
        assert_eq!(close_tokens, &["%]".to_owned()]);
        assert_eq!(elements.len(), 2);
        for element in elements {
            let Expr::OpChain {
                kind: OpChainKind::Normal { pattern, slots },
                ..
            } = element
            else {
                panic!("ordinary operator element");
            };
            assert_eq!(slots.len(), 2);
            assert!(matches!(&pattern[1], OpPart::Token { content, .. } if content == "=>"));
        }
    }

    #[test]
    fn variadic_declaration_is_complete_in_lazy_parse() {
        let source = "module x; \
             pub varop [* *] { foldr step zero; };";
        let eager = parse(source).expect("eager parse");
        let lazy = parse_lazy(source).expect("lazy header parse");
        assert_eq!(lazy.module(), &eager);
        let forced = lazy.force_all().expect("force deferred function bodies");
        assert_eq!(forced, eager);
    }

    #[test]
    fn imported_operator_is_available_in_deferred_rec_member() {
        let source = "module x; \
             import syntax(op ? _ : _); \
             rec(loop) { \
               fn choose(a: A, b: A) -> A { ? a : b }; \
               fn keep(a: A) -> A { a } \
             }";
        let eager = parse(source).expect("consumer-declared parse");
        let lazy = parse_lazy(source).expect("lazy header parse");
        let forced = lazy.force_all().expect("force recursive member body");
        assert_eq!(forced, eager);
    }

    #[test]
    fn import_nullary_fixed_grammar_is_admitted() {
        let module = p_header("module x; import syntax(op ++);");
        assert_eq!(
            imported_grammar(&selective_items(&module.imports[0])[0]).render(),
            "op ++"
        );
    }

    #[test]
    fn import_operator_leading_dot_needs_a_second_dot() {
        let message = p_err("module x; import syntax(op _ .+ _);");
        assert!(
            message.contains("starts with `.`") || message.contains("must contain at least two"),
            "unexpected: {message}"
        );
        let module = p_header("module x; import syntax(op _ .+. _);");
        assert_eq!(
            imported_grammar(&selective_items(&module.imports[0])[0]).render(),
            "op _ .+. _"
        );
    }

    // ---- fn ------------------------------------------------------------

    #[test]
    fn fn_def_zero_arg_unit() {
        let m = p("module x; fn main() -> . { () }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert!(!d.vis.is_pub());
                assert_eq!(d.name, "main");
                assert!(d.sig.params.is_empty());
                assert!(matches!(d.ret, Type::Unit { .. }));
                assert!(!d.ret_elided);
                assert!(matches!(d.body, Expr::Unit { .. }));
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_unit_return_elided() {
        // language.md § Function definitions: `-> .` may be omitted.
        // The parser synthesizes Type::Unit and records `ret_elided`
        // so the formatter can round-trip the original shape.
        let m = p("module x; fn say_hi() { () }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert_eq!(d.name, "say_hi");
                assert!(matches!(d.ret, Type::Unit { .. }));
                assert!(d.ret_elided);
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_explicit_return_keeps_elided_false() {
        // The dual: an explicit `-> .` annotation produces the same
        // synthesized Type::Unit but with `ret_elided = false`.
        let m = p("module x; fn say_hi() -> . { () }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert!(matches!(d.ret, Type::Unit { .. }));
                assert!(!d.ret_elided);
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_non_unit_return_must_be_explicit() {
        // Only `-> .` is elidable — a non-unit fn that omits the
        // arrow falls through to the body's `{` and the parser
        // accepts it; the typer (rejected at synth time) is what
        // catches mis-elision. At parse alone, the body is read as
        // `String` (a path) and then `{ ... }`, which is a parse error.
        let msg = p_err("module x; fn greet() String { \"hi\" }");
        assert!(msg.contains("`->` or `{`") || msg.contains("`{`"));
    }

    #[test]
    fn fn_def_pub_visibility() {
        let m = p("module x; pub fn main() -> . { () }");
        match &m.items[0] {
            Item::FnDef(d) => assert!(d.vis.is_pub()),
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_identity_function() {
        let m = p("module x; pub fn id[A](x: A) -> A { x }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert_eq!(d.sig.params.len(), 2);
                match &d.sig.params[0] {
                    SignatureParam::Type(tp) => assert_eq!(tp.name, "A"),
                    other => panic!("expected type param, got {other:?}"),
                }
                match &d.sig.params[1] {
                    SignatureParam::Value(p) => {
                        assert_eq!(p.name, "x");
                        match p.ty.as_ref() {
                            Some(Type::Path { segments, args, .. }) => {
                                assert_eq!(segments, &vec!["A".to_string()]);
                                assert!(args.is_empty());
                            }
                            other => panic!("expected annotated Path, got {other:?}"),
                        }
                    }
                    other => panic!("expected value param, got {other:?}"),
                }
                match &d.ret {
                    Type::Path { segments, .. } => assert_eq!(segments, &vec!["A".to_string()]),
                    other => panic!("expected Path, got {other:?}"),
                }
                match &d.body {
                    Expr::Path { segments, .. } => assert_eq!(segments, &vec!["x".to_string()]),
                    other => panic!("expected Path, got {other:?}"),
                }
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_intermixed_params_preserve_order() {
        let m = p("module x; fn f[A](x: A)[B](y: B) -> A { x }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.sig.params[0], SignatureParam::Type(_)));
            assert!(matches!(d.sig.params[1], SignatureParam::Value(_)));
            assert!(matches!(d.sig.params[2], SignatureParam::Type(_)));
            assert!(matches!(d.sig.params[3], SignatureParam::Value(_)));
        }
    }

    #[test]
    fn fn_def_universal_binder_bracket_spelling() {
        // `[A]` is the spelling for universal binder groups.
        let m = p("module x; pub fn id[A](x: A) -> A { x }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert_eq!(d.sig.params.len(), 2);
                match &d.sig.params[0] {
                    SignatureParam::Type(tp) => assert_eq!(tp.name, "A"),
                    other => panic!("expected type param, got {other:?}"),
                }
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn parenthesized_type_param_in_fn_signature_rejected() {
        let err = p_err("module x; fn id([A], x: A) -> A { x }");
        assert!(
            err.contains("expected identifier") || err.contains("expected `,` or `)`"),
            "parenthesized type parameter must not parse in a fn value group, got: {err}"
        );
    }

    #[test]
    fn fn_comma_binder_group_accepts_shorthand() {
        let m = p("module x; fn pair[A, B](x: A, y: B) -> A { x }");
        match &m.items[0] {
            Item::FnDef(d) => {
                assert!(matches!(&d.sig.params[0], SignatureParam::Type(tp) if tp.name == "A"));
                assert!(matches!(&d.sig.params[1], SignatureParam::Type(tp) if tp.name == "B"));
                assert!(matches!(&d.sig.params[2], SignatureParam::Value(_)));
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_universal_binder_angle_form_rejected() {
        // Universal binders are `[A]`-shaped; `<A>` is reserved for
        // existentials in their paren-wrapped position.
        let err = p_err("module x; fn id<U>(x: U) -> U { x }");
        assert!(
            err.contains("expected a signature parameter group"),
            "want bracket complaint, got: {err}"
        );
    }

    #[test]
    fn fn_def_mismatched_binder_brackets_is_parse_error() {
        // `[A>` mismatches the bracket opener with the angle closer —
        // the type-param group reports the missing `]`.
        let err = p_err("module x; fn id[A>(x: A) -> A { x }");
        assert!(err.contains("]"), "want `]` complaint, got: {err}");
    }

    #[test]
    fn newtype_universal_binder_bracket_spelling() {
        let m = p("module x; newtype Box[A] : A { constructor mk_box; pub projector un_box; };");
        match &m.items[0] {
            Item::Newtype(d) => {
                assert_eq!(d.type_params.len(), 1);
                assert_eq!(d.type_params[0].name, "A");
            }
            other => panic!("expected Newtype, got {other:?}"),
        }
    }

    #[test]
    fn parenthesized_type_param_in_newtype_header_rejected() {
        let err =
            p_err("module x; newtype Box([A]) : A { constructor mk_box; projector un_box; };");
        assert!(
            err.contains("expected `:`") || err.contains("expected type expression"),
            "parenthesized type parameter must not parse in a newtype header, got: {err}"
        );
    }

    #[test]
    fn newtype_comma_binder_group_accepts_shorthand() {
        let m = p("module x; newtype Box[A, B] : A { constructor mk_box; projector un_box; };");
        match &m.items[0] {
            Item::Newtype(d) => {
                assert_eq!(d.type_params.len(), 2);
                assert_eq!(d.type_params[0].name, "A");
                assert_eq!(d.type_params[1].name, "B");
            }
            other => panic!("expected Newtype, got {other:?}"),
        }
    }

    #[test]
    fn type_alias_universal_binder_bracket_spelling() {
        let m = p("module x; type Same[A] = A;");
        match &m.items[0] {
            Item::TypeAlias(t) => {
                assert_eq!(t.type_params.len(), 1);
                assert_eq!(t.type_params[0].name, "A");
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn parenthesized_type_param_in_type_alias_header_rejected() {
        let err = p_err("module x; type Same([A]) = A;");
        assert!(
            err.contains("expected `=`") || err.contains("expected type expression"),
            "parenthesized type parameter must not parse in a type-alias header, got: {err}"
        );
    }

    #[test]
    fn type_alias_comma_binder_group_accepts_shorthand() {
        let m = p("module x; type Pair[A, B] = A & B;");
        match &m.items[0] {
            Item::TypeAlias(t) => {
                assert_eq!(t.type_params.len(), 2);
                assert_eq!(t.type_params[0].name, "A");
                assert_eq!(t.type_params[1].name, "B");
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn kind_annotated_binders_parse_stars_to_kinds() {
        // `[A]` ⇒ kind `*` (None), `[*F]` ⇒ kind `*→*`, `[**G]` ⇒
        // kind `*→*→*`. See `specs/grammar.md` § Kind grammar.
        let m = p("module x; fn higher[A][*F][**G]( x: A) -> A { x }");
        match &m.items[0] {
            Item::FnDef(d) => {
                let tps: Vec<&TypeParam> = d
                    .sig
                    .params
                    .iter()
                    .filter_map(|p| match p {
                        SignatureParam::Type(tp) => Some(tp),
                        _ => None,
                    })
                    .collect();
                assert_eq!(tps.len(), 3);
                assert_eq!(tps[0].name, "A");
                assert_eq!(tps[0].kind, None);
                assert_eq!(tps[1].name, "F");
                assert_eq!(tps[1].kind, Some(Kind::arrow_chain(1)));
                assert_eq!(tps[2].name, "G");
                assert_eq!(tps[2].kind, Some(Kind::arrow_chain(2)));
            }
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn compact_forall_brackets_peel_from_maximal_runs() {
        p("module x; fn higher[A][*F][**G](x: A) -> A { x }");
        p("module x; type Nested = [A][B] A -> B;");
        p("module x; fn lambda() -> . { .[*F](x: F(.)) -> F(.) { x } }");
    }

    #[test]
    fn malformed_or_nested_forall_binders_stay_rejected() {
        for source in [
            "module x; fn missing[A(x: A) -> A { x }",
            "module x; fn nameless[*](x: .) -> . { x }",
            "module x; fn nested[[A]](x: A) -> A { x }",
        ] {
            let message = p_err(source);
            assert!(
                !message.is_empty(),
                "malformed binder unexpectedly parsed: {source}"
            );
        }
    }

    #[test]
    fn kind_annotated_binders_round_trip_through_fmt() {
        // The pretty-printer re-emits the star annotation, so a kinded
        // newtype header survives a parse → print → parse round-trip.
        let src = "module x; newtype Either[*E][**A] : A { constructor mk; pub projector un; };";
        let m = p(src);
        match &m.items[0] {
            Item::Newtype(d) => {
                assert_eq!(d.type_params[0].kind, Some(Kind::arrow_chain(1)));
                assert_eq!(d.type_params[1].kind, Some(Kind::arrow_chain(2)));
            }
            other => panic!("expected Newtype, got {other:?}"),
        }
    }

    #[test]
    fn function_type_polymorphic_bracket_spelling() {
        // `[A] A -> A` as a function-type expression in a type.
        let m = p("module x; type Id = [A] A -> A;");
        match &m.items[0] {
            Item::TypeAlias(t) => match t.type_body() {
                Type::Forall { param, .. } => {
                    assert_eq!(param.name, "A");
                }
                other => panic!("expected Forall, got {other:?}"),
            },
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn type_alias_standalone_existential_rejected() {
        // Standalone existential type expressions are no longer
        // admissible; the parser rejects `(<U>, body)` with a
        // diagnostic pointing at the newtype-header alternative.
        let msg = p_err("module x; type Pack[A] = (<U>, (A & U));");
        assert!(
            msg.contains("existential type expressions are no longer admissible")
                || msg.contains("declared on a `newtype` header"),
            "want existential-rejection diagnostic, got: {msg}"
        );
    }

    #[test]
    fn fn_def_call_in_body() {
        let m = p(r#"module x; pub fn main() -> . { print("Hello, world!") }"#);
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, args, .. } => {
                    assert!(matches!(**callee, Expr::Path { .. }));
                    assert_eq!(args.len(), 1);
                    assert!(matches!(args[0], CallArg::Value(Expr::StrLit { .. })));
                }
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn rec_single_fn_shorthand_parses() {
        let m = p("module x; rec(loop) pub fn f[A](x: A) -> A { rec f(String, x) }");
        let Item::RecGroup(group, _) = &m.items[0] else {
            panic!("expected RecGroup, got {:?}", m.items[0]);
        };
        assert_eq!(
            group
                .loop_path
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            vec!["loop"]
        );
        assert_eq!(group.members.len(), 1);
        let member = &group.members[0];
        assert!(member.vis.is_pub());
        assert_eq!(member.name, "f");
        let Expr::RecCall { callee, args, .. } = &member.body else {
            panic!("expected rec call body, got {:?}", member.body);
        };
        assert_eq!(callee.name, "f");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn recursive_data_singleton_spellings_parse() {
        for source in [
            "module x; rec newtype List[A] : . | (A & List(A)) { constructor mk_list; projector un_list; };",
            "module x; rec labels Tree = { leaf: I32 } | { branch: Tree & Tree };",
        ] {
            parse(source).unwrap_or_else(|error| {
                panic!("recursive data declaration failed to parse for {source:?}: {error:?}")
            });
        }
    }

    #[test]
    fn mutual_recursive_type_group_parses() {
        let source = "module x; rec { newtype A : . | B { constructor mk_a; projector un_a; }; newtype B : . | A { constructor mk_b; projector un_b; }; }";
        parse(source)
            .unwrap_or_else(|error| panic!("recursive type group failed to parse: {error:?}"));
    }

    #[test]
    fn type_declaration_editable_ranges_own_only_attached_prefixes() {
        let source = concat!(
            "module x ;\n",
            "// Ambiguous attachment.\n",
            "type A = .;\n",
            "/// B docs.\n",
            "pub type B = .;\n",
        );
        let module = parse(source).expect("type declarations");
        let Item::TypeAlias(a) = &module.items[0] else {
            panic!("expected A")
        };
        assert_eq!(a.editable_span, None);

        let Item::TypeAlias(b) = &module.items[1] else {
            panic!("expected B")
        };
        let editable = b
            .editable_span
            .expect("attached docs are declaration-owned");
        assert_eq!(
            &source[editable.start as usize..editable.end as usize],
            "/// B docs.\npub type B = .;"
        );
    }

    #[test]
    fn recursive_type_group_retains_prefix_owned_source_layout() {
        let source = concat!(
            "module x; rec {\n",
            "  // group-leading context\n",
            "  /// Alias docs.\n",
            "  pub type A = B;\n",
            "  /// Nominal docs.\n",
            "  pub(x) newtype B : A { constructor mk; projector un; };\n",
            "  // group-trailing context\n",
            "}",
        );
        let module = parse(source).expect("recursive group");
        let Item::TypeRecGroup(group) = &module.items[0] else {
            panic!("expected recursive type group")
        };
        let layout = group.source_layout.as_ref().expect("written source layout");
        assert_eq!(layout.member_spans.len(), 2);
        assert_eq!(layout.member_marker_offsets.len(), 2);
        assert!(
            source[layout.member_spans[0].start as usize..layout.member_spans[0].end as usize]
                .starts_with("// group-leading context")
        );
        assert!(
            source[layout.member_spans[1].start as usize..layout.member_spans[1].end as usize]
                .starts_with("/// Nominal docs.")
        );
        assert_eq!(
            &source[layout.member_marker_offsets[1] as usize..][.."newtype".len()],
            "newtype"
        );
        let trailing = layout
            .trailing_comment_span
            .expect("trailing group comment");
        assert_eq!(
            &source[trailing.start as usize..trailing.end as usize],
            "// group-trailing context"
        );
    }

    #[test]
    fn bare_type_group_leading_visibility_highlights_the_complete_modifier() {
        let source = "module x/y; pub(x) rec { type A = B; newtype B : A { constructor mk; projector un; }; }";
        let error = parse(source).expect_err("a bare type group has member visibility only");
        let (span, message) = error.diag();
        assert_eq!(
            message,
            "a bare `rec { ... }` type group has no leading visibility"
        );
        assert_eq!(&source[span.start as usize..span.end as usize], "pub(x)");
        let [fix] = error.diagnostic().fixes() else {
            panic!("complete private members admit one visibility-distribution fix")
        };
        assert_eq!(fix.title, "Put visibility on each recursive type member");
        assert_eq!(fix.edits.len(), 3);
    }

    #[test]
    fn rec_mutual_group_parses_member_visibility() {
        let m = p(
            "module x; rec(loop) { pub fn even(n: Bool) -> Bool { rec odd(n) }; fn odd(n: Bool) -> Bool { rec even(n) } }",
        );
        let Item::RecGroup(group, _) = &m.items[0] else {
            panic!("expected RecGroup, got {:?}", m.items[0]);
        };
        assert_eq!(group.members.len(), 2);
        assert!(group.members[0].vis.is_pub());
        assert_eq!(group.members[0].name, "even");
        assert!(!group.members[1].vis.is_pub());
        assert_eq!(group.members[1].name, "odd");
    }

    #[test]
    fn rec_prefix_accepts_flexible_visibility_order() {
        for src in [
            "module x; pub rec(loop) fn f() -> . { () }",
            "module x; rec(loop) pub fn f() -> . { () }",
        ] {
            let m = p(src);
            let Item::RecGroup(group, _) = &m.items[0] else {
                panic!("expected RecGroup for {src:?}, got {:?}", m.items[0]);
            };
            assert_eq!(group.members.len(), 1);
            assert!(group.members[0].vis.is_pub());
        }

        for src in [
            "module x/y; pub(x) rec(loop) fn f() -> . { () }",
            "module x/y; rec(loop) pub(x) fn f() -> . { () }",
        ] {
            let m = p(src);
            let Item::RecGroup(group, _) = &m.items[0] else {
                panic!("expected RecGroup for {src:?}, got {:?}", m.items[0]);
            };
            assert!(matches!(
                &group.members[0].vis,
                crate::ast::Visibility::PublicIn(path) if path.segments == ["x"]
            ));
        }
    }

    #[test]
    fn braced_rec_group_rejects_leading_visibility() {
        for src in [
            "module x; pub rec(loop) { fn f() -> . { () } }",
            "module x/y; pub(x) rec(loop) { fn f() -> . { () } }",
        ] {
            let message = p_err(src);
            assert!(
                message.contains("group has no leading visibility"),
                "unexpected error for {src:?}: {message}"
            );
        }
    }

    #[test]
    fn pure_modifier_accepts_flexible_visibility_order() {
        for src in [
            "module x; pub pure fn f() -> . { () }",
            "module x; pure pub fn f() -> . { () }",
        ] {
            let m = p(src);
            let Item::FnDef(d) = &m.items[0] else {
                panic!("expected FnDef for {src:?}, got {:?}", m.items[0]);
            };
            assert!(d.vis.is_pub());
            assert!(d.purity.is_pure());
        }
    }

    #[test]
    fn pure_modifier_rejects_unsupported_items() {
        for src in [
            "module x; pure host type T;",
            "module x; pure host fn f() -> .;",
            "module x; pure type T = .;",
            "module x; pure newtype T : . { constructor mk; projector un; };",
            "module x; pure labels { field : . };",
            "module x; pure elab e : [S] S -> [T] T { impl f; };",
            "module x; pure op + _ { impl f; };",
            "module x; pure varop [* *] { foldr g f; };",
            "module x; pure literal n = 1;",
            "module x; pure equiv same() { (); () }",
            "module x; pub pure rec(loop) fn f() -> . { () }",
            "module x; pub rec(loop) pure fn f() -> . { () }",
            "module x; pure rec(loop) fn f() -> . { () }",
            "module x; pure rec(loop) pub fn f() -> . { () }",
            "module x; rec(loop) pure pub fn f() -> . { () }",
            "module x; rec(loop) pure fn f() -> . { () }",
            "module x; rec(loop) pub pure fn f() -> . { () }",
            "module x; pure rec(loop) { fn f() -> . { () } }",
            "module x; rec(loop) { pure fn f() -> . { () } }",
            "module x; rec(loop) { pub pure fn f() -> . { () } }",
        ] {
            let msg = p_err(src);
            assert!(msg.contains("pure"), "unexpected error for {src:?}: {msg}");
        }
    }

    #[test]
    fn pure_rec_diagnostic_explains_required_loop_call() {
        for source in [
            "module x; pure rec(loop) fn f() -> . { () }",
            "module x; rec(loop) { pure fn f() -> . { () } }",
        ] {
            let message = p_err(source);
            assert!(
                message.contains(
                    "every recursive member's execution requires the group's declared `loop` function"
                ),
                "got: {message}"
            );
        }
    }

    #[test]
    fn pure_host_fn_diagnostic_explains_no_host_call_contract() {
        let message = p_err("module x; pure host fn effect() -> .;");
        assert!(
            message.contains("`pure` means that a function does not call host functions"),
            "got: {message}"
        );
    }

    #[test]
    fn elaborator_item_accepts_colon_type_declaration() {
        let m =
            p("module x; pub elab demo : [Source] Source -> [Target] Target { impl demo_impl; };");
        let Item::Elaborator(elaborator, _) = &m.items[0] else {
            panic!("expected elaborator item, got {:?}", m.items[0]);
        };
        assert_eq!(elaborator.name, "demo");
        assert!(elaborator.captures.is_empty());
        assert_eq!(
            elaborator
                .implementation
                .segments()
                .iter()
                .map(PathSegment::as_str)
                .collect::<Vec<_>>(),
            ["demo_impl"]
        );
    }

    #[test]
    fn elaborator_fills_schedule_uses_exact_marker_and_lexical_path() {
        for (source, expected_schedule) in [
            (
                "module x; elab demo : . -> . { impl fills; };",
                crate::ast::ElaboratorSchedule::Late,
            ),
            (
                "module x; elab demo : . -> . { impl(fills) helper.run; };",
                crate::ast::ElaboratorSchedule::Fills,
            ),
            (
                "module x; elab demo : . -> . { impl ( fills ) helper.Type.run; };",
                crate::ast::ElaboratorSchedule::Fills,
            ),
        ] {
            let eager = p(source);
            let Item::Elaborator(elaborator, _) = &eager.items[0] else {
                panic!("expected elaborator item")
            };
            assert_eq!(elaborator.schedule, expected_schedule);
            assert!(!elaborator.implementation.is_empty());
            let lazy = parse_lazy(source).expect("lazy elaborator header");
            assert_eq!(lazy.module(), &eager);
            assert_eq!(lazy.force_all().expect("force module"), eager);
        }

        for source in [
            "module x; elab demo : . -> . { impl .(value: .) -> . { value }; };",
            "module x; elab demo : . -> . { impl(fills)(x); };",
            "module x; elab demo : . -> . { impl(fills) .(value: .) -> . { value }; };",
        ] {
            for error in [
                parse(source).expect_err("eager parsing must reject implementation expressions"),
                parse_lazy(source)
                    .expect_err("lazy parsing must reject implementation expressions"),
            ] {
                assert!(
                    error
                        .diag()
                        .1
                        .contains("elaborator implementations must name a lexical callable"),
                    "unexpected diagnostic for {source:?}: {error:?}",
                );
                assert_eq!(
                    error.diagnostic().help(),
                    Some("move any reordering or wrapping logic into a named `pure fn`")
                );
            }
        }

        let direct_fqn = "module x; elab demo : . -> . { impl helpers/core.run; };";
        for error in [
            parse(direct_fqn).expect_err("eager parsing must reject a direct callable FQN"),
            parse_lazy(direct_fqn).expect_err("lazy parsing must reject a direct callable FQN"),
        ] {
            assert!(
                error
                    .diag()
                    .1
                    .contains("slash-qualified item paths are not lexical callable targets"),
                "unexpected direct-FQN diagnostic: {error:?}",
            );
        }
    }

    #[test]
    fn elaborator_item_parses_captures() {
        let m = p(
            "module x; pub elab type_of : [T] . -> Type_rep { captures (Type_rep, Type_rep.mk_type_rep, helper); impl type_of_impl; };",
        );
        let Item::Elaborator(elaborator, _) = &m.items[0] else {
            panic!("expected elaborator item, got {:?}", m.items[0]);
        };
        let captures: Vec<Vec<&str>> = elaborator
            .captures
            .iter()
            .map(|capture| {
                capture
                    .segments
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect()
            })
            .collect();
        assert_eq!(
            captures,
            vec![
                vec!["Type_rep"],
                vec!["Type_rep", "mk_type_rep"],
                vec!["helper"],
            ]
        );
    }

    #[test]
    fn fn_def_name_must_be_lowercase() {
        let msg = p_err("module x; fn Foo() -> . { () }");
        assert!(msg.contains("value name"));
    }

    #[test]
    fn fn_def_name_double_underscore_rejected() {
        let msg = p_err("module x; fn __secret() -> . { () }");
        assert!(msg.contains("`__`"));
    }

    // ---- aliases ----------------------------------------------------

    #[test]
    fn type_alias_nullary() {
        let m = p("module x; type Logger = String -> .;");
        match &m.items[0] {
            Item::TypeAlias(t) => {
                assert_eq!(t.name, "Logger");
                assert!(t.type_params.is_empty());
                assert!(matches!(t.type_body(), Type::Function { .. }));
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn type_alias_parametric_pub() {
        let m = p("module x; pub type Pair[A][B] = (A & B);");
        match &m.items[0] {
            Item::TypeAlias(t) => {
                assert!(t.vis.is_pub());
                assert_eq!(t.type_params.len(), 2);
                assert!(matches!(t.type_body(), Type::Product { .. }));
            }
            other => panic!("expected a type alias, got {other:?}"),
        }
    }

    #[test]
    fn literal_alias_preserves_exact_visibility_in_both_parse_paths_and_formatting() {
        for prefix in ["", "pub ", "pub(api) ", "pub(api/inner) ", "pub(other) "] {
            let source = format!("module api/inner; {prefix}literal value = 7;");
            for module in [p(&source), p_header(&source)] {
                let Item::LiteralAlias(literal, _) = &module.items[0] else {
                    panic!("the input declares a literal alias");
                };
                match (prefix, &literal.vis) {
                    ("", Visibility::Private) | ("pub ", Visibility::Public) => {}
                    (prefix, Visibility::PublicIn(path)) => {
                        assert_eq!(format!("pub({}) ", path.segments.join("/")), prefix);
                    }
                    _ => panic!("{prefix:?} became {:?}", literal.vis),
                }
                assert_eq!(literal.vis.is_exported(), prefix == "pub ");
                let formatted = crate::pretty::pretty_module(&module);
                assert!(
                    formatted.contains(&format!("{prefix}literal value = 7;")),
                    "{formatted}"
                );
            }
        }
    }

    #[test]
    fn literal_alias_accepts_bare_literal() {
        let m = p("module x; literal logger = \"hi\";");
        let Item::LiteralAlias(lit, _) = &m.items[0] else {
            panic!("expected literal, got {:?}", m.items[0]);
        };
        assert_eq!(lit.name, "logger");
        assert!(matches!(
            lit.value,
            crate::ast::LiteralAliasValue::Str { .. }
        ));
    }

    #[test]
    fn literal_alias_rejects_expression_body() {
        let msg = p_err("module x; literal logger = ();");
        assert!(
            msg.contains("expected a bare literal token"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn literal_alias_rejects_annotation() {
        let msg = p_err("module x; literal logger = \"hi\"(String);");
        assert!(msg.contains("`;`"), "unexpected: {msg}");
    }

    // ---- newtype ---------------------------------------------------------

    #[test]
    fn newtype_nullary() {
        let m = p(
            "module x; newtype Celsius : . { pub constructor mk_celsius; pub projector to_unit; };",
        );
        match &m.items[0] {
            Item::Newtype(d) => {
                assert_eq!(d.name, "Celsius");
                assert!(matches!(d.payload, Type::Unit { .. }));
                assert_eq!(d.constructor.name, "mk_celsius");
                assert!(d.constructor.vis.is_pub());
                assert_eq!(d.projector.name, "to_unit");
                assert!(d.projector.vis.is_pub());
            }
            other => panic!("expected Newtype, got {other:?}"),
        }
    }

    #[test]
    fn newtype_parametric_with_rec() {
        let m = p("module x; rec newtype List[A] : (. | (A & List(A))) { \
             pub constructor cons; pub projector un_list; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.name, "List");
            assert!(d.rec_span.is_some());
            assert_eq!(d.type_params.len(), 1);
            // Payload is a sum of unit and a product whose right is
            // a Path reference to List.
            match &d.payload {
                Type::Sum { left, right, .. } => {
                    assert!(matches!(**left, Type::Unit { .. }));
                    if let Type::Product { right: rr, .. } = right.as_ref() {
                        match rr.as_ref() {
                            Type::Path { segments, args, .. } => {
                                assert_eq!(segments, &vec!["List".to_string()]);
                                assert_eq!(args.len(), 1);
                            }
                            other => panic!("expected Path on the right of Product, got {other:?}"),
                        }
                    } else {
                        panic!("expected Product on the right of Sum");
                    }
                }
                other => panic!("expected Sum payload, got {other:?}"),
            }
        }
    }

    #[test]
    fn newtype_member_order_swappable() {
        let m = p("module x; newtype Foo : . { pub projector un_foo; pub constructor mk_foo; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.constructor.name, "mk_foo");
            assert_eq!(d.projector.name, "un_foo");
        }
    }

    #[test]
    fn newtype_member_visibility_must_lead_keyword() {
        let msg = p_err("module x; newtype Foo : . { constructor pub mk_foo; projector un_foo; };");
        assert!(
            msg.contains("write `pub constructor mk`") && msg.contains("not `constructor pub mk`"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn newtype_member_comma_terminator_rejected() {
        let msg =
            p_err("module x; newtype Foo : . { pub constructor mk_foo, pub projector un_foo; };");
        assert!(
            msg.contains("`;` between block entries"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn newtype_missing_member_errors() {
        let msg = p_err("module x; newtype Foo : . { pub constructor mk_foo; };");
        assert!(msg.contains("projector"));
    }

    #[test]
    fn newtype_with_existentials_one_binder() {
        // Existential binders trail the universal-parameter list on
        // the newtype header: `newtype Pack[A] <U> : A & U`.
        let m = p("module x; newtype Pack[A] <U> : A & U { \
             constructor mk_pack; projector un_pack; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.name, "Pack");
            assert_eq!(d.type_params.len(), 1);
            assert_eq!(d.type_params[0].name, "A");
            assert_eq!(d.existential_params.len(), 1);
            assert_eq!(d.existential_params[0].name, "U");
        } else {
            panic!("expected Newtype");
        }
    }

    #[test]
    fn newtype_with_existentials_multiple_binders() {
        let m = p("module x; newtype Pack[A] <L> <R> : A & L & R { \
             constructor mk_pack; projector un_pack; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.existential_params.len(), 2);
            assert_eq!(d.existential_params[0].name, "L");
            assert_eq!(d.existential_params[1].name, "R");
        } else {
            panic!("expected Newtype");
        }
    }

    #[test]
    fn newtype_with_existentials_no_universals() {
        // Existentials-only header: `Pack <U> : U`.
        let m = p("module x; newtype Pack <U> : U { constructor mk_pack; projector un_pack; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert!(d.type_params.is_empty());
            assert_eq!(d.existential_params.len(), 1);
            assert_eq!(d.existential_params[0].name, "U");
        } else {
            panic!("expected Newtype");
        }
    }

    #[test]
    fn newtype_existential_in_payload_is_parse_error() {
        // Standalone existential type expressions no longer parse at
        // all, and the diagnostic points at the newtype-header
        // alternative.
        let msg = p_err(
            "module x; newtype Pack[A] : (<U>, A & U) { constructor mk_pack; projector un_pack; };",
        );
        assert!(
            msg.contains("existential type expressions are no longer admissible")
                || msg.contains("declared on a `newtype` header"),
            "want existential-rejection diagnostic, got: {msg}"
        );
    }

    #[test]
    fn newtype_no_existentials_keeps_field_empty() {
        let m = p("module x; newtype Foo[A] : A { constructor mk_foo; projector un_foo; };");
        if let Item::Newtype(d) = &m.items[0] {
            assert!(d.existential_params.is_empty());
        } else {
            panic!("expected Newtype");
        }
    }

    // ---- type expressions ------------------------------------------------

    #[test]
    fn type_function_one_param() {
        let m = p("module x; type F = Int -> Bool;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function { param, .. } => {
                    let ps = Type::flatten_param_list(param);
                    assert_eq!(ps.len(), 1);
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_function_bare_one_param() {
        let m = p("module x; type F = Int -> Bool;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function { param, .. } => {
                    let ps = Type::flatten_param_list(param);
                    assert_eq!(ps.len(), 1);
                    assert!(matches!(ps[0], Type::Path { .. }));
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_function_bare_arrow_right_associates() {
        let m = p("module x; type F = A -> B -> C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function { ret, .. } => {
                    assert!(matches!(ret.as_ref(), Type::Function { .. }));
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_function_rhs_can_be_product_chain() {
        let m = p("module x; type F = A -> B & C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function { ret, .. } => {
                    assert!(matches!(ret.as_ref(), Type::Product { .. }));
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_function_parenthesized_bare_arrow_groups() {
        let m = p("module x; type F = (A -> B);");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Function { .. }));
        }
    }

    #[test]
    fn comma_after_function_type_binder_rejected() {
        let msg = p_err("module x; type Id = ([A], A) -> A;");
        assert!(
            msg.contains("expected type expression") || msg.contains("expected `)`"),
            "comma after function-type binder must be rejected, got: {msg}"
        );
    }

    #[test]
    fn function_type_comma_binder_group_accepts_shorthand() {
        p("module x; type F = [A, B]A -> B;");
    }

    #[test]
    fn function_type_comma_value_group_rejected() {
        for source in [
            "module x; type F = (A, B) -> C;",
            "module x; type F = [A](A, A) -> A;",
        ] {
            let msg = p_err(source);
            assert!(
                msg.contains("commas form tuple values, not product types"),
                "source = {source}; got: {msg}"
            );
        }
    }

    #[test]
    fn type_function_bare_product_lhs_rejected() {
        let msg = p_err("module x; type F = A & B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_function_bare_sum_lhs_rejected() {
        let msg = p_err("module x; type F = A | B -> C;");
        assert!(
            msg.contains("left of `->`") || msg.contains("parenthesized"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_function_zero_param() {
        let m = p("module x; type F = . -> Int;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function {
                    param, abi_arity, ..
                } => {
                    // The unit domain has zero normalized ABI slots.
                    assert!(matches!(**param, Type::Unit { .. }));
                    assert_eq!(*abi_arity, 0);
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_function_unit_param_group() {
        let m = p("module x; type F = (.) -> Int;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function {
                    param, abi_arity, ..
                } => {
                    assert!(matches!(**param, Type::Unit { .. }));
                    assert_eq!(*abi_arity, 0);
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_unit_vs_zero_param_function() {
        let m = p("module x; type U = .;");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Unit { .. }));
        }
    }

    #[test]
    fn type_bottom() {
        let m = p("module x; type Bot = !;");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Bottom { .. }));
        }
    }

    #[test]
    fn type_sum_and_product() {
        let m = p("module x; type S = (A | B); type P = (A & B);");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Sum { .. }));
        }
        if let Item::TypeAlias(t) = &m.items[1] {
            assert!(matches!(t.type_body(), Type::Product { .. }));
        }
    }

    // ---- value expressions -----------------------------------------------

    #[test]
    fn fn_expr_value_param_annotation_parses() {
        // Annotations on `fn` value parameters parse as arbitrary
        // type expressions; the typer verifies them against
        // bidirectional flow.
        let m = p("module x; fn make() -> . { (.(x: .) { x })(()) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, .. } => match callee.as_ref() {
                    Expr::FnExpr { sig, .. } => {
                        if let SignatureParam::Value(vp) = &sig.params[0] {
                            assert_eq!(vp.name, "x");
                            assert!(matches!(vp.ty, Some(Type::Unit { .. })));
                        }
                    }
                    other => panic!("expected FnExpr, got {other:?}"),
                },
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn fn_expr_return_annotation_parses() {
        // `fn` return-type annotations parse as arbitrary type
        // expressions; the typer verifies them against bidirectional
        // flow.
        let m = p("module x; fn make() -> . { (.(x) -> . { x })(()) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, .. } => match callee.as_ref() {
                    Expr::FnExpr { ret_ty, .. } => {
                        assert!(matches!(ret_ty, Some(Type::Unit { .. })));
                    }
                    other => panic!("expected FnDef, got {other:?}"),
                },
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn fn_expr_underscore_placeholder_annotations_parse_as_infer() {
        // `: _` and `-> _` placeholders parse as `Type::Infer`. The
        // typer treats them identically to no annotation: the type
        // is recovered from bidirectional flow.
        let m = p("module x; fn make() -> . { (.(x: _) -> _ { x })(()) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, .. } => match callee.as_ref() {
                    Expr::FnExpr { sig, ret_ty, .. } => {
                        if let SignatureParam::Value(vp) = &sig.params[0] {
                            assert_eq!(vp.name, "x");
                            assert!(matches!(vp.ty, Some(Type::Infer { .. })));
                        }
                        assert!(matches!(ret_ty, Some(Type::Infer { .. })));
                    }
                    other => panic!("expected FnDef, got {other:?}"),
                },
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn fn_expr_no_annotations_parses() {
        let m = p("module x; fn make() -> . { .(x) { x }(0) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Call { callee, .. } => match callee.as_ref() {
                    Expr::FnExpr { sig, .. } => {
                        if let SignatureParam::Value(vp) = &sig.params[0] {
                            assert_eq!(vp.name, "x");
                            assert!(vp.ty.is_none());
                        } else {
                            panic!("expected value param");
                        }
                    }
                    other => panic!("expected FnDef, got {other:?}"),
                },
                other => panic!("expected Call, got {other:?}"),
            }
        }
    }

    #[test]
    fn let_expr_simple() {
        let m = p("module x; fn main() -> . { let x = (); x }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Let { name, .. } => {
                    assert_eq!(name, "x");
                }
                other => panic!("expected Let, got {other:?}"),
            }
        }
    }

    #[test]
    fn let_unpack_single_binder_desugars_to_cps_call() {
        // `let .(<U> x) = e; rest` parses as a call applying `e` to a
        // continuation: `e(_, .[U](x) { rest })`. The callee is the
        // user-written RHS (typically a curried CPS-projector call).
        // This parser test exercises only the syntactic desugar shape;
        // the typer test verifies the CPS form composes correctly.
        let m = p("module x; fn open(thunk: I32) -> . { let .(<U> x) = thunk; () }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected FnDef");
        };
        let Expr::Call { callee, args, .. } = &d.body else {
            panic!("expected Call, got {:?}", d.body);
        };
        // Callee is the RHS expression `thunk` directly.
        let Expr::Path { segments, .. } = callee.as_ref() else {
            panic!("expected Path callee");
        };
        assert_eq!(segments, &["thunk".to_string()]);
        assert_eq!(args.len(), 2);
        // First arg: `_` (Type::Infer for the result type).
        assert!(matches!(args[0], CallArg::Type(Type::Infer { .. })));
        // Second arg: the continuation `.[U](x) { … }`.
        let CallArg::Value(Expr::FnExpr { sig, .. }) = &args[1] else {
            panic!("expected FnExpr continuation, got {:?}", args[1]);
        };
        assert_eq!(sig.params.len(), 2);
        assert!(matches!(&sig.params[0], SignatureParam::Type(tp) if tp.name == "U"));
        assert!(matches!(&sig.params[1], SignatureParam::Value(p) if p.name == "x"));
    }

    #[test]
    fn let_unpack_multi_binder_desugars_to_cps_call() {
        let m = p("module x; fn open(thunk: I32) -> . { let .(<L> <R2> x) = thunk; () }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected FnDef");
        };
        let Expr::Call { args, .. } = &d.body else {
            panic!("expected Call");
        };
        let CallArg::Value(Expr::FnExpr { sig, .. }) = &args[1] else {
            panic!("expected FnExpr continuation");
        };
        assert_eq!(sig.params.len(), 3);
        assert!(matches!(&sig.params[0], SignatureParam::Type(tp) if tp.name == "L"));
        assert!(matches!(&sig.params[1], SignatureParam::Type(tp) if tp.name == "R2"));
        assert!(matches!(&sig.params[2], SignatureParam::Value(p) if p.name == "x"));
    }

    #[test]
    fn let_unpack_rejects_annotation() {
        let msg = p_err(
            "module x; \
             fn open() -> . { let .(<U> x: u) = e; () }",
        );
        assert!(msg.contains("type annotations on existential-opening `let` binders"));
    }

    #[test]
    fn let_unpack_works_inside_equiv_arm() {
        let m = p("module x; equiv check { scope! { let .(<U> x) = thunk; () }; () }");
        let Item::Equiv(e, _) = &m.items[0] else {
            panic!("expected Equiv");
        };
        let Expr::BlockCall { blocks, .. } = &e.terms[0].body else {
            panic!("expected block call inside equiv arm");
        };
        let crate::ast::NeutralItem::ExistentialBinding {
            type_params,
            name,
            value,
            ..
        } = &blocks[0].items[0]
        else {
            panic!("expected existential-opening neutral binding");
        };
        assert_eq!(type_params[0].name, "U");
        assert_eq!(name, "x");
        assert!(matches!(value, Expr::Path { segments, .. } if segments == &["thunk".to_string()]));
        assert!(matches!(
            blocks[0].items[1],
            crate::ast::NeutralItem::Expression {
                value: Expr::Unit { .. },
                ..
            }
        ));
    }

    #[test]
    fn let_expr_annotation_parses_as_typed_let() {
        let m = p("module x; fn main() -> . { let .(x: .) = (); x }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected FnDef");
        };
        let Expr::Let {
            name,
            ty: Some(Type::Unit { .. }),
            pattern,
            ..
        } = &d.body
        else {
            panic!("expected typed Expr::Let");
        };
        assert_eq!(name, "x");
        assert!(pattern.is_none());
    }

    #[test]
    fn let_in_form_rejected() {
        // The parser should produce a pointed error pointing at
        // the block-statement spelling when the user wrote
        // `let X = E in body`.
        let msg = p_err("module x; fn main() -> . { let x = () in x }");
        assert!(
            msg.contains("expected `;` after let binding"),
            "expected pointed `let-in` rejection error, got: {msg}"
        );
    }

    #[test]
    fn block_must_end_with_expression() {
        let msg = p_err("module x; fn f() -> . { let x = (); }");
        assert!(
            msg.contains("must end with an expression"),
            "expected end-with-expression error, got: {msg}"
        );
    }

    #[test]
    fn empty_block_rejected() {
        let msg = p_err("module x; fn f() -> . { }");
        assert!(
            msg.contains("no final expression") || msg.contains("must end with"),
            "expected empty-block error, got: {msg}"
        );
    }

    #[test]
    fn seq_statement_parses() {
        // `e;` is an expression statement (Expr::Seq), distinct
        // from `let _ = e;` (Expr::Let with name "_").
        let m = p("module x; fn f() -> . { ()  ; () }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Seq { value, body, .. } => {
                    assert!(matches!(value.as_ref(), Expr::Unit { .. }));
                    assert!(matches!(body.as_ref(), Expr::Unit { .. }));
                }
                other => panic!("expected Seq, got {other:?}"),
            }
        }
    }

    #[test]
    fn block_repeated_and_trailing_semicolons_parse() {
        let m = p("module x; fn f() -> . { ;;; ();;; ();;; }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::Seq { value, body, .. } = &d.body else {
            panic!("expected Seq, got {:?}", d.body);
        };
        assert!(matches!(value.as_ref(), Expr::Unit { .. }));
        assert!(matches!(body.as_ref(), Expr::Unit { .. }));
    }

    #[test]
    fn final_semicolon_after_block_expression_is_ignored() {
        let m = p("module x; fn f() -> . { (); }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        assert!(matches!(d.body, Expr::Unit { .. }), "got {:?}", d.body);
    }

    #[test]
    fn wildcard_let_parses() {
        let m = p("module x; fn f() -> . { let _ = (); () }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Let { name, .. } => {
                    assert_eq!(name, "_");
                }
                other => panic!("expected Let, got {other:?}"),
            }
        }
    }

    #[test]
    fn tuple_literal_two_elements() {
        let m = p("module x; fn f() -> . { (.t, .f) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Tuple { items, .. } => {
                    assert_eq!(items.len(), 2);
                    assert!(matches!(items[0], Expr::BoolLit { value: true, .. }));
                    assert!(matches!(items[1], Expr::BoolLit { value: false, .. }));
                }
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
    }

    #[test]
    fn tuple_literal_three_elements() {
        let m = p("module x; fn f() -> . { (.t, .f, .t) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Tuple { items, .. } => assert_eq!(items.len(), 3),
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
    }

    #[test]
    fn tuple_literal_trailing_comma_allowed() {
        let m = p("module x; fn f() -> . { (.t, .f,) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Tuple { items, .. } => assert_eq!(items.len(), 2),
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
    }

    #[test]
    fn parens_single_element_is_grouping() {
        let m = p("module x; fn f() -> . { (.t) }");
        if let Item::FnDef(d) = &m.items[0] {
            // A bare `(e)` is grouping — yields the inner expression
            // unchanged, not a 1-tuple.
            assert!(matches!(d.body, Expr::BoolLit { .. }));
        }
    }

    #[test]
    fn tuple_single_element_with_trailing_comma_is_grouping() {
        let m = p("module x; fn f() -> . { (.t,) }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.body, Expr::BoolLit { value: true, .. }));
        }
    }

    #[test]
    fn tuple_single_element_with_leading_comma_is_grouping() {
        let m = p("module x; fn f() -> . { (,,, .t ,,,) }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.body, Expr::BoolLit { value: true, .. }));
        }
    }

    #[test]
    fn tuple_empty_comma_runs_are_unit() {
        let m = p("module x; fn f() -> . { (,,,) }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.body, Expr::Unit { .. }));
        }
    }

    #[test]
    fn type_product_chain_three_elements() {
        let m = p("module x; pub type T = (A & B & C);");
        if let Item::TypeAlias(t) = &m.items[0] {
            // Right-folded: `Product { A, Product { B, C } }`.
            match t.type_body() {
                Type::Product { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Product { .. }));
                }
                other => panic!("expected outer Product, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_sum_chain_three_elements() {
        let m = p("module x; pub type T = (A | B | C);");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Sum { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Sum { .. }));
                }
                other => panic!("expected outer Sum, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_comma_product_rejected() {
        let msg = p_err("module x; pub type T = (A, B);");
        assert!(
            msg.contains("commas form tuple values, not product types"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_comma_single_element_rejected() {
        for source in [
            "module x; pub type T = (, A);",
            "module x; pub type T = (A,);",
            "module x; pub type T = (,,, A ,,,);",
        ] {
            let msg = p_err(source);
            assert!(
                msg.contains("commas form tuple values, not product types"),
                "source = {source}; got: {msg}"
            );
        }
    }

    #[test]
    fn type_comma_empty_runs_rejected() {
        let msg = p_err("module x; pub type T = (,,,);");
        assert!(
            msg.contains("write `.` for the unit type"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_mixing_amp_and_pipe_rejected() {
        let msg = p_err("module x; pub type T = (A & B | C);");
        assert!(
            msg.contains("mixing `&` and `|`") || msg.contains("explicit parentheses"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_explicit_left_associative_keeps_shape() {
        // `((A & B) & C)` should still produce a left-leaning tree —
        // structurally distinct from the right-leaning chain form.
        let m = p("module x; pub type T = ((A & B) & C);");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Product { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Product { .. }));
                    assert!(matches!(right.as_ref(), Type::Path { .. }));
                }
                other => panic!("expected outer Product, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_bare_product_chain_two_elements() {
        // Bare `A & B` (no outer parens) folds to the same right-
        // associated `Product` as `(A & B)`.
        let m = p("module x; pub type T = A & B;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Product { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Path { .. }));
                }
                other => panic!("expected outer Product, got {other:?}"),
            }
        } else {
            panic!("expected type alias");
        }
    }

    #[test]
    fn type_bare_product_chain_three_elements() {
        let m = p("module x; pub type T = A & B & C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Product { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Product { .. }));
                }
                other => panic!("expected outer Product, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_bare_sum_chain_three_elements() {
        let m = p("module x; pub type T = A | B | C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Sum { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Sum { .. }));
                }
                other => panic!("expected outer Sum, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_bare_chain_in_fn_def_param() {
        // Bare `A & B` lands in a `fn` parameter annotation.
        let m = p("module x; fn f(x: A & B) -> . { x }");
        if let Item::FnDef(d) = &m.items[0] {
            if let SignatureParam::Value(vp) = &d.sig.params[0] {
                assert!(matches!(vp.ty, Some(Type::Product { .. })));
            } else {
                panic!("expected value param");
            }
        }
    }

    #[test]
    fn type_bare_chain_in_fn_def_return() {
        // Bare chains in return-type position.
        let m = p("module x; fn f(x: A) -> A | B { x }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.ret, Type::Sum { .. }));
        }
    }

    #[test]
    fn type_bare_leading_op_product() {
        // Multi-line layout: `& A & B` with the leading operator
        // accepted at the top of the chain.
        let m = p("module x; pub type T = & A & B;");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Product { .. }));
        }
    }

    #[test]
    fn type_bare_leading_op_sum() {
        let m = p("module x; pub type T = | A | B | C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Sum { left, right, .. } => {
                    assert!(matches!(left.as_ref(), Type::Path { .. }));
                    assert!(matches!(right.as_ref(), Type::Sum { .. }));
                }
                other => panic!("expected outer Sum, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_bare_chain_mixing_rejected() {
        // Bare `A & B | C` mixing rejected, mirroring the inside-paren rule.
        let msg = p_err("module x; pub type T = A & B | C;");
        assert!(
            msg.contains("mixing `&` and `|`") || msg.contains("explicit parentheses"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn type_bare_single_leading_op_is_grouping() {
        let m = p("module x; pub type T = & A;");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Path { .. }));
        }
    }

    #[test]
    fn type_bare_single_leading_pipe_is_grouping() {
        let m = p("module x; pub type T = | A;");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Path { .. }));
        }
    }

    #[test]
    fn type_empty_product_and_sum_chains_use_identities() {
        let m = p("module x;
             pub type P = &;
             pub type S = |;
             pub type Pp = (&);
             pub type Ss = (|);");
        if let Item::TypeAlias(t) = &m.items[0] {
            assert!(matches!(t.type_body(), Type::Unit { .. }));
        }
        if let Item::TypeAlias(t) = &m.items[1] {
            assert!(matches!(t.type_body(), Type::Bottom { .. }));
        }
        if let Item::TypeAlias(t) = &m.items[2] {
            assert!(matches!(t.type_body(), Type::Unit { .. }));
        }
        if let Item::TypeAlias(t) = &m.items[3] {
            assert!(matches!(t.type_body(), Type::Bottom { .. }));
        }
    }

    #[test]
    fn type_parenthesized_single_item_chains_are_grouping() {
        for source in [
            "module x; pub type T = (& A);",
            "module x; pub type T = (A &);",
            "module x; pub type T = (| A);",
            "module x; pub type T = (A |);",
        ] {
            let m = p(source);
            if let Item::TypeAlias(t) = &m.items[0] {
                assert!(
                    matches!(t.type_body(), Type::Path { .. }),
                    "source = {source}"
                );
            }
        }
    }

    #[test]
    fn type_comma_after_product_chain_in_function_param_rejected() {
        let msg = p_err("module x; pub type T = (A & B, C) -> R;");
        assert!(
            msg.contains("commas form tuple values, not product types"),
            "comma in function-type value group must be rejected, got: {msg}"
        );
    }

    #[test]
    fn type_bare_chain_then_arrow_parens_required() {
        // `(A & B) -> C` parses as a function from `A & B` to `C`.
        // The type-level representation has one product-domain slot;
        // multi-slot ABI is a named-signature lowering detail.
        let m = p("module x; pub type T = (A & B) -> C;");
        if let Item::TypeAlias(t) = &m.items[0] {
            match t.type_body() {
                Type::Function {
                    param,
                    ret,
                    abi_arity,
                    ..
                } => {
                    assert!(matches!(**param, Type::Product { .. }));
                    assert_eq!(*abi_arity, 1);
                    let params = Type::flatten_param_list(param);
                    assert!(matches!(params[0], Type::Path { .. }));
                    assert!(matches!(**ret, Type::Path { .. }));
                }
                other => panic!("expected Function, got {other:?}"),
            }
        }
    }

    #[test]
    fn type_product_function_alias_keeps_product_domain_single_slot() {
        let m = p("module x; pub type Deps[St][Res][Str] = \
             (((St -> St | Res) & St) -> Res) & ((Str & Str) -> Str);");
        let Item::TypeAlias(t) = &m.items[0] else {
            panic!("expected type alias");
        };
        let Type::Product { left, right, .. } = t.type_body() else {
            panic!("expected product alias");
        };
        let Type::Function {
            param: loop_param,
            abi_arity: loop_abi,
            ..
        } = left.as_ref()
        else {
            panic!("expected loop function slot");
        };
        assert!(matches!(loop_param.as_ref(), Type::Product { .. }));
        assert_eq!(*loop_abi, 1);
        let Type::Function {
            param: concat_param,
            abi_arity: concat_abi,
            ..
        } = right.as_ref()
        else {
            panic!("expected concat function slot");
        };
        assert!(matches!(concat_param.as_ref(), Type::Product { .. }));
        assert_eq!(*concat_abi, 1);
    }

    #[test]
    fn braced_label_type_form_rejected() {
        let msg = p_err("module x; labels { box[A] : A }; pub type T = {box: A & B};");
        assert!(
            msg.contains("label") && msg.contains("type position"),
            "got: {msg}"
        );
    }

    // ---- UFCS -------------------------------------------------------------

    #[test]
    fn ufcs_bare_no_args() {
        // `r.>f` with no parens parses to an `Expr::Ufcs` with an
        // empty arg list — projector-style call form.
        let m = p("module x; fn f() -> . { r.>f }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["f".to_owned()]);
                    assert!(args.is_empty());
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn explicit_empty_ufcs_tails_are_rejected_before_eager_or_lazy_ast_construction() {
        for (tail, list) in [
            ("r.>f()", "()"),
            ("r.>>f()", "()"),
            ("f().<r", "()"),
            ("f().<<r", "()"),
            ("r.>T.member()", "()"),
            ("r.>>T.member()", "()"),
            ("T.member().<r", "()"),
            ("T.member().<<r", "()"),
            ("r.>transform!()", "()"),
            ("r.>>transform!()", "()"),
            ("transform!().<r", "()"),
            ("transform!().<<r", "()"),
            ("r.>f(,)", "(,)"),
            ("f(,,).<r", "(,,)"),
            ("r.>f(\n// empty\n)", "(\n// empty\n)"),
            ("r.>f(\n// empty\n,)", "(\n// empty\n,)"),
            ("f(\n// empty\n).<r", "(\n// empty\n)"),
            ("(f()).<r", "()"),
            ("((T.member())).<<r", "()"),
            ("(f(\n// empty\n)).<r", "(\n// empty\n)"),
        ] {
            let source = format!("module x; fn run() -> . {{ {tail} }}");
            let eager = parse(&source).expect_err("an explicit empty UFCS tail must be rejected");
            assert_eq!(
                eager.diagnostic().message,
                "an explicitly empty UFCS argument list is ambiguous",
                "source: {tail}"
            );
            assert_eq!(
                eager.diagnostic().help(),
                Some(
                    "remove this argument list to write the bare UFCS form, or replace it with `(())` to pass Unit"
                ),
                "source: {tail}"
            );
            let span = eager.diagnostic().span;
            assert_eq!(
                &source[span.start as usize..span.end as usize],
                list,
                "source: {tail}"
            );
            let fixes = eager.diagnostic().fixes();
            let exact_list = list == "()";
            assert_eq!(
                fixes.len(),
                if exact_list { 2 } else { 1 },
                "source: {tail}"
            );
            assert_eq!(fixes[0].title, "Pass Unit explicitly", "source: {tail}");
            assert_eq!(fixes[0].applicability, Applicability::MaybeIncorrect);
            assert_eq!(fixes[0].edits.len(), 1, "source: {tail}");
            if exact_list {
                assert_eq!(fixes[0].edits[0].span, span, "source: {tail}");
                assert_eq!(fixes[0].edits[0].replacement, "(())", "source: {tail}");
                assert_eq!(fixes[1].title, "Use the bare UFCS form", "source: {tail}");
                assert_eq!(fixes[1].applicability, Applicability::MaybeIncorrect);
                assert_eq!(fixes[1].edits.len(), 1, "source: {tail}");
                assert_eq!(fixes[1].edits[0].span, span, "source: {tail}");
                assert_eq!(fixes[1].edits[0].replacement, "", "source: {tail}");
            } else {
                assert_eq!(
                    fixes[0].edits[0].span,
                    Span::new(span.end - 1, span.end - 1),
                    "source: {tail}"
                );
                assert_eq!(fixes[0].edits[0].replacement, "()", "source: {tail}");
            }

            let edit = &fixes[0].edits[0];
            let mut repaired = source.clone();
            repaired.replace_range(
                edit.span.start as usize..edit.span.end as usize,
                &edit.replacement,
            );
            if source.contains("// empty") {
                assert!(repaired.contains("// empty"), "source: {tail}");
            }
            assert_eq!(
                repaired.matches(',').count(),
                source.matches(',').count(),
                "the Unit repair must preserve commas for {tail}"
            );
            parse(&repaired).unwrap_or_else(|error| {
                panic!("Pass Unit explicitly produced invalid source for {tail}: {error:?}")
            });

            let lazy = parse_lazy(&source).expect("lazy parsing defers the function body");
            let forced = lazy
                .force_all()
                .expect_err("forcing the body must reject an explicit empty UFCS tail");
            assert_eq!(forced, eager, "eager/lazy diagnostic drift for {tail}");
        }

        for direct in ["f()", "T.member()", "transform!()"] {
            let source = format!("module x; fn run() -> . {{ {direct} }}");
            let eager = parse(&source)
                .unwrap_or_else(|error| panic!("direct call {direct} was rejected: {error:?}"));
            let lazy = parse_lazy(&source)
                .expect("lazy direct-call header")
                .force_all()
                .unwrap_or_else(|error| {
                    panic!("lazy direct call {direct} was rejected: {error:?}")
                });
            assert_eq!(lazy, eager, "direct-call eager/lazy drift for {direct}");
        }

        for grouped_nonempty in ["(f(a)).<r", "((T.member(a))).<<r"] {
            let source = format!("module x; fn run() -> . {{ {grouped_nonempty} }}");
            let eager = parse(&source).unwrap_or_else(|error| {
                panic!("grouped nonempty left splice {grouped_nonempty} was rejected: {error:?}")
            });
            let lazy = parse_lazy(&source)
                .expect("lazy grouped-splice header")
                .force_all()
                .unwrap_or_else(|error| {
                    panic!(
                        "lazy grouped nonempty left splice {grouped_nonempty} was rejected: {error:?}"
                    )
                });
            assert_eq!(
                lazy, eager,
                "grouped nonempty left-splice eager/lazy drift for {grouped_nonempty}"
            );
            let Item::FnDef(function) = &eager.items[0] else {
                panic!("expected function for {grouped_nonempty}");
            };
            let Expr::Ufcs { args, .. } = &function.body else {
                panic!(
                    "expected UFCS for {grouped_nonempty}, got {:?}",
                    function.body
                );
            };
            assert_eq!(args.len(), 1, "source: {grouped_nonempty}");
        }
    }

    #[test]
    fn grouped_empty_left_ufcs_tail_keeps_the_exact_list_span_across_trivia() {
        let source = "module x; fn run() -> . { (f // gap\n ()).<r }";
        let eager = parse(source).expect_err("the grouped empty tail must be rejected");
        let span = eager.diagnostic().span;
        assert_eq!(&source[span.start as usize..span.end as usize], "()");

        let forced = parse_lazy(source)
            .expect("lazy parsing defers the function body")
            .force_all()
            .expect_err("forcing the body must reject the grouped empty tail");
        assert_eq!(forced, eager);
    }

    #[test]
    fn bare_ufcs_tails_cover_every_direction_and_callee_shape() {
        for (tail, expected_flavor, bang) in [
            ("r.>f", UfcsFlavor::ReceiverFirst, false),
            ("r.>>f", UfcsFlavor::ReceiverLast, false),
            ("f.<r", UfcsFlavor::ArgumentLast, false),
            ("f.<<r", UfcsFlavor::ArgumentFirst, false),
            ("r.>T.member", UfcsFlavor::ReceiverFirst, false),
            ("r.>>T.member", UfcsFlavor::ReceiverLast, false),
            ("T.member.<r", UfcsFlavor::ArgumentLast, false),
            ("T.member.<<r", UfcsFlavor::ArgumentFirst, false),
            ("r.>transform!", UfcsFlavor::ReceiverFirst, true),
            ("r.>>transform!", UfcsFlavor::ReceiverLast, true),
            ("transform!.<r", UfcsFlavor::ArgumentLast, true),
            ("transform!.<<r", UfcsFlavor::ArgumentFirst, true),
        ] {
            let source = format!("module x; fn run() -> . {{ {tail} }}");
            let eager = parse(&source).unwrap_or_else(|error| {
                panic!("bare UFCS tail failed to parse for {tail}: {error:?}")
            });
            let lazy = parse_lazy(&source)
                .expect("lazy header parse")
                .force_all()
                .unwrap_or_else(|error| {
                    panic!("bare UFCS tail failed lazy force for {tail}: {error:?}")
                });
            assert_eq!(lazy, eager, "eager/lazy AST drift for {tail}");
            let Item::FnDef(function) = &eager.items[0] else {
                panic!("expected function for {tail}");
            };
            let Expr::Ufcs {
                flavor,
                args,
                bang: parsed_bang,
                ..
            } = &function.body
            else {
                panic!("expected UFCS for {tail}, got {:?}", function.body);
            };
            assert_eq!(*flavor, expected_flavor, "source: {tail}");
            assert!(args.is_empty(), "bare form gained arguments: {tail}");
            assert_eq!(parsed_bang.is_some(), bang, "bang drift for {tail}");
        }
    }

    #[test]
    fn explicit_unit_ufcs_tails_cover_every_direction_and_callee_shape() {
        for (tail, expected_flavor, bang) in [
            ("r.>f(())", UfcsFlavor::ReceiverFirst, false),
            ("r.>>f(())", UfcsFlavor::ReceiverLast, false),
            ("f(()).<r", UfcsFlavor::ArgumentLast, false),
            ("f(()).<<r", UfcsFlavor::ArgumentFirst, false),
            ("r.>T.member(())", UfcsFlavor::ReceiverFirst, false),
            ("r.>>T.member(())", UfcsFlavor::ReceiverLast, false),
            ("T.member(()).<r", UfcsFlavor::ArgumentLast, false),
            ("T.member(()).<<r", UfcsFlavor::ArgumentFirst, false),
            ("r.>transform!(())", UfcsFlavor::ReceiverFirst, true),
            ("r.>>transform!(())", UfcsFlavor::ReceiverLast, true),
            ("transform!(()).<r", UfcsFlavor::ArgumentLast, true),
            ("transform!(()).<<r", UfcsFlavor::ArgumentFirst, true),
        ] {
            let source = format!("module x; fn run() -> . {{ {tail} }}");
            let eager = parse(&source)
                .unwrap_or_else(|error| panic!("explicit Unit failed for {tail}: {error:?}"));
            let lazy = parse_lazy(&source)
                .expect("lazy header parse")
                .force_all()
                .unwrap_or_else(|error| panic!("lazy explicit Unit failed for {tail}: {error:?}"));
            assert_eq!(lazy, eager, "eager/lazy AST drift for {tail}");
            let Item::FnDef(function) = &eager.items[0] else {
                panic!("expected function for {tail}");
            };
            let Expr::Ufcs {
                flavor,
                args,
                bang: parsed_bang,
                ..
            } = &function.body
            else {
                panic!("expected UFCS for {tail}, got {:?}", function.body);
            };
            assert_eq!(*flavor, expected_flavor, "source: {tail}");
            assert!(
                matches!(args.as_slice(), [CallArg::Value(Expr::Unit { .. })]),
                "explicit Unit was not retained for {tail}: {args:?}"
            );
            assert_eq!(parsed_bang.is_some(), bang, "bang drift for {tail}");
        }
    }

    #[test]
    fn ufcs_with_value_args() {
        let m = p("module x; fn f() -> . { r.>f(x, y) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    flavor,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["f".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ReceiverFirst);
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_receiver_last_with_value_args() {
        let m = p("module x; fn f() -> . { r.>>f(x, y) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    flavor,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["f".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ReceiverLast);
                    assert_eq!(args.len(), 2);
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_regular_callee_accepts_explicit_type_args() {
        for (src, expected_flavor) in [
            (
                "module x; fn f() -> . { r.>f(T, x) }",
                UfcsFlavor::ReceiverFirst,
            ),
            (
                "module x; fn f() -> . { r.>>f(T, x) }",
                UfcsFlavor::ReceiverLast,
            ),
            (
                "module x; fn f() -> . { f(T, x).<r }",
                UfcsFlavor::ArgumentLast,
            ),
            (
                "module x; fn f() -> . { f(T, x).<<r }",
                UfcsFlavor::ArgumentFirst,
            ),
        ] {
            let m = p(src);
            if let Item::FnDef(d) = &m.items[0] {
                match &d.body {
                    Expr::Ufcs {
                        callee_segments,
                        flavor,
                        args,
                        bang,
                        ..
                    } => {
                        assert_eq!(callee_segments, &vec!["f".to_owned()]);
                        assert_eq!(*flavor, expected_flavor, "source: {src}");
                        assert!(bang.is_none(), "regular UFCS should not carry bang");
                        assert_eq!(args.len(), 2, "source: {src}");
                        assert!(
                            matches!(
                                args[0],
                                CallArg::Type(_) | CallArg::Value(Expr::Path { .. })
                            ),
                            "type-shaped first arg should be preserved for typing: {src}"
                        );
                    }
                    other => panic!("expected Ufcs for {src}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn call_splice_argument_last_zero_existing_args() {
        let m = p("module x; fn f() -> . { f.<x }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    flavor,
                    args,
                    ..
                } => {
                    assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
                    assert_eq!(callee_segments, &vec!["f".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ArgumentLast);
                    assert!(args.is_empty());
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn call_splice_argument_first_existing_args() {
        let m = p("module x; fn f() -> . { f(a).<<x }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    flavor,
                    args,
                    ..
                } => {
                    assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
                    assert_eq!(callee_segments, &vec!["f".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ArgumentFirst);
                    assert_eq!(args.len(), 1);
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn call_splice_rhs_stops_before_next_splice() {
        let m = p("module x; fn f() -> . { f.<x.>g }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    flavor,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["g".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ReceiverFirst);
                    assert!(matches!(
                        receiver.as_ref(),
                        Expr::Ufcs {
                            flavor: UfcsFlavor::ArgumentLast,
                            ..
                        }
                    ));
                }
                other => panic!("expected outer Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn call_splice_parenthesized_rhs_can_contain_splice() {
        let m = p("module x; fn f() -> . { f.<(x.>g) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver, flavor, ..
                } => {
                    assert_eq!(*flavor, UfcsFlavor::ArgumentLast);
                    assert!(matches!(
                        receiver.as_ref(),
                        Expr::Ufcs {
                            flavor: UfcsFlavor::ReceiverFirst,
                            ..
                        }
                    ));
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_qualified_callee() {
        let m = p("module x; fn f() -> . { r.>m.f(x) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments, ..
                } => {
                    assert_eq!(callee_segments, &vec!["m".to_owned(), "f".to_owned()]);
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_chained() {
        // `r.>f.>g(x)` chains UFCS calls; the outer call's
        // receiver is the inner UFCS expression.
        let m = p("module x; fn f() -> . { r.>f.>g(x) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["g".to_owned()]);
                    assert_eq!(args.len(), 1);
                    // Inner receiver is itself a UFCS.
                    assert!(matches!(receiver.as_ref(), Expr::Ufcs { .. }));
                }
                other => panic!("expected outer Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_call_then_ufcs() {
        // `f(r).>g` mixes call with UFCS — the postfix loop
        // alternates freely.
        let m = p("module x; fn f() -> . { f(r).>g }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs { receiver, .. } => {
                    assert!(matches!(receiver.as_ref(), Expr::Call { .. }));
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn ufcs_missing_callee_rejected() {
        let msg = p_err("module x; fn f() -> . { r.> }");
        assert!(
            msg.contains("expected") && (msg.contains("ident") || msg.contains("name")),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn ufcs_dotted_callee_no_trailing_segment_rejected() {
        let msg = p_err("module x; fn f() -> . { r.>f. }");
        assert!(
            msg.contains("expected") && (msg.contains("ident") || msg.contains("name")),
            "unexpected: {msg}"
        );
    }

    /// `r.>iso!` parses to an `Expr::Ufcs` with `bang = Some(_)`
    /// (the surface-form-preserving spelling). The receiver flows
    /// through `receiver`, the elaborator name is the single segment,
    /// and the absent target leaves `args` empty.
    #[test]
    fn elaborator_ufcs_no_args_iso() {
        let m = p("module x; fn f() -> . { r.>iso! }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    args,
                    bang,
                    ..
                } => {
                    assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
                    assert_eq!(callee_segments, &vec!["iso".to_owned()]);
                    assert!(args.is_empty(), "no target supplied");
                    assert!(bang.is_some(), "bang must be recorded for elaborator-UFCS");
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    /// `r.>into!(T)` parses to `Expr::Ufcs` with the target type
    /// recorded as the single trailing ordinary call arg.
    #[test]
    fn elaborator_ufcs_with_target_into() {
        let m = p("module x; fn f() -> . { r.>into!(T) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    args,
                    bang,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["into".to_owned()]);
                    assert_eq!(args.len(), 1);
                    assert!(
                        matches!(
                            args[0],
                            CallArg::Type(_) | CallArg::Value(Expr::Path { .. })
                        ),
                        "generic UFCS stores the raw call arg until typing"
                    );
                    assert!(bang.is_some());
                }
                other => panic!("expected Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn elaborator_ufcs_receiver_last_parses() {
        let m = p("module x; fn f() -> . { r.>>iso! }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    flavor,
                    bang,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["iso".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ReceiverLast);
                    assert!(bang.is_some());
                    assert!(args.is_empty());
                }
                other => panic!("expected elaborator Ufcs, got {other:?}"),
            }
        }
    }

    #[test]
    fn elaborator_call_splice_argument_first_preserves_ufcs_until_typing() {
        let m = p("module x; fn f() -> . { iso!(T).<<r }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    flavor,
                    bang,
                    args,
                    ..
                } => {
                    assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
                    assert_eq!(callee_segments, &vec!["iso".to_owned()]);
                    assert_eq!(*flavor, UfcsFlavor::ArgumentFirst);
                    assert!(bang.is_some());
                    assert_eq!(args.len(), 1, "target slot should remain unspliced");
                }
                other => panic!("expected elaborator call-splice UFCS, got {other:?}"),
            }
        }
    }

    #[test]
    fn elaborator_ufcs_onto_align_atom() {
        for (src, expected_name) in [
            ("module x; fn f() -> . { r.>onto! }", "onto"),
            ("module x; fn f() -> . { r.>align! }", "align"),
            ("module x; fn f() -> . { r.>atom! }", "atom"),
        ] {
            let m = p(src);
            if let Item::FnDef(d) = &m.items[0] {
                match &d.body {
                    Expr::Ufcs {
                        callee_segments,
                        bang,
                        ..
                    } => {
                        assert!(bang.is_some(), "expected bang on {src}");
                        assert_eq!(
                            callee_segments,
                            &vec![expected_name.to_owned()],
                            "for {src}"
                        );
                    }
                    other => panic!("expected elaborator Ufcs node, got {other:?} for {src}"),
                }
            }
        }
    }

    /// An elaborator-UFCS result is itself a valid receiver: `r.>iso!.>frob`
    /// chains a regular UFCS over the elaborator-UFCS result. The outer
    /// is a regular UFCS (`bang = None`); the inner receiver is the
    /// elaborator-UFCS (`bang = Some`).
    #[test]
    fn elaborator_ufcs_followed_by_ufcs() {
        let m = p("module x; fn f() -> . { r.>iso!.>frob }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    bang,
                    ..
                } => {
                    assert!(bang.is_none(), "outer UFCS is regular, no bang");
                    assert_eq!(callee_segments, &vec!["frob".to_owned()]);
                    match receiver.as_ref() {
                        Expr::Ufcs {
                            callee_segments: inner_segs,
                            bang: inner_bang,
                            ..
                        } => {
                            assert_eq!(inner_segs, &vec!["iso".to_owned()]);
                            assert!(inner_bang.is_some());
                        }
                        other => panic!("expected inner elaborator-UFCS, got {other:?}"),
                    }
                }
                other => panic!("expected outer Ufcs, got {other:?}"),
            }
        }
    }

    /// A regular UFCS followed by elaborator-UFCS: `r.>frob.>iso!`.
    /// The outer is elaborator-UFCS (`bang = Some`) whose receiver is
    /// the regular UFCS.
    #[test]
    fn ufcs_followed_by_elaborator_ufcs() {
        let m = p("module x; fn f() -> . { r.>frob.>iso! }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    receiver,
                    callee_segments,
                    bang,
                    ..
                } => {
                    assert!(bang.is_some(), "outer is elaborator-UFCS");
                    assert_eq!(callee_segments, &vec!["iso".to_owned()]);
                    match receiver.as_ref() {
                        Expr::Ufcs {
                            callee_segments: inner_segs,
                            bang: inner_bang,
                            ..
                        } => {
                            assert_eq!(inner_segs, &vec!["frob".to_owned()]);
                            assert!(inner_bang.is_none(), "inner is regular UFCS");
                        }
                        other => panic!("expected inner regular UFCS, got {other:?}"),
                    }
                }
                other => panic!("expected outer elaborator-UFCS, got {other:?}"),
            }
        }
    }

    /// `r.>frob!(x)` parses as a user-defined elaborator UFCS call. The
    /// name resolves later in ordinary scope.
    #[test]
    fn elaborator_ufcs_user_defined_name_parses() {
        let m = p("module x; fn f() -> . { r.>frob!(x) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    bang,
                    args,
                    ..
                } => {
                    assert_eq!(callee_segments, &vec!["frob".to_owned()]);
                    assert!(bang.is_some(), "user-defined elaborator keeps bang marker");
                    assert_eq!(args.len(), 1);
                }
                other => panic!("expected user-defined elaborator-UFCS, got {other:?}"),
            }
        }
    }

    /// `r.>m.iso!(...)` is rejected — multi-segment paths can't
    /// carry the `!` suffix; the elaborator form is a single segment.
    #[test]
    fn elaborator_ufcs_multi_segment_rejected() {
        let msg = p_err("module x; fn f() -> . { r.>m.iso! }");
        assert!(
            msg.contains("elaborator") && msg.contains("m.iso"),
            "unexpected: {msg}"
        );
    }

    /// `r.>atom!(T)` parses — the trailing target is admitted
    /// (same shape as `iso!`/`onto!`/etc.); the single-arm
    /// constraint on the target is enforced by the `atom` user
    /// elaborator's own logic, not a built-in typer pass.
    #[test]
    fn elaborator_ufcs_atom_with_target_parses() {
        let m = p("module x; fn f() -> . { r.>atom!(T) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    args,
                    bang,
                    ..
                } => {
                    assert!(bang.is_some());
                    assert_eq!(callee_segments, &vec!["atom".to_owned()]);
                    assert_eq!(args.len(), 1, "expected target type-arg");
                    assert!(
                        matches!(
                            args[0],
                            CallArg::Type(_) | CallArg::Value(Expr::Path { .. })
                        ),
                        "bang UFCS keeps the parser-classified target argument for type-directed slot selection"
                    );
                }
                other => panic!("expected elaborator-UFCS Ufcs, got {other:?}"),
            }
        }
    }

    // ---- Operators (op) ------------------------------------------------

    #[test]
    fn op_non_assoc_parses() {
        let m = p("module x; fn add(a: A, b: A) -> A { a } op _ + _ { impl add; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, function } = &d.body;
        assert_eq!(
            function.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["add"]
        );
        assert_eq!(pattern.len(), 3);
    }

    #[test]
    fn op_right_assoc_parses() {
        let m = p("module x; fn add(a: A, b: A) -> A { a } op _ + __ { impl add; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, .. } = &d.body;
        assert!(matches!(pattern[0], OpPart::SlotPlain { .. }));
        assert!(matches!(pattern[2], OpPart::SlotRecursive { .. }));
    }

    #[test]
    fn op_left_assoc_parses() {
        let m = p("module x; fn add(a: A, b: A) -> A { a } op __ + _ { impl add; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, .. } = &d.body;
        assert!(matches!(pattern[0], OpPart::SlotRecursive { .. }));
        assert!(matches!(pattern[2], OpPart::SlotPlain { .. }));
    }

    // ---- Internal operator dispatch keys -------------------------------

    /// Collision identity for the expression parser, distinct from the full
    /// grammar written in imports and documentation queries.
    #[cfg(feature = "surface")]
    fn fixed_dispatch_key_of(decl: &str) -> String {
        let src = format!("module x; fn f() -> . {{ () }} {decl}");
        let m = p(&src);
        let Item::Op(d, _) = m.items.last().expect("op item") else {
            panic!("expected Op as last item");
        };
        crate::ast::OperatorDispatchKey::from_body(&d.body).render()
    }

    #[cfg(feature = "surface")]
    fn variadic_dispatch_key_of(decl: &str) -> String {
        let src = format!("module x; fn f() -> . {{ () }} {decl}");
        let m = p(&src);
        let Item::VariadicOperator(d, _) = m.items.last().expect("variadic item") else {
            panic!("expected variadic operator as last item");
        };
        crate::ast::OperatorDispatchKey::from_variadic(d).render()
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_binary() {
        assert_eq!(fixed_dispatch_key_of("op _ + _ { impl f; };"), "_ +");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_drops_slot_kinds() {
        // The name carries no slot kinds — `op _ + __` and `op _ + _`
        // share the name `_ +` (they can't coexist in one scope).
        assert_eq!(fixed_dispatch_key_of("op _ + __ { impl f; };"), "_ +");
        assert_eq!(fixed_dispatch_key_of("op _ $ ___ { impl f; };"), "_ $");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_ternary_drops_tail() {
        // The ternary tail `: _` is not part of the name.
        assert_eq!(fixed_dispatch_key_of("op _ ? _ : __ { impl f; };"), "_ ?");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_adjacent_runs_keep_load_bearing_space() {
        // Two whitespace-separated op-token runs (`&&` then `++`)
        // must NOT collapse to `&&++` — the lexer would re-fuse them
        // into a single run, changing the operator's identity.
        assert_eq!(
            fixed_dispatch_key_of("op _ && ++ _ { impl f; };"),
            "_ && ++"
        );
        // A single fused run keeps no space.
        assert_eq!(fixed_dispatch_key_of("op _ &&++ _ { impl f; };"), "_ &&++");
        assert_eq!(fixed_dispatch_key_of("op _ < ! _ { impl f; };"), "_ < !");
        assert_eq!(fixed_dispatch_key_of("op _ <! _ { impl f; };"), "_ <!");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_prefix_and_bracket() {
        assert_eq!(fixed_dispatch_key_of("op + _ { impl f; };"), "+ _");
        assert_eq!(fixed_dispatch_key_of("op <% _ %> { impl f; };"), "<% _");
        assert_eq!(fixed_dispatch_key_of("op _ <% _ %> { impl f; };"), "_ <%");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_quoted_token() {
        // The reserved `=` token is admitted via quotation `(=)`; the
        // name carries the token content `=`, not the quotation marks.
        assert_eq!(fixed_dispatch_key_of("op _ (=) _ { impl f; };"), "_ =");
    }

    #[test]
    fn op_equal_token_requires_quotation() {
        let msg = p_err("module x; fn eq(a: A, b: A) -> A { a } op _ = _ { impl eq; };");
        assert!(
            msg.contains("operator-token quotation"),
            "unexpected: {msg}"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn fixed_dispatch_key_lenient_grouping_keeps_leading_run() {
        assert_eq!(
            fixed_dispatch_key_of("op _ ( <% _ %> ) { impl f; };"),
            "_ <%"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn variadic_dispatch_key_keyed_on_open_only() {
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            assert_eq!(
                variadic_dispatch_key_of(&format!("varop [* *] {{ {mode} f f; }};")),
                "[* _"
            );
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn variadic_dispatch_key_multi_character_open() {
        assert_eq!(
            variadic_dispatch_key_of("varop [! !] { foldr f f; };"),
            "[! _"
        );
    }

    // ---- `,` / `;` admissibility boundary -----------------------------

    #[test]
    fn op_bare_infix_semicolon_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ ; _ { impl f; };");
        assert!(
            msg.contains("not admissible") && msg.contains("semicolons separate statements"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_quoted_infix_semicolon_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ (;) _ { impl f; };");
        assert!(
            msg.contains("not admissible") && msg.contains("semicolons separate statements"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_bare_infix_comma_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ , _ { impl f; };");
        assert!(
            msg.contains("not admissible") && msg.contains("commas separate list elements"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_quoted_infix_comma_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ (,) _ { impl f; };");
        assert!(
            msg.contains("not admissible") && msg.contains("commas separate list elements"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn variadic_semicolon_separator_rejected() {
        for source in [
            "module x; varop [* *] { foldr cons nil; }; fn run(a: A, b: A) -> A { [* a; b *] }",
            "module x; op variadic [* (_ ;) ... *] { foldr cons nil; };",
        ] {
            assert!(parse(source).is_err());
            assert!(
                parse_lazy(source)
                    .and_then(|module| module.force_all())
                    .is_err()
            );
        }
    }

    #[test]
    fn variadic_comma_separator_accepted() {
        let m = p("module x; varop [* *] { foldr cons nil; }; \
             fn run(a: A, b: A) -> A { [* , a,, b, *] }");
        let Item::FnDef(function) = m.items.last().unwrap() else {
            panic!("expected function");
        };
        let Expr::OpChain {
            kind: OpChainKind::Variadic { elements, .. },
            ..
        } = &function.body
        else {
            panic!("expected varop literal");
        };
        assert_eq!(elements.len(), 2);
    }

    #[test]
    fn variadic_callables_must_be_lexical_paths() {
        let sources = [
            "module x; varop [* *] { foldr cons .x. { x1 }; };",
            "module x; varop [* *] { foldr .x. { x1 } nil; };",
            "module x; varop [* *] { foldr cons nil; finalize .x. { x1 }; };",
        ];

        for source in sources {
            let eager =
                parse(source).expect_err("eager parsing must reject inline variadic callable");
            assert!(
                eager
                    .diag()
                    .1
                    .contains("variadic callables must name a lexical callable")
                    && eager.diagnostic().help()
                        == Some("move any reordering or wrapping logic into a named fn"),
                "unexpected eager diagnostic for {source:?}: {eager:?}",
            );

            let lazy = parse_lazy(source)
                .expect_err("lazy parsing must reject an inline variadic callable");
            assert!(
                lazy.diag()
                    .1
                    .contains("variadic callables must name a lexical callable")
                    && lazy.diagnostic().help()
                        == Some("move any reordering or wrapping logic into a named fn"),
                "unexpected lazy diagnostic for {source:?}: {lazy:?}",
            );
        }
    }

    #[test]
    fn variadic_modes_and_callable_roles_are_explicit() {
        for (keyword, expected_mode) in [
            ("foldl", VariadicMode::FoldLeft),
            ("foldr", VariadicMode::FoldRight),
            ("foldl1", VariadicMode::FoldLeftOne),
            ("foldr1", VariadicMode::FoldRightOne),
        ] {
            for finalize in [false, true] {
                let tail = if finalize {
                    " finalize ops.finish;"
                } else {
                    ""
                };
                let source = format!(
                    "module x; varop [% %] {{ {keyword} ops.step ops.initialize;{tail} }};"
                );
                let eager = p(&source);
                let lazy = parse_lazy(&source).expect("complete variadic declaration");
                assert_eq!(lazy.module(), &eager);
                let Item::VariadicOperator(operator, _) = &eager.items[0] else {
                    panic!("variadic")
                };
                assert_eq!(operator.spec.mode, expected_mode);
                assert_eq!(operator.spec.close, ["%]"]);
                let names = |call: &CallableSpec| {
                    call.path
                        .iter()
                        .map(|segment| segment.name.clone())
                        .collect::<Vec<_>>()
                };
                assert_eq!(names(&operator.spec.step), ["ops", "step"]);
                assert_eq!(names(&operator.spec.initializer), ["ops", "initialize"]);
                assert_eq!(
                    operator.spec.finalize.as_ref().map(names),
                    finalize.then(|| vec!["ops".to_owned(), "finish".to_owned()]),
                );
            }
        }
    }

    #[test]
    fn variadic_body_has_one_primary_clause_and_optional_finalize() {
        for body in [
            "",
            "finalize finish;",
            "foldl step;",
            "foldl step initialize; foldr step initialize;",
            "foldl step initialize; finalize finish; finalize again;",
        ] {
            let source = format!("module x; varop [* *] {{ {body} }};");
            assert!(parse(&source).is_err(), "{body}");
            assert!(parse_lazy(&source).is_err(), "{body}");
        }
        let canonical = "module x; varop [* *] { foldl step initialize; finalize finish; };";
        let reordered = "module x; varop [* *] { finalize finish; foldl step initialize; };";
        let expected = crate::pretty::pretty_module(&parse(canonical).unwrap());
        assert_eq!(
            crate::pretty::pretty_module(&parse(reordered).unwrap()),
            expected
        );
        assert_eq!(
            crate::pretty::pretty_module(&parse_lazy(reordered).unwrap().force_all().unwrap()),
            expected
        );
    }

    #[test]
    fn variadic_pattern_requires_its_kind_tag() {
        for head in [
            "op [ (_ ,) ... ]",
            "op variadic [* (_ ,) ... *]",
            "op variadic <% (_ => _ ;) ... %>",
            "op variadic [ ! (_ ~) ... ! ]",
            "varop [* (_ ,) ... *]",
            "varop [% (_ => _ ,) ... %]",
            "varop [ ! ! ]",
            "varop [% ]%",
            "op [* *]",
        ] {
            let source = format!("module x; {head} {{ foldl step initialize; }};");
            assert!(parse(&source).is_err());
            assert!(parse_lazy(&source).is_err());
        }
    }

    #[test]
    fn op_body_comma_terminator_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ + _ { impl f, };");
        assert!(msg.contains("`;`"), "unexpected: {msg}");
    }

    #[test]
    fn op_two_recursive_slots_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op __ + __ { impl f; };");
        assert!(
            msg.contains("at most one recursive slot"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_adjacent_slots_rejected() {
        let msg = p_err("module x; fn f() -> . { () } op _ _ { impl f; };");
        assert!(msg.contains("adjacent slots"), "unexpected: {msg}");
    }

    #[test]
    fn op_no_token_rejected() {
        // `op _ _ _ { impl f; };` has no operator token. Caught either by
        // the adjacent-slots check or the empty-token check.
        let msg = p_err("module x; fn f() -> . { () } op _ _ _ { impl f; };");
        assert!(
            msg.contains("operator token") || msg.contains("adjacent"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_pub_accepted() {
        // `pub op` now parses — surface acceptance for the
        // cross-module operator-import feature. The `vis_pub`
        // flag is plumbed through to the AST so the resolver /
        // operator-fold pass can pick it up.
        let m = p("module x; fn add(a: A, b: A) -> A { a } pub op _ + _ { impl add; };");
        match m.items.last().expect("op item") {
            Item::Op(d, _) => assert!(d.vis.is_pub()),
            other => panic!("expected `pub op`, got {other:?}"),
        }
    }

    #[test]
    fn op_duplicate_in_scope_rejected() {
        let msg = p_err(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             fn other(a: A, b: A) -> A { a } \
             op _ + _ { impl add; }; \
             op _ + _ { impl other; };",
        );
        assert!(
            msg.contains("`op _ + _`") && msg.contains("already defined"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_distinct_slot_kinds_share_one_dispatch_key() {
        let msg = p_err(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             fn other(a: A, b: A) -> A { a } \
             op _ + _ { impl add; }; \
             op _ + __ { impl other; };",
        );
        assert!(
            msg.contains("`op _ + __`") && msg.contains("already defined"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn variadic_duplicate_diagnostic_retains_the_incoming_projection() {
        let message = p_err(
            "module x; varop [% %] { foldl append empty; }; \
             varop [% %] { foldr push zero; };",
        );
        assert!(message.contains("`varop [% %]`"), "{message}");
        assert!(message.contains("already defined"), "{message}");
    }

    #[test]
    fn op_prefix_forbidden_conflict_rejected() {
        // The prefix-forbidden conflict rule (see `specs/language.md`
        // § Operators) applies *within* a keyspace. Two prefix-shaped
        // declarations whose op-token sequences have a prefix
        // relationship — `-` vs `--` here — collide in the prefix
        // keyspace and the second declaration is rejected.
        let msg = p_err(
            "module x; \
             fn neg(a: A) -> A { a } \
             fn decrement(a: A) -> A { a } \
             op - _ { impl neg; }; \
             op - - _ { impl decrement; };",
        );
        assert!(
            msg.contains("prefix relationship") || msg.contains("longer operator"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_prefix_and_binary_coexist_same_token() {
        // The same op-token (`-`) is admitted simultaneously in the
        // prefix and non-prefix keyspaces — unary `op - __ { impl neg; };`
        // plus binary `op _ - __ { impl sub; };` coexist because the keyspaces
        // are separate.
        let m = p("module x; \
             fn neg(a: A) -> A { a } \
             fn sub(a: A, b: A) -> A { a } \
             op - __ { impl neg; }; \
             op _ - __ { impl sub; };");
        // Both `op` items registered without a parse error.
        assert!(matches!(&m.items[2], Item::Op(_, _)));
        assert!(matches!(&m.items[3], Item::Op(_, _)));
    }

    #[test]
    fn operator_usage_non_assoc_parses() {
        // The parser emits an `OpChain` placeholder at every operator
        // usage; the operator-fold pass (`kio-rs/src/op_fold.rs`)
        // resolves the placeholder to a concrete `Expr::Call`. These
        // parser-level tests check the placeholder shape; the fold
        // pass's own tests cover the resolved-call shape.
        let m = p("module x; \
             fn add(a: A, b: A) -> A { a } \
             op _ + _ { impl add; }; \
             fn use_op(x: A, y: A) -> A { x + y }");
        match &m.items[2] {
            Item::FnDef(d) => match &d.body {
                Expr::OpChain { kind, .. } => {
                    assert_eq!(kind.leading_op_run(), vec!["+".to_owned()]);
                    assert!(!kind.is_prefix());
                    let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                        panic!("expected Normal OpChain");
                    };
                    assert_eq!(slots.len(), 2);
                }
                other => panic!("expected OpChain, got {other:?}"),
            },
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn operator_usage_bang_prefixed_op_after_elaborator_named_value_is_not_elaborator_call() {
        let m = p("module x; \
             fn pick(a: A, b: A) -> A { a } \
             op _ !? _ { impl pick; }; \
             fn use_op(fit: A, y: A) -> A { fit !? y }");
        match &m.items[2] {
            Item::FnDef(d) => match &d.body {
                Expr::OpChain { kind, .. } => {
                    assert_eq!(kind.leading_op_run(), vec!["!?".to_owned()]);
                    assert!(!kind.is_prefix());
                    let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                        panic!("expected Normal OpChain");
                    };
                    assert_eq!(slots.len(), 2);
                }
                other => panic!("expected OpChain, got {other:?}"),
            },
            other => panic!("expected FnDef, got {other:?}"),
        }
    }

    #[test]
    fn operator_usage_non_assoc_chain_rejected() {
        let msg = p_err(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             op _ + _ { impl add; }; \
             fn use_op(x: A) -> A { x + x + x }",
        );
        assert!(
            msg.contains("non-associative") || msg.contains("explicit parens"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn operator_usage_right_assoc_chains() {
        // `a + b + c` with right-assoc parses as nested OpChain
        // placeholders: outer `+(x, [inner])`, inner `+(y, z)`.
        let m = p("module x; \
             fn add(a: A, b: A) -> A { a } \
             op _ + __ { impl add; }; \
             fn use_op(x: A, y: A, z: A) -> A { x + y + z }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots, .. },
                ..
            } = &d.body
        {
            assert_eq!(slots.len(), 2);
            if let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots: inner, .. },
                ..
            } = &slots[1]
            {
                assert_eq!(inner.len(), 2);
            } else {
                panic!("expected inner OpChain, got {:?}", slots[1]);
            }
        } else {
            panic!("expected outer FnDef → OpChain");
        }
    }

    #[test]
    fn operator_usage_left_assoc_chains() {
        // `a + b + c` with left-assoc parses as nested OpChain:
        // outer `+([inner], z)`, inner `+(x, y)`.
        let m = p("module x; \
             fn add(a: A, b: A) -> A { a } \
             op __ + _ { impl add; }; \
             fn use_op(x: A, y: A, z: A) -> A { x + y + z }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots, .. },
                ..
            } = &d.body
        {
            assert_eq!(slots.len(), 2);
            if let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots: inner, .. },
                ..
            } = &slots[0]
            {
                assert_eq!(inner.len(), 2);
            } else {
                panic!("expected inner OpChain, got {:?}", slots[0]);
            }
        } else {
            panic!("expected outer FnDef → OpChain");
        }
    }

    #[test]
    fn operator_usage_mixing_rejected() {
        let msg = p_err(
            "module x; \
             fn add(a: A, b: A) -> A { a } \
             fn mul(a: A, b: A) -> A { a } \
             op _ + __ { impl add; }; \
             op _ * __ { impl mul; }; \
             fn use_ops(x: A, y: A, z: A) -> A { x + y * z }",
        );
        assert!(
            msg.contains("different operators") || msg.contains("explicit parens"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn op_prefix_unary_parses() {
        let m = p("module x; \
             fn neg(a: A) -> A { a } \
             op - __ { impl neg; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, .. } = &d.body;
        assert!(matches!(pattern[0], OpPart::Token { .. }));
        assert!(matches!(pattern[1], OpPart::SlotRecursive { .. }));
    }

    #[test]
    fn op_postfix_unary_parses() {
        let m = p("module x; \
             fn maybe(a: A) -> A { a } \
             op __ ? { impl maybe; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, .. } = &d.body;
        assert!(matches!(pattern[0], OpPart::SlotRecursive { .. }));
        assert!(matches!(pattern[1], OpPart::Token { .. }));
    }

    #[test]
    fn op_ternary_right_assoc_parses() {
        let m = p("module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             op _ ? _ : __ { impl cond; };");
        let Item::Op(d, _) = &m.items[1] else {
            panic!("expected Op, got {:?}", m.items[1])
        };
        let OpBody::Normal { pattern, .. } = &d.body;
        assert_eq!(pattern.len(), 5);
        assert!(matches!(pattern[0], OpPart::SlotPlain { .. }));
        assert!(matches!(&pattern[1], OpPart::Token { content, .. } if content == "?"));
        assert!(matches!(pattern[2], OpPart::SlotPlain { .. }));
        assert!(matches!(&pattern[3], OpPart::Token { content, .. } if content == ":"));
        assert!(matches!(pattern[4], OpPart::SlotRecursive { .. }));
    }

    #[test]
    fn op_colon_sole_token_admitted() {
        // `:` is admissible in op pattern position because the
        // parser disambiguates type-annotation contexts (binding
        // sites) from expression position (operator).
        let m = p("module x; fn annot(a: A, b: A) -> A { a } op _ : _ { impl annot; };");
        assert!(matches!(&m.items[1], Item::Op(_, _)));
    }

    #[test]
    fn op_reserved_token_inside_multi_token_ok() {
        // `:` is also admitted inside larger operator patterns.
        let m = p("module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             op _ ? _ : _ { impl cond; };");
        assert!(matches!(&m.items[1], Item::Op(_, _)));
    }

    #[test]
    fn op_leading_dot_one_dot_rejected() {
        let msg = p_err("module x; fn f(a: A, b: A) -> A { a } op _ .+ _ { impl f; };");
        assert!(msg.contains("must contain at least two"), "got: {msg}");
    }

    #[test]
    fn op_quoted_leading_dot_one_dot_rejected() {
        let msg = p_err("module x; fn f(a: A, b: A) -> A { a } op _ (.+) _ { impl f; };");
        assert!(msg.contains("must contain at least two"), "got: {msg}");
    }

    #[test]
    fn op_marker_shaped_leading_dot_one_dot_rejected() {
        for spelling in [".#", ".$$$$"] {
            let msg = p_err(&format!(
                "module x; fn f(a: A, b: A) -> A {{ a }} op _ {spelling} _ {{ impl f; }};"
            ));
            assert!(
                msg.contains("must contain at least two"),
                "{spelling}: {msg}"
            );
        }
    }

    #[test]
    fn op_leading_dot_two_dots_admitted() {
        let m = p("module x; fn f(a: A, b: A) -> A { a } op _ .+. _ { impl f; };");
        assert!(matches!(&m.items[1], Item::Op(_, _)));
    }

    #[test]
    fn op_internal_or_trailing_dot_admitted() {
        let m = p("module x; \
             fn f(a: A, b: A) -> A { a } \
             op _ +. _ { impl f; }; \
             op _ <.> _ { impl f; };");
        assert!(matches!(&m.items[1], Item::Op(_, _)));
        assert!(matches!(&m.items[2], Item::Op(_, _)));
    }

    #[test]
    fn operator_usage_prefix_unary() {
        let m = p("module x; \
             fn neg(a: A) -> A { a } \
             op - __ { impl neg; }; \
             fn use_op(x: A) -> A { -x }");
        if let Item::FnDef(d) = &m.items[2] {
            match &d.body {
                Expr::OpChain { kind, .. } => {
                    assert_eq!(kind.leading_op_run(), vec!["-".to_owned()]);
                    assert!(kind.is_prefix());
                    let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                        panic!("expected Normal OpChain");
                    };
                    assert_eq!(slots.len(), 1);
                }
                other => panic!("expected OpChain, got {other:?}"),
            }
        }
    }

    #[test]
    fn operator_usage_prefix_unary_chains() {
        // `- - x` parses as nested prefix OpChains for a
        // right-recursive prefix.
        let m = p("module x; \
             fn neg(a: A) -> A { a } \
             op - __ { impl neg; }; \
             fn use_op(x: A) -> A { - - x }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::OpChain { kind, .. } = &d.body
        {
            assert!(kind.is_prefix());
            let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                panic!("expected Normal OpChain");
            };
            assert_eq!(slots.len(), 1);
            if let Expr::OpChain {
                kind: inner_kind, ..
            } = &slots[0]
            {
                assert!(inner_kind.is_prefix());
                let crate::ast::OpChainKind::Normal { slots: inner, .. } = inner_kind else {
                    panic!("expected inner Normal OpChain");
                };
                assert_eq!(inner.len(), 1);
            } else {
                panic!("expected nested prefix OpChain");
            }
        } else {
            panic!("expected outer FnDef → OpChain");
        }
    }

    #[test]
    fn operator_usage_postfix_unary() {
        let m = p("module x; \
             fn maybe(a: A) -> A { a } \
             op __ ? { impl maybe; }; \
             fn use_op(x: A) -> A { x? }");
        if let Item::FnDef(d) = &m.items[2] {
            match &d.body {
                Expr::OpChain { kind, .. } => {
                    assert_eq!(kind.leading_op_run(), vec!["?".to_owned()]);
                    assert!(!kind.is_prefix());
                    let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                        panic!("expected Normal OpChain");
                    };
                    assert_eq!(slots.len(), 1);
                }
                other => panic!("expected OpChain, got {other:?}"),
            }
        }
    }

    #[test]
    fn operator_usage_postfix_unary_chains() {
        // `x ? ?` parses as nested postfix OpChains.
        let m = p("module x; \
             fn maybe(a: A) -> A { a } \
             op __ ? { impl maybe; }; \
             fn use_op(x: A) -> A { x ? ? }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::OpChain { kind, .. } = &d.body
        {
            assert!(!kind.is_prefix());
            let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                panic!("expected Normal OpChain");
            };
            assert_eq!(slots.len(), 1);
            if let Expr::OpChain { .. } = &slots[0] {
                // Outer OpChain wraps inner OpChain — left-fold.
            } else {
                panic!("expected nested postfix OpChain");
            }
        } else {
            panic!("expected nested postfix OpChain");
        }
    }

    #[test]
    fn operator_usage_ternary() {
        let m = p("module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             op _ ? _ : __ { impl cond; }; \
             fn use_op(p: A, t: A, e: A) -> A { p ? t : e }");
        if let Item::FnDef(d) = &m.items[2] {
            match &d.body {
                Expr::OpChain { kind, .. } => {
                    assert_eq!(kind.leading_op_run(), vec!["?".to_owned()]);
                    let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                        panic!("expected Normal OpChain");
                    };
                    assert_eq!(slots.len(), 3);
                }
                other => panic!("expected OpChain, got {other:?}"),
            }
        }
    }

    #[test]
    fn operator_usage_ternary_right_assoc_chains() {
        // `a ? b : c ? d : e` with right-assoc ternary parses as
        // a nested OpChain: outer 3 slots, third slot itself a
        // 3-slot OpChain.
        let m = p("module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             op _ ? _ : __ { impl cond; }; \
             fn use_op(a: A, b: A, c: A, d: A, e: A) -> A { a ? b : c ? d : e }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots, .. },
                ..
            } = &d.body
        {
            assert_eq!(slots.len(), 3);
            if let Expr::OpChain {
                kind: crate::ast::OpChainKind::Normal { slots: inner, .. },
                ..
            } = &slots[2]
            {
                assert_eq!(inner.len(), 3);
            } else {
                panic!("expected nested OpChain in third slot, got {:?}", slots[2]);
            }
        } else {
            panic!("expected outer FnDef → OpChain");
        }
    }

    #[test]
    fn operator_nesting_prefix_with_binary_composes() {
        // `- x + y` parses as nested OpChain placeholders: outer
        // binary `+`, first slot a prefix `-`. After op-fold the
        // tree resolves to `add(neg(x), y)`.
        let m = p("module x; \
             fn neg(a: A) -> A { a } \
             fn add(a: A, b: A) -> A { a } \
             op - __ { impl neg; }; \
             op _ + __ { impl add; }; \
             fn use_op(x: A, y: A) -> A { - x + y }");
        if let Item::FnDef(d) = &m.items[4]
            && let Expr::OpChain { kind, .. } = &d.body
        {
            assert_eq!(kind.leading_op_run(), vec!["+".to_owned()]);
            let crate::ast::OpChainKind::Normal { slots, .. } = kind else {
                panic!("expected outer Normal OpChain");
            };
            assert!(matches!(
                &slots[0],
                Expr::OpChain { kind: inner_kind, .. }
                    if inner_kind.is_prefix()
            ));
        } else {
            panic!("expected outer + OpChain");
        }
    }

    #[test]
    fn operator_nesting_postfix_with_binary_rejected() {
        // `x ? + y` is a parse error — postfix completion does not
        // implicitly compose with binary; user writes `(x ?) + y`.
        let msg = p_err(
            "module x; \
             fn maybe(a: A) -> A { a } \
             fn add(a: A, b: A) -> A { a } \
             op __ ? { impl maybe; }; \
             op _ + __ { impl add; }; \
             fn use_op(x: A, y: A) -> A { x ? + y }",
        );
        assert!(
            msg.contains("different operators") || msg.contains("explicit parens"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn operator_nesting_parens_force_composition() {
        // `(x ?) + y` parses as outer `+` OpChain whose first slot
        // is the parenthesized `?` OpChain.
        let m = p("module x; \
             fn maybe(a: A) -> A { a } \
             fn add(a: A, b: A) -> A { a } \
             op __ ? { impl maybe; }; \
             op _ + __ { impl add; }; \
             fn use_op(x: A, y: A) -> A { (x ?) + y }");
        if let Item::FnDef(d) = &m.items[4]
            && let Expr::OpChain { kind, .. } = &d.body
        {
            assert_eq!(kind.leading_op_run(), vec!["+".to_owned()]);
        } else {
            panic!("expected outer + OpChain");
        }
    }

    #[test]
    fn operator_nesting_ternary_recursive_slot_rejects_other_op() {
        // The recursive slot of `_ ? _ : __` chains the same
        // ternary only. Different operators in that slot need
        // explicit parens.
        let msg = p_err(
            "module x; \
             fn cond(a: A, b: A, c: A) -> A { a } \
             fn add(a: A, b: A) -> A { a } \
             op _ ? _ : __ { impl cond; }; \
             op _ + __ { impl add; }; \
             fn use_op(a: A, b: A, c: A, d: A) -> A { a ? b : c + d }",
        );
        assert!(
            msg.contains("different operators") || msg.contains("explicit parens"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn operator_token_unbound_falls_through() {
        // An operator token NOT registered in scope is just left
        // alone — the surrounding production decides what to do
        // with the unexpected token. Here `+` after `x` makes the
        // function body `x` complete, then `+ y` is leftover and
        // fails the `}` expectation.
        let msg = p_err("module x; fn f(x: A) -> A { x + y }");
        // The exact wording depends on the surrounding parser
        // state; just verify we didn't try to apply an unknown op.
        assert!(!msg.contains("non-associative"), "unexpected: {msg}");
    }

    #[test]
    fn literals_in_body() {
        let m = p(
            r#"module x; fn f() -> . { let s = "hi"; let n = 42; let f = 3.14; let b = .t; () }"#,
        );
        if let Item::FnDef(d) = &m.items[0] {
            // Innermost call carries a BoolLit
            fn drill(e: &Expr) -> &Expr {
                match e {
                    Expr::Let { value, body, .. } => {
                        let _ = value;
                        drill(body)
                    }
                    _ => e,
                }
            }
            // Just make sure the structure is the chain of lets.
            let _ = drill(&d.body);
        }
    }

    #[test]
    fn bare_int_literal_has_no_annotation() {
        let m = p("module x; fn f() -> . { let n = 42; () }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Let { value, .. } = &d.body
        {
            match value.as_ref() {
                Expr::IntLit {
                    digits, annotation, ..
                } => {
                    assert_eq!(digits, "42");
                    assert!(annotation.is_none());
                }
                other => panic!("expected IntLit, got {other:?}"),
            }
        }
    }

    #[test]
    fn int_literal_call_form_carries_annotation() {
        // `100(I32)` — the trailing `(Type)` is the explicit
        // annotation, parsed onto the literal node.
        let m = p("module x; fn f() -> . { let n = 100(I32); () }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Let { value, .. } = &d.body
        {
            match value.as_ref() {
                Expr::IntLit {
                    digits,
                    annotation: Some(Type::Path { segments, .. }),
                    ..
                } => {
                    assert_eq!(digits, "100");
                    assert_eq!(segments[0].name, "I32");
                }
                other => panic!("expected annotated IntLit, got {other:?}"),
            }
        }
    }

    #[test]
    fn float_literal_call_form_carries_annotation() {
        let m = p("module x; fn f() -> . { let n = 3.14(F64); () }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Let { value, .. } = &d.body
        {
            match value.as_ref() {
                Expr::FloatLit {
                    digits,
                    annotation: Some(Type::Path { segments, .. }),
                    ..
                } => {
                    assert_eq!(digits, "3.14");
                    assert_eq!(segments[0].name, "F64");
                }
                other => panic!("expected annotated FloatLit, got {other:?}"),
            }
        }
    }

    #[test]
    fn dotted_path_in_value_position() {
        let m = p("module x; fn f() -> . { Foo.mk_foo() }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { callee, .. } = &d.body
        {
            match callee.as_ref() {
                Expr::Path { segments, .. } => {
                    assert_eq!(segments, &vec!["Foo".to_string(), "mk_foo".into()])
                }
                other => panic!("expected Path, got {other:?}"),
            }
        }
    }

    #[test]
    fn dotted_selection_is_not_a_generic_expression_suffix() {
        for source in ["(f).g", "f().g"] {
            assert!(
                p_expr_with_context(source, None).is_err(),
                "a dotted value path must start at its identifier head: {source}"
            );
        }
    }

    #[test]
    fn call_arg_capital_ident_routes_via_backtracking() {
        // After the call-arg backtracking fix: any Ident-headed
        // call-arg parses as expression first, then falls back to
        // type. A type-shaped path like `String` parses
        // successfully as `Expr::Path` and lands as `CallArg::Value`.
        // The typer's existing `Expr::Path` reinterpretation in
        // type-arg slots keeps such call sites kind-correct.
        let m = p(r#"module x; fn f() -> . { id(String, "hi") }"#);
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(
                matches!(&args[0], CallArg::Value(Expr::Path { .. })),
                "got: {:?}",
                args[0]
            );
            assert!(matches!(args[1], CallArg::Value(Expr::StrLit { .. })));
        }
    }

    #[test]
    fn failed_dot_lambda_call_arg_does_not_become_a_forall_type() {
        assert!(
            parse("module x; fn f() -> . { consume(.[A] A) }").is_err(),
            "a failed `.[` lambda parse must retain its leading dot when the call-arg type fallback runs"
        );
    }

    #[test]
    fn call_arg_paren_type_form_routes_to_type() {
        // `(A & B)` at a call-arg site is a type-only form. Backtracking:
        // the value-side `expr_paren` rejects `&` after the first item,
        // we restore and try `type_expr`, which succeeds.
        let m = p("module x; fn f() -> . { id((A & B), 0) }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(&args[0], CallArg::Type(Type::Product { .. })));
        }
    }

    #[test]
    fn call_arg_product_type_wins_over_amp_operator() {
        let m = p("module x; \
             fn both(a: A, b: A) -> A { a } \
             op _ & _ { impl both; }; \
             fn f() -> . { id(String & String, value) }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(&args[0], CallArg::Type(Type::Product { .. })));
            assert!(matches!(&args[1], CallArg::Value(Expr::Path { .. })));
        }
    }

    #[test]
    fn call_arg_paren_product_type_wins_over_amp_operator() {
        let m = p("module x; \
             fn both(a: A, b: A) -> A { a } \
             op _ & _ { impl both; }; \
             fn f() -> . { id((String & String), value) }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(&args[0], CallArg::Type(Type::Product { .. })));
            assert!(matches!(&args[1], CallArg::Value(Expr::Path { .. })));
        }
    }

    #[test]
    fn call_arg_prefix_bang_with_operand_stays_value_operator() {
        let m = p("module x; \
             fn invert(a: A) -> A { a } \
             op ! _ { impl invert; }; \
             fn f() -> . { id(! value, other) }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(&args[0], CallArg::Value(Expr::OpChain { .. })));
            assert!(matches!(&args[1], CallArg::Value(Expr::Path { .. })));
        }
    }

    #[test]
    fn call_arg_bare_bang_stays_bottom_type_with_prefix_operator_in_scope() {
        let m = p("module x; \
             fn invert(a: A) -> A { a } \
             op ! _ { impl invert; }; \
             fn f() -> . { id(!, value) }");
        if let Item::FnDef(d) = &m.items[2]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(&args[0], CallArg::Type(Type::Bottom { .. })));
            assert!(matches!(&args[1], CallArg::Value(Expr::Path { .. })));
        }
    }

    #[test]
    fn call_arg_comma_tuple_stays_value() {
        let m = p("module x; fn f() -> . { id((A, B), 0) }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(
                &args[0],
                CallArg::Value(Expr::Tuple { items, .. }) if items.len() == 2
            ));
        }
    }

    #[test]
    fn lowercase_identifier_is_not_a_type_expr() {
        let msg = p_err("module x; fn f(value: string) -> . { () }");
        assert_eq!(
            msg,
            "type name `string` must have an uppercase first letter"
        );
    }

    #[test]
    fn call_arg_lowercase_tuple_stays_value() {
        let m = p("module x; fn f() -> . { id((a, b), 0) }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(matches!(
                &args[0],
                CallArg::Value(Expr::Tuple { items, .. }) if items.len() == 2
            ));
        }
    }

    #[test]
    fn call_arg_leading_sum_type_form_routes_to_type() {
        // `| A | B` is valid type syntax in an elaborator target slot even
        // though it starts with a symbol run. The call-arg dispatcher
        // must try expression first, then fall back to `type_expr`.
        let m = p("module x; fn f() -> . { flatten_sum!(x, | A | B) }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::UserElaborator { args, .. } = &d.body else {
            panic!(
                "expected user elaborator with leading-sum target, got {:?}",
                d.body
            );
        };
        assert!(matches!(args.get(1), Some(CallArg::Type(Type::Sum { .. }))));
    }

    #[test]
    fn call_arg_member_call_routes_to_value() {
        // `List.un_list(a, xs)` at a call-arg site is a value-position
        // member call whose head spells like a type name. Pre-fix the
        // type-head heuristic forced this into `type_expr` and
        // surfaced a kind error; backtracking now parses it as an
        // expression call.
        let m = p("module x; \
             fn f() -> . { id(List.un_list(a, xs), 0) }");
        if let Item::FnDef(d) = &m.items[0]
            && let Expr::Call { args, .. } = &d.body
        {
            assert!(
                matches!(&args[0], CallArg::Value(Expr::Call { .. })),
                "got: {:?}",
                args[0]
            );
        }
    }

    // ---- trailing blocks ------------------------------------------------

    #[test]
    fn sequence_block_call_parses() {
        let m = p("module x; fn pure[A](x: A) -> A { x } \
             fn bind[A][B](xs: A, k: A -> B) -> B { k(xs) } \
             fn f() -> . { do! bind { let x <- (); pure(x) } }");
        let Item::FnDef(d) = &m.items[2] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call, got {:?}", d.body);
        };
        assert_eq!(blocks[0].items.len(), 2);
        assert!(matches!(
            blocks[0].items[0],
            crate::ast::NeutralItem::Binding { bind: true, .. }
        ));
        assert!(matches!(
            blocks[0].items[1],
            crate::ast::NeutralItem::Expression {
                value: Expr::Call { .. },
                ..
            }
        ));
    }

    #[test]
    fn scope_block_call_parses() {
        let m = p("module x; fn f() -> . { scope! { let x = (); x } }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call, got {:?}", d.body);
        };
        assert!(matches!(
            blocks[0].items[0],
            crate::ast::NeutralItem::Binding { bind: false, .. }
        ));
        assert!(matches!(
            blocks[0].items[1],
            crate::ast::NeutralItem::Expression { .. }
        ));
    }

    #[test]
    fn neutral_pure_binding_only_block_keeps_its_items() {
        let m = p("module x; fn f() -> . { scope! { let x = (); } }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call");
        };
        assert!(matches!(
            blocks[0].items.as_slice(),
            [crate::ast::NeutralItem::Binding { bind: false, .. }]
        ));
    }

    #[test]
    fn block_label_value_prefix_can_be_parenthesized() {
        let m = p("module x; fn f() -> . { do!({bind = f}) { () } }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { prefix, .. } = &d.body else {
            panic!("expected block call, got {:?}", d.body);
        };
        assert!(matches!(prefix.as_slice(), [Expr::LabelValue { .. }]));
    }

    #[test]
    fn match_elaborator_direct_bang_call_parses() {
        // `match!(v, (<arms>), B)` is parsed as an ordinary user
        // elaborator call; resolution supplies the call contract later.
        let m = p("module x; fn f(v: I32 | String) -> . { \
             match!(v, (.(n: I32) { () }, .(s: String) { () }), . | .) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::UserElaborator { name, args, .. } = &f.body else {
            panic!("expected match! user elaborator, got {:?}", f.body);
        };
        assert_eq!(name, "match");
        assert_eq!(args.len(), 3);
        assert!(matches!(
            args.first(),
            Some(CallArg::Value(Expr::Path { .. }))
        ));
        assert!(matches!(
            args.get(1),
            Some(CallArg::Value(Expr::Tuple { .. }))
        ));
        assert!(matches!(args.get(2), Some(CallArg::Type(Type::Sum { .. }))));
    }

    #[test]
    fn match_elaborator_arm_parses_nested_elaborator_with_leading_sum_target() {
        let m = p("module x; fn f(v: A) -> A | B { \
             match!(v, .(x: A) { flatten_sum!(x, | A | B) }) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::UserElaborator { name, args, .. } = &f.body else {
            panic!("expected match! user elaborator, got {:?}", f.body);
        };
        assert_eq!(name, "match");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn match_elaborator_ufcs_parses() {
        // `v.>match!((<arms>), B)` is parsed as ordinary elaborator-UFCS.
        // Desugar turns the receiver into the match scrutinee.
        let m = p("module x; fn f(v: I32 | String) -> . { \
             v.>match!((.(n: I32) { () }, .(s: String) { () }), ()) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            bang,
            ..
        } = &f.body
        else {
            panic!("expected match! UFCS, got {:?}", f.body);
        };
        assert!(
            matches!(receiver.as_ref(), Expr::Path { .. }),
            "receiver is retained"
        );
        assert_eq!(callee_segments, &vec!["match".to_owned()]);
        assert!(bang.is_some(), "bang is retained on elaborator-UFCS");
        assert_eq!(args.len(), 2, "arms plus explicit codomain");
        match &args[0] {
            CallArg::Value(Expr::Tuple { items, .. }) => {
                assert_eq!(items.len(), 2, "two arms in the ordinary arg tuple")
            }
            other => panic!("expected value tuple arms, got {other:?}"),
        }
    }

    #[test]
    fn derive_elaborator_bang_call_parses() {
        // `derive!(T, (c0, c1))` parses as an ordinary user
        // elaborator call.
        let m = p("module x; fn f() -> Monad(Maybe) { \
             derive!(Monad(Maybe), (maybe_monad, maybe_t_monad)) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::UserElaborator { name, args, .. } = &f.body else {
            panic!("expected derive! user elaborator, got {:?}", f.body);
        };
        assert_eq!(name, "derive");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn derive_elaborator_elided_target_parses() {
        // `derive!(_, c0)` parses as an ordinary user elaborator call;
        // the `_` argument is preserved for typechecking.
        let m = p("module x; fn f() -> Monad(Maybe) { \
             derive!(_, maybe_monad) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::UserElaborator { name, args, .. } = &f.body else {
            panic!("expected derive! user elaborator");
        };
        assert_eq!(name, "derive");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn derive_elaborator_ufcs_parses() {
        let m = p("module x; fn f() -> Monad(Maybe) { \
             maybe_monad.>derive!(Monad(Maybe)) }");
        let Item::FnDef(f) = &m.items[0] else {
            panic!("expected fn item");
        };
        let Expr::Ufcs {
            receiver,
            callee_segments,
            args,
            bang,
            ..
        } = &f.body
        else {
            panic!("expected derive! UFCS, got {:?}", f.body);
        };
        assert!(matches!(receiver.as_ref(), Expr::Path { .. }));
        assert_eq!(callee_segments, &vec!["derive".to_owned()]);
        assert!(bang.is_some());
        assert_eq!(args.len(), 1, "target type supplied as ordinary arg");
    }

    #[test]
    fn neutral_block_binding_and_expression_forms_parse() {
        let m = p("module x; \
             fn bind[A][B](xs: A, k: A -> B) -> B { k(xs) } \
             fn unit_step() -> . { () } \
             fn f() -> . { \
               do! bind { \
                 let x <- (); \
                 let y = (); \
                 unit_step(); \
                 () \
               } \
             }");
        let Item::FnDef(d) = &m.items[2] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call");
        };
        let items = &blocks[0].items;
        assert_eq!(items.len(), 4);
        assert!(matches!(
            items[0],
            crate::ast::NeutralItem::Binding { bind: true, .. }
        ));
        assert!(matches!(
            items[1],
            crate::ast::NeutralItem::Binding { bind: false, .. }
        ));
        assert!(matches!(
            items[2],
            crate::ast::NeutralItem::Expression {
                value: Expr::Call { .. },
                ..
            }
        ));
        assert!(matches!(
            items[3],
            crate::ast::NeutralItem::Expression {
                value: Expr::Unit { .. },
                ..
            }
        ));
    }

    #[test]
    fn neutral_block_repeated_and_trailing_separators_parse() {
        let m = p("module x; \
             fn bind[A][B](xs: A, k: A -> B) -> B { k(xs) } \
             fn f() -> . { do! bind { ;;; ();;; ();;; } }");
        let Item::FnDef(d) = &m.items[1] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call, got {:?}", d.body);
        };
        assert_eq!(blocks[0].items.len(), 2);
        assert!(blocks[0].items.iter().all(|item| matches!(
            item,
            crate::ast::NeutralItem::Expression {
                value: Expr::Unit { .. },
                ..
            }
        )));
        assert_eq!(blocks[0].separators.len(), 9);
    }

    #[test]
    fn neutral_bind_only_block_keeps_its_items() {
        let m = p("module x; \
             fn bind[A][B](xs: A, k: A -> B) -> B { k(xs) } \
             fn f() -> . { do! bind { let x <- (); } }");
        let Item::FnDef(d) = &m.items[1] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call");
        };
        assert!(matches!(
            blocks[0].items.as_slice(),
            [crate::ast::NeutralItem::Binding { bind: true, .. }]
        ));
    }

    #[test]
    fn neutral_empty_body_is_preserved() {
        let m = p("module x; \
             fn bind[A][B](xs: A, k: A -> B) -> B { k(xs) } \
             fn f() -> . { do! bind { } }");
        let Item::FnDef(d) = &m.items[1] else {
            panic!("expected fn");
        };
        let Expr::BlockCall { blocks, .. } = &d.body else {
            panic!("expected block call");
        };
        assert!(blocks[0].items.is_empty());
    }

    #[test]
    fn neutral_bind_outside_a_trailing_block_is_an_error() {
        // `let x <- e;` at ordinary block scope: the let-stmt's `=`-
        // only rule rejects with a diagnostic pointing at the
        // unexpected `<-`.
        let msg = p_err("module x; fn f() -> . { let x <- (); () }");
        // The block-let parser expects `=`; encountering `<-`
        // surfaces as a connective mismatch.
        assert!(msg.contains("`=`") || msg.contains("<-"), "got: {msg}");
    }

    // ---- equiv -----------------------------------------------------------

    #[test]
    fn equiv_two_terms_parses() {
        let m = p("module x; fn id[A](v: A) -> A { v } \
             equiv id_id[A](x: A) { id(x); id(id(x)) }");
        assert_eq!(m.items.len(), 2);
        match &m.items[1] {
            Item::Equiv(e, _) => {
                assert_eq!(e.name, "id_id");
                assert_eq!(e.sig.params.len(), 2);
                assert_eq!(e.terms.len(), 2);
            }
            other => panic!("expected Equiv, got {other:?}"),
        }
    }

    #[test]
    fn equiv_n_terms_with_repeated_and_trailing_semicolons_parses() {
        let m = p("module x; \
             equiv three { ;; ();;; ();; ();;;; }");
        match &m.items[0] {
            Item::Equiv(e, _) => {
                assert_eq!(e.name, "three");
                assert_eq!(e.terms.len(), 3);
            }
            other => panic!("expected Equiv, got {other:?}"),
        }
    }

    #[test]
    fn equiv_no_params_parses() {
        let m = p("module x; equiv simple { (); () }");
        match &m.items[0] {
            Item::Equiv(e, _) => {
                assert!(e.sig.params.is_empty());
                assert_eq!(e.terms.len(), 2);
            }
            other => panic!("expected Equiv, got {other:?}"),
        }
    }

    #[test]
    fn equiv_comma_arms_rejected_with_separator_message() {
        let msg = p_err("module x; equiv simple { (), () }");
        assert!(msg.contains("separated with `;`, not `,`"), "got: {msg}");
        assert!(msg.contains(".() { ... }()"), "got: {msg}");
    }

    #[test]
    fn equiv_singleton_rejected() {
        // `N=1` is a hard error per the spec.
        let msg = p_err("module x; equiv lonely { () }");
        assert!(msg.contains("at least two"), "got: {msg}");
    }

    #[test]
    fn equiv_empty_rejected() {
        // `N=0` is a hard error per the spec.
        let msg = p_err("module x; equiv empty { }");
        assert!(msg.contains("at least two"), "got: {msg}");
    }

    #[test]
    fn equiv_pub_rejected() {
        // `equiv` is not a public-API form.
        let msg = p_err("module x; pub equiv e { (); () }");
        assert!(msg.contains("pub"), "got: {msg}");
    }

    #[test]
    fn equiv_anonymous_rejected() {
        // Names are required; the parser expects an identifier here.
        let msg = p_err("module x; equiv { (); () }");
        // Whatever the exact message, the failure happens at the
        // missing-identifier slot, not later.
        assert!(!msg.is_empty(), "got: {msg}");
    }

    #[test]
    fn if_else_parses() {
        let m = p("module x; fn f() -> . { if! .t { () } else { () } }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(
                matches!(&d.body, Expr::BlockCall { head, blocks, .. } if head.name == "if" && blocks.len() == 2)
            );
        }
    }

    #[test]
    fn if_condition_before_block_parses() {
        let m = p("module x; fn pick[A](c: Bool, t: A, e: A) -> A { if! c { t } else { e } }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(
                matches!(&d.body, Expr::BlockCall { prefix, blocks, .. } if prefix.len() == 1 && blocks.len() == 2)
            );
        }
    }

    #[test]
    fn leading_dot_bool_literals_parse() {
        let m = p("module x; fn f() -> . { (.t, .f) }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Tuple { items, .. } => {
                    assert!(matches!(items[0], Expr::BoolLit { value: true, .. }));
                    assert!(matches!(items[1], Expr::BoolLit { value: false, .. }));
                }
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
    }

    #[test]
    fn leading_dot_let_statement_is_rejected() {
        assert!(parse("module x; fn f() -> . { .x = (); x }").is_err());
    }

    #[test]
    fn leading_dot_fn_literals_parse() {
        let m = p("module x; fn f() -> . { .(x) { x } }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.body, Expr::FnExpr { .. }));
        }
    }

    #[test]
    fn leading_dot_fn_literals_parse_type_binder_group() {
        let m = p("module x; fn f() -> . { .[A](x: A) { x } }");
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::FnExpr { sig, .. } => {
                    assert!(matches!(sig.params[0], SignatureParam::Type(_)));
                    assert!(matches!(sig.params[1], SignatureParam::Value(_)));
                }
                other => panic!("expected FnExpr, got {other:?}"),
            }
        }
    }

    #[test]
    fn leading_dot_placeholder_lambda_parses() {
        let m = p("module x; fn f() -> . { .x. { x1 } }");
        if let Item::FnDef(d) = &m.items[0] {
            assert!(matches!(d.body, Expr::FnPlaceholder { .. }));
        }
    }

    #[test]
    fn expression_keywords_parse_as_function_names() {
        let m = p("module x; \
             fn if() -> . { () } \
             fn else() -> . { () } \
             fn do() -> . { () } \
             fn use_all() -> . { let f = if; let _ = f(); let _ = else(); do() }");
        assert_eq!(m.items.len(), 4);
    }

    #[test]
    fn fold_keyword_parses_as_function_name() {
        let m = p("module x; \
             pub fn fold[A][B](f: (B & A) -> B, seed: B, xs: Seq(A)) -> B { seed }");
        assert_eq!(m.items.len(), 1);
        assert!(matches!(&m.items[0], Item::FnDef(d) if d.name == "fold"));
    }

    #[test]
    fn wholly_lenient_operator_after_function_keeps_its_item_boundary() {
        let source = "module x;
            fn op(left: ., right: .) -> . { left }
            pub op (_ ? _) { impl op; };
            fn run() -> . { () ? () }";
        for module in [
            parse(source).expect("eager lenient operator"),
            parse_lazy(source)
                .expect("lazy lenient operator")
                .force_all()
                .expect("materialized lenient operator"),
        ] {
            assert_eq!(module.items.len(), 3);
            assert!(matches!(&module.items[0], Item::FnDef(def) if def.name == "op"));
            let Item::Op(declaration, _) = &module.items[1] else {
                panic!("separate lenient operator declaration");
            };
            let crate::ast::OpBody::Normal { pattern, .. } = &declaration.body;
            assert_eq!(
                crate::ast::OperatorGrammar::fixed(pattern).render(),
                "op (_) ? (_)"
            );
            assert!(matches!(&module.items[2], Item::FnDef(def) if def.name == "run"));
        }
    }

    #[test]
    fn unmarked_if_block_is_not_an_elaborator_call() {
        let msg = p_err("module x; fn f() -> . { if .t { () } }");
        assert!(msg.contains("expected `}`"), "got: {msg}");
        let m = p("module x; fn f() -> . { if! .t { () } }");
        assert!(matches!(
            &m.items[0],
            Item::FnDef(d) if matches!(&d.body, Expr::BlockCall { head, blocks, .. }
                if head.name == "if" && blocks.len() == 1)
        ));
    }

    #[test]
    fn label_forward_preserves_written_binding_and_target_spans() {
        let source = "module x; pub(x) type {local} = {provider.original};";
        let module = p(source);
        let Item::LabelForward(forward, ()) = &module.items[0] else {
            panic!("expected label forwarding declaration");
        };
        assert_eq!(forward.name, "local");
        assert_eq!(forward.target, "provider.original");
        assert!(
            matches!(&forward.vis, crate::ast::Visibility::PublicIn(path) if path.segments == ["x"])
        );
        assert_eq!(
            &source[forward.name_span.start as usize..forward.name_span.end as usize],
            "local"
        );
        assert_eq!(
            &source[forward.target_span.start as usize..forward.target_span.end as usize],
            "provider.original"
        );
        let editable = forward.editable_span.expect("complete written declaration");
        assert_eq!(
            &source[editable.start as usize..editable.end as usize],
            "pub(x) type {local} = {provider.original};"
        );
    }

    #[test]
    fn label_forward_is_not_a_type_alias_application_or_recursive_member() {
        for source in [
            "module x; type {local}[A] = {original};",
            "module x; type {local} = {original(A)};",
            "module x; type {local} = {original: .};",
            "module x; type {local} = Original;",
            "module x; type {local, other} = {original};",
            "module x; type {local} = {original, other};",
            "module x; type {Upper} = {original};",
            "module x; type {_local} = {original};",
            "module x; rec type {local} = {original};",
            "module x; rec { type {local} = {original}; }",
        ] {
            assert!(parse(source).is_err(), "unexpectedly parsed {source}");
        }
        let module = p("module x; type {local} = {original}; type Alias[A] = Original(A);");
        assert!(matches!(&module.items[0], Item::LabelForward(_, ())));
        assert!(matches!(&module.items[1], Item::TypeAlias(alias) if alias.type_params.len() == 1));
    }

    #[test]
    fn labels_anonymous_single_entry() {
        let m = p("module x; labels { foo : . };");
        match &m.items[0] {
            Item::Labels(d, _) => {
                assert!(d.type_alias_name.is_none());
                assert!(d.type_alias_params.is_empty());
                assert_eq!(d.entries.len(), 1);
                assert_eq!(d.entries[0].name, "foo");
                assert!(d.entries[0].type_params.is_empty());
                assert!(matches!(d.entries[0].payload, Type::Unit { .. }));
            }
            other => panic!("expected Labels, got {other:?}"),
        }
    }

    #[test]
    fn labels_anonymous_multi_entry() {
        let m = p("module x; labels { foo : ., bar : ., baz : . };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert_eq!(d.entries.len(), 3);
            let names: Vec<&str> = d.entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["foo", "bar", "baz"]);
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_pub_form() {
        let m = p("module x; pub labels { foo : . };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert!(d.vis.is_pub());
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_named_form_introduces_type_alias_name() {
        let m = p("module x; labels T = { foo : . };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert_eq!(d.type_alias_name.as_deref(), Some("T"));
            assert!(d.type_alias_params.is_empty());
            assert_eq!(d.entries.len(), 1);
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_parametric_named_form() {
        let m = p("module x; labels T[A] = { foo[A] : A };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert_eq!(d.type_alias_name.as_deref(), Some("T"));
            assert_eq!(d.type_alias_params.len(), 1);
            assert_eq!(d.type_alias_params[0].name, "A");
            assert_eq!(d.entries.len(), 1);
            assert_eq!(d.entries[0].type_params.len(), 1);
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_reuse_marker_parses_as_exact_infer_payload() {
        let m = p(
            "module x; labels { value[*F][A] : F(A) }; labels Choice[*G][B] = { value[*G][B] : _, other : . };",
        );
        let Item::Labels(labels, _) = &m.items[1] else {
            panic!("expected named labels declaration");
        };
        assert!(labels.entries[0].is_reuse_marker());
        assert!(!labels.entries[1].is_reuse_marker());
    }

    #[test]
    fn nested_infer_payload_is_not_a_reuse_marker() {
        let m = p("module x; labels { value[*F] : F(_) };");
        let Item::Labels(labels, _) = &m.items[0] else {
            panic!("expected labels declaration");
        };
        assert!(!labels.entries[0].is_reuse_marker());
    }

    #[test]
    fn grouped_infer_payload_is_not_a_reuse_marker() {
        let message = p_err("module x; labels Row = { field : (_) };");
        assert!(message.contains("exact bare `_`"), "got: {message}");
    }

    #[test]
    fn labels_same_arm_explicit_and_reuse_is_a_parse_error() {
        let message = p_err("module x; labels Choice = { value : ., value : _ };");
        assert!(
            message.contains("duplicate label `value`"),
            "got: {message}"
        );
    }

    #[test]
    fn qualified_label_reuse_is_a_directed_parse_error() {
        let message = p_err("module x; labels Choice = { dep.value : _ };");
        assert!(
            message.contains("unqualified module-local"),
            "got: {message}"
        );
    }

    #[test]
    fn labels_entry_with_existentials() {
        // Same header shape as `newtype`: existentials trail the
        // universal-parameter list on the entry.
        let m = p("module x; labels { dproduct[A] <L> <R> : A & L & R };");
        if let Item::Labels(d, _) = &m.items[0] {
            let e = &d.entries[0];
            assert_eq!(e.name, "dproduct");
            assert_eq!(e.type_params.len(), 1);
            assert_eq!(e.type_params[0].name, "A");
            assert_eq!(e.existential_params.len(), 2);
            assert_eq!(e.existential_params[0].name, "L");
            assert_eq!(e.existential_params[1].name, "R");
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_entry_existentials_without_universals() {
        let m = p("module x; labels { pack <U> : U };");
        if let Item::Labels(d, _) = &m.items[0] {
            let e = &d.entries[0];
            assert!(e.type_params.is_empty());
            assert_eq!(e.existential_params.len(), 1);
            assert_eq!(e.existential_params[0].name, "U");
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_entry_no_existentials_keeps_field_empty() {
        let m = p("module x; labels { foo[A] : A };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert!(d.entries[0].existential_params.is_empty());
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_entry_existential_in_payload_is_parse_error() {
        // Standalone existential type expressions no longer parse in
        // labels entries, and the diagnostic points at the header
        // alternative.
        let msg = p_err("module x; labels { foo[A] : (<U>, A & U) };");
        assert!(
            msg.contains("existential type expressions are no longer admissible")
                || msg.contains("declared on a `newtype` header"),
            "want existential-rejection diagnostic, got: {msg}"
        );
    }

    #[test]
    fn labels_trailing_comma_allowed() {
        let m = p("module x; labels { foo : ., bar : ., };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert_eq!(d.entries.len(), 2);
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn labels_empty_block_rejected() {
        let msg = p_err("module x; labels { };");
        assert!(msg.contains("at least one label"), "got: {msg}");
    }

    #[test]
    fn labels_named_must_use_a_type_name() {
        let msg = p_err("module x; labels t = { foo : . };");
        assert!(msg.contains("type name"), "got: {msg}");
    }

    #[test]
    fn labels_entry_name_must_be_lowercase() {
        let msg = p_err("module x; labels { Foo : . };");
        assert!(
            msg.contains("label name `Foo` must have a lowercase first letter"),
            "got: {msg}"
        );
    }

    #[test]
    fn labels_self_reference_payload_legal() {
        // A `labels` entry's payload may reference its own label —
        // `labels` bodies inherit the lifted-scope rule from
        // `newtype` bodies, so the entry's name resolves inside its
        // own payload.
        let m = p("module x; labels { list[A] : (. | (A & List(A))) };");
        if let Item::Labels(d, _) = &m.items[0] {
            assert_eq!(d.entries[0].name, "list");
        } else {
            panic!("expected Labels");
        }
    }

    #[test]
    fn field_access_postfix_parses() {
        let m = p("module x; fn read(r: Foo) -> . { r.?{foo} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Access,
                call: ElaboratorCall::FieldAccess { labels, .. },
                ..
            } => {
                assert_eq!(labels.len(), 1);
                assert_eq!(labels[0].label, "foo");
            }
            other => panic!("expected field access elaborator, got {other:?}"),
        }
    }

    #[test]
    fn qualified_field_access_postfix_parses() {
        let m = p("module x; fn read(r: Foo) -> . { r.?{m.foo, m.bar} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Access,
                call: ElaboratorCall::FieldAccess { labels, .. },
                ..
            } => {
                let got: Vec<&str> = labels.iter().map(|label| label.label.as_str()).collect();
                assert_eq!(got, vec!["m.foo", "m.bar"]);
            }
            other => panic!("expected field access elaborator, got {other:?}"),
        }
    }

    #[test]
    fn field_update_postfix_parses() {
        let m = p("module x; fn write(r: Foo) -> . { r.!{foo = ()} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Filtered,
                call: ElaboratorCall::FieldUpdate { updates, .. },
                ..
            } => {
                assert_eq!(updates.len(), 1);
                assert_eq!(updates[0].label, "foo");
            }
            other => panic!("expected field update elaborator, got {other:?}"),
        }
    }

    #[test]
    fn field_access_empty_postfix_parses() {
        let m = p("module x; fn read(r: Foo) -> . { r.?{} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Access,
                call: ElaboratorCall::FieldAccess { labels, .. },
                ..
            } => {
                assert!(labels.is_empty());
            }
            other => panic!("expected field access elaborator, got {other:?}"),
        }
    }

    #[test]
    fn field_update_shorthand_postfix_parses() {
        let m = p("module x; fn write(r: Foo) -> . { r.!{foo, bar=} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Filtered,
                call: ElaboratorCall::FieldUpdate { updates, .. },
                ..
            } => {
                assert_eq!(updates.len(), 2);
                assert_eq!(updates[0].label, "foo");
                assert!(matches!(updates[0].value, Expr::Path { .. }));
                assert_eq!(updates[1].label, "bar");
                assert!(matches!(updates[1].value, Expr::Unit { .. }));
            }
            other => panic!("expected field update elaborator, got {other:?}"),
        }
    }

    #[test]
    fn field_update_empty_postfix_parses() {
        let m = p("module x; fn write(r: Foo) -> . { r.!{} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::Elaborator {
                kind: ElaboratorKind::Filtered,
                call: ElaboratorCall::FieldUpdate { updates, .. },
                ..
            } => {
                assert!(updates.is_empty());
            }
            other => panic!("expected field update elaborator, got {other:?}"),
        }
    }

    #[test]
    fn label_value_shorthands_parse() {
        let m = p("module x; fn make(foo: .) -> . { {foo, m.bar=} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::LabelValue { labels, .. } => {
                assert_eq!(labels.len(), 2);
                assert_eq!(labels[0].label, "foo");
                assert!(matches!(labels[0].value, Expr::Path { .. }));
                assert_eq!(labels[1].label, "m.bar");
                assert!(matches!(labels[1].value, Expr::Unit { .. }));
            }
            other => panic!("expected label value, got {other:?}"),
        }
    }

    #[test]
    fn empty_label_value_is_unit() {
        let m = p("module x; fn make() -> . { {} }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        assert!(matches!(d.body, Expr::Unit { .. }));
    }

    #[test]
    fn row_let_aliases_parse() {
        let m =
            p("module x; fn read(row: Foo) -> . { let .({abc, m.def as payload}) = row; payload }");
        let Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        match &d.body {
            Expr::RowLet { entries, body, .. } => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].label, "abc");
                assert_eq!(entries[0].local, "abc");
                assert!(!entries[0].alias_explicit);
                assert_eq!(entries[1].label, "m.def");
                assert_eq!(entries[1].local, "payload");
                assert!(entries[1].alias_explicit);
                assert!(matches!(body.as_ref(), Expr::Path { .. }));
            }
            other => panic!("expected row-let, got {other:?}"),
        }
    }

    fn assert_user_elaborator_name(src: &str, expected: &str) {
        let m = p(src);
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::UserElaborator { name, .. } => assert_eq!(name, expected),
                other => panic!("expected Expr::UserElaborator for {src:?}, got {other:?}"),
            }
        } else {
            panic!("expected fn for {src:?}");
        }
    }

    #[test]
    fn iso_bang_call_parses() {
        assert_user_elaborator_name("module x; fn f() -> . { iso!(x) }", "iso");
    }

    #[test]
    fn into_bang_call_parses() {
        assert_user_elaborator_name("module x; fn f() -> . { into!(x) }", "into");
    }

    #[test]
    fn onto_bang_call_parses() {
        assert_user_elaborator_name("module x; fn f() -> . { onto!(x) }", "onto");
    }

    #[test]
    fn ease_bang_call_parses() {
        assert_user_elaborator_name("module x; fn f() -> . { ease!(x) }", "ease");
    }

    #[test]
    fn ease_ufcs_parses() {
        // Receiver-first UFCS form `r.>ease!(T)` is still a `Ufcs`
        // node at parse time; verify it parses without error.
        let _m = p("module x; fn f() -> . { x.>ease!(()) }");
    }

    // ---- Spine-palette bang calls -------------------------------------
    //
    // The spine forms parse as user-elaborator bang calls plus
    // right-callee bang UFCS splices.

    fn assert_elaborator_kind(src: &str, expected: &str) {
        let m = p(src);
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::UserElaborator { name, .. } => assert_eq!(
                    name, expected,
                    "wrong user elaborator for {src:?}: expected {expected:?}, got {name:?}"
                ),
                other => panic!("expected Expr::UserElaborator for {src:?}, got {other:?}"),
            }
        } else {
            panic!("expected fn for {src:?}");
        }
    }

    fn assert_elaborator_ufcs(src: &str, expected_segment: &str) {
        let m = p(src);
        if let Item::FnDef(d) = &m.items[0] {
            match &d.body {
                Expr::Ufcs {
                    callee_segments,
                    bang,
                    ..
                } => {
                    assert!(bang.is_some(), "expected bang-call UFCS for {src:?}");
                    assert_eq!(
                        callee_segments,
                        &vec![expected_segment.to_owned()],
                        "wrong segment for {src:?}"
                    );
                }
                other => panic!("expected Expr::Ufcs for {src:?}, got {other:?}"),
            }
        } else {
            panic!("expected fn for {src:?}");
        }
    }

    #[test]
    fn reorder_sum_bang_call_parses() {
        assert_elaborator_kind(
            "module x; fn f() -> . { reorder_sum!(x, ()) }",
            "reorder_sum",
        );
    }

    #[test]
    fn reorder_sum_ufcs_parses() {
        assert_elaborator_ufcs(
            "module x; fn f() -> . { x.>reorder_sum!(()) }",
            "reorder_sum",
        );
    }

    #[test]
    fn reorder_prod_bang_call_parses() {
        assert_elaborator_kind(
            "module x; fn f() -> . { reorder_prod!(x, ()) }",
            "reorder_prod",
        );
    }

    #[test]
    fn reorder_prod_ufcs_parses() {
        assert_elaborator_ufcs(
            "module x; fn f() -> . { x.>reorder_prod!(()) }",
            "reorder_prod",
        );
    }

    #[test]
    fn narrow_sum_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { narrow_sum!(x, ()) }", "narrow_sum");
    }

    #[test]
    fn narrow_sum_ufcs_parses() {
        assert_elaborator_ufcs("module x; fn f() -> . { x.>narrow_sum!(()) }", "narrow_sum");
    }

    #[test]
    fn narrow_prod_bang_call_parses() {
        assert_elaborator_kind(
            "module x; fn f() -> . { narrow_prod!(x, ()) }",
            "narrow_prod",
        );
    }

    #[test]
    fn narrow_prod_ufcs_parses() {
        assert_elaborator_ufcs(
            "module x; fn f() -> . { x.>narrow_prod!(()) }",
            "narrow_prod",
        );
    }

    #[test]
    fn widen_sum_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { widen_sum!(x, ()) }", "widen_sum");
    }

    #[test]
    fn widen_sum_ufcs_parses() {
        assert_elaborator_ufcs("module x; fn f() -> . { x.>widen_sum!(()) }", "widen_sum");
    }

    #[test]
    fn widen_prod_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { widen_prod!(x, ()) }", "widen_prod");
    }

    #[test]
    fn widen_prod_ufcs_parses() {
        assert_elaborator_ufcs("module x; fn f() -> . { x.>widen_prod!(()) }", "widen_prod");
    }

    #[test]
    fn flatten_sum_bang_call_parses() {
        assert_elaborator_kind(
            "module x; fn f() -> . { flatten_sum!(x, ()) }",
            "flatten_sum",
        );
    }

    #[test]
    fn flatten_sum_ufcs_parses() {
        assert_elaborator_ufcs(
            "module x; fn f() -> . { x.>flatten_sum!(()) }",
            "flatten_sum",
        );
    }

    #[test]
    fn flatten_prod_bang_call_parses() {
        assert_elaborator_kind(
            "module x; fn f() -> . { flatten_prod!(x, ()) }",
            "flatten_prod",
        );
    }

    #[test]
    fn flatten_prod_ufcs_parses() {
        assert_elaborator_ufcs(
            "module x; fn f() -> . { x.>flatten_prod!(()) }",
            "flatten_prod",
        );
    }

    #[test]
    fn one_sum_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { one_sum!(x, ()) }", "one_sum");
    }

    #[test]
    fn one_sum_ufcs_parses() {
        // `one_sum!` accepts target-elided UFCS; parens-iff-args drops
        // the parens entirely when no target is supplied.
        assert_elaborator_ufcs("module x; fn f() -> . { x.>one_sum! }", "one_sum");
    }

    #[test]
    fn one_prod_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { one_prod!(x, ()) }", "one_prod");
    }

    #[test]
    fn one_prod_ufcs_parses() {
        assert_elaborator_ufcs("module x; fn f() -> . { x.>one_prod! }", "one_prod");
    }

    #[test]
    fn fit_bang_call_parses() {
        assert_elaborator_kind("module x; fn f() -> . { fit!(x, ()) }", "fit");
    }

    #[test]
    fn fit_ufcs_parses() {
        assert_elaborator_ufcs("module x; fn f() -> . { x.>fit!(()) }", "fit");
    }

    // ---- placeholder lambdas -----------------------------------------

    #[test]
    fn placeholder_stems_preserve_source_references() {
        for stem in ["x", "arg", "_p", "x1_y", "t", "f"] {
            let source = format!("module m; fn run() {{ .{stem}. {{ pair({stem}2, {stem}1) }} }}");
            let module = p(&source);
            let Item::FnDef(def) = &module.items[0] else {
                panic!("function")
            };
            let Expr::FnPlaceholder {
                stem: parsed_stem,
                state,
                body,
                ..
            } = &def.body
            else {
                panic!("placeholder");
            };
            assert_eq!(parsed_stem.name, stem);
            assert_eq!(
                *state,
                crate::ast::PlaceholderState::Source { slot_count: 2 }
            );
            let Expr::Call { args, .. } = body.as_ref() else {
                panic!("call")
            };
            let CallArg::Value(Expr::Path { segments, .. }) = &args[0] else {
                panic!("reference")
            };
            assert_eq!(segments[0].name, format!("{stem}2"));
        }
    }

    #[test]
    fn placeholder_arity_obeys_all_source_local_binding_regions() {
        for (body, expected) in [
            ("pair(x2, x1, x2)", 2),
            ("let x2 = x1; x2", 1),
            ("let x2 = x2; x2", 2),
            (".(x2) { x1 }", 1),
            ("let .(x2, x3) = x1; pair(x2, x3)", 1),
            ("let .(whole: (x2: Int, x3: Int)) = x1; pair(x2, x3)", 1),
            ("let .({foo as x2}) = x1; x2", 1),
            ("do! m { let x2 <- x1; let x3 = x2; x3 }", 1),
            ("pair(x1, .y. { pair(x2, y1) })", 1),
            ("pair(x1, m.x9)", 1),
            ("value.>x1", 1),
            ("pair(x1, x9!(x2))", 2),
            ("let x01 = x1; x01", 1),
            ("{field = x1}", 1),
            ("{x1}", 1),
            ("let .(<U> x2) = x1; x2", 1),
        ] {
            let source = format!("module m; fn run() {{ .x. {{ {body} }} }}");
            let mut module = p(&source);
            let Item::FnDef(def) = &mut module.items[0] else {
                panic!("function")
            };
            let Expr::FnPlaceholder {
                stem, body, meta, ..
            } = &mut def.body
            else {
                panic!("placeholder");
            };
            let references =
                crate::pass::placeholder::classify(&stem.name, body, meta.span).unwrap();
            assert_eq!(references.slot_count, expected, "{source}");
        }
    }

    #[test]
    fn placeholder_invalid_stems_and_indices_reject_eager_and_lazy() {
        for expression in [
            ".x1. { x11 }",
            ".x_. { x_1 }",
            ". x. { x1 }",
            ".x . { x1 }",
            ".(x: Int). { x1 }",
            ".x. { x0 }",
            ".x. { x01 }",
            ".x. { x4294967296 }",
            ".x. { x }",
            ".x. { m.x1 }",
            ".x. { .y. { y1 } }",
            ".x. { let x1 = (); x1 }",
            ".# { #1 }",
            ".$$$$ { $$$$2 }",
            ".# { # }",
        ] {
            let source = format!("module m; fn run() {{ {expression} }}");
            assert!(parse(&source).is_err(), "{source}");
            assert!(
                parse_lazy(&source)
                    .and_then(|module| module.force_all())
                    .is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn placeholder_no_reference_diagnostic() {
        let message = p_err("module m; fn run() { .x. { () } }");
        assert!(message.contains("at least one unshadowed"), "{message}");
        p("module m; fn run() { .() { () } }");
    }

    // ---- Build block body (`build { ... }` in `<name>.pkg.kio`) ---

    fn pb(src: &str) -> BuildBlock {
        parse_build_block_body(src)
            .unwrap_or_else(|e| panic!("parse_build_block_body failed for {src:?}: {e:?}"))
    }

    fn pb_err(src: &str) -> String {
        match parse_build_block_body(src) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {src:?}"),
        }
    }

    #[test]
    fn build_block_minimal() {
        // The `cache` declaration alone is a legal (target-less) build
        // block — every block must declare it, but `target` blocks are
        // optional. (The `kio build` driver rejects target-less
        // packages at command time, but the grammar admits them.)
        let bf = pb("cache ();");
        assert!(matches!(bf.cache, BuildBlockCache::Disabled { .. }));
        assert!(bf.targets.is_empty());
    }

    #[test]
    fn build_block_cache_path() {
        let bf = pb("cache \"out/.kio-cache/\";\ntarget js { out \"out/js/\"; }");
        match bf.cache {
            BuildBlockCache::Path { ref path, .. } => assert_eq!(path, "out/.kio-cache/"),
            other => panic!("expected Path cache, got {other:?}"),
        }
    }

    #[test]
    fn build_block_one_target_one_entry() {
        let bf = pb("cache ();\ntarget js { out \"out/js/\"; }");
        assert_eq!(bf.targets.len(), 1);
        let t = &bf.targets[0];
        assert_eq!(t.id, "js");
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].key, "out");
        assert_eq!(t.entries[0].value, "out/js/");
    }

    #[test]
    fn build_block_hyphenated_target_id() {
        // `kio-prime` spells as one bare kebab id; the lexer splits it
        // into `kio`, `-`, `prime` and the parser reassembles the run.
        let bf = pb("cache ();\ntarget kio-prime { out \"out/kio-prime/\"; }");
        assert_eq!(bf.targets.len(), 1);
        assert_eq!(bf.targets[0].id, "kio-prime");
    }

    #[test]
    fn build_block_target_block_can_be_empty() {
        let bf = pb("cache ();\ntarget js { }");
        assert_eq!(bf.targets.len(), 1);
        assert!(bf.targets[0].entries.is_empty());
    }

    #[test]
    fn build_block_multiple_targets() {
        let bf = pb("cache ();\n\
             target js { out \"out/js/\"; }; \
             target wasm { out \"out/wasm/\"; }");
        assert_eq!(bf.targets.len(), 2);
        assert_eq!(bf.targets[0].id, "js");
        assert_eq!(bf.targets[1].id, "wasm");
    }

    #[test]
    fn build_block_multiple_entries_in_block() {
        let bf = pb("cache ();\n\
             target js { \
               out \"out/js/\"; \
               module_format \"esm\"; \
             }");
        let t = &bf.targets[0];
        assert_eq!(t.entries.len(), 2);
        assert_eq!(t.entries[0].key, "out");
        assert_eq!(t.entries[1].key, "module_format");
        assert_eq!(t.entries[1].value, "esm");
    }

    #[test]
    fn build_block_quoted_target_id_rejected() {
        // The old quoted form is rejected with a migration message
        // naming the bare-identifier form.
        let msg = pb_err("cache ();\ntarget \"js\" { out \"out/\"; }");
        assert!(msg.contains("bare identifier"), "got: {msg}");
    }

    #[test]
    fn build_block_value_must_be_string_literal() {
        let msg = pb_err("cache ();\ntarget js { out 42; }");
        assert!(msg.contains("string-literal value"), "got: {msg}");
    }

    #[test]
    fn build_block_value_kind_error_precedes_missing_semicolon() {
        let msg = pb_err("cache ();\ntarget js { out () namespace \"app\" }");
        assert!(msg.contains("string-literal value"), "got: {msg}");
    }

    #[test]
    fn build_block_boolean_value_reports_field_kind() {
        let msg = pb_err("cache ();\ntarget js { out true; }");
        assert!(msg.contains("string-literal value"), "got: {msg}");
    }

    #[test]
    fn build_block_unknown_top_level_token_errors() {
        // `module` at the top level of a build block is not allowed.
        // `cache` is optional (it defaults to disabled), so an unknown
        // leading token is rejected where a `target` block is expected.
        let msg = pb_err("module foo;");
        assert!(msg.contains("`target`"), "got: {msg}");
    }

    #[test]
    fn build_block_missing_peer_semicolon_errors() {
        let msg = pb_err("cache ();\ntarget js { out \"out/\" namespace \"app\" }");
        assert_eq!(msg, "expected `;` between block entries");
    }

    #[test]
    fn build_block_missing_cache_defaults_to_disabled() {
        // `cache` is the build block's optional field: an omitted cache
        // declaration defaults to caching disabled (`kio fmt` then
        // inserts `cache ();`).
        let bf = pb("target js { out \"out/js/\"; }");
        assert!(matches!(bf.cache, BuildBlockCache::Disabled { .. }));
        assert_eq!(bf.targets.len(), 1);
    }

    #[test]
    fn build_block_cache_unit_disables_caching() {
        let bf = pb("cache ();");
        assert!(matches!(bf.cache, BuildBlockCache::Disabled { .. }));
    }

    #[test]
    fn build_block_cache_value_must_be_string_or_unit() {
        let msg = pb_err("cache 42;\ntarget js { out \"out/js/\"; }");
        assert!(msg.contains("string-literal path"), "got: {msg}");
    }

    #[test]
    fn build_block_docs_value_kind_error_precedes_missing_semicolon() {
        let msg = pb_err("cache ();\ndocs { md () }");
        assert!(msg.contains("string-literal path"), "got: {msg}");
    }

    #[test]
    fn build_block_cache_value_unclosed_unit_errors() {
        let msg = pb_err("cache (;\ntarget js { out \"out/js/\"; }");
        assert!(msg.contains("`)`"), "got: {msg}");
    }

    // ---- Build block inside the package file --------------------------

    #[test]
    fn package_file_with_build_block() {
        let src = "package app;\n\n\
             build {\n  cache ();\n\n  target js { out \"out/js/\"; }\n}\n\n\
             bridge {\n  main;\n}\n";
        let ef = parse_package_file(src, Some("app")).expect("parse");
        let build = ef.build.expect("build block present");
        assert!(matches!(build.cache, BuildBlockCache::Disabled { .. }));
        assert_eq!(build.targets.len(), 1);
        assert_eq!(build.targets[0].id, "js");
        assert_eq!(ef.bridge.expect("bridge block").globs.len(), 1);
    }

    #[test]
    fn package_file_build_block_before_bridge() {
        let src = "package app;\n\n\
             build {\n  cache ();\n}\n\n\
             bridge {\n  main;\n}\n";
        let ef = parse_package_file(src, Some("app")).expect("parse");
        assert!(ef.build.is_some());
        assert!(ef.bridge.is_some());
    }

    #[test]
    fn package_file_build_block_after_bridge_parses() {
        let src = "package app;\n\n\
             bridge {\n  main;\n}\n\n\
             build {\n  cache ();\n}\n";
        let file = parse_package_file(src, Some("app")).expect("unordered package sections");
        assert!(file.build.is_some());
        let bridge = file.bridge.as_ref().expect("bridge survives");
        assert_eq!(bridge.globs.len(), 1);
        assert_eq!(glob_text(&bridge.globs[0]), "main");
        assert_eq!(file.meta.span.end as usize, src.trim_end().len());
    }

    #[test]
    fn package_file_without_build_block() {
        let src = "package app;\n\nbridge {\n  main;\n}\n";
        let ef = parse_package_file(src, Some("app")).expect("parse");
        assert!(ef.build.is_none());
    }

    // ---- Package files (`<name>.pkg.kio`) -----------------------------

    fn pe(src: &str) -> PackageFile {
        let full = format!("package pkg;\n{src}");
        parse_package_file(&full, None)
            .unwrap_or_else(|e| panic!("parse_package_file failed for {full:?}: {e:?}"))
    }

    fn pe_err(src: &str) -> String {
        let full = format!("package pkg;\n{src}");
        match parse_package_file(&full, None) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {full:?}"),
        }
    }

    fn bridge_globs(ef: &PackageFile) -> &[BridgeGlob] {
        ef.bridge.as_ref().expect("bridge block").globs.as_slice()
    }

    fn glob_text(g: &BridgeGlob) -> String {
        g.segments
            .iter()
            .map(|s| match s {
                BridgeGlobSegment::Literal(name) => name.as_str(),
                BridgeGlobSegment::Star => "*",
                BridgeGlobSegment::DoubleStar => "**",
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn package_empty_file() {
        let ef = pe("");
        assert!(ef.bridge.is_none());
    }

    #[test]
    fn package_bridge_single_literal_glob() {
        let ef = pe("bridge { main; }");
        assert_eq!(bridge_globs(&ef).len(), 1);
        assert_eq!(glob_text(&bridge_globs(&ef)[0]), "main");
    }

    #[test]
    fn package_bridge_nested_literal_glob() {
        let ef = pe("bridge { app/api; }");
        assert_eq!(glob_text(&bridge_globs(&ef)[0]), "app/api");
    }

    #[test]
    fn package_bridge_star_and_double_star() {
        let ef = pe("bridge { app/*; lib/**; }");
        assert_eq!(bridge_globs(&ef).len(), 2);
        assert_eq!(glob_text(&bridge_globs(&ef)[0]), "app/*");
        assert_eq!(glob_text(&bridge_globs(&ef)[1]), "lib/**");
    }

    #[test]
    fn package_bridge_double_star_only() {
        let ef = pe("bridge { **; }");
        assert_eq!(glob_text(&bridge_globs(&ef)[0]), "**");
    }

    #[test]
    fn package_empty_bridge_parses() {
        let ef = pe("bridge {}");
        assert!(bridge_globs(&ef).is_empty());
    }

    #[test]
    fn package_bridge_invalid_segment_rejected() {
        let msg = pe_err("bridge { Foo; }");
        assert!(
            msg.contains("value name") || msg.contains("module-name"),
            "got: {msg}"
        );
    }

    #[test]
    fn package_file_two_bridge_blocks_rejected() {
        let msg = pe_err("bridge { a; } bridge { b; }");
        assert!(msg.contains("only one `bridge`"), "got: {msg}");
    }

    #[test]
    fn package_file_env_block_rejected() {
        let msg = pe_err("env { type String role(str); }");
        assert!(msg.contains("`host` declarations"), "got: {msg}");
    }

    #[test]
    fn package_file_export_block_rejected() {
        let msg = pe_err("export { main.main; }");
        assert!(msg.contains("`host` declarations"), "got: {msg}");
    }

    #[test]
    fn package_file_module_decl_rejected() {
        let msg = pe_err("module foo;");
        assert_eq!(
            msg,
            "a package file contains a `package <name>;` header, an optional `build` block, and an optional `bridge` block"
        );
    }

    #[test]
    fn package_file_regular_fn_def_rejected() {
        let msg = pe_err("fn foo() -> . { () }");
        assert_eq!(
            msg,
            "a package file contains a `package <name>;` header, an optional `build` block, and an optional `bridge` block"
        );
    }

    // ---- Module-level `host` declarations -----------------------------

    fn host_items(m: &Module) -> Vec<&crate::ast::Item> {
        m.items
            .iter()
            .filter(|it| matches!(it, Item::HostType(_) | Item::HostFn(_)))
            .collect()
    }

    #[test]
    fn host_type_with_role() {
        let m = p("module x; host type String role(str);");
        match host_items(&m)[0] {
            Item::HostType(h) => {
                assert_eq!(h.name, "String");
                assert_eq!(h.role.unwrap().role, Role::Str);
                assert!(h.type_params.is_empty());
            }
            other => panic!("expected host type, got {other:?}"),
        }
    }

    #[test]
    fn host_type_without_role() {
        let m = p("module x; host type Map[K][V];");
        match host_items(&m)[0] {
            Item::HostType(h) => {
                assert_eq!(h.name, "Map");
                assert_eq!(h.type_params.len(), 2);
                assert!(h.role.is_none());
            }
            other => panic!("expected host type, got {other:?}"),
        }
    }

    #[test]
    fn host_type_owned_block() {
        let m = p("module x; host type Owned_str role(str) { owned };");
        match host_items(&m)[0] {
            Item::HostType(h) => {
                assert_eq!(h.name, "Owned_str");
                assert!(h.owned);
            }
            other => panic!("expected host type, got {other:?}"),
        }
    }

    #[test]
    fn host_type_without_owned_defaults_false() {
        let m = p("module x; host type Str role(str);");
        match host_items(&m)[0] {
            Item::HostType(h) => assert!(!h.owned),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn host_type_unknown_role_kind_errors() {
        let msg = p_err("module x; host type Weird role(quux);");
        assert!(msg.contains("unknown `role` kind"), "got: {msg}");
    }

    #[test]
    fn host_fn_anonymous_param_rejected() {
        let err = p_err("module x; host fn print_string(String) -> .;");
        assert!(
            err.contains("expected named host function parameter"),
            "anonymous host fn param must be rejected, got: {err}"
        );
    }

    #[test]
    fn host_fn_named_param() {
        let m = p("module x; host fn print(s: String) -> .;");
        match host_items(&m)[0] {
            Item::HostFn(h) => match &h.params[0] {
                crate::ast::HostFnParam::Value(vp) => {
                    assert_eq!(vp.name.as_deref(), Some("s"));
                }
                other => panic!("got {other:?}"),
            },
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn host_fn_polymorphic() {
        let m = p("module x; host fn fold[A][B](f: (B & A) -> B, seed: B, xs: Seq(A)) -> B;");
        match host_items(&m)[0] {
            Item::HostFn(h) => {
                assert_eq!(h.name, "fold");
                assert_eq!(h.params.len(), 5);
                assert!(matches!(h.params[0], crate::ast::HostFnParam::Type(_)));
                assert!(matches!(h.params[2], crate::ast::HostFnParam::Value(_)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn host_fn_returns_bottom() {
        let m = p("module x; host fn panic(msg: String) -> !;");
        match host_items(&m)[0] {
            Item::HostFn(h) => assert!(matches!(&h.ret, Type::Bottom { .. })),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn host_pub_normalizes_to_plain_host() {
        // `pub host` / `host pub` are accepted (host items are always
        // public); both parse to the same `Item::HostType`.
        let m = p("module x; pub host type Foo;");
        assert!(matches!(host_items(&m)[0], Item::HostType(_)));
        let m = p("module x; host pub type Foo;");
        assert!(matches!(host_items(&m)[0], Item::HostType(_)));
    }

    #[test]
    fn host_as_identifier_still_parses() {
        // `host` is a contextual keyword: as an ordinary value name it
        // keeps parsing (here as a `fn` binding named `host`).
        let m = p("module x; fn host() -> . { () }");
        match &m.items[0] {
            Item::FnDef(d) => assert_eq!(d.name, "host"),
            other => panic!("expected fn `host`, got {other:?}"),
        }
    }

    // ---- naming-convention validators -----------------------------------

    #[test]
    fn one_leading_underscore_marks_type_names() {
        let module = p("module x; \
             host type Host; \
             host type _Host; \
             type Alias[A] = A; \
             type _Alias[_A] = _A; \
             type _Poly = [_T] _T -> _T; \
             newtype Box[B] : Alias(B) { constructor mk_plain; projector un_plain; }; \
             newtype _Box[_B] : _Alias(_B) { constructor mk_box; projector un_box; }; \
             newtype _Pack[_C] <_Hidden> : _C & _Hidden { constructor mk_pack; projector un_pack; }; \
             labels Row = { plain: . }; \
             labels _Row = { field: . };");
        assert_eq!(module.items.len(), 10);
    }

    #[test]
    fn malformed_type_names_report_the_broken_word_rule() {
        for (name, reason) in [
            ("_foo", "uppercase first letter"),
            (
                "_1Foo",
                "start every underscore-separated word with a letter",
            ),
            ("FooBar", "lowercase letters after its first letter"),
            ("_FooBar", "lowercase letters after its first letter"),
            ("_1", "at least one ASCII letter"),
            (
                "Foo_123",
                "start every underscore-separated word with a letter",
            ),
            ("Foo__bar", "exactly one `_`"),
            ("Foo1bar", "letter following a digit"),
        ] {
            let msg = p_err(&format!("module x; type {name} = .;"));
            assert!(msg.contains(reason), "{name}: {msg}");
        }
    }

    #[test]
    fn ambiguous_call_args_accept_exact_name_roles_only() {
        p("module ok; fn id[A](x: A) -> A { x } \
           fn ordinary[A](value: A) -> A { id(A, value) } \
           fn marked[_A](_value: _A) -> _A { id(_A, _value) } \
           fn qualified[_A](_value: _A) -> _A { \
               id(m._Foo, _value); \
               let _ = id(_A, Formtype.member(_A, _value)); \
               let _ = id(_A, m._value1.member(_value)); \
               id(_A, m._Foo.member(_value)) \
           }");
        for (argument, invalid_name) in [
            ("_FooBar", "_FooBar"),
            ("_FooBar(_A)", "_FooBar"),
            ("_1Foo", "_1Foo"),
            ("FooBar", "FooBar"),
            ("__FooBar", "__FooBar"),
            ("m._FooBar", "_FooBar"),
            ("m._FooBar(_A)", "_FooBar"),
            ("m._1Foo", "_1Foo"),
            ("m.FooBar", "FooBar"),
            ("m.__FooBar", "__FooBar"),
            ("m._FooBar.member", "_FooBar"),
            ("m._FooBar.member(_A)", "_FooBar"),
            ("m._1Foo.member", "_1Foo"),
            ("m.FooBar.member", "FooBar"),
            ("m.__FooBar.member", "__FooBar"),
        ] {
            let msg = p_err(&format!(
                "module bad; fn id[A](x: A) -> A {{ x }} \
                 fn f[_A](x: .) -> . {{ id({argument}, x) }}"
            ));
            assert!(msg.contains(invalid_name), "{argument}: {msg}");
        }
    }

    #[test]
    fn expression_paths_accept_exact_name_roles_only() {
        p("module ok; fn f[_A](_value: .) -> . { m._Foo.member(_A, _value) }");
        p("module ok; fn f[_A](_value: .) -> . { __pair__(_A, _A, _value, _value) }");
        p("module ok; fn f() { __Foo.member(__value) }");
        for (expression, invalid_name) in [
            ("_FooBar", "_FooBar"),
            ("_1Foo", "_1Foo"),
            ("FooBar", "FooBar"),
            ("__FooBar", "__FooBar"),
            ("m._FooBar.member", "_FooBar"),
            ("m._1Foo.member", "_1Foo"),
            ("m.FooBar.member", "FooBar"),
            ("m.__FooBar.member", "__FooBar"),
        ] {
            let message = p_err(&format!("module bad; fn f() -> . {{ {expression} }}"));
            assert!(message.contains(invalid_name), "{expression}: {message}");
        }
    }

    #[test]
    fn elaborator_bang_call_names_are_value_names() {
        p("module ok; fn f(x: .) -> . { _foo!.<<x }");
        for call in ["_Foo!()", "_FooBar!()", "_Foo!.<<x"] {
            let message = p_err(&format!("module bad; fn f(x: .) -> . {{ {call} }}"));
            assert!(
                message.contains(call.split('!').next().unwrap()),
                "{call}: {message}"
            );
        }
    }

    #[test]
    fn operator_and_variadic_callable_paths_use_exact_name_roles() {
        p("module ok; op _ + _ { impl _Box.mk; };");
        p("module ok; varop [* *] { foldr _foo _Box.mk; };");
        p("module ok; op _ + _ { impl __pair__; };");
        p("module ok; varop [* *] { foldr __pair__ _foo; finalize __fst__; };");
        for (reference, invalid_name) in [
            ("_Foo", "_Foo"),
            ("FooBar", "FooBar"),
            ("__Foo", "__Foo"),
            ("_FooBar.member", "_FooBar"),
            ("_1Foo.member", "_1Foo"),
        ] {
            let op_message = p_err(&format!("module bad; op _ + _ {{ impl {reference}; }};"));
            assert!(
                op_message.contains(invalid_name),
                "op {reference}: {op_message}"
            );
            let fold_message = p_err(&format!(
                "module bad; varop [* *] {{ foldr ok {reference}; }};"
            ));
            assert!(
                fold_message.contains(invalid_name),
                "fold {reference}: {fold_message}"
            );
        }
    }

    #[test]
    fn selective_imports_accept_exact_name_roles_only() {
        p("module x; import provider(Foo, foo, _Foo, _foo, _foo1); fn ok() -> . { () }");
        for name in ["_FooBar", "_1Foo", "_1foo", "FooBar", "__Foo"] {
            let msg = p_err(&format!(
                "module x; import provider({name}); fn bad() -> . {{ () }}"
            ));
            assert!(msg.contains(name), "{name}: {msg}");
        }
    }

    #[test]
    fn marked_type_names_are_rejected_in_value_only_roles() {
        for source in [
            "module m; type T = m._Foo;",
            "module m; type T = m.Foo;",
            "module m; fn f(_: .) -> . { let _ = (); .(_: .) { () }(()) }",
            "module m; rec(__pair__) fn f() -> . { () }",
        ] {
            p(source);
        }
        for (role, invalid, valid) in [
            ("module head", "module _Foo;", "module _foo;"),
            ("module tail", "module m/_Foo;", "module m/_foo;"),
            (
                "qualified import source",
                "module m; import _Foo as dep; fn f() -> . { () }",
                "module m; import _foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import source tail",
                "module m; import provider/_Foo as dep; fn f() -> . { () }",
                "module m; import provider/_foo as dep; fn f() -> . { () }",
            ),
            (
                "qualified import alias",
                "module m; import provider as _Foo; fn f() -> . { () }",
                "module m; import provider as _foo; fn f() -> . { () }",
            ),
            (
                "function name",
                "module m; fn _Foo() -> . { () }",
                "module m; fn _foo() -> . { () }",
            ),
            (
                "host function name",
                "module m; host fn _Foo() -> .;",
                "module m; host fn _foo() -> .;",
            ),
            (
                "function parameter",
                "module m; fn f(_Foo: .) -> . { () }",
                "module m; fn f(_foo: .) -> . { () }",
            ),
            (
                "host function parameter",
                "module m; host fn f(_Foo: .) -> .;",
                "module m; host fn f(_foo: .) -> .;",
            ),
            (
                "lambda parameter",
                "module m; fn f() -> . { .(_Foo: .) { () } }",
                "module m; fn f() -> . { .(_foo: .) { () } }",
            ),
            (
                "let binder",
                "module m; fn f() -> . { let _Foo = (); () }",
                "module m; fn f() -> . { let _foo = (); () }",
            ),
            (
                "constructor",
                "module m; newtype Box : . { constructor _Foo; projector un_box; };",
                "module m; newtype Box : . { constructor _foo; projector un_box; };",
            ),
            (
                "projector",
                "module m; newtype Box : . { constructor mk_box; projector _Foo; };",
                "module m; newtype Box : . { constructor mk_box; projector _foo; };",
            ),
            (
                "type-path module qualifier",
                "module m; type T = _Foo.Bar;",
                "module m; type T = _foo.Bar;",
            ),
            (
                "type-path module qualifier tail",
                "module m; type T = provider/_Foo.Bar;",
                "module m; type T = provider/_foo.Bar;",
            ),
            (
                "recursion loop path",
                "module m; rec(_Foo) fn f() -> . { () }",
                "module m; rec(_foo) fn f() -> . { () }",
            ),
            (
                "UFCS callee",
                "module m; fn f(x: .) -> . { x.>_Foo }",
                "module m; fn f(x: .) -> . { x.>_foo }",
            ),
        ] {
            let message = p_err(invalid);
            assert!(message.contains("_Foo"), "{role}: {message}");
            p(valid);
        }
    }

    #[test]
    fn elaborator_captures_accept_exact_type_or_value_roles() {
        for source in [
            "module m; elab demo : . -> . { captures Foo; impl _foo; };",
            "module m; elab demo : . -> . { captures _Foo; impl _foo; };",
            "module m; elab demo : . -> . { captures _foo; impl _foo; };",
            "module m; elab demo : . -> . { captures m._Foo.member; impl _foo; };",
        ] {
            p(source);
        }
        for name in ["_FooBar", "_1Foo", "__pair__"] {
            let bare = p_err(&format!(
                "module m; elab demo : . -> . {{ captures {name}; impl _foo; }};"
            ));
            assert!(bare.contains(name), "bare {name}: {bare}");
            let dotted = p_err(&format!(
                "module m; elab demo : . -> . {{ captures m.{name}; impl _foo; }};"
            ));
            assert!(dotted.contains(name), "dotted {name}: {dotted}");
        }
        for name in ["Foo", "_Foo"] {
            let message = p_err(&format!(
                "module m; elab demo : . -> . {{ captures m.{name}; impl _foo; }};"
            ));
            assert!(message.contains(name), "dotted {name}: {message}");
        }
    }

    #[test]
    fn ufcs_callee_accepts_documented_reserved_value_references() {
        p("module m; fn f(x: ., y: .) -> . { x.>__pair__(., ., y) }");
        p(
            "module m; fn f[Bool](packet: Bool & (. -> Bool) & (. -> Bool)) -> Bool { packet.>>__if_then_else__(Bool) }",
        );
        for name in ["_Foo", "_FooBar", "__Foo"] {
            let message = p_err(&format!("module m; fn f(x: .) -> . {{ x.>{name} }}"));
            assert!(message.contains(name), "{name}: {message}");
        }
    }

    #[test]
    fn elaborator_ufcs_requires_a_user_value_name() {
        p("module m; fn f(x: .) -> . { x.>_foo! }");
        for call in ["x.>__pair__!", "x.>>__if_then_else__!"] {
            let message = p_err(&format!("module m; fn f(x: .) -> . {{ {call} }}"));
            let invalid_name = call
                .split('>')
                .next_back()
                .unwrap()
                .split('!')
                .next()
                .unwrap();
            assert!(message.contains(invalid_name), "{call}: {message}");
        }
    }

    #[test]
    fn left_callee_ufcs_accepts_exact_value_references() {
        for source in [
            "module m; fn f(x: .) -> . { _foo.<x }",
            "module m; fn f(x: .) -> . { _foo(()).<x }",
            "module m; fn f(x: .) -> . { _Box.mk.<x }",
            "module m; fn f(x: .) -> . { _Box.mk(()).<x }",
            "module m; fn f(x: .) -> . { __pair__(., ., x, x).<x }",
            "module m; fn f(x: .) -> . { _foo!(()).<<x }",
        ] {
            p(source);
        }
        for (callee, invalid_name) in [
            ("_Foo", "_Foo"),
            ("_Foo(())", "_Foo"),
            ("_FooBar", "_FooBar"),
            ("_FooBar(())", "_FooBar"),
            ("__Foo", "__Foo"),
        ] {
            let message = p_err(&format!("module m; fn f(x: .) -> . {{ {callee}.<x }}"));
            assert!(message.contains(invalid_name), "{callee}: {message}");
        }
    }

    #[test]
    fn slash_only_qualified_type_paths_are_rejected() {
        for source in ["module m; type T = m._Foo;", "module m; type T = m.Foo;"] {
            p(source);
        }
        for source in ["module m; type T = m/_Foo;", "module m; type T = m/Foo;"] {
            let message = p_err(source);
            assert!(
                message.contains("qualified paths require"),
                "{source}: {message}"
            );
        }
    }

    #[test]
    fn bare_underscore_remains_a_slot_not_a_type_name() {
        let msg = p_err("module x; type _ = .;");
        assert!(
            msg.contains("type name `_` must contain at least one ASCII letter"),
            "{msg}"
        );
    }

    #[test]
    fn reserved_shape_newtype_name_rejected() {
        // A user declaration cannot occupy the reserved hygiene namespace.
        let msg = p_err(
            "module x; newtype __Tag_foo__ : . { pub constructor mk_foo; pub projector foo; };",
        );
        assert!(msg.contains("may not begin with `__`"), "got: {msg}");
    }

    #[test]
    fn reserved_shape_value_param_name_rejected() {
        // Reservation applies equally to local and module-level declarations.
        let msg = p_err("module x; fn f() -> . { .(__p1__) { () }(()) }");
        assert!(msg.contains("`__`"), "got: {msg}");
    }

    #[test]
    fn intrinsic_import_target_is_admitted() {
        // The magic phrase `import __intrinsics__;` is unchanged, and
        // bare references like `__pair__` after that import go
        // through the IDENT lexer rule (not the declaration
        // validators), so they continue to parse.
        let _ = p("module x; import __intrinsics__; fn f() -> . { __pair__(., ., (), ()) }");
    }

    #[test]
    fn user_leading_underscore_pair_rejected() {
        // Trailing affixes do not affect leading-prefix reservation.
        let msg = p_err("module x; fn __foo() -> . { () }");
        assert!(msg.contains("`__`"), "got: {msg}");
    }

    #[test]
    fn user_trailing_underscore_pair_accepted() {
        // `foo__` (no leading `__`) is just a value name with a
        // trailing underscore — fine.
        let _ = p("module x; fn foo__() -> . { () }");
    }

    #[test]
    fn letterless_value_name_rejected() {
        // A name must carry at least one letter: role classification
        // keys on the case of the first letter, so `_`, `_1`, `_1_2`
        // (letterless) have no role and are rejected at every name
        // position — here a fn name and a `let` binder name.
        for src in [
            "module x; fn _() -> . { () }",
            "module x; fn _1() -> . { () }",
            "module x; fn _1_2() -> . { () }",
            "module x; fn f() -> . { let _1 = (); _1 }",
        ] {
            let msg = p_err(src);
            assert!(
                msg.contains("at least one ASCII letter"),
                "src {src:?} got: {msg}"
            );
        }
    }

    #[test]
    fn bare_wildcard_stays_legal_at_binder_positions() {
        // `_` is the wildcard-discard binder, not a name, and stays
        // legal wherever a binder admits it: `let`, top-level fn
        // params, and lambda params. (Rejection applies only to `_`
        // in a *name* role — see `letterless_value_name_rejected`.)
        let _ = p("module x; fn f() -> . { let _ = (); () }");
        let _ = p("module x; fn f(_: .) -> . { () }");
        let _ = p("module x; fn use_it() -> . { g(.(_: .) { () }) }");
    }

    #[test]
    fn underscore_parses_as_type_infer_placeholder() {
        // `_` in a type position parses as `Type::Infer` — the
        // surface-only placeholder the typer is responsible for
        // resolving.
        let m = p("module x; fn id(x: _) -> _ { x }");
        let crate::ast::Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn, got {:?}", m.items[0]);
        };
        // The first parameter's type annotation is `_`.
        let crate::ast::SignatureParam::Value(vp) = &d.sig.params[0] else {
            panic!("expected value param");
        };
        assert!(
            matches!(vp.ty, Some(Type::Infer { .. })),
            "expected Type::Infer, got {:?}",
            vp.ty
        );
        // The return type is `_`.
        assert!(
            matches!(d.ret, Type::Infer { .. }),
            "expected Type::Infer return, got {:?}",
            d.ret
        );
    }

    #[test]
    fn underscore_parses_in_type_arg_list() {
        // `_` in a type-arg list at a call site parses fine.
        // The body `f(_, "")` builds a `Call` whose first argument
        // is a `CallArg::Type(Type::Infer)`.
        let m = p("module x; fn use_id() -> . { f(_, ()) }");
        let crate::ast::Item::FnDef(d) = &m.items[0] else {
            panic!("expected fn");
        };
        // The body is `f(_, ())`.
        let crate::ast::Expr::Call { args, .. } = &d.body else {
            panic!("expected Call expression, got {:?}", d.body);
        };
        let crate::ast::CallArg::Type(t) = &args[0] else {
            panic!("expected first arg to be type-arg, got {:?}", args[0]);
        };
        assert!(
            matches!(t, Type::Infer { .. }),
            "expected Type::Infer, got {t:?}"
        );
    }

    // =====================================================================
    // Package-file header tests
    // =====================================================================

    #[test]
    fn package_file_requires_leading_header() {
        let err = parse_package_file("env { type String role(str); }", None)
            .expect_err("expected parse error");
        let Error::Parse(Diagnostic { message, .. }) = err else {
            panic!("expected Parse error");
        };
        assert!(message.contains("package <name>"), "got: {message}");
    }

    #[test]
    fn package_file_header_name_must_match_stem() {
        let err =
            parse_package_file("package wrong;", Some("logger")).expect_err("expected parse error");
        let Error::Parse(Diagnostic { message, .. }) = err else {
            panic!("expected Parse error");
        };
        assert!(message.contains("does not match"), "got: {message}");
    }

    // ---- Signature files (`<name>.sig.kio`) ---------------------------

    fn sig(src: &str) -> SignatureFile {
        parse_signature_file(src, None)
            .unwrap_or_else(|e| panic!("parse_signature_file failed for {src:?}: {e:?}"))
    }

    fn sig_err(src: &str) -> String {
        match parse_signature_file(src, None) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {src:?}"),
        }
    }

    fn module_path_str(path: &ModulePath) -> String {
        path.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn sig_header_only() {
        let f = sig("signature app v(1);\n");
        assert_eq!(f.pkg, "app");
        assert_eq!(f.version, 1);
        assert!(f.versions.is_empty());
    }

    #[test]
    fn sig_header_stem_mismatch_rejected() {
        let msg = match parse_signature_file("signature app v(1);\n", Some("other")) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected stem mismatch error"),
        };
        assert!(msg.contains("does not match filename stem"), "got: {msg}");
    }

    #[test]
    fn sig_header_missing_signature_keyword_rejected() {
        let msg = sig_err("package app v(1);\n");
        assert!(msg.contains("signature <pkg> v(<N>)"), "got: {msg}");
    }

    #[test]
    fn sig_header_missing_version_rejected() {
        let msg = sig_err("signature app;\n");
        assert!(msg.contains("v(<N>)"), "got: {msg}");
    }

    /// A representative multi-version changelog: `v(1)` introduces a
    /// module (under `nonbreaking`, an `add` block) bearing every
    /// signature-only declaration kind — `host type`, `host fn`, `type`,
    /// `newtype`, and a body-less `pub fn` export — and `v(2)` carries
    /// both a `breaking` section (with `modify` and `remove`) and a
    /// `nonbreaking` section (an `add`).
    #[test]
    fn sig_multi_version_full_shape() {
        let src = r#"signature app v(2);

v(1) {
  nonbreaking {
    add {
      module app/api {
        import app/types as t;
        host type Handle role(i32);
        host fn open(path: Text) -> Handle;
        type Alias = Handle;
        newtype Id : I32 { constructor mk_id; projector un_id; };
        pub fn serve(h: Handle) -> .;
      }
    }
  }
}

v(2) {
  breaking {
    modify {
      module app/api {
        pub fn serve(h: Handle, opts: I32) -> .;
      }
    };
    remove {
      module app/api {
        open;
        Alias;
      }
    }
  };
  nonbreaking {
    add {
      module app/api {
        pub fn close(h: Handle) -> .;
      }
    }
  }
}
"#;
        let f = sig(src);
        assert_eq!(f.pkg, "app");
        assert_eq!(f.version, 2);
        assert_eq!(f.versions.len(), 2);

        // ---- v(1): one nonbreaking `add` module with all kinds ----
        let v1 = &f.versions[0];
        assert_eq!(v1.version, 1);
        assert!(v1.breaking.is_none());
        let nb1 = v1.nonbreaking.as_ref().expect("v(1) nonbreaking");
        assert_eq!(nb1.add.len(), 1);
        assert!(nb1.modify.is_empty());
        assert!(nb1.remove.is_empty());
        let sec = &nb1.add[0];
        assert_eq!(module_path_str(&sec.path), "app/api");
        assert_eq!(sec.imports.len(), 1);
        assert!(matches!(sec.imports[0].kind, ImportKind::Qualified { .. }));
        assert_eq!(sec.items.len(), 5);
        assert!(
            matches!(&sec.items[0], SigItem::HostType(h) if h.name == "Handle" && h.role.is_some())
        );
        assert!(matches!(&sec.items[1], SigItem::HostFn(h) if h.name == "open"));
        assert!(matches!(&sec.items[2], SigItem::TypeAlias(a) if a.name == "Alias"));
        assert!(matches!(&sec.items[3], SigItem::Newtype(n) if n.name == "Id"));
        match &sec.items[4] {
            SigItem::ExportFn(export) => {
                assert_eq!(export.function.name, "serve");
                assert!(matches!(export.function.ret, Type::Unit { .. }));
            }
            other => panic!("expected ExportFn serve, got {other:?}"),
        }

        // ---- v(2): breaking { modify, remove } + nonbreaking { add } ----
        let v2 = &f.versions[1];
        assert_eq!(v2.version, 2);
        let br2 = v2.breaking.as_ref().expect("v(2) breaking");
        assert!(br2.add.is_empty());
        assert_eq!(br2.modify.len(), 1);
        assert_eq!(br2.remove.len(), 1);
        let modify_sec = &br2.modify[0];
        assert_eq!(module_path_str(&modify_sec.path), "app/api");
        assert!(
            matches!(&modify_sec.items[0], SigItem::ExportFn(export) if export.function.name == "serve")
        );
        let removed = &br2.remove[0];
        assert_eq!(module_path_str(&removed.path), "app/api");
        let names: Vec<&str> = removed.names.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["open", "Alias"]);

        let nb2 = v2.nonbreaking.as_ref().expect("v(2) nonbreaking");
        assert_eq!(nb2.add.len(), 1);
        assert!(
            matches!(&nb2.add[0].items[0], SigItem::ExportFn(export) if export.function.name == "close")
        );
    }

    #[test]
    fn sig_recursive_group_uses_version_context_and_exact_member_refs() {
        let src = r#"signature app v(1);

v(1) {
  with {
    module api {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  nonbreaking {
    add {
      api.A;
      api.B;
    }
  }
}
"#;
        let file = sig(src);
        let version = &file.versions[0];
        assert_eq!(version.with.len(), 1);
        assert!(matches!(
            &version.with[0].items[..],
            [SigItem::TypeRecGroup(group)] if group.members.len() == 2
        ));
        let add_refs = &version.nonbreaking.as_ref().unwrap().add_refs;
        assert_eq!(add_refs.len(), 2);
        assert_eq!(module_path_str(&add_refs[0].path), "api");
        assert_eq!(add_refs[0].name, "A");
        assert_eq!(add_refs[1].name, "B");

        let emitted = crate::sig::emit_signature_file(&file);
        assert!(emitted.contains("with {\n    module api {\n      rec {"));
        assert!(emitted.contains("add {\n      api.A;\n      api.B\n    }"));
        let reparsed = sig(&emitted);
        assert_eq!(reparsed.versions[0].with.len(), 1);
        assert_eq!(
            reparsed.versions[0]
                .nonbreaking
                .as_ref()
                .unwrap()
                .add_refs
                .len(),
            2
        );
    }

    #[test]
    fn sig_recursive_group_is_rejected_inline_in_an_operation() {
        let message = sig_err(
            "signature app v(1);\n\
             v(1) { nonbreaking { add { module api { rec { type A = B; newtype B : A { constructor mk; projector un; }; } } } } }\n",
        );
        assert!(message.contains("version-leading `with`"), "got: {message}");
    }

    #[test]
    fn sig_operation_reference_requires_an_exact_module_path() {
        let message = sig_err(
            "signature app v(1);\n\
             v(1) { nonbreaking { add { api; } } }\n",
        );
        assert!(
            message.contains("between the module path and declaration name"),
            "got: {message}"
        );
    }

    #[test]
    fn sig_item_refs_use_the_nonexpression_fqn_category_with_contextual_names() {
        let file = sig(r#"signature app v(1);
v(1) {
  with {
    module module/with {
      rec {
        pub type A = B;
        pub newtype B : A { pub constructor mk_b; pub projector un_b; };
      }
    }
  };
  nonbreaking { add { module/with.A; module/with.B; } }
}
"#);
        let refs = &file.versions[0]
            .nonbreaking
            .as_ref()
            .expect("nonbreaking")
            .add_refs;
        assert_eq!(module_path_str(&refs[0].path), "module/with");
        assert_eq!(refs[0].name, "A");
        assert_eq!(module_path_str(&refs[1].path), "module/with");
        assert_eq!(refs[1].name, "B");
    }

    #[test]
    fn sig_legacy_nested_remove_emits_as_an_exact_fqn() {
        let file = sig("signature app v(1);\n\
             v(1) { breaking { remove { module foo/bar { A; } } } }\n");
        let emitted = crate::sig::emit_signature_file(&file);
        assert!(emitted.contains("remove {\n      foo/bar.A\n    }"));
        assert!(!emitted.contains("module foo/bar"));
    }

    #[test]
    fn sig_export_fn_stray_body_rejected() {
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app { pub fn f() -> . { () } } } } }\n";
        let msg = sig_err(src);
        assert!(
            msg.contains("body-less signature") && msg.contains("pub fn f(p) -> R;"),
            "got: {msg}"
        );
    }

    #[test]
    fn sig_export_fn_must_be_pub() {
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app { fn f() -> .; } } } }\n";
        let msg = sig_err(src);
        assert!(msg.contains("must be `pub`"), "got: {msg}");
    }

    #[test]
    fn sig_export_fn_accepts_pure() {
        let file = sig("signature app v(1);\n\
            v(1) { nonbreaking { add { module app { pub pure fn f() -> .; } } } }\n");
        let SigItem::ExportFn(export) =
            &file.versions[0].nonbreaking.as_ref().unwrap().add[0].items[0]
        else {
            panic!("expected export fn");
        };
        assert!(export.purity.is_pure());

        let emitted = crate::sig::emit_signature_file(&file);
        assert!(emitted.contains("pub pure fn f() -> .\n      }"));
        let reparsed = sig(&emitted);
        let SigItem::ExportFn(export) =
            &reparsed.versions[0].nonbreaking.as_ref().unwrap().add[0].items[0]
        else {
            panic!("expected export fn after round trip");
        };
        assert!(export.purity.is_pure());
    }

    #[test]
    fn sig_rejects_pure_type_declarations() {
        for declaration in [
            "pub pure type T = .;",
            "pub pure newtype N : . { pub constructor mk; pub projector un; };",
        ] {
            let source = format!(
                "signature app v(1);\n\
                 v(1) {{ nonbreaking {{ add {{ module app {{ {declaration} }} }} }} }}\n"
            );
            let message = sig_err(&source);
            assert!(
                message.contains("`pure` is valid only on ordinary `fn` declarations"),
                "got: {message}"
            );
        }
    }

    #[test]
    fn sig_rejects_pure_host_fn_with_no_host_call_reason() {
        let source = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app { pure host fn effect() -> .; } } } }\n";
        let message = sig_err(source);
        assert!(
            message.contains("`pure` means that a function does not call host functions"),
            "got: {message}"
        );
    }

    #[test]
    fn sig_surface_only_form_rejected() {
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app { labels { a: I32 }; } } } }\n";
        let msg = sig_err(src);
        assert!(
            msg.contains("signature-file declaration must be"),
            "got: {msg}"
        );
    }

    #[test]
    fn sig_owned_source_annotation_rejected() {
        // A `.sig.kio` records backend-independent signatures. The redundant
        // source-compatible `{ owned }` annotation is not part of that
        // contract and is rejected with a sig-context diagnostic.
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module api { host type H role(str) { owned }; } } } }\n";
        let msg = sig_err(src);
        assert!(msg.contains("backend-independent signatures"), "got: {msg}");
    }

    #[test]
    fn sig_empty_version_block_rejected() {
        let msg = sig_err("signature app v(1);\nv(1) {}\n");
        assert!(
            msg.contains("at least one of `breaking") || msg.contains("nonbreaking"),
            "got: {msg}"
        );
    }

    #[test]
    fn sig_named_sections_accept_nonbreaking_before_breaking() {
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app { pub fn f() -> .; } } }; \
            breaking { add { module app { host type H; } } } }\n";
        let parsed = sig(src);
        assert_eq!(
            parsed.versions[0].breaking.as_ref().unwrap().add[0]
                .items
                .len(),
            1
        );
        assert_eq!(
            parsed.versions[0].nonbreaking.as_ref().unwrap().add[0]
                .items
                .len(),
            1
        );
        let replayed = crate::sig::replay(&parsed).expect("valid reordered version");
        let names: Vec<_> = replayed
            .current
            .items
            .values()
            .map(|item| item.name.to_string())
            .collect();
        assert_eq!(names, ["app.H", "app.f"]);
    }

    #[test]
    fn sig_change_buckets_accept_remove_before_add() {
        let src = "signature app v(2);\n\
            v(1) { nonbreaking { add { module app { pub fn f() -> .; } } } }\n\
            v(2) { nonbreaking { remove { module app { f; } }; \
            add { module app { pub fn g() -> .; } } } }\n";
        let parsed = sig(src);
        let set = parsed.versions[1].nonbreaking.as_ref().unwrap();
        assert_eq!(set.add[0].items.len(), 1);
        assert_eq!(set.remove[0].names.len(), 1);
        let replayed = crate::sig::replay(&parsed).expect("valid reordered buckets");
        let names: Vec<_> = replayed
            .current
            .items
            .values()
            .map(|item| item.name.to_string())
            .collect();
        assert_eq!(names, ["app.g"]);
        assert_eq!(replayed.removed.len(), 1);
        assert_eq!(replayed.removed[0].entry.name.to_string(), "app.f");
        assert_eq!(replayed.removed[0].removed_at_version, 2);
    }

    #[test]
    fn sig_remove_accepts_value_and_type_names() {
        // Both sides are admissible name shapes: a value name (host fn /
        // export fn) and a `_?[A-Z][a-z0-9_]*` type name (host
        // type / exported newtype / type alias). Side is recovered by
        // replay, so the remove block lists names of either shape.
        let src = "signature app v(1);\n\
            v(1) { breaking { remove { module app { old_fn; Old_type; _Old_type; } } } }\n";
        let f = sig(src);
        let removed = &f.versions[0].breaking.as_ref().unwrap().remove[0];
        let names: Vec<&str> = removed.names.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["old_fn", "Old_type", "_Old_type"]);
    }

    #[test]
    fn sig_empty_module_section_admissible() {
        // A module section with no items models module removal (the
        // module is empty after replay). Empty modules are admissible.
        let src = "signature app v(1);\n\
            v(1) { nonbreaking { add { module app {} } } }\n";
        let f = sig(src);
        let sec = &f.versions[0].nonbreaking.as_ref().unwrap().add[0];
        assert!(sec.items.is_empty());
        assert!(sec.imports.is_empty());
    }

    #[test]
    fn sig_trailing_garbage_rejected() {
        let msg = sig_err("signature app v(1);\nmodule app;\n");
        assert!(
            msg.contains("version block or end of signature file"),
            "got: {msg}"
        );
    }

    #[test]
    fn sig_duplicate_version_block_rejected() {
        // Two `v(2)` blocks: a malformed user-authored sig. The parser
        // rejects it (an input error, not a panic) so replay and the
        // draft recompute can't disagree about the same file.
        let msg = sig_err(
            "signature app v(2);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn a() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn b() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn c() -> .;\n      }\n    }\n  }\n}\n",
        );
        assert!(msg.contains("duplicate `v(2)`"), "got: {msg}");
    }

    #[test]
    fn sig_block_version_above_header_rejected() {
        // A block `v(3)` under a `v(2)` header is incoherent — the
        // header generation is the open draft, so no later block exists.
        let msg = sig_err(
            "signature app v(2);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn a() -> .;\n      }\n    }\n  }\n}\n\
             v(3) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn b() -> .;\n      }\n    }\n  }\n}\n",
        );
        assert!(msg.contains("out of range"), "got: {msg}");
    }

    #[test]
    fn sig_non_contiguous_version_blocks_rejected() {
        // A gap: `v(1)` then `v(3)` (missing `v(2)`) under a `v(3)`
        // header. Version blocks must run `v(1)`..=`v(max)` with no hole.
        let msg = sig_err(
            "signature app v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn a() -> .;\n      }\n    }\n  }\n}\n\
             v(3) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn b() -> .;\n      }\n    }\n  }\n}\n",
        );
        assert!(msg.contains("non-contiguous"), "got: {msg}");
    }

    #[test]
    fn sig_later_add_only_compact_boundary_reparses_with_suffix() {
        // Compaction through v(3) legitimately removes v(1)..v(2): the
        // first surviving block is the correctly partitioned
        // add-only boundary, and the original v(4) removal remains at
        // its own generation.
        let file = sig("signature app v(4);\n\
             v(3) {\n  breaking {\n    add {\n      module api {\n        host fn need() -> .;\n      }\n    }\n  };\n  nonbreaking {\n    add {\n      module api {\n        pub fn provide() -> .;\n      }\n    }\n  }\n}\n\
             v(4) {\n  nonbreaking {\n    remove {\n      module api {\n        need;\n      }\n    }\n  }\n}\n");
        assert_eq!(
            file.versions
                .iter()
                .map(|block| block.version)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn sig_later_recursive_compact_boundary_is_self_validating() {
        let file = sig("signature app v(3);\n\
             v(2) {\n  with {\n    module api {\n      rec {\n        pub type A = B;\n        pub newtype B : A { pub constructor mk_b; pub projector un_b; };\n      }\n    }\n  };\n  nonbreaking {\n    add {\n      api.A;\n      api.B;\n    }\n  }\n}\n\
             v(3) {\n  breaking {\n    remove { api.A; }\n  }\n}\n");
        assert_eq!(
            file.versions
                .iter()
                .map(|block| block.version)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn sig_later_recursive_boundary_rejects_unintroduced_public_peer() {
        let message = sig_err(
            "signature app v(2);\n\
             v(2) {\n  with {\n    module api {\n      rec {\n        pub type A = B;\n        pub newtype B : A { pub constructor mk_b; pub projector un_b; };\n      }\n    }\n  };\n  nonbreaking { add { api.A; } }\n}\n",
        );
        assert!(
            message.contains("add-only compact boundary"),
            "got: {message}"
        );
    }

    #[test]
    fn sig_later_compact_boundary_rejects_ordinary_with_target() {
        let message = sig_err(
            "signature app v(2);\n\
             v(2) {\n  with {\n    module api {\n      pub type T = .;\n    }\n  };\n  nonbreaking { add { api.T; } }\n}\n",
        );
        assert!(
            message.contains("add-only compact boundary"),
            "got: {message}"
        );
    }

    #[test]
    fn sig_later_compact_boundary_accepts_recursive_singleton_target() {
        let file = sig("signature app v(2);\n\
             v(2) {\n  with {\n    module api {\n      pub rec newtype Loop : . | Loop { pub constructor mk; pub projector un; };\n    }\n  };\n  nonbreaking { add { api.Loop; } }\n}\n");
        assert_eq!(file.versions[0].version, 2);
    }

    #[test]
    fn sig_later_first_block_must_have_compact_boundary_shape() {
        let modified = sig_err(
            "signature app v(3);\n\
             v(3) {\n  breaking {\n    modify {\n      module api {\n        pub fn provide(x: .) -> .;\n      }\n    }\n  }\n}\n",
        );
        assert!(
            modified.contains("add-only compact boundary"),
            "got: {modified}"
        );
    }

    #[test]
    fn sig_later_compact_boundary_accepts_inert_version_message() {
        // A message is replay-inert. Allowing it keeps a compacted later
        // boundary closed under uncommit followed by a message-bearing
        // commit, without weakening the add-only structural gate.
        let file = sig("signature app v(3);\n\
             /// Summarize the compacted additive history.\n\
             v(3) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn provide() -> .;\n      }\n    }\n  }\n}\n");
        assert!(file.versions[0].doc.is_some());
    }

    #[test]
    fn sig_compact_history_with_omitted_suffix_rejected() {
        // The boundary may omit the collapsed prefix, not trailing
        // generations. Under a v(5) header, ending at v(3) omits both
        // the latest sealed generation and the open draft.
        let msg = sig_err(
            "signature app v(5);\n\
             v(3) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn provide() -> .;\n      }\n    }\n  }\n}\n",
        );
        assert!(msg.contains("omits trailing generations"), "got: {msg}");
    }

    #[test]
    fn sig_compact_history_may_end_before_absent_open_draft() {
        // A v(5) header may omit the empty v(5) draft when the compact
        // boundary and surviving suffix run contiguously through v(4).
        let file = sig("signature app v(5);\n\
             v(3) {\n  breaking {\n    add {\n      module api {\n        host fn need() -> .;\n      }\n    }\n  }\n}\n\
             v(4) {\n  nonbreaking {\n    remove {\n      module api {\n        need;\n      }\n    }\n  }\n}\n");
        assert_eq!(file.version, 5);
        assert_eq!(
            file.versions
                .iter()
                .map(|block| block.version)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn sig_empty_open_draft_admissible() {
        // A sealed `v(1)` under a `v(2)` header with no `v(2)` block is a
        // valid empty open draft (the top draft may be absent). The
        // contiguity check must not reject it.
        let f = sig("signature app v(2);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        pub fn a() -> .;\n      }\n    }\n  }\n}\n");
        assert_eq!(f.version, 2);
        assert_eq!(f.versions.len(), 1);
    }

    // ---- Dependency files (`<local>.dep.kio`) -------------------------

    fn dep(src: &str) -> DependencyFile {
        parse_dependency_file(src, None)
            .unwrap_or_else(|e| panic!("parse_dependency_file failed for {src:?}: {e:?}"))
    }

    fn dep_err(src: &str) -> String {
        match parse_dependency_file(src, None) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {src:?}"),
        }
    }

    fn dep_path(f: &DependencyFile) -> &str {
        match &f.source.origin {
            SourceOrigin::Path { path, .. } => path.as_str(),
            other => panic!("expected a `path` source, got {other:?}"),
        }
    }

    /// The `(url, ref)` of a `git` dependency's source — the dual of
    /// [`dep_path`] for git-origin tests.
    fn dep_git(f: &DependencyFile) -> (&str, &str) {
        match &f.source.origin {
            SourceOrigin::Git(source) => (source.url.as_str(), source.git_ref.as_str()),
            other => panic!("expected a `git` source, got {other:?}"),
        }
    }

    #[test]
    fn dep_valid_local_path() {
        let f = dep("dependency foobar;\nsource { path \"../foo/foo.pkg.kio\"; }\n");
        assert_eq!(f.name, "foobar");
        assert_eq!(dep_path(&f), "../foo/foo.pkg.kio");
    }

    #[test]
    fn dep_rehost_parsed_and_sorted() {
        // Two `rehost` statements after the source block, given out of
        // canonical order — the formatter sorts them by `from`.
        let f = dep("dependency foobar;\n\
             source { path \"../foo/foo.pkg.kio\"; }\n\
             rehost foobar/types to testapi;\n\
             rehost foobar/io to testapi/io;\n");
        assert_eq!(f.name, "foobar");
        assert_eq!(f.rehost.len(), 2);
        let rendered = crate::pretty::pretty_dependency_file(&f);
        let src = rendered.find("source {").expect("source block");
        let io = rendered
            .find("rehost foobar/io to testapi/io;")
            .expect("io rehost");
        let types = rendered
            .find("rehost foobar/types to testapi;")
            .expect("types rehost");
        // Emitted after `source`, sorted `io` before `types`.
        assert!(src < io && io < types, "rehost order/placement: {rendered}");
    }

    #[test]
    fn dep_rehost_missing_to_rejected() {
        let msg = dep_err(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\nrehost foobar/io testapi/io;\n",
        );
        assert!(msg.contains("to <local>"), "got: {msg}");
    }

    #[test]
    fn dep_retype_parsed_sorted_after_rehost() {
        // `retype` statements (module + per-type forms), given out of
        // canonical order and interleaved with a `rehost`. The formatter
        // emits the `rehost` group first, then the `retype` group sorted by
        // `from` (module form before the per-type one on the same module).
        let f = dep("dependency foobar;\n\
             source { path \"../foo/foo.pkg.kio\"; }\n\
             retype foobar/zmod to local/zmod;\n\
             rehost foobar/io to testapi/io;\n\
             retype foobar/amod.Tag to local/amod.Tag;\n\
             retype foobar/amod to local/amod;\n");
        assert_eq!(f.rehost.len(), 1);
        assert_eq!(f.retype.len(), 3);
        // The per-type form records the single newtype name; the module
        // form records `None`.
        let per_type = f
            .retype
            .iter()
            .find(|r| r.type_name.as_deref() == Some("Tag"))
            .expect("per-type retype");
        assert_eq!(
            per_type.from.segments.last().map(|s| s.name.as_str()),
            Some("amod")
        );
        let rendered = crate::pretty::pretty_dependency_file(&f);
        let rehost = rendered.find("rehost foobar/io").expect("rehost");
        let amod = rendered
            .find("retype foobar/amod to local/amod;")
            .expect("amod module retype");
        let amod_tag = rendered
            .find("retype foobar/amod.Tag to local/amod.Tag;")
            .expect("amod per-type retype");
        let zmod = rendered
            .find("retype foobar/zmod to local/zmod;")
            .expect("zmod retype");
        // rehost group first, then retype sorted: amod (module) < amod.Tag
        // (per-type) < zmod.
        assert!(
            rehost < amod && amod < amod_tag && amod_tag < zmod,
            "retype order/placement: {rendered}"
        );
    }

    #[test]
    fn dep_retype_missing_to_rejected() {
        let msg = dep_err(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\nretype foobar/m local/m;\n",
        );
        assert!(msg.contains("to <local>"), "got: {msg}");
    }

    #[test]
    fn dep_retype_per_type_name_mismatch_rejected() {
        let msg = dep_err(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\n\
             retype foobar/m.Foo to local/m.Bar;\n",
        );
        assert!(msg.contains("same newtype on both sides"), "got: {msg}");
    }

    #[test]
    fn dep_retype_lopsided_per_type_rejected() {
        let msg = dep_err(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\n\
             retype foobar/m.Foo to local/m;\n",
        );
        assert!(msg.contains("both sides"), "got: {msg}");
    }

    #[test]
    fn dep_header_stem_mismatch_rejected() {
        let msg = match parse_dependency_file(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\n",
            Some("other"),
        ) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected stem mismatch error"),
        };
        assert!(msg.contains("does not match filename stem"), "got: {msg}");
    }

    #[test]
    fn dep_header_missing_dependency_keyword_rejected() {
        let msg = dep_err("package foobar;\nsource { path \"foo.pkg.kio\"; }\n");
        assert!(msg.contains("dependency <local>"), "got: {msg}");
    }

    #[test]
    fn dep_absolute_path_rejected() {
        let msg = dep_err("dependency foobar;\nsource { path \"/abs/foo.pkg.kio\"; }\n");
        assert!(msg.contains("must be relative"), "got: {msg}");
    }

    #[test]
    fn dep_backslash_path_rejected() {
        let msg = dep_err("dependency foobar;\nsource { path \"..\\\\foo\\\\foo.pkg.kio\"; }\n");
        assert!(msg.contains("must use `/`"), "got: {msg}");
    }

    #[test]
    fn dep_valid_git_ref() {
        let f = dep(
            "dependency foobar;\nsource { git \"https://example.com/foo.git\"; ref \"main\"; }\n",
        );
        assert_eq!(f.name, "foobar");
        assert_eq!(dep_git(&f), ("https://example.com/foo.git", "main"));
    }

    #[test]
    fn dep_git_file_url_and_sha_ref() {
        // A `file://` URL and a commit-SHA ref both parse — the parser
        // is uniform over the ref's shape (git resolves branch/tag/sha).
        let f = dep(
            "dependency lib;\nsource { git \"file:///srv/lib.git\"; ref \"0123456789abcdef0123456789abcdef01234567\"; }\n",
        );
        assert_eq!(
            dep_git(&f),
            (
                "file:///srv/lib.git",
                "0123456789abcdef0123456789abcdef01234567"
            )
        );
    }

    #[test]
    fn dep_git_without_ref_rejected() {
        let msg = dep_err("dependency foobar;\nsource { git \"https://example.com/foo.git\"; }\n");
        assert!(msg.contains("`git` source requires a `ref"), "got: {msg}");
    }

    #[test]
    fn dep_git_manifest_path_permutations_preserve_comments_and_format() {
        let fields = [
            "// repository\ngit \"repo\";\n",
            "// revision\nref \"main\";\n",
            "// manifest\npath \"packages/lib/lib.pkg.kio\";\n",
        ];
        let mut canonical = None;
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let input = format!(
                "dependency lib;\nsource {{\n{}{}{} }}\n",
                fields[order[0]], fields[order[1]], fields[order[2]]
            );
            let file = dep(&input);
            let SourceOrigin::Git(source) = &file.source.origin else {
                panic!("git source")
            };
            assert_eq!(
                source.manifest_path.as_ref().unwrap().path,
                "packages/lib/lib.pkg.kio"
            );
            let rendered = crate::pretty::pretty_dependency_file(&file);
            for (comment, field) in [
                ("repository", "git"),
                ("revision", "ref"),
                ("manifest", "path"),
            ] {
                assert_eq!(rendered.matches(&format!("// {comment}")).count(), 1);
                assert!(
                    rendered.contains(&format!("// {comment}\n  {field} ")),
                    "{rendered}"
                );
            }
            assert_eq!(
                crate::pretty::pretty_dependency_file(&dep(&rendered)),
                rendered
            );
            assert_eq!(canonical.get_or_insert(rendered.clone()), &rendered);
        }
    }

    #[test]
    fn dep_git_manifest_path_relative_syntax_is_portable() {
        for path in [
            "/root/lib.pkg.kio",
            "C:/lib.pkg.kio",
            "z:lib.pkg.kio",
            r"a\lib.pkg.kio",
        ] {
            let quoted = format!("{path:?}");
            let input =
                format!("dependency lib; source {{ git \"repo\"; ref \"main\"; path {quoted}; }}");
            let error =
                parse_dependency_file(&input, None).expect_err("rooted or non-slash selector");
            let (span, message) = error.diag();
            assert_eq!(&input[span.start as usize..span.end as usize], quoted);
            assert!(message.contains("git manifest `path`"), "{message}");
        }
        for path in ["./lib.pkg.kio", "packages/../lib.pkg.kio", "../lib.pkg.kio"] {
            let input = format!(
                "dependency lib; source {{ git \"repo\"; ref \"main\"; path \"{path}\"; }}"
            );
            assert!(matches!(dep(&input).source.origin, SourceOrigin::Git(_)));
        }
    }

    #[test]
    fn dep_git_manifest_path_requires_unique_string_and_complete_origin() {
        for (fields, fragment) in [
            (
                "git \"repo\"; ref \"main\"; path \"a.pkg.kio\"; path \"b.pkg.kio\";",
                "duplicate `path`",
            ),
            (
                "git \"repo\"; ref \"main\"; path ();",
                "requires a string-literal",
            ),
            ("git \"repo\"; path \"a.pkg.kio\";", "requires a `ref"),
            ("ref \"main\"; path \"a.pkg.kio\";", "`ref` without a `git`"),
        ] {
            let message = dep_err(&format!("dependency lib; source {{ {fields} }}"));
            assert!(message.contains(fragment), "{message}");
        }
    }

    #[test]
    fn dep_ref_without_git_rejected() {
        let msg = dep_err("dependency foobar;\nsource { path \"foo.pkg.kio\"; ref \"main\"; }\n");
        assert!(msg.contains("`ref` without a `git`"), "got: {msg}");
    }

    #[test]
    fn dep_git_requires_string_url() {
        let msg = dep_err("dependency foobar;\nsource { git foo; ref \"main\"; }\n");
        assert!(
            msg.contains("`git` requires a string-literal value"),
            "got: {msg}"
        );
    }

    #[test]
    fn dep_file_and_url_source_keys_rejected_as_unknown() {
        for key in ["file", "url"] {
            let msg = dep_err(&format!("dependency foobar;\nsource {{ {key} \"x\"; }}\n"));
            assert!(
                msg.contains(&format!("unknown key `{key}`")),
                "key {key}: got: {msg}"
            );
        }
    }

    #[test]
    fn dep_unknown_source_key_rejected() {
        let msg = dep_err("dependency foobar;\nsource { wat \"x\"; }\n");
        assert!(msg.contains("unknown key `wat`"), "got: {msg}");
    }

    #[test]
    fn dep_empty_source_rejected() {
        let msg = dep_err("dependency foobar;\nsource { }\n");
        assert!(msg.contains("must declare a source"), "got: {msg}");
    }

    #[test]
    fn dep_missing_source_block_rejected() {
        let msg = dep_err("dependency foobar;\n");
        assert!(msg.contains("`source {"), "got: {msg}");
    }

    #[test]
    fn dep_path_requires_string_value() {
        let msg = dep_err("dependency foobar;\nsource { path foo; }\n");
        assert!(
            msg.contains("requires a string-literal value"),
            "got: {msg}"
        );
    }

    #[test]
    fn dep_trailing_garbage_rejected() {
        let msg = dep_err("dependency foobar;\nsource { path \"foo.pkg.kio\"; }\nextra junk\n");
        assert!(msg.contains("or end of dependency file"), "got: {msg}");
    }

    #[test]
    fn dep_second_source_block_rejected() {
        let msg = dep_err(
            "dependency foobar;\nsource { path \"foo.pkg.kio\"; }\nsource { path \"bar.pkg.kio\"; }\n",
        );
        assert!(msg.contains("only one `source` block"), "got: {msg}");
    }

    // ---- Dependency lock files (`<local>.lock.kio`) -------------------

    fn lock(src: &str) -> LockFile {
        parse_lock_file(src, None)
            .unwrap_or_else(|e| panic!("parse_lock_file failed for {src:?}: {e:?}"))
    }

    fn lock_err(src: &str) -> String {
        match parse_lock_file(src, None) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected parse error for {src:?}"),
        }
    }

    #[test]
    fn lock_valid() {
        let f = lock(
            "lock foobar;\nresolved {\n  git \"https://example.com/foo.git\";\n  ref \"main\";\n  commit \"0123456789abcdef0123456789abcdef01234567\";\n  sig \"abc123\";\n}\n",
        );
        assert_eq!(f.name, "foobar");
        assert_eq!(f.url, "https://example.com/foo.git");
        assert_eq!(f.git_ref, "main");
        assert_eq!(f.manifest_path, None);
        assert_eq!(f.commit, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(f.sig, "abc123");
    }

    #[test]
    fn lock_manifest_path_is_optional_unordered_and_comment_preserving() {
        let file = lock(
            "lock lib; resolved {\n// digest\nsig \"s\";\n// manifest\npath \"packages/lib/lib.pkg.kio\";\n// commit\ncommit \"c\";\n// revision\nref \"main\";\n// repository\ngit \"repo\";\n}",
        );
        assert_eq!(
            file.manifest_path.as_deref(),
            Some("packages/lib/lib.pkg.kio")
        );
        let rendered = crate::pretty::pretty_lock_file(&file);
        let positions = ["git", "ref", "path", "commit", "sig"]
            .map(|key| rendered.find(&format!("  {key} \"")).unwrap());
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{rendered}"
        );
        for (comment, field) in [
            ("repository", "git"),
            ("revision", "ref"),
            ("manifest", "path"),
            ("commit", "commit"),
            ("digest", "sig"),
        ] {
            assert_eq!(rendered.matches(&format!("// {comment}")).count(), 1);
            assert!(
                rendered.contains(&format!("// {comment}\n  {field} ")),
                "{rendered}"
            );
        }
        assert_eq!(crate::pretty::pretty_lock_file(&lock(&rendered)), rendered);
        let old = "lock lib;\n\nresolved {\n  git \"repo\";\n  ref \"main\";\n  commit \"c\";\n  sig \"s\"\n}\n";
        assert_eq!(crate::pretty::pretty_lock_file(&lock(old)), old);
    }

    #[test]
    fn lock_manifest_path_rejects_duplicate_non_string_and_rooted_syntax() {
        for (fields, fragment) in [
            (
                "path \"a.pkg.kio\"; path \"b.pkg.kio\";",
                "duplicate `path`",
            ),
            ("path ();", "requires a string-literal"),
            ("path \"C:/a.pkg.kio\";", "must be relative"),
            ("path \"a\\\\b.pkg.kio\";", "must use `/`"),
        ] {
            let message = lock_err(&format!(
                "lock lib; resolved {{ git \"repo\"; ref \"main\"; commit \"c\"; sig \"s\"; {fields} }}"
            ));
            assert!(message.contains(fragment), "{message}");
        }
    }

    #[test]
    fn lock_header_stem_mismatch_rejected() {
        let msg = match parse_lock_file(
            "lock foobar;\nresolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; }\n",
            Some("other"),
        ) {
            Err(e) => e.diag().1.to_owned(),
            Ok(_) => panic!("expected stem mismatch error"),
        };
        assert!(msg.contains("does not match filename stem"), "got: {msg}");
    }

    #[test]
    fn lock_missing_lock_keyword_rejected() {
        let msg = lock_err(
            "dependency foobar;\nresolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; }\n",
        );
        assert!(msg.contains("lock <local>"), "got: {msg}");
    }

    #[test]
    fn lock_missing_resolved_block_rejected() {
        let msg = lock_err("lock foobar;\n");
        assert!(msg.contains("resolved {"), "got: {msg}");
    }

    #[test]
    fn lock_missing_commit_rejected() {
        let msg = lock_err("lock foobar;\nresolved { git \"u\"; ref \"r\"; sig \"s\"; }\n");
        assert!(msg.contains("must declare a `commit`"), "got: {msg}");
    }

    #[test]
    fn lock_missing_sig_rejected() {
        let msg = lock_err("lock foobar;\nresolved { git \"u\"; ref \"r\"; commit \"c\"; }\n");
        assert!(msg.contains("must declare a `sig`"), "got: {msg}");
    }

    #[test]
    fn lock_missing_git_rejected() {
        let msg = lock_err("lock foobar;\nresolved { ref \"r\"; commit \"c\"; sig \"s\"; }\n");
        assert!(msg.contains("must declare a `git`"), "got: {msg}");
    }

    #[test]
    fn lock_unknown_key_rejected() {
        let msg = lock_err(
            "lock foobar;\nresolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; wat \"x\"; }\n",
        );
        assert!(msg.contains("unknown key `wat`"), "got: {msg}");
    }

    #[test]
    fn lock_duplicate_key_rejected() {
        let msg = lock_err(
            "lock foobar;\nresolved { git \"u\"; git \"v\"; ref \"r\"; commit \"c\"; sig \"s\"; }\n",
        );
        assert!(msg.contains("duplicate `git`"), "got: {msg}");
    }

    #[test]
    fn lock_trailing_garbage_rejected() {
        let msg = lock_err(
            "lock foobar;\nresolved { git \"u\"; ref \"r\"; commit \"c\"; sig \"s\"; }\njunk\n",
        );
        assert!(msg.contains("expected end of lock file"), "got: {msg}");
    }

    #[test]
    fn lock_non_string_value_rejected() {
        let msg =
            lock_err("lock foobar;\nresolved { git u; ref \"r\"; commit \"c\"; sig \"s\"; }\n");
        assert!(
            msg.contains("requires a string-literal value"),
            "got: {msg}"
        );
    }
}

/// Public grammar serialization preserves complete shapes and token boundaries.
#[cfg(all(test, feature = "repl"))]
mod operator_grammar_proptests {
    use crate::ast::{OpPart, OperatorGrammar};
    use crate::span::Span;
    use proptest::prelude::*;

    fn token() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "+", "-", "*", "..", ".+.", "+.", "<.>", "&&", "++", "?", ":", "$", "<|", "=>",
        ])
        .prop_map(str::to_owned)
    }

    fn run() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(token(), 1..6)
    }

    fn grammar() -> impl Strategy<Value = OperatorGrammar> {
        prop_oneof![
            (any::<bool>(), any::<bool>(), run()).prop_map(|(prefix, lenient, run)| {
                let span = Span::new(0, 0);
                let slot = OpPart::SlotPlain { span, lenient };
                let mut pattern = Vec::new();
                if !prefix {
                    pattern.push(slot.clone());
                }
                pattern.extend(run.into_iter().map(|content| OpPart::Token {
                    content,
                    span,
                    lenient: false,
                    quoted: false,
                }));
                pattern.push(slot);
                OperatorGrammar::fixed(&pattern)
            }),
            (
                prop::collection::vec(
                    prop::sample::select(vec!['+', '-', '*', '%', '<', '>', '?', ':', '[']),
                    1..16
                ),
                any::<bool>()
            )
                .prop_map(|(parts, leading)| {
                    let symbols: String = parts.into_iter().collect();
                    let open = if leading {
                        format!("[{symbols}")
                    } else {
                        format!("{symbols}[")
                    };
                    let close = open
                        .chars()
                        .rev()
                        .map(|ch| if ch == '[' { ']' } else { ch })
                        .collect();
                    OperatorGrammar::Variadic {
                        open: vec![open],
                        close: vec![close],
                    }
                }),
        ]
    }

    proptest! {
        #[test]
        fn render_then_parse_is_identity(grammar in grammar()) {
            let rendered = grammar.render();
            let parsed = super::parse_operator_grammar(&rendered)
                .unwrap_or_else(|error| panic!("parse failed for {rendered:?}: {error:?}"));
            prop_assert_eq!(parsed, grammar);
        }

        #[test]
        fn grammar_is_a_fixpoint(grammar in grammar()) {
            let rendered = grammar.render();
            let parsed = super::parse_operator_grammar(&rendered).unwrap();
            prop_assert_eq!(parsed.render(), rendered);
        }

        #[test]
        fn distinct_grammars_have_distinct_renderings(a in grammar(), b in grammar()) {
            if a != b { prop_assert_ne!(a.render(), b.render()); }
        }
    }
}

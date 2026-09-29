//! The expression-query entry point for `kio repl`.
//!
//! `:normalize <expr>`, the expression branches of
//! `:t <name-or-expression>` and `:pure <name-or-expression>`, and bare
//! compound-expression input all
//! need the typed `Prime` form of an expression: the AST
//! the front-end produces after desugaring, label lowering, elaborator
//! discharge, UFCS rewriting, and type-argument inference. This module
//! is the seam that produces it.
//!
//! ## How an expression query runs
//!
//! The Kio front-end has no standalone single-expression entry point —
//! it type-checks whole packages. So [`query_expr`] wraps the
//! queried expression in a synthetic function appended to the current
//! module's source:
//!
//! ```text
//! fn _expr_<n>() {
//!   let _ = <expr>;
//!   ()
//! }
//! ```
//!
//! `:pure` uses the same wrapper with the ordinary `pure fn` modifier, so its
//! verdict comes from the compiler's shared pure-function rule rather than a
//! REPL-specific classifier. The wrapped module text is supplied to the
//! type-checker through a
//! [`SourceOverlay`](crate::package_collection::SourceOverlay), so the
//! synthetic function sees exactly the current module's imports and declared
//! items — name resolution for the expression matches every other REPL
//! command. The whole package is type-checked; on success the synthetic
//! function's `let`-binding RHS is the typed `Prime` expression a query
//! renders, and the position index carries its synthesized type.
//!
//! The synthetic function is invisible to the user: it is never listed,
//! and a type error inside it is reported as an error *in the
//! expression*, with no mention of the `_expr_<n>` wrapper.
//!
//! ## Name-shape vs. compound-expression classification
//!
//! A bare *name* — a single identifier, a dotted path, a
//! fully-qualified `mod/path.item`, or an operator symbol — routes to
//! `:doc` (its doc-comment + signature) from a bare prompt. A compound
//! expression reaches [`query_expr`]. The dual-category `:t` command
//! gives resolution priority to a declared name or exact item FQN, then
//! retries an unresolved dotted name shape as an expression when the current
//! module grammar parses it that way. This includes ordinary alias-qualified
//! value paths and slash-operator expressions with dotted right operands.
//! [`classify_input`] draws the lexical line; command dispatch then applies
//! those category rules explicitly.

use std::collections::HashSet;

use crate::ast::{Expr, Item, Lowered, Prime, Purity, Type};
use crate::cmd::check::analyze_root_typed_with_overlay;
use crate::normalization::{
    AuthenticatedEvalCtx, Env, EvalMetrics, EvalMetricsSnapshot, Value, eval_authenticated,
    structural_recur_stuck_message,
};
use crate::pass::lexer::{TokenKind, lex};
use crate::pass::substitute::substitute_expr_for_eval;

use super::session::Session;

/// What a non-`:` REPL input line is, syntactically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// A bare name — a single identifier (`foo`), a module path
    /// (`demo/main`), a dotted path (`alias.item`), a slash-qualified FQN
    /// (`demo/main.item`), or an operator symbol (`+`, `(+)`). The carried
    /// string is the normalized name spelling (the parentheses around a
    /// parenthesized operator are stripped). Routes to `:doc`.
    BareName(String),
    /// Any other expression the Kio parser accepts in module-body
    /// context — a literal, an application, an operator expression, …
    /// The carried string is the trimmed input. Routes to
    /// [`query_expr`].
    Compound(String),
}

/// Classify a non-`:` REPL input line as a bare name or a compound
/// expression.
///
/// A bare name is one of:
///
/// - a single [`TokenKind::Ident`] — `foo`;
/// - a module path — `Ident (/ Ident)+`, e.g. `demo/main`;
/// - a dotted path — `Ident (. Ident)+`, e.g. `alias.item`;
/// - a slash-qualified FQN — `ModulePath . Ident`, e.g. `demo/main.item`;
/// - a bare operator symbol — a single [`TokenKind::SymbolRun`] that is
///   not the path dot (`+`, `++`, `<*>`);
/// - a parenthesized operator — `( SymbolRun )`, e.g. `(+)`.
///
/// Everything else — a literal, an application, an operator expression,
/// a parenthesized non-operator — is a compound expression. Per the
/// "shortest parse wins for the name kind" rule, a bare `foo` is a
/// name, never a zero-argument call.
///
/// Returns `None` when the input does not lex at all (it cannot be a
/// name *or* an expression — the caller reports a syntax error).
pub fn classify_input(input: &str) -> Option<InputKind> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip trivia (comments) — only the significant tokens decide the
    // shape. A lexer failure means the input is not even tokenizable.
    let tokens = lex(trimmed).ok()?;
    let kinds: Vec<&TokenKind> = tokens.iter().map(|t| &t.kind).collect();

    if let Some(name) = bare_name_of(&kinds) {
        Some(InputKind::BareName(name))
    } else {
        Some(InputKind::Compound(trimmed.to_owned()))
    }
}

/// If `kinds` is exactly a bare-name token shape, return the name
/// spelling; otherwise `None`.
fn bare_name_of(kinds: &[&TokenKind]) -> Option<String> {
    match kinds {
        // A single identifier.
        [TokenKind::Ident(name)] => Some(name.clone()),
        // A single operator symbol — but not the path dot, which is
        // never a name on its own.
        [TokenKind::SymbolRun(op)] if op != "." => Some(op.clone()),
        // A parenthesized operator: `( SymbolRun )`.
        [
            TokenKind::LParen,
            TokenKind::SymbolRun(op),
            TokenKind::RParen,
        ] if op != "." => Some(op.clone()),
        // A module path — `Ident (/ Ident)+` — or an item-access /
        // fully-qualified path — `Ident (. Ident)+`. The module-path
        // shape is tried first so a slash-joined name reads as a module
        // reference (routes to `:doc`), the dotted shape second. The
        // genuine fully-qualified form `module/path.item` mixes `/` and
        // `.`, so it matches neither uniform shape — recognise it last.
        _ => separated_path_of(kinds, "/")
            .or_else(|| separated_path_of(kinds, "."))
            .or_else(|| mixed_fqn_of(kinds)),
    }
}

/// If `kinds` is the genuine fully-qualified shape — a `/`-joined module
/// path followed by a single dotted item, `Ident (/ Ident)* . Ident` —
/// return the joined spelling (`module/path.item`); otherwise `None`.
///
/// This is the form `:which` / `split_fqn` resolve (`module/path` is the
/// module, the trailing `.item` reaches the declared item). It mixes the
/// two separators, so neither uniform [`separated_path_of`] shape catches
/// it. A name with no `.` is a pure module path (handled by the `/`
/// shape); a name with no `/` is a dotted access (handled by the `.`
/// shape); only the mixed form reaches here.
fn mixed_fqn_of(kinds: &[&TokenKind]) -> Option<String> {
    // Split at the final `.`: everything before it must be a `/`-joined
    // module path (`Ident (/ Ident)*`), and exactly one `Ident` must
    // follow.
    let dot = kinds
        .iter()
        .rposition(|k| matches!(k, TokenKind::SymbolRun(op) if op == "."))?;
    // The item segment is a single identifier immediately after the dot.
    let TokenKind::Ident(item) = kinds.get(dot + 1)? else {
        return None;
    };
    if dot + 2 != kinds.len() {
        return None;
    }
    // The module portion is a `/`-joined path: `Ident (/ Ident)*`. A
    // lone `Ident` (no slash) is admissible too — `m.item` where `m` is a
    // single-segment module path.
    let module = &kinds[..dot];
    if module.is_empty() {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for (i, kind) in module.iter().enumerate() {
        if i.is_multiple_of(2) {
            match kind {
                TokenKind::Ident(name) => segments.push(name),
                _ => return None,
            }
        } else {
            match kind {
                TokenKind::SymbolRun(op) if op == "/" => {}
                _ => return None,
            }
        }
    }
    // A trailing `/` (even module length) is malformed.
    if module.len().is_multiple_of(2) {
        return None;
    }
    Some(format!("{}.{item}", segments.join("/")))
}

/// If `kinds` is a path of identifiers joined by the single-character
/// `sep` operator (`Ident (<sep> Ident)+`), return the joined spelling;
/// otherwise `None`. A single identifier is handled by the caller and
/// rejected here (a path needs at least one separator).
fn separated_path_of(kinds: &[&TokenKind], sep: &str) -> Option<String> {
    // Need an odd number of tokens, at least 3: Ident <sep> Ident […]
    if kinds.len() < 3 || kinds.len().is_multiple_of(2) {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        if i.is_multiple_of(2) {
            // Even positions are identifiers.
            match kind {
                TokenKind::Ident(name) => segments.push(name),
                _ => return None,
            }
        } else {
            // Odd positions are the separator.
            match kind {
                TokenKind::SymbolRun(op) if op == sep => {}
                _ => return None,
            }
        }
    }
    Some(segments.join(sep))
}

/// A successful expression query: its typed `Prime` AST, its
/// synthesized type, and its residual normal form.
pub struct ExprQueryResult {
    /// The expression's typed `Prime` form — every surface form
    /// removed, every elaborator discharged, every inferred type-argument
    /// filled in.
    pub typed_expr: Expr<Prime>,
    /// The expression's synthesized type. Carried at the `Lowered`
    /// phase because the position index is populated during the
    /// `Lowered`-phase typer walk; `Lowered` and `Prime` share the
    /// same surface-free type grammar, so this renders identically to
    /// the `Prime` type.
    pub ty: Type<Lowered>,
    /// The expression's residual normal form after applying every
    /// reduction the host-independent partial evaluator admits — the same
    /// reduction relation `equiv`-discharge uses. Pure expressions
    /// reduce to ground values; host items reduce to opaque atoms,
    /// stuck applications carry the reduced callee and args.
    pub value: Value,
    /// The residual rendered in the current module's scope. When an inlined
    /// dependency contributes a newtype member that the current scope cannot
    /// name, this includes the ordinary qualified imports needed to make that
    /// declaration identity unambiguous.
    pub rendered_value: String,
}

/// Why an expression query could not be answered.
pub enum ExprQueryError {
    /// No current module is set, so short names in the expression have
    /// nothing to resolve against, and there is no module overlay in which
    /// to host the synthetic function.
    NoCurrentModule,
    /// The current module's source could not be read from the cached
    /// analysis.
    NoModuleSource,
    /// The input did not parse as a Kio expression.
    Syntax(String),
    /// The expression parsed but did not type-check. The message is the
    /// type-checker's diagnostic; any reference to the synthetic
    /// wrapper has been removed.
    TypeError(String),
    /// The package itself does not type-check, independent of the queried
    /// expression, or evaluation failed its compile-time Totality check. The
    /// interactive command path retains those categories separately until
    /// rendering; this public compatibility enum does not grow a new variant.
    PackageError(String),
}

/// A command-facing expression-query failure. Structural-recursion Totality
/// faults stay typed until the command renderer; the public query API maps
/// them to its existing [`ExprQueryError::PackageError`] compatibility shape.
pub(super) enum ReplExprQueryError {
    /// An ordinary failure represented by the source-compatible public API.
    Query(ExprQueryError),
    /// An executed structural-recursion edge failed its Totality check.
    Totality(String),
}

/// Whether an expression is admitted in an ordinary function's pure context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PurityVerdict {
    Pure,
    Impure,
}

impl PurityVerdict {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Pure => "pure",
            Self::Impure => "impure",
        }
    }
}

impl From<ExprQueryError> for ReplExprQueryError {
    fn from(error: ExprQueryError) -> Self {
        Self::Query(error)
    }
}

impl ReplExprQueryError {
    fn into_public(self) -> ExprQueryError {
        match self {
            Self::Query(error) => error,
            Self::Totality(message) => ExprQueryError::PackageError(format!(
                "compile-time evaluation failed its Totality check: {message}"
            )),
        }
    }
}

/// Query `expr_src` against the session's current module.
///
/// Wraps the expression in a synthetic function, type-checks the
/// package through a [`SourceOverlay`], and returns the typed `Prime`
/// expression, its synthesized type, and its residual normal form
/// (the result of applying every reduction the host-independent
/// partial evaluator admits). See the module documentation for the wrapping
/// scheme; see [`normalization`](crate::normalization) for the reduction
/// relation. A compile-time Totality failure uses the existing
/// [`ExprQueryError::PackageError`] public compatibility arm; interactive
/// commands preserve and render that category separately.
pub fn query_expr(
    session: &mut Session,
    expr_src: &str,
) -> Result<ExprQueryResult, ExprQueryError> {
    query_expr_for_repl(session, expr_src).map_err(ReplExprQueryError::into_public)
}

/// Run an expression query while retaining command-only diagnostic categories.
pub(super) fn query_expr_for_repl(
    session: &mut Session,
    expr_src: &str,
) -> Result<ExprQueryResult, ReplExprQueryError> {
    let expr_src = expr_src.trim();

    let current = session
        .current()
        .ok_or(ExprQueryError::NoCurrentModule)?
        .to_owned();
    let entry = session
        .module(&current)
        .ok_or(ExprQueryError::NoCurrentModule)?;
    let module_file = entry.file_path.clone();
    let original = session
        .source_of(&module_file)
        .ok_or(ExprQueryError::NoModuleSource)?
        .to_owned();
    let original_item_count = entry.module.items.len();

    let n = session.next_expr_counter();
    let wrap = SyntheticWrap::build(&original, expr_src, n);
    wrap.authenticate_parse_shape(original_item_count)?;

    // The overlay key must match the walker's canonical-path scheme.
    let canonical = std::fs::canonicalize(&module_file).unwrap_or(module_file.clone());
    let mut overlay = session.source_overlay().clone();
    overlay.insert(canonical, wrap.source.clone());

    let analysis = match analyze_root_typed_with_overlay(session.package_root(), &overlay) {
        Ok(a) => a,
        Err(failure) => return Err(wrap.classify_failure(&failure, &module_file)),
    };

    // Find the synthetic function in the current module and read its
    // wildcard-`let` RHS: the typed expression. The `Prime` form is the
    // rendered typed AST; the `Lowered` form remains on this adapter
    // side of the evaluator boundary and is consumed into an
    // `UncheckedPrime` root before reduction.
    let module = analysis.root_package.module(&current).ok_or_else(|| {
        ExprQueryError::PackageError(format!("module `{current}` vanished from the package"))
    })?;
    let typed_expr = wrap.extract_let_rhs(&module.module.items).ok_or_else(|| {
        ExprQueryError::PackageError(
            "the synthetic expression statement was not produced by the type-checker".to_owned(),
        )
    })?;

    let lowered_module = analysis
        .checked_lowered
        .package()
        .module(&current)
        .ok_or_else(|| {
            ExprQueryError::PackageError(format!(
                "module `{current}` vanished from the lowered package"
            ))
        })?;
    let lowered_expr = wrap
        .extract_let_rhs(&lowered_module.module.items)
        .ok_or_else(|| {
            ExprQueryError::PackageError(
                "the synthetic expression statement was not produced at the lowered phase"
                    .to_owned(),
            )
        })?;

    // The synthesized type is recorded in the position index at the
    // expression's *source* span. Substitution can rewrite the typed
    // `Prime` node (a `match!` / `if` / elaboration replaces the
    // surface node entirely), so the type is keyed off the original
    // byte range, not the post-substitution node's span — the position
    // index is populated pre-substitution.
    let ty = wrap
        .expr_type(&current, &analysis.position_index)
        .ok_or_else(|| {
            ExprQueryError::PackageError(
                "the type-checker recorded no type for the expression".to_owned(),
            )
        })?;

    // Reduce the typed expression to its residual normal form.
    // The evaluation context is built against the current module the
    // same way `kio test` builds it for `equiv` discharge — same-module
    // declarations plus selective imports and `import m as alias;`
    // qualified imports. The result is reduced lazily through
    // the host-independent partial evaluator: pure expressions collapse to
    // ground values, host items stay as opaque `Value::Atom`s,
    // applications that can't be reduced residualize as `Value::Stuck`.
    let value = reduce_lowered(lowered_expr, &analysis.checked_lowered, &current)?;
    let rendered_value = crate::normalization::value_to_repl_string(&value, &lowered_module.module);

    Ok(ExprQueryResult {
        typed_expr: typed_expr.clone(),
        ty,
        value,
        rendered_value,
    })
}

/// Check whether `expr_src` is admissible in an ordinary `pure fn` body.
///
/// The first analysis wraps the expression in a synthetic `pure fn`, so the
/// verdict comes from the same resolved-reference rule used for source
/// functions. When that fails, an ordinary-function retry distinguishes an
/// impure well-typed expression from malformed or ill-typed input. The retry
/// is structural — no diagnostic text is inspected — and neither analysis
/// evaluates the expression.
pub(super) fn query_expr_purity_for_repl(
    session: &mut Session,
    expr_src: &str,
) -> Result<PurityVerdict, ReplExprQueryError> {
    let expr_src = expr_src.trim();

    let current = session
        .current()
        .ok_or(ExprQueryError::NoCurrentModule)?
        .to_owned();
    let entry = session
        .module(&current)
        .ok_or(ExprQueryError::NoCurrentModule)?;
    let module_file = entry.file_path.clone();
    let original = session
        .source_of(&module_file)
        .ok_or(ExprQueryError::NoModuleSource)?
        .to_owned();
    let original_item_count = entry.module.items.len();
    let canonical = std::fs::canonicalize(&module_file).unwrap_or(module_file.clone());
    let n = session.next_expr_counter();

    let pure_wrap = SyntheticWrap::build_pure(&original, expr_src, n);
    pure_wrap.authenticate_parse_shape(original_item_count)?;
    let mut pure_overlay = session.source_overlay().clone();
    pure_overlay.insert(canonical.clone(), pure_wrap.source.clone());
    if analyze_root_typed_with_overlay(session.package_root(), &pure_overlay).is_ok() {
        return Ok(PurityVerdict::Pure);
    }

    let ordinary_wrap = SyntheticWrap::build(&original, expr_src, n);
    ordinary_wrap.authenticate_parse_shape(original_item_count)?;
    let mut ordinary_overlay = session.source_overlay().clone();
    ordinary_overlay.insert(canonical, ordinary_wrap.source.clone());
    match analyze_root_typed_with_overlay(session.package_root(), &ordinary_overlay) {
        Ok(_) => Ok(PurityVerdict::Impure),
        Err(failure) => Err(ordinary_wrap.classify_failure(&failure, &module_file)),
    }
}

/// Reduce `expr` to its residual normal form against `module_path` in
/// `package`, through the immutable artifact-backed evaluator context — the
/// same assembly `kio test`'s equiv runner batches per module — so the prompt
/// reduces exactly the way `equiv` discharge does.
fn reduce_lowered(
    expr: &Expr<Lowered>,
    checked: &crate::pass::typecheck_full::CheckedLoweredPackage,
    module_path: &str,
) -> Result<Value, ReplExprQueryError> {
    let package = checked.package();
    let source_module = package.module(module_path).ok_or_else(|| {
        ExprQueryError::PackageError(format!(
            "module `{module_path}` vanished from the lowered package"
        ))
    })?;
    let eval_expr = substitute_expr_for_eval(
        expr,
        checked.elaborations(),
        module_path,
        Some(package),
        Some(&source_module.module),
    );
    let eval_package = checked.eval_artifact().map_err(|located| {
        ExprQueryError::PackageError(located.error.diagnostic().message.clone())
    })?;
    let eval_module = eval_package.module(module_path).ok_or_else(|| {
        ExprQueryError::PackageError(format!(
            "module `{module_path}` vanished from the evaluator artifact"
        ))
    })?;

    let metrics = EvalMetrics::enabled();
    let mut ctx = AuthenticatedEvalCtx::for_module(&eval_package, &eval_module.module, module_path)
        .map_err(|error| ExprQueryError::PackageError(error.diagnostic().message.clone()))?;
    if let Some(metrics) = &metrics {
        ctx = ctx.with_metrics(metrics.clone());
    }
    let value = eval_authenticated(&eval_expr, &Env::new(), &ctx);
    if let Some(metrics) = metrics {
        log_eval_timing_line("repl-normalize", metrics.snapshot());
    }
    checked_reduction_result(value, &ctx)
}

fn checked_reduction_result(
    value: Value,
    ctx: &crate::normalization::EvalCtx<'_>,
) -> Result<Value, ReplExprQueryError> {
    match structural_recur_stuck_message(&value, ctx) {
        Some(message) => Err(ReplExprQueryError::Totality(message)),
        None => Ok(value),
    }
}

fn log_eval_timing_line(label: &str, eval: EvalMetricsSnapshot) {
    eprintln!(
        "eval-timing: {} root_eval_ms={:.3} root_eval_calls={} expr_visits={} \
         apply_ms={:.3} apply_calls={} closure_apply_ms={:.3} closure_apply_calls={} \
         comptime_ms={:.3} comptime_calls={} type_unfold_ms={:.3} \
         type_unfold_calls={} type_equiv_ms={:.3} \
         type_equiv_calls={} nf_eq_ms={:.3} nf_eq_calls={} eta_contract_ms={:.3} \
         eta_contract_calls={} env_clones={} closure_builds={}",
        label,
        duration_ms(eval.root_eval),
        eval.root_eval_calls,
        eval.expr_visits,
        duration_ms(eval.apply),
        eval.apply_calls,
        duration_ms(eval.closure_apply),
        eval.closure_apply_calls,
        duration_ms(eval.reflection),
        eval.reflection_calls,
        duration_ms(eval.type_unfold),
        eval.type_unfold_calls,
        duration_ms(eval.type_equiv),
        eval.type_equiv_calls,
        duration_ms(eval.nf_eq),
        eval.nf_eq_calls,
        duration_ms(eval.eta_contract),
        eval.eta_contract_calls,
        eval.env_clones,
        eval.closure_builds,
    );
}

fn duration_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The synthetic-wrapper bookkeeping for one expression query: the wrapped
/// module source, the synthetic item's name, and the byte range
/// the queried expression occupies inside the wrapped source.
struct SyntheticWrap {
    /// The wrapped module source — the original plus the appended
    /// synthetic function.
    source: String,
    /// The synthetic function's name (`_expr_<n>`).
    fn_name: String,
    /// Byte offset where the queried expression begins in `source`.
    expr_start: usize,
    /// Byte offset just past the queried expression in `source`.
    expr_end: usize,
    /// Purity modifier placed on the synthetic function. This is authenticated
    /// against the parsed wrapper before whole-package analysis.
    purity: Purity,
}

impl SyntheticWrap {
    /// Build the wrapped source for `expr_src` against `original`,
    /// starting the synthetic name search at counter `n`.
    fn build(original: &str, expr_src: &str, n: u64) -> Self {
        Self::build_with_purity(original, expr_src, n, Purity::Impure)
    }

    /// Build the same query wrapper under the compiler's pure-function rule.
    fn build_pure(original: &str, expr_src: &str, n: u64) -> Self {
        Self::build_with_purity(original, expr_src, n, Purity::Pure)
    }

    fn build_with_purity(original: &str, expr_src: &str, n: u64, purity: Purity) -> Self {
        let fn_name = fresh_wrapper_name(original, expr_src, n);
        let modifier = match purity {
            Purity::Impure => "",
            Purity::Pure => "pure ",
        };
        // Append the synthetic function. A trailing newline keeps the
        // appended block on its own line whether or not `original`
        // ended with one.
        let prefix = format!("{original}\n{modifier}fn {fn_name}() {{\n  let _ = ");
        // Start the terminator on a fresh line so a valid trailing `//`
        // comment in the queried expression cannot consume wrapper syntax.
        let suffix = "\n;\n  ()\n}\n";
        let expr_start = prefix.len();
        let expr_end = expr_start + expr_src.len();
        let source = format!("{prefix}{expr_src}{suffix}");
        Self {
            source,
            fn_name,
            expr_start,
            expr_end,
            purity,
        }
    }

    /// Authenticate that a successfully parsed wrapper consists of the
    /// original module plus exactly the synthetic function, whose body is
    /// exactly the expected wildcard let followed by synthetic unit.
    ///
    /// Raw text must be spliced because the front-end deliberately has no
    /// standalone expression parser. The structural check makes that splice a
    /// parser adapter rather than a way for prompt input to inject declarations
    /// or wrapper statements. Parse failures still flow through the ordinary
    /// whole-package analysis below so users retain its precise diagnostics.
    fn authenticate_parse_shape(
        &self,
        original_item_count: usize,
    ) -> Result<(), ReplExprQueryError> {
        let parsed = match crate::pass::parser::parse(&self.source) {
            Ok(module) => module,
            Err(_) => return Ok(()),
        };

        let Some(Item::FnDef(fndef)) = parsed.items.last() else {
            return Err(invalid_single_expression());
        };
        if parsed.items.len() != original_item_count + 1
            || fndef.name != self.fn_name
            || fndef.purity != self.purity
            || !fndef.sig.params.is_empty()
            || !fndef.ret_elided
        {
            return Err(invalid_single_expression());
        }

        let Expr::Let {
            name,
            ty,
            value,
            body,
            ..
        } = &fndef.body
        else {
            return Err(invalid_single_expression());
        };
        if name != "_" || ty.is_some() || !matches!(body.as_ref(), Expr::Unit { .. }) {
            return Err(invalid_single_expression());
        }

        // Parenthesized expressions can legitimately have a narrower AST span
        // than their source text because grouping punctuation is erased. The
        // RHS itself must nevertheless originate wholly inside the prompt
        // range. Since the parser consumes the complete module, the exact body
        // and item topology above proves that no other significant prompt token
        // became a sibling statement or declaration.
        let value_span = value.span();
        if value_span.start < self.expr_start as u32 || value_span.end > self.expr_end as u32 {
            return Err(invalid_single_expression());
        }

        Ok(())
    }

    /// Find the synthetic function among `items` and return its
    /// wildcard-`let` RHS — the typed expression. Phase-polymorphic so
    /// the caller can extract from either the `Prime` (source-faithful
    /// render path) or the `Lowered` (residual-NF reduction path) AST
    /// produced by the same typecheck.
    fn extract_let_rhs<'a, P: crate::ast::Phase>(
        &self,
        items: &'a [Item<P>],
    ) -> Option<&'a Expr<P>> {
        let Item::FnDef(fndef) = items.last()? else {
            return None;
        };
        if fndef.name != self.fn_name {
            return None;
        }
        // The body is `{ let _ = <expr>; () }` — an
        // `Expr::Let` whose `value` is the queried expression.
        match &fndef.body {
            Expr::Let {
                name, value, body, ..
            } if name == "_" && matches!(body.as_ref(), Expr::Unit { .. }) => Some(value),
            _ => None,
        }
    }

    /// Look up the queried expression's synthesized type in the
    /// position `index`.
    ///
    /// The type is recorded keyed by the *source* span of every
    /// expression node the typer visits. The whole-expression type is
    /// the widest recorded span fully contained in the queried
    /// expression's byte range — the outermost node of the expression.
    fn expr_type(
        &self,
        module_path: &str,
        index: &crate::pass::typecheck_full::PositionIndex,
    ) -> Option<Type<Lowered>> {
        let start = self.expr_start as u32;
        let end = self.expr_end as u32;
        let mut best: Option<(u32, &Type<Lowered>)> = None;
        for ((mp, span), ty) in index.types_iter() {
            if mp != module_path {
                continue;
            }
            if span.start < start || span.end > end {
                continue;
            }
            let width = span.end.saturating_sub(span.start);
            if best.is_none_or(|(w, _)| width > w) {
                best = Some((width, ty));
            }
        }
        best.map(|(_, ty)| ty.clone())
    }

    /// Map an [`AnalysisFailure`](crate::cmd::check::AnalysisFailure) from
    /// the wrapped type-check into a command-facing query error.
    ///
    /// An error whose span falls inside the appended synthetic function
    /// is about the queried expression; the wrapper offset is hidden so
    /// the user never sees `_expr_<n>`. An error elsewhere means the
    /// package itself is broken.
    fn classify_failure(
        &self,
        failure: &crate::cmd::check::AnalysisFailure,
        module_file: &std::path::Path,
    ) -> ReplExprQueryError {
        let located = failure.primary_error();
        let (span, message) = located.error.diag();
        let message = message.to_owned();

        if matches!(located.error, crate::error::Error::Totality(_)) {
            return ReplExprQueryError::Totality(message);
        }

        // Is the error in the current module's file, inside the
        // appended synthetic region?
        let in_synthetic_file = located.file_path == module_file
            || std::fs::canonicalize(&located.file_path)
                .ok()
                .zip(std::fs::canonicalize(module_file).ok())
                .is_some_and(|(a, b)| a == b);
        let in_appended_region = in_synthetic_file && (span.start as usize) >= self.expr_start;

        let is_parse = matches!(located.error, crate::error::Error::Parse(_));

        if in_appended_region {
            if is_parse {
                ExprQueryError::Syntax(self.scrub_wrapper_framing(message, span.start)).into()
            } else {
                ExprQueryError::TypeError(self.scrub_wrapper_framing(message, span.start)).into()
            }
        } else {
            // The package itself does not type-check — render a
            // located diagnostic for the offending file.
            let loc = failure
                .sources
                .get(&located.file_path)
                .map(|src| {
                    let (line, col) = line_col(src, span.start);
                    format!("{}:{line}:{col}", located.file_path.display())
                })
                .unwrap_or_else(|| located.file_path.display().to_string());
            ExprQueryError::PackageError(format!("{loc}: {message}")).into()
        }
    }

    /// Strip the synthetic-wrap framing from a diagnostic so no
    /// user-facing message mentions a token the user never typed.
    ///
    /// The wrapper splices the queried expression into
    /// `let _ = <expr>;`, so the trailing `;` terminator can leak into a
    /// parse error. A genuine
    /// incomplete expression (`1 +`) makes the parser report "expected
    /// `;`" against that synthetic terminator — a `;` the user neither
    /// wrote nor can write.
    ///
    /// - When the error sits at the synthetic terminator (`err_start` is
    ///   at or past `expr_end`) and the message is the expected-`;`
    ///   complaint, rephrase it to describe the real fault: the input
    ///   ended before it was a complete expression.
    fn scrub_wrapper_framing(&self, message: String, err_start: u32) -> String {
        let at_terminator = (err_start as usize) >= self.expr_end;
        if at_terminator && message.contains("expected `;`") {
            return "the input is incomplete — it ends before it is a whole Kio expression"
                .to_owned();
        }
        message
    }
}

fn invalid_single_expression() -> ReplExprQueryError {
    ExprQueryError::Syntax("expected exactly one Kio expression".to_owned()).into()
}

/// Choose an ordinary Kio identifier for the appended wrapper without
/// changing the meaning of either the current module or the query.
///
/// Leading-`__` identifiers are parser-reserved and therefore cannot name an
/// ordinary declaration in the textual source overlay. Instead, reserve every
/// identifier already present in the module *and* the query. Including
/// unresolved query spellings makes disjointness itself the capture-proof
/// invariant instead of relying on the current non-recursive self-visibility
/// rule. Elaborators cannot add an arbitrary caller-scoped spelling: their
/// checked-term constructors receive resolved captures, checked call slots,
/// and hygienic callback locals. Starting from the session's monotone counter
/// keeps the usual short names; wrapping makes the parse-only completion
/// probe's `u64::MAX` seed total as well.
fn fresh_wrapper_name(original: &str, expr_src: &str, start: u64) -> String {
    let mut occupied = HashSet::new();
    collect_identifiers(original, &mut occupied);
    collect_identifiers(expr_src, &mut occupied);

    let mut n = start;
    loop {
        let candidate = format!("_expr{n}");
        if !occupied.contains(&candidate) {
            return candidate;
        }
        n = n.wrapping_add(1);
        assert_ne!(
            n, start,
            "the finite source cannot occupy every synthetic wrapper identifier"
        );
    }
}

/// Add every lexer identifier in `source` to `occupied`.
///
/// Invalid prompt input can fail before the lexer returns its tokens. In that
/// case a conservative ASCII scan retains every identifier-shaped run,
/// including runs inside malformed framing. False positives only make the
/// private wrapper choose the next number; omitting a real identifier could
/// cause capture.
fn collect_identifiers(source: &str, occupied: &mut HashSet<String>) {
    match lex(source) {
        Ok(tokens) => {
            occupied.extend(tokens.into_iter().filter_map(|token| match token.kind {
                TokenKind::Ident(name) => Some(name),
                _ => None,
            }));
        }
        Err(_) => collect_identifier_runs(source, occupied),
    }
}

fn collect_identifier_runs(source: &str, occupied: &mut HashSet<String>) {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !(bytes[cursor].is_ascii_alphabetic() || bytes[cursor] == b'_') {
            cursor += 1;
            continue;
        }
        let start = cursor;
        cursor += 1;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        occupied.insert(source[start..cursor].to_owned());
    }
}

/// 1-based `(line, column)` of a byte offset within `source`.
fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let off = (offset as usize).min(source.len());
    let mut line = 1;
    let mut col = 1;
    for ch in source[..off].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Pipeline as _;

    fn checked_structural_fault_package() -> crate::pass::typecheck_full::CheckedLoweredPackage {
        let package_source = "\
package repl_totality;

bridge {
  main;
}
";
        let module_source = "\
module main;

import __comptime__;

pure fn run(ct: __Comptime__, k: . -> .) -> . {
  __structural_recur__(
    , ct
    , __Type__
    , .
    , .
    , __type_unit__(ct)
    , ()
    , .(_recur: (__Type__ & .) -> ., _fuel: __Type__, _input: .) -> . { k(()) }
    )
}

pure fn main(ct: __Comptime__) -> . {
  let ignored = run(ct, .(_outer: .) -> . {
    run(ct, .(_inner: .) -> . { () })
  });
  ()
}
";
        let surface_module = crate::pass::parser::parse(module_source).expect("module parses");
        let surface_package =
            crate::pass::parser::parse_package_file(package_source, None).expect("package parses");
        let (modules, package_file) = crate::pass::full::FullPipeline::lower_package(
            vec![(std::path::PathBuf::from("main.kio"), surface_module)],
            Some(surface_package),
        )
        .expect("package lowers");
        let package_file =
            package_file.map(|package_file| crate::pass::resolve::PackageFileEntry {
                file_path: std::path::PathBuf::from("repl_totality.pkg.kio"),
                package_name: "repl_totality".to_owned(),
                package_file,
            });
        let package =
            crate::pass::resolve::Package::build(std::path::Path::new(""), modules, package_file)
                .expect("package resolves");
        crate::pass::typecheck_full::check_package_preserving_normalization(&package)
            .expect("package type-checks")
    }

    #[test]
    fn repl_reduction_classifies_structural_fault_as_totality() {
        let ctx = crate::normalization::EvalCtx::new();
        let callback = Value::StructuralRecur {
            root_measure: 1,
            current_measure: 100,
            step: crate::normalization::EvalRef::new(Value::Atom("step-must-not-run".to_owned())),
        };
        let fault = crate::normalization::apply(
            callback,
            vec![Value::ReflTypeArity(49), Value::Unit],
            &ctx,
        );
        let error = checked_reduction_result(fault, &ctx)
            .expect_err("REPL normalization must not render a Totality fault as a residual");
        let ReplExprQueryError::Totality(message) = error else {
            panic!("structural fault lost its private Totality category");
        };
        assert!(
            message.contains("root=1, current=100, next=50"),
            "{message}"
        );
    }

    #[test]
    fn reduce_lowered_propagates_executed_structural_fault() {
        let checked = checked_structural_fault_package();
        let module = checked.package().module("main").expect("main module");
        let body = module
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name.as_str() == "main" => Some(&def.body),
                _ => None,
            })
            .expect("main definition");
        let error = reduce_lowered(body, &checked, "main")
            .expect_err("the actual REPL reduction seam must surface Totality");
        let ReplExprQueryError::Totality(message) = error else {
            panic!("structural fault lost its private Totality category");
        };
        assert!(message.contains("root=1, current=1, next=1"), "{message}");
    }

    #[test]
    fn classify_bare_identifier() {
        assert_eq!(
            classify_input("foo"),
            Some(InputKind::BareName("foo".to_owned()))
        );
    }

    #[test]
    fn classify_dotted_path() {
        assert_eq!(
            classify_input("X.a.b.foo"),
            Some(InputKind::BareName("X.a.b.foo".to_owned()))
        );
    }

    #[test]
    fn classify_mixed_fully_qualified_path() {
        // The genuine FQN form `module/path.item` mixes `/` and `.`. It
        // is a bare name (routes through name resolution / `split_fqn`),
        // not a compound expression.
        assert_eq!(
            classify_input("o/main.cond"),
            Some(InputKind::BareName("o/main.cond".to_owned()))
        );
        // A deeper module path with a trailing item.
        assert_eq!(
            classify_input("a/b/c/d.e"),
            Some(InputKind::BareName("a/b/c/d.e".to_owned()))
        );
    }

    #[test]
    fn classify_malformed_mixed_path_is_compound() {
        // A trailing `/` or a dotted item that is not a single
        // identifier is not a name — it falls through to compound.
        assert_eq!(
            classify_input("a/b/.foo"),
            Some(InputKind::Compound("a/b/.foo".to_owned()))
        );
        assert_eq!(
            classify_input("a/b.c.d"),
            Some(InputKind::Compound("a/b.c.d".to_owned()))
        );
    }

    #[test]
    fn classify_slash_module_path() {
        // A `/`-joined module path is a bare name (routes to `:doc`),
        // carrying its surface spelling.
        assert_eq!(
            classify_input("demo/main"),
            Some(InputKind::BareName("demo/main".to_owned()))
        );
    }

    #[test]
    fn classify_bare_operator() {
        assert_eq!(
            classify_input("+"),
            Some(InputKind::BareName("+".to_owned()))
        );
        assert_eq!(
            classify_input("<*>"),
            Some(InputKind::BareName("<*>".to_owned()))
        );
    }

    #[test]
    fn classify_parenthesized_operator() {
        assert_eq!(
            classify_input("(+)"),
            Some(InputKind::BareName("+".to_owned()))
        );
    }

    #[test]
    fn classify_numeric_literal_is_compound() {
        assert_eq!(
            classify_input("42"),
            Some(InputKind::Compound("42".to_owned()))
        );
    }

    #[test]
    fn classify_string_literal_is_compound() {
        assert_eq!(
            classify_input("\"hello\""),
            Some(InputKind::Compound("\"hello\"".to_owned()))
        );
    }

    #[test]
    fn classify_application_is_compound() {
        assert_eq!(
            classify_input("foo ()"),
            Some(InputKind::Compound("foo ()".to_owned()))
        );
    }

    #[test]
    fn classify_op_expression_is_compound() {
        assert_eq!(
            classify_input("1 + 2"),
            Some(InputKind::Compound("1 + 2".to_owned()))
        );
    }

    #[test]
    fn classify_unit_literal_is_compound() {
        // `()` is the unit value — a compound expression, not a name.
        assert_eq!(
            classify_input("()"),
            Some(InputKind::Compound("()".to_owned()))
        );
    }

    #[test]
    fn classify_trims_whitespace() {
        assert_eq!(
            classify_input("   foo   "),
            Some(InputKind::BareName("foo".to_owned()))
        );
    }

    #[test]
    fn classify_empty_is_none() {
        assert_eq!(classify_input(""), None);
        assert_eq!(classify_input("   "), None);
    }

    #[test]
    fn classify_bare_dot_is_not_a_name() {
        // A lone `.` is not a name; it falls through to compound (and
        // will fail when parsed as an expression).
        assert_eq!(
            classify_input("."),
            Some(InputKind::Compound(".".to_owned()))
        );
    }

    #[test]
    fn synthetic_wrap_places_expression_after_original() {
        let wrap = SyntheticWrap::build("module pkg/m;\n", "1 + 2", 0);
        assert_eq!(&wrap.source[wrap.expr_start..wrap.expr_end], "1 + 2");
        assert!(wrap.source.starts_with("module pkg/m;\n"));
        assert!(wrap.source.contains("fn _expr0()"));
        assert!(wrap.source.contains("let _ = 1 + 2\n;"));
    }

    #[test]
    fn synthetic_wrap_authenticates_one_expression_and_trailing_comment() {
        let original = "module pkg/m;\nfn existing() { () }\n";
        let wrap = SyntheticWrap::build(original, "() // trailing comment", 0);

        assert!(
            wrap.authenticate_parse_shape(1).is_ok(),
            "one expression plus trailing trivia must be authentic"
        );
    }

    #[test]
    fn synthetic_wrap_rejects_declaration_injection() {
        let original = "module pkg/m;\n";
        let injected = "(); () } fn injected() { ()";
        let wrap = SyntheticWrap::build_pure(original, injected, 0);

        assert!(
            matches!(
                wrap.authenticate_parse_shape(0),
                Err(ReplExprQueryError::Query(ExprQueryError::Syntax(_)))
            ),
            "query text must not escape the synthetic function"
        );
    }

    #[test]
    fn synthetic_wrap_avoids_existing_module_identifier() {
        let original = "module pkg/m;\npure fn _expr0(x: .) -> . { x }\n";
        let wrap = SyntheticWrap::build(original, "()", 0);

        assert_eq!(wrap.fn_name, "_expr1");
        assert!(wrap.source.contains("fn _expr1()"));
    }

    #[test]
    fn synthetic_wrap_does_not_capture_unresolved_query_identifier() {
        let wrap = SyntheticWrap::build("module pkg/m;\n", "(_expr0)", 0);

        assert_eq!(wrap.fn_name, "_expr1");
        assert_eq!(&wrap.source[wrap.expr_start..wrap.expr_end], "(_expr0)");
    }

    #[test]
    fn synthetic_wrap_reserves_identifiers_in_unlexable_query() {
        let wrap = SyntheticWrap::build("module pkg/m;\n", "(_expr0 \"", 0);

        assert_eq!(wrap.fn_name, "_expr1");
    }

    #[test]
    fn pure_synthetic_wrap_changes_only_the_function_modifier() {
        let original = "module pkg/m;\n";
        let ordinary = SyntheticWrap::build(original, "call(())", 2);
        let pure = SyntheticWrap::build_pure(original, "call(())", 2);

        assert!(pure.source.contains("pure fn _expr2()"));
        assert_eq!(&pure.source[pure.expr_start..pure.expr_end], "call(())");
        assert_eq!(pure.expr_start, ordinary.expr_start + "pure ".len());
        assert_eq!(pure.expr_end, ordinary.expr_end + "pure ".len());
        assert_eq!(
            pure.source.replacen("pure fn", "fn", 1),
            ordinary.source,
            "the purity probe must not alter the expression or its context"
        );
    }

    #[test]
    fn synthetic_wrap_expr_range_is_exact() {
        // The recorded byte range must bracket exactly the expression
        // text, so the position-index width search picks the right
        // node.
        let wrap = SyntheticWrap::build("module pkg/m;", "foo(bar)", 7);
        assert_eq!(wrap.expr_end - wrap.expr_start, "foo(bar)".len());
        assert_eq!(&wrap.source[wrap.expr_start..wrap.expr_end], "foo(bar)");
    }

    #[test]
    fn scrub_rephrases_leaked_synthetic_terminator() {
        // A parse error at the synthetic `;` terminator must never reach
        // the user as "expected `;`" — the `;` is the wrapper's, not the
        // user's. The error span is at (or past) `expr_end`.
        let wrap = SyntheticWrap::build("module pkg/m;", "1 +", 3);
        let scrubbed = wrap.scrub_wrapper_framing("expected `;`".to_owned(), wrap.expr_end as u32);
        assert!(
            !scrubbed.contains(';'),
            "the leaked terminator must be scrubbed, got: {scrubbed}"
        );
        assert!(scrubbed.contains("incomplete"), "got: {scrubbed}");
    }

    #[test]
    fn synthetic_wrap_counter_wraps_without_collision() {
        let original = format!("module pkg/m;\nfn _expr{}() {{ () }}", u64::MAX);
        let wrap = SyntheticWrap::build(&original, "()", u64::MAX);

        assert_eq!(wrap.fn_name, "_expr0");
    }

    #[test]
    fn line_col_counts_lines_and_columns() {
        let src = "ab\ncd\nef";
        assert_eq!(line_col(src, 0), (1, 1));
        assert_eq!(line_col(src, 3), (2, 1));
        assert_eq!(line_col(src, 7), (3, 2));
    }
}

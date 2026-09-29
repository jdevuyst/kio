//! Backend-agnostic optimizations over `Module<Enriched>`.
//!
//! Runs after [`crate::pass::structural_recovery`] has collapsed the
//! Kio'-shaped intrinsic chains into the enriched IR, and before
//! per-backend lowering in [`crate::backends`] consumes the package.
//! Every optimization here is observationally a no-op: same exit
//! code, same host-call ordering, same observable result. The wins
//! are tighter, smaller emit and (transitively) faster JIT / linker
//! work downstream.
//!
//! ## Pass shape
//!
//! [`optimize_package`] threads a peephole-rule sweep over every
//! expression-bearing position in a [`Package<Enriched>`] (module
//! `fn` bodies and exported fn bodies). The walk is
//! **bottom-up**: children are rewritten before
//! parents, so a parent's rule sees already-reduced children. The
//! catalog runs as a **fixpoint** — passes apply in order, then the
//! catalog re-applies until no pass reports a change. Convergence is
//! cheap (most modules need one or two iterations).
//!
//! ## Catalog (in order)
//!
//! - [`project_after_tuple`] — `Project(Tuple([…, e_i, …]), i)` →
//!   `e_i`. When the non-`i`th items are not statically pure, they
//!   are kept in evaluation order via a [`Expr::Seq`] chain so
//!   host-call ordering is preserved.
//! - [`compose_recovered_tail_access`] — a projection or field-get
//!   over the tail tuple materialized for `__snd__` composes back
//!   into a direct projection of the original product. This keeps
//!   wide label-field reads and updates from rebuilding every suffix
//!   product before selecting a slot.
//! - [`inject_then_match`] — `Match(Inject(p, idx, _), arms)` →
//!   `let arms[idx].param = p in arms[idx].body`. The dispatch is
//!   decidable at compile time; the let form preserves the payload-
//!   binding shape so a later [`let_fuse`] sweep can inline it when
//!   the param is used at most once.
//! - [`absurd_propagate`] — `Let|Seq { value: __absurd__(…),
//!   body: _ }` → `__absurd__(…)`. The body is dead — `!` is
//!   uninhabited and the call diverges (throws at runtime), so the
//!   continuation never runs. Drops the body subtree entirely.
//! - [`match_fusion`] — case-of-case: `Match(Match(s, inner_arms),
//!   outer_arms)` →
//!   `Match(s, [arm_i: Match(inner_arms[i].body, outer_arms)])`.
//!   Pushes the outer dispatch into each inner arm. Composes with
//!   [`inject_then_match`]: when an inner arm body is a static
//!   injection, the pushed-down outer match reduces to a single
//!   arm. Conservative on size: only fires when the duplicated
//!   outer arms would not blow up the IR — small outer match (≤2
//!   arms) or small arm bodies (≤8 IR nodes each).
//! - [`let_fuse`] — `let x = E in F[x]` where `x` occurs at most
//!   once in `F`: inline E at the use site. Single-use is the
//!   conservative threshold (multi-use would duplicate E's
//!   computation). Zero-use is also handled — the let degrades to
//!   `E; F` (or just `F` if E is pure; the DCE pass takes care of
//!   the latter).
//! - [`beta_reduce_unary_iife`] — `.(x: T) { body }(arg)` →
//!   `let x = arg; body` for non-polymorphic unary function literals.
//!   This removes closure-call ladders that surface Kio' elaboration
//!   can otherwise leave in backend output.
//! - [`dead_code_elimination`] — drops pure values whose result is
//!   discarded. `Seq { value: pure, body }` → `body`. The zero-use
//!   pure-let case is already covered by [`let_fuse`]; this rule
//!   completes coverage by also catching expression-statement
//!   positions.
//! - [`newtype_identity_elision`] — collapses `Foo.un_foo(Foo.mk_foo(x))`
//!   (and the symmetric `Foo.mk_foo(Foo.un_foo(v))`) to `x` /
//!   `v`. Per Kio' semantics, every newtype's constructor and
//!   projector are observable inverses (`un_foo(mk_foo(x)) = x` for
//!   all `x`), so wrap-then-unwrap is identity at the IR level and
//!   is backend-agnostic — neither JS nor Rust needs the round-trip.
//!   The single-call form (`Foo.mk_foo(x)` alone) is preserved: a
//!   backend that keeps a nominal wrapper still needs the
//!   constructor call to coerce the payload to the wrapper.
//! - [`constant_fold`] — compile-time-known conditionals and
//!   degenerate matches collapse: `Conditional(BoolLit(b), t, e)`
//!   → `t` (when `b = true`) or `e` (when `b = false`); a `Match`
//!   on a literal that the IR can statically dispatch on (covered
//!   by [`inject_then_match`] when the literal is an injection, by
//!   this rule when the literal is a host bool / int constant
//!   reaching a one-arm match). Reduces structural overhead in
//!   compiler-self-hosting code where lots of dispatch is on
//!   compile-time constants (op tables, intrinsic registries,
//!   etc.).
//!
//! ## Purity
//!
//! Several rules in the catalog gate on a syntactic purity check —
//! "this expression has no host calls and no observable side effects."
//! The check is intentionally **conservative**: literals, unit, path
//! references, and lambda literals are pure; calls (other than to known
//! pure forms), `match!`, `if`/`else`, `let`/`seq` chains involving
//! sub-expressions of unknown purity, and every Enriched node whose
//! sub-tree could harbor a host call are not.
//!
//! Refinements arrive with the per-fn pure-effect sub-pass that
//! lands alongside DCE. Until then, conservatism is the right side
//! to err on — a pass that
//! refuses to fire on a syntactically-impure expression is correct;
//! a pass that fires when it shouldn't is a behavior bug.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::ast::{CallArg, Enriched, Expr, FnDef, Item, Module, PackageFile, RecordField, Type};
use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry};
#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Maximum number of catalog iterations before the fixpoint loop
/// gives up and emits whatever it has. In practice most modules
/// converge in 1–2 iterations; this cap is a safety net against an
/// optimizer bug that loops indefinitely on a pathological input. A
/// hit signals a bug (two rules that ping-pong rewrites at each
/// other) and is loud in tests; in release it surfaces as missed
/// optimization rather than a compile-time hang.
const FIXPOINT_CAP: usize = 32;

/// Per-newtype info used by [`newtype_identity_elision`].
#[derive(Debug, Clone)]
struct NewtypeMemberInfo {
    identity: crate::pass::resolve::ResolvedNewtypeIdentity,
    constructor: String,
    projector: String,
}

/// Each context is scoped to one consumer module. A lexical head therefore
/// denotes only the exact local, selectively imported, or qualified-imported
/// newtype selected in that module.
#[derive(Debug, Clone, Default)]
pub struct OptimizerCtx {
    newtype_members: crate::pass::resolve::ResolvedNewtypeHeadMap<NewtypeMemberInfo>,
}

impl OptimizerCtx {
    fn for_module(package: &Package<Enriched>, module: &Module<Enriched>) -> Self {
        let mut ctx = Self::default();
        crate::pass::resolve::for_each_resolved_newtype_member_head(
            package,
            module,
            |visible_head, owner, declaration| {
                ctx.newtype_members.insert(
                    visible_head,
                    NewtypeMemberInfo {
                        identity: crate::pass::resolve::ResolvedNewtypeIdentity::new(
                            owner,
                            declaration,
                        ),
                        constructor: declaration.constructor.name.clone(),
                        projector: declaration.projector.name.clone(),
                    },
                );
            },
        );
        ctx
    }

    fn collect_newtypes_from_export(&mut self, _e: &PackageFile<Enriched>) {
        // Export `type` entries in the package file are type
        // aliases, not newtypes — no constructor / projector
        // members to elide. Newtype elision only fires for items
        // declared via `newtype` in module bodies.
    }
}

/// Optimize a whole enriched package: every module body, every
/// exported fn body, and every bridge adaptation
/// body pass through every catalog pass to a fixpoint.
///
/// **Parallelism.** Exact per-module [`OptimizerCtx`] values are built before
/// the package is consumed, then read independently by the module fan-out.
/// The `BTreeMap` collect preserves input order.
pub fn optimize_package(package: Package<Enriched>) -> Package<Enriched> {
    let contexts = package
        .modules()
        .map(|(path, entry)| {
            (
                (*path).to_owned(),
                OptimizerCtx::for_module(&package, &entry.module),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let (modules_map, package_file) = package.into_parts();
    let modules: std::collections::BTreeMap<_, _> = crate::maybe_into_par_iter!(modules_map)
        .map(|(path, entry)| {
            let ctx = contexts
                .get(&path)
                .expect("every package module has an optimizer context");
            (path, optimize_module_entry(ctx, entry))
        })
        .collect();
    let package_file =
        package_file.map(|e| optimize_package_file_entry(&OptimizerCtx::default(), e));
    Package::<Enriched>::from_parts(modules, package_file)
}

fn optimize_module_entry(
    ctx: &OptimizerCtx,
    entry: ModuleEntry<Enriched>,
) -> ModuleEntry<Enriched> {
    ModuleEntry::<Enriched> {
        file_path: entry.file_path,
        module: optimize_module_in_ctx(ctx, entry.module),
        scope: entry.scope,
    }
}

fn optimize_package_file_entry(
    ctx: &OptimizerCtx,
    entry: PackageFileEntry<Enriched>,
) -> PackageFileEntry<Enriched> {
    PackageFileEntry::<Enriched> {
        file_path: entry.file_path,
        package_name: entry.package_name,
        package_file: optimize_package_file_in_ctx(ctx, entry.package_file),
    }
}

/// Run the catalog to a fixpoint on a single module using its entries from a
/// pre-computed package catalogue. Mirrors what [`optimize_package`] does
/// internally per module. Used by the
/// [`crate::cache::enriched`] miss path so the per-module
/// optimization is equivalent whether the cache is enabled or not.
///
/// `cli`-gated: it takes the cache layer's `PackageKeyInputs` and is
/// only ever called from `cache::enriched`, so it compiles only when the
/// `cache` module is in the build.
#[cfg(feature = "cli")]
pub fn optimize_module_with_package_inputs(
    m: Module<Enriched>,
    inputs: &crate::cache::enriched::PackageKeyInputs,
) -> Module<Enriched> {
    let mut ctx = OptimizerCtx::default();
    let module_path = m.path.segments.join("/");
    for (scoped_name, input) in &inputs.newtype_members {
        if scoped_name.module_path() != module_path {
            continue;
        }
        ctx.newtype_members.insert(
            scoped_name.visible_head(),
            NewtypeMemberInfo {
                identity: crate::pass::resolve::ResolvedNewtypeIdentity::from_parts(
                    input.identity().module_path(),
                    input.identity().declaration_name(),
                ),
                constructor: input.constructor().as_str().to_owned(),
                projector: input.projector().as_str().to_owned(),
            },
        );
    }
    optimize_module_in_ctx(&ctx, m)
}

fn optimize_module_in_ctx(ctx: &OptimizerCtx, mut m: Module<Enriched>) -> Module<Enriched> {
    m.items = m
        .items
        .into_iter()
        .map(|it| optimize_item(ctx, it))
        .collect();
    m
}

/// Run the catalog to a fixpoint on a single package file without a
/// pre-computed [`OptimizerCtx`] — builds one from the package file's
/// own exported type entries.
pub fn optimize_package_file(e: PackageFile<Enriched>) -> PackageFile<Enriched> {
    let mut ctx = OptimizerCtx::default();
    ctx.collect_newtypes_from_export(&e);
    optimize_package_file_in_ctx(&ctx, e)
}

fn optimize_package_file_in_ctx(
    _ctx: &OptimizerCtx,
    e: PackageFile<Enriched>,
) -> PackageFile<Enriched> {
    // The package file carries only the phase-independent `bridge` glob
    // list — no item bodies to optimize.
    e
}

fn optimize_item(ctx: &OptimizerCtx, item: Item<Enriched>) -> Item<Enriched> {
    match item {
        Item::FnDef(d) => Item::FnDef(optimize_fn_def(ctx, d)),
        // No expression body to walk; pass through.
        Item::TypeAlias(a) => Item::TypeAlias(a),
        Item::Newtype(d) => Item::Newtype(d),
        Item::TypeRecGroup(group) => Item::TypeRecGroup(group),
        Item::HostType(h) => Item::HostType(h),
        Item::HostFn(h) => Item::HostFn(h),
        Item::LiteralAlias(_, ext)
        | Item::Labels(_, ext)
        | Item::LabelForward(_, ext)
        | Item::Equiv(_, ext)
        | Item::Elaborator(_, ext)
        | Item::Op(_, ext)
        | Item::VariadicOperator(_, ext)
        | Item::RecGroup(_, ext) => match ext {},
    }
}

fn optimize_fn_def(ctx: &OptimizerCtx, mut d: FnDef<Enriched>) -> FnDef<Enriched> {
    d.body = optimize_expr(ctx, d.body);
    d
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogMode {
    Full,
    NoMatchFusion,
}

/// Walk one expression. Bottom-up: every child is optimized before
/// the parent's peephole rules fire. The fixpoint loop sits at the
/// top of this function — each rule walks the new tree until no rule
/// reports a change.
fn optimize_expr(ctx: &OptimizerCtx, e: Expr<Enriched>) -> Expr<Enriched> {
    optimize_expr_in_mode(ctx, e, CatalogMode::Full)
}

fn optimize_expr_in_mode(
    ctx: &OptimizerCtx,
    e: Expr<Enriched>,
    mode: CatalogMode,
) -> Expr<Enriched> {
    let mut e = recurse(ctx, e, mode);
    for _ in 0..FIXPOINT_CAP {
        let mut changed = false;
        e = project_after_tuple(e, &mut changed);
        e = compose_recovered_tail_access(e, &mut changed);
        e = let_bound_tuple_projection(e, &mut changed);
        e = inject_then_match(e, &mut changed);
        e = absurd_propagate(e, &mut changed);
        if mode == CatalogMode::Full {
            e = match_fusion(e, &mut changed);
        }
        e = let_fuse(e, &mut changed);
        e = beta_reduce_unary_iife(e, &mut changed);
        e = dead_code_elimination(e, &mut changed);
        e = newtype_identity_elision(ctx, e, &mut changed);
        e = constant_fold(e, &mut changed);
        if !changed {
            return e;
        }
        // A parent rewrite can expose redexes below the root. Revisit
        // those children without the case-arm-duplicating
        // `match_fusion`, so recursive cleanup cannot trigger nested
        // case-tree fanout beyond this expression's cap.
        e = recurse(ctx, e, CatalogMode::NoMatchFusion);
    }
    e
}

/// Descend through children first, applying [`optimize_expr`] to
/// each. The parent shape comes back unchanged — only its children
/// are now in their optimized form. Peephole rules at the parent
/// then see fully-reduced children.
fn recurse(ctx: &OptimizerCtx, e: Expr<Enriched>, mode: CatalogMode) -> Expr<Enriched> {
    match e {
        // Pass-through Kio' shape nodes — only their child
        // expressions get optimized.
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(optimize_expr_in_mode(ctx, *callee, mode)),
            args: args
                .into_iter()
                .map(|a| optimize_call_arg(ctx, a, mode))
                .collect(),
            meta,
            ext: (),
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty,
            body: Box::new(optimize_expr_in_mode(ctx, *body, mode)),
            meta,
            caps,
        },
        Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty,
            pattern,
            value: Box::new(optimize_expr_in_mode(ctx, *value, mode)),
            body: Box::new(optimize_expr_in_mode(ctx, *body, mode)),
            meta,
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(optimize_expr_in_mode(ctx, *value, mode)),
            body: Box::new(optimize_expr_in_mode(ctx, *body, mode)),
            meta,
        },
        // Enriched variants: walk every sub-expression.
        Expr::EnrichedTuple {
            occurrence: _,
            items,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: items
                .into_iter()
                .map(|i| optimize_expr_in_mode(ctx, i, mode))
                .collect(),
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedProject {
            occurrence: _,
            target,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(optimize_expr_in_mode(ctx, *target, mode)),
            index,
            arity,
            target_ty,
            meta,
            ext,
        },
        Expr::EnrichedInject {
            occurrence: _,
            payload,
            variant,
            variants,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedInject {
            occurrence: Default::default(),
            payload: Box::new(optimize_expr_in_mode(ctx, *payload, mode)),
            variant,
            variants,
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedMatch {
            occurrence: _,
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            meta,
            ext,
        } => Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(optimize_expr_in_mode(ctx, *scrutinee, mode)),
            arms: arms
                .into_iter()
                .map(|mut a| {
                    a.body = optimize_expr_in_mode(ctx, a.body, mode);
                    a
                })
                .collect(),
            scrutinee_ty,
            result_ty,
            meta,
            ext,
        },
        Expr::EnrichedConditional {
            occurrence: _,
            cond,
            then_branch,
            else_branch,
            result_ty,
            meta,
            ext,
        } => Expr::EnrichedConditional {
            occurrence: Default::default(),
            cond: Box::new(optimize_expr_in_mode(ctx, *cond, mode)),
            then_branch: Box::new(optimize_expr_in_mode(ctx, *then_branch, mode)),
            else_branch: Box::new(optimize_expr_in_mode(ctx, *else_branch, mode)),
            result_ty,
            meta,
            ext,
        },
        Expr::EnrichedRecord {
            occurrence: _,
            fields,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedRecord {
            occurrence: Default::default(),
            fields: fields
                .into_iter()
                .map(|f| RecordField {
                    name: f.name,
                    value: optimize_expr_in_mode(ctx, f.value, mode),
                    meta: f.meta,
                })
                .collect(),
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedFieldGet {
            occurrence: _,
            target,
            field_name,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => Expr::EnrichedFieldGet {
            occurrence: Default::default(),
            target: Box::new(optimize_expr_in_mode(ctx, *target, mode)),
            field_name,
            index,
            arity,
            target_ty,
            meta,
            ext,
        },
        // Leaves.
        e @ (Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. }) => e,
        // Surface-only / pre-recovery variants are uninhabited at
        // `Enriched`.
        Expr::Tuple { ext, .. } => match ext {},
        Expr::FnPlaceholder { ext, .. } => match ext {},
        Expr::LabelValue { ext, .. } => match ext {},
        Expr::RowLet { ext, .. } => match ext {},
        Expr::Elaborator { ext, .. } => match ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match ext {},
        Expr::Ufcs { ext, .. } => match ext {},
        Expr::OpChain { ext, .. } => match ext {},
    }
}

fn optimize_call_arg(
    ctx: &OptimizerCtx,
    a: CallArg<Enriched>,
    mode: CatalogMode,
) -> CallArg<Enriched> {
    match a {
        CallArg::Type(t) => CallArg::Type(t),
        CallArg::Value(v) => CallArg::Value(optimize_expr_in_mode(ctx, v, mode)),
    }
}

// ---- syntactic purity check -------------------------------------

/// Conservative syntactic purity check over an enriched expression.
///
/// Returns `true` when the expression provably has no observable
/// side effects: it can be dropped, duplicated, or reordered with
/// any other pure expression without changing observable behavior.
///
/// **What's pure:** literals (unit, string, int, float, bool), path
/// references (variables and qualified path references — the
/// referenced item itself may not be pure, but reading a name from
/// the environment is), lambda literals (closure creation is pure;
/// invoking the closure is *not*), pure `let`/`seq` chains, pure
/// tuples / records / projections / field-gets / injections, and
/// matches / conditionals whose scrutinee / arms / branches are
/// pure.
///
/// **What's not pure (conservatively):** every `Call` (any host
/// call, but also any package-boundary call whose body might do I/O — we
/// don't yet track per-fn purity), every sub-expression containing
/// a Call. This is the conservative cut; the DCE pass's per-fn
/// pure-effect sub-pass will refine it later.
fn is_pure(e: &Expr<Enriched>) -> bool {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => true,
        // A `fn` *literal* is a closure-construction site; that's
        // pure (no host call fires until the closure is invoked).
        // The body may contain calls; those run only when the
        // closure is invoked, which is the caller's purity, not the
        // construction's.
        Expr::FnExpr { .. } => true,
        // Every Call is conservatively impure until per-fn analysis
        // lands.
        Expr::Call { .. } => false,
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            is_pure(value) && is_pure(body)
        }
        Expr::EnrichedTuple { items, .. } => items.iter().all(is_pure),
        Expr::EnrichedRecord { fields, .. } => fields.iter().all(|f| is_pure(&f.value)),
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            is_pure(target)
        }
        Expr::EnrichedInject { payload, .. } => is_pure(payload),
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => is_pure(scrutinee) && arms.iter().all(|a| is_pure(&a.body)),
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => is_pure(cond) && is_pure(then_branch) && is_pure(else_branch),
        // Surface / pre-recovery variants are uninhabited at
        // `Enriched`.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Low-IR variants are uninhabited at `Enriched`.
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

/// Wrap `kept` in a chain of [`Expr::Seq`] nodes that evaluate every
/// expression in `effects` first, in order. Used by rules that drop
/// some operands but must preserve their evaluation for side effects
/// (host-call ordering). Pure expressions in `effects` are filtered
/// out — they have no observable behavior and can be elided.
fn sequence_effects(effects: Vec<Expr<Enriched>>, kept: Expr<Enriched>) -> Expr<Enriched> {
    let kept_meta = kept.meta().clone();
    effects
        .into_iter()
        .filter(|e| !is_pure(e))
        .rfold(kept, |acc, e| Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(e),
            body: Box::new(acc),
            meta: kept_meta.clone(),
        })
}

// ---- pass: Project-after-Tuple ----------------------------------

/// `Project(Tuple([…, e_i, …]), i)` → `e_i`.
///
/// **Pattern**: an [`Expr::EnrichedProject`] whose `target` is itself
/// an [`Expr::EnrichedTuple`] of matching `arity`, with
/// `index < arity`. **Rewrite**: replace the whole projection with
/// the i-th component of the tuple's `items`. The projection is
/// decidable at compile time — no allocation, no field access, just
/// the value flowing through.
///
/// Non-`i`th items that are not syntactically pure are kept in
/// evaluation order via a leading [`Expr::Seq`] chain. The rule
/// preserves host-call ordering by construction: any side-effecting
/// component still runs at its original position in the tuple's
/// left-to-right evaluation order.
///
/// Do not apply this rewrite to [`Expr::EnrichedFieldGet`]. A
/// field-get is not only positional selection: it represents a label
/// newtype accessor folded over that selection. Replacing it with
/// the tuple slot would return the wrapped label value and drop the
/// accessor.
///
/// Conservative on shape mismatches: if the projection's `arity` and
/// the tuple's `items.len()` disagree (which would be a typer / IR-
/// shape bug), the pass leaves the node alone so the bug surfaces
/// later rather than masking it with a silent reduction.
fn project_after_tuple(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::EnrichedProject {
            occurrence: _,
            target,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => match *target {
            Expr::EnrichedTuple { items, .. } if items.len() == arity && index < arity => {
                *changed = true;
                pick_with_effects(items, index)
            }
            Expr::EnrichedRecord { fields, .. } if fields.len() == arity && index < arity => {
                *changed = true;
                pick_with_effects(fields.into_iter().map(|f| f.value).collect(), index)
            }
            other => Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(other),
                index,
                arity,
                target_ty,
                meta,
                ext,
            },
        },
        e @ Expr::EnrichedFieldGet { .. } => e,
        e => e,
    }
}

/// Pick component `index` out of `items`, preserving the
/// side-effecting components in evaluation order via a leading
/// [`Expr::Seq`] chain. Used by Project-after-Tuple to drop the
/// non-picked slots while keeping their effects live.
fn pick_with_effects(mut items: Vec<Expr<Enriched>>, index: usize) -> Expr<Enriched> {
    let picked = items.remove(index);
    sequence_effects(items, picked)
}

// ---- pass: Compose Recovered Tail Access ------------------------

/// `Project(let r = p; Tuple([Project(r, 1), ...]), 0)` →
/// `Project(p, 1)`, and likewise for [`Expr::EnrichedFieldGet`].
///
/// `structural_recovery` represents `__snd__` of a product tail as a
/// fresh tuple of direct projections from the original product. That
/// is the right standalone value for `__snd__`, but a later
/// `__fst__` / label projector over that tail should not build the
/// suffix product only to select one slot from it. This rule follows
/// the recovered tail tuple chain and composes the access into the
/// original product slot in one sweep, so field `n` of a wide product
/// does not require `n` optimizer fixpoint iterations.
fn compose_recovered_tail_access(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::EnrichedProject {
            occurrence: _,
            target,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => {
            let (target, index, arity, target_ty, did_compose) =
                compose_tail_target(*target, index, arity, target_ty);
            if did_compose {
                *changed = true;
            }
            Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(target),
                index,
                arity,
                target_ty,
                meta,
                ext,
            }
        }
        Expr::EnrichedFieldGet {
            occurrence: _,
            target,
            field_name,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => {
            let (target, index, arity, target_ty, did_compose) =
                compose_tail_target(*target, index, arity, target_ty);
            if did_compose {
                *changed = true;
            }
            Expr::EnrichedFieldGet {
                occurrence: Default::default(),
                target: Box::new(target),
                field_name,
                index,
                arity,
                target_ty,
                meta,
                ext,
            }
        }
        e => e,
    }
}

fn compose_tail_target(
    mut target: Expr<Enriched>,
    mut index: usize,
    mut arity: usize,
    mut target_ty: Type<Enriched>,
) -> (Expr<Enriched>, usize, usize, Type<Enriched>, bool) {
    let mut changed = false;
    while let Some((next_index, next_arity, next_target_ty)) =
        recovered_tail_slot_projection(&target, index, arity)
    {
        let Expr::Let { value, .. } = target else {
            unreachable!("recovered_tail_slot_projection only accepts Let targets")
        };
        target = *value;
        index = next_index;
        arity = next_arity;
        target_ty = next_target_ty;
        changed = true;
    }
    (target, index, arity, target_ty, changed)
}

fn recovered_tail_slot_projection(
    target: &Expr<Enriched>,
    index: usize,
    arity: usize,
) -> Option<(usize, usize, Type<Enriched>)> {
    let Expr::Let { name, body, .. } = target else {
        return None;
    };
    let Expr::EnrichedTuple { items, .. } = body.as_ref() else {
        return None;
    };
    if items.len() != arity || index >= items.len() {
        return None;
    }
    let source_arity = arity.checked_add(1)?;
    let mut selected = None;
    for (position, item) in items.iter().enumerate() {
        let (slot_index, slot_arity, slot_target_ty) = projection_from_bound(item, name)?;
        if slot_index != position + 1 || slot_arity != source_arity {
            return None;
        }
        if position == index {
            selected = Some((slot_index, slot_arity, slot_target_ty));
        }
    }
    selected
}

fn projection_from_bound(
    item: &Expr<Enriched>,
    name: &str,
) -> Option<(usize, usize, Type<Enriched>)> {
    let Expr::EnrichedProject {
        target,
        index,
        arity,
        target_ty,
        ..
    } = item
    else {
        return None;
    };
    let Expr::Path { segments, .. } = target.as_ref() else {
        return None;
    };
    if segments.len() != 1 || segments[0].name != name {
        return None;
    }
    Some((*index, *arity, target_ty.clone()))
}

fn let_bound_tuple_projection(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let Expr::Let {
        occurrence: _,
        name,
        name_span,
        ty,
        pattern,
        value,
        body,
        meta,
    } = e
    else {
        return e;
    };
    let value_expr = *value;
    let body_expr = *body;
    if let (
        Expr::EnrichedTuple {
            items: bound_items, ..
        },
        Expr::Call { callee, args, .. },
    ) = (&value_expr, &body_expr)
    {
        let projects_once_in_order = bound_items.iter().all(is_pure)
            && count_free_uses(callee, &name) == 0
            && call_projects_bound_tuple_once_in_order(args, &name, bound_items.len());
        if projects_once_in_order {
            let Expr::EnrichedTuple {
                items: bound_items, ..
            } = value_expr
            else {
                unreachable!("let-bound call projection guarded above")
            };
            let Expr::Call {
                occurrence: _,
                callee,
                args,
                meta,
                ext,
            } = body_expr
            else {
                unreachable!("let-bound call projection guarded above")
            };
            let mut bound_items = bound_items.into_iter();
            let args = args
                .into_iter()
                .map(|arg| match arg {
                    CallArg::Type(ty) => CallArg::Type(ty),
                    CallArg::Value(_) => CallArg::Value(
                        bound_items
                            .next()
                            .expect("guard requires one tuple item per value argument"),
                    ),
                })
                .collect();
            debug_assert!(bound_items.next().is_none());
            *changed = true;
            return Expr::Call {
                occurrence: Default::default(),
                callee,
                args,
                meta,
                ext,
            };
        }
    }
    let projected_indices = match (&value_expr, &body_expr) {
        (
            Expr::EnrichedTuple {
                items: bound_items, ..
            },
            Expr::EnrichedTuple { items, .. },
        ) if bound_items.iter().all(is_pure) => {
            let mut indices = Vec::with_capacity(items.len());
            for item in items {
                let Expr::EnrichedProject {
                    target,
                    index,
                    arity,
                    ..
                } = item
                else {
                    return Expr::Let {
                        occurrence: Default::default(),
                        name,
                        name_span,
                        ty,
                        pattern,
                        value: Box::new(value_expr),
                        body: Box::new(body_expr),
                        meta,
                    };
                };
                let is_bound_target = matches!(
                    &**target,
                    Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].name == name
                );
                if !is_bound_target || *arity != bound_items.len() || *index >= bound_items.len() {
                    return Expr::Let {
                        occurrence: Default::default(),
                        name,
                        name_span,
                        ty,
                        pattern,
                        value: Box::new(value_expr),
                        body: Box::new(body_expr),
                        meta,
                    };
                }
                indices.push(*index);
            }
            indices
        }
        _ => {
            return Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value: Box::new(value_expr),
                body: Box::new(body_expr),
                meta,
            };
        }
    };
    let Expr::EnrichedTuple {
        items: bound_items, ..
    } = value_expr
    else {
        unreachable!("let-bound tuple projection guarded above")
    };
    let Expr::EnrichedTuple {
        synth_ty,
        meta,
        ext,
        ..
    } = body_expr
    else {
        unreachable!("let-bound tuple projection guarded above")
    };
    *changed = true;
    Expr::EnrichedTuple {
        occurrence: Default::default(),
        items: projected_indices
            .into_iter()
            .map(|index| bound_items[index].clone())
            .collect(),
        synth_ty,
        meta,
        ext,
    }
}

fn call_projects_bound_tuple_once_in_order(
    args: &[CallArg<Enriched>],
    name: &str,
    tuple_arity: usize,
) -> bool {
    let mut value_index = 0;
    for arg in args {
        let CallArg::Value(value) = arg else {
            continue;
        };
        let Some((index, arity, _)) = projection_from_bound(value, name) else {
            return false;
        };
        if index != value_index || arity != tuple_arity {
            return false;
        }
        value_index += 1;
    }
    value_index == tuple_arity
}

// ---- pass: Inject-then-Match ------------------------------------

/// `Match(Inject(payload, idx, _), arms)` → `let arms[idx].param =
/// payload in arms[idx].body`.
///
/// **Pattern**: an [`Expr::EnrichedMatch`] whose `scrutinee` is a
/// statically-known [`Expr::EnrichedInject`] — a variant injection
/// with index `idx`. **Rewrite**: replace the whole match with the
/// `idx`-th arm's body, binding the arm's `param` to the injection
/// payload via [`Expr::Let`]. Reaches the right arm at compile time
/// — no runtime dispatch.
///
/// The let form (rather than direct substitution) keeps two
/// guarantees:
///
/// - **Payload evaluation order**: the let RHS runs once, before
///   the arm body, exactly where the original `Match(Inject(p, …))`
///   would have evaluated `p`. Even when `param` is unused in the
///   body, the let's RHS evaluation preserves any host-call effects
///   in `p`.
/// - **Capture-free**: introducing a fresh binding avoids any need
///   to alpha-rename inside the arm body, which a direct
///   substitution would require if `payload` captured a name that
///   shadowed something in `arm.body`'s context.
///
/// The later [`let_fuse`] sweep collapses the let when `param`
/// occurs at most once and `payload` is duplicable (single-use or
/// pure).
///
/// Conservative on shape mismatches: when `variant >= arms.len()`
/// the rewrite is skipped — that combination would be a typer / IR-
/// shape bug; surfacing it later is preferable to silently picking
/// the wrong arm. The typer's `EnrichedMatch::scrutinee_ty` /
/// `EnrichedInject::synth_ty` invariant guarantees arity match on
/// well-typed inputs.
fn inject_then_match(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::EnrichedMatch {
            occurrence: _,
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            meta,
            ext,
        } => match *scrutinee {
            Expr::EnrichedInject {
                occurrence: _,
                payload,
                variant,
                variants: _,
                synth_ty: _,
                meta: inject_meta,
                ext: _,
            } if variant < arms.len() => {
                *changed = true;
                let arm = arms
                    .into_iter()
                    .nth(variant)
                    .expect("checked variant < arms");
                Expr::Let {
                    occurrence: Default::default(),
                    name: arm.param,
                    name_span: arm.meta.span,
                    ty: None,
                    pattern: (),
                    value: payload,
                    body: Box::new(arm.body),
                    meta: inject_meta,
                }
            }
            scrut => Expr::EnrichedMatch {
                occurrence: Default::default(),
                scrutinee: Box::new(scrut),
                arms,
                scrutinee_ty,
                result_ty,
                meta,
                ext,
            },
        },
        e => e,
    }
}

// ---- pass: __absurd__ propagation -------------------------------

/// Drop the continuation past a `!`-typed expression.
///
/// **Pattern**: a [`Expr::Let`] or [`Expr::Seq`] whose `value` is a
/// call to `__absurd__` — the Kio' intrinsic that consumes a value
/// of bottom and produces any type. Per Kio' semantics, a value of
/// type `!` is uninhabited in well-typed programs; the call either
/// never executes (the host fn declared `-> !` never returned, so
/// the path is unreachable) or it throws at runtime (the
/// belt-and-braces JS lowering — see [`crate::backends::js::emit`]'s
/// `emit_absurd_call`). Either way the continuation cannot run.
///
/// **Rewrite**: drop the `body` subtree; the value becomes the whole
/// expression. The call itself stays — it carries the host-call /
/// throw effect that the type-system contract demands.
///
/// This is a small but useful rule: cleaner control-flow downstream
/// (no dangling `let _ = throw(); rest` shapes in emitted code) and
/// it composes with DCE to clean up any bindings the dropped body
/// might have introduced.
fn absurd_propagate(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::Let {
            name: _,
            value,
            body: _,
            meta: _,
            ..
        }
        | Expr::Seq {
            occurrence: _,
            value,
            body: _,
            meta: _,
        } if is_absurd_call(&value) => {
            *changed = true;
            *value
        }
        e => e,
    }
}

/// True iff `e` is a syntactic `__absurd__(…)` call: an
/// [`Expr::Call`] whose callee is the bare path `__absurd__`. Used
/// by [`absurd_propagate`] to detect a diverging RHS.
fn is_absurd_call(e: &Expr<Enriched>) -> bool {
    match e {
        Expr::Call { callee, .. } => match &**callee {
            Expr::Path { segments, .. } => segments.len() == 1 && segments[0].name == "__absurd__",
            _ => false,
        },
        _ => false,
    }
}

// ---- pass: Match fusion -----------------------------------------

/// Maximum total IR-node count for the *outer* match's arm bodies
/// when considering case-of-case fusion. Above this, the
/// duplication cost of pushing the outer match into every inner
/// arm outweighs the gain; the rule declines.
///
/// 32 is a deliberately small ceiling: case-of-case shines on tight
/// dispatch chains (a sum-of-sums where each level resolves cleanly
/// after fusion); duplicating a large arm body N times bloats the
/// emit. Adjust against profiling when the catalog's measure-and-
/// tune loop lands.
const MATCH_FUSION_OUTER_BUDGET: usize = 32;

/// Match fusion (case-of-case): push an outer match into each arm of
/// an inner match whose result is the outer scrutinee.
///
/// **Pattern**: an [`Expr::EnrichedMatch`] whose `scrutinee` is
/// itself an [`Expr::EnrichedMatch`].
/// **Rewrite**: distribute the outer match across the inner match's
/// arms. Each inner-arm body becomes the body of a new outer match
/// (one per inner arm), and the result is a single match on the
/// original innermost scrutinee.
///
/// Wins (the spec's intended payoff): the inner match disappears as
/// a separate dispatch — what was two switches becomes one. When an
/// inner-arm body is a static [`Expr::EnrichedInject`], the pushed-
/// down outer match reduces to a single arm via
/// [`inject_then_match`], collapsing the dispatch entirely. The
/// duplicate outer arms shake out as the same code in every branch
/// gets folded by downstream rules.
///
/// Cost: the outer arms duplicate N times (one per inner arm). To
/// stop emit bloat the rule declines when the outer arms' total IR
/// size exceeds [`MATCH_FUSION_OUTER_BUDGET`] — small outer matches
/// are the sweet spot.
fn match_fusion(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let Expr::EnrichedMatch {
        occurrence: _,
        scrutinee,
        arms: outer_arms,
        scrutinee_ty: outer_scrutinee_ty,
        result_ty,
        meta: outer_meta,
        ext: outer_ext,
    } = e
    else {
        return e;
    };
    let Expr::EnrichedMatch {
        occurrence: _,
        scrutinee: inner_scrut,
        arms: inner_arms,
        scrutinee_ty: inner_scrutinee_ty,
        result_ty: _,
        meta: inner_meta,
        ext: _,
    } = *scrutinee
    else {
        // Restore the unmodified outer match.
        return Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee,
            arms: outer_arms,
            scrutinee_ty: outer_scrutinee_ty,
            result_ty,
            meta: outer_meta,
            ext: outer_ext,
        };
    };

    // Budget check: pushing the outer match into every inner arm
    // duplicates the outer arms N times. Decline if that would blow
    // the IR up.
    let outer_size: usize = outer_arms.iter().map(|a| ir_node_count(&a.body)).sum();
    if outer_size > MATCH_FUSION_OUTER_BUDGET {
        // Restore.
        return Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(Expr::EnrichedMatch {
                occurrence: Default::default(),
                scrutinee: inner_scrut,
                arms: inner_arms,
                scrutinee_ty: inner_scrutinee_ty,
                result_ty: result_ty.clone(),
                meta: inner_meta,
                ext: (),
            }),
            arms: outer_arms,
            scrutinee_ty: outer_scrutinee_ty,
            result_ty,
            meta: outer_meta,
            ext: outer_ext,
        };
    }

    // Capture-avoidance. Each inner arm binds `inner_arm.param` over
    // the wrapped outer match. If an outer arm body references a free
    // variable whose name equals some inner arm's param, cloning the
    // outer arms under that binder would silently re-bind that
    // reference to the inner payload — a wrong runtime value. Mirror
    // `let_fuse`'s conservative bail: if any inner arm param appears
    // free in any outer arm body, leave the nested match unmodified.
    let outer_free: std::collections::HashSet<String> =
        outer_arms.iter().flat_map(|a| free_vars(&a.body)).collect();
    if inner_arms
        .iter()
        .any(|a| outer_free.contains(a.param.as_str()))
    {
        return Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(Expr::EnrichedMatch {
                occurrence: Default::default(),
                scrutinee: inner_scrut,
                arms: inner_arms,
                scrutinee_ty: inner_scrutinee_ty,
                result_ty: result_ty.clone(),
                meta: inner_meta,
                ext: (),
            }),
            arms: outer_arms,
            scrutinee_ty: outer_scrutinee_ty,
            result_ty,
            meta: outer_meta,
            ext: outer_ext,
        };
    }

    *changed = true;
    // For each inner arm, wrap its body in a fresh copy of the outer
    // match.
    let new_arms = inner_arms
        .into_iter()
        .map(|inner_arm| {
            let wrapped = Expr::EnrichedMatch {
                occurrence: Default::default(),
                scrutinee: Box::new(inner_arm.body),
                arms: outer_arms.clone(),
                scrutinee_ty: outer_scrutinee_ty.clone(),
                result_ty: result_ty.clone(),
                meta: outer_meta.clone(),
                ext: outer_ext,
            };
            crate::ast::EnrichedArm {
                param: inner_arm.param,
                body: wrapped,
                meta: inner_arm.meta,
            }
        })
        .collect();

    Expr::EnrichedMatch {
        occurrence: Default::default(),
        scrutinee: inner_scrut,
        arms: new_arms,
        scrutinee_ty: inner_scrutinee_ty,
        result_ty,
        meta: outer_meta,
        ext: outer_ext,
    }
}

/// Count occurrences of a free `name` (a bare single-segment path)
/// in `e`. Used by [`let_fuse`] to decide whether inlining is safe
/// — at most one use is OK to inline (no duplication of `E`'s
/// computation); zero uses lets the let degrade.
///
/// **Scoping**: respects shadowing. When a nested binder
/// (`Expr::Let { name: target, .. }`, an `Expr::FnExpr` whose
/// signature binds `target`, an `Expr::EnrichedMatch` arm whose
/// `param` is `target`) introduces a fresh `target`, occurrences
/// of `target` inside that binder's scope are bindings of the
/// inner, not free references to the outer. The count skips them.
///
/// Returns early at threshold 2: callers only care about 0 vs. 1
/// vs. ≥2, so once two are seen, the walk stops without descending
/// further.
fn count_free_uses(e: &Expr<Enriched>, name: &str) -> usize {
    let mut count = 0;
    walk_count_free(e, name, &mut count);
    count
}

fn walk_count_free(e: &Expr<Enriched>, name: &str, count: &mut usize) {
    if *count >= 2 {
        return;
    }
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            // A bare single-segment path with `name` is a free
            // occurrence. Longer paths are member accesses or
            // qualified imports — they reach different keyspaces
            // and can't match a local binding.
            if segments.len() == 1 && segments[0].name == name {
                *count += 1;
            }
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Call { callee, args, .. } => {
            walk_count_free(callee, name, count);
            for a in args {
                if let CallArg::Value(v) = a {
                    walk_count_free(v, name, count);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            // A fn binds its value-parameters in `body`. If `name`
            // is one of them, the body's occurrences of `name` are
            // bindings of the inner, not free references — skip.
            if sig
                .params
                .iter()
                .any(|p| matches!(p, crate::ast::SignatureParam::Value(v) if v.name == name))
            {
                return;
            }
            walk_count_free(body, name, count);
        }
        Expr::Let {
            name: bind,
            value,
            body,
            ..
        } => {
            walk_count_free(value, name, count);
            // `let bind = …; body` — uses of `name` inside `body`
            // are shadowed when `bind == name`.
            if bind != name {
                walk_count_free(body, name, count);
            }
        }
        Expr::Seq { value, body, .. } => {
            walk_count_free(value, name, count);
            walk_count_free(body, name, count);
        }
        Expr::EnrichedTuple { items, .. } => {
            for i in items {
                walk_count_free(i, name, count);
                if *count >= 2 {
                    return;
                }
            }
        }
        Expr::EnrichedRecord { fields, .. } => {
            for f in fields {
                walk_count_free(&f.value, name, count);
                if *count >= 2 {
                    return;
                }
            }
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            walk_count_free(target, name, count);
        }
        Expr::EnrichedInject { payload, .. } => {
            walk_count_free(payload, name, count);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            walk_count_free(scrutinee, name, count);
            for a in arms {
                if a.param == name {
                    // Arm shadows `name` — body's uses don't count.
                    continue;
                }
                walk_count_free(&a.body, name, count);
                if *count >= 2 {
                    return;
                }
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_count_free(cond, name, count);
            walk_count_free(then_branch, name, count);
            walk_count_free(else_branch, name, count);
        }
        // Pre-Enriched variants are uninhabited.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Low-IR variants are uninhabited at `Enriched`.
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

/// Substitute every free occurrence of `name` in `e` with a fresh
/// clone of `replacement`. Used by [`let_fuse`] to inline a let-
/// bound value at its (sole) use site.
///
/// **Scoping**: respects shadowing (same rules as
/// [`count_free_uses`]). Inner binders that shadow `name` leave
/// their sub-tree untouched.
///
/// Capture-avoidance is **not** performed: the caller ensures the
/// `replacement` doesn't accidentally capture inner binders'
/// scopes. In practice the let-fuse rule is called with a
/// `replacement` whose free variables are already in scope at the
/// inlining site (the let RHS came from the same outer scope), so
/// no fresh names need to be introduced.
fn subst_free(e: Expr<Enriched>, name: &str, replacement: &Expr<Enriched>) -> Expr<Enriched> {
    match e {
        Expr::Path {
            occurrence: _,
            segments,
            meta,
            ext: _,
        } => {
            if segments.len() == 1 && segments[0].name == name {
                replacement.clone()
            } else {
                Expr::Path {
                    occurrence: Default::default(),
                    segments,
                    meta,
                    ext: (),
                }
            }
        }
        e @ (Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. }) => e,
        Expr::Call {
            occurrence: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(subst_free(*callee, name, replacement)),
            args: args
                .into_iter()
                .map(|a| match a {
                    CallArg::Value(v) => CallArg::Value(subst_free(v, name, replacement)),
                    CallArg::Type(t) => CallArg::Type(t),
                })
                .collect(),
            meta,
            ext: (),
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => {
            // Param shadowing: if `name` is a param, the body's
            // uses are bindings of the inner — skip the descent.
            let shadows = sig
                .params
                .iter()
                .any(|p| matches!(p, crate::ast::SignatureParam::Value(v) if v.name == name));
            let body = if shadows {
                body
            } else {
                Box::new(subst_free(*body, name, replacement))
            };
            Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty,
                body,
                meta,
                caps,
            }
        }
        Expr::Let {
            occurrence: _,
            name: bind,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        } => {
            let value = Box::new(subst_free(*value, name, replacement));
            let body = if bind == name {
                body
            } else {
                Box::new(subst_free(*body, name, replacement))
            };
            Expr::Let {
                occurrence: Default::default(),
                name: bind,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta,
            }
        }
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(subst_free(*value, name, replacement)),
            body: Box::new(subst_free(*body, name, replacement)),
            meta,
        },
        Expr::EnrichedTuple {
            occurrence: _,
            items,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: items
                .into_iter()
                .map(|i| subst_free(i, name, replacement))
                .collect(),
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedProject {
            occurrence: _,
            target,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(subst_free(*target, name, replacement)),
            index,
            arity,
            target_ty,
            meta,
            ext,
        },
        Expr::EnrichedInject {
            occurrence: _,
            payload,
            variant,
            variants,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedInject {
            occurrence: Default::default(),
            payload: Box::new(subst_free(*payload, name, replacement)),
            variant,
            variants,
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedMatch {
            occurrence: _,
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            meta,
            ext,
        } => Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(subst_free(*scrutinee, name, replacement)),
            arms: arms
                .into_iter()
                .map(|a| {
                    let body = if a.param == name {
                        a.body
                    } else {
                        subst_free(a.body, name, replacement)
                    };
                    crate::ast::EnrichedArm {
                        param: a.param,
                        body,
                        meta: a.meta,
                    }
                })
                .collect(),
            scrutinee_ty,
            result_ty,
            meta,
            ext,
        },
        Expr::EnrichedConditional {
            occurrence: _,
            cond,
            then_branch,
            else_branch,
            result_ty,
            meta,
            ext,
        } => Expr::EnrichedConditional {
            occurrence: Default::default(),
            cond: Box::new(subst_free(*cond, name, replacement)),
            then_branch: Box::new(subst_free(*then_branch, name, replacement)),
            else_branch: Box::new(subst_free(*else_branch, name, replacement)),
            result_ty,
            meta,
            ext,
        },
        Expr::EnrichedRecord {
            occurrence: _,
            fields,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedRecord {
            occurrence: Default::default(),
            fields: fields
                .into_iter()
                .map(|f| RecordField {
                    name: f.name,
                    value: subst_free(f.value, name, replacement),
                    meta: f.meta,
                })
                .collect(),
            synth_ty,
            meta,
            ext,
        },
        Expr::EnrichedFieldGet {
            occurrence: _,
            target,
            field_name,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => Expr::EnrichedFieldGet {
            occurrence: Default::default(),
            target: Box::new(subst_free(*target, name, replacement)),
            field_name,
            index,
            arity,
            target_ty,
            meta,
            ext,
        },
        // Pre-Enriched variants are uninhabited.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::Ufcs { ext, .. } => match ext {},
        Expr::OpChain { ext, .. } => match ext {},
    }
}

// ---- pass: Let-immediate-use fusion -----------------------------

/// `let x = E in F[x]` → inline E at the use site when `x` appears
/// at most once in `F`.
///
/// **Pattern**: an [`Expr::Let`] whose binding `x` is used at most
/// once in the body. **Rewrite**:
///
/// - **Zero uses**: drop the binder. If `E` is pure, the let
///   collapses to just `F` (the DCE pass also catches this, but
///   doing it here lets later rules see `F` directly). If `E`
///   is impure, the let collapses to `E; F` (a [`Expr::Seq`])
///   to preserve the side-effect.
/// - **One use**: substitute `E` for `x` in `F`. The single use site
///   receives `E` directly; the let disappears. Host-call ordering
///   is preserved because the use site is the one place in `F`
///   where evaluation would have hit the binding, and `E` was
///   evaluated immediately before `F` in the original semantics.
///
/// **Caveat — evaluation order with impure E**: when E is impure
/// and `x` is used exactly once, naive substitution can in principle
/// reorder E's effects with earlier sub-expressions of F that get
/// evaluated *before* the use site. For an enriched-IR Expr<Enriched>
/// in let-position, this only matters when F has earlier-evaluating
/// sub-expressions (a wider scrutinee, an earlier tuple slot, etc.)
/// that themselves contain side effects. The rule is conservative
/// here too: when E is impure, only substitute when every expression
/// evaluated before the use site is pure. That preserves E's place
/// relative to every observable effect while allowing it to move past
/// literals, paths, projections, and other proven-pure expressions.
///
/// The conservative fallback when E is impure and the use follows an
/// earlier effect or lies in a deferred branch: leave the let alone.
/// The pass declines rather than risk reordering side-effects.
fn let_fuse(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let Expr::Let {
        occurrence: _,
        name,
        name_span,
        ty,
        pattern,
        value,
        body,
        meta,
    } = e
    else {
        return e;
    };
    let n = count_free_uses(&body, &name);
    match n {
        0 => {
            *changed = true;
            if is_pure(&value) {
                // Pure — drop the let entirely.
                *body
            } else {
                // Impure — keep E's effect via a Seq.
                Expr::Seq {
                    occurrence: Default::default(),
                    value,
                    body,
                    meta,
                }
            }
        }
        1 => {
            // Inlining is safe only if `value`'s free variables
            // aren't shadowed at the (single) use site inside
            // `body`. The conservative check: collect every binder
            // name introduced in `body` (let / fn / match arm) and
            // bail when any of them appears free in `value`. A
            // single-binder shadow is harmless when the use site
            // sits *outside* that binder, but tracking
            // per-use-site scope is more work than just bailing.
            let v_free = free_vars(&value);
            let b_binders = body_binders(&body);
            let would_capture = v_free.iter().any(|n| b_binders.contains(n.as_str()));
            if !would_capture && (is_pure(&value) || use_precedes_effects(&body, &name)) {
                *changed = true;
                subst_free(*body, &name, &value)
            } else {
                // Single-use but unsafe (would capture, or cross an
                // earlier effect): bail to keep correct semantics.
                Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty,
                    pattern,
                    value,
                    body,
                    meta,
                }
            }
        }
        _ => {
            // Multi-use: don't duplicate E's computation.
            Expr::Let {
                occurrence: Default::default(),
                name,
                name_span,
                ty,
                pattern,
                value,
                body,
                meta,
            }
        }
    }
}

/// `.(x: T) { body }(arg)` → `let x = arg; body`.
///
/// The function literal is pure, so the only observable work before
/// entering the body is evaluating the single value argument. A `let`
/// preserves that order and gives the rest of the catalog a chance to
/// inline or drop the binding. The rule stays deliberately narrow:
/// polymorphic and multi-value-parameter function literals are left
/// alone until the optimizer has type-argument substitution and
/// simultaneous argument-binding machinery.
fn beta_reduce_unary_iife(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let Expr::Call {
        occurrence: _,
        callee,
        args,
        meta,
        ext,
    } = e
    else {
        return e;
    };

    let safe_unary_fn = match &*callee {
        Expr::FnExpr { sig, .. } => {
            let mut params = sig.params.iter();
            matches!(params.next(), Some(crate::ast::SignatureParam::Value(_)))
                && params.next().is_none()
        }
        _ => false,
    };
    let safe_unary_arg = matches!(args.as_slice(), [CallArg::Value(_)]);
    if !safe_unary_fn || !safe_unary_arg {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext,
        };
    }

    let Expr::FnExpr { sig, body, .. } = *callee else {
        unreachable!("safe_unary_fn only holds for Expr::FnExpr")
    };
    let mut params = sig.params.into_iter();
    let crate::ast::SignatureParam::Value(param) = params
        .next()
        .expect("safe_unary_fn requires one value parameter")
    else {
        unreachable!("safe_unary_fn rejects non-value parameters")
    };
    let mut args_iter = args.into_iter();
    let CallArg::Value(value) = args_iter
        .next()
        .expect("safe_unary_arg requires one value argument")
    else {
        unreachable!("safe_unary_arg rejects non-value arguments")
    };

    *changed = true;
    Expr::Let {
        occurrence: Default::default(),
        name: param.name,
        name_span: param.meta.span,
        ty: None,
        pattern: (),
        value: Box::new(value),
        body,
        meta,
    }
}

/// Collect the set of free variables in `e` — bare single-segment
/// paths that aren't bound by a surrounding `Let` / `FnExpr` /
/// match-arm inside `e`. Used by [`let_fuse`]'s capture-avoidance
/// check.
fn free_vars(e: &Expr<Enriched>) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut bound: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_free_vars(e, &mut bound, &mut out);
    out
}

fn collect_free_vars(
    e: &Expr<Enriched>,
    bound: &mut std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => {
            if segments.len() == 1 && !bound.contains(&segments[0].name) {
                out.insert(segments[0].name.clone());
            }
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Call { callee, args, .. } => {
            collect_free_vars(callee, bound, out);
            for a in args {
                if let CallArg::Value(v) = a {
                    collect_free_vars(v, bound, out);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            let added: Vec<String> = sig
                .params
                .iter()
                .filter_map(|p| match p {
                    crate::ast::SignatureParam::Value(v) => Some(v.name.clone()),
                    _ => None,
                })
                .filter(|n| !bound.contains(n))
                .collect();
            for n in &added {
                bound.insert(n.clone());
            }
            collect_free_vars(body, bound, out);
            for n in &added {
                bound.remove(n);
            }
        }
        Expr::Let {
            name, value, body, ..
        } => {
            collect_free_vars(value, bound, out);
            let inserted = bound.insert(name.clone());
            collect_free_vars(body, bound, out);
            if inserted {
                bound.remove(name);
            }
        }
        Expr::Seq { value, body, .. } => {
            collect_free_vars(value, bound, out);
            collect_free_vars(body, bound, out);
        }
        Expr::EnrichedTuple { items, .. } => {
            for i in items {
                collect_free_vars(i, bound, out);
            }
        }
        Expr::EnrichedRecord { fields, .. } => {
            for f in fields {
                collect_free_vars(&f.value, bound, out);
            }
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            collect_free_vars(target, bound, out);
        }
        Expr::EnrichedInject { payload, .. } => {
            collect_free_vars(payload, bound, out);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            collect_free_vars(scrutinee, bound, out);
            for a in arms {
                let inserted = bound.insert(a.param.clone());
                collect_free_vars(&a.body, bound, out);
                if inserted {
                    bound.remove(&a.param);
                }
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_free_vars(cond, bound, out);
            collect_free_vars(then_branch, bound, out);
            collect_free_vars(else_branch, bound, out);
        }
        // Pre-Enriched variants are uninhabited.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Low-IR variants are uninhabited at `Enriched`.
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

/// Collect the set of all binder names introduced anywhere in `e`
/// — let-bindings, fn-param names, match-arm params. Used by
/// [`let_fuse`]'s capture-avoidance check: if any of `value`'s free
/// variables matches a binder name in `body`, substituting `value`
/// into `body` at the use site might shadow the free reference.
/// The bail-out is conservative: it covers the dangerous case even
/// when the use site isn't actually inside the offending binder's
/// scope.
fn body_binders(e: &Expr<Enriched>) -> std::collections::HashSet<&str> {
    let mut out = std::collections::HashSet::new();
    collect_binders(e, &mut out);
    out
}

fn collect_binders<'a>(e: &'a Expr<Enriched>, out: &mut std::collections::HashSet<&'a str>) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Call { callee, args, .. } => {
            collect_binders(callee, out);
            for a in args {
                if let CallArg::Value(v) = a {
                    collect_binders(v, out);
                }
            }
        }
        Expr::FnExpr { sig, body, .. } => {
            for p in &sig.params {
                if let crate::ast::SignatureParam::Value(v) = p {
                    out.insert(&v.name);
                }
            }
            collect_binders(body, out);
        }
        Expr::Let {
            name, value, body, ..
        } => {
            out.insert(name);
            collect_binders(value, out);
            collect_binders(body, out);
        }
        Expr::Seq { value, body, .. } => {
            collect_binders(value, out);
            collect_binders(body, out);
        }
        Expr::EnrichedTuple { items, .. } => {
            for i in items {
                collect_binders(i, out);
            }
        }
        Expr::EnrichedRecord { fields, .. } => {
            for f in fields {
                collect_binders(&f.value, out);
            }
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            collect_binders(target, out);
        }
        Expr::EnrichedInject { payload, .. } => {
            collect_binders(payload, out);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            collect_binders(scrutinee, out);
            for a in arms {
                out.insert(&a.param);
                collect_binders(&a.body, out);
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_binders(cond, out);
            collect_binders(then_branch, out);
            collect_binders(else_branch, out);
        }
        // Pre-Enriched variants are uninhabited.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Low-IR variants are uninhabited at `Enriched`.
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

// ---- pass: Dead code elimination --------------------------------

/// Drop pure values whose result is discarded.
///
/// **Pattern**: an [`Expr::Seq`] whose `value` is statically pure
/// — a literal, a path reference, a pure subtree. Per spec the
/// statement's value is `()` and is discarded, so a pure value in
/// that position is dead weight.
/// **Rewrite**: drop the `value`; the whole expression becomes the
/// `body`.
///
/// The let-zero-use case is already handled by [`let_fuse`]; this
/// rule covers the [`Expr::Seq`] expression-statement form. Once
/// per-fn purity analysis lands, more calls become provably pure
/// and this rule (plus the let-fuse zero-use case) fires more
/// often — the catalog stays the same, the purity verdict gets
/// sharper.
fn dead_code_elimination(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => {
            if is_pure(&value) {
                *changed = true;
                *body
            } else {
                Expr::Seq {
                    occurrence: Default::default(),
                    value,
                    body,
                    meta,
                }
            }
        }
        e => e,
    }
}

// ---- pass: Newtype identity elision -----------------------------

/// Collapse a wrap-then-unwrap (or unwrap-then-wrap) newtype call
/// pair to the inner value.
///
/// **Pattern**:
/// - `Foo.un_foo(Foo.mk_foo(x))` → `x`
/// - `Foo.mk_foo(Foo.un_foo(v))` → `v`
///
/// Per Kio' semantics, every newtype's constructor and projector
/// are observable inverses: `un_foo(mk_foo(x)) = x` for all `x`, and
/// `mk_foo(un_foo(v)) = v` for all `v : Foo<...>`. The
/// round-trip is identity at the IR level. Backend-agnostic — neither
/// JS (where the wrap/unwrap is runtime-erased) nor Rust (where the
/// wrap is a real struct) needs the round-trip; collapsing it
/// removes a strict pair of operations.
///
/// **Single-call form is preserved.** A bare `Foo.mk_foo(x)` (no
/// surrounding unwrap) stays put: the Rust backend needs the
/// constructor call to coerce `x` to the nominal `Foo` wrapper; the
/// JS backend doesn't, but its emit pass produces an identity
/// function-call shape anyway. Both backends handle a lone
/// constructor / projector call correctly — we'd lose backend
/// flexibility if we eagerly elided.
///
/// **Restriction on type-args**: the round-trip elision fires only
/// when the outer and inner type-args match. A
/// `Foo.un_foo[A](Foo.mk_foo[B](x))` cannot be collapsed when
/// `A != B` because they're calls into different parametric
/// instantiations.
fn newtype_identity_elision(
    ctx: &OptimizerCtx,
    e: Expr<Enriched>,
    changed: &mut bool,
) -> Expr<Enriched> {
    let Expr::Call {
        occurrence: _,
        callee,
        args,
        meta,
        ext: _,
    } = e
    else {
        return e;
    };
    let Some(outer) = match_newtype_member_call(&callee, &args) else {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    let Some((outer_identity, outer_kind, outer_type_args, outer_payload)) =
        ctx.newtype_members.get(outer.visible_head).map(|info| {
            let kind = if outer.member == info.constructor {
                NewtypeMemberKind::Constructor
            } else if outer.member == info.projector {
                NewtypeMemberKind::Projector
            } else {
                NewtypeMemberKind::Neither
            };
            (
                &info.identity,
                kind,
                outer.type_args.to_vec(),
                outer.payload,
            )
        })
    else {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    if matches!(outer_kind, NewtypeMemberKind::Neither) {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    }
    // The outer is a recognized constructor or projector call;
    // inspect the payload for a matching inverse call on the same
    // newtype.
    let inner = match outer_payload {
        Expr::Call {
            callee: c, args: a, ..
        } => match_newtype_member_call(c, a),
        _ => None,
    };
    let Some(inner) = inner else {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    let Some(inner_info) = ctx.newtype_members.get(inner.visible_head) else {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    if &inner_info.identity != outer_identity {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    }
    let inner_kind = if inner.member == inner_info.constructor {
        NewtypeMemberKind::Constructor
    } else if inner.member == inner_info.projector {
        NewtypeMemberKind::Projector
    } else {
        NewtypeMemberKind::Neither
    };
    // Inverse pair check: outer Projector + inner Constructor, or
    // outer Constructor + inner Projector.
    let is_inverse_pair = matches!(
        (outer_kind, inner_kind),
        (NewtypeMemberKind::Projector, NewtypeMemberKind::Constructor)
            | (NewtypeMemberKind::Constructor, NewtypeMemberKind::Projector)
    );
    if !is_inverse_pair {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    }
    // Both call sites must agree on type-args. Same number of type
    // args, AST-equal at each slot.
    if outer_type_args.len() != inner.type_args.len() {
        return Expr::Call {
            occurrence: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    }
    for (a, b) in outer_type_args.iter().zip(inner.type_args.iter()) {
        if !types_equal(a, b) {
            return Expr::Call {
                occurrence: Default::default(),
                callee,
                args,
                meta,
                ext: (),
            };
        }
    }
    *changed = true;
    inner.payload.clone()
}

/// Discriminator for a newtype member call's role: the constructor,
/// the projector, or neither. Used by [`newtype_identity_elision`]
/// to verify an inverse pair.
#[derive(Debug, Clone, Copy)]
enum NewtypeMemberKind {
    Constructor,
    Projector,
    Neither,
}

/// One newtype-member call view: the call's callee path naming the
/// newtype and a member, plus the type-args and the single value
/// payload. Returned from [`match_newtype_member_call`].
#[derive(Debug)]
struct NewtypeMemberCall<'a> {
    visible_head: &'a [crate::ast::PathSegment],
    member: &'a str,
    type_args: &'a [CallArg<Enriched>],
    payload: &'a Expr<Enriched>,
}

/// Match `e` against `<NT>.<member>([T1, …,] value)` or
/// `<alias>.<NT>.<member>([T1, …,] value)` — a local/selective or written
/// qualified member head plus a tail value argument optionally preceded by
/// type arguments. The caller resolves the borrowed head through its exact
/// per-module context.
///
/// Returns view-fields by borrow: the caller is responsible for
/// extending lifetimes through the consuming move.
fn match_newtype_member_call<'a>(
    callee: &'a Expr<Enriched>,
    args: &'a [CallArg<Enriched>],
) -> Option<NewtypeMemberCall<'a>> {
    let Expr::Path { segments, .. } = callee else {
        return None;
    };
    if !(2..=3).contains(&segments.len()) {
        return None;
    }
    let (member, visible_head) = segments.split_last()?;
    // Split args into leading type-args and the trailing value
    // payload. A newtype-member call has at most one value arg.
    let mut payload = None;
    let mut type_args_end = 0;
    for (i, a) in args.iter().enumerate() {
        match a {
            CallArg::Type(_) => {
                type_args_end = i + 1;
            }
            CallArg::Value(v) => {
                if payload.is_some() {
                    return None;
                }
                payload = Some(v);
            }
        }
    }
    let type_args = &args[..type_args_end];
    let payload = payload?;
    // The type-args must precede the value arg (strict order — no
    // interleaving). Mixed shapes are valid Kio but unusual; we
    // accept the simpler "all type-args then one value-arg" form.
    if !args[type_args_end..]
        .iter()
        .all(|a| matches!(a, CallArg::Value(_)))
    {
        return None;
    }
    Some(NewtypeMemberCall {
        visible_head,
        member: member.as_str(),
        type_args,
        payload,
    })
}

/// Compare two `CallArg`s structurally — used to confirm a wrap-
/// then-unwrap pair share the same type-args. `CallArg::Type` slots
/// compare by structural type equality; `CallArg::Value` slots
/// shouldn't ever appear in the type-arg head, but compare as
/// trivially unequal if they do (a defensive check).
fn types_equal(a: &CallArg<Enriched>, b: &CallArg<Enriched>) -> bool {
    match (a, b) {
        (CallArg::Type(t1), CallArg::Type(t2)) => structural_type_equal(t1, t2),
        _ => false,
    }
}

// ---- pass: Constant folding -------------------------------------

/// Compile-time evaluation of conditionals and degenerate matches
/// whose scrutinee is a statically-known constant.
///
/// **Patterns covered**:
/// - `EnrichedConditional(BoolLit(true), t, e)` → `t`. Pure
///   `then`-branch selection; the `else` branch is dead.
/// - `EnrichedConditional(BoolLit(false), t, e)` → `e`. Symmetric.
///
/// **Out-of-scope** (would require deeper analysis): arithmetic on
/// integer literals (host-typed), string concatenation, and
/// pure-fn-applied-to-literals folding via cross-fn inlining. These
/// would require running the `normalization` partial-evaluator over
/// enriched IR; today's evaluator is Lowered-only, so the deeper
/// folding is queued behind that port.
fn constant_fold(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    match e {
        Expr::EnrichedConditional {
            occurrence: _,
            cond,
            then_branch,
            else_branch,
            result_ty,
            meta,
            ext,
        } => match *cond {
            Expr::BoolLit { value: true, .. } => {
                *changed = true;
                *then_branch
            }
            Expr::BoolLit { value: false, .. } => {
                *changed = true;
                *else_branch
            }
            cond => Expr::EnrichedConditional {
                occurrence: Default::default(),
                cond: Box::new(cond),
                then_branch,
                else_branch,
                result_ty,
                meta,
                ext,
            },
        },
        Expr::EnrichedMatch { arms, .. } if arms.len() == 1 => {
            // Kio' has no unary sum: an `EnrichedMatch` is only minted
            // by `structural_recovery`'s `recover_either` /
            // `recover_dynamic_right`, both of which always emit ≥2
            // arms (a sum elimination dispatches over a `Type::Sum`,
            // whose spine count is ≥2). No optimize pass reduces arm
            // count. A 1-arm match here would be an earlier-phase
            // contract violation, not a foldable input.
            unreachable!(
                "constant_fold: 1-arm EnrichedMatch — structural_recovery mints ≥2 sum arms"
            )
        }
        e => e,
    }
}

/// Structural equality on `Type<Enriched>`. Compares construction
/// shape and segment names without consulting any resolve scope —
/// suffices for the elision rule, which only needs a syntactic
/// "these two types are the same" check.
fn structural_type_equal(a: &Type<Enriched>, b: &Type<Enriched>) -> bool {
    match (a, b) {
        (
            Type::Path {
                segments: s1,
                args: a1,
                ..
            },
            Type::Path {
                segments: s2,
                args: a2,
                ..
            },
        ) => {
            if s1.len() != s2.len() {
                return false;
            }
            if !s1.iter().zip(s2.iter()).all(|(x, y)| x.name == y.name) {
                return false;
            }
            if a1.len() != a2.len() {
                return false;
            }
            a1.iter()
                .zip(a2.iter())
                .all(|(x, y)| structural_type_equal(x, y))
        }
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: p1, ret: r1, ..
            },
            Type::Function {
                param: p2, ret: r2, ..
            },
        ) => structural_type_equal(p1, p2) && structural_type_equal(r1, r2),
        (
            Type::Product {
                left: l1,
                right: r1,
                ..
            },
            Type::Product {
                left: l2,
                right: r2,
                ..
            },
        )
        | (
            Type::Sum {
                left: l1,
                right: r1,
                ..
            },
            Type::Sum {
                left: l2,
                right: r2,
                ..
            },
        ) => structural_type_equal(l1, l2) && structural_type_equal(r1, r2),
        (
            Type::Forall {
                param: p1,
                body: b1,
                ..
            },
            Type::Forall {
                param: p2,
                body: b2,
                ..
            },
        ) => p1.name == p2.name && structural_type_equal(b1, b2),
        _ => false,
    }
}

/// True when left-to-right evaluation reaches the single free use of
/// `name` before any impure expression. [`let_fuse`] uses this to
/// preserve effect order while substituting an impure binding. Match
/// arms, conditional branches, and function bodies are excluded
/// because they are not unconditionally evaluated.
fn use_precedes_effects(body: &Expr<Enriched>, name: &str) -> bool {
    matches!(
        search_use_before_effects(body, name),
        UseBeforeEffects::Found
    )
}

#[derive(Clone, Copy)]
enum UseBeforeEffects {
    Clear,
    Found,
    Blocked,
}

fn search_use_before_effects(expression: &Expr<Enriched>, name: &str) -> UseBeforeEffects {
    match expression {
        Expr::Path { segments, .. } => {
            if segments.len() == 1 && segments[0].name == name {
                UseBeforeEffects::Found
            } else {
                UseBeforeEffects::Clear
            }
        }
        Expr::Call { callee, args, .. } => {
            let found = search_use_before_effects_in_order(
                std::iter::once(callee.as_ref()).chain(args.iter().filter_map(|arg| match arg {
                    CallArg::Value(value) => Some(value),
                    CallArg::Type(_) => None,
                })),
                name,
            );
            match found {
                UseBeforeEffects::Clear => UseBeforeEffects::Blocked,
                other => other,
            }
        }
        Expr::Let {
            name: binder,
            value,
            body,
            ..
        } => match search_use_before_effects(value, name) {
            UseBeforeEffects::Clear if binder == name => {
                if is_pure(body) {
                    UseBeforeEffects::Clear
                } else {
                    UseBeforeEffects::Blocked
                }
            }
            UseBeforeEffects::Clear => search_use_before_effects(body, name),
            other => other,
        },
        Expr::Seq { value, body, .. } => {
            search_use_before_effects_in_order([value.as_ref(), body.as_ref()], name)
        }
        Expr::EnrichedTuple { items, .. } => search_use_before_effects_in_order(items.iter(), name),
        Expr::EnrichedRecord { fields, .. } => {
            search_use_before_effects_in_order(fields.iter().map(|field| &field.value), name)
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            search_use_before_effects(target, name)
        }
        Expr::EnrichedInject { payload, .. } => search_use_before_effects(payload, name),
        Expr::EnrichedMatch { scrutinee, .. }
        | Expr::EnrichedConditional {
            cond: scrutinee, ..
        } => match search_use_before_effects(scrutinee, name) {
            UseBeforeEffects::Clear => {
                if count_free_uses(expression, name) == 0 && is_pure(expression) {
                    UseBeforeEffects::Clear
                } else {
                    UseBeforeEffects::Blocked
                }
            }
            other => other,
        },
        Expr::FnExpr { .. } => {
            if count_free_uses(expression, name) == 0 {
                UseBeforeEffects::Clear
            } else {
                UseBeforeEffects::Blocked
            }
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => UseBeforeEffects::Clear,
        _ => UseBeforeEffects::Blocked,
    }
}

fn search_use_before_effects_in_order<'a>(
    expressions: impl IntoIterator<Item = &'a Expr<Enriched>>,
    name: &str,
) -> UseBeforeEffects {
    for expression in expressions {
        match search_use_before_effects(expression, name) {
            UseBeforeEffects::Clear => {}
            other => return other,
        }
    }
    UseBeforeEffects::Clear
}

/// Count IR nodes in `e`. Used by [`match_fusion`]'s budget check;
/// approximate. Counts every `Expr` node in the tree (sub-trees of
/// `CallArg::Type` are not counted, as they don't contribute to
/// runtime cost).
fn ir_node_count(e: &Expr<Enriched>) -> usize {
    1 + match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => 0,
        Expr::Call { callee, args, .. } => {
            ir_node_count(callee)
                + args
                    .iter()
                    .map(|a| match a {
                        CallArg::Value(v) => ir_node_count(v),
                        CallArg::Type(_) => 0,
                    })
                    .sum::<usize>()
        }
        Expr::FnExpr { body, .. } => ir_node_count(body),
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            ir_node_count(value) + ir_node_count(body)
        }
        Expr::EnrichedTuple { items, .. } => items.iter().map(ir_node_count).sum(),
        Expr::EnrichedRecord { fields, .. } => fields.iter().map(|f| ir_node_count(&f.value)).sum(),
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            ir_node_count(target)
        }
        Expr::EnrichedInject { payload, .. } => ir_node_count(payload),
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => ir_node_count(scrutinee) + arms.iter().map(|a| ir_node_count(&a.body)).sum::<usize>(),
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => ir_node_count(cond) + ir_node_count(then_branch) + ir_node_count(else_branch),
        // Pre-enriched / surface variants are uninhabited at
        // `Enriched`.
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // Low-IR variants are uninhabited at `Enriched`.
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

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::{Enriched, Expr, Item, Meta, Module, PathSegment};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_module_file};
    use crate::pass::structural_recovery::recover_package;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use crate::span::Span;
    use std::path::{Path, PathBuf};

    /// Derive the on-disk file path for a module from its declared
    /// `module a/b;` path: `a/b.kio`. Per `specs/package.md`
    /// § Module-name rules, the declared segments equal the file's
    /// path relative to the package root — the package name is not
    /// prepended.
    fn module_file_path(module_path: &str) -> PathBuf {
        let segs: Vec<&str> = module_path.split('/').collect();
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(seg);
        }
        let stem = segs.last().copied().unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    fn elab_support_entries() -> Vec<(PathBuf, crate::ast::Module<crate::ast::Surface>)> {
        [
            (
                "testapi.kio",
                include_str!("../../../test-data/poc/elab/workdir/testapi.kio"),
            ),
            (
                "elaborator_util.kio",
                include_str!("../../../test-data/poc/elab/workdir/elaborator_util.kio"),
            ),
            (
                "spine_elaborators.kio",
                include_str!("../../../test-data/poc/elab/workdir/spine_elaborators.kio"),
            ),
            (
                "match.kio",
                include_str!("../../../test-data/poc/elab/workdir/match.kio"),
            ),
            (
                "control.kio",
                include_str!("../../../test-data/poc/elab/workdir/control.kio"),
            ),
        ]
        .into_iter()
        .map(|(path, source)| {
            let module = parse_module_file(source).expect("parse support").module;
            (PathBuf::from(path), module)
        })
        .collect()
    }

    fn optimize_one(module_path: &str, src: &str) -> Module<Enriched> {
        let parsed = parse(src).expect("parse");
        let mut parsed_modules = vec![(module_file_path(module_path), parsed)];
        parsed_modules.extend(elab_support_entries());
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed_modules, None).expect("lower_package");
        let package = crate::pass::resolve::Package::build(Path::new(""), lowered_modules, None)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let recovered = recover_package(&prime);
        let optimized = super::optimize_package(recovered);
        optimized
            .module(module_path)
            .expect("module present")
            .module
            .clone()
    }

    fn recover_one(module_path: &str, src: &str) -> Module<Enriched> {
        let parsed = parse(src).expect("parse");
        let mut parsed_modules = vec![(module_file_path(module_path), parsed)];
        parsed_modules.extend(elab_support_entries());
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed_modules, None).expect("lower_package");
        let package = crate::pass::resolve::Package::build(Path::new(""), lowered_modules, None)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        recover_package(&prime)
            .module(module_path)
            .expect("module present")
            .module
            .clone()
    }

    fn fn_body<'m>(module: &'m Module<Enriched>, name: &str) -> &'m Expr<Enriched> {
        module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == name => Some(&d.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn `{name}` not found"))
    }

    fn is_path_ref(e: &Expr<Enriched>, expected: &str) -> bool {
        matches!(
            e,
            Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].name == expected
        )
    }

    fn test_meta() -> Meta<Enriched> {
        Meta::new(Span::new(0, 0))
    }

    fn test_unit_ty() -> Type<Enriched> {
        Type::Unit { meta: test_meta() }
    }

    fn test_pair_ty() -> Type<Enriched> {
        Type::Product {
            left: Box::new(test_unit_ty()),
            right: Box::new(test_unit_ty()),
            meta: test_meta(),
        }
    }

    fn test_path(name: &str) -> Expr<Enriched> {
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name, Span::new(0, 0))],
            meta: test_meta(),
            ext: (),
        }
    }

    fn test_project(target: &str, index: usize) -> Expr<Enriched> {
        Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(test_path(target)),
            index,
            arity: 2,
            target_ty: test_pair_ty(),
            meta: test_meta(),
            ext: (),
        }
    }

    fn bound_tuple_projection_call(
        tuple_items: Vec<Expr<Enriched>>,
        call_indices: &[usize],
        callee: &str,
    ) -> Expr<Enriched> {
        Expr::Let {
            occurrence: Default::default(),
            name: "packet".to_owned(),
            name_span: Span::new(0, 0),
            ty: Some(test_pair_ty()),
            pattern: (),
            value: Box::new(Expr::EnrichedTuple {
                occurrence: Default::default(),
                items: tuple_items,
                synth_ty: test_pair_ty(),
                meta: test_meta(),
                ext: (),
            }),
            body: Box::new(Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(test_path(callee)),
                args: std::iter::once(CallArg::Type(test_unit_ty()))
                    .chain(
                        call_indices
                            .iter()
                            .map(|index| CallArg::Value(test_project("packet", *index))),
                    )
                    .collect(),
                meta: test_meta(),
                ext: (),
            }),
            meta: test_meta(),
        }
    }

    fn contains<F>(e: &Expr<Enriched>, pred: &F) -> bool
    where
        F: Fn(&Expr<Enriched>) -> bool,
    {
        if pred(e) {
            return true;
        }
        match e {
            Expr::Call { callee, args, .. } => {
                contains(callee, pred)
                    || args.iter().any(|a| match a {
                        CallArg::Value(v) => contains(v, pred),
                        CallArg::Type(_) => false,
                    })
            }
            Expr::FnExpr { body, .. } => contains(body, pred),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                contains(value, pred) || contains(body, pred)
            }
            Expr::EnrichedTuple { items, .. } => items.iter().any(|i| contains(i, pred)),
            Expr::EnrichedRecord { fields, .. } => fields.iter().any(|f| contains(&f.value, pred)),
            Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
                contains(target, pred)
            }
            Expr::EnrichedInject { payload, .. } => contains(payload, pred),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => contains(scrutinee, pred) || arms.iter().any(|a| contains(&a.body, pred)),
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => contains(cond, pred) || contains(then_branch, pred) || contains(else_branch, pred),
            _ => false,
        }
    }

    /// `__fst__(_, _, __pair__(_, _, a, b))` recovers to
    /// `Project(Tuple[A][B], 0)`; the optimizer collapses that to
    /// `a`. The whole body is `()`.
    #[test]
    fn project_zero_after_tuple_picks_first() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t() -> . { __fst__(., ., __pair__(., ., (), ())) }",
        );
        let body = fn_body(&m, "t");
        assert!(
            matches!(body, Expr::Unit { .. }),
            "expected Unit, got {body:?}"
        );
        // No `EnrichedProject` anywhere.
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedProject { .. })),
            "expected no project, got body: {body:?}"
        );
        // No `EnrichedTuple` anywhere — only its component flowed
        // through.
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedTuple { .. })),
            "expected no tuple, got body: {body:?}"
        );
    }

    /// `Hed.get(__fst__(__pair__({hed = x}, ())))` may recover as a
    /// folded field-get or as canonical administrative lets around the
    /// constructor, tuple, and projection. Either route may collapse
    /// the complete constructor/projector round-trip, but it must yield
    /// the payload `x`, not the raw tuple slot `Hed.mk(x)`.
    #[test]
    fn field_get_after_tuple_preserves_projector_semantics() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type I32 role(i32); \
             labels { hed: I32 }; \
             fn t(x: I32) -> I32 { \
               Hed.get(__fst__(Hed, ., __pair__(Hed, ., {hed = x}, ()))) \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            is_path_ref(body, "x"),
            "expected the selected label slot to be unwrapped to `x`, got {body:?}"
        );
    }

    #[test]
    fn field_get_over_recovered_tail_composes_to_original_product() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             labels { a: ., b: ., c: . }; \
             type Wide = A & B & C; \
             fn read(r: Wide) -> . { r.?{c} }",
        );
        let body = fn_body(&m, "read");
        match body {
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                ..
            } => {
                assert_eq!(*index, 2, "field access should target the flat slot");
                assert_eq!(
                    *arity, 3,
                    "field access should keep the outer product arity"
                );
                assert!(
                    is_path_ref(target, "r"),
                    "field access should read from the original product: {target:?}"
                );
            }
            other => panic!("expected EnrichedFieldGet, got {other:?}"),
        }
        assert!(
            !contains(body, &|e| matches!(e, Expr::Let { .. })),
            "tail materialization should not survive under field access: {body:?}"
        );
    }

    #[test]
    fn field_update_preserved_tail_slots_compose_to_original_product() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             labels { a: ., b: ., c: ., d: . }; \
             type Wide = A & B & C & D; \
             fn update(r: Wide) -> Wide { r.!{c = ()} }",
        );
        let body = fn_body(&m, "update");
        assert!(
            !contains(body, &|e| matches!(
                e,
                Expr::EnrichedProject { target, .. }
                    if matches!(target.as_ref(), Expr::Let { .. })
            )),
            "preserved slots should not project from rebuilt tail tuples: {body:?}"
        );
        let Expr::Let {
            name: receiver,
            body: update,
            ..
        } = body
        else {
            panic!("field update should bind its receiver once: {body:?}");
        };
        assert!(
            contains(update, &|e| matches!(
                e,
                Expr::EnrichedProject { target, index: 3, arity: 4, .. }
                    if matches!(
                        target.as_ref(),
                        Expr::Path { segments, .. }
                            if segments.len() == 1 && segments[0].name == *receiver
                    )
            )),
            "last preserved slot should read directly from the bound receiver: {body:?}"
        );
    }

    /// `let x = E in F[x]` with `x` used exactly once and `E` pure
    /// → substitute E for x in F. The let disappears.
    #[test]
    fn let_fuse_single_use_inlines_pure_value() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             fn t[A](y: A) -> A { let x = y; x }",
        );
        // Whole body should reduce to a bare `y`.
        let body = fn_body(&m, "t");
        assert!(!contains(body, &|e| matches!(e, Expr::Let { .. })));
        match body {
            Expr::Path { segments, .. } => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].name, "y");
            }
            other => panic!("expected Path `y`, got {other:?}"),
        }
    }

    #[test]
    fn let_fuse_keeps_type_application_before_a_later_effect() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             host type N; \
             host fn effect(_unit: .) -> .; \
             fn stage(_unit: .)[A] -> A -> A { .(value: A) -> A { value } } \
             fn t(value: N) -> N { \
               let residual = stage()(N); \
               effect(()); \
               residual(value) \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            matches!(body, Expr::Let { value, .. }
                if matches!(value.as_ref(), Expr::Call { args, .. }
                    if matches!(args.as_slice(), [CallArg::Type(_)]))),
            "a type application is an effectful application boundary and must stay before the later host effect: {body:#?}"
        );
    }

    /// `let x = E in F` with `x` unused and `E` pure → `F` (let
    /// drops entirely).
    #[test]
    fn let_fuse_zero_use_pure_drops_let() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             fn t[A](y: A, z: A) -> A { let x = y; z }",
        );
        let body = fn_body(&m, "t");
        // Body should be just `z`.
        match body {
            Expr::Path { segments, .. } => {
                assert_eq!(segments[0].name, "z");
            }
            other => panic!("expected Path `z`, got {other:?}"),
        }
    }

    fn fuse_recovered_top_let(src: &str) -> (Expr<Enriched>, bool) {
        let module = recover_one("x/main", src);
        let mut changed = false;
        let body = let_fuse(fn_body(&module, "t").clone(), &mut changed);
        (body, changed)
    }

    #[test]
    fn let_fuse_impure_value_moves_past_pure_predecessor() {
        let (body, changed) = fuse_recovered_top_let(
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               pick_second(x, delayed) \
             }",
        );
        assert!(changed, "expected the top-level let to fuse: {body:?}");
        assert!(!matches!(body, Expr::Let { .. }));
    }

    #[test]
    fn let_fuse_impure_value_stops_at_impure_predecessor() {
        let (body, changed) = fuse_recovered_top_let(
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               pick_second(sub_i32(x, 2(I32)), delayed) \
             }",
        );
        assert!(!changed, "let crossed an earlier effect: {body:?}");
        assert!(matches!(body, Expr::Let { .. }));
    }

    #[test]
    fn let_fuse_impure_value_stays_outside_deferred_uses() {
        let sources = [
            "module x/main; import control(if); \
             host type Bool role(bool); \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(cond: Bool, x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               if! cond { delayed } else { x } \
             }",
            "module x/main; import __intrinsics__; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(value: I32 | I32, x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               __either__(value, \
                 .(left: I32) -> I32 { delayed }, \
                 .(right: I32) -> I32 { x }) \
             }",
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(x: I32) -> (I32 -> I32) { \
               let delayed = sub_i32(x, 1(I32)); \
               .(_arg: I32) -> I32 { delayed } \
             }",
        ];
        for source in sources {
            let (body, changed) = fuse_recovered_top_let(source);
            assert!(!changed, "let moved into a deferred use: {body:?}");
            assert!(matches!(body, Expr::Let { .. }));
        }
    }

    #[test]
    fn let_fuse_impure_value_moves_past_pure_let_and_seq_prefixes() {
        let sources = [
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               let prefix = x; \
               pick_second(prefix, delayed) \
             }",
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               (); \
               pick_second(x, delayed) \
             }",
        ];
        for source in sources {
            let (body, changed) = fuse_recovered_top_let(source);
            assert!(changed, "pure prefix prevented safe fusion: {body:?}");
            assert!(!matches!(body, Expr::Let { name, .. } if name == "delayed"));
        }
    }

    #[test]
    fn let_fuse_uses_alpha_distinct_inner_binder() {
        let (body, changed) = fuse_recovered_top_let(
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32, replacement: I32) -> I32 { \
               let delayed = sub_i32(x, 1(I32)); \
               let x = replacement; \
               pick_second(x, delayed) \
             }",
        );
        assert!(
            changed,
            "alpha-distinct binder prevented safe fusion: {body:?}"
        );
        assert!(matches!(&body, Expr::Let { name, .. } if name == "x_n2"));
        assert!(
            !contains(
                &body,
                &|expr| matches!(expr, Expr::Let { name, .. } if name == "delayed")
            ),
            "the fused binding survived: {body:?}"
        );
    }

    #[test]
    fn beta_reduces_unary_function_literal_call() {
        let m = optimize_one(
            "x/main",
            "module x/main; \
             fn t() -> . { .(_u: .) { () }(()) }",
        );
        let body = fn_body(&m, "t");
        assert!(
            matches!(body, Expr::Unit { .. }),
            "expected Unit, got {body:?}"
        );
        assert!(
            !contains(body, &|e| matches!(
                e,
                Expr::Call { .. } | Expr::FnExpr { .. }
            )),
            "expected function literal call to disappear, got body: {body:?}"
        );
    }

    #[test]
    fn nested_unit_match_clauses_do_not_survive_as_calls() {
        const MODULE_PATH: &str = "x/main";
        const SOURCE: &str = "module x/main; import match(match); \
             fn t() -> . { \
               match! () { .(_u: .) { \
                 match! () { .(_u: .) { \
                   match! () { .(_u: .) { () } } \
                 } } \
               } } \
             }";

        #[cfg(feature = "parallel")]
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .stack_size(1792 * 1024)
            .build()
            .expect("build bounded-stack optimizer regression pool");
        #[cfg(feature = "parallel")]
        let m = pool.install(|| optimize_one(MODULE_PATH, SOURCE));

        #[cfg(not(feature = "parallel"))]
        let m = std::thread::Builder::new()
            .name("bounded-stack-optimizer-regression".into())
            .stack_size(1792 * 1024)
            .spawn(|| optimize_one(MODULE_PATH, SOURCE))
            .expect("spawn bounded-stack optimizer regression thread")
            .join()
            .expect("bounded-stack optimizer regression thread panicked");
        let body = fn_body(&m, "t");
        assert!(
            matches!(body, Expr::Unit { .. }),
            "expected Unit, got {body:?}"
        );
        assert!(
            !contains(body, &|e| matches!(
                e,
                Expr::Call { .. } | Expr::FnExpr { .. }
            )),
            "expected nested unit match clauses to fold away, got body: {body:?}"
        );
    }

    /// `let x = __absurd__(A, b) in body` → `__absurd__(A, b)`. The body
    /// is dead — the call diverges. The optimizer drops the body
    /// subtree.
    #[test]
    fn absurd_propagates_past_let_body() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A](b: !) -> A { \
               let x = __absurd__(A, b); x \
             }",
        );
        let body = fn_body(&m, "t");
        // No `Let` remains — collapsed to the bare call.
        assert!(
            !contains(body, &|e| matches!(e, Expr::Let { .. })),
            "expected no let, got body: {body:?}"
        );
        // The body is exactly `__absurd__(A, b)`.
        assert!(
            matches!(body, Expr::Call { callee, .. }
                if matches!(&**callee, Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].name == "__absurd__")),
            "expected `__absurd__(…)` call, got {body:?}"
        );
    }

    /// `__either__(__left__(p, …), fl, fr)` recovers to
    /// `Match(Inject(p, 0), [fl_arm, fr_arm])`; the optimizer
    /// collapses that to `let fl_arm.param = p in fl_arm.body`.
    #[test]
    fn match_after_left_injection_picks_left_arm() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A][B](x: A, y: A) -> A { \
               __either__(\
                 __left__(A, B, x), \
                 .(l: A) -> A { l }, \
                 .(r: B) -> A { y }) \
             }",
        );
        // No `EnrichedMatch` should remain — collapsed to a let.
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedMatch { .. })),
            "expected no match, got body: {body:?}"
        );
        // No `EnrichedInject` either — collapsed into the let RHS.
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedInject { .. })),
            "expected no inject, got body: {body:?}"
        );
    }

    /// `__snd__(_, _, __pair__(_, _, x, y))` → `y`. Same as the
    /// `__fst__` test but for index 1.
    #[test]
    fn project_one_after_tuple_picks_second() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A][B](x: A, y: B) -> B { \
               __snd__(A, B, __pair__(A, B, x, y)) \
             }",
        );
        let body = fn_body(&m, "t");
        // The body should be exactly the second arg's path
        // reference.
        match body {
            Expr::Path { segments, .. } => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].name, "y");
            }
            other => panic!("expected Path `y`, got {other:?}"),
        }
    }

    #[test]
    fn pure_tuple_tail_projection_drops_recovery_let() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A][B][C](x: A, y: B, z: C) -> B & C { \
               __snd__(A, B & C, __pair__(A, B & C, x, __pair__(B, C, y, z))) \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::Let { .. })),
            "expected no recovery let, got body: {body:?}"
        );
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedProject { .. })),
            "expected no projection, got body: {body:?}"
        );
        let Expr::EnrichedTuple { items, .. } = body else {
            panic!("expected EnrichedTuple tail, got {body:?}");
        };
        assert_eq!(items.len(), 2);
        for (item, expected) in items.iter().zip(["y", "z"]) {
            match item {
                Expr::Path { segments, .. } => {
                    assert_eq!(segments.len(), 1);
                    assert_eq!(segments[0].name, expected);
                }
                other => panic!("expected Path `{expected}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn pure_bound_tuple_call_projections_inline_once_in_order() {
        let input = bound_tuple_projection_call(
            vec![test_project("source", 0), test_project("source", 1)],
            &[0, 1],
            "callee",
        );

        let mut changed = false;
        let output = let_bound_tuple_projection(input, &mut changed);
        assert!(
            changed,
            "ordered projections should eliminate the tuple let"
        );
        let Expr::Call { args, .. } = output else {
            panic!("expected the call without its tuple let")
        };
        assert_eq!(args.len(), 3);
        assert!(matches!(&args[0], CallArg::Type(Type::Unit { .. })));
        for (arg, expected_index) in args[1..].iter().zip([0, 1]) {
            assert!(matches!(
                arg,
                CallArg::Value(Expr::EnrichedProject { target, index, .. })
                    if *index == expected_index && is_path_ref(target, "source")
            ));
        }
    }

    #[test]
    fn bound_tuple_call_projection_guard_rejects_unsafe_shapes() {
        let impure_item = Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(test_path("effect")),
            args: Vec::new(),
            meta: test_meta(),
            ext: (),
        };
        let mut callee_packet_use = bound_tuple_projection_call(
            vec![test_project("source", 0), test_project("source", 1)],
            &[0, 1],
            "callee",
        );
        let Expr::Let { body, .. } = &mut callee_packet_use else {
            unreachable!("test helper always returns a let")
        };
        let Expr::Call { callee, .. } = body.as_mut() else {
            unreachable!("test helper always places a call in the let body")
        };
        **callee = Expr::Let {
            occurrence: Default::default(),
            name: "captured".to_owned(),
            name_span: Span::new(0, 0),
            ty: Some(test_pair_ty()),
            pattern: (),
            value: Box::new(test_path("packet")),
            body: Box::new(test_path("callee")),
            meta: test_meta(),
        };
        let cases = [
            (
                "impure tuple item",
                bound_tuple_projection_call(
                    vec![impure_item, test_project("source", 1)],
                    &[0, 1],
                    "callee",
                ),
            ),
            (
                "reversed projections",
                bound_tuple_projection_call(
                    vec![test_project("source", 0), test_project("source", 1)],
                    &[1, 0],
                    "callee",
                ),
            ),
            (
                "duplicate projections",
                bound_tuple_projection_call(
                    vec![test_project("source", 0), test_project("source", 1)],
                    &[0, 0],
                    "callee",
                ),
            ),
            ("callee packet use", callee_packet_use),
        ];
        for (label, input) in cases {
            let mut changed = false;
            let output = let_bound_tuple_projection(input, &mut changed);
            assert!(!changed, "{label} must not remove the tuple let");
            assert!(matches!(output, Expr::Let { .. }), "{label}: {output:?}");
        }
    }

    /// A two-arm `EnrichedMatch` over a unit-payload `__left__`
    /// recovers to `Match(Inject(?, 0, 2), [...])`, and Inject-then-Match
    /// collapses to `let arm.param = payload in arm.body`. The
    /// let-fuse may then simplify further. End-to-end smoke that
    /// the catalog composes.
    #[test]
    fn inject_match_let_fuse_compose() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A](x: A) -> A { \
               __either__(\
                 __left__(A, A, x), \
                 .(l: A) -> A { l }, \
                 .(r: A) -> A { r }) \
             }",
        );
        let body = fn_body(&m, "t");
        // No match, no inject, no let — collapsed to `x`.
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedMatch { .. })),
            "expected no match, got body: {body:?}"
        );
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedInject { .. })),
            "expected no inject, got body: {body:?}"
        );
        // Note: the let may or may not survive depending on
        // post-substitution shape; either is acceptable behavior.
    }

    #[test]
    fn let_fuse_revisits_nested_static_injection_match() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             newtype Red : . { constructor mk_red; projector un_red; }; \
             newtype Green : . { constructor mk_green; projector un_green; }; \
             newtype Blue : . { constructor mk_blue; projector un_blue; }; \
             host fn consume[A](value: A) -> .; \
             fn t() -> . { \
               let green = Green.mk_green(()); \
               let inner = __left__(Green, Blue, green); \
               let widened = __right__(Red, Green | Blue, inner); \
               consume(Red | Green | Blue, widened) \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedMatch { .. })),
            "newly exposed Match(Inject) should reduce: {body:?}"
        );
        assert!(
            contains(body, &|e| matches!(
                e,
                Expr::EnrichedInject {
                    variant: 1,
                    variants: 3,
                    ..
                }
            )),
            "expected the flat middle injection: {body:?}"
        );
    }

    /// `match_fusion` firing coverage. A non-colliding case-of-case
    /// `Match(Match(s, [x, y]), [p, q])` fuses by pushing the outer
    /// arms into each inner arm, so the result's top-level scrutinee is
    /// the innermost scrutinee `s` (no surviving `EnrichedMatch` at the
    /// scrutinee position). The transform is observationally a no-op
    /// when it fires correctly, so a golden can't pin firing — this
    /// asserts the structural rewrite directly.
    #[test]
    fn match_fusion_fires_on_non_colliding_case_of_case() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A](s: A | A) -> (A | A) { \
               __either__(\
                 __either__(A, A, A | A, s, \
                   .(x: A) -> (A | A) { __left__(A, A, x) }, \
                   .(y: A) -> (A | A) { __right__(A, A, y) }), \
                 .(p: A) -> (A | A) { __left__(A, A, p) }, \
                 .(q: A) -> (A | A) { __right__(A, A, q) }) \
             }",
        );
        let body = fn_body(&m, "t");
        let Expr::EnrichedMatch { scrutinee, .. } = body else {
            panic!("expected top-level EnrichedMatch, got {body:?}");
        };
        // Fused: the outer match was pushed into the inner arms, so the
        // top-level scrutinee is the original innermost scrutinee `s`,
        // not the inner `EnrichedMatch`.
        assert!(
            !matches!(**scrutinee, Expr::EnrichedMatch { .. }),
            "match_fusion did not fire: scrutinee is still a nested match: {scrutinee:?}"
        );
        match &**scrutinee {
            Expr::Path { segments, .. } => {
                assert_eq!(segments.len(), 1);
                assert_eq!(segments[0].name, "s");
            }
            other => panic!("expected innermost scrutinee Path `s`, got {other:?}"),
        }
    }

    #[test]
    fn no_match_fusion_mode_leaves_nested_case_tree_unexpanded() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A](s: A | A) -> (A | A) { \
               __either__( \
                 __either__(A, A, A | A, s, \
                   .(x: A) -> (A | A) { __left__(A, A, x) }, \
                   .(y: A) -> (A | A) { __right__(A, A, y) }), \
                 .(p: A) -> (A | A) { __left__(A, A, p) }, \
                 .(q: A) -> (A | A) { __right__(A, A, q) }) \
             }",
        );
        let body = optimize_expr_in_mode(
            &OptimizerCtx::default(),
            fn_body(&m, "t").clone(),
            CatalogMode::NoMatchFusion,
        );
        let Expr::EnrichedMatch { scrutinee, .. } = body else {
            panic!("expected outer EnrichedMatch");
        };
        assert!(
            matches!(*scrutinee, Expr::EnrichedMatch { .. }),
            "restricted child revisit expanded a nested case tree: {scrutinee:?}"
        );
    }

    /// Alpha normalization makes the authored outer `x` and inner arm `x`
    /// distinct before optimization, so match fusion can safely retain the
    /// former while binding the latter under its normalized name.
    #[test]
    fn match_fusion_uses_alpha_distinct_case_binders() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn t[A](s: A | A, x: A) -> (A | A) { \
               __either__(\
                 __either__(A, A, A | A, s, \
                   .(x: A) -> (A | A) { __left__(A, A, x) }, \
                   .(y: A) -> (A | A) { __right__(A, A, y) }), \
                 .(p: A) -> (A | A) { __left__(A, A, x) }, \
                 .(q: A) -> (A | A) { __right__(A, A, q) }) \
             }",
        );
        let body = fn_body(&m, "t");
        let Expr::EnrichedMatch {
            scrutinee, arms, ..
        } = body
        else {
            panic!("expected top-level EnrichedMatch, got {body:?}");
        };
        assert!(matches!(
            scrutinee.as_ref(),
            Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].name == "s"
        ));
        assert!(arms.iter().any(|arm| arm.param == "x_n2"));
        assert!(arms.iter().any(|arm| {
            contains(&arm.body, &|expr| {
                matches!(
                    expr,
                    Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].name == "x"
                )
            })
        }));
    }

    fn assert_selected_nullary_branch(body: &Expr<Enriched>, expected: &str) {
        let Expr::Call { callee, args, .. } = body else {
            panic!("expected selected thunk call, got {body:?}");
        };
        let Expr::FnExpr { sig, body, .. } = callee.as_ref() else {
            panic!("expected selected nullary thunk, got {callee:?}");
        };
        assert!(sig.params.is_empty());
        assert!(matches!(
            sig.groups.as_slice(),
            [crate::ast::SignatureGroupKind::Value { len: 0 }]
        ));
        assert!(matches!(
            args.as_slice(),
            [crate::ast::CallArg::Value(Expr::Unit { .. })]
        ));
        assert!(
            matches!(body.as_ref(), Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].name == expected),
            "selected the wrong branch: {body:?}"
        );
    }

    /// `constant_fold` firing coverage: `if! .t { a } else { b }`
    /// (an `EnrichedConditional(BoolLit(true), …)` — `.t` is the
    /// `role(bool)` true marker) folds to the `then` branch `a`, with
    /// no `EnrichedConditional` remaining. Folding vs. emitting a
    /// `if! .t { } else { }` print the same value, so a runtime
    /// golden can't pin the fold.
    #[test]
    fn constant_fold_selects_then_on_literal_true() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; import control(if); \
             import testapi(Bool); \
             fn t[A](a: A, b: A) -> A { \
               if! .t { a } else { b } \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedConditional { .. })),
            "expected no conditional after fold, got body: {body:?}"
        );
        assert_selected_nullary_branch(body, "a");
    }

    /// Symmetric: `if! .f { a } else { b }` folds to the `else`
    /// branch `b`.
    #[test]
    fn constant_fold_selects_else_on_literal_false() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; import control(if); \
             import testapi(Bool); \
             fn t[A](a: A, b: A) -> A { \
               if! .f { a } else { b } \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedConditional { .. })),
            "expected no conditional after fold, got body: {body:?}"
        );
        assert_selected_nullary_branch(body, "b");
    }

    /// True when `e` contains any call whose callee is a local/selective
    /// `<NT>.<member>` or qualified `<alias>.<NT>.<member>` path.
    fn contains_newtype_member_call(e: &Expr<Enriched>) -> bool {
        contains(e, &|e| {
            matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if (2..=3).contains(&segments.len()))
            )
        })
    }

    /// `newtype_identity_elision` firing coverage:
    /// `Foo.un_foo(A, Foo.mk_foo(A, x))` collapses to the bare `x`.
    /// The optimizer is observationally a no-op (the round-trip is
    /// identity in every backend), so no runtime golden can pin it.
    #[test]
    fn newtype_unwrap_of_wrap_collapses_to_payload() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             newtype Foo[A] : A { pub constructor mk_foo; pub projector un_foo; }; \
             fn t[A](x: A) -> A { Foo.un_foo(A, Foo.mk_foo(A, x)) }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains_newtype_member_call(body),
            "expected no surviving newtype-member call after elision, got body: {body:?}"
        );
        match body {
            Expr::Path { segments, .. } => {
                assert_eq!(segments.len(), 1, "expected bare payload path");
                assert_eq!(segments[0].name, "x");
            }
            other => panic!("expected bare payload Path `x`, got {other:?}"),
        }
    }

    #[test]
    fn same_named_newtype_uses_consumer_module_members() {
        let parsed_modules = [
            (
                "a.kio",
                "module a; \
                 newtype Foo : . { pub constructor make_a; pub projector un_a; };",
            ),
            (
                "b.kio",
                "module b; \
                 newtype Foo : . { pub constructor wrap; pub projector unwrap; }; \
                 fn t(value: .) -> . { Foo.unwrap(Foo.wrap(value)) }",
            ),
        ]
        .into_iter()
        .map(|(path, source)| {
            (
                PathBuf::from(path),
                parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
            )
        })
        .collect();
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed_modules, None).expect("lower package");
        let package = crate::pass::resolve::Package::build(Path::new(""), lowered_modules, None)
            .expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = check_package(&package).expect("check package");
        let recovered = recover_package(&prime);
        let optimized = super::optimize_package(recovered);
        let b = &optimized.module("b").expect("module b").module;
        let body = fn_body(b, "t");

        assert!(
            matches!(body, Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].name == "value"),
            "an unrelated same-named newtype hid `b.Foo`'s identity pair: {body:?}"
        );
    }

    #[test]
    fn qualified_newtype_elision_compares_resolved_identity_not_alias_spelling() {
        let parsed_modules = [
            (
                "provider.kio",
                "module provider; \
                 pub newtype Box : . { pub constructor wrap; pub projector unwrap; };",
            ),
            (
                "decoy.kio",
                "module decoy; \
                 pub newtype Box : . { pub constructor wrap; pub projector unwrap; };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import provider(Box); \
                 import provider as p; import provider as q; import decoy as d; \
                 fn same_provider(value: .) -> . { p.Box.unwrap(q.Box.wrap(value)) } \
                 fn mixed_route(value: .) -> . { Box.unwrap(p.Box.wrap(value)) } \
                 fn distinct_provider(value: d.Box) -> p.Box { \
                   p.Box.wrap(d.Box.unwrap(value)) \
                 }",
            ),
        ]
        .into_iter()
        .map(|(path, source)| {
            (
                PathBuf::from(path),
                parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
            )
        })
        .collect();
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed_modules, None).expect("lower package");
        let package = crate::pass::resolve::Package::build(Path::new(""), lowered_modules, None)
            .expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = check_package(&package).expect("check package");
        let optimized = super::optimize_package(recover_package(&prime));
        let consumer = &optimized
            .module("consumer")
            .expect("consumer module")
            .module;

        for function in ["same_provider", "mixed_route"] {
            let body = fn_body(consumer, function);
            assert!(
                matches!(body, Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].name == "value"),
                "{function} must elide through the exact provider identity: {body:?}"
            );
        }
        let distinct = fn_body(consumer, "distinct_provider");
        assert!(
            contains_newtype_member_call(distinct),
            "same-spelled members from distinct providers must not form an inverse pair: {distinct:?}"
        );
    }

    /// `dead_code_elimination` Seq firing coverage: a pure
    /// expression-statement `Seq { value: pure, body }` drops to
    /// `body`. `(); ()` is `Seq { value: Unit, body: Unit }` — the
    /// leading `()` is pure and dead, so the whole body reduces to a
    /// bare `Unit` with no surviving `Seq`. The adjacent let-fuse
    /// tests never build a `Seq`, so this is the only firing test for
    /// the DCE Seq arm.
    #[test]
    fn dead_code_elimination_drops_pure_seq_statement() {
        let m = optimize_one("x/main", "module x/main; fn t() -> . { (); () }");
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::Seq { .. })),
            "expected no Seq after dead-code elimination, got body: {body:?}"
        );
        assert!(
            matches!(body, Expr::Unit { .. }),
            "expected bare Unit, got {body:?}"
        );
    }
}

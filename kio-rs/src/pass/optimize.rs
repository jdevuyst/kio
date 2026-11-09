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
//!   `e_i` when moving it past the later items preserves effect order.
//!   Non-selected impure items remain in a leading [`Expr::Seq`] chain.
//! - [`compose_recovered_tail_access`] — a projection or field-get
//!   over a retained final-slot projection or a reconstructed tail composes back
//!   into a direct projection of the original product. This keeps
//!   wide label-field reads and updates from rebuilding every suffix
//!   product before selecting a slot.
//! - [`let_tuple_reconstruction`] — reuse an ordered, complete product
//!   reconstruction without unpacking and rebuilding its bound value.
//! - [`propagate_bound_tuple_slots`] — move inert single-use slots through
//!   projections, retaining at most one computed or shared slot in its let.
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
//! - [`beta_reduce_nullary_iife`] — `.() { body }(unit)` →
//!   `unit; body` for a literal with one empty value stage. An omitted
//!   Unit argument reduces directly to the body; an explicit argument
//!   retains its effects before the body.
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
//! pure forms), `match`, `if`/`else`, `let`/`seq` chains involving
//! sub-expressions of unknown purity, and every Enriched node whose
//! sub-tree could harbor a host call are not.
//!
//! The shared purity analysis keeps closure construction separate from
//! invocation. Unknown callable values retain their effects.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
use crate::ast::{CallArg, Enriched, Expr, FnDef, Item, Module, PackageFile, Type};
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
    pure_functions: crate::pass::purity::PureFunctions,
}

impl OptimizerCtx {
    fn for_module(package: &Package<Enriched>, module: &Module<Enriched>) -> Self {
        let mut ctx = Self {
            pure_functions: crate::pass::purity::PureFunctions::for_module(package, module),
            ..Default::default()
        };
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

    fn shadowed<'a, 'n>(
        &'a self,
        names: impl Iterator<Item = &'n str>,
    ) -> std::borrow::Cow<'a, Self> {
        let mut context = std::borrow::Cow::Borrowed(self);
        for name in names {
            if context.pure_functions.contains_head(name) {
                context.to_mut().pure_functions.remove_head(name);
            }
        }
        context
    }

    fn evaluation_is_pure(&self, expression: &Expr<Enriched>) -> bool {
        crate::pass::purity::evaluation_is_pure(expression, |path| {
            self.pure_functions.contains(path)
        })
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
    ctx.pure_functions = inputs
        .pure_functions()
        .get(&module_path)
        .cloned()
        .unwrap_or_default();
    for (scoped_name, input) in inputs.newtype_members() {
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
        | Item::Equiv(_, ext)
        | Item::Macro(_, ext)
        | Item::Op(_, ext)
        | Item::VariadicOperator(_, ext)
        | Item::RecGroup(_, ext) => match ext {},
    }
}

fn optimize_fn_def(ctx: &OptimizerCtx, mut d: FnDef<Enriched>) -> FnDef<Enriched> {
    d.body = optimize_expr(&ctx.shadowed(d.sig.value_param_names()), d.body);
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
        e = let_tuple_reconstruction(e, &mut changed);
        e = propagate_bound_tuple_slots(e, &mut changed);
        e = let_bound_tuple_projection(e, &mut changed);
        e = recover_bound_conditional(e, &mut changed);
        e = inject_then_match(e, &mut changed);
        e = absurd_propagate(e, &mut changed);
        if mode == CatalogMode::Full {
            e = match_fusion(e, &mut changed);
        }
        e = let_fuse(ctx, e, &mut changed);
        e = beta_reduce_unary_iife(e, &mut changed);
        e = beta_reduce_nullary_iife(e, &mut changed);
        e = dead_code_elimination(ctx, e, &mut changed);
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
            source_trivia: _,
            occurrence: _,
            diagnostic: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
            body: Box::new(optimize_expr_in_mode(
                &ctx.shadowed(sig.value_param_names()),
                *body,
                mode,
            )),
            sig,
            ret_ty,
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
            body: Box::new(optimize_expr_in_mode(
                &ctx.shadowed(std::iter::once(name.as_str())),
                *body,
                mode,
            )),
            name,
            name_span,
            ty,
            pattern,
            value: Box::new(optimize_expr_in_mode(ctx, *value, mode)),
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
                    a.body = optimize_expr_in_mode(
                        &ctx.shadowed(std::iter::once(a.param.as_str())),
                        a.body,
                        mode,
                    );
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
fn is_pure(e: &Expr<Enriched>) -> bool {
    crate::pass::purity::evaluation_is_pure(e, |_| false)
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
/// Non-selected impure items remain in order in a leading [`Expr::Seq`]
/// chain. That moves the selected expression after the tuple's suffix,
/// so either the selected expression or the entire suffix must be pure.
/// Recovery and single-use substitution can both leave effects in tuple members.
///
/// Do not apply this rewrite to [`Expr::EnrichedFieldGet`]. A
/// field-get is not only positional selection: it represents a nominal
/// projector folded over that selection. Replacing it with the tuple slot
/// would return the nominal value and drop the projector.
///
/// A final slot covering several tuple items keeps that suffix as a tuple,
/// with its type taken from the original tuple's explicit right spine.
/// Discarded prefix effects run before the retained suffix in their original
/// order. Other differing layouts keep the projection for ordinary routing.
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
        } => {
            let suffix_ty = match target.as_ref() {
                Expr::EnrichedTuple {
                    items, synth_ty, ..
                } if arity >= 2 && index.checked_add(1) == Some(arity) && arity < items.len() => (0
                    ..index)
                    .try_fold(synth_ty, |ty, _| match ty {
                        Type::Product { right, .. } => Some(right.as_ref()),
                        _ => None,
                    })
                    .cloned(),
                _ => None,
            };
            match *target {
                Expr::EnrichedTuple { mut items, .. } if suffix_ty.is_some() => {
                    let tail = items.split_off(index);
                    *changed = true;
                    sequence_effects(
                        items,
                        Expr::EnrichedTuple {
                            occurrence: Default::default(),
                            items: tail,
                            synth_ty: suffix_ty.expect("explicit suffix type checked above"),
                            meta,
                            ext,
                        },
                    )
                }
                Expr::EnrichedTuple { items, .. }
                    if items.len() == arity
                        && index < arity
                        && can_pick_with_leading_effects(&items, index) =>
                {
                    *changed = true;
                    pick_with_leading_effects(items, index)
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
            }
        }
        e @ Expr::EnrichedFieldGet { .. } => e,
        e => e,
    }
}

fn can_pick_with_leading_effects(items: &[Expr<Enriched>], index: usize) -> bool {
    is_pure(&items[index]) || items[index + 1..].iter().all(is_pure)
}

/// The selected expression runs last, so the caller must prove it can
/// cross the suffix without changing observable evaluation order.
fn pick_with_leading_effects(mut items: Vec<Expr<Enriched>>, index: usize) -> Expr<Enriched> {
    debug_assert!(can_pick_with_leading_effects(&items, index));
    let picked = items.remove(index);
    sequence_effects(items, picked)
}

// ---- pass: Compose Recovered Tail Access ------------------------

/// Compose an access through a bounded projection's final slot, retaining
/// the original product type and evaluating its target once. For an inner
/// final slot `i` and outer view `(j, b)`, the composed view is `(i + j, i + b)`.
/// Nonterminal slots may be compound left components and cannot be combined.
///
/// The same rule also recognizes an ordinary `let`-bound tuple consisting
/// exactly of the original product's suffix projections. Field gets retain
/// their nominal unwrap while only their positional target is composed.
/// Following the whole chain avoids one optimizer iteration per tail step.
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
        target = match target {
            Expr::Let { value, .. } => *value,
            Expr::EnrichedProject { target, .. } => *target,
            _ => {
                unreachable!("tail composition accepts a retained projection or bound suffix tuple")
            }
        };
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
    if let Expr::EnrichedProject {
        index: tail_index,
        arity: tail_arity,
        target_ty,
        ..
    } = target
    {
        if arity < 2
            || index >= arity
            || *tail_arity < 2
            || tail_index.checked_add(1)? != *tail_arity
        {
            return None;
        }
        return Some((
            tail_index.checked_add(index)?,
            tail_index.checked_add(arity)?,
            target_ty.clone(),
        ));
    }
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

/// An ordered reconstruction of a bound product is the original value,
/// including when its initializer has effects. The final projection retains
/// the complete right suffix; nominal field getters are not projections here.
fn let_tuple_reconstruction(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let identity = (|| {
        let Expr::Let {
            name,
            ty,
            value,
            body,
            ..
        } = &e
        else {
            return None;
        };
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = body.as_ref()
        else {
            return None;
        };
        let source_ty = match value.as_ref() {
            Expr::EnrichedTuple { synth_ty, .. } => synth_ty,
            _ => ty.as_ref()?,
        };
        if !structural_type_equal(source_ty, synth_ty) {
            return None;
        }
        let mut max_arity = 1;
        let mut tail = source_ty;
        while let Type::Product { right, .. } = tail {
            max_arity += 1;
            tail = right;
        }
        if items.len() < 2 || items.len() > max_arity {
            return None;
        }
        for (position, item) in items.iter().enumerate() {
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
            if !bound_tuple_name(target, name)
                || *index != position
                || *arity < 2
                || *arity > max_arity
                || !structural_type_equal(source_ty, target_ty)
                || (position + 1 == items.len() && *arity != items.len())
                || (position + 1 < items.len() && *arity <= position + 1)
            {
                return None;
            }
        }
        Some(())
    })()
    .is_some();
    if !identity {
        return e;
    }
    let Expr::Let { value, .. } = e else {
        unreachable!("reconstruction requires a let")
    };
    *changed = true;
    *value
}

fn explicit_tuple_slots(ty: &Type<Enriched>, arity: usize) -> Option<Vec<&Type<Enriched>>> {
    if arity < 2 {
        return None;
    }
    let mut slots = Vec::with_capacity(arity);
    let mut tail = ty;
    for _ in 1..arity {
        let Type::Product { left, right, .. } = tail else {
            return None;
        };
        slots.push(left.as_ref());
        tail = right;
    }
    slots.push(tail);
    Some(slots)
}

fn bound_tuple_name(e: &Expr<Enriched>, name: &str) -> bool {
    matches!(e, Expr::Path { segments, .. }
        if segments.len() == 1 && segments[0].name == name)
}

/// Visit only complete slot projections. A bare packet use, a suffix with a
/// different layout, or a use deferred beneath a function keeps the let intact.
/// The stack walks disjoint children and never rebuilds a tuple at each use.
fn visit_bound_tuple_slots(
    body: &mut Expr<Enriched>,
    name: &str,
    tuple_ty: &Type<Enriched>,
    arity: usize,
    mut visit: impl FnMut(usize, &mut Expr<Enriched>) -> bool,
) -> bool {
    let mut pending = vec![body];
    while let Some(e) = pending.pop() {
        if let Expr::EnrichedProject {
            target,
            index,
            arity: projected_arity,
            target_ty,
            ..
        } = e
            && bound_tuple_name(target, name)
        {
            let index = *index;
            if *projected_arity != arity
                || index >= arity
                || !structural_type_equal(target_ty, tuple_ty)
                || !visit(index, e)
            {
                return false;
            }
            continue;
        }
        match e {
            Expr::Path { .. } if bound_tuple_name(e, name) => return false,
            Expr::FnExpr { .. } => {
                if count_free_uses(e, name) != 0 {
                    return false;
                }
            }
            Expr::Call { callee, args, .. } => {
                pending.push(callee.as_mut());
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        pending.push(value);
                    }
                }
            }
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                pending.push(value.as_mut());
                pending.push(body.as_mut());
            }
            Expr::EnrichedTuple { items, .. } => pending.extend(items.iter_mut()),
            Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
                pending.push(target.as_mut());
            }
            Expr::EnrichedInject { payload, .. } => pending.push(payload.as_mut()),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => {
                pending.push(scrutinee.as_mut());
                pending.extend(arms.iter_mut().map(|arm| &mut arm.body));
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                pending.push(cond.as_mut());
                pending.push(then_branch.as_mut());
                pending.push(else_branch.as_mut());
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => {}
            _ => return false,
        }
    }
    true
}

/// Keep explicit host-literal construction out of propagation. Function values
/// defer their bodies; a syntactically invoked function does not. Computed
/// callees decline rather than requiring analysis of returned functions.
fn tuple_slots_preserve_literal_boundary(items: &[Expr<Enriched>]) -> bool {
    let mut pending = items.iter().collect::<Vec<_>>();
    while let Some(e) = pending.pop() {
        match e {
            Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => return false,
            Expr::Path { .. } | Expr::Unit { .. } | Expr::FnExpr { .. } => {}
            Expr::Call { callee, args, .. } => {
                match callee.as_ref() {
                    Expr::Path { .. } => {}
                    Expr::FnExpr { body, .. } => pending.push(body.as_ref()),
                    _ => return false,
                }
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        pending.push(value);
                    }
                }
            }
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                pending.push(value.as_ref());
                pending.push(body.as_ref());
            }
            Expr::EnrichedTuple { items, .. } => pending.extend(items),
            Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
                pending.push(target.as_ref())
            }
            Expr::EnrichedInject { payload, .. } => pending.push(payload.as_ref()),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => {
                pending.push(scrutinee.as_ref());
                pending.extend(arms.iter().map(|arm| &arm.body));
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                pending.push(cond.as_ref());
                pending.push(then_branch.as_ref());
                pending.push(else_branch.as_ref());
            }
            _ => return false,
        }
    }
    true
}

fn movable_tuple_slot(e: &Expr<Enriched>) -> bool {
    match e {
        Expr::Path { .. } | Expr::Unit { .. } => true,
        Expr::FnExpr { sig, .. } => sig
            .params
            .iter()
            .all(|param| matches!(param, crate::ast::SignatureParam::Value(_))),
        _ => false,
    }
}

/// Propagate inert tuple slots without duplicating their syntax. At most one
/// computed or repeatedly used slot keeps the original binding and eager
/// evaluation point. Other computations, escapes, or capture risks are no-ops.
fn propagate_bound_tuple_slots(mut e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let plan = (|| {
        let Expr::Let {
            name, value, body, ..
        } = &mut e
        else {
            return None;
        };
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = value.as_ref()
        else {
            return None;
        };
        if !tuple_slots_preserve_literal_boundary(items) {
            return None;
        }
        let slot_types = explicit_tuple_slots(synth_ty, items.len())?;
        let binders = body_binders(body);
        if binders.contains(name.as_str()) {
            return None;
        }
        let mut forbidden = binders
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>();
        forbidden.insert(name.clone());
        let mut uses = vec![0usize; items.len()];
        if !visit_bound_tuple_slots(body, name, synth_ty, items.len(), |index, _| {
            uses[index] = (uses[index] + 1).min(2);
            true
        }) {
            return None;
        }
        let mut retained = None;
        for (index, item) in items.iter().enumerate() {
            let keep = !movable_tuple_slot(item)
                || (uses[index] > 1 && !matches!(item, Expr::Unit { .. }));
            if keep {
                if retained.replace(index).is_some() {
                    return None;
                }
            } else if uses[index] != 0
                && free_vars(item).iter().any(|name| forbidden.contains(name))
            {
                return None;
            }
        }
        Some(retained.map(|index| (index, (*slot_types[index]).clone())))
    })();
    let Some(retained) = plan else { return e };
    let Expr::Let {
        name,
        name_span,
        pattern,
        value,
        mut body,
        meta,
        ..
    } = e
    else {
        unreachable!("slot propagation requires a let")
    };
    let Expr::EnrichedTuple {
        items, synth_ty, ..
    } = *value
    else {
        unreachable!("slot propagation requires a tuple")
    };
    let arity = items.len();
    let mut items = items.into_iter().map(Some).collect::<Vec<_>>();
    let complete = visit_bound_tuple_slots(&mut body, &name, &synth_ty, arity, |index, slot| {
        if retained.as_ref().is_some_and(|(kept, _)| *kept == index) {
            let projection = std::mem::replace(
                slot,
                Expr::Unit {
                    source_trivia: Default::default(),
                    occurrence: Default::default(),
                    meta: crate::ast::Meta::new(name_span),
                },
            );
            let Expr::EnrichedProject { target, .. } = projection else {
                unreachable!("visitor supplies a projection")
            };
            *slot = *target;
        } else if matches!(items[index], Some(Expr::Unit { .. })) {
            *slot = items[index]
                .as_ref()
                .expect("unit slot retained for repeated use")
                .clone();
        } else {
            *slot = items[index]
                .take()
                .expect("non-unit movable slot has one use");
        }
        true
    });
    assert!(complete, "validated tuple slot plan must remain applicable");
    *changed = true;
    match retained {
        Some((index, ty)) => Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty: Some(ty),
            pattern,
            value: Box::new(
                items[index]
                    .take()
                    .expect("retained slot was not substituted"),
            ),
            body,
            meta,
        },
        None => *body,
    }
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
                source_trivia: _,
                occurrence: _,
                diagnostic: _,
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
                source_trivia: Default::default(),
                occurrence: Default::default(),
                diagnostic: Default::default(),
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

/// Recover a conditional whose condition and literal thunks were first
/// evaluated together in a let-bound tuple. Only pure non-condition slots may
/// be discarded; the condition itself stays at the conditional entry.
fn recover_bound_conditional(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    fn branch_index(
        branch: &Expr<Enriched>,
        name: &str,
        items: &[Expr<Enriched>],
    ) -> Option<usize> {
        let Expr::Call { callee, args, .. } = branch else {
            return None;
        };
        if !matches!(args.as_slice(), [] | [CallArg::Value(Expr::Unit { .. })]) {
            return None;
        }
        let (index, arity, _) = projection_from_bound(callee, name)?;
        if arity != items.len() {
            return None;
        }
        let Expr::FnExpr { sig, .. } = items.get(index)? else {
            return None;
        };
        if !sig.params.is_empty()
            || !matches!(
                sig.groups.as_slice(),
                [crate::ast::SignatureGroupKind::Value { len: 0 }]
            )
        {
            return None;
        }
        Some(index)
    }

    let indices = (|| {
        let Expr::Let {
            name, value, body, ..
        } = &e
        else {
            return None;
        };
        let Expr::EnrichedTuple { items, .. } = value.as_ref() else {
            return None;
        };
        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = body.as_ref()
        else {
            return None;
        };
        let (condition, arity, _) = projection_from_bound(cond, name)?;
        if arity != items.len() || condition >= items.len() {
            return None;
        }
        let then = branch_index(then_branch, name, items)?;
        let otherwise = branch_index(else_branch, name, items)?;
        if condition == then
            || condition == otherwise
            || then == otherwise
            || items
                .iter()
                .enumerate()
                .any(|(index, item)| index != condition && !is_pure(item))
        {
            return None;
        }
        Some((condition, then, otherwise))
    })();
    let Some((condition_index, then_index, else_index)) = indices else {
        return e;
    };
    let Expr::Let { value, body, .. } = e else {
        unreachable!("guard requires a let")
    };
    let Expr::EnrichedTuple { items, .. } = *value else {
        unreachable!("guard requires a tuple")
    };
    let Expr::EnrichedConditional {
        mut then_branch,
        mut else_branch,
        result_ty,
        meta,
        ext,
        ..
    } = *body
    else {
        unreachable!("guard requires a conditional")
    };
    let (mut condition, mut then_thunk, mut else_thunk) = (None, None, None);
    for (index, item) in items.into_iter().enumerate() {
        if index == condition_index {
            condition = Some(item);
        } else if index == then_index {
            then_thunk = Some(item);
        } else if index == else_index {
            else_thunk = Some(item);
        }
    }
    let Expr::Call {
        callee: then_callee,
        ..
    } = then_branch.as_mut()
    else {
        unreachable!("guard requires a then call")
    };
    let Expr::Call {
        callee: else_callee,
        ..
    } = else_branch.as_mut()
    else {
        unreachable!("guard requires an else call")
    };
    **then_callee = then_thunk.expect("guard checks then slot");
    **else_callee = else_thunk.expect("guard checks else slot");
    *changed = true;
    Expr::EnrichedConditional {
        occurrence: Default::default(),
        cond: Box::new(condition.expect("guard checks condition slot")),
        then_branch,
        else_branch,
        result_ty,
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
        Expr::Let { name: _, value, .. }
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
        Expr::Tuple { ext, .. } | Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserMacro { ext, .. }
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
            source_trivia: _,
            occurrence: _,
            diagnostic: _,
            callee,
            args,
            meta,
            ext: _,
        } => Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
        Expr::Tuple { ext, .. } | Expr::FnPlaceholder { ext, .. } => match ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } | Expr::Ufcs { ext, .. } => {
            match ext {}
        }
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
fn let_fuse(ctx: &OptimizerCtx, e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
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
            if ctx.evaluation_is_pure(&value) {
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
            if !would_capture
                && (ctx.evaluation_is_pure(&value) || use_precedes_effects(&body, &name))
            {
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
        source_trivia: _,
        occurrence: _,
        diagnostic: _,
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
                && matches!(
                    sig.groups.as_slice(),
                    [crate::ast::SignatureGroupKind::Value { len: 1 }]
                )
        }
        _ => false,
    };
    let safe_unary_arg = matches!(args.as_slice(), [CallArg::Value(_)]);
    if !safe_unary_fn || !safe_unary_arg {
        return Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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

/// A literal with exactly one empty value stage binds no variables. Entering
/// it preserves an explicit Unit argument's evaluation before the body; no
/// substitution or callable-stage construction is needed.
fn beta_reduce_nullary_iife(e: Expr<Enriched>, changed: &mut bool) -> Expr<Enriched> {
    let eligible = matches!(&e,
        Expr::Call { callee, args, .. }
            if matches!(callee.as_ref(), Expr::FnExpr { sig, .. }
                if sig.params.is_empty()
                    && matches!(sig.groups.as_slice(), [crate::ast::SignatureGroupKind::Value { len: 0 }]))
                && matches!(args.as_slice(), [] | [CallArg::Value(_)])
    );
    if !eligible {
        return e;
    }
    let Expr::Call {
        callee, args, meta, ..
    } = e
    else {
        unreachable!("eligible nullary beta reduction requires a call")
    };
    let Expr::FnExpr { body, .. } = *callee else {
        unreachable!("eligible nullary beta reduction requires a literal")
    };
    *changed = true;
    match args.into_iter().next() {
        None => *body,
        Some(CallArg::Value(value)) => Expr::Seq {
            occurrence: Default::default(),
            value: Box::new(value),
            body,
            meta,
        },
        Some(CallArg::Type(_)) => unreachable!("a nullary value stage has no type argument"),
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
        Expr::Tuple { ext, .. } | Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserMacro { ext, .. }
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
        Expr::Tuple { ext, .. } | Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserMacro { ext, .. }
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
/// rule covers the [`Expr::Seq`] expression-statement form using the
/// same conservative expression and callable proof.
fn dead_code_elimination(
    ctx: &OptimizerCtx,
    e: Expr<Enriched>,
    changed: &mut bool,
) -> Expr<Enriched> {
    match e {
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => {
            if ctx.evaluation_is_pure(&value) {
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
        source_trivia: _,
        occurrence: _,
        diagnostic: _,
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    if matches!(outer_kind, NewtypeMemberKind::Neither) {
        return Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    let Some(inner_info) = ctx.newtype_members.get(inner.visible_head) else {
        return Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    };
    if &inner_info.identity != outer_identity {
        return Expr::Call {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
            callee,
            args,
            meta,
            ext: (),
        };
    }
    for (a, b) in outer_type_args.iter().zip(inner.type_args.iter()) {
        if !types_equal(a, b) {
            return Expr::Call {
                source_trivia: Default::default(),
                occurrence: Default::default(),
                diagnostic: Default::default(),
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
/// Literal roles do not specify the host's arithmetic or string-operation
/// behavior, so those host operations remain opaque rather than being
/// evaluated from the literal syntax alone.
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
        Expr::Tuple { ext, .. } | Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserMacro { ext, .. }
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

    fn macro_support_entries() -> Vec<(PathBuf, crate::ast::Module<crate::ast::Surface>)> {
        [
            (
                "testapi.kio",
                include_str!("../../../test-data/poc/macros/workdir/testapi.kio"),
            ),
            (
                "macro_util.kio",
                include_str!("../../../test-data/poc/macros/workdir/macro_util.kio"),
            ),
            (
                "spine_macros.kio",
                include_str!("../../../test-data/poc/macros/workdir/spine_macros.kio"),
            ),
            (
                "match.kio",
                include_str!("../../../test-data/poc/macros/workdir/match.kio"),
            ),
            (
                "control.kio",
                include_str!("../../../test-data/poc/macros/workdir/control.kio"),
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
        parsed_modules.extend(macro_support_entries());
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
        parsed_modules.extend(macro_support_entries());
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
                source_trivia: Default::default(),
                occurrence: Default::default(),
                diagnostic: Default::default(),
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

    fn packet_product(types: Vec<Type<Enriched>>) -> Type<Enriched> {
        let mut types = types.into_iter().rev();
        let tail = types.next().expect("nonempty product");
        types.fold(tail, |right, left| Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: test_meta(),
        })
    }

    fn packet_projection(ty: &Type<Enriched>, index: usize, arity: usize) -> Expr<Enriched> {
        Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(test_path("packet")),
            index,
            arity,
            target_ty: ty.clone(),
            meta: test_meta(),
            ext: (),
        }
    }

    fn packet_fixture(
        items: Vec<(Expr<Enriched>, Type<Enriched>)>,
        body: impl FnOnce(&Type<Enriched>) -> Expr<Enriched>,
    ) -> Expr<Enriched> {
        let (items, types): (Vec<_>, Vec<_>) = items.into_iter().unzip();
        let ty = packet_product(types);
        let body = body(&ty);
        Expr::Let {
            occurrence: Default::default(),
            name: "packet".into(),
            name_span: Span::new(0, 0),
            ty: Some(ty.clone()),
            pattern: (),
            value: Box::new(Expr::EnrichedTuple {
                occurrence: Default::default(),
                items,
                synth_ty: ty,
                meta: test_meta(),
                ext: (),
            }),
            body: Box::new(body),
            meta: test_meta(),
        }
    }

    fn packet_unit() -> Expr<Enriched> {
        Expr::Unit {
            source_trivia: Default::default(),
            occurrence: Default::default(),
            meta: test_meta(),
        }
    }

    fn packet_call(name: &str, args: Vec<Expr<Enriched>>) -> Expr<Enriched> {
        Expr::synth_call(
            test_path(name),
            args.into_iter().map(CallArg::Value).collect(),
            Span::new(0, 0),
        )
    }

    fn packet_lambda(body: Expr<Enriched>) -> Expr<Enriched> {
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::new(vec![crate::ast::SignatureParam::Value(
                crate::ast::Param {
                    name: "payload".into(),
                    ty: Some(test_unit_ty()),
                    pattern: (),
                    meta: test_meta(),
                },
            )]),
            ret_ty: Some(test_unit_ty()),
            body: Box::new(body),
            meta: test_meta(),
            caps: (),
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum PacketTestValue {
        Unit,
        Pair(Box<Self>, Box<Self>),
    }

    // Independent strict evaluator for the unit-valued effect witnesses below.
    // An ordinary call evaluates every argument before recording its effect.
    fn packet_evaluate(
        e: &Expr<Enriched>,
        env: &mut std::collections::HashMap<String, PacketTestValue>,
        events: &mut Vec<String>,
    ) -> PacketTestValue {
        match e {
            Expr::Unit { .. } => PacketTestValue::Unit,
            Expr::Path { segments, .. } => env[&segments[0].name].clone(),
            Expr::Call { callee, args, .. } => {
                for arg in args {
                    if let CallArg::Value(value) = arg {
                        packet_evaluate(value, env, events);
                    }
                }
                let Expr::Path { segments, .. } = callee.as_ref() else {
                    panic!("named test effect")
                };
                events.push(segments[0].name.clone());
                PacketTestValue::Unit
            }
            Expr::Let {
                name, value, body, ..
            } => {
                let value = packet_evaluate(value, env, events);
                let old = env.insert(name.clone(), value);
                let result = packet_evaluate(body, env, events);
                if let Some(old) = old {
                    env.insert(name.clone(), old);
                } else {
                    env.remove(name);
                }
                result
            }
            Expr::Seq { value, body, .. } => {
                packet_evaluate(value, env, events);
                packet_evaluate(body, env, events)
            }
            Expr::EnrichedTuple { items, .. } => {
                let values = items
                    .iter()
                    .map(|item| packet_evaluate(item, env, events))
                    .collect::<Vec<_>>();
                let mut values = values.into_iter().rev();
                let last = values.next().expect("nonempty test tuple");
                values.fold(last, |right, left| {
                    PacketTestValue::Pair(Box::new(left), Box::new(right))
                })
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => {
                let mut value = packet_evaluate(target, env, events);
                for _ in 0..*index {
                    let PacketTestValue::Pair(_, right) = value else {
                        panic!("product spine")
                    };
                    value = *right;
                }
                if index + 1 == *arity {
                    value
                } else {
                    let PacketTestValue::Pair(left, _) = value else {
                        panic!("product slot")
                    };
                    *left
                }
            }
            other => panic!("unsupported test expression: {other:?}"),
        }
    }

    #[test]
    fn bound_tuple_identity_keeps_effects_and_complete_suffix() {
        let input = packet_fixture(
            ["first", "second", "third"]
                .into_iter()
                .map(|name| (packet_call(name, vec![]), test_unit_ty()))
                .collect(),
            |ty| Expr::EnrichedTuple {
                occurrence: Default::default(),
                items: vec![packet_projection(ty, 0, 3), packet_projection(ty, 1, 2)],
                synth_ty: ty.clone(),
                meta: test_meta(),
                ext: (),
            },
        );
        let mut changed = false;
        let output = let_tuple_reconstruction(input.clone(), &mut changed);
        assert!(changed, "complete head-plus-suffix identity must fire");
        assert!(!contains(&output, &|e| matches!(
            e,
            Expr::Let { .. } | Expr::EnrichedProject { .. }
        )));
        let (mut before, mut after) = (Vec::new(), Vec::new());
        assert_eq!(
            packet_evaluate(&input, &mut Default::default(), &mut before),
            packet_evaluate(&output, &mut Default::default(), &mut after)
        );
        assert_eq!(before, ["first", "second", "third"]);
        assert_eq!(after, before);
        for indices in [[1, 0], [0, 0]] {
            let wrong = packet_fixture(vec![(packet_unit(), test_unit_ty()); 2], |ty| {
                Expr::EnrichedTuple {
                    occurrence: Default::default(),
                    items: indices
                        .into_iter()
                        .map(|i| packet_projection(ty, i, 2))
                        .collect(),
                    synth_ty: ty.clone(),
                    meta: test_meta(),
                    ext: (),
                }
            });
            let mut changed = false;
            assert_eq!(let_tuple_reconstruction(wrong.clone(), &mut changed), wrong);
            assert!(!changed, "permuted or duplicate projection is not identity");
        }
    }

    #[test]
    fn bound_tuple_slots_move_literal_handler_without_cloning() {
        let handler = packet_lambda(packet_call("chosen", vec![test_path("payload")]));
        let handler_ty =
            Type::synth_function(vec![test_unit_ty()], test_unit_ty(), Span::new(0, 0));
        let input = packet_fixture(
            vec![
                (packet_unit(), test_unit_ty()),
                (handler.clone(), handler_ty),
            ],
            |ty| {
                Expr::synth_call(
                    packet_projection(ty, 1, 2),
                    vec![CallArg::Value(packet_projection(ty, 0, 2))],
                    Span::new(0, 0),
                )
            },
        );
        let nodes = ir_node_count(&input);
        let mut changed = false;
        let output = propagate_bound_tuple_slots(input, &mut changed);
        assert!(changed);
        let Expr::Call { callee, args, .. } = &output else {
            panic!("ordinary call remains")
        };
        assert_eq!(callee.as_ref(), &handler);
        assert!(matches!(
            args.as_slice(),
            [CallArg::Value(Expr::Unit { .. })]
        ));
        assert!(ir_node_count(&output) < nodes);
        assert!(!contains(&output, &|e| matches!(
            e,
            Expr::Let { .. } | Expr::EnrichedProject { .. } | Expr::EnrichedTuple { .. }
        )));
    }

    #[test]
    fn repeated_literal_handler_keeps_one_existing_binding() {
        let handler = packet_lambda(packet_call("chosen", vec![test_path("payload")]));
        let handler_ty =
            Type::synth_function(vec![test_unit_ty()], test_unit_ty(), Span::new(0, 0));
        let input = packet_fixture(
            vec![
                (packet_unit(), test_unit_ty()),
                (handler.clone(), handler_ty.clone()),
            ],
            |ty| {
                packet_call(
                    "consume",
                    vec![packet_projection(ty, 1, 2), packet_projection(ty, 1, 2)],
                )
            },
        );
        let before_nodes = ir_node_count(&input);
        let mut changed = false;
        let output = propagate_bound_tuple_slots(input, &mut changed);
        assert!(changed);
        let Expr::Let {
            name,
            ty,
            value,
            body,
            ..
        } = &output
        else {
            panic!("shared handler binding")
        };
        assert_eq!(name, "packet");
        assert!(structural_type_equal(ty.as_ref().unwrap(), &handler_ty));
        assert_eq!(value.as_ref(), &handler);
        assert_eq!(count_free_uses(body, "packet"), 2);
        assert!(!contains(body, &|e| matches!(
            e,
            Expr::FnExpr { .. } | Expr::EnrichedProject { .. }
        )));
        assert!(ir_node_count(&output) < before_nodes);
    }

    #[test]
    fn bound_tuple_slots_retain_one_factory_once_even_unused() {
        for factory_index in [0, 1] {
            for uses in [0, 1, 2] {
                let items = (0..2)
                    .map(|index| {
                        (
                            if index == factory_index {
                                packet_call("factory", vec![])
                            } else {
                                packet_unit()
                            },
                            test_unit_ty(),
                        )
                    })
                    .collect();
                let input = packet_fixture(items, |ty| {
                    packet_call(
                        "consume",
                        (0..uses)
                            .map(|_| packet_projection(ty, factory_index, 2))
                            .collect(),
                    )
                });
                let mut changed = false;
                let output = propagate_bound_tuple_slots(input.clone(), &mut changed);
                assert!(changed);
                let Expr::Let {
                    name, ty, value, ..
                } = &output
                else {
                    panic!("factory remains staged")
                };
                assert_eq!(name, "packet");
                assert!(
                    matches!(ty, Some(Type::Unit { .. })),
                    "retained binding gets leaf type"
                );
                assert_eq!(value.as_ref(), &packet_call("factory", vec![]));
                let (mut before, mut after) = (Vec::new(), Vec::new());
                assert_eq!(
                    packet_evaluate(&input, &mut Default::default(), &mut before),
                    packet_evaluate(&output, &mut Default::default(), &mut after)
                );
                assert_eq!(before, ["factory", "consume"]);
                assert_eq!(after, before);
            }
        }
        let input = packet_fixture(
            vec![
                (packet_call("first", vec![]), test_unit_ty()),
                (packet_call("second", vec![]), test_unit_ty()),
            ],
            |ty| {
                packet_call(
                    "consume",
                    vec![packet_projection(ty, 1, 2), packet_projection(ty, 0, 2)],
                )
            },
        );
        let mut changed = false;
        let output = propagate_bound_tuple_slots(input.clone(), &mut changed);
        assert!(!changed, "two factories stay in their ordered packet");
        assert_eq!(output, input);
        let mut events = Vec::new();
        packet_evaluate(&output, &mut Default::default(), &mut events);
        assert_eq!(events, ["first", "second", "consume"]);
    }

    #[test]
    fn bound_tuple_slots_decline_literal_conversion_boundaries() {
        let carrier = Type::synth_path(vec!["Carrier".into()], vec![], Span::new(0, 0));
        let literals = vec![
            Expr::StrLit {
                occurrence: Default::default(),
                value: "text".into(),
                annotation: carrier.clone(),
                meta: test_meta(),
            },
            Expr::IntLit {
                occurrence: Default::default(),
                diagnostic: Default::default(),
                digits: "1".into(),
                annotation: carrier.clone(),
                meta: test_meta(),
            },
            Expr::FloatLit {
                occurrence: Default::default(),
                digits: "1.0".into(),
                annotation: carrier.clone(),
                meta: test_meta(),
            },
            Expr::BoolLit {
                occurrence: Default::default(),
                value: true,
                annotation: carrier.clone(),
                meta: test_meta(),
            },
        ];
        for literal in literals {
            let input = packet_fixture(
                vec![
                    (literal.clone(), carrier.clone()),
                    (packet_unit(), test_unit_ty()),
                ],
                |_| packet_unit(),
            );
            let mut changed = false;
            let output = propagate_bound_tuple_slots(input.clone(), &mut changed);
            assert!(!changed, "literal slot keeps its whole original packet");
            assert_eq!(output, input);
            let input = packet_fixture(vec![(literal.clone(), carrier.clone()); 2], |_| {
                packet_unit()
            });
            let mut changed = false;
            assert_eq!(
                propagate_bound_tuple_slots(input.clone(), &mut changed),
                input
            );
            assert!(
                !changed,
                "two literal conversions retain their original packet"
            );
            let nested_ty = packet_product(vec![carrier.clone(), test_unit_ty()]);
            let nested = Expr::EnrichedTuple {
                occurrence: Default::default(),
                items: vec![literal.clone(), packet_unit()],
                synth_ty: nested_ty.clone(),
                meta: test_meta(),
                ext: (),
            };
            let input = packet_fixture(
                vec![(nested, nested_ty), (packet_unit(), test_unit_ty())],
                |_| packet_unit(),
            );
            let mut changed = false;
            assert_eq!(
                propagate_bound_tuple_slots(input.clone(), &mut changed),
                input
            );
            assert!(!changed, "eager nested conversion keeps its packet");

            let mut deferred = packet_lambda(literal.clone());
            let Expr::FnExpr { ret_ty, .. } = &mut deferred else {
                unreachable!()
            };
            *ret_ty = Some(carrier.clone());
            let deferred_ty =
                Type::synth_function(vec![test_unit_ty()], carrier.clone(), Span::new(0, 0));
            let input = packet_fixture(
                vec![
                    (deferred.clone(), deferred_ty),
                    (packet_unit(), test_unit_ty()),
                ],
                |ty| packet_projection(ty, 0, 2),
            );
            let mut changed = false;
            assert_eq!(propagate_bound_tuple_slots(input, &mut changed), deferred);
            assert!(
                changed,
                "a literal inside a deferred function is not evaluated at packet construction"
            );

            let invoked = Expr::synth_call(
                deferred,
                vec![CallArg::Value(packet_unit())],
                Span::new(0, 0),
            );
            let input = packet_fixture(
                vec![(invoked, carrier.clone()), (packet_unit(), test_unit_ty())],
                |_| packet_unit(),
            );
            let mut changed = false;
            assert_eq!(
                propagate_bound_tuple_slots(input.clone(), &mut changed),
                input
            );
            assert!(
                !changed,
                "a syntactically invoked literal-function body is inspected"
            );
        }
    }

    #[test]
    fn bound_tuple_slot_propagation_declines_capture_escapes_and_suffixes() {
        let make = |captured: &str, body: Expr<Enriched>| {
            packet_fixture(
                vec![
                    (test_path(captured), test_unit_ty()),
                    (packet_unit(), test_unit_ty()),
                ],
                |_| body,
            )
        };
        let shadow = |name: &str, body: Expr<Enriched>| Expr::Let {
            occurrence: Default::default(),
            name: name.into(),
            name_span: Span::new(0, 0),
            ty: Some(test_unit_ty()),
            pattern: (),
            value: Box::new(packet_unit()),
            body: Box::new(body),
            meta: test_meta(),
        };
        let projection = || packet_projection(&test_pair_ty(), 0, 2);
        let mut polymorphic = packet_lambda(projection());
        let Expr::FnExpr { sig, .. } = &mut polymorphic else {
            unreachable!()
        };
        let mut params = vec![crate::ast::SignatureParam::Type(crate::ast::TypeParam {
            name: "T".into(),
            span: Span::new(0, 0),
            kind: None,
            presentation: Default::default(),
        })];
        params.extend(std::mem::take(&mut sig.params));
        *sig = crate::ast::Signature::new(params);
        let cases = vec![
            make("outer", test_path("packet")),
            make("outer", shadow("packet", projection())),
            make("outer", shadow("outer", projection())),
            make("packet", projection()),
            packet_fixture(
                vec![
                    (test_path("packet"), test_unit_ty()),
                    (packet_call("factory", vec![]), test_unit_ty()),
                ],
                |ty| {
                    packet_call(
                        "consume",
                        vec![packet_projection(ty, 0, 2), packet_projection(ty, 1, 2)],
                    )
                },
            ),
            make("outer", packet_lambda(projection())),
            make("outer", polymorphic),
            make(
                "outer",
                Expr::EnrichedFieldGet {
                    occurrence: Default::default(),
                    target: Box::new(test_path("packet")),
                    field_name: "Nominal".into(),
                    index: 0,
                    arity: 2,
                    target_ty: test_pair_ty(),
                    meta: test_meta(),
                    ext: (),
                },
            ),
            make(
                "outer",
                Expr::EnrichedMatch {
                    occurrence: Default::default(),
                    scrutinee: Box::new(packet_unit()),
                    arms: vec![crate::ast::EnrichedArm {
                        param: "outer".into(),
                        body: projection(),
                        meta: test_meta(),
                    }],
                    scrutinee_ty: test_unit_ty(),
                    result_ty: test_unit_ty(),
                    meta: test_meta(),
                    ext: (),
                },
            ),
            packet_fixture(vec![(packet_unit(), test_unit_ty()); 3], |ty| {
                packet_projection(ty, 1, 2)
            }),
            make(
                "outer",
                packet_projection(&Type::Bottom { meta: test_meta() }, 0, 2),
            ),
        ];
        for input in cases {
            let mut changed = false;
            assert_eq!(
                propagate_bound_tuple_slots(input.clone(), &mut changed),
                input
            );
            assert!(
                !changed,
                "unsupported binding or layout must remain unchanged"
            );
        }
    }

    #[test]
    fn bound_tuple_slot_growth_is_bounded_for_wide_and_nested_candidates() {
        for width in [2, 8, 32, 128] {
            let input = packet_fixture(vec![(packet_unit(), test_unit_ty()); width], |ty| {
                packet_call(
                    "consume",
                    (0..width)
                        .map(|i| packet_projection(ty, i, width))
                        .collect(),
                )
            });
            let before_nodes = ir_node_count(&input);
            let mut changed = false;
            let output = propagate_bound_tuple_slots(input, &mut changed);
            assert!(changed);
            assert!(ir_node_count(&output) < before_nodes);
            let mut changed = false;
            let again = propagate_bound_tuple_slots(output.clone(), &mut changed);
            assert!(!changed);
            assert_eq!(again, output);
        }
        let mut input = test_path("packet");
        for _ in 0..64 {
            input = packet_fixture(vec![(packet_unit(), test_unit_ty()); 2], |_| input);
        }
        let before_nodes = ir_node_count(&input);
        let mut output = input.clone();
        for _ in 0..FIXPOINT_CAP {
            let mut changed = false;
            output = propagate_bound_tuple_slots(output, &mut changed);
            assert!(!changed, "shadowed nested packet must remain a no-op");
        }
        assert_eq!(output, input);
        assert_eq!(ir_node_count(&output), before_nodes);
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
             fn t() -> . { __fst__[.][ .]( __pair__[.][ .]( (), ())) }",
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

    #[test]
    fn project_after_tuple_preserves_each_effectful_position() {
        fn call_names(e: &Expr<Enriched>, names: &mut Vec<String>) {
            match e {
                Expr::Call { callee, args, .. } => {
                    assert!(matches!(
                        args.as_slice(),
                        [CallArg::Value(Expr::Unit { .. })]
                    ));
                    let Expr::Path { segments, .. } = callee.as_ref() else {
                        panic!("expected a named test call: {e:?}")
                    };
                    assert_eq!(segments.len(), 1);
                    names.push(segments[0].name.clone());
                }
                Expr::Seq { value, body, .. } => {
                    call_names(value, names);
                    call_names(body, names);
                }
                Expr::EnrichedProject { target, .. } => call_names(target, names),
                Expr::EnrichedTuple { items, .. } => {
                    for item in items {
                        call_names(item, names);
                    }
                }
                Expr::Path { .. } => {}
                _ => panic!("unexpected straight-line test expression: {e:?}"),
            }
        }

        let names = ["first", "middle", "last"];
        for effect_mask in 0..8 {
            for index in 0..3 {
                let items = names
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        if effect_mask & (1 << position) == 0 {
                            test_path(name)
                        } else {
                            Expr::synth_call(
                                test_path(name),
                                vec![CallArg::Value(Expr::Unit {
                                    source_trivia: Default::default(),
                                    occurrence: Default::default(),
                                    meta: test_meta(),
                                })],
                                Span::new(0, 0),
                            )
                        }
                    })
                    .collect();
                let target_ty = Type::Product {
                    left: Box::new(test_unit_ty()),
                    right: Box::new(test_pair_ty()),
                    meta: test_meta(),
                };
                let target = Expr::EnrichedTuple {
                    occurrence: Default::default(),
                    items,
                    synth_ty: target_ty.clone(),
                    meta: test_meta(),
                    ext: (),
                };
                let projection = Expr::EnrichedProject {
                    occurrence: Default::default(),
                    target: Box::new(target),
                    index,
                    arity: names.len(),
                    target_ty,
                    meta: test_meta(),
                    ext: (),
                };
                let mut changed = false;
                let result = project_after_tuple(projection, &mut changed);
                let last_effect = (0..3)
                    .rev()
                    .find(|position| effect_mask & (1 << position) != 0);
                let selected_is_effectful = effect_mask & (1 << index) != 0;
                assert_eq!(
                    changed,
                    !selected_is_effectful || last_effect == Some(index)
                );
                let expected_calls: Vec<_> = names
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| effect_mask & (1 << position) != 0)
                    .map(|(_, name)| *name)
                    .collect();
                let mut actual_calls = Vec::new();
                call_names(&result, &mut actual_calls);
                assert_eq!(
                    actual_calls, expected_calls,
                    "mask={effect_mask}, index={index}"
                );
                if changed {
                    let mut selected = &result;
                    while let Expr::Seq { body, .. } = selected {
                        selected = body;
                    }
                    let selected = match selected {
                        Expr::Call { callee, .. } => callee.as_ref(),
                        other => other,
                    };
                    assert!(
                        is_path_ref(selected, names[index]),
                        "wrong selected member: {result:?}"
                    );
                } else {
                    assert!(matches!(result, Expr::EnrichedProject {
                        index: retained_index, arity: 3, ..
                    } if retained_index == index));
                }
            }
        }
    }

    #[test]
    fn final_tuple_slot_folding_preserves_prefix_and_suffix_effect_order() {
        fn calls(expr: &Expr<Enriched>, out: &mut Vec<String>) {
            match expr {
                Expr::Call { callee, .. } => {
                    let Expr::Path { segments, .. } = callee.as_ref() else {
                        panic!("named effect");
                    };
                    out.push(segments[0].name.clone());
                }
                Expr::Seq { value, body, .. } => {
                    calls(value, out);
                    calls(body, out);
                }
                Expr::EnrichedTuple { items, .. } => {
                    for item in items {
                        calls(item, out);
                    }
                }
                Expr::Path { .. } => {}
                other => panic!("unexpected straight-line term: {other:?}"),
            }
        }
        let names = ["first", "second", "third", "last"];
        let tuple_ty = Type::Product {
            left: Box::new(test_unit_ty()),
            right: Box::new(Type::Product {
                left: Box::new(test_unit_ty()),
                right: Box::new(test_pair_ty()),
                meta: test_meta(),
            }),
            meta: test_meta(),
        };
        for mask in 0..16 {
            for index in [1, 2] {
                let items = names
                    .iter()
                    .enumerate()
                    .map(|(position, name)| {
                        if mask & (1 << position) == 0 {
                            test_path(name)
                        } else {
                            Expr::synth_call(test_path(name), vec![], Span::new(0, 0))
                        }
                    })
                    .collect();
                let projection = Expr::EnrichedProject {
                    occurrence: Default::default(),
                    target: Box::new(Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: tuple_ty.clone(),
                        meta: test_meta(),
                        ext: (),
                    }),
                    index,
                    arity: index + 1,
                    target_ty: tuple_ty.clone(),
                    meta: test_meta(),
                    ext: (),
                };
                let mut changed = false;
                let result = project_after_tuple(projection, &mut changed);
                assert!(changed, "final slot retains the constructed suffix");
                let mut actual = Vec::new();
                calls(&result, &mut actual);
                let expected: Vec<_> = names
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| mask & (1 << position) != 0)
                    .map(|(_, name)| *name)
                    .collect();
                assert_eq!(actual, expected, "mask={mask}, index={index}");
                let mut tail = &result;
                while let Expr::Seq { body, .. } = tail {
                    tail = body;
                }
                let Expr::EnrichedTuple {
                    items, synth_ty, ..
                } = tail
                else {
                    panic!("retained suffix tuple: {result:?}");
                };
                assert_eq!(items.len(), names.len() - index);
                for (item, expected) in items.iter().zip(&names[index..]) {
                    let value = match item {
                        Expr::Call { callee, .. } => callee.as_ref(),
                        other => other,
                    };
                    assert!(is_path_ref(value, expected));
                }
                let expected_ty = (0..index).fold(&tuple_ty, |ty, _| match ty {
                    Type::Product { right, .. } => right.as_ref(),
                    _ => unreachable!(),
                });
                assert!(structural_type_equal(synth_ty, expected_ty));
            }
        }
    }

    #[test]
    fn project_after_tuple_retains_effects_exposed_by_substitution_and_beta() {
        let module = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type N; host fn first() -> N; host fn second() -> N; \
             fn direct() -> N { __fst__[N][ N]( __pair__[N][ N]( first(), second())) } \
             fn bound() -> N { let pair = (first(), second()); __fst__[N][ N]( pair) } \
             fn closure() -> N { __fst__[N][ N]( .() -> N & N { (first(), second()) }()) }",
        );
        for name in ["direct", "bound", "closure"] {
            let body = fn_body(&module, name);
            let Expr::EnrichedProject {
                target, index: 0, ..
            } = body
            else {
                panic!("{name}: effectful selected member must remain before the suffix: {body:?}")
            };
            let Expr::EnrichedTuple { items, .. } = target.as_ref() else {
                panic!("{name}: expected recovered or substituted tuple: {target:?}")
            };
            assert_eq!(items.len(), 2);
            for (item, expected) in items.iter().zip(["first", "second"]) {
                assert!(
                    matches!(item, Expr::Call { callee, .. } if is_path_ref(callee, expected)),
                    "{name}: reordered tuple member: {item:?}"
                );
            }
        }
    }

    /// Selecting a nominal tuple slot must retain its projector. Complete
    /// constructor/projector elision may yield `x`; otherwise the recovered
    /// field-get must still unwrap exactly `Hed.mk(x)`, not return that wrapper.
    #[test]
    fn field_get_after_tuple_preserves_projector_semantics() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type I32 role(i32); \
             type { Hed: I32; }; \
             fn t(x: I32) -> I32 { \
               Hed.get(__fst__[Hed][ .]( __pair__[Hed][ .]( Hed.mk(x), ()))) \
             }",
        );
        let body = fn_body(&m, "t");
        if is_path_ref(body, "x") {
            return;
        }
        let Expr::EnrichedFieldGet {
            target,
            field_name,
            index,
            arity,
            target_ty,
            ..
        } = body
        else {
            panic!("expected payload `x` or its retained nominal projector, got {body:?}");
        };
        assert_eq!((field_name.as_str(), *index, *arity), ("Hed", 0, 2));
        assert!(
            matches!(target_ty,
                Type::Product { left, right, .. }
                if matches!(left.as_ref(), Type::Path { segments, args, .. }
                    if segments.len() == 1 && segments[0].name == "Hed" && args.is_empty())
                    && matches!(right.as_ref(), Type::Unit { .. })
            ),
            "unexpected selected nominal type: {target_ty:?}"
        );
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = target.as_ref()
        else {
            panic!("expected the original nominal tuple, got {target:?}");
        };
        assert!(super::structural_type_equal(synth_ty, target_ty));
        assert!(
            matches!(items.as_slice(),
                [Expr::Call { callee, args, .. }, Expr::Unit { .. }]
                if matches!(callee.as_ref(), Expr::Path { segments, .. }
                    if segments.len() == 2 && segments[0].name == "Hed" && segments[1].name == "mk")
                    && matches!(args.as_slice(), [CallArg::Value(payload)] if is_path_ref(payload, "x"))
            ),
            "projector must unwrap exactly Hed.mk(x) in the selected slot: {items:?}"
        );
    }

    #[test]
    fn field_get_over_recovered_tail_composes_to_original_product() {
        let module = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; type { A: .; B: .; C: .; } type Wide = A & B & C; fn read(r: Wide) -> . { C.get(__snd__[B][C](__snd__[A][B & C](r))) }",
        );
        let body = fn_body(&module, "read");
        assert!(
            matches!(body, Expr::EnrichedFieldGet { target, index: 2, arity: 3, .. }
            if is_path_ref(target, "r")),
            "{body:?}"
        );
        assert!(!contains(body, &|expr| matches!(expr, Expr::Let { .. })));
    }

    #[test]
    fn field_update_preserved_tail_slots_compose_to_original_product() {
        let module = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; type { A: .; B: .; C: .; D: .; } type Wide = A & B & C & D; fn update(r: Wide) -> Wide { (__fst__(r), __fst__(__snd__(r)), C.mk(()), __snd__(__snd__(__snd__(r)))) }",
        );
        let body = fn_body(&module, "update");
        assert!(
            !contains(
                body,
                &|expr| matches!(expr, Expr::EnrichedProject { target, .. }
            if matches!(target.as_ref(), Expr::Let { .. }))
            ),
            "{body:?}"
        );
        assert!(
            contains(body, &|expr| matches!(expr,
            Expr::EnrichedProject { target, index: 3, arity: 4, .. } if is_path_ref(target, "r"))),
            "last preserved slot reads the original receiver: {body:?}"
        );
    }

    #[test]
    fn direct_tail_composition_preserves_bounded_values_and_effects() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        enum Value {
            Atom(&'static str),
            Unit,
            Function,
            Nominal(Box<Value>),
            Pair(Box<Value>, Box<Value>),
        }
        fn pair(left: Value, right: Value) -> Value {
            Value::Pair(Box::new(left), Box::new(right))
        }
        fn value_type(value: &Value) -> Type<Enriched> {
            match value {
                Value::Unit => test_unit_ty(),
                Value::Function => Type::synth_function(vec![], test_unit_ty(), Span::new(0, 0)),
                Value::Pair(left, right) => Type::Product {
                    left: Box::new(value_type(left)),
                    right: Box::new(value_type(right)),
                    meta: test_meta(),
                },
                Value::Atom(_) => Type::synth_path(vec!["Scalar".into()], vec![], Span::new(0, 0)),
                Value::Nominal(payload) => Type::synth_path(
                    vec!["Opaque".into()],
                    vec![value_type(payload)],
                    Span::new(0, 0),
                ),
            }
        }
        fn select(mut value: Value, index: usize, arity: usize) -> Value {
            assert!(arity >= 2 && index < arity);
            for _ in 0..index {
                let Value::Pair(_, right) = value else {
                    panic!("right-spine step requires a pair");
                };
                value = *right;
            }
            if index + 1 == arity {
                value
            } else {
                let Value::Pair(left, _) = value else {
                    panic!("nonterminal slot requires a pair");
                };
                *left
            }
        }
        fn source_call() -> Expr<Enriched> {
            Expr::synth_call(test_path("source"), vec![], Span::new(0, 0))
        }
        fn project(
            target: Expr<Enriched>,
            value: &Value,
            index: usize,
            arity: usize,
        ) -> Expr<Enriched> {
            Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(target),
                index,
                arity,
                target_ty: value_type(value),
                meta: test_meta(),
                ext: (),
            }
        }
        fn evaluate(
            expr: &Expr<Enriched>,
            source: &Value,
            effects: &mut Vec<&'static str>,
        ) -> Value {
            match expr {
                Expr::Call { callee, args, .. } => {
                    assert!(is_path_ref(callee, "source") && args.is_empty());
                    effects.push("source");
                    source.clone()
                }
                Expr::EnrichedProject {
                    target,
                    index,
                    arity,
                    ..
                } => select(evaluate(target, source, effects), *index, *arity),
                Expr::EnrichedFieldGet {
                    target,
                    index,
                    arity,
                    ..
                } => {
                    let Value::Nominal(payload) =
                        select(evaluate(target, source, effects), *index, *arity)
                    else {
                        panic!("field getter unwraps its nominal slot");
                    };
                    *payload
                }
                other => panic!("unexpected evaluator term: {other:?}"),
            }
        }
        let source = pair(
            pair(Value::Atom("left"), Value::Unit),
            pair(
                Value::Unit,
                pair(
                    Value::Nominal(Box::new(Value::Atom("payload"))),
                    pair(Value::Function, pair(Value::Atom("tail"), Value::Unit)),
                ),
            ),
        );
        for inner_arity in 2..=5 {
            let inner_index = inner_arity - 1;
            let tail = select(source.clone(), inner_index, inner_arity);
            for outer_arity in 2..=6 - inner_index {
                for outer_index in 0..outer_arity {
                    let before = project(
                        project(source_call(), &source, inner_index, inner_arity),
                        &tail,
                        outer_index,
                        outer_arity,
                    );
                    let mut changed = false;
                    let after = compose_recovered_tail_access(before.clone(), &mut changed);
                    assert!(changed);
                    let mut before_effects = Vec::new();
                    let mut after_effects = Vec::new();
                    assert_eq!(
                        evaluate(&before, &source, &mut before_effects),
                        evaluate(&after, &source, &mut after_effects)
                    );
                    assert_eq!(before_effects, ["source"]);
                    assert_eq!(after_effects, before_effects);
                    assert!(
                        matches!(&after, Expr::EnrichedProject { target, index, arity, target_ty, .. }
                        if *index == inner_index + outer_index && *arity == inner_index + outer_arity
                            && matches!(target.as_ref(), Expr::Call { .. }) && structural_type_equal(target_ty, &value_type(&source)))
                    );
                }
            }
        }
        let left = select(source.clone(), 0, 6);
        let before = project(project(source_call(), &source, 0, 6), &left, 1, 2);
        let mut changed = false;
        let after = compose_recovered_tail_access(before.clone(), &mut changed);
        assert!(!changed, "a compound nonterminal left slot is not a tail");
        assert_eq!(
            evaluate(&before, &source, &mut Vec::new()),
            evaluate(&after, &source, &mut Vec::new())
        );

        let tail = select(source.clone(), 2, 3);
        let getter = Expr::EnrichedFieldGet {
            occurrence: Default::default(),
            target: Box::new(project(source_call(), &source, 2, 3)),
            field_name: "Opaque".into(),
            index: 0,
            arity: 2,
            target_ty: value_type(&tail),
            meta: test_meta(),
            ext: (),
        };
        let mut changed = false;
        let after = compose_recovered_tail_access(getter.clone(), &mut changed);
        assert!(changed);
        let mut effects = Vec::new();
        assert_eq!(
            evaluate(&after, &source, &mut effects),
            Value::Atom("payload")
        );
        assert_eq!(effects, ["source"]);
        assert!(
            matches!(after, Expr::EnrichedFieldGet { field_name, index: 2, arity: 4, .. } if field_name == "Opaque")
        );
    }

    #[test]
    fn direct_tail_composition_rejects_invalid_views_and_overflow() {
        for (inner_index, inner_arity, index, arity) in [
            (0, 1, 0, 2),
            (1, 2, 2, 2),
            (1, 2, 0, 1),
            (usize::MAX, usize::MAX, 0, 2),
            (usize::MAX - 1, usize::MAX, 0, 2),
        ] {
            let mut inner = test_project("source", inner_index);
            let Expr::EnrichedProject { arity: stored, .. } = &mut inner else {
                unreachable!()
            };
            *stored = inner_arity;
            let before = Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(inner),
                index,
                arity,
                target_ty: test_pair_ty(),
                meta: test_meta(),
                ext: (),
            };
            let mut changed = false;
            let after = compose_recovered_tail_access(before.clone(), &mut changed);
            assert!(!changed);
            assert_eq!(after, before);
        }
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
               let residual = stage()[N]; \
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

    #[test]
    fn declared_pure_calls_feed_dce_without_overriding_lexical_binders() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             pure fn factory() { () } \
             host fn effect() -> .; \
             fn direct() { factory(); () } \
             fn parameter(factory: . -> .) { factory(); () } \
             fn binding() { let factory = effect; factory(); () } \
             fn closure(factory: . -> .) -> . -> . { .() { factory(); () } } \
             fn branch(s: (. -> .) | .) { \
               __either__[. -> .][ .][ .]( s, .(factory: . -> .) { factory(); () }, .() { () }) \
             }",
        );
        assert!(matches!(fn_body(&m, "direct"), Expr::Unit { .. }));
        for name in ["parameter", "binding", "closure", "branch"] {
            assert!(
                contains(fn_body(&m, name), &|expr| matches!(expr, Expr::Call { .. })),
                "a shadowed callable invocation was removed: {name}"
            );
        }
    }

    #[test]
    fn shared_purity_distinguishes_closure_construction_from_invocation() {
        let m = recover_one(
            "x/main",
            "module x/main; host fn effect() -> .; \
             fn literal() -> . -> . { .() { effect() } } \
             fn pure_call() { .() { () }() } \
             fn effect_call() { .() { effect() }() }",
        );
        assert!(is_pure(fn_body(&m, "literal")));
        assert!(is_pure(fn_body(&m, "pure_call")));
        assert!(!is_pure(fn_body(&m, "effect_call")));
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
        let body = let_fuse(
            &OptimizerCtx::default(),
            fn_body(&module, "t").clone(),
            &mut changed,
        );
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
               let delayed = sub_i32(x, 1[I32]); \
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
               let delayed = sub_i32(x, 1[I32]); \
               pick_second(sub_i32(x, 2[I32]), delayed) \
             }",
        );
        assert!(!changed, "let crossed an earlier effect: {body:?}");
        assert!(matches!(body, Expr::Let { .. }));
    }

    #[test]
    fn let_fuse_impure_value_stays_outside_deferred_uses() {
        let sources = [
            "module x/main; import control(Bool, macro if); \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(cond: Bool, x: I32) -> I32 { \
               let delayed = sub_i32(x, 1[I32]); \
               if (cond) { delayed } else { x } \
             }",
            "module x/main; import __intrinsics__; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(value: I32 | I32, x: I32) -> I32 { \
               let delayed = sub_i32(x, 1[I32]); \
               __either__(value, \
                 .(left: I32) -> I32 { delayed }, \
                 .(right: I32) -> I32 { x }) \
             }",
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn t(x: I32) -> (I32 -> I32) { \
               let delayed = sub_i32(x, 1[I32]); \
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
               let delayed = sub_i32(x, 1[I32]); \
               let prefix = x; \
               pick_second(prefix, delayed) \
             }",
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(x: I32) -> I32 { \
               let delayed = sub_i32(x, 1[I32]); \
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
               let delayed = sub_i32(x, 1[I32]); \
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
    fn beta_reduces_nullary_literal_with_omitted_or_explicit_unit() {
        for call in [".() { () }()", ".() { () }(())"] {
            let m = optimize_one("x/main", &format!("module x/main; fn t() {{ {call} }}"));
            assert!(matches!(fn_body(&m, "t"), Expr::Unit { .. }), "{call}");
        }
    }

    #[test]
    fn nullary_beta_keeps_effectful_unit_argument_before_body() {
        let m = optimize_one(
            "x/main",
            "module x/main; host fn before() -> .; host fn after() -> .; \
             fn t() { .() { after() }(before()) }",
        );
        let Expr::Seq { value, body, .. } = fn_body(&m, "t") else {
            panic!("Unit argument and body effects must both remain")
        };
        assert!(
            matches!(value.as_ref(), Expr::Call { callee, .. } if is_path_ref(callee, "before"))
        );
        assert!(matches!(body.as_ref(), Expr::Call { callee, .. } if is_path_ref(callee, "after")));
        assert!(!contains(fn_body(&m, "t"), &|e| matches!(
            e,
            Expr::FnExpr { .. }
        )));
    }

    #[test]
    fn nullary_beta_preserves_returned_closure_and_capture() {
        let m = optimize_one(
            "x/main",
            "module x/main; fn t[A](captured: A) -> A -> A { \
             .() { .(later: A) { captured } }() }",
        );
        let Expr::FnExpr { sig, body, .. } = fn_body(&m, "t") else {
            panic!("the returned closure must remain")
        };
        assert_eq!(sig.value_param_names().collect::<Vec<_>>(), ["later"]);
        assert!(is_path_ref(body, "captured"));
    }

    #[test]
    fn literal_beta_keeps_unconsumed_value_and_type_stages() {
        for source in [
            "fn t[A]() -> A -> A { .()(value: A) { value }(()) }",
            "fn t[A](value: A) -> . -> A { .(item: A)() { item }(value) }",
            "fn t() -> . -> . { .()() { () }(()) }",
            "fn t() -> [A] A -> A { .()[A](value: A) { value }(()) }",
        ] {
            let m = optimize_one("x/main", &format!("module x/main; {source}"));
            assert!(
                matches!(fn_body(&m, "t"), Expr::Call { callee, .. }
                if matches!(callee.as_ref(), Expr::FnExpr { .. })),
                "{source}"
            );
        }
    }

    #[test]
    fn natural_match_and_user_named_equivalent_recover_one_dispatch() {
        let m = optimize_one(
            "x/main",
            "module x/main; import match(macro match, match_checked); \
             macro choose : [Source][Clauses] (Source & Clauses) -> [Target] Target \
             { trailing product; impl(fills) match_checked } \
             host type Left; host type Middle; host type Right; \
             host fn chosen[A](payload: A) -> .; \
             fn ordinary(value: Left | Middle | Right) { \
               match (value) { \
                 (.(left: Left) { chosen(left) }, \
                  .(middle: Middle) { chosen(middle) }, \
                  .(right: Right) { chosen(right) }) \
               } \
             } \
             fn renamed(value: Left | Middle | Right) { \
               choose (value) { \
                 (.(left: Left) { chosen(left) }, \
                  .(middle: Middle) { chosen(middle) }, \
                  .(right: Right) { chosen(right) }) \
               } \
             }",
        );
        for name in ["ordinary", "renamed"] {
            let body = fn_body(&m, name);
            let Expr::EnrichedMatch {
                arms, scrutinee, ..
            } = body
            else {
                panic!("{name} must reach shared dispatch: {body:?}")
            };
            assert_eq!(arms.len(), 3);
            assert!(is_path_ref(scrutinee, "value"));
            for arm in arms {
                assert_eq!(count_free_uses(&arm.body, &arm.param), 1);
            }
            assert!(!arms.iter().any(|arm| contains(&arm.body, &|expr| matches!(
                expr,
                Expr::EnrichedMatch { .. }
            ))));
        }
    }

    #[test]
    fn nested_unit_match_clauses_do_not_survive_as_calls() {
        const MODULE_PATH: &str = "x/main";
        const SOURCE: &str = "module x/main; import match(macro match); \
             fn t() -> . { \
               match () { .(_u: .) { \
                 match () { .(_u: .) { \
                   match () { .(_u: .) { () } } \
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
               let x = __absurd__[A]( b); x \
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
                 __left__[A][ B]( x), \
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
               __snd__[A][ B]( __pair__[A][ B]( x, y)) \
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
               __snd__[A][ B & C]( __pair__[A][ B & C]( x, __pair__[B][ C]( y, z))) \
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
            source_trivia: Default::default(),
            occurrence: Default::default(),
            diagnostic: Default::default(),
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
                 __left__[A][ A]( x), \
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
             type { Red: . { mk; get; }; }; \
             type { Green: . { mk; get; }; }; \
             type { Blue: . { mk; get; }; }; \
             host fn consume[A](value: A) -> .; \
             fn t() -> . { \
               let green = Green.mk(()); \
               let inner = __left__[Green][ Blue]( green); \
               let widened = __right__[Red][ Green | Blue]( inner); \
               consume[Red | Green | Blue](widened) \
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
                 __either__[A][ A][ A | A]( s, \
                   .(x: A) -> (A | A) { __left__[A][ A]( x) }, \
                   .(y: A) -> (A | A) { __right__[A][ A]( y) }), \
                 .(p: A) -> (A | A) { __left__[A][ A]( p) }, \
                 .(q: A) -> (A | A) { __right__[A][ A]( q) }) \
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
                 __either__[A][ A][ A | A]( s, \
                   .(x: A) -> (A | A) { __left__[A][ A]( x) }, \
                   .(y: A) -> (A | A) { __right__[A][ A]( y) }), \
                 .(p: A) -> (A | A) { __left__[A][ A]( p) }, \
                 .(q: A) -> (A | A) { __right__[A][ A]( q) }) \
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
                 __either__[A][ A][ A | A]( s, \
                   .(x: A) -> (A | A) { __left__[A][ A]( x) }, \
                   .(y: A) -> (A | A) { __right__[A][ A]( y) }), \
                 .(p: A) -> (A | A) { __left__[A][ A]( x) }, \
                 .(q: A) -> (A | A) { __right__[A][ A]( q) }) \
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

    fn assert_selected_branch_value(body: &Expr<Enriched>, expected: &str) {
        assert!(
            matches!(body, Expr::Path { segments, .. }
                if segments.len() == 1 && segments[0].name == expected),
            "selected the wrong branch: {body:?}"
        );
    }

    /// `constant_fold` firing coverage: `if .t { a } else { b }`
    /// (an `EnrichedConditional(BoolLit(true), …)` — `.t` is the
    /// `role(bool)` true marker) folds to the `then` branch `a`, with
    /// no `EnrichedConditional` remaining. Folding vs. emitting a
    /// `if .t { } else { }` print the same value, so a runtime
    /// golden can't pin the fold.
    #[test]
    fn constant_fold_selects_then_on_literal_true() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; import control(macro if); \
             import testapi(Bool); \
             fn t[A](a: A, b: A) -> A { \
               if (.t) { a } else { b } \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedConditional { .. })),
            "expected no conditional after fold, got body: {body:?}"
        );
        assert_selected_branch_value(body, "a");
    }

    #[test]
    fn bound_conditional_recovers_dynamic_literal_thunks_and_captures() {
        let m = optimize_one(
            "x/main",
            "module x/main; import control(Bool, macro if); \
             fn t[A](flag: Bool, outer: A, other: A) -> A { \
               if(flag) { outer } else { other } \
             }",
        );
        let body = fn_body(&m, "t");
        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = body
        else {
            panic!("expected a native conditional without thunk calls: {body:?}")
        };
        assert!(is_path_ref(cond, "flag"));
        assert!(is_path_ref(then_branch, "outer"));
        assert!(is_path_ref(else_branch, "other"));
    }

    #[test]
    fn bound_conditional_preserves_shadowed_branch_captures() {
        let m = optimize_one(
            "x/main",
            "module x/main; import control(Bool, macro if); \
             fn t[A](flag: Bool, outer: A, other: A) -> A { \
               if(flag) { .(outer: A) { outer }(other) } else { outer } \
             }",
        );
        let body = fn_body(&m, "t");
        let Expr::EnrichedConditional {
            then_branch,
            else_branch,
            ..
        } = body
        else {
            panic!("expected a recovered conditional: {body:?}")
        };
        assert!(is_path_ref(then_branch, "other"));
        assert!(is_path_ref(else_branch, "outer"));
    }

    #[test]
    fn bound_conditional_keeps_effectful_condition_once() {
        let m = optimize_one(
            "x/main",
            "module x/main; import control(Bool, macro if); \
             host fn condition() -> Bool; \
             fn t[A](left: A, right: A) -> A { \
               if(condition()) { left } else { right } \
             }",
        );
        let body = fn_body(&m, "t");
        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = body
        else {
            panic!("expected one conditional with its condition call: {body:?}")
        };
        assert!(matches!(cond.as_ref(), Expr::Call { callee, .. }
            if matches!(callee.as_ref(), Expr::Path { segments, .. }
                if segments.last().is_some_and(|part| part.name == "condition"))));
        assert!(is_path_ref(then_branch, "left"));
        assert!(is_path_ref(else_branch, "right"));
    }

    #[test]
    fn bound_conditional_keeps_effectful_unselected_thunk_factory() {
        let m = optimize_one(
            "x/main",
            "module x/main; import control(Bool, macro if); \
             host fn factory() -> . -> .; \
             fn t(flag: Bool) { if(flag, .() { () }, factory()) }",
        );
        let body = fn_body(&m, "t");
        let Expr::Let {
            name,
            value,
            body: dispatch,
            ..
        } = body
        else {
            panic!("the eager factory binding must remain: {body:?}")
        };
        assert!(
            matches!(value.as_ref(), Expr::Call { callee, args, .. }
            if matches!(callee.as_ref(), Expr::Path { segments, .. }
                if segments.last().is_some_and(|part| part.name == "factory"))
                && matches!(args.as_slice(), [] | [CallArg::Value(Expr::Unit { .. })])),
            "the factory must run exactly once in the eager initializer: {value:?}"
        );
        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = dispatch.as_ref()
        else {
            panic!("the factory must run before the conditional: {dispatch:?}")
        };
        assert!(is_path_ref(cond, "flag"));
        assert!(matches!(then_branch.as_ref(), Expr::Unit { .. }));
        assert!(
            matches!(else_branch.as_ref(), Expr::Call { callee, args, .. }
            if is_path_ref(callee, name)
                && matches!(args.as_slice(), [] | [CallArg::Value(Expr::Unit { .. })]))
        );
        assert_eq!(count_free_uses(dispatch, name), 1);
    }

    #[test]
    fn bound_conditional_guard_keeps_a_unary_unit_thunk() {
        let m = recover_one(
            "x/main",
            "module x/main; import control(Bool, macro if); \
             fn t(flag: Bool) { if(flag, .(unit: .) { unit }, .() { () }) }",
        );
        let mut body = fn_body(&m, "t").clone();
        // The catalog composes child tail projections before this parent rule.
        // Prepare that exact input without running the conditional rewrite.
        let Expr::Let { body: dispatch, .. } = &mut body else {
            panic!("expected a bound packet: {body:?}")
        };
        let Expr::EnrichedConditional {
            then_branch,
            else_branch,
            ..
        } = dispatch.as_mut()
        else {
            panic!("expected a recovered conditional: {dispatch:?}")
        };
        let mut access_changed = false;
        for branch in [then_branch, else_branch] {
            let Expr::Call { callee, args, .. } = branch.as_mut() else {
                panic!("expected an ordinary branch call: {branch:?}")
            };
            assert!(matches!(
                args.as_slice(),
                [] | [CallArg::Value(Expr::Unit { .. })]
            ));
            **callee = compose_recovered_tail_access((**callee).clone(), &mut access_changed);
        }
        let Expr::Let {
            name,
            value,
            body: dispatch,
            ..
        } = &body
        else {
            panic!("expected a bound packet: {body:?}")
        };
        let Expr::EnrichedTuple { items, .. } = value.as_ref() else {
            panic!("expected a tuple packet: {value:?}")
        };
        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = dispatch.as_ref()
        else {
            panic!("expected a packet-projection conditional: {dispatch:?}")
        };
        let (condition_index, condition_arity, _) =
            projection_from_bound(cond, name).expect("condition projects the bound packet");
        let Expr::Call {
            callee: then_callee,
            ..
        } = then_branch.as_ref()
        else {
            panic!("then branch calls the projected function")
        };
        let Expr::Call {
            callee: else_callee,
            ..
        } = else_branch.as_ref()
        else {
            panic!("else branch calls the projected function")
        };
        let (then_index, then_arity, _) = projection_from_bound(then_callee, name)
            .expect("then callee projects the bound packet");
        let (else_index, else_arity, _) = projection_from_bound(else_callee, name)
            .expect("else callee projects the bound packet");
        assert_eq!(
            (condition_arity, then_arity, else_arity),
            (items.len(), items.len(), items.len())
        );
        assert_ne!(condition_index, then_index);
        assert_ne!(condition_index, else_index);
        assert_ne!(then_index, else_index);
        assert!(matches!(&items[then_index], Expr::FnExpr { sig, .. }
            if matches!(sig.groups.as_slice(), [crate::ast::SignatureGroupKind::Value { len: 1 }])));
        assert!(matches!(&items[else_index], Expr::FnExpr { sig, .. }
            if matches!(sig.groups.as_slice(), [crate::ast::SignatureGroupKind::Value { len: 0 }])));
        // Every other guard is satisfied: replacing only the unary literal
        // with the already typed nullary literal must make the rule fire.
        let nullary = items[else_index].clone();
        let mut positive = body.clone();
        let Expr::Let { value, .. } = &mut positive else {
            unreachable!("the positive control preserves the packet")
        };
        let Expr::EnrichedTuple { items, .. } = value.as_mut() else {
            unreachable!("the positive control preserves the tuple")
        };
        items[then_index] = nullary;
        let mut changed = false;
        let output = recover_bound_conditional(positive, &mut changed);
        assert!(
            changed,
            "the nullary counterpart must reach and fire the rule"
        );
        assert!(matches!(output, Expr::EnrichedConditional { .. }));

        let mut changed = false;
        let output = recover_bound_conditional(body.clone(), &mut changed);
        assert!(
            !changed,
            "a unary Unit function is outside the nullary rule"
        );
        assert!(matches!(output, Expr::Let { .. }));
        assert_eq!(
            serde_json::to_value(&output).unwrap(),
            serde_json::to_value(&body).unwrap(),
            "declining the unary literal must preserve the complete input"
        );
    }

    /// Symmetric: `if .f { a } else { b }` folds to the `else`
    /// branch `b`.
    #[test]
    fn constant_fold_selects_else_on_literal_false() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; import control(macro if); \
             import testapi(Bool); \
             fn t[A](a: A, b: A) -> A { \
               if (.f) { a } else { b } \
             }",
        );
        let body = fn_body(&m, "t");
        assert!(
            !contains(body, &|e| matches!(e, Expr::EnrichedConditional { .. })),
            "expected no conditional after fold, got body: {body:?}"
        );
        assert_selected_branch_value(body, "b");
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
    /// `Foo.get[A](Foo.mk[A](x))` collapses to the bare `x`.
    /// The optimizer is observationally a no-op (the round-trip is
    /// identity in every backend), so no runtime golden can pin it.
    #[test]
    fn newtype_unwrap_of_wrap_collapses_to_payload() {
        let m = optimize_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             type { Foo[A]: A { pub mk; pub get; }; }; \
             fn t[A](x: A) -> A { Foo.get[A](Foo.mk[A](x)) }",
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
                 type { Foo: . { pub mk; pub get; }; };",
            ),
            (
                "b.kio",
                "module b; \
                 type { Foo: . { pub mk; pub get; }; }; \
                 fn t(value: .) -> . { Foo.get(Foo.mk(value)) }",
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
        let consumer = &recovered.module("b").expect("module b").module;
        let declaration = consumer
            .items
            .iter()
            .find_map(|item| match item {
                Item::Newtype(declaration) if declaration.name == "Foo" => Some(declaration),
                _ => None,
            })
            .expect("b.Foo declaration");
        let context = super::OptimizerCtx::for_module(&recovered, consumer);
        let head = [PathSegment::new("Foo", Span::new(0, 0))];
        let selected = context
            .newtype_members
            .get(&head)
            .expect("local Foo selection");
        assert_eq!(
            selected.identity,
            crate::pass::resolve::ResolvedNewtypeIdentity::new(&consumer.path, declaration),
            "the consumer must resolve its own Foo identity"
        );
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
                 pub type { Box: . { pub mk; pub get; }; };",
            ),
            (
                "decoy.kio",
                "module decoy; \
                 pub type { Box: . { pub mk; pub get; }; };",
            ),
            (
                "consumer.kio",
                "module consumer; \
                 import provider(Box); \
                 import provider as p; import provider as q; import decoy as d; \
                 fn same_provider(value: .) -> . { p.Box.get(q.Box.mk(value)) } \
                 fn mixed_route(value: .) -> . { Box.get(p.Box.mk(value)) } \
                 fn distinct_provider(value: d.Box) -> p.Box { \
                   p.Box.mk(d.Box.get(value)) \
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

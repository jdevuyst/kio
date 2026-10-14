//! Capability annotations on the [`Routed`] AST.
//!
//! A post-[`crate::pass::route`] suite of per-module passes that
//! write capability annotations onto the AST at the [`Routed`]
//! phase. Each pass is purely additive — every sub-expression is the
//! same shape it was before, only the per-position capability
//! fields change.
//!
//! ## What's annotated today
//!
//! Each pass populates one position-specific annotation:
//!
//! - **[`annotate_escapes`]** — fills [`Capabilities`] on every
//!   [`Expr::FnExpr`] with `captured_from`, the outer-scope binders
//!   the body references.
//! - **[`annotate_lifetime`]** — fills the `lifetime` field of
//!   [`FnTypeCapabilities`] on every [`crate::ast::Type::Function`].
//!   [`Lifetime::Stack`] for fn-typed value-params whose name
//!   doesn't escape the surrounding fn body; [`Lifetime::Heap`]
//!   everywhere else. Backend representation planning consumes this
//!   lifetime classification independently of callable carrier syntax.
//!
//! ## Pipeline position
//!
//! ```text
//! route::route_package → Routed
//!                       → annotate_escapes         ← Expr::FnExpr
//!                       → annotate_lifetime        ← Type::Function
//!                       → per-backend lowering
//! ```
//!
//! Escape annotation projects the shared executable body's capture plan.
//! Lifetime annotation then consumes those captures when classifying values
//! retained by nested callable entries.
//!
//! ## Per-module parallelism
//!
//! Each pass inherits the per-module fan-out convention from
//! [`crate::pass::structural_recovery::recover_package`] and
//! [`crate::pass::optimize::optimize_package`]: each module is annotated
//! independently across rayon's `par_iter`. The passes hold no
//! package-wide mutable state; the per-module work runs against the
//! module body alone.
//!
//! [`FnTypeCapabilities`]: crate::ast::FnTypeCapabilities
//! [`Lifetime::Heap`]: crate::ast::Lifetime::Heap
//! [`Lifetime::Stack`]: crate::ast::Lifetime::Stack
//! [`Expr::Let`]: crate::ast::Expr::Let
//! [`Expr::LowIndirectCall`]: crate::ast::Expr::LowIndirectCall

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;

use crate::ast::{Capabilities, EnrichedArm, Expr, Item, Lifetime, Routed, SignatureParam, Type};
use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry};

// ---- public entry points ------------------------------------------------

/// Annotate every [`Expr::FnExpr`] in the package's modules and
/// package file with [`Capabilities`]. The pass is observationally a
/// no-op modulo the new fields: each `Expr::FnExpr` node's
/// `sig` / `ret_ty` / `body` / `meta` are unchanged.
///
/// **Parallelism.** Each module is annotated independently via
/// rayon's `par_iter`; the pass has no cross-module state.
pub fn annotate_escapes(package: Package<Routed>) -> Package<Routed> {
    let (modules, package_file) = package.into_parts();
    let modules: std::collections::BTreeMap<_, _> = crate::maybe_into_par_iter!(modules)
        .map(|(path, entry)| (path, annotate_module_entry(entry)))
        .collect();
    let package_file = package_file.map(annotate_package_file_entry);
    Package::<Routed>::from_parts(modules, package_file)
}

fn annotate_module_entry(entry: ModuleEntry<Routed>) -> ModuleEntry<Routed> {
    let ModuleEntry::<Routed> {
        file_path,
        mut module,
        scope,
    } = entry;
    for item in &mut module.items {
        if let Item::FnDef(d) = item {
            let mut captures =
                crate::backends::closure_plan::ExecutableBodyPlan::new(&d.sig, &d.body)
                    .nested_capture_names()
                    .into_iter();
            annotate_walk(&mut d.body, &mut captures);
            assert!(
                captures.next().is_none(),
                "every planned closure was annotated"
            );
        }
    }
    ModuleEntry::<Routed> {
        file_path,
        module,
        scope,
    }
}

fn annotate_package_file_entry(entry: PackageFileEntry<Routed>) -> PackageFileEntry<Routed> {
    // The package file carries only the phase-independent `bridge` glob
    // list — no item bodies to annotate.
    let PackageFileEntry::<Routed> {
        file_path,
        package_name,
        package_file,
    } = entry;
    PackageFileEntry::<Routed> {
        file_path,
        package_name,
        package_file,
    }
}

/// Annotate every [`crate::ast::Type::Function`] in the package
/// with the `lifetime` field of [`FnTypeCapabilities`]. A fn-typed
/// value-param's slot is marked [`Lifetime::Stack`] iff the param
/// name doesn't escape the surrounding fn body (the same
/// "non-escaping" criterion [`annotate_escapes`] uses for
/// closures); every other slot stays at the conservative default
/// [`Lifetime::Heap`].
///
/// This classifies value lifetimes; the backend's callable representation
/// and public carrier remain separate decisions.
///
/// **Parallelism.** Each module is annotated independently via
/// rayon's `par_iter`; the pass has no cross-module state.
pub fn annotate_lifetime(package: Package<Routed>) -> Package<Routed> {
    let (modules, package_file) = package.into_parts();
    let modules: std::collections::BTreeMap<_, _> = crate::maybe_into_par_iter!(modules)
        .map(|(path, entry)| (path, lifetime_annotate_module_entry(entry)))
        .collect();
    let package_file = package_file.map(lifetime_annotate_package_file_entry);
    Package::<Routed>::from_parts(modules, package_file)
}

fn annotate_walk(e: &mut Expr<Routed>, captures: &mut impl Iterator<Item = Vec<String>>) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::FnExpr { body, caps, .. } => {
            *caps = Capabilities {
                captured_from: captures
                    .next()
                    .expect("the shared body plan contains each nested callable"),
            };
            annotate_walk(body, captures);
        }
        Expr::Let { value, body, .. } => {
            annotate_walk(value, captures);
            annotate_walk(body, captures);
        }
        Expr::Seq { value, body, .. } => {
            annotate_walk(value, captures);
            annotate_walk(body, captures);
        }
        Expr::LowIndirectCall { callee, args, .. } => {
            annotate_walk(callee, captures);
            for a in args {
                annotate_walk(a, captures);
            }
        }
        Expr::LowTypeApplication { callee, .. } => annotate_walk(callee, captures),
        Expr::LowHostCall { args, .. }
        | Expr::LowModuleCall { args, .. }
        | Expr::LowQualifiedModuleCall { args, .. }
        | Expr::LowClosureCall { args, .. } => {
            for a in args {
                annotate_walk(a, captures);
            }
        }
        Expr::LowAbsurdCall { value_arg, .. } => {
            annotate_walk(value_arg, captures);
        }
        Expr::LowNewtypeCtor { payload, .. } | Expr::LowQualifiedNewtypeMember { payload, .. } => {
            annotate_walk(payload, captures);
        }
        Expr::LowNewtypeProj { target, .. } => {
            annotate_walk(target, captures);
        }
        Expr::LowCpsProjectorApply {
            receiver,
            continuation,
            ..
        } => {
            annotate_walk(receiver, captures);
            annotate_walk(continuation, captures);
        }
        Expr::EnrichedTuple { items, .. } => {
            for it in items {
                annotate_walk(it, captures);
            }
        }
        Expr::EnrichedInject { payload, .. } => {
            annotate_walk(payload, captures);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            annotate_walk(scrutinee, captures);
            for arm in arms.iter_mut() {
                annotate_walk(&mut arm.body, captures);
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            annotate_walk(cond, captures);
            annotate_walk(then_branch, captures);
            annotate_walk(else_branch, captures);
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            annotate_walk(target, captures);
        }
        // Leaves and `Low*` value-position variants — nothing to
        // descend into.
        Expr::LowBoundRef { .. }
        | Expr::LowHostFnValueRef { .. }
        | Expr::LowModuleFnValueRef { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        // The surface-residual `Path` / `Call` and every surface-only
        // variant are statically unreachable at the Routed phase —
        // discharge via `match *ext {}`.
        Expr::Path { ext, .. } => match *ext {},
        Expr::Call { ext, .. } => match *ext {},
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
    }
}

// ---- Lifetime annotation -----------------------------------------------

fn lifetime_annotate_module_entry(entry: ModuleEntry<Routed>) -> ModuleEntry<Routed> {
    let ModuleEntry::<Routed> {
        file_path,
        mut module,
        scope,
    } = entry;
    for item in &mut module.items {
        if let Item::FnDef(d) = item {
            lifetime_annotate_fn_body(&mut d.sig, &mut d.body);
            // Stamp the return type's fn slots: their values come
            // from the body's tail expression, which is itself
            // analyzed by `annotate_escapes`. The return type
            // itself is a slot whose value escapes the frame by
            // construction, so it stays `Heap`. Just walk it for
            // any nested fn types inside (e.g., a returned
            // closure-of-closures) — those inherit `Heap` too.
            lifetime_walk_type(&mut d.ret, false);
        }
    }
    ModuleEntry::<Routed> {
        file_path,
        module,
        scope,
    }
}

fn lifetime_annotate_package_file_entry(
    entry: PackageFileEntry<Routed>,
) -> PackageFileEntry<Routed> {
    // The package file carries only the phase-independent `bridge` glob
    // list — no item bodies to annotate.
    entry
}

/// Walk a fn signature and body, refining each
/// [`crate::ast::Type::Function`] slot's `lifetime`. A value-param's
/// fn-typed slot is marked [`Lifetime::Stack`] iff the param name
/// doesn't escape the body; otherwise [`Lifetime::Heap`] (default).
///
/// **Escape criterion.** A param `f` of fn type escapes when
/// `f` appears in any position that lets a closure flow beyond the
/// current stack frame — captured by an inner closure, stored in a
/// let-bound value that escapes, returned, or passed as an arg to a
/// call that retains it. The conservative
/// approximation here: `f` escapes iff it appears anywhere outside
/// the callee position of a `LowClosureCall { name: f }` or a
/// `LowIndirectCall` whose callee is a bare `LowBoundRef { name: f }`
/// — i.e. uses of `f` that are *not* immediate, direct invocations.
fn lifetime_annotate_fn_body(sig: &mut crate::ast::Signature<Routed>, body: &mut Expr<Routed>) {
    for p in sig.params.iter_mut() {
        if let SignatureParam::Value(vp) = p {
            // Refine the param's fn-typed slot (if any). Non-fn
            // types keep their `()` annotation; nested fn types
            // inside a product/sum slot are conservatively
            // marked Heap (we don't trace the per-component
            // lifetime through structural types).
            let name = vp.name.clone();
            let ty_is_fn = matches!(vp.ty, Some(Type::Function { .. }));
            if ty_is_fn {
                let escapes = param_escapes_in_body(&name, body);
                if let Some(Type::Function { caps, .. }) = vp.ty.as_mut() {
                    caps.lifetime = if escapes {
                        Lifetime::Heap
                    } else {
                        Lifetime::Stack
                    };
                }
            } else if let Some(t) = vp.ty.as_mut() {
                // The param itself isn't a fn type, but its type
                // tree may contain nested fn slots. Default
                // every such slot to Heap (the safe choice
                // for nested-in-structure fn types).
                lifetime_walk_type(t, false);
            }
        }
    }
    // Recurse into the body to annotate any nested fn slots
    // (closure params, let-bindings carrying fn-type annotations,
    // etc.).
    lifetime_walk_expr(body);
}

/// Recursively annotate every fn-type slot in `ty` as
/// [`Lifetime::Heap`]. Used for non-leaf positions where the
/// per-slot analysis isn't run — those slots carry the
/// conservative default.
fn lifetime_walk_type(ty: &mut Type<Routed>, _stack_eligible: bool) {
    match ty {
        Type::Function {
            param, ret, caps, ..
        } => {
            caps.lifetime = Lifetime::Heap;
            lifetime_walk_type(param, false);
            lifetime_walk_type(ret, false);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            lifetime_walk_type(left, false);
            lifetime_walk_type(right, false);
        }
        Type::Path { args, .. } => {
            for a in args {
                lifetime_walk_type(a, false);
            }
        }
        Type::Forall { body, .. } => {
            lifetime_walk_type(body, false);
        }
        Type::Unit { .. } | Type::Bottom { .. } => {}
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Recurse into an expression tree to annotate every fn-type slot
/// it contains (param types of nested closures, etc.). Inner-fn
/// closures get their own value-param-level analysis via
/// `lifetime_annotate_fn_body`.
fn lifetime_walk_expr(e: &mut Expr<Routed>) {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::FnExpr {
            sig, ret_ty, body, ..
        } => {
            lifetime_annotate_fn_body(sig, body);
            if let Some(t) = ret_ty.as_mut() {
                lifetime_walk_type(t, false);
            }
        }
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            lifetime_walk_expr(value);
            lifetime_walk_expr(body);
        }
        Expr::LowIndirectCall { callee, args, .. } => {
            lifetime_walk_expr(callee);
            for a in args {
                lifetime_walk_expr(a);
            }
        }
        Expr::LowTypeApplication {
            callee, type_arg, ..
        } => {
            lifetime_walk_expr(callee);
            lifetime_walk_type(type_arg, false);
        }
        Expr::LowHostCall { args, .. }
        | Expr::LowModuleCall { args, .. }
        | Expr::LowQualifiedModuleCall { args, .. }
        | Expr::LowClosureCall { args, .. } => {
            for a in args {
                lifetime_walk_expr(a);
            }
        }
        Expr::LowAbsurdCall { value_arg, .. } => {
            lifetime_walk_expr(value_arg);
        }
        Expr::LowNewtypeCtor { payload, .. } | Expr::LowQualifiedNewtypeMember { payload, .. } => {
            lifetime_walk_expr(payload);
        }
        Expr::LowNewtypeProj { target, .. } => {
            lifetime_walk_expr(target);
        }
        Expr::LowCpsProjectorApply {
            receiver,
            continuation,
            ..
        } => {
            lifetime_walk_expr(receiver);
            lifetime_walk_expr(continuation);
        }
        Expr::EnrichedTuple { items, .. } => {
            for it in items {
                lifetime_walk_expr(it);
            }
        }
        Expr::EnrichedInject { payload, .. } => {
            lifetime_walk_expr(payload);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            lifetime_walk_expr(scrutinee);
            for arm in arms.iter_mut() {
                lifetime_walk_expr(&mut arm.body);
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            lifetime_walk_expr(cond);
            lifetime_walk_expr(then_branch);
            lifetime_walk_expr(else_branch);
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            lifetime_walk_expr(target);
        }
        Expr::LowBoundRef { .. }
        | Expr::LowHostFnValueRef { .. }
        | Expr::LowModuleFnValueRef { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Path { ext, .. } => match *ext {},
        Expr::Call { ext, .. } => match *ext {},
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
    }
}

/// Does `name` (a fn-typed value-param) appear in `body` in any
/// position other than the callee of a direct call? Returns `true`
/// when it does (conservatively assume escape) and `false` only
/// when every use is a `name(args)` invocation — the closure is
/// consumed inline and doesn't outlive the current frame.
fn param_escapes_in_body(name: &str, body: &Expr<Routed>) -> bool {
    let mut escapes = false;
    let mut bound: Vec<String> = Vec::new();
    walk_param_escapes(name, body, &mut bound, &mut escapes);
    escapes
}

fn walk_param_escapes(name: &str, e: &Expr<Routed>, bound: &mut Vec<String>, escapes: &mut bool) {
    if *escapes {
        return;
    }
    // A shadowing binder hides `name` in the sub-tree it scopes.
    if bound.iter().any(|n| n == name) {
        return;
    }
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::LowBoundRef { name: n, .. } => {
            if n == name {
                // A bare reference outside an immediate-call
                // callee position — `f` escapes.
                *escapes = true;
            }
        }
        Expr::LowClosureCall { name: n, args, .. } => {
            // Direct invocation pattern `name(args, …)`. The
            // callee position itself is fine for `Stack`; the args
            // need full walking because `name` could appear inside
            // an arg.
            if n != name {
                // The call isn't of `name` — its callee position
                // is some other binder. No effect on `name` here.
            }
            for a in args {
                walk_param_escapes(name, a, bound, escapes);
            }
        }
        Expr::LowIndirectCall { callee, args, .. } => {
            // An indirect call's callee is an expression. Treat
            // `name` in the callee position as fine — it's an
            // invocation pattern equivalent to `name(args, …)`.
            // Anything else (the callee is a complex expression
            // that mentions `name` nested inside) goes through
            // the regular walk, which catches `LowBoundRef`.
            if let Expr::LowBoundRef { name: n, .. } = callee.as_ref() {
                if n == name {
                    // OK — direct invocation, not escape.
                } else {
                    walk_param_escapes(name, callee, bound, escapes);
                }
            } else {
                walk_param_escapes(name, callee, bound, escapes);
            }
            for a in args {
                walk_param_escapes(name, a, bound, escapes);
            }
        }
        Expr::LowTypeApplication { callee, .. } => {
            if !matches!(callee.as_ref(), Expr::LowBoundRef { name: n, .. } if n == name) {
                walk_param_escapes(name, callee, bound, escapes);
            }
        }
        Expr::LowHostCall { args, .. }
        | Expr::LowModuleCall { args, .. }
        | Expr::LowQualifiedModuleCall { args, .. } => {
            for a in args {
                walk_param_escapes(name, a, bound, escapes);
            }
        }
        Expr::LowAbsurdCall { value_arg, .. } => {
            walk_param_escapes(name, value_arg, bound, escapes);
        }
        Expr::LowNewtypeCtor { payload, .. } | Expr::LowQualifiedNewtypeMember { payload, .. } => {
            walk_param_escapes(name, payload, bound, escapes);
        }
        Expr::LowNewtypeProj { target, .. } => {
            walk_param_escapes(name, target, bound, escapes);
        }
        Expr::LowCpsProjectorApply {
            receiver,
            continuation,
            ..
        } => {
            walk_param_escapes(name, receiver, bound, escapes);
            walk_param_escapes(name, continuation, bound, escapes);
        }
        Expr::FnExpr {
            sig, body, caps, ..
        } => {
            // An inner closure that *captures* `name` is an
            // escape: the closure may outlive the current frame, or,
            // even when it doesn't, crossing the closure boundary
            // forces the captured value to be cloned / boxed.
            if caps.captured_from.iter().any(|c| c == name) {
                *escapes = true;
                return;
            }
            // The inner closure doesn't capture `name`; just
            // walk its body in case it shadows / rebinds.
            let pre = bound.len();
            for p in &sig.params {
                if let SignatureParam::Value(v) = p {
                    bound.push(v.name.clone());
                }
            }
            walk_param_escapes(name, body, bound, escapes);
            bound.truncate(pre);
        }
        Expr::Let {
            name: n,
            value,
            body,
            ..
        } => {
            walk_param_escapes(name, value, bound, escapes);
            let added = if n != name {
                bound.push(n.clone());
                true
            } else {
                false
            };
            walk_param_escapes(name, body, bound, escapes);
            if added {
                bound.pop();
            }
        }
        Expr::Seq { value, body, .. } => {
            walk_param_escapes(name, value, bound, escapes);
            walk_param_escapes(name, body, bound, escapes);
        }
        Expr::EnrichedTuple { items, .. } => {
            for it in items {
                walk_param_escapes(name, it, bound, escapes);
            }
        }
        Expr::EnrichedInject { payload, .. } => {
            walk_param_escapes(name, payload, bound, escapes);
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            walk_param_escapes(name, scrutinee, bound, escapes);
            for EnrichedArm { param, body, .. } in arms {
                let added = if param != name {
                    bound.push(param.clone());
                    true
                } else {
                    false
                };
                walk_param_escapes(name, body, bound, escapes);
                if added {
                    bound.pop();
                }
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_param_escapes(name, cond, bound, escapes);
            walk_param_escapes(name, then_branch, bound, escapes);
            walk_param_escapes(name, else_branch, bound, escapes);
        }
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            walk_param_escapes(name, target, bound, escapes);
        }
        Expr::LowHostFnValueRef { .. }
        | Expr::LowModuleFnValueRef { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        Expr::Path { ext, .. } => match *ext {},
        Expr::Call { ext, .. } => match *ext {},
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
    }
}

// ---- tests --------------------------------------------------------------

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::Surface;
    use std::path::PathBuf;

    /// Drive a module source through the full pipeline up to Routed
    /// (the input to `annotate_escapes`).
    fn pipeline_to_routed(src: &str) -> Package<Routed> {
        let module: Module<Surface> = crate::pass::parser::parse(src).expect("parse");
        let module = crate::pass::desugar::desugar_module(module).expect("desugar");
        let (lowered, _) = crate::pass::label_elab::elaborate_package(
            vec![(PathBuf::from("test.kio"), module)],
            None,
        )
        .expect("elaborate");
        let lowered_module = lowered.into_iter().next().expect("one module").1;
        let elabs = crate::pass::typecheck_full::Elaborations::new();
        let normalized = crate::pass::alpha_normalize::normalize_module(&lowered_module);
        let prime = crate::pass::substitute::substitute_module(normalized.module(), &elabs);
        let scope = crate::pass::resolve::TopLevelScope::build(&prime).expect("scope");
        let package = Package::<crate::ast::Prime>::from_parts(
            std::iter::once((
                "test".to_owned(),
                ModuleEntry::<crate::ast::Prime> {
                    file_path: PathBuf::from("test.kio"),
                    module: prime,
                    scope,
                },
            ))
            .collect(),
            None,
        );
        let enriched = crate::pass::structural_recovery::recover_package_for_test(&package);
        crate::pass::route::route_package(&enriched)
    }

    use crate::ast::Module;

    /// Find the body of the named fn in the (single-)module package.
    fn fn_body<'a>(pkg: &'a Package<Routed>, name: &str) -> &'a Expr<Routed> {
        let m = pkg.modules().next().expect("one module").1;
        let d = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == name => Some(d),
                _ => None,
            })
            .expect("named fn");
        &d.body
    }

    /// Find the deepest `Expr::FnExpr` reachable from `e` —
    /// recurse into the body first; only if no nested closure
    /// shows up there does this return the current FnExpr's caps.
    /// The simple tests below construct a single closure under
    /// `caller`'s body so "deepest" and "the closure" coincide;
    /// tests that care about a specific level destructure the AST
    /// explicitly instead of going through this helper.
    fn find_fn_expr(e: &Expr<Routed>) -> Option<&Capabilities> {
        match e {
            Expr::FnExpr { caps, body, .. } => {
                let outer = caps;
                find_fn_expr(body).or(Some(outer))
            }
            Expr::Let { value, body, .. } => find_fn_expr(value).or_else(|| find_fn_expr(body)),
            Expr::Seq { value, body, .. } => find_fn_expr(value).or_else(|| find_fn_expr(body)),
            Expr::LowIndirectCall { callee, args, .. } => {
                find_fn_expr(callee).or_else(|| args.iter().find_map(find_fn_expr))
            }
            Expr::LowTypeApplication { callee, .. } => find_fn_expr(callee),
            _ => None,
        }
    }

    /// A closure that *captures* an outer-scope binder records it in
    /// `captured_from`.
    #[test]
    fn captured_from_lists_outer_binder() {
        let src = "module x; fn caller(y: .) -> (. -> .) { .(x: .) -> . { y } }";
        let pkg = pipeline_to_routed(src);
        let pkg = annotate_escapes(pkg);
        let body = fn_body(&pkg, "caller");
        let caps = find_fn_expr(body).expect("closure");
        assert_eq!(caps.captured_from, vec!["y".to_owned()]);
    }

    /// Shadowing: an inner closure whose own value-param has the same
    /// name as an outer binder doesn't list the outer name in its
    /// `captured_from` (the inner param shadows the outer reference).
    #[test]
    fn shadowing_omits_captured() {
        let src = "module x; fn caller(y: .) -> (. -> .) { .(y: .) -> . { y } }";
        let pkg = pipeline_to_routed(src);
        let pkg = annotate_escapes(pkg);
        let body = fn_body(&pkg, "caller");
        let caps = find_fn_expr(body).expect("closure");
        assert!(
            caps.captured_from.is_empty(),
            "shadowed binder: captured_from is empty, got {:?}",
            caps.captured_from
        );
    }

    /// Nested closure capturing an outer-outer binder: the
    /// grandparent's value-param flows through both the parent
    /// closure's outer scope and the inner closure's outer scope.
    /// The outer captures `y`; the inner captures `y` too (the
    /// parent's parameter list doesn't list `y`, so it remains an
    /// outer-scope reference for the inner closure).
    #[test]
    fn nested_closure_captures_grandparent() {
        let src = "module x; \
                   fn caller[A](y: A) -> (A -> (. -> A)) { \
                       .(a: A) -> (. -> A) { .(b: .) -> A { y } } \
                   }";
        let pkg = pipeline_to_routed(src);
        let pkg = annotate_escapes(pkg);
        let body = fn_body(&pkg, "caller");
        // Outer closure: captures `y` (referenced by the inner body).
        let Expr::FnExpr {
            caps: outer_caps,
            body: outer_body,
            ..
        } = body
        else {
            panic!("expected outer FnExpr, got {:?}", body);
        };
        assert_eq!(outer_caps.captured_from, vec!["y".to_owned()]);
        // Inner closure: captures `y` (the grandparent's param).
        let Expr::FnExpr {
            caps: inner_caps, ..
        } = outer_body.as_ref()
        else {
            panic!("expected inner FnExpr, got {:?}", outer_body);
        };
        assert_eq!(inner_caps.captured_from, vec!["y".to_owned()]);
    }

    // ---- Lifetime annotation tests ----------------------------

    /// Pull the first fn-typed value-param's lifetime off the
    /// named fn's signature. Returns `None` when no such param
    /// exists.
    fn fn_param_lifetime(pkg: &Package<Routed>, name: &str) -> Option<Lifetime> {
        let m = pkg.modules().next().expect("one module").1;
        let d = m.module.items.iter().find_map(|it| match it {
            Item::FnDef(d) if d.name == name => Some(d),
            _ => None,
        })?;
        for p in &d.sig.params {
            if let SignatureParam::Value(vp) = p
                && let Some(Type::Function { caps, .. }) = vp.ty.as_ref()
            {
                return Some(caps.lifetime);
            }
        }
        None
    }

    /// A fn-typed param that's called immediately (no escape) is
    /// marked `Stack`.
    #[test]
    fn non_escaping_fn_param_marked_stack() {
        let src = "module x; \
                   fn caller(k: . -> .) -> . { k(()) }";
        let pkg = pipeline_to_routed(src);
        let pkg = annotate_lifetime(pkg);
        let lifetime = fn_param_lifetime(&pkg, "caller").expect("fn-typed param");
        assert_eq!(
            lifetime,
            Lifetime::Stack,
            "param used only in direct call should be Stack"
        );
    }

    /// A fn-typed param captured by an inner closure is marked
    /// `Heap` (the param value flows beyond the outer frame).
    #[test]
    fn captured_fn_param_marked_heap() {
        let src = "module x; \
                   fn caller(k: . -> .) -> (. -> .) { \
                       .(s: .) -> . { k(s) } \
                   }";
        let pkg = pipeline_to_routed(src);
        let pkg = annotate_escapes(pkg);
        let pkg = annotate_lifetime(pkg);
        let lifetime = fn_param_lifetime(&pkg, "caller").expect("fn-typed param");
        assert_eq!(
            lifetime,
            Lifetime::Heap,
            "param captured by escaping inner closure should be Heap"
        );
    }
}

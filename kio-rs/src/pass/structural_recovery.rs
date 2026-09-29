//! Structural recovery: `Module<Prime>` → `Module<Enriched>`.
//!
//! On the backend-facing build route this is a post-validation, pre-emit pass:
//! its `Module<Prime>` input has passed standalone Kio' checking and
//! statement-spine canonicalization. The `Prime` marker itself guarantees the
//! AST shape, not that a direct caller ran that validation route.
//! Products, sums, matches and conditionals are expressed through
//! right-leaning chains of the
//! intrinsics (`__pair__` / `__fst__` / `__snd__` /
//! `__left__` / `__right__` / `__either__` /
//! `__if_then_else__`). That shape is faithful but verbose: every
//! conditional is an IIFE-wrapped thunk pair, every n-arm match a
//! nest of `__either__` dispatch wrappers.
//!
//! This pass collapses those chains into the **enriched IR** — the
//! seven `Expr::Enriched*` variants — for backend-neutral optimization and
//! later routing. Host emitters consume the resulting `Module<Routed>`;
//! `backends::kio_prime` bypasses recovery and consumes `Module<Prime>` because
//! its job is to re-emit Kio' source.
//!
//! ## Recovered patterns
//!
//! - **Product construction** — a right-leaning `__pair__` chain
//!   becomes a flat n-ary [`Expr::EnrichedTuple`]. Chains flatten
//!   maximally: every `__pair__` level contributes one slot to
//!   the resulting `items: Vec<…>`. A chain that breaks at a
//!   non-`__pair__` interior recovers up to the break (the
//!   broken-off subtree becomes the final slot and recovers on its
//!   own).
//! - **Product access** — a `__fst__` / `__snd__` call becomes an
//!   [`Expr::EnrichedProject`] over the n-ary spine of its target
//!   product. A `__snd__` whose `R` type-arg is itself a product
//!   returns a sub-product — recovery materializes it as an
//!   [`Expr::EnrichedTuple`] of per-slot projections, rebuilding
//!   the sub-product explicitly under the n-ary `Prod_<spine(R)>`
//!   mint.
//! - **Sum injection** — a `__left__` / `__right__` chain that
//!   names a single positional variant in a statically-known sum
//!   becomes an [`Expr::EnrichedInject`] with n-ary
//!   `variant` / `variants`. A `__right__` whose payload is a
//!   *dynamic* sub-sum value (`R` is a sum, but the payload is not
//!   itself an injection) recovers as an [`Expr::EnrichedMatch`]
//!   that re-injects each variant of the inner sum into the outer
//!   n-ary at the appropriate offset — `Sum(A, R)` mints one
//!   `Sum_<1+spine(R)>` shape, so the dynamic-right needs a
//!   per-variant re-tag rather than a single inject.
//! - **Sum elimination** — a right-leaning `__either__` chain
//!   becomes an n-arm [`Expr::EnrichedMatch`]. The chain follows the
//!   right handler `fr = .(r) { __either__(r, …) }` while its
//!   scrutinee is exactly the bound `r`; it stops otherwise. Computed
//!   handler expressions are staged before dispatch, in source-call
//!   order, while literal functions and paths remain direct values.
//! - **Conditional** — `__if_then_else__(c, then, else)` becomes an
//!   [`Expr::EnrichedConditional`]. A literal `.() { body }` thunk
//!   has its wrapper stripped; a thunk expression that is neither a
//!   literal function nor a path is staged before branch selection
//!   and applied via `Call(thunk, [])` only on the selected branch.
//!   Staging keeps the intrinsic call's eager left-to-right argument
//!   evaluation separate from its lazy thunk invocation.
//!
//! The pass is **total over saturated, directly recoverable structural
//! roots**: every matching `__pair__`, `__fst__` / `__snd__`, `__left__` /
//! `__right__`, `__either__`, or `__if_then_else__` root collapses to one of
//! the seven `Enriched*` nodes. Residual applications — including partially
//! applied and type-only application stages — and indirect uses do not match
//! that root shape. They retain their generic core shape as children recover
//! recursively. `__absurd__` has no structural form and likewise remains for
//! routing as a call. Recovery is observationally a no-op; it only gives codegen
//! a higher-level shape to lower.
//!
//! Type-arg awareness: on the backend-facing route, standalone validation's
//! completion bake has made every intrinsic type-argument slot an explicit
//! `CallArg::Type` (with no `Type::Infer` placeholder), whether the artifact
//! came through full-Kio substitution or direct Kio' checking. The `__fst__` / `__snd__` /
//! `__left__` / `__right__` chains therefore carry the `R` type argument from
//! which recovery reads `arity` / `variants` through [`type_arg_type`].

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::ast::{
    CallArg, Enriched, EnrichedArm, Expr, FnDef, Item, Meta, Module, PackageFile, PathSegment,
    Prime, RecordField, Type, convert_host_fn, convert_host_type, convert_meta, convert_newtype,
    convert_signature, convert_type, convert_type_alias,
};
use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry};
use crate::span::Span;

/// Recover a whole typed package into the enriched-IR phase. Each
/// module body, every exported fn body in the package file, and the
/// per-module name-resolution scope are carried across; only the
/// expression bodies actually change shape.
///
/// **Parallelism.** Each worker builds the small exact newtype view for its
/// module, then runs independently. Each module gets its own [`Recoverer`]
/// and [`RecoveryCtx`], so the per-walk `fresh` counter is local — the
/// synthetic `__rec<N>` names it mints are bound inside a single
/// match-arm scope, so per-module counters don't collide. The
/// `BTreeMap` collect preserves input order, so the resulting
/// package's module iteration is byte-identical to the serial walk.
pub fn recover_package(package: &Package<Prime>) -> Package<Enriched> {
    // Per-module recovery via rayon. Each module resolves its own exact
    // local/selective/qualified newtype surface and mutates only its per-walk
    // `Recoverer` (just a `fresh` counter for synthetic param names). The
    // input order from `package.modules()` (a `BTreeMap`) is
    // preserved by the collect, so the output `BTreeMap` matches
    // the serial walk's iteration order.
    let entries: Vec<(String, ModuleEntry<Enriched>)> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(path, entry)| {
                let ctx = RecoveryCtx::for_module(package, &entry.module);
                let mut r = Recoverer::new(&ctx);
                (
                    (*path).to_owned(),
                    ModuleEntry::<Enriched> {
                        file_path: entry.file_path.clone(),
                        module: r.recover_module(&entry.module),
                        scope: entry.scope.clone(),
                    },
                )
            })
            .collect();
    let modules: BTreeMap<String, ModuleEntry<Enriched>> = entries.into_iter().collect();
    let package_file = package
        .package_file()
        .map(|entry| PackageFileEntry::<Enriched> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: Recoverer::new(&RecoveryCtx::default())
                .recover_package_file(&entry.package_file),
        });
    Package::<Enriched>::from_parts(modules, package_file)
}

/// Recover a single typed module using a pre-computed package catalogue.
/// Only entries whose consumer identity is this module enter the local
/// recovery context. Used by the
/// [`crate::cache::enriched`] miss path so the per-module work is
/// equivalent whether the cache is enabled or not.
///
/// `cli`-gated: it takes the cache layer's `PackageKeyInputs` and is
/// only ever called from `cache::enriched`, so it compiles only when the
/// `cache` module is in the build.
#[cfg(feature = "cli")]
pub fn recover_module_with_package_inputs(
    module: &Module<Prime>,
    inputs: &crate::cache::enriched::PackageKeyInputs,
) -> Module<Enriched> {
    let mut ctx = RecoveryCtx::default();
    let module_path = module.path.segments.join("/");
    for (scoped_name, input) in &inputs.newtype_fields {
        if scoped_name.module_path() != module_path {
            continue;
        }
        let Some(member) = inputs.newtype_members.get(scoped_name) else {
            continue;
        };
        assert_eq!(
            input.identity(),
            member.identity(),
            "enriched package inputs select one exact nominal per lexical head"
        );
        let info = NewtypeFieldInfo {
            field_name: input.ffi_key().as_str().to_owned(),
            projector_name: member.projector().as_str().to_owned(),
        };
        ctx.newtype_fields.insert(scoped_name.visible_head(), info);
    }
    Recoverer::new(&ctx).recover_module(module)
}

/// Recover a single package file. Companion to
/// [`recover_module_with_package_inputs`].
pub fn recover_package_file(export: &PackageFile<Prime>) -> PackageFile<Enriched> {
    let ctx = RecoveryCtx::default();
    Recoverer::new(&ctx).recover_package_file(export)
}

/// One slot of an in-progress `__pair__` chain flatten. Newtype
/// constructor calls retain their wrappers as ordinary slot values.
struct TupleSlot {
    name: Option<String>,
    value: Expr<Enriched>,
}

/// One non-immediate value argument lifted out of a recovered
/// structural dispatch. The enclosing `let` spine preserves the
/// original intrinsic call's eager, left-to-right argument evaluation;
/// the bound path can then appear inside a lazy match arm or branch
/// without moving the argument expression there.
struct StagedValue {
    name: String,
    span: Span,
    value: Expr<Enriched>,
}

/// Per-newtype lookup info used by record recovery. The exact nominal
/// scope selects this entry; its FFI key names the recovered field and
/// its projector member name recognizes the wrapper crossing.
#[derive(Debug, Clone)]
struct NewtypeFieldInfo {
    /// The newtype's FFI key (see
    /// [`crate::ast::Newtype::ffi_key`]) — the field name on the
    /// recovered `EnrichedRecord` / `EnrichedFieldGet`.
    field_name: String,
    /// The projector's member name.
    projector_name: String,
}

/// Per-module read-only side table consulted during recovery. A visible
/// one- or two-segment newtype head maps to the exact local, selectively
/// imported, or qualified-imported declaration selected by that module's
/// ordinary nominal scope.
#[derive(Default)]
struct RecoveryCtx {
    newtype_fields: crate::pass::resolve::ResolvedNewtypeHeadMap<NewtypeFieldInfo>,
}

impl RecoveryCtx {
    fn for_module(package: &Package<Prime>, module: &Module<Prime>) -> Self {
        let mut ctx = RecoveryCtx::default();
        crate::pass::resolve::for_each_resolved_newtype_member_head(
            package,
            module,
            |visible_head, _owner, declaration| {
                let info = NewtypeFieldInfo {
                    field_name: declaration.ffi_key().to_owned(),
                    projector_name: declaration.projector.name.clone(),
                };
                ctx.newtype_fields.insert(visible_head, info);
            },
        );
        ctx
    }
}

/// Per-walk state. The only mutable bit is `fresh`, a monotonic
/// counter for the synthetic binding and parameter names the `__either__`,
/// `__if_then_else__`, and sub-product recoveries mint. The `ctx` borrow points at the
/// module-scoped read-only [`RecoveryCtx`].
///
/// A fresh `Recoverer` is minted per module in
/// [`recover_package`]'s per-module fan-out: the `fresh` counter is
/// scoped to one module's recovery, so two modules' counters never
/// alias. Every synthetic name (`__rec<N>`) is lexically bound within
/// the recovered expression that contains it, so per-module counters give
/// the same observable shape as a shared counter would.
struct Recoverer<'a> {
    fresh: u32,
    ctx: &'a RecoveryCtx,
}

impl<'a> Recoverer<'a> {
    /// Mint a per-walk recoverer that shares a module-scoped
    /// [`RecoveryCtx`]. The `fresh` counter starts at zero;
    /// synthetic names produced inside this walker do not interfere
    /// with any other walker's counter.
    fn new(ctx: &'a RecoveryCtx) -> Self {
        Recoverer { fresh: 0, ctx }
    }

    /// A fresh compiler-reserved identifier. The `__` prefix is
    /// reserved (user identifiers cannot begin with it), so these
    /// never collide with a source name; the counter keeps them
    /// distinct from each other.
    fn fresh_name(&mut self) -> String {
        let n = self.fresh;
        self.fresh += 1;
        format!("__rec{n}")
    }

    // ---- container walks (phase rebrand + body recovery) ------------

    fn recover_module(&mut self, m: &Module<Prime>) -> Module<Enriched> {
        Module {
            path: m.path.clone(),
            imports: m.imports.clone(),
            items: m.items.iter().map(|it| self.recover_item(it)).collect(),
            meta: convert_meta(&m.meta),
            doc: m.doc.clone(),
        }
    }

    fn recover_item(&mut self, item: &Item<Prime>) -> Item<Enriched> {
        match item {
            Item::FnDef(d) => Item::FnDef(self.recover_fn_def(d)),
            // `TypeAlias` (type-side only at Prime) and `Newtype` carry
            // no expression bodies — a plain phase rebrand suffices.
            Item::TypeAlias(a) => Item::TypeAlias(convert_type_alias::<Prime, Enriched>(a)),
            Item::Newtype(d) => Item::Newtype(convert_newtype::<Prime, Enriched>(d)),
            Item::TypeRecGroup(group) => {
                Item::TypeRecGroup(crate::ast::convert_type_rec_group::<Prime, Enriched>(group))
            }
            // Host items are signature-only and opaque in Kio' — a plain
            // phase rebrand suffices, no body to recover.
            Item::HostType(h) => Item::HostType(convert_host_type::<Prime, Enriched>(h)),
            Item::HostFn(h) => Item::HostFn(convert_host_fn::<Prime, Enriched>(h)),
            // Statically uninhabited at Prime.
            Item::LiteralAlias(_, ext) => match *ext {},
            Item::Labels(_, ext) => match *ext {},
            Item::LabelForward(_, ext) => match *ext {},
            Item::Equiv(_, ext) => match *ext {},
            Item::Elaborator(_, ext) => match *ext {},
            Item::Op(_, ext) => match *ext {},
            Item::VariadicOperator(_, ext) => match *ext {},
            Item::RecGroup(_, ext) => match *ext {},
        }
    }

    fn recover_fn_def(&mut self, d: &FnDef<Prime>) -> FnDef<Enriched> {
        FnDef {
            vis: d.vis.clone(),
            purity: (),
            name: d.name.clone(),
            sig: convert_signature::<Prime, Enriched>(&d.sig),
            ret: convert_type::<Prime, Enriched>(&d.ret),
            // Post-Prime passes consume Kio' shape alone. Both Prime
            // and Enriched narrow `ret_elided` to `()`, so there is no
            // surface-provenance to carry.
            ret_elided: (),
            body: self.recover_expr(&d.body),
            meta: convert_meta(&d.meta),
            doc: d.doc.clone(),
        }
    }

    fn recover_package_file(&mut self, e: &PackageFile<Prime>) -> PackageFile<Enriched> {
        PackageFile {
            name: e.name.clone(),
            build: e.build.clone(),
            // `bridge` is phase-independent — carried verbatim.
            bridge: e.bridge.clone(),
            meta: convert_meta(&e.meta),
        }
    }

    // ---- expression recovery ----------------------------------------

    fn recover_call_arg(&mut self, a: &CallArg<Prime>) -> CallArg<Enriched> {
        match a {
            CallArg::Type(t) => CallArg::Type(convert_type::<Prime, Enriched>(t)),
            CallArg::Value(v) => CallArg::Value(self.recover_expr(v)),
        }
    }

    /// Recover one expression. Non-`Call` nodes are a structural
    /// phase rebrand (mirroring `convert_expr`); a `Call` whose
    /// callee is a recoverable intrinsic chain root becomes the
    /// matching enriched node, and every other `Call` recurses.
    fn recover_expr(&mut self, e: &Expr<Prime>) -> Expr<Enriched> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Path {
                occurrence: _,
                segments,
                meta,
                ext: _,
            } => Expr::Path {
                occurrence: Default::default(),
                segments: segments.clone(),
                meta: convert_meta(meta),
                ext: (),
            },
            Expr::Call {
                occurrence: _,
                callee,
                args,
                meta,
                ext: _,
            } => {
                if let Some(name) = single_segment_callee(callee) {
                    let recovered = match name {
                        "__pair__" => self.recover_mk_pair(e),
                        "__fst__" | "__snd__" => self.recover_projection(e),
                        "__left__" | "__right__" => self.recover_injection(e),
                        "__either__" => self.recover_either(e),
                        "__if_then_else__" => self.recover_conditional(e),
                        _ => None,
                    };
                    if let Some(node) = recovered {
                        return node;
                    }
                }
                // A projector call on an ordinary newtype whose
                // argument is itself a positional access folds to an
                // `EnrichedFieldGet`. Opportunistic: only fires when
                // the recovered payload is an `EnrichedProject`;
                // otherwise the call recovers structurally below.
                if let Some(node) = self.recover_field_get(e) {
                    return node;
                }
                // Not a recoverable chain root — recurse structurally.
                Expr::Call {
                    occurrence: Default::default(),
                    callee: Box::new(self.recover_expr(callee)),
                    args: args.iter().map(|a| self.recover_call_arg(a)).collect(),
                    meta: convert_meta(meta),
                    ext: (),
                }
            }
            Expr::FnExpr {
                occurrence: _,
                sig,
                ret_ty,
                body,
                meta,
                caps: _,
            } => Expr::FnExpr {
                occurrence: Default::default(),
                sig: convert_signature::<Prime, Enriched>(sig),
                ret_ty: ret_ty.as_ref().map(convert_type::<Prime, Enriched>),
                body: Box::new(self.recover_expr(body)),
                meta: convert_meta(meta),
                caps: (),
            },
            Expr::Let {
                occurrence: _,
                name,
                name_span,
                ty,
                pattern: (),
                value,
                body,
                meta,
            } => {
                let value = Box::new(self.recover_expr(value));
                let body = Box::new(self.recover_expr(body));
                match flatten_bound_tuple_tail(name, value, body) {
                    Ok(flattened) => flattened,
                    Err((value, body)) => Expr::Let {
                        occurrence: Default::default(),
                        name: name.clone(),
                        name_span: *name_span,
                        ty: ty.as_ref().map(convert_type::<Prime, Enriched>),
                        pattern: (),
                        value,
                        body,
                        meta: convert_meta(meta),
                    },
                }
            }
            Expr::Seq {
                occurrence: _,
                value,
                body,
                meta,
            } => Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(self.recover_expr(value)),
                body: Box::new(self.recover_expr(body)),
                meta: convert_meta(meta),
            },
            Expr::Unit {
                occurrence: _,
                meta,
            } => Expr::Unit {
                occurrence: Default::default(),
                meta: convert_meta(meta),
            },
            Expr::StrLit {
                occurrence: _,
                value,
                annotation,
                meta,
            } => Expr::StrLit {
                occurrence: Default::default(),
                value: value.clone(),
                annotation: convert_type::<Prime, Enriched>(annotation),
                meta: convert_meta(meta),
            },
            Expr::IntLit {
                occurrence: _,
                digits,
                annotation,
                meta,
            } => Expr::IntLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: convert_type::<Prime, Enriched>(annotation),
                meta: convert_meta(meta),
            },
            Expr::FloatLit {
                occurrence: _,
                digits,
                annotation,
                meta,
            } => Expr::FloatLit {
                occurrence: Default::default(),
                digits: digits.clone(),
                annotation: convert_type::<Prime, Enriched>(annotation),
                meta: convert_meta(meta),
            },
            Expr::BoolLit {
                occurrence: _,
                value,
                annotation,
                meta,
            } => Expr::BoolLit {
                occurrence: Default::default(),
                value: *value,
                annotation: convert_type::<Prime, Enriched>(annotation),
                meta: convert_meta(meta),
            },
            // Surface-only / elaboration-bearing variants are
            // statically uninhabited at `Prime`.
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
            // The enriched variants are this pass's *output*; they
            // cannot appear in its `Prime` input.
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

    // ---- pattern: product construction ------------------------------

    /// Recover the complete right-leaning `__pair__` chain into one
    /// n-ary product, preserving each nominal constructor boundary.
    fn recover_mk_pair(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let span = e.span();
        // The outermost `__pair__(A, R, …)` pins the whole product
        // type: `Product(A, R)` where `R` is the right spine of the
        // rest of the chain.
        let call = classify_intrinsic("__pair__", call_args(e)?)?;
        let synth_ty = synth_ty_from_pair(&call, SpineKind::Product, span)?;
        let slots = self.flatten_mk_pair_chain(e)?;
        if slots.iter().all(|s| s.name.is_some()) {
            let fields = slots
                .into_iter()
                .map(|s| RecordField {
                    name: s.name.expect("checked all-named above"),
                    value: s.value,
                    meta: Meta::new(span),
                })
                .collect();
            return Some(Expr::EnrichedRecord {
                occurrence: Default::default(),
                fields,
                synth_ty,
                meta: Meta::new(span),
                ext: (),
            });
        }
        Some(Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: slots.into_iter().map(|s| s.value).collect(),
            synth_ty,
            meta: Meta::new(span),
            ext: (),
        })
    }

    /// Walk a right-leaning `__pair__` chain rooted at `e`,
    /// returning one [`TupleSlot`] per component in order. Each slot
    /// carries the recovered value. Returns `None` when `e` is not a
    /// canonical `__pair__` call — the recursive case stops when
    /// `rest` is anything else (a host call, a value, a `__left__`,
    /// etc.), and that last component is pushed as a single slot by
    /// the caller.
    fn flatten_mk_pair_chain(&mut self, e: &Expr<Prime>) -> Option<Vec<TupleSlot>> {
        // `classify_intrinsic` matches arg-list shapes only — it
        // doesn't verify the callee name — so an explicit check is
        // required here. Otherwise any 2-arg `Call` (e.g.
        // `mul_i32(acc, base)`) would be mis-classified as a
        // `__pair__` and flatten through.
        if single_segment_callee(call_callee(e)?)? != "__pair__" {
            return None;
        }
        // N-ary recovery: walk the full right-leaning `__pair__`
        // chain, producing one slot per level. A chain
        // `__pair__(_, _, x, __pair__(_, _, y, z))` recovers as
        // a flat `[x, y, z]` matching the n-ary `Prod_<3>` shape
        // minted by `right_spine_walk_product` /
        // `canonical_walk`. A non-`__pair__` interior breaks the
        // chain: the broken-off subtree becomes the final slot and
        // recovers on its own.
        let mut slots: Vec<TupleSlot> = Vec::new();
        let mut cur = e;
        loop {
            let is_mk_pair = call_callee(cur)
                .and_then(single_segment_callee)
                .is_some_and(|n| n == "__pair__");
            if !is_mk_pair {
                slots.push(self.recover_slot(cur));
                break;
            }
            let values = classify_intrinsic("__pair__", call_args(cur)?)?.value_part;
            let a = value_arg(values, 0)?;
            let rest = value_arg(values, 1)?;
            slots.push(self.recover_slot(a));
            cur = rest;
        }
        Some(slots)
    }

    /// Recover one item of a product chain into a [`TupleSlot`].
    fn recover_slot(&mut self, e: &Expr<Prime>) -> TupleSlot {
        // Keep a newtype constructor call (`Foo.mk(payload)`) as a
        // plain item so every backend emits it through its own
        // newtype-constructor path. Lifting it here — stripping the
        // wrap and naming the slot — would force the Rust backend to
        // restore the wrap (its newtype struct is a real type, not
        // runtime-erased like JS), and a lifted slot cannot be
        // distinguished from a bare value of the newtype's type.
        TupleSlot {
            name: None,
            value: self.recover_expr(e),
        }
    }

    /// If `e` is a projector application on an ordinary newtype
    /// using `Foo.get(payload)` or `alias.Foo.get(payload)`, return the
    /// FFI field key and payload value. Type-args preceding the value-arg
    /// are accepted and skipped.
    fn try_lift_newtype_projector<'e>(
        &self,
        e: &'e Expr<Prime>,
    ) -> Option<(String, &'e Expr<Prime>)> {
        let Expr::Call { callee, args, .. } = e else {
            return None;
        };
        let Expr::Path { segments, .. } = &**callee else {
            return None;
        };
        let (member, visible_head) = segments.split_last()?;
        let info = self.ctx.newtype_fields.get(visible_head)?;
        if member.as_str() != info.projector_name {
            return None;
        }
        let mut value_args = args.iter().filter_map(|a| match a {
            CallArg::Value(v) => Some(v),
            CallArg::Type(_) => None,
        });
        let payload = value_args.next()?;
        if value_args.next().is_some() {
            return None;
        }
        Some((info.field_name.clone(), payload))
    }

    // ---- pattern: product access ------------------------------------

    /// A `__fst__` / `__snd__` call → [`Expr::EnrichedProject`] over
    /// the n-ary spine of its target product, or — when the call is
    /// `__snd__(A, R, p)` with `R` itself a product — an
    /// [`Expr::EnrichedTuple`] of per-slot projections that rebuilds
    /// the sub-product `R` from `p`'s tail slots.
    ///
    /// The outer product's spine count is `1 + product_spine_count(R)`:
    /// `__fst__` projects to slot 0; `__snd__` with atomic `R`
    /// projects to slot 1 of a 2-spine outer. `__snd__` with a
    /// product `R` materializes the sub-product as a fresh n-ary
    /// tuple whose items are the outer's slots `1..n` projected
    /// directly — no raw `__snd__` call survives, and the recovered
    /// tree carries the same observable shape (the sub-product `R`
    /// at the source level) under the n-ary `Prod_<spine(R)>` mint.
    fn recover_projection(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let name = single_segment_callee(call_callee(e)?)?;
        let call = classify_intrinsic(name, call_args(e)?)?;
        let span = e.span();
        let r_ty = type_arg_type(call.type_part.get(1)?);
        let inner = value_arg(call.value_part, 0)?;
        let target_ty = synth_ty_from_pair(&call, SpineKind::Product, span)?;
        let arity = 1 + product_spine_count(r_ty.as_ref());
        match name {
            "__fst__" => Some(Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(self.recover_expr(inner)),
                index: 0,
                arity,
                target_ty,
                meta: Meta::new(span),
                ext: (),
            }),
            "__snd__" => {
                if matches!(r_ty.as_ref(), Type::Product { .. }) {
                    // Sub-product case: `__snd__` returns the right
                    // tail `R`, itself an n-ary product. Materialize
                    // it as an `EnrichedTuple` over per-slot
                    // projections of the outer product — slots
                    // `1..arity`. The resulting shape's synth_ty is
                    // `R` (n-ary `Prod_<spine(R)>`). Codegen reads
                    // each slot from one bound copy of the original
                    // `p` and packs them into a fresh n-ary mint.
                    let target_name = self.fresh_name();
                    let recovered_inner = self.recover_expr(inner);
                    let tail_ty = convert_type::<Prime, Enriched>(r_ty.as_ref());
                    let items: Vec<Expr<Enriched>> = (1..arity)
                        .map(|i| Expr::EnrichedProject {
                            occurrence: Default::default(),
                            target: Box::new(Expr::Path {
                                occurrence: Default::default(),
                                segments: vec![PathSegment::new(target_name.clone(), span)],
                                meta: Meta::new(span),
                                ext: (),
                            }),
                            index: i,
                            arity,
                            target_ty: target_ty.clone(),
                            meta: Meta::new(span),
                            ext: (),
                        })
                        .collect();
                    let tuple = Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: tail_ty,
                        meta: Meta::new(span),
                        ext: (),
                    };
                    Some(Expr::Let {
                        occurrence: Default::default(),
                        name: target_name,
                        name_span: span,
                        ty: None,
                        pattern: (),
                        value: Box::new(recovered_inner),
                        body: Box::new(tuple),
                        meta: Meta::new(span),
                    })
                } else {
                    // R is atomic / nominal / aliased / function /
                    // Unit / Bottom — `__snd__` projects to a
                    // single slot. `arity` is `1 + 1 = 2` by the
                    // `product_spine_count` opaqueness rule.
                    Some(Expr::EnrichedProject {
                        occurrence: Default::default(),
                        target: Box::new(self.recover_expr(inner)),
                        index: 1,
                        arity,
                        target_ty,
                        meta: Meta::new(span),
                        ext: (),
                    })
                }
            }
            _ => None,
        }
    }

    // ---- pattern: named-field access --------------------------------

    /// A projector call `Foo.foo(<positional access>)` /
    /// `alias.Foo.foo(<positional access>)` on an ordinary newtype whose
    /// argument, after recovery, is an [`Expr::EnrichedProject`] →
    /// [`Expr::EnrichedFieldGet`]. The synthetic field-get reads the
    /// named field directly from the projection's own `target` (the
    /// outer record value), carrying both the field name and the
    /// projection's positional layout (`index` / `arity` /
    /// `target_ty`) so a positional target lowers it as the same
    /// positional access while a native-record backend lowers it as
    /// `<target>.<field_name>`.
    ///
    /// Opportunistic: returns `None` when `e` is not a projector call
    /// on a resolved newtype member, or when the recovered payload is not
    /// a positional access. The caller then recovers the call
    /// structurally, leaving a plain projector call.
    fn recover_field_get(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let (field_name, payload) = self.try_lift_newtype_projector(e)?;
        let recovered_payload = self.recover_expr(payload);
        match recovered_payload {
            Expr::EnrichedProject {
                occurrence: _,
                target,
                index,
                arity,
                target_ty,
                meta,
                ext: (),
            } => Some(Expr::EnrichedFieldGet {
                occurrence: Default::default(),
                target,
                field_name,
                index,
                arity,
                target_ty,
                meta,
                ext: (),
            }),
            _ => None,
        }
    }

    // ---- pattern: sum injection -------------------------------------

    /// Recover a `__left__` / `__right__` root into structural sum IR.
    /// A static chain that names a single positional variant in a
    /// statically-known sum collapses to one flattened
    /// [`Expr::EnrichedInject`] with `{variant, variants}`. When a
    /// `__right__` carries a dynamic sub-sum value, recovery instead
    /// produces an [`Expr::EnrichedMatch`] whose arms re-tag each inner
    /// variant into the outer sum.
    fn recover_injection(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let span = e.span();
        // The outermost `__left__(A, R, …)` / `__right__(A, R, …)`
        // pins the whole sum type — `Sum(A, R)` — regardless of
        // whether the chain bottoms out statically or hands off to
        // `recover_dynamic_right` for a sub-sum payload.
        let name = single_segment_callee(call_callee(e)?)?;
        let outer_call = classify_intrinsic(name, call_args(e)?)?;
        let synth_ty = synth_ty_from_pair(&outer_call, SpineKind::Sum, span)?;
        if let Some((payload, variant, variants)) = self.static_injection_parts(e) {
            return Some(Expr::EnrichedInject {
                occurrence: Default::default(),
                payload: Box::new(payload),
                variant,
                variants,
                synth_ty,
                meta: Meta::new(span),
                ext: (),
            });
        }
        // Static-chain recovery failed: this must be a `__right__` whose
        // payload is a dynamic sub-sum value (`R` is structurally a sum,
        // while the payload is not itself an injection). Recover it as an
        // `EnrichedMatch` that re-tags every inner variant. `__left__` always
        // succeeds on the static path, and a non-injection call returns
        // `None` at both stages, preserving the `recover_expr` fallthrough.
        self.recover_dynamic_right(e, synth_ty)
    }

    /// `(recovered payload, flattened variant index, flattened variant
    /// count)` for a *static* injection chain rooted at `e`. The chain
    /// bottoms out at either a `__left__` (variant 0 of its
    /// immediate sum) or a `__right__` whose `R` is non-sum (the
    /// last variant of its immediate sum). Returns `None` when `e` is
    /// not an injection call, or when the chain breaks at a
    /// `__right__` whose `R` is a sum but whose payload is not
    /// itself an injection — the dynamic-sub-sum case
    /// [`Self::recover_dynamic_right`] handles instead.
    fn static_injection_parts(
        &mut self,
        e: &Expr<Prime>,
    ) -> Option<(Expr<Enriched>, usize, usize)> {
        let name = single_segment_callee(call_callee(e)?)?;
        let call = classify_intrinsic(name, call_args(e)?)?;
        let r_ty = type_arg_type(call.type_part.get(1)?);
        let inner = value_arg(call.value_part, 0)?;
        let variants = 1 + sum_spine_count(r_ty.as_ref());
        match name {
            "__left__" => Some((self.recover_expr(inner), 0, variants)),
            "__right__" => {
                if matches!(r_ty.as_ref(), Type::Sum { .. }) {
                    // `inner : R`, and `R` is structurally a sum. The
                    // static path only fires when `inner` is itself an
                    // injection into `R` — then its variant index,
                    // shifted up by one, is ours. A dynamic payload
                    // bails here and falls to `recover_dynamic_right`.
                    let (payload, inner_variant, _) = self.static_injection_parts(inner)?;
                    Some((payload, inner_variant + 1, variants))
                } else {
                    // `R` is atomic / nominal / aliased: `inner` is
                    // the raw payload of the last variant.
                    Some((self.recover_expr(inner), 1, variants))
                }
            }
            _ => None,
        }
    }

    /// A `__right__` whose payload is a *dynamic* sub-sum (`R` is
    /// structurally a sum, but the payload is not itself an injection
    /// call) → an [`Expr::EnrichedMatch`] over the recovered payload
    /// that re-injects each variant of `R` into the outer n-ary sum
    /// at the appropriate offset.
    ///
    /// Under n-ary minting, `Sum(A, R)` mints one `Sum_<N>` with
    /// `N = 1 + sum_spine_count(R)` positional variants, not a
    /// binary outer wrapping a separately-minted `R`. A dynamic
    /// payload `x : R` therefore cannot be expressed as a single
    /// `EnrichedInject` — its variants must be re-tagged into the
    /// outer's `_1`, `_2`, … positions. The recovered dispatch does
    /// that in terms of nodes the n-ary emit already supports.
    /// Returns `None` when `e` is not a `__right__` with sum-typed
    /// `R` — keeping the function a targeted fallback, not a
    /// catch-all.
    fn recover_dynamic_right(
        &mut self,
        e: &Expr<Prime>,
        synth_ty: Type<Enriched>,
    ) -> Option<Expr<Enriched>> {
        let name = single_segment_callee(call_callee(e)?)?;
        if name != "__right__" {
            return None;
        }
        let call = classify_intrinsic(name, call_args(e)?)?;
        let r_ty = type_arg_type(call.type_part.get(1)?);
        if !matches!(r_ty.as_ref(), Type::Sum { .. }) {
            return None;
        }
        let inner = value_arg(call.value_part, 0)?;
        let span = e.span();
        let inner_recovered = self.recover_expr(inner);
        let inner_variants = sum_spine_count(r_ty.as_ref());
        let outer_variants = 1 + inner_variants;
        let scrutinee_ty = convert_type::<Prime, Enriched>(r_ty.as_ref());
        // Re-inject every variant of `R` into the outer at offset 1.
        // The arm-parameter name doesn't collide with anything in
        // scope: it's bound only inside its arm body, and the body
        // immediately re-injects it. Using a fresh leading-underscore
        // name keeps the recovered tree readable and consistent
        // across arms.
        let arms: Vec<EnrichedArm<Enriched>> = (0..inner_variants)
            .map(|i| {
                let param = format!("__r{i}");
                let payload = Expr::Path {
                    occurrence: Default::default(),
                    segments: vec![PathSegment::new(param.clone(), span)],
                    meta: Meta::new(span),
                    ext: (),
                };
                let body = Expr::EnrichedInject {
                    occurrence: Default::default(),
                    payload: Box::new(payload),
                    variant: i + 1,
                    variants: outer_variants,
                    synth_ty: synth_ty.clone(),
                    meta: Meta::new(span),
                    ext: (),
                };
                EnrichedArm {
                    param,
                    body,
                    meta: Meta::new(span),
                }
            })
            .collect();
        Some(Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(inner_recovered),
            arms,
            scrutinee_ty,
            result_ty: synth_ty,
            meta: Meta::new(span),
            ext: (),
        })
    }

    // ---- pattern: sum elimination -----------------------------------

    /// A right-leaning `__either__` chain → an n-arm
    /// [`Expr::EnrichedMatch`].
    fn recover_either(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let span = e.span();
        // `__either__(A, R, Res, …)` pins both the scrutinee's sum
        // type (`Sum(A, R)` — same right-spine shape as the
        // injection chain that produces it) and the result type
        // (`Res`).
        let call = classify_intrinsic("__either__", call_args(e)?)?;
        let left_ty = type_arg_type(call.type_part.first()?);
        let right_ty = type_arg_type(call.type_part.get(1)?);
        let scrutinee_ty = synth_ty_from_pair(&call, SpineKind::Sum, span)?;
        let result_ty =
            convert_type::<Prime, Enriched>(type_arg_type(call.type_part.get(2)?).as_ref());
        let values = call.value_part;
        let scrutinee = value_arg(values, 0)?;
        let fl = value_arg(values, 1)?;
        let fr = value_arg(values, 2)?;

        // `__either__` is an ordinary strict call: both handler *values*
        // are evaluated before dispatch, even though only the selected
        // handler is invoked. `EnrichedMatch` makes invocation lazy, so
        // any computed handler must be lifted out first. Once lifting is
        // needed, a computed scrutinee is lifted ahead of the handlers to
        // retain the call's exact left-to-right order. Literal functions
        // and paths are already immediate values and need no binding.
        let stages_handlers = !handler_is_immediate_value(fl) || !handler_is_immediate_value(fr);
        let mut staged = Vec::new();
        let recovered_scrutinee = if stages_handlers {
            self.recover_or_stage_call_value(scrutinee, &mut staged)
        } else {
            self.recover_expr(scrutinee)
        };
        let fl_binding = if stages_handlers && !handler_is_immediate_value(fl) {
            Some(self.stage_call_value(fl, &mut staged))
        } else {
            None
        };
        let fr_binding = if stages_handlers && !handler_is_immediate_value(fr) {
            Some(self.stage_call_value(fr, &mut staged))
        } else {
            None
        };
        let mut arms = Vec::new();
        self.collect_either_arms(
            (fl, fl_binding.as_deref()),
            (fr, fr_binding.as_deref()),
            left_ty.as_ref(),
            right_ty.as_ref(),
            &mut arms,
        );
        let recovered = Expr::EnrichedMatch {
            occurrence: Default::default(),
            scrutinee: Box::new(recovered_scrutinee),
            arms,
            scrutinee_ty,
            result_ty,
            meta: Meta::new(span),
            ext: (),
        };
        Some(wrap_staged_values(staged, recovered))
    }

    /// Append the arms of one `__either__` level to `arms`. `fl`
    /// always contributes one arm; `fr` either continues the chain
    /// (when it is `.(r) { __either__(r, …) }` dispatching on its
    /// own bound `r`) or contributes a final arm.
    fn collect_either_arms(
        &mut self,
        (fl, fl_binding): (&Expr<Prime>, Option<&str>),
        (fr, fr_binding): (&Expr<Prime>, Option<&str>),
        left_ty: &Type<Prime>,
        right_ty: &Type<Prime>,
        arms: &mut Vec<EnrichedArm<Enriched>>,
    ) {
        let left = self.arm_from_handler(fl, fl_binding, left_ty);
        arms.push(left);
        if let Some((r_param, r_body)) = as_unary_fn(fr)
            && let Some(inner_args) = either_chain_args(r_body, r_param)
            && handler_is_immediate_value(inner_args.0)
            && handler_is_immediate_value(inner_args.1)
            && let Type::Sum {
                left: inner_left,
                right: inner_right,
                ..
            } = right_ty
        {
            // `fr`'s body is `__either__(r, fl', fr')` — the chain
            // continues with the same scrutinee. A computed inner
            // handler stops flattening: recovering the inner call in
            // the outer right arm keeps its eager factory evaluation
            // conditional on entering that nested call.
            self.collect_either_arms(
                (inner_args.0, None),
                (inner_args.1, None),
                inner_left,
                inner_right,
                arms,
            );
        } else {
            let right = self.arm_from_handler(fr, fr_binding, right_ty);
            arms.push(right);
        }
    }

    /// One match arm from a handler expression. A literal `.(p) { … }`
    /// contributes its bound name and body directly; any other
    /// function-typed expression gets a fresh parameter and a body
    /// that applies the handler to it.
    fn arm_from_handler(
        &mut self,
        handler: &Expr<Prime>,
        binding: Option<&str>,
        payload_ty: &Type<Prime>,
    ) -> EnrichedArm<Enriched> {
        let span = handler.span();
        if let Some(binding) = binding {
            let param = self.fresh_name();
            EnrichedArm {
                body: Expr::Call {
                    occurrence: Default::default(),
                    callee: Box::new(enriched_local_path(binding, span)),
                    args: vec![CallArg::Value(enriched_local_path(&param, span))],
                    meta: Meta::new(span),
                    ext: (),
                },
                param,
                meta: Meta::new(span),
            }
        } else if let Some((param, body)) = as_unary_fn(handler) {
            EnrichedArm {
                param: param.to_owned(),
                body: self.recover_expr(body),
                meta: Meta::new(span),
            }
        } else if matches!(payload_ty, Type::Unit { .. })
            && let Some(body) = as_nullary_fn(handler)
        {
            EnrichedArm {
                param: self.fresh_name(),
                body: self.recover_expr(body),
                meta: Meta::new(span),
            }
        } else {
            let param = self.fresh_name();
            let callee = self.recover_expr(handler);
            EnrichedArm {
                body: Expr::Call {
                    occurrence: Default::default(),
                    callee: Box::new(callee),
                    args: vec![CallArg::Value(enriched_local_path(&param, span))],
                    meta: Meta::new(span),
                    ext: (),
                },
                param,
                meta: Meta::new(span),
            }
        }
    }

    // ---- pattern: conditional ---------------------------------------

    /// `__if_then_else__(c, then_thunk, else_thunk)` →
    /// [`Expr::EnrichedConditional`]. A literal `.() { body }` thunk
    /// has its wrapper stripped (`body` becomes the branch
    /// expression); a thunk that is neither a literal function nor a
    /// path is first staged eagerly and then applied via
    /// `Call(bound_thunk, [])` only on the selected branch.
    fn recover_conditional(&mut self, e: &Expr<Prime>) -> Option<Expr<Enriched>> {
        let span = e.span();
        // `__if_then_else__(Res, …)` pins the branches' shared result
        // type `Res`.
        let call = classify_intrinsic("__if_then_else__", call_args(e)?)?;
        let result_ty =
            convert_type::<Prime, Enriched>(type_arg_type(call.type_part.first()?).as_ref());
        let values = call.value_part;
        let cond = value_arg(values, 0)?;
        let then_thunk = value_arg(values, 1)?;
        let else_thunk = value_arg(values, 2)?;
        let stages_thunks =
            !handler_is_immediate_value(then_thunk) || !handler_is_immediate_value(else_thunk);
        let mut staged = Vec::new();
        let recovered_cond = if stages_thunks {
            self.recover_or_stage_call_value(cond, &mut staged)
        } else {
            self.recover_expr(cond)
        };
        let then_binding = if stages_thunks && !handler_is_immediate_value(then_thunk) {
            Some(self.stage_call_value(then_thunk, &mut staged))
        } else {
            None
        };
        let else_binding = if stages_thunks && !handler_is_immediate_value(else_thunk) {
            Some(self.stage_call_value(else_thunk, &mut staged))
        } else {
            None
        };
        let recovered = Expr::EnrichedConditional {
            occurrence: Default::default(),
            cond: Box::new(recovered_cond),
            then_branch: Box::new(self.recover_branch_thunk(then_thunk, then_binding.as_deref())),
            else_branch: Box::new(self.recover_branch_thunk(else_thunk, else_binding.as_deref())),
            result_ty,
            meta: Meta::new(span),
            ext: (),
        };
        Some(wrap_staged_values(staged, recovered))
    }

    /// One branch of an `__if_then_else__`. A literal `.() { body }`
    /// contributes `recover(body)` directly; any other expression `f`
    /// of type `. -> A` contributes `f()` — i.e., `Call(recover(f),
    /// [])`. Both fit the `EnrichedConditional`'s "evaluate exactly
    /// the taken side" contract under the ternary lowering.
    fn recover_branch_thunk(
        &mut self,
        thunk: &Expr<Prime>,
        binding: Option<&str>,
    ) -> Expr<Enriched> {
        if let Some(binding) = binding {
            let span = thunk.span();
            return Expr::Call {
                occurrence: Default::default(),
                callee: Box::new(enriched_local_path(binding, span)),
                args: Vec::new(),
                meta: Meta::new(span),
                ext: (),
            };
        }
        if let Some(body) = as_nullary_fn(thunk) {
            return self.recover_expr(body);
        }
        let span = thunk.span();
        Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(self.recover_expr(thunk)),
            args: Vec::new(),
            meta: Meta::new(span),
            ext: (),
        }
    }

    /// Recover one original value argument, lifting it to a fresh
    /// binding unless it is already an obviously immediate value.
    /// Used only when a later computed handler/thunk must itself be
    /// lifted before dispatch; the accumulated vector is therefore
    /// also the original call's evaluation order.
    fn recover_or_stage_call_value(
        &mut self,
        value: &Expr<Prime>,
        staged: &mut Vec<StagedValue>,
    ) -> Expr<Enriched> {
        if expr_is_immediate_value(value) {
            self.recover_expr(value)
        } else {
            let name = self.stage_call_value(value, staged);
            enriched_local_path(&name, value.span())
        }
    }

    /// Lift a computed call argument and return its fresh binding name.
    fn stage_call_value(&mut self, value: &Expr<Prime>, staged: &mut Vec<StagedValue>) -> String {
        let name = self.fresh_name();
        staged.push(StagedValue {
            name: name.clone(),
            span: value.span(),
            value: self.recover_expr(value),
        });
        name
    }
}

/// Function literals and paths are already callable values: evaluating them
/// cannot run a handler factory. Keeping them direct avoids manufacturing a
/// binding for the common surface-lowered `match!` / `if` shape.
fn handler_is_immediate_value(value: &Expr<Prime>) -> bool {
    matches!(value, Expr::FnExpr { .. } | Expr::Path { .. })
}

/// Values whose evaluation is observably inert. When a later call operand is
/// staged, these may remain in the recovered dispatch without changing the
/// relative order of any effectful computation.
fn expr_is_immediate_value(value: &Expr<Prime>) -> bool {
    matches!(
        value,
        Expr::Path { .. }
            | Expr::FnExpr { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. }
    )
}

fn enriched_local_path(name: &str, span: Span) -> Expr<Enriched> {
    Expr::Path {
        occurrence: Default::default(),
        segments: vec![PathSegment::new(name.to_owned(), span)],
        meta: Meta::new(span),
        ext: (),
    }
}

/// Wrap a recovered dispatch in the staged operands' source-call order. The
/// reverse fold makes the first argument the outermost (and therefore first
/// evaluated) binding.
fn wrap_staged_values(staged: Vec<StagedValue>, body: Expr<Enriched>) -> Expr<Enriched> {
    staged
        .into_iter()
        .rev()
        .fold(body, |body, staged| Expr::Let {
            occurrence: Default::default(),
            name: staged.name,
            name_span: staged.span,
            ty: None,
            pattern: (),
            value: Box::new(staged.value),
            body: Box::new(body),
            meta: Meta::new(staged.span),
        })
}

// ---- expression-shape helpers ---------------------------------------

type TupleTailFlattenResult = Result<Expr<Enriched>, (Box<Expr<Enriched>>, Box<Expr<Enriched>>)>;

/// Flatten a single-use tuple tail that statement-spine
/// canonicalization lifted into a surrounding `let`.
///
/// `let tail = Tuple([b, c]); Tuple([a, tail])` becomes
/// `Tuple([a, b, c])` when every item before `tail` is an immediate
/// pure value and contains no other free use of the binding. The
/// bound tuple type must equal the outer explicit-product suffix, and
/// both item counts must match their right-spine widths; opaque alias
/// suffixes therefore remain bound. The tuple RHS originally evaluates
/// before the outer tuple, but moving it after pure atoms cannot
/// reorder an observable effect. The bound path is the only use
/// removed, so the rewrite neither duplicates work nor changes
/// capture. When the consumer is the next let-bound tuple rather than
/// the terminal tuple, the same proof merges into that RHS and repeats
/// down the statement spine.
fn flatten_bound_tuple_tail(
    name: &str,
    value: Box<Expr<Enriched>>,
    body: Box<Expr<Enriched>>,
) -> TupleTailFlattenResult {
    if can_merge_bound_tuple_tail(name, value.as_ref(), body.as_ref()) {
        return Ok(merge_bound_tuple_tail(value, body));
    }

    let can_merge_into_next_binding = match (value.as_ref(), body.as_ref()) {
        (
            Expr::EnrichedTuple { .. },
            Expr::Let {
                name: next_name,
                value: next_value,
                body: next_body,
                ..
            },
        ) => {
            can_merge_bound_tuple_tail(name, value.as_ref(), next_value)
                && !has_free_value_ref(next_body, name, next_name == name)
        }
        _ => false,
    };
    if !can_merge_into_next_binding {
        return Err((value, body));
    }

    let Expr::Let {
        occurrence: _,
        name: next_name,
        name_span,
        ty,
        pattern,
        value: next_value,
        body: next_body,
        meta,
    } = *body
    else {
        unreachable!("next tuple binding shape checked above")
    };
    let next_value = merge_bound_tuple_tail(value, next_value);
    Ok(flatten_tuple_let_spine(Expr::Let {
        occurrence: Default::default(),
        name: next_name,
        name_span,
        ty,
        pattern,
        value: Box::new(next_value),
        body: next_body,
        meta,
    }))
}

fn flatten_tuple_let_spine(e: Expr<Enriched>) -> Expr<Enriched> {
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
    match flatten_bound_tuple_tail(&name, value, body) {
        Ok(flattened) => flattened,
        Err((value, body)) => Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty,
            pattern,
            value,
            body,
            meta,
        },
    }
}

fn can_merge_bound_tuple_tail(name: &str, value: &Expr<Enriched>, body: &Expr<Enriched>) -> bool {
    match (value, body) {
        (
            Expr::EnrichedTuple {
                items: tail_items,
                synth_ty: tail_ty,
                ..
            },
            Expr::EnrichedTuple {
                items,
                synth_ty: outer_ty,
                ..
            },
        ) if matches!(tail_ty, Type::Product { .. })
            && matches!(outer_ty, Type::Product { .. }) =>
        {
            items.split_last().is_some_and(|(tail, prefix)| {
                let tail_arity = Type::right_spine_product(tail_ty).len();
                let outer_arity = Type::right_spine_product(outer_ty).len();
                is_local_path(tail, name)
                    && tail_items.len() == tail_arity
                    && prefix.len() + tail_items.len() == outer_arity
                    && explicit_product_suffix(outer_ty, prefix.len())
                        .is_some_and(|suffix| structural_type_equal(suffix, tail_ty))
                    && prefix.iter().all(is_immediate_pure_value)
                    && prefix
                        .iter()
                        .all(|item| !has_free_value_ref(item, name, false))
            })
        }
        _ => false,
    }
}

fn merge_bound_tuple_tail(value: Box<Expr<Enriched>>, body: Box<Expr<Enriched>>) -> Expr<Enriched> {
    let Expr::EnrichedTuple {
        items: tail_items, ..
    } = *value
    else {
        unreachable!("tuple-tail flattening shape checked above")
    };
    let Expr::EnrichedTuple {
        occurrence: _,
        mut items,
        synth_ty,
        meta,
        ext,
    } = *body
    else {
        unreachable!("tuple-tail flattening body checked above")
    };
    items.pop();
    items.extend(tail_items);
    Expr::EnrichedTuple {
        occurrence: Default::default(),
        items,
        synth_ty,
        meta,
        ext,
    }
}

fn is_local_path(e: &Expr<Enriched>, name: &str) -> bool {
    matches!(e, Expr::Path { segments, .. } if segments.len() == 1 && segments[0].name == name)
}

fn is_immediate_pure_value(e: &Expr<Enriched>) -> bool {
    matches!(
        e,
        Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. }
            | Expr::FnExpr { .. }
    )
}

fn explicit_product_suffix(
    mut ty: &Type<Enriched>,
    prefix_arity: usize,
) -> Option<&Type<Enriched>> {
    for _ in 0..prefix_arity {
        let Type::Product { right, .. } = ty else {
            return None;
        };
        ty = right;
    }
    Some(ty)
}

fn structural_type_equal(a: &Type<Enriched>, b: &Type<Enriched>) -> bool {
    match (a, b) {
        (
            Type::Path {
                segments: a_segments,
                args: a_args,
                ..
            },
            Type::Path {
                segments: b_segments,
                args: b_args,
                ..
            },
        ) => {
            a_segments.len() == b_segments.len()
                && a_segments
                    .iter()
                    .zip(b_segments)
                    .all(|(a, b)| a.name == b.name)
                && a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args)
                    .all(|(a, b)| structural_type_equal(a, b))
        }
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: a_param,
                ret: a_ret,
                ..
            },
            Type::Function {
                param: b_param,
                ret: b_ret,
                ..
            },
        ) => structural_type_equal(a_param, b_param) && structural_type_equal(a_ret, b_ret),
        (
            Type::Product {
                left: a_left,
                right: a_right,
                ..
            },
            Type::Product {
                left: b_left,
                right: b_right,
                ..
            },
        )
        | (
            Type::Sum {
                left: a_left,
                right: a_right,
                ..
            },
            Type::Sum {
                left: b_left,
                right: b_right,
                ..
            },
        ) => structural_type_equal(a_left, b_left) && structural_type_equal(a_right, b_right),
        (
            Type::Forall {
                param: a_param,
                body: a_body,
                ..
            },
            Type::Forall {
                param: b_param,
                body: b_body,
                ..
            },
        ) => a_param.name == b_param.name && structural_type_equal(a_body, b_body),
        _ => false,
    }
}

fn has_free_value_ref(e: &Expr<Enriched>, name: &str, shadowed: bool) -> bool {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Path { segments, .. } => !shadowed && segments.len() == 1 && segments[0].name == name,
        Expr::Call { callee, args, .. } => {
            has_free_value_ref(callee, name, shadowed)
                || args.iter().any(|arg| match arg {
                    CallArg::Value(value) => has_free_value_ref(value, name, shadowed),
                    CallArg::Type(_) => false,
                })
        }
        Expr::FnExpr { sig, body, .. } => {
            let shadows = sig.params.iter().any(|param| {
                matches!(param, crate::ast::SignatureParam::Value(param) if param.name == name)
            });
            has_free_value_ref(body, name, shadowed || shadows)
        }
        Expr::Let {
            name: binder,
            value,
            body,
            ..
        } => {
            has_free_value_ref(value, name, shadowed)
                || has_free_value_ref(body, name, shadowed || binder == name)
        }
        Expr::Seq { value, body, .. } => {
            has_free_value_ref(value, name, shadowed) || has_free_value_ref(body, name, shadowed)
        }
        Expr::EnrichedTuple { items, .. } => items
            .iter()
            .any(|item| has_free_value_ref(item, name, shadowed)),
        Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
            has_free_value_ref(target, name, shadowed)
        }
        Expr::EnrichedInject { payload, .. } => has_free_value_ref(payload, name, shadowed),
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            has_free_value_ref(scrutinee, name, shadowed)
                || arms
                    .iter()
                    .any(|arm| has_free_value_ref(&arm.body, name, shadowed || arm.param == name))
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            has_free_value_ref(cond, name, shadowed)
                || has_free_value_ref(then_branch, name, shadowed)
                || has_free_value_ref(else_branch, name, shadowed)
        }
        Expr::EnrichedRecord { fields, .. } => fields
            .iter()
            .any(|field| has_free_value_ref(&field.value, name, shadowed)),
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => false,
        Expr::Tuple { ext, .. }
        | Expr::FnPlaceholder { ext, .. }
        | Expr::LabelValue { ext, .. }
        | Expr::RowLet { ext, .. }
        | Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. }
        | Expr::Ufcs { ext, .. }
        | Expr::OpChain { ext, .. }
        | Expr::RecCall { ext, .. }
        | Expr::LowHostCall { ext, .. }
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

/// The callee of an `Expr::Call`, or `None` for any other shape.
fn call_callee(e: &Expr<Prime>) -> Option<&Expr<Prime>> {
    match e {
        Expr::Call { callee, .. } => Some(callee),
        _ => None,
    }
}

/// The argument list of an `Expr::Call`, or `None` for any other shape.
fn call_args(e: &Expr<Prime>) -> Option<&[CallArg<Prime>]> {
    match e {
        Expr::Call { args, .. } => Some(args),
        _ => None,
    }
}

/// The single path segment of a `callee` that is a one-segment
/// `Expr::Path` — the shape every intrinsic callee has.
fn single_segment_callee(callee: &Expr<Prime>) -> Option<&str> {
    match callee {
        Expr::Path { segments, .. } if segments.len() == 1 => Some(segments[0].as_str()),
        _ => None,
    }
}

/// `(type_arity, value_arity)` of a recoverable intrinsic — the
/// scheme shapes mirror `backends::js::emit::Intrinsic`. `__absurd__` is
/// deliberately absent: it has no enriched structural form, so it is
/// not a recovery target.
fn intrinsic_arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "__pair__" => (2, 2),
        "__fst__" | "__snd__" => (2, 1),
        "__left__" | "__right__" => (2, 1),
        "__either__" => (3, 3),
        "__if_then_else__" => (1, 3),
        _ => return None,
    })
}

/// An intrinsic call's argument list split into its leading
/// type-arg slots and trailing value-arg slots. See
/// [`classify_intrinsic`].
struct IntrinsicCall<'a> {
    /// The leading type-arg slots — empty when the call is in the
    /// fully-inferred shape (every `[type-param]` elided).
    type_part: &'a [CallArg<Prime>],
    /// The trailing value-arg slots; guaranteed all `CallArg::Value`.
    value_part: &'a [CallArg<Prime>],
}

/// Classify an intrinsic call's argument list into its type-arg and
/// value-arg parts, exactly the way `backends::js::emit::emit_intrinsic_call`
/// does. An intrinsic call carries either every type-arg slot
/// (`args.len() == type_arity + value_arity`) or none of them
/// (`args.len() == value_arity`, the fully-inferred shape); any
/// other count is not a shape this pass recovers.
fn classify_intrinsic<'a>(name: &str, args: &'a [CallArg<Prime>]) -> Option<IntrinsicCall<'a>> {
    let (type_arity, value_arity) = intrinsic_arity(name)?;
    let skip = if args.len() == type_arity + value_arity {
        type_arity
    } else if args.len() == value_arity {
        0
    } else {
        return None;
    };
    let (type_part, value_part) = args.split_at(skip);
    if value_part.iter().all(|a| matches!(a, CallArg::Value(_))) {
        Some(IntrinsicCall {
            type_part,
            value_part,
        })
    } else {
        None
    }
}

/// The type at a type-argument slot of a Prime intrinsic call.
///
/// Backend-facing Prime has passed standalone checking, whose bake rewrites
/// every accepted type slot to `CallArg::Type`. A direct caller can construct a
/// Prime-shaped value without that validation provenance; the `CallArg::Value`
/// arm covers such a value-shaped type argument when it satisfies the typer's
/// interpretation rule. It delegates to
/// [`crate::pass::typecheck_core::expr_to_type_arg`] rather than defining a
/// second rule.
fn type_arg_type(arg: &CallArg<Prime>) -> Cow<'_, Type<Prime>> {
    match arg {
        CallArg::Type(t) => Cow::Owned(erase_comptime_type_arg(t.clone())),
        CallArg::Value(e) => {
            let ty = crate::pass::typecheck_core::expr_to_type_arg(e).unwrap_or_else(|_| {
                unreachable!(
                    "structural recovery: a value-shaped type-arg slot of a \
                 typechecked intrinsic call failed to reinterpret as a type. \
                 The typer reinterprets and validates every type-arg via \
                 `expr_to_type_arg` before recovery runs — if this fires, an \
                 upstream pass changed the call's argument shape after typecheck."
                )
            });
            Cow::Owned(erase_comptime_type_arg(ty))
        }
    }
}

fn erase_comptime_type_arg(ty: Type<Prime>) -> Type<Prime> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            if args.is_empty()
                && let [single] = segments.as_slice()
                && let Some(builtin) =
                    crate::comptime::ComptimeBuiltin::from_public_name(single.as_str())
                && let Some(erasure) = builtin.runtime_erasure()
            {
                return match erasure {
                    crate::comptime::ComptimeRuntimeErasure::Bottom => Type::Bottom { meta },
                    crate::comptime::ComptimeRuntimeErasure::Unit => Type::Unit { meta },
                };
            }
            Type::Path {
                segments,
                args: args.into_iter().map(erase_comptime_type_arg).collect(),
                meta,
            }
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(erase_comptime_type_arg(*left)),
            right: Box::new(erase_comptime_type_arg(*right)),
            meta,
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(erase_comptime_type_arg(*left)),
            right: Box::new(erase_comptime_type_arg(*right)),
            meta,
        },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => Type::Function {
            param: Box::new(erase_comptime_type_arg(*param)),
            ret: Box::new(erase_comptime_type_arg(*ret)),
            meta,
            abi_arity,
            caps,
        },
        Type::Forall { param, body, meta } => Type::Forall {
            param,
            body: Box::new(erase_comptime_type_arg(*body)),
            meta,
        },
        other => other,
    }
}

/// The expression at value-arg index `i` of a classified intrinsic's
/// `value_part`. `classify_intrinsic` guarantees every slot there is
/// a `CallArg::Value`.
fn value_arg(value_part: &[CallArg<Prime>], i: usize) -> Option<&Expr<Prime>> {
    match value_part.get(i)? {
        CallArg::Value(v) => Some(v),
        CallArg::Type(_) => None,
    }
}

/// A literal `.(p) { body }` — exactly one value parameter, no type
/// parameters — as `(param_name, body)`.
fn as_unary_fn(e: &Expr<Prime>) -> Option<(&str, &Expr<Prime>)> {
    if let Expr::FnExpr { sig, body, .. } = e
        && sig.params.len() == 1
        && let crate::ast::SignatureParam::Value(p) = &sig.params[0]
    {
        return Some((p.name.as_str(), body));
    }
    None
}

/// A literal `.() { body }` — no parameters at all — as `body`.
fn as_nullary_fn(e: &Expr<Prime>) -> Option<&Expr<Prime>> {
    match e {
        Expr::FnExpr { sig, body, .. } if sig.params.is_empty() => Some(body),
        _ => None,
    }
}

/// If `body` is `__either__(scrutinee, fl, fr)` whose scrutinee is
/// exactly the path `param`, return `(fl, fr)` — the inner level's
/// handlers. This is the test for "the `__either__` chain continues".
fn either_chain_args<'a>(
    body: &'a Expr<Prime>,
    param: &str,
) -> Option<(&'a Expr<Prime>, &'a Expr<Prime>)> {
    if single_segment_callee(call_callee(body)?)? != "__either__" {
        return None;
    }
    let values = classify_intrinsic("__either__", call_args(body)?)?.value_part;
    let scrutinee = value_arg(values, 0)?;
    let scrutinee_is_param = matches!(
        scrutinee,
        Expr::Path { segments, .. } if segments.len() == 1 && segments[0] == param
    );
    if !scrutinee_is_param {
        return None;
    }
    Some((value_arg(values, 1)?, value_arg(values, 2)?))
}

/// The number of slots in a product type, walking the `&`
/// right-spine. Nominal types and aliases are opaque (count 1)
/// because the structural canonicalization the Rust backend does
/// likewise treats them as one positional slot — never unfolding a
/// nominal mid-spine. Atomic / function / Unit / Bottom likewise
/// count 1.
fn product_spine_count(t: &Type<Prime>) -> usize {
    match t {
        Type::Product { right, .. } => 1 + product_spine_count(right),
        _ => 1,
    }
}

/// The number of variants in a sum type, walking the `|`
/// right-spine. As with [`product_spine_count`], nominal types and
/// aliases are opaque.
fn sum_spine_count(t: &Type<Prime>) -> usize {
    match t {
        Type::Sum { right, .. } => 1 + sum_spine_count(right),
        _ => 1,
    }
}

/// Build the structural Type the enriched node inhabits, from the
/// recovered intrinsic call's `A, R` type args:
///
/// - `__pair__` / `__fst__` / `__snd__` → `Product(A, R)`
/// - `__left__` / `__right__` → `Sum(A, R)`
fn synth_ty_from_pair(
    call: &IntrinsicCall<'_>,
    kind: SpineKind,
    span: Span,
) -> Option<Type<Enriched>> {
    let a = type_arg_type(call.type_part.first()?);
    let r = type_arg_type(call.type_part.get(1)?);
    let meta = Meta::new(span);
    let left = Box::new(convert_type::<Prime, Enriched>(a.as_ref()));
    let right = Box::new(convert_type::<Prime, Enriched>(r.as_ref()));
    Some(match kind {
        SpineKind::Product => Type::Product { left, right, meta },
        SpineKind::Sum => Type::Sum { left, right, meta },
    })
}

#[derive(Clone, Copy)]
enum SpineKind {
    Product,
    Sum,
}

// ---- container rebrand helpers (no expression bodies) ---------------

// The tests drive the full pipeline (parse → desugar → label_elab →
// resolve → typecheck → substitute) to obtain the actual
// `Package<Prime>` `kio build` would feed to recovery, then recover
// it and inspect the enriched AST. That route is only available
// under the `full` feature.
#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::{CallArg, Item};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_module_file, parse_package_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    /// Derive the on-disk file path for a module from its declared
    /// `module a/b;` path: `a/b.kio`. Per `specs/package.md`
    /// § Module-name rules, the declared segments equal the file's
    /// path relative to the package root, with `.kio` stripped — the
    /// package name is not prepended.
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

    /// Drive the full pipeline on `src` (plus an optional package
    /// file body) and recover the named module to `Enriched`.
    fn recover_one(
        module_path: &str,
        src: &str,
        package_file_src: Option<&str>,
    ) -> Module<Enriched> {
        let parsed = parse(src).expect("parse");
        let package_file = package_file_src.map(|s| {
            parse_package_file(&format!("package pkg;\n{s}"), None).expect("parse package file")
        });
        let mut parsed_modules = vec![(module_file_path(module_path), parsed)];
        parsed_modules.extend(elab_support_entries());
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed_modules, package_file).expect("lower_package");
        let package_file_entry = lowered_package_file.map(|e| PackageFileEntry {
            file_path: PathBuf::from("x.pkg.kio"),
            package_name: "x".to_owned(),
            package_file: e,
        });
        let package = Package::build(Path::new(""), lowered_modules, package_file_entry)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("check_package");
        let recovered = recover_package(&prime);
        recovered
            .module(module_path)
            .expect("module present")
            .module
            .clone()
    }

    /// The body of the first `fn` named `name` in a recovered module.
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

    /// Strip any leading `Let` wrappers off `e` and return the inner
    /// expression. The match elaboration `let`-binds each clause
    /// expression at the outer scope before the decision tree, so
    /// tests targeting the dispatch shape walk through the binding
    /// chain.
    fn strip_lets(mut e: &Expr<Enriched>) -> &Expr<Enriched> {
        while let Expr::Let { body, .. } = e {
            e = body;
        }
        e
    }

    type LeadingBinding<'a> = (&'a str, &'a Expr<Enriched>);

    fn local_path(name: &str) -> Expr<Enriched> {
        let span = Span::new(0, 0);
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::new(name.to_owned(), span)],
            meta: Meta::new(span),
            ext: (),
        }
    }

    fn effect_call(name: &str) -> Expr<Enriched> {
        let span = Span::new(0, 0);
        Expr::Call {
            occurrence: Default::default(),
            callee: Box::new(local_path(name)),
            args: Vec::new(),
            meta: Meta::new(span),
            ext: (),
        }
    }

    fn direct_call_name(e: &Expr<Enriched>) -> Option<&str> {
        let Expr::Call { callee, .. } = e else {
            return None;
        };
        let Expr::Path { segments, .. } = callee.as_ref() else {
            return None;
        };
        let [segment] = segments.as_slice() else {
            return None;
        };
        Some(segment.as_str())
    }

    fn named_type(name: &str) -> Type<Enriched> {
        Type::Path {
            segments: vec![PathSegment::new(name.to_owned(), Span::new(0, 0))],
            args: Vec::new(),
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    fn product_type(names: &[&str]) -> Type<Enriched> {
        let (last, prefix) = names
            .split_last()
            .expect("test product type has at least one slot");
        prefix
            .iter()
            .rev()
            .fold(named_type(last), |right, left| Type::Product {
                left: Box::new(named_type(left)),
                right: Box::new(right),
                meta: Meta::new(Span::new(0, 0)),
            })
    }

    fn enriched_tuple(items: Vec<Expr<Enriched>>, synth_ty: Type<Enriched>) -> Expr<Enriched> {
        let span = Span::new(0, 0);
        Expr::EnrichedTuple {
            occurrence: Default::default(),
            items,
            synth_ty,
            meta: Meta::new(span),
            ext: (),
        }
    }

    fn leading_let_spine<'a>(
        mut e: &'a Expr<Enriched>,
    ) -> (Vec<LeadingBinding<'a>>, &'a Expr<Enriched>) {
        let mut bindings = Vec::new();
        while let Expr::Let {
            name, value, body, ..
        } = e
        {
            bindings.push((name.as_str(), value.as_ref()));
            e = body.as_ref();
        }
        (bindings, e)
    }

    fn resolve_bound_value<'a>(
        e: &'a Expr<Enriched>,
        bindings: &[LeadingBinding<'a>],
        before: usize,
    ) -> (Option<usize>, &'a Expr<Enriched>) {
        let Expr::Path { segments, .. } = e else {
            return (None, e);
        };
        let [segment] = segments.as_slice() else {
            return (None, e);
        };
        let Some(index) = bindings[..before]
            .iter()
            .rposition(|(name, _)| *name == segment.name)
        else {
            return (None, e);
        };
        (Some(index), bindings[index].1)
    }

    fn constructor_slot<'a>(
        slot: &'a Expr<Enriched>,
        bindings: &[LeadingBinding<'a>],
        expected_newtype: &str,
    ) -> (usize, Option<usize>, &'a Expr<Enriched>) {
        let (Some(constructor_index), constructor) =
            resolve_bound_value(slot, bindings, bindings.len())
        else {
            panic!("{expected_newtype}: tuple slot is not wired to a leading binding: {slot:?}");
        };
        let Expr::Call { callee, args, .. } = constructor else {
            panic!("{expected_newtype}: expected constructor Call, got {constructor:?}");
        };
        assert!(
            matches!(callee.as_ref(), Expr::Path { segments, .. }
                if segments.len() == 2
                    && segments[0].name == expected_newtype
                    && segments[1].name == "mk"),
            "{expected_newtype}: constructor callee: {callee:?}"
        );
        let mut value_args = args.iter().filter_map(|arg| match arg {
            CallArg::Value(value) => Some(value),
            CallArg::Type(_) => None,
        });
        let payload = value_args
            .next()
            .unwrap_or_else(|| panic!("{expected_newtype}: constructor has no value payload"));
        assert!(
            value_args.next().is_none(),
            "{expected_newtype}: constructor has more than one value payload: {args:?}"
        );
        let (payload_index, payload) = resolve_bound_value(payload, bindings, constructor_index);
        (constructor_index, payload_index, payload)
    }

    /// Count every node in `e` for which `pred` holds, walking the
    /// whole enriched subtree.
    fn count(e: &Expr<Enriched>, pred: &dyn Fn(&Expr<Enriched>) -> bool) -> usize {
        let here = usize::from(pred(e));
        let children: usize = match e {
            Expr::Call { callee, args, .. } => {
                count(callee, pred)
                    + args
                        .iter()
                        .map(|a| match a {
                            CallArg::Value(v) => count(v, pred),
                            CallArg::Type(_) => 0,
                        })
                        .sum::<usize>()
            }
            Expr::FnExpr { body, .. } => count(body, pred),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                count(value, pred) + count(body, pred)
            }
            Expr::EnrichedTuple { items, .. } => items.iter().map(|i| count(i, pred)).sum(),
            Expr::EnrichedRecord { fields, .. } => {
                fields.iter().map(|f| count(&f.value, pred)).sum()
            }
            Expr::EnrichedProject { target, .. } | Expr::EnrichedFieldGet { target, .. } => {
                count(target, pred)
            }
            Expr::EnrichedInject { payload, .. } => count(payload, pred),
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => count(scrutinee, pred) + arms.iter().map(|a| count(&a.body, pred)).sum::<usize>(),
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => count(cond, pred) + count(then_branch, pred) + count(else_branch, pred),
            _ => 0,
        };
        here + children
    }

    #[test]
    fn bound_tuple_tail_flattens_without_reordering_tail_effects() {
        let value = enriched_tuple(
            vec![effect_call("second"), effect_call("third")],
            product_type(&["B", "C"]),
        );
        let body = enriched_tuple(
            vec![
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(Span::new(0, 0)),
                },
                local_path("tail"),
            ],
            product_type(&["A", "B", "C"]),
        );
        let flattened = flatten_bound_tuple_tail("tail", Box::new(value), Box::new(body))
            .expect("pure prefix permits tuple-tail flattening");
        let Expr::EnrichedTuple { items, .. } = flattened else {
            panic!("expected flattened tuple");
        };
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], Expr::Unit { .. }));
        for (item, expected) in items[1..].iter().zip(["second", "third"]) {
            assert!(
                matches!(item, Expr::Call { callee, .. }
                    if matches!(callee.as_ref(), Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].name == expected)),
                "tail effects remain in order: {item:?}"
            );
        }
    }

    #[test]
    fn bound_tuple_tail_stays_bound_across_effect_or_another_use() {
        let tail = || {
            enriched_tuple(
                vec![effect_call("second"), effect_call("third")],
                product_type(&["B", "C"]),
            )
        };
        let effectful_prefix = enriched_tuple(
            vec![effect_call("first"), local_path("tail")],
            product_type(&["A", "B", "C"]),
        );
        assert!(
            flatten_bound_tuple_tail("tail", Box::new(tail()), Box::new(effectful_prefix)).is_err()
        );

        let repeated_use = enriched_tuple(
            vec![local_path("tail"), local_path("tail")],
            product_type(&["A", "B", "C"]),
        );
        assert!(
            flatten_bound_tuple_tail("tail", Box::new(tail()), Box::new(repeated_use)).is_err()
        );
    }

    #[test]
    fn bound_tuple_tail_does_not_flatten_across_opaque_or_mismatched_suffix() {
        let tail = || {
            enriched_tuple(
                vec![effect_call("second"), effect_call("third")],
                product_type(&["B", "C"]),
            )
        };
        let opaque_tail = enriched_tuple(
            vec![local_path("first"), local_path("tail")],
            Type::Product {
                left: Box::new(named_type("A")),
                right: Box::new(named_type("Tail")),
                meta: Meta::new(Span::new(0, 0)),
            },
        );
        assert!(flatten_bound_tuple_tail("tail", Box::new(tail()), Box::new(opaque_tail)).is_err());

        let mismatched_tail = enriched_tuple(
            vec![local_path("first"), local_path("tail")],
            product_type(&["A", "B", "D"]),
        );
        assert!(
            flatten_bound_tuple_tail("tail", Box::new(tail()), Box::new(mismatched_tail)).is_err()
        );
    }

    #[test]
    fn effectful_outer_prefix_retains_the_tuple_tail_let() {
        let m = recover_one(
            "main",
            "module main; \
             host type Int role(i32); \
             host fn mark(value: Int) -> Int; \
             fn keep(x: Int, y: Int, z: Int) -> (Int & Int & Int) { \
               let tail = (mark(y), mark(z)); \
               (mark(x), tail) \
             }",
            None,
        );
        let body = fn_body(&m, "keep");
        let Expr::Let {
            name, value, body, ..
        } = body
        else {
            panic!("effectful prefix must retain the tuple-tail binding: {body:?}");
        };
        assert!(
            matches!(value.as_ref(), Expr::EnrichedTuple { items, .. } if items.len() == 2),
            "bound tail remains a two-slot tuple: {value:?}"
        );
        let Expr::EnrichedTuple { items, .. } = body.as_ref() else {
            panic!("expected outer tuple body, got {body:?}");
        };
        assert_eq!(items.len(), 2, "opaque tail stays one outer slot");
        assert!(matches!(&items[0], Expr::Call { .. }));
        assert!(is_local_path(&items[1], name));
    }

    /// A right-leaning `__pair__` chain (a tuple literal) recovers
    /// to one flat n-ary `EnrichedTuple`.
    #[test]
    fn tuple_literal_recovers_to_nary_enriched_tuple() {
        let m = recover_one(
            "x/main",
            "module x/main; fn t() -> (. & (. & .)) { ((), (), ()) }",
            None,
        );
        let body = fn_body(&m, "t");
        // N-ary recovery: `((), (), ())` becomes a flat `[(), (), ()]`
        // — one `EnrichedTuple` node carrying all three slots,
        // matching the n-ary `Prod_<3>` shape minted by
        // `right_spine_walk_product`.
        match body {
            Expr::EnrichedTuple { items, .. } => {
                assert_eq!(items.len(), 3, "n-ary 3-tuple flat");
                for (i, it) in items.iter().enumerate() {
                    assert!(matches!(it, Expr::Unit { .. }), "slot {i} is `()`");
                }
            }
            other => panic!("expected EnrichedTuple, got {other:?}"),
        }
        // No bare `__pair__` call survives anywhere in the body.
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__pair__")
            )),
            0,
            "no `__pair__` call should survive recovery"
        );
    }

    /// A `match!` over an n-variant sum recovers to a single n-arm
    /// `EnrichedMatch` — the nested `__either__` chain is collapsed,
    /// not left as nested dispatch nodes.
    #[test]
    fn match_recovers_to_single_n_arm_enriched_match() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn pick[A][B][C](s: (A | (B | C))) -> . { \
               __either__(A, B | C, ., s, \
                 .(_x: A) { () }, \
                 .(rest: B | C) { \
                   __either__(B, C, ., rest, .(_y: B) { () }, .(_z: C) { () }) \
                 }) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        // Exactly one `EnrichedMatch` node, and it carries all three
        // arms — the `__either__` chain collapsed rather than nesting.
        let matches = count(body, &|e| matches!(e, Expr::EnrichedMatch { .. }));
        assert_eq!(matches, 1, "one EnrichedMatch node, not nested");
        // The dispatch tree sits under a `let`-chain that binds each
        // clause expression once before the dispatch — strip those
        // outer lets before asserting on the dispatch node.
        let inner = strip_lets(body);
        match inner {
            Expr::EnrichedMatch { arms, .. } => assert_eq!(arms.len(), 3, "three arms"),
            other => panic!("expected EnrichedMatch, got {other:?}"),
        }
        // No `__either__` call survives.
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__either__")
            )),
            0,
            "no `__either__` call should survive recovery"
        );
    }

    #[test]
    fn either_computed_handlers_stage_before_dispatch_in_call_order() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type A; host type B; \
             host fn scrutinee() -> A | B; \
             host fn left_factory() -> A -> .; \
             host fn right_factory() -> B -> .; \
             fn pick() -> . { \
               __either__(A, B, ., scrutinee(), left_factory(), right_factory()) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        let (bindings, terminal) = leading_let_spine(body);
        assert_eq!(bindings.len(), 3, "scrutinee and both factories are staged");
        assert_eq!(direct_call_name(bindings[0].1), Some("scrutinee"));
        assert_eq!(direct_call_name(bindings[1].1), Some("left_factory"));
        assert_eq!(direct_call_name(bindings[2].1), Some("right_factory"));

        let Expr::EnrichedMatch {
            scrutinee, arms, ..
        } = terminal
        else {
            panic!("expected staged EnrichedMatch, got {terminal:?}");
        };
        assert!(is_local_path(scrutinee, bindings[0].0));
        assert_eq!(arms.len(), 2);
        for (arm, binding) in arms.iter().zip(&bindings[1..]) {
            let Expr::Call { callee, args, .. } = &arm.body else {
                panic!("expected bound handler call, got {:?}", arm.body);
            };
            assert!(is_local_path(callee, binding.0));
            assert!(
                matches!(args.as_slice(), [CallArg::Value(value)] if is_local_path(value, &arm.param))
            );
        }
    }

    #[test]
    fn either_path_handlers_remain_direct_without_staging() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type A; host type B; \
             fn pick(s: A | B, left: A -> ., right: B -> .) -> . { \
               __either__(A, B, ., s, left, right) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        let (bindings, terminal) = leading_let_spine(body);
        assert!(bindings.is_empty(), "path values need no staging");
        let Expr::EnrichedMatch {
            scrutinee, arms, ..
        } = terminal
        else {
            panic!("expected direct EnrichedMatch, got {terminal:?}");
        };
        assert!(is_local_path(scrutinee, "s"));
        assert_eq!(arms.len(), 2);
        assert_eq!(direct_call_name(&arms[0].body), Some("left"));
        assert_eq!(direct_call_name(&arms[1].body), Some("right"));
    }

    #[test]
    fn either_stages_one_computed_handler_beside_one_path_handler() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type A; host type B; \
             host fn scrutinee() -> A | B; \
             host fn left_factory() -> A -> .; \
             fn pick(right: B -> .) -> . { \
               __either__(A, B, ., scrutinee(), left_factory(), right) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        let (bindings, terminal) = leading_let_spine(body);
        assert_eq!(
            bindings.len(),
            2,
            "computed scrutinee and left factory stage"
        );
        assert_eq!(direct_call_name(bindings[0].1), Some("scrutinee"));
        assert_eq!(direct_call_name(bindings[1].1), Some("left_factory"));
        let Expr::EnrichedMatch { arms, .. } = terminal else {
            panic!("expected staged EnrichedMatch, got {terminal:?}");
        };
        assert_eq!(direct_call_name(&arms[0].body), Some(bindings[1].0));
        assert_eq!(direct_call_name(&arms[1].body), Some("right"));
    }

    #[test]
    fn computed_inner_either_handlers_stay_inside_outer_right_arm() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             host type A; host type B; host type C; \
             host fn b_factory() -> B -> .; \
             host fn c_factory() -> C -> .; \
             fn pick(s: A | (B | C)) -> . { \
               __either__(A, B | C, ., s, .(_a: A) { () }, \
                 .(rest: B | C) { \
                   __either__(B, C, ., rest, b_factory(), c_factory()) \
                 }) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        let (outer_bindings, outer_terminal) = leading_let_spine(body);
        assert!(
            outer_bindings.is_empty(),
            "inner factories must not be hoisted above outer selection"
        );
        let Expr::EnrichedMatch {
            scrutinee,
            arms: outer_arms,
            ..
        } = outer_terminal
        else {
            panic!("expected outer EnrichedMatch, got {outer_terminal:?}");
        };
        assert!(is_local_path(scrutinee, "s"));
        assert_eq!(
            outer_arms.len(),
            2,
            "computed inner handlers stop flattening"
        );
        assert_eq!(
            count(&outer_arms[0].body, &|e| {
                matches!(direct_call_name(e), Some("b_factory" | "c_factory"))
            }),
            0,
            "outer left arm never constructs inner handlers"
        );

        let (inner_bindings, inner_terminal) = leading_let_spine(&outer_arms[1].body);
        assert_eq!(
            inner_bindings.len(),
            2,
            "only the two factories need staging"
        );
        assert_eq!(direct_call_name(inner_bindings[0].1), Some("b_factory"));
        assert_eq!(direct_call_name(inner_bindings[1].1), Some("c_factory"));
        let Expr::EnrichedMatch {
            scrutinee,
            arms: inner_arms,
            ..
        } = inner_terminal
        else {
            panic!("expected nested EnrichedMatch, got {inner_terminal:?}");
        };
        assert!(is_local_path(scrutinee, "rest"));
        assert_eq!(inner_arms.len(), 2);
        assert_eq!(
            count(body, &|e| matches!(e, Expr::EnrichedMatch { .. })),
            2,
            "outer and inner dispatch remain distinct"
        );
    }

    /// A sum injection recovers to a positional `EnrichedInject`.
    #[test]
    fn intrinsic_left_recovers_to_enriched_inject() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn w[A][B](x: A) -> (A | B) { __left__(A, B, x) }",
            None,
        );
        let body = fn_body(&m, "w");
        match body {
            Expr::EnrichedInject {
                variant, variants, ..
            } => {
                assert_eq!(*variant, 0, "left injection is variant 0");
                assert_eq!(*variants, 2, "two-variant sum");
            }
            other => panic!("expected EnrichedInject, got {other:?}"),
        }
    }

    /// `if!` / `else` recovers to a first-class `EnrichedConditional`
    /// — the `__if_then_else__` thunk pair is gone.
    #[test]
    fn if_else_recovers_to_enriched_conditional() {
        let m = recover_one(
            "main",
            "module main; import control(if); \
             host type Bool role(bool); \
             fn choose(c: Bool) -> . { if! c { () } else { () } }",
            None,
        );
        let body = fn_body(&m, "choose");
        let conds = count(body, &|e| matches!(e, Expr::EnrichedConditional { .. }));
        assert_eq!(conds, 1, "one EnrichedConditional");
        // No `__if_then_else__` call survives.
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__if_then_else__")
            )),
            0,
            "no `__if_then_else__` call should survive recovery"
        );
    }

    #[test]
    fn conditional_computed_thunks_stage_before_dispatch_in_call_order() {
        let m = recover_one(
            "main",
            "module main; import __intrinsics__; \
             host type Bool role(bool); \
             host fn condition() -> Bool; \
             host fn then_factory() -> . -> .; \
             host fn else_factory() -> . -> .; \
             fn choose() -> . { \
               __if_then_else__(., condition(), then_factory(), else_factory()) \
             }",
            None,
        );
        let body = fn_body(&m, "choose");
        let (bindings, terminal) = leading_let_spine(body);
        assert_eq!(bindings.len(), 3, "condition and both factories are staged");
        assert_eq!(direct_call_name(bindings[0].1), Some("condition"));
        assert_eq!(direct_call_name(bindings[1].1), Some("then_factory"));
        assert_eq!(direct_call_name(bindings[2].1), Some("else_factory"));

        let Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } = terminal
        else {
            panic!("expected staged EnrichedConditional, got {terminal:?}");
        };
        assert!(is_local_path(cond, bindings[0].0));
        for (branch, binding) in [then_branch.as_ref(), else_branch.as_ref()]
            .into_iter()
            .zip(&bindings[1..])
        {
            let Expr::Call { callee, args, .. } = branch else {
                panic!("expected bound thunk call, got {branch:?}");
            };
            assert!(is_local_path(callee, binding.0));
            assert!(args.is_empty());
        }
    }

    #[test]
    fn conditional_stages_one_computed_thunk_beside_one_path_thunk() {
        let m = recover_one(
            "main",
            "module main; import __intrinsics__; \
             host type Bool role(bool); \
             host fn condition() -> Bool; \
             host fn then_factory() -> . -> .; \
             fn choose(otherwise: . -> .) -> . { \
               __if_then_else__(., condition(), then_factory(), otherwise) \
             }",
            None,
        );
        let body = fn_body(&m, "choose");
        let (bindings, terminal) = leading_let_spine(body);
        assert_eq!(
            bindings.len(),
            2,
            "computed condition and then factory stage"
        );
        assert_eq!(direct_call_name(bindings[0].1), Some("condition"));
        assert_eq!(direct_call_name(bindings[1].1), Some("then_factory"));
        let Expr::EnrichedConditional {
            then_branch,
            else_branch,
            ..
        } = terminal
        else {
            panic!("expected staged EnrichedConditional, got {terminal:?}");
        };
        assert_eq!(direct_call_name(then_branch), Some(bindings[1].0));
        assert_eq!(direct_call_name(else_branch), Some("otherwise"));
    }

    /// A direct product projection recovers to an `EnrichedProject`
    /// node.
    #[test]
    fn product_match_recovers_projections() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn first[A][B](p: (A & B)) -> (A | B) { \
               __left__(A, B, __fst__(A, B, p)) \
             }",
            None,
        );
        let body = fn_body(&m, "first");
        let projects = count(body, &|e| matches!(e, Expr::EnrichedProject { .. }));
        assert!(projects >= 1, "at least one EnrichedProject recovered");
        // No bare `__fst__` / `__snd__` call survives.
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1
                            && (segments[0] == "__fst__" || segments[0] == "__snd__"))
            )),
            0,
            "no `__fst__` / `__snd__` call should survive recovery"
        );
    }

    /// `__snd__` whose `R` type argument is a parametric application
    /// (`Foo(A)`) still recovers after the parser's initial value-shaped call
    /// is reclassified and baked as `CallArg::Type` by standalone checking.
    #[test]
    fn snd_with_parametric_type_arg_value_recovers() {
        let m = recover_one(
            "x/main",
            // The parser initially reads `Box(A)` as a value call; standalone
            // checking reclassifies and bakes the type-argument slot before
            // recovery.
            "module x/main; import __intrinsics__; \
             newtype Box[A] : A { pub constructor mk_box; pub projector un_box; }; \
             fn snd_of[A](p: (A & Box(A))) -> Box(A) { __snd__(A, Box(A), p) }",
            None,
        );
        let body = fn_body(&m, "snd_of");
        // `Box(A)` is a nominal (newtype application) — `product_spine_count`
        // treats nominals as opaque (count 1), so the outer spine is
        // 2 and `__snd__` projects to slot 1 of a 2-spine product.
        assert!(
            matches!(
                body,
                Expr::EnrichedProject {
                    index: 1,
                    arity: 2,
                    ..
                }
            ),
            "expected EnrichedProject{{index:1, arity:2}}, got {body:?}"
        );
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__snd__")
            )),
            0,
            "no `__snd__` call should survive recovery"
        );
    }

    /// A bare `__snd__` whose result is itself a *sub-product* (not a
    /// single component) recovers by binding the recovered source once
    /// and projecting the rebuilt sub-product slots from that binding.
    /// No `__snd__` call survives.
    #[test]
    fn snd_run_to_sub_product_recovers_as_nary_tuple() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             fn rest[A][B][C](p: (A & (B & C))) -> (B & C) { __snd__(p) }",
            None,
        );
        let body = fn_body(&m, "rest");
        match body {
            Expr::Let {
                name,
                value,
                body: tuple,
                ..
            } => {
                assert!(
                    matches!(&**value, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].name == "p"),
                    "recovery should bind the original source once: {value:?}"
                );
                let Expr::EnrichedTuple { items, .. } = &**tuple else {
                    panic!("expected let body to be EnrichedTuple, got {tuple:?}");
                };
                assert_eq!(items.len(), 2, "sub-product has 2 slots (b, c)");
                for (i, it) in items.iter().enumerate() {
                    let slot = i + 1;
                    match it {
                        Expr::EnrichedProject {
                            index,
                            arity,
                            target,
                            ..
                        } => {
                            assert_eq!(*index, slot, "slot {i} projects outer index {slot}");
                            assert_eq!(*arity, 3, "outer arity is 3");
                            assert!(
                                matches!(&**target, Expr::Path { segments, .. }
                                    if segments.len() == 1 && segments[0].name == name.as_str()),
                                "target should be the recovery binding `{name}`: {target:?}"
                            );
                        }
                        other => panic!("slot {i}: expected EnrichedProject, got {other:?}"),
                    }
                }
            }
            other => panic!("expected EnrichedTuple, got {other:?}"),
        }
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__snd__")
            )),
            0,
            "no `__snd__` call should survive recovery"
        );
    }

    /// `__right__` whose payload is a *dynamic* sub-sum value (`R`
    /// is structurally a sum, the payload is not a static
    /// `__left__` / `__right__` chain) recovers as an
    /// `EnrichedMatch` that re-injects each variant of the inner sum
    /// into the outer n-ary sum at offset 1. The dynamic payload's
    /// own structure lives inside the recovered scrutinee.
    #[test]
    fn mk_right_of_dynamic_sub_sum_recovers_as_reinjection_match() {
        let m = recover_one(
            "x/main",
            // The payload `x : (B | C)` is a variable, not an
            // injection chain — recovery must re-tag.
            "module x/main; import __intrinsics__; \
             fn widen[A][B][C](x: (B | C)) -> (A | (B | C)) { \
               __right__(A, (B | C), x) }",
            None,
        );
        let body = fn_body(&m, "widen");
        // The inner sum `(B | C)` has 2 variants; the outer has 3.
        // The match re-injects: arm 0 → outer variant 1, arm 1 →
        // outer variant 2.
        match body {
            Expr::EnrichedMatch { arms, .. } => {
                assert_eq!(arms.len(), 2, "two arms (one per inner variant)");
                for (i, arm) in arms.iter().enumerate() {
                    match &arm.body {
                        Expr::EnrichedInject {
                            variant, variants, ..
                        } => {
                            assert_eq!(*variant, i + 1, "arm {i} re-injects at offset 1+i");
                            assert_eq!(*variants, 3, "outer n-ary 3 variants");
                        }
                        other => panic!("arm {i} body: expected EnrichedInject, got {other:?}"),
                    }
                }
            }
            other => panic!("expected EnrichedMatch, got {other:?}"),
        }
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__right__")
            )),
            0,
            "no `__right__` call should survive recovery"
        );
    }

    /// `__if_then_else__` with already-bound thunk paths recovers without
    /// staging — each branch becomes `Call(thunk, [])`, so only the selected
    /// thunk is invoked.
    #[test]
    fn if_then_else_with_path_branch_thunks_recovers_via_call() {
        let m = recover_one(
            "main",
            "module main; \
             import __intrinsics__; \
             host type Bool role(bool); \
             fn pick(c: Bool, t: . -> ., e: . -> .) -> . { \
               __if_then_else__(., c, t, e) \
             }",
            None,
        );
        let body = fn_body(&m, "pick");
        let (bindings, terminal) = leading_let_spine(body);
        assert!(bindings.is_empty(), "bound paths need no staging");
        match terminal {
            Expr::EnrichedConditional {
                then_branch,
                else_branch,
                ..
            } => {
                // Each branch is now `Call(<thunk>, [])`.
                assert!(
                    matches!(&**then_branch, Expr::Call { args, .. } if args.is_empty()),
                    "then-branch is a zero-arg Call: {then_branch:?}"
                );
                assert!(
                    matches!(&**else_branch, Expr::Call { args, .. } if args.is_empty()),
                    "else-branch is a zero-arg Call: {else_branch:?}"
                );
            }
            other => panic!("expected EnrichedConditional, got {other:?}"),
        }
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__if_then_else__")
            )),
            0,
            "no `__if_then_else__` call should survive recovery"
        );
    }

    /// A surface tuple of label-derived newtype constructor calls (the
    /// record literal `({hed = ()}, {mid = ()}, {tal = ()})` after
    /// label elaboration) recovers to one flat [`Expr::EnrichedTuple`]
    /// while retaining each constructor wrap. Prime canonicalization
    /// may lift the constructor and intermediate pair calls onto the
    /// statement spine; recovery follows the binding relationships
    /// and removes only the single-use pair temporaries.
    #[test]
    fn labels_record_literal_recovers_to_enriched_tuple() {
        let m = recover_one(
            "x/main",
            "module x/main; \
             labels { hed: ., mid: ., tal: . }; \
             fn rec() -> (Hed & Mid & Tal) { \
               ({hed = ()}, {mid = ()}, {tal = ()}) \
             }",
            None,
        );
        let body = fn_body(&m, "rec");
        let (bindings, terminal) = leading_let_spine(body);
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = terminal
        else {
            panic!("expected terminal EnrichedTuple, got {terminal:?}");
        };
        assert_eq!(items.len(), 3, "three slots flat");
        assert_eq!(
            items.len(),
            Type::right_spine_product(synth_ty).len(),
            "tuple width matches its synthesized product type"
        );
        let mut previous_constructor = None;
        for (slot, expected_newtype) in items.iter().zip(["Hed", "Mid", "Tal"]) {
            let (constructor_index, _, payload) =
                constructor_slot(slot, &bindings, expected_newtype);
            if let Some(previous) = previous_constructor {
                assert!(
                    previous < constructor_index,
                    "constructor bindings preserve source order"
                );
            }
            previous_constructor = Some(constructor_index);
            assert!(
                matches!(payload, Expr::Unit { .. }),
                "{expected_newtype}: constructor payload: {payload:?}"
            );
        }
        assert_eq!(
            count(body, &|e| matches!(e, Expr::EnrichedTuple { .. })),
            1,
            "the recovered product has one maximally-flat tuple node"
        );
    }

    #[test]
    fn unrelated_grouped_newtype_does_not_reclassify_local_constructor() {
        let a = parse(
            "module a; \
             rec { \
               pub newtype Foo : Peer { pub constructor mk_a; pub projector mk_b; }; \
               pub newtype Peer : Foo { pub constructor make_peer; pub projector un_peer; }; \
             }",
        )
        .expect("parse a");
        let b = parse(
            "module b; import __intrinsics__; \
             pub newtype Foo : . { pub constructor mk_b; pub projector un_b; }; \
             pub fn keep(p: . & .) -> Foo { Foo.mk_b(__fst__(., ., p)) }",
        )
        .expect("parse b");
        let (lowered_modules, _) = FullPipeline::lower_package(
            vec![(PathBuf::from("a.kio"), a), (PathBuf::from("b.kio"), b)],
            None,
        )
        .expect("lower package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = check_package(&package).expect("check package");
        let recovered = recover_package(&prime);
        let b = &recovered.module("b").expect("module b").module;
        let body = fn_body(b, "keep");

        assert!(
            matches!(body,
                Expr::Call { callee, .. }
                    if matches!(callee.as_ref(), Expr::Path { segments, .. }
                        if segments.len() == 2
                            && segments[0].name == "Foo"
                            && segments[1].name == "mk_b")),
            "an unrelated module reclassified `b.Foo.mk_b` during recovery: {body:?}"
        );
    }

    #[test]
    fn qualified_and_selective_projectors_use_their_exact_provider() {
        let sources = [
            (
                "provider.kio",
                "module provider; \
                 pub newtype Hed : . { pub constructor put; pub projector get; }; \
                 pub newtype Tal : . { pub constructor put_tal; pub projector get_tal; };",
            ),
            (
                "decoy.kio",
                "module decoy; \
                 pub newtype Hed : . { pub constructor stash; pub projector peek; }; \
                 pub newtype Tal : . { pub constructor stash_tal; pub projector peek_tal; };",
            ),
            (
                "qualified.kio",
                "module qualified; import __intrinsics__; \
                 import provider as p; import decoy as d; \
                 fn read(r: (p.Hed & p.Tal)) -> . { p.Hed.get(__fst__(r)) } \
                 fn read_decoy(r: (d.Hed & d.Tal)) -> . { d.Hed.peek(__fst__(r)) }",
            ),
            (
                "selective.kio",
                "module selective; import __intrinsics__; \
                 import provider(Hed, Tal); \
                 fn read(r: (Hed & Tal)) -> . { Hed.get(__fst__(r)) }",
            ),
        ];
        let parsed = sources
            .into_iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    parse(source).unwrap_or_else(|error| panic!("parse {path}: {error:?}")),
                )
            })
            .collect();
        let (lowered_modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower package");
        let package = Package::build(Path::new(""), lowered_modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = check_package(&package).expect("check package");
        let recovered = recover_package(&prime);

        for (module, function) in [
            ("qualified", "read"),
            ("qualified", "read_decoy"),
            ("selective", "read"),
        ] {
            let module = &recovered.module(module).expect("consumer module").module;
            let body = fn_body(module, function);
            assert!(
                matches!(
                    body,
                    Expr::EnrichedFieldGet {
                        index: 0,
                        arity: 2,
                        ..
                    }
                ),
                "{module:?}::{function} must recover through its exact lexical provider: {body:?}"
            );
        }
    }

    /// The member projector spelling `Hed.get(__fst__(r))` on a
    /// positional access into a record `Hed & Tal` folds to an
    /// [`Expr::EnrichedFieldGet`] carrying the field name and the
    /// underlying projection's positional layout. No `__fst__` call
    /// and no surviving projector `Call` remain.
    #[test]
    fn projector_on_positional_recovers_to_field_get_one_segment() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             labels { hed: ., tal: . }; \
             fn read(r: (Hed & Tal)) -> . { Hed.get(__fst__(r)) }",
            None,
        );
        let body = fn_body(&m, "read");
        match body {
            Expr::EnrichedFieldGet {
                field_name,
                index,
                arity,
                target,
                ..
            } => {
                assert_eq!(field_name, "Hed", "field name is the FFI key");
                assert_eq!(*index, 0, "`__fst__` projects slot 0");
                assert_eq!(*arity, 2, "two-slot product");
                assert!(
                    matches!(&**target, Expr::Path { .. }),
                    "field-get reads from the bare record value: {target:?}"
                );
            }
            other => panic!("expected EnrichedFieldGet, got {other:?}"),
        }
        // No `__fst__` call survives — the projection folded into the
        // field-get's positional layout.
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__fst__")
            )),
            0,
            "no `__fst__` call should survive recovery"
        );
        // No raw projector `Call` survives either.
        assert_eq!(
            count(body, &|e| matches!(e, Expr::Call { .. })),
            0,
            "no `Call` should survive — the projector folded to a field-get"
        );
    }

    /// The 2-segment member spelling `Tal.get(__snd__(r))` folds the
    /// same way as the 1-segment form. The `__snd__` projects slot 1
    /// of the 2-slot product.
    #[test]
    fn projector_on_positional_recovers_to_field_get_two_segment() {
        let m = recover_one(
            "x/main",
            "module x/main; import __intrinsics__; \
             labels { hed: ., tal: . }; \
             fn read(r: (Hed & Tal)) -> . { Tal.get(__snd__(r)) }",
            None,
        );
        let body = fn_body(&m, "read");
        match body {
            Expr::EnrichedFieldGet {
                field_name,
                index,
                arity,
                ..
            } => {
                assert_eq!(field_name, "Tal", "field name is the FFI key");
                assert_eq!(*index, 1, "`__snd__` projects slot 1");
                assert_eq!(*arity, 2, "two-slot product");
            }
            other => panic!("expected EnrichedFieldGet, got {other:?}"),
        }
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__snd__")
            )),
            0,
            "no `__snd__` call should survive recovery"
        );
    }

    /// A projector call whose argument is *not* a positional access
    /// (a bare record value, here a `fn` parameter of the label type)
    /// stays a structural projector call — recovery is opportunistic
    /// and leaves the projector untouched; so no `EnrichedFieldGet`
    /// appears.
    #[test]
    fn projector_on_non_positional_stays_a_call() {
        let m = recover_one(
            "x/main",
            "module x/main; \
             labels { hed: . }; \
             fn read(h: Hed) -> . { Hed.get(h) }",
            None,
        );
        let body = fn_body(&m, "read");
        assert_eq!(
            count(body, &|e| matches!(e, Expr::EnrichedFieldGet { .. })),
            0,
            "no EnrichedFieldGet when the argument is not a positional access"
        );
        // The projector call survives structurally.
        assert!(
            matches!(body, Expr::Call { .. }),
            "expected a structural projector Call; got {body:?}"
        );
    }

    /// A surface tuple with **mixed** named / unnamed components
    /// (`({hed = 7}, 42)`) does *not* promote to a record — recovery
    /// falls back to [`Expr::EnrichedTuple`], dropping the partial
    /// name (the IR's positional form has no per-slot name slot at
    /// this first cut). Label-derived names are an all-or-nothing
    /// signal.
    #[test]
    fn mixed_named_tuple_stays_enriched_tuple() {
        let m = recover_one(
            "x/main",
            "module x/main; \
             labels { hed: . }; \
             fn mixed() -> (Hed & .) { ({hed = ()}, ()) }",
            None,
        );
        let body = fn_body(&m, "mixed");
        let (bindings, terminal) = leading_let_spine(body);
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = terminal
        else {
            panic!("expected 2-item EnrichedTuple, got {terminal:?}");
        };
        assert_eq!(items.len(), 2, "mixed tuple retains both slots");
        assert_eq!(items.len(), Type::right_spine_product(synth_ty).len());
        let (_, _, payload) = constructor_slot(&items[0], &bindings, "Hed");
        assert!(
            matches!(payload, Expr::Unit { .. }),
            "named slot retains its constructor payload: {payload:?}"
        );
        assert!(matches!(&items[1], Expr::Unit { .. }), "unnamed unit slot");
        assert_eq!(
            count(body, &|e| matches!(e, Expr::EnrichedRecord { .. })),
            0,
            "the unnamed slot blocks record promotion throughout the body"
        );
    }

    /// A 5-component tuple recovers as one flat n-ary `EnrichedTuple`
    /// with five items — one slot per source-level intent.
    #[test]
    fn five_component_tuple_recovers_as_nary_flat() {
        let m = recover_one(
            "x/main",
            "module x/main; \
             fn quint() -> (. & . & . & . & .) { ((), (), (), (), ()) }",
            None,
        );
        let body = fn_body(&m, "quint");
        match body {
            Expr::EnrichedTuple { items, .. } => {
                assert_eq!(items.len(), 5, "five flat slots");
                for (i, leaf) in items.iter().enumerate() {
                    assert!(
                        matches!(leaf, Expr::Unit { .. }),
                        "slot {i} is `()`: {leaf:?}"
                    );
                }
            }
            other => panic!("expected EnrichedTuple, got {other:?}"),
        }
        assert_eq!(
            count(body, &|e| matches!(
                e,
                Expr::Call { callee, .. }
                    if matches!(&**callee, Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0] == "__pair__")
            )),
            0,
            "no bare `__pair__` call should survive recovery"
        );
    }

    /// A 4-field record with effectful payloads recovers to one flat
    /// n-ary tuple. The payload and constructor bindings retain source
    /// order, and every tuple slot still resolves through its matching
    /// newtype constructor.
    #[test]
    fn four_field_record_keeps_wraps() {
        let m = recover_one(
            "main",
            "module main; \
             host type Int role(i32); \
             host fn mark(value: Int) -> Int; \
             labels { a: Int, b: Int, c: Int, d: Int }; \
             fn quad(_kp0: Int) -> (A & B & C & D) { \
               ({a = mark(10(Int))}, {b = mark(20(Int))}, \
                {c = mark(30(Int))}, {d = mark(40(Int))}) \
             }",
            None,
        );
        let body = fn_body(&m, "quad");
        let (bindings, terminal) = leading_let_spine(body);
        assert!(
            bindings.iter().all(|(name, _)| *name != "_kp0"),
            "generated bindings do not capture the user parameter"
        );
        let Expr::EnrichedTuple {
            items, synth_ty, ..
        } = terminal
        else {
            panic!("expected terminal EnrichedTuple, got {terminal:?}");
        };
        assert_eq!(items.len(), 4, "four slots flat");
        assert_eq!(
            items.len(),
            Type::right_spine_product(synth_ty).len(),
            "tuple width matches its synthesized product type"
        );

        let mut previous_payload = None;
        let mut previous_constructor = None;
        for ((slot, expected_newtype), expected_digits) in items
            .iter()
            .zip(["A", "B", "C", "D"])
            .zip(["10", "20", "30", "40"])
        {
            let (constructor_index, Some(payload_index), payload) =
                constructor_slot(slot, &bindings, expected_newtype)
            else {
                panic!("{expected_newtype}: constructor payload is not let-bound");
            };
            assert!(
                payload_index < constructor_index,
                "{expected_newtype}: payload evaluates before its constructor"
            );
            if let Some(previous) = previous_payload {
                assert!(
                    previous < payload_index,
                    "payload effects preserve source order"
                );
            }
            if let Some(previous) = previous_constructor {
                assert!(
                    previous < constructor_index,
                    "constructors preserve source order"
                );
            }
            previous_payload = Some(payload_index);
            previous_constructor = Some(constructor_index);

            let Expr::Call { callee, args, .. } = payload else {
                panic!("{expected_newtype}: expected effectful payload Call, got {payload:?}");
            };
            assert!(
                matches!(callee.as_ref(), Expr::Path { segments, .. }
                    if segments.len() == 1 && segments[0].name == "mark"),
                "{expected_newtype}: payload callee: {callee:?}"
            );
            assert!(
                args.iter().any(|arg| matches!(arg,
                    CallArg::Value(Expr::IntLit { digits, .. })
                        if digits.as_str() == expected_digits)),
                "{expected_newtype}: payload carries {expected_digits}: {args:?}"
            );
        }
        assert_eq!(
            count(body, &|e| matches!(e, Expr::EnrichedTuple { .. })),
            1,
            "intermediate pair bindings are absorbed into one flat tuple"
        );
        assert_eq!(
            count(body, &|e| matches!(e,
                Expr::Call { callee, .. }
                    if matches!(callee.as_ref(), Expr::Path { segments, .. }
                        if segments.len() == 1 && segments[0].name == "mark"))),
            4,
            "each payload effect appears exactly once"
        );
        assert_eq!(
            count(body, &|e| matches!(e,
                Expr::Call { callee, .. }
                    if matches!(callee.as_ref(), Expr::Path { segments, .. }
                        if segments.len() == 2
                            && segments[1].name == "mk"
                            && ["A", "B", "C", "D"].contains(&segments[0].name.as_str())))),
            4,
            "each constructor wrap appears exactly once"
        );
    }

    /// A module with no recoverable intrinsic chains is carried across
    /// the `Prime → Enriched` boundary unchanged in shape — item count,
    /// kinds, and module path all survive.
    #[test]
    fn module_without_chains_passes_through() {
        let m = recover_one(
            "x/main",
            "module x/main; \
             fn id[A](x: A) -> A { x } \
             fn use_id(y: .) -> . { id(., y) }",
            None,
        );
        assert_eq!(m.path.segments, vec!["x", "main"]);
        assert_eq!(m.items.len(), 2);
        for it in &m.items {
            assert!(matches!(it, Item::FnDef(_)), "got {it:?}");
        }
    }
}

//! Shared body-type reconstruction for emitters that need the static
//! `Type<Routed>` of a `Low*` IR expression.
//!
//! A native-HKT backend renders each IR node at its native `f a` type
//! rather than an erased carrier, so it must recover that type: walk the
//! Routed IR, reading the type off the nodes the typer annotated
//! (host-call return types, synthesised compound types, match/conditional
//! result types, the surrounding fn's value-param types) and threading a
//! `let`-local env through the walk. The walk is **purely structural**:
//! it short-circuits on the annotated nodes and recurses into the rest,
//! returning `None` at the gaps the caller copes with (an untyped closure
//! param or an indirect-call result the host language's own type system
//! covers). Erased-static emitters normally need none of this because
//! their body uses one universal carrier. Rust and Swift use the walk at an
//! exact-Boolean boundary: the self-describing Routed annotations determine
//! which host-selected `role(bool)` type an erased condition must be cast to,
//! without adding provenance to the IR.
//!
//! This is the cross-cutting *shape* of that walk, behind the
//! [`ReconProfile`] trait. Haskell uses it throughout native body emission;
//! Rust and Swift use it where an erased body must name an exact condition
//! type. What varies per backend lives behind the hooks:
//!
//! - **The surrounding fn's param env** ([`ReconProfile::bound_param_ty`])
//!   — Haskell threads it as a [`mut`][ReconProfile]-able
//!   [`TypeRecon`][crate::backends::haskell::reconstruct::TypeRecon]
//!   context the native emitter pushes inferred closure-param types into.
//!   The `locals` map overlays it for `let`-bindings.
//! - **A resolved call's monomorphised return type**
//!   ([`ReconProfile::resolved_call_return_type`]) — substitutes the
//!   signature's type-params by the call's type-args (the pure
//!   [`apply_subst`] / [`build_typearg_subst_from_sig`] core); a member
//!   that needs returned-fn source recovery or value-shaped type-arg
//!   canonicalisation would layer it here.
//! - **The enriched-slot acceptance gate**
//!   ([`ReconProfile::accept_direct_enriched_slot`]) — when a projection
//!   targets a literal tuple/record, whether to keep the slot's
//!   reconstructed type. Haskell keeps it unconditionally; a member that
//!   stores fn-typed slots specially would gate on the slot being a
//!   function.
//! - **The projection target's normalisation**
//!   ([`ReconProfile::normalize_target_ty`]) — Haskell's native packages
//!   carry no erased `comptime` aliases, so it is identity; a member that
//!   unfolds such aliases before slicing the product spine would do so
//!   here.
//! - **A backend-only IR arm** ([`ReconProfile::reconstruct_other`]) —
//!   Haskell reconstructs a [`Expr::LowCpsProjectorApply`] from the exact
//!   continuation type carried by that routed node. Each member sees only
//!   the IR shapes its own lowering produces, so the hook keeps the shared
//!   walk from having to name every member's extra arm.
//! - **A module-fn value's return type**
//!   ([`ReconProfile::module_fn_value_type`]) — the Routed value-ref carries
//!   its canonical signature but `Signature` deliberately carries no return
//!   slot, so the profile resolves the declaration that owns the return type.
//!
//! Every other arm — literals, bound refs, `let`, `seq`, visible type
//! applications, the synthesised compounds, `fn` values, host-fn value refs
//! — is identical across backends and lives in the driver below.

use std::collections::HashMap;

use crate::ast::{Expr, Routed, Signature, SignatureParam, Type};
use crate::span::Span;

/// Per-backend variation points of body-type reconstruction.
///
/// A backend implements this against its own borrowed state. The shared
/// [`value_type_with_locals`] driver walks the IR and calls these hooks
/// where members diverge; every other arm is identical and the driver
/// renders it directly. Each hook re-enters the driver through
/// [`Self::value_type_with_locals`] to recurse, so the walk is the same
/// across backends while the divergent decisions stay the profile's.
pub trait ReconProfile {
    /// The declared type of a name bound by the surrounding fn's value
    /// params (the closure / fn binders in scope for its body). `None`
    /// when the name is not a fn param the member tracks. The `let`-local
    /// env overlays this; the driver consults `locals` first.
    fn bound_param_ty(&self, name: &str) -> Option<Type<Routed>>;

    /// The monomorphised value type of a resolved host / module /
    /// qualified-module call, given its signature, type-args, value-args,
    /// and declared return type. The shared core is
    /// `apply_subst(ret_ty, subst)` with `subst` built from the signature's
    /// type-params; a member that needs more (returned-fn source recovery,
    /// value-shaped type-arg canonicalisation) layers it here. Haskell
    /// needs neither.
    fn resolved_call_return_type(
        &self,
        sig: &Signature<Routed>,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        ret_ty: &Type<Routed>,
    ) -> Type<Routed>;

    /// Whether a projection / field-get whose target is a literal
    /// tuple/record should keep the slot's directly-reconstructed type.
    /// Haskell keeps it unconditionally; a member that stores fn-typed
    /// slots specially would keep it only for a function slot.
    fn accept_direct_enriched_slot(&self, slot_ty: &Type<Routed>) -> bool;

    /// Normalise a projection target's type before slicing its product
    /// spine. A member that carries erased `comptime` aliases unfolds them
    /// here; Haskell carries none, so it returns the type unchanged.
    fn normalize_target_ty(&self, ty: &Type<Routed>) -> Type<Routed>;

    /// The payload type returned by an [`Expr::EnrichedFieldGet`] whose
    /// product slot has type `field_ty`. A field-get includes the label
    /// newtype's runtime-identity projection; an ordinary enriched project
    /// retains `field_ty` itself.
    fn enriched_field_payload_type(&self, field_ty: &Type<Routed>) -> Option<Type<Routed>>;

    /// Reconstruct a module function referenced in value position. The node
    /// carries its canonical signature, while the profile supplies the
    /// declaration-owned return type that [`Signature`] does not store.
    fn module_fn_value_type(
        &self,
        mangled: &str,
        sig: &Signature<Routed>,
        span: Span,
    ) -> Option<Type<Routed>>;

    /// Reconstruct an IR shape only one member produces. Reached from the
    /// driver's catch-all, after every shared arm has been tried; `None`
    /// is the reconstruction gap the catch-all otherwise yields. The hook
    /// recurses via [`Self::value_type_with_locals`].
    fn reconstruct_other(
        &self,
        expr: &Expr<Routed>,
        locals: &mut HashMap<String, Type<Routed>>,
    ) -> Option<Type<Routed>>;

    /// Re-enter the shared reconstruction driver. Hooks call this to
    /// recurse; it is exactly [`value_type_with_locals`] with `self` as
    /// the profile, surfaced as a method.
    fn value_type_with_locals(
        &self,
        expr: &Expr<Routed>,
        locals: &mut HashMap<String, Type<Routed>>,
    ) -> Option<Type<Routed>>
    where
        Self: Sized,
    {
        value_type_with_locals(self, expr, locals)
    }
}

/// The reconstructed value type of `expr` under the `let`-local env
/// `locals`, dispatching the divergent arms to `profile`.
///
/// Purely structural: every arm either reads a type the typer annotated
/// on the node, recurses into a sub-expression, or — at the four
/// divergence points — defers to the profile. `None` is a reconstruction
/// gap (an untyped closure param, an unhandled call shape) the caller
/// copes with, never a silent erasure.
pub fn value_type_with_locals<P: ReconProfile>(
    profile: &P,
    expr: &Expr<Routed>,
    locals: &mut HashMap<String, Type<Routed>>,
) -> Option<Type<Routed>> {
    match expr {
        Expr::Unit {
            occurrence: _,
            meta,
        } => Some(Type::Unit { meta: meta.clone() }),
        Expr::LowHostCall {
            sig,
            type_args,
            args,
            ret_ty,
            ..
        } => Some(profile.resolved_call_return_type(sig, type_args, args, ret_ty)),
        // Every Kio' literal carries its host type directly in the
        // now-mandatory `(Type)` annotation (`"ok"(String)`,
        // `42(I32)`), so a literal never reconstructs to a gap.
        Expr::StrLit { annotation, .. }
        | Expr::IntLit { annotation, .. }
        | Expr::FloatLit { annotation, .. }
        | Expr::BoolLit { annotation, .. } => Some(annotation.clone()),
        Expr::LowModuleCall {
            ret_ty: Some(ret_ty),
            sig,
            type_args,
            args,
            ..
        }
        | Expr::LowQualifiedModuleCall {
            ret_ty: Some(ret_ty),
            sig,
            type_args,
            args,
            ..
        } => Some(profile.resolved_call_return_type(sig, type_args, args, ret_ty)),
        Expr::LowBoundRef { name, .. } => locals
            .get(name)
            .cloned()
            .or_else(|| profile.bound_param_ty(name)),
        Expr::LowClosureCall {
            name, type_args, ..
        } => locals
            .get(name)
            .cloned()
            .or_else(|| profile.bound_param_ty(name))
            .and_then(|ty| function_return_after_call(&ty, type_args)),
        Expr::LowIndirectCall {
            callee, type_args, ..
        } => profile
            .value_type_with_locals(callee, locals)
            .and_then(|ty| function_return_after_call(&ty, type_args)),
        Expr::LowTypeApplication {
            callee, type_arg, ..
        } => {
            let Type::Forall { param, body, .. } =
                profile.value_type_with_locals(callee, locals)?
            else {
                return None;
            };
            Some(apply_subst(
                &body,
                &HashMap::from([(param.name, type_arg.clone())]),
            ))
        }
        Expr::LowAbsurdCall { type_arg, .. } => Some(type_arg.clone()),
        Expr::Let {
            name, value, body, ..
        } => {
            let value_ty = profile.value_type_with_locals(value, locals);
            let previous = value_ty
                .as_ref()
                .map(|ty| locals.insert(name.clone(), ty.clone()));
            let body_ty = profile.value_type_with_locals(body, locals);
            if let Some(previous) = previous {
                match previous {
                    Some(ty) => {
                        locals.insert(name.clone(), ty);
                    }
                    None => {
                        locals.remove(name);
                    }
                }
            }
            body_ty
        }
        Expr::Seq { body, .. } => profile.value_type_with_locals(body, locals),
        Expr::EnrichedTuple { synth_ty, .. }
        | Expr::EnrichedRecord { synth_ty, .. }
        | Expr::EnrichedInject { synth_ty, .. } => Some(synth_ty.clone()),
        Expr::EnrichedMatch { result_ty, .. } | Expr::EnrichedConditional { result_ty, .. } => {
            Some(result_ty.clone())
        }
        Expr::EnrichedProject {
            target,
            index,
            arity,
            target_ty,
            ..
        } => {
            if let Some(source_ty) =
                direct_enriched_slot_source_type(profile, target, *index, *arity, locals)
                && profile.accept_direct_enriched_slot(&source_ty)
            {
                return Some(source_ty);
            }
            let actual_target_ty = profile
                .value_type_with_locals(target, locals)
                .unwrap_or_else(|| target_ty.clone());
            let normalized_target_ty = profile.normalize_target_ty(&actual_target_ty);
            nth_product_slot_ty_for_arity(&normalized_target_ty, *index, *arity).cloned()
        }
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            target_ty,
            ..
        } => {
            if let Some(source_ty) =
                direct_enriched_slot_source_type(profile, target, *index, *arity, locals)
                && profile.accept_direct_enriched_slot(&source_ty)
            {
                return if matches!(target.as_ref(), Expr::EnrichedRecord { .. }) {
                    // Enriched records store their field payloads unwrapped.
                    Some(source_ty)
                } else {
                    // An enriched tuple stores the label/newtype carrier, so
                    // field access still includes the newtype projection.
                    profile.enriched_field_payload_type(&source_ty)
                };
            }
            let actual_target_ty = profile
                .value_type_with_locals(target, locals)
                .unwrap_or_else(|| target_ty.clone());
            let normalized_target_ty = profile.normalize_target_ty(&actual_target_ty);
            let field_ty = nth_product_slot_ty_for_arity(&normalized_target_ty, *index, *arity)?;
            profile.enriched_field_payload_type(field_ty)
        }
        Expr::FnExpr {
            sig,
            body,
            ret_ty,
            meta,
            ..
        } => fn_expr_value_type(profile, sig, body, ret_ty.as_ref(), meta.span, locals),
        // A `host fn` referenced in value position has the function
        // value type `(p0 & p1 & …) -> ret` at the host fn's own
        // abi-arity (one target-method arg per value param). The
        // fn-adapter wrap needs this source type to bridge a multi-arg
        // host fn (`string_concat(Str, Str) -> Str`, abi-arity 2) into a
        // product-domain slot (`(Str & Str) -> Str`, abi-arity 1): it
        // destructures the product into the host fn's separate args.
        Expr::LowHostFnValueRef {
            sig, ret_ty, meta, ..
        } => Some(sig.signature_ty(ret_ty.clone(), meta.span)),
        Expr::LowModuleFnValueRef {
            mangled, sig, meta, ..
        } => profile.module_fn_value_type(mangled, sig, meta.span),
        _ => profile.reconstruct_other(expr, locals),
    }
}

fn function_return_after_call(
    callee_ty: &Type<Routed>,
    type_args: &[Type<Routed>],
) -> Option<Type<Routed>> {
    let mut cur = callee_ty.clone();
    let mut subst = HashMap::new();
    let mut next_type_arg = 0usize;
    loop {
        match cur {
            Type::Forall { param, body, .. } => {
                if let Some(arg) = type_args.get(next_type_arg) {
                    subst.insert(param.name, arg.clone());
                    next_type_arg += 1;
                }
                cur = *body;
            }
            Type::Function { ret, .. } => return Some(apply_subst(&ret, &subst)),
            _ => return None,
        }
    }
}

/// The reconstructed type of a projection / field-get whose target is a
/// **literal** tuple or record built right at the projection site: the
/// slot's own value type, recovered by recursing into the literal's
/// `index`-th item. `None` when the target is not such a literal of the
/// matching arity.
fn direct_enriched_slot_source_type<P: ReconProfile>(
    profile: &P,
    target: &Expr<Routed>,
    index: usize,
    arity: usize,
    locals: &mut HashMap<String, Type<Routed>>,
) -> Option<Type<Routed>> {
    match target {
        Expr::EnrichedTuple { items, .. } if items.len() == arity && index < arity => {
            profile.value_type_with_locals(&items[index], locals)
        }
        Expr::EnrichedRecord { fields, .. } if fields.len() == arity && index < arity => {
            profile.value_type_with_locals(&fields[index].value, locals)
        }
        _ => None,
    }
}

/// The value type of an `fn` expression: the signature's exact ordered
/// type/value groups folded over its return, with any untyped value param
/// inferred from the body's direct call ([`infer_fn_param_tys_from_body`])
/// and an omitted return reconstructed from the body under those binders.
/// `None` means a value parameter or the body result could not be recovered.
fn fn_expr_value_type<P: ReconProfile>(
    profile: &P,
    sig: &Signature<Routed>,
    body: &Expr<Routed>,
    ret_ty: Option<&Type<Routed>>,
    span: Span,
    locals: &mut HashMap<String, Type<Routed>>,
) -> Option<Type<Routed>> {
    let value_param_names: Vec<String> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(v) => Some(v.name.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect();
    let mut value_tys: Vec<Option<Type<Routed>>> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(v) => Some(v.ty.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect();
    infer_fn_param_tys_from_body(&value_param_names, &mut value_tys, body);
    let mut value_tys = value_tys.into_iter();
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(param) => Some(SignatureParam::Type(param.clone())),
            SignatureParam::Value(param) => Some(SignatureParam::Value(crate::ast::Param {
                name: param.name.clone(),
                ty: Some(value_tys.next().flatten()?),
                pattern: (),
                meta: param.meta.clone(),
            })),
        })
        .collect::<Option<Vec<_>>>()?;
    let sig = Signature::from_parts(params, sig.groups.clone());
    let ret = if let Some(ret_ty) = ret_ty {
        ret_ty.clone()
    } else {
        let mut prior_bindings = Vec::new();
        for param in &sig.params {
            if let SignatureParam::Value(param) = param
                && let Some(ty) = &param.ty
            {
                let prior = locals.insert(param.name.clone(), ty.clone());
                prior_bindings.push((param.name.clone(), prior));
            }
        }
        let body_ty = profile.value_type_with_locals(body, locals);
        for (name, prior) in prior_bindings.into_iter().rev() {
            match prior {
                Some(ty) => {
                    locals.insert(name, ty);
                }
                None => {
                    locals.remove(&name);
                }
            }
        }
        body_ty?
    };
    Some(sig.signature_ty(ret, span))
}

/// Build a `tparam-name → type` substitution from a signature's type
/// params zipped positionally with a call's type-args.
pub fn build_typearg_subst_from_sig(
    sig: &Signature<Routed>,
    type_args: &[Type<Routed>],
) -> HashMap<String, Type<Routed>> {
    let mut out = HashMap::new();
    let mut idx = 0usize;
    for p in &sig.params {
        if let SignatureParam::Type(tp) = p {
            if let Some(t) = type_args.get(idx) {
                out.insert(tp.name.clone(), t.clone());
            }
            idx += 1;
        }
    }
    out
}

/// Apply a `tparam-name → type` map using the compiler's shared
/// capture-avoiding substitution.
pub fn apply_subst(ty: &Type<Routed>, subst: &HashMap<String, Type<Routed>>) -> Type<Routed> {
    crate::pass::typecheck_core::subst_type(ty, subst)
}

/// The `index`-th slot type of a right-nested product viewed as `arity`
/// slots: a terminal slot is the whole remaining type, a non-terminal is
/// that level's left operand. `None` if the spine is not a product where
/// one is required (which includes `index >= arity`, and so `arity == 0`).
pub fn nth_product_slot_ty_for_arity(
    t: &Type<Routed>,
    index: usize,
    arity: usize,
) -> Option<&Type<Routed>> {
    if index >= arity {
        return None;
    }
    let mut cur = t;
    for _ in 0..index {
        match cur {
            Type::Product { right, .. } => cur = right,
            _ => return None,
        }
    }
    if index + 1 == arity {
        // Terminal slot: the whole remaining type.
        Some(cur)
    } else {
        // Non-terminal slot: this level's left operand.
        match cur {
            Type::Product { left, .. } => Some(left),
            _ => None,
        }
    }
}

/// Infer a closure's untyped value-param types from a directly-called
/// body: a param passed straight through to a resolved call lands at that
/// call's declared param type. Mutates `value_tys` in place, filling only
/// the `None` slots.
pub fn infer_fn_param_tys_from_body(
    value_param_names: &[String],
    value_tys: &mut [Option<Type<Routed>>],
    body: &Expr<Routed>,
) {
    let Some((callee_sig, callee_args)) = direct_call_info(body) else {
        return;
    };
    let callee_value_param_tys: Vec<Option<Type<Routed>>> = callee_sig
        .params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(v) => Some(v.ty.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect();
    if callee_value_param_tys.is_empty() {
        return;
    }
    for (value_arg_pos, a) in callee_args.iter().enumerate() {
        if let Expr::LowBoundRef { name: ref_name, .. } = a
            && let Some(closure_param_idx) = value_param_names.iter().position(|n| n == ref_name)
            && value_tys[closure_param_idx].is_none()
            && let Some(Some(ty)) = callee_value_param_tys.get(value_arg_pos)
        {
            value_tys[closure_param_idx] = Some(ty.clone());
        }
    }
}

/// The signature and value-args of a directly-resolved call expression
/// (host / module / qualified-module), or `None` for any other shape.
pub fn direct_call_info(body: &Expr<Routed>) -> Option<(&Signature<Routed>, &[Expr<Routed>])> {
    match body {
        Expr::LowHostCall { sig, args, .. }
        | Expr::LowModuleCall { sig, args, .. }
        | Expr::LowQualifiedModuleCall { sig, args, .. } => Some((sig, args.as_slice())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Meta;

    #[test]
    fn apply_subst_avoids_capturing_free_type_argument() {
        let span = Span::new(10, 20);
        let free_a = Type::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let source = Type::Forall {
            param: crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(Type::synth_function(
                vec![Type::synth_path(vec!["T".to_owned()], Vec::new(), span)],
                free_a.clone(),
                span,
            )),
            meta: Meta::new(span),
        };

        let result = apply_subst(&source, &HashMap::from([("T".to_owned(), free_a.clone())]));
        let Type::Forall {
            param: fresh_param,
            body,
            ..
        } = result
        else {
            panic!("substitution must preserve the forall");
        };
        assert_ne!(fresh_param.name, "A");

        let Type::Function { param, ret, .. } = body.as_ref() else {
            panic!("substitution must preserve the function body");
        };
        assert!(matches!(
            param.as_ref(),
            Type::Path { segments, args, .. }
                if segments.len() == 1 && segments[0].as_str() == "A" && args.is_empty()
        ));
        assert!(matches!(
            ret.as_ref(),
            Type::Path { segments, args, .. }
                if segments.len() == 1
                    && segments[0].as_str() == fresh_param.name.as_str()
                    && args.is_empty()
        ));
    }

    struct FieldProfile {
        row_ty: Type<Routed>,
    }

    impl ReconProfile for FieldProfile {
        fn bound_param_ty(&self, name: &str) -> Option<Type<Routed>> {
            match name {
                "row" => Some(self.row_ty.clone()),
                "carrier" => Some(Type::synth_path(
                    vec!["Field".to_owned()],
                    Vec::new(),
                    Span::new(0, 0),
                )),
                "payload" => Some(Type::Unit {
                    meta: Meta::new(Span::new(0, 0)),
                }),
                _ => None,
            }
        }

        fn resolved_call_return_type(
            &self,
            sig: &Signature<Routed>,
            type_args: &[Type<Routed>],
            _args: &[Expr<Routed>],
            ret_ty: &Type<Routed>,
        ) -> Type<Routed> {
            apply_subst(ret_ty, &build_typearg_subst_from_sig(sig, type_args))
        }

        fn accept_direct_enriched_slot(&self, _slot_ty: &Type<Routed>) -> bool {
            true
        }

        fn normalize_target_ty(&self, ty: &Type<Routed>) -> Type<Routed> {
            ty.clone()
        }

        fn enriched_field_payload_type(&self, field_ty: &Type<Routed>) -> Option<Type<Routed>> {
            matches!(field_ty, Type::Path { segments, .. } if segments.last().is_some_and(|segment| segment.as_str() == "Field"))
                .then(|| Type::Unit {
                    meta: Meta::new(Span::new(0, 0)),
                })
        }

        fn module_fn_value_type(
            &self,
            _mangled: &str,
            _sig: &Signature<Routed>,
            _span: Span,
        ) -> Option<Type<Routed>> {
            None
        }

        fn reconstruct_other(
            &self,
            _expr: &Expr<Routed>,
            _locals: &mut HashMap<String, Type<Routed>>,
        ) -> Option<Type<Routed>> {
            None
        }
    }

    fn enriched_access(field_get: bool) -> (FieldProfile, Expr<Routed>) {
        let span = Span::new(0, 0);
        let field_ty = Type::synth_path(vec!["Field".to_owned()], Vec::new(), span);
        let row_ty = Type::Product {
            left: Box::new(Type::Unit {
                meta: Meta::new(span),
            }),
            right: Box::new(field_ty),
            meta: Meta::new(span),
        };
        let target = Box::new(Expr::LowBoundRef {
            occurrence: Default::default(),
            name: "row".to_owned(),
            meta: Meta::new(span),
            ext: (),
        });
        let expr = if field_get {
            Expr::EnrichedFieldGet {
                occurrence: Default::default(),
                target,
                field_name: "field".to_owned(),
                index: 1,
                arity: 2,
                target_ty: row_ty.clone(),
                meta: Meta::new(span),
                ext: (),
            }
        } else {
            Expr::EnrichedProject {
                occurrence: Default::default(),
                target,
                index: 1,
                arity: 2,
                target_ty: row_ty.clone(),
                meta: Meta::new(span),
                ext: (),
            }
        };
        (FieldProfile { row_ty }, expr)
    }

    fn direct_field_get(record: bool) -> (FieldProfile, Expr<Routed>) {
        let span = Span::new(0, 0);
        let field_ty = Type::synth_path(vec!["Field".to_owned()], Vec::new(), span);
        let unit_ty = Type::Unit {
            meta: Meta::new(span),
        };
        let row_ty = Type::Product {
            left: Box::new(unit_ty.clone()),
            right: Box::new(field_ty),
            meta: Meta::new(span),
        };
        let first = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
        let selected = Expr::LowBoundRef {
            occurrence: Default::default(),
            name: if record { "payload" } else { "carrier" }.to_owned(),
            meta: Meta::new(span),
            ext: (),
        };
        let target = if record {
            Expr::EnrichedRecord {
                occurrence: Default::default(),
                fields: vec![
                    crate::ast::RecordField {
                        name: "first".to_owned(),
                        value: first,
                        meta: Meta::new(span),
                    },
                    crate::ast::RecordField {
                        name: "field".to_owned(),
                        value: selected,
                        meta: Meta::new(span),
                    },
                ],
                synth_ty: row_ty.clone(),
                meta: Meta::new(span),
                ext: (),
            }
        } else {
            Expr::EnrichedTuple {
                occurrence: Default::default(),
                items: vec![first, selected],
                synth_ty: row_ty.clone(),
                meta: Meta::new(span),
                ext: (),
            }
        };
        (
            FieldProfile {
                row_ty: row_ty.clone(),
            },
            Expr::EnrichedFieldGet {
                occurrence: Default::default(),
                target: Box::new(target),
                field_name: "field".to_owned(),
                index: 1,
                arity: 2,
                target_ty: row_ty,
                meta: Meta::new(span),
                ext: (),
            },
        )
    }

    #[test]
    fn unit_reconstructs_structurally() {
        let span = Span::new(0, 0);
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };
        let expr = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };

        assert!(matches!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(Type::Unit { .. })
        ));
    }

    #[test]
    fn generic_host_call_reconstructs_instantiated_return_type() {
        let span = Span::new(0, 0);
        let bool_ty = Type::synth_path(vec!["Bool".to_owned()], Vec::new(), span);
        let t_ty = Type::synth_path(vec!["T".to_owned()], Vec::new(), span);
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "T".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(crate::ast::Param {
                name: "value".to_owned(),
                ty: Some(t_ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            }),
        ]);
        let expr = Expr::LowHostCall {
            occurrence: Default::default(),
            name: "id".to_owned(),
            module_path: "testapi".to_owned(),
            type_args: vec![bool_ty.clone()],
            args: vec![Expr::BoolLit {
                occurrence: Default::default(),
                value: true,
                annotation: bool_ty.clone(),
                meta: Meta::new(span),
            }],
            sig,
            ret_ty: t_ty,
            meta: Meta::new(span),
            ext: (),
        };
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };

        assert_eq!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(bool_ty)
        );
    }

    #[test]
    fn rank_n_host_and_lambda_values_preserve_signature_groups() {
        let span = Span::new(0, 0);
        let a_ty = Type::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let b_ty = Type::synth_path(vec!["B".to_owned()], Vec::new(), span);
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(crate::ast::Param {
                name: "first".to_owned(),
                ty: Some(a_ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            }),
            SignatureParam::Type(crate::ast::TypeParam {
                name: "B".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(crate::ast::Param {
                name: "second".to_owned(),
                ty: Some(b_ty),
                pattern: (),
                meta: Meta::new(span),
            }),
        ]);
        let expected = sig.signature_ty(a_ty.clone(), span);
        let host_value = Expr::LowHostFnValueRef {
            occurrence: Default::default(),
            name: "choose".to_owned(),
            module_path: "testapi".to_owned(),
            sig: sig.clone(),
            ret_ty: a_ty.clone(),
            meta: Meta::new(span),
            ext: (),
        };
        let lambda_value = Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty: Some(a_ty),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };

        for value in [host_value, lambda_value] {
            assert_eq!(
                value_type_with_locals(&profile, &value, &mut HashMap::new()),
                Some(expected.clone())
            );
        }
    }

    #[test]
    fn omitted_lambda_return_reconstructs_from_its_typed_binder() {
        let span = Span::new(0, 0);
        let a_ty = Type::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(crate::ast::Param {
                name: "item".to_owned(),
                ty: Some(a_ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            }),
        ]);
        let expected = sig.signature_ty(a_ty, span);
        let expr = Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty: None,
            body: Box::new(Expr::LowBoundRef {
                occurrence: Default::default(),
                name: "item".to_owned(),
                meta: Meta::new(span),
                ext: (),
            }),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };

        assert_eq!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(expected)
        );
    }

    #[test]
    fn nested_omitted_return_reconstructs_a_rank_n_function_value() {
        let span = Span::new(0, 0);
        let unit_ty = Type::Unit {
            meta: Meta::new(span),
        };
        let a_ty = Type::synth_path(vec!["A".to_owned()], Vec::new(), span);
        let inner_sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(crate::ast::Param {
                name: "item".to_owned(),
                ty: Some(a_ty.clone()),
                pattern: (),
                meta: Meta::new(span),
            }),
        ]);
        let inner_ty = inner_sig.signature_ty(a_ty, span);
        let inner = Expr::FnExpr {
            occurrence: Default::default(),
            sig: inner_sig,
            ret_ty: None,
            body: Box::new(Expr::LowBoundRef {
                occurrence: Default::default(),
                name: "item".to_owned(),
                meta: Meta::new(span),
                ext: (),
            }),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let outer_sig = Signature::new(vec![SignatureParam::Value(crate::ast::Param {
            name: "unit".to_owned(),
            ty: Some(unit_ty),
            pattern: (),
            meta: Meta::new(span),
        })]);
        let expected = outer_sig.signature_ty(inner_ty, span);
        let outer = Expr::FnExpr {
            occurrence: Default::default(),
            sig: outer_sig,
            ret_ty: None,
            body: Box::new(inner),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };

        assert_eq!(
            value_type_with_locals(&profile, &outer, &mut HashMap::new()),
            Some(expected)
        );
    }

    #[test]
    fn lambda_body_reconstruction_restores_shadowed_locals() {
        let span = Span::new(0, 0);
        let prior_ty = Type::synth_path(vec!["Prior".to_owned()], Vec::new(), span);
        let param_ty = Type::synth_path(vec!["Param".to_owned()], Vec::new(), span);
        let sig = Signature::new(vec![SignatureParam::Value(crate::ast::Param {
            name: "item".to_owned(),
            ty: Some(param_ty.clone()),
            pattern: (),
            meta: Meta::new(span),
        })]);
        let expr = Expr::FnExpr {
            occurrence: Default::default(),
            sig: sig.clone(),
            ret_ty: None,
            body: Box::new(Expr::LowBoundRef {
                occurrence: Default::default(),
                name: "item".to_owned(),
                meta: Meta::new(span),
                ext: (),
            }),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let profile = FieldProfile {
            row_ty: Type::Unit {
                meta: Meta::new(span),
            },
        };
        let mut locals = HashMap::from([("item".to_owned(), prior_ty.clone())]);

        assert_eq!(
            value_type_with_locals(&profile, &expr, &mut locals),
            Some(sig.signature_ty(param_ty, span))
        );
        assert_eq!(locals.get("item"), Some(&prior_ty));
    }

    #[test]
    fn field_get_reconstructs_the_label_payload() {
        let (profile, expr) = enriched_access(true);

        assert!(matches!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(Type::Unit { .. })
        ));
    }

    #[test]
    fn positional_project_retains_the_newtype_slot() {
        let (profile, expr) = enriched_access(false);

        assert!(matches!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(Type::Path { segments, .. })
                if segments.last().is_some_and(|segment| segment.as_str() == "Field")
        ));
    }

    #[test]
    fn field_get_projects_a_direct_tuple_carrier() {
        let (profile, expr) = direct_field_get(false);

        assert!(matches!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(Type::Unit { .. })
        ));
    }

    #[test]
    fn field_get_keeps_a_direct_record_payload() {
        let (profile, expr) = direct_field_get(true);

        assert!(matches!(
            value_type_with_locals(&profile, &expr, &mut HashMap::new()),
            Some(Type::Unit { .. })
        ));
    }
}

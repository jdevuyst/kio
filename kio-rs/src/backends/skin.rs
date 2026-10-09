//! Shared boundary-conversion primitives used by backend emitters.
//!
//! Public callable topology, semantic slots, structural shells, and nominal
//! dependencies are planned once in [`crate::backends::boundary_facade`]. This
//! module retains only reusable conversion algorithms and IR helpers:
//! boundary arity, exact type reach and substitution utilities,
//! canonical hashing, and the first-non-colliding key selector. A backend's
//! public declaration catalog and naming policy stay in its prepared facade
//! renderer; this module does not discover or register a second public shape
//! inventory.

use crate::ast::{Newtype, Role, Routed, Type, TypeParam};
use crate::pass::resolve::Package;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Direction of a boundary conversion: which side of the FFI the
/// `expr` being converted currently sits on.
///
/// Backend internal value representations differ, but the
/// boundary they cross is the same: the *host-facing skin shape* on one
/// side and the package's *internal representation* on the other. A
/// conversion runs in one of two directions, and the direction flips
/// across a function value's parameter leg (a value handed *into* an
/// exported fn is converted the opposite way from the fn's result), so
/// each boundary converter threads the direction through its parameter
/// and result conversions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfiDir {
    /// Host-facing skin shape → package-internal representation. Runs at
    /// an exported fn's parameter entry and at the return of a host call.
    In,
    /// Package-internal representation → host-facing skin shape. Runs at
    /// an exported fn's return and at a host call's arguments.
    Out,
}

impl FfiDir {
    /// The direction a function value's *parameter* leg converts, given
    /// the direction the function value itself is being converted.
    ///
    /// A function flowing `Out` (internal → host) receives host-shaped
    /// arguments that must be converted `In` before the internal body
    /// runs; a function flowing `In` is symmetric. Every backend's
    /// function conversion flips the same way, so the rule lives on the
    /// shared direction.
    pub fn flip(self) -> FfiDir {
        match self {
            FfiDir::Out => FfiDir::In,
            FfiDir::In => FfiDir::Out,
        }
    }
}

/// The semantic parameter arity of a polymorphic-function newtype payload
/// at the host boundary.
///
/// The host sees the semantic right-spine product (except that a raw
/// zero-arity domain remains zero slots), while the stored callable keeps the
/// lowering ABI recorded by [`Type::Function::abi_arity`]. Recovery may pack
/// several semantic slots into the stored callable's final argument, so its
/// ABI arity does not determine the host arity.
pub(crate) fn function_boundary_arity(param: &Type<Routed>, abi_arity: usize) -> usize {
    let internal_slots = Type::right_spine_take(param, abi_arity);
    assert_eq!(
        internal_slots.len(),
        abi_arity,
        "Routed function parameter spine is shorter than its ABI arity"
    );
    let boundary_slots = if abi_arity == 0 {
        Vec::new()
    } else {
        Type::right_spine_product(param)
    };
    assert!(
        internal_slots.len() <= boundary_slots.len(),
        "Routed function ABI has more slots than its semantic parameter spine"
    );
    boundary_slots.len()
}

/// Assign a host-visible key to each of `slot_count` spine slots by
/// first-non-colliding selection over per-slot candidate lists.
///
/// This is the shared collision-resolution core of the README's
/// [§ 3-step key fallback](../../../specs/backends/README.md). For
/// each slot `i`, the backend supplies an ordered list of candidate
/// keys via `candidates_for(i)`; the driver picks the first candidate
/// not already claimed by an earlier slot. A slot whose candidates are
/// all taken (or empty) falls back to the positional key `_<i>`, which
/// is unique per index and therefore can never itself collide.
///
/// The candidate *ordering* is the backend's policy: the canonical
/// `[bare, qualified, positional]` of [`three_step_candidates`] (JS,
/// which keys every structural slot directly), or a subset (Rust keys
/// only sum arms, and against its own identifier grammar). The driver
/// is agnostic to which.
pub fn assign_spine_keys<F>(slot_count: usize, mut candidates_for: F) -> Vec<String>
where
    F: FnMut(usize) -> Vec<String>,
{
    let mut taken: HashSet<String> = HashSet::new();
    let mut keys = Vec::with_capacity(slot_count);
    for i in 0..slot_count {
        let chosen = candidates_for(i)
            .into_iter()
            .find(|c| !taken.contains(c))
            .unwrap_or_else(|| positional_key(i));
        taken.insert(chosen.clone());
        keys.push(chosen);
    }
    keys
}

/// The positional fallback key for spine slot `index`: `_<index>`.
///
/// Step 3 of the README's 3-step key fallback. Unique per index, so
/// two distinct slots never propose the same positional key.
pub fn positional_key(index: usize) -> String {
    format!("_{index}")
}

/// The README's canonical 3-step candidate ordering for one slot.
///
/// When the slot resolves to a newtype, `resolved` carries its
/// `(bare, qualified)` spelling and the ordering is
/// `[bare, qualified, positional]` — step 1 (bare newtype name),
/// step 2 (canonical `<modulepath>.<name>`), step 3 (positional).
/// When the slot does not resolve to a newtype, only the positional
/// candidate applies. Feed the result to [`assign_spine_keys`]'s
/// `candidates_for`.
///
/// A backend whose identifier grammar cannot carry a given spelling
/// (Rust cannot put `.` in an identifier) renders its own candidates
/// rather than calling this; this is the spelling a host that accepts
/// arbitrary string keys (JS object keys) uses verbatim.
pub fn three_step_candidates(resolved: Option<(String, String)>, index: usize) -> Vec<String> {
    match resolved {
        Some((bare, qualified)) => vec![bare, qualified, positional_key(index)],
        None => vec![positional_key(index)],
    }
}

// =========================================================================
// Backend-neutral IR helpers — deterministic hashing, type reach, payload
// instantiation, and type-variable substitution. Public facade topology is
// already prepared before these helpers run.
// =========================================================================

/// FNV-1a 64-bit hash of `s`, as 16 lowercase hex digits.
///
/// The caller supplies the complete identity being bounded or disambiguated;
/// the lowercase hex is identifier-safe in every shipping host language.
pub(crate) fn fnv1a_64_hex(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Conservative component budget for a backend-generated readable identifier
/// before a target-specific exact-identity digest is required.
pub(crate) const READABLE_MINT_MAX_LEN: usize = 64;

/// Keep a readable name within the shared byte budget, otherwise construct its
/// backend-specific digest spelling from the complete owner identity. The
/// fallback is lazy so a short name needs no additional identity rendering.
/// Public collision rejection and private claim allocation remain separate.
pub(crate) fn budgeted_mint_name<E>(
    readable: String,
    fallback: impl FnOnce(&str) -> Result<String, E>,
) -> Result<String, E> {
    if readable.len() <= READABLE_MINT_MAX_LEN {
        Ok(readable)
    } else {
        fallback(&readable)
    }
}

/// The candidate schedule for claiming a readable mint name owned by
/// `owner` (the shape's canonical identity): the base name itself, then
/// the base with the owner's FNV-1a hash suffix at 8 and 16 hex digits.
/// A claimer walks the schedule and takes the first candidate not held by a
/// different owner; exhausting it means two distinct owners share a full
/// 64-bit FNV-1a hash, which the claimer surfaces loudly. This helper is for
/// private/internal names; public facade shell identity is the injective
/// `FacadeShellId` codec and never depends on registry occupancy.
pub(crate) fn mint_claim_candidates(base: &str, owner: &str) -> [String; 3] {
    let hash = fnv1a_64_hex(owner);
    [
        base.to_owned(),
        format!("{base}_{}", &hash[..8]),
        format!("{base}_{}", &hash[..16]),
    ]
}

pub(crate) enum TypeReachAction<C> {
    IgnoreSubtree,
    TraverseArguments,
    Found,
    Descend {
        ty: Type<Routed>,
        context: C,
        traverse_arguments: bool,
    },
}

/// Structural reachability driver for backend-side decisions that must
/// agree with a renderer's type-scope normalization. The caller owns
/// the backend policy: normalize the current node, report terminal leaves,
/// classify path subtrees, and update scope when entering a forall. A path
/// visit decides in one place whether to ignore the subtree, inspect only its
/// syntactic arguments, report a match, or descend through a resolved payload
/// with or without independently inspecting its arguments.
pub(crate) fn type_reaches_with<C, Normalize, PreVisit, PathVisit, EnterForall>(
    ty: &Type<Routed>,
    context: C,
    normalize: &Normalize,
    pre_visit: &mut PreVisit,
    path_visit: &mut PathVisit,
    enter_forall: &EnterForall,
) -> bool
where
    C: Clone,
    Normalize: Fn(&Type<Routed>, &C) -> Type<Routed>,
    PreVisit: FnMut(&Type<Routed>, &C) -> bool,
    PathVisit: FnMut(&Type<Routed>, &C) -> TypeReachAction<C>,
    EnterForall: Fn(&C, &TypeParam) -> C,
{
    let normalized = normalize(ty, &context);
    if pre_visit(&normalized, &context) {
        return true;
    }
    match &normalized {
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            type_reaches_with(
                left,
                context.clone(),
                normalize,
                pre_visit,
                path_visit,
                enter_forall,
            ) || type_reaches_with(
                right,
                context,
                normalize,
                pre_visit,
                path_visit,
                enter_forall,
            )
        }
        Type::Function { param, ret, .. } => {
            type_reaches_with(
                param,
                context.clone(),
                normalize,
                pre_visit,
                path_visit,
                enter_forall,
            ) || type_reaches_with(ret, context, normalize, pre_visit, path_visit, enter_forall)
        }
        Type::Forall { param, body, .. } => type_reaches_with(
            body,
            enter_forall(&context, param),
            normalize,
            pre_visit,
            path_visit,
            enter_forall,
        ),
        Type::Path { args, .. } => {
            let arguments_reach = |pre_visit: &mut PreVisit, path_visit: &mut PathVisit| {
                args.iter().any(|arg| {
                    type_reaches_with(
                        arg,
                        context.clone(),
                        normalize,
                        pre_visit,
                        path_visit,
                        enter_forall,
                    )
                })
            };
            match path_visit(&normalized, &context) {
                TypeReachAction::IgnoreSubtree => false,
                TypeReachAction::TraverseArguments => arguments_reach(pre_visit, path_visit),
                TypeReachAction::Found => true,
                TypeReachAction::Descend {
                    ty,
                    context,
                    traverse_arguments,
                } => {
                    (traverse_arguments && arguments_reach(pre_visit, path_visit))
                        || type_reaches_with(
                            &ty,
                            context,
                            normalize,
                            pre_visit,
                            path_visit,
                            enter_forall,
                        )
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::LabelSugar { .. } | Type::Infer { .. } => {
            false
        }
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Replace lexically bound type-variable references with an opaque path
/// before a typed host boundary resolves nominal declarations. The
/// placeholder cannot be written as a Kio type name, so a same-leaf
/// newtype elsewhere in the package cannot capture it. An application whose
/// constructor is already that placeholder stays the same atomic carrier:
/// the erased-static boundary gives an abstract HKT application no distinct
/// shape.
pub(crate) fn erase_scoped_type_vars(ty: &Type<Routed>, scope: &BTreeSet<String>) -> Type<Routed> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            if let [name] = segments.as_slice()
                && (scope.contains(name.as_str()) || name.as_str() == "$")
            {
                Type::synth_path(vec!["$".to_owned()], Vec::new(), meta.span)
            } else {
                Type::Path {
                    segments: segments.clone(),
                    args: args
                        .iter()
                        .map(|arg| erase_scoped_type_vars(arg, scope))
                        .collect(),
                    meta: meta.clone(),
                }
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } => ty.clone(),
        Type::Function {
            param,
            ret,
            abi_arity,
            caps,
            meta,
        } => Type::Function {
            param: Box::new(erase_scoped_type_vars(param, scope)),
            ret: Box::new(erase_scoped_type_vars(ret, scope)),
            abi_arity: *abi_arity,
            caps: caps.clone(),
            meta: meta.clone(),
        },
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(erase_scoped_type_vars(left, scope)),
            right: Box::new(erase_scoped_type_vars(right, scope)),
            meta: meta.clone(),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(erase_scoped_type_vars(left, scope)),
            right: Box::new(erase_scoped_type_vars(right, scope)),
            meta: meta.clone(),
        },
        Type::Forall { param, body, meta } => {
            let mut nested = scope.clone();
            nested.insert(param.name.clone());
            Type::Forall {
                param: param.clone(),
                body: Box::new(erase_scoped_type_vars(body, &nested)),
                meta: meta.clone(),
            }
        }
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Binding layers for bodies whose erased type binders are nullary closures.
/// Value groups retain their source names; an empty signature has one nullary
/// value layer. This follows abstraction order, not public call inference.
pub(crate) fn runtime_param_name_groups(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<&str>> {
    let mut groups = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                groups.extend(params.iter().map(|_| Vec::new()));
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                groups.push(
                    params
                        .iter()
                        .filter_map(|param| match param {
                            crate::ast::SignatureParam::Value(param) => Some(param.name.as_str()),
                            crate::ast::SignatureParam::Type(_) => None,
                        })
                        .collect(),
                );
            }
        }
    }
    if groups.is_empty() {
        groups.push(Vec::new());
    }
    groups
}

pub(crate) fn erase_signature_value_groups(
    sig: &crate::ast::Signature<Routed>,
) -> Vec<Vec<Option<Type<Routed>>>> {
    let mut scope = BTreeSet::new();
    let mut groups = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                for param in params {
                    if let crate::ast::SignatureParam::Type(param) = param {
                        scope.insert(param.name.clone());
                    }
                }
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                groups.push(
                    params
                        .iter()
                        .filter_map(|param| match param {
                            crate::ast::SignatureParam::Value(param) => Some(
                                param
                                    .ty
                                    .as_ref()
                                    .map(|ty| erase_scoped_type_vars(ty, &scope)),
                            ),
                            crate::ast::SignatureParam::Type(_) => None,
                        })
                        .collect(),
                );
            }
        }
    }
    groups
}

pub(crate) fn qualify_newtype_declaration_payload(
    declaration: &Newtype<Routed>,
    module_path: &str,
    package: &Package<Routed>,
) -> Type<Routed> {
    let Some(entry) = package.module(module_path) else {
        return declaration.payload.clone();
    };
    let locals = declaration
        .type_params
        .iter()
        .chain(&declaration.existential_params)
        .map(|param| (param.name.clone(), param.effective_kind()))
        .collect();
    crate::pass::resolve::qualify_routed_contract_type_in_module(
        &declaration.payload,
        &entry.module,
        &locals,
    )
}

/// The erased-static host boundary of one declared newtype member, retaining
/// the member scheme's real universal/existential structure.
pub(crate) struct ErasedNewtypeMemberBoundary {
    pub(crate) param_groups: Vec<Vec<Type<Routed>>>,
    pub(crate) existential_projector: bool,
}

/// Project a public newtype member's exact synthesized scheme into the
/// canonical type-erased host ABI. In particular, an existential projector
/// remains two-stage CPS (`value`, then `continuation`) instead of being
/// reconstructed from an empty nominal application.
pub(crate) fn erased_newtype_member_boundary(
    package: &Package<Routed>,
    module_path: &str,
    declaration: &Newtype<Routed>,
    member: &str,
) -> Result<ErasedNewtypeMemberBoundary, String> {
    let scheme =
        super::reconstruct::newtype_member_scheme(package, module_path, declaration, member)?;

    let mut scope = BTreeSet::new();
    let mut outer = &scheme;
    while let Type::Forall { param, body, .. } = outer {
        scope.insert(param.name.clone());
        outer = body;
    }
    let Type::Function {
        param,
        ret,
        abi_arity,
        ..
    } = outer
    else {
        return Err(format!(
            "newtype member `{module_path}.{member}` did not synthesize a function scheme"
        ));
    };
    let outer_params = Type::right_spine_take(param, *abi_arity)
        .into_iter()
        .map(|ty| erase_scoped_type_vars(ty, &scope))
        .collect();

    let existential_projector =
        member == declaration.projector.name && !declaration.existential_params.is_empty();
    if !existential_projector {
        return Ok(ErasedNewtypeMemberBoundary {
            param_groups: vec![outer_params],
            existential_projector: false,
        });
    }

    let mut inner = ret.as_ref();
    while let Type::Forall { param, body, .. } = inner {
        scope.insert(param.name.clone());
        inner = body;
    }
    let Type::Function {
        param, abi_arity, ..
    } = inner
    else {
        return Err(format!(
            "existential projector `{module_path}.{member}` lost its CPS continuation layer"
        ));
    };
    let continuation_params = Type::right_spine_take(param, *abi_arity)
        .into_iter()
        .map(|ty| erase_scoped_type_vars(ty, &scope))
        .collect();
    Ok(ErasedNewtypeMemberBoundary {
        param_groups: vec![outer_params, continuation_params],
        existential_projector: true,
    })
}

/// Runtime value arity of an existential projector's continuation group,
/// taken from the exact synthesized member scheme after alias normalization.
pub(crate) fn existential_projector_continuation_arity(
    package: &Package<Routed>,
    module_path: &str,
    declaration: &Newtype<Routed>,
) -> Result<usize, String> {
    let boundary = erased_newtype_member_boundary(
        package,
        module_path,
        declaration,
        &declaration.projector.name,
    )?;
    if !boundary.existential_projector {
        return Err(format!(
            "newtype projector '{module_path}.{}' is not existential",
            declaration.projector.name
        ));
    }
    let continuation_group = boundary.param_groups.get(1).ok_or_else(|| {
        format!(
            "existential projector '{module_path}.{}' lost its continuation group",
            declaration.projector.name
        )
    })?;
    let [continuation] = continuation_group.as_slice() else {
        return Err(format!(
            "existential projector '{module_path}.{}' has {} continuation values instead of one",
            declaration.projector.name,
            continuation_group.len()
        ));
    };
    let mut continuation = continuation;
    while let Type::Forall { body, .. } = continuation {
        continuation = body;
    }
    let Type::Function { abi_arity, .. } = continuation else {
        return Err(format!(
            "existential projector '{module_path}.{}' lost its continuation function",
            declaration.projector.name
        ));
    };
    Ok(*abi_arity)
}

/// `true` if `name` is a compile-time / intrinsic type name: an `__…__`
/// intrinsic (`__Type__`, `__Checked_term__`, `__Comptime__`,
/// `__Diagnostic_text__`, …) or one of the `Comptime_*` reflected
/// scalars. These are opaque at the FFI boundary (no host-visible
/// structured form). Shared with Haskell's module-scoped comptime walk.
pub(crate) fn is_comptime_type_name(name: &str) -> bool {
    (name.starts_with("__") && name.ends_with("__")) || name.starts_with("Comptime_")
}

pub(crate) fn type_vars_referenced_in_args(
    args: &[Type<Routed>],
    type_vars: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for arg in args {
        collect_referenced_type_vars(arg, type_vars, &mut out);
    }
    out
}

fn collect_referenced_type_vars(
    ty: &Type<Routed>,
    type_vars: &BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    match ty {
        Type::Path { segments, args, .. } => {
            if segments.len() == 1
                && let Some(name) = segments.first().map(|s| s.as_str())
                && type_vars.contains(name)
            {
                out.insert(name.to_owned());
            }
            for arg in args {
                collect_referenced_type_vars(arg, type_vars, out);
            }
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_referenced_type_vars(left, type_vars, out);
            collect_referenced_type_vars(right, type_vars, out);
        }
        Type::Function { param, ret, .. } => {
            collect_referenced_type_vars(param, type_vars, out);
            collect_referenced_type_vars(ret, type_vars, out);
        }
        Type::Forall { param, body, .. } => {
            let mut scoped = type_vars.clone();
            scoped.remove(&param.name);
            collect_referenced_type_vars(body, &scoped, out);
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::LabelSugar { .. } | Type::Infer { .. } => {}
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Instantiate a newtype's payload by substituting the reference's type
/// arguments for the newtype's type parameters. A newtype with no type
/// args (or fewer than its params) leaves the unfilled params as their
/// declared type variables.
pub(crate) fn instantiate_newtype_payload(
    d: &Newtype<Routed>,
    reference: &Type<Routed>,
) -> Type<Routed> {
    let args: &[Type<Routed>] = match reference {
        Type::Path { args, .. } => args,
        _ => &[],
    };
    let subst: HashMap<String, Type<Routed>> = d
        .type_params
        .iter()
        .zip(args.iter())
        .map(|(param, arg)| (param.name.clone(), arg.clone()))
        .collect();
    crate::pass::typecheck_core::subst_type(&d.payload, &subst)
}

/// Substitute free type-variable references in `ty` per `subst`.
#[cfg(test)]
pub(crate) fn substitute_type_vars(
    ty: &Type<Routed>,
    subst: &BTreeMap<String, Type<Routed>>,
) -> Type<Routed> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } if segments.len() == 1 && args.is_empty() => {
            if let Some(replacement) = subst.get(segments[0].as_str()) {
                replacement.clone()
            } else {
                Type::Path {
                    segments: segments.clone(),
                    args: args.clone(),
                    meta: meta.clone(),
                }
            }
        }
        Type::Path {
            segments,
            args,
            meta,
        } => Type::Path {
            segments: segments.clone(),
            args: args
                .iter()
                .map(|a| substitute_type_vars(a, subst))
                .collect(),
            meta: meta.clone(),
        },
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(substitute_type_vars(left, subst)),
            right: Box::new(substitute_type_vars(right, subst)),
            meta: meta.clone(),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(substitute_type_vars(left, subst)),
            right: Box::new(substitute_type_vars(right, subst)),
            meta: meta.clone(),
        },
        Type::Function {
            param,
            ret,
            abi_arity,
            caps,
            meta,
        } => Type::Function {
            param: Box::new(substitute_type_vars(param, subst)),
            ret: Box::new(substitute_type_vars(ret, subst)),
            abi_arity: *abi_arity,
            caps: caps.clone(),
            meta: meta.clone(),
        },
        Type::Forall { param, body, meta } => {
            // Shadow: drop a substitution the binder rebinds.
            let mut inner = subst.clone();
            inner.remove(&param.name);
            Type::Forall {
                param: param.clone(),
                body: Box::new(substitute_type_vars(body, &inner)),
                meta: meta.clone(),
            }
        }
        Type::Unit { meta } => Type::Unit { meta: meta.clone() },
        Type::Bottom { meta } => Type::Bottom { meta: meta.clone() },
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// A stable, backend-neutral canonical string for a `Routed` type, used as
/// the shape-mint hash key. Two structurally equal FFI shapes must
/// produce the same string (so they share one mint); distinct shapes
/// must differ. The string is never shown to a user — only hashed.
pub(crate) fn canonical_type_string(ty: &Type<Routed>) -> String {
    let mut s = String::new();
    write_canonical(ty, &mut s);
    s
}

fn write_canonical(ty: &Type<Routed>, out: &mut String) {
    match ty {
        Type::Unit { .. } => out.push_str("()"),
        Type::Bottom { .. } => out.push('!'),
        Type::Product { left, right, .. } => {
            out.push('(');
            write_canonical(left, out);
            out.push_str(" & ");
            write_canonical(right, out);
            out.push(')');
        }
        Type::Sum { left, right, .. } => {
            out.push('(');
            write_canonical(left, out);
            out.push_str(" | ");
            write_canonical(right, out);
            out.push(')');
        }
        Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => {
            out.push_str(&format!("fn{abi_arity}("));
            write_canonical(param, out);
            out.push_str(") -> ");
            write_canonical(ret, out);
        }
        Type::Forall { param, body, .. } => {
            out.push_str(&format!("forall {}. ", param.name));
            write_canonical(body, out);
        }
        Type::Path { segments, args, .. } => {
            let path: Vec<&str> = segments.iter().map(|s| s.as_str()).collect();
            out.push_str(&path.join("."));
            if !args.is_empty() {
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_canonical(a, out);
                }
                out.push(')');
            }
        }
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Exact declaring identity to literal role for host types on a package
/// boundary. Routed nominal paths carry the same `(module, name)` identity,
/// so same-leaf declarations never overwrite one another.
pub(crate) type HostRoleTable = BTreeMap<(String, String), Role>;

/// Exact nullary host declaration identity to optional literal role. A
/// roleless declaration is deliberately present with `None`.
pub(crate) type ExactHostTypeTable = BTreeMap<(String, String), Option<Role>>;

pub(crate) fn exact_nullary_host_type_table(
    desc: &crate::host_descriptor::HostDescriptor<'_>,
) -> ExactHostTypeTable {
    desc.host_types
        .iter()
        .filter(|host_type| host_type.type_params.is_empty())
        .map(|host_type| {
            (
                (host_type.module_path.to_owned(), host_type.name.to_owned()),
                host_type.role,
            )
        })
        .collect()
}

pub(crate) fn host_role_table(desc: &crate::host_descriptor::HostDescriptor<'_>) -> HostRoleTable {
    desc.host_types
        .iter()
        .filter_map(|host_type| {
            host_type.role.map(|role| {
                (
                    (host_type.module_path.to_owned(), host_type.name.to_owned()),
                    role,
                )
            })
        })
        .collect()
}

pub(crate) fn exact_host_role(roles: &HostRoleTable, ty: &Type<Routed>) -> Option<Role> {
    if !matches!(ty, Type::Path { args, .. } if args.is_empty()) {
        return None;
    }
    let (module, name) = crate::host_descriptor::routed_host_type_identity(ty)?;
    roles.get(&(module, name.to_owned())).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Meta, Param, PathSegment, Signature, SignatureGroup};
    use crate::span::Span;
    #[cfg(feature = "surface")]
    use crate::{
        pass::parser::{parse, parse_package_file},
        pass::resolve::PackageFileEntry,
    };
    #[cfg(feature = "surface")]
    use std::path::{Path, PathBuf};

    fn meta() -> Meta<Routed> {
        Meta::new(Span::new(0, 0))
    }

    fn path(name: &str) -> Type<Routed> {
        Type::Path {
            segments: vec![PathSegment::new(name.to_owned(), Span::new(0, 0))],
            args: vec![],
            meta: meta(),
        }
    }

    #[test]
    fn runtime_binding_groups_preserve_erased_and_value_stages() {
        let type_param = |name: &str| TypeParam {
            presentation: Default::default(),
            name: name.to_owned(),
            span: Span::new(0, 0),
            kind: None,
        };
        let value_param = |name: &str| Param {
            name: name.to_owned(),
            ty: Some(Type::Unit { meta: meta() }),
            pattern: (),
            meta: meta(),
        };
        let signature = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("A"), type_param("B")]),
            SignatureGroup::Value(vec![value_param("left"), value_param("right")]),
            SignatureGroup::Type(vec![type_param("C")]),
            SignatureGroup::Value(vec![]),
            SignatureGroup::Value(vec![value_param("__kio_tail")]),
        ]);

        assert_eq!(
            runtime_param_name_groups(&signature),
            vec![
                vec![],
                vec![],
                vec!["left", "right"],
                vec![],
                vec![],
                vec!["__kio_tail"],
            ]
        );
        let type_only = Signature::from_groups(vec![SignatureGroup::Type(vec![
            type_param("A"),
            type_param("B"),
        ])]);
        assert_eq!(
            runtime_param_name_groups(&type_only),
            vec![Vec::<&str>::new(), Vec::new()]
        );
    }

    #[test]
    fn runtime_binding_empty_signature_has_one_nullary_layer() {
        let signature = Signature::from_groups(vec![]);
        assert_eq!(
            runtime_param_name_groups(&signature),
            vec![Vec::<&str>::new()]
        );
    }

    fn qualified_path(module: &str, name: &str) -> Type<Routed> {
        Type::Path {
            segments: module
                .split('/')
                .chain(std::iter::once(name))
                .map(|segment| PathSegment::new(segment.to_owned(), Span::new(0, 0)))
                .collect(),
            args: vec![],
            meta: meta(),
        }
    }

    #[test]
    fn exact_host_roles_distinguish_same_leaf_declarations() {
        let roles = HostRoleTable::from([
            (("left".to_owned(), "Shared".to_owned()), Role::I32),
            (("right".to_owned(), "Shared".to_owned()), Role::Str),
        ]);

        assert_eq!(
            exact_host_role(&roles, &qualified_path("left", "Shared")),
            Some(Role::I32)
        );
        assert_eq!(
            exact_host_role(&roles, &qualified_path("right", "Shared")),
            Some(Role::Str)
        );
        assert_eq!(exact_host_role(&roles, &path("Shared")), None);
    }

    fn product(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: meta(),
        }
    }

    fn sum(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: meta(),
        }
    }

    fn function(param: Type<Routed>, ret: Type<Routed>, abi_arity: usize) -> Type<Routed> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: meta(),
            abi_arity,
            caps: crate::ast::FnTypeCapabilities::default(),
        }
    }

    #[test]
    fn function_boundary_arity_separates_semantic_and_stored_partitions() {
        let param = product(path("A"), product(path("B"), path("C")));
        for abi_arity in 1..=3 {
            assert_eq!(function_boundary_arity(&param, abi_arity), 3);
        }
    }

    #[test]
    fn function_boundary_arity_preserves_raw_zero_arity() {
        let unit = Type::Unit { meta: meta() };
        assert_eq!(function_boundary_arity(&unit, 0), 0);
        assert_eq!(function_boundary_arity(&unit, 1), 1);
    }

    #[cfg(feature = "surface")]
    fn module_file_path(module: &crate::ast::Module<crate::ast::Surface>) -> PathBuf {
        let segments = &module.path.segments;
        let mut path = PathBuf::new();
        for segment in &segments[..segments.len().saturating_sub(1)] {
            path.push(&segment.name);
        }
        let stem = segments
            .last()
            .map(|segment| segment.name.as_str())
            .unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    #[cfg(feature = "surface")]
    fn build_package(source: &str) -> Package<Routed> {
        build_package_with_modules(&[source])
    }

    #[cfg(feature = "surface")]
    fn build_package_with_modules(sources: &[&str]) -> Package<Routed> {
        use crate::pass::full::FullPipeline;
        use crate::pass::typecheck_full::check_package;
        use crate::pipeline::Pipeline;

        let parsed_modules = sources
            .iter()
            .map(|source| {
                let parsed = parse(source).expect("parse");
                (module_file_path(&parsed), parsed)
            })
            .collect();
        let package_file = parse_package_file("package scopecheck;\nbridge { main; }", None)
            .expect("parse package file");
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed_modules, Some(package_file)).expect("lower package");
        let package_file_entry = lowered_package_file.map(|entry| PackageFileEntry {
            file_path: PathBuf::from("scopecheck.pkg.kio"),
            package_name: "scopecheck".to_owned(),
            package_file: entry,
        });
        let package = Package::build(Path::new(""), lowered_modules, package_file_entry)
            .expect("build package");
        package.resolve_imports().expect("resolve imports");
        package.check_in_body_resolution().expect("body resolution");
        let prime = check_package(&package).expect("typecheck");
        let enriched = crate::pass::structural_recovery::recover_package(&prime);
        crate::pass::route::route_package(&enriched)
    }

    #[cfg(feature = "surface")]
    fn main_payload_plans(
        package: &Package<Routed>,
    ) -> BTreeMap<String, crate::backends::value_plan::ValuePlan> {
        let entry = package.module("main").expect("main module");
        let mut plans = BTreeMap::new();
        for item in &entry.module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(declaration) = declaration.newtype() {
                    let parameters = declaration
                        .type_params
                        .iter()
                        .chain(&declaration.existential_params)
                        .cloned()
                        .collect::<Vec<_>>();
                    let plan = crate::backends::value_plan::ValuePlan::from_routed_type(
                        package,
                        &entry.module,
                        &parameters,
                        &declaration.payload,
                    )
                    .expect("prepare exact declaration-owned payload");
                    plans.insert(declaration.name.clone(), plan);
                }
            });
        }
        plans
    }

    #[cfg(feature = "surface")]
    fn assert_swift_facade_unchanged(
        base: &Package<Routed>,
        extended: &Package<Routed>,
        added_internal_carriers: &[&str],
    ) {
        use crate::backends::boundary_facade::PreparedBoundaryCallableSites;

        assert_eq!(
            PreparedBoundaryCallableSites::collect_live(base).expect("base facade"),
            PreparedBoundaryCallableSites::collect_live(extended).expect("extended facade"),
        );
        assert_eq!(main_payload_plans(base), main_payload_plans(extended));
        let base = crate::backends::swift::emit::lower_package(base, "Scopecheck")
            .expect("emit base Swift facade");
        let extended = crate::backends::swift::emit::lower_package(extended, "Scopecheck")
            .expect("emit extended Swift facade");
        assert_eq!(base.host_swift, extended.host_swift);
        assert_eq!(base.ffi_swift, extended.ffi_swift);

        // Body-owned newtypes add internal carriers; no existing declaration,
        // storage field, witness, or public access may change with them.
        let mut shapes = extended.shapes_swift;
        for name in added_internal_carriers {
            let carrier = format!("KioNewtype_{name}");
            let added = format!(
                "struct {carrier}<H: ScopecheckHost>: __KioNominalValue {{\n    let __kioValue: Any\n    init(_ value: Any) {{ self.__kioValue = value }}\n}}\nprivate struct __KioWitness_{carrier}<H: ScopecheckHost>: __KioNominalWitness {{\n    typealias Native = {carrier}<H>\n    func construct(_ payload: Any) -> Any {{ Native(payload) }}\n}}\nfunc __kioWitness_{carrier}<H: ScopecheckHost>(_: H.Type) -> any __KioTypeWitness {{ __KioWitness_{carrier}<H>() }}\n"
            );
            assert_eq!(shapes.matches(&added).count(), 1, "{shapes}");
            shapes = shapes.replacen(&added, "", 1);
        }
        assert_eq!(base.shapes_swift, shapes);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn unbridged_public_declarations_do_not_change_bridged_shape_claims() {
        let main = "module main; \
                    host type Number role(i32); \
                    pub fn keep(value: (Number & Number)) -> (Number & Number) { value }";
        let base = build_package_with_modules(&[main]);
        let with_unbridged = build_package_with_modules(&[
            main,
            "module extra; \
             host type Text role(str); \
             pub fn distract(value: (Text & Text)) -> (Text & Text) { value } \
             pub newtype Noise : (Text | Text) { \
               pub constructor make_noise; \
               pub projector take_noise; \
             };",
        ]);

        assert_swift_facade_unchanged(&base, &with_unbridged, &["extra__Noise"]);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn local_binders_cannot_resolve_to_unrelated_newtypes_in_carrier_storage() {
        let main = "module main; \
                    host type Exact; \
                    pub newtype Existential <U> : U { \
                      constructor make_existential; \
                      projector read_existential; \
                    }; \
                    pub newtype Rank_n : [U] U { \
                      constructor make_rank_n; \
                      projector read_rank_n; \
                    }; \
                    newtype Hidden <V> : V { \
                      constructor make_hidden; \
                      projector read_hidden; \
                    }; \
                    pub newtype Outer : Hidden { \
                      constructor make_outer; \
                      projector read_outer; \
                    }; \
                    newtype Inner[A] : A { \
                      constructor make_inner; \
                      projector read_inner; \
                    }; \
                    pub newtype Nested <U> : Inner(U) { \
                      constructor make_nested; \
                      projector read_nested; \
                    };";
        let base = build_package_with_modules(&[main]);
        let with_same_leaf = build_package_with_modules(&[
            main,
            "module extra; \
             import main(Exact); \
             newtype U : Exact { constructor make_u; projector read_u; }; \
             newtype V : Exact { constructor make_v; projector read_v; };",
        ]);

        assert_swift_facade_unchanged(&base, &with_same_leaf, &["extra__U", "extra__V"]);
        use crate::backends::value_plan::ValueUse;
        let plans = main_payload_plans(&with_same_leaf);
        for name in ["Existential", "Hidden", "Inner"] {
            let plan = &plans[name];
            assert!(matches!(plan.use_at(plan.root()), ValueUse::Bound(_)));
        }
        let rank_n = &plans["Rank_n"];
        let ValueUse::Forall { binder, result } = rank_n.use_at(rank_n.root()) else {
            panic!("rank-N payload lost its binder");
        };
        assert_eq!(rank_n.use_at(*result), &ValueUse::Bound(*binder));
        let nested = &plans["Nested"];
        let ValueUse::Apply { constructor, args } = nested.use_at(nested.root()) else {
            panic!("nested payload lost its exact constructor application");
        };
        assert!(
            matches!(nested.use_at(*constructor), ValueUse::Nominal(name)
            if name.module_segments() == ["main"] && name.name() == "Inner")
        );
        assert_eq!(args.len(), 1);
        assert!(matches!(nested.use_at(args[0]), ValueUse::Bound(_)));
    }

    #[cfg(feature = "surface")]
    #[test]
    fn recursive_newtype_resolution_is_module_scoped() {
        let main = "module main; \
                    pub rec newtype Chain : . | Chain { \
                      pub constructor make_chain; \
                      pub projector read_chain; \
                    };";
        let base = build_package_with_modules(&[main]);
        let with_same_leaf = build_package_with_modules(&[
            "module aaa; \
             pub newtype Chain : . { \
               pub constructor make_chain; \
               pub projector read_chain; \
             };",
            main,
        ]);

        assert_swift_facade_unchanged(&base, &with_same_leaf, &["aaa__Chain"]);
        use crate::backends::value_plan::ValueUse;
        let plans = main_payload_plans(&with_same_leaf);
        let chain = &plans["Chain"];
        let ValueUse::Sum { left, right } = chain.use_at(chain.root()) else {
            panic!("recursive payload lost its sum");
        };
        assert_eq!(chain.use_at(*left), &ValueUse::Unit);
        assert!(matches!(chain.use_at(*right), ValueUse::Nominal(name)
            if name.module_segments() == ["main"] && name.name() == "Chain"));
    }
    #[test]
    fn scoped_type_var_erasure_enters_nested_forall_without_touching_nominals() {
        let nested = Type::Forall {
            param: crate::ast::TypeParam {
                presentation: Default::default(),
                name: "T".to_owned(),
                span: Span::new(0, 0),
                kind: None,
            },
            body: Box::new(function(path("T"), path("T"), 1)),
            meta: meta(),
        };
        let erased = erase_scoped_type_vars(&nested, &BTreeSet::new());
        let Type::Forall { body, .. } = erased else {
            panic!("forall retained")
        };
        let Type::Function { param, ret, .. } = *body else {
            panic!("function retained")
        };
        for leg in [param.as_ref(), ret.as_ref()] {
            let Type::Path { segments, args, .. } = leg else {
                panic!("bound type variable remains a path")
            };
            assert_eq!(segments.len(), 1);
            assert_eq!(segments[0].as_str(), "$");
            assert!(args.is_empty());
        }

        assert_eq!(
            canonical_type_string(&erase_scoped_type_vars(&path("T"), &BTreeSet::new())),
            "T"
        );

        let applied_type_var = Type::Path {
            segments: vec![PathSegment::new("F".to_owned(), Span::new(0, 0))],
            args: vec![path("A")],
            meta: meta(),
        };
        assert_eq!(
            canonical_type_string(&erase_scoped_type_vars(
                &applied_type_var,
                &BTreeSet::from(["F".to_owned()])
            )),
            "$"
        );
        let applied_erased_carrier = Type::Path {
            segments: vec![PathSegment::new("$".to_owned(), Span::new(0, 0))],
            args: vec![path("$")],
            meta: meta(),
        };
        assert_eq!(
            canonical_type_string(&erase_scoped_type_vars(
                &applied_erased_carrier,
                &BTreeSet::new()
            )),
            "$"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn typed_ffi_artifacts_observe_ordered_and_nested_binder_scope() {
        let package = build_package(
            "module main; \
             host type Str role(str); \
             pub newtype T : Str { pub constructor make_t; pub projector un_t; }; \
             pub newtype Pack <T> : T { pub constructor make_pack; pub projector un_pack; }; \
             pub newtype Poly : [T] T -> T { pub constructor make_poly; pub projector un_poly; }; \
             pub newtype Capture[A] : [T] A -> T { pub constructor make_capture; pub projector un_capture; }; \
             host fn nested(f: [T] T -> T) -> Str; \
             host fn ordered(value: T)[T](later: T) -> T; \
             host fn packed(value: Pack) -> Pack; \
             host fn poly(value: Poly) -> Poly; \
             host fn capture(value: Capture(T)) -> Capture(T);",
        );
        let descriptor = crate::host_descriptor::build_host_descriptor(&package);
        let capture_ty = descriptor
            .host_fns
            .iter()
            .find(|host_fn| host_fn.source.name == "capture")
            .and_then(|host_fn| {
                host_fn.source.params.iter().find_map(|param| match param {
                    crate::ast::HostFnParam::Value(param) => Some(&param.ty),
                    crate::ast::HostFnParam::Type(_) => None,
                })
            })
            .expect("Capture host parameter");
        let Type::Path { segments, .. } = capture_ty else {
            panic!("Capture nominal")
        };
        let (name, module) = segments.split_last().expect("qualified Capture name");
        let name = crate::backends::boundary_facade::QualifiedTypeName::new(
            module.iter().map(|segment| segment.name.clone()).collect(),
            name.name.clone(),
        )
        .expect("qualified Capture identity");
        let Some(crate::backends::value_plan::ValueNominalDeclaration::Newtype(capture_decl)) =
            crate::backends::value_plan::nominal_declaration(&package, &name)
        else {
            panic!("Capture declaration")
        };
        assert_eq!(
            canonical_type_string(&instantiate_newtype_payload(capture_decl, capture_ty)),
            "forall T. fn1(main.T) -> T"
        );

        let swift = crate::backends::swift::emit::lower_package(&package, "Scopecheck")
            .expect("emit Swift");
        assert!(
            swift
                .host_swift
                .contains("associatedtype main__Str: ExpressibleByStringLiteral"),
            "{}",
            swift.host_swift
        );
        assert!(
            swift.host_swift.contains(
                "main__nested(_ arg0: KioForall_56313b65303b712a6662303b62303b<Self>) -> Self.main__Str"
            ),
            "{}",
            swift.host_swift
        );
        let swift_ordered = swift
            .host_swift
            .lines()
            .find(|line| line.contains("main__ordered<"))
            .expect("Swift ordered host member");
        assert!(
            swift_ordered.contains(
                "<KioType_0>(_ arg0: KioNewtype_main__T<Self>, _ arg1: KioType_0) -> KioType_0"
            ),
            "{swift_ordered}\n\n{}",
            swift.host_swift
        );
        assert!(
            swift.host_swift.contains(
                "main__packed(_ arg0: KioNewtype_main__Pack<Self>) -> KioNewtype_main__Pack<Self>"
            ),
            "{}",
            swift.host_swift
        );
        assert!(
            swift.host_swift.contains(
                "main__poly(_ arg0: KioNewtype_main__Poly<Self>) -> KioNewtype_main__Poly<Self>"
            ),
            "{}",
            swift.host_swift
        );
        assert!(
            swift
                .host_swift
                .contains("main__capture(_ arg0: KioApply1<KioNewtypeMk_main__Capture<Self>, KioNewtype_main__T<Self>>) -> KioApply1<KioNewtypeMk_main__Capture<Self>, KioNewtype_main__T<Self>>"),
            "{}",
            swift.host_swift
        );

        let haskell = crate::backends::haskell::emit::lower_package(&package, "Scopecheck")
            .expect("emit Haskell");
        assert!(
            haskell.pkg_hs.contains(
                "host__main__nested :: (forall (t_t :: Scopecheck.KioStandardDataKind.Type). m (t_t -> m t_t)) -> m (HostType__main__Str h)"
            ),
            "{}",
            haskell.pkg_hs
        );
        let haskell_ordered = haskell
            .pkg_hs
            .lines()
            .find(|line| line.contains("host__main__ordered ::"))
            .expect("Haskell ordered host member");
        assert!(
            haskell_ordered.contains(
                "forall (t_t :: Scopecheck.KioStandardDataKind.Type). (HostType__main__Str h) -> t_t -> m t_t"
            ),
            "{haskell_ordered}\n\n{}",
            haskell.pkg_hs
        );
        assert!(
            haskell.pkg_hs.contains(
                "host__main__packed :: (KioExists_main__Pack h m) -> m (KioExists_main__Pack h m)"
            ),
            "{}",
            haskell.pkg_hs
        );
        assert!(
            haskell.pkg_hs.contains(
                "host__main__poly :: (forall (t_t :: Scopecheck.KioStandardDataKind.Type). m (t_t -> m t_t)) -> m (forall (t_t :: Scopecheck.KioStandardDataKind.Type). m (t_t -> m t_t))"
            ),
            "{}",
            haskell.pkg_hs
        );
        assert!(
            haskell.pkg_hs.contains(
                "host__main__capture :: (KioType_main__Capture h m (HostType__main__Str h)) -> m (KioType_main__Capture h m (HostType__main__Str h))"
            ),
            "{}",
            haskell.pkg_hs
        );
    }

    #[test]
    fn ffi_dir_flip_is_an_involution() {
        assert_eq!(FfiDir::Out.flip(), FfiDir::In);
        assert_eq!(FfiDir::In.flip(), FfiDir::Out);
        assert_eq!(FfiDir::Out.flip().flip(), FfiDir::Out);
    }

    #[test]
    fn first_noncolliding_picks_bare_when_free() {
        // Two slots, distinct bare names — each keeps its bare key.
        let keys = assign_spine_keys(2, |i| match i {
            0 => three_step_candidates(Some(("A".into(), "m.A".into())), 0),
            _ => three_step_candidates(Some(("B".into(), "m.B".into())), 1),
        });
        assert_eq!(keys, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn bare_collision_falls_through_to_qualified() {
        // Two slots share the bare name `A`; the second falls through
        // to its qualified spelling.
        let keys = assign_spine_keys(2, |i| {
            three_step_candidates(Some(("A".into(), format!("m{i}.A"))), i)
        });
        assert_eq!(keys, vec!["A".to_string(), "m1.A".to_string()]);
    }

    #[test]
    fn bare_and_qualified_collision_falls_through_to_positional() {
        // Three slots share the same `(bare, qualified)` spelling. Only
        // a slot's *chosen* key joins the taken set (matching the
        // emitters' collision discipline), so slot 0 takes the bare
        // `A`, slot 1 falls to the qualified `m.A`, and slot 2 — now
        // finding both taken — exhausts steps 1 and 2 and lands on its
        // positional fallback `_2`.
        let keys = assign_spine_keys(3, |i| {
            three_step_candidates(Some(("A".into(), "m.A".into())), i)
        });
        assert_eq!(
            keys,
            vec!["A".to_string(), "m.A".to_string(), "_2".to_string()]
        );
    }

    #[test]
    fn non_newtype_slots_take_positional() {
        let keys = assign_spine_keys(3, |i| three_step_candidates(None, i));
        assert_eq!(
            keys,
            vec!["_0".to_string(), "_1".to_string(), "_2".to_string()]
        );
    }

    #[test]
    fn positional_keys_are_unique_and_never_collide() {
        // Every slot offers only `_<i>`; no two collide regardless of count.
        let keys = assign_spine_keys(5, |i| vec![positional_key(i)]);
        assert_eq!(keys, vec!["_0", "_1", "_2", "_3", "_4"]);
    }

    #[test]
    fn fnv1a_is_stable_and_distinguishes_shapes() {
        // The mint identity must be deterministic and collision-free for
        // distinct canonical strings (a collision returns the wrong host
        // type for a different shape — a correctness bug).
        let a = fnv1a_64_hex("(I32 | .)");
        let b = fnv1a_64_hex("(I32 | .)");
        let c = fnv1a_64_hex("(String | .)");
        assert_eq!(a, b, "same input hashes the same");
        assert_ne!(a, c, "distinct shapes hash distinctly");
        assert_eq!(a.len(), 16, "16 hex digits");
    }

    #[test]
    fn mint_budget_preserves_the_boundary_and_lazy_fallback() {
        for length in [63, 64] {
            let readable = "A".repeat(length);
            assert_eq!(
                budgeted_mint_name::<()>(readable.clone(), |_| {
                    panic!("an in-budget name needs no fallback identity")
                }),
                Ok(readable)
            );
        }
        let readable = "A".repeat(65);
        assert_eq!(
            budgeted_mint_name::<()>(readable.clone(), |identity| {
                assert_eq!(identity, readable);
                Ok(format!("Shape_H{}", fnv1a_64_hex(identity)))
            }),
            Ok(format!("Shape_H{}", fnv1a_64_hex(&readable)))
        );
        assert_eq!(
            budgeted_mint_name("é".repeat(33), |_| Err("identity error")),
            Err("identity error"),
            "the existing threshold counts bytes and propagates fallback errors"
        );
    }

    #[test]
    fn canonical_string_distinguishes_product_from_sum() {
        // The canonical string feeds the mint hash; a product and a sum
        // over the same factors must serialize differently so they mint
        // distinct host types.
        let prod = product(path("A"), path("B"));
        let total = sum(path("A"), path("B"));
        assert_eq!(canonical_type_string(&prod), "(A & B)");
        assert_eq!(canonical_type_string(&total), "(A | B)");
    }

    #[test]
    fn substitute_type_vars_replaces_free_and_respects_shadowing() {
        let mut subst: BTreeMap<String, Type<Routed>> = BTreeMap::new();
        subst.insert("T".to_owned(), path("Int"));
        // A free `T` is replaced.
        assert_eq!(
            canonical_type_string(&substitute_type_vars(&path("T"), &subst)),
            "Int"
        );
        // A `T` rebound by an enclosing `forall T.` is left untouched.
        let shadowed = Type::Forall {
            param: crate::ast::TypeParam {
                presentation: Default::default(),
                name: "T".to_owned(),
                span: Span::new(0, 0),
                kind: None,
            },
            body: Box::new(path("T")),
            meta: meta(),
        };
        assert_eq!(
            canonical_type_string(&substitute_type_vars(&shadowed, &subst)),
            "forall T. T"
        );
    }
}

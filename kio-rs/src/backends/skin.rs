//! Shared boundary-conversion primitives used by backend emitters.
//!
//! Public callable topology, semantic slots, structural shells, and nominal
//! dependencies are planned once in [`crate::backends::boundary_facade`]. This
//! module retains only reusable conversion algorithms and IR helpers: the
//! [`SkinProfile`] driver, exact type reach and substitution utilities,
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
/// the driver threads the direction through the walk and each profile
/// renders its leaves accordingly.
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
    /// function-leaf hook flips the same way, so the rule lives on the
    /// shared direction rather than in each profile.
    pub fn flip(self) -> FfiDir {
        match self {
            FfiDir::Out => FfiDir::In,
            FfiDir::In => FfiDir::Out,
        }
    }
}

/// The two parameter partitions of a polymorphic-function newtype payload at
/// the host boundary.
///
/// The host sees the semantic right-spine product (except that a raw
/// zero-arity domain remains zero slots), while the stored callable keeps the
/// lowering ABI recorded by [`Type::Function::abi_arity`]. Recovery may pack
/// several semantic slots into the stored callable's final argument, so these
/// partitions must never be conflated.
#[derive(Debug)]
pub struct FunctionBoundaryAdapterPlan<'a> {
    boundary_slots: Vec<&'a Type<Routed>>,
    internal_slots: Vec<&'a Type<Routed>>,
}

impl<'a> FunctionBoundaryAdapterPlan<'a> {
    pub(crate) fn new(param: &'a Type<Routed>, abi_arity: usize) -> Self {
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
        Self {
            boundary_slots,
            internal_slots,
        }
    }

    pub(crate) fn boundary_slots(&self) -> &[&'a Type<Routed>] {
        &self.boundary_slots
    }

    #[cfg(test)]
    pub(crate) fn internal_slots(&self) -> &[&'a Type<Routed>] {
        &self.internal_slots
    }

    pub(crate) fn boundary_arity(&self) -> usize {
        self.boundary_slots.len()
    }

    pub(crate) fn internal_arity(&self) -> usize {
        self.internal_slots.len()
    }

    /// Repartition converted canonical boundary slots into the stored
    /// callable's ABI arguments. Every leading ABI argument keeps one slot;
    /// the final argument receives the right-nested remainder.
    pub(crate) fn boundary_to_internal_args(
        &self,
        values: &[String],
        pair: impl FnMut(&str, &str) -> String,
    ) -> Vec<String> {
        assert_eq!(values.len(), self.boundary_arity());
        if self.internal_arity() == 0 {
            return Vec::new();
        }
        let final_start = self.internal_arity() - 1;
        let mut args = values[..final_start].to_vec();
        args.push(right_nest_values(&values[final_start..], pair));
        args
    }

    /// Rebuild the full semantic product from stored ABI arguments, then
    /// flatten it back to canonical boundary slots.
    pub(crate) fn internal_to_boundary_args(
        &self,
        values: &[String],
        pair: impl FnMut(&str, &str) -> String,
        mut project: impl FnMut(&str, usize, usize) -> String,
    ) -> Vec<String> {
        assert_eq!(values.len(), self.internal_arity());
        if self.boundary_arity() == 0 {
            return Vec::new();
        }
        let product = self.internal_product(values, pair);
        if self.boundary_arity() == 1 {
            return vec![product];
        }
        (0..self.boundary_arity())
            .map(|index| project(&product, index, self.boundary_arity()))
            .collect()
    }

    pub(crate) fn internal_product(
        &self,
        values: &[String],
        pair: impl FnMut(&str, &str) -> String,
    ) -> String {
        assert_eq!(values.len(), self.internal_arity());
        right_nest_values(values, pair)
    }
}

fn right_nest_values(values: &[String], mut pair: impl FnMut(&str, &str) -> String) -> String {
    let (last, prefix) = values
        .split_last()
        .expect("a non-zero function partition has at least one value");
    let mut nested = last.clone();
    for value in prefix.iter().rev() {
        nested = pair(value, &nested);
    }
    nested
}

/// The per-backend variation points of the FFI boundary-wrapper.
///
/// A backend implements this against its own value representation; the
/// shared [`convert`] driver walks the signature's type and calls these
/// hooks at each leaf. The profile is the visitor: it carries the
/// borrowed per-backend state (JS the resolved [`Package`], Rust its
/// emitter-local shape registry plus the in-scope type-parameter set) as
/// `&self` fields,
/// and each leaf hook re-enters the driver through [`Self::convert`] to
/// recurse into its slots. The driver never sees that state and never
/// names a host construct, so a profile is free to choose any value
/// representation its host language expresses — a JS tagged object, a
/// Rust native enum, or a Swift native enum.
///
/// [`Package`]: crate::pass::resolve::Package
pub trait SkinProfile {
    /// The backend's emit-error type (each backend ships its own
    /// `EmitError`).
    type Err;

    /// Re-enter the shared boundary-wrapper driver. Leaf hooks call
    /// this to recurse into their slots; it is exactly [`convert`] with
    /// `self` as the profile, surfaced as a method so a hook can write
    /// `self.convert(slot, access, dir)`.
    fn convert(&self, ty: &Type<Routed>, expr: &str, dir: FfiDir) -> Result<String, Self::Err>
    where
        Self: Sized,
    {
        convert(self, ty, expr, dir)
    }

    /// `true` when a value of type `ty` crosses the FFI boundary
    /// unchanged — no per-leaf conversion is needed. The driver
    /// short-circuits to an identity conversion (returns `expr`
    /// verbatim) before dispatching.
    ///
    /// The complement is what the driver walks. The predicate is the
    /// profile's because what counts as passthrough is a property of the
    /// backend's value representation: JS passes atomics, type variables,
    /// function values whose legs need no conversion, unit, bottom,
    /// recursive newtypes, and comptime-opaque shapes; Rust additionally
    /// passes every newtype (it mints structs, never wraps) and any
    /// binary 2-slot compound whose slots are all atomic (its binary
    /// mint already *is* the FFI mint).
    fn is_passthrough(&self, ty: &Type<Routed>) -> bool;

    /// Convert a newtype-typed slot at the boundary.
    ///
    /// `Some(rendered)` when the profile gives a newtype its own boundary
    /// shape (JS wraps `{ <ffi_key>: payload }` on `Out` and unwraps on
    /// `In`); `None` when the profile carries the newtype transparently
    /// (Rust — the newtype's nominal struct *is* its boundary shape, so
    /// the driver falls through to identity). The hook recurses into the
    /// payload via [`Self::convert`].
    ///
    /// Reached only for a `Type::Path` that [`Self::is_passthrough`] did
    /// **not** rule out — i.e. a non-recursive, non-comptime newtype
    /// reference. A profile that never wraps returns `None` unconditionally.
    fn convert_newtype(
        &self,
        ty: &Type<Routed>,
        expr: &str,
        dir: FfiDir,
    ) -> Result<Option<String>, Self::Err>;

    /// Convert a product-typed value at the boundary.
    ///
    /// `slots` is the type's right-spine factors (from
    /// [`Type::right_spine_product`]); the hook keys them (via
    /// [`assign_spine_keys`]), reads / builds its backend's product
    /// carrier, and recurses into each slot through [`Self::convert`].
    fn convert_product(
        &self,
        ty: &Type<Routed>,
        slots: &[&Type<Routed>],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, Self::Err>;

    /// Convert a sum-typed value at the boundary.
    ///
    /// `slots` is the type's right-spine arms (from
    /// [`Type::right_spine_sum`]). The hook owns the *sum carrier*: a JS
    /// tagged object, a Rust native enum, or a Swift native enum. It recurses
    /// into the inhabited arm through [`Self::convert`].
    fn convert_sum(
        &self,
        ty: &Type<Routed>,
        slots: &[&Type<Routed>],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, Self::Err>;

    /// Convert a function value whose legs differ in representation
    /// across the boundary.
    ///
    /// The function value itself crosses verbatim, but a compound
    /// parameter or return type has a host-facing shape on one side and
    /// an internal shape on the other. The hook builds the bridging
    /// wrapper, converting each parameter leg in the flipped direction
    /// ([`FfiDir::flip`]) and the return leg in `dir`, recursing via
    /// [`Self::convert`]. `param` / `ret` / `abi_arity` are the
    /// function type's components; the parameter spine is recovered with
    /// [`Type::right_spine_take`].
    fn convert_function(
        &self,
        param: &Type<Routed>,
        ret: &Type<Routed>,
        abi_arity: usize,
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, Self::Err>;

    /// Cast an already-`Out`-converted `expr` to the host-facing boundary
    /// type of `ty`, or `None` when no cast is needed.
    ///
    /// An erased-static profile reaches a struct field / sum payload /
    /// function leg through the universal body carrier. A slot whose boundary
    /// type is *concrete* must be asserted to it (for example, Swift
    /// `expr as! H.main__I32`). A slot whose boundary type
    /// is itself the universal needs no cast (`None`). This is the one leaf
    /// the shared [`Self::convert_out_into_typed`] default cannot supply: it
    /// is the spelling of the host's downcast and the name of its universal
    /// carrier, both per-backend.
    ///
    /// The default returns `None` — a host with no erased body (JS dynamic,
    /// Rust nominal) never down-asserts an erased access, so it never calls
    /// [`Self::convert_out_into_typed`] and inherits the no-op.
    fn cast_to_boundary(
        &self,
        _expr: &str,
        _ty: &Type<Routed>,
    ) -> Result<Option<String>, Self::Err> {
        Ok(None)
    }

    /// Assert an erased newtype payload access to the payload's exact host
    /// spelling. Most payloads share [`Self::cast_to_boundary`]; an
    /// outer-`forall` function is the exception because its newtype-member
    /// public boundary canonicalizes the semantic product domain rather than
    /// exposing the function node's stored `abi_arity` partition.
    fn cast_newtype_payload_to_boundary(
        &self,
        expr: &str,
        payload: &Type<Routed>,
    ) -> Result<Option<String>, Self::Err> {
        self.cast_to_boundary(expr, payload)
    }

    /// Convert `access` `Out` and, when the slot lands as a **concrete**
    /// host type at the boundary (not the erased universal carrier), cast
    /// the converted value to it via [`Self::cast_to_boundary`].
    ///
    /// The shared shape of the erased-static profile's typed-slot recovery:
    /// an internal `[]any` access feeding a typed struct field / sum
    /// payload / function leg yields the universal carrier, so a passthrough
    /// atom's `convert` returns it unchanged but the typed boundary slot
    /// needs its concrete type. A compound slot's `convert` already yields
    /// the concrete struct / enum, so the cast is suppressed there
    /// ([`Self::is_passthrough`] is false). The per-backend cast spelling and
    /// universal-carrier name live in [`Self::cast_to_boundary`].
    fn convert_out_into_typed(&self, ty: &Type<Routed>, access: &str) -> Result<String, Self::Err>
    where
        Self: Sized,
    {
        let conv = self.convert(ty, access, FfiDir::Out)?;
        if self.is_passthrough(ty)
            && let Some(cast) = self.cast_to_boundary(&conv, ty)?
        {
            return Ok(cast);
        }
        Ok(conv)
    }

    /// Bridge a **polymorphic-function newtype payload** — a dictionary
    /// shape (`Functor[*F]` / `Monad[*F]`), a newtype over `[..](P) -> R` —
    /// across a boundary whose canonical semantic slots may differ from the
    /// stored callable partition recorded by `Type::Function::abi_arity`.
    ///
    /// The semantic callable is constructed and applied with the surface
    /// slots (`mk_functor(.(step, m) {…})` / `fmap(d)(step, m)`), while
    /// recovery may retain a closure with a coarser ABI partition. The
    /// boundary adapter exposes the canonical slots and repartitions them
    /// before the stored callable is invoked. Per `specs/backends/README.md`
    /// § Function-type FFI canonicalization the boundary walks the
    /// right-spine of the domain.
    ///
    /// Each backend's payload bridge differs (Swift uses the erased-static
    /// adapter; JS spells the same plan dynamically), so this
    /// is a per-backend leaf. The default is unreachable — it fires only
    /// through the shared [`Self::convert_newtype_payload`], which a backend
    /// that handles polymorphic payloads overrides and a backend that does
    /// not never calls.
    fn convert_poly_fn_payload(
        &self,
        _param: &Type<Routed>,
        _ret: &Type<Routed>,
        _plan: &FunctionBoundaryAdapterPlan<'_>,
        _type_stages: usize,
        _expr: &str,
        _dir: FfiDir,
    ) -> Result<String, Self::Err> {
        unreachable!(
            "convert_poly_fn_payload reached on a target that does not bridge \
             polymorphic newtype payloads — the default convert_newtype_payload \
             only reaches it for targets that override this leaf"
        )
    }

    /// Convert a **newtype payload** across the FFI boundary.
    ///
    /// Identical to [`Self::convert`] except for a polymorphic-function
    /// payload (a dictionary shape), which routes through
    /// [`Self::convert_poly_fn_payload`] with the shared boundary/ABI adapter
    /// plan. The peel-foralls / detect-function / partition scaffold is
    /// shared; the function-case bridge is the per-backend leaf. A
    /// non-function payload (the common case) converts directly.
    fn convert_newtype_payload(
        &self,
        payload: &Type<Routed>,
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, Self::Err>
    where
        Self: Sized,
    {
        // The dictionary payload is `Forall(binders, Function(P, R))`; peel
        // the rank-N binders to reach the function (the erased body carries
        // no runtime shape for them).
        let polymorphic = matches!(payload, Type::Forall { .. });
        let mut type_stages = 0;
        let mut inner = payload;
        while let Type::Forall { body, .. } = inner {
            type_stages += 1;
            inner = body;
        }
        if polymorphic
            && let Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } = inner
            && !self.is_passthrough(payload)
        {
            let plan = FunctionBoundaryAdapterPlan::new(param, *abi_arity);
            return self.convert_poly_fn_payload(param, ret, &plan, type_stages, expr, dir);
        }
        self.convert(payload, expr, dir)
    }

    /// Convert a newtype payload `Out` and recover a concrete boundary leaf
    /// type when the erased-static body carries it as the universal value.
    /// This is the newtype-payload analogue of
    /// [`Self::convert_out_into_typed`]: its function case preserves the
    /// outer-`forall` boundary partition before applying the ordinary leaf
    /// cast.
    fn convert_newtype_payload_out_into_typed(
        &self,
        payload: &Type<Routed>,
        access: &str,
    ) -> Result<String, Self::Err>
    where
        Self: Sized,
    {
        let conv = self.convert_newtype_payload(payload, access, FfiDir::Out)?;
        if self.is_passthrough(payload)
            && let Some(cast) = self.cast_newtype_payload_to_boundary(&conv, payload)?
        {
            return Ok(cast);
        }
        Ok(conv)
    }
}

/// Walk `ty` over its right spine and emit a host-language expression that
/// converts `expr` between the host-facing skin shape and the package's
/// internal representation, in direction `dir`.
///
/// This is the shared *shape* of a profile-backed per-signature boundary
/// wrapper. It is purely structural: it short-circuits passthrough
/// types to identity, then dispatches on the type constructor and hands
/// each leaf to `profile`. The profile's hooks own the backend's value
/// representation and recurse back through this function, so callers share
/// the walk while the rendered output is entirely the profile's.
///
/// - **Passthrough** ([`SkinProfile::is_passthrough`] true) → `expr`
///   unchanged.
/// - **Newtype** (`Type::Path`) → [`SkinProfile::convert_newtype`];
///   `None` falls through to identity (the profile carries the newtype
///   transparently).
/// - **Product** / **Sum** → [`SkinProfile::convert_product`] /
///   [`SkinProfile::convert_sum`], passing the right-spine slots.
/// - **Function** → [`SkinProfile::convert_function`].
/// - **Forall** → erase the binders and recurse on the body (the host
///   supplies no type arguments).
///
/// A profile may override [`SkinProfile::convert`] for an atomic representation
/// change before entering this driver. `Type::Unit` / `Type::Bottom` that reach
/// the dispatch fall through to identity; `Type::Infer` / `Type::LabelSugar`
/// are uninhabited at `Routed`.
pub fn convert<P: SkinProfile>(
    profile: &P,
    ty: &Type<Routed>,
    expr: &str,
    dir: FfiDir,
) -> Result<String, P::Err> {
    if profile.is_passthrough(ty) {
        return Ok(expr.to_owned());
    }
    match ty {
        Type::Path { .. } => match profile.convert_newtype(ty, expr, dir)? {
            Some(rendered) => Ok(rendered),
            None => Ok(expr.to_owned()),
        },
        Type::Product { .. } => {
            let slots = Type::right_spine_product(ty);
            profile.convert_product(ty, &slots, expr, dir)
        }
        Type::Sum { .. } => {
            let slots = Type::right_spine_sum(ty);
            profile.convert_sum(ty, &slots, expr, dir)
        }
        Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => profile.convert_function(param, ret, *abi_arity, expr, dir),
        Type::Forall { body, .. } => convert(profile, body, expr, dir),
        // Atomic leaves are either passthrough or handled by a profile's
        // `convert` override; Infer / LabelSugar are uninhabited at Routed.
        Type::Unit { .. } | Type::Bottom { .. } => Ok(expr.to_owned()),
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
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

pub(crate) fn erase_signature_ret(
    sig: &crate::ast::Signature<Routed>,
    ret: &Type<Routed>,
) -> Type<Routed> {
    let scope: BTreeSet<String> = sig
        .params
        .iter()
        .filter_map(|param| match param {
            crate::ast::SignatureParam::Type(param) => Some(param.name.clone()),
            crate::ast::SignatureParam::Value(_) => None,
        })
        .collect();
    erase_scoped_type_vars(ret, &scope)
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

/// Instantiate a newtype payload and make its nominal heads self-contained
/// for a package-global boundary walk. Unfilled declaration parameters stay
/// local, so a same-named module declaration cannot capture them.
pub(crate) fn qualify_boundary_newtype_payload(
    declaration: &Newtype<Routed>,
    nominal: &Type<Routed>,
    module_path: &str,
    package: &Package<Routed>,
) -> Type<Routed> {
    let payload = instantiate_boundary_newtype_payload(declaration, nominal);
    let Some(entry) = package.module(module_path) else {
        return payload;
    };
    let locals = declaration
        .type_params
        .iter()
        .chain(&declaration.existential_params)
        .map(|param| (param.name.clone(), param.effective_kind()))
        .collect();
    crate::pass::resolve::qualify_routed_contract_type_in_module(&payload, &entry.module, &locals)
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
    let Some(entry) = package.module(module_path) else {
        return Err(format!("missing declaring module `{module_path}`"));
    };
    let span = declaration.meta.span;
    let mut nominal_segments: Vec<crate::ast::PathSegment> = module_path
        .split('/')
        .map(|segment| crate::ast::PathSegment::synth(segment, span))
        .collect();
    nominal_segments.push(crate::ast::PathSegment::synth(
        declaration.name.clone(),
        span,
    ));
    let scheme = crate::pass::typecheck_core::newtype_member_scheme_in_module(
        declaration,
        member,
        &nominal_segments,
        &entry.module,
        Some(package),
        span,
    )
    .map_err(|error| format!("cannot synthesize `{module_path}.{member}`: {error:?}"))?;

    let mut scope = BTreeSet::new();
    let mut outer = scheme.ty.as_type();
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

/// Resolve a `Type::Path` reference to the newtype it names, plus the
/// declaring module's slash-path. `None` for anything else (type
/// variable, host type, unresolvable).
pub(crate) fn resolve_newtype<'p>(
    t: &Type<Routed>,
    package: &'p Package<Routed>,
) -> Option<(&'p Newtype<Routed>, String)> {
    let segments = match t {
        Type::Path { segments, .. } => segments,
        _ => return None,
    };
    if segments.is_empty() {
        return None;
    }
    if segments.len() == 1 {
        let name = segments[0].as_str();
        // Value-shaped single-segment paths cannot name newtypes.
        if crate::naming::is_value_name(name) {
            return None;
        }
        for (module_path_str, entry) in package.modules() {
            for item in &entry.module.items {
                let mut found = None;
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    if let Some(newtype) = declaration.newtype()
                        && newtype.name == name
                    {
                        found = Some(newtype);
                    }
                });
                if let Some(newtype) = found {
                    return Some((newtype, module_path_str.to_owned()));
                }
            }
        }
        return None;
    }
    let last = segments.last().unwrap();
    let module_path = segments[..segments.len() - 1].join("/");
    let entry = package.module(&module_path)?;
    for item in &entry.module.items {
        let mut found = None;
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype()
                && newtype.name == *last
            {
                found = Some(newtype);
            }
        });
        if let Some(newtype) = found {
            return Some((newtype, module_path));
        }
    }
    None
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

/// Instantiate a newtype payload for a typed host boundary without letting
/// declaration or nested binders become package-resolvable nominal leaves.
/// Filled universal parameters use the typer's capture-avoiding substitution;
/// unfilled universals, existentials, and nested `forall` binders erase to the
/// boundary's opaque carrier before any skin classification or conversion.
pub(crate) fn instantiate_boundary_newtype_payload(
    d: &Newtype<Routed>,
    reference: &Type<Routed>,
) -> Type<Routed> {
    let args: &[Type<Routed>] = match reference {
        Type::Path { args, .. } => args,
        _ => &[],
    };
    let filled = args.len().min(d.type_params.len());
    let declaration_scope: BTreeSet<String> = d
        .type_params
        .iter()
        .skip(filled)
        .chain(&d.existential_params)
        .map(|param| param.name.clone())
        .collect();

    // Erase declaration-local names before inserting actual arguments. This
    // preserves a nominal argument whose leaf happens to match an unfilled
    // binder. `subst_type` then alpha-renames nested foralls around replacement
    // names, and the final empty-scope walk erases binders introduced by an
    // argument itself.
    let payload = erase_scoped_type_vars(&d.payload, &declaration_scope);
    let subst = d
        .type_params
        .iter()
        .take(filled)
        .zip(args.iter())
        .map(|(param, arg)| (param.name.clone(), arg.clone()))
        .collect();
    let payload = crate::pass::typecheck_core::subst_type(&payload, &subst);
    erase_scoped_type_vars(&payload, &BTreeSet::new())
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
    use crate::ast::{Meta, PathSegment};
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
    fn function_boundary_plan_separates_semantic_and_stored_partitions() {
        let param = product(path("A"), product(path("B"), path("C")));
        let plan = FunctionBoundaryAdapterPlan::new(&param, 1);
        assert_eq!(plan.boundary_arity(), 3);
        assert_eq!(plan.internal_arity(), 1);

        let boundary = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(
            plan.boundary_to_internal_args(&boundary, |left, right| {
                format!("pair({left},{right})")
            }),
            vec!["pair(a,pair(b,c))"]
        );

        let internal = vec!["packed".to_owned()];
        assert_eq!(
            plan.internal_to_boundary_args(
                &internal,
                |left, right| format!("pair({left},{right})"),
                |value, index, arity| format!("slot({value},{index},{arity})"),
            ),
            vec!["slot(packed,0,3)", "slot(packed,1,3)", "slot(packed,2,3)",]
        );
    }

    #[test]
    fn function_boundary_plan_preserves_raw_zero_arity() {
        let unit = Type::Unit { meta: meta() };
        let raw_zero = FunctionBoundaryAdapterPlan::new(&unit, 0);
        assert_eq!(raw_zero.boundary_arity(), 0);
        assert_eq!(raw_zero.internal_arity(), 0);

        let substituted_unit_slot = FunctionBoundaryAdapterPlan::new(&unit, 1);
        assert_eq!(substituted_unit_slot.boundary_slots(), vec![&unit]);
        assert_eq!(substituted_unit_slot.internal_slots(), vec![&unit]);
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
        crate::pass::recover_to_low::lower(&enriched)
    }

    #[cfg(feature = "surface")]
    fn rendered_swift_shapes(package: &Package<Routed>) -> String {
        crate::backends::swift::emit::lower_package(package, "Scopecheck")
            .expect("prepare Swift facade shapes")
            .shapes_swift
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

        assert_eq!(
            rendered_swift_shapes(&base),
            rendered_swift_shapes(&with_unbridged),
        );
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

        assert_eq!(
            rendered_swift_shapes(&base),
            rendered_swift_shapes(&with_same_leaf),
        );
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

        assert_eq!(
            rendered_swift_shapes(&base),
            rendered_swift_shapes(&with_same_leaf),
        );
    }
    /// A profile that records every hook the driver invokes, so the
    /// tests can pin the driver's dispatch (which arm, which slots,
    /// which direction) without committing to any backend's value
    /// representation. The leaf hooks recurse through the driver and
    /// render a compact symbolic trace rather than real source.
    ///
    /// `passthrough` decides which `Path` names short-circuit; `wraps`
    /// decides which non-passthrough `Path` names the newtype hook
    /// wraps (vs. returns `None` to fall through to identity).
    struct TraceProfile {
        passthrough: fn(&str) -> bool,
        wraps: bool,
    }

    impl TraceProfile {
        fn path_name(ty: &Type<Routed>) -> String {
            match ty {
                Type::Path { segments, .. } => segments
                    .last()
                    .map(|s| s.as_str().to_owned())
                    .unwrap_or_default(),
                Type::Unit { .. } => "()".to_owned(),
                _ => "?".to_owned(),
            }
        }
    }

    impl SkinProfile for TraceProfile {
        type Err = String;

        fn is_passthrough(&self, ty: &Type<Routed>) -> bool {
            match ty {
                Type::Unit { .. } | Type::Bottom { .. } => true,
                Type::Path { segments, .. } => segments
                    .last()
                    .is_some_and(|s| (self.passthrough)(s.as_str())),
                _ => false,
            }
        }

        fn convert_newtype(
            &self,
            ty: &Type<Routed>,
            expr: &str,
            dir: FfiDir,
        ) -> Result<Option<String>, String> {
            if !self.wraps {
                return Ok(None);
            }
            let name = Self::path_name(ty);
            Ok(Some(format!("nt[{name},{dir:?}]({expr})")))
        }

        fn convert_product(
            &self,
            _ty: &Type<Routed>,
            slots: &[&Type<Routed>],
            expr: &str,
            dir: FfiDir,
        ) -> Result<String, String> {
            let parts: Result<Vec<String>, String> =
                slots.iter().map(|s| convert(self, s, expr, dir)).collect();
            Ok(format!("prod[{dir:?}]({})", parts?.join(",")))
        }

        fn convert_sum(
            &self,
            _ty: &Type<Routed>,
            slots: &[&Type<Routed>],
            expr: &str,
            dir: FfiDir,
        ) -> Result<String, String> {
            let parts: Result<Vec<String>, String> =
                slots.iter().map(|s| convert(self, s, expr, dir)).collect();
            Ok(format!("sum[{dir:?}]({})", parts?.join(",")))
        }

        fn convert_function(
            &self,
            param: &Type<Routed>,
            ret: &Type<Routed>,
            abi_arity: usize,
            expr: &str,
            dir: FfiDir,
        ) -> Result<String, String> {
            let param_slots = Type::right_spine_take(param, abi_arity);
            let params: Result<Vec<String>, String> = param_slots
                .iter()
                .map(|p| convert(self, p, "p", dir.flip()))
                .collect();
            let ret_conv = convert(self, ret, "r", dir)?;
            Ok(format!(
                "fn[{dir:?}](({});{ret_conv})({expr})",
                params?.join(",")
            ))
        }
    }

    fn never_passthrough(_: &str) -> bool {
        false
    }

    fn passthrough_lowercase(name: &str) -> bool {
        name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
    }

    #[test]
    fn driver_short_circuits_passthrough_to_identity() {
        let p = TraceProfile {
            passthrough: passthrough_lowercase,
            wraps: true,
        };
        // A lowercase path is passthrough → identity, no hook fires.
        let out = convert(&p, &path("i32"), "x", FfiDir::Out).unwrap();
        assert_eq!(out, "x");
        // Unit is passthrough by predicate.
        assert_eq!(
            convert(&p, &Type::Unit { meta: meta() }, "u", FfiDir::In).unwrap(),
            "u"
        );
    }

    #[test]
    fn driver_routes_newtype_wrap_and_passthrough_policies() {
        // A profile that wraps newtypes (JS-shaped).
        let wrapping = TraceProfile {
            passthrough: passthrough_lowercase,
            wraps: true,
        };
        assert_eq!(
            convert(&wrapping, &path("Meters"), "v", FfiDir::Out).unwrap(),
            "nt[Meters,Out](v)"
        );
        // A profile that carries newtypes transparently (Rust-shaped):
        // the `None` return falls the driver through to identity.
        let transparent = TraceProfile {
            passthrough: passthrough_lowercase,
            wraps: false,
        };
        assert_eq!(
            convert(&transparent, &path("Meters"), "v", FfiDir::Out).unwrap(),
            "v"
        );
    }

    #[test]
    fn driver_walks_product_right_spine_into_slots() {
        let p = TraceProfile {
            passthrough: never_passthrough,
            wraps: true,
        };
        // (A & B & C) flattens to a 3-slot product; each slot recurses
        // through the newtype hook in the same direction.
        let ty = product(path("A"), product(path("B"), path("C")));
        let out = convert(&p, &ty, "v", FfiDir::Out).unwrap();
        assert_eq!(out, "prod[Out](nt[A,Out](v),nt[B,Out](v),nt[C,Out](v))");
    }

    #[test]
    fn driver_walks_sum_right_spine_into_arms() {
        let p = TraceProfile {
            passthrough: never_passthrough,
            wraps: true,
        };
        let ty = sum(path("A"), sum(path("B"), path("C")));
        let out = convert(&p, &ty, "v", FfiDir::In).unwrap();
        assert_eq!(out, "sum[In](nt[A,In](v),nt[B,In](v),nt[C,In](v))");
    }

    #[test]
    fn driver_flips_direction_across_function_param_leg() {
        let p = TraceProfile {
            passthrough: never_passthrough,
            wraps: true,
        };
        // (A & B) -> C, abi_arity 2: params convert in the flipped
        // direction (Out value → In params), return in the original.
        let ty = function(product(path("A"), path("B")), path("C"), 2);
        let out = convert(&p, &ty, "f", FfiDir::Out).unwrap();
        assert_eq!(out, "fn[Out]((nt[A,In](p),nt[B,In](p));nt[C,Out](r))(f)");
    }

    #[test]
    fn driver_erases_forall_binders_and_recurses_body() {
        let p = TraceProfile {
            passthrough: never_passthrough,
            wraps: true,
        };
        let body = product(path("A"), path("B"));
        let ty = Type::Forall {
            param: crate::ast::TypeParam {
                name: "T".to_owned(),
                span: Span::new(0, 0),
                kind: None,
            },
            body: Box::new(body),
            meta: meta(),
        };
        let out = convert(&p, &ty, "v", FfiDir::Out).unwrap();
        assert_eq!(out, "prod[Out](nt[A,Out](v),nt[B,Out](v))");
    }

    #[test]
    fn scoped_type_var_erasure_enters_nested_forall_without_touching_nominals() {
        let nested = Type::Forall {
            param: crate::ast::TypeParam {
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
        let (capture_decl, _) = resolve_newtype(capture_ty, &package).expect("Capture declaration");
        assert_eq!(
            canonical_type_string(&instantiate_boundary_newtype_payload(
                capture_decl,
                capture_ty
            )),
            "forall T. fn1(main.T) -> $"
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
                "main__nested(_ arg0: KioForall_main__HostFn_unested_U3<Self>) -> Self.main__Str"
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
                .contains("main__capture(_ arg0: KioNewtype_main__Capture<Self, KioNewtype_main__T<Self>>) -> KioNewtype_main__Capture<Self, KioNewtype_main__T<Self>>"),
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
                "host__main__packed :: (KioExistential_H6d61696e005061636b__main_Pack h m) -> m (KioExistential_H6d61696e005061636b__main_Pack h m)"
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
                "host__main__capture :: (KioCarrier_H6d61696e0043617074757265__main_Capture h m (HostType__main__Str h)) -> m (KioCarrier_H6d61696e0043617074757265__main_Capture h m (HostType__main__Str h))"
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

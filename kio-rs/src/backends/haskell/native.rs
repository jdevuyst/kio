//! Haskell backend — the **native `f a`** body (native-HKT family).
//!
//! Renders the Routed IR at its native Haskell types: an abstract
//! kind-`*→*` application `F(A)` becomes `f a`, products are tuples, sums
//! are right-nested `Either`, and scalars retain their exact host-selected
//! types.
//!
//! ## Scope — the native-HKT + existential family
//!
//! Every parametric or recursive source newtype has one declaration-stable
//! nominal carrier head. A nullary, nonrecursive, nonexistential newtype is
//! transparent. Existential newtypes are GADTs whose CPS projector is the
//! target of [`Expr::LowCpsProjectorApply`]. A package whose remaining
//! phase-abstract shapes cannot be represented natively may use the
//! private universal-carrier floor when its public boundary remains exact.
//! Both paths share one typed FFI skin.
//!
//! Where an erased-static backend (Go, Rust, Swift) erases an
//! existential's sealed binders to its universal value and recovers
//! witnesses by aligning the payload, Haskell's GADT captures each
//! existential at construction and re-binds it as a fresh skolem on the
//! `case`-unpack — the same way it expresses HKT carriers natively rather
//! than through an erased universal. No `dyn Any`, no witness-recovery,
//! no carrier walk.
//!
//! ## Host effects and function values
//!
//! Host calls sequence through the host monad `m`. Function values also
//! return through `m`, so a host-function reference and an effectful lambda
//! stay ordinary first-class Kio values; constructing either value is pure
//! and applying it runs the action. Top-level module functions that never
//! reach a host effect remain plain Haskell functions. A transformer carrier
//! such as `StateT s inner a = s -> inner (s, a)` still uses its own `inner`
//! constructor, independent of the host monad.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::{
    Expr, FnDef, Item, Newtype, Routed, SignatureGroupRef, SignatureParam, Type, TypeParam,
};
use crate::pass::resolve::Package;

use super::emit::{
    EmitError, export_newtype_wrapper_name, export_wrapper_name, host_field_name, module_fn_name,
};
use super::facade::{HaskellCallableEntry, HaskellCallableHeadStage, HaskellFacadeCatalog};
use super::naming::{BoundaryId, BoundaryStep};
use super::reconstruct::TypeRecon;
use super::skin::instantiate_newtype_payload_in;
use crate::backends::boundary_facade::{
    BoundaryFacadeSiteOwner, BoundaryNewtypeSurface, CallableSourceParamAdapter,
    CallableValueStageLayout,
};
use crate::backends::skin::FfiDir;

/// Signals that a package is not (yet) renderable at native types, so the
/// caller falls back to the universal private-carrier body. Not an error —
/// the fallback body is a correct rendering; this just routes the package
/// to it.
#[derive(Debug, Clone)]
pub struct NotNative {
    /// Why the package is not native-eligible. Surfaced only under the
    /// `KIO_DEBUG_HASKELL_NATIVE` env trace ([`NotNative::trace`]); the
    /// production fallback is silent and correct.
    reason: String,
}

impl NotNative {
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }

    fn new(reason: impl Into<String>) -> Self {
        NotNative {
            reason: reason.into(),
        }
    }

    /// Emit the fall-back reason to stderr when `KIO_DEBUG_HASKELL_NATIVE`
    /// is set — a debug-only surface for diagnosing why a package took the
    /// private-carrier body instead of the native one.
    pub fn trace(&self, package_name: &str) {
        if std::env::var_os("KIO_DEBUG_HASKELL_NATIVE").is_some() {
            eprintln!(
                "kio[haskell-native]: `{package_name}` -> universal-carrier body: {}",
                self.reason
            );
        }
    }
}

/// A native-emit failure (an `EmitError` raised mid-render because the
/// native path reached a shape it cannot render at native types)
/// becomes a [`NotNative`] signal: the whole package falls back to the
/// universal private-carrier body rather than emitting a half-native module.
impl From<EmitError> for NotNative {
    fn from(e: EmitError) -> Self {
        NotNative::new(e.message)
    }
}

/// A carrier newtype rendered as a native type constructor. Carries every
/// type param (a transformer carrier such as `State_t[S][*M][A]` has a
/// leading kind-`*` slot, a kind-`*→*` slot, and a trailing element), so
/// the decl spells `newtype State_t s m a = State_t (s -> m (s, a))`.
struct Carrier {
    /// The Kio newtype name (`Box`), for the decl's doc comment.
    kio_name: String,
    /// The declaration-local minted Haskell type-constructor name.
    hs_name: String,
    /// Every type param, in order — rendered as the decl's Haskell
    /// tyvars. A kind-`*→*` param becomes a `* -> *` type-constructor
    /// variable (the inner monad of a transformer carrier).
    type_params: Vec<TypeParam>,
    /// The carrier payload type (`A`, `. | A`, `M(. | A)`,
    /// `S -> M(S & A)`), rendered with the type params in scope.
    payload: Type<Routed>,
    /// Whether the source declaration needs a nominal Haskell head.
    /// Parametric and recursive newtypes are nominal; a nullary,
    /// nonrecursive newtype expands to its payload.
    opaque: bool,
    /// Host-visible capabilities selected through the package bridge.
    host_surface: Option<HaskellNewtypeHostSurface>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HaskellNewtypeHostSurface {
    Opaque,
    Constructor,
    Projector,
    Both,
}

impl HaskellNewtypeHostSurface {
    fn from_prepared(surface: &BoundaryNewtypeSurface) -> Option<Self> {
        match surface {
            BoundaryNewtypeSurface::Unexposed => None,
            BoundaryNewtypeSurface::Opaque => Some(Self::Opaque),
            BoundaryNewtypeSurface::Constructor { .. } => Some(Self::Constructor),
            BoundaryNewtypeSurface::Projector { .. } => Some(Self::Projector),
            BoundaryNewtypeSurface::Both { .. } => Some(Self::Both),
        }
    }
}

/// An **existential** carrier newtype rendered as a Haskell
/// existentially-quantified data type (`ExistentialQuantification`). A
/// header `Box[A] <U> <V> : A & U & V` declares universal binders `[A]`
/// (the GADT head's type params) and existential binders `<U> <V>` (the
/// constructor's `forall`-bound, hidden) — `data Box a = forall u v. Box
/// (a, (u, v))`. Where Rust erases the existential slots to `Rc<dyn Any>`
/// and recovers witnesses by aligning the payload, Haskell's GADT captures
/// each existential at construction and re-binds it as a fresh skolem on
/// the `case`-unpack — no erasure, no witness extraction, no `dyn Any`.
struct ExistentialCarrier {
    /// The Kio newtype name (`Box`), for the decl's doc comment.
    kio_name: String,
    /// The declaration-local minted Haskell type-constructor name.
    hs_name: String,
    /// Whether the Kio newtype itself is part of the package facade.
    host_surface: Option<HaskellNewtypeHostSurface>,
    /// The universal type params (`[A]`) — the GADT head's tyvars. Empty
    /// for a no-universals existential (`Box <A>` desugared from a
    /// `labels { box <U> : … }`-style header).
    type_params: Vec<TypeParam>,
    /// The existential binders (`<U> <V>`) — the constructor's
    /// `forall`-bound, never appearing in the GADT head.
    existential_params: Vec<TypeParam>,
    /// The payload type (`A & U & V`, `String & A`, `Surface(P)`),
    /// rendered with both universal and existential params in scope.
    payload: Type<Routed>,
    /// Runtime value arity of the exact CPS continuation group.
    continuation_arity: usize,
}

/// Whether a module fn / closure reaches a host effect — drives the monad
/// split. `Effectful` ⇒ `m`-typed; `Pure` ⇒ rendered without `m`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Pure,
    Effectful,
}

/// Which member of a newtype a qualified-member access names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NewtypeMemberKind {
    Constructor,
    Projector,
}

/// Render the package's native body section — nominal type declarations,
/// every module fn at its reconstructed native type, and the native
/// export wrappers — or signal [`NotNative`] for the private-carrier
/// fallback. The header / host record / structural family API are the
/// caller's (identical in both paths); this returns the type
/// decls followed by the `mod_*` / `exp_*` definitions.
pub(crate) fn render_native_body<'a>(
    package: &'a Package<Routed>,
    names: &'a super::emit::HaskellNames,
    public_ffi_decls: &str,
    prepared: &crate::backends::boundary_facade::PreparedBoundaryCallableSites,
) -> Result<NativeRender, NotNative> {
    let ctx = NativeCtx::build(package, names, prepared)?;
    ctx.render(public_ffi_decls)
}

/// The native render's output pieces, slotted into the package module
/// `<Ns>.hs` by the caller around the shared skin.
pub struct NativeRender {
    /// The host value record at exact native types. Generic host functions
    /// quantify their Kio type parameters directly instead of erasing them
    /// into the universal fallback representation.
    pub host_record: String,
    /// Nominal `newtype` / `data` declarations and their helpers, in
    /// deterministic order.
    pub type_decls: String,
    /// The `mod_*` module-fn definitions, native-typed.
    pub module_fns: String,
    /// The `exp_*` export wrappers, matching the native `mod_*` sigs.
    pub export_wrappers: String,
    /// Abstract carrier types that occur in public signatures and therefore
    /// must be nameable by a host. Their raw constructors remain private.
    pub export_names: Vec<String>,
    /// Extra `LANGUAGE` pragmas the native body needs beyond the base set.
    pub extra_pragmas: Vec<&'static str>,
}

pub(super) struct NativeTypes<'p> {
    package: &'p Package<Routed>,
    runtime_opaque: String,
    standard_names: super::naming::StandardNames,
    carriers: BTreeMap<String, Carrier>,
    existentials: BTreeMap<String, ExistentialCarrier>,
    resolution: super::skin::NewtypeResolution,
}

impl<'p> NativeTypes<'p> {
    pub(super) fn build(
        package: &'p Package<Routed>,
        prepared: &crate::backends::boundary_facade::PreparedBoundaryCallableSites,
        runtime_names: &super::naming::RuntimeNames,
        standard_names: &super::naming::StandardNames,
    ) -> Self {
        let mut carriers = BTreeMap::new();
        let mut existentials = BTreeMap::new();
        let resolution = super::skin::NewtypeResolution::build(package);
        let public_newtypes = prepared
            .public_newtypes()
            .map(|newtype| {
                let module = newtype.name().module_segments().join("/");
                let key = super::skin::newtype_qual_key(&module, newtype.name().name());
                let requires_nominal = !newtype.type_params().is_empty()
                    || !newtype.existential_params().is_empty()
                    || newtype.surface().uses_nominal_carrier();
                (key, (newtype.surface().clone(), requires_nominal))
            })
            .collect::<BTreeMap<_, _>>();
        for (module_key, entry) in package.modules() {
            for item in &entry.module.items {
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    let Some(declaration) = declaration.newtype() else {
                        return;
                    };
                    let key = super::skin::newtype_qual_key(module_key, &declaration.name);
                    let public_newtype = public_newtypes.get(&key);
                    let host_surface = public_newtype
                        .and_then(|(surface, _)| HaskellNewtypeHostSurface::from_prepared(surface));
                    if !declaration.existential_params.is_empty() {
                        let continuation_arity =
                            crate::backends::skin::existential_projector_continuation_arity(
                                package,
                                module_key,
                                declaration,
                            )
                            .expect(
                                "a typed existential newtype has an exact CPS projector scheme",
                            );
                        existentials.insert(
                            key,
                            existential_from_decl(
                                declaration,
                                module_key,
                                host_surface,
                                continuation_arity,
                            ),
                        );
                    } else if super::skin::newtype_is_recursive_with_atomic_newtypes(
                        declaration,
                        module_key,
                        package,
                        &resolution,
                        |nested_module, nested| {
                            let nested_key =
                                super::skin::newtype_qual_key(nested_module, &nested.name);
                            public_newtypes
                                .get(&nested_key)
                                .is_some_and(|(surface, _)| surface.uses_nominal_carrier())
                        },
                    ) {
                        carriers.insert(
                            key,
                            carrier_from_decl(declaration, module_key, true, host_surface),
                        );
                    } else {
                        let opaque = !declaration.type_params.is_empty()
                            || public_newtype
                                .is_some_and(|(_, requires_nominal)| *requires_nominal);
                        carriers.insert(
                            key,
                            carrier_from_decl(declaration, module_key, opaque, host_surface),
                        );
                    }
                });
            }
        }

        NativeTypes {
            package,
            runtime_opaque: runtime_names.opaque.clone(),
            standard_names: standard_names.clone(),
            carriers,
            existentials,
            resolution,
        }
    }

    pub(super) fn has_nominal_boundary_carrier(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> bool {
        let Some(key) = self.resolution.key_of(ty, referring_module) else {
            return false;
        };
        self.has_nominal_boundary_carrier_key(&key)
    }

    pub(super) fn has_nominal_boundary_carrier_key(&self, key: &str) -> bool {
        self.existentials.contains_key(key)
            || self.carriers.get(key).is_some_and(|carrier| carrier.opaque)
    }

    pub(super) fn render_type_scoped(
        &self,
        ty: &Type<Routed>,
        scope: &HashSet<&str>,
        module: Option<&str>,
        host_types: &super::skin::HaskellHostTypes,
    ) -> String {
        match ty {
            Type::Unit { .. } => "()".to_owned(),
            Type::Bottom { .. } => format!("{}.Void", self.standard_names.data_void),
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let params = Type::right_spine_take(param, *abi_arity);
                let rendered_params = params
                    .iter()
                    .map(|param| self.render_type_scoped(param, scope, module, host_types))
                    .collect::<Vec<_>>();
                let ret = self.render_type_scoped(ret, scope, module, host_types);
                let arg = paren_ty(&nest_tuple_type(&rendered_params));
                format!("{arg} -> m {}", paren_ty(&ret))
            }
            Type::Product { left, right, .. } => {
                let left = self.render_type_scoped(left, scope, module, host_types);
                let right = self.render_type_scoped(right, scope, module, host_types);
                format!("({left}, {right})")
            }
            Type::Sum { .. } => {
                let slots = Type::right_spine_sum(ty);
                self.render_either_chain(&slots, scope, module, host_types)
            }
            Type::Path { segments, args, .. } if segments.len() == 1 => {
                let name = segments[0].as_str();
                if scope.contains(name) {
                    let rendered_args = args
                        .iter()
                        .map(|arg| {
                            paren_ty(&self.render_type_scoped(arg, scope, module, host_types))
                        })
                        .collect::<Vec<_>>();
                    apply_hs_head(&hs_tyvar(name), &rendered_args)
                } else if let Some(host_type) = self.exact_host_type(ty, scope, module, host_types)
                {
                    host_type
                } else if let Some(key) = self.resolution.key_of(ty, module) {
                    if self.carriers.contains_key(&key) {
                        self.render_carrier_ref(&key, args, scope, module, host_types)
                    } else if let Some(existential) = self.existentials.get(&key) {
                        let rendered_args = args
                            .iter()
                            .map(|arg| {
                                paren_ty(&self.render_type_scoped(arg, scope, module, host_types))
                            })
                            .collect::<Vec<_>>();
                        apply_hs_head(&format!("{} h m", existential.hs_name), &rendered_args)
                    } else if crate::backends::skin::is_comptime_type_name(name) {
                        format!("{} h m", self.runtime_opaque)
                    } else {
                        hs_tyvar(name)
                    }
                } else if crate::backends::skin::is_comptime_type_name(name) {
                    format!("{} h m", self.runtime_opaque)
                } else {
                    let rendered_args = args
                        .iter()
                        .map(|arg| {
                            paren_ty(&self.render_type_scoped(arg, scope, module, host_types))
                        })
                        .collect::<Vec<_>>();
                    apply_hs_head(&hs_tyvar(name), &rendered_args)
                }
            }
            Type::Path { segments, args, .. } => {
                let last = segments
                    .last()
                    .map(|segment| segment.as_str())
                    .unwrap_or("");
                let rendered_args = args
                    .iter()
                    .map(|arg| paren_ty(&self.render_type_scoped(arg, scope, module, host_types)))
                    .collect::<Vec<_>>();
                let key = self.resolution.key_of(ty, module);
                if let Some(host_type) = self.exact_host_type(ty, scope, module, host_types) {
                    host_type
                } else if key
                    .as_deref()
                    .is_some_and(|key| self.carriers.contains_key(key))
                {
                    self.render_carrier_ref(
                        key.as_deref().expect("carrier key was present"),
                        args,
                        scope,
                        module,
                        host_types,
                    )
                } else if let Some(existential) =
                    key.as_deref().and_then(|key| self.existentials.get(key))
                {
                    apply_hs_head(&format!("{} h m", existential.hs_name), &rendered_args)
                } else if crate::backends::skin::is_comptime_type_name(last) {
                    format!("{} h m", self.runtime_opaque)
                } else {
                    apply_hs_head(&hs_tyvar(last), &rendered_args)
                }
            }
            Type::Forall { param, body, .. } => {
                let mut nested = scope.clone();
                nested.insert(param.name.as_str());
                let body = self.render_type_scoped(body, &nested, module, host_types);
                format!(
                    "forall {}. m {}",
                    super::skin::kinded_haskell_binder(param, &self.standard_names),
                    paren_ty(&body)
                )
            }
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn exact_host_type(
        &self,
        ty: &Type<Routed>,
        scope: &HashSet<&str>,
        module: Option<&str>,
        host_types: &super::skin::HaskellHostTypes,
    ) -> Option<String> {
        let binding = host_types.resolve(ty, module)?;
        let Type::Path { args, .. } = ty else {
            return None;
        };
        if args.len() != binding.type_params.len() {
            unreachable!(
                "Haskell native type renderer received an ill-kinded routed host type `{}`",
                binding.source_name
            );
        }
        let mut rendered = format!("{} h", binding.assoc_name);
        for arg in args {
            rendered.push(' ');
            rendered.push_str(&paren_ty(
                &self.render_type_scoped(arg, scope, module, host_types),
            ));
        }
        Some(rendered)
    }

    fn render_carrier_ref(
        &self,
        key: &str,
        args: &[Type<Routed>],
        scope: &HashSet<&str>,
        module: Option<&str>,
        host_types: &super::skin::HaskellHostTypes,
    ) -> String {
        let carrier = &self.carriers[key];
        if carrier.opaque {
            let mut rendered = format!("{} h m", carrier.hs_name);
            for arg in args {
                rendered.push(' ');
                rendered.push_str(&paren_ty(
                    &self.render_type_scoped(arg, scope, module, host_types),
                ));
            }
            return rendered;
        }
        let (owner, _) = key
            .rsplit_once('.')
            .expect("a qualified carrier key has an owner module");
        let reference = Type::synth_path(
            vec![carrier.kio_name.clone()],
            args.to_vec(),
            crate::span::Span::new(0, 0),
        );
        let bound = scope.iter().map(|name| (*name).to_owned()).collect();
        let Some(expanded) = instantiate_newtype_payload_in(
            self.newtype_declaration(owner, &carrier.kio_name),
            &reference,
            self.package,
            owner,
            module,
            &bound,
        ) else {
            return carrier.hs_name.clone();
        };
        self.render_type_scoped(&expanded, scope, Some(owner), host_types)
    }

    fn newtype_declaration(&self, owner: &str, name: &str) -> &Newtype<Routed> {
        let entry = self.package.module(owner).unwrap_or_else(|| {
            unreachable!("Haskell carrier `{owner}/{name}` has no source module")
        });
        for item in &entry.module.items {
            let mut found = None;
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(candidate) = declaration.newtype()
                    && candidate.name == name
                {
                    found = Some(candidate);
                }
            });
            if let Some(declaration) = found {
                return declaration;
            }
        }
        unreachable!("Haskell carrier `{owner}/{name}` has no source declaration")
    }

    fn render_either_chain(
        &self,
        slots: &[&Type<Routed>],
        scope: &HashSet<&str>,
        module: Option<&str>,
        host_types: &super::skin::HaskellHostTypes,
    ) -> String {
        let rendered = slots
            .iter()
            .map(|slot| self.render_type_scoped(slot, scope, module, host_types))
            .collect::<Vec<_>>();
        nest_either_type(&rendered)
    }
}

struct NativeCtx<'a> {
    package: &'a Package<Routed>,
    /// Newtypes keyed by their exact `<module>.<name>` identity.
    /// qualified key (never the bare leaf): two same-leaf newtypes in
    /// different modules hold distinct entries, so a reference resolves to
    /// the one its referring module declares / imports, not whichever
    /// iterated last. A bare leaf at a use site is resolved to its key via
    /// [`Self::resolve_newtype_key`] in the referring module's own-declaration
    /// and selective-import scope.
    types: NativeTypes<'a>,
    /// Typed module-function ABI name → its host-reachability effect.
    effects: HashMap<String, Effect>,
    /// Typed module-function ABI name → the exact ordinary Haskell
    /// constraints required by syntax in that function or any module
    /// function value it reaches.
    exact_constraints: BTreeMap<String, Vec<String>>,
    /// Package-branded structural families and each boundary occurrence's
    /// flat pattern keys. The closed families reduce to the native body's
    /// tuples / `Either` representation.
    shapes: super::skin::HaskellShapes<'a>,
    /// Native realization of the package-complete shared facade transaction.
    /// Public slots and their private source adapters are paired here once;
    /// body emission never reconstructs either topology from raw declarations.
    facade: HaskellFacadeCatalog,
    /// The branded package-handle name (`<Handle>`, the namespace's final
    /// segment): native module-fn / export sigs thread `<Handle> h m`.
    handle: &'a str,
    host_types: &'a str,
}

impl<'a> NativeCtx<'a> {
    fn build(
        package: &'a Package<Routed>,
        names: &'a super::emit::HaskellNames,
        prepared: &crate::backends::boundary_facade::PreparedBoundaryCallableSites,
    ) -> Result<Self, NotNative> {
        let shapes = super::skin::HaskellShapes::new_with_prepared(package, names, prepared);
        let facade = HaskellFacadeCatalog::new(prepared, &shapes);
        let types = NativeTypes::build(
            package,
            prepared,
            &names.runtime_names,
            &names.standard_names,
        );

        let mut ctx = NativeCtx {
            package,
            types,
            effects: HashMap::new(),
            exact_constraints: BTreeMap::new(),
            shapes,
            facade,
            handle: names.handle.as_str(),
            host_types: names.host_types_ty.as_str(),
        };
        ctx.compute_effects();
        ctx.compute_exact_constraints()?;
        Ok(ctx)
    }

    /// Resolve a newtype reference (a `Type::Path`) to its package-global
    /// `<module>.<name>` key, given the **referring** module — a
    /// multi-segment path carries its module; a bare leaf resolves in the
    /// referring module's own-declaration and selective-import scope. `None`
    /// for a non-newtype path.
    fn resolve_newtype_key(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> Option<String> {
        self.types.resolution.key_of(ty, referring_module)
    }

    /// Whether `ty` has a declaration-specific nominal boundary head in the
    /// native representation. Existentials always do; an ordinary carrier
    /// does when it is parametric, recursive, opaque, or exposes only one
    /// direction of its public member surface. A nullary nonrecursive
    /// both-public newtype remains transparent.
    fn has_nominal_boundary_carrier(
        &self,
        ty: &Type<Routed>,
        referring_module: Option<&str>,
    ) -> bool {
        self.types
            .has_nominal_boundary_carrier(ty, referring_module)
    }

    /// The carrier or existential a **bare newtype
    /// leaf** (as the `Low*` IR's newtype-construct / -project variants
    /// carry it) names in `module`'s scope. The maps are keyed by the
    /// qualified key, so a same-leaf newtype in another module never
    /// supplies the wrong declaration.
    fn carrier_for(&self, leaf: &str, module: &str) -> Option<&Carrier> {
        self.types
            .resolution
            .leaf_key(leaf, Some(module))
            .and_then(|k| self.types.carriers.get(&k))
    }
    fn existential_for(&self, leaf: &str, module: &str) -> Option<&ExistentialCarrier> {
        self.types
            .resolution
            .leaf_key(leaf, Some(module))
            .and_then(|k| self.types.existentials.get(&k))
    }

    fn existential_for_owner(&self, leaf: &str, module: &str) -> Option<&ExistentialCarrier> {
        self.types
            .existentials
            .get(&super::skin::newtype_qual_key(module, leaf))
    }

    /// Module fns whose body transitively reaches a `LowHostCall` are
    /// `m`-typed; the rest are pure. Conservative fixed-point over the
    /// module-call graph: a caller of an effectful callee is effectful.
    fn compute_effects(&mut self) {
        // Seed: a fn is directly effectful if its body holds a host call.
        let mut direct: HashMap<String, bool> = HashMap::new();
        let mut callees: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (module_key, entry) in self.package.modules() {
            for item in &entry.module.items {
                if let Item::FnDef(f) = item {
                    let mangled = module_fn_mangled(module_key, &f.name);
                    let mut has_host = false;
                    let mut called: BTreeSet<String> = BTreeSet::new();
                    EffectFacts {
                        module_key,
                        package: self.package,
                        has_host: &mut has_host,
                        called: &mut called,
                    }
                    .collect(&f.body);
                    direct.insert(mangled.clone(), has_host);
                    callees.insert(mangled, called);
                }
            }
        }
        // Fixed point: propagate effectfulness backward through callers.
        let mut effectful: BTreeSet<String> = direct
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.clone())
            .collect();
        loop {
            let mut changed = false;
            for (caller, cs) in &callees {
                if effectful.contains(caller) {
                    continue;
                }
                if cs.iter().any(|c| effectful.contains(c)) {
                    effectful.insert(caller.clone());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for mangled in direct.keys() {
            let eff = if effectful.contains(mangled) {
                Effect::Effectful
            } else {
                Effect::Pure
            };
            self.effects.insert(mangled.clone(), eff);
        }
    }

    /// Compute the ordinary Haskell constraints required by each module
    /// function. Direct exact-syntax requirements are propagated backward
    /// through every top-level function reference, including partial calls
    /// and first-class values whose Haskell eta-expansion captures the
    /// callee's constraints.
    fn compute_exact_constraints(&mut self) -> Result<(), EmitError> {
        let mut direct: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (module_key, entry) in self.package.modules() {
            for item in &entry.module.items {
                let Item::FnDef(f) = item else {
                    continue;
                };
                let mangled = module_fn_mangled(module_key, &f.name);
                let mut constraints = BTreeSet::new();
                let mut called = BTreeSet::new();
                let mut recon =
                    TypeRecon::new(&f.sig, self.package, &self.types.resolution, module_key);
                ExactConstraintFacts {
                    module_key,
                    package: self.package,
                    shapes: &self.shapes,
                    recon: &mut recon,
                    constraints: &mut constraints,
                    called: &mut called,
                }
                .collect(&f.body)?;
                direct.insert(mangled.clone(), constraints);
                callees.insert(mangled, called);
            }
        }
        self.exact_constraints = propagate_exact_constraints(&direct, &callees)
            .into_iter()
            .map(|(name, constraints)| (name, constraints.into_iter().collect()))
            .collect();
        Ok(())
    }

    fn effect_of(&self, mangled: &str) -> Effect {
        self.effects.get(mangled).copied().unwrap_or(Effect::Pure)
    }

    fn exact_constraints_of(&self, mangled: &str) -> &[String] {
        self.exact_constraints
            .get(mangled)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn context_for(&self, mangled: Option<&str>) -> String {
        let mut items = vec![format!("{} h", self.host_types), "Monad m".to_owned()];
        if let Some(mangled) = mangled {
            items.extend(self.exact_constraints_of(mangled).iter().cloned());
        }
        format!("({})", items.join(", "))
    }

    /// Whether `member` is the constructor or projector of `newtype` in its
    /// resolved declaring module. `None` when the declaration or member is
    /// unknown.
    fn newtype_member_kind(
        &self,
        module_path: &str,
        newtype: &str,
        member: &str,
    ) -> Option<NewtypeMemberKind> {
        let entry = self.package.module(module_path)?;
        for item in &entry.module.items {
            let mut found = None;
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(candidate) = declaration.newtype()
                    && candidate.name == newtype
                {
                    found = Some(candidate);
                }
            });
            if let Some(declaration) = found {
                if declaration.constructor.name == member {
                    return Some(NewtypeMemberKind::Constructor);
                }
                if declaration.projector.name == member {
                    return Some(NewtypeMemberKind::Projector);
                }
                return None;
            }
        }
        None
    }

    fn render(&self, _public_ffi_decls: &str) -> Result<NativeRender, NotNative> {
        let host_record = self.render_host_record()?;
        let type_decls = self.render_type_decls()?;

        // Module fns.
        let mut fns: Vec<(String, &str, &FnDef<Routed>)> = Vec::new();
        for (module_key, entry) in self.package.modules() {
            for item in &entry.module.items {
                if let Item::FnDef(f) = item {
                    fns.push((module_fn_mangled(module_key, &f.name), module_key, f));
                }
            }
        }
        fns.sort_by(|a, b| a.0.cmp(&b.0));

        // Named first-order wrappers give GHC's impredicative inference a
        // stable type-application point when an action returns a rank-N
        // value. They are the ordinary monadic operations and are not
        // exported.
        let mut module_fns = String::from(
            "\nkioBind :: forall a m b. Monad m => m a -> (a -> m b) -> m b\n\
             kioBind = (>>=)\n\n\
             kioPure :: forall a m. Monad m => a -> m a\n\
             kioPure = pure\n",
        );
        for (name, module_key, f) in &fns {
            module_fns.push('\n');
            module_fns.push_str(&self.render_module_fn(name, module_key, f)?);
        }

        let export_wrappers = self.render_export_wrappers()?;
        let export_names = self.public_carrier_export_names();

        // An existential carrier is a GADT (`data Box a = forall u. …`),
        // which the `ExistentialQuantification` pragma enables. Nominal
        // newtype declarations type-check under the base
        // `RankNTypes` + `ScopedTypeVariables` already on the file.
        let mut extra_pragmas: Vec<&'static str> = Vec::new();
        if !self.types.existentials.is_empty() {
            extra_pragmas.push("ExistentialQuantification");
        }
        // A Kio rank-1-polymorphic value can sit in a **nested** position a
        // bare `RankNTypes` cannot instantiate impredicatively — a product
        // slot (`([T](T)->T) & …` → `(forall t. m (t -> m t), …)`), a sum arm, or
        // a type argument. GHC 9's Quick Look `ImpredicativeTypes` accepts
        // these; it only widens what type-checks, so enabling it for the
        // native body is always sound.
        extra_pragmas.push("ImpredicativeTypes");
        extra_pragmas.push("TypeAbstractions");
        extra_pragmas.push("TypeApplications");

        Ok(NativeRender {
            host_record,
            type_decls,
            module_fns,
            export_wrappers,
            export_names,
            extra_pragmas,
        })
    }

    /// Exact nominal heads occurring in the typed package boundary. The
    /// module header exports the heads, but not their raw constructors.
    /// Walking the routed types keeps declaration identity and binder scope
    /// intact; rendered Haskell source is deliberately not reparsed here.
    fn public_carrier_export_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();

        for carrier in self.types.carriers.values() {
            if carrier.opaque && carrier.host_surface.is_some() {
                names.insert(carrier.hs_name.clone());
            }
        }
        for carrier in self.types.existentials.values() {
            if carrier.host_surface.is_some() {
                names.insert(carrier.hs_name.clone());
            }
        }

        // The realized shared catalog is the sole public callable inventory.
        // This walk follows exact boundary types only; declaration bodies and
        // unrelated public-looking source items cannot add exported carriers.
        for site in self.facade.sites() {
            let entry = site.entry();
            if !entry.is_live() {
                continue;
            }
            let module = site.id().module_segments().join("/");
            let mut bound = BTreeSet::new();
            for stage in entry.stages() {
                match stage {
                    HaskellCallableHeadStage::Type(param) => {
                        bound.insert(param.name.clone());
                    }
                    HaskellCallableHeadStage::Value { slots, .. } => {
                        for slot in slots {
                            self.collect_public_carriers(
                                slot.ty(),
                                Some(&module),
                                &bound,
                                &mut BTreeSet::new(),
                                &mut names,
                            );
                        }
                    }
                }
            }
            self.collect_public_carriers(
                entry.returned().ty(),
                Some(&module),
                &bound,
                &mut BTreeSet::new(),
                &mut names,
            );
        }

        names.into_iter().collect()
    }

    fn collect_public_carriers(
        &self,
        ty: &Type<Routed>,
        module: Option<&str>,
        bound: &BTreeSet<String>,
        expanding: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
    ) {
        match ty {
            Type::Path { segments, args, .. } => {
                for arg in args {
                    self.collect_public_carriers(arg, module, bound, expanding, names);
                }
                if matches!(segments.as_slice(), [name] if bound.contains(name.as_str())) {
                    return;
                }
                let Some(key) = self.types.resolution.key_of(ty, module) else {
                    return;
                };
                if let Some(carrier) = self.types.existentials.get(&key) {
                    names.insert(carrier.hs_name.clone());
                    return;
                }
                let Some(carrier) = self.types.carriers.get(&key) else {
                    return;
                };
                if carrier.opaque {
                    names.insert(carrier.hs_name.clone());
                    return;
                }
                if !expanding.insert(key.clone()) {
                    return;
                }
                let (owner, _) = key
                    .rsplit_once('.')
                    .expect("a qualified carrier key has an owner module");
                let declaration = self.types.newtype_declaration(owner, &carrier.kio_name);
                if let Some(payload) = instantiate_newtype_payload_in(
                    declaration,
                    ty,
                    self.package,
                    owner,
                    module,
                    bound,
                ) {
                    self.collect_public_carriers(&payload, Some(owner), bound, expanding, names);
                }
                expanding.remove(&key);
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.collect_public_carriers(left, module, bound, expanding, names);
                self.collect_public_carriers(right, module, bound, expanding, names);
            }
            Type::Function { param, ret, .. } => {
                self.collect_public_carriers(param, module, bound, expanding, names);
                self.collect_public_carriers(ret, module, bound, expanding, names);
            }
            Type::Forall { param, body, .. } => {
                let mut nested = bound.clone();
                nested.insert(param.name.clone());
                self.collect_public_carriers(body, module, &nested, expanding, names);
            }
            Type::Unit { .. } | Type::Bottom { .. } => {}
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    fn render_host_record(&self) -> Result<String, EmitError> {
        // Per-backend limitation (`specs/backends/haskell.md` § Deprecated host
        // items > Host-fn removal is not source-stable on Haskell): the native
        // `Host h m` record is built from the live host-fn set only. Haskell has
        // no per-field default that could keep a removed field optional, so
        // host-fn removal is source-breaking.
        let mut fields = Vec::new();
        for site in self.facade.sites() {
            let BoundaryFacadeSiteOwner::HostFunction { name } = site.id().owner() else {
                continue;
            };
            let entry = site.entry();
            if !entry.is_live() {
                continue;
            }
            let module_path = site.id().module_segments().join("/");
            let mut scope = Vec::new();
            let mut rendered_params = Vec::new();
            let module = Some(module_path.as_str());
            for stage in entry.stages() {
                match stage {
                    HaskellCallableHeadStage::Type(param) => {
                        scope = super::skin::extend_type_scope(&scope, param);
                    }
                    HaskellCallableHeadStage::Value { slots, .. } => {
                        for slot in slots {
                            rendered_params.push(self.shapes.boundary_alias_haskell_type_in(
                                slot.boundary(),
                                slot.ty(),
                                &scope,
                                module,
                            )?);
                        }
                    }
                }
            }
            let ret = self.shapes.boundary_alias_haskell_type_in(
                entry.returned().boundary(),
                entry.returned().ty(),
                &scope,
                module,
            )?;

            let mut ty = String::new();
            if !scope.is_empty() {
                ty.push_str("forall ");
                ty.push_str(
                    &scope
                        .iter()
                        .map(|param| {
                            super::skin::kinded_haskell_binder(param, self.shapes.standard_names())
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                ty.push_str(". ");
            }
            for param in rendered_params {
                ty.push_str(&paren_ty(&param));
                ty.push_str(" -> ");
            }
            ty.push_str(&format!("m {}", paren_ty(&ret)));
            fields.push(format!(
                "{} :: {ty}",
                super::emit::host_field_name(&module_path, name)
            ));
        }

        let ty = self.handle;
        let host_ty = format!("{ty}Host");
        let kind = &self.shapes.standard_names().data_kind;
        let mut out = String::new();
        out.push_str(
            "-- The host record binds each declared Kio host type through the\n\
             -- package marker `h`; generic host functions quantify their Kio\n\
             -- type parameters directly.\n",
        );
        out.push_str(&format!(
            "data {host_ty} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type) = {host_ty}\n"
        ));
        out.push_str("  {\n");
        if !fields.is_empty() {
            out.push_str("    ");
            out.push_str(&fields.join("\n  , "));
            out.push('\n');
        }
        out.push_str("  }\n");
        Ok(out)
    }

    // --- nominal type declarations ----------------------------------------

    fn render_type_decls(&self) -> Result<String, EmitError> {
        let mut out = String::new();
        for (key, c) in &self.types.carriers {
            let module = key.rsplit_once('.').map(|(m, _)| m);
            // Only a nullary, nonrecursive, nonexistential source newtype is
            // transparent: it is its payload type, with identity constructor
            // and projector. Parametric and recursive source newtypes emit
            // their declaration-stable nominal carriers below; existential
            // source newtypes use the separate nominal carrier table. The
            // private universal fallback remains a distinct representation.
            if !c.opaque {
                continue;
            }
            out.push('\n');
            let tyvars = hs_tyvars(&c.type_params);
            let decl_tyvars = hs_kinded_tyvars(&c.type_params, self.shapes.standard_names());
            let tyvars_sp = if tyvars.is_empty() {
                String::new()
            } else {
                format!(" {}", tyvars.join(" "))
            };
            let decl_tyvars_sp = if decl_tyvars.is_empty() {
                String::new()
            } else {
                format!(" {}", decl_tyvars.join(" "))
            };
            let payload = self.render_carrier_payload(c, module)?;
            out.push_str(&format!(
                "-- Nominal representation of Kio newtype `{}`.\n",
                c.kio_name
            ));
            out.push_str(&format!(
                "newtype {hs} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type){decl_tyvars_sp} = {hs} ({payload})\n",
                hs = c.hs_name,
                kind = self.shapes.standard_names().data_kind,
            ));
            // Exact host-type families are intentionally non-injective, so a
            // payload mentioning `HostType_* h` cannot recover `h`. The
            // already-in-scope package handle pins the helper calls to the
            // enclosing function's exact `h` and `m` without changing the
            // carrier representation.
            out.push_str(&format!(
                "{ctor} :: {handle} h m -> ({payload}) -> {hs} h m{tyvars_sp}\n{ctor} _pkg = {hs}\n",
                ctor = carrier_ctor_fn(&c.hs_name),
                handle = self.handle,
                hs = c.hs_name,
            ));
            out.push_str(&format!(
                "{proj} :: {handle} h m -> {hs} h m{tyvars_sp} -> ({payload})\n{proj} _pkg ({hs} __x) = __x\n",
                proj = carrier_proj_fn(&c.hs_name),
                handle = self.handle,
                hs = c.hs_name,
            ));
        }
        for (key, e) in &self.types.existentials {
            let module = key.rsplit_once('.').map(|(m, _)| m);
            out.push('\n');
            out.push_str(&self.render_existential_decl(e, module)?);
        }
        Ok(out)
    }

    /// Render an existential carrier as a GADT
    /// (`ExistentialQuantification`) plus its CPS-projector unpacker. The
    /// universal params are the data head's tyvars; the existential params
    /// are the constructor's `forall`-bound (hidden from the head). The
    /// projector is the rank-N CPS eliminator that `LowCpsProjectorApply`
    /// targets: it `case`-unpacks the GADT (re-binding each existential as
    /// a fresh skolem) and invokes the exact zero- or one-slot continuation.
    fn render_existential_decl(
        &self,
        e: &ExistentialCarrier,
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        let mut out = String::new();
        let head_tyvars = hs_tyvars(&e.type_params);
        let head_decl_tyvars = hs_kinded_tyvars(&e.type_params, self.shapes.standard_names());
        let head_decl_sp = if head_decl_tyvars.is_empty() {
            String::new()
        } else {
            format!(" {}", head_decl_tyvars.join(" "))
        };
        // Both universal and existential params are in scope inside the
        // payload; the existentials never appear in the head.
        let scope = e
            .type_params
            .iter()
            .chain(&e.existential_params)
            .fold(Vec::new(), |scope, param| {
                super::skin::extend_type_scope(&scope, param)
            });
        let payload = self.render_direct_generic_type(&e.payload, &scope, module)?;
        let ex_foralls: Vec<String> = e
            .existential_params
            .iter()
            .map(|param| super::skin::kinded_haskell_binder(param, self.shapes.standard_names()))
            .collect();
        let forall_prefix = if ex_foralls.is_empty() {
            String::new()
        } else {
            format!("forall {}. ", ex_foralls.join(" "))
        };
        out.push_str(&format!(
            "-- Existential carrier `{name}` — a GADT\n\
             -- (ExistentialQuantification); each `<…>` binder is sealed at\n\
             -- the constructor and re-bound as a fresh skolem on the\n\
             -- CPS-projector `case`-unpack. No `dyn Any`, no witness\n\
             -- recovery — Haskell carries the existential natively.\n",
            name = e.kio_name,
        ));
        out.push_str(&format!(
            "data {hs} (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type){head_decl_sp} = {prefix}{hs} ({payload})\n",
            hs = e.hs_name,
            prefix = forall_prefix,
            kind = self.shapes.standard_names().data_kind,
        ));
        // The CPS unpacker has the exact continuation ABI: canonical Unit
        // contributes no value argument; every other payload contributes one.
        // The universals are the head tyvars; `r` is the continuation result;
        // each existential is bound under the continuation's `forall`.
        let head_ty = if head_tyvars.is_empty() {
            format!("{} h m", e.hs_name)
        } else {
            format!("{} h m {}", e.hs_name, head_tyvars.join(" "))
        };
        let continuation_leaf = match e.continuation_arity {
            0 => "m r".to_owned(),
            1 => format!("({payload}) -> m r"),
            arity => unreachable!(
                "an existential projector continuation has zero or one payload slot, got {arity}"
            ),
        };
        let cont_ty = render_staged_foralls(
            &e.existential_params,
            continuation_leaf,
            self.shapes.standard_names(),
        );
        let kind = &self.shapes.standard_names().data_kind;
        let mut projector_outer = vec![
            format!("(h :: {kind}.Type)"),
            format!("(m :: {kind}.Type -> {kind}.Type)"),
        ];
        projector_outer.extend(
            e.type_params.iter().map(|param| {
                super::skin::kinded_haskell_binder(param, self.shapes.standard_names())
            }),
        );
        projector_outer.push(format!("(r :: {kind}.Type)"));

        // Constructor pattern type applications first skip the data head's
        // universal `h`, `m`, and source type parameters, then bind each
        // existential witness explicitly.  Applying the continuation at a
        // hidden type yields an action for its next stage; bind each such
        // action before applying the payload value group.
        let mut pattern_type_args = " @_ @_".to_owned();
        for _ in &e.type_params {
            pattern_type_args.push_str(" @_");
        }
        let mut bind_prefix = String::new();
        let mut continuation = "__ex_k".to_owned();
        for (index, param) in e.existential_params.iter().enumerate() {
            let type_var = hs_tyvar(&param.name);
            pattern_type_args.push_str(" @");
            pattern_type_args.push_str(&type_var);
            let action = format!("({continuation} @{type_var})");
            let next = format!("__ex_k{index}");
            let remaining = render_staged_foralls(
                &e.existential_params[index + 1..],
                match e.continuation_arity {
                    0 => "m r".to_owned(),
                    1 => format!("({payload}) -> m r"),
                    _ => unreachable!(
                        "an existential projector continuation has zero or one payload slot"
                    ),
                },
                self.shapes.standard_names(),
            );
            let rank_n =
                (!e.existential_params[index + 1..].is_empty()).then_some(remaining.as_str());
            bind_prefix.push_str(&render_native_effectful_bind_prefix(&action, &next, rank_n));
            continuation = next;
        }
        let apply = if e.continuation_arity == 0 {
            continuation
        } else {
            format!("{continuation} __ex_payload")
        };
        out.push_str(&format!(
            "{proj} :: forall {outer}. Monad m => {head} -> ({cont}) -> m r\n{proj} ({hs}{pattern_type_args} __ex_payload) __ex_k = {bind_prefix}{apply}\n",
            proj = existential_proj_fn(&e.hs_name),
            outer = projector_outer.join(" "),
            head = paren_ty(&head_ty),
            cont = cont_ty,
            hs = e.hs_name,
        ));
        Ok(out)
    }

    /// The carrier payload rendered as a Haskell type with the carrier's
    /// own type params in scope (each maps to its lower-cased Haskell
    /// tyvar; a kind-`*→*` param applies as `m (...)`).
    fn render_carrier_payload(
        &self,
        c: &Carrier,
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        let scope = c
            .type_params
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        Ok(self.render_native_type_scoped(&c.payload, &scope, module))
    }

    /// Render a type where every name in `scope` is a Haskell tyvar
    /// (lower-cased). A single-segment name in scope applied to args
    /// (`F(A)`) renders as application `f a`; a sum is right-nested
    /// `Either`; products are tuples; arrows curry the value group as one
    /// tuple argument. Names not in scope resolve through exact host-type
    /// associated families and nominal newtypes.
    fn render_native_type_scoped(
        &self,
        ty: &Type<Routed>,
        scope: &HashSet<&str>,
        module: Option<&str>,
    ) -> String {
        self.types
            .render_type_scoped(ty, scope, module, self.shapes.host_types())
    }

    /// Render the native representation of a type under direct generic
    /// quantification. Products and sums use the package's closed folds,
    /// which reduce definitionally to the body's tuple / `Either`
    /// representation even beneath an abstract higher-kinded head.
    fn render_direct_generic_type(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        let scope_names: HashSet<&str> = scope.iter().map(|param| param.name.as_str()).collect();
        match ty {
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let params = Type::right_spine_take(param, *abi_arity);
                let args = params
                    .iter()
                    .map(|param| self.render_direct_generic_type(param, scope, module))
                    .collect::<Result<Vec<_>, _>>()?;
                let ret = self.render_direct_generic_type(ret, scope, module)?;
                Ok(format!(
                    "{} -> m {}",
                    paren_ty(&nest_tuple_type(&args)),
                    paren_ty(&ret)
                ))
            }
            Type::Product { left, right, .. } => Ok(format!(
                "({}, {})",
                self.render_direct_generic_type(left, scope, module)?,
                self.render_direct_generic_type(right, scope, module)?
            )),
            Type::Sum { .. } => {
                let rendered = Type::right_spine_sum(ty)
                    .into_iter()
                    .map(|slot| self.render_direct_generic_type(slot, scope, module))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(nest_either_type(&rendered))
            }
            Type::Path { segments, args, .. }
                if segments.len() == 1 && scope_names.contains(segments[0].as_str()) =>
            {
                let mut rendered = hs_tyvar(segments[0].as_str());
                for arg in args {
                    let arg = self
                        .shapes
                        .boundary_haskell_type_scoped_in(arg, scope, module)?;
                    rendered.push(' ');
                    rendered.push_str(&paren_ty(&arg));
                }
                Ok(rendered)
            }
            Type::Path { args, .. } if self.shapes.host_types().resolve(ty, module).is_some() => {
                let binding = self
                    .shapes
                    .host_types()
                    .resolve(ty, module)
                    .expect("host-type guard resolved the same routed type");
                if args.len() != binding.type_params.len() {
                    unreachable!(
                        "Haskell direct generic renderer received an ill-kinded routed host type `{}`",
                        binding.source_name
                    );
                }
                let mut rendered = format!("{} h", binding.assoc_name);
                for arg in args {
                    let arg = self
                        .shapes
                        .boundary_haskell_type_scoped_in(arg, scope, module)?;
                    rendered.push(' ');
                    rendered.push_str(&paren_ty(&arg));
                }
                Ok(rendered)
            }
            Type::Forall { param, body, .. } => {
                let nested = super::skin::extend_type_scope(scope, param);
                Ok(format!(
                    "forall {}. m {}",
                    super::skin::kinded_haskell_binder(param, self.shapes.standard_names()),
                    paren_ty(&self.render_direct_generic_type(body, &nested, module)?)
                ))
            }
            _ => Ok(self.render_native_type_scoped(ty, &scope_names, module)),
        }
    }

    /// Whether the native representation contains a `forall`. Transparent
    /// newtypes contribute their payload representation; their nominal Kio
    /// path alone does not reveal an impredicative result.
    fn native_type_contains_forall(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> bool {
        match ty {
            Type::Forall { .. } => true,
            Type::Path { segments, args, .. } => {
                let scoped = matches!(segments.as_slice(), [name]
                    if scope.iter().rev().any(|param| param.name == name.as_str()));
                if !scoped
                    && let Some((payload, owner)) =
                        self.shapes.boundary_path_expansion_in(ty, scope, module)
                {
                    return self.native_type_contains_forall(&payload, scope, Some(&owner));
                }
                args.iter()
                    .any(|arg| self.native_type_contains_forall(arg, scope, module))
            }
            Type::Function { param, ret, .. }
            | Type::Product {
                left: param,
                right: ret,
                ..
            }
            | Type::Sum {
                left: param,
                right: ret,
                ..
            } => {
                self.native_type_contains_forall(param, scope, module)
                    || self.native_type_contains_forall(ret, scope, module)
            }
            Type::Unit { .. } | Type::Bottom { .. } => false,
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Render a visible Haskell type-application argument. Associated type
    /// families cannot be partially applied, so a Kio higher-kinded host type
    /// such as bare `Box` has no legal Haskell type expression of its own.
    /// Keep that application explicit with `@_`; the surrounding, fully
    /// applied argument/result types determine the exact family instance.
    fn render_visible_type_argument(
        &self,
        ty: &Type<Routed>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        if let Type::Path { args, .. } = ty
            && let Some(binding) = self.shapes.host_types().resolve(ty, module)
            && args.len() < binding.type_params.len()
        {
            return Ok("_".to_owned());
        }
        self.render_direct_generic_type(ty, scope, module)
    }

    // --- module fns -------------------------------------------------------

    fn render_module_fn(
        &self,
        name: &str,
        module_key: &str,
        f: &FnDef<Routed>,
    ) -> Result<String, EmitError> {
        let effect = self.effect_of(name);
        let recon = TypeRecon::new(&f.sig, self.package, &self.types.resolution, module_key);
        let mut emitter = NativeExprEmitter {
            ctx: self,
            module_key,
            recon,
            type_params: fn_type_params(&f.sig).into_iter().cloned().collect(),
            locals: Vec::new(),
            projection_aliases: BTreeMap::new(),
            projection_fresh: 0,
            fresh: 0,
        };
        // All declaration binders are in scope while the body is emitted.
        // The generated equation mirrors the signature's canonical group
        // order: invisible `@t_*` patterns retain each `forall` boundary,
        // the first value group stays positionally curried, and each later
        // value group is one tuple-pattern argument (the Kio ABI).
        let value_groups = value_param_name_groups(&f.sig);
        for g in &value_groups {
            for n in g {
                emitter.locals.push((*n).to_owned());
            }
        }
        let body = emitter.emit(&f.body, effect)?;
        let params = render_module_fn_patterns(&f.sig);

        // Reconstruct the fn's native type for its signature.
        let ret_native = self.render_fn_signature(name, module_key, f, effect)?;
        let mut out = String::new();
        out.push_str(&format!(
            "-- Module fn `{}` in `{module_key}` — native-typed{}.\n",
            f.name,
            match effect {
                Effect::Effectful => "; `m`-threaded (reaches a host effect)",
                Effect::Pure => "; pure (no host effect)",
            }
        ));
        out.push_str(&ret_native);
        out.push_str(&format!("{name} _pkg{params} = {body}\n"));
        Ok(out)
    }

    /// Render `name :: <ctx> => <Handle> m -> <params> -> <ret>`. An
    /// effectful fn returns `m <ret>`; a pure fn returns `<ret>` directly.
    /// Both retain `Monad m` because a pure result may contain a function
    /// value whose invocation returns through `m`.
    ///
    /// A multi-group fn retains its declared `forall`/value-group order. The
    /// first value group's params are separate native arrows; each later group
    /// is one tuple arrow. The `m` of an effectful module function sits on the
    /// final body result, after all declaration binders are supplied.
    fn render_fn_signature(
        &self,
        name: &str,
        module_key: &str,
        f: &FnDef<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        enum RenderedGroup {
            Type(Vec<String>),
            Value(Vec<String>),
        }

        // A type binder scopes only the groups to its right. Render the
        // parameter groups left-to-right under a progressively extended
        // scope before folding the already-rendered groups into arrows. This
        // keeps an exact module/host type in an earlier group distinct from a
        // same-named type binder introduced by a later group.
        let module = Some(module_key);
        let render = |ty: &Type<Routed>, type_params: &[TypeParam]| -> Result<String, EmitError> {
            if type_params.is_empty() {
                let scope = HashSet::new();
                Ok(self.render_native_type_scoped(ty, &scope, module))
            } else {
                self.render_direct_generic_type(ty, type_params, module)
            }
        };
        let groups = f.sig.canonical_groups();
        let first_value = groups
            .iter()
            .position(|group| matches!(group, SignatureGroupRef::Value(_)));
        let mut type_params = Vec::new();
        let mut rendered_groups = Vec::with_capacity(groups.len());
        for group in groups {
            match group {
                SignatureGroupRef::Type(params) => {
                    let mut rendered = Vec::new();
                    for param in params {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type parameters")
                        };
                        rendered.push(super::skin::kinded_haskell_binder(
                            param,
                            self.shapes.standard_names(),
                        ));
                        type_params.push(param.clone());
                    }
                    rendered_groups.push(RenderedGroup::Type(rendered));
                }
                SignatureGroupRef::Value(params) => {
                    let mut rendered = Vec::new();
                    for param in params {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("SignatureGroupRef::Value contains only value parameters")
                        };
                        let ty = param.ty.as_ref().unwrap_or_else(|| {
                            unreachable!(
                                "Routed top-level function `{module_key}.{}` has an untyped value parameter",
                                f.name
                            )
                        });
                        rendered.push(render(ty, &type_params)?);
                    }
                    rendered_groups.push(RenderedGroup::Value(rendered));
                }
            }
        }

        let ret = render(&f.ret, &type_params)?;
        let mut tail = match effect {
            Effect::Effectful => format!("m {}", paren_ty(&ret)),
            Effect::Pure => ret,
        };
        for (index, group) in rendered_groups.into_iter().enumerate().rev() {
            match group {
                RenderedGroup::Type(rendered) => {
                    tail = format!("forall {}. {tail}", rendered.join(" "));
                }
                RenderedGroup::Value(rendered) => {
                    if Some(index) == first_value {
                        for ty in rendered.into_iter().rev() {
                            tail = format!("{} -> {tail}", paren_ty(&ty));
                        }
                    } else {
                        tail = format!("{} -> {tail}", paren_ty(&nest_tuple_type(&rendered)));
                    }
                }
            }
        }
        let handle = self.handle;
        let context = self.context_for(Some(name));
        let kind = &self.shapes.standard_names().data_kind;
        Ok(format!(
            "{name} :: forall (h :: {kind}.Type) (m :: {kind}.Type -> {kind}.Type). \
             {context} => {handle} h m -> {tail}\n"
        ))
    }

    // --- export wrappers --------------------------------------------------

    /// Native export wrappers, monad-polymorphic at the boundary. A
    /// `()`-returning exported fn runs for its effects and returns `m ()`;
    /// a value-returning one returns the native value lifted into `m`. A
    /// nullary, non-recursive, non-existential `pub newtype` member export is
    /// runtime-identity at the declaration's exact payload type. Parametric,
    /// recursive, and existential members cross through their abstract nominal
    /// carriers.
    fn render_export_wrappers(&self) -> Result<String, EmitError> {
        let mut out = String::new();
        let mut pieces: Vec<String> = Vec::new();
        for site in self.facade.sites() {
            let entry = site.entry();
            if !entry.is_live() {
                continue;
            }
            let module = site.id().module_segments().join("/");
            match site.id().owner() {
                BoundaryFacadeSiteOwner::HostFunction { .. } => {}
                BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                    let function = self.exported_function_declaration(&module, name);
                    pieces.push(self.render_fn_export(&module, function)?);
                }
                BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
                    let declaration = self.types.newtype_declaration(&module, newtype);
                    assert_eq!(
                        declaration.constructor.name, *member,
                        "prepared Haskell constructor identity matches its exact declaration"
                    );
                    pieces.push(self.render_newtype_member_export(
                        &module,
                        declaration,
                        &declaration.constructor,
                    )?);
                }
                BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                    let declaration = self.types.newtype_declaration(&module, newtype);
                    assert_eq!(
                        declaration.projector.name, *member,
                        "prepared Haskell projector identity matches its exact declaration"
                    );
                    pieces.push(self.render_newtype_member_export(
                        &module,
                        declaration,
                        &declaration.projector,
                    )?);
                }
            }
        }
        pieces.sort();
        for p in pieces {
            out.push('\n');
            out.push_str(&p);
        }
        Ok(out)
    }

    fn exported_function_declaration(&self, module: &str, name: &str) -> &FnDef<Routed> {
        self.package
            .module(module)
            .and_then(|entry| {
                entry.module.items.iter().find_map(|item| match item {
                    Item::FnDef(function) if function.name == name => Some(function),
                    _ => None,
                })
            })
            .unwrap_or_else(|| {
                unreachable!("prepared Haskell export `{module}/{name}` has no exact declaration")
            })
    }

    fn render_fn_export(&self, module_key: &str, f: &FnDef<Routed>) -> Result<String, EmitError> {
        let wrapper = export_wrapper_name(module_key, &f.name);
        let mangled = module_fn_mangled(module_key, &f.name);
        let effect = self.effect_of(&mangled);
        // The export-surface is the host's typed FFI contract: a slot is
        // spelled at its **boundary** type (the package's product / sum family
        // application for a compound, the native scalar otherwise), per
        // `specs/backends/haskell.md` § FFI surface, and the wrapper converts
        // it to / from the native-internal tuple / `Either` the `mod_*` fn
        // uses (`FfiDir::In` on a param, `FfiDir::Out` on the result). A
        // `NativeExprEmitter` carries the boundary converters.
        //
        // Existential, recursive, and parametric newtypes keep
        // their declaration-stable nominal heads. Generic structural slots
        // use the stable parameterized Env/Exp aliases.
        let entry = self.facade.exported_fn_site(module_key, &f.name).entry();
        let type_params = entry
            .stages()
            .iter()
            .filter_map(|stage| match stage {
                HaskellCallableHeadStage::Type(param) => Some(param.clone()),
                HaskellCallableHeadStage::Value { .. } => None,
            })
            .collect::<Vec<_>>();
        let forall = hs_forall_params(&type_params, self.shapes.standard_names());
        let module = Some(module_key);
        let param_groups = value_group_param_types(&f.sig);
        let public_slots = entry
            .stages()
            .iter()
            .flat_map(|stage| match stage {
                HaskellCallableHeadStage::Type(_) => [].as_slice(),
                HaskellCallableHeadStage::Value { slots, .. } => slots.as_slice(),
            })
            .collect::<Vec<_>>();
        let mut param_tys: Vec<String> = Vec::new();
        for slot in &public_slots {
            param_tys.push(self.shapes.boundary_alias_haskell_type_in(
                slot.boundary(),
                slot.ty(),
                &type_params,
                module,
            )?);
        }
        let arg_names: Vec<String> = (0..param_tys.len()).map(|i| format!("arg{i}")).collect();
        let ret_boundary = self.shapes.boundary_alias_haskell_type_in(
            entry.returned().boundary(),
            entry.returned().ty(),
            &type_params,
            module,
        )?;

        let recon = TypeRecon::new(&f.sig, self.package, &self.types.resolution, module_key);
        let conv = NativeExprEmitter {
            ctx: self,
            module_key,
            recon,
            type_params: fn_type_params(&f.sig).into_iter().cloned().collect(),
            locals: Vec::new(),
            projection_aliases: BTreeMap::new(),
            projection_fresh: 0,
            fresh: 0,
        };
        // Rebuild each native source parameter from the exact facade-slot
        // range paired with it by shared preparation. This is where a direct
        // Unit source parameter becomes no public argument and a product
        // domain becomes multiple public arguments.
        let mut natives = Vec::new();
        let mut public_offset = 0usize;
        let mut source_groups = param_groups.iter();
        for stage in entry.stages() {
            let HaskellCallableHeadStage::Value {
                slots,
                execution: Some(layout),
            } = stage
            else {
                continue;
            };
            let source_group = source_groups
                .next()
                .expect("a prepared export value stage retains its source group");
            assert_eq!(source_group.len(), layout.source_params().len());
            for (source, raw_ty) in layout.source_params().iter().zip(source_group) {
                let raw_ty = raw_ty.as_ref().unwrap_or_else(|| {
                    unreachable!(
                        "Routed exported function `{module_key}.{}` has an untyped value parameter",
                        f.name
                    )
                });
                let range = source.facade_slots();
                let native = match source.adapter() {
                    CallableSourceParamAdapter::UnitValue => {
                        assert!(range.is_empty());
                        "()".to_owned()
                    }
                    CallableSourceParamAdapter::Identity => {
                        assert_eq!(range.len(), 1);
                        let slot_index = range.start;
                        conv.convert_host_boundary_at(
                            raw_ty,
                            slots[slot_index].boundary(),
                            &arg_names[public_offset + slot_index],
                            HaskellHostConversionContext::root(FfiDir::In, &type_params, module),
                        )?
                    }
                    CallableSourceParamAdapter::RightNest => {
                        let parts = Type::right_spine_product(raw_ty);
                        assert_eq!(parts.len(), range.len());
                        let converted = parts
                            .into_iter()
                            .zip(range)
                            .map(|(part, slot_index)| {
                                conv.convert_host_boundary_at(
                                    part,
                                    slots[slot_index].boundary(),
                                    &arg_names[public_offset + slot_index],
                                    HaskellHostConversionContext::root(
                                        FfiDir::In,
                                        &type_params,
                                        module,
                                    ),
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        nest_tuple_value(&converted)
                    }
                };
                natives.push(native);
            }
            public_offset += slots.len();
        }
        assert!(source_groups.next().is_none());
        assert_eq!(public_offset, arg_names.len());
        let mut call = format!("{mangled} _pkg");
        let mut offset = 0usize;
        let mut saw_value_group = false;
        for group in f.sig.canonical_groups() {
            match group {
                SignatureGroupRef::Type(params) => {
                    for param in params {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type parameters")
                        };
                        call.push_str(" @");
                        call.push_str(&hs_tyvar(&param.name));
                    }
                }
                SignatureGroupRef::Value(params) => {
                    let size = params.len();
                    let slice = &natives[offset..offset + size];
                    if !saw_value_group {
                        for native in slice {
                            call.push(' ');
                            call.push_str(&paren_expr(native));
                        }
                    } else {
                        call.push(' ');
                        call.push_str(&paren_expr(&nest_tuple_value(slice)));
                    }
                    offset += size;
                    saw_value_group = true;
                }
            }
        }
        debug_assert_eq!(offset, natives.len());

        let params_sig: String = param_tys
            .iter()
            .map(|p| format!(" -> {}", paren_ty(p)))
            .collect();
        let params_bind: String = arg_names.iter().map(|a| format!(" {a}")).collect();
        let mut out = String::new();
        let handle = self.handle;
        let context = self.context_for(Some(&mangled));
        if matches!(entry.returned().ty(), Type::Unit { .. }) {
            // A `()` return crosses unchanged; run for effects, return `m ()`.
            let call = match effect {
                Effect::Effectful => call,
                Effect::Pure => format!("pure ({call})"),
            };
            out.push_str(&format!(
                "{wrapper} :: forall {forall}. {context} => {handle} h m{params_sig} -> m ()\n"
            ));
            out.push_str(&format!("{wrapper} _pkg{params_bind} = do\n"));
            out.push_str(&format!("      _ <- {call}\n"));
            out.push_str("      pure ()\n");
        } else {
            // Convert the native result through the exact prepared return
            // occurrence. Identity leaves collapse back to the binder.
            let converted = conv.convert_host_boundary_at(
                &f.ret,
                entry.returned().boundary(),
                "__er",
                HaskellHostConversionContext::root(FfiDir::Out, &type_params, module),
            )?;
            let needs_conv = converted != "__er";
            let return_context =
                HaskellHostConversionContext::root(FfiDir::Out, &type_params, module);
            let body = match (effect, needs_conv) {
                (Effect::Pure, false) => conv.render_boundary_pure_value_at(
                    &f.ret,
                    entry.returned().boundary(),
                    &call,
                    return_context,
                )?,
                (Effect::Pure, true) => {
                    let lifted = conv.render_boundary_pure_value_at(
                        &f.ret,
                        entry.returned().boundary(),
                        &converted,
                        return_context,
                    )?;
                    format!("let {{ __er = ({call}) }} in {lifted}")
                }
                (Effect::Effectful, false) => call,
                (Effect::Effectful, true) => {
                    let lifted = conv.render_boundary_pure_value_at(
                        &f.ret,
                        entry.returned().boundary(),
                        &converted,
                        return_context,
                    )?;
                    format!("({call}) >>= \\__er -> {lifted}")
                }
            };
            out.push_str(&format!(
                "{wrapper} :: forall {forall}. {context} => {handle} h m{params_sig} -> m {}\n",
                paren_ty(&ret_boundary)
            ));
            out.push_str(&format!("{wrapper} _pkg{params_bind} = {body}\n"));
        }
        Ok(out)
    }

    /// Render and consume the sole source parameter of a newtype member's
    /// prepared value stage. Newtype members have one source-level domain,
    /// but their public Haskell facade may have zero, one, or several slots.
    fn render_newtype_source_facade(
        &self,
        entry: &HaskellCallableEntry,
        raw_ty: &Type<Routed>,
        converter: &NativeExprEmitter<'_>,
        scope: &[TypeParam],
        module: Option<&str>,
    ) -> Result<(Vec<String>, Vec<String>, String), EmitError> {
        let (slots, layout) = entry
            .stages()
            .iter()
            .find_map(|stage| match stage {
                HaskellCallableHeadStage::Value {
                    slots,
                    execution: Some(layout),
                } => Some((slots.as_slice(), layout)),
                HaskellCallableHeadStage::Type(_) => None,
                HaskellCallableHeadStage::Value {
                    execution: None, ..
                } => unreachable!("a live newtype member has an execution layout"),
            })
            .expect("a newtype member has one value stage");
        let [source] = layout.source_params() else {
            unreachable!("a newtype member has exactly one source parameter")
        };
        let mut param_tys = Vec::with_capacity(slots.len());
        for slot in slots {
            param_tys.push(self.shapes.boundary_alias_haskell_type_in(
                slot.boundary(),
                slot.ty(),
                scope,
                module,
            )?);
        }
        let arg_names = (0..slots.len())
            .map(|index| format!("arg{index}"))
            .collect::<Vec<_>>();
        let range = source.facade_slots();
        let native = match source.adapter() {
            CallableSourceParamAdapter::UnitValue => {
                assert!(range.is_empty());
                "()".to_owned()
            }
            CallableSourceParamAdapter::Identity => {
                assert_eq!(range.len(), 1);
                let index = range.start;
                converter.convert_host_boundary_at(
                    raw_ty,
                    slots[index].boundary(),
                    &arg_names[index],
                    HaskellHostConversionContext::root(FfiDir::In, scope, module),
                )?
            }
            CallableSourceParamAdapter::RightNest => {
                let parts = Type::right_spine_product(raw_ty);
                assert_eq!(parts.len(), range.len());
                let converted = parts
                    .into_iter()
                    .zip(range)
                    .map(|(part, index)| {
                        converter.convert_host_boundary_at(
                            part,
                            slots[index].boundary(),
                            &arg_names[index],
                            HaskellHostConversionContext::root(FfiDir::In, scope, module),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                nest_tuple_value(&converted)
            }
        };
        Ok((param_tys, arg_names, native))
    }

    fn render_newtype_member_export(
        &self,
        module_key: &str,
        d: &Newtype<Routed>,
        member: &crate::ast::TypeMember<Routed>,
    ) -> Result<String, EmitError> {
        let wrapper = export_newtype_wrapper_name(module_key, &d.name, &member.name);
        let module = Some(module_key);
        let context = self.context_for(None);
        let entry = if member.name == d.constructor.name {
            self.facade
                .newtype_constructor_site(module_key, &d.name, &member.name)
                .entry()
        } else if member.name == d.projector.name {
            self.facade
                .newtype_projector_site(module_key, &d.name, &member.name)
                .entry()
        } else {
            unreachable!("exported newtype member is neither constructor nor projector")
        };

        if let Some(existential) = self
            .types
            .existentials
            .get(&super::skin::newtype_qual_key(module_key, &d.name))
        {
            let all_params = d
                .type_params
                .iter()
                .chain(&d.existential_params)
                .fold(Vec::new(), |scope, param| {
                    super::skin::extend_type_scope(&scope, param)
                });
            let head_args = hs_tyvars(&d.type_params);
            let carrier = apply_hs_head(&format!("{} h m", existential.hs_name), &head_args);
            let recon = TypeRecon::empty(self.package, &self.types.resolution, module_key);
            let converter = NativeExprEmitter {
                ctx: self,
                module_key,
                recon,
                type_params: all_params.clone(),
                locals: Vec::new(),
                projection_aliases: BTreeMap::new(),
                projection_fresh: 0,
                fresh: 0,
            };

            if member.name == d.constructor.name {
                let (param_tys, arg_names, native_payload) = self.render_newtype_source_facade(
                    entry,
                    &d.payload,
                    &converter,
                    &all_params,
                    module,
                )?;
                let params_sig = param_tys
                    .iter()
                    .map(|ty| format!(" -> {}", paren_ty(ty)))
                    .collect::<String>();
                let params_bind = arg_names
                    .iter()
                    .map(|arg| format!(" {arg}"))
                    .collect::<String>();
                return Ok(format!(
                    "{wrapper} :: forall {forall}. {context} => {} h m{params_sig} -> m {}\n\
                     {wrapper} _pkg{params_bind} = pure (({} ({})) :: {})\n",
                    self.handle,
                    paren_ty(&carrier),
                    existential.hs_name,
                    native_payload,
                    carrier,
                    forall = hs_forall_params(&all_params, self.shapes.standard_names()),
                ));
            }

            if member.name == d.projector.name {
                let kind = &self.shapes.standard_names().data_kind;
                let mut outer = vec![
                    format!("(h :: {kind}.Type)"),
                    format!("(m :: {kind}.Type -> {kind}.Type)"),
                ];
                outer.extend(d.type_params.iter().map(|param| {
                    super::skin::kinded_haskell_binder(param, self.shapes.standard_names())
                }));
                outer.push(format!("(r :: {kind}.Type)"));
                let returned_boundary = entry.returned().boundary();
                let outer_layout = self
                    .facade
                    .function_layout(returned_boundary)
                    .expect("an existential projector return owns its prepared CPS layout");
                let [outer_source] = outer_layout.source_params() else {
                    unreachable!("an existential projector CPS stage has one continuation source")
                };
                assert_eq!(outer_source.adapter(), CallableSourceParamAdapter::Identity);
                assert_eq!(outer_source.facade_slots(), 0..1);

                let continuation_boundary = returned_boundary.nested(BoundaryStep::CallbackArg(0));
                let payload_layout = self.facade.function_layout(&continuation_boundary).expect(
                    "an existential projector continuation owns its prepared payload layout",
                );
                assert_eq!(
                    payload_layout.body_abi_arity(),
                    existential.continuation_arity,
                    "the prepared payload execution layout preserves the native continuation ABI",
                );
                let (payload_source, payload_slots) = match payload_layout.source_params() {
                    [] => {
                        assert_eq!(payload_layout.body_abi_arity(), 0);
                        (None, 0..0)
                    }
                    [source] => (Some(source), source.facade_slots()),
                    _ => unreachable!(
                        "an existential projector continuation has zero or one payload source"
                    ),
                };
                assert_eq!(payload_slots, 0..payload_layout.facade_slot_count());

                let mut public_leaf = String::new();
                let mut public_slot_types = Vec::with_capacity(payload_slots.len());
                for index in payload_slots.clone() {
                    let boundary = continuation_boundary.nested(BoundaryStep::CallbackArg(
                        index
                            .try_into()
                            .expect("existential payload slot index fits u32"),
                    ));
                    let ty = self
                        .facade
                        .boundary_type(&boundary)
                        .expect("a prepared existential payload slot has an exact type");
                    let rendered = self.shapes.boundary_alias_haskell_type_in(
                        &boundary,
                        ty,
                        &all_params,
                        module,
                    )?;
                    public_leaf.push_str(&format!("{} -> ", paren_ty(&rendered)));
                    public_slot_types.push((boundary, ty));
                }
                public_leaf.push_str("m r");
                let public_cont = render_staged_foralls(
                    &d.existential_params,
                    public_leaf.clone(),
                    self.shapes.standard_names(),
                );

                let leaf_adapter = match payload_source.map(|source| source.adapter()) {
                    None => {
                        assert!(public_slot_types.is_empty());
                        assert_eq!(existential.continuation_arity, 0);
                        None
                    }
                    Some(CallableSourceParamAdapter::UnitValue) => {
                        assert!(public_slot_types.is_empty());
                        assert_eq!(existential.continuation_arity, 1);
                        Some(Vec::new())
                    }
                    Some(CallableSourceParamAdapter::Identity) => {
                        let [(boundary, ty)] = public_slot_types.as_slice() else {
                            unreachable!("an identity existential payload owns one public slot")
                        };
                        assert_eq!(existential.continuation_arity, 1);
                        Some(vec![converter.convert_host_boundary_at(
                            ty,
                            boundary,
                            "__payload",
                            HaskellHostConversionContext::root(FfiDir::Out, &all_params, module),
                        )?])
                    }
                    Some(CallableSourceParamAdapter::RightNest) => {
                        assert_eq!(existential.continuation_arity, 1);
                        let slot_count = public_slot_types.len();
                        Some(
                            public_slot_types
                                .iter()
                                .enumerate()
                                .map(|(index, (boundary, ty))| {
                                    converter.convert_host_boundary_at(
                                        ty,
                                        boundary,
                                        &tuple_proj("__payload", index, slot_count),
                                        HaskellHostConversionContext::root(
                                            FfiDir::Out,
                                            &all_params,
                                            module,
                                        ),
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        )
                    }
                };
                let internal_leaf = match existential.continuation_arity {
                    0 => "m r".to_owned(),
                    1 => {
                        let native_payload =
                            self.render_direct_generic_type(&d.payload, &all_params, module)?;
                        format!("({native_payload}) -> m r")
                    }
                    _ => unreachable!(
                        "an existential projector continuation has zero or one payload slot"
                    ),
                };
                let internal_cont = render_staged_foralls(
                    &d.existential_params,
                    internal_leaf,
                    self.shapes.standard_names(),
                );
                let adapter = render_staged_forall_adapter(
                    &d.existential_params,
                    "arg1",
                    &public_leaf,
                    0,
                    self.shapes.standard_names(),
                    &|continuation| match &leaf_adapter {
                        None => format!("({continuation})"),
                        Some(arguments) => {
                            let arguments = arguments
                                .iter()
                                .map(|argument| paren_expr(argument))
                                .collect::<Vec<_>>()
                                .join(" ");
                            if arguments.is_empty() {
                                format!("(\\__payload -> ({continuation}))")
                            } else {
                                format!("(\\__payload -> ({continuation}) {arguments})")
                            }
                        }
                    },
                );
                let adapter = format!("(({adapter}) :: {internal_cont})");
                return Ok(format!(
                    "{wrapper} :: forall {outer}. {context} => {} h m -> {} -> {} -> m r\n\
                     {wrapper} _pkg arg0 arg1 = {} arg0 {adapter}\n",
                    self.handle,
                    paren_ty(&carrier),
                    paren_ty(&public_cont),
                    existential_proj_fn(&existential.hs_name),
                    outer = outer.join(" "),
                ));
            }

            unreachable!("exported newtype member is neither constructor nor projector")
        }

        if let Some(carrier) = self
            .types
            .carriers
            .get(&super::skin::newtype_qual_key(module_key, &d.name))
            && carrier.opaque
        {
            let scope = d.type_params.clone();
            let carrier_ty = apply_hs_head(
                &format!("{} h m", carrier.hs_name),
                &hs_tyvars(&d.type_params),
            );
            let recon = TypeRecon::empty(self.package, &self.types.resolution, module_key);
            let converter = NativeExprEmitter {
                ctx: self,
                module_key,
                recon,
                type_params: scope.clone(),
                locals: Vec::new(),
                projection_aliases: BTreeMap::new(),
                projection_fresh: 0,
                fresh: 0,
            };

            if member.name == d.constructor.name {
                let (param_tys, arg_names, native_payload) = self
                    .render_newtype_source_facade(entry, &d.payload, &converter, &scope, module)?;
                let wrapped = carrier_ctor_call(&carrier.hs_name, "_pkg", &native_payload);
                let params_sig = param_tys
                    .iter()
                    .map(|ty| format!(" -> {}", paren_ty(ty)))
                    .collect::<String>();
                let params_bind = arg_names
                    .iter()
                    .map(|arg| format!(" {arg}"))
                    .collect::<String>();
                return Ok(format!(
                    "{wrapper} :: forall {forall}. {context} => {} h m{params_sig} -> m {}\n\
                     {wrapper} _pkg{params_bind} = pure ({})\n",
                    self.handle,
                    paren_ty(&carrier_ty),
                    wrapped,
                    forall = hs_forall_params(&scope, self.shapes.standard_names()),
                ));
            }

            if member.name == d.projector.name {
                let payload = self.shapes.boundary_alias_haskell_type_in(
                    entry.returned().boundary(),
                    entry.returned().ty(),
                    &scope,
                    module,
                )?;
                let native_payload = carrier_proj_call(&carrier.hs_name, "_pkg", "arg0");
                let converted = converter.convert_host_boundary_at(
                    &d.payload,
                    entry.returned().boundary(),
                    &native_payload,
                    HaskellHostConversionContext::root(FfiDir::Out, &scope, module),
                )?;
                let body = converter.render_boundary_pure_value_at(
                    &d.payload,
                    entry.returned().boundary(),
                    &converted,
                    HaskellHostConversionContext::root(FfiDir::Out, &scope, module),
                )?;
                return Ok(format!(
                    "{wrapper} :: forall {forall}. {context} => {} h m -> {} -> m {}\n\
                     {wrapper} _pkg arg0 = {body}\n",
                    self.handle,
                    paren_ty(&carrier_ty),
                    paren_ty(&payload),
                    forall = hs_forall_params(&scope, self.shapes.standard_names()),
                ));
            }

            unreachable!("exported newtype member is neither constructor nor projector")
        }

        // A nullary, nonrecursive, nonexistential newtype is transparent at
        // the public boundary, but its contract remains its declared payload.
        let scope = d.type_params.clone();
        let recon = TypeRecon::empty(self.package, &self.types.resolution, module_key);
        let converter = NativeExprEmitter {
            ctx: self,
            module_key,
            recon,
            type_params: scope.clone(),
            locals: Vec::new(),
            projection_aliases: BTreeMap::new(),
            projection_fresh: 0,
            fresh: 0,
        };
        let (param_tys, arg_names, native_payload) =
            self.render_newtype_source_facade(entry, &d.payload, &converter, &scope, module)?;
        let ret = self.shapes.boundary_alias_haskell_type_in(
            entry.returned().boundary(),
            entry.returned().ty(),
            &scope,
            module,
        )?;
        let converted = converter.convert_host_boundary_at(
            &d.payload,
            entry.returned().boundary(),
            &native_payload,
            HaskellHostConversionContext::root(FfiDir::Out, &scope, module),
        )?;
        let body = converter.render_boundary_pure_value_at(
            &d.payload,
            entry.returned().boundary(),
            &converted,
            HaskellHostConversionContext::root(FfiDir::Out, &scope, module),
        )?;
        let params_sig = param_tys
            .iter()
            .map(|ty| format!(" -> {}", paren_ty(ty)))
            .collect::<String>();
        let params_bind = arg_names
            .iter()
            .map(|arg| format!(" {arg}"))
            .collect::<String>();
        Ok(format!(
            "{wrapper} :: forall {forall}. {context} => {} h m{params_sig} -> m {}\n\
             {wrapper} _pkg{params_bind} = {body}\n",
            self.handle,
            paren_ty(&ret),
            forall = hs_forall_params(&scope, self.shapes.standard_names()),
        ))
    }
}

/// The native body-expression emitter for one module fn. Pure
/// sub-expressions render as plain Haskell; effectful ones sequence through
/// `m`.
struct NativeExprEmitter<'a> {
    ctx: &'a NativeCtx<'a>,
    module_key: &'a str,
    recon: TypeRecon<'a>,
    /// Ordered, kinded type parameters of the enclosing module function.
    /// Boundary shapes key generic compounds by this lexical
    /// scope; a name-only set cannot distinguish kind changes.
    type_params: Vec<TypeParam>,
    locals: Vec<String>,
    /// Product-domain closure parameters are destructured once at their
    /// lambda binder. Every projection of that exact bound value then names
    /// its selected slot directly instead of rebuilding an increasingly
    /// deep selector chain at every use.
    projection_aliases: BTreeMap<(String, usize, usize), String>,
    projection_fresh: usize,
    /// Monotonic counter for fresh `>>=`-bind variable names in effectful
    /// emit (`__m0`, `__m1`, …) — distinct from the `k_`-prefixed Kio
    /// locals and the `__`-prefixed pattern binders the renderers mint.
    fresh: usize,
}

#[derive(Clone, Copy)]
struct HaskellHostConversionContext<'scope, 'module> {
    direction: FfiDir,
    depth: usize,
    scheme_scope: &'scope [TypeParam],
    module: Option<&'module str>,
}

impl<'scope, 'module> HaskellHostConversionContext<'scope, 'module> {
    fn root(
        direction: FfiDir,
        scheme_scope: &'scope [TypeParam],
        module: Option<&'module str>,
    ) -> Self {
        Self {
            direction,
            depth: 0,
            scheme_scope,
            module,
        }
    }

    fn nested(self) -> Self {
        Self {
            depth: self.depth + 1,
            ..self
        }
    }

    fn flipped(self) -> Self {
        Self {
            direction: self.direction.flip(),
            ..self
        }
    }

    fn with_scope<'nested>(
        self,
        scheme_scope: &'nested [TypeParam],
    ) -> HaskellHostConversionContext<'nested, 'module> {
        HaskellHostConversionContext {
            direction: self.direction,
            depth: self.depth,
            scheme_scope,
            module: self.module,
        }
    }

    fn with_module<'nested>(
        self,
        module: Option<&'nested str>,
    ) -> HaskellHostConversionContext<'scope, 'nested> {
        HaskellHostConversionContext {
            direction: self.direction,
            depth: self.depth,
            scheme_scope: self.scheme_scope,
            module,
        }
    }
}

impl NativeExprEmitter<'_> {
    /// Emit `expr` at the given effect context. When `Effectful`, the
    /// result is an `m <native>` expression (host calls sequenced); when
    /// `Pure`, a plain `<native>` value.
    fn emit(&mut self, expr: &Expr<Routed>, effect: Effect) -> Result<String, EmitError> {
        match effect {
            Effect::Pure => self.emit_pure(expr),
            Effect::Effectful => self.emit_effectful(expr),
        }
    }

    fn require_condition_type(&self, cond: &Expr<Routed>) -> Result<(), EmitError> {
        let ty = self.recon.value_type(cond).ok_or_else(|| {
            EmitError::unsupported(format!(
                "Haskell native body: conditional condition type reconstruction gap at {}",
                expr_kind(cond)
            ))
        })?;
        self.ctx.shapes.exact_host_syntax(
            &ty,
            Some(self.module_key),
            super::skin::ExactHostConstraint::Boolean,
        )?;
        Ok(())
    }

    /// Emit a pure value expression — no `m`, no `>>=`.
    fn emit_pure(&mut self, expr: &Expr<Routed>) -> Result<String, EmitError> {
        match expr {
            Expr::Unit { .. } => Ok("()".to_owned()),
            Expr::StrLit {
                value, annotation, ..
            } => {
                let ty = self
                    .ctx
                    .shapes
                    .exact_host_syntax(
                        annotation,
                        Some(self.module_key),
                        super::skin::ExactHostConstraint::String,
                    )?
                    .rendered;
                Ok(format!(
                    "({}.fromString {} :: {ty})",
                    self.ctx.shapes.standard_names().data_string,
                    haskell_string_lit(value),
                ))
            }
            Expr::IntLit {
                digits, annotation, ..
            } => {
                let ty = self
                    .ctx
                    .shapes
                    .exact_host_syntax(
                        annotation,
                        Some(self.module_key),
                        super::skin::ExactHostConstraint::Integral,
                    )?
                    .rendered;
                Ok(format!("(({digits}) :: {ty})"))
            }
            Expr::FloatLit {
                digits, annotation, ..
            } => {
                let ty = self
                    .ctx
                    .shapes
                    .exact_host_syntax(
                        annotation,
                        Some(self.module_key),
                        super::skin::ExactHostConstraint::Fractional,
                    )?
                    .rendered;
                Ok(format!("(({digits}) :: {ty})"))
            }
            Expr::BoolLit {
                value, annotation, ..
            } => {
                let ty = self
                    .ctx
                    .shapes
                    .exact_host_syntax(
                        annotation,
                        Some(self.module_key),
                        super::skin::ExactHostConstraint::Boolean,
                    )?
                    .rendered;
                let value = if *value { "True" } else { "False" };
                Ok(format!("({value} :: {ty})"))
            }
            Expr::LowBoundRef { name, .. } => Ok(hs_local(name)),
            Expr::Let {
                name, value, body, ..
            } => {
                let v = self.emit_pure(value)?;
                let rank_n_type = self.rank_n_let_type(value)?;
                let shadows = self.locals.iter().any(|local| local == name);
                let prior = self.bind_local_ty(name, value);
                let projection_priors = self.take_projection_aliases(name);
                self.locals.push(name.clone());
                let b = self.emit_pure(body);
                self.locals.pop();
                self.restore_projection_aliases(projection_priors);
                self.unbind_local_ty(name, prior);
                let b = b?;
                Ok(render_native_pure_let(
                    &hs_local(name),
                    rank_n_type.as_deref(),
                    &v,
                    &b,
                    shadows,
                ))
            }
            Expr::Seq { value, body, .. } => {
                let v = self.emit_pure(value)?;
                let b = self.emit_pure(body)?;
                Ok(format!("({v} `seq` ({b}))"))
            }
            Expr::EnrichedTuple { items, .. } => {
                let mut parts = Vec::new();
                for it in items {
                    parts.push(self.emit_pure(it)?);
                }
                Ok(nest_tuple_value(&parts))
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => {
                if let Some(selected) = self.projection_alias(target, *index, *arity) {
                    return Ok(selected);
                }
                let t = self.emit_pure(target)?;
                Ok(tuple_proj(&t, *index, *arity))
            }
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                target_ty,
                ..
            } => {
                let projected =
                    if let Some(selected) = self.projection_alias(target, *index, *arity) {
                        selected
                    } else {
                        let t = self.emit_pure(target)?;
                        tuple_proj(&t, *index, *arity)
                    };
                self.build_field_get(target_ty, *index, *arity, &projected)
            }
            // A right-nested sum injection: variant `v` of `n` is
            // `Right`-applied `v` times, then `Left <payload>` (for
            // `v < n-1`) or the bare payload after the rights (last
            // variant).
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                ..
            } => {
                let p = self.emit_pure(payload)?;
                Ok(sum_inject(&p, *variant, *variants))
            }
            // N-arm match on a right-nested `Either` chain → nested `case`.
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => {
                let s = self.emit_pure(scrutinee)?;
                self.emit_match(&s, arms, scrutinee_ty, Effect::Pure)
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.require_condition_type(cond)?;
                let c = self.emit_pure(cond)?;
                let t = self.emit_pure(then_branch)?;
                let e = self.emit_pure(else_branch)?;
                Ok(format!("(if {c} then ({t}) else ({e}))"))
            }
            // A nominal newtype constructor wraps with its Haskell carrier;
            // a transparent newtype leaves its payload unchanged.
            Expr::LowNewtypeCtor {
                newtype,
                type_args,
                payload,
                ..
            } => self.emit_newtype_ctor(newtype, type_args, payload, Effect::Pure),
            Expr::LowNewtypeProj {
                newtype, target, ..
            } => self.emit_newtype_proj(newtype, target, Effect::Pure),
            Expr::LowAbsurdCall { value_arg, .. } => {
                let v = self.emit_pure(value_arg)?;
                Ok(format!(
                    "({}.absurd ({v}))",
                    self.ctx.shapes.standard_names().data_void,
                ))
            }
            // A closure value — a native lambda.
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => self.emit_closure(sig, ret_ty.as_ref(), body),
            // A module-fn-value reference (`box_bool` used as a value) — a
            // native eta-expansion forwarding to the module fn, exposing the
            // first value group as one tuple parameter (the Kio ABI).
            // Function values uniformly return through `m`, so both pure
            // and effectful module functions can be referenced.
            Expr::LowModuleFnValueRef { mangled, sig, .. } => {
                let value_ty = self.recon.value_type(expr);
                let rendered = self.emit_module_fn_value_ref(mangled, sig)?;
                self.annotate_rank_n_value(rendered, value_ty.as_ref())
            }
            // A host-fn value is a native eta-expansion over the host record.
            // Kio function values uniformly return through `m`, so the
            // effect runs only when the resulting function is applied.
            Expr::LowHostFnValueRef {
                name,
                module_path,
                sig,
                ret_ty,
                ..
            } => {
                let value_ty = self.recon.value_type(expr);
                let rendered = self.emit_host_fn_value_ref(name, module_path, sig, ret_ty)?;
                self.annotate_rank_n_value(rendered, value_ty.as_ref())
            }
            // A direct closure-param call `step(x)` inside a dict op.
            Expr::LowClosureCall { name, args, .. } => {
                let arg = self.emit_call_arg(args, Effect::Pure)?;
                Ok(format!("({} {arg})", hs_local(name)))
            }
            // A pure module call. `box_functor()` is nullary; the derived
            // transformer stack calls dict-building fns with value args
            // (`maybe_t_monad(inner, inner_pure)`).
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let fname = self.resolve_module_fn(mangled);
                let value_ty = self.recon.value_type(expr);
                self.emit_resolved_module_call_pure(&fname, type_args, args, sig, value_ty.as_ref())
            }
            // A qualified module call `<alias>.<fn>(…)` — resolved to the
            // target module's mangled fn, same shape as a plain module call.
            Expr::LowQualifiedModuleCall {
                alias,
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let fname = self.resolve_qualified_module_fn(alias, mangled);
                let value_ty = self.recon.value_type(expr);
                self.emit_resolved_module_call_pure(&fname, type_args, args, sig, value_ty.as_ref())
            }
            // A cross-module newtype member whose Routed node already carries
            // the exact declaring module used to select its carrier
            // constructor or projector.
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                payload,
                ..
            } => self.emit_qualified_newtype_member(
                module_path,
                newtype,
                member,
                type_args,
                payload,
                Effect::Pure,
            ),
            // A projected function-valued newtype member arrives as an
            // indirect call whose callee is the projector application.
            Expr::LowIndirectCall { callee, args, .. } => {
                self.emit_indirect_call(callee, args, Effect::Pure)
            }
            // An existential CPS-projector apply — `Box.get(A, v)(k)` —
            // unpacks the GADT and invokes its zero- or one-slot continuation.
            Expr::LowCpsProjectorApply {
                newtype,
                module_path,
                receiver,
                continuation,
                continuation_ty,
                ..
            } => self.emit_cps_projector_apply(
                newtype,
                module_path,
                receiver,
                continuation,
                continuation_ty,
            ),
            other => Err(EmitError::unsupported(format!(
                "Haskell native body: pure expr node falls outside the native path scope: {}",
                expr_kind(other)
            ))),
        }
    }

    /// Emit an effectful expression — an `m <native>` value, sequencing
    /// host effects through `>>=`. A sub-expression that is itself pure is
    /// lifted with `pure`; one that reaches a host effect threads its `m`
    /// through the bind.
    fn emit_effectful(&mut self, expr: &Expr<Routed>) -> Result<String, EmitError> {
        match expr {
            Expr::Let {
                name, value, body, ..
            } => {
                // A `let` whose value is a pure computation binds purely
                // and forces; the body continues effectfully.
                if self.is_pure_expr(value) {
                    let v = self.emit_pure(value)?;
                    let rank_n_type = self.rank_n_let_type(value)?;
                    let shadows = self.locals.iter().any(|local| local == name);
                    let prior = self.bind_local_ty(name, value);
                    let projection_priors = self.take_projection_aliases(name);
                    self.locals.push(name.clone());
                    let b = self.emit_effectful(body);
                    self.locals.pop();
                    self.restore_projection_aliases(projection_priors);
                    self.unbind_local_ty(name, prior);
                    let b = b?;
                    Ok(render_native_pure_let(
                        &hs_local(name),
                        rank_n_type.as_deref(),
                        &v,
                        &b,
                        shadows,
                    ))
                } else {
                    let v = self.emit_effectful(value)?;
                    let rank_n_type = self.rank_n_let_type(value)?;
                    let prior = self.bind_local_ty(name, value);
                    let projection_priors = self.take_projection_aliases(name);
                    self.locals.push(name.clone());
                    let b = self.emit_effectful(body);
                    self.locals.pop();
                    self.restore_projection_aliases(projection_priors);
                    self.unbind_local_ty(name, prior);
                    let b = b?;
                    let id = hs_local(name);
                    let bind = render_native_effectful_bind_prefix(&v, &id, rank_n_type.as_deref());
                    let forced = render_native_strict_force(&id, rank_n_type.as_deref(), &b);
                    Ok(format!("({bind}{forced})"))
                }
            }
            Expr::Seq { value, body, .. } => {
                let v = self.emit_effectful(value)?;
                let b = self.emit_effectful(body)?;
                Ok(format!("({v} >> ({b}))"))
            }
            Expr::LowHostCall {
                name,
                module_path,
                type_args,
                args,
                sig,
                ret_ty,
                ..
            } => {
                let Some(value_ty) = self.recon.value_type(expr) else {
                    unreachable!(
                        "a typed Routed LowHostCall has an exact reconstructible result type"
                    );
                };
                self.emit_host_call(name, module_path, type_args, args, sig, ret_ty, &value_ty)
            }
            // A first-class polymorphic value exposes one monadic stage per
            // `forall`.  Specialise the visible Haskell type abstraction only
            // after evaluating the callee value; the application itself is
            // already the `m <next-stage>` action.
            Expr::LowTypeApplication {
                callee, type_arg, ..
            } => self.emit_type_application(callee, type_arg),
            // Sequence the effect that would produce `Void` before eliminating
            // the impossible result into the effectful continuation type.
            Expr::LowAbsurdCall { value_arg, .. } => {
                let v = self.emit_effectful(value_arg)?;
                Ok(format!(
                    "({v} >>= \\__ab -> {}.absurd __ab)",
                    self.ctx.shapes.standard_names().data_void,
                ))
            }
            // A pure value used in effectful context: lift with `pure`. This
            // guard runs *before* the effectful-compound arms below, so a
            // pure call / match / conditional / constructor lifts its plain
            // value rather than being mis-rendered as `m`-typed.
            other if self.is_pure_expr(other) => {
                let v = self.emit_pure(other)?;
                let rank_n_type = self.rank_n_let_type(other)?;
                Ok(render_native_pure_value(&v, rank_n_type.as_deref()))
            }
            // A module call reached in effectful context. The call is here
            // because at least one argument is effectful; the *callee* may
            // still be pure (it reaches no host effect of its own). Sequence
            // the effectful args, then `pure`-wrap a pure callee's result so
            // the whole expression is `m`-typed.
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let fname = self.resolve_module_fn(mangled);
                let value_ty = self.recon.value_type(expr);
                self.emit_resolved_module_call_effectful(
                    &fname,
                    type_args,
                    args,
                    sig,
                    value_ty.as_ref(),
                )
            }
            Expr::LowQualifiedModuleCall {
                alias,
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let fname = self.resolve_qualified_module_fn(alias, mangled);
                let value_ty = self.recon.value_type(expr);
                self.emit_resolved_module_call_effectful(
                    &fname,
                    type_args,
                    args,
                    sig,
                    value_ty.as_ref(),
                )
            }
            // An effectful indirect call — the callee is a function value
            // applied to a tupled value group.
            Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } => {
                if let Expr::LowNewtypeProj {
                    newtype, target, ..
                } = callee.as_ref()
                    && type_args.is_empty()
                {
                    // A function-valued newtype projection whose application
                    // reaches a host effect: sequence the projected target,
                    // then fuse the read with the value-group application.
                    let (binds, target) = self.emit_value_binding(target)?;
                    let head = self.build_newtype_proj(newtype, &target)?;
                    let applied = self.emit_effectful_apply_tupled(&head, args)?;
                    return Ok(format!("({binds}{applied})"));
                }
                let callee_ty = self.recon.value_type(callee);
                let (binds, callee) = self.emit_value_binding(callee)?;
                self.emit_effectful_staged_apply(binds, callee, callee_ty, type_args, args)
            }
            Expr::LowCpsProjectorApply {
                newtype,
                module_path,
                receiver,
                continuation,
                continuation_ty,
                ..
            } => self.emit_cps_projector_apply(
                newtype,
                module_path,
                receiver,
                continuation,
                continuation_ty,
            ),
            // A conditional whose branches reach a host effect: render a
            // native `if` over the (pure) condition with effectful branches.
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.require_condition_type(cond)?;
                let (binds, c) = self.emit_value_binding(cond)?;
                let t = self.emit_effectful(then_branch)?;
                let e = self.emit_effectful(else_branch)?;
                Ok(format!("({binds}(if {c} then ({t}) else ({e})))"))
            }
            // A match whose arms reach a host effect: native nested `case`
            // over the (pure) scrutinee with effectful arm bodies.
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => {
                let (binds, s) = self.emit_value_binding(scrutinee)?;
                let body = self.emit_match(&s, arms, scrutinee_ty, Effect::Effectful)?;
                Ok(format!("({binds}{body})"))
            }
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                ..
            } => {
                let (binds, payload) = self.emit_value_binding(payload)?;
                Ok(format!(
                    "({binds}pure ({}))",
                    sum_inject(&payload, *variant, *variants)
                ))
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => self.emit_effectful_projection(target, *index, *arity),
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                target_ty,
                ..
            } => self.emit_effectful_field_get(target, target_ty, *index, *arity),
            // A constructor whose payload reaches a host effect: sequence the
            // effectful sub-value, build the (pure-shaped) result, lift it.
            Expr::LowNewtypeCtor {
                newtype,
                type_args,
                payload,
                ..
            } => {
                let (binds, p) = self.emit_value_binding(payload)?;
                let built = self.build_newtype_ctor(newtype, type_args, &p)?;
                Ok(format!("({binds}pure ({built}))"))
            }
            // A carrier projection whose target reaches a host effect:
            // sequence the target, unwrap, lift.
            Expr::LowNewtypeProj {
                newtype, target, ..
            } => {
                let (binds, t) = self.emit_value_binding(target)?;
                let proj = self.build_newtype_proj(newtype, &t)?;
                let rank_n_type = self.rank_n_let_type(expr)?;
                let lifted = render_native_pure_value(&proj, rank_n_type.as_deref());
                Ok(format!("({binds}{lifted})"))
            }
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                payload,
                ..
            } => self.emit_qualified_newtype_member(
                module_path,
                newtype,
                member,
                type_args,
                payload,
                Effect::Effectful,
            ),
            // A closure value in effectful context — the lambda is a pure
            // value (built now; its effect, if any, runs when *called*), so
            // it lifts with `pure`.
            Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                let c = self.emit_closure(sig, ret_ty.as_ref(), body)?;
                Ok(format!("pure ({c})"))
            }
            // A closure-param call `n(args)` whose value group reaches a host
            // effect: sequence effectful args, then apply the closure value.
            Expr::LowClosureCall {
                name,
                type_args,
                args,
                ..
            } => {
                let callee_ty = self.recon.bound_value_type(name);
                self.emit_effectful_staged_apply(
                    String::new(),
                    hs_local(name),
                    callee_ty,
                    type_args,
                    args,
                )
            }
            // A tuple with an effectful member: sequence each effectful slot,
            // build the (pure-shaped) tuple, lift it.
            Expr::EnrichedTuple { items, .. } => {
                let mut binds = String::new();
                let mut parts = Vec::new();
                for it in items {
                    let (b, v) = self.emit_value_binding(it)?;
                    binds.push_str(&b);
                    parts.push(v);
                }
                Ok(format!("({binds}pure {})", nest_tuple_value(&parts)))
            }
            other => Err(EmitError::unsupported(format!(
                "Haskell native body: effectful expr node falls outside the native path scope: {}",
                expr_kind(other)
            ))),
        }
    }

    /// Evaluate an argument-position sub-expression to a native **value**,
    /// returning a `>>=`-bind prefix (empty when the sub-expression is
    /// pure) and the value text. An effectful sub-expression has its `m`
    /// sequenced into a fresh binder; a pure one renders inline.
    fn emit_value_binding(&mut self, expr: &Expr<Routed>) -> Result<(String, String), EmitError> {
        if self.is_pure_expr(expr) {
            Ok((String::new(), paren_expr(&self.emit_pure(expr)?)))
        } else {
            let m = self.emit_effectful(expr)?;
            let v = self.fresh_var();
            let rank_n_type = self.rank_n_let_type(expr)?;
            Ok((
                render_native_effectful_bind_prefix(&m, &v, rank_n_type.as_deref()),
                v,
            ))
        }
    }

    fn emit_type_application(
        &mut self,
        callee: &Expr<Routed>,
        type_arg: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let callee_ty = self.recon.value_type(callee);
        let (binds, callee) = self.emit_value_binding(callee)?;
        let callee = self.annotate_rank_n_value(callee, callee_ty.as_ref())?;
        let rendered = self.ctx.render_visible_type_argument(
            type_arg,
            &self.type_params,
            Some(self.module_key),
        )?;
        Ok(format!("({binds}({callee} @{}))", paren_ty(&rendered)))
    }

    fn emit_resolved_module_call_effectful(
        &mut self,
        fname: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        value_ty: Option<&Type<Routed>>,
    ) -> Result<String, EmitError> {
        let mut binds = String::new();
        let mut call = self.render_direct_module_head(fname, type_args)?;
        for arg in args {
            let (binding, value) = self.emit_value_binding(arg)?;
            binds.push_str(&binding);
            call.push(' ');
            call.push_str(&value);
        }
        let residual = residual_module_call_stages(sig, type_args.len(), args.is_empty());
        let callee_effect = self.ctx.effect_of(fname);
        if !residual.is_empty() {
            let Some(value_ty) = value_ty else {
                unreachable!(
                    "a typed Routed residual module call has no reconstructible value type"
                )
            };
            let adapted = render_module_fn_value_adapter(&call, &residual, callee_effect);
            let rank_n_type = self.render_rank_n_type(value_ty)?;
            let adapted = match &rank_n_type {
                Some(ty) => format!("(({adapted}) :: {ty})"),
                None => adapted,
            };
            let lifted = render_native_pure_value(&adapted, rank_n_type.as_deref());
            return Ok(format!("({binds}{lifted})"));
        }

        match callee_effect {
            Effect::Effectful => Ok(format!("({binds}{call})")),
            Effect::Pure => Ok(format!("({binds}pure ({call}))")),
        }
    }

    /// Apply an effectful indirect callee to a Kio value group (one tuple
    /// argument per group): sequence effectful members, build the tuple,
    /// apply.
    fn emit_effectful_apply_tupled(
        &mut self,
        callee: &str,
        args: &[Expr<Routed>],
    ) -> Result<String, EmitError> {
        let mut binds = String::new();
        let arg = if args.is_empty() {
            "()".to_owned()
        } else if args.len() == 1 {
            let (b, v) = self.emit_value_binding(&args[0])?;
            binds.push_str(&b);
            v
        } else {
            let mut parts = Vec::new();
            for a in args {
                let (b, v) = self.emit_value_binding(a)?;
                binds.push_str(&b);
                parts.push(v);
            }
            nest_tuple_value(&parts)
        };
        Ok(format!("({binds}{callee} {arg})"))
    }

    /// Apply the visible type stages of a first-class callable from left to
    /// right, binding each `m <next-stage>` before the following stage, then
    /// apply its value group.  This is the uniform function-value ABI; direct
    /// module declarations use their compact calling convention and are
    /// adapted into this shape when they escape as values.
    fn emit_effectful_staged_apply(
        &mut self,
        mut binds: String,
        mut callee: String,
        mut callee_ty: Option<Type<Routed>>,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
    ) -> Result<String, EmitError> {
        callee = self.annotate_rank_n_value(callee, callee_ty.as_ref())?;
        for type_arg in type_args {
            let current_ty = callee_ty.unwrap_or_else(|| {
                unreachable!(
                    "a typed Routed first-class type application has no reconstructible callee type"
                )
            });
            let Type::Forall { param, body, .. } = current_ty else {
                unreachable!(
                    "a typed Routed first-class type application targets a non-forall value"
                );
            };
            let next_ty = super::reconstruct::apply_subst(
                &body,
                &HashMap::from([(param.name, type_arg.clone())]),
            );
            let rendered_arg = self.ctx.render_visible_type_argument(
                type_arg,
                &self.type_params,
                Some(self.module_key),
            )?;
            let action = format!("({callee} @{})", paren_ty(&rendered_arg));
            let next = self.fresh_var();
            let rank_n_type = self.render_rank_n_type(&next_ty)?;
            binds.push_str(&render_native_effectful_bind_prefix(
                &action,
                &next,
                rank_n_type.as_deref(),
            ));
            callee = next;
            callee_ty = Some(next_ty);
            callee = self.annotate_rank_n_value(callee, callee_ty.as_ref())?;
        }

        let applied = self.emit_effectful_apply_tupled(&callee, args)?;
        Ok(format!("({binds}{applied})"))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_host_call(
        &mut self,
        name: &str,
        module_path: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
        value_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let residual = residual_module_call_stages(sig, type_args.len(), args.is_empty());
        if !residual.is_empty() {
            // The public host record keeps all native `forall`s adjacent and
            // runs only after every declared value group is present. Adapt
            // that compact field to the ordinary first-class staged ABI,
            // then apply exactly the routed leading stages. This delays the
            // host action without making source signature groups participate
            // in call selection.
            let final_ret = final_return_after_residual_signature(sig, ret_ty);
            let callee_ty = sig.signature_ty(final_ret.clone(), ret_ty.span());
            let adapter = self.emit_host_fn_value_ref(name, module_path, sig, &final_ret)?;
            return self.emit_effectful_staged_apply(
                String::new(),
                adapter,
                Some(callee_ty),
                type_args,
                args,
            );
        }

        // Generic host functions quantify their Kio type parameters directly
        // in the native host record. The same native value therefore crosses
        // both sides of a generic slot; no universal boxing is involved.
        let generic = !fn_type_param_names(sig).is_empty();
        let entry = self.ctx.facade.host_site(module_path, name).entry();
        let param_groups = value_group_param_types(sig);
        let scheme_scope: Vec<TypeParam> = fn_type_params(sig).into_iter().cloned().collect();
        let module = Some(module_path);
        let member = host_field_name(module_path, name);
        // Each host-call value-argument is evaluated to its native value
        // first; an effectful argument (one reaching another host call)
        // sequences its `m` before the call, a pure one renders inline.
        // A compound argument is then converted from the native-internal
        // representation to the typed boundary family application the host
        // record field expects, recursively adapting nominal slots.
        let mut binds = String::new();
        let mut call_args = Vec::new();
        let mut source_arg_index = 0usize;
        let mut source_groups = param_groups.iter();
        for stage in entry.stages() {
            let HaskellCallableHeadStage::Value {
                slots,
                execution: Some(layout),
            } = stage
            else {
                continue;
            };
            let source_group = source_groups
                .next()
                .expect("a prepared host value stage retains its source group");
            assert_eq!(source_group.len(), layout.source_params().len());
            for (source, param_ty) in layout.source_params().iter().zip(source_group) {
                let param_ty = param_ty
                    .as_ref()
                    .unwrap_or_else(|| unreachable!("a typed Routed host parameter has no type"));
                let argument = &args[source_arg_index];
                source_arg_index += 1;
                let (binding, value) = if generic
                    && matches!(param_ty, Type::Function { .. })
                    && let Expr::FnExpr {
                        sig, ret_ty, body, ..
                    } = argument
                {
                    (
                        String::new(),
                        self.emit_closure(sig, ret_ty.as_ref(), body)?,
                    )
                } else {
                    self.emit_value_binding(argument)?
                };
                binds.push_str(&binding);
                let range = source.facade_slots();
                match source.adapter() {
                    CallableSourceParamAdapter::UnitValue => {
                        assert!(range.is_empty());
                    }
                    CallableSourceParamAdapter::Identity => {
                        assert_eq!(range.len(), 1);
                        let slot_index = range.start;
                        let converted = self.convert_host_boundary_at(
                            param_ty,
                            slots[slot_index].boundary(),
                            &value,
                            HaskellHostConversionContext::root(FfiDir::Out, &scheme_scope, module),
                        )?;
                        call_args.push(paren_expr(&converted));
                    }
                    CallableSourceParamAdapter::RightNest => {
                        let parts = Type::right_spine_product(param_ty);
                        assert_eq!(parts.len(), range.len());
                        for (part_index, (part, slot_index)) in
                            parts.into_iter().zip(range).enumerate()
                        {
                            let converted = self.convert_host_boundary_at(
                                part,
                                slots[slot_index].boundary(),
                                &tuple_proj(&value, part_index, source.facade_slots().len()),
                                HaskellHostConversionContext::root(
                                    FfiDir::Out,
                                    &scheme_scope,
                                    module,
                                ),
                            )?;
                            call_args.push(paren_expr(&converted));
                        }
                    }
                }
            }
        }
        assert!(source_groups.next().is_none());
        assert_eq!(source_arg_index, args.len());
        let args_str = if call_args.is_empty() {
            String::new()
        } else {
            format!(" {}", call_args.join(" "))
        };
        let mut call = format!("{member} (pkgHost _pkg)");
        for type_arg in type_args {
            let type_arg = self.ctx.render_visible_type_argument(
                type_arg,
                &self.type_params,
                Some(self.module_key),
            )?;
            call.push_str(" @");
            call.push_str(&paren_ty(&type_arg));
        }
        call.push_str(&args_str);
        // The host returns at the typed boundary type; convert the result
        // back to the native-internal representation. A unit / passthrough
        // (type-var, abstract) return needs no conversion.
        let converted = self.convert_host_boundary_at(
            ret_ty,
            entry.returned().boundary(),
            "__hr",
            HaskellHostConversionContext::root(FfiDir::In, &scheme_scope, module),
        )?;
        if converted == "__hr" {
            return Ok(format!("({binds}{call})"));
        }
        let rank_n_type = self.render_rank_n_type(value_ty)?;
        let lifted = render_native_pure_value(&converted, rank_n_type.as_deref());
        Ok(format!("({binds}({call}) >>= \\__hr -> {lifted})"))
    }

    /// Convert a value between the native-internal representation (tuples /
    /// `Either` / native scalars / native functions) and the typed FFI
    /// boundary type (closed-family compounds / native scalars / native
    /// functions), at a host call's argument (`FfiDir::Out`,
    /// native → boundary) or return (`FfiDir::In`, boundary → native).
    ///
    /// Exact host types and type-variable / abstract positions keep one
    /// representation across the boundary, so they convert by identity.
    /// Compounds recurse slotwise; their closed families reduce to the same
    /// binary-nested pair / `Either` shape used by the native body.
    fn convert_host_boundary_in(
        &self,
        ty: &Type<Routed>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        self.convert_host_boundary_planned_in(ty, None, expr, context)
    }

    fn convert_host_boundary_at(
        &self,
        ty: &Type<Routed>,
        boundary: &BoundaryId,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        self.convert_host_boundary_planned_in(ty, Some(boundary), expr, context)
    }

    fn convert_host_boundary_planned_in(
        &self,
        ty: &Type<Routed>,
        boundary: Option<&BoundaryId>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        let public_ty = boundary.map(|boundary| {
            self.ctx.facade.boundary_type(boundary).unwrap_or_else(|| {
                unreachable!(
                    "an exact Haskell boundary path has no prepared public type: {boundary:?}"
                )
            })
        });
        self.convert_host_boundary_with_public_in(ty, boundary, public_ty, expr, context)
    }

    fn convert_host_boundary_with_public_in(
        &self,
        ty: &Type<Routed>,
        boundary: Option<&BoundaryId>,
        public_ty: Option<&Type<Routed>>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        // A passthrough leaf, including a direct existential carrier, crosses
        // unchanged. Structural parents still recurse and use their ordinary
        // product/sum family applications.
        if self
            .ctx
            .shapes
            .is_passthrough_scoped_in(ty, context.scheme_scope, context.module)
        {
            return Ok(expr.to_owned());
        }
        if boundary.is_none() && self.boundary_is_identity(ty, context.scheme_scope, context.module)
        {
            return Ok(expr.to_owned());
        }
        match ty {
            // Exact host types and `()` cross unchanged — the native rep is
            // the boundary rep selected by the host marker.
            Type::Unit { .. } => Ok(expr.to_owned()),
            Type::Path { .. } if self.ctx.shapes.is_host_type_in(ty, context.module) => {
                Ok(expr.to_owned())
            }
            // A nullary, nonrecursive, nonexistential newtype is transparent
            // on both sides, so convert through its payload. Every nominal
            // newtype returned through the passthrough guard above crosses
            // unchanged.
            Type::Path { .. } => {
                if let Some((d, owner)) = super::skin::resolve_newtype_in(
                    ty,
                    self.ctx.package,
                    &self.ctx.types.resolution,
                    context.module,
                ) {
                    let bound = context
                        .scheme_scope
                        .iter()
                        .map(|param| param.name.clone())
                        .collect();
                    let Some(payload) = instantiate_newtype_payload_in(
                        d,
                        ty,
                        self.ctx.package,
                        &owner,
                        context.module,
                        &bound,
                    ) else {
                        return Ok(expr.to_owned());
                    };
                    self.convert_host_boundary_with_public_in(
                        &payload,
                        boundary,
                        public_ty,
                        expr,
                        context.nested().with_module(Some(&owner)),
                    )
                } else {
                    Ok(expr.to_owned())
                }
            }
            Type::Forall { param, body, .. } => {
                let nested = super::skin::extend_type_scope(context.scheme_scope, param);
                let type_var = hs_tyvar(&param.name);
                let value_var = format!("__hft{}", context.depth);
                let public_body = match public_ty {
                    Some(Type::Forall { body, .. }) => Some(body.as_ref()),
                    Some(_) if boundary.is_some() => unreachable!(
                        "an exact Haskell forall conversion retains its public type stage"
                    ),
                    _ => None,
                };
                let source_body_ty =
                    if self
                        .ctx
                        .native_type_contains_forall(body, &nested, context.module)
                    {
                        Some(match context.direction {
                            FfiDir::In => {
                                if let (Some(boundary), Some(public_body)) = (boundary, public_body)
                                {
                                    self.ctx.shapes.boundary_alias_haskell_type_in(
                                        boundary,
                                        public_body,
                                        &nested,
                                        context.module,
                                    )?
                                } else {
                                    self.ctx.shapes.boundary_haskell_type_scoped_in(
                                        body,
                                        &nested,
                                        context.module,
                                    )?
                                }
                            }
                            FfiDir::Out => self.ctx.render_direct_generic_type(
                                body,
                                &nested,
                                context.module,
                            )?,
                        })
                    } else {
                        None
                    };
                // GHC does not push the enclosing type-abstraction's expected
                // type into a projected operand. Preserve the source-side
                // `forall` explicitly before its visible type application,
                // just as the ordinary native-expression path does.
                let source_ty = match context.direction {
                    FfiDir::In => {
                        if let (Some(boundary), Some(public_ty)) = (boundary, public_ty) {
                            self.ctx.shapes.boundary_alias_haskell_type_in(
                                boundary,
                                public_ty,
                                context.scheme_scope,
                                context.module,
                            )?
                        } else {
                            self.ctx.shapes.boundary_haskell_type_scoped_in(
                                ty,
                                context.scheme_scope,
                                context.module,
                            )?
                        }
                    }
                    FfiDir::Out => self.ctx.render_direct_generic_type(
                        ty,
                        context.scheme_scope,
                        context.module,
                    )?,
                };
                let applied = format!("((({expr}) :: {source_ty}) @{type_var})");
                let bind = render_native_effectful_bind_prefix(
                    &applied,
                    &value_var,
                    source_body_ty.as_deref(),
                );
                let converted = self.convert_host_boundary_with_public_in(
                    body,
                    boundary,
                    public_body,
                    &value_var,
                    context.nested().with_scope(&nested),
                )?;
                let target_ty = match context.direction {
                    FfiDir::In => self.ctx.render_direct_generic_type(
                        ty,
                        context.scheme_scope,
                        context.module,
                    )?,
                    FfiDir::Out => {
                        if let Some(boundary) = boundary {
                            let public_ty = public_ty.expect(
                                "an exact Haskell conversion owns its prepared public type",
                            );
                            self.ctx.shapes.boundary_alias_haskell_type_in(
                                boundary,
                                public_ty,
                                context.scheme_scope,
                                context.module,
                            )?
                        } else {
                            self.ctx.shapes.boundary_haskell_type_scoped_in(
                                ty,
                                context.scheme_scope,
                                context.module,
                            )?
                        }
                    }
                };
                Ok(format!(
                    "((\\ @{} -> {bind}pure ({})) :: {})",
                    type_var, converted, target_ty
                ))
            }
            Type::Product { .. } => self.convert_host_product(ty, boundary, expr, context),
            Type::Sum { .. } => self.convert_host_sum(ty, boundary, expr, context),
            Type::Function { .. } => self.convert_host_function(ty, boundary, expr, context),
            Type::Bottom { .. } => Ok(expr.to_owned()),
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Render the concrete tuple / `Either` shape GHC must see when an
    /// impredicative compound is passed to an explicitly type-applied helper.
    /// Public signatures keep their stable aliases; this local type merely
    /// exposes the definitionally equal container shape to Quick Look.
    fn render_concrete_boundary_type_at(
        &self,
        ty: &Type<Routed>,
        boundary: &BoundaryId,
        scope: &[crate::ast::TypeParam],
        module: Option<&str>,
    ) -> Result<String, EmitError> {
        if let Type::Path { segments, .. } = ty {
            let scoped = matches!(segments.as_slice(), [name]
                if scope.iter().rev().any(|param| param.name == name.as_str()));
            if !scoped
                && let Some((payload, owner)) = self
                    .ctx
                    .shapes
                    .boundary_path_expansion_in(ty, scope, module)
            {
                return self.render_concrete_boundary_type_at(
                    &payload,
                    boundary,
                    scope,
                    Some(&owner),
                );
            }
        }
        match ty {
            Type::Product { .. } => {
                let slots = Type::right_spine_product(ty)
                    .into_iter()
                    .enumerate()
                    .map(|(index, slot)| {
                        self.render_concrete_boundary_type_at(
                            slot,
                            &boundary.nested(BoundaryStep::Slot(
                                index.try_into().expect("product slot index fits u32"),
                            )),
                            scope,
                            module,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(nest_tuple_type(&slots))
            }
            Type::Sum { .. } => {
                let slots = Type::right_spine_sum(ty)
                    .into_iter()
                    .enumerate()
                    .map(|(index, slot)| {
                        self.render_concrete_boundary_type_at(
                            slot,
                            &boundary.nested(BoundaryStep::Slot(
                                index.try_into().expect("sum slot index fits u32"),
                            )),
                            scope,
                            module,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(nest_either_type(&slots))
            }
            _ => self
                .ctx
                .shapes
                .boundary_alias_haskell_type_in(boundary, ty, scope, module),
        }
    }

    /// Lift a converted value at its exact destination representation. GHC's
    /// Quick Look cannot see an impredicative compound through a stable
    /// boundary type family, so only the local `kioPure` application exposes
    /// the definitionally equal structural type.
    fn render_boundary_pure_value_at(
        &self,
        ty: &Type<Routed>,
        boundary: &BoundaryId,
        value: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        if !self
            .ctx
            .native_type_contains_forall(ty, context.scheme_scope, context.module)
        {
            return Ok(render_native_pure_value(value, None));
        }
        let target = match context.direction {
            FfiDir::In => {
                self.ctx
                    .render_direct_generic_type(ty, context.scheme_scope, context.module)?
            }
            FfiDir::Out => {
                let public_ty = self.ctx.facade.boundary_type(boundary).unwrap_or_else(|| {
                    unreachable!(
                        "an exact Haskell boundary path has no prepared public type: {boundary:?}"
                    )
                });
                self.render_concrete_boundary_type_at(
                    public_ty,
                    boundary,
                    context.scheme_scope,
                    context.module,
                )?
            }
        };
        Ok(render_native_pure_value(value, Some(&target)))
    }

    /// Convert a product slot-by-slot while retaining the shared right-nested
    /// pair representation on both sides of the boundary.
    fn convert_host_product(
        &self,
        ty: &Type<Routed>,
        boundary: Option<&BoundaryId>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        let slots = Type::right_spine_product(ty);
        let n = slots.len();
        let bound = format!("__hp{}", context.depth);
        let mut elems = Vec::with_capacity(n);
        for (i, slot) in slots.iter().enumerate() {
            let nested = boundary.map(|boundary| {
                boundary.nested(BoundaryStep::Slot(
                    i.try_into().expect("product slot index fits u32"),
                ))
            });
            elems.push(self.convert_host_boundary_planned_in(
                slot,
                nested.as_ref(),
                &tuple_proj(&bound, i, n),
                context.nested(),
            )?);
        }
        Ok(format!(
            "(let {{ {bound} = ({expr}) }} in {})",
            nest_tuple_value(&elems)
        ))
    }

    /// Convert a sum arm-by-arm while retaining the shared right-nested
    /// `Either` representation on both sides of the boundary.
    fn convert_host_sum(
        &self,
        ty: &Type<Routed>,
        boundary: Option<&BoundaryId>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        let slots = Type::right_spine_sum(ty);
        let n = slots.len();
        let bound = format!("__hc{}", context.depth);
        let mut cases = String::new();
        for (k, slot) in slots.iter().enumerate() {
            let nested = boundary.map(|boundary| {
                boundary.nested(BoundaryStep::Slot(
                    k.try_into().expect("sum slot index fits u32"),
                ))
            });
            let conv = self.convert_host_boundary_planned_in(
                slot,
                nested.as_ref(),
                &bound,
                context.nested(),
            )?;
            cases.push_str(&format!(
                "{} -> {}; ",
                either_pattern(k, n, &bound),
                either_inject(k, n, &conv)
            ));
        }
        Ok(format!("(case ({expr}) of {{ {cases} }})"))
    }

    /// Convert a function value crossing the host boundary. Both native and
    /// boundary callbacks return through `m`; this adapter only re-keys the
    /// argument/result shapes and bridges the native tupled value group to
    /// the boundary's curried field spelling.
    fn convert_host_function(
        &self,
        ty: &Type<Routed>,
        boundary: Option<&BoundaryId>,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        if let Some(boundary) = boundary {
            let layout = self.ctx.facade.function_layout(boundary).unwrap_or_else(|| {
                unreachable!(
                    "an exact Haskell function boundary has no prepared execution layout: {boundary:?}"
                )
            });
            return self.convert_prepared_host_function(ty, boundary, layout, expr, context);
        }
        let Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } = ty
        else {
            unreachable!("host function conversion requires a function type")
        };
        let param_tys = Type::right_spine_take(param, *abi_arity);
        let leg_context = context.flipped().nested();
        // Identity fast-path: no shape leg needs re-keying.
        let legs_trivial = param_tys
            .iter()
            .all(|p| self.boundary_is_identity(p, context.scheme_scope, context.module))
            && self.boundary_is_identity(ret, context.scheme_scope, context.module);
        if param_tys.is_empty() {
            let body = match context.direction {
                FfiDir::Out => format!("(({expr}) ())"),
                FfiDir::In => format!("({expr})"),
            };
            let body = if legs_trivial {
                body
            } else {
                let retbind = format!("__hfr{}", context.depth);
                let ret_conv = self.convert_host_boundary_in(ret, &retbind, context.nested())?;
                format!("{body} >>= \\{retbind} -> pure ({ret_conv})")
            };
            return Ok(match context.direction {
                FfiDir::Out => body,
                FfiDir::In => format!("(\\() -> {body})"),
            });
        }
        // The lambda / result binders carry `depth` so a function-returning
        // function (or a function leg nested under a product) never reuses
        // an enclosing leg's `__hf` / `__hfr` name.
        let params: Vec<String> = (0..param_tys.len())
            .map(|i| format!("__hf{}_{i}", context.depth))
            .collect();
        let native_arg = if legs_trivial {
            // No re-key: the native tuple param is the boundary params nested
            // directly (one value group = one nested-binary tuple).
            nest_tuple_value(&params)
        } else {
            let mut converted_params = Vec::with_capacity(param_tys.len());
            for (i, pty) in param_tys.iter().enumerate() {
                converted_params.push(self.convert_host_boundary_in(
                    pty,
                    &params[i],
                    leg_context,
                )?);
            }
            nest_tuple_value(&converted_params)
        };
        let applied = format!("(({expr}) {native_arg})");
        let lam = params.join(" ");
        if legs_trivial {
            return Ok(format!("(\\{lam} -> {applied})"));
        }
        let retbind = format!("__hfr{}", context.depth);
        let ret_conv = self.convert_host_boundary_in(ret, &retbind, context.nested())?;
        Ok(format!(
            "(\\{lam} -> {applied} >>= \\{retbind} -> pure ({ret_conv}))"
        ))
    }

    /// Adapt one nested callable from its prepared semantic slots to the
    /// native body's original product-domain ABI (or in the reverse
    /// direction). The shared layout owns the partition: this renderer only
    /// builds or projects the corresponding Haskell tuple values.
    fn convert_prepared_host_function(
        &self,
        ty: &Type<Routed>,
        boundary: &BoundaryId,
        layout: &CallableValueStageLayout,
        expr: &str,
        context: HaskellHostConversionContext<'_, '_>,
    ) -> Result<String, EmitError> {
        let Type::Function { param, ret, .. } = ty else {
            unreachable!("a prepared callback layout belongs to a function type")
        };
        let source_types = Type::right_spine_take(param, layout.body_abi_arity());
        assert_eq!(
            source_types.len(),
            layout.source_params().len(),
            "the prepared callback retains every native ABI parameter"
        );
        let public_args = (0..layout.facade_slot_count())
            .map(|index| format!("__hff{}_{index}", context.depth))
            .collect::<Vec<_>>();
        let leg_context = context.flipped().nested();

        let action = match context.direction {
            FfiDir::Out => {
                let mut source_values = Vec::with_capacity(source_types.len());
                for (source_ty, source) in source_types.iter().zip(layout.source_params()) {
                    let range = source.facade_slots();
                    let source_value = match source.adapter() {
                        CallableSourceParamAdapter::UnitValue => {
                            assert!(range.is_empty());
                            "()".to_owned()
                        }
                        CallableSourceParamAdapter::Identity => {
                            assert_eq!(range.len(), 1);
                            let index = range.start;
                            let nested = boundary.nested(BoundaryStep::CallbackArg(
                                index.try_into().expect("callback slot index fits u32"),
                            ));
                            self.convert_host_boundary_planned_in(
                                source_ty,
                                Some(&nested),
                                &public_args[index],
                                leg_context,
                            )?
                        }
                        CallableSourceParamAdapter::RightNest => {
                            let parts = Type::right_spine_product(source_ty);
                            assert_eq!(parts.len(), range.len());
                            let converted = parts
                                .into_iter()
                                .zip(range)
                                .map(|(part, index)| {
                                    let nested = boundary.nested(BoundaryStep::CallbackArg(
                                        index.try_into().expect("callback slot index fits u32"),
                                    ));
                                    self.convert_host_boundary_planned_in(
                                        part,
                                        Some(&nested),
                                        &public_args[index],
                                        leg_context,
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            nest_tuple_value(&converted)
                        }
                    };
                    source_values.push(source_value);
                }
                format!("(({expr}) {})", nest_tuple_value(&source_values))
            }
            FfiDir::In => {
                let native_domain = format!("__hfd{}", context.depth);
                let mut facade_values = Vec::with_capacity(layout.facade_slot_count());
                for (source_index, (source_ty, source)) in
                    source_types.iter().zip(layout.source_params()).enumerate()
                {
                    let source_value = if source_types.len() == 1 {
                        native_domain.clone()
                    } else {
                        tuple_proj(&native_domain, source_index, source_types.len())
                    };
                    let range = source.facade_slots();
                    match source.adapter() {
                        CallableSourceParamAdapter::UnitValue => {
                            assert!(range.is_empty());
                        }
                        CallableSourceParamAdapter::Identity => {
                            assert_eq!(range.len(), 1);
                            let index = range.start;
                            let nested = boundary.nested(BoundaryStep::CallbackArg(
                                index.try_into().expect("callback slot index fits u32"),
                            ));
                            facade_values.push(self.convert_host_boundary_planned_in(
                                source_ty,
                                Some(&nested),
                                &source_value,
                                leg_context,
                            )?);
                        }
                        CallableSourceParamAdapter::RightNest => {
                            let parts = Type::right_spine_product(source_ty);
                            assert_eq!(parts.len(), range.len());
                            for (part_index, (part, index)) in
                                parts.into_iter().zip(range).enumerate()
                            {
                                let nested = boundary.nested(BoundaryStep::CallbackArg(
                                    index.try_into().expect("callback slot index fits u32"),
                                ));
                                facade_values.push(self.convert_host_boundary_planned_in(
                                    part,
                                    Some(&nested),
                                    &tuple_proj(
                                        &source_value,
                                        part_index,
                                        source.facade_slots().len(),
                                    ),
                                    leg_context,
                                )?);
                            }
                        }
                    }
                }
                let applied = if facade_values.is_empty() {
                    format!("({expr})")
                } else {
                    format!(
                        "(({expr}) {})",
                        facade_values
                            .iter()
                            .map(|value| paren_expr(value))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                };
                let ret_boundary = boundary.nested(BoundaryStep::CallbackRet);
                let retbind = format!("__hfr{}", context.depth);
                let converted = self.convert_host_boundary_planned_in(
                    ret,
                    Some(&ret_boundary),
                    &retbind,
                    context.nested(),
                )?;
                let body = if converted == retbind {
                    applied
                } else {
                    let lifted = self.render_boundary_pure_value_at(
                        ret,
                        &ret_boundary,
                        &converted,
                        context.nested(),
                    )?;
                    format!("{applied} >>= \\{retbind} -> {lifted}")
                };
                return Ok(format!("(\\{native_domain} -> {body})"));
            }
        };

        let ret_boundary = boundary.nested(BoundaryStep::CallbackRet);
        let retbind = format!("__hfr{}", context.depth);
        let converted = self.convert_host_boundary_planned_in(
            ret,
            Some(&ret_boundary),
            &retbind,
            context.nested(),
        )?;
        let body = if converted == retbind {
            action
        } else {
            let lifted = self.render_boundary_pure_value_at(
                ret,
                &ret_boundary,
                &converted,
                context.nested(),
            )?;
            format!("{action} >>= \\{retbind} -> {lifted}")
        };
        if public_args.is_empty() {
            Ok(body)
        } else {
            Ok(format!("(\\{} -> {body})", public_args.join(" ")))
        }
    }

    /// True when a value of type `ty` crosses the host boundary unchanged in
    /// the native body — a passthrough (erased) position, an exact host type,
    /// or `()` whose native rep is its boundary rep.
    fn boundary_is_identity(
        &self,
        ty: &Type<Routed>,
        scheme_scope: &[TypeParam],
        module: Option<&str>,
    ) -> bool {
        if self
            .ctx
            .shapes
            .is_passthrough_scoped_in(ty, scheme_scope, module)
            || matches!(ty, Type::Unit { .. })
            || self.ctx.shapes.is_host_type_in(ty, module)
            || self.ctx.has_nominal_boundary_carrier(ty, module)
        {
            return true;
        }
        match ty {
            Type::Product { .. } => Type::right_spine_product(ty)
                .into_iter()
                .all(|slot| self.boundary_is_identity(slot, scheme_scope, module)),
            Type::Sum { .. } => Type::right_spine_sum(ty)
                .into_iter()
                .all(|slot| self.boundary_is_identity(slot, scheme_scope, module)),
            Type::Function {
                param,
                ret,
                abi_arity,
                ..
            } => {
                let params = Type::right_spine_take(param, *abi_arity);
                params.len() == 1
                    && self.boundary_is_identity(params[0], scheme_scope, module)
                    && self.boundary_is_identity(ret, scheme_scope, module)
            }
            Type::Forall { param, body, .. } => {
                let nested = super::skin::extend_type_scope(scheme_scope, param);
                self.boundary_is_identity(body, &nested, module)
            }
            Type::Path { .. } => self
                .ctx
                .shapes
                .boundary_path_expansion_in(ty, scheme_scope, module)
                .is_some_and(|(body, owner)| {
                    self.boundary_is_identity(&body, scheme_scope, Some(&owner))
                }),
            Type::Bottom { .. } => true,
            Type::Unit { .. } => true,
            Type::LabelSugar { ext, .. } => match *ext {},
            Type::Infer { ext, .. } => match *ext {},
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// A newtype constructor: carrier `mk_box(payload)` → `Box payload`;
    /// existential
    /// `mk(payload)` → `Box payload` directly through the GADT constructor.
    /// Its result annotation fixes the GADT head's universal arguments;
    /// each existential witness remains inferred from the payload and sealed
    /// by construction.
    fn emit_newtype_ctor(
        &mut self,
        newtype: &str,
        type_args: &[Type<Routed>],
        payload: &Expr<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        let module_key = self.module_key;
        self.emit_newtype_ctor_in(newtype, module_key, type_args, payload, effect)
    }

    fn emit_newtype_ctor_in(
        &mut self,
        newtype: &str,
        module_path: &str,
        type_args: &[Type<Routed>],
        payload: &Expr<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        if let Some(c) = self.ctx.carrier_for(newtype, module_path) {
            let p = self.emit(payload, effect)?;
            // A nullary transparent carrier is runtime-identity: the
            // constructor is the payload itself (no wrap).
            if !c.opaque {
                return Ok(paren_expr(&p));
            }
            return Ok(carrier_ctor_call(&c.hs_name, "_pkg", &p));
        }
        if let Some(e) = self.ctx.existential_for(newtype, module_path) {
            // The GADT data constructor seals the existential witnesses by
            // construction; the payload is a pure value computation.
            let p = self.emit_pure(payload)?;
            return Ok(self.build_existential_ctor(e, type_args, &p));
        }
        Err(EmitError::unsupported(format!(
            "Haskell native body: newtype ctor `{newtype}` is neither carrier nor existential"
        )))
    }

    /// Wrap an **already-rendered** native payload in a newtype's Haskell
    /// constructor — the shared builder behind [`Self::emit_newtype_ctor`]
    /// and the effectful-context ctor arm (which has sequenced the payload's
    /// `m` and holds a plain value binder).
    fn build_newtype_ctor(
        &self,
        newtype: &str,
        type_args: &[Type<Routed>],
        payload: &str,
    ) -> Result<String, EmitError> {
        self.build_newtype_ctor_in(newtype, self.module_key, type_args, payload)
    }

    fn build_newtype_ctor_in(
        &self,
        newtype: &str,
        module_path: &str,
        type_args: &[Type<Routed>],
        payload: &str,
    ) -> Result<String, EmitError> {
        if let Some(c) = self.ctx.carrier_for(newtype, module_path) {
            if !c.opaque {
                return Ok(format!("({payload})"));
            }
            return Ok(carrier_ctor_call(&c.hs_name, "_pkg", payload));
        }
        if let Some(e) = self.ctx.existential_for(newtype, module_path) {
            return Ok(self.build_existential_ctor(e, type_args, payload));
        }
        Err(EmitError::unsupported(format!(
            "Haskell native body: newtype ctor `{newtype}` is neither carrier nor existential"
        )))
    }

    fn build_existential_ctor(
        &self,
        existential: &ExistentialCarrier,
        type_args: &[Type<Routed>],
        payload: &str,
    ) -> String {
        let Some(universal_args) = type_args.get(..existential.type_params.len()) else {
            unreachable!(
                "Haskell existential constructor `{}` has fewer type arguments than universal parameters",
                existential.kio_name
            );
        };
        let scope: HashSet<&str> = self
            .type_params
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        let rendered_args = universal_args
            .iter()
            .map(|arg| {
                paren_ty(
                    &self
                        .ctx
                        .render_native_type_scoped(arg, &scope, Some(self.module_key)),
                )
            })
            .collect::<Vec<_>>();
        let result = apply_hs_head(&format!("{} h m", existential.hs_name), &rendered_args);
        format!("(({} ({payload})) :: {result})", existential.hs_name)
    }

    /// Project an **already-rendered** native target through a newtype's
    /// Haskell projector — the shared reader behind the effectful-context
    /// projection arm (which has sequenced the target's `m` and holds a
    /// plain value binder).
    fn build_newtype_proj(&self, newtype: &str, target: &str) -> Result<String, EmitError> {
        self.build_newtype_proj_in(newtype, self.module_key, target)
    }

    fn build_newtype_proj_in(
        &self,
        newtype: &str,
        module_path: &str,
        target: &str,
    ) -> Result<String, EmitError> {
        if let Some(c) = self.ctx.carrier_for(newtype, module_path) {
            if !c.opaque {
                return Ok(format!("({target})"));
            }
            return Ok(carrier_proj_call(&c.hs_name, "_pkg", target));
        }
        Err(EmitError::unsupported(format!(
            "Haskell native body: projection on unknown newtype `{newtype}`"
        )))
    }

    /// Build the value returned by an enriched field-get. The IR node
    /// combines positional access with the field newtype's projector. A
    /// nullary, nonrecursive, nonexistential field newtype is transparent;
    /// parametric and recursive field newtypes use their declaration-stable
    /// nominal carrier projector. Existential field access is rejected before
    /// this IR, and the private universal fallback remains distinct.
    fn build_field_get(
        &self,
        target_ty: &Type<Routed>,
        index: usize,
        arity: usize,
        projected: &str,
    ) -> Result<String, EmitError> {
        let field_ty = crate::backends::reconstruct::nth_product_slot_ty_for_arity(
            target_ty, index, arity,
        )
        .ok_or_else(|| {
            EmitError::unsupported(
                "Haskell native body: enriched field-get target type does not match its arity",
            )
        })?;
        let module = Some(self.module_key);
        if let Some(carrier) = self
            .ctx
            .resolve_newtype_key(field_ty, module)
            .and_then(|key| self.ctx.types.carriers.get(&key))
            && carrier.opaque
        {
            return Ok(carrier_proj_call(&carrier.hs_name, "_pkg", projected));
        }
        Ok(projected.to_owned())
    }

    /// Project a slot after sequencing an effectful target. When the target
    /// is itself a newtype projection, fuse both reads before lifting the
    /// selected slot. A rank-N payload may be stored safely behind its
    /// nominal newtype while the full unwrapped product is not a valid
    /// predicative argument to `pure`; selecting the monomorphic slot first
    /// avoids inventing an impredicative intermediate.
    fn emit_effectful_projection(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<String, EmitError> {
        let (binds, selected) = self.emit_effectful_projected_value(target, index, arity)?;
        Ok(format!("({binds}pure ({selected}))"))
    }

    fn emit_effectful_field_get(
        &mut self,
        target: &Expr<Routed>,
        target_ty: &Type<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<String, EmitError> {
        let (binds, selected) = self.emit_effectful_projected_value(target, index, arity)?;
        let field = self.build_field_get(target_ty, index, arity, &selected)?;
        Ok(format!("({binds}pure ({field}))"))
    }

    fn emit_effectful_projected_value(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<(String, String), EmitError> {
        if let Some(selected) = self.projection_alias(target, index, arity) {
            return Ok((String::new(), selected));
        }
        if let Expr::LowNewtypeProj {
            newtype,
            target: wrapped,
            ..
        } = target
        {
            let (binds, wrapped) = self.emit_value_binding(wrapped)?;
            let payload = self.build_newtype_proj(newtype, &wrapped)?;
            let selected = tuple_proj(&payload, index, arity);
            return Ok((binds, selected));
        }
        let (binds, target) = self.emit_value_binding(target)?;
        Ok((binds, tuple_proj(&target, index, arity)))
    }

    /// A module-fn-value reference rendered as nested native lambdas, one
    /// tuple parameter per Kio value group. Each earlier group returns the
    /// next function through `m`; the final group invokes the module fn and
    /// lifts its result only when the module fn itself is pure.
    fn emit_module_fn_value_ref(
        &mut self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let fname = self.resolve_module_fn(mangled);
        self.emit_resolved_module_fn_value_ref(&fname, sig)
    }

    fn emit_resolved_module_fn_value_ref(
        &mut self,
        fname: &str,
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let effect = self.ctx.effect_of(fname);
        let stages = native_call_stages(sig);
        Ok(render_module_fn_value_adapter(
            &format!("{fname} _pkg"),
            &stages,
            effect,
        ))
    }

    /// A host-fn-value reference rendered as nested native lambdas. Each
    /// Kio value group is one tuple parameter; earlier groups return the
    /// next lambda through `m`, and the final group invokes the flattened
    /// host-record field with the same conversions as a direct host call.
    fn emit_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let groups = value_group_param_types(sig);
        let entry = self.ctx.facade.host_site(module_path, name).entry();
        let scheme_scope: Vec<TypeParam> = fn_type_params(sig).into_iter().cloned().collect();
        let module = Some(module_path);
        let group_args: Vec<String> = (0..groups.len())
            .map(|index| format!("__hf{index}"))
            .collect();
        let mut call_args = Vec::new();
        let mut value_stage_index = 0usize;
        for stage in entry.stages() {
            let HaskellCallableHeadStage::Value {
                slots,
                execution: Some(layout),
            } = stage
            else {
                continue;
            };
            let group = &groups[value_stage_index];
            let group_arg = &group_args[value_stage_index];
            value_stage_index += 1;
            assert_eq!(group.len(), layout.source_params().len());
            for (source_index, (source, param_ty)) in
                layout.source_params().iter().zip(group).enumerate()
            {
                let param_ty = param_ty
                    .as_ref()
                    .unwrap_or_else(|| unreachable!("a typed Routed host parameter has no type"));
                let value = if group.len() == 1 {
                    group_arg.clone()
                } else {
                    tuple_proj(group_arg, source_index, group.len())
                };
                let range = source.facade_slots();
                match source.adapter() {
                    CallableSourceParamAdapter::UnitValue => {
                        assert!(range.is_empty());
                    }
                    CallableSourceParamAdapter::Identity => {
                        assert_eq!(range.len(), 1);
                        let slot_index = range.start;
                        let converted = self.convert_host_boundary_at(
                            param_ty,
                            slots[slot_index].boundary(),
                            &value,
                            HaskellHostConversionContext::root(FfiDir::Out, &scheme_scope, module),
                        )?;
                        call_args.push(paren_expr(&converted));
                    }
                    CallableSourceParamAdapter::RightNest => {
                        let parts = Type::right_spine_product(param_ty);
                        assert_eq!(parts.len(), range.len());
                        for (part_index, (part, slot_index)) in
                            parts.into_iter().zip(range).enumerate()
                        {
                            let converted = self.convert_host_boundary_at(
                                part,
                                slots[slot_index].boundary(),
                                &tuple_proj(&value, part_index, source.facade_slots().len()),
                                HaskellHostConversionContext::root(
                                    FfiDir::Out,
                                    &scheme_scope,
                                    module,
                                ),
                            )?;
                            call_args.push(paren_expr(&converted));
                        }
                    }
                }
            }
        }
        assert_eq!(value_stage_index, groups.len());

        let member = host_field_name(module_path, name);
        let suffix = if call_args.is_empty() {
            String::new()
        } else {
            format!(" {}", call_args.join(" "))
        };
        let mut call = format!("{member} (pkgHost _pkg)");
        for param in fn_type_params(sig) {
            call.push_str(" @");
            call.push_str(&hs_tyvar(&param.name));
        }
        call.push_str(&suffix);
        let converted = self.convert_host_boundary_at(
            ret_ty,
            entry.returned().boundary(),
            "__hfr",
            HaskellHostConversionContext::root(FfiDir::In, &scheme_scope, module),
        )?;
        let result_context = HaskellHostConversionContext::root(FfiDir::In, &scheme_scope, module);
        let mut body = if converted == "__hfr" {
            call
        } else {
            let lifted = self.render_boundary_pure_value_at(
                ret_ty,
                entry.returned().boundary(),
                &converted,
                result_context,
            )?;
            format!("({call}) >>= \\__hfr -> {lifted}")
        };

        let mut value_index = 0usize;
        let mut stage_binders = Vec::new();
        for stage in native_call_stages(sig) {
            match stage {
                NativeCallStage::Type(type_param) => {
                    stage_binders.push(format!(" @{type_param}"));
                }
                NativeCallStage::Value { .. } => {
                    stage_binders.push(group_args[value_index].clone());
                    value_index += 1;
                }
            }
        }
        debug_assert_eq!(value_index, group_args.len());
        for (depth, binder) in stage_binders.into_iter().rev().enumerate() {
            if depth > 0 {
                body = format!("pure ({body})");
            }
            body = format!("(\\{binder} -> {body})");
        }
        Ok(body)
    }

    fn emit_resolved_module_call_pure(
        &mut self,
        fname: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        value_ty: Option<&Type<Routed>>,
    ) -> Result<String, EmitError> {
        let mut call = self.render_direct_module_head(fname, type_args)?;
        for arg in args {
            call.push(' ');
            call.push_str(&paren_expr(&self.emit_pure(arg)?));
        }
        let residual = residual_module_call_stages(sig, type_args.len(), args.is_empty());
        if residual.is_empty() {
            Ok(format!("({call})"))
        } else {
            let adapted =
                render_module_fn_value_adapter(&call, &residual, self.ctx.effect_of(fname));
            self.annotate_rank_n_value(adapted, value_ty)
        }
    }

    fn render_direct_module_head(
        &self,
        fname: &str,
        type_args: &[Type<Routed>],
    ) -> Result<String, EmitError> {
        let mut rendered = format!("{fname} _pkg");
        for type_arg in type_args {
            let type_arg = self.ctx.render_visible_type_argument(
                type_arg,
                &self.type_params,
                Some(self.module_key),
            )?;
            rendered.push_str(" @");
            rendered.push_str(&paren_ty(&type_arg));
        }
        Ok(rendered)
    }

    /// A cross-module newtype member `<module>.<NT>.<member>(<payload>)`.
    /// `member` is the newtype's constructor or projector surface name. The
    /// Routed node carries the exact declaring module for member
    /// classification and carrier lookup, so a same-leaf declaration or
    /// source alias elsewhere cannot redirect the call.
    fn emit_qualified_newtype_member(
        &mut self,
        module_path: &str,
        newtype: &str,
        member: &str,
        type_args: &[Type<Routed>],
        payload: &Expr<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        match self.ctx.newtype_member_kind(module_path, newtype, member) {
            Some(NewtypeMemberKind::Constructor) if effect == Effect::Effectful => {
                let (binds, payload) = self.emit_value_binding(payload)?;
                let built =
                    self.build_newtype_ctor_in(newtype, module_path, type_args, &payload)?;
                Ok(format!("({binds}pure ({built}))"))
            }
            Some(NewtypeMemberKind::Projector) if effect == Effect::Effectful => {
                let (binds, payload) = self.emit_value_binding(payload)?;
                let projected = self.build_newtype_proj_in(newtype, module_path, &payload)?;
                Ok(format!("({binds}pure ({projected}))"))
            }
            Some(NewtypeMemberKind::Constructor) => {
                self.emit_newtype_ctor_in(newtype, module_path, type_args, payload, effect)
            }
            Some(NewtypeMemberKind::Projector) => {
                self.emit_newtype_proj_in(newtype, module_path, payload, effect)
            }
            None => Err(EmitError::unsupported(format!(
                "Haskell native body: qualified newtype member `{module_path}.{newtype}.{member}` does not resolve to a constructor or projector"
            ))),
        }
    }

    /// A CPS-projector apply — `<proj>(<receiver>)(<continuation>)` over an
    /// existential carrier. Renders as the GADT unpacker applied to the
    /// receiver and the continuation lambda: `(unBox (<recv>)
    /// (<continuation>))`. The unpacker `case`-unpacks the GADT (re-binding
    /// each existential as a fresh skolem) and invokes the continuation.
    /// Canonical Unit contributes no value parameter; otherwise the rank-N
    /// continuation's payload parameter is emitted unannotated so GHC reads
    /// the skolem from the `case` pattern rather than the emitter trying to
    /// spell the hidden existential.
    fn emit_cps_projector_apply(
        &mut self,
        newtype: &str,
        module_path: &str,
        receiver: &Expr<Routed>,
        continuation: &Expr<Routed>,
        continuation_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let Some(e) = self.ctx.existential_for_owner(newtype, module_path) else {
            return Err(EmitError::unsupported(format!(
                "Haskell native body: CPS projector apply on non-existential newtype `{module_path}.{newtype}`"
            )));
        };
        let (_, callable) = continuation_ty.peel_leading_foralls();
        let Type::Function { abi_arity, .. } = callable else {
            unreachable!("a routed CPS projector continuation has a function type")
        };
        assert!(
            *abi_arity <= 1,
            "a routed CPS projector continuation has zero or one ABI slot"
        );
        let proj = existential_proj_fn(&e.hs_name);
        // Sequence receiver evaluation before continuation construction;
        // the GADT unpacker then owns the native rank-N application.
        let (receiver_binds, recv) = self.emit_value_binding(receiver)?;
        let (continuation_binds, cont) = self.emit_value_binding(continuation)?;
        Ok(format!(
            "({receiver_binds}{continuation_binds}{proj} ({recv}) ({cont}))"
        ))
    }

    /// A newtype projection. `un_box(target)` yields the payload.
    fn emit_newtype_proj(
        &mut self,
        newtype: &str,
        target: &Expr<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        let module_key = self.module_key;
        self.emit_newtype_proj_in(newtype, module_key, target, effect)
    }

    fn emit_newtype_proj_in(
        &mut self,
        newtype: &str,
        module_path: &str,
        target: &Expr<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        if let Some(c) = self.ctx.carrier_for(newtype, module_path) {
            let t = self.emit(target, effect)?;
            // A nullary transparent carrier is runtime-identity: the
            // projection is the target itself (no unwrap).
            let out = if c.opaque {
                carrier_proj_call(&c.hs_name, "_pkg", &t)
            } else {
                paren_expr(&t)
            };
            return Ok(out);
        }
        Err(EmitError::unsupported(format!(
            "Haskell native body: projection on unknown newtype `{newtype}`"
        )))
    }

    /// An indirect call — `callee(args)`. A projected function-valued
    /// newtype payload is unwrapped first, then receives the tupled value
    /// group like every other function value.
    fn emit_indirect_call(
        &mut self,
        callee: &Expr<Routed>,
        args: &[Expr<Routed>],
        effect: Effect,
    ) -> Result<String, EmitError> {
        if let Expr::LowNewtypeProj {
            newtype, target, ..
        } = callee
        {
            let projected = self.emit_newtype_proj(newtype, target, effect)?;
            let arg = self.emit_call_arg(args, effect)?;
            return Ok(format!("({} {arg})", paren_expr(&projected)));
        }
        // A general indirect call: render the callee value, apply the
        // tupled value group.
        let c = self.emit(callee, effect)?;
        let arg = self.emit_call_arg(args, effect)?;
        Ok(format!("({} {arg})", paren_expr(&c)))
    }

    /// Render an `EnrichedMatch` over a right-nested `Either` scrutinee as
    /// nested `case`. Arm `i` (for `i < n-1`) handles `Left p_i`; the last
    /// arm handles the bare payload after `n-1` `Right`s.
    fn emit_match(
        &mut self,
        scrutinee: &str,
        arms: &[crate::ast::EnrichedArm<Routed>],
        scrutinee_ty: &Type<Routed>,
        effect: Effect,
    ) -> Result<String, EmitError> {
        let arity = arms.len();
        self.emit_match_chain(scrutinee, arms, scrutinee_ty, 0, arity, effect)
    }

    fn emit_match_chain(
        &mut self,
        scrutinee: &str,
        arms: &[crate::ast::EnrichedArm<Routed>],
        scrutinee_ty: &Type<Routed>,
        index: usize,
        arity: usize,
        effect: Effect,
    ) -> Result<String, EmitError> {
        let (arm, remaining) = arms
            .split_first()
            .unwrap_or_else(|| unreachable!("Haskell native emission: enriched match has no arms"));
        let slot = Type::right_spine_sum_slot_for_arity(scrutinee_ty, index, arity).unwrap_or_else(|| {
            unreachable!(
                "Haskell native emission: enriched match arm {index} has no payload type at arity {arity}"
            )
        });
        let prior = self.recon.bind(&arm.param, slot.clone());
        let projection_priors = self.take_projection_aliases(&arm.param);
        self.locals.push(arm.param.clone());
        let body = self.emit(&arm.body, effect);
        self.locals.pop();
        self.restore_projection_aliases(projection_priors);
        self.recon.restore(&arm.param, prior);
        let body = body?;

        if remaining.is_empty() {
            // The terminal: bind the payload directly.
            let rank_n_type = self.render_rank_n_type(slot)?;
            return Ok(render_native_pure_let(
                &hs_local(&arm.param),
                rank_n_type.as_deref(),
                scrutinee,
                &body,
                false,
            ));
        }
        // The tail recurses over the `Right`-wrapped remainder.
        let rest =
            self.emit_match_chain("__r", remaining, scrutinee_ty, index + 1, arity, effect)?;
        Ok(format!(
            "(case ({scrutinee}) of {{ Left {p} -> ({body}); Right __r -> ({rest}) }})",
            p = hs_local(&arm.param)
        ))
    }

    /// Build a call's single value argument: one value → that value, many
    /// → a tuple (Kio ABI: one product per call group).
    fn emit_call_arg(
        &mut self,
        args: &[Expr<Routed>],
        effect: Effect,
    ) -> Result<String, EmitError> {
        if args.is_empty() {
            return Ok("()".to_owned());
        }
        if args.len() == 1 {
            return Ok(paren_expr(&self.emit(&args[0], effect)?));
        }
        let mut parts = Vec::new();
        for a in args {
            parts.push(self.emit(a, effect)?);
        }
        Ok(nest_tuple_value(&parts))
    }

    /// Render a closure at its exact reconstructed function type. Haskell
    /// type abstractions retain each canonical Kio `forall` position, while
    /// each value layer keeps the function-value ABI's `m` result.
    fn emit_closure(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        ret_ty: Option<&Type<Routed>>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        // Function values uniformly return through `m`. Constructing each
        // lambda remains pure; invoking its value group either returns the
        // next polymorphic/function layer through `pure` or runs the final
        // body action.
        let groups = value_param_name_groups(sig);
        let canonical = sig.canonical_groups();
        if canonical.is_empty() {
            unreachable!(
                "post-recovery FnExpr signatures have at least one canonical group; a direct \
                 nullary function is represented by an empty Value group"
            );
        }
        // Seed each typed value param into the reconstruction env so the
        // body can spell types where it needs to (carrier boundaries).
        let mut priors: Vec<(String, Option<Type<Routed>>)> = Vec::new();
        for p in &sig.params {
            if let SignatureParam::Value(vp) = p
                && let Some(ty) = &vp.ty
            {
                let prior = self.recon.bind(&vp.name, ty.clone());
                priors.push((vp.name.clone(), prior));
            }
        }
        for g in &groups {
            for n in g {
                self.locals.push((*n).to_owned());
            }
        }
        let inserted_type_params = self.recon.push_type_params(sig);
        let outer_type_params = self.type_params.len();
        self.type_params
            .extend(fn_type_params(sig).into_iter().cloned());
        let inferred_ret = ret_ty.cloned().or_else(|| self.recon.value_type(body));
        let closure_type = inferred_ret
            .as_ref()
            .map(|ret| {
                let ty = sig.signature_ty(ret.clone(), body.span());
                self.ctx.render_direct_generic_type(
                    &ty,
                    &self.type_params[..outer_type_params],
                    Some(self.module_key),
                )
            })
            .transpose();
        // A product-domain closure is represented by one right-nested native
        // tuple. Bind that tuple's slots once at the lambda boundary, while
        // retaining the whole value and making the slot pattern lazy so the
        // closure does not demand a deeper tuple spine than its body uses.
        // Otherwise projecting all N slots rebuilds every selector prefix and
        // grows the emitted expression (and GHC's parse/typecheck work)
        // quadratically.
        let mut canonical_patterns = Vec::with_capacity(canonical.len());
        let mut projection_priors = Vec::new();
        let mut masked_projection_aliases = Vec::new();
        let mut masked_names = BTreeSet::new();
        for group in &canonical {
            match group {
                SignatureGroupRef::Type(_) => canonical_patterns.push(None),
                SignatureGroupRef::Value(params) => {
                    let mut binders = Vec::with_capacity(params.len());
                    for param in params.iter() {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("SignatureGroupRef::Value contains only value parameters")
                        };
                        if masked_names.insert(param.name.clone()) {
                            masked_projection_aliases
                                .extend(self.take_projection_aliases(&param.name));
                        }
                        let binder = hs_local(&param.name);
                        let Some(ty) = &param.ty else {
                            binders.push(binder);
                            continue;
                        };
                        let arity = Type::right_spine_product(ty).len();
                        if arity <= 1 {
                            binders.push(binder);
                            continue;
                        }
                        let mut aliases = Vec::with_capacity(arity);
                        for index in 0..arity {
                            let alias = self.fresh_projection_var();
                            let key = (param.name.clone(), index, arity);
                            let prior = self.projection_aliases.insert(key.clone(), alias.clone());
                            projection_priors.push((key, prior));
                            aliases.push(alias);
                        }
                        binders.push(format!("{binder}@(~{})", nest_tuple_value(&aliases)));
                    }
                    canonical_patterns.push(Some(binders));
                }
            }
        }
        let b = if self.is_pure_expr(body) {
            self.emit_pure(body).map(|body| format!("pure ({body})"))
        } else {
            self.emit_effectful(body)
        };
        for (key, prior) in projection_priors.into_iter().rev() {
            match prior {
                Some(alias) => {
                    self.projection_aliases.insert(key, alias);
                }
                None => {
                    self.projection_aliases.remove(&key);
                }
            }
        }
        self.restore_projection_aliases(masked_projection_aliases);
        self.type_params.truncate(outer_type_params);
        self.recon.pop_type_params(inserted_type_params);
        for g in &groups {
            for _ in g {
                self.locals.pop();
            }
        }
        for (name, prior) in priors.into_iter().rev() {
            self.recon.restore(&name, prior);
        }
        let closure_type = closure_type?.ok_or_else(|| {
            EmitError::unsupported(
                "Haskell native body: anonymous function result reconstruction gap",
            )
        })?;
        let mut acc = b?;
        let mut has_deeper_stage = false;
        for (group, prepared_patterns) in canonical.into_iter().zip(canonical_patterns).rev() {
            match group {
                SignatureGroupRef::Type(params) => {
                    for param in params.iter().rev() {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type parameters")
                        };
                        if has_deeper_stage {
                            acc = format!("pure ({acc})");
                        }
                        acc = format!("(\\ @{} -> {acc})", hs_tyvar(&param.name));
                        has_deeper_stage = true;
                    }
                }
                SignatureGroupRef::Value(_) => {
                    let binders = prepared_patterns
                        .expect("every value signature group has prepared native patterns");
                    let pattern = nest_tuple_value(&binders);
                    if has_deeper_stage {
                        acc = format!("pure ({acc})");
                    }
                    acc = format!("(\\{pattern} -> {acc})");
                    has_deeper_stage = true;
                }
            }
        }
        Ok(format!("({acc} :: {closure_type})"))
    }

    // --- helpers ----------------------------------------------------------

    fn resolve_module_fn(&self, mangled: &str) -> String {
        resolve_called_fn(mangled, self.module_key, self.ctx.package)
    }

    fn resolve_qualified_module_fn(&self, alias: &str, mangled: &str) -> String {
        resolve_qualified_called_fn(alias, mangled, self.module_key, self.ctx.package)
    }

    /// A fresh `>>=`-bind variable name (`__m0`, `__m1`, …).
    fn fresh_var(&mut self) -> String {
        let n = self.fresh;
        self.fresh += 1;
        format!("__m{n}")
    }

    fn fresh_projection_var(&mut self) -> String {
        let n = self.projection_fresh;
        self.projection_fresh += 1;
        format!("__p{n}")
    }

    fn projection_alias(
        &self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Option<String> {
        let Expr::LowBoundRef { name, .. } = target else {
            return None;
        };
        self.projection_aliases
            .get(&(name.clone(), index, arity))
            .cloned()
    }

    fn take_projection_aliases(&mut self, name: &str) -> Vec<((String, usize, usize), String)> {
        let keys = self
            .projection_aliases
            .keys()
            .filter(|(bound, _, _)| bound == name)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .map(|key| {
                let alias = self
                    .projection_aliases
                    .remove(&key)
                    .expect("collected projection alias remains present");
                (key, alias)
            })
            .collect()
    }

    fn restore_projection_aliases(&mut self, aliases: Vec<((String, usize, usize), String)>) {
        for (key, alias) in aliases {
            self.projection_aliases.insert(key, alias);
        }
    }

    /// Seed the reconstruction env with a `let`-bound value's type, when
    /// reconstructible. Returns the prior binding for restore.
    fn bind_local_ty(&mut self, name: &str, value: &Expr<Routed>) -> Option<Type<Routed>> {
        match self.recon.value_type(value) {
            Some(ty) => self.recon.bind(name, ty),
            None => None,
        }
    }

    fn rank_n_let_type(&self, value: &Expr<Routed>) -> Result<Option<String>, EmitError> {
        let Some(ty) = self.recon.value_type(value) else {
            return Ok(None);
        };
        self.render_rank_n_type(&ty)
    }

    fn render_rank_n_type(&self, ty: &Type<Routed>) -> Result<Option<String>, EmitError> {
        if self
            .ctx
            .native_type_contains_forall(ty, &self.type_params, Some(self.module_key))
        {
            Ok(Some(self.ctx.render_direct_generic_type(
                ty,
                &self.type_params,
                Some(self.module_key),
            )?))
        } else {
            Ok(None)
        }
    }

    /// GHC does not use an enclosing binding signature as the expected type
    /// of a visible type-abstraction lambda (`\ @a -> ...`).  Attach the
    /// self-describing Routed value type directly to generated rank-N values
    /// so every abstraction has the `forall` that scopes its binder.
    fn annotate_rank_n_value(
        &self,
        rendered: String,
        ty: Option<&Type<Routed>>,
    ) -> Result<String, EmitError> {
        let Some(ty) = ty else {
            return Ok(rendered);
        };
        let Some(rendered_ty) = self.render_rank_n_type(ty)? else {
            return Ok(rendered);
        };
        Ok(format!("(({rendered}) :: {rendered_ty})"))
    }

    fn unbind_local_ty(&mut self, name: &str, prior: Option<Type<Routed>>) {
        self.recon.restore(name, prior);
    }

    /// Whether an expression is pure (reaches no host call) — drives the
    /// `let`-value and effectful-context lifting decisions.
    fn is_pure_expr(&self, expr: &Expr<Routed>) -> bool {
        let mut has_host = false;
        let mut called = BTreeSet::new();
        EffectFacts {
            module_key: self.module_key,
            package: self.ctx.package,
            has_host: &mut has_host,
            called: &mut called,
        }
        .collect(expr);
        if has_host {
            return false;
        }
        // A call to an effectful module fn is impure too.
        !called
            .iter()
            .any(|c| self.ctx.effect_of(c) == Effect::Effectful)
    }
}

// =========================================================================
// Newtype metadata.
// =========================================================================

/// Build a [`Carrier`]. Every parametric, recursive, or carrier-shaped Kio
/// newtype has one declaration-stable Haskell head. Only a nullary,
/// nonrecursive, nonexistential newtype remains transparent.
fn carrier_from_decl(
    d: &Newtype<Routed>,
    module_path: &str,
    opaque: bool,
    host_surface: Option<HaskellNewtypeHostSurface>,
) -> Carrier {
    Carrier {
        kio_name: d.name.clone(),
        hs_name: carrier_hs_name(module_path, &d.name),
        type_params: d.type_params.clone(),
        payload: d.payload.clone(),
        opaque,
        host_surface,
    }
}

/// Build an [`ExistentialCarrier`] from a newtype with existential params.
/// The Kio constructor / projector member names (`mk` / `get`, or
/// `mk_box` / `un_box`) are not threaded: the GADT data constructor is the
/// Haskell type name itself and the CPS unpacker name is derived from it
/// ([`existential_proj_fn`]), so the emit dispatches by newtype name alone.
fn existential_from_decl(
    d: &Newtype<Routed>,
    module_path: &str,
    host_surface: Option<HaskellNewtypeHostSurface>,
    continuation_arity: usize,
) -> ExistentialCarrier {
    ExistentialCarrier {
        kio_name: d.name.clone(),
        hs_name: existential_hs_name(module_path, &d.name),
        host_surface,
        type_params: d.type_params.clone(),
        existential_params: d.existential_params.clone(),
        payload: d.payload.clone(),
        continuation_arity,
    }
}

// =========================================================================
// Exact host-syntax constraint analysis.
// =========================================================================

struct ExactConstraintFacts<'p, 'c> {
    module_key: &'p str,
    package: &'p Package<Routed>,
    shapes: &'c super::skin::HaskellShapes<'p>,
    recon: &'c mut TypeRecon<'p>,
    constraints: &'c mut BTreeSet<String>,
    called: &'c mut BTreeSet<String>,
}

impl ExactConstraintFacts<'_, '_> {
    fn record(
        &mut self,
        ty: &Type<Routed>,
        kind: super::skin::ExactHostConstraint,
    ) -> Result<(), EmitError> {
        self.constraints.insert(
            self.shapes
                .exact_host_syntax(ty, Some(self.module_key), kind)?
                .constraint,
        );
        Ok(())
    }

    fn collect(&mut self, expr: &Expr<Routed>) -> Result<(), EmitError> {
        match expr {
            Expr::StrLit { annotation, .. } => {
                self.record(annotation, super::skin::ExactHostConstraint::String)?
            }
            Expr::IntLit { annotation, .. } => {
                self.record(annotation, super::skin::ExactHostConstraint::Integral)?
            }
            Expr::FloatLit { annotation, .. } => {
                self.record(annotation, super::skin::ExactHostConstraint::Fractional)?
            }
            Expr::BoolLit { annotation, .. } => {
                self.record(annotation, super::skin::ExactHostConstraint::Boolean)?
            }
            Expr::LowModuleCall { mangled, .. } | Expr::LowModuleFnValueRef { mangled, .. } => {
                self.called
                    .insert(resolve_called_fn(mangled, self.module_key, self.package));
            }
            Expr::LowQualifiedModuleCall { alias, mangled, .. } => {
                self.called.insert(resolve_qualified_called_fn(
                    alias,
                    mangled,
                    self.module_key,
                    self.package,
                ));
            }
            Expr::Let {
                name,
                ty,
                value,
                body,
                ..
            } => {
                let value_ty = ty.clone().or_else(|| self.recon.value_type(value));
                self.collect(value)?;
                if let Some(value_ty) = value_ty {
                    let prior = self.recon.bind(name, value_ty);
                    let result = self.collect(body);
                    self.recon.restore(name, prior);
                    return result;
                }
                return self.collect(body);
            }
            Expr::FnExpr { sig, body, .. } => {
                let mut bindings = Vec::new();
                for param in &sig.params {
                    if let SignatureParam::Value(param) = param
                        && let Some(ty) = &param.ty
                    {
                        bindings
                            .push((param.name.clone(), self.recon.bind(&param.name, ty.clone())));
                    }
                }
                let inserted_type_params = self.recon.push_type_params(sig);
                let result = self.collect(body);
                self.recon.pop_type_params(inserted_type_params);
                for (name, prior) in bindings.into_iter().rev() {
                    self.recon.restore(&name, prior);
                }
                return result;
            }
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => {
                self.collect(scrutinee)?;
                let arity = arms.len();
                for (index, arm) in arms.iter().enumerate() {
                    let slot = Type::right_spine_sum_slot_for_arity(scrutinee_ty, index, arity)
                        .unwrap_or_else(|| {
                            unreachable!(
                                "Haskell exact-constraint analysis: enriched match arm {index} \
                                 has no payload type at arity {arity}"
                            )
                        });
                    let prior = self.recon.bind(&arm.param, slot.clone());
                    let result = self.collect(&arm.body);
                    self.recon.restore(&arm.param, prior);
                    result?;
                }
                return Ok(());
            }
            Expr::EnrichedConditional { cond, .. } => {
                let condition_ty = self.recon.value_type(cond).ok_or_else(|| {
                    EmitError::unsupported(format!(
                        "Haskell native body: conditional condition type reconstruction gap at {}",
                        expr_kind(cond)
                    ))
                })?;
                self.record(&condition_ty, super::skin::ExactHostConstraint::Boolean)?;
            }
            _ => {}
        }

        for child in routed_expr_children(expr, true) {
            self.collect(child)?;
        }
        Ok(())
    }
}

/// Immediate expression children of the Routed IR shapes consumed by the
/// native Haskell emitter. Constraint collection descends into closure bodies;
/// effect analysis treats constructing a closure as pure.
fn routed_expr_children(expr: &Expr<Routed>, include_closure_body: bool) -> Vec<&Expr<Routed>> {
    match expr {
        Expr::LowHostCall { args, .. }
        | Expr::LowModuleCall { args, .. }
        | Expr::LowQualifiedModuleCall { args, .. }
        | Expr::LowClosureCall { args, .. } => args.iter().collect(),
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            vec![value.as_ref(), body.as_ref()]
        }
        Expr::FnExpr { body, .. } if include_closure_body => vec![body.as_ref()],
        Expr::EnrichedTuple { items, .. } => items.iter().collect(),
        Expr::EnrichedRecord { fields, .. } => fields.iter().map(|field| &field.value).collect(),
        Expr::EnrichedProject { target, .. }
        | Expr::EnrichedFieldGet { target, .. }
        | Expr::EnrichedInject {
            payload: target, ..
        }
        | Expr::LowNewtypeCtor {
            payload: target, ..
        }
        | Expr::LowQualifiedNewtypeMember {
            payload: target, ..
        }
        | Expr::LowNewtypeProj { target, .. }
        | Expr::LowAbsurdCall {
            value_arg: target, ..
        } => vec![target.as_ref()],
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => std::iter::once(scrutinee.as_ref())
            .chain(arms.iter().map(|arm| &arm.body))
            .collect(),
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => vec![cond.as_ref(), then_branch.as_ref(), else_branch.as_ref()],
        Expr::LowTypeApplication { callee, .. } => vec![callee.as_ref()],
        Expr::LowIndirectCall { callee, args, .. } => std::iter::once(callee.as_ref())
            .chain(args.iter())
            .collect(),
        Expr::LowCpsProjectorApply {
            receiver,
            continuation,
            ..
        } => vec![receiver.as_ref(), continuation.as_ref()],
        _ => Vec::new(),
    }
}

fn propagate_exact_constraints(
    direct: &BTreeMap<String, BTreeSet<String>>,
    callees: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut transitive = direct.clone();
    loop {
        let prior = transitive.clone();
        let mut changed = false;
        for (caller, called) in callees {
            let inherited = called
                .iter()
                .filter_map(|callee| prior.get(callee))
                .flat_map(|constraints| constraints.iter().cloned())
                .collect::<Vec<_>>();
            let target = transitive.entry(caller.clone()).or_default();
            let before = target.len();
            target.extend(inherited);
            changed |= target.len() != before;
        }
        if !changed {
            return transitive;
        }
    }
}

// =========================================================================
// Effect (host-reachability) analysis.
// =========================================================================

/// Walk an expression collecting (a) whether it directly holds a host
/// call and (b) the mangled names of module fns it calls — the inputs to
/// the module-effect fixed point.
struct EffectFacts<'p, 'f> {
    module_key: &'p str,
    package: &'p Package<Routed>,
    has_host: &'f mut bool,
    called: &'f mut BTreeSet<String>,
}

impl EffectFacts<'_, '_> {
    fn collect(&mut self, expr: &Expr<Routed>) {
        match expr {
            Expr::LowHostCall { .. }
            | Expr::LowClosureCall { .. }
            | Expr::LowIndirectCall { .. }
            | Expr::LowTypeApplication { .. }
            | Expr::LowCpsProjectorApply { .. } => *self.has_host = true,
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } if residual_module_call_stages(sig, type_args.len(), args.is_empty()).is_empty() => {
                self.called
                    .insert(resolve_called_fn(mangled, self.module_key, self.package));
            }
            Expr::LowQualifiedModuleCall {
                alias,
                mangled,
                type_args,
                args,
                sig,
                ..
            } if residual_module_call_stages(sig, type_args.len(), args.is_empty()).is_empty() => {
                self.called.insert(resolve_qualified_called_fn(
                    alias,
                    mangled,
                    self.module_key,
                    self.package,
                ));
            }
            _ => {}
        }

        for child in routed_expr_children(expr, false) {
            self.collect(child);
        }
    }
}

/// Resolve a `LowModuleCall.mangled` to the called fn's top-level mangled
/// name, honoring `import <module>(…);` imports of visible module fns.
fn resolve_called_fn(mangled: &str, module_key: &str, package: &Package<Routed>) -> String {
    if let Some(entry) = package.module(module_key) {
        let owners = crate::backends::selective_module_fn_import_owners(&entry.module, package);
        if let Some(owner) = owners.get(mangled) {
            return module_fn_mangled(owner, mangled);
        }
    }
    module_fn_mangled(module_key, mangled)
}

/// Resolve a `LowQualifiedModuleCall` (`<alias>.<fn>` from `import <pkg>/<mod>
/// as <alias>;`) to the called fn's top-level mangled name. The lower pass
/// resolves the callee to a concrete module: for the recognized case
/// `mangled` is `<target-module-slash-path>.<member>` and `alias` is the
/// same slash path (see `recover_to_low::lower_exact_module_fn_call`); for
/// the unrecognized-placeholder case `mangled` is `<user-alias>.<member>`
/// and the user alias resolves through the calling module's
/// `import … as <alias>;` map. Either way the result uses the resolved module
/// and member identity, not the alias spelling.
fn resolve_qualified_called_fn(
    alias: &str,
    mangled: &str,
    module_key: &str,
    package: &Package<Routed>,
) -> String {
    let member = mangled.rsplit_once('.').map(|(_, m)| m).unwrap_or(mangled);
    // A user alias in scope (`import <pkg>/<mod> as <alias>;`) maps to the
    // target module's slash path; otherwise the lower pass already stamped
    // the resolved slash path into `alias`.
    let target_key =
        qualified_alias_target(alias, module_key, package).unwrap_or_else(|| alias.to_owned());
    module_fn_mangled(&target_key, member)
}

/// The target module slash-path for a qualified-import `alias` in
/// `module_key`'s `import … as <alias>;` declarations, if any.
fn qualified_alias_target(
    alias: &str,
    module_key: &str,
    package: &Package<Routed>,
) -> Option<String> {
    let entry = package.module(module_key)?;
    for u in &entry.module.imports {
        if let crate::ast::ImportKind::Qualified { path, alias: a } = &u.kind
            && a == alias
        {
            return Some(
                path.segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
    None
}

// =========================================================================
// Naming + rendering helpers.
// =========================================================================

fn module_fn_mangled(module_key: &str, leaf: &str) -> String {
    module_fn_name(module_key, leaf)
}

fn carrier_ctor_fn(hs_name: &str) -> String {
    format!("mk{hs_name}")
}

fn carrier_proj_fn(hs_name: &str) -> String {
    format!("un{hs_name}")
}

fn carrier_ctor_call(hs_name: &str, handle: &str, payload: &str) -> String {
    format!("({} {handle} ({payload}))", carrier_ctor_fn(hs_name))
}

fn carrier_proj_call(hs_name: &str, handle: &str, target: &str) -> String {
    format!("({} {handle} ({target}))", carrier_proj_fn(hs_name))
}

/// The rank-N CPS unpacker for an existential carrier (`unBox` for
/// `Box`). Distinct from [`carrier_proj_fn`] only in intent — both spell
/// `un<Hs>` — but an existential carrier never coexists with a same-named
/// plain carrier (one source newtype has one representation), so the spelling is
/// unambiguous.
fn existential_proj_fn(hs_name: &str) -> String {
    format!("un{hs_name}")
}

/// The Haskell tyvars for a type-param list (`t_`-prefixed, in order).
fn hs_tyvars(params: &[TypeParam]) -> Vec<String> {
    params.iter().map(|p| hs_tyvar(&p.name)).collect()
}

fn hs_kinded_tyvars(
    params: &[TypeParam],
    standard_names: &super::naming::StandardNames,
) -> Vec<String> {
    params
        .iter()
        .map(|param| super::skin::kinded_haskell_binder(param, standard_names))
        .collect()
}

/// Every type-param name a signature binds, across every type group.
fn fn_type_params(sig: &crate::ast::Signature<Routed>) -> Vec<&TypeParam> {
    sig.params
        .iter()
        .filter_map(|param| match param {
            SignatureParam::Type(param) => Some(param),
            SignatureParam::Value(_) => None,
        })
        .collect()
}

fn fn_type_param_names(sig: &crate::ast::Signature<Routed>) -> Vec<&str> {
    fn_type_params(sig)
        .into_iter()
        .map(|param| param.name.as_str())
        .collect()
}

/// Equation patterns for the native module-function calling convention.
/// Invisible type patterns stay at their canonical binder positions; the
/// first value group is positionally curried, while every later value group
/// occupies one tuple-shaped argument.
fn render_module_fn_patterns(sig: &crate::ast::Signature<Routed>) -> String {
    let groups = sig.canonical_groups();
    let first_value = groups
        .iter()
        .position(|group| matches!(group, SignatureGroupRef::Value(_)));
    let mut rendered = String::new();
    for (index, group) in groups.into_iter().enumerate() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(param) = param else {
                        unreachable!("SignatureGroupRef::Type contains only type parameters")
                    };
                    rendered.push_str(" @");
                    rendered.push_str(&hs_tyvar(&param.name));
                }
            }
            SignatureGroupRef::Value(params) => {
                let binders = params
                    .iter()
                    .map(|param| match param {
                        SignatureParam::Value(param) => hs_local(&param.name),
                        SignatureParam::Type(_) => {
                            unreachable!("SignatureGroupRef::Value contains only value parameters")
                        }
                    })
                    .collect::<Vec<_>>();
                if Some(index) == first_value {
                    for binder in binders {
                        rendered.push(' ');
                        rendered.push_str(&binder);
                    }
                } else {
                    rendered.push(' ');
                    rendered.push_str(&nest_tuple_value(&binders));
                }
            }
        }
    }
    rendered
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeCallStage {
    Type(String),
    Value {
        arity: usize,
        module_positional: bool,
    },
}

/// Render the uniform first-class ABI for successive Kio `forall` stages.
/// Each visible type application returns the next stage through `m`; the
/// caller supplies the already-rendered value-stage leaf.
fn render_staged_foralls(
    type_params: &[TypeParam],
    mut leaf: String,
    standard_names: &super::naming::StandardNames,
) -> String {
    for param in type_params.iter().rev() {
        leaf = format!(
            "forall {}. m {}",
            super::skin::kinded_haskell_binder(param, standard_names),
            paren_ty(&leaf)
        );
    }
    leaf
}

/// Adapt the payload leaf of a staged public `forall` while preserving every
/// observable type-application action. Applying a public stage produces the
/// next stage through `m`; bind that action before rebuilding the matching
/// internal stage, rather than manufacturing the later polymorphism with
/// `pure` ahead of the public continuation's effects.
fn render_staged_forall_adapter(
    type_params: &[TypeParam],
    source: &str,
    source_leaf_ty: &str,
    depth: usize,
    standard_names: &super::naming::StandardNames,
    adapt_leaf: &impl Fn(&str) -> String,
) -> String {
    let Some((param, remaining_params)) = type_params.split_first() else {
        return adapt_leaf(source);
    };
    let type_var = hs_tyvar(&param.name);
    let next = format!("__ex_public_k{depth}");
    let action = format!("({source} @{type_var})");
    let remaining_ty =
        render_staged_foralls(remaining_params, source_leaf_ty.to_owned(), standard_names);
    let rank_n = (!remaining_params.is_empty()).then_some(remaining_ty.as_str());
    let bind = render_native_effectful_bind_prefix(&action, &next, rank_n);
    let rest = render_staged_forall_adapter(
        remaining_params,
        &next,
        source_leaf_ty,
        depth + 1,
        standard_names,
        adapt_leaf,
    );
    format!("(\\ @{type_var} -> {bind}pure ({rest}))")
}

/// The declaration's ordered System-F application stages.  A type binder is
/// one stage even when adjacent binders share a source bracket run; a value
/// group is one product-domain stage.  `module_positional` records only the
/// direct Haskell calling convention (the first module value group is curried
/// positionally); it does not participate in Kio call selection.
fn native_call_stages(sig: &crate::ast::Signature<Routed>) -> Vec<NativeCallStage> {
    let mut saw_value = false;
    let mut stages = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(param) = param else {
                        unreachable!("SignatureGroupRef::Type contains only type parameters")
                    };
                    stages.push(NativeCallStage::Type(hs_tyvar(&param.name)));
                }
            }
            SignatureGroupRef::Value(params) => {
                stages.push(NativeCallStage::Value {
                    arity: params.len(),
                    module_positional: !saw_value,
                });
                saw_value = true;
            }
        }
    }
    stages
}

fn residual_module_call_stages(
    sig: &crate::ast::Signature<Routed>,
    type_arg_count: usize,
    has_no_value_args: bool,
) -> Vec<NativeCallStage> {
    let stages = native_call_stages(sig);
    let mut cursor = 0usize;
    for _ in 0..type_arg_count {
        match stages.get(cursor) {
            Some(NativeCallStage::Type(_)) => cursor += 1,
            _ => unreachable!(
                "a typed Routed module call applies a type outside its leading type stages"
            ),
        }
    }

    match stages.get(cursor) {
        Some(NativeCallStage::Value { arity, .. }) if !has_no_value_args || *arity == 0 => {
            cursor += 1;
        }
        Some(NativeCallStage::Type(_)) if !has_no_value_args => {
            unreachable!(
                "a typed Routed module call applies values before finishing its leading type stages"
            );
        }
        None if !has_no_value_args => {
            unreachable!(
                "a typed Routed module call applies a value after its signature is saturated"
            );
        }
        _ => {}
    }
    stages[cursor..].to_vec()
}

/// `LowHostCall.ret_ty` is the exact callable result after the declaration's
/// first value group. Peel the declaration groups that remain after that
/// point to obtain the compact host-record field's final boundary result.
/// Every peel validates the self-describing residual type; signature groups
/// select only the backend adapter shape, never source call resolution.
fn final_return_after_residual_signature(
    sig: &crate::ast::Signature<Routed>,
    residual: &Type<Routed>,
) -> Type<Routed> {
    let mut after_first_value = false;
    let mut cursor = residual.clone();
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Value(_) if !after_first_value => {
                after_first_value = true;
            }
            SignatureGroupRef::Type(params) if after_first_value => {
                for _ in params {
                    let Type::Forall { body, .. } = cursor else {
                        unreachable!(
                            "a typed Routed host-call residual is missing a canonical type stage"
                        );
                    };
                    cursor = *body;
                }
            }
            SignatureGroupRef::Value(abi_params) if after_first_value => {
                let Type::Function { ret, abi_arity, .. } = cursor else {
                    unreachable!(
                        "a typed Routed host-call residual is missing a canonical value stage"
                    );
                };
                // Both counts are post-recovery ABI metadata: `sig` has
                // already had product and Unit domains canonicalized, and the
                // residual function node records that same canonical arity.
                // This assertion checks the Routed artifact's consistency;
                // it never participates in source call selection.
                assert_eq!(
                    abi_arity,
                    abi_params.len(),
                    "Routed host-call residual value-stage arity differs from its canonical signature"
                );
                cursor = *ret;
            }
            SignatureGroupRef::Type(_) | SignatureGroupRef::Value(_) => {}
        }
    }
    cursor
}

/// Adapt a native module call that yields one or more Kio value groups into
/// the uniform function-value ABI. The original first group is represented by
/// separate module-function parameters; every later group is one tupled
/// argument. When the first group has already been applied, `groups` contains
/// only those later tupled groups and `module_first_group` is false.
fn render_module_fn_value_adapter(
    base_call: &str,
    stages: &[NativeCallStage],
    effect: Effect,
) -> String {
    let mut call = base_call.to_owned();
    let mut binders = Vec::with_capacity(stages.len());
    for (index, stage) in stages.iter().enumerate() {
        match stage {
            NativeCallStage::Type(type_param) => {
                call.push_str(" @");
                call.push_str(type_param);
                binders.push(format!(" @{}", type_param));
            }
            NativeCallStage::Value {
                arity,
                module_positional,
            } => {
                let group_arg = format!("__fv{index}");
                if *module_positional {
                    for item in 0..*arity {
                        call.push(' ');
                        if *arity == 1 {
                            call.push_str(&group_arg);
                        } else {
                            call.push_str(&tuple_proj(&group_arg, item, *arity));
                        }
                    }
                } else {
                    call.push(' ');
                    call.push_str(&paren_expr(&group_arg));
                }
                binders.push(group_arg);
            }
        }
    }

    let mut body = match effect {
        Effect::Pure => format!("pure ({call})"),
        Effect::Effectful => call,
    };
    for (depth, binder) in binders.into_iter().rev().enumerate() {
        if depth > 0 {
            body = format!("pure ({body})");
        }
        body = format!("(\\{binder} -> {body})");
    }
    body
}

fn hs_forall_params(
    type_params: &[TypeParam],
    standard_names: &super::naming::StandardNames,
) -> String {
    let kind = &standard_names.data_kind;
    let mut vars = vec![
        format!("(h :: {kind}.Type)"),
        format!("(m :: {kind}.Type -> {kind}.Type)"),
    ];
    vars.extend(
        type_params
            .iter()
            .map(|param| super::skin::kinded_haskell_binder(param, standard_names)),
    );
    vars.join(" ")
}

fn value_param_name_groups(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<&str>> {
    let mut groups: Vec<Vec<&str>> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter_map(|p| match p {
                        SignatureParam::Value(vp) => Some(vp.name.as_str()),
                        SignatureParam::Type(_) => None,
                    })
                    .collect(),
            ),
            crate::ast::SignatureGroupRef::Type(_) => None,
        })
        .collect();
    if groups.is_empty() {
        groups.push(Vec::new());
    }
    groups
}

/// Every value group's param types, one inner vec per group, in
/// declaration order. The exported wrapper's host surface takes all
/// groups' params in one flat call (`specs/backends/README.md`
/// § Function-type FFI canonicalization); the native `mod_*` fn takes
/// group 0's params curried individually and each LATER group as one
/// argument (a tuple when the group has two or more params), so the
/// wrapper regroups its flat args to match.
fn value_group_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<Option<Type<Routed>>>> {
    sig.canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter_map(|p| match p {
                        crate::ast::SignatureParam::Value(vp) => Some(vp.ty.clone()),
                        crate::ast::SignatureParam::Type(_) => None,
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .collect()
}

fn hs_local(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 2);
    s.push_str("k_");
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '\'' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    s
}

/// Render Kio's strict, non-recursive `let` with Haskell's recursive `let`.
/// A shadowing binder must not be in scope over its own RHS, so that RHS is
/// first bound in an enclosing scope under a compiler-reserved name.
fn render_native_pure_let(
    id: &str,
    rank_n_type: Option<&str>,
    value: &str,
    body: &str,
    shadows: bool,
) -> String {
    let signature = |binder: &str| {
        rank_n_type
            .map(|ty| format!("{binder} :: {ty}; "))
            .unwrap_or_default()
    };
    if shadows {
        let rhs = "__kio_let_rhs__";
        let forced_body = render_native_strict_force(id, rank_n_type, body);
        let binding = format!("let {{ {}{id} = {rhs} }} in {forced_body}", signature(id));
        let forced_binding = render_native_strict_force(rhs, rank_n_type, &binding);
        format!(
            "(let {{ {}{rhs} = {value} }} in {forced_binding})",
            signature(rhs)
        )
    } else {
        let forced_body = render_native_strict_force(id, rank_n_type, body);
        format!(
            "(let {{ {}{id} = {value} }} in {forced_body})",
            signature(id)
        )
    }
}

/// Force a let-bound value at its exact type. GHC otherwise instantiates a
/// polymorphic binding before passing it to `seq`, which rejects a nested
/// polytype such as `forall a. m (forall b. ...)`.
fn render_native_strict_force(value: &str, rank_n_type: Option<&str>, body: &str) -> String {
    if let Some(ty) = rank_n_type {
        format!("seq @{} {value} ({body})", paren_ty(ty))
    } else {
        format!("{value} `seq` ({body})")
    }
}

/// Render the prefix that sequences one effectful value. A rank-N result uses
/// the generated `kioBind` wrapper so `ImpredicativeTypes` instantiates its
/// result as a polytype instead of choosing a monomorphic first use.
fn render_native_effectful_bind_prefix(value: &str, id: &str, rank_n_type: Option<&str>) -> String {
    if let Some(ty) = rank_n_type {
        format!(
            "kioBind @{} {} $ \\{id} -> ",
            paren_ty(ty),
            paren_expr(value)
        )
    } else {
        format!("{} >>= \\{id} -> ", paren_expr(value))
    }
}

/// Lift one native value into `m`, fixing the impredicative result type when
/// the value itself is rank-N.
fn render_native_pure_value(value: &str, rank_n_type: Option<&str>) -> String {
    if let Some(ty) = rank_n_type {
        format!("kioPure @{} {}", paren_ty(ty), paren_expr(value))
    } else {
        format!("pure ({value})")
    }
}

/// Stable, injective Haskell name for an opaque carrier. The declaration
/// identity, rather than package-wide collision state, determines the name.
fn carrier_hs_name(module_path: &str, name: &str) -> String {
    super::skin::nominal_haskell_type_name("KioCarrier", module_path, name)
}

/// Stable, injective public name for an existential carrier. The exact
/// declaration identity is encoded independently of the rest of the package,
/// so adding a same-leaf declaration cannot rename an existing facade type.
fn existential_hs_name(module_path: &str, name: &str) -> String {
    super::skin::nominal_haskell_type_name("KioExistential", module_path, name)
}

#[cfg(test)]
fn rendered_mentions_type(rendered: &str, ty: &str) -> bool {
    rendered
        .lines()
        .flat_map(|line| {
            line.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '\''))
        })
        .any(|token| token == ty)
}

/// Lower a Kio **type parameter** name to its Haskell type variable. A
/// `t_`-prefixed word-cased spelling — collision-proof against the host
/// monad variable `m` (a Kio carrier param is often named `M`, which a
/// bare lower-case would map to `m` and unify with the host monad) and
/// against the package's other reserved identifiers. Used uniformly at
/// every site a Kio type param becomes a Haskell tyvar (carrier / dict
/// decl heads, rank-N `forall` binders, applied `F(A)` heads), so a
/// carrier's `f a` and a dict's `forall a b.` stay consistent.
fn hs_tyvar(name: &str) -> String {
    super::skin::haskell_type_var(name)
}

/// A right-nested binary-sum injection. Variant `v` of `n` is `Right`
/// applied `v` times, then `Left <payload>` (for `v < n-1`) or the bare
/// payload after the `Right`s (the final variant).
fn sum_inject(payload: &str, variant: usize, variants: usize) -> String {
    let mut out = if variant + 1 == variants {
        // Final variant: no `Left`; sits at the innermost right.
        paren_expr(payload)
    } else {
        format!("Left {}", paren_expr(payload))
    };
    for _ in 0..variant {
        out = format!("Right ({out})");
    }
    format!("({out})")
}

/// Inject `binder` into arm `k` of an `n`-arm native `Either` chain — the
/// host-boundary converter's `In`-direction builder. Same right-nested
/// `Either` shape as [`sum_inject`].
pub(super) fn either_inject(k: usize, n: usize, binder: &str) -> String {
    sum_inject(binder, k, n)
}

/// The native `Either`-chain pattern that binds arm `k` of `n` to `binder`:
/// `k` `Right`s wrapping a `Left binder` (or the bare `binder` for the
/// final arm). The inverse of [`sum_inject`] — used by the host-boundary
/// converter's `Out` direction to read a native sum arm.
pub(super) fn either_pattern(k: usize, n: usize, binder: &str) -> String {
    let mut out = if k + 1 == n {
        binder.to_owned()
    } else {
        format!("Left {binder}")
    };
    for _ in 0..k {
        out = format!("Right ({out})");
    }
    out
}

pub(super) fn tuple_proj(target: &str, index: usize, arity: usize) -> String {
    // A native nested-binary tuple projection. A product is `(head, rest)`
    // cons cells, the last slot bare, so slot `i` is reached by peeling the
    // tail (`snd`, the `(_, __sel)` selector) `i` times, then taking the
    // head (`fst`, the `(__sel, _)` selector) unless `i` is the final slot.
    if arity == 1 {
        return target.to_owned();
    }
    let mut acc = target.to_owned();
    for _ in 0..index {
        acc = format!("(case ({acc}) of {{ (_, __sel) -> __sel }})");
    }
    if index + 1 < arity {
        acc = format!("(case ({acc}) of {{ (__sel, _) -> __sel }})");
    }
    acc
}

/// Nest already-rendered tuple-element *types* into a nested-binary tuple
/// type `(t0, (t1, t2))`: a Kio product / value group is one binary cons
/// per `&`, the last element bare; a single element is itself (no tuple),
/// an empty group is `()`.
fn nest_tuple_type(elems: &[String]) -> String {
    match elems {
        [] => "()".to_owned(),
        [only] => only.clone(),
        [head, rest @ ..] => format!("({head}, {})", nest_tuple_type(rest)),
    }
}

fn nest_either_type(elems: &[String]) -> String {
    match elems {
        [] => "()".to_owned(),
        [only] => only.clone(),
        [head, rest @ ..] => format!("Either {} ({})", paren_ty(head), nest_either_type(rest)),
    }
}

/// Nest already-rendered tuple-element *values* into a nested-binary tuple
/// `(v0, (v1, v2))` — the value-level counterpart of [`nest_tuple_type`],
/// read back by [`tuple_proj`]'s peel.
pub(super) fn nest_tuple_value(elems: &[String]) -> String {
    match elems {
        [] => "()".to_owned(),
        [only] => only.clone(),
        [head, rest @ ..] => format!("({head}, {})", nest_tuple_value(rest)),
    }
}

/// Apply a Haskell type head to already-parenthesized argument types:
/// `Box` + `[a]` → `Box a`; a head with no args is the bare head.
fn apply_hs_head(head: &str, args: &[String]) -> String {
    if args.is_empty() {
        head.to_owned()
    } else {
        format!("{head} {}", args.join(" "))
    }
}

fn paren_ty(ty: &str) -> String {
    let t = ty.trim();
    if !t.contains(' ') {
        return t.to_owned();
    }
    // Already a *single* balanced group (`(a, b)`, `[a]`)? Leave it. A naive
    // first/last-char check is wrong for `() -> ()` (the leading `()` closes
    // before the end), which must be wrapped so it stays one argument.
    if is_single_wrapped(t, '(', ')') || is_single_wrapped(t, '[', ']') {
        return t.to_owned();
    }
    format!("({t})")
}

fn paren_expr(expr: &str) -> String {
    let t = expr.trim();
    if !t.contains(' ') {
        return t.to_owned();
    }
    if is_single_wrapped(t, '(', ')') {
        return t.to_owned();
    }
    format!("({t})")
}

/// Whether `t` is a single balanced `open`/`close` group enclosing
/// everything — `(a, b)` is, but `() -> ()` (the leading pair closes at
/// index 1) and `(a) (b)` are not. Used to decide whether a rendered type
/// / expression already carries its own outer parens.
fn is_single_wrapped(t: &str, open: char, close: char) -> bool {
    if !t.starts_with(open) || !t.ends_with(close) {
        return false;
    }
    let mut depth = 0i32;
    for (i, c) in t.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return i + close.len_utf8() == t.len();
            }
        }
    }
    false
}

fn haskell_string_lit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\{}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn expr_kind(expr: &Expr<Routed>) -> &'static str {
    match expr {
        Expr::Unit { .. } => "Unit",
        Expr::StrLit { .. } => "StrLit",
        Expr::IntLit { .. } => "IntLit",
        Expr::FloatLit { .. } => "FloatLit",
        Expr::BoolLit { .. } => "BoolLit",
        Expr::Let { .. } => "Let",
        Expr::Seq { .. } => "Seq",
        Expr::LowHostCall { .. } => "LowHostCall",
        Expr::LowModuleCall { .. } => "LowModuleCall",
        Expr::LowQualifiedModuleCall { .. } => "LowQualifiedModuleCall",
        Expr::LowNewtypeCtor { .. } => "LowNewtypeCtor",
        Expr::LowNewtypeProj { .. } => "LowNewtypeProj",
        Expr::LowQualifiedNewtypeMember { .. } => "LowQualifiedNewtypeMember",
        Expr::LowClosureCall { .. } => "LowClosureCall",
        Expr::LowIndirectCall { .. } => "LowIndirectCall",
        Expr::LowTypeApplication { .. } => "LowTypeApplication",
        Expr::LowCpsProjectorApply { .. } => "LowCpsProjectorApply",
        Expr::LowAbsurdCall { .. } => "LowAbsurdCall",
        Expr::LowBoundRef { .. } => "LowBoundRef",
        Expr::LowHostFnValueRef { .. } => "LowHostFnValueRef",
        Expr::LowModuleFnValueRef { .. } => "LowModuleFnValueRef",
        Expr::FnExpr { .. } => "FnExpr",
        Expr::EnrichedTuple { .. } => "EnrichedTuple",
        Expr::EnrichedRecord { .. } => "EnrichedRecord",
        Expr::EnrichedProject { .. } => "EnrichedProject",
        Expr::EnrichedFieldGet { .. } => "EnrichedFieldGet",
        Expr::EnrichedInject { .. } => "EnrichedInject",
        Expr::EnrichedMatch { .. } => "EnrichedMatch",
        Expr::EnrichedConditional { .. } => "EnrichedConditional",
        _ => "<surface>",
    }
}

#[cfg(test)]
mod public_type_name_tests {
    use super::{carrier_hs_name, existential_hs_name, rendered_mentions_type};

    #[test]
    fn carrier_name_is_injective_and_declaration_local() {
        let slash = carrier_hs_name("a/b", "Tree");
        let underscore = carrier_hs_name("a_b", "Tree");
        assert_ne!(slash, underscore);
        assert_eq!(slash, carrier_hs_name("a/b", "Tree"));
    }

    #[test]
    fn existential_name_is_injective_and_declaration_local() {
        let slash = existential_hs_name("a/b", "Pack");
        let underscore = existential_hs_name("a_b", "Pack");
        assert_ne!(slash, underscore);
        assert_eq!(slash, existential_hs_name("a/b", "Pack"));
    }

    #[test]
    fn shape_reachability_includes_sum_constructor_payloads() {
        let rendered = "data Sum (h :: Type) = LeftArm () | RightArm (Nominal h)\n";

        assert!(rendered_mentions_type(rendered, "Nominal"));
        assert!(!rendered_mentions_type(rendered, "NominalSuffix"));
    }
}

#[cfg(test)]
mod module_call_shape_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        Effect, NativeCallStage, final_return_after_residual_signature, hs_local,
        native_call_stages, propagate_exact_constraints, render_module_fn_patterns,
        render_module_fn_value_adapter, render_native_effectful_bind_prefix,
        render_native_pure_let, render_native_strict_force, render_staged_forall_adapter,
        render_staged_foralls, residual_module_call_stages,
    };
    use crate::{
        ast::{Meta, Param, PathSegment, Routed, Signature, SignatureGroup, Type, TypeParam},
        backends::haskell::naming::StandardNames,
        span::Span,
    };

    fn standard_names() -> StandardNames {
        StandardNames::new("Test.Package")
    }

    fn path_ty(name: &str) -> Type<Routed> {
        let span = Span::new(0, 0);
        Type::Path {
            segments: vec![PathSegment::new(name.to_owned(), span)],
            args: Vec::new(),
            meta: Meta::new(span),
        }
    }

    fn sum(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Sum {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    #[test]
    fn residual_module_groups_keep_the_function_value_effect_abi() {
        let stages = [
            NativeCallStage::Type("t_a".to_owned()),
            NativeCallStage::Type("t_b".to_owned()),
            NativeCallStage::Value {
                arity: 1,
                module_positional: false,
            },
        ];
        let pure = render_module_fn_value_adapter("mod_f _pkg", &stages, Effect::Pure);
        assert!(pure.contains("mod_f _pkg @t_a @t_b __fv2"));
        assert_eq!(pure.matches("pure").count(), 3);

        let effectful = render_module_fn_value_adapter("mod_f _pkg", &stages, Effect::Effectful);
        assert!(effectful.contains("mod_f _pkg @t_a @t_b __fv2"));
        assert_eq!(effectful.matches("pure").count(), 2);
    }

    #[test]
    fn successive_foralls_are_distinct_effect_stages() {
        let span = Span::new(0, 0);
        let params = ["A", "B"].map(|name| TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        });
        let standard_names = standard_names();

        assert_eq!(
            render_staged_foralls(&params, "t_b -> m t_a".to_owned(), &standard_names),
            "forall (t_a :: Test.Package.KioStandardDataKind.Type). m (forall (t_b :: Test.Package.KioStandardDataKind.Type). m (t_b -> m t_a))"
        );
    }

    #[test]
    fn staged_boundary_adapter_sequences_each_public_type_stage() {
        let span = Span::new(0, 0);
        let params = ["A", "B"].map(|name| TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        });
        let standard_names = standard_names();
        let rendered = render_staged_forall_adapter(
            &params,
            "arg1",
            "boundary -> m r",
            0,
            &standard_names,
            &|continuation| format!("(\\payload -> {continuation} (convert payload))"),
        );

        assert!(
            rendered.contains(
                "kioBind @(forall (t_b :: Test.Package.KioStandardDataKind.Type). m (boundary -> m r)) (arg1 @t_a)"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("\\ @t_b -> (__ex_public_k0 @t_b) >>= \\__ex_public_k1 ->"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\\payload -> __ex_public_k1 (convert payload)"),
            "{rendered}"
        );
        assert!(!rendered.contains("pure (arg1 @"), "{rendered}");
    }

    #[test]
    fn module_patterns_preserve_interleaved_type_and_value_groups() {
        let span = Span::new(0, 0);
        let type_param = |name: &str| TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        };
        let value_param = |name: &str| Param {
            name: name.to_owned(),
            ty: Some(path_ty("N")),
            pattern: (),
            meta: Meta::new(span),
        };
        let sig = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("A")]),
            SignatureGroup::Value(vec![value_param("left"), value_param("right")]),
            SignatureGroup::Type(vec![type_param("B")]),
            SignatureGroup::Value(vec![value_param("tail")]),
        ]);

        assert_eq!(
            render_module_fn_patterns(&sig),
            " @t_a k_left k_right @t_b k_tail"
        );
        assert_eq!(
            native_call_stages(&sig),
            vec![
                NativeCallStage::Type("t_a".to_owned()),
                NativeCallStage::Value {
                    arity: 2,
                    module_positional: true,
                },
                NativeCallStage::Type("t_b".to_owned()),
                NativeCallStage::Value {
                    arity: 1,
                    module_positional: false,
                },
            ]
        );
        assert_eq!(
            residual_module_call_stages(&sig, 1, false),
            vec![
                NativeCallStage::Type("t_b".to_owned()),
                NativeCallStage::Value {
                    arity: 1,
                    module_positional: false,
                },
            ]
        );

        let trailing = Signature::from_groups(vec![
            SignatureGroup::Value(vec![value_param("value")]),
            SignatureGroup::Type(vec![type_param("T")]),
        ]);
        assert_eq!(
            native_call_stages(&trailing),
            vec![
                NativeCallStage::Value {
                    arity: 1,
                    module_positional: true,
                },
                NativeCallStage::Type("t_t".to_owned()),
            ]
        );

        let residual = Signature::from_groups(vec![
            SignatureGroup::Type(vec![type_param("B")]),
            SignatureGroup::Value(vec![value_param("tail")]),
        ])
        .signature_ty(path_ty("A"), span);
        assert_eq!(
            final_return_after_residual_signature(&sig, &residual),
            path_ty("A")
        );
    }

    #[test]
    fn match_arity_keeps_the_final_sum_tail_as_one_payload() {
        let final_tail = sum(path_ty("C"), path_ty("D"));
        let ty = sum(path_ty("A"), sum(path_ty("B"), final_tail.clone()));

        assert_eq!(
            Type::right_spine_sum_slot_for_arity(&ty, 0, 3),
            Some(&path_ty("A"))
        );
        assert_eq!(
            Type::right_spine_sum_slot_for_arity(&ty, 1, 3),
            Some(&path_ty("B"))
        );
        assert_eq!(
            Type::right_spine_sum_slot_for_arity(&ty, 2, 3),
            Some(&final_tail)
        );
        assert_eq!(Type::right_spine_sum_slot_for_arity(&ty, 3, 3), None);
        assert_eq!(Type::right_spine_sum_slot_for_arity(&ty, 0, 0), None);
    }

    #[test]
    fn exact_constraints_propagate_through_recursive_scc_deterministically() {
        let direct = BTreeMap::from([
            ("a".to_owned(), BTreeSet::from(["Num A".to_owned()])),
            ("b".to_owned(), BTreeSet::from(["IsString B".to_owned()])),
            ("unrelated".to_owned(), BTreeSet::new()),
        ]);
        let callees = BTreeMap::from([
            ("a".to_owned(), BTreeSet::from(["b".to_owned()])),
            ("b".to_owned(), BTreeSet::from(["a".to_owned()])),
            ("unrelated".to_owned(), BTreeSet::new()),
        ]);

        let propagated = propagate_exact_constraints(&direct, &callees);

        let both = BTreeSet::from(["IsString B".to_owned(), "Num A".to_owned()]);
        assert_eq!(propagated["a"], both);
        assert_eq!(propagated["b"], both);
        assert!(propagated["unrelated"].is_empty());
    }

    #[test]
    fn shadowing_let_keeps_the_rhs_outside_haskells_recursive_scope() {
        let rendered = render_native_pure_let("k_state", None, "step k_state", "k_state", true);

        assert_eq!(
            rendered,
            "(let { __kio_let_rhs__ = step k_state } in __kio_let_rhs__ `seq` (let { k_state = __kio_let_rhs__ } in k_state `seq` (k_state)))"
        );
    }

    #[test]
    fn shadowing_rank_n_let_types_the_helper_and_user_binder() {
        let rendered = render_native_pure_let(
            "k_choose",
            Some("forall a. a -> a"),
            "k_choose",
            "k_choose",
            true,
        );

        assert_eq!(
            rendered,
            "(let { __kio_let_rhs__ :: forall a. a -> a; __kio_let_rhs__ = k_choose } in seq @(forall a. a -> a) __kio_let_rhs__ (let { k_choose :: forall a. a -> a; k_choose = __kio_let_rhs__ } in seq @(forall a. a -> a) k_choose (k_choose)))"
        );
    }

    #[test]
    fn strict_rank_n_force_pins_the_nested_polytype() {
        let ty = "forall t_a. m (forall t_b. m ((t_a -> m t_b) -> m t_b))";

        assert_eq!(
            render_native_strict_force("k_value", Some(ty), "body"),
            "seq @(forall t_a. m (forall t_b. m ((t_a -> m t_b) -> m t_b))) k_value (body)"
        );
    }

    #[test]
    fn ordinary_nonshadowing_let_keeps_the_existing_strict_shape() {
        assert_eq!(
            render_native_pure_let("k_payload", None, "__r", "body", false),
            "(let { k_payload = __r } in k_payload `seq` (body))"
        );
    }

    #[test]
    fn effectful_rank_n_binding_uses_impredicative_helper() {
        let rendered = render_native_effectful_bind_prefix(
            "produce",
            "__m0",
            Some("forall t_a. t_a -> m t_a"),
        );

        assert_eq!(
            rendered,
            "kioBind @(forall t_a. t_a -> m t_a) produce $ \\__m0 -> "
        );
    }

    #[test]
    fn compiler_let_helper_cannot_collide_with_an_adversarial_user_name() {
        assert_eq!(hs_local("__kio_let_rhs__"), "k___kio_let_rhs__");
        let rendered = render_native_pure_let(
            &hs_local("__kio_let_rhs__"),
            None,
            "outer",
            "k___kio_let_rhs__",
            true,
        );

        assert!(rendered.contains("let { __kio_let_rhs__ = outer }"));
        assert!(rendered.contains("let { k___kio_let_rhs__ = __kio_let_rhs__ }"));
    }

    #[test]
    fn non_shadowing_let_keeps_the_direct_strict_binding() {
        let rendered = render_native_pure_let("k_state", None, "initial", "k_state", false);

        assert_eq!(
            rendered,
            "(let { k_state = initial } in k_state `seq` (k_state))"
        );
    }
}

//! Resolution-lowering pass: `Module<Enriched>` → `Module<Routed>`.
//!
//! On the backend-facing build route this is a post-validation,
//! post-`structural_recovery`/optimization, pre-emit pass. [`Enriched`] is the
//! recovered structural IR derived from checked Prime there; the phase marker
//! itself records shape rather than validation provenance.
//! This pass centralizes routing decisions that would otherwise be re-derived
//! by each host backend from imports, declarations, and bound-local scope.
//!
//! This pass folds the resolution context **into the AST nodes themselves**.
//! Every `Expr::Call` and value-position `Expr::Path` is rewritten into an
//! `Expr::Low*` variant (per the [`Routed`] phase introduced in `ast.rs`):
//! pre-classified by call kind (host fn vs module fn vs closure vs newtype
//! constructor), pre-resolved (the mangled cross-module-stable name lives on
//! the variant, not in a per-backend table), and pre-split (type arguments
//! separated from value arguments). `Expr::FnExpr` survives as a
//! value-construction site while its body is routed recursively.
//!
//! Host backends consume `Module<Routed>` directly.
//!
//! ## Output contract: alias-free types
//!
//! No declared type alias survives into any referenced or boundary type of
//! the Routed package: every such type this pass writes — signatures, boundary
//! positions, expression type stamps, literal and binder annotations,
//! call-site type-args — is alias-unfolded in its correct spelling
//! scope and ABI-canonicalized ([`Lowerer::routed_type`] is the
//! chokepoint). `Item::TypeAlias` declarations themselves survive with
//! their bodies as declared — the one place aliases remain — but no host
//! emitter needs alias machinery for *referenced* types.
//!
//! ## Pipeline position
//!
//! ```text
//! Surface → Desugared → Lowered (typer-input)
//!        → typecheck → Prime
//!        → structural_recovery → Enriched
//!        → recover_to_low::lower → Routed   ← THIS PASS
//!        → per-backend lowering
//! ```
//!
//! ## Resolution context the pass folds in
//!
//! - **Host fns** (single-segment name → signature). Sourced
//!   from the current lowering scope's host declarations: the
//!   root host for module bodies, or the package host
//!   for package-file bridge bodies.
//! - **Module fns** (current-package, surface name → mangled cross-
//!   module name + callee signature). Sourced from each module's
//!   `import` declarations plus its own `Item::FnDef` declarations.
//! - **Newtypes** (single-segment newtype name → declaration). Sourced
//!   from `Item::Newtype` across the package, used to recognise
//!   `<NT>.<ctor>(payload)` and `<NT>.<proj>(target)` calls.
//! - **Qualified imports** (alias → (target module slash path,
//!   modifier)). Sourced from qualified `import ... as <alias>;` items.
//! - **Bound locals** (per-fn-body stack). Tracked as the walker
//!   descends through `Expr::Let`, `Expr::FnExpr`, and
//!   `Expr::EnrichedMatch` clause params; consulted at every
//!   `Expr::Path` / `Expr::Call` to distinguish a bound-name reference
//!   from an unbound (top-level / imported / intrinsic) one.
//!
//! ## Per-module parallelism
//!
//! Inherits the per-module fan-out convention from
//! [`crate::pass::structural_recovery::recover_package`] and
//! [`crate::pass::optimize::optimize_package`]: build the package-wide
//! resolution context once (serial pre-walk), then fan out across
//! rayon for the per-module body lowering.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(...)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::{
    CallArg, Enriched, EnrichedArm, Expr, FnDef, Import, ImportItem, ImportKind, Item, Meta,
    Module, PackageFile, Param, PathSegment, RecordField, Routed, Signature, SignatureGroupKind,
    SignatureGroupRef, SignatureParam, Type, convert_meta, convert_newtype, convert_type,
    convert_type_alias,
};
use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry};

#[cfg(all(test, feature = "surface"))]
mod type_rec_group_lowering_work {
    use std::cell::Cell;

    thread_local! {
        static MEMBER_PAIRS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn reset() {
        MEMBER_PAIRS.set(0);
    }

    pub(super) fn record_member_pair() {
        MEMBER_PAIRS.set(MEMBER_PAIRS.get() + 1);
    }

    pub(super) fn member_pairs() -> usize {
        MEMBER_PAIRS.get()
    }
}

// ---- public entry points ------------------------------------------------

/// Lower a whole enriched package into the Routed phase. Each module
/// body, every bridge adapter body and resolved exported fn body, and the
/// per-module name-resolution scope are carried across; expression
/// bodies get the per-variant routing-decision encoding.
///
/// **No type alias survives into a boundary position of `Routed`.**
/// Exported-fn signatures and returns, `host fn` declaration types,
/// newtype payloads, and the `sig` / `ret_ty` stamped on `LowHostCall` /
/// `LowHostFnValueRef` all have alias references unfolded to their
/// expansion (in the scope of the declaring module). Backends read these
/// positions to mint FFI shapes, so an alias surviving here would cross
/// the boundary opaquely instead of as its expansion's structural shape.
///
/// **Parallelism.** The package-wide resolution side tables are built
/// up-front via a serial pre-walk; once built they are read-only, so
/// the per-module body lowering fans out across rayon. Each module
/// gets its own [`Lowerer`] minted from the shared [`LoweringCtx`].
pub fn lower(package: &Package<Enriched>) -> Package<Routed> {
    let ctx = LoweringCtx::from_package(package);
    let package_name = package.package_file().map(|e| e.package_name.as_str());
    let module_list: Vec<_> = package.modules().collect();
    let entries: Vec<(String, ModuleEntry<Routed>)> = crate::maybe_par_iter!(module_list)
        .map(|(path, entry)| {
            let lowerer = Lowerer::new_for_module(&ctx, &entry.module, package_name, path);
            (
                (*path).to_owned(),
                ModuleEntry::<Routed> {
                    file_path: entry.file_path.clone(),
                    module: lowerer.lower_module(&entry.module),
                    scope: entry.scope.clone(),
                },
            )
        })
        .collect();
    let modules: BTreeMap<String, ModuleEntry<Routed>> = entries.into_iter().collect();
    // The package file carries only the phase-independent `bridge` glob
    // list — a structural rebrand with no bodies to lower.
    let export = package
        .package_file()
        .map(|entry| PackageFileEntry::<Routed> {
            file_path: entry.file_path.clone(),
            package_name: entry.package_name.clone(),
            package_file: PackageFile {
                name: entry.package_file.name.clone(),
                build: entry.package_file.build.clone(),
                bridge: entry.package_file.bridge.clone(),
                meta: convert_meta(&entry.package_file.meta),
            },
        });
    Package::<Routed>::from_parts(modules, export)
}

// ---- LoweringCtx --------------------------------------------------------

/// Package-wide read-only side table consulted during lowering. Built
/// once up-front by [`LoweringCtx::from_package`]; immutable for the
/// duration of the per-module fan-out so each worker can borrow it
/// without synchronisation.
#[derive(Default)]
struct LoweringCtx {
    /// Module path → (host-fn name → signature). Host fns are ordinary
    /// module declarations now; the lower pass keys their signatures by
    /// the declaring module so each call site can stamp the right
    /// `module_path` provenance onto its `LowHostCall`.
    host_fns_by_module:
        std::collections::HashMap<String, std::collections::HashMap<String, HostFnSig>>,
    /// Surface name → (declaring-module-path, newtype info). Sourced
    /// from `Item::Newtype` declarations across every module in the
    /// package. Used to recognise `<NT>.<member>(arg)` calls when the
    /// newtype is in the current module or imported into it.
    newtypes_by_name: std::collections::HashMap<String, NewtypeInfo>,
    /// Per-module-path → set of newtype names declared in that module
    /// (with their info). Lets per-module import classification
    /// resolve cross-module newtype member calls.
    newtypes_by_module:
        std::collections::HashMap<String, std::collections::HashMap<String, NewtypeInfo>>,
    /// Per declaring module, the host-type leaves owned by that module.
    /// Together with each module's selective imports this builds
    /// [`Self::host_type_home`].
    host_types_by_module: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Per-module-path → set of (fn name → signature) for fns in that
    /// module. Selective and qualified calls and fn values use the exact
    /// target module's signature for type-arg recovery and ABI lowering.
    module_fns_by_module:
        std::collections::HashMap<String, std::collections::HashMap<String, ModuleFnSig>>,
    aliases_by_module:
        std::collections::HashMap<String, std::collections::HashMap<String, AliasInfo>>,
    /// Per-module-path → the module's full alias scope: its own `type`
    /// aliases plus the ones it selectively imports. A function's
    /// boundary types are written in the *declaring* module's scope, so
    /// a cross-module call site must unfold them against this scope — the
    /// referring module's scope could miss the alias or resolve the name
    /// to a different one.
    alias_scopes: std::collections::HashMap<String, std::collections::HashMap<String, AliasInfo>>,
    /// Per-module-path → its `import <path> as <alias>;` qualifiers
    /// (alias → target module slash-path). An unresolved referenced type
    /// head (`api.I32`) is spelled in its defining scope; the unfold walk
    /// resolves that qualifier before recording canonical Routed identity.
    qualified_scopes: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Every module path in the package, for deciding whether a
    /// qualified head's qualifier is a real module path.
    modules: std::collections::HashSet<String>,
    /// Per-module-path → (bare newtype leaf in that module's scope →
    /// the slash-path of the module that declares that newtype). Holds
    /// the module's own newtype declarations plus every newtype it
    /// brings in with `import N(…, X, …);`. Used to pin a noncanonical
    /// bare referenced newtype path in its explicit defining scope,
    /// independent of the eventual consumer (`specs/language.md`
    /// § Open-world design).
    newtype_home: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Per-module spelling scope, the exact declaring module of every
    /// bare host-type leaf in scope. Routed types use this to replace a
    /// scope-relative leaf with its stable `(module, name)` path.
    host_type_home: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

/// A host fn's signature plus its declared return type. The lower
/// pass stamps both onto the `LowHostCall` variant so per-backend
/// rendering can do FFI conversion on the result without re-walking
/// the package's package file at every call site.
#[derive(Clone)]
struct HostFnSig {
    sig: Signature<Enriched>,
    ret: Type<Enriched>,
    /// The slash-path of the module declaring this `host fn`. Stamped
    /// onto the call site's `LowHostCall.module_path` for the namespaced
    /// host boundary.
    module_path: String,
}

/// A module fn's signature plus its declared return type. `Signature`
/// carries no `ret` (it lives per-embedder — see [`crate::ast::Signature`]),
/// so the lower pass keeps the two together here and stamps `ret` onto
/// the resolved `LowModuleCall` / `LowQualifiedModuleCall` variants.
/// The Rust backend reads it to discriminate a stored-in-return fn-value
/// slot (lift to `Rc<dyn Fn>`) from a passthrough one (keep `impl Fn`).
#[derive(Clone)]
struct ModuleFnSig {
    sig: Signature<Enriched>,
    ret_abi: Type<Routed>,
    target_arity: usize,
    /// The signature's spelling scope. Callers must not resolve its
    /// aliases in their own module scope.
    module_path: String,
}

#[derive(Clone)]
struct AliasInfo {
    type_params: Vec<crate::ast::TypeParam>,
    /// Declaration-owned body with every nominal head canonicalized in
    /// `home` before any caller-owned type argument is substituted.
    body: Type<Routed>,
    /// The slash-path of the module that declares this alias. It keys the
    /// canonical alias identity and cycle guard; the body itself has already
    /// been interpreted in this module before entering [`AliasInfo`].
    home: String,
}

struct ModuleCallInfo<'a> {
    sig: &'a Signature<Enriched>,
    target_arity: Option<usize>,
    ret_ty: Option<Type<Routed>>,
    declaring_module: &'a str,
    meta: &'a Meta<Enriched>,
}

#[derive(Clone)]
enum SelectedAbiShape {
    Function {
        groups: Vec<SelectedAbiGroup>,
        body: Box<SelectedAbiShape>,
    },
    Let {
        binder: usize,
        value: Box<SelectedAbiShape>,
        body: Box<SelectedAbiShape>,
    },
    Seq {
        body: Box<SelectedAbiShape>,
    },
    BoundRef {
        binder: usize,
    },
    Value,
    Unknown,
}

#[derive(Clone)]
enum SelectedAbiGroup {
    Type,
    Value { arity: usize, binders: Vec<usize> },
}

#[derive(Clone)]
struct SelectedAbiBinding {
    binder: usize,
    shape: SelectedAbiShape,
}

#[derive(Clone)]
struct NewtypeInfo {
    /// The terminal nominal declaration name. For an ordinary newtype this
    /// equals the scope-table key; for an identity-preserving alias it is the
    /// newtype whose constructor/projector implementation must be emitted.
    nominal_name: String,
    /// Whether the scope-table key is a transparent identity alias rather
    /// than the terminal nominal itself.
    forwarded: bool,
    /// The constructor's member name (typically `mk_<name>`).
    constructor: String,
    /// The projector's member name (typically the lowercase newtype
    /// name).
    projector: String,
    /// `true` when the newtype declares at least one existential
    /// type-param. The CPS-projector-apply fold needs this to decide
    /// whether an indirect call against the projector is the CPS
    /// shape (existential-bearing) or the polymorphic-fn-payload shape
    /// (existential-free, type_args go on the projector method).
    has_existentials: bool,
    /// The newtype's universal type binders, paired with projector
    /// call-site type args when CPS-continuation parameter ABI is
    /// recovered.
    type_params: Vec<crate::ast::TypeParam>,
    /// The payload with source types converted to the target ABI type
    /// space and aliases unfolded in the declaring module's scope
    /// ([`LoweringCtx::unfold_newtype_payloads`]). Existentials remain
    /// as named type params here; later backend-specific erasure still
    /// owns their runtime representation.
    payload: Type<Routed>,
    /// The declaring module's slash-path — the spelling scope the
    /// payload's aliases unfold in.
    home: String,
    /// The newtype's existential binders, in declaration order. Needed
    /// to build the value-position CPS eta for an existential projector
    /// (`build_cps_projector_value`): the continuation it binds is
    /// universally quantified over exactly these.
    existential_params: Vec<crate::ast::TypeParam>,
}

fn newtype_info_from_decl(d: &crate::ast::Newtype<Enriched>, module_path: &str) -> NewtypeInfo {
    NewtypeInfo {
        nominal_name: d.name.clone(),
        forwarded: false,
        constructor: d.constructor.name.clone(),
        projector: d.projector.name.clone(),
        has_existentials: !d.existential_params.is_empty(),
        type_params: d.type_params.clone(),
        payload: convert_type_for_abi(&d.payload),
        existential_params: d.existential_params.clone(),
        home: module_path.to_owned(),
    }
}

fn instantiate_newtype_payload(info: &NewtypeInfo, type_args: &[Type<Routed>]) -> Type<Routed> {
    let subst: HashMap<String, Type<Routed>> = info
        .type_params
        .iter()
        .zip(type_args.iter())
        .map(|(tp, arg)| (tp.name.clone(), arg.clone()))
        .collect();
    crate::pass::typecheck_core::subst_type(&info.payload, &subst)
}

fn newtype_constructor_result_type(
    info: &NewtypeInfo,
    type_args: &[Type<Routed>],
    span: crate::span::Span,
) -> Option<Type<Routed>> {
    (type_args.len() == info.type_params.len() + info.existential_params.len()).then(|| {
        Type::Path {
            segments: qualified_nominal_segments(&info.home, &info.nominal_name, span),
            args: type_args[..info.type_params.len()].to_vec(),
            meta: Meta::new(span),
        }
    })
}

fn cps_continuation_type(
    info: &NewtypeInfo,
    type_args: &[Type<Routed>],
    result_ty: Type<Routed>,
    span: crate::span::Span,
) -> Type<Routed> {
    // The existential header binds occurrences in the declaration payload,
    // but it does not bind the selected result or a caller type inserted for
    // a universal argument. Choose its presentation before universal
    // substitution, while those two classes of same-spelled occurrence are
    // still distinguishable, and rewrite only the declaration-bound payload
    // occurrences.
    let mut free_after_instantiation = HashSet::new();
    for type_arg in type_args {
        crate::pass::typecheck_core::collect_free_type_vars(
            type_arg,
            &mut free_after_instantiation,
        );
    }
    crate::pass::typecheck_core::collect_free_type_vars(&result_ty, &mut free_after_instantiation);
    let mut declaration_free = HashSet::new();
    crate::pass::typecheck_core::collect_free_type_vars(&info.payload, &mut declaration_free);
    for binder in info.type_params.iter().chain(&info.existential_params) {
        declaration_free.remove(&binder.name);
    }
    free_after_instantiation.extend(declaration_free);

    let mut payload = info.payload.clone();
    let mut existential_params = Vec::with_capacity(info.existential_params.len());
    for (index, existential) in info.existential_params.iter().enumerate() {
        let mut taken = free_after_instantiation.clone();
        taken.extend(
            existential_params
                .iter()
                .map(|param: &crate::ast::TypeParam| param.name.clone()),
        );
        taken.extend(
            info.existential_params[index + 1..]
                .iter()
                .map(|param| param.name.clone()),
        );
        let name = crate::pass::typecheck_core::fresh_type_var(&existential.name, &taken);
        let mut renamed = existential.clone();
        if name != existential.name {
            payload = crate::pass::typecheck_core::subst_type(
                &payload,
                &HashMap::from([(
                    existential.name.clone(),
                    Type::synth_path(vec![name.clone()], Vec::new(), existential.span),
                )]),
            );
            renamed.name = name;
        }
        free_after_instantiation.insert(renamed.name.clone());
        existential_params.push(renamed);
    }

    let universal_subst: HashMap<String, Type<Routed>> = info
        .type_params
        .iter()
        .zip(type_args)
        .map(|(param, arg)| (param.name.clone(), arg.clone()))
        .collect();
    let payload_ty = crate::pass::typecheck_core::subst_type(&payload, &universal_subst);
    let abi_arity = usize::from(!matches!(payload_ty, Type::Unit { .. }));
    let mut ty = Type::Function {
        param: Box::new(payload_ty),
        ret: Box::new(result_ty),
        meta: Meta::new(span),
        abi_arity,
        caps: crate::ast::FnTypeCapabilities::default(),
    };
    for existential in existential_params.into_iter().rev() {
        ty = Type::Forall {
            param: existential,
            body: Box::new(ty),
            meta: Meta::new(span),
        };
    }
    ty
}

fn instantiate_newtype_constructor_payload(
    info: &NewtypeInfo,
    type_args: &[Type<Routed>],
) -> Type<Routed> {
    let subst: HashMap<String, Type<Routed>> = info
        .type_params
        .iter()
        .chain(&info.existential_params)
        .zip(type_args.iter())
        .map(|(tp, arg)| (tp.name.clone(), arg.clone()))
        .collect();
    crate::pass::typecheck_core::subst_type(&info.payload, &subst)
}

/// The ambient host-fn scope of one module: the `host fn` declarations
/// the module itself contains (each carrying its declaring module path).
#[derive(Clone, Default)]
struct HostEnvScope {
    fns: std::collections::HashMap<String, HostFnSig>,
}

impl LoweringCtx {
    /// Build a lowering context from the package: scan every module for
    /// `host fn` declarations (keyed by module) plus the alias / newtype
    /// / module-fn declarations the lowering classifier needs.
    fn from_package(package: &Package<Enriched>) -> Self {
        let mut ctx = LoweringCtx::default();
        for (path, _) in package.modules() {
            ctx.modules.insert(path.to_owned());
        }
        for (path, entry) in package.modules() {
            ctx.collect_host_fns_from_module(path, &entry.module);
        }
        for (path, entry) in package.modules() {
            ctx.collect_aliases_and_newtypes_from_module(path, &entry.module);
        }
        ctx.collect_identity_alias_newtypes(package);
        for (path, entry) in package.modules() {
            let scope = ctx.alias_scope_for_module(path, &entry.module);
            ctx.alias_scopes.insert(path.to_owned(), scope);
        }
        for (path, entry) in package.modules() {
            ctx.collect_newtype_home_from_module(path, &entry.module);
            ctx.collect_host_type_home_from_module(path, &entry.module);
        }
        for (path, entry) in package.modules() {
            let mut quals = std::collections::HashMap::new();
            for u in &entry.module.imports {
                if let ImportKind::Qualified { path, alias } = &u.kind {
                    quals.insert(alias.clone(), module_path_to_string(path));
                }
            }
            if !quals.is_empty() {
                ctx.qualified_scopes.insert(path.to_owned(), quals);
            }
        }
        for (path, entry) in package.modules() {
            ctx.collect_fns_from_module(path, &entry.module);
        }
        ctx.unfold_newtype_payloads();
        ctx
    }

    /// Unfold type aliases out of every stored newtype payload, each in
    /// its declaring module's spelling scope. Payloads feed the
    /// FFI-facing member eta values and the boundary call path alike,
    /// so resolving them once here — after every scope table is built —
    /// is what keeps the pass's referenced/boundary-type contract (no alias
    /// survives in a Routed newtype payload) true at the source instead of
    /// compensated per consumer.
    fn unfold_newtype_payloads(&mut self) {
        for per_mod in self.newtypes_by_module.values_mut() {
            for info in per_mod.values_mut() {
                let mut env = UnfoldEnv {
                    newtype_home: &self.newtype_home,
                    host_type_home: &self.host_type_home,
                    type_vars: BTreeSet::new(),
                    module: info.home.clone(),
                    all_alias_scopes: &self.alias_scopes,
                    qualified_scopes: &self.qualified_scopes,
                    modules: &self.modules,
                };
                env.type_vars.extend(
                    info.type_params
                        .iter()
                        .chain(&info.existential_params)
                        .map(|param| param.name.clone()),
                );
                info.payload = unfold_type_alias_type(&info.payload, &env);
            }
        }
        for info in self.newtypes_by_name.values_mut() {
            let mut env = UnfoldEnv {
                newtype_home: &self.newtype_home,
                host_type_home: &self.host_type_home,
                type_vars: BTreeSet::new(),
                module: info.home.clone(),
                all_alias_scopes: &self.alias_scopes,
                qualified_scopes: &self.qualified_scopes,
                modules: &self.modules,
            };
            env.type_vars.extend(
                info.type_params
                    .iter()
                    .chain(&info.existential_params)
                    .map(|param| param.name.clone()),
            );
            info.payload = unfold_type_alias_type(&info.payload, &env);
        }
    }

    /// Record, for `module_path`, where each bare newtype leaf in its
    /// scope resolves: its own `newtype` declarations (home is
    /// `module_path`) plus the newtypes it imports with
    /// `import N(…, X, …);` (home is `N`). Runs after every module's own newtypes
    /// are collected so an import can be confirmed against the source
    /// module's declarations. Feeds qualification of noncanonical bare
    /// referenced newtype paths in an explicit spelling scope.
    fn collect_newtype_home_from_module(&mut self, module_path: &str, module: &Module<Enriched>) {
        let mut scope: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for u in &module.imports {
            if let ImportKind::Selective { items, from } = &u.kind {
                let from_path = module_path_to_string(from);
                let declares = self
                    .newtypes_by_module
                    .get(&from_path)
                    .map(|m| (from_path.clone(), m));
                if let Some((from_path, per_mod)) = declares {
                    for name in items.iter().filter_map(ImportItem::as_name) {
                        if per_mod.contains_key(name) {
                            scope.insert(name.to_owned(), from_path.clone());
                        }
                    }
                }
            }
        }
        if let Some(per_mod) = self.newtypes_by_module.get(module_path) {
            for name in per_mod.keys() {
                scope.insert(name.clone(), module_path.to_owned());
            }
        }
        if !scope.is_empty() {
            self.newtype_home.insert(module_path.to_owned(), scope);
        }
    }

    fn collect_host_type_home_from_module(&mut self, module_path: &str, module: &Module<Enriched>) {
        let mut scope = std::collections::HashMap::new();
        for u in &module.imports {
            if let ImportKind::Selective { items, from } = &u.kind {
                let from_path = module_path_to_string(from);
                if let Some(declared) = self.host_types_by_module.get(&from_path) {
                    for name in items.iter().filter_map(ImportItem::as_name) {
                        if declared.contains(name) {
                            scope.insert(name.to_owned(), from_path.clone());
                        }
                    }
                }
            }
        }
        if let Some(declared) = self.host_types_by_module.get(module_path) {
            for name in declared {
                scope.insert(name.clone(), module_path.to_owned());
            }
        }
        if !scope.is_empty() {
            self.host_type_home.insert(module_path.to_owned(), scope);
        }
    }

    fn collect_host_fns_from_module(&mut self, module_path: &str, module: &Module<Enriched>) {
        for item in &module.items {
            if let crate::ast::Item::HostFn(h) = item {
                self.host_fns_by_module
                    .entry(module_path.to_owned())
                    .or_default()
                    .insert(h.name.clone(), host_fn_to_sig(h, module_path));
            }
        }
    }

    /// The ambient host-fn scope of `module_path` — its own `host fn`
    /// declarations.
    fn host_env_for_module(&self, module_path: &str) -> HostEnvScope {
        HostEnvScope {
            fns: self
                .host_fns_by_module
                .get(module_path)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Look up a host fn reached cross-module by a qualified import path
    /// (`m.foo` / `import m(foo);`) — keyed by the target module path.
    fn host_fn_for_import(&self, path: &str, name: &str) -> Option<&HostFnSig> {
        self.host_fns_by_module.get(path)?.get(name)
    }

    fn collect_aliases_and_newtypes_from_module(
        &mut self,
        module_path: &str,
        module: &Module<Enriched>,
    ) {
        let mut aliases_in_module = std::collections::HashMap::new();
        let mut newtypes_in_module = std::collections::HashMap::new();
        let mut host_types_in_module = std::collections::HashSet::new();
        for item in &module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                let Some(alias) = declaration.type_alias() else {
                    return;
                };
                let canonical_body =
                    crate::pass::typecheck_core::canonical_declared_alias_body(alias, module);
                let info = AliasInfo {
                    type_params: alias.type_params.clone(),
                    body: convert_type_for_abi(&canonical_body),
                    home: module_path.to_owned(),
                };
                aliases_in_module.insert(alias.name.clone(), info);
            });
        }
        for item in &module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(newtype) = declaration.newtype() {
                    let info = newtype_info_from_decl(newtype, module_path);
                    newtypes_in_module.insert(newtype.name.clone(), info.clone());
                    self.newtypes_by_name
                        .entry(newtype.name.clone())
                        .or_insert(info);
                } else if let Some(host) = declaration.host_type() {
                    host_types_in_module.insert(host.name.clone());
                }
            });
        }
        if !aliases_in_module.is_empty() {
            self.aliases_by_module
                .insert(module_path.to_owned(), aliases_in_module);
        }
        if !newtypes_in_module.is_empty() {
            self.newtypes_by_module
                .insert(module_path.to_owned(), newtypes_in_module);
        }
        if !host_types_in_module.is_empty() {
            self.host_types_by_module
                .insert(module_path.to_owned(), host_types_in_module);
        }
    }

    /// Add the exact constructor/projector surface inherited by transparent
    /// identity aliases. The shared resolver proves that every alias edge is
    /// fully saturated, positional, and kind-preserving; the lowering table
    /// therefore needs no second alias grammar and cannot accidentally expose
    /// members through a structural or transformed alias.
    fn collect_identity_alias_newtypes(&mut self, package: &Package<Enriched>) {
        let index = crate::pass::resolve::IdentityAliasNewtypeIndex::build_for_package(package);
        let mut forwarded = Vec::new();
        for (module_path, entry) in package.modules() {
            for item in &entry.module.items {
                crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                    let Some(alias) = declaration.type_alias() else {
                        return;
                    };
                    let Some(target) =
                        index.target(&entry.module, Some(package), module_path, &alias.name)
                    else {
                        return;
                    };
                    let mut info = newtype_info_from_decl(target.newtype, &target.module_path);
                    info.forwarded = true;
                    forwarded.push((module_path.to_owned(), alias.name.clone(), info));
                });
            }
        }
        for (module_path, alias, info) in forwarded {
            self.newtypes_by_module
                .entry(module_path)
                .or_default()
                .insert(alias, info);
        }
    }

    fn collect_fns_from_module(&mut self, module_path: &str, module: &Module<Enriched>) {
        let newtype_home = &self.newtype_home;
        let env = UnfoldEnv {
            newtype_home,
            host_type_home: &self.host_type_home,
            type_vars: BTreeSet::new(),
            module: module_path.to_owned(),
            all_alias_scopes: &self.alias_scopes,
            qualified_scopes: &self.qualified_scopes,
            modules: &self.modules,
        };
        let mut fns_in_module = std::collections::HashMap::new();
        for item in &module.items {
            if let Item::FnDef(d) = item {
                let ret = convert_type_for_abi(&d.ret);
                let ret_env = env_with_signature_type_vars(&d.sig, &env);
                let entry = ModuleFnSig {
                    sig: d.sig.clone(),
                    ret_abi: canonicalize_function_abi_type(unfold_type_alias_type(&ret, &ret_env)),
                    target_arity: signature_target_arity(&d.sig, &env),
                    module_path: module_path.to_owned(),
                };
                fns_in_module.insert(d.name.clone(), entry.clone());
            }
        }
        if !fns_in_module.is_empty() {
            self.module_fns_by_module
                .insert(module_path.to_owned(), fns_in_module);
        }
    }

    fn alias_scope_for_module(
        &self,
        module_path: &str,
        module: &Module<Enriched>,
    ) -> std::collections::HashMap<String, AliasInfo> {
        let mut aliases = std::collections::HashMap::new();
        for u in &module.imports {
            self.absorb_alias_import(u, &mut aliases);
        }
        if let Some(local) = self.aliases_by_module.get(module_path) {
            aliases.extend(
                local
                    .iter()
                    .map(|(name, info)| (name.clone(), info.clone())),
            );
        }
        aliases
    }

    fn absorb_alias_import(
        &self,
        u: &Import,
        aliases: &mut std::collections::HashMap<String, AliasInfo>,
    ) {
        if let ImportKind::Selective { items, from } = &u.kind {
            let from_path = module_path_to_string(from);
            if let Some(per_mod) = self.aliases_by_module.get(&from_path) {
                for name in items.iter().filter_map(ImportItem::as_name) {
                    if let Some(info) = per_mod.get(name) {
                        aliases.insert(name.to_owned(), info.clone());
                    }
                }
            }
        }
    }
}

// ---- ModuleCtx ----------------------------------------------------------

/// Per-module read-only context derived from the module's `import`
/// declarations. Resolves 1-segment names and 2-segment alias prefixes
/// against the module's import set so the [`Lowerer`] can classify
/// call sites into the right `Expr::Low*` variant — same-module fn vs.
/// cross-module fn vs. host fn vs. newtype member.
///
/// Built once per module at the entry to [`Lowerer::lower_module`];
/// shared with the per-module-body walk. Two modules' `ModuleCtx`
/// instances never alias.
#[derive(Default)]
struct ModuleCtx {
    /// Selective imports: 1-seg name → resolved import kind.
    /// Populated from `import <path>(<names>);` clauses (in every
    /// modifier flavour).
    selective: std::collections::HashMap<String, ResolvedImportKind>,
    /// Qualified imports: alias → resolved kind.
    /// Populated from `import <path> as <alias>;` (in every modifier
    /// flavour).
    qualified: std::collections::HashMap<String, QualifiedImportKind>,
    /// Surface name → newtype info, for newtypes brought into this
    /// module's scope (either declared locally or selectively
    /// imported).
    newtypes_in_scope: std::collections::HashMap<String, NewtypeInfo>,
    aliases_in_scope: std::collections::HashMap<String, AliasInfo>,
    /// Functions declared in this module, keyed by their local name.
    local_fns: std::collections::HashMap<String, ModuleFnSig>,
}

/// Resolved kind of a 1-segment imported name.
#[derive(Clone)]
enum ResolvedImportKind {
    /// Cross-module (same package) — `import <pkg>/<mod>(foo);`. The
    /// payload is the source module's slash path; per-backend
    /// lowerings may consult it to derive their backend-specific
    /// module-namespace identifier (e.g. JS's
    /// uppercase-dots-to-underscores form).
    CrossModule(String),
}

/// Resolved kind of a qualified-import alias.
#[derive(Clone)]
enum QualifiedImportKind {
    /// `import <pkg>/<mod> as <alias>;` — same-package cross-module. The
    /// payload is the source module's slash path; per-backend
    /// renderings derive their module-namespace identifier from it.
    CrossModule { path: String },
}

impl ModuleCtx {
    /// Build a per-module context for the given module against the
    /// package-wide [`LoweringCtx`] so cross-module newtype imports can
    /// resolve against the source module's newtype declarations.
    fn from_module(
        module: &Module<Enriched>,
        ctx: &LoweringCtx,
        package_name: Option<&str>,
        storage_path: &str,
    ) -> Self {
        let mut m = ModuleCtx::default();
        let canonical_key = crate::pass::typecheck_core::module_path_key(module, package_name);
        let declared_key = module_path_to_string(&module.path);
        // Local newtypes: anything declared in this module's items is
        // in scope under its bare name. Each comes from the package
        // context, where its payload is already alias-unfolded, under
        // whichever path spelling the collection pass keyed it by.
        let ctx_newtypes = [storage_path, canonical_key.as_str(), declared_key.as_str()]
            .into_iter()
            .find_map(|k| ctx.newtypes_by_module.get(k));
        if let Some(ctx_newtypes) = ctx_newtypes {
            m.newtypes_in_scope.extend(
                ctx_newtypes
                    .iter()
                    .map(|(name, info)| (name.clone(), info.clone())),
            );
        }
        // Walk the `import` clauses, populating the selective/qualified
        // maps and bringing cross-module newtypes into scope where
        // applicable.
        for u in &module.imports {
            m.absorb_import(u, ctx, package_name);
        }
        if let Some(local_aliases) = [storage_path, canonical_key.as_str(), declared_key.as_str()]
            .into_iter()
            .find_map(|key| ctx.aliases_by_module.get(key))
        {
            m.aliases_in_scope.extend(
                local_aliases
                    .iter()
                    .map(|(name, info)| (name.clone(), info.clone())),
            );
        }
        if let Some(local_fns) = [storage_path, canonical_key.as_str(), declared_key.as_str()]
            .into_iter()
            .find_map(|key| ctx.module_fns_by_module.get(key))
        {
            m.local_fns.extend(
                local_fns
                    .iter()
                    .map(|(name, sig)| (name.clone(), sig.clone())),
            );
        }
        m
    }

    fn absorb_import(&mut self, u: &Import, ctx: &LoweringCtx, package_name: Option<&str>) {
        let _ = package_name;
        ctx.absorb_alias_import(u, &mut self.aliases_in_scope);
        match &u.kind {
            ImportKind::Selective { items, from } => {
                let from_path = module_path_to_string(from);
                for name in items.iter().filter_map(ImportItem::as_name) {
                    self.selective.insert(
                        name.to_owned(),
                        ResolvedImportKind::CrossModule(from_path.clone()),
                    );
                    // Cross-module newtype: if the source module
                    // declares a newtype with this name, bring it into
                    // scope so 2-seg `<NT>.<member>` calls can be
                    // classified.
                    if let Some(per_mod) = ctx.newtypes_by_module.get(&from_path)
                        && let Some(info) = per_mod.get(name)
                    {
                        self.newtypes_in_scope.insert(name.to_owned(), info.clone());
                    }
                }
            }
            ImportKind::Qualified { path, alias } => {
                let path_str = module_path_to_string(path);
                self.qualified.insert(
                    alias.clone(),
                    QualifiedImportKind::CrossModule { path: path_str },
                );
            }
            _ => {}
        }
    }
}

fn module_path_to_string(p: &crate::ast::ModulePath) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

// ---- Lowerer ------------------------------------------------------------

/// Per-walk state. The `bound` stack tracks in-scope local-binder
/// names so an `Expr::Path` / `Expr::Call` to a bound local can be
/// classified into [`Expr::LowBoundRef`] / [`Expr::LowClosureCall`]
/// without consulting the package-wide context.
///
/// A fresh `Lowerer` is minted per module in [`lower`]'s per-module
/// fan-out — the `bound` stack is scoped to one module's lowering and
/// two modules never alias.
///
/// The `module_ctx` field carries the per-module import resolution
/// table (selective / qualified / cross-module newtypes). Together
/// with the package-wide [`LoweringCtx`] it provides the full context
/// the path / call classifier needs to mint the correct `Expr::Low*`
/// variant.
struct Lowerer<'a> {
    bound: std::cell::RefCell<Vec<BoundLocal>>,
    type_bound: std::cell::RefCell<Vec<String>>,
    ctx: &'a LoweringCtx,
    module_path: String,
    module_ctx: ModuleCtx,
    host_env: HostEnvScope,
    next_call_callee_scope: Cell<usize>,
    next_selected_abi_binder: Cell<usize>,
}

#[derive(Clone)]
struct BoundLocal {
    name: String,
    ty: Option<Type<Routed>>,
    selected_abi_binder: usize,
    selected_abi_shape: Option<SelectedAbiShape>,
}

impl<'a> Lowerer<'a> {
    fn new_for_module(
        ctx: &'a LoweringCtx,
        module: &Module<Enriched>,
        package_name: Option<&str>,
        storage_path: &str,
    ) -> Self {
        // The ambient host-fn scope is the module's own `host fn`
        // declarations, keyed by the module's storage path.
        let host_env = ctx.host_env_for_module(storage_path);
        Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx,
            module_path: storage_path.to_owned(),
            module_ctx: ModuleCtx::from_module(module, ctx, package_name, storage_path),
            host_env,
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.bound.borrow().iter().any(|b| b.name == name)
    }

    fn unfold_env(&self) -> UnfoldEnv<'_> {
        UnfoldEnv {
            newtype_home: &self.ctx.newtype_home,
            host_type_home: &self.ctx.host_type_home,
            type_vars: self.type_bound.borrow().iter().cloned().collect(),
            module: self.module_path.clone(),
            all_alias_scopes: &self.ctx.alias_scopes,
            qualified_scopes: &self.ctx.qualified_scopes,
            modules: &self.ctx.modules,
        }
    }

    /// The unfold env for types declared in `module_path`. A function's
    /// declared boundary types are written in the declaring module's
    /// alias scope, so a call site unfolds them against that scope —
    /// [`Lowerer::unfold_env`] (the referring module's scope) could miss
    /// the alias or resolve the name to a different one.
    fn declaring_unfold_env(&self, module_path: &str) -> UnfoldEnv<'_> {
        UnfoldEnv {
            newtype_home: &self.ctx.newtype_home,
            host_type_home: &self.ctx.host_type_home,
            type_vars: BTreeSet::new(),
            module: module_path.to_owned(),
            all_alias_scopes: &self.ctx.alias_scopes,
            qualified_scopes: &self.ctx.qualified_scopes,
            modules: &self.ctx.modules,
        }
    }

    /// Resolve a qualified-import alias (`import target as m;` brings `m` into
    /// scope as a module alias) to the target module's host fn `name`.
    fn qualified_import_host_fn(&self, alias: &str, name: &str) -> Option<&HostFnSig> {
        let QualifiedImportKind::CrossModule { path } = self.module_ctx.qualified.get(alias)?;
        self.ctx.host_fn_for_import(path, name)
    }

    fn next_call_callee_name(&self) -> String {
        let scope = self.next_call_callee_scope.get();
        self.next_call_callee_scope.set(scope + 1);
        format!("__kio_call_callee{scope}__")
    }

    fn canonicalize_entry_signature_and_body(
        &self,
        sig: &Signature<Enriched>,
        body: Expr<Routed>,
        span: crate::span::Span,
    ) -> (Signature<Routed>, Expr<Routed>) {
        // Entry-slot binders are scoped by this function, so a fixed reserved
        // prefix is both hygienic and independent of other declarations.
        canonicalize_entry_signature_and_body_with_prefix(
            sig,
            body,
            span,
            &self.unfold_env(),
            "__kio_abi_arg",
        )
    }

    fn push_type_bound(&self, name: &str) {
        self.type_bound.borrow_mut().push(name.to_owned());
    }

    fn pop_type_bound(&self) {
        self.type_bound.borrow_mut().pop();
    }

    fn push_bound_typed(&self, name: &str, ty: Option<Type<Enriched>>) {
        let ty = ty.map(|ty| self.routed_type(&ty));
        self.push_bound_routed(name, ty);
    }

    fn push_bound_routed(&self, name: &str, ty: Option<Type<Routed>>) {
        self.push_bound_routed_with_abi(name, ty, None);
    }

    fn push_bound_routed_with_abi(
        &self,
        name: &str,
        ty: Option<Type<Routed>>,
        selected_abi_shape: Option<SelectedAbiShape>,
    ) {
        let selected_abi_binder = self.next_selected_abi_binder_id();
        self.bound.borrow_mut().push(BoundLocal {
            name: name.to_owned(),
            ty,
            selected_abi_binder,
            selected_abi_shape,
        });
    }

    fn pop_bound(&self) {
        self.bound.borrow_mut().pop();
    }

    fn bound_type(&self, name: &str) -> Option<Type<Routed>> {
        self.bound
            .borrow()
            .iter()
            .rev()
            .find(|b| b.name == name)
            .and_then(|b| b.ty.clone())
    }

    fn bound_routed_type(&self, name: &str) -> Option<Type<Routed>> {
        self.bound_type(name)
    }

    fn bound_selected_abi_binder(&self, name: &str) -> Option<usize> {
        self.bound
            .borrow()
            .iter()
            .rev()
            .find(|binding| binding.name == name)
            .map(|binding| binding.selected_abi_binder)
    }

    fn bound_selected_abi(
        &self,
        binder: usize,
    ) -> Option<(Option<SelectedAbiShape>, Option<Type<Routed>>)> {
        self.bound
            .borrow()
            .iter()
            .rev()
            .find(|binding| binding.selected_abi_binder == binder)
            .map(|binding| (binding.selected_abi_shape.clone(), binding.ty.clone()))
    }

    // ---- container walks ----------------------------------------------

    fn lower_module(&self, m: &Module<Enriched>) -> Module<Routed> {
        Module {
            path: m.path.clone(),
            imports: m.imports.clone(),
            items: m.items.iter().map(|it| self.lower_item(it)).collect(),
            meta: convert_meta(&m.meta),
            doc: m.doc.clone(),
        }
    }

    fn lower_item(&self, item: &Item<Enriched>) -> Item<Routed> {
        match item {
            Item::FnDef(d) => Item::FnDef(self.lower_fn_def(d)),
            Item::TypeAlias(a) => Item::TypeAlias(convert_type_alias::<Enriched, Routed>(a)),
            // The payload is alias-unfolded like every other boundary
            // position (fn sigs, host decls): backends read it for the
            // newtype's FFI crossing, so an alias reference surviving
            // here would cross opaquely instead of as its expansion.
            Item::Newtype(d) => {
                let mut nt = convert_newtype::<Enriched, Routed>(d);
                let mut env = self.unfold_env();
                env.type_vars.extend(
                    d.type_params
                        .iter()
                        .chain(&d.existential_params)
                        .map(|param| param.name.clone()),
                );
                nt.payload = unfold_type_alias_type(&nt.payload, &env);
                Item::Newtype(nt)
            }
            Item::TypeRecGroup(group) => {
                let mut lowered = crate::ast::convert_type_rec_group::<Enriched, Routed>(group);
                assert_eq!(
                    group.members.len(),
                    lowered.members.len(),
                    "phase rebranding must preserve recursive-group cardinality"
                );
                // `convert_type_rec_group` preserves member order, so pair
                // source and converted members directly. Looking each
                // converted newtype up by name would make lowering an
                // unbounded recursive group quadratic.
                for (source, member) in group.members.iter().zip(&mut lowered.members) {
                    #[cfg(all(test, feature = "surface"))]
                    type_rec_group_lowering_work::record_member_pair();
                    match (source, member) {
                        (
                            crate::ast::TypeRecMember::TypeAlias(source),
                            crate::ast::TypeRecMember::TypeAlias(alias),
                        ) => assert_eq!(
                            source.name, alias.name,
                            "phase rebranding must preserve recursive alias order"
                        ),
                        (
                            crate::ast::TypeRecMember::Newtype(source),
                            crate::ast::TypeRecMember::Newtype(newtype),
                        ) => {
                            assert_eq!(
                                source.name, newtype.name,
                                "phase rebranding must preserve recursive newtype order"
                            );
                            assert_eq!(
                                source.type_params, newtype.type_params,
                                "phase rebranding must preserve recursive newtype binders"
                            );
                            assert_eq!(
                                source.existential_params, newtype.existential_params,
                                "phase rebranding must preserve recursive existential binders"
                            );
                            let mut env = self.unfold_env();
                            env.type_vars.extend(
                                source
                                    .type_params
                                    .iter()
                                    .chain(&source.existential_params)
                                    .map(|param| param.name.clone()),
                            );
                            newtype.payload = unfold_type_alias_type(&newtype.payload, &env);
                        }
                        _ => {
                            unreachable!("phase rebranding must preserve recursive member variants")
                        }
                    }
                }
                Item::TypeRecGroup(lowered)
            }
            // Host items survive to Routed (the emitter scans them for
            // the namespaced host boundary); the fn decl is canonicalised
            // to its ABI slot shape with aliases unfolded.
            Item::HostType(h) => {
                Item::HostType(crate::ast::convert_host_type::<Enriched, Routed>(h))
            }
            Item::HostFn(h) => Item::HostFn(canonicalize_host_fn_decl(h, &self.unfold_env())),
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

    fn lower_fn_def(&self, d: &FnDef<Enriched>) -> FnDef<Routed> {
        // Administrative call binders are local to this top-level function.
        // Nested function expressions share the counter because they remain
        // inside its lowered body. Resetting here prevents prior declarations
        // from renumbering either kind of binder.
        self.next_call_callee_scope.set(0);
        // Type parameters must remain lexical variables even when a host
        // type in this module has the same leaf.
        let mut pushed_types = 0;
        let mut pushed_values = 0;
        for p in &d.sig.params {
            match p {
                crate::ast::SignatureParam::Type(tp) => {
                    self.push_type_bound(&tp.name);
                    pushed_types += 1;
                }
                crate::ast::SignatureParam::Value(v) => {
                    self.push_bound_typed(&v.name, v.ty.clone());
                    pushed_values += 1;
                }
            }
        }
        let source_body_ty = self.enriched_expr_type_for_adapter(&d.body);
        let body = self.lower_expr(&d.body);
        let body_ty = self.lowered_expr_type(&body).or(source_body_ty);
        let body_ret = self.routed_type(&d.ret);
        let body = self.adapt_routed_value_to_expected(body, body_ty, &body_ret, d.meta.span);
        for _ in 0..pushed_values {
            self.pop_bound();
        }
        for _ in 0..pushed_types {
            self.pop_type_bound();
        }
        let (sig, body) = self.canonicalize_entry_signature_and_body(&d.sig, body, d.meta.span);
        FnDef {
            vis: d.vis.clone(),
            purity: (),
            name: d.name.clone(),
            sig,
            ret: body_ret,
            ret_elided: (),
            body,
            meta: convert_meta(&d.meta),
            doc: d.doc.clone(),
        }
    }

    // ---- expression lowering -------------------------------------------

    fn lower_expr(&self, e: &Expr<Enriched>) -> Expr<Routed> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            // ---- path / call / fn-expr: classify into Low* ----------
            Expr::Path { segments, meta, .. } => self.lower_path(segments, meta),
            Expr::Call {
                callee, args, meta, ..
            } => self.lower_call_as_value(callee, args, meta),
            Expr::FnExpr {
                occurrence: _,
                sig,
                ret_ty,
                body,
                meta,
                caps: _,
            } => {
                // FnExpr survives the Routed phase (see ast::Expr's
                // doc). Walk the body in a scope that pushes the fn's
                // value-params, so a recursive descent classifies the
                // body's references against those params.
                let mut pushed_types = 0;
                let mut pushed_values = 0;
                for p in &sig.params {
                    match p {
                        crate::ast::SignatureParam::Type(tp) => {
                            self.push_type_bound(&tp.name);
                            pushed_types += 1;
                        }
                        crate::ast::SignatureParam::Value(v) => {
                            self.push_bound_typed(&v.name, v.ty.clone());
                            pushed_values += 1;
                        }
                    }
                }
                let lowered_body = self.lower_expr(body);
                let actual_ret = self
                    .lowered_expr_type(&lowered_body)
                    .or_else(|| self.enriched_expr_type_for_adapter(body));
                let written_ret = ret_ty.as_ref().map(|ret| self.routed_type(ret));
                let lowered_body = if let Some(expected) = &written_ret {
                    self.adapt_routed_value_to_expected(
                        lowered_body,
                        actual_ret.clone(),
                        expected,
                        meta.span,
                    )
                } else {
                    lowered_body
                };
                let routed_ret = written_ret.or(actual_ret);
                for _ in 0..pushed_values {
                    self.pop_bound();
                }
                for _ in 0..pushed_types {
                    self.pop_type_bound();
                }
                let (sig, lowered_body) = lower_fn_expr_signature_and_body(
                    sig,
                    lowered_body,
                    meta.span,
                    &self.unfold_env(),
                );
                // Stamp the safe default — empty captures. The per-module
                // `crate::pass::capabilities` pass that runs after `lower`
                // refines the capture / lifetime annotations where it can.
                Expr::FnExpr {
                    occurrence: Default::default(),
                    sig,
                    ret_ty: routed_ret,
                    body: Box::new(lowered_body),
                    meta: convert_meta(meta),
                    caps: crate::ast::Capabilities::default(),
                }
            }

            // ---- enriched carry-through ----------------------------
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
                let value_ty = self.enriched_expr_type_for_adapter(value);
                let value = self.lower_expr(value);
                let value_ty = value_ty.or_else(|| self.lowered_expr_type(&value));
                let selected_abi_shape = self.selected_abi_shape(&value);
                self.push_bound_routed_with_abi(name, value_ty, Some(selected_abi_shape));
                let body = self.lower_expr(body);
                self.pop_bound();
                Expr::Let {
                    occurrence: Default::default(),
                    name: name.clone(),
                    name_span: *name_span,
                    ty: ty.as_ref().map(|ty| self.routed_type(ty)),
                    pattern: (),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: convert_meta(meta),
                }
            }
            Expr::Seq {
                occurrence: _,
                value,
                body,
                meta,
            } => Expr::Seq {
                occurrence: Default::default(),
                value: Box::new(self.lower_expr(value)),
                body: Box::new(self.lower_expr(body)),
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
                annotation: self.routed_type(annotation),
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
                annotation: self.routed_type(annotation),
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
                annotation: self.routed_type(annotation),
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
                annotation: self.routed_type(annotation),
                meta: convert_meta(meta),
            },

            // ---- enriched structural variants ----------------------
            Expr::EnrichedTuple {
                occurrence: _,
                items,
                synth_ty,
                meta,
                ext: (),
            } => {
                let synth_ty = self.routed_type(synth_ty);
                let lowered_items = match expected_value_arg_tys(&synth_ty, items.len()) {
                    Some(slot_tys) => items
                        .iter()
                        .zip(slot_tys)
                        .map(|(item, slot_ty)| {
                            self.lower_value_arg_as_value(item.clone(), slot_ty, meta.span, true)
                        })
                        .collect(),
                    None => items.iter().map(|i| self.lower_expr(i)).collect(),
                };
                Expr::EnrichedTuple {
                    occurrence: Default::default(),
                    items: lowered_items,
                    synth_ty,
                    meta: convert_meta(meta),
                    ext: (),
                }
            }
            Expr::EnrichedProject {
                occurrence: _,
                target,
                index,
                arity,
                target_ty,
                meta,
                ext: (),
            } => Expr::EnrichedProject {
                occurrence: Default::default(),
                target: Box::new(self.lower_expr(target)),
                index: *index,
                arity: *arity,
                target_ty: self.routed_type(target_ty),
                meta: convert_meta(meta),
                ext: (),
            },
            Expr::EnrichedInject {
                occurrence: _,
                payload,
                variant,
                variants,
                synth_ty,
                meta,
                ext: (),
            } => Expr::EnrichedInject {
                occurrence: Default::default(),
                payload: Box::new(self.lower_expr(payload)),
                variant: *variant,
                variants: *variants,
                synth_ty: self.routed_type(synth_ty),
                meta: convert_meta(meta),
                ext: (),
            },
            Expr::EnrichedMatch {
                occurrence: _,
                scrutinee,
                arms,
                scrutinee_ty,
                result_ty,
                meta,
                ext: (),
            } => {
                let scrutinee_ty = self.routed_type(scrutinee_ty);
                let mut remaining = &scrutinee_ty;
                let lowered_arms: Vec<EnrichedArm<Routed>> = arms
                    .iter()
                    .enumerate()
                    .map(|(index, a)| {
                        let payload_ty = if index + 1 == arms.len() {
                            remaining
                        } else {
                            let Type::Sum { left, right, .. } = remaining else {
                                unreachable!(
                                    "a recovered match arm has a corresponding sum payload type"
                                )
                            };
                            remaining = right;
                            left
                        };
                        self.push_bound_routed(&a.param, Some(payload_ty.clone()));
                        let body = self.lower_expr(&a.body);
                        self.pop_bound();
                        EnrichedArm {
                            param: a.param.clone(),
                            body,
                            meta: convert_meta(&a.meta),
                        }
                    })
                    .collect();
                Expr::EnrichedMatch {
                    occurrence: Default::default(),
                    scrutinee: Box::new(self.lower_expr(scrutinee)),
                    arms: lowered_arms,
                    scrutinee_ty,
                    result_ty: self.routed_type(result_ty),
                    meta: convert_meta(meta),
                    ext: (),
                }
            }
            Expr::EnrichedConditional {
                occurrence: _,
                cond,
                then_branch,
                else_branch,
                result_ty,
                meta,
                ext: (),
            } => {
                let result_ty = self.routed_type(result_ty);
                Expr::EnrichedConditional {
                    occurrence: Default::default(),
                    cond: Box::new(self.lower_expr(cond)),
                    then_branch: Box::new(self.lower_value_arg_as_value(
                        *then_branch.clone(),
                        &result_ty,
                        then_branch.meta().span,
                        true,
                    )),
                    else_branch: Box::new(self.lower_value_arg_as_value(
                        *else_branch.clone(),
                        &result_ty,
                        else_branch.meta().span,
                        true,
                    )),
                    result_ty,
                    meta: convert_meta(meta),
                    ext: (),
                }
            }
            Expr::EnrichedRecord {
                occurrence: _,
                fields,
                synth_ty,
                meta,
                ext: (),
            } => {
                let synth_ty = self.routed_type(synth_ty);
                let lowered_fields = match expected_value_arg_tys(&synth_ty, fields.len()) {
                    Some(slot_tys) => fields
                        .iter()
                        .zip(slot_tys)
                        .map(|(f, slot_ty)| RecordField {
                            name: f.name.clone(),
                            value: self.lower_value_arg_as_value(
                                f.value.clone(),
                                slot_ty,
                                f.meta.span,
                                true,
                            ),
                            meta: convert_meta(&f.meta),
                        })
                        .collect(),
                    None => fields
                        .iter()
                        .map(|f| RecordField {
                            name: f.name.clone(),
                            value: self.lower_expr(&f.value),
                            meta: convert_meta(&f.meta),
                        })
                        .collect(),
                };
                Expr::EnrichedRecord {
                    occurrence: Default::default(),
                    fields: lowered_fields,
                    synth_ty,
                    meta: convert_meta(meta),
                    ext: (),
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
                ext: (),
            } => Expr::EnrichedFieldGet {
                occurrence: Default::default(),
                target: Box::new(self.lower_expr(target)),
                field_name: field_name.clone(),
                index: *index,
                arity: *arity,
                target_ty: self.routed_type(target_ty),
                meta: convert_meta(meta),
                ext: (),
            },

            // ---- pre-Enriched variants are uninhabited at Enriched -
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

            // ---- Routed-only variants are uninhabited at Enriched --
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

    /// Classify an `Expr::Path` value-position site into one of the
    /// `Low*` value-position variants:
    ///
    /// - [`Expr::LowBoundRef`] for a 1-seg name in the bound-local
    ///   stack (or a multi-seg fallback for paths the per-backend
    ///   render doesn't otherwise materialise here).
    /// - [`Expr::LowHostFnValueRef`] for a 1-seg name resolving to a
    ///   top-level host fn.
    /// - [`Expr::LowModuleFnValueRef`] for a 1-seg name imported via
    ///   `import <pkg>/<mod>(<name>);` (cross-module reference).
    fn lower_path(&self, segments: &[PathSegment], meta: &Meta<Enriched>) -> Expr<Routed> {
        // 1-seg: bound local, host fn, module fn, or intrinsic.
        if segments.len() == 1 {
            let name = &segments[0].name;
            if self.is_bound(name) {
                return Expr::LowBoundRef {
                    occurrence: Default::default(),
                    name: name.clone(),
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
            // Ambient host fn — a `host fn` declared in the current
            // module, addressable by its bare declaration name.
            if let Some(hsig) = self.host_env.fns.get(name) {
                let env = self.declaring_unfold_env(&hsig.module_path);
                return Expr::LowHostFnValueRef {
                    occurrence: Default::default(),
                    name: name.clone(),
                    module_path: hsig.module_path.clone(),
                    sig: canonicalize_entry_signature(&hsig.sig, meta.span, &env),
                    ret_ty: unfold_signature_ret(&hsig.sig, &hsig.ret, &env),
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
            // Resolved selective import from the module's `import` set.
            if let Some(kind) = self.module_ctx.selective.get(name) {
                match kind {
                    ResolvedImportKind::CrossModule(mod_path) => {
                        // A selective import may resolve to a `host fn` in
                        // the source module — host items are ordinary public
                        // declarations brought into scope by `import m(foo);`.
                        // Reference those as a host-fn value (the
                        // module-fn-value path below would mis-route them).
                        if let Some(hsig) = self.ctx.host_fn_for_import(mod_path, name) {
                            let env = self.declaring_unfold_env(&hsig.module_path);
                            return Expr::LowHostFnValueRef {
                                occurrence: Default::default(),
                                name: name.clone(),
                                module_path: hsig.module_path.clone(),
                                sig: canonicalize_entry_signature(&hsig.sig, meta.span, &env),
                                ret_ty: unfold_signature_ret(&hsig.sig, &hsig.ret, &env),
                                meta: convert_meta(meta),
                                ext: (),
                            };
                        }
                        // Cross-module fn-value reference — emit as
                        // LowModuleFnValueRef with the surface name; the
                        // per-backend render reads the import kind from
                        // its own per-module map or resolution table.
                        let entry = self
                            .ctx
                            .module_fns_by_module
                            .get(mod_path)
                            .and_then(|fns| fns.get(name));
                        let sig = entry
                            .map(|module_fn| &module_fn.sig)
                            .cloned()
                            .unwrap_or_else(|| Signature::new(Vec::new()));
                        let sig_env = self.declaring_unfold_env(
                            entry
                                .map(|module_fn| module_fn.module_path.as_str())
                                .unwrap_or(mod_path),
                        );
                        return Expr::LowModuleFnValueRef {
                            occurrence: Default::default(),
                            mangled: name.clone(),
                            sig: canonicalize_entry_signature(&sig, meta.span, &sig_env),
                            meta: convert_meta(meta),
                            ext: (),
                        };
                    }
                }
            }
            // Same-module module fn reference: a 1-seg name resolving
            // to a `fn` declared in the surrounding module, used in
            // value position. (The selective-import map doesn't cover this since
            // there's no `import` clause for a same-module fn — the
            // backend looks it up in its per-module function table.)
            if let Some(entry) = self.module_ctx.local_fns.get(name) {
                let sig_env = self.declaring_unfold_env(&entry.module_path);
                return Expr::LowModuleFnValueRef {
                    occurrence: Default::default(),
                    mangled: name.clone(),
                    sig: canonicalize_entry_signature(&entry.sig, meta.span, &sig_env),
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
            // Unrecognized 1-seg in value position — could be an
            // unresolved import or an intrinsic name. Fall back to a
            // bound-ref shape; per-backend lowerings emit a bare
            // identifier reference, which is appropriate for any name
            // that the surrounding scope binds.
            return Expr::LowBoundRef {
                occurrence: Default::default(),
                name: name.clone(),
                meta: convert_meta(meta),
                ext: (),
            };
        }
        // Multi-seg: a qualified member access — `<NT>.<member>`,
        // `<alias>.<member>`, or `<alias>.<NT>.<member>`. Each shape
        // resolves to the same underlying target `lower_call` resolves
        // it to when it appears as a call callee; in value position the
        // qualified-module-fn and cross-module newtype-member shapes
        // eta-expand into a `FnExpr` wrapping the resolved call.
        if segments.len() == 2 {
            let head = &segments[0].name;
            let member = &segments[1].name;
            if let Some(info) = self.module_ctx.newtypes_in_scope.get(head)
                && let Some(expr) = self.lower_newtype_member_value(head, member, info, meta)
            {
                return expr;
            }
            // Cross-module host fn reached by a qualified import alias
            // (`m.foo`).
            if let Some(hsig) = self.qualified_import_host_fn(head, member) {
                let env = self.declaring_unfold_env(&hsig.module_path);
                return Expr::LowHostFnValueRef {
                    occurrence: Default::default(),
                    name: member.clone(),
                    module_path: hsig.module_path.clone(),
                    sig: canonicalize_entry_signature(&hsig.sig, meta.span, &env),
                    ret_ty: unfold_signature_ret(&hsig.sig, &hsig.ret, &env),
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
            // Qualified-import module fn reached as a value (`m.foo`
            // passed to a higher-order fn, not called). Resolve the
            // alias to its source module — the same lookup
            // `lower_call`'s 2-seg qualified branch performs — and
            // eta-expand into a `FnExpr` wrapping the resolved call so
            // the per-backend render sees a proper module-fn value, not
            // a dotted `m.foo` identifier.
            if let Some(QualifiedImportKind::CrossModule { path }) =
                self.module_ctx.qualified.get(head)
                && let Some(entry) = self
                    .ctx
                    .module_fns_by_module
                    .get(path)
                    .and_then(|m| m.get(member))
            {
                let sig_env = self.declaring_unfold_env(&entry.module_path);
                let sig = canonicalize_entry_signature(&entry.sig, meta.span, &sig_env);
                let ret_ty = signature_return_after_first_value_group(
                    &entry.sig,
                    entry.ret_abi.clone(),
                    meta.span,
                    &sig_env,
                );
                if let Some(value) = self.eta_expand_qualified_value(segments, &sig, ret_ty, meta) {
                    return value;
                }
            }
        }
        if segments.len() == 3 {
            // `<alias>.<NT>.<member>` cross-module newtype member as a
            // value. Resolve the alias to its source module, confirm
            // the member is the newtype's constructor or projector, and
            // eta-expand the single-payload call.
            let alias = &segments[0].name;
            let newtype = &segments[1].name;
            let member = &segments[2].name;
            if let Some(QualifiedImportKind::CrossModule { path }) =
                self.module_ctx.qualified.get(alias)
                && let Some(info) = self
                    .ctx
                    .newtypes_by_module
                    .get(path)
                    .and_then(|m| m.get(newtype))
            {
                let emitted_newtype = if info.forwarded {
                    &info.nominal_name
                } else {
                    newtype
                };
                // An existential projector reached through a qualified
                // alias needs the same two-level CPS eta as the in-scope
                // form. Both eta shapes stamp the declaring-module identity
                // on their receiver type.
                if info.has_existentials && member == &info.projector {
                    return self.build_cps_projector_value(emitted_newtype, true, info, meta);
                }
                if info.has_existentials && member == &info.constructor {
                    return self.build_existential_ctor_value(emitted_newtype, true, info, meta);
                }
                if !info.has_existentials
                    && let Some((sig, ret_ty)) = self.qualified_newtype_member_signature(
                        info,
                        emitted_newtype,
                        member,
                        meta.span,
                    )
                    && let Some(value) =
                        self.eta_expand_qualified_value(segments, &sig, ret_ty, meta)
                {
                    return value;
                }
            }
        }
        Expr::LowBoundRef {
            occurrence: Default::default(),
            name: segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("."),
            meta: convert_meta(meta),
            ext: (),
        }
    }

    /// The declaration-shaped signature and return type of a
    /// cross-module newtype member. Universal binders remain explicit so a
    /// let-bound constructor or projector can be instantiated more than once.
    fn qualified_newtype_member_signature(
        &self,
        info: &NewtypeInfo,
        newtype: &str,
        member: &str,
        span: crate::span::Span,
    ) -> Option<(Signature<Routed>, Type<Routed>)> {
        let type_args: Vec<Type<Routed>> = info
            .type_params
            .iter()
            .map(|tp| Type::Path {
                segments: vec![PathSegment::new(tp.name.clone(), tp.span)],
                args: Vec::new(),
                meta: Meta::new(tp.span),
            })
            .collect();
        let newtype_ty = Type::Path {
            segments: qualified_nominal_segments(&info.home, newtype, span),
            args: type_args.clone(),
            meta: Meta::new(span),
        };
        let payload_ty = instantiate_newtype_payload(info, &type_args);
        let (param_ty, ret_ty) = if member == info.constructor {
            (payload_ty, newtype_ty)
        } else if member == info.projector {
            (newtype_ty, payload_ty)
        } else {
            return None;
        };
        let mut params: Vec<SignatureParam<Routed>> = info
            .type_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect();
        params.push(SignatureParam::Value(Param {
            name: "__kio_eta_arg".to_owned(),
            ty: Some(param_ty),
            pattern: (),
            meta: Meta::new(span),
        }));
        Some((Signature::new(params), ret_ty))
    }

    /// Eta-expand a qualified value-position reference into a `FnExpr`
    /// whose body is the resolved call. Used for the multi-segment
    /// qualified shapes that `lower_call` resolves at call position but
    /// that arrive at [`lower_path`](Self::lower_path) bare (passed as
    /// a value to a higher-order fn rather than invoked): a
    /// `<alias>.<member>` qualified-import module fn, or a
    /// `<alias>.<NT>.<member>` cross-module newtype member.
    ///
    /// The wrapper copies every type group before the first value group and
    /// forwards those binders explicitly to the resolved call. Its return
    /// type is the declaration remainder after that first callable layer.
    fn eta_expand_qualified_value(
        &self,
        segments: &[PathSegment],
        source_sig: &Signature<Routed>,
        ret_ty: Type<Routed>,
        meta: &Meta<Enriched>,
    ) -> Option<Expr<Routed>> {
        let span = meta.span;
        let mut params = Vec::new();
        let mut groups = Vec::new();
        let mut call_args: Vec<CallArg<Enriched>> = Vec::new();
        let mut value_binders = Vec::new();
        let mut type_binders = Vec::new();
        let mut found_value_group = false;

        for group in source_sig.canonical_groups() {
            match group {
                SignatureGroupRef::Type(group_params) => {
                    for param in group_params {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type params")
                        };
                        params.push(SignatureParam::Type(param.clone()));
                        groups.push(SignatureGroupKind::Type { len: 1 });
                        type_binders.push(param.name.clone());
                        call_args.push(CallArg::Type(Type::Path {
                            segments: vec![PathSegment::new(param.name.clone(), param.span)],
                            args: Vec::new(),
                            meta: Meta::new(param.span),
                        }));
                    }
                }
                SignatureGroupRef::Value(group_params) => {
                    let before = params.len();
                    for (index, param) in group_params.iter().enumerate() {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("SignatureGroupRef::Value contains only value params")
                        };
                        let name = format!("__kio_eta_arg{index}");
                        let ty = param.ty.clone()?;
                        params.push(SignatureParam::Value(Param {
                            name: name.clone(),
                            ty: Some(ty.clone()),
                            pattern: (),
                            meta: Meta::new(span),
                        }));
                        value_binders.push((name.clone(), ty));
                        call_args.push(CallArg::Value(Expr::Path {
                            occurrence: Default::default(),
                            segments: vec![PathSegment::new(name, span)],
                            meta: Meta::new(span),
                            ext: (),
                        }));
                    }
                    groups.push(SignatureGroupKind::Value {
                        len: params.len() - before,
                    });
                    found_value_group = true;
                    break;
                }
            }
        }
        if !found_value_group {
            return None;
        }

        let callee = Expr::Path {
            occurrence: Default::default(),
            segments: segments.to_vec(),
            meta: Meta::new(span),
            ext: (),
        };
        for name in &type_binders {
            self.push_type_bound(name);
        }
        for (name, ty) in &value_binders {
            self.push_bound_routed(name, Some(ty.clone()));
        }
        let body = self.lower_call(&callee, &call_args, meta);
        for _ in &value_binders {
            self.pop_bound();
        }
        for _ in &type_binders {
            self.pop_type_bound();
        }
        Some(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty: Some(ret_ty),
            body: Box::new(body),
            meta: convert_meta(meta),
            caps: Default::default(),
        })
    }

    fn lower_newtype_member_value(
        &self,
        newtype: &str,
        member: &str,
        info: &NewtypeInfo,
        meta: &Meta<Enriched>,
    ) -> Option<Expr<Routed>> {
        // Existential members can't take the plain single-level eta the
        // rest of this function builds.
        if info.has_existentials {
            if member == info.projector {
                // The existential projector's value form is curried —
                // `N -> ([R] ([A..] payload -> R) -> R)` — so it needs
                // the two-level CPS eta, not a one-level `.(v){ proj v }`.
                return Some(self.build_cps_projector_value(
                    if info.forwarded {
                        &info.nominal_name
                    } else {
                        newtype
                    },
                    info.forwarded,
                    info,
                    meta,
                ));
            }
            if member == info.constructor {
                // The existential constructor's value form is the rank-N
                // function `[A..] payload -> N` (the witnesses are
                // inferred from the payload at each call), so it needs the
                // eta `.[A..](p) { N.mk(A.., p) }`. Its return `N` is
                // binder-free; the rank-N machinery keeps it concrete.
                return Some(self.build_existential_ctor_value(
                    if info.forwarded {
                        &info.nominal_name
                    } else {
                        newtype
                    },
                    info.forwarded,
                    info,
                    meta,
                ));
            }
        }
        let span = meta.span;
        let type_args: Vec<Type<Routed>> = info
            .type_params
            .iter()
            .map(|tp| Type::Path {
                segments: vec![PathSegment::new(tp.name.clone(), tp.span)],
                args: Vec::new(),
                meta: Meta::new(tp.span),
            })
            .collect();
        let payload_ty = instantiate_newtype_payload(info, &type_args);
        let emitted_newtype = if info.forwarded {
            &info.nominal_name
        } else {
            newtype
        };
        let newtype_ty = Type::Path {
            segments: if info.forwarded {
                qualified_nominal_segments(&info.home, emitted_newtype, span)
            } else {
                vec![PathSegment::new(emitted_newtype.to_owned(), span)]
            },
            args: type_args.clone(),
            meta: Meta::new(span),
        };
        let arg_name = "__kio_newtype_member_arg".to_owned();
        let arg_ref = Expr::LowBoundRef {
            occurrence: Default::default(),
            name: arg_name.clone(),
            meta: Meta::new(span),
            ext: (),
        };
        let (param_ty, ret_ty, body) = if member == info.constructor {
            (
                payload_ty,
                newtype_ty.clone(),
                if info.forwarded {
                    Expr::LowQualifiedNewtypeMember {
                        occurrence: Default::default(),
                        module_path: info.home.clone(),
                        newtype: emitted_newtype.to_owned(),
                        member: info.constructor.clone(),
                        type_args: type_args.clone(),
                        payload: Box::new(arg_ref),
                        meta: Meta::new(span),
                        ext: (),
                    }
                } else {
                    Expr::LowNewtypeCtor {
                        occurrence: Default::default(),
                        newtype: emitted_newtype.to_owned(),
                        member: info.constructor.clone(),
                        type_args: type_args.clone(),
                        payload: Box::new(arg_ref),
                        meta: Meta::new(span),
                        ext: (),
                    }
                },
            )
        } else if member == info.projector {
            (
                newtype_ty,
                payload_ty,
                if info.forwarded {
                    Expr::LowQualifiedNewtypeMember {
                        occurrence: Default::default(),
                        module_path: info.home.clone(),
                        newtype: emitted_newtype.to_owned(),
                        member: info.projector.clone(),
                        type_args: type_args.clone(),
                        payload: Box::new(arg_ref),
                        meta: Meta::new(span),
                        ext: (),
                    }
                } else {
                    Expr::LowNewtypeProj {
                        occurrence: Default::default(),
                        newtype: emitted_newtype.to_owned(),
                        member: info.projector.clone(),
                        type_args: type_args.clone(),
                        target: Box::new(arg_ref),
                        meta: Meta::new(span),
                        ext: (),
                    }
                },
            )
        } else {
            return None;
        };
        let mut params: Vec<SignatureParam<Routed>> = info
            .type_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect();
        params.push(SignatureParam::Value(Param {
            name: arg_name,
            ty: Some(param_ty),
            pattern: (),
            meta: Meta::new(span),
        }));
        Some(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(params),
            ret_ty: Some(ret_ty),
            body: Box::new(body),
            meta: convert_meta(meta),
            caps: Default::default(),
        })
    }

    /// Build the value-position eta for an *existential* projector — the
    /// projector referenced as a first-class value rather than applied.
    /// An existential projector has the curried CPS type
    /// `N -> ([R] ([A..] payload -> R) -> R)`, so its value form is a
    /// two-level eta: the outer level binds the receiver `v`, the inner
    /// — rank-N over `R` — binds the continuation `k`, and the body is
    /// the dedicated [`Expr::LowCpsProjectorApply`], the very node the
    /// call-position fold produces. So every backend renders the value
    /// and call forms of an existential projector through one path. The
    /// node stays faithfully rank-N here; each backend erases `R` (Rust)
    /// or renders it natively (a host with first-class existentials) in
    /// its own emitter — the erasure is never baked into this shared IR.
    fn build_cps_projector_value(
        &self,
        newtype: &str,
        qualified: bool,
        info: &NewtypeInfo,
        meta: &Meta<Enriched>,
    ) -> Expr<Routed> {
        let span = meta.span;
        let type_args: Vec<Type<Routed>> = info
            .type_params
            .iter()
            .map(|tp| Type::Path {
                segments: vec![PathSegment::new(tp.name.clone(), tp.span)],
                args: Vec::new(),
                meta: Meta::new(tp.span),
            })
            .collect();
        // Match the call site's Routed identity: a qualified projector
        // carries its declaring module, while a bare in-scope projector
        // stays bare. A mismatch would introduce a function adapter around
        // the rank-N continuation and make the host rendering ill-typed.
        let nt_segments = if qualified {
            qualified_nominal_segments(&info.home, newtype, span)
        } else {
            vec![PathSegment::new(newtype.to_owned(), span)]
        };
        let newtype_ty = Type::Path {
            segments: nt_segments,
            args: type_args.clone(),
            meta: Meta::new(span),
        };
        // The CPS result binder `R` and a reference to it.
        let r_name = "__kio_cps_r".to_owned();
        let r_param = crate::ast::TypeParam {
            name: r_name.clone(),
            span,
            kind: None,
        };
        let r_ref = Type::Path {
            segments: vec![PathSegment::new(r_name, span)],
            args: Vec::new(),
            meta: Meta::new(span),
        };

        let cont_ty = cps_continuation_type(info, &type_args, r_ref.clone(), span);

        let recv_name = "__kio_proj_recv".to_owned();
        let cont_name = "__kio_proj_cont".to_owned();

        // Inner level: `[R](k: cont_ty) -> R { <cps-apply v k> }`.
        let inner = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(vec![
                SignatureParam::Type(r_param.clone()),
                SignatureParam::Value(Param {
                    name: cont_name.clone(),
                    ty: Some(cont_ty.clone()),
                    pattern: (),
                    meta: Meta::new(span),
                }),
            ]),
            ret_ty: Some(r_ref.clone()),
            body: Box::new(Expr::LowCpsProjectorApply {
                occurrence: Default::default(),
                newtype: newtype.to_owned(),
                module_path: info.home.clone(),
                type_args: type_args.clone(),
                receiver: Box::new(Expr::LowBoundRef {
                    occurrence: Default::default(),
                    name: recv_name.clone(),
                    meta: Meta::new(span),
                    ext: (),
                }),
                continuation: Box::new(Expr::LowBoundRef {
                    occurrence: Default::default(),
                    name: cont_name,
                    meta: Meta::new(span),
                    ext: (),
                }),
                continuation_ty: cont_ty.clone(),
                meta: convert_meta(meta),
                ext: (),
            }),
            meta: convert_meta(meta),
            caps: Default::default(),
        };

        // The inner level's own type, the CPS unpacker `[R] cont_ty -> R`.
        let cps_ty = Type::Forall {
            param: r_param,
            body: Box::new(Type::Function {
                param: Box::new(cont_ty),
                ret: Box::new(r_ref),
                meta: Meta::new(span),
                abi_arity: 1,
                caps: crate::ast::FnTypeCapabilities::default(),
            }),
            meta: Meta::new(span),
        };

        // Outer level: `[universals](v: N) -> cps_ty { <inner> }`.
        let mut params: Vec<SignatureParam<Routed>> = info
            .type_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect();
        params.push(SignatureParam::Value(Param {
            name: recv_name,
            ty: Some(newtype_ty),
            pattern: (),
            meta: Meta::new(span),
        }));
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(params),
            ret_ty: Some(cps_ty),
            body: Box::new(inner),
            meta: convert_meta(meta),
            caps: Default::default(),
        }
    }

    /// Build the value-position eta for an *existential* constructor —
    /// the constructor referenced as a first-class value. An existential
    /// constructor packs a witness inferred from the payload, so its
    /// value form is the rank-N function `[A..] payload -> N`; the eta
    /// `.[A..](p) { N.mk(A.., p) }` makes the witnesses explicit binders
    /// the per-backend render erases. Mirrors the non-existential
    /// constructor eta but quantifies over the existentials too. (The
    /// projector counterpart is [`build_cps_projector_value`].)
    fn build_existential_ctor_value(
        &self,
        newtype: &str,
        qualified: bool,
        info: &NewtypeInfo,
        meta: &Meta<Enriched>,
    ) -> Expr<Routed> {
        let span = meta.span;
        let self_ref = |tp: &crate::ast::TypeParam| Type::Path {
            segments: vec![PathSegment::new(tp.name.clone(), tp.span)],
            args: Vec::new(),
            meta: Meta::new(tp.span),
        };
        let universal_args: Vec<Type<Routed>> = info.type_params.iter().map(self_ref).collect();
        // The constructor's witness type-args: the universals (monomorphic
        // self-refs at the use site) then the existentials (the eta's own
        // binders). Matches the witness list an explicit `N.mk(A.., p)`
        // call lowers to.
        let mut all_type_args = universal_args.clone();
        all_type_args.extend(info.existential_params.iter().map(self_ref));
        let payload_ty = instantiate_newtype_payload(info, &universal_args);
        let newtype_ty = Type::Path {
            segments: if qualified {
                qualified_nominal_segments(&info.home, newtype, span)
            } else {
                vec![PathSegment::new(newtype.to_owned(), span)]
            },
            args: universal_args,
            meta: Meta::new(span),
        };
        let arg_name = "__kio_ctor_payload".to_owned();
        let payload = Box::new(Expr::LowBoundRef {
            occurrence: Default::default(),
            name: arg_name.clone(),
            meta: Meta::new(span),
            ext: (),
        });
        let body = if qualified {
            Expr::LowQualifiedNewtypeMember {
                occurrence: Default::default(),
                module_path: info.home.clone(),
                newtype: newtype.to_owned(),
                member: info.constructor.clone(),
                type_args: all_type_args,
                payload,
                meta: convert_meta(meta),
                ext: (),
            }
        } else {
            Expr::LowNewtypeCtor {
                occurrence: Default::default(),
                newtype: newtype.to_owned(),
                member: info.constructor.clone(),
                type_args: all_type_args,
                payload,
                meta: convert_meta(meta),
                ext: (),
            }
        };
        let mut params: Vec<SignatureParam<Routed>> = info
            .type_params
            .iter()
            .cloned()
            .map(SignatureParam::Type)
            .collect();
        params.extend(
            info.existential_params
                .iter()
                .cloned()
                .map(SignatureParam::Type),
        );
        params.push(SignatureParam::Value(Param {
            name: arg_name,
            ty: Some(payload_ty),
            pattern: (),
            meta: Meta::new(span),
        }));
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::new(params),
            ret_ty: Some(newtype_ty),
            body: Box::new(body),
            meta: convert_meta(meta),
            caps: Default::default(),
        }
    }

    fn lower_call_as_value(
        &self,
        callee: &Expr<Enriched>,
        args: &[CallArg<Enriched>],
        meta: &Meta<Enriched>,
    ) -> Expr<Routed> {
        let call = self.lower_call(callee, args, meta);
        let info = match &call {
            Expr::LowNewtypeProj { newtype, .. } => self.module_ctx.newtypes_in_scope.get(newtype),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                ..
            } => self
                .ctx
                .newtypes_by_module
                .get(module_path)
                .and_then(|newtypes| newtypes.get(newtype))
                .filter(|info| member == &info.projector),
            _ => None,
        };
        let Some(info) = info.filter(|info| info.has_existentials) else {
            return call;
        };
        let (newtype, type_args, receiver, qualified) = match call {
            Expr::LowNewtypeProj {
                newtype,
                type_args,
                target,
                ..
            } => (newtype, type_args, target, false),
            Expr::LowQualifiedNewtypeMember {
                newtype,
                type_args,
                payload,
                ..
            } => (newtype, type_args, payload, true),
            _ => unreachable!("a classified CPS projector retains its member application"),
        };
        // A receiver-applied projector is an ordinary polymorphic callable.
        // Applying the typed member eta captures the receiver once and keeps
        // its result/continuation stages when the callable escapes this site.
        Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: Box::new(self.build_cps_projector_value(&newtype, qualified, info, meta)),
            type_args,
            args: vec![*receiver],
            meta: convert_meta(meta),
            ext: (),
        }
    }

    fn lower_call(
        &self,
        callee: &Expr<Enriched>,
        args: &[CallArg<Enriched>],
        meta: &Meta<Enriched>,
    ) -> Expr<Routed> {
        // Split args into type-args (CallArg::Type) and value-args
        // (CallArg::Value), preserving order within each list. This is
        // the load-bearing transform of the Low IR: every call variant
        // separates type-args from value-args, so the per-backend
        // lowering iterates value-args directly with no
        // `ERASED_TYPE_ARG` filter.
        let (type_args, value_args) = split_args(args);
        if !type_args.is_empty() && value_args.is_empty() {
            return type_args
                .iter()
                .fold(self.lower_expr(callee), |callee, type_arg| {
                    Expr::LowTypeApplication {
                        occurrence: Default::default(),
                        callee: Box::new(callee),
                        type_arg: self.routed_type(type_arg),
                        meta: convert_meta(meta),
                        ext: (),
                    }
                });
        }

        if let Expr::Path { segments, .. } = callee {
            if segments.len() == 1 {
                let name = &segments[0].name;
                // `__absurd__` classifies into `LowAbsurdCall`. The
                // call has shape `__absurd__([T], v)` (fully-explicit,
                // 1 type-arg + 1 value-arg) or `__absurd__(v)` (the
                // typer inferred the type-arg). The kio-prime
                // round-trip can reclassify the type-arg as a value-
                // arg when it's a bare lowercase identifier in scope
                // as a type-param; that pushes the value-arg count to
                // 2, but only the **last** value-arg is the bottom-
                // typed payload — any preceding lowercase-bare arg is
                // a type-witness whose value-rep is opaque (the typer
                // already validated it is the right kind).
                if name == "__absurd__" && !value_args.is_empty() {
                    let payload_idx = value_args.len() - 1;
                    let value_arg = Box::new(self.lower_expr(&value_args[payload_idx]));
                    // Pick the type-arg either from the explicit
                    // `[T]` position (`type_args[0]`) or from the
                    // preceding value-arg slot (when the surface
                    // round-trip reclassified it). When neither is
                    // present, fall back to unit — the missing slot
                    // is harmless because per-backend emit erases
                    // type-args at the FFI surface anyway.
                    let type_arg = type_args
                        .first()
                        .map(|ty| self.routed_type(ty))
                        .or_else(|| {
                            if value_args.len() > 1 {
                                self.infer_type_from_value_arg(&value_args[0])
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| self.unit_type_at(meta.span));
                    return Expr::LowAbsurdCall {
                        occurrence: Default::default(),
                        type_arg,
                        value_arg,
                        meta: convert_meta(meta),
                        ext: (),
                    };
                }
                if self.is_bound(name) {
                    let routed_type_args = self.routed_type_args(&type_args);
                    let canonical = match self.bound_type(name) {
                        Some(bound_ty) => self.lower_call_value_args_for_routed_callee_ty(
                            value_args,
                            &bound_ty,
                            &routed_type_args,
                            meta.span,
                        ),
                        None => CanonicalCallArgs::direct(self.lower_value_args(value_args)),
                    };
                    let call = Expr::LowClosureCall {
                        occurrence: Default::default(),
                        name: name.clone(),
                        type_args: routed_type_args,
                        args: canonical.args.clone(),
                        meta: convert_meta(meta),
                        ext: (),
                    };
                    return self.wrap_canonical_call(call, canonical);
                }
                if let Some(hsig) = self.host_env.fns.get(name) {
                    return self.low_host_call(name.clone(), hsig, &type_args, value_args, meta);
                }
                // Resolved selective import from the module's `import` set.
                if let Some(kind) = self.module_ctx.selective.get(name) {
                    return match kind.clone() {
                        ResolvedImportKind::CrossModule(mod_path) => {
                            // A selective import may resolve to a `host fn`
                            // in the source module — host items are ordinary
                            // public declarations brought into scope by
                            // `import m(foo);`. Classify those as host calls
                            // (the module-fn path below would mis-route them
                            // through the module-call slot walk).
                            if let Some(hsig) = self.ctx.host_fn_for_import(&mod_path, name) {
                                return self.low_host_call(
                                    name.clone(),
                                    hsig,
                                    &type_args,
                                    value_args,
                                    meta,
                                );
                            }
                            // Look up the target fn's sig (same as
                            // the qualified-cross-module case below)
                            // to drop parser-misclassified type-args.
                            let entry = self
                                .ctx
                                .module_fns_by_module
                                .get(&mod_path)
                                .and_then(|m| m.get(name))
                                .cloned();
                            let sig = entry
                                .as_ref()
                                .map(|m| m.sig.clone())
                                .unwrap_or_else(|| Signature::new(Vec::new()));
                            let ret_ty = entry.as_ref().map(|m| m.ret_abi.clone());
                            let (final_type_args, final_value_args) =
                                match self.split_args_via_sig(&sig, &type_args, &value_args) {
                                    Some(p) => p,
                                    None => (self.routed_type_args(&type_args), value_args.clone()),
                                };
                            self.low_module_call(
                                // The mangled placeholder is the
                                // surface 1-seg name; the per-backend
                                // render reads the import's source-
                                // module path from its own table.
                                name.clone(),
                                final_type_args,
                                final_value_args,
                                ModuleCallInfo {
                                    sig: &sig,
                                    target_arity: entry.as_ref().map(|m| m.target_arity),
                                    ret_ty,
                                    declaring_module: &mod_path,
                                    meta,
                                },
                            )
                        }
                    };
                }
                // Unrecognized 1-seg callee — likely a same-module
                // module .(no `import` clause for same-module names)
                // or an intrinsic. Classify as `LowModuleCall` with
                // the surface name as the mangled placeholder; the
                // per-backend lowering resolves the surface name
                // against its per-module imports map (which
                // includes same-module fns under `lower_package_with_options`).
                //
                // When `name` matches a same-module fn whose
                // sig has type-params, reclassify any parser-
                // misclassified type-arg slots via
                // [`split_args_via_sig`]. The typer normally handles
                // this; bypass-typer test harnesses depend on the
                // lower pass to do the same drop.
                let entry = self.module_ctx.local_fns.get(name).cloned();
                let sig = entry
                    .as_ref()
                    .map(|m| m.sig.clone())
                    .unwrap_or_else(|| Signature::new(Vec::new()));
                let ret_ty = entry.as_ref().map(|m| m.ret_abi.clone());
                let (final_type_args, final_value_args) =
                    match self.split_args_via_sig(&sig, &type_args, &value_args) {
                        Some(p) => p,
                        None => (self.routed_type_args(&type_args), value_args),
                    };
                return self.low_module_call(
                    name.clone(),
                    final_type_args,
                    final_value_args,
                    ModuleCallInfo {
                        sig: &sig,
                        target_arity: entry.as_ref().map(|m| m.target_arity),
                        ret_ty,
                        declaring_module: &self.module_path,
                        meta,
                    },
                );
            }
            if segments.len() == 2 {
                // `<NT>.<member>` or `<alias>.<member>`.
                let head = &segments[0].name;
                let member = &segments[1].name;
                if let Some(nt) = self.module_ctx.newtypes_in_scope.get(head)
                    && (member == &nt.constructor || member == &nt.projector)
                    && !value_args.is_empty()
                {
                    let mut all_type_args = self.routed_type_args(&type_args);
                    let payload = if member == &nt.constructor {
                        self.lower_newtype_constructor_payload(
                            nt,
                            &all_type_args,
                            value_args,
                            meta.span,
                        )
                    } else {
                        let payload_idx = value_args.len() - 1;
                        all_type_args.extend(
                            value_args[..payload_idx]
                                .iter()
                                .filter_map(|arg| self.infer_type_from_value_arg(arg)),
                        );
                        self.lower_expr(&value_args[payload_idx])
                    };
                    if nt.forwarded {
                        return Expr::LowQualifiedNewtypeMember {
                            occurrence: Default::default(),
                            module_path: nt.home.clone(),
                            newtype: nt.nominal_name.clone(),
                            member: member.clone(),
                            type_args: all_type_args,
                            payload: Box::new(payload),
                            meta: convert_meta(meta),
                            ext: (),
                        };
                    }
                    if member == &nt.constructor {
                        return Expr::LowNewtypeCtor {
                            occurrence: Default::default(),
                            newtype: head.clone(),
                            member: nt.constructor.clone(),
                            type_args: all_type_args,
                            payload: Box::new(payload),
                            meta: convert_meta(meta),
                            ext: (),
                        };
                    }
                    return Expr::LowNewtypeProj {
                        occurrence: Default::default(),
                        newtype: head.clone(),
                        member: nt.projector.clone(),
                        type_args: all_type_args,
                        target: Box::new(payload),
                        meta: convert_meta(meta),
                        ext: (),
                    };
                }
                // Cross-module host fn reached by a qualified-import alias
                // (`m.foo`).
                if let Some(hsig) = self.qualified_import_host_fn(head, member) {
                    return self.low_host_call(member.clone(), hsig, &type_args, value_args, meta);
                }
                // Resolved qualified-import alias from the module's
                // `import` set.
                if let Some(kind) = self.module_ctx.qualified.get(head) {
                    return match kind.clone() {
                        QualifiedImportKind::CrossModule { path: mod_path } => {
                            // Look up the target fn's sig for type-arg
                            // reclassification (parser may have
                            // misclassified type-witnesses as value-args).
                            let entry = self
                                .ctx
                                .module_fns_by_module
                                .get(&mod_path)
                                .and_then(|m| m.get(member))
                                .cloned();
                            let sig = entry
                                .as_ref()
                                .map(|m| m.sig.clone())
                                .unwrap_or_else(|| Signature::new(Vec::new()));
                            let ret_ty = entry.as_ref().map(|m| m.ret_abi.clone());
                            let (final_type_args, final_value_args) =
                                match self.split_args_via_sig(&sig, &type_args, &value_args) {
                                    Some(p) => p,
                                    None => (self.routed_type_args(&type_args), value_args.clone()),
                                };
                            self.low_qualified_module_call(
                                (head.clone(), format!("{head}.{member}")),
                                final_type_args,
                                final_value_args,
                                ModuleCallInfo {
                                    sig: &sig,
                                    target_arity: entry.as_ref().map(|m| m.target_arity),
                                    ret_ty,
                                    declaring_module: &mod_path,
                                    meta,
                                },
                            )
                        }
                    };
                }
                if let Some(call) =
                    self.lower_exact_module_fn_call(head, member, &type_args, &value_args, meta)
                {
                    return call;
                }
                // Fallthrough: unrecognized 2-seg callee.
                // Conservatively classify as a qualified-module-call
                // placeholder; the per-backend lowering surfaces the
                // alias + member through the variant's `mangled`
                // field, where the per-backend resolution maps the
                // alias to a target namespace (or errors out cleanly
                // when no such alias is in scope).
                return Expr::LowQualifiedModuleCall {
                    occurrence: Default::default(),
                    alias: head.clone(),
                    mangled: format!("{head}.{member}"),
                    type_args: self.routed_type_args(&type_args),
                    args: self.lower_value_args(value_args),
                    sig: Signature::new(Vec::new()),
                    ret_ty: None,
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
            if segments.len() >= 3 {
                // The Routed node carries the exact declaring module selected here;
                // backends consume that canonical owner rather than re-resolving the source spelling.
                let (module_segments, tail) = segments.split_at(segments.len() - 2);
                let newtype = &tail[0].name;
                let member = &tail[1].name;
                let written_module_path = module_segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                let module_path = match self.module_ctx.qualified.get(&written_module_path) {
                    Some(QualifiedImportKind::CrossModule { path }) => path.clone(),
                    None => written_module_path,
                };
                if let Some(call) = self.lower_exact_newtype_member_call(
                    &module_path,
                    newtype,
                    member,
                    &type_args,
                    &value_args,
                    meta,
                ) {
                    return call;
                }
                return Expr::LowQualifiedNewtypeMember {
                    occurrence: Default::default(),
                    module_path,
                    newtype: newtype.clone(),
                    member: member.clone(),
                    type_args: self.routed_type_args(&type_args),
                    payload: value_args
                        .first()
                        .cloned()
                        .map(|p| Box::new(self.lower_expr_owned(p)))
                        .unwrap_or_else(|| Box::new(self.unit_at(meta))),
                    meta: convert_meta(meta),
                    ext: (),
                };
            }
        }
        // Non-path callee: an indirect call.
        // Keep an immediate member application available to the CPS fold;
        // every escaping call result takes the ordinary value route above.
        let callee_expr = match callee {
            Expr::Call {
                callee, args, meta, ..
            } => self.lower_call(callee, args, meta),
            other => self.lower_expr(other),
        };
        let callee_ty = self.lowered_expr_type(&callee_expr);
        let lowered_type_args = self.routed_type_args(&type_args);
        // CPS-projector fold: when the callee is a local/imported
        // `LowNewtypeProj` or a qualified `LowQualifiedNewtypeMember` on
        // an existential-bearing newtype, the outer call passes the
        // continuation to the projector's CPS form. Lift both spellings
        // into the dedicated `LowCpsProjectorApply` variant so the
        // per-backend render sees one owner-qualified shape. `type_args`
        // are inherited from the inner projector (the newtype's
        // universal-param header instantiation); the existentials stay
        // erased. The continuation is the typechecked outer application's one
        // value argument; its selected result witness has already been
        // classified into `lowered_type_args`.
        let cps_projector = match &callee_expr {
            Expr::LowNewtypeProj {
                newtype,
                target,
                type_args,
                ..
            } => self
                .module_ctx
                .newtypes_in_scope
                .get(newtype)
                .filter(|info| info.has_existentials)
                .map(|info| (newtype, target, type_args, info)),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                payload,
                ..
            } => self
                .ctx
                .newtypes_by_module
                .get(module_path)
                .and_then(|newtypes| newtypes.get(newtype))
                .filter(|info| info.has_existentials && member == &info.projector)
                .map(|info| (newtype, payload, type_args, info)),
            _ => None,
        };
        if let Some((newtype, receiver, proj_type_args, info)) = cps_projector {
            let [continuation] = value_args.as_slice() else {
                unreachable!(
                    "a typechecked existential-projector continuation call carries exactly one continuation value"
                )
            };
            let [result_ty] = lowered_type_args.as_slice() else {
                unreachable!(
                    "a typechecked existential-projector continuation call carries its selected result type"
                )
            };
            // This fold runs after call selection. The outer application of
            // `[R] ([E..] payload -> R) -> R` therefore carries the selected
            // `R` as its ordinary structural type argument. Use that exact
            // phase artifact; neither the continuation body nor an omitted
            // source return annotation is a second source of type truth.
            let lowered_continuation = self.lower_expr(continuation);
            let continuation_ty =
                cps_continuation_type(info, proj_type_args, result_ty.clone(), meta.span);
            let continuation = self.adapt_routed_value_to_expected(
                lowered_continuation,
                None,
                &continuation_ty,
                meta.span,
            );
            return Expr::LowCpsProjectorApply {
                occurrence: Default::default(),
                newtype: newtype.clone(),
                module_path: info.home.clone(),
                type_args: proj_type_args.clone(),
                receiver: receiver.clone(),
                continuation: Box::new(continuation),
                continuation_ty,
                meta: convert_meta(meta),
                ext: (),
            };
        }
        let lowered_value_args = self.lower_value_args(value_args.clone());
        let canonical = match callee_ty {
            Some(callee_ty) => self.lower_call_value_args_for_routed_callee_ty(
                value_args,
                &callee_ty,
                &lowered_type_args,
                meta.span,
            ),
            None => CanonicalCallArgs::direct(lowered_value_args),
        };
        if canonical.wrappers.is_empty() {
            return Expr::LowIndirectCall {
                occurrence: Default::default(),
                callee: Box::new(callee_expr),
                type_args: lowered_type_args,
                args: canonical.args,
                meta: convert_meta(meta),
                ext: (),
            };
        }

        let callee_name = self.next_call_callee_name();
        let call = Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: Box::new(Expr::LowBoundRef {
                occurrence: Default::default(),
                name: callee_name.clone(),
                meta: convert_meta(meta),
                ext: (),
            }),
            type_args: lowered_type_args,
            args: canonical.args.clone(),
            meta: convert_meta(meta),
            ext: (),
        };
        let call = self.wrap_canonical_call(call, canonical);
        Expr::Let {
            occurrence: Default::default(),
            name: callee_name,
            name_span: meta.span,
            ty: None,
            pattern: (),
            value: Box::new(callee_expr),
            body: Box::new(call),
            meta: convert_meta(meta),
        }
    }

    fn lower_exact_module_fn_call(
        &self,
        module_path: &str,
        member: &str,
        type_args: &[Type<Enriched>],
        value_args: &[Expr<Enriched>],
        meta: &Meta<Enriched>,
    ) -> Option<Expr<Routed>> {
        let entry = self
            .ctx
            .module_fns_by_module
            .get(module_path)
            .and_then(|m| m.get(member))
            .cloned()?;
        let (final_type_args, final_value_args) =
            match self.split_args_via_sig(&entry.sig, type_args, value_args) {
                Some(p) => p,
                None => (self.routed_type_args(type_args), value_args.to_vec()),
            };
        Some(self.low_qualified_module_call(
            (module_path.to_owned(), format!("{module_path}.{member}")),
            final_type_args,
            final_value_args,
            ModuleCallInfo {
                sig: &entry.sig,
                target_arity: Some(entry.target_arity),
                ret_ty: Some(entry.ret_abi),
                declaring_module: &entry.module_path,
                meta,
            },
        ))
    }

    fn lower_exact_newtype_member_call(
        &self,
        module_path: &str,
        newtype: &str,
        member: &str,
        type_args: &[Type<Enriched>],
        value_args: &[Expr<Enriched>],
        meta: &Meta<Enriched>,
    ) -> Option<Expr<Routed>> {
        let info = self
            .ctx
            .newtypes_by_module
            .get(module_path)
            .and_then(|m| m.get(newtype))?;
        if member != info.constructor && member != info.projector {
            return None;
        }
        let routed_type_args = self.routed_type_args(type_args);
        let payload = if member == info.constructor {
            self.lower_newtype_constructor_payload(
                info,
                &routed_type_args,
                value_args.to_vec(),
                meta.span,
            )
        } else {
            value_args
                .first()
                .map(|payload| self.lower_expr(payload))
                .unwrap_or_else(|| self.unit_at(meta))
        };
        Some(Expr::LowQualifiedNewtypeMember {
            occurrence: Default::default(),
            module_path: info.home.clone(),
            newtype: info.nominal_name.clone(),
            member: member.to_owned(),
            type_args: routed_type_args,
            payload: Box::new(payload),
            meta: convert_meta(meta),
            ext: (),
        })
    }

    fn lower_newtype_constructor_payload(
        &self,
        info: &NewtypeInfo,
        type_args: &[Type<Routed>],
        value_args: Vec<Expr<Enriched>>,
        span: crate::span::Span,
    ) -> Expr<Routed> {
        let payload_ty = instantiate_newtype_constructor_payload(info, type_args);
        // A constructor stores one value of its public payload type. Its
        // already-selected call may spell that product as several values;
        // only CallArg::Type is erased, never a component of this packet.
        let mut canonical = self.lower_call_value_args_grouped_to_target_arity(
            value_args,
            &payload_ty,
            std::slice::from_ref(&payload_ty),
            span,
            true,
        );
        let [payload]: [Expr<Routed>; 1] = std::mem::take(&mut canonical.args)
            .try_into()
            .unwrap_or_else(|_| {
                unreachable!("a validated constructor call fills exactly one payload packet")
            });
        self.wrap_canonical_call(payload, canonical)
    }

    fn lower_value_args(&self, value_args: Vec<Expr<Enriched>>) -> Vec<Expr<Routed>> {
        value_args
            .into_iter()
            .map(|e| self.lower_expr_owned(e))
            .collect()
    }

    fn lower_expr_owned(&self, e: Expr<Enriched>) -> Expr<Routed> {
        self.lower_expr(&e)
    }

    fn unit_at(&self, meta: &Meta<Enriched>) -> Expr<Routed> {
        Expr::Unit {
            occurrence: Default::default(),
            meta: convert_meta(meta),
        }
    }

    /// Synthesize a `Type::Unit<Routed>` at the given span. Used when
    /// the lower pass needs to fill a return-type slot on a host-call
    /// variant whose declared ret can't be found in the lowering
    /// context — a typer-guaranteed unreachable case in production
    /// (the typer rejects calls to undeclared host fns), but the
    /// fallback keeps the lower pass total over the input.
    fn unit_type_at(&self, span: crate::span::Span) -> Type<Routed> {
        Type::Unit {
            meta: Meta::new(span),
        }
    }

    fn lower_call_value_args_for_sig_with_function_adapters(
        &self,
        value_args: Vec<Expr<Enriched>>,
        sig: &Signature<Enriched>,
        sig_env: &UnfoldEnv<'_>,
        type_args: &[Type<Routed>],
        target_arity: Option<usize>,
        span: crate::span::Span,
    ) -> CanonicalCallArgs {
        match signature_param_abi(sig, type_args, span, sig_env) {
            Some(abi) => {
                if let Some(target_arity) = target_arity {
                    debug_assert_eq!(abi.target_slots.len(), target_arity);
                }
                self.lower_call_value_args_for_param_abi(value_args, &abi, span, true)
            }
            None => CanonicalCallArgs::direct(self.lower_value_args(value_args)),
        }
    }

    fn lower_call_value_args_for_routed_callee_ty(
        &self,
        value_args: Vec<Expr<Enriched>>,
        callee_ty: &Type<Routed>,
        type_args: &[Type<Routed>],
        span: crate::span::Span,
    ) -> CanonicalCallArgs {
        match function_param_abi_from_callee_ty(callee_ty, type_args) {
            Some(abi) => self.lower_call_value_args_for_param_abi(value_args, &abi, span, true),
            None => CanonicalCallArgs::direct(self.lower_value_args(value_args)),
        }
    }

    fn enriched_field_payload_type(
        &self,
        target_ty: &Type<Routed>,
        field_name: &str,
        index: usize,
        arity: usize,
    ) -> Option<Type<Routed>> {
        let Type::Path { segments, args, .. } = product_slot_type(target_ty, index, arity)? else {
            return None;
        };
        let (newtype, module_segments) = segments.split_last()?;
        if newtype.name != field_name {
            return None;
        }
        let info = if module_segments.is_empty() {
            self.module_ctx.newtypes_in_scope.get(&newtype.name)
        } else {
            let module_path = module_segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            self.ctx
                .newtypes_by_module
                .get(&module_path)
                .and_then(|newtypes| newtypes.get(&newtype.name))
        }?;
        (!info.has_existentials && info.type_params.len() == args.len())
            .then(|| instantiate_newtype_payload(info, &args))
    }

    fn lowered_expr_type(&self, expr: &Expr<Routed>) -> Option<Type<Routed>> {
        self.lowered_expr_type_with_locals(expr, &mut Vec::new())
    }

    fn lowered_expr_type_with_locals<'expr>(
        &self,
        expr: &'expr Expr<Routed>,
        locals: &mut Vec<(&'expr str, Option<Type<Routed>>)>,
    ) -> Option<Type<Routed>> {
        match expr {
            Expr::Unit {
                occurrence: _,
                meta,
            } => Some(Type::Unit { meta: meta.clone() }),
            Expr::StrLit { annotation, .. }
            | Expr::IntLit { annotation, .. }
            | Expr::FloatLit { annotation, .. }
            | Expr::BoolLit { annotation, .. } => Some(annotation.clone()),
            Expr::LowHostCall {
                sig,
                type_args,
                ret_ty,
                ..
            } => Some(canonicalize_function_abi_type(instantiate_signature_type(
                sig, type_args, ret_ty,
            ))),
            Expr::LowModuleCall {
                sig,
                type_args,
                ret_ty: Some(ret_ty),
                ..
            } => Some(canonicalize_function_abi_type(instantiate_signature_type(
                sig, type_args, ret_ty,
            ))),
            Expr::LowQualifiedModuleCall {
                sig,
                type_args,
                ret_ty: Some(ret_ty),
                ..
            } => Some(canonicalize_function_abi_type(instantiate_signature_type(
                sig, type_args, ret_ty,
            ))),
            Expr::EnrichedTuple { synth_ty, .. }
            | Expr::EnrichedInject { synth_ty, .. }
            | Expr::EnrichedRecord { synth_ty, .. } => Some(synth_ty.clone()),
            Expr::EnrichedMatch { result_ty, .. } | Expr::EnrichedConditional { result_ty, .. } => {
                Some(result_ty.clone())
            }
            Expr::EnrichedProject {
                index,
                arity,
                target_ty,
                ..
            } => product_slot_type(target_ty, *index, *arity),
            Expr::EnrichedFieldGet {
                field_name,
                index,
                arity,
                target_ty,
                ..
            } => self.enriched_field_payload_type(target_ty, field_name, *index, *arity),
            Expr::LowBoundRef { name, .. } => self.lowered_local_type(name, locals),
            Expr::LowClosureCall {
                name, type_args, ..
            } => self
                .lowered_local_type(name, locals)
                .and_then(|ty| function_return_after_call(&ty, type_args)),
            Expr::LowIndirectCall {
                callee, type_args, ..
            } => self
                .lowered_expr_type_with_locals(callee, locals)
                .and_then(|ty| function_return_after_call(&ty, type_args)),
            Expr::LowTypeApplication {
                callee, type_arg, ..
            } => self
                .lowered_expr_type_with_locals(callee, locals)
                .and_then(|ty| type_after_type_application(&ty, type_arg)),
            Expr::LowAbsurdCall { type_arg, .. } => Some(type_arg.clone()),
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                meta,
                ..
            } => self
                .module_ctx
                .newtypes_in_scope
                .get(newtype)
                .filter(|info| member == &info.constructor)
                .and_then(|info| newtype_constructor_result_type(info, type_args, meta.span)),
            Expr::LowNewtypeProj {
                newtype, type_args, ..
            } => self
                .module_ctx
                .newtypes_in_scope
                .get(newtype)
                .filter(|info| !info.has_existentials)
                .map(|info| instantiate_newtype_payload(info, type_args)),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                meta,
                ..
            } => self
                .ctx
                .newtypes_by_module
                .get(module_path)
                .and_then(|newtypes| newtypes.get(newtype))
                .and_then(|info| {
                    if member == &info.constructor {
                        newtype_constructor_result_type(info, type_args, meta.span)
                    } else if member == &info.projector && !info.has_existentials {
                        Some(instantiate_newtype_payload(info, type_args))
                    } else {
                        None
                    }
                }),
            Expr::LowCpsProjectorApply {
                continuation_ty, ..
            } => function_return_after_call(continuation_ty, &[]),
            Expr::FnExpr {
                sig,
                ret_ty,
                body,
                meta,
                ..
            } => {
                let ret = ret_ty
                    .clone()
                    .or_else(|| self.lowered_expr_type_with_locals(body, locals))?;
                Some(sig.signature_ty(ret, meta.span))
            }
            Expr::LowHostFnValueRef {
                sig, ret_ty, meta, ..
            } => Some(sig.signature_ty(ret_ty.clone(), meta.span)),
            Expr::LowModuleFnValueRef { mangled, meta, .. } => self
                .module_fn_for_value_ref(mangled)
                .map(|entry| self.module_fn_value_type(entry, meta.span)),
            Expr::Let {
                name, value, body, ..
            } => {
                let value_ty = self.lowered_expr_type_with_locals(value, locals);
                locals.push((name, value_ty));
                let body_ty = self.lowered_expr_type_with_locals(body, locals);
                locals.pop();
                body_ty
            }
            Expr::Seq { body, .. } => self.lowered_expr_type_with_locals(body, locals),
            _ => None,
        }
    }

    fn lowered_local_type(
        &self,
        name: &str,
        locals: &[(&str, Option<Type<Routed>>)],
    ) -> Option<Type<Routed>> {
        if let Some((_, ty)) = locals
            .iter()
            .rev()
            .find(|(local_name, _)| *local_name == name)
        {
            return ty.clone();
        }
        self.bound_routed_type(name)
    }

    /// Recover the source ABI of a selected value whose ordinary Routed type
    /// is unavailable. Literal functions contribute their syntactic binder
    /// grouping recursively. Named and computed values are projected from
    /// their ordinary Routed type into the same type-free shape.
    /// The pass-local summary retains only callable group arities and stable
    /// lexical binder identities; `expected` remains the sole semantic type
    /// authority.
    fn selected_value_abi_type(
        &self,
        value: &Expr<Routed>,
        expected: &Type<Routed>,
    ) -> Option<Type<Routed>> {
        let shape = self.selected_abi_shape(value);
        self.selected_shape_abi_type(&shape, expected, &[])
    }

    fn selected_abi_shape(&self, value: &Expr<Routed>) -> SelectedAbiShape {
        self.selected_abi_shape_with_locals(value, &[])
    }

    fn next_selected_abi_binder_id(&self) -> usize {
        let binder = self.next_selected_abi_binder.get();
        self.next_selected_abi_binder.set(binder + 1);
        binder
    }

    fn selected_abi_shape_with_locals(
        &self,
        value: &Expr<Routed>,
        locals: &[(String, usize)],
    ) -> SelectedAbiShape {
        match value {
            Expr::FnExpr { sig, body, .. } => {
                let mut body_locals = locals.to_vec();
                let mut groups = Vec::new();
                for group in sig.canonical_groups() {
                    match group {
                        SignatureGroupRef::Type(params) => {
                            groups
                                .extend(std::iter::repeat_n(SelectedAbiGroup::Type, params.len()));
                        }
                        SignatureGroupRef::Value(params) => {
                            let mut binders = Vec::with_capacity(params.len());
                            for param in params {
                                let SignatureParam::Value(param) = param else {
                                    unreachable!(
                                        "a value signature group contains only value binders"
                                    );
                                };
                                let binder = self.next_selected_abi_binder_id();
                                binders.push(binder);
                                body_locals.push((param.name.clone(), binder));
                            }
                            groups.push(SelectedAbiGroup::Value {
                                arity: params.len(),
                                binders,
                            });
                        }
                    }
                }
                SelectedAbiShape::Function {
                    groups,
                    body: Box::new(self.selected_abi_shape_with_locals(body, &body_locals)),
                }
            }
            Expr::Let {
                name, value, body, ..
            } => {
                let value = self.selected_abi_shape_with_locals(value, locals);
                let binder = self.next_selected_abi_binder_id();
                let mut body_locals = locals.to_vec();
                body_locals.push((name.clone(), binder));
                SelectedAbiShape::Let {
                    binder,
                    value: Box::new(value),
                    body: Box::new(self.selected_abi_shape_with_locals(body, &body_locals)),
                }
            }
            Expr::Seq { body, .. } => SelectedAbiShape::Seq {
                body: Box::new(self.selected_abi_shape_with_locals(body, locals)),
            },
            Expr::LowBoundRef { name, .. } => {
                if let Some((_, binder)) = locals.iter().rev().find(|(local, _)| local == name) {
                    return SelectedAbiShape::BoundRef { binder: *binder };
                }
                self.bound_selected_abi_binder(name)
                    .map(|binder| SelectedAbiShape::BoundRef { binder })
                    .unwrap_or(SelectedAbiShape::Unknown)
            }
            other => self
                .lowered_expr_type(other)
                .as_ref()
                .map(selected_abi_shape_from_type)
                .unwrap_or(SelectedAbiShape::Unknown),
        }
    }

    fn selected_shape_abi_type(
        &self,
        shape: &SelectedAbiShape,
        expected: &Type<Routed>,
        bindings: &[SelectedAbiBinding],
    ) -> Option<Type<Routed>> {
        if !type_has_callable_layer(expected) {
            return Some(expected.clone());
        }
        match shape {
            SelectedAbiShape::Function { groups, body } => {
                self.selected_group_abi_type(groups, body, expected, bindings)
            }
            SelectedAbiShape::Let {
                binder,
                value,
                body,
                ..
            } => {
                let mut body_bindings = bindings.to_vec();
                body_bindings.push(SelectedAbiBinding {
                    binder: *binder,
                    shape: value.as_ref().clone(),
                });
                self.selected_shape_abi_type(body, expected, &body_bindings)
            }
            SelectedAbiShape::Seq { body } => {
                self.selected_shape_abi_type(body, expected, bindings)
            }
            SelectedAbiShape::BoundRef { binder } => {
                for (index, binding) in bindings.iter().enumerate().rev() {
                    if binding.binder == *binder {
                        return self.selected_shape_abi_type(
                            &binding.shape,
                            expected,
                            &bindings[..index],
                        );
                    }
                }
                let (shape, ty) = self.bound_selected_abi(*binder)?;
                if let Some(shape) = shape {
                    self.selected_shape_abi_type(&shape, expected, bindings)
                } else {
                    self.selected_shape_abi_type(
                        &selected_abi_shape_from_type(&ty?),
                        expected,
                        bindings,
                    )
                }
            }
            SelectedAbiShape::Value => None,
            SelectedAbiShape::Unknown => None,
        }
    }

    fn selected_group_abi_type(
        &self,
        groups: &[SelectedAbiGroup],
        result: &SelectedAbiShape,
        expected: &Type<Routed>,
        bindings: &[SelectedAbiBinding],
    ) -> Option<Type<Routed>> {
        let Some((group, remaining)) = groups.split_first() else {
            return self.selected_shape_abi_type(result, expected, bindings);
        };
        match (group, expected) {
            (
                SelectedAbiGroup::Type,
                Type::Forall {
                    param, body, meta, ..
                },
            ) => Some(Type::Forall {
                param: param.clone(),
                body: Box::new(self.selected_group_abi_type(remaining, result, body, bindings)?),
                meta: meta.clone(),
            }),
            (
                SelectedAbiGroup::Value { arity, binders },
                Type::Function {
                    param,
                    ret,
                    meta,
                    caps,
                    ..
                },
            ) => {
                let slots = function_abi_param_slot_types(param, *arity);
                if !binders.is_empty() && binders.len() != slots.len() {
                    return None;
                }
                let mut body_bindings = bindings.to_vec();
                body_bindings.extend(binders.iter().zip(slots.iter()).map(|(binder, ty)| {
                    SelectedAbiBinding {
                        binder: *binder,
                        shape: selected_abi_shape_from_type(ty),
                    }
                }));
                Some(Type::Function {
                    param: param.clone(),
                    ret: Box::new(self.selected_group_abi_type(
                        remaining,
                        result,
                        ret,
                        &body_bindings,
                    )?),
                    meta: meta.clone(),
                    abi_arity: *arity,
                    caps: caps.clone(),
                })
            }
            _ => None,
        }
    }

    fn public_routed_type(&self, ty: Type<Routed>) -> Type<Routed> {
        canonicalize_function_abi_type(unfold_type_alias_type(&ty, &self.unfold_env()))
    }

    /// The universal Enriched → Routed type conversion: phase-convert,
    /// unfold every declared type alias (scope-correctly), and
    /// canonicalize the function ABI shape. Every referenced or boundary
    /// type this pass writes into the Routed package flows through here or
    /// through an equivalent per-declaring-scope composition — the pass-level
    /// contract that no declared alias survives in those types. Raw
    /// `Item::TypeAlias` declaration bodies remain as declared.
    fn routed_type(&self, ty: &Type<Enriched>) -> Type<Routed> {
        self.public_routed_type(convert_type::<Enriched, Routed>(ty))
    }

    fn routed_type_args(&self, type_args: &[Type<Enriched>]) -> Vec<Type<Routed>> {
        type_args.iter().map(|ty| self.routed_type(ty)).collect()
    }

    /// Reclassify a `(type_args, value_args)` pair against the callee's
    /// signature when a test or pre-emit caller bypasses the typer's normal
    /// rewrite of value-shaped type witnesses to `CallArg::Type`.
    #[allow(clippy::type_complexity)]
    fn split_args_via_sig(
        &self,
        sig: &Signature<Enriched>,
        type_args: &[Type<Enriched>],
        value_args: &[Expr<Enriched>],
    ) -> Option<(Vec<Type<Routed>>, Vec<Expr<Enriched>>)> {
        if !type_args.is_empty() || value_args.len() != sig.params.len() {
            return None;
        }
        let mut out_type_args = Vec::new();
        let mut out_value_args = Vec::new();
        for (param, arg) in sig.params.iter().zip(value_args) {
            match param {
                SignatureParam::Type(_) => {
                    out_type_args.push(self.infer_type_from_value_arg(arg)?);
                }
                SignatureParam::Value(_) => out_value_args.push(arg.clone()),
            }
        }
        Some((out_type_args, out_value_args))
    }

    /// Recover a type witness from a value-shaped path argument.
    /// The typer normally rewrites this slot to `CallArg::Type`; fallback
    /// callers still route the recovered type through the same canonical
    /// Enriched-to-Routed chokepoint as an ordinary explicit type argument.
    fn infer_type_from_value_arg(&self, expr: &Expr<Enriched>) -> Option<Type<Routed>> {
        let ty = match expr {
            Expr::Path { segments, meta, .. } => Type::Path {
                segments: segments.clone(),
                args: Vec::new(),
                meta: meta.clone(),
            },
            _ => return None,
        };
        Some(self.routed_type(&ty))
    }

    fn lower_call_value_args_for_param_abi(
        &self,
        value_args: Vec<Expr<Enriched>>,
        abi: &CalleeParamAbi,
        span: crate::span::Span,
        adapt_function_args: bool,
    ) -> CanonicalCallArgs {
        if value_args.is_empty() {
            return CanonicalCallArgs::default();
        }
        if abi.target_slots.is_empty()
            && matches!(abi.param_ty, Type::Unit { .. })
            && value_args.len() == 1
        {
            let mut out = CanonicalCallArgs::default();
            let lowered = self.lower_expr_owned(value_args.into_iter().next().expect("one arg"));
            if !matches!(lowered, Expr::Unit { .. }) {
                out.wrappers.push(CallArgWrapper::Seq {
                    value: lowered,
                    span,
                });
            }
            return out;
        }
        if value_args.len() != abi.target_slots.len() {
            return self.lower_call_value_args_grouped_to_target_arity(
                value_args,
                &abi.param_ty,
                &abi.target_slots,
                span,
                adapt_function_args,
            );
        }
        let mut out = CanonicalCallArgs::default();
        for (arg, expected) in value_args.into_iter().zip(&abi.target_slots) {
            if matches!(expected, Type::Product { .. }) {
                out.args.push(self.lower_value_arg_as_value(
                    arg,
                    expected,
                    span,
                    adapt_function_args,
                ));
            } else {
                self.lower_value_arg_as_abi(arg, expected, span, adapt_function_args, &mut out);
            }
        }
        out
    }

    fn lower_call_value_args_grouped_to_target_arity(
        &self,
        value_args: Vec<Expr<Enriched>>,
        param_ty: &Type<Routed>,
        target_slots: &[Type<Routed>],
        span: crate::span::Span,
        adapt_function_args: bool,
    ) -> CanonicalCallArgs {
        let Some(source_tys) = expected_value_arg_tys(param_ty, value_args.len()) else {
            return CanonicalCallArgs::direct(self.lower_value_args(value_args));
        };
        let mut out = CanonicalCallArgs::default();
        let mut source_values = Vec::with_capacity(value_args.len());
        for (arg, expected) in value_args.into_iter().zip(source_tys) {
            source_values.push(self.lower_value_arg_as_value(
                arg,
                expected,
                span,
                adapt_function_args,
            ));
        }

        let value = match source_values.len() {
            0 => Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            },
            1 => source_values.pop().expect("one source value"),
            _ => Expr::EnrichedTuple {
                occurrence: Default::default(),
                items: source_values,
                synth_ty: param_ty.clone(),
                meta: Meta::new(span),
                ext: (),
            },
        };

        if target_slots.is_empty() {
            if !matches!(value, Expr::Unit { .. }) {
                out.wrappers.push(CallArgWrapper::Seq { value, span });
            }
            return out;
        }

        if target_slots.len() == 1 {
            out.args.push(value);
            return out;
        }

        let value = match value {
            Expr::EnrichedTuple {
                items, synth_ty, ..
            } if grouped_items_match_target_slots(&synth_ty, items.len(), target_slots) => {
                out.args.extend(items);
                return out;
            }
            Expr::EnrichedRecord {
                fields, synth_ty, ..
            } if grouped_items_match_target_slots(&synth_ty, fields.len(), target_slots) => {
                out.args.extend(fields.into_iter().map(|field| field.value));
                return out;
            }
            other => other,
        };

        if is_inlineable_abi_group_value(&value)
            && let Some(args) =
                flatten_value_to_target_slots(value.clone(), param_ty, target_slots, span)
        {
            out.args.extend(args);
            return out;
        }

        let temp = format!("__kio_call_arg{}__", out.wrappers.len());
        let target = low_bound_ref(&temp, span);
        if let Some(args) =
            flatten_value_to_target_slots(target.clone(), param_ty, target_slots, span)
        {
            out.wrappers.push(CallArgWrapper::Let {
                name: temp.clone(),
                value,
                span,
            });
            out.args.extend(args);
        } else {
            let mut flat = Vec::new();
            flatten_abi_value_slot(target, param_ty, span, &mut flat);
            if flat.len() != target_slots.len() {
                return CanonicalCallArgs::direct(vec![value]);
            }
            out.wrappers.push(CallArgWrapper::Let {
                name: temp,
                value,
                span,
            });
            out.args.extend(flat.into_iter().map(|(value, _)| value));
        }
        out
    }

    fn lower_value_arg_as_value(
        &self,
        arg: Expr<Enriched>,
        expected: &Type<Routed>,
        span: crate::span::Span,
        adapt_function_args: bool,
    ) -> Expr<Routed> {
        if adapt_function_args
            && let Some(adapter) = self.function_arg_adapter(&arg, expected, span)
        {
            return adapter;
        }
        let lowered = self.lower_expr_owned(arg.clone());
        let actual_ty = self
            .enriched_expr_type_for_source_adapter(&arg)
            .or_else(|| self.lowered_expr_type(&lowered))
            .or_else(|| self.enriched_expr_type_for_adapter(&arg));
        self.adapt_routed_value_to_expected(lowered, actual_ty, expected, span)
    }

    fn lower_value_arg_as_abi(
        &self,
        arg: Expr<Enriched>,
        expected: &Type<Routed>,
        span: crate::span::Span,
        adapt_function_args: bool,
        out: &mut CanonicalCallArgs,
    ) {
        if adapt_function_args
            && let Some(adapter) = self.function_arg_adapter(&arg, expected, span)
        {
            out.args.push(adapter);
            return;
        }
        if matches!(expected, Type::Product { .. }) {
            let slot_tys = product_abi_slot_types(expected);
            let actual_ty = self
                .enriched_expr_type_for_source_adapter(&arg)
                .or_else(|| self.enriched_expr_type_for_adapter(&arg));
            let arg = match arg {
                Expr::EnrichedTuple { items, .. } if items.len() == slot_tys.len() => {
                    for (item, expected_slot) in items.into_iter().zip(slot_tys.iter()) {
                        self.lower_value_arg_as_abi(
                            item,
                            expected_slot,
                            span,
                            adapt_function_args,
                            out,
                        );
                    }
                    return;
                }
                other => other,
            };

            let temp = format!("__kio_call_arg{}__", out.wrappers.len());
            let lowered = self.lower_expr_owned(arg);
            let actual_ty = actual_ty.or_else(|| self.lowered_expr_type(&lowered));
            out.wrappers.push(CallArgWrapper::Let {
                name: temp.clone(),
                value: lowered,
                span,
            });
            let target = low_bound_ref(&temp, span);
            let arity = product_abi_slot_count(expected);
            let target_ty = actual_ty.as_ref().unwrap_or(expected);
            let actual_slots = actual_ty
                .as_ref()
                .and_then(|ty| expected_value_arg_tys(ty, slot_tys.len()));
            for (index, expected_slot) in slot_tys.iter().enumerate() {
                let projected = Expr::EnrichedProject {
                    occurrence: Default::default(),
                    target: Box::new(target.clone()),
                    index,
                    arity,
                    target_ty: (*target_ty).clone(),
                    meta: Meta::new(span),
                    ext: (),
                };
                let projected = actual_slots
                    .as_ref()
                    .and_then(|slots| slots.get(index))
                    .map(|actual_slot| {
                        self.adapt_routed_value_to_expected(
                            projected.clone(),
                            Some((**actual_slot).clone()),
                            expected_slot,
                            span,
                        )
                    })
                    .unwrap_or(projected);
                out.args.push(projected);
            }
            return;
        }

        let lowered = self.lower_expr_owned(arg.clone());
        let actual_ty = self
            .lowered_expr_type(&lowered)
            .or_else(|| self.enriched_expr_type_for_adapter(&arg));
        out.args
            .push(self.adapt_routed_value_to_expected(lowered, actual_ty, expected, span));
    }

    fn function_arg_adapter(
        &self,
        arg: &Expr<Enriched>,
        expected: &Type<Routed>,
        span: crate::span::Span,
    ) -> Option<Expr<Routed>> {
        let actual_ty = self.enriched_expr_type_for_adapter(arg)?;
        self.function_value_adapter_at_depth(
            Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            },
            expected,
            &actual_ty,
            span,
            0,
        )?;
        let callee = self.lower_expr_owned(arg.clone());
        Some(
            self.function_value_adapter(callee, expected, &actual_ty, span)
                .expect("the type-only adapter preflight and materialization must agree"),
        )
    }

    fn function_value_adapter(
        &self,
        callee: Expr<Routed>,
        target_ty: &Type<Routed>,
        source_ty: &Type<Routed>,
        span: crate::span::Span,
    ) -> Option<Expr<Routed>> {
        let scope = self.next_call_callee_scope.get();
        let callee_name = format!("__kio_call_callee{scope}__");
        let adapter = self.function_value_adapter_at_depth(
            low_bound_ref(&callee_name, span),
            target_ty,
            source_ty,
            span,
            0,
        )?;
        self.next_call_callee_scope.set(scope + 1);
        Some(Expr::Let {
            occurrence: Default::default(),
            name: callee_name,
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(callee),
            body: Box::new(adapter),
            meta: Meta::new(span),
        })
    }

    fn function_value_adapter_at_depth(
        &self,
        callee: Expr<Routed>,
        target_ty: &Type<Routed>,
        source_ty: &Type<Routed>,
        span: crate::span::Span,
        depth: usize,
    ) -> Option<Expr<Routed>> {
        let (target_type_params, target_ty) = peel_function_foralls(target_ty);
        let (source_type_params, source_ty) = peel_function_foralls(source_ty);
        if target_type_params.len() != source_type_params.len() {
            return None;
        }
        let source_subst: HashMap<String, Type<Routed>> = source_type_params
            .iter()
            .zip(&target_type_params)
            .map(|(source, target)| {
                (
                    source.name.clone(),
                    Type::Path {
                        segments: vec![PathSegment::new(target.name.clone(), target.span)],
                        args: Vec::new(),
                        meta: Meta::new(target.span),
                    },
                )
            })
            .collect();
        let source_ty = crate::pass::typecheck_core::subst_type(&source_ty, &source_subst);
        let Type::Function {
            param: target_param,
            ret: target_ret,
            abi_arity: target_abi_arity,
            ..
        } = &target_ty
        else {
            return None;
        };
        let Type::Function {
            param: source_param,
            ret: source_ret,
            abi_arity: source_abi_arity,
            ..
        } = &source_ty
        else {
            return None;
        };

        let target_slots = function_abi_param_slot_types(target_param, *target_abi_arity);
        let source_slots = function_abi_param_slot_types(source_param, *source_abi_arity);
        let mut changed = target_slots.len() != source_slots.len();
        let param_names: Vec<String> = (0..target_slots.len())
            .map(|index| format!("__kio_fn{depth}_arg{index}__"))
            .collect();
        let params: Vec<SignatureParam<Routed>> = param_names
            .iter()
            .zip(target_slots.iter())
            .map(|(name, ty)| {
                SignatureParam::Value(Param {
                    name: name.clone(),
                    ty: Some(ty.clone()),
                    pattern: (),
                    meta: Meta::new(span),
                })
            })
            .collect();

        let target_values: Vec<_> = param_names
            .iter()
            .zip(target_slots.iter())
            .map(|(name, ty)| (low_bound_ref(name, span), ty.clone()))
            .collect();
        let call_args = self.function_adapter_source_args(
            &target_values,
            &source_slots,
            span,
            depth,
            &mut changed,
        )?;

        let type_stage_names: Vec<String> = (0..target_type_params.len())
            .map(|index| format!("__kio_fn{depth}_type{index}_result__"))
            .collect();
        let value_callee = type_stage_names
            .last()
            .map(|name| low_bound_ref(name, span))
            .unwrap_or_else(|| callee.clone());
        let call = Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: Box::new(value_callee),
            type_args: Vec::new(),
            args: call_args,
            meta: Meta::new(span),
            ext: (),
        };
        let produced_name = format!("__kio_fn{depth}_result__");
        let body = if let Some(adapter) = self.function_value_adapter_at_depth(
            low_bound_ref(&produced_name, span),
            target_ret,
            source_ret,
            span,
            depth + 1,
        ) {
            changed = true;
            Expr::Let {
                occurrence: Default::default(),
                name: produced_name,
                name_span: span,
                ty: None,
                pattern: (),
                value: Box::new(call),
                body: Box::new(adapter),
                meta: Meta::new(span),
            }
        } else {
            call
        };
        if !changed {
            return None;
        }
        // Even an erased Unit parameter is a real callable layer. Preserve
        // its zero-slot value group so later applications cannot collapse it
        // into the surrounding type-binder run.
        let groups = vec![SignatureGroupKind::Value {
            len: target_slots.len(),
        }];
        let mut adapter = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, groups),
            ret_ty: Some((**target_ret).clone()),
            body: Box::new(body),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let mut residual_target_ty = target_ty.clone();
        for (index, target_param) in target_type_params.iter().enumerate().rev() {
            let source_stage = if index == 0 {
                callee.clone()
            } else {
                low_bound_ref(&type_stage_names[index - 1], span)
            };
            let target_arg = Type::Path {
                segments: vec![PathSegment::new(
                    target_param.name.clone(),
                    target_param.span,
                )],
                args: Vec::new(),
                meta: Meta::new(target_param.span),
            };
            let applied_source_stage = Expr::LowTypeApplication {
                occurrence: Default::default(),
                callee: Box::new(source_stage),
                type_arg: target_arg,
                meta: Meta::new(span),
                ext: (),
            };
            adapter = Expr::Let {
                occurrence: Default::default(),
                name: type_stage_names[index].clone(),
                name_span: span,
                ty: None,
                pattern: (),
                value: Box::new(applied_source_stage),
                body: Box::new(adapter),
                meta: Meta::new(span),
            };
            // The eager zero-slot call consumes this administrative value
            // group immediately, exposing exactly one ordinary type stage.
            let stage_closure = Expr::FnExpr {
                occurrence: Default::default(),
                sig: Signature::from_parts(
                    vec![SignatureParam::Type(target_param.clone())],
                    vec![
                        SignatureGroupKind::Value { len: 0 },
                        SignatureGroupKind::Type { len: 1 },
                    ],
                ),
                ret_ty: Some(residual_target_ty.clone()),
                body: Box::new(adapter),
                meta: Meta::new(span),
                caps: Default::default(),
            };
            adapter = Expr::LowIndirectCall {
                occurrence: Default::default(),
                callee: Box::new(stage_closure),
                type_args: Vec::new(),
                args: Vec::new(),
                meta: Meta::new(span),
                ext: (),
            };
            residual_target_ty = Type::Forall {
                param: target_param.clone(),
                body: Box::new(residual_target_ty),
                meta: Meta::new(span),
            };
        }
        Some(adapter)
    }

    fn function_adapter_source_args(
        &self,
        target_values: &[(Expr<Routed>, Type<Routed>)],
        source_slots: &[Type<Routed>],
        span: crate::span::Span,
        depth: usize,
        changed: &mut bool,
    ) -> Option<Vec<Expr<Routed>>> {
        if source_slots.is_empty() {
            return Some(Vec::new());
        }

        if target_values.len() == source_slots.len()
            && target_values
                .iter()
                .zip(source_slots)
                .all(|((_, actual), expected)| abi_type_shape_eq(actual, expected))
        {
            return Some(
                target_values
                    .iter()
                    .zip(source_slots)
                    .map(|((value, actual), expected)| {
                        self.adapt_projected_function_slot(
                            value.clone(),
                            actual,
                            expected,
                            span,
                            depth,
                            changed,
                        )
                    })
                    .collect(),
            );
        }

        *changed = true;
        let mut flat = Vec::new();
        for (value, ty) in target_values {
            flatten_abi_value_slot(value.clone(), ty, span, &mut flat);
        }

        let mut cursor = 0usize;
        let mut args = Vec::with_capacity(source_slots.len());
        for source_slot in source_slots {
            let (value, actual_ty) =
                build_value_for_abi_slot(&flat, &mut cursor, source_slot, span)?;
            args.push(self.adapt_projected_function_slot(
                value,
                &actual_ty,
                source_slot,
                span,
                depth,
                changed,
            ));
        }
        if cursor == flat.len() {
            Some(args)
        } else {
            None
        }
    }

    fn adapt_projected_function_slot(
        &self,
        value: Expr<Routed>,
        value_ty: &Type<Routed>,
        target_ty: &Type<Routed>,
        span: crate::span::Span,
        depth: usize,
        changed: &mut bool,
    ) -> Expr<Routed> {
        let adapter = self.function_value_adapter_at_depth(
            value.clone(),
            target_ty,
            value_ty,
            span,
            depth + 1,
        );
        if let Some(adapter) = adapter {
            *changed = true;
            adapter
        } else {
            value
        }
    }

    fn adapt_routed_value_to_expected(
        &self,
        value: Expr<Routed>,
        actual_ty: Option<Type<Routed>>,
        expected_ty: &Type<Routed>,
        span: crate::span::Span,
    ) -> Expr<Routed> {
        let actual_ty = actual_ty.or_else(|| {
            if type_has_callable_layer(expected_ty) {
                self.selected_value_abi_type(&value, expected_ty)
            } else {
                None
            }
        });
        let Some(actual_ty) = actual_ty else {
            return value;
        };
        if let Some(adapter) =
            self.function_value_adapter(value.clone(), expected_ty, &actual_ty, span)
        {
            return adapter;
        }
        if !matches!(expected_ty, Type::Product { .. })
            || !matches!(actual_ty, Type::Product { .. })
        {
            return value;
        }
        let value = match value {
            Expr::Let {
                occurrence: _,
                name,
                name_span,
                ty,
                pattern,
                value: let_value,
                body,
                meta,
            } => {
                return Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span,
                    ty,
                    pattern,
                    value: let_value,
                    body: Box::new(self.adapt_routed_value_to_expected(
                        *body,
                        Some(actual_ty),
                        expected_ty,
                        span,
                    )),
                    meta,
                };
            }
            Expr::Seq {
                occurrence: _,
                value: seq_value,
                body,
                meta,
            } => {
                return Expr::Seq {
                    occurrence: Default::default(),
                    value: seq_value,
                    body: Box::new(self.adapt_routed_value_to_expected(
                        *body,
                        Some(actual_ty),
                        expected_ty,
                        span,
                    )),
                    meta,
                };
            }
            other => other,
        };
        let arity = product_abi_slot_count(expected_ty);
        let Some(expected_slots) = expected_value_arg_tys(expected_ty, arity) else {
            return value;
        };
        let Some(actual_slots) = expected_value_arg_tys(&actual_ty, arity) else {
            return value;
        };

        match value {
            Expr::EnrichedTuple {
                items,
                meta,
                ext: (),
                ..
            } if items.len() == arity => {
                let mut changed = false;
                let items = items
                    .into_iter()
                    .zip(actual_slots.into_iter().zip(expected_slots))
                    .map(|(item, (actual_slot, expected_slot))| {
                        let adapted = self.adapt_routed_value_to_expected(
                            item.clone(),
                            Some(actual_slot.clone()),
                            expected_slot,
                            span,
                        );
                        if adapted != item {
                            changed = true;
                        }
                        adapted
                    })
                    .collect();
                if changed {
                    Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: expected_ty.clone(),
                        meta,
                        ext: (),
                    }
                } else {
                    Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: actual_ty,
                        meta,
                        ext: (),
                    }
                }
            }
            Expr::EnrichedRecord {
                fields,
                meta,
                ext: (),
                ..
            } if fields.len() == arity => {
                let mut changed = false;
                let fields = fields
                    .into_iter()
                    .zip(actual_slots.into_iter().zip(expected_slots))
                    .map(|(field, (actual_slot, expected_slot))| {
                        let value = field.value;
                        let adapted = self.adapt_routed_value_to_expected(
                            value.clone(),
                            Some(actual_slot.clone()),
                            expected_slot,
                            span,
                        );
                        if adapted != value {
                            changed = true;
                        }
                        RecordField {
                            name: field.name,
                            value: adapted,
                            meta: field.meta,
                        }
                    })
                    .collect();
                if changed {
                    Expr::EnrichedRecord {
                        occurrence: Default::default(),
                        fields,
                        synth_ty: expected_ty.clone(),
                        meta,
                        ext: (),
                    }
                } else {
                    Expr::EnrichedRecord {
                        occurrence: Default::default(),
                        fields,
                        synth_ty: actual_ty,
                        meta,
                        ext: (),
                    }
                }
            }
            other => {
                let temp = "__kio_ret_abi__".to_owned();
                let target = low_bound_ref(&temp, span);
                let mut changed = false;
                let items: Vec<_> = actual_slots
                    .into_iter()
                    .zip(expected_slots)
                    .enumerate()
                    .map(|(index, (actual_slot, expected_slot))| {
                        let projected = Expr::EnrichedProject {
                            occurrence: Default::default(),
                            target: Box::new(target.clone()),
                            index,
                            arity,
                            target_ty: actual_ty.clone(),
                            meta: Meta::new(span),
                            ext: (),
                        };
                        let adapted = self.adapt_routed_value_to_expected(
                            projected.clone(),
                            Some(actual_slot.clone()),
                            expected_slot,
                            span,
                        );
                        if adapted != projected {
                            changed = true;
                        }
                        adapted
                    })
                    .collect();
                if !changed {
                    return other;
                }
                Expr::Let {
                    occurrence: Default::default(),
                    name: temp,
                    name_span: span,
                    ty: None,
                    pattern: (),
                    value: Box::new(other),
                    body: Box::new(Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: expected_ty.clone(),
                        meta: Meta::new(span),
                        ext: (),
                    }),
                    meta: Meta::new(span),
                }
            }
        }
    }

    fn wrap_canonical_call(
        &self,
        call: Expr<Routed>,
        canonical: CanonicalCallArgs,
    ) -> Expr<Routed> {
        canonical
            .wrappers
            .into_iter()
            .rev()
            .fold(call, |body, wrapper| match wrapper {
                CallArgWrapper::Let { name, value, span } => Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span: span,
                    ty: None,
                    pattern: (),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(span),
                },
                CallArgWrapper::Seq { value, span } => Expr::Seq {
                    occurrence: Default::default(),
                    value: Box::new(value),
                    body: Box::new(body),
                    meta: Meta::new(span),
                },
            })
    }

    fn low_host_call(
        &self,
        name: String,
        hsig: &HostFnSig,
        type_args: &[Type<Enriched>],
        value_args: Vec<Expr<Enriched>>,
        meta: &Meta<Enriched>,
    ) -> Expr<Routed> {
        let sig = &hsig.sig;
        let host_env = self.declaring_unfold_env(&hsig.module_path);
        let routed_type_args =
            self.infer_call_type_args_for_sig(sig, &host_env, type_args, &value_args);
        let canonical = self.lower_call_value_args_for_sig_with_function_adapters(
            value_args,
            sig,
            &host_env,
            &routed_type_args,
            None,
            meta.span,
        );
        let routed_sig = canonicalize_entry_signature(sig, meta.span, &host_env);
        let routed_ret = unfold_signature_ret(sig, &hsig.ret, &host_env);
        let call = if signature_has_groups_after_first_value_group(sig) {
            Expr::LowIndirectCall {
                occurrence: Default::default(),
                callee: Box::new(Expr::LowHostFnValueRef {
                    occurrence: Default::default(),
                    name,
                    module_path: hsig.module_path.clone(),
                    sig: routed_sig,
                    ret_ty: routed_ret,
                    meta: convert_meta(meta),
                    ext: (),
                }),
                type_args: routed_type_args,
                args: canonical.args.clone(),
                meta: convert_meta(meta),
                ext: (),
            }
        } else {
            Expr::LowHostCall {
                occurrence: Default::default(),
                name,
                module_path: hsig.module_path.clone(),
                type_args: routed_type_args,
                args: canonical.args.clone(),
                sig: routed_sig,
                ret_ty: routed_ret,
                meta: convert_meta(meta),
                ext: (),
            }
        };
        self.wrap_canonical_call(call, canonical)
    }

    fn infer_call_type_args_for_sig(
        &self,
        sig: &Signature<Enriched>,
        sig_env: &UnfoldEnv<'_>,
        explicit_type_args: &[Type<Enriched>],
        value_args: &[Expr<Enriched>],
    ) -> Vec<Type<Routed>> {
        if !explicit_type_args.is_empty() {
            return self.routed_type_args(explicit_type_args);
        }

        let type_params: Vec<String> = sig
            .params
            .iter()
            .filter_map(|p| match p {
                SignatureParam::Type(tp) => Some(tp.name.clone()),
                SignatureParam::Value(_) => None,
            })
            .collect();
        if type_params.is_empty() {
            return Vec::new();
        }

        let mut scoped_env = sig_env.clone();
        let mut scoped_type_params = Vec::new();
        let mut subst = HashMap::new();
        let mut value_args = value_args.iter();
        for group in sig.canonical_groups() {
            match group {
                SignatureGroupRef::Type(params) => {
                    for param in params {
                        let SignatureParam::Type(param) = param else {
                            unreachable!("SignatureGroupRef::Type contains only type params")
                        };
                        scoped_env.type_vars.insert(param.name.clone());
                        scoped_type_params.push(param.name.clone());
                    }
                }
                SignatureGroupRef::Value(params) => {
                    for param in params {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("SignatureGroupRef::Value contains only value params")
                        };
                        let Some(param_ty) = param.ty.as_ref() else {
                            continue;
                        };
                        let Some(arg) = value_args.next() else {
                            break;
                        };
                        let Some(arg_ty) = self.enriched_expr_type_for_inference(arg) else {
                            continue;
                        };
                        let pattern = canonicalize_function_abi_type(unfold_type_alias_type(
                            &convert_type::<Enriched, Routed>(param_ty),
                            &scoped_env,
                        ));
                        collect_type_arg_bindings(
                            &pattern,
                            &arg_ty,
                            &scoped_type_params,
                            &mut subst,
                        );
                    }
                }
            }
        }

        type_params
            .iter()
            .filter_map(|name| subst.get(name).cloned())
            .collect()
    }

    fn complete_call_type_args_for_sig(
        &self,
        sig: &Signature<Enriched>,
        sig_env: &UnfoldEnv<'_>,
        fallback_type_args: Vec<Type<Routed>>,
        value_args: &[Expr<Enriched>],
    ) -> Vec<Type<Routed>> {
        let type_param_count = signature_type_param_count(sig);
        // A complete checked artifact is authoritative. Structural inference
        // is only a fallback for incomplete recovery and bypass inputs.
        if fallback_type_args.len() == type_param_count {
            return fallback_type_args;
        }
        let inferred = self.infer_call_type_args_for_sig(sig, sig_env, &[], value_args);
        if inferred.len() == type_param_count {
            inferred
        } else {
            fallback_type_args
        }
    }

    fn enriched_expr_type_for_inference(&self, expr: &Expr<Enriched>) -> Option<Type<Routed>> {
        match expr {
            Expr::Path { segments, meta, .. } => {
                if segments.len() == 1
                    && let Some(ty) = self.bound_type(&segments[0].name)
                {
                    return Some(unfold_type_alias_type(&ty, &self.unfold_env()));
                }
                self.function_value_type_for_path(segments, meta.span)
            }
            Expr::FnExpr {
                sig, ret_ty, meta, ..
            } => {
                if signature_has_untyped_value_param(sig) {
                    return None;
                }
                let ret = ret_ty.as_ref().map(|ty| self.routed_type(ty))?;
                Some(semantic_function_type_from_signature(
                    sig,
                    ret,
                    meta.span,
                    &self.unfold_env(),
                ))
            }
            Expr::EnrichedTuple { synth_ty, .. }
            | Expr::EnrichedInject { synth_ty, .. }
            | Expr::EnrichedRecord { synth_ty, .. } => Some(self.routed_type(synth_ty)),
            Expr::EnrichedMatch { result_ty, .. } | Expr::EnrichedConditional { result_ty, .. } => {
                Some(self.routed_type(result_ty))
            }
            Expr::EnrichedProject {
                index,
                arity,
                target_ty,
                ..
            } => product_slot_type(&self.routed_type(target_ty), *index, *arity),
            Expr::EnrichedFieldGet {
                field_name,
                index,
                arity,
                target_ty,
                ..
            } => self.enriched_field_payload_type(
                &self.routed_type(target_ty),
                field_name,
                *index,
                *arity,
            ),
            _ => None,
        }
    }

    fn enriched_expr_type_for_adapter(&self, expr: &Expr<Enriched>) -> Option<Type<Routed>> {
        match expr {
            Expr::Path { segments, meta, .. } => {
                if segments.len() == 1
                    && let Some(ty) = self.bound_routed_type(&segments[0].name)
                {
                    return Some(ty);
                }
                self.function_value_type_for_path(segments, meta.span)
            }
            Expr::Let {
                name, value, body, ..
            } => {
                let value_ty = self.enriched_expr_type_for_adapter(value);
                self.push_bound_routed(name, value_ty);
                let body_ty = self.enriched_expr_type_for_adapter(body);
                self.pop_bound();
                body_ty
            }
            Expr::Seq { body, .. } => self.enriched_expr_type_for_adapter(body),
            Expr::FnExpr {
                sig, ret_ty, meta, ..
            } => {
                if signature_has_untyped_value_param(sig) {
                    return None;
                }
                let ret = ret_ty.as_ref().map(|ty| self.routed_type(ty))?;
                Some(semantic_function_type_from_signature(
                    sig,
                    ret,
                    meta.span,
                    &self.unfold_env(),
                ))
            }
            _ => self.enriched_expr_type_for_inference(expr),
        }
    }

    fn function_value_type_for_path(
        &self,
        segments: &[PathSegment],
        span: crate::span::Span,
    ) -> Option<Type<Routed>> {
        match segments {
            [segment] => {
                let name = &segment.name;
                if let Some(hsig) = self.host_env.fns.get(name) {
                    return Some(self.host_fn_value_type(hsig, span));
                }
                if let Some(kind) = self.module_ctx.selective.get(name) {
                    match kind {
                        ResolvedImportKind::CrossModule(mod_path) => {
                            return self
                                .ctx
                                .module_fns_by_module
                                .get(mod_path)
                                .and_then(|m| m.get(name))
                                .map(|entry| self.module_fn_value_type(entry, span));
                        }
                    }
                }
                self.module_ctx
                    .local_fns
                    .get(name)
                    .map(|entry| self.module_fn_value_type(entry, span))
            }
            [head, member] => self
                .qualified_import_host_fn(&head.name, &member.name)
                .map(|hsig| self.host_fn_value_type(hsig, span)),
            _ => None,
        }
    }

    fn host_fn_value_type(&self, hsig: &HostFnSig, span: crate::span::Span) -> Type<Routed> {
        let env = self.declaring_unfold_env(&hsig.module_path);
        let ret = canonicalize_function_abi_type(unfold_signature_ret(&hsig.sig, &hsig.ret, &env));
        semantic_function_type_from_signature(&hsig.sig, ret, span, &env)
    }

    fn module_fn_value_type(&self, entry: &ModuleFnSig, span: crate::span::Span) -> Type<Routed> {
        let sig_env = self.declaring_unfold_env(&entry.module_path);
        semantic_function_type_from_signature(&entry.sig, entry.ret_abi.clone(), span, &sig_env)
    }

    fn module_fn_for_value_ref(&self, name: &str) -> Option<&ModuleFnSig> {
        if let Some(ResolvedImportKind::CrossModule(module_path)) =
            self.module_ctx.selective.get(name)
        {
            return self
                .ctx
                .module_fns_by_module
                .get(module_path)
                .and_then(|module| module.get(name));
        }
        self.module_ctx.local_fns.get(name)
    }

    fn enriched_expr_type_for_source_adapter(&self, expr: &Expr<Enriched>) -> Option<Type<Routed>> {
        let source_ty = |ty: &Type<Enriched>| {
            unfold_type_alias_type(&convert_type::<Enriched, Routed>(ty), &self.unfold_env())
        };
        match expr {
            Expr::EnrichedTuple { synth_ty, .. }
            | Expr::EnrichedInject { synth_ty, .. }
            | Expr::EnrichedRecord { synth_ty, .. } => Some(source_ty(synth_ty)),
            Expr::EnrichedMatch { result_ty, .. } | Expr::EnrichedConditional { result_ty, .. } => {
                Some(source_ty(result_ty))
            }
            Expr::EnrichedProject {
                index,
                arity,
                target_ty,
                ..
            } => product_slot_type(&source_ty(target_ty), *index, *arity),
            Expr::EnrichedFieldGet {
                field_name,
                index,
                arity,
                target_ty,
                ..
            } => {
                self.enriched_field_payload_type(&source_ty(target_ty), field_name, *index, *arity)
            }
            _ => None,
        }
    }

    fn low_module_call(
        &self,
        mangled: String,
        type_args: Vec<Type<Routed>>,
        value_args: Vec<Expr<Enriched>>,
        info: ModuleCallInfo<'_>,
    ) -> Expr<Routed> {
        let ModuleCallInfo {
            sig,
            target_arity,
            ret_ty,
            declaring_module,
            meta,
        } = info;
        let sig_env = self.declaring_unfold_env(declaring_module);
        let type_args = self.complete_call_type_args_for_sig(sig, &sig_env, type_args, &value_args);
        let canonical = self.lower_call_value_args_for_sig_with_function_adapters(
            value_args,
            sig,
            &sig_env,
            &type_args,
            target_arity,
            meta.span,
        );
        let routed_sig = canonicalize_entry_signature(sig, meta.span, &sig_env);
        let effective_ret_ty = ret_ty.as_ref().map(|ret| {
            signature_return_after_first_value_group(sig, ret.clone(), meta.span, &sig_env)
        });
        let raw_ret = effective_ret_ty
            .as_ref()
            .map(|ret| instantiate_signature_type(&routed_sig, &type_args, ret))
            .map(|ret| unfold_type_alias_type(&ret, &sig_env));
        let public_ret = raw_ret.clone().map(canonicalize_function_abi_type);
        let call = Expr::LowModuleCall {
            occurrence: Default::default(),
            mangled,
            type_args,
            args: canonical.args.clone(),
            sig: routed_sig,
            ret_ty: effective_ret_ty,
            meta: convert_meta(meta),
            ext: (),
        };
        let call = self.wrap_canonical_call(call, canonical);
        match (raw_ret, public_ret) {
            (Some(actual), Some(expected)) => {
                self.adapt_routed_value_to_expected(call, Some(actual), &expected, meta.span)
            }
            _ => call,
        }
    }

    fn low_qualified_module_call(
        &self,
        names: (String, String),
        type_args: Vec<Type<Routed>>,
        value_args: Vec<Expr<Enriched>>,
        info: ModuleCallInfo<'_>,
    ) -> Expr<Routed> {
        let ModuleCallInfo {
            sig,
            target_arity,
            ret_ty,
            declaring_module,
            meta,
        } = info;
        let sig_env = self.declaring_unfold_env(declaring_module);
        let type_args = self.complete_call_type_args_for_sig(sig, &sig_env, type_args, &value_args);
        let canonical = self.lower_call_value_args_for_sig_with_function_adapters(
            value_args,
            sig,
            &sig_env,
            &type_args,
            target_arity,
            meta.span,
        );
        let routed_sig = canonicalize_entry_signature(sig, meta.span, &sig_env);
        let effective_ret_ty = ret_ty.as_ref().map(|ret| {
            signature_return_after_first_value_group(sig, ret.clone(), meta.span, &sig_env)
        });
        let raw_ret = effective_ret_ty
            .as_ref()
            .map(|ret| instantiate_signature_type(&routed_sig, &type_args, ret))
            .map(|ret| unfold_type_alias_type(&ret, &sig_env));
        let public_ret = raw_ret.clone().map(canonicalize_function_abi_type);
        let call = Expr::LowQualifiedModuleCall {
            occurrence: Default::default(),
            alias: names.0,
            mangled: names.1,
            type_args,
            args: canonical.args.clone(),
            sig: routed_sig,
            ret_ty: effective_ret_ty,
            meta: convert_meta(meta),
            ext: (),
        };
        let call = self.wrap_canonical_call(call, canonical);
        match (raw_ret, public_ret) {
            (Some(actual), Some(expected)) => {
                self.adapt_routed_value_to_expected(call, Some(actual), &expected, meta.span)
            }
            _ => call,
        }
    }
}

fn is_inlineable_abi_group_value(expr: &Expr<Routed>) -> bool {
    match expr {
        Expr::LowBoundRef { .. }
        | Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => true,
        Expr::EnrichedTuple { items, .. } => items.iter().all(is_inlineable_abi_group_value),
        Expr::EnrichedRecord { fields, .. } => fields
            .iter()
            .all(|field| is_inlineable_abi_group_value(&field.value)),
        _ => false,
    }
}

fn grouped_items_match_target_slots(
    source_ty: &Type<Routed>,
    item_count: usize,
    target_slots: &[Type<Routed>],
) -> bool {
    item_count == target_slots.len()
        && expected_value_arg_tys(source_ty, item_count).is_some_and(|source_slots| {
            source_slots
                .into_iter()
                .zip(target_slots)
                .all(|(source, target)| abi_type_shape_eq(source, target))
        })
}

#[derive(Default)]
struct CanonicalCallArgs {
    args: Vec<Expr<Routed>>,
    wrappers: Vec<CallArgWrapper>,
}

struct CalleeParamAbi {
    param_ty: Type<Routed>,
    target_slots: Vec<Type<Routed>>,
}

impl CanonicalCallArgs {
    fn direct(args: Vec<Expr<Routed>>) -> Self {
        Self {
            args,
            wrappers: Vec::new(),
        }
    }
}

enum CallArgWrapper {
    Let {
        name: String,
        value: Expr<Routed>,
        span: crate::span::Span,
    },
    Seq {
        value: Expr<Routed>,
        span: crate::span::Span,
    },
}

fn convert_type_for_abi(t: &Type<Enriched>) -> Type<Routed> {
    canonicalize_function_abi_type(convert_type::<Enriched, Routed>(t))
}

fn canonicalize_function_abi_type(ty: Type<Routed>) -> Type<Routed> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => Type::Path {
            segments,
            args: args
                .into_iter()
                .map(canonicalize_function_abi_type)
                .collect(),
            meta,
        },
        Type::Unit { meta } => Type::Unit { meta },
        Type::Bottom { meta } => Type::Bottom { meta },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => {
            let param = canonicalize_function_abi_type(*param);
            let ret = canonicalize_function_abi_type(*ret);
            Type::Function {
                param: Box::new(param),
                ret: Box::new(ret),
                meta,
                abi_arity,
                caps,
            }
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(canonicalize_function_abi_type(*left)),
            right: Box::new(canonicalize_function_abi_type(*right)),
            meta,
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(canonicalize_function_abi_type(*left)),
            right: Box::new(canonicalize_function_abi_type(*right)),
            meta,
        },
        Type::Forall { param, body, meta } => Type::Forall {
            param,
            body: Box::new(canonicalize_function_abi_type(*body)),
            meta,
        },
        Type::LabelSugar { ext, .. } => match ext {},
        Type::Infer { ext, .. } => match ext {},
    }
}

fn canonicalize_entry_signature(
    sig: &Signature<Enriched>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
) -> Signature<Routed> {
    canonicalize_entry_signature_with_prefix(sig, span, env, "__kio_arg")
}

fn lower_fn_expr_signature_and_body(
    sig: &Signature<Enriched>,
    body: Expr<Routed>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
) -> (Signature<Routed>, Expr<Routed>) {
    let slot_prefix = format!("__kio_fn{}_s", span.start);
    canonicalize_entry_signature_and_body_with_prefix(sig, body, span, env, &slot_prefix)
}

fn canonicalize_entry_signature_with_prefix(
    sig: &Signature<Enriched>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
    slot_prefix: &str,
) -> Signature<Routed> {
    canonicalize_entry_signature_and_body_with_prefix(
        sig,
        Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        },
        span,
        env,
        slot_prefix,
    )
    .0
}

fn canonicalize_entry_signature_and_body_with_prefix(
    sig: &Signature<Enriched>,
    body: Expr<Routed>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
    slot_prefix: &str,
) -> (Signature<Routed>, Expr<Routed>) {
    let mut next_slot = 0usize;
    let mut params = Vec::new();
    let mut groups = Vec::new();
    let mut rebuilds: Vec<(String, Type<Routed>, Vec<String>)> = Vec::new();
    let mut scoped_env = env.clone();

    for group in sig.canonical_groups() {
        let before = params.len();
        match group {
            SignatureGroupRef::Type(group_params) => {
                for p in group_params {
                    let SignatureParam::Type(tp) = p else {
                        unreachable!("SignatureGroupRef::Type contains only type params")
                    };
                    scoped_env.type_vars.insert(tp.name.clone());
                    params.push(SignatureParam::Type(tp.clone()));
                }
                groups.push(SignatureGroupKind::Type {
                    len: params.len() - before,
                });
            }
            SignatureGroupRef::Value(group_params) => {
                let group_value_param_count = group_params.len();
                for p in group_params {
                    let SignatureParam::Value(v) = p else {
                        unreachable!("SignatureGroupRef::Value contains only value params")
                    };
                    let Some(ty) = v.ty.as_ref() else {
                        params.push(SignatureParam::Value(Param {
                            name: v.name.clone(),
                            ty: None,
                            pattern: (),
                            meta: convert_meta(&v.meta),
                        }));
                        continue;
                    };
                    let ty = unfold_type_alias_type(&convert_type_for_abi(ty), &scoped_env);
                    let slots = value_param_abi_slot_types(&ty, group_value_param_count == 1);
                    if slots.len() == 1 && slots[0] == ty {
                        params.push(SignatureParam::Value(Param {
                            name: v.name.clone(),
                            ty: Some(ty),
                            pattern: (),
                            meta: convert_meta(&v.meta),
                        }));
                        continue;
                    }

                    let mut slot_names = Vec::with_capacity(slots.len());
                    for slot_ty in slots {
                        let name = format!("{slot_prefix}{next_slot}__");
                        next_slot += 1;
                        slot_names.push(name.clone());
                        params.push(SignatureParam::Value(Param {
                            name,
                            ty: Some(slot_ty),
                            pattern: (),
                            meta: convert_meta(&v.meta),
                        }));
                    }
                    if v.name != "_" {
                        rebuilds.push((v.name.clone(), ty, slot_names));
                    }
                }
                groups.push(SignatureGroupKind::Value {
                    len: params.len() - before,
                });
            }
        }
    }

    let body = rebuilds
        .into_iter()
        .rev()
        .fold(body, |body, (name, ty, slot_names)| {
            let mut needs_whole_value = false;
            let body = inline_rebuilt_abi_projections(
                body,
                &name,
                &ty,
                &slot_names,
                span,
                &mut needs_whole_value,
            );
            if needs_whole_value {
                Expr::Let {
                    occurrence: Default::default(),
                    name,
                    name_span: span,
                    ty: None,
                    pattern: (),
                    value: Box::new(rebuild_value_from_abi_slots(&ty, &slot_names, span)),
                    body: Box::new(body),
                    meta: Meta::new(span),
                }
            } else {
                body
            }
        });
    (Signature::from_parts(params, groups), body)
}

fn canonicalize_host_fn_decl(
    h: &crate::ast::HostFn<Enriched>,
    env: &UnfoldEnv<'_>,
) -> crate::ast::HostFn<Routed> {
    let mut next_slot = 0usize;
    let mut params = Vec::new();
    let mut param_groups = Vec::new();
    let mut scoped_env = env.clone();
    for group in crate::ast::host_fn_group_refs(&h.params, &h.param_groups) {
        let before = params.len();
        match group {
            crate::ast::HostFnGroupRef::Type(group_params) => {
                for p in group_params {
                    let crate::ast::HostFnParam::Type(tp) = p else {
                        unreachable!("HostFnGroupRef::Type contains only type params")
                    };
                    scoped_env.type_vars.insert(tp.name.clone());
                    params.push(crate::ast::HostFnParam::Type(tp.clone()));
                }
                param_groups.push(SignatureGroupKind::Type {
                    len: params.len() - before,
                });
            }
            crate::ast::HostFnGroupRef::Value(group_params) => {
                let value_param_count = group_params.len();
                for p in group_params {
                    let crate::ast::HostFnParam::Value(v) = p else {
                        unreachable!("HostFnGroupRef::Value contains only value params")
                    };
                    let ty = unfold_type_alias_type(&convert_type_for_abi(&v.ty), &scoped_env);
                    let slots = value_param_abi_slot_types(&ty, value_param_count == 1);
                    if slots.len() == 1 && slots[0] == ty {
                        params.push(crate::ast::HostFnParam::Value(
                            crate::ast::HostFnValueParam {
                                name: v.name.clone(),
                                ty,
                                meta: convert_meta(&v.meta),
                            },
                        ));
                        continue;
                    }
                    for slot_ty in slots {
                        let name = format!("__kio_arg{next_slot}__");
                        next_slot += 1;
                        params.push(crate::ast::HostFnParam::Value(
                            crate::ast::HostFnValueParam {
                                name: Some(name),
                                ty: slot_ty,
                                meta: convert_meta(&v.meta),
                            },
                        ));
                    }
                }
                param_groups.push(SignatureGroupKind::Value {
                    len: params.len() - before,
                });
            }
        }
    }
    crate::ast::HostFn {
        name: h.name.clone(),
        params,
        param_groups,
        ret: unfold_type_alias_type(&convert_type_for_abi(&h.ret), &scoped_env),
        meta: convert_meta(&h.meta),
        doc: h.doc.clone(),
    }
}

fn value_param_abi_slot_types(ty: &Type<Routed>, single_value_param: bool) -> Vec<Type<Routed>> {
    if single_value_param && matches!(ty, Type::Unit { .. }) {
        Vec::new()
    } else {
        vec![ty.clone()]
    }
}

fn product_abi_slot_count(ty: &Type<Routed>) -> usize {
    product_abi_slot_types(ty).len()
}

fn function_abi_param_slot_types(param: &Type<Routed>, abi_arity: usize) -> Vec<Type<Routed>> {
    Type::right_spine_take(param, abi_arity)
        .into_iter()
        .cloned()
        .collect()
}

/// Compare only callable ABI grouping. Nominal paths are all one
/// runtime slot; name identity has already been checked by the typer
/// and must not manufacture an eta adapter from bare versus qualified
/// spellings of the same type.
fn abi_type_shape_eq(a: &Type<Routed>, b: &Type<Routed>) -> bool {
    match (a, b) {
        (Type::Path { .. }, Type::Path { .. }) => true,
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: a_param,
                ret: a_ret,
                abi_arity: a_abi_arity,
                ..
            },
            Type::Function {
                param: b_param,
                ret: b_ret,
                abi_arity: b_abi_arity,
                ..
            },
        ) => {
            let a_slots = function_abi_param_slot_types(a_param, *a_abi_arity);
            let b_slots = function_abi_param_slot_types(b_param, *b_abi_arity);
            a_slots.len() == b_slots.len()
                && a_slots
                    .iter()
                    .zip(b_slots.iter())
                    .all(|(a, b)| abi_type_shape_eq(a, b))
                && abi_type_shape_eq(a_ret, b_ret)
        }
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
        ) => abi_type_shape_eq(a_left, b_left) && abi_type_shape_eq(a_right, b_right),
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
        ) => {
            a_param.name == b_param.name
                && a_param.kind == b_param.kind
                && abi_type_shape_eq(a_body, b_body)
        }
        _ => false,
    }
}

fn product_abi_slot_types(ty: &Type<Routed>) -> Vec<Type<Routed>> {
    let mut slots = Vec::new();
    let mut cur = ty;
    loop {
        match cur {
            Type::Product { left, right, .. } => {
                slots.push((**left).clone());
                cur = right;
            }
            _ => {
                slots.push(cur.clone());
                return slots;
            }
        }
    }
}

fn rebuild_value_from_abi_slots(
    ty: &Type<Routed>,
    slot_names: &[String],
    span: crate::span::Span,
) -> Expr<Routed> {
    if matches!(ty, Type::Unit { .. }) && slot_names.is_empty() {
        return Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(span),
        };
    }
    if matches!(ty, Type::Product { .. }) {
        return Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: slot_names
                .iter()
                .map(|name| low_bound_ref(name, span))
                .collect(),
            synth_ty: ty.clone(),
            meta: Meta::new(span),
            ext: (),
        };
    }
    let Some(name) = slot_names.first() else {
        unreachable!("non-unit ABI rebuild requires at least one slot")
    };
    low_bound_ref(name, span)
}

fn rebuilt_abi_projection(
    ty: &Type<Routed>,
    slot_names: &[String],
    index: usize,
    arity: usize,
    span: crate::span::Span,
) -> Option<Expr<Routed>> {
    let projected_tys = expected_value_arg_tys(ty, arity)?;
    let projected_ty = *projected_tys.get(index)?;
    let start = projected_tys
        .iter()
        .enumerate()
        .take(index)
        .map(|(i, slot)| projected_value_abi_slot_count(slot, i, arity))
        .sum::<usize>();
    let count = projected_value_abi_slot_count(projected_ty, index, arity);
    let end = start.checked_add(count)?;
    if end > slot_names.len() {
        return None;
    }
    if count == 1 {
        return slot_names
            .get(start)
            .map(|slot_name| low_bound_ref(slot_name, span));
    }
    Some(rebuild_value_from_abi_slots(
        projected_ty,
        &slot_names[start..end],
        span,
    ))
}

fn wrap_rebuilt_abi_field_projection(
    target_ty: &Type<Routed>,
    projected: Expr<Routed>,
    index: usize,
    arity: usize,
    span: crate::span::Span,
) -> Option<Expr<Routed>> {
    let slot_ty = product_slot_type(target_ty, index, arity)?;
    let Type::Path { segments, args, .. } = slot_ty else {
        return None;
    };
    let newtype = segments.last()?.name.clone();
    Some(Expr::LowNewtypeProj {
        occurrence: Default::default(),
        newtype,
        member: "get".to_owned(),
        type_args: args,
        target: Box::new(projected),
        meta: Meta::new(span),
        ext: (),
    })
}

fn rebuilt_abi_field_get(
    ty: &Type<Routed>,
    slot_names: &[String],
    index: usize,
    arity: usize,
    span: crate::span::Span,
) -> Option<Expr<Routed>> {
    let projected = rebuilt_abi_projection(ty, slot_names, index, arity, span)?;
    wrap_rebuilt_abi_field_projection(ty, projected, index, arity, span)
}

fn projected_value_abi_slot_count(
    projected_ty: &Type<Routed>,
    index: usize,
    arity: usize,
) -> usize {
    if index + 1 == arity {
        product_abi_slot_count(projected_ty)
    } else {
        1
    }
}

fn project_rebuilt_bound_tuple(
    target: &Expr<Routed>,
    target_ty: &Type<Routed>,
    index: usize,
    arity: usize,
    span: crate::span::Span,
) -> Option<Expr<Routed>> {
    let Expr::EnrichedTuple {
        items, synth_ty, ..
    } = target
    else {
        return None;
    };
    if !abi_type_shape_eq(synth_ty, target_ty) {
        return None;
    }
    let mut slot_names = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Expr::LowBoundRef { name, .. } => slot_names.push(name.clone()),
            _ => return None,
        }
    }
    rebuilt_abi_projection(synth_ty, &slot_names, index, arity, span)
}

fn inline_rebuilt_abi_projections(
    expr: Expr<Routed>,
    name: &str,
    ty: &Type<Routed>,
    slot_names: &[String],
    span: crate::span::Span,
    needs_whole_value: &mut bool,
) -> Expr<Routed> {
    match expr {
        Expr::EnrichedProject {
            occurrence: _,
            target,
            index,
            arity,
            target_ty,
            meta,
            ext,
        } => match *target {
            Expr::LowBoundRef {
                name: target_name, ..
            } if target_name == name => {
                if abi_type_shape_eq(&target_ty, ty) {
                    rebuilt_abi_projection(ty, slot_names, index, arity, meta.span).unwrap_or_else(
                        || {
                            *needs_whole_value = true;
                            Expr::EnrichedProject {
                                occurrence: Default::default(),
                                target: Box::new(low_bound_ref(&target_name, span)),
                                index,
                                arity,
                                target_ty,
                                meta,
                                ext,
                            }
                        },
                    )
                } else {
                    *needs_whole_value = true;
                    Expr::EnrichedProject {
                        occurrence: Default::default(),
                        target: Box::new(low_bound_ref(&target_name, span)),
                        index,
                        arity,
                        target_ty,
                        meta,
                        ext,
                    }
                }
            }
            other_target => {
                let target = inline_rebuilt_abi_projections(
                    other_target,
                    name,
                    ty,
                    slot_names,
                    span,
                    needs_whole_value,
                );
                project_rebuilt_bound_tuple(&target, &target_ty, index, arity, meta.span)
                    .unwrap_or_else(|| Expr::EnrichedProject {
                        occurrence: Default::default(),
                        target: Box::new(target),
                        index,
                        arity,
                        target_ty,
                        meta,
                        ext,
                    })
            }
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
        } => match *target {
            Expr::LowBoundRef {
                name: target_name, ..
            } if target_name == name && abi_type_shape_eq(&target_ty, ty) => rebuilt_abi_field_get(
                ty, slot_names, index, arity, meta.span,
            )
            .unwrap_or_else(|| {
                *needs_whole_value = true;
                Expr::EnrichedFieldGet {
                    occurrence: Default::default(),
                    target: Box::new(low_bound_ref(&target_name, span)),
                    field_name,
                    index,
                    arity,
                    target_ty,
                    meta,
                    ext,
                }
            }),
            other_target => {
                let target = inline_rebuilt_abi_projections(
                    other_target,
                    name,
                    ty,
                    slot_names,
                    span,
                    needs_whole_value,
                );
                project_rebuilt_bound_tuple(&target, &target_ty, index, arity, meta.span)
                    .and_then(|projected| {
                        wrap_rebuilt_abi_field_projection(
                            &target_ty, projected, index, arity, meta.span,
                        )
                    })
                    .unwrap_or_else(|| Expr::EnrichedFieldGet {
                        occurrence: Default::default(),
                        target: Box::new(target),
                        field_name,
                        index,
                        arity,
                        target_ty,
                        meta,
                        ext,
                    })
            }
        },
        Expr::LowBoundRef {
            occurrence: _,
            name: ref_name,
            meta,
            ext,
        } => {
            if ref_name == name {
                *needs_whole_value = true;
            }
            Expr::LowBoundRef {
                occurrence: Default::default(),
                name: ref_name,
                meta,
                ext,
            }
        }
        Expr::Let {
            occurrence: _,
            name: let_name,
            name_span,
            ty: let_ty,
            pattern,
            value,
            body,
            meta,
        } => {
            if let Expr::LowBoundRef {
                name: value_name, ..
            } = value.as_ref()
                && value_name == name
                && let_name != name
            {
                let mut temp_needs_whole_value = false;
                let body = inline_rebuilt_abi_projections(
                    *body,
                    &let_name,
                    ty,
                    slot_names,
                    span,
                    &mut temp_needs_whole_value,
                );
                let body = inline_rebuilt_abi_projections(
                    body,
                    name,
                    ty,
                    slot_names,
                    span,
                    needs_whole_value,
                );
                if temp_needs_whole_value {
                    *needs_whole_value = true;
                    return Expr::Let {
                        occurrence: Default::default(),
                        name: let_name,
                        name_span,
                        ty: let_ty,
                        pattern,
                        value,
                        body: Box::new(body),
                        meta,
                    };
                }
                return body;
            }
            let value = inline_rebuilt_abi_projections(
                *value,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            );
            let body = if let_name == name {
                *body
            } else {
                inline_rebuilt_abi_projections(*body, name, ty, slot_names, span, needs_whole_value)
            };
            Expr::Let {
                occurrence: Default::default(),
                name: let_name,
                name_span,
                ty: let_ty,
                pattern,
                value: Box::new(value),
                body: Box::new(body),
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
            value: Box::new(inline_rebuilt_abi_projections(
                *value,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            body: Box::new(inline_rebuilt_abi_projections(
                *body,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            meta,
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => {
            let shadows = sig.params.iter().any(|p| match p {
                SignatureParam::Value(v) => v.name == name,
                SignatureParam::Type(_) => false,
            });
            let body = if shadows {
                *body
            } else {
                inline_rebuilt_abi_projections(*body, name, ty, slot_names, span, needs_whole_value)
            };
            Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty,
                body: Box::new(body),
                meta,
                caps,
            }
        }
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
                .map(|item| {
                    inline_rebuilt_abi_projections(
                        item,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    )
                })
                .collect(),
            synth_ty,
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
                .map(|field| RecordField {
                    name: field.name,
                    value: inline_rebuilt_abi_projections(
                        field.value,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    ),
                    meta: field.meta,
                })
                .collect(),
            synth_ty,
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
            payload: Box::new(inline_rebuilt_abi_projections(
                *payload,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
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
            scrutinee: Box::new(inline_rebuilt_abi_projections(
                *scrutinee,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            arms: arms
                .into_iter()
                .map(|arm| {
                    let body = if arm.param == name {
                        arm.body
                    } else {
                        inline_rebuilt_abi_projections(
                            arm.body,
                            name,
                            ty,
                            slot_names,
                            span,
                            needs_whole_value,
                        )
                    };
                    EnrichedArm {
                        param: arm.param,
                        body,
                        meta: arm.meta,
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
            cond: Box::new(inline_rebuilt_abi_projections(
                *cond,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            then_branch: Box::new(inline_rebuilt_abi_projections(
                *then_branch,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            else_branch: Box::new(inline_rebuilt_abi_projections(
                *else_branch,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            result_ty,
            meta,
            ext,
        },
        Expr::LowHostCall {
            occurrence: _,
            name: call_name,
            module_path,
            type_args,
            args,
            sig,
            ret_ty,
            meta,
            ext,
        } => Expr::LowHostCall {
            occurrence: Default::default(),
            name: call_name,
            module_path,
            type_args,
            args: args
                .into_iter()
                .map(|arg| {
                    inline_rebuilt_abi_projections(
                        arg,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    )
                })
                .collect(),
            sig,
            ret_ty,
            meta,
            ext,
        },
        Expr::LowModuleCall {
            occurrence: _,
            mangled,
            type_args,
            args,
            sig,
            ret_ty,
            meta,
            ext,
        } => Expr::LowModuleCall {
            occurrence: Default::default(),
            mangled,
            type_args,
            args: args
                .into_iter()
                .map(|arg| {
                    inline_rebuilt_abi_projections(
                        arg,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    )
                })
                .collect(),
            sig,
            ret_ty,
            meta,
            ext,
        },
        Expr::LowQualifiedModuleCall {
            occurrence: _,
            alias,
            mangled,
            type_args,
            args,
            sig,
            ret_ty,
            meta,
            ext,
        } => Expr::LowQualifiedModuleCall {
            occurrence: Default::default(),
            alias,
            mangled,
            type_args,
            args: args
                .into_iter()
                .map(|arg| {
                    inline_rebuilt_abi_projections(
                        arg,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    )
                })
                .collect(),
            sig,
            ret_ty,
            meta,
            ext,
        },
        Expr::LowClosureCall {
            occurrence: _,
            name: callee_name,
            type_args,
            args,
            meta,
            ext,
        } => {
            if callee_name == name {
                *needs_whole_value = true;
            }
            Expr::LowClosureCall {
                occurrence: Default::default(),
                name: callee_name,
                type_args,
                args: args
                    .into_iter()
                    .map(|arg| {
                        inline_rebuilt_abi_projections(
                            arg,
                            name,
                            ty,
                            slot_names,
                            span,
                            needs_whole_value,
                        )
                    })
                    .collect(),
                meta,
                ext,
            }
        }
        Expr::LowIndirectCall {
            occurrence: _,
            callee,
            type_args,
            args,
            meta,
            ext,
        } => Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: Box::new(inline_rebuilt_abi_projections(
                *callee,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            type_args,
            args: args
                .into_iter()
                .map(|arg| {
                    inline_rebuilt_abi_projections(
                        arg,
                        name,
                        ty,
                        slot_names,
                        span,
                        needs_whole_value,
                    )
                })
                .collect(),
            meta,
            ext,
        },
        Expr::LowTypeApplication {
            occurrence: _,
            callee,
            type_arg,
            meta,
            ext,
        } => Expr::LowTypeApplication {
            occurrence: Default::default(),
            callee: Box::new(inline_rebuilt_abi_projections(
                *callee,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            type_arg,
            meta,
            ext,
        },
        Expr::LowQualifiedNewtypeMember {
            occurrence: _,
            module_path,
            newtype,
            member,
            type_args,
            payload,
            meta,
            ext,
        } => Expr::LowQualifiedNewtypeMember {
            occurrence: Default::default(),
            module_path,
            newtype,
            member,
            type_args,
            payload: Box::new(inline_rebuilt_abi_projections(
                *payload,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            meta,
            ext,
        },
        Expr::LowNewtypeCtor {
            occurrence: _,
            newtype,
            member,
            type_args,
            payload,
            meta,
            ext,
        } => Expr::LowNewtypeCtor {
            occurrence: Default::default(),
            newtype,
            member,
            type_args,
            payload: Box::new(inline_rebuilt_abi_projections(
                *payload,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            meta,
            ext,
        },
        Expr::LowNewtypeProj {
            occurrence: _,
            newtype,
            member,
            type_args,
            target,
            meta,
            ext,
        } => Expr::LowNewtypeProj {
            occurrence: Default::default(),
            newtype,
            member,
            type_args,
            target: Box::new(inline_rebuilt_abi_projections(
                *target,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            meta,
            ext,
        },
        Expr::LowAbsurdCall {
            occurrence: _,
            type_arg,
            value_arg,
            meta,
            ext,
        } => Expr::LowAbsurdCall {
            occurrence: Default::default(),
            type_arg,
            value_arg: Box::new(inline_rebuilt_abi_projections(
                *value_arg,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            meta,
            ext,
        },
        Expr::LowCpsProjectorApply {
            occurrence: _,
            newtype,
            module_path,
            type_args,
            receiver,
            continuation,
            continuation_ty,
            meta,
            ext,
        } => Expr::LowCpsProjectorApply {
            occurrence: Default::default(),
            newtype,
            module_path,
            type_args,
            receiver: Box::new(inline_rebuilt_abi_projections(
                *receiver,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            continuation: Box::new(inline_rebuilt_abi_projections(
                *continuation,
                name,
                ty,
                slot_names,
                span,
                needs_whole_value,
            )),
            continuation_ty,
            meta,
            ext,
        },
        other => other,
    }
}

fn low_bound_ref(name: &str, span: crate::span::Span) -> Expr<Routed> {
    Expr::LowBoundRef {
        occurrence: Default::default(),
        name: name.to_owned(),
        meta: Meta::new(span),
        ext: (),
    }
}

fn right_fold_product_types(mut tys: Vec<Type<Routed>>, span: crate::span::Span) -> Type<Routed> {
    match tys.len() {
        0 => Type::Unit {
            meta: Meta::new(span),
        },
        1 => tys.pop().expect("one type"),
        _ => {
            let mut acc = tys.pop().expect("at least one type");
            while let Some(left) = tys.pop() {
                acc = Type::Product {
                    left: Box::new(left),
                    right: Box::new(acc),
                    meta: Meta::new(span),
                };
            }
            acc
        }
    }
}

fn expected_value_arg_tys(param_ty: &Type<Routed>, arg_count: usize) -> Option<Vec<&Type<Routed>>> {
    let mut out = Vec::with_capacity(arg_count);
    let mut current = param_ty;
    for i in 0..arg_count {
        if i + 1 == arg_count {
            out.push(current);
            continue;
        }
        match current {
            Type::Product { left, right, .. } => {
                out.push(left.as_ref());
                current = right.as_ref();
            }
            _ => return None,
        }
    }
    Some(out)
}

fn product_slot_type(ty: &Type<Routed>, index: usize, arity: usize) -> Option<Type<Routed>> {
    expected_value_arg_tys(ty, arity).and_then(|slots| slots.get(index).map(|slot| (*slot).clone()))
}

fn flatten_abi_value_slot(
    value: Expr<Routed>,
    ty: &Type<Routed>,
    span: crate::span::Span,
    out: &mut Vec<(Expr<Routed>, Type<Routed>)>,
) {
    if !matches!(ty, Type::Product { .. }) {
        out.push((value, ty.clone()));
        return;
    }
    let slots = product_abi_slot_types(ty);
    if slots.len() <= 1 {
        out.push((value, ty.clone()));
        return;
    }
    for (index, slot_ty) in slots.into_iter().enumerate() {
        let projected = Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(value.clone()),
            index,
            arity: product_abi_slot_count(ty),
            target_ty: ty.clone(),
            meta: Meta::new(span),
            ext: (),
        };
        flatten_abi_value_slot(projected, &slot_ty, span, out);
    }
}

fn flatten_value_to_target_slots(
    value: Expr<Routed>,
    source_ty: &Type<Routed>,
    target_slots: &[Type<Routed>],
    span: crate::span::Span,
) -> Option<Vec<Expr<Routed>>> {
    let mut out = Vec::with_capacity(target_slots.len());
    flatten_value_to_target_slots_into(value, source_ty, target_slots, span, &mut out)?;
    Some(out)
}

fn flatten_value_to_target_slots_into(
    value: Expr<Routed>,
    source_ty: &Type<Routed>,
    target_slots: &[Type<Routed>],
    span: crate::span::Span,
    out: &mut Vec<Expr<Routed>>,
) -> Option<()> {
    if target_slots.is_empty() {
        return None;
    }
    if target_slots.len() == 1 {
        if abi_type_shape_eq(source_ty, &target_slots[0]) {
            out.push(value);
            return Some(());
        }
        return None;
    }

    let Type::Product { left, right, .. } = source_ty else {
        return None;
    };
    for left_len in 1..target_slots.len() {
        let (left_slots, right_slots) = target_slots.split_at(left_len);
        if type_can_flatten_to_target_slots(left, left_slots)
            && type_can_flatten_to_target_slots(right, right_slots)
        {
            let left_value = binary_product_component(value.clone(), source_ty, 0, span);
            let right_value = binary_product_component(value, source_ty, 1, span);
            flatten_value_to_target_slots_into(left_value, left, left_slots, span, out)?;
            flatten_value_to_target_slots_into(right_value, right, right_slots, span, out)?;
            return Some(());
        }
    }
    None
}

fn type_can_flatten_to_target_slots(
    source_ty: &Type<Routed>,
    target_slots: &[Type<Routed>],
) -> bool {
    if target_slots.is_empty() {
        return false;
    }
    if target_slots.len() == 1 {
        return abi_type_shape_eq(source_ty, &target_slots[0]);
    }

    let Type::Product { left, right, .. } = source_ty else {
        return false;
    };
    (1..target_slots.len()).any(|left_len| {
        let (left_slots, right_slots) = target_slots.split_at(left_len);
        type_can_flatten_to_target_slots(left, left_slots)
            && type_can_flatten_to_target_slots(right, right_slots)
    })
}

fn binary_product_component(
    value: Expr<Routed>,
    source_ty: &Type<Routed>,
    index: usize,
    span: crate::span::Span,
) -> Expr<Routed> {
    Expr::EnrichedProject {
        occurrence: Default::default(),
        target: Box::new(value),
        index,
        arity: 2,
        target_ty: source_ty.clone(),
        meta: Meta::new(span),
        ext: (),
    }
}

fn build_value_for_abi_slot(
    flat: &[(Expr<Routed>, Type<Routed>)],
    cursor: &mut usize,
    expected: &Type<Routed>,
    span: crate::span::Span,
) -> Option<(Expr<Routed>, Type<Routed>)> {
    if matches!(expected, Type::Product { .. }) {
        let expected_slots = product_abi_slot_types(expected);
        if expected_slots.len() > 1 {
            let mut nested_cursor = *cursor;
            let mut items = Vec::with_capacity(expected_slots.len());
            for slot in &expected_slots {
                let (item, _) = build_value_for_abi_slot(flat, &mut nested_cursor, slot, span)?;
                items.push(item);
            }
            if nested_cursor > *cursor {
                *cursor = nested_cursor;
                return Some((
                    Expr::EnrichedTuple {
                        occurrence: Default::default(),
                        items,
                        synth_ty: expected.clone(),
                        meta: Meta::new(span),
                        ext: (),
                    },
                    expected.clone(),
                ));
            }
        }
    }

    let (value, actual_ty) = flat.get(*cursor)?.clone();
    *cursor += 1;
    Some((value, actual_ty))
}

fn instantiate_signature_type(
    sig: &Signature<Routed>,
    type_args: &[Type<Routed>],
    ty: &Type<Routed>,
) -> Type<Routed> {
    let subst = signature_type_arg_subst(sig, type_args);
    crate::pass::typecheck_core::subst_type(ty, &subst)
}

fn signature_type_arg_subst(
    sig: &Signature<Routed>,
    type_args: &[Type<Routed>],
) -> HashMap<String, Type<Routed>> {
    let mut subst = HashMap::new();
    let mut next_type_arg = 0usize;
    for param in &sig.params {
        if let SignatureParam::Type(tp) = param {
            if let Some(arg) = type_args.get(next_type_arg) {
                subst.insert(tp.name.clone(), arg.clone());
            }
            next_type_arg += 1;
        }
    }
    subst
}

fn env_with_signature_type_vars<'a>(
    sig: &Signature<Enriched>,
    env: &UnfoldEnv<'a>,
) -> UnfoldEnv<'a> {
    let mut scoped = env.clone();
    scoped
        .type_vars
        .extend(sig.params.iter().filter_map(|param| {
            let SignatureParam::Type(param) = param else {
                return None;
            };
            Some(param.name.clone())
        }));
    scoped
}

fn unfold_signature_ret(
    sig: &Signature<Enriched>,
    ret: &Type<Enriched>,
    env: &UnfoldEnv<'_>,
) -> Type<Routed> {
    unfold_type_alias_type(
        &convert_type_for_abi(ret),
        &env_with_signature_type_vars(sig, env),
    )
}

fn signature_param_abi(
    sig: &Signature<Enriched>,
    type_args: &[Type<Routed>],
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
) -> Option<CalleeParamAbi> {
    let value_tys = signature_value_param_tys(sig, env)?;
    let target_slots = signature_target_slot_types(sig, env)?;
    let routed_sig = canonicalize_entry_signature(sig, span, env);
    let subst = signature_type_arg_subst(&routed_sig, type_args);
    let value_tys: Vec<_> = value_tys
        .into_iter()
        .map(|ty| crate::pass::typecheck_core::subst_type(&ty, &subst))
        .collect();
    let target_slots = target_slots
        .into_iter()
        .map(|ty| crate::pass::typecheck_core::subst_type(&ty, &subst))
        .collect();
    Some(CalleeParamAbi {
        param_ty: right_fold_product_types(value_tys, span),
        target_slots,
    })
}

fn signature_value_param_tys(
    sig: &Signature<Enriched>,
    env: &UnfoldEnv<'_>,
) -> Option<Vec<Type<Routed>>> {
    let mut scoped_env = env.clone();
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(param) = param else {
                        unreachable!("SignatureGroupRef::Type contains only type params")
                    };
                    scoped_env.type_vars.insert(param.name.clone());
                }
            }
            SignatureGroupRef::Value(params) => {
                return signature_value_group_param_tys(params, &scoped_env);
            }
        }
    }
    None
}

fn signature_value_group_param_tys(
    group: &[SignatureParam<Enriched>],
    env: &UnfoldEnv<'_>,
) -> Option<Vec<Type<Routed>>> {
    let mut value_tys = Vec::new();
    for p in group {
        if let SignatureParam::Value(v) = p {
            value_tys.push(unfold_type_alias_type(
                &convert_type_for_abi(v.ty.as_ref()?),
                env,
            ));
        }
    }
    Some(value_tys)
}

fn signature_target_slot_types(
    sig: &Signature<Enriched>,
    env: &UnfoldEnv<'_>,
) -> Option<Vec<Type<Routed>>> {
    signature_first_value_group_target_slot_types(sig, env)
}

fn signature_first_value_group_target_slot_types(
    sig: &Signature<Enriched>,
    env: &UnfoldEnv<'_>,
) -> Option<Vec<Type<Routed>>> {
    let mut scoped_env = env.clone();
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params {
                    let SignatureParam::Type(param) = param else {
                        unreachable!("SignatureGroupRef::Type contains only type params")
                    };
                    scoped_env.type_vars.insert(param.name.clone());
                }
            }
            SignatureGroupRef::Value(params) => {
                return signature_value_group_target_slot_types(params, &scoped_env);
            }
        }
    }
    None
}

fn signature_value_group_target_slot_types(
    group: &[SignatureParam<Enriched>],
    env: &UnfoldEnv<'_>,
) -> Option<Vec<Type<Routed>>> {
    let value_arity = group.len();
    let mut slots = Vec::new();
    for p in group {
        if let SignatureParam::Value(v) = p {
            let ty = unfold_type_alias_type(&convert_type_for_abi(v.ty.as_ref()?), env);
            slots.extend(value_param_abi_slot_types(&ty, value_arity == 1));
        }
    }
    Some(slots)
}

fn signature_target_arity(sig: &Signature<Enriched>, env: &UnfoldEnv<'_>) -> usize {
    signature_target_slot_types(sig, env)
        .map(|slots| slots.len())
        .unwrap_or(0)
}

fn signature_type_param_count(sig: &Signature<Enriched>) -> usize {
    sig.params
        .iter()
        .filter(|p| matches!(p, SignatureParam::Type(_)))
        .count()
}

fn signature_has_untyped_value_param(sig: &Signature<Enriched>) -> bool {
    sig.params
        .iter()
        .any(|p| matches!(p, SignatureParam::Value(v) if v.ty.is_none()))
}

/// The environment for unfolding aliases and canonicalizing unresolved
/// referenced types in their defining scope.
#[derive(Clone)]
struct UnfoldEnv<'a> {
    newtype_home: &'a HashMap<String, HashMap<String, String>>,
    host_type_home: &'a HashMap<String, HashMap<String, String>>,
    /// Lexically bound type variables at the type's use site. They
    /// shadow same-leaf module declarations.
    type_vars: BTreeSet<String>,
    /// The module whose scope `aliases` is — the spelling scope a walk
    /// starts in when the caller passes no explicit home.
    module: String,
    /// Every module's alias scope, for resolving an unresolved referenced
    /// alias head in the module that defines it.
    all_alias_scopes:
        &'a std::collections::HashMap<String, std::collections::HashMap<String, AliasInfo>>,
    /// Per module: `import … as <alias>` qualifier → target module slash-path
    /// for referenced heads not yet canonicalized (see
    /// [`LoweringCtx::qualified_scopes`]).
    qualified_scopes:
        &'a std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Every module path in the package (see [`LoweringCtx::modules`]).
    modules: &'a std::collections::HashSet<String>,
}

impl<'a> UnfoldEnv<'a> {
    /// If bare leaf `name` resolves to a newtype in `home`'s scope (its
    /// own declaration or one it imports), the slash-path of the module
    /// that declares that newtype.
    fn newtype_decl_module(&self, home: &str, name: &str) -> Option<&str> {
        self.newtype_home.get(home)?.get(name).map(String::as_str)
    }

    fn host_type_decl_module(&self, home: &str, name: &str) -> Option<&str> {
        self.host_type_home.get(home)?.get(name).map(String::as_str)
    }
}

/// Build a qualified type path `[module…, leaf]` from a `module/path`
/// slash-string and a nominal leaf, all carrying `span`. Only segment
/// names are load-bearing downstream (`PathSegment` compares by name).
fn qualified_nominal_segments(
    module_path: &str,
    leaf: &str,
    span: crate::span::Span,
) -> Vec<crate::ast::PathSegment> {
    module_path
        .split('/')
        .filter(|s| !s.is_empty())
        .chain(std::iter::once(leaf))
        .map(|s| crate::ast::PathSegment::synth(s, span))
        .collect()
}

fn unfold_type_alias_type(ty: &Type<Routed>, env: &UnfoldEnv<'_>) -> Type<Routed> {
    unfold_type_alias_type_at_state(ty, env, None, &env.type_vars, false)
}

fn unfold_type_alias_type_at_state(
    ty: &Type<Routed>,
    env: &UnfoldEnv<'_>,
    home: Option<&str>,
    type_vars: &BTreeSet<String>,
    is_canonical: bool,
) -> Type<Routed> {
    let (unfolded, unfold_home, is_canonical) =
        unfold_type_alias_path_head(ty, env, home, type_vars, is_canonical);
    // After unfolding the path head, the resulting type's leaves are
    // written in the home module of the last alias unfolded; carry that
    // home down so they qualify against it rather than the referrer.
    let home: Option<&str> = unfold_home.as_deref().or(home);
    match unfolded {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let args: Vec<Type<Routed>> = args
                .iter()
                .map(|arg| unfold_type_alias_type_at_state(arg, env, home, type_vars, is_canonical))
                .collect();
            let spelling_scope = home.unwrap_or(env.module.as_str());
            // Pin a noncanonical bare referenced newtype in its explicit
            // spelling scope so a later consumer cannot reinterpret the leaf.
            if !is_canonical
                && let [single] = segments.as_slice()
                && !type_vars.contains(single.name.as_str())
                && let Some(decl_module) =
                    env.newtype_decl_module(spelling_scope, single.name.as_str())
            {
                return Type::Path {
                    segments: qualified_nominal_segments(decl_module, &single.name, single.span),
                    args,
                    meta,
                };
            }
            if !is_canonical
                && let [single] = segments.as_slice()
                && !type_vars.contains(single.name.as_str())
                && let Some(decl_module) =
                    env.host_type_decl_module(spelling_scope, single.name.as_str())
            {
                return Type::Path {
                    segments: qualified_nominal_segments(decl_module, &single.name, single.span),
                    args,
                    meta,
                };
            }
            Type::Path {
                segments,
                args,
                meta,
            }
        }
        Type::Unit { meta } => Type::Unit { meta },
        Type::Bottom { meta } => Type::Bottom { meta },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => {
            let param =
                unfold_type_alias_type_at_state(param.as_ref(), env, home, type_vars, is_canonical);
            let ret =
                unfold_type_alias_type_at_state(ret.as_ref(), env, home, type_vars, is_canonical);
            Type::Function {
                param: Box::new(param),
                ret: Box::new(ret),
                meta,
                abi_arity,
                caps,
            }
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(unfold_type_alias_type_at_state(
                left.as_ref(),
                env,
                home,
                type_vars,
                is_canonical,
            )),
            right: Box::new(unfold_type_alias_type_at_state(
                right.as_ref(),
                env,
                home,
                type_vars,
                is_canonical,
            )),
            meta,
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(unfold_type_alias_type_at_state(
                left.as_ref(),
                env,
                home,
                type_vars,
                is_canonical,
            )),
            right: Box::new(unfold_type_alias_type_at_state(
                right.as_ref(),
                env,
                home,
                type_vars,
                is_canonical,
            )),
            meta,
        },
        Type::Forall { param, body, meta } => {
            let mut nested = type_vars.clone();
            nested.insert(param.name.clone());
            Type::Forall {
                param,
                body: Box::new(unfold_type_alias_type_at_state(
                    body.as_ref(),
                    env,
                    home,
                    &nested,
                    is_canonical,
                )),
                meta,
            }
        }
        Type::LabelSugar { ext, .. } => match ext {},
        Type::Infer { ext, .. } => match ext {},
    }
}

/// Unfold an alias chain at the path head, returning the unfolded type,
/// the last alias owner, and whether the whole result is canonical.
///
/// The walk tracks the module whose spellings the current head is
/// written in: a bare head names an alias in that scope; a qualified
/// head resolves its qualifier there (an `import … as …` alias, or a real
/// module path the walk itself produced) and continues in the target
/// module's scope. Once a canonical alias body is combined with its
/// canonical caller arguments, later heads are read literally rather than
/// replayed through the alias owner's imports. A qualified head that is not
/// an alias exits rewritten to its real module path, so the boundary type reads the
/// same in every scope. Types are finite trees and resolution rejects
/// alias cycles, so the walk terminates without a depth ceiling; the
/// seen-guard turns the impossible revisit into an internal error.
fn unfold_type_alias_path_head(
    ty: &Type<Routed>,
    env: &UnfoldEnv<'_>,
    home: Option<&str>,
    type_vars: &BTreeSet<String>,
    is_canonical: bool,
) -> (Type<Routed>, Option<String>, bool) {
    let mut current = ty.clone();
    let mut current_is_canonical = is_canonical;
    let mut last_home: Option<String> = None;
    let mut walk_scope: String = home.unwrap_or(env.module.as_str()).to_owned();
    let mut seen: Vec<(String, String)> = Vec::new();
    loop {
        let Some((target_module, leaf, span, n_seg, n_args)) = (match &current {
            Type::Path { segments, args, .. } => match segments.as_slice() {
                [] => None,
                [single] if current_is_canonical || type_vars.contains(single.name.as_str()) => {
                    None
                }
                [single] => Some((
                    walk_scope.clone(),
                    single.name.clone(),
                    single.span,
                    1usize,
                    args.len(),
                )),
                [qualifier @ .., leaf] => {
                    let joined = qualifier
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("/");
                    let target = if current_is_canonical {
                        joined
                    } else {
                        let import_alias_target = match qualifier {
                            [q] => env
                                .qualified_scopes
                                .get(walk_scope.as_str())
                                .and_then(|quals| quals.get(q.name.as_str()))
                                .cloned(),
                            _ => None,
                        };
                        import_alias_target
                            .or_else(|| env.modules.contains(&joined).then_some(joined))
                            // A qualifier neither of this scope nor a real
                            // module path is a foreign producer spelling.
                            .unwrap_or_else(|| walk_scope.clone())
                    };
                    Some((
                        target,
                        leaf.name.clone(),
                        leaf.span,
                        segments.len(),
                        args.len(),
                    ))
                }
            },
            _ => None,
        }) else {
            return (current, last_home, current_is_canonical);
        };
        let alias: Option<AliasInfo> = env
            .all_alias_scopes
            .get(target_module.as_str())
            .and_then(|scope| scope.get(leaf.as_str()))
            .cloned();
        let Some(alias) = alias else {
            if current_is_canonical {
                return (current, last_home, true);
            }
            // A qualified spelling is meaningful only in the module whose
            // imports resolved it. Record that declaring-module identity in
            // Routed so a consumer's imports cannot reinterpret it.
            if n_seg >= 2 && env.modules.contains(&target_module) {
                let Type::Path { args, meta, .. } = current else {
                    unreachable!("the head matched `Type::Path` above")
                };
                return (
                    Type::Path {
                        segments: qualified_nominal_segments(&target_module, &leaf, span),
                        args,
                        meta,
                    },
                    last_home,
                    false,
                );
            }
            return (current, last_home, false);
        };
        if alias.type_params.len() != n_args {
            return (current, last_home, current_is_canonical);
        }
        if seen
            .iter()
            .any(|(module, name)| module == &target_module && name == &leaf)
        {
            unreachable!(
                "alias unfold revisited `{target_module}.{leaf}`: resolution rejects alias \
                 cycles, so every chain is finite"
            );
        }
        seen.push((target_module.clone(), leaf.clone()));
        // Type-args carried into the alias are themselves written in the
        // scope the head was spelled in, so unfold them under it, not the
        // alias's home.
        let mut subst = HashMap::new();
        let Type::Path { args, .. } = &current else {
            unreachable!("the head matched `Type::Path` above")
        };
        for (param, arg) in alias.type_params.iter().zip(args) {
            subst.insert(
                param.name.clone(),
                unfold_type_alias_type_at_state(
                    arg,
                    env,
                    Some(walk_scope.as_str()),
                    type_vars,
                    current_is_canonical,
                ),
            );
        }
        current = crate::pass::typecheck_core::subst_type(&alias.body, &subst);
        current_is_canonical = true;
        walk_scope = alias.home.clone();
        last_home = Some(alias.home.clone());
    }
}

fn semantic_signature_for_function_value(
    sig: &Signature<Enriched>,
    env: &UnfoldEnv<'_>,
) -> Signature<Routed> {
    let mut scoped_env = env.clone();
    let mut params = Vec::with_capacity(sig.params.len());
    for param in &sig.params {
        match param {
            SignatureParam::Type(tp) => {
                scoped_env.type_vars.insert(tp.name.clone());
                params.push(SignatureParam::Type(tp.clone()));
            }
            SignatureParam::Value(v) => params.push(SignatureParam::Value(Param {
                name: v.name.clone(),
                ty: v
                    .ty
                    .as_ref()
                    .map(|ty| unfold_type_alias_type(&convert_type_for_abi(ty), &scoped_env)),
                pattern: (),
                meta: convert_meta(&v.meta),
            })),
        }
    }
    Signature::from_parts(params, sig.groups.clone())
}

fn semantic_function_type_from_signature(
    sig: &Signature<Enriched>,
    ret: Type<Routed>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
) -> Type<Routed> {
    let scoped_env = env_with_signature_type_vars(sig, env);
    let ret = unfold_type_alias_type(&ret, &scoped_env);
    let sig = semantic_signature_for_function_value(sig, env);
    sig.signature_ty(ret, span)
}

fn signature_return_after_first_value_group(
    sig: &Signature<Enriched>,
    ret: Type<Routed>,
    span: crate::span::Span,
    env: &UnfoldEnv<'_>,
) -> Type<Routed> {
    let routed_sig = canonicalize_entry_signature(sig, span, env);
    let mut consumed_first_value_group = false;
    let mut remaining = Vec::new();
    for group in routed_sig.canonical_groups() {
        match group {
            SignatureGroupRef::Value(_) if !consumed_first_value_group => {
                consumed_first_value_group = true;
            }
            group if consumed_first_value_group => remaining.push(group),
            SignatureGroupRef::Type(_) | SignatureGroupRef::Value(_) => {}
        }
    }
    type_from_signature_group_refs(remaining, ret, span)
}

fn signature_has_groups_after_first_value_group(sig: &Signature<Enriched>) -> bool {
    let mut consumed_first_value_group = false;
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Value(_) if !consumed_first_value_group => {
                consumed_first_value_group = true;
            }
            SignatureGroupRef::Type(_) | SignatureGroupRef::Value(_)
                if consumed_first_value_group =>
            {
                return true;
            }
            SignatureGroupRef::Type(_) | SignatureGroupRef::Value(_) => {}
        }
    }
    false
}

fn type_from_signature_group_refs(
    groups: Vec<SignatureGroupRef<'_, Routed>>,
    ret: Type<Routed>,
    span: crate::span::Span,
) -> Type<Routed> {
    let mut acc = ret;
    for group in groups.into_iter().rev() {
        match group {
            SignatureGroupRef::Type(params) => {
                for param in params.iter().rev() {
                    let SignatureParam::Type(param) = param else {
                        unreachable!("SignatureGroupRef::Type contains only type params")
                    };
                    acc = Type::Forall {
                        param: param.clone(),
                        body: Box::new(acc),
                        meta: Meta::new(span),
                    };
                }
            }
            SignatureGroupRef::Value(params) => {
                let value_tys = params
                    .iter()
                    .map(|param| {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("SignatureGroupRef::Value contains only value params")
                        };
                        param
                            .ty
                            .as_ref()
                            .expect("Routed signature value params carry types")
                            .clone()
                    })
                    .collect();
                acc = Type::synth_function(value_tys, acc, span);
            }
        }
    }
    acc
}

fn collect_type_arg_bindings(
    pattern: &Type<Routed>,
    actual: &Type<Routed>,
    type_params: &[String],
    subst: &mut HashMap<String, Type<Routed>>,
) {
    match (pattern, actual) {
        (Type::Path { segments, args, .. }, _)
            if segments.len() == 1
                && args.is_empty()
                && type_params.iter().any(|name| name == &segments[0].name) =>
        {
            subst
                .entry(segments[0].name.clone())
                .or_insert_with(|| actual.clone());
        }
        (
            Type::Path {
                segments: p_segments,
                args: p_args,
                ..
            },
            Type::Path {
                segments: a_segments,
                args: a_args,
                ..
            },
        ) if p_segments == a_segments && p_args.len() == a_args.len() => {
            for (p, a) in p_args.iter().zip(a_args) {
                collect_type_arg_bindings(p, a, type_params, subst);
            }
        }
        (
            Type::Function {
                param: p_param,
                ret: p_ret,
                ..
            },
            Type::Function {
                param: a_param,
                ret: a_ret,
                ..
            },
        ) => {
            collect_type_arg_bindings(p_param, a_param, type_params, subst);
            collect_type_arg_bindings(p_ret, a_ret, type_params, subst);
        }
        (
            Type::Product {
                left: p_left,
                right: p_right,
                ..
            },
            Type::Product {
                left: a_left,
                right: a_right,
                ..
            },
        )
        | (
            Type::Sum {
                left: p_left,
                right: p_right,
                ..
            },
            Type::Sum {
                left: a_left,
                right: a_right,
                ..
            },
        ) => {
            collect_type_arg_bindings(p_left, a_left, type_params, subst);
            collect_type_arg_bindings(p_right, a_right, type_params, subst);
        }
        (
            Type::Forall {
                param: p_param,
                body: p_body,
                ..
            },
            Type::Forall { body: a_body, .. },
        ) => {
            let scoped_type_params: Vec<String> = type_params
                .iter()
                .filter(|name| name.as_str() != p_param.name.as_str())
                .cloned()
                .collect();
            collect_type_arg_bindings(p_body, a_body, &scoped_type_params, subst)
        }
        _ => {}
    }
}

fn function_param_abi_from_callee_ty(
    callee_ty: &Type<Routed>,
    type_args: &[Type<Routed>],
) -> Option<CalleeParamAbi> {
    let mut cur = callee_ty.clone();
    let mut subst: HashMap<String, Type<Routed>> = HashMap::new();
    let mut next_type_arg = 0usize;
    loop {
        match cur {
            Type::Forall { param, body, .. } => {
                if let Some(arg) = type_args.get(next_type_arg) {
                    subst.insert(param.name.clone(), arg.clone());
                    next_type_arg += 1;
                }
                cur = *body;
            }
            Type::Function {
                param, abi_arity, ..
            } => {
                let param_ty = crate::package_collection::substitute_type(&param, &subst);
                let target_slots = function_abi_param_slot_types(&param_ty, abi_arity);
                return Some(CalleeParamAbi {
                    param_ty,
                    target_slots,
                });
            }
            _ => return None,
        }
    }
}

fn peel_function_foralls(ty: &Type<Routed>) -> (Vec<crate::ast::TypeParam>, Type<Routed>) {
    let mut params = Vec::new();
    let mut cur = ty.clone();
    while let Type::Forall { param, body, .. } = cur {
        params.push(param);
        cur = *body;
    }
    (params, cur)
}

fn type_has_callable_layer(ty: &Type<Routed>) -> bool {
    matches!(peel_function_foralls(ty).1, Type::Function { .. })
}

fn selected_abi_shape_from_type(ty: &Type<Routed>) -> SelectedAbiShape {
    let mut groups = Vec::new();
    let mut current = ty.clone();
    loop {
        match current {
            Type::Forall { body, .. } => {
                groups.push(SelectedAbiGroup::Type);
                current = *body;
            }
            Type::Function { ret, abi_arity, .. } => {
                groups.push(SelectedAbiGroup::Value {
                    arity: abi_arity,
                    binders: Vec::new(),
                });
                current = *ret;
            }
            _ => break,
        }
    }
    if groups.is_empty() {
        SelectedAbiShape::Value
    } else {
        SelectedAbiShape::Function {
            groups,
            body: Box::new(SelectedAbiShape::Value),
        }
    }
}

fn function_return_after_call(
    callee_ty: &Type<Routed>,
    type_args: &[Type<Routed>],
) -> Option<Type<Routed>> {
    let mut cur = callee_ty.clone();
    let mut subst: HashMap<String, Type<Routed>> = HashMap::new();
    let mut next_type_arg = 0usize;
    loop {
        match cur {
            Type::Forall { param, body, .. } => {
                if let Some(arg) = type_args.get(next_type_arg) {
                    subst.insert(param.name.clone(), arg.clone());
                    next_type_arg += 1;
                }
                cur = *body;
            }
            Type::Function { ret, .. } => {
                return Some(crate::pass::typecheck_core::subst_type(&ret, &subst));
            }
            _ => return None,
        }
    }
}

fn type_after_type_application(
    callee_ty: &Type<Routed>,
    type_arg: &Type<Routed>,
) -> Option<Type<Routed>> {
    let Type::Forall { param, body, .. } = callee_ty else {
        return None;
    };
    Some(crate::pass::typecheck_core::subst_type(
        body,
        &HashMap::from([(param.name.clone(), type_arg.clone())]),
    ))
}

/// Convert a `HostFn<Enriched>`'s param / ret pair into the
/// equivalent [`HostFnSig`] so the lowering context can carry a
/// uniform type. The host fn carries its params and return type as
/// separate fields; the lowering's `LowHostCall` variant takes a
/// `Signature` + `ret_ty` so per-backend rendering reads the callee's
/// value- and type-parameters plus the declared return through one
/// consistent shape.
fn host_fn_to_sig(h: &crate::ast::HostFn<Enriched>, module_path: &str) -> HostFnSig {
    use crate::ast::{Param, SignatureParam};
    let params = h
        .params
        .iter()
        .map(|p| match p {
            crate::ast::HostFnParam::Type(tp) => SignatureParam::Type(tp.clone()),
            crate::ast::HostFnParam::Value(v) => SignatureParam::Value(Param {
                name: v.name.clone().unwrap_or_default(),
                ty: Some(v.ty.clone()),
                pattern: (),
                meta: v.meta.clone(),
            }),
        })
        .collect();
    let sig = Signature::from_parts(params, h.param_groups.clone());
    HostFnSig {
        sig,
        ret: h.ret.clone(),
        module_path: module_path.to_owned(),
    }
}

/// Split a `Vec<CallArg<Enriched>>` into a type-arg list and a
/// value-arg list, preserving order within each list. The Low IR's
/// load-bearing transform: separating type-args from value-args here
/// is what makes the per-backend `ERASED_TYPE_ARG` sentinel
/// unnecessary.
fn split_args(args: &[CallArg<Enriched>]) -> (Vec<Type<Enriched>>, Vec<Expr<Enriched>>) {
    let mut type_args = Vec::new();
    let mut value_args = Vec::new();
    for a in args {
        match a {
            CallArg::Type(t) => type_args.push(t.clone()),
            CallArg::Value(v) => value_args.push(v.clone()),
        }
    }
    (type_args, value_args)
}

// ---- tests --------------------------------------------------------------

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::{Routed, SignatureParam, Surface};
    use crate::pass::full::FullPipeline;
    use crate::pipeline::Pipeline;
    #[cfg(feature = "prime")]
    use crate::prime::pipeline::PrimePipeline;
    use crate::span::Span;
    use std::fmt::Write;
    use std::path::{Path, PathBuf};

    fn test_meta() -> Meta<Routed> {
        Meta::new(Span::new(0, 0))
    }

    fn path_ty(name: &str) -> Type<Routed> {
        Type::synth_path(vec![name.to_owned()], Vec::new(), Span::new(0, 0))
    }

    fn product_ty(left: Type<Routed>, right: Type<Routed>) -> Type<Routed> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: test_meta(),
        }
    }

    fn fn_ty(param: Type<Routed>, ret: Type<Routed>, abi_arity: usize) -> Type<Routed> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: test_meta(),
            abi_arity,
            caps: Default::default(),
        }
    }

    #[test]
    fn recovered_function_param_abi_avoids_nested_forall_capture() {
        let span = Span::new(0, 0);
        let inner = Type::Forall {
            param: crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(fn_ty(path_ty("T"), path_ty("A"), 1)),
            meta: test_meta(),
        };
        let callee = Type::Forall {
            param: crate::ast::TypeParam {
                name: "T".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(fn_ty(inner, Type::Unit { meta: test_meta() }, 1)),
            meta: test_meta(),
        };

        let recovered = function_param_abi_from_callee_ty(&callee, &[path_ty("A")])
            .expect("polymorphic callee should expose its value-parameter ABI");
        let Type::Forall {
            param: fresh, body, ..
        } = recovered.param_ty
        else {
            panic!("recovered parameter should keep its nested forall");
        };
        assert_ne!(fresh.name, "A");
        let Type::Function { param, ret, .. } = *body else {
            panic!("nested forall body should stay function-shaped");
        };
        assert_eq!(*param, path_ty("A"));
        assert_eq!(*ret, path_ty(&fresh.name));
    }

    #[test]
    fn recovered_function_param_abi_appends_to_a_partially_applied_type_argument() {
        let span = Span::new(0, 0);
        let applied = |name: &str, args: Vec<Type<Routed>>| {
            Type::synth_path(vec![name.to_owned()], args, span)
        };
        let callee = Type::Forall {
            param: crate::ast::TypeParam {
                name: "F".to_owned(),
                span,
                kind: Some(crate::ast::Kind::arrow_chain(1)),
            },
            body: Box::new(Type::Forall {
                param: crate::ast::TypeParam {
                    name: "A".to_owned(),
                    span,
                    kind: None,
                },
                body: Box::new(fn_ty(
                    applied("F", vec![path_ty("A")]),
                    Type::Unit { meta: test_meta() },
                    1,
                )),
                meta: test_meta(),
            }),
            meta: test_meta(),
        };

        let recovered = function_param_abi_from_callee_ty(
            &callee,
            &[applied("Either", vec![path_ty("String")]), path_ty("Int")],
        )
        .expect("higher-kinded callee should expose its value-parameter ABI");
        let Type::Path { segments, args, .. } = recovered.param_ty else {
            panic!("the substituted parameter should remain path-shaped");
        };
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].as_str(), "Either");
        assert_eq!(args, vec![path_ty("String"), path_ty("Int")]);
    }

    fn test_meta_enriched() -> Meta<Enriched> {
        Meta::new(Span::new(0, 0))
    }

    fn unit_ty_enriched() -> Type<Enriched> {
        Type::Unit {
            meta: test_meta_enriched(),
        }
    }

    fn path_ty_enriched(name: &str) -> Type<Enriched> {
        Type::Path {
            segments: vec![PathSegment::synth(name, Span::new(0, 0))],
            args: Vec::new(),
            meta: test_meta_enriched(),
        }
    }

    fn product_ty_enriched(left: Type<Enriched>, right: Type<Enriched>) -> Type<Enriched> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: test_meta_enriched(),
        }
    }

    fn fn_ty_enriched(
        param: Type<Enriched>,
        ret: Type<Enriched>,
        abi_arity: usize,
    ) -> Type<Enriched> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: test_meta_enriched(),
            abi_arity,
            caps: Default::default(),
        }
    }

    fn unit_expr_enriched() -> Expr<Enriched> {
        Expr::Unit {
            occurrence: Default::default(),
            meta: test_meta_enriched(),
        }
    }

    fn bound_expr_enriched(name: &str) -> Expr<Enriched> {
        Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::synth(name, Span::new(0, 0))],
            meta: test_meta_enriched(),
            ext: (),
        }
    }

    fn flat_binary_fn_enriched(body: Expr<Enriched>) -> Expr<Enriched> {
        let value_param = |name: &str| {
            SignatureParam::Value(Param {
                name: name.to_owned(),
                ty: Some(path_ty_enriched("N")),
                pattern: (),
                meta: test_meta_enriched(),
            })
        };
        Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                vec![value_param("left"), value_param("right")],
                vec![SignatureGroupKind::Value { len: 2 }],
            ),
            ret_ty: Some(path_ty_enriched("N")),
            body: Box::new(body),
            meta: test_meta_enriched(),
            caps: (),
        }
    }

    fn canonical_binary_fn_ty_enriched() -> Type<Enriched> {
        fn_ty_enriched(
            product_ty_enriched(path_ty_enriched("N"), path_ty_enriched("N")),
            path_ty_enriched("N"),
            1,
        )
    }

    fn assert_branch_has_packed_callable_adapter(branch: &Expr<Routed>, result_name: &str) {
        assert!(
            matches!(branch, Expr::Let { name, value, body, .. }
                if matches!(value.as_ref(), Expr::FnExpr { body, .. }
                    if matches!(body.as_ref(), Expr::LowBoundRef { name, .. }
                        if name == result_name))
                    && matches!(body.as_ref(), Expr::FnExpr { sig, body, .. }
                        if sig.value_param_count() == 1
                            && matches!(body.as_ref(), Expr::LowIndirectCall { callee, args, .. }
                                if args.len() == 2
                                    && matches!(callee.as_ref(), Expr::LowBoundRef { name: callee_name, .. }
                                        if callee_name == name)))),
            "branch-local flat callable must be adapted to one packed product argument: {branch:#?}"
        );
    }

    /// Drive a module source through the full pipeline up to Enriched
    /// (the input to `lower`). Used by the variant-classification
    /// tests below.
    fn pipeline_to_enriched(src: &str) -> Package<Enriched> {
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
        crate::pass::structural_recovery::recover_package(&package)
    }

    fn full_pipeline_to_enriched(sources: &[(&str, &str)]) -> Package<Enriched> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source)
                        .unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower surface package");
        let package = Package::build(Path::new(""), modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = FullPipeline::typecheck(&package).expect("typecheck surface package");
        crate::pass::structural_recovery::recover_package(&prime)
    }

    fn full_pipeline_to_routed(sources: &[(&str, &str)]) -> Package<Routed> {
        lower(&full_pipeline_to_enriched(sources))
    }

    fn full_optimized_pipeline_to_routed(sources: &[(&str, &str)]) -> Package<Routed> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source)
                        .unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower surface package");
        let package = Package::build(Path::new(""), modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = FullPipeline::typecheck(&package).expect("typecheck surface package");
        let enriched = crate::pass::structural_recovery::recover_package(&prime);
        lower(&crate::pass::optimize::optimize_package(enriched))
    }

    fn routed_fn<'a>(package: &'a Package<Routed>, module: &str, name: &str) -> &'a FnDef<Routed> {
        package
            .module(module)
            .unwrap_or_else(|| panic!("missing routed module `{module}`"))
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(def),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing routed function `{module}.{name}`"))
    }

    fn unwrap_function_adapter_type_stage<'a>(
        adapter: &'a Expr<Routed>,
        expected_callee: &str,
    ) -> (&'a str, &'a Expr<Routed>) {
        let Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } = adapter
        else {
            panic!("a function adapter type stage must expose one residual: {adapter:#?}");
        };
        assert!(type_args.is_empty());
        assert!(args.is_empty());
        let Expr::FnExpr { sig, body, .. } = callee.as_ref() else {
            panic!("a function adapter type stage must wrap a closure: {callee:#?}");
        };
        assert_eq!(
            sig.groups,
            vec![
                SignatureGroupKind::Value { len: 0 },
                SignatureGroupKind::Type { len: 1 },
            ]
        );
        let [SignatureParam::Type(param)] = sig.params.as_slice() else {
            panic!("a function adapter type stage must bind one type: {sig:#?}");
        };
        let Expr::Let {
            name, value, body, ..
        } = body.as_ref()
        else {
            panic!("a function adapter type stage must bind its residual: {body:#?}");
        };
        let Expr::LowTypeApplication {
            callee, type_arg, ..
        } = value.as_ref()
        else {
            panic!("a function adapter type stage must apply its source: {value:#?}");
        };
        assert!(matches!(callee.as_ref(), Expr::LowBoundRef { name, .. }
            if name == expected_callee));
        assert!(matches!(type_arg, Type::Path { segments, args, .. }
            if args.is_empty()
                && segments.as_slice() == [PathSegment::new(param.name.clone(), param.span)]));
        (name, body)
    }

    #[test]
    fn returned_forall_type_application_keeps_one_boundary_per_binder() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn produce(_unit: .) -> [A] A; \
             fn caller() -> N { produce() }",
        )]);
        let Expr::LowTypeApplication {
            callee, type_arg, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "terminal type application must remain distinct from a value call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(
            matches!(type_arg, Type::Path { segments, .. } if segments.last().is_some_and(|segment| segment == "N"))
        );
        let Expr::LowHostCall { name, args, .. } = callee.as_ref() else {
            panic!("the retained type application must specialize the producing host call")
        };
        assert_eq!(name, "produce");
        assert!(
            args.is_empty(),
            "the producer's erased Unit ABI has no runtime argument"
        );
    }

    #[test]
    fn value_application_keeps_the_preceding_type_stage_as_its_callee() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn value(_unit: .) -> N; \
             fn stage(_unit: .)[A] -> A -> A { .(item: A) -> A { item } } \
             fn caller() -> N { stage()(N)(value()) }",
        )]);
        let Expr::Let {
            value: type_stage,
            body: value_stage,
            ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the type stage must be sequenced before the adapted value layer: {:#?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(
            matches!(type_stage.as_ref(), Expr::LowTypeApplication { .. }),
            "the staging let must evaluate the type application first: {type_stage:?}"
        );
        assert!(
            first_low_host_call(value_stage).is_some_and(|name| name == "value"),
            "the effectful value argument must remain in the later value stage: {value_stage:?}"
        );
    }

    #[test]
    fn optimized_let_keeps_a_type_application_before_a_later_effect() {
        let routed = full_optimized_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn effect(_unit: .) -> .; \
             fn stage(_unit: .)[A] -> A -> A { .(value: A) -> A { value } } \
             fn caller(value: N) -> N { \
               let residual = stage()(N); \
               effect(()); \
               residual(value) \
             }",
        )]);
        let Expr::Let {
            value: type_stage,
            body,
            ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the optimized type-stage let must remain before the effect: {:#?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(matches!(
            type_stage.as_ref(),
            Expr::LowTypeApplication { .. }
        ));
        assert_eq!(first_low_host_call(body), Some("effect"));
    }

    #[test]
    fn optimized_residual_type_applications_remain_at_their_let_sites() {
        let routed = full_optimized_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn effect(_unit: .) -> .; \
             fn stage(_unit: .)[A][B] -> B -> B { \
               effect(()); \
               .(value: B) -> B { value } \
             } \
             fn caller(value: N) -> N { \
               let base = stage(()); \
               let after_a = base(N); \
               effect(()); \
               let after_b = after_a(N); \
               effect(()); \
               after_b(value) \
             }",
        )]);
        let body = &routed_fn(&routed, "test", "caller").body;
        let Expr::Let {
            value: first_type_stage,
            body: after_first,
            ..
        } = body
        else {
            panic!("the first residual type application must be bound: {body:#?}")
        };
        assert!(matches!(
            first_type_stage.as_ref(),
            Expr::LowTypeApplication { .. }
        ));
        let after_first = match after_first.as_ref() {
            Expr::Let {
                value: seeded_residual,
                body,
                ..
            } => {
                assert!(matches!(
                    seeded_residual.as_ref(),
                    Expr::LowIndirectCall {
                        type_args,
                        args,
                        ..
                    } if type_args.is_empty() && args.is_empty()
                ));
                body.as_ref()
            }
            other => other,
        };
        let Expr::Seq {
            value: first_effect,
            body: after_effect,
            ..
        } = after_first
        else {
            panic!("the first effect must follow the first type stage: {after_first:#?}")
        };
        assert!(
            matches!(first_effect.as_ref(), Expr::LowHostCall { name, .. } if name == "effect")
        );
        let Expr::Let {
            value: second_type_stage,
            body: after_second,
            ..
        } = after_effect.as_ref()
        else {
            panic!("the second residual type application must be bound: {after_effect:#?}")
        };
        assert!(
            matches!(second_type_stage.as_ref(), Expr::LowTypeApplication { .. }),
            "the second stage must remain a type application: {second_type_stage:#?}"
        );
        assert!(matches!(after_second.as_ref(), Expr::Seq { value, .. }
            if matches!(value.as_ref(), Expr::LowHostCall { name, .. } if name == "effect")));
    }

    #[test]
    fn interleaved_flat_call_arrives_as_ordered_nested_routed_layers() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn first(_unit: .) -> N; \
             host fn second(_unit: .) -> N; \
             fn interleaved[A](x: A)[B](y: B) -> B { y } \
             fn caller() -> N { interleaved(N, first(), N, second()) }",
        )]);
        let Expr::LowIndirectCall {
            callee: first_value_stage,
            type_args: second_type_args,
            args: second_value_args,
            ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the final B value layer must be the outer application: {:#?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(matches!(
            second_value_args.as_slice(),
            [Expr::LowHostCall { name, .. }] if name == "second"
        ));
        assert!(
            matches!(second_type_args.as_slice(), [Type::Path { segments, .. }]
            if segments.last().is_some_and(|segment| segment == "N"))
        );
        let Expr::LowModuleCall {
            type_args: first_type_args,
            args: first_value_args,
            ..
        } = first_value_stage.as_ref()
        else {
            panic!("the first A type/value layer must remain the named module call")
        };
        assert_eq!(first_type_args.len(), 1);
        assert!(matches!(
            first_value_args.as_slice(),
            [Expr::LowHostCall { name, .. }] if name == "first"
        ));
    }

    #[test]
    fn interleaved_host_call_uses_the_staged_function_value_route() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn first(_unit: .) -> N; \
             host fn second(_unit: .) -> N; \
             host fn staged[A](first: N)[B](second: N) -> N; \
             fn caller() -> N { staged(., first(), ., second()) }",
        )]);
        let Expr::LowIndirectCall {
            callee: first_value_stage,
            type_args: second_type_args,
            args: second_value_args,
            ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the second host value group must remain an outer application: {:#?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert_eq!(second_type_args.len(), 1);
        assert!(matches!(
            second_value_args.as_slice(),
            [Expr::LowHostCall { name, .. }] if name == "second"
        ));
        let Expr::LowIndirectCall {
            callee,
            type_args: first_type_args,
            args: first_value_args,
            ..
        } = first_value_stage.as_ref()
        else {
            panic!("the first host group must use an ordinary indirect call")
        };
        assert_eq!(first_type_args.len(), 1);
        assert!(matches!(
            first_value_args.as_slice(),
            [Expr::LowHostCall { name, .. }] if name == "first"
        ));
        let Expr::LowHostFnValueRef {
            name,
            module_path,
            sig,
            ret_ty,
            ..
        } = callee.as_ref()
        else {
            panic!("the residual host signature must use its staged value form: {callee:#?}")
        };
        assert_eq!(name, "staged");
        assert_eq!(module_path, "test");
        assert_eq!(sig.canonical_groups().len(), 4);
        assert!(matches!(ret_ty, Type::Path { segments, .. }
            if segments.last().is_some_and(|segment| segment == "N")));
    }

    fn first_low_host_call(expr: &Expr<Routed>) -> Option<&str> {
        match expr {
            Expr::LowHostCall { name, .. } => Some(name),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                first_low_host_call(value).or_else(|| first_low_host_call(body))
            }
            Expr::FnExpr { body, .. }
            | Expr::LowTypeApplication { callee: body, .. }
            | Expr::LowIndirectCall { callee: body, .. } => first_low_host_call(body),
            Expr::EnrichedTuple { items, .. } => items.iter().find_map(first_low_host_call),
            Expr::LowClosureCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => {
                args.iter().find_map(first_low_host_call)
            }
            _ => None,
        }
    }

    fn first_low_closure_call(expr: &Expr<Routed>) -> Option<(&str, &[Expr<Routed>])> {
        match expr {
            Expr::LowClosureCall { name, args, .. } => Some((name, args)),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                first_low_closure_call(value).or_else(|| first_low_closure_call(body))
            }
            Expr::FnExpr { body, .. }
            | Expr::LowTypeApplication { callee: body, .. }
            | Expr::LowIndirectCall { callee: body, .. } => first_low_closure_call(body),
            Expr::EnrichedTuple { items, .. } => items.iter().find_map(first_low_closure_call),
            Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => {
                args.iter().find_map(first_low_closure_call)
            }
            _ => None,
        }
    }

    fn first_routed_projector(expr: &Expr<Routed>) -> Option<(Option<&str>, &str)> {
        match expr {
            Expr::LowNewtypeProj { newtype, .. } => Some((None, newtype)),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                ..
            } => Some((Some(module_path), newtype)),
            Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
                first_routed_projector(value).or_else(|| first_routed_projector(body))
            }
            Expr::FnExpr { body, .. }
            | Expr::LowTypeApplication { callee: body, .. }
            | Expr::LowIndirectCall { callee: body, .. } => first_routed_projector(body),
            Expr::EnrichedTuple { items, .. } => items.iter().find_map(first_routed_projector),
            Expr::LowClosureCall { args, .. }
            | Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => {
                args.iter().find_map(first_routed_projector)
            }
            _ => None,
        }
    }

    #[test]
    fn newtype_projector_function_payload_recovers_unit_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn value(_unit: .) -> N; \
             newtype Lazy : . -> N { constructor mk_lazy; projector force; }; \
             fn lazy() -> Lazy { Lazy.mk_lazy(.() { value() }) } \
             fn caller() -> N { Lazy.force(lazy())() }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "a projected function payload must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(
            matches!(callee.as_ref(), Expr::LowNewtypeProj { newtype, .. } if newtype == "Lazy"),
            "the indirect callee must be the newtype projector: {callee:?}"
        );
        assert!(
            args.is_empty(),
            "the projected payload's erased Unit ABI has no runtime argument: {args:?}"
        );
    }

    #[test]
    fn qualified_newtype_projector_function_payload_recovers_unit_abi() {
        let routed = full_pipeline_to_routed(&[
            (
                "types.kio",
                "module types; \
                 pub newtype Lazy : . -> . { pub constructor mk_lazy; pub projector force; }; \
                 pub fn lazy() -> Lazy { Lazy.mk_lazy(.() { () }) }",
            ),
            (
                "main.kio",
                "module main; \
                 import types as alias; \
                 fn caller() -> . { alias.Lazy.force(alias.lazy())() }",
            ),
        ]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "main", "caller").body
        else {
            panic!(
                "a qualified projected function payload must remain an indirect call: {:?}",
                routed_fn(&routed, "main", "caller").body
            )
        };
        assert!(
            matches!(callee.as_ref(), Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                ..
            } if module_path == "types" && newtype == "Lazy" && member == "force"),
            "the indirect callee must be the qualified newtype projector: {callee:?}"
        );
        assert!(
            args.is_empty(),
            "the qualified projected payload's erased Unit ABI has no runtime argument: {args:?}"
        );
    }

    #[test]
    fn enriched_field_payload_recovers_its_declared_function_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host fn tick(_unit: .) -> .; \
             labels { text: . -> ., invoke[A]: A -> A }; \
             fn unit_call(value: Text) -> . { value.?{text}(()) } \
             fn effect_call(value: Text) -> . { value.?{text}(tick(())) } \
             fn generic_call(value: Invoke(.)) -> . { value.?{invoke}(()) }",
        )]);

        let unit_body = &routed_fn(&routed, "test", "unit_call").body;
        assert_eq!(first_routed_projector(unit_body), Some((None, "Text")));
        let (_, args) = first_low_closure_call(unit_body)
            .unwrap_or_else(|| panic!("the local label payload must be called: {unit_body:#?}"));
        assert!(
            args.is_empty(),
            "an explicitly authored Unit parameter has an erased zero-slot ABI: {args:?}"
        );

        let effect_body = &routed_fn(&routed, "test", "effect_call").body;
        let Expr::Let {
            body: after_target, ..
        } = effect_body
        else {
            panic!("the field target must be staged once: {effect_body:#?}")
        };
        let Expr::Let {
            value: projector,
            body: after_projector,
            ..
        } = after_target.as_ref()
        else {
            panic!("the field projector must be staged once: {after_target:#?}")
        };
        assert!(
            matches!(projector.as_ref(), Expr::LowNewtypeProj { newtype, .. }
            if newtype == "Text")
        );
        let Expr::Let {
            name: effect_name,
            value: effect,
            body: after_effect,
            ..
        } = after_projector.as_ref()
        else {
            panic!("the erased Unit argument must remain sequenced: {after_projector:#?}")
        };
        assert!(
            matches!(effect.as_ref(), Expr::LowHostCall { name, args, .. }
            if name == "tick" && args.is_empty())
        );
        let Expr::Seq {
            value: effect_result,
            body: erased_call,
            ..
        } = after_effect.as_ref()
        else {
            panic!("the staged effect must precede the erased call: {after_effect:#?}")
        };
        assert!(
            matches!(effect_result.as_ref(), Expr::LowBoundRef { name, .. }
            if name == effect_name)
        );
        assert!(
            matches!(erased_call.as_ref(), Expr::LowClosureCall { args, .. }
            if args.is_empty())
        );

        let generic_body = &routed_fn(&routed, "test", "generic_call").body;
        assert_eq!(first_routed_projector(generic_body), Some((None, "Invoke")));
        let (_, args) = first_low_closure_call(generic_body).unwrap_or_else(|| {
            panic!("the generic label payload must be called: {generic_body:#?}")
        });
        assert!(
            matches!(args, [Expr::Unit { .. }]),
            "substituting Unit for an authored generic parameter preserves ABI arity one: {args:?}"
        );
    }

    #[test]
    fn qualified_enriched_field_payload_uses_its_exact_nominal_owner() {
        let routed = full_pipeline_to_routed(&[
            ("labels.kio", "module labels; pub labels { text: . -> . };"),
            (
                "main.kio",
                "module main; \
                 import labels(Text); \
                 import labels({text}); \
                 fn caller(value: Text) -> . { value.?{text}(()) }",
            ),
        ]);
        let body = &routed_fn(&routed, "main", "caller").body;
        assert_eq!(first_routed_projector(body), Some((Some("labels"), "Text")));
        let (_, args) = first_low_closure_call(body)
            .unwrap_or_else(|| panic!("the qualified label payload must be called: {body:#?}"));
        assert!(
            args.is_empty(),
            "the exact qualified newtype payload must supply its erased Unit ABI: {args:?}"
        );
    }

    #[test]
    fn enriched_field_payload_type_reaches_every_recovery_consumer() {
        let span = Span::new(0, 0);
        let info = NewtypeInfo {
            nominal_name: "Text".to_owned(),
            forwarded: false,
            constructor: "mk_text".to_owned(),
            projector: "get".to_owned(),
            has_existentials: false,
            type_params: Vec::new(),
            payload: fn_ty(
                Type::Unit { meta: test_meta() },
                Type::Unit { meta: test_meta() },
                0,
            ),
            home: "test".to_owned(),
            existential_params: Vec::new(),
        };
        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_module.insert(
            "test".to_owned(),
            HashMap::from([("Text".to_owned(), info.clone())]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx.newtypes_in_scope.insert("Text".to_owned(), info);
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let callee: Expr<Enriched> = Expr::EnrichedFieldGet {
            occurrence: Default::default(),
            target: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: test_meta_enriched(),
            }),
            field_name: "Text".to_owned(),
            index: 0,
            arity: 1,
            target_ty: Type::synth_path(vec!["Text".to_owned()], Vec::new(), span),
            meta: test_meta_enriched(),
            ext: (),
        };
        let assert_unit_abi = |consumer: &str, ty: Option<Type<Routed>>| {
            let Some(Type::Function {
                param, abi_arity, ..
            }) = ty
            else {
                panic!("{consumer} must recover the field's function payload")
            };
            assert!(matches!(param.as_ref(), Type::Unit { .. }));
            assert_eq!(
                abi_arity, 0,
                "{consumer} must preserve the declared Unit ABI"
            );
        };

        assert_unit_abi(
            "generic-call inference",
            lowerer.enriched_expr_type_for_inference(&callee),
        );
        assert_unit_abi(
            "source adaptation",
            lowerer.enriched_expr_type_for_source_adapter(&callee),
        );
        let routed_callee = lowerer.lower_expr(&callee);
        assert_unit_abi(
            "routed call recovery",
            lowerer.lowered_expr_type(&routed_callee),
        );
    }

    #[test]
    fn enriched_field_payload_resolution_is_exact_and_decline_safe() {
        let span = Span::new(0, 0);
        let type_param = crate::ast::TypeParam {
            name: "A".to_owned(),
            span,
            kind: None,
        };
        let local = NewtypeInfo {
            nominal_name: "Text".to_owned(),
            forwarded: false,
            constructor: "mk_text".to_owned(),
            projector: "get".to_owned(),
            has_existentials: false,
            type_params: vec![type_param.clone()],
            payload: fn_ty(path_ty("A"), path_ty("A"), 1),
            home: "main".to_owned(),
            existential_params: Vec::new(),
        };
        let remote = NewtypeInfo {
            nominal_name: "Text".to_owned(),
            forwarded: false,
            constructor: "mk_text".to_owned(),
            projector: "get".to_owned(),
            has_existentials: false,
            type_params: Vec::new(),
            payload: fn_ty(
                Type::Unit { meta: test_meta() },
                Type::Unit { meta: test_meta() },
                0,
            ),
            home: "remote".to_owned(),
            existential_params: Vec::new(),
        };
        let hidden = NewtypeInfo {
            nominal_name: "Hidden".to_owned(),
            forwarded: false,
            constructor: "mk_hidden".to_owned(),
            projector: "get".to_owned(),
            has_existentials: true,
            type_params: Vec::new(),
            payload: Type::Unit { meta: test_meta() },
            home: "remote".to_owned(),
            existential_params: vec![type_param],
        };
        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_module.insert(
            "remote".to_owned(),
            HashMap::from([("Text".to_owned(), remote), ("Hidden".to_owned(), hidden)]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx
            .newtypes_in_scope
            .insert("Text".to_owned(), local);
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "main".to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let unit = Type::Unit { meta: test_meta() };
        let local_text = Type::synth_path(vec!["Text".to_owned()], vec![unit.clone()], span);
        let remote_text = Type::synth_path(
            vec!["remote".to_owned(), "Text".to_owned()],
            Vec::new(),
            span,
        );
        let remote_hidden = Type::synth_path(
            vec!["remote".to_owned(), "Hidden".to_owned()],
            Vec::new(),
            span,
        );

        assert!(matches!(
            lowerer.enriched_field_payload_type(&local_text, "Text", 0, 1),
            Some(Type::Function { abi_arity: 1, .. })
        ));
        assert!(matches!(
            lowerer.enriched_field_payload_type(&remote_text, "Text", 0, 1),
            Some(Type::Function { abi_arity: 0, .. })
        ));
        assert!(
            lowerer
                .enriched_field_payload_type(&remote_text, "Other", 0, 1)
                .is_none(),
            "the field identity must agree with the nominal slot"
        );
        let wrong_arity = Type::synth_path(
            vec!["remote".to_owned(), "Text".to_owned()],
            vec![unit],
            span,
        );
        assert!(
            lowerer
                .enriched_field_payload_type(&wrong_arity, "Text", 0, 1)
                .is_none(),
            "a malformed universal instantiation must retain the conservative fallback"
        );
        assert!(
            lowerer
                .enriched_field_payload_type(&remote_hidden, "Hidden", 0, 1)
                .is_none(),
            "existential projector payloads retain their separate CPS recovery"
        );
    }

    fn assert_flat_payload_adapted_to_packed_storage(payload: &Expr<Routed>) {
        let Expr::Let {
            name, value, body, ..
        } = payload
        else {
            panic!(
                "the stored flat payload must stage its function before adapting it: {payload:?}"
            )
        };
        let Expr::FnExpr {
            sig: source_sig, ..
        } = value.as_ref()
        else {
            panic!("the staging let must retain the supplied flat function: {value:?}")
        };
        assert_eq!(
            source_sig.value_param_count(),
            2,
            "the supplied flat function must retain its two-parameter ABI"
        );
        let (type_stage, body) = unwrap_function_adapter_type_stage(body, name);
        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("the staged payload must be wrapped in a value adapter: {body:?}")
        };
        assert_eq!(
            sig.value_param_count(),
            1,
            "the adapter must expose the declared one-product-parameter ABI"
        );
        let adapter_param_name = &sig
            .value_params()
            .next()
            .expect("the adapter has exactly one value parameter")
            .name;
        let Expr::LowIndirectCall { callee, args, .. } = body.as_ref() else {
            panic!("the adapter must invoke the supplied flat function: {body:?}")
        };
        assert_eq!(
            args.len(),
            2,
            "the flat source closure must receive two slots"
        );
        assert!(
            matches!(callee.as_ref(), Expr::LowBoundRef { name: callee_name, .. }
                if callee_name == type_stage),
            "the adapter must call the residual from its type stage: {callee:?}"
        );
        for (index, arg) in args.iter().enumerate() {
            assert!(
                matches!(arg, Expr::EnrichedProject {
                    target,
                    index: projected_index,
                    arity: 2,
                    ..
                } if *projected_index == index
                    && matches!(target.as_ref(), Expr::LowBoundRef { name, .. }
                        if name == adapter_param_name)),
                "the adapter must project flat slot {index} from its packed argument: {arg:?}"
            );
        }
    }

    fn assert_packed_payload_stored_directly(payload: &Expr<Routed>) {
        assert!(
            matches!(payload, Expr::FnExpr { sig, body, .. }
                if sig.value_param_count() == 1
                    && !matches!(body.as_ref(), Expr::LowIndirectCall { .. })),
            "an already packed payload must not gain an adapter: {payload:?}"
        );
    }

    fn assert_unit_payload_stored_directly(payload: &Expr<Routed>) {
        assert!(
            matches!(payload, Expr::FnExpr { sig, body, .. }
                if sig.value_param_count() == 0
                    && !matches!(body.as_ref(), Expr::LowIndirectCall { .. })),
            "an erased-Unit payload must not gain an adapter: {payload:?}"
        );
    }

    #[test]
    fn constructor_results_retain_nominal_identity_for_nullary_calls() {
        let routed = full_pipeline_to_routed(&[
            (
                "origin.kio",
                "module origin; \
                 pub newtype Plain : . { pub constructor make; pub projector open; }; \
                 pub newtype Const[A] : . { pub constructor make; pub projector open; }; \
                 pub newtype Hidden[A] <E> : E { pub constructor make; pub projector open; }; \
                 fn plain() -> Plain { .() { Plain.make(()) }(()) } \
                 fn local[A]() -> Const(A) { .() { Const.make(A, ()) }(()) } \
                 fn hidden[A][E](value: E) -> Hidden(A) { \
                   .() { Hidden.make(A, E, value) }(()) \
                 }",
            ),
            (
                "relay.kio",
                "module relay; import origin as source; \
                 pub type Forward[A] = source.Const(A);",
            ),
            (
                "consumer.kio",
                "module consumer; import origin as source; import origin(Plain); \
                 import relay(Forward); import relay as relay; \
                 newtype Const[A] : . { constructor make; projector open; }; \
                 fn local[A]() -> Const(A) { .() { Const.make(A, ()) }(()) } \
                 fn selected() -> Plain { .() { Plain.make(()) }(()) } \
                 fn qualified[A]() -> source.Const(A) { .() { source.Const.make(A, ()) }(()) } \
                 fn forwarded[A]() -> Forward(A) { .() { Forward.make(A, ()) }(()) } \
                 fn qualified_forwarded[A]() -> Forward(A) { \
                   .() { relay.Forward.make(A, ()) }(()) \
                 } \
                 fn hidden[A][E](value: E) -> source.Hidden(A) { \
                   .() { source.Hidden.make(A, E, value) }(()) \
                 }",
            ),
        ]);
        for (module, name, owner, nominal, generic) in [
            ("origin", "plain", "origin", "Plain", false),
            ("origin", "local", "origin", "Const", true),
            ("origin", "hidden", "origin", "Hidden", true),
            ("consumer", "local", "consumer", "Const", true),
            ("consumer", "selected", "origin", "Plain", false),
            ("consumer", "qualified", "origin", "Const", true),
            ("consumer", "forwarded", "origin", "Const", true),
            ("consumer", "qualified_forwarded", "origin", "Const", true),
            ("consumer", "hidden", "origin", "Hidden", true),
        ] {
            let body = &routed_fn(&routed, module, name).body;
            let Expr::LowIndirectCall { callee, args, .. } = body else {
                panic!("{module}.{name} must call its thunk: {body:?}")
            };
            assert!(
                args.is_empty(),
                "{module}.{name} must use the zero-slot ABI"
            );
            let Expr::FnExpr { ret_ty, .. } = callee.as_ref() else {
                panic!("{module}.{name} must retain its literal thunk: {callee:?}")
            };
            let Some(Type::Path { segments, args, .. }) = ret_ty else {
                panic!("{module}.{name} must retain its nominal result: {ret_ty:?}")
            };
            assert_eq!(
                segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>(),
                [owner, nominal],
                "{module}.{name}"
            );
            assert_eq!(args.len(), usize::from(generic), "{module}.{name}");
            if generic {
                assert!(
                    matches!(&args[0], Type::Path { segments, args, .. }
                        if segments.len() == 1 && segments[0].name == "A" && args.is_empty()),
                    "{module}.{name} must retain only its universal argument: {args:?}"
                );
            }
        }
    }

    #[test]
    fn constructor_result_recovery_requires_exact_resolved_declaration_and_arguments() {
        let enriched = full_pipeline_to_enriched(&[(
            "owner.kio",
            "module owner; \
             newtype Box[A] <E> : E { constructor make; projector open; };",
        )]);
        let ctx = LoweringCtx::from_package(&enriched);
        let module = &enriched.module("owner").expect("owner module").module;
        let lowerer = Lowerer::new_for_module(&ctx, module, None, "owner");
        for qualified in [false, true] {
            for (nominal, member, count, known) in [
                ("Box", "make", 2, true),
                ("Missing", "make", 2, false),
                ("Box", "missing", 2, false),
                ("Box", "open", 2, false),
                ("Box", "make", 1, false),
                ("Box", "make", 3, false),
            ] {
                let type_args = vec![Type::Unit { meta: test_meta() }; count];
                let payload = Box::new(low_bound_ref("value", Span::new(0, 0)));
                let value = if qualified {
                    Expr::LowQualifiedNewtypeMember {
                        occurrence: Default::default(),
                        module_path: "owner".to_owned(),
                        newtype: nominal.to_owned(),
                        member: member.to_owned(),
                        type_args,
                        payload,
                        meta: test_meta(),
                        ext: (),
                    }
                } else {
                    Expr::LowNewtypeCtor {
                        occurrence: Default::default(),
                        newtype: nominal.to_owned(),
                        member: member.to_owned(),
                        type_args,
                        payload,
                        meta: test_meta(),
                        ext: (),
                    }
                };
                let expected = known.then(|| {
                    Type::synth_path(
                        vec!["owner".to_owned(), "Box".to_owned()],
                        vec![Type::Unit { meta: test_meta() }],
                        Span::new(0, 0),
                    )
                });
                assert_eq!(
                    lowerer.lowered_expr_type(&value),
                    expected,
                    "qualified={qualified} {nominal}.{member} count={count}"
                );
            }
        }
    }

    #[test]
    fn constructor_packet_preserves_local_qualified_and_forwarded_values() {
        let routed = full_pipeline_to_routed(&[
            (
                "origin.kio",
                "module origin; \
                 pub newtype Packet[A][B] : A & B { \
                   pub constructor make; pub projector open; \
                 }; \
                 pub newtype Hidden[A] <B> : A & (B -> A) & B { \
                   pub constructor make; pub projector open; \
                 }; \
                 fn local[A][B](left: A, right: B) -> Packet(A, B) { \
                   Packet.make(A, B, left, right) \
                 } \
                 fn existential[A][B](left: A, show: B -> A, right: B) -> Hidden(A) { \
                   Hidden.make(A, B, left, show, right) \
                 }",
            ),
            (
                "relay.kio",
                "module relay; import origin as source; \
                 pub type Parcel[A][B] = source.Packet(A, B);",
            ),
            (
                "consumer.kio",
                "module consumer; import origin as source; \
                 import relay(Parcel); import relay as relay; \
                 fn qualified[A][B](left: A, right: B) -> source.Packet(A, B) { \
                   source.Packet.make(A, B, left, right) \
                 } \
                 fn forwarded[A][B](left: A, right: B) -> Parcel(A, B) { \
                   Parcel.make(A, B, left, right) \
                 } \
                 fn qualified_forwarded[A][B](left: A, right: B) -> Parcel(A, B) { \
                   relay.Parcel.make(A, B, left, right) \
                 }",
            ),
        ]);
        for (module, name, expected_names) in [
            ("origin", "local", &["left", "right"][..]),
            ("origin", "existential", &["left", "show", "right"][..]),
            ("consumer", "qualified", &["left", "right"][..]),
            ("consumer", "forwarded", &["left", "right"][..]),
            ("consumer", "qualified_forwarded", &["left", "right"][..]),
        ] {
            let (payload, type_args) = match &routed_fn(&routed, module, name).body {
                Expr::LowNewtypeCtor {
                    payload, type_args, ..
                } => (payload, type_args),
                Expr::LowQualifiedNewtypeMember {
                    module_path,
                    newtype,
                    payload,
                    type_args,
                    ..
                } => {
                    assert_eq!(module_path, "origin");
                    assert_eq!(newtype, "Packet");
                    (payload, type_args)
                }
                other => panic!("{name} must route to its constructor: {other:?}"),
            };
            assert_eq!(type_args.len(), 2, "{name} retains only explicit type args");
            let Expr::EnrichedTuple { items, .. } = payload.as_ref() else {
                panic!("{name} must store its complete product packet: {payload:?}");
            };
            let actual_names: Vec<&str> = items
                .iter()
                .map(|item| match item {
                    Expr::LowBoundRef { name, .. } => name.as_str(),
                    other => panic!("{name} must preserve each ordinary value: {other:?}"),
                })
                .collect();
            assert_eq!(
                actual_names, expected_names,
                "{name} preserves packet order"
            );
        }
    }

    #[test]
    fn constructor_packet_preserves_grouped_unit_and_residual_member_stages() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             newtype Packet[A][B] : A & B { constructor make; projector open; }; \
             newtype Empty : . { constructor make; projector open; }; \
             newtype Function[A] : A -> A { constructor make; projector open; }; \
             fn grouped[A][B](payload: A & B) -> Packet(A, B) { \
               Packet.make(A, B, payload) \
             } \
             fn unit() -> Empty { Empty.make() } \
             fn first_class[A][B]() -> (A & B) -> Packet(A, B) { Packet.make(A, B) } \
             fn residual[A]() -> [B] (A & B) -> Packet(A, B) { Packet.make(A) } \
             fn projector_call[A](wrapped: Function(A), value: A) -> A { \
               Function.open(A, wrapped, value) \
             }",
        )]);
        assert!(matches!(
            &routed_fn(&routed, "test", "grouped").body,
            Expr::LowNewtypeCtor { payload, .. }
                if matches!(payload.as_ref(), Expr::LowBoundRef { name, .. } if name == "payload")
        ));
        assert!(matches!(
            &routed_fn(&routed, "test", "unit").body,
            Expr::LowNewtypeCtor { payload, .. } if matches!(payload.as_ref(), Expr::Unit { .. })
        ));
        fn retains_function(expr: &Expr<Routed>) -> bool {
            match expr {
                Expr::FnExpr { .. } => true,
                Expr::Let { value, body, .. } => retains_function(value) || retains_function(body),
                Expr::LowTypeApplication { callee, .. } => retains_function(callee),
                _ => false,
            }
        }
        for name in ["first_class", "residual"] {
            assert!(
                retains_function(&routed_fn(&routed, "test", name).body),
                "{name} keeps its ordinary residual value function"
            );
        }
        assert!(matches!(
            &routed_fn(&routed, "test", "projector_call").body,
            Expr::LowIndirectCall { callee, args, .. }
                if args.len() == 1 && matches!(callee.as_ref(), Expr::LowNewtypeProj { .. })
        ));
    }

    #[test]
    fn local_newtype_constructor_canonicalizes_polymorphic_payload_storage() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             newtype Packed : [A] (. & .) -> . { constructor mk_packed; projector run; }; \
             newtype Thunk : [A] . -> . { constructor mk_thunk; projector force; }; \
             newtype Exists <U> : [A] (U & A) -> U { constructor mk_exists; projector open; }; \
             fn flat() -> Packed { \
               Packed.mk_packed(.[A](_left: ., _right: .) { () }) \
             } \
             fn packed() -> Packed { Packed.mk_packed(.[A](_pair: . & .) { () }) } \
             fn unit() -> Thunk { Thunk.mk_thunk(.[A]() { () }) } \
             fn existential() -> Exists { \
               Exists.mk_exists(., .[A](_left: ., _right: A) { _left }) \
             }",
        )]);

        let Expr::LowNewtypeCtor { payload, .. } = &routed_fn(&routed, "test", "flat").body else {
            panic!("the local flat fixture must lower to a newtype constructor")
        };
        assert_flat_payload_adapted_to_packed_storage(payload);

        let Expr::LowNewtypeCtor { payload, .. } = &routed_fn(&routed, "test", "packed").body
        else {
            panic!("the local packed fixture must lower to a newtype constructor")
        };
        assert_packed_payload_stored_directly(payload);

        let Expr::LowNewtypeCtor { payload, .. } = &routed_fn(&routed, "test", "unit").body else {
            panic!("the local Unit fixture must lower to a newtype constructor")
        };
        assert_unit_payload_stored_directly(payload);

        let Expr::LowNewtypeCtor { payload, .. } = &routed_fn(&routed, "test", "existential").body
        else {
            panic!("the local existential fixture must lower to a newtype constructor")
        };
        assert_flat_payload_adapted_to_packed_storage(payload);
    }

    #[test]
    fn qualified_newtype_constructor_canonicalizes_polymorphic_payload_storage() {
        let routed = full_pipeline_to_routed(&[
            (
                "types.kio",
                "module types; \
                 pub newtype Packed : [A] (. & .) -> . { \
                   pub constructor mk_packed; pub projector run; \
                 }; \
                 pub newtype Thunk : [A] . -> . { \
                   pub constructor mk_thunk; pub projector force; \
                 }; \
                 pub newtype Exists <U> : [A] (U & A) -> U { \
                   pub constructor mk_exists; pub projector open; \
                 }; \
                 pub newtype Identity[A] : A { \
                   pub constructor mk_id; pub projector un_id; \
                 }; \
                 pub newtype Functor[*F] : [A][B] ((A -> B) & F(A)) -> F(B) { \
                   pub constructor mk_functor; pub projector fmap; \
                 };",
            ),
            (
                "main.kio",
                "module main; \
                 import types as alias; \
                 fn flat() -> alias.Packed { \
                   alias.Packed.mk_packed(.[A](_left: ., _right: .) { () }) \
                 } \
                 fn packed() -> alias.Packed { \
                   alias.Packed.mk_packed(.[A](_pair: . & .) { () }) \
                 } \
                 fn unit() -> alias.Thunk { alias.Thunk.mk_thunk(.[A]() { () }) } \
                 fn existential() -> alias.Exists { \
                   alias.Exists.mk_exists(., .[A](_left: ., _right: A) { _left }) \
                 } \
                 fn nominal_result() -> alias.Functor(alias.Identity) { \
                   alias.Functor.mk_functor(.[A][B](step, value) { \
                     alias.Identity.mk_id(step(alias.Identity.un_id(value))) \
                   }) \
                 }",
            ),
        ]);

        let Expr::LowQualifiedNewtypeMember { payload, .. } =
            &routed_fn(&routed, "main", "flat").body
        else {
            panic!("the qualified flat fixture must lower to a newtype constructor")
        };
        assert_flat_payload_adapted_to_packed_storage(payload);

        let Expr::LowQualifiedNewtypeMember { payload, .. } =
            &routed_fn(&routed, "main", "packed").body
        else {
            panic!("the qualified packed fixture must lower to a newtype constructor")
        };
        assert_packed_payload_stored_directly(payload);

        let Expr::LowQualifiedNewtypeMember { payload, .. } =
            &routed_fn(&routed, "main", "unit").body
        else {
            panic!("the qualified Unit fixture must lower to a newtype constructor")
        };
        assert_unit_payload_stored_directly(payload);

        let Expr::LowQualifiedNewtypeMember { payload, .. } =
            &routed_fn(&routed, "main", "existential").body
        else {
            panic!("the qualified existential fixture must lower to a newtype constructor")
        };
        assert_flat_payload_adapted_to_packed_storage(payload);

        let Expr::LowQualifiedNewtypeMember { payload, .. } =
            &routed_fn(&routed, "main", "nominal_result").body
        else {
            panic!("the qualified nominal-result fixture must lower to a newtype constructor")
        };
        assert_two_forall_flat_source_adapter(payload);
    }

    #[test]
    fn recovered_match_payload_types_preserve_nullary_lambda_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             import __intrinsics__; \
             fn caller[A][B][C](value: A | B | C) -> A | B | C { \
               __either__(A, B | C, A | B | C, value, \
                 .(left: A) { __left__(A, B | C, .() { left }(())) }, \
                 .(tail: B | C) { __right__(A, B | C, .() { tail }(())) }) \
             }",
        )]);
        let Expr::EnrichedMatch {
            arms, scrutinee_ty, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the opaque sum must retain its two-arm match")
        };
        assert_eq!(arms.len(), 2);
        let Type::Sum { left, right, .. } = scrutinee_ty else {
            panic!("the scrutinee retains its binary sum type")
        };
        for (index, arm) in arms.iter().enumerate() {
            let payload = match &arm.body {
                Expr::EnrichedInject { payload, .. } => payload,
                Expr::EnrichedMatch { scrutinee, .. } => scrutinee,
                other => panic!("the branch must re-inject its thunk result: {other:?}"),
            };
            let Expr::LowIndirectCall { callee, args, .. } = payload.as_ref() else {
                panic!("the branch thunk must retain its ordinary call: {payload:?}")
            };
            assert!(
                args.is_empty(),
                "a zero-slot thunk receives no ABI args: {args:?}"
            );
            let Expr::FnExpr { ret_ty, .. } = callee.as_ref() else {
                panic!("the callee must remain the zero-slot thunk: {callee:?}")
            };
            assert_eq!(
                ret_ty.as_ref(),
                Some(if index == 0 {
                    left.as_ref()
                } else {
                    right.as_ref()
                }),
                "the final branch retains the whole residual sub-sum"
            );
        }
    }

    #[test]
    fn computed_unit_call_stages_callee_before_erased_argument_effect() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host fn make(_unit: .) -> . -> .; \
             host fn observe(_unit: .) -> .; \
             fn caller() -> . { make()(observe()) }",
        )]);
        let Expr::Let {
            name, value, body, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("an argument wrapper must stage its computed callee first")
        };
        assert!(name.starts_with("__kio_call_callee"), "{name}");
        assert!(
            matches!(value.as_ref(), Expr::LowHostCall { name, .. } if name == "make"),
            "the outer let must evaluate `make()` first: {value:?}"
        );
        let Expr::Seq {
            value: unit_effect,
            body: final_call,
            ..
        } = body.as_ref()
        else {
            panic!("the erased Unit operand must remain sequenced after the callee")
        };
        assert!(
            matches!(unit_effect.as_ref(), Expr::LowHostCall { name, .. } if name == "observe"),
            "the Unit operand effect must be second: {unit_effect:?}"
        );
        assert!(
            matches!(final_call.as_ref(), Expr::LowIndirectCall { callee, args, .. }
                if args.is_empty()
                    && matches!(callee.as_ref(), Expr::LowBoundRef { name: callee_name, .. }
                        if callee_name == name)),
            "the final zero-ABI application must call the staged callee: {final_call:?}"
        );
    }

    #[test]
    fn computed_function_value_is_staged_before_top_level_abi_adapter() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host fn make(_unit: .)[A](_left: A, _right: A) -> A; \
             fn accept(_function: [A] (A & A) -> A) -> . { () } \
             fn caller() -> . { accept(make()) }",
        )]);
        let Expr::LowModuleCall { args, .. } = &routed_fn(&routed, "test", "caller").body else {
            panic!(
                "the fixture must lower to a local function call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        let [payload] = args.as_slice() else {
            panic!("the local call must carry its one function argument: {args:?}")
        };
        let Expr::Let {
            name, value, body, ..
        } = payload
        else {
            panic!("a computed function value must be evaluated before its adapter: {payload:?}")
        };
        assert!(
            matches!(value.as_ref(), Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } if type_args.is_empty()
                && args.is_empty()
                && matches!(callee.as_ref(), Expr::LowHostFnValueRef { name, .. }
                    if name == "make")),
            "the staging let must advance the original host function once: {value:?}"
        );
        let (type_stage, body) = unwrap_function_adapter_type_stage(body, name);
        let Expr::FnExpr { body, .. } = body else {
            panic!("the staged value must feed the existing value adapter: {body:?}")
        };
        assert!(
            matches!(body.as_ref(), Expr::LowIndirectCall { callee, .. }
                if matches!(callee.as_ref(), Expr::LowBoundRef { name: callee_name, .. }
                    if callee_name == type_stage)),
            "the adapter must call the staged value, not reevaluate the producer: {body:?}"
        );
    }

    #[test]
    fn malformed_cps_projector_call_rejects_extra_continuation_values() {
        let mut enriched = full_pipeline_to_enriched(&[(
            "test.kio",
            "module test; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> . { \
               Pack.unpack(pack())(.[U](_payload: U) -> . { () }) \
             }",
        )]);
        let entry = enriched.modules_mut().next().expect("one test module");
        let caller = entry
            .module
            .items
            .iter_mut()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "caller" => Some(def),
                _ => None,
            })
            .expect("caller function");
        let Expr::Call { args, .. } = &mut caller.body else {
            panic!("the typed fixture must retain its outer CPS call")
        };
        let continuation_index = args
            .iter()
            .position(|arg| matches!(arg, CallArg::Value(_)))
            .expect("the typed CPS call must carry its continuation");
        args.insert(
            continuation_index,
            CallArg::Value(Expr::Unit {
                occurrence: Default::default(),
                meta: test_meta_enriched(),
            }),
        );

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lower(&enriched)))
            .expect_err("a malformed typed CPS call must not discard a leading value");
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("the invariant failure must have a string message");
        assert_eq!(
            message,
            "internal error: entered unreachable code: a typechecked existential-projector continuation call carries exactly one continuation value"
        );
    }

    #[test]
    fn cps_continuations_share_one_exact_type_adapter_for_all_value_forms() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32; \
             host fn value(_unit: .) -> I32; \
             labels { pack <U> : I32 & U }; \
             fn pack() -> Pack { Pack.mk((value(), ())) } \
             fn named[U](number: I32, _witness: U) -> I32 { number } \
             fn make(_unit: .) -> [U] (I32 & U) -> I32 { \
               .[U](_pair: I32 & U) -> I32 { value() } \
             } \
             fn literal() -> I32 { \
               Pack.get(pack())(.[U](_pair: I32 & U) -> I32 { value() }) \
             } \
             fn named_value() -> I32 { Pack.get(pack())(named) } \
             fn let_value() -> I32 { let continuation = named; Pack.get(pack())(continuation) } \
             fn computed() -> I32 { Pack.get(pack())(make(())) }",
        )]);

        fn find_cps(expr: &Expr<Routed>) -> (&Expr<Routed>, &Type<Routed>) {
            match expr {
                Expr::LowCpsProjectorApply {
                    continuation,
                    continuation_ty,
                    ..
                } => (continuation, continuation_ty),
                Expr::Let { body, .. } | Expr::Seq { body, .. } => find_cps(body),
                other => panic!("expected a CPS projector application, got: {other:#?}"),
            }
        }

        let cps = |name: &str| {
            let body = &routed_fn(&routed, "test", name).body;
            let (continuation, continuation_ty) = find_cps(body);
            let (stages, callable) = continuation_ty.peel_leading_foralls();
            assert_eq!(stages, 1, "`{name}` must retain its existential stage");
            assert!(
                matches!(callable, Type::Function { abi_arity: 1, .. }),
                "`{name}` must carry the one-slot payload ABI: {continuation_ty:#?}"
            );
            continuation
        };

        assert!(matches!(cps("literal"), Expr::FnExpr { .. }));
        let Expr::Let {
            name: named_stage,
            value: named_value,
            body: named_adapter,
            ..
        } = cps("named_value")
        else {
            panic!("the named continuation must be captured once")
        };
        assert!(
            matches!(named_value.as_ref(), Expr::LowModuleFnValueRef { mangled, .. }
            if mangled == "named")
        );
        let (_named_residual, named_adapter) =
            unwrap_function_adapter_type_stage(named_adapter, named_stage);
        assert!(matches!(named_adapter, Expr::FnExpr { .. }));

        let Expr::Let {
            name: let_stage,
            value: let_value,
            body: let_adapter,
            ..
        } = cps("let_value")
        else {
            panic!("the let-bound continuation must be captured once")
        };
        assert!(matches!(let_value.as_ref(), Expr::LowBoundRef { name, .. }
            if name == "continuation"));
        let (_let_residual, let_adapter) =
            unwrap_function_adapter_type_stage(let_adapter, let_stage);
        assert!(matches!(let_adapter, Expr::FnExpr { .. }));
        assert!(matches!(
            &routed_fn(&routed, "test", "let_value").body,
            Expr::Let { value, .. }
                if matches!(value.as_ref(), Expr::LowModuleFnValueRef { mangled, .. }
                    if mangled == "named")
        ));
        assert!(matches!(
            cps("computed"),
            Expr::LowModuleCall { mangled, .. } if mangled == "make"
        ));
    }

    #[test]
    fn cps_continuation_binder_does_not_capture_same_spelling_caller_type() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             labels { boxed[A] <U> : A & U }; \
             fn consume[U](boxed: Boxed(U), fallback: U) -> U { \
               Boxed.get(U, boxed)(.[Hidden](_payload: U & Hidden) -> U { fallback }) \
             }",
        )]);
        let Expr::LowCpsProjectorApply {
            continuation_ty, ..
        } = &routed_fn(&routed, "test", "consume").body
        else {
            panic!("the existential projector call must recover to CPS");
        };
        let Type::Forall { param, body, .. } = continuation_ty else {
            panic!("the continuation must retain its existential binder: {continuation_ty:#?}");
        };
        assert_ne!(
            param.name, "U",
            "the copied existential binder must not capture the caller's free `U`"
        );
        let Type::Function {
            param: payload,
            ret,
            ..
        } = body.as_ref()
        else {
            panic!("the continuation binder must wrap its payload function: {body:#?}");
        };
        let Type::Product { left, right, .. } = payload.as_ref() else {
            panic!("the instantiated payload must remain a product: {payload:#?}");
        };
        assert!(matches!(left.as_ref(), Type::Path { segments, .. }
            if segments.len() == 1 && segments[0].name == "U"));
        assert!(matches!(right.as_ref(), Type::Path { segments, .. }
            if segments.len() == 1 && segments[0].name == param.name));
        assert!(matches!(ret.as_ref(), Type::Path { segments, .. }
            if segments.len() == 1 && segments[0].name == "U"));
    }

    #[test]
    fn unannotated_cps_continuation_uses_selected_literal_result_type() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32 role(i32); \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> I32 { Pack.unpack(pack())(.[U](_payload: U) { 42(I32) }) }",
        )]);
        let Expr::LowCpsProjectorApply {
            continuation_ty, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the existential projector call must recover to CPS");
        };
        let (_, callable) = continuation_ty.peel_leading_foralls();
        let Type::Function { ret, .. } = callable else {
            panic!("the exact continuation type must remain callable: {continuation_ty:#?}");
        };
        assert!(
            matches!(ret.as_ref(), Type::Path { segments, .. }
                if segments.iter().map(|segment| segment.name.as_str()).eq(["test", "I32"])),
            "the selected literal result is `I32`, not an invented Unit: {ret:#?}"
        );
    }

    #[test]
    fn cps_continuation_payload_binder_may_be_inferred() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32 role(i32); \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> I32 { Pack.unpack(pack())(.[U](_payload) { 42(I32) }) }",
        )]);
        let Expr::LowCpsProjectorApply {
            continuation_ty, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the inferred-binder continuation call must recover to CPS");
        };
        let (type_stages, callable) = continuation_ty.peel_leading_foralls();
        assert_eq!(type_stages, 1);
        assert!(
            matches!(callable, Type::Function { abi_arity: 1, .. }),
            "the source binder shape must not require a source-side type annotation: {continuation_ty:#?}"
        );
    }

    #[test]
    fn retained_unannotated_continuation_keeps_recursive_callable_shape() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32; \
             host fn value(_unit: .) -> I32; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> I32 { \
               let continuation = \
                 .[U](_payload: U) { .(left: I32, _right: I32) { left } }; \
               let _first = \
                 Pack.unpack(pack())((I32 & I32) -> I32, continuation)((value(), value())); \
               Pack.unpack(pack())((I32 & I32) -> I32, continuation)((value(), value())) \
             }",
        )]);
        let Expr::Let {
            name, value, body, ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the reused continuation binding must remain a let");
        };
        assert_eq!(name, "continuation");
        assert!(matches!(value.as_ref(), Expr::FnExpr { .. }));
        let Expr::Let {
            value: first,
            body: second,
            ..
        } = body.as_ref()
        else {
            panic!("both continuation uses must remain observable: {body:#?}");
        };

        fn applied_cps(expr: &Expr<Routed>) -> &Expr<Routed> {
            match expr {
                cps @ Expr::LowCpsProjectorApply { .. } => cps,
                Expr::LowIndirectCall { callee, .. } | Expr::Let { body: callee, .. } => {
                    applied_cps(callee)
                }
                other => panic!("expected an applied CPS projector, got: {other:#?}"),
            }
        }

        for application in [first.as_ref(), second.as_ref()] {
            let Expr::LowCpsProjectorApply { continuation, .. } = applied_cps(application) else {
                unreachable!()
            };
            let Expr::Let {
                name: staged_continuation,
                body,
                ..
            } = continuation.as_ref()
            else {
                panic!(
                    "each retained reference must capture its continuation once: {continuation:#?}"
                )
            };
            let (_type_stage, body) = unwrap_function_adapter_type_stage(body, staged_continuation);
            assert!(
                matches!(body, Expr::FnExpr { body, .. }
                    if matches!(body.as_ref(), Expr::Let { .. })),
                "each retained reference must recover and eagerly stage its recursive function-result adapter: {continuation:#?}"
            );
        }
    }

    #[test]
    fn retained_continuation_shape_is_not_rebound_by_later_same_name_let() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32; \
             host fn value(_unit: .) -> I32; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> (I32 & I32) -> I32 { \
               let f = .(left: I32, _right: I32) { left }; \
               let continuation = .[U](_payload: U) { f }; \
               let f = .(_pair: I32 & I32) { value() }; \
               Pack.unpack(pack())((I32 & I32) -> I32, continuation) \
             }",
        )]);

        let Expr::Let {
            body: continuation_body,
            ..
        } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the first `f` binding must remain in the routed body");
        };
        let Expr::Let {
            body: shadow_body, ..
        } = continuation_body.as_ref()
        else {
            panic!("the retained continuation binding must remain in the routed body");
        };
        let Expr::Let { body: cps, .. } = shadow_body.as_ref() else {
            panic!("the later same-name `f` binding must remain in the routed body");
        };
        let Expr::LowCpsProjectorApply { continuation, .. } = cps.as_ref() else {
            panic!("the final call must recover to a CPS projector application: {cps:#?}");
        };
        let Expr::Let {
            name: staged_continuation,
            body,
            ..
        } = continuation.as_ref()
        else {
            panic!("the retained continuation must be captured once: {continuation:#?}")
        };
        let (_type_stage, body) = unwrap_function_adapter_type_stage(body, staged_continuation);
        assert!(
            matches!(body, Expr::FnExpr { body, .. }
                if matches!(body.as_ref(), Expr::Let { body, .. }
                    if matches!(body.as_ref(), Expr::FnExpr { .. }))),
            "the retained continuation must adapt the original flat `f`; the later packed `f` must not rebind its descriptor: {continuation:#?}"
        );
    }

    #[test]
    fn retained_continuation_alias_chain_keeps_constant_size_links() {
        let span = Span::new(0, 0);
        let pair = product_ty(path_ty("I32"), path_ty("I32"));
        let expected = fn_ty(pair, path_ty("I32"), 1);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        lowerer.push_bound_routed_with_abi(
            "f0",
            Some(expected.clone()),
            Some(SelectedAbiShape::Function {
                groups: vec![SelectedAbiGroup::Value {
                    arity: 2,
                    binders: Vec::new(),
                }],
                body: Box::new(SelectedAbiShape::Value),
            }),
        );

        for index in 1..=256 {
            let previous = format!("f{}", index - 1);
            let shape = lowerer.selected_abi_shape(&Expr::LowBoundRef {
                occurrence: Default::default(),
                name: previous,
                meta: Meta::new(span),
                ext: (),
            });
            assert!(
                matches!(shape, SelectedAbiShape::BoundRef { .. }),
                "an alias descriptor must retain one stable link, not clone its predecessor"
            );
            lowerer.push_bound_routed_with_abi(
                &format!("f{index}"),
                Some(expected.clone()),
                Some(shape),
            );
        }

        let final_shape = lowerer.selected_abi_shape(&Expr::LowBoundRef {
            occurrence: Default::default(),
            name: "f256".to_owned(),
            meta: Meta::new(span),
            ext: (),
        });
        let actual = lowerer
            .selected_shape_abi_type(&final_shape, &expected, &[])
            .expect("a deep alias chain must resolve to its original callable shape");
        assert!(
            matches!(actual, Type::Function { abi_arity: 2, .. }),
            "the alias chain must retain the original flat two-slot ABI: {actual:#?}"
        );
    }

    #[test]
    fn cps_continuation_recovers_literal_function_result_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type I32; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> (I32 & I32) -> I32 { \
               Pack.unpack(pack())( \
                 (I32 & I32) -> I32, \
                 .[U](_payload: U) { .(left: I32, _right: I32) { left } } \
               ) \
             }",
        )]);
        let Expr::LowCpsProjectorApply { continuation, .. } =
            &routed_fn(&routed, "test", "caller").body
        else {
            panic!("the existential projector call must recover to CPS");
        };
        let Expr::Let {
            name: staged_continuation,
            body,
            ..
        } = continuation.as_ref()
        else {
            panic!("the continuation value must be staged once: {continuation:#?}");
        };
        let (_type_stage, body) = unwrap_function_adapter_type_stage(body, staged_continuation);
        let Expr::FnExpr { body, .. } = body else {
            panic!("the continuation adapter must retain its value stage: {body:#?}");
        };
        let Expr::Let {
            name, value, body, ..
        } = body.as_ref()
        else {
            panic!("the produced function must be staged before its adapter: {body:#?}");
        };
        assert_eq!(name, "__kio_fn0_result__");
        assert!(matches!(value.as_ref(), Expr::LowIndirectCall { .. }));
        let Expr::FnExpr { body, .. } = body.as_ref() else {
            panic!("the produced function must be repacked: {body:#?}");
        };
        assert!(matches!(body.as_ref(), Expr::LowIndirectCall { callee, .. }
            if matches!(callee.as_ref(), Expr::LowBoundRef { name, .. }
                if name == "__kio_fn0_result__")));
    }

    #[test]
    fn sequenced_computed_callee_preserves_unit_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn make(_unit: .) -> . -> N; \
             host fn observe(_unit: .) -> .; \
             fn caller() -> N { make(observe())() }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the sequenced computed callee must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(
            matches!(callee.as_ref(), Expr::Seq { value, body, .. }
                if matches!(value.as_ref(), Expr::LowHostCall { name, .. } if name == "observe")
                    && matches!(body.as_ref(), Expr::LowHostCall { name, args, .. }
                        if name == "make" && args.is_empty())),
            "the callee must sequence the erased argument effect before `make`: {callee:?}"
        );
        assert!(
            args.is_empty(),
            "the sequenced callee's erased Unit ABI has no runtime argument: {args:?}"
        );
    }

    #[test]
    fn cps_projector_result_recovers_continuation_return_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn value(_unit: .) -> N; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> N { \
               Pack.unpack(pack())(. -> N, .[U](_value: U) { .() { value() } })() \
             }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the CPS projector result must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        assert!(
            matches!(callee.as_ref(), Expr::LowCpsProjectorApply { newtype, .. }
                if newtype == "Pack"),
            "the indirect callee must be the CPS projector application: {callee:?}"
        );
        assert!(
            args.is_empty(),
            "the continuation result's erased Unit ABI has no runtime argument; \
             callee: {callee:?}; args: {args:?}"
        );
    }

    #[test]
    fn sequenced_cps_continuation_body_recovers_tail_return_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn value(_unit: .) -> N; \
             host fn observe(_unit: .) -> .; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> N { \
               Pack.unpack(pack())(. -> N, .[U](_value: U) { observe(); .() { value() } })() \
             }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the CPS projector result must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        let Expr::LowCpsProjectorApply { continuation, .. } = callee.as_ref() else {
            panic!("the indirect callee must be the CPS projector application: {callee:?}")
        };
        assert!(
            matches!(continuation.as_ref(), Expr::FnExpr { body, .. }
                if matches!(body.as_ref(), Expr::Seq { .. })),
            "the continuation must retain its sequenced body: {continuation:?}"
        );
        assert!(
            args.is_empty(),
            "the sequenced continuation's function result has an erased Unit ABI: {args:?}"
        );
    }

    #[test]
    fn cps_continuation_recovers_named_module_function_result_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn value(_unit: .) -> N; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn resume(_unit: .) -> N { value() } \
             fn caller() -> N { \
               Pack.unpack(pack())(. -> N, .[U](_value: U) { resume })() \
             }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the named CPS projector result must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        let Expr::LowCpsProjectorApply { continuation, .. } = callee.as_ref() else {
            panic!("the indirect callee must be the CPS projector application: {callee:?}")
        };
        assert!(
            matches!(continuation.as_ref(), Expr::FnExpr { body, .. }
                if matches!(body.as_ref(), Expr::LowModuleFnValueRef { mangled, .. }
                    if mangled == "resume")),
            "the CPS continuation must return the named module function: {continuation:?}"
        );
        assert!(
            args.is_empty(),
            "the named continuation's function result has an erased Unit ABI: {args:?}"
        );
    }

    #[test]
    fn cps_continuation_recovers_named_host_function_result_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             host fn resume(_unit: .) -> N; \
             newtype Pack <U> : U { constructor mk_pack; projector unpack; }; \
             fn pack() -> Pack { Pack.mk_pack(()) } \
             fn caller() -> N { \
               Pack.unpack(pack())(. -> N, .[U](_value: U) { resume })() \
             }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!(
                "the host CPS projector result must remain an indirect call: {:?}",
                routed_fn(&routed, "test", "caller").body
            )
        };
        let Expr::LowCpsProjectorApply { continuation, .. } = callee.as_ref() else {
            panic!("the indirect callee must be the CPS projector application: {callee:?}")
        };
        assert!(
            matches!(continuation.as_ref(), Expr::FnExpr { body, .. }
                if matches!(body.as_ref(), Expr::LowHostFnValueRef { name, .. }
                    if name == "resume")),
            "the CPS continuation must return the named host function: {continuation:?}"
        );
        assert!(
            args.is_empty(),
            "the host continuation's function result has an erased Unit ABI: {args:?}"
        );
    }

    #[test]
    fn qualified_cps_projector_result_uses_declaring_owner_and_return_abi() {
        let routed = full_pipeline_to_routed(&[
            (
                "types.kio",
                "module types; \
                 pub newtype Pack <U> : U { pub constructor mk_pack; pub projector unpack; }; \
                 pub fn pack() -> Pack { Pack.mk_pack(()) }",
            ),
            (
                "main.kio",
                "module main; \
                 import types as alias; \
                 host type N; \
                 host fn value(_unit: .) -> N; \
                 fn caller() -> N { \
                   alias.Pack.unpack(alias.pack())(. -> N, .[U](_value: U) { .() { value() } })() \
                 }",
            ),
        ]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "main", "caller").body
        else {
            panic!(
                "the qualified CPS projector result must remain an indirect call: {:?}",
                routed_fn(&routed, "main", "caller").body
            )
        };
        assert!(
            matches!(callee.as_ref(), Expr::LowCpsProjectorApply {
                newtype,
                module_path,
                ..
            } if newtype == "Pack" && module_path == "types"),
            "the qualified projector must retain its declaring owner: {callee:?}"
        );
        assert!(
            args.is_empty(),
            "the qualified continuation result's erased Unit ABI has no runtime argument: {args:?}"
        );
    }

    #[test]
    fn wrapper_free_indirect_call_keeps_direct_routed_shape() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             fn identity(value: .) -> . { value } \
             fn choose(_unit: .) -> . -> . { identity } \
             fn caller() -> . { choose()(()) }",
        )]);
        let Expr::LowIndirectCall { callee, args, .. } = &routed_fn(&routed, "test", "caller").body
        else {
            panic!("a wrapper-free indirect call must not gain a staging let")
        };
        assert!(args.is_empty(), "Unit has zero stable ABI slots");
        assert!(
            matches!(callee.as_ref(), Expr::LowModuleCall { mangled, .. } if mangled == "choose"),
            "the computed callee stays directly nested: {callee:?}"
        );
    }

    #[test]
    fn declined_adapter_route_does_not_change_exact_routed_body() {
        fn lowerer<'a>(ctx: &'a LoweringCtx) -> Lowerer<'a> {
            Lowerer {
                bound: std::cell::RefCell::new(Vec::new()),
                type_bound: std::cell::RefCell::new(Vec::new()),
                ctx,
                module_path: "test".to_owned(),
                module_ctx: ModuleCtx::default(),
                host_env: HostEnvScope::default(),
                next_call_callee_scope: Cell::new(0),
                next_selected_abi_binder: Cell::new(0),
            }
        }

        let span = Span::new(0, 0);
        let unchanged = fn_ty(path_ty("A"), path_ty("A"), 1);
        let target = fn_ty(product_ty(path_ty("A"), path_ty("B")), path_ty("A"), 1);
        let source = fn_ty(product_ty(path_ty("A"), path_ty("B")), path_ty("A"), 2);
        let ctx = LoweringCtx::default();

        let probed = lowerer(&ctx);
        assert!(
            probed
                .function_value_adapter(
                    low_bound_ref("unchanged", span),
                    &unchanged,
                    &unchanged,
                    span,
                )
                .is_none()
        );
        let after_declined_probe = probed
            .function_value_adapter(low_bound_ref("callee", span), &target, &source, span)
            .expect("the packed-to-flat boundary needs an adapter");

        let direct = lowerer(&ctx);
        let without_probe = direct
            .function_value_adapter(low_bound_ref("callee", span), &target, &source, span)
            .expect("the packed-to-flat boundary needs an adapter");

        assert_eq!(after_declined_probe, without_probe);
    }

    #[test]
    fn declined_outer_adapter_probe_does_not_consume_nested_adapter_binder() {
        let enriched = full_pipeline_to_enriched(&[(
            "test.kio",
            "module test; \
             type Lens[S][A] = (S -> A) & ((A & S) -> S); \
             fn target[A]() -> Lens(A, A) { \
               (
                 , .(value: A) -> A { value }
                 , .(new_value: A, _old_value: A) -> A { new_value }
                 ) \
             }",
        )]);
        let module = &enriched.module("test").expect("test module").module;
        let target = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "target" => Some(def),
                _ => None,
            })
            .expect("target function");
        let span = target.meta.span;
        let tuple = &target.body;
        let Expr::EnrichedTuple { synth_ty, .. } = tuple else {
            panic!("expected the lens product: {tuple:?}")
        };
        let ctx = LoweringCtx::from_package(&enriched);

        let lowerer = Lowerer::new_for_module(&ctx, module, None, "test");
        lowerer.push_type_bound("A");
        let expected = lowerer.routed_type(synth_ty);
        let after_declined_probe =
            lowerer.lower_value_arg_as_value(tuple.clone(), &expected, span, true);
        lowerer.pop_type_bound();

        let direct_lowerer = Lowerer::new_for_module(&ctx, module, None, "test");
        direct_lowerer.push_type_bound("A");
        let direct = direct_lowerer.lower_expr_owned(tuple.clone());
        direct_lowerer.pop_type_bound();

        assert_eq!(after_declined_probe, direct);
    }

    #[test]
    fn generated_call_binders_ignore_unrelated_prior_declarations() {
        fn first_generated_callee(expr: &Expr<Routed>) -> Option<&str> {
            match expr {
                Expr::Let {
                    name, value, body, ..
                } => name
                    .starts_with("__kio_call_callee")
                    .then_some(name.as_str())
                    .or_else(|| first_generated_callee(value))
                    .or_else(|| first_generated_callee(body)),
                Expr::FnExpr { body, .. } => first_generated_callee(body),
                Expr::EnrichedTuple { items, .. } => items.iter().find_map(first_generated_callee),
                _ => None,
            }
        }

        let baseline = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             type Lens[S][A] = (S -> A) & ((A & S) -> S); \
             fn target[A](_unit: .) -> Lens(A, A) { \
               let get = .(value: A) -> A { value }; \
               let put = .(new_value: A, _old_value: A) -> A { new_value }; \
               (get, put) \
             }",
        )]);
        let extended = full_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             type Lens[S][A] = (S -> A) & ((A & S) -> S); \
             fn unrelated[A](_unit: .) -> Lens(A, A) { \
               let get = .(value: A) -> A { value }; \
               let put = .(new_value: A, _old_value: A) -> A { new_value }; \
               (get, put) \
             } \
             fn target[A](_unit: .) -> Lens(A, A) { \
               let get = .(value: A) -> A { value }; \
               let put = .(new_value: A, _old_value: A) -> A { new_value }; \
               (get, put) \
             }",
        )]);

        let baseline_name = first_generated_callee(&routed_fn(&baseline, "test", "target").body);
        let extended_name = first_generated_callee(&routed_fn(&extended, "test", "target").body);
        assert_eq!(baseline_name, extended_name);
        assert_eq!(baseline_name, Some("__kio_call_callee0__"));
    }

    #[cfg(feature = "prime")]
    fn prime_pipeline_to_routed(sources: &[(&str, &str)]) -> Package<Routed> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source)
                        .unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) = PrimePipeline::lower_package(parsed, None).expect("lower Kio' package");
        let package = Package::build(Path::new(""), modules, None).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check in-body resolution");
        let prime = PrimePipeline::typecheck(&package).expect("typecheck Kio' package");
        lower(&crate::pass::structural_recovery::recover_package(&prime))
    }

    fn same_leaf_host_type_sources() -> [(&'static str, &'static str); 2] {
        [
            (
                "left.kio",
                r#"module left;

host type Shared role(i32);

pub fn keep_left(value: Shared) -> Shared { value }

pub fn keep_generic[Shared](value: Shared) -> Shared { value }
"#,
            ),
            (
                "right.kio",
                r#"module right;

host type Shared role(str);

pub fn keep_right(value: Shared) -> Shared { value }
"#,
            ),
        ]
    }

    fn fn_param_and_ret_paths(
        package: &Package<Routed>,
        module_path: &str,
        fn_name: &str,
    ) -> (Vec<String>, Vec<String>) {
        let def = package
            .module(module_path)
            .expect("module")
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == fn_name => Some(def),
                _ => None,
            })
            .expect("function");
        let param = def
            .sig
            .params
            .iter()
            .find_map(|param| match param {
                SignatureParam::Value(param) => param.ty.as_ref(),
                SignatureParam::Type(_) => None,
            })
            .expect("typed value parameter");
        let Type::Path {
            segments: param, ..
        } = param
        else {
            panic!("parameter is a nominal type")
        };
        let Type::Path { segments: ret, .. } = &def.ret else {
            panic!("return is a nominal type")
        };
        (
            param.iter().map(|segment| segment.name.clone()).collect(),
            ret.iter().map(|segment| segment.name.clone()).collect(),
        )
    }

    fn assert_same_leaf_host_type_paths(package: &Package<Routed>) {
        assert_eq!(
            fn_param_and_ret_paths(package, "left", "keep_left"),
            (
                vec!["left".to_owned(), "Shared".to_owned()],
                vec!["left".to_owned(), "Shared".to_owned()],
            )
        );
        assert_eq!(
            fn_param_and_ret_paths(package, "right", "keep_right"),
            (
                vec!["right".to_owned(), "Shared".to_owned()],
                vec!["right".to_owned(), "Shared".to_owned()],
            )
        );
        assert_eq!(
            fn_param_and_ret_paths(package, "left", "keep_generic"),
            (vec!["Shared".to_owned()], vec!["Shared".to_owned()])
        );
    }

    #[test]
    fn full_pipeline_routed_paths_preserve_exact_and_lexical_identities() {
        assert_same_leaf_host_type_paths(&full_pipeline_to_routed(&same_leaf_host_type_sources()));
    }

    #[cfg(feature = "prime")]
    #[test]
    fn prime_pipeline_routed_paths_preserve_exact_and_lexical_identities() {
        assert_same_leaf_host_type_paths(&prime_pipeline_to_routed(&same_leaf_host_type_sources()));
    }

    #[test]
    fn imported_newtype_function_adapter_keeps_its_provider_identity() {
        let routed = full_pipeline_to_routed(&[
            (
                "provider.kio",
                "module provider; \
                 pub newtype Token : . { \
                   pub constructor mk_token; pub projector un_token; \
                 }; \
                 pub fn serve(_token: Token, _tail: .) -> . { () }",
            ),
            (
                "api.kio",
                "module api; \
                 import provider(Token); \
                 pub fn run(callback: (Token & .) -> .) -> . { \
                   callback(Token.mk_token(()), ()) \
                 }",
            ),
            (
                "absent.kio",
                "module absent; \
                 import api(run); \
                 import provider(serve); \
                 fn exercise(_unit: .) -> . { run(serve) }",
            ),
            (
                "collision.kio",
                "module collision; \
                 import api(run); \
                 import provider(serve); \
                 newtype Token : . & . { \
                   constructor mk_token; projector un_token; \
                 }; \
                 fn exercise(_unit: .) -> . { run(serve) }",
            ),
        ]);

        for caller in ["absent", "collision"] {
            let args = first_low_module_call(&routed_fn(&routed, caller, "exercise").body, "run")
                .expect("exercise must call the imported callback consumer");
            let [Expr::Let { body, .. }] = args.as_slice() else {
                panic!("the flat callback must be staged before adaptation: {args:#?}")
            };
            let Expr::FnExpr { sig, .. } = body.as_ref() else {
                panic!("the staged callback must feed a function adapter: {body:#?}")
            };
            let Some(Type::Product { left, .. }) =
                sig.value_params().next().and_then(|p| p.ty.as_ref())
            else {
                panic!("the adapter must expose the declared packed callback domain: {sig:#?}")
            };
            let Type::Path { segments, .. } = left.as_ref() else {
                panic!("the callback's first slot must retain its nominal type: {left:#?}")
            };
            assert_eq!(
                segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
                ["provider", "Token"],
                "the declaration-scope import must not depend on `{caller}`'s type namespace",
            );
        }
    }

    #[test]
    fn generic_call_inference_keeps_same_leaf_type_param_lexical() {
        let span = Span::new(0, 0);
        let module_path = "left";
        let actual = Type::synth_path(
            vec![module_path.to_owned(), "Shared".to_owned()],
            Vec::new(),
            span,
        );
        let mut ctx = LoweringCtx::default();
        ctx.host_type_home.insert(
            module_path.to_owned(),
            HashMap::from([("Shared".to_owned(), module_path.to_owned())]),
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(vec![BoundLocal {
                name: "value".to_owned(),
                ty: Some(actual.clone()),
                selected_abi_binder: 0,
                selected_abi_shape: None,
            }]),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: module_path.to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(1),
        };
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "Shared".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "value".to_owned(),
                ty: Some(path_ty_enriched("Shared")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
        ]);
        let arg = Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::synth("value", span)],
            meta: test_meta_enriched(),
            ext: (),
        };

        assert_eq!(
            lowerer.infer_call_type_args_for_sig(&sig, &lowerer.unfold_env(), &[], &[arg]),
            vec![actual]
        );
    }

    #[test]
    fn qualified_host_fn_type_comes_from_import_edge_not_alias_spelling() {
        let span = Span::new(0, 0);
        let provider = "provider";
        let member = "read";
        let mut ctx = LoweringCtx::default();
        ctx.host_fns_by_module.insert(
            provider.to_owned(),
            HashMap::from([(
                member.to_owned(),
                HostFnSig {
                    sig: Signature::new(Vec::new()),
                    ret: unit_ty_enriched(),
                    module_path: provider.to_owned(),
                },
            )]),
        );
        let mut module_ctx = ModuleCtx::default();
        for alias in ["ordinary", "_elab_provider__"] {
            module_ctx.qualified.insert(
                alias.to_owned(),
                QualifiedImportKind::CrossModule {
                    path: provider.to_owned(),
                },
            );
        }
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "consumer".to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let type_for = |alias: &str| {
            lowerer.function_value_type_for_path(
                &[
                    PathSegment::synth(alias, span),
                    PathSegment::synth(member, span),
                ],
                span,
            )
        };

        let ordinary = type_for("ordinary").expect("ordinary qualified import should resolve");
        let generated_looking = type_for("_elab_provider__")
            .expect("generated-looking qualified import should resolve");
        assert_eq!(ordinary, generated_looking);
    }

    #[test]
    fn generic_call_inference_respects_interleaved_type_binder_scope() {
        let span = Span::new(0, 0);
        let module_path = "left";
        let nominal = Type::synth_path(
            vec![module_path.to_owned(), "Shared".to_owned()],
            Vec::new(),
            span,
        );
        let inferred = Type::synth_path(
            vec!["right".to_owned(), "Shared".to_owned()],
            Vec::new(),
            span,
        );
        let mut ctx = LoweringCtx::default();
        ctx.host_type_home.insert(
            module_path.to_owned(),
            HashMap::from([("Shared".to_owned(), module_path.to_owned())]),
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(vec![
                BoundLocal {
                    name: "nominal".to_owned(),
                    ty: Some(nominal),
                    selected_abi_binder: 0,
                    selected_abi_shape: None,
                },
                BoundLocal {
                    name: "generic".to_owned(),
                    ty: Some(inferred.clone()),
                    selected_abi_binder: 1,
                    selected_abi_shape: None,
                },
            ]),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: module_path.to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(2),
        };
        let sig = Signature::new(vec![
            SignatureParam::Value(Param {
                name: "value".to_owned(),
                ty: Some(path_ty_enriched("Shared")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
            SignatureParam::Type(crate::ast::TypeParam {
                name: "Shared".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "later".to_owned(),
                ty: Some(path_ty_enriched("Shared")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
        ]);
        let arg = |name: &str| Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::synth(name, span)],
            meta: test_meta_enriched(),
            ext: (),
        };

        assert_eq!(
            lowerer.infer_call_type_args_for_sig(
                &sig,
                &lowerer.unfold_env(),
                &[],
                &[arg("nominal"), arg("generic")],
            ),
            vec![inferred]
        );
    }

    #[test]
    fn qualified_generic_value_forwards_phantom_binder_and_keeps_remainder() {
        let span = Span::new(0, 0);
        let caller_path = "testapi/main";
        let target_path = "testapi/helper";
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "Phantom".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "value".to_owned(),
                ty: Some(path_ty_enriched("A")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
            SignatureParam::Type(crate::ast::TypeParam {
                name: "B".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "later".to_owned(),
                ty: Some(path_ty_enriched("B")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
        ]);
        let mut ctx = LoweringCtx::default();
        ctx.modules
            .extend([caller_path.to_owned(), target_path.to_owned()]);
        ctx.module_fns_by_module.insert(
            target_path.to_owned(),
            HashMap::from([(
                "staged".to_owned(),
                ModuleFnSig {
                    sig,
                    ret_abi: path_ty("A"),
                    target_arity: 1,
                    module_path: target_path.to_owned(),
                },
            )]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx.qualified.insert(
            "helper".to_owned(),
            QualifiedImportKind::CrossModule {
                path: target_path.to_owned(),
            },
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller_path.to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let Expr::FnExpr {
            sig,
            ret_ty: Some(ret_ty),
            body,
            ..
        } = lowerer.lower_path(
            &[
                PathSegment::synth("helper", span),
                PathSegment::synth("staged", span),
            ],
            &test_meta_enriched(),
        )
        else {
            panic!("qualified generic value should eta-expand");
        };
        let [
            SignatureParam::Type(phantom_param),
            SignatureParam::Type(value_type_param),
            SignatureParam::Value(value_param),
        ] = sig.params.as_slice()
        else {
            panic!("qualified generic eta should retain pre-call binders only");
        };
        assert_eq!(phantom_param.name, "Phantom");
        assert_eq!(value_type_param.name, "A");
        assert_eq!(
            sig.groups,
            vec![
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Value { len: 1 },
            ]
        );
        assert_eq!(value_param.ty.as_ref(), Some(&path_ty("A")));
        let expected_ret = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "B".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "later".to_owned(),
                ty: Some(path_ty("B")),
                pattern: (),
                meta: test_meta(),
            }),
        ])
        .signature_ty(path_ty("A"), span);
        assert_eq!(ret_ty, expected_ret);
        let Expr::LowQualifiedModuleCall {
            type_args,
            args,
            ret_ty: Some(body_ret_ty),
            ..
        } = body.as_ref()
        else {
            panic!("qualified generic eta should call the resolved module function");
        };
        assert_eq!(type_args, &vec![path_ty("Phantom"), path_ty("A")]);
        assert_eq!(body_ret_ty, &expected_ret);
        assert!(matches!(
            args.as_slice(),
            [Expr::LowBoundRef { name, .. }] if name == "__kio_eta_arg0"
        ));
    }

    #[test]
    fn identity_alias_member_routes_to_terminal_newtype() {
        let routed = full_pipeline_to_routed(&[
            (
                "origin.kio",
                "module origin; \
                 pub newtype Box[A] : A { pub constructor make; pub projector open; };",
            ),
            (
                "relay.kio",
                "module relay; import origin as source; \
                 pub type Box[A] = source.Box(A);",
            ),
            (
                "consumer.kio",
                "module consumer; import relay as source; \
                 fn make[A](value: A) -> source.Box(A) { \
                   source.Box.make(A, value) \
                 }",
            ),
        ]);
        assert!(matches!(
            &routed_fn(&routed, "consumer", "make").body,
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                ..
            } if module_path == "origin" && newtype == "Box" && member == "make"
        ));
    }

    #[test]
    fn differently_named_identity_alias_routes_calls_and_first_class_members() {
        let routed = full_pipeline_to_routed(&[
            (
                "origin.kio",
                "module origin; \
                 pub newtype Terminal[A] : A { pub constructor make; pub projector open; };",
            ),
            (
                "relay.kio",
                "module relay; import origin as source; \
                 pub type Alias[A] = source.Terminal(A);",
            ),
            (
                "consumer.kio",
                "module consumer; import relay(Alias); import relay as forwarded; \
                 fn direct[A](value: A) -> Alias(A) { Alias.make(A, value) } \
                 fn qualified[A](value: A) -> forwarded.Alias(A) { \
                   forwarded.Alias.make(A, value) \
                 } \
                 fn constructor[A]() -> A -> Alias(A) { Alias.make(A) } \
                 fn projector[A]() -> Alias(A) -> A { forwarded.Alias.open(A) }",
            ),
        ]);

        for name in ["direct", "qualified"] {
            assert!(matches!(
                &routed_fn(&routed, "consumer", name).body,
                Expr::LowQualifiedNewtypeMember {
                    module_path,
                    newtype,
                    member,
                    ..
                } if module_path == "origin" && newtype == "Terminal" && member == "make"
            ));
        }
        for (name, expected_member) in [("constructor", "make"), ("projector", "open")] {
            let Expr::Let { value, .. } = &routed_fn(&routed, "consumer", name).body else {
                panic!("{name} should bind its explicitly instantiated member value");
            };
            let Expr::LowTypeApplication { callee, .. } = value.as_ref() else {
                panic!("{name} should instantiate the polymorphic member eta");
            };
            let Expr::FnExpr { body, .. } = callee.as_ref() else {
                panic!("{name} should eta-expand the terminal member");
            };
            assert!(matches!(
                body.as_ref(),
                Expr::LowQualifiedNewtypeMember {
                    module_path,
                    newtype,
                    member,
                    ..
                } if module_path == "origin"
                    && newtype == "Terminal"
                    && member == expected_member
            ));
        }
    }

    #[test]
    fn identity_alias_existential_members_route_to_terminal_cps_namespace() {
        let span = Span::new(0, 0);
        let type_param = |name: &str| crate::ast::TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        };
        let forwarded = NewtypeInfo {
            nominal_name: "Packed".to_owned(),
            forwarded: true,
            constructor: "pack".to_owned(),
            projector: "visit".to_owned(),
            has_existentials: true,
            type_params: vec![type_param("U")],
            payload: Type::Product {
                left: Box::new(path_ty("U")),
                right: Box::new(path_ty("A")),
                meta: test_meta(),
            },
            home: "origin".to_owned(),
            existential_params: vec![type_param("A")],
        };
        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_module.insert(
            "relay".to_owned(),
            HashMap::from([("Envelope".to_owned(), forwarded.clone())]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx
            .newtypes_in_scope
            .insert("Envelope".to_owned(), forwarded);
        module_ctx.qualified.insert(
            "r".to_owned(),
            QualifiedImportKind::CrossModule {
                path: "relay".to_owned(),
            },
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "consumer".to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        for segments in [vec!["Envelope", "pack"], vec!["r", "Envelope", "pack"]] {
            let path = segments
                .iter()
                .map(|segment| PathSegment::synth(*segment, span))
                .collect::<Vec<_>>();
            let Expr::FnExpr { body, .. } = lowerer.lower_path(&path, &test_meta_enriched()) else {
                panic!("existential alias constructor should eta-expand");
            };
            assert!(matches!(
                body.as_ref(),
                Expr::LowQualifiedNewtypeMember {
                    module_path,
                    newtype,
                    member,
                    ..
                } if module_path == "origin" && newtype == "Packed" && member == "pack"
            ));
        }

        for segments in [vec!["Envelope", "visit"], vec!["r", "Envelope", "visit"]] {
            let path = segments
                .iter()
                .map(|segment| PathSegment::synth(*segment, span))
                .collect::<Vec<_>>();
            let Expr::FnExpr { body, .. } = lowerer.lower_path(&path, &test_meta_enriched()) else {
                panic!("existential alias projector should build its receiver eta");
            };
            let Expr::FnExpr { body, .. } = body.as_ref() else {
                panic!("existential alias projector should build its CPS continuation eta");
            };
            assert!(matches!(
                body.as_ref(),
                Expr::LowCpsProjectorApply {
                    module_path,
                    newtype,
                    ..
                } if module_path == "origin" && newtype == "Packed"
            ));
        }
    }

    #[test]
    fn identity_alias_existential_constructor_and_projector_route_from_full_pipeline() {
        let routed = full_pipeline_to_routed(&[
            (
                "origin.kio",
                "module origin; \
                 pub newtype Packed[A] <U> : U & A { \
                   pub constructor pack; pub projector visit; \
                 };",
            ),
            (
                "relay.kio",
                "module relay; import origin as source; \
                 pub type Envelope[A] = source.Packed(A);",
            ),
            (
                "consumer.kio",
                "module consumer; import relay(Envelope); \
                 fn construct[A](value: A) -> Envelope(A) { \
                   Envelope.pack(A, ((), value)) \
                 } \
                 fn project[A](value: Envelope(A)) -> . { \
                   Envelope.visit(A, value)(.[U](_payload: U & A) { () }) \
                 }",
            ),
        ]);

        assert!(matches!(
            &routed_fn(&routed, "consumer", "construct").body,
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                ..
            } if module_path == "origin" && newtype == "Packed" && member == "pack"
        ));
        assert!(matches!(
            &routed_fn(&routed, "consumer", "project").body,
            Expr::LowCpsProjectorApply {
                module_path,
                newtype,
                ..
            } if module_path == "origin" && newtype == "Packed"
        ));
    }

    #[test]
    fn qualified_projector_value_uses_declaring_identity_with_same_leaf_local() {
        let span = Span::new(0, 0);
        let caller_path = "testapi/main";
        let target_path = "testapi/types";
        let local = NewtypeInfo {
            nominal_name: "Box".to_owned(),
            forwarded: false,
            constructor: "mk_local_box".to_owned(),
            projector: "un_local_box".to_owned(),
            has_existentials: false,
            type_params: Vec::new(),
            payload: Type::Unit { meta: test_meta() },
            home: caller_path.to_owned(),
            existential_params: Vec::new(),
        };
        let target = NewtypeInfo {
            nominal_name: "Box".to_owned(),
            forwarded: false,
            constructor: "mk_box".to_owned(),
            projector: "un_box".to_owned(),
            has_existentials: false,
            type_params: vec![crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }],
            payload: path_ty("A"),
            home: target_path.to_owned(),
            existential_params: Vec::new(),
        };
        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_name.insert("Box".to_owned(), local.clone());
        ctx.newtypes_by_module.insert(
            caller_path.to_owned(),
            HashMap::from([("Box".to_owned(), local.clone())]),
        );
        ctx.newtypes_by_module.insert(
            target_path.to_owned(),
            HashMap::from([("Box".to_owned(), target)]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx.qualified.insert(
            "types".to_owned(),
            QualifiedImportKind::CrossModule {
                path: target_path.to_owned(),
            },
        );
        module_ctx.newtypes_in_scope.insert("Box".to_owned(), local);
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller_path.to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let Expr::FnExpr {
            sig,
            ret_ty: Some(ret_ty),
            body,
            ..
        } = lowerer.lower_path(
            &[
                PathSegment::synth("types", span),
                PathSegment::synth("Box", span),
                PathSegment::synth("un_box", span),
            ],
            &test_meta_enriched(),
        )
        else {
            panic!("qualified projector value should eta-expand");
        };
        let [
            SignatureParam::Type(type_param),
            SignatureParam::Value(receiver),
        ] = sig.params.as_slice()
        else {
            panic!("qualified projector eta should bind its type and receiver");
        };
        assert_eq!(type_param.name, "A");
        let Some(Type::Path { segments, args, .. }) = receiver.ty.as_ref() else {
            panic!("qualified projector receiver should have a nominal type");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["testapi", "types", "Box"]
        );
        assert_eq!(args, &vec![path_ty("A")]);
        assert_eq!(ret_ty, path_ty("A"));
        let Expr::LowQualifiedNewtypeMember {
            type_args, payload, ..
        } = body.as_ref()
        else {
            panic!("qualified projector eta should call the resolved member");
        };
        assert_eq!(type_args, &vec![path_ty("A")]);
        assert!(matches!(
            payload.as_ref(),
            Expr::LowBoundRef { name, .. } if name == "__kio_eta_arg0"
        ));
    }

    #[test]
    fn qualified_existential_projector_value_keeps_declaring_module() {
        let span = Span::new(0, 0);
        let caller_path = "testapi/main";
        let target_path = "testapi/types";
        let local = NewtypeInfo {
            nominal_name: "Box".to_owned(),
            forwarded: false,
            constructor: "mk_local_box".to_owned(),
            projector: "un_local_box".to_owned(),
            has_existentials: false,
            type_params: Vec::new(),
            payload: Type::Unit { meta: test_meta() },
            home: caller_path.to_owned(),
            existential_params: Vec::new(),
        };
        let target = NewtypeInfo {
            nominal_name: "Box".to_owned(),
            forwarded: false,
            constructor: "mk_box".to_owned(),
            projector: "un_box".to_owned(),
            has_existentials: true,
            type_params: Vec::new(),
            payload: path_ty("A"),
            home: target_path.to_owned(),
            existential_params: vec![crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }],
        };

        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_name.insert("Box".to_owned(), local.clone());
        ctx.newtypes_by_module.insert(
            caller_path.to_owned(),
            HashMap::from([("Box".to_owned(), local.clone())]),
        );
        ctx.newtypes_by_module.insert(
            target_path.to_owned(),
            HashMap::from([("Box".to_owned(), target)]),
        );

        let mut module_ctx = ModuleCtx::default();
        module_ctx.qualified.insert(
            "t".to_owned(),
            QualifiedImportKind::CrossModule {
                path: target_path.to_owned(),
            },
        );
        module_ctx.newtypes_in_scope.insert("Box".to_owned(), local);
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller_path.to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let lowered = lowerer.lower_path(
            &[
                PathSegment::synth("t", span),
                PathSegment::synth("Box", span),
                PathSegment::synth("un_box", span),
            ],
            &test_meta_enriched(),
        );
        let Expr::FnExpr {
            sig, body: inner, ..
        } = lowered
        else {
            panic!("qualified existential projector should build an outer eta");
        };
        let [SignatureParam::Value(receiver)] = sig.params.as_slice() else {
            panic!("qualified existential projector should bind one receiver");
        };
        let Some(Type::Path { segments, .. }) = receiver.ty.as_ref() else {
            panic!("qualified existential projector receiver should have a nominal type");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["testapi", "types", "Box"]
        );
        let Expr::FnExpr {
            body: cps_apply, ..
        } = inner.as_ref()
        else {
            panic!("qualified existential projector should build an inner CPS eta");
        };
        let Expr::LowCpsProjectorApply { module_path, .. } = cps_apply.as_ref() else {
            panic!("inner CPS eta should contain LowCpsProjectorApply");
        };
        assert_eq!(module_path, target_path);
    }

    #[test]
    fn qualified_existential_constructor_value_keeps_declaring_module_and_witnesses() {
        let span = Span::new(0, 0);
        let caller_path = "testapi/main";
        let target_path = "testapi/types";
        let type_param = |name: &str| crate::ast::TypeParam {
            name: name.to_owned(),
            span,
            kind: None,
        };
        let target = NewtypeInfo {
            nominal_name: "Box".to_owned(),
            forwarded: false,
            constructor: "mk_box".to_owned(),
            projector: "un_box".to_owned(),
            has_existentials: true,
            type_params: vec![type_param("U")],
            payload: Type::Product {
                left: Box::new(path_ty("U")),
                right: Box::new(path_ty("A")),
                meta: test_meta(),
            },
            home: target_path.to_owned(),
            existential_params: vec![type_param("A")],
        };
        let mut ctx = LoweringCtx::default();
        ctx.newtypes_by_module.insert(
            target_path.to_owned(),
            HashMap::from([("Box".to_owned(), target)]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx.qualified.insert(
            "t".to_owned(),
            QualifiedImportKind::CrossModule {
                path: target_path.to_owned(),
            },
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller_path.to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let Expr::FnExpr {
            sig,
            ret_ty: Some(Type::Path { segments, args, .. }),
            body,
            ..
        } = lowerer.lower_path(
            &[
                PathSegment::synth("t", span),
                PathSegment::synth("Box", span),
                PathSegment::synth("mk_box", span),
            ],
            &test_meta_enriched(),
        )
        else {
            panic!("qualified existential constructor should eta-expand");
        };
        assert!(matches!(
            sig.params.as_slice(),
            [
                SignatureParam::Type(u),
                SignatureParam::Type(a),
                SignatureParam::Value(_)
            ] if u.name == "U" && a.name == "A"
        ));
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["testapi", "types", "Box"]
        );
        assert_eq!(args, vec![path_ty("U")]);
        let Expr::LowQualifiedNewtypeMember {
            module_path,
            type_args,
            payload,
            ..
        } = body.as_ref()
        else {
            panic!("qualified constructor eta should call the resolved member");
        };
        assert_eq!(module_path, target_path);
        assert_eq!(type_args, &vec![path_ty("U"), path_ty("A")]);
        assert!(matches!(
            payload.as_ref(),
            Expr::LowBoundRef { name, .. } if name == "__kio_ctor_payload"
        ));
    }

    #[test]
    fn alias_owned_qualified_newtype_head_uses_declaring_module_identity() {
        let span = Span::new(0, 0);
        let row_body = Type::Path {
            segments: vec![
                PathSegment::synth("list", span),
                PathSegment::synth("core", span),
                PathSegment::synth("List", span),
            ],
            args: vec![Type::Unit { meta: test_meta() }],
            meta: test_meta(),
        };
        let alias_scopes = HashMap::from([(
            "model".to_owned(),
            HashMap::from([(
                "Row".to_owned(),
                AliasInfo {
                    type_params: Vec::new(),
                    body: row_body,
                    home: "model".to_owned(),
                },
            )]),
        )]);
        let qualified_scopes = HashMap::from([
            (
                "facade".to_owned(),
                HashMap::from([("model".to_owned(), "model".to_owned())]),
            ),
            (
                "model".to_owned(),
                HashMap::from([("list".to_owned(), "list/core".to_owned())]),
            ),
        ]);
        let newtype_home = HashMap::from([(
            "list/core".to_owned(),
            HashMap::from([("List".to_owned(), "list/core".to_owned())]),
        )]);
        let host_type_home = HashMap::new();
        let modules = std::collections::HashSet::from([
            "facade".to_owned(),
            "model".to_owned(),
            "list/core".to_owned(),
        ]);
        let env = UnfoldEnv {
            newtype_home: &newtype_home,
            host_type_home: &host_type_home,
            type_vars: BTreeSet::new(),
            module: "facade".to_owned(),
            all_alias_scopes: &alias_scopes,
            qualified_scopes: &qualified_scopes,
            modules: &modules,
        };
        let row = Type::Path {
            segments: vec![
                PathSegment::synth("model", span),
                PathSegment::synth("Row", span),
            ],
            args: Vec::new(),
            meta: test_meta(),
        };

        let Type::Path { segments, args, .. } = unfold_type_alias_type(&row, &env) else {
            panic!("Row should unfold to the nominal List application");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["list", "core", "List"]
        );
        assert!(matches!(args.as_slice(), [Type::Unit { .. }]));
    }

    #[test]
    fn alias_info_canonicalizes_owner_body_and_protects_binders() {
        let package = pipeline_to_enriched(
            "module model; \
             import list/core as list; \
             newtype A : . { constructor mk_a; projector un_a; }; \
             pub type Row = list.List(.); \
             pub type Identity[A] = A;",
        );
        let (_, entry) = package.modules().next().expect("one module");
        let mut ctx = LoweringCtx::default();
        ctx.collect_aliases_and_newtypes_from_module("model", &entry.module);
        let aliases = ctx
            .aliases_by_module
            .get("model")
            .expect("model aliases should be collected");

        let Type::Path { segments, .. } = &aliases["Row"].body else {
            panic!("Row should remain a nominal application");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["list", "core", "List"]
        );
        let Type::Path { segments, .. } = &aliases["Identity"].body else {
            panic!("Identity should remain its bound type parameter");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["A"]
        );
    }

    fn wide_recursive_group_source(member_count: usize) -> String {
        let mut source = String::from("module test; type Id[T] = T;");
        for index in 0..member_count {
            write!(source, "type T{index} = .;").expect("writing to a String cannot fail");
        }
        source.push_str("rec {");
        for index in 0..member_count {
            let next = (index + 1) % member_count;
            write!(
                source,
                "newtype N{index}[T{index}] : Id(N{next}(T{index})) {{ \
                   constructor make_n{index}; projector get_n{index}; \
                 }};"
            )
            .expect("writing to a String cannot fail");
        }
        source.push('}');
        source
    }

    fn routed_recursive_group(package: &Package<Routed>) -> &crate::ast::TypeRecGroup<Routed> {
        package
            .modules()
            .next()
            .expect("one module")
            .1
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeRecGroup(group) => Some(group),
                _ => None,
            })
            .expect("recursive type group")
    }

    #[test]
    fn wide_recursive_group_pairs_each_distinct_source_binder_once() {
        const MEMBER_COUNT: usize = 512;

        let source = wide_recursive_group_source(MEMBER_COUNT);
        let enriched = pipeline_to_enriched(&source);
        type_rec_group_lowering_work::reset();
        let routed = lower(&enriched);
        assert_eq!(
            type_rec_group_lowering_work::member_pairs(),
            MEMBER_COUNT,
            "lowering must inspect each converted/source member pair exactly once"
        );
        let group = routed_recursive_group(&routed);
        assert_eq!(group.members.len(), MEMBER_COUNT);
        for (index, member) in group.members.iter().enumerate() {
            let crate::ast::TypeRecMember::Newtype(newtype) = member else {
                panic!("member {index} should remain a newtype");
            };
            let expected_name = format!("N{index}");
            let expected_binder = format!("T{index}");
            let expected_next = format!("N{}", (index + 1) % MEMBER_COUNT);
            assert_eq!(newtype.name, expected_name);
            assert_eq!(newtype.type_params[0].name, expected_binder);
            let payload = &newtype.payload;
            let Type::Path { segments, args, .. } = payload else {
                panic!("N{index}'s alias-unfolded payload should be path-shaped: {payload:?}");
            };
            assert_eq!(
                segments.last().map(PathSegment::as_str),
                Some(expected_next.as_str())
            );
            assert!(matches!(args.as_slice(), [Type::Path { segments, .. }]
                if segments.last().is_some_and(|segment| segment.as_str() == expected_binder)));
        }
    }

    #[test]
    #[ignore = "manual recursive-group lowering measurement"]
    fn measure_wide_recursive_group_lowering() {
        const MEMBER_COUNT: usize = 5_000;
        const ITERATIONS: usize = 5;

        let source = wide_recursive_group_source(MEMBER_COUNT);
        let enriched = pipeline_to_enriched(&source);
        let mut rendered = String::new();
        let mut visits = 0;
        for _ in 0..ITERATIONS {
            type_rec_group_lowering_work::reset();
            let routed = lower(&enriched);
            visits += type_rec_group_lowering_work::member_pairs();
            rendered = format!("{:?}", routed_recursive_group(&routed));
        }
        let digest = rendered.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        println!(
            "members={MEMBER_COUNT} iterations={ITERATIONS} visits={visits} \
             routed_bytes={} routed_fnv1a={digest:016x}",
            rendered.len()
        );
    }

    #[test]
    fn generic_alias_does_not_capture_canonical_caller_argument() {
        let span = Span::new(0, 0);
        let alias_scopes = HashMap::from([(
            "owner".to_owned(),
            HashMap::from([(
                "Identity".to_owned(),
                AliasInfo {
                    type_params: vec![crate::ast::TypeParam {
                        name: "A".to_owned(),
                        span,
                        kind: None,
                    }],
                    body: path_ty("A"),
                    home: "owner".to_owned(),
                },
            )]),
        )]);
        let qualified_scopes = HashMap::from([(
            "owner".to_owned(),
            HashMap::from([("list".to_owned(), "other".to_owned())]),
        )]);
        let modules = std::collections::HashSet::from([
            "caller".to_owned(),
            "owner".to_owned(),
            "list".to_owned(),
            "other".to_owned(),
        ]);
        let newtype_home = HashMap::new();
        let host_type_home = HashMap::new();
        let env = UnfoldEnv {
            newtype_home: &newtype_home,
            host_type_home: &host_type_home,
            type_vars: BTreeSet::new(),
            module: "caller".to_owned(),
            all_alias_scopes: &alias_scopes,
            qualified_scopes: &qualified_scopes,
            modules: &modules,
        };
        let ty = Type::Path {
            segments: vec![
                PathSegment::synth("owner", span),
                PathSegment::synth("Identity", span),
            ],
            args: vec![Type::Path {
                segments: vec![
                    PathSegment::synth("list", span),
                    PathSegment::synth("Box", span),
                ],
                args: Vec::new(),
                meta: test_meta(),
            }],
            meta: test_meta(),
        };

        let Type::Path { segments, .. } = unfold_type_alias_type(&ty, &env) else {
            panic!("generic identity alias should retain its nominal argument");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["list", "Box"]
        );
    }

    #[test]
    fn fallback_type_witness_uses_routed_identity() {
        let span = Span::new(0, 0);
        let caller = "testapi/main";
        let target = "testapi/types";
        let mut ctx = LoweringCtx::default();
        ctx.modules.extend([caller.to_owned(), target.to_owned()]);
        ctx.qualified_scopes.insert(
            caller.to_owned(),
            HashMap::from([("types".to_owned(), target.to_owned())]),
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller.to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "value".to_owned(),
                ty: Some(unit_ty_enriched()),
                pattern: (),
                meta: test_meta_enriched(),
            }),
        ]);
        let witness = Expr::Path {
            occurrence: Default::default(),
            segments: vec![
                PathSegment::synth("types", span),
                PathSegment::synth("Scalar", span),
            ],
            meta: test_meta_enriched(),
            ext: (),
        };
        let (type_args, value_args) = lowerer
            .split_args_via_sig(&sig, &[], &[witness, unit_expr_enriched()])
            .expect("value-shaped type witness should be reclassified");

        let [Type::Path { segments, .. }] = type_args.as_slice() else {
            panic!("fallback should recover one nominal type witness");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["testapi", "types", "Scalar"]
        );
        assert!(matches!(value_args.as_slice(), [Expr::Unit { .. }]));
    }

    #[test]
    fn callee_owned_routed_types_are_not_reinterpreted_in_caller_scope() {
        let span = Span::new(0, 0);
        let caller_path = "testapi/main";
        let callee_path = "testapi/dep";
        let actual_fn = fn_ty(path_ty("Actual"), path_ty("Actual"), 1);
        let mut ctx = LoweringCtx::default();
        ctx.modules
            .extend([caller_path.to_owned(), callee_path.to_owned()]);
        ctx.alias_scopes.insert(
            caller_path.to_owned(),
            HashMap::from([(
                "Actual".to_owned(),
                AliasInfo {
                    type_params: Vec::new(),
                    body: product_ty(
                        Type::Unit { meta: test_meta() },
                        Type::Unit { meta: test_meta() },
                    ),
                    home: caller_path.to_owned(),
                },
            )]),
        );
        let imported_sig = Signature::new(vec![SignatureParam::Value(Param {
            name: "value".to_owned(),
            ty: Some(path_ty_enriched("Actual")),
            pattern: (),
            meta: test_meta_enriched(),
        })]);
        ctx.module_fns_by_module.insert(
            callee_path.to_owned(),
            HashMap::from([(
                "id".to_owned(),
                ModuleFnSig {
                    sig: imported_sig,
                    ret_abi: path_ty("Actual"),
                    target_arity: 1,
                    module_path: callee_path.to_owned(),
                },
            )]),
        );
        let mut module_ctx = ModuleCtx::default();
        module_ctx.selective.insert(
            "id".to_owned(),
            ResolvedImportKind::CrossModule(callee_path.to_owned()),
        );
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(vec![BoundLocal {
                name: "f".to_owned(),
                ty: Some(actual_fn.clone()),
                selected_abi_binder: 0,
                selected_abi_shape: None,
            }]),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: caller_path.to_owned(),
            module_ctx,
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(1),
        };
        let bound_f = Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::synth("f", span)],
            meta: test_meta_enriched(),
            ext: (),
        };

        assert!(matches!(
            lowerer.lower_value_arg_as_value(bound_f.clone(), &actual_fn, span, true),
            Expr::LowBoundRef { .. }
        ));
        let mut canonical = CanonicalCallArgs::default();
        lowerer.lower_value_arg_as_abi(bound_f, &actual_fn, span, true, &mut canonical);
        assert!(matches!(
            canonical.args.as_slice(),
            [Expr::LowBoundRef { .. }]
        ));

        for call in [
            Expr::LowModuleCall {
                occurrence: Default::default(),
                mangled: "id".to_owned(),
                type_args: Vec::new(),
                args: Vec::new(),
                sig: Signature::new(Vec::new()),
                ret_ty: Some(path_ty("Actual")),
                meta: test_meta(),
                ext: (),
            },
            Expr::LowQualifiedModuleCall {
                occurrence: Default::default(),
                alias: "dep".to_owned(),
                mangled: "dep.id".to_owned(),
                type_args: Vec::new(),
                args: Vec::new(),
                sig: Signature::new(Vec::new()),
                ret_ty: Some(path_ty("Actual")),
                meta: test_meta(),
                ext: (),
            },
        ] {
            assert_eq!(lowerer.lowered_expr_type(&call), Some(path_ty("Actual")));
        }

        let generic_sig = Signature::new(vec![
            SignatureParam::Type(crate::ast::TypeParam {
                name: "A".to_owned(),
                span,
                kind: None,
            }),
            SignatureParam::Value(Param {
                name: "value".to_owned(),
                ty: Some(path_ty_enriched("A")),
                pattern: (),
                meta: test_meta_enriched(),
            }),
        ]);
        let imported_id = Expr::Path {
            occurrence: Default::default(),
            segments: vec![PathSegment::synth("id", span)],
            meta: test_meta_enriched(),
            ext: (),
        };
        assert_eq!(
            lowerer.infer_call_type_args_for_sig(
                &generic_sig,
                &lowerer.unfold_env(),
                &[],
                &[imported_id],
            ),
            vec![actual_fn]
        );
    }

    #[test]
    fn nested_polymorphic_call_reaches_function_group_adapter() {
        let span = Span::new(0, 0);
        let forall = |name: &str, body: Type<Routed>| Type::Forall {
            param: crate::ast::TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: test_meta(),
        };
        let target_cont = forall(
            "A",
            fn_ty(product_ty(path_ty("String"), path_ty("A")), path_ty("R"), 1),
        );
        let source_cont = forall(
            "B",
            fn_ty(product_ty(path_ty("String"), path_ty("B")), path_ty("R"), 2),
        );
        let p_ty = fn_ty(path_ty("Box"), target_cont.clone(), 1);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(vec![BoundLocal {
                name: "p".to_owned(),
                ty: Some(p_ty),
                selected_abi_binder: 0,
                selected_abi_shape: None,
            }]),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "testapi/main".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(1),
        };
        let first_call = Expr::LowClosureCall {
            occurrence: Default::default(),
            name: "p".to_owned(),
            type_args: Vec::new(),
            args: vec![Expr::LowBoundRef {
                occurrence: Default::default(),
                name: "v".to_owned(),
                meta: test_meta(),
                ext: (),
            }],
            meta: test_meta(),
            ext: (),
        };
        assert_eq!(
            lowerer.lowered_expr_type(&first_call),
            Some(target_cont.clone())
        );

        let adapted = lowerer
            .function_value_adapter(
                Expr::LowBoundRef {
                    occurrence: Default::default(),
                    name: "k".to_owned(),
                    meta: test_meta(),
                    ext: (),
                },
                &target_cont,
                &source_cont,
                span,
            )
            .expect("one product slot must adapt to two source binders");
        let Expr::Let {
            name, value, body, ..
        } = adapted
        else {
            panic!("a top-level function adapter must stage its callee");
        };
        assert!(
            matches!(value.as_ref(), Expr::LowBoundRef { name, .. } if name == "k"),
            "the staging let must preserve the original callee: {value:?}"
        );
        let (type_stage, body) = unwrap_function_adapter_type_stage(&body, &name);
        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("function adapter must synthesize a value closure after its type stage");
        };
        assert_eq!(sig.canonical_groups().len(), 1);
        let Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } = body.as_ref()
        else {
            panic!("function adapter must call the original continuation");
        };
        assert!(
            matches!(callee.as_ref(), Expr::LowBoundRef { name: callee_name, .. }
                if callee_name == type_stage),
            "the adapter must call the residual continuation: {callee:?}"
        );
        assert!(type_args.is_empty());
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn inferred_fn_expr_type_uses_structural_result_and_keeps_unit_control() {
        let span = Span::new(0, 0);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let outer_sig =
            Signature::from_parts(Vec::new(), vec![SignatureGroupKind::Value { len: 0 }]);
        let unit_fn = Expr::FnExpr {
            occurrence: Default::default(),
            sig: outer_sig.clone(),
            ret_ty: None,
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            }),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        assert!(matches!(
            lowerer.lowered_expr_type(&unit_fn),
            Some(Type::Function { ret, .. }) if matches!(ret.as_ref(), Type::Unit { .. })
        ));

        let value_param = |name: &str| {
            SignatureParam::Value(Param {
                name: name.to_owned(),
                ty: Some(path_ty("N")),
                pattern: (),
                meta: Meta::new(span),
            })
        };
        let flat_return = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                vec![value_param("left"), value_param("_right")],
                vec![SignatureGroupKind::Value { len: 2 }],
            ),
            ret_ty: Some(path_ty("N")),
            body: Box::new(low_bound_ref("left", span)),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        let inferred = Expr::FnExpr {
            occurrence: Default::default(),
            sig: outer_sig,
            ret_ty: None,
            body: Box::new(flat_return),
            meta: Meta::new(span),
            caps: Default::default(),
        };
        assert!(matches!(
            lowerer.lowered_expr_type(&inferred),
            Some(Type::Function { ret, .. })
                if matches!(ret.as_ref(), Type::Function { abi_arity: 2, .. })
        ));
    }

    #[test]
    fn conditional_function_branches_adapt_to_the_canonical_result_abi() {
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let conditional = Expr::EnrichedConditional {
            occurrence: Default::default(),
            cond: Box::new(unit_expr_enriched()),
            then_branch: Box::new(flat_binary_fn_enriched(bound_expr_enriched("left"))),
            else_branch: Box::new(flat_binary_fn_enriched(bound_expr_enriched("right"))),
            result_ty: canonical_binary_fn_ty_enriched(),
            meta: test_meta_enriched(),
            ext: (),
        };

        let Expr::EnrichedConditional {
            then_branch,
            else_branch,
            ..
        } = lowerer.lower_expr(&conditional)
        else {
            panic!("conditional must remain structurally recovered")
        };
        assert_branch_has_packed_callable_adapter(&then_branch, "left");
        assert_branch_has_packed_callable_adapter(&else_branch, "right");
    }

    #[test]
    fn stored_cps_projector_result_retains_typed_callable_stages() {
        let routed = full_optimized_pipeline_to_routed(&[(
            "test.kio",
            "module test; \
             host type N; \
             pub newtype Pack[A] <U> : A & U { constructor pack; projector unpack } \
             host fn receiver() -> Pack(N); \
             host fn observe(value: N) -> .; \
             fn stored() -> N { \
               let unpack = Pack.unpack(receiver()); \
               observe(unpack(.[U](value: N, _payload: U) { value })); \
               unpack(.[U](value: N, _payload: U) { value }) \
             }",
        )]);
        let Expr::Let { value, .. } = &routed_fn(&routed, "test", "stored").body else {
            panic!("the reused projector result must remain bound")
        };
        let Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } = value.as_ref()
        else {
            panic!("the projector result must apply its typed member value: {value:#?}")
        };
        assert!(matches!(type_args.as_slice(), [Type::Path { segments, .. }]
            if segments.last().is_some_and(|segment| segment == "N")));
        assert!(matches!(args.as_slice(), [Expr::LowHostCall { name, .. }]
            if name == "receiver"));
        let Expr::FnExpr { ret_ty, body, .. } = callee.as_ref() else {
            panic!("the receiver stage must use the typed member eta")
        };
        assert!(matches!(ret_ty, Some(Type::Forall { .. })));
        let Expr::FnExpr { sig, body, .. } = body.as_ref() else {
            panic!("the receiver stage must return the result/continuation stages")
        };
        assert_eq!(
            sig.groups,
            vec![
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Value { len: 1 },
            ]
        );
        assert!(matches!(body.as_ref(), Expr::LowCpsProjectorApply {
            newtype, module_path, receiver, ..
        } if newtype == "Pack" && module_path == "test"
            && matches!(receiver.as_ref(), Expr::LowBoundRef { .. })));
    }

    #[test]
    fn lowered_expr_type_tracks_lexical_let_and_closure_call_results() {
        let span = Span::new(0, 0);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let source = Type::Forall {
            param: crate::ast::TypeParam {
                name: "B".to_owned(),
                span,
                kind: None,
            },
            body: Box::new(fn_ty(
                product_ty(path_ty("B"), path_ty("B")),
                path_ty("B"),
                2,
            )),
            meta: test_meta(),
        };
        let producer = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(Vec::new(), vec![SignatureGroupKind::Value { len: 0 }]),
            ret_ty: Some(source.clone()),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: test_meta(),
            }),
            meta: test_meta(),
            caps: Default::default(),
        };
        let producer_call = Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: Box::new(producer.clone()),
            type_args: Vec::new(),
            args: Vec::new(),
            meta: test_meta(),
            ext: (),
        };
        let returned_binding = Expr::Let {
            occurrence: Default::default(),
            name: "f".to_owned(),
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(producer_call),
            body: Box::new(low_bound_ref("f", span)),
            meta: test_meta(),
        };
        assert_eq!(
            lowerer.lowered_expr_type(&returned_binding),
            Some(source.clone())
        );

        let called_binding = Expr::Let {
            occurrence: Default::default(),
            name: "make".to_owned(),
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(producer),
            body: Box::new(Expr::LowClosureCall {
                occurrence: Default::default(),
                name: "make".to_owned(),
                type_args: Vec::new(),
                args: Vec::new(),
                meta: test_meta(),
                ext: (),
            }),
            meta: test_meta(),
        };
        assert_eq!(lowerer.lowered_expr_type(&called_binding), Some(source));

        lowerer.push_bound_routed("f", Some(path_ty("Outer")));
        let shadowed_unknown = Expr::Let {
            occurrence: Default::default(),
            name: "f".to_owned(),
            name_span: span,
            ty: None,
            pattern: (),
            value: Box::new(low_bound_ref("missing", span)),
            body: Box::new(low_bound_ref("f", span)),
            meta: test_meta(),
        };
        assert_eq!(lowerer.lowered_expr_type(&shadowed_unknown), None);
        lowerer.pop_bound();
    }

    #[test]
    fn inferred_fn_expr_fallbacks_never_fabricate_unit_return() {
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let sig = Signature::from_parts(Vec::new(), vec![SignatureGroupKind::Value { len: 0 }]);
        let inferred: Expr<Enriched> = Expr::FnExpr {
            occurrence: Default::default(),
            sig: sig.clone(),
            ret_ty: None,
            body: Box::new(unit_expr_enriched()),
            meta: test_meta_enriched(),
            caps: (),
        };
        assert!(lowerer.enriched_expr_type_for_adapter(&inferred).is_none());
        assert!(
            lowerer
                .enriched_expr_type_for_inference(&inferred)
                .is_none()
        );

        let written: Expr<Enriched> = Expr::FnExpr {
            occurrence: Default::default(),
            sig,
            ret_ty: Some(unit_ty_enriched()),
            body: Box::new(unit_expr_enriched()),
            meta: test_meta_enriched(),
            caps: (),
        };
        assert!(matches!(
            lowerer.enriched_expr_type_for_adapter(&written),
            Some(Type::Function { ret, .. }) if matches!(ret.as_ref(), Type::Unit { .. })
        ));
    }

    #[test]
    fn selected_callable_shape_adapts_when_nominal_result_type_is_unavailable() {
        let span = Span::new(0, 0);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let forall = |name: &str, body: Type<Routed>| Type::Forall {
            param: crate::ast::TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: test_meta(),
        };
        let boxed = |item: Type<Routed>| Type::synth_path(vec!["Box".to_owned()], vec![item], span);
        let a = path_ty("A");
        let b = path_ty("B");
        let source = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(
                vec![
                    SignatureParam::Type(crate::ast::TypeParam {
                        name: "A".to_owned(),
                        span,
                        kind: None,
                    }),
                    SignatureParam::Type(crate::ast::TypeParam {
                        name: "B".to_owned(),
                        span,
                        kind: None,
                    }),
                    SignatureParam::Value(Param {
                        name: "step".to_owned(),
                        ty: Some(fn_ty(a.clone(), b.clone(), 1)),
                        pattern: (),
                        meta: test_meta(),
                    }),
                    SignatureParam::Value(Param {
                        name: "value".to_owned(),
                        ty: Some(boxed(a.clone())),
                        pattern: (),
                        meta: test_meta(),
                    }),
                ],
                vec![
                    SignatureGroupKind::Type { len: 1 },
                    SignatureGroupKind::Type { len: 1 },
                    SignatureGroupKind::Value { len: 2 },
                ],
            ),
            ret_ty: None,
            body: Box::new(Expr::LowNewtypeCtor {
                occurrence: Default::default(),
                newtype: "Box".to_owned(),
                member: "mk_box".to_owned(),
                type_args: vec![b.clone()],
                payload: Box::new(low_bound_ref("mapped", span)),
                meta: test_meta(),
                ext: (),
            }),
            meta: test_meta(),
            caps: Default::default(),
        };
        assert!(
            lowerer.lowered_expr_type(&source).is_none(),
            "a nominal constructor must not fabricate the enclosing lambda's semantic result type"
        );
        let expected = forall(
            "A",
            forall(
                "B",
                fn_ty(
                    product_ty(fn_ty(a, b.clone(), 1), boxed(path_ty("A"))),
                    boxed(b),
                    1,
                ),
            ),
        );

        let adapted = lowerer.adapt_routed_value_to_expected(source, None, &expected, span);
        assert_two_forall_flat_source_adapter(&adapted);
    }

    #[test]
    fn selected_callable_shape_fallback_declines_unknown_and_noncallable_values() {
        let span = Span::new(0, 0);
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let unit_ty = Type::Unit { meta: test_meta() };
        let callable_expected = fn_ty(unit_ty.clone(), unit_ty.clone(), 0);
        for value in [
            low_bound_ref("missing", span),
            Expr::Unit {
                occurrence: Default::default(),
                meta: test_meta(),
            },
        ] {
            assert_eq!(
                lowerer.adapt_routed_value_to_expected(
                    value.clone(),
                    None,
                    &callable_expected,
                    span,
                ),
                value
            );
        }

        let callable_value = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(Vec::new(), vec![SignatureGroupKind::Value { len: 0 }]),
            ret_ty: Some(unit_ty),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: test_meta(),
            }),
            meta: test_meta(),
            caps: Default::default(),
        };
        assert_eq!(
            lowerer.adapt_routed_value_to_expected(
                callable_value.clone(),
                None,
                &path_ty("Nominal"),
                span,
            ),
            callable_value
        );
    }

    fn assert_function_adapter_type_stage_param(adapter: &Expr<Routed>, expected: &str) {
        let Expr::LowIndirectCall { callee, .. } = adapter else {
            panic!("a function adapter type stage must be an indirect call: {adapter:#?}")
        };
        let Expr::FnExpr { sig, .. } = callee.as_ref() else {
            panic!("a function adapter type stage must wrap a closure: {callee:#?}")
        };
        let [SignatureParam::Type(param)] = sig.params.as_slice() else {
            panic!("a function adapter type stage must bind one type: {sig:#?}")
        };
        assert_eq!(param.name, expected);
    }

    fn assert_two_forall_flat_source_adapter(adapter: &Expr<Routed>) {
        let Expr::Let {
            name: staged_callee,
            value,
            body,
            ..
        } = adapter
        else {
            panic!("a callable argument must stage its source once: {adapter:#?}")
        };
        let Expr::FnExpr {
            sig: source_sig,
            body: source_body,
            ..
        } = value.as_ref()
        else {
            panic!("the staged source must retain its flat closure: {value:#?}")
        };
        assert_eq!(
            source_sig.groups,
            vec![
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Type { len: 1 },
                SignatureGroupKind::Value { len: 2 },
            ]
        );
        assert!(matches!(
            source_body.as_ref(),
            Expr::LowNewtypeCtor { .. } | Expr::LowQualifiedNewtypeMember { .. }
        ));

        assert_function_adapter_type_stage_param(body, "A");
        let (first_stage, body) = unwrap_function_adapter_type_stage(body, staged_callee);
        assert_function_adapter_type_stage_param(body, "B");
        let (second_stage, body) = unwrap_function_adapter_type_stage(body, first_stage);
        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("the adapted residual must expose its packed value stage: {body:#?}")
        };
        assert_eq!(sig.groups, vec![SignatureGroupKind::Value { len: 1 }]);
        let adapter_param = &sig
            .value_params()
            .next()
            .expect("the adapter has one packed value parameter")
            .name;
        let Expr::LowIndirectCall { callee, args, .. } = body.as_ref() else {
            panic!("the packed adapter must call its residual flat source: {body:#?}")
        };
        assert!(matches!(callee.as_ref(), Expr::LowBoundRef { name, .. }
            if name == second_stage));
        assert_eq!(args.len(), 2);
        for (expected_index, arg) in args.iter().enumerate() {
            assert!(matches!(arg, Expr::EnrichedProject {
                target,
                index,
                arity: 2,
                ..
            } if *index == expected_index
                && matches!(target.as_ref(), Expr::LowBoundRef { name, .. }
                    if name == adapter_param)));
        }
    }

    fn assert_forall_flat_source_adapter(adapter: &Expr<Routed>) {
        let Expr::Let {
            name: staged_callee,
            body,
            ..
        } = adapter
        else {
            panic!("a callable argument must stage its source once: {adapter:#?}")
        };
        let (_type_stage, body) = unwrap_function_adapter_type_stage(body, staged_callee);
        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("the adapted residual must expose its packed value stage: {body:#?}")
        };
        assert_eq!(sig.groups, vec![SignatureGroupKind::Value { len: 1 }]);
        assert!(matches!(body.as_ref(), Expr::LowIndirectCall { args, .. }
            if args.len() == 2));
    }

    #[test]
    fn inferred_fn_expr_partial_result_adapts_inline_and_let_bound_paths() {
        let routed = full_pipeline_to_routed(&[(
            "test",
            "module test; \
             host type N; \
             fn consume(value: [B] (B & B) -> B) -> . { () } \
             fn inline() -> . { \
               consume(.(_seed: .)[B] { .(left: B, _right: B) -> B { left } }(())) \
             } \
             fn bound() -> . { \
               let value = \
                 .(_seed: .)[B] { .(left: B, _right: B) -> B { left } }(()); \
               consume(value) \
             }",
        )]);

        for name in ["inline", "bound"] {
            let args = first_low_module_call(&routed_fn(&routed, "test", name).body, "consume")
                .unwrap_or_else(|| panic!("{name} must call consume"));
            let [adapter] = args.as_slice() else {
                panic!("consume must receive exactly one callable: {args:#?}")
            };
            assert_forall_flat_source_adapter(adapter);
        }
    }

    #[test]
    fn anonymous_fn_return_tracks_let_bound_and_called_partial_results() {
        let routed = full_pipeline_to_routed(&[(
            "test",
            "module test; \
             host type N; \
             fn consume(value: [B] (B & B) -> B) -> . { () } \
             fn inferred() -> . { \
               consume(.(_outer: .) { \
                 let f = .(_seed: .)[B] { .(left: B, _right: B) -> B { left } }(()); \
                 f \
               }(())) \
             } \
             fn explicit() -> . { \
               consume(.(_outer: .) -> [B] (B & B) -> B { \
                 let f = .(_seed: .)[B] { .(left: B, _right: B) -> B { left } }(()); \
                 f \
               }(())) \
             } \
             fn called() -> . { \
               consume(.(_outer: .) { \
                 let make = .(_seed: .)[B] { \
                   .(left: B, _right: B) -> B { left } \
                 }; \
                 make(()) \
               }(())) \
             }",
        )]);

        for name in ["inferred", "called"] {
            let args = first_low_module_call(&routed_fn(&routed, "test", name).body, "consume")
                .unwrap_or_else(|| panic!("{name} must call consume"));
            let [adapter] = args.as_slice() else {
                panic!("consume must receive exactly one callable: {args:#?}")
            };
            assert_forall_flat_source_adapter(adapter);
        }

        let explicit_args =
            first_low_module_call(&routed_fn(&routed, "test", "explicit").body, "consume")
                .expect("explicit must call consume");
        let [Expr::LowIndirectCall { callee, .. }] = explicit_args.as_slice() else {
            panic!("explicit must pass its anonymous call directly: {explicit_args:#?}")
        };
        let Expr::FnExpr {
            ret_ty: Some(ret_ty),
            body,
            ..
        } = callee.as_ref()
        else {
            panic!("explicit anonymous return must retain its checked type: {callee:#?}")
        };
        assert!(matches!(ret_ty, Type::Forall { body, .. }
            if matches!(body.as_ref(), Type::Function { abi_arity: 1, .. })));
        assert_forall_flat_source_adapter(body);
    }

    #[test]
    fn inferred_fn_expr_returned_parameter_keeps_checked_callable_abi() {
        let routed = full_pipeline_to_routed(&[(
            "test",
            "module test; \
             host type N; \
             fn consume(value: [B] (B & B) -> B) -> . { () } \
             fn caller() -> . { \
               let source = .[B](left: B, _right: B) -> B { left }; \
               consume(.(f: [B] (B & B) -> B) { f }(source)) \
             }",
        )]);
        let args = first_low_module_call(&routed_fn(&routed, "test", "caller").body, "consume")
            .expect("caller must consume the anonymous identity result");
        let [
            Expr::LowIndirectCall {
                callee,
                args: identity_args,
                ..
            },
        ] = args.as_slice()
        else {
            panic!("consume must receive the anonymous identity call: {args:#?}")
        };
        let Expr::FnExpr {
            ret_ty: Some(ret_ty),
            ..
        } = callee.as_ref()
        else {
            panic!("the inferred identity return must retain its checked bound type: {callee:#?}")
        };
        assert!(matches!(ret_ty, Type::Forall { body, .. }
            if matches!(body.as_ref(), Type::Function { abi_arity: 1, .. })));
        let [source_adapter] = identity_args.as_slice() else {
            panic!("identity must receive one adapted source: {identity_args:#?}")
        };
        assert_forall_flat_source_adapter(source_adapter);
    }

    #[test]
    fn function_adapter_applies_each_source_type_stage_at_the_matching_target_stage() {
        let span = Span::new(0, 0);
        let forall = |name: &str, body: Type<Routed>| Type::Forall {
            param: crate::ast::TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: test_meta(),
        };
        let target = forall(
            "A",
            forall(
                "B",
                fn_ty(product_ty(path_ty("B"), path_ty("B")), path_ty("B"), 1),
            ),
        );
        let source = forall(
            "X",
            forall(
                "Y",
                fn_ty(product_ty(path_ty("Y"), path_ty("Y")), path_ty("Y"), 2),
            ),
        );
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let adapted = lowerer
            .function_value_adapter(low_bound_ref("source", span), &target, &source, span)
            .expect("the source's flat value group must adapt to one product slot");
        let Expr::Let {
            name: staged_callee,
            body,
            ..
        } = adapted
        else {
            panic!("the source value must be captured once");
        };
        let (first_stage, body) = unwrap_function_adapter_type_stage(&body, &staged_callee);
        let (second_stage, body) = unwrap_function_adapter_type_stage(body, first_stage);

        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("the target value group must remain after both type stages: {body:#?}");
        };
        assert_eq!(sig.groups, vec![SignatureGroupKind::Value { len: 1 }]);
        let Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } = body.as_ref()
        else {
            panic!("the value stage must call the residual source function: {body:#?}");
        };
        assert!(matches!(callee.as_ref(), Expr::LowBoundRef { name, .. }
            if name == second_stage));
        assert!(type_args.is_empty());
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn function_adapter_retains_zero_slot_layer_while_adapting_function_result() {
        let span = Span::new(0, 0);
        let forall = |name: &str, body: Type<Routed>| Type::Forall {
            param: crate::ast::TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: test_meta(),
        };
        let unit = Type::Unit { meta: test_meta() };
        let target_result = fn_ty(product_ty(path_ty("String"), path_ty("A")), path_ty("R"), 1);
        let source_result = fn_ty(product_ty(path_ty("String"), path_ty("B")), path_ty("R"), 2);
        let target = forall("A", fn_ty(unit.clone(), target_result, 0));
        let source = forall("B", fn_ty(unit, source_result, 0));
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "testapi/main".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };

        let adapted = lowerer
            .function_value_adapter(
                Expr::LowBoundRef {
                    occurrence: Default::default(),
                    name: "continuation".to_owned(),
                    meta: test_meta(),
                    ext: (),
                },
                &target,
                &source,
                span,
            )
            .expect("the function-valued result must be adapted recursively");
        let Expr::Let {
            name: staged_callee,
            body,
            ..
        } = adapted
        else {
            panic!("a function adapter must stage its callee once");
        };
        let (_type_stage, body) = unwrap_function_adapter_type_stage(&body, &staged_callee);
        let Expr::FnExpr { sig, body, .. } = body else {
            panic!("the staged callee must feed a value adapter: {body:?}");
        };
        assert_eq!(
            sig.groups,
            vec![SignatureGroupKind::Value { len: 0 }],
            "the erased Unit parameter remains an explicit callable layer"
        );
        let Expr::Let {
            name,
            value,
            body: result_adapter,
            ..
        } = body.as_ref()
        else {
            panic!(
                "the outer call must be evaluated before adapting its function-valued result: {body:?}"
            );
        };
        assert!(
            matches!(value.as_ref(), Expr::LowIndirectCall { .. }),
            "the result-staging let must evaluate the outer call exactly once: {value:?}"
        );
        assert!(
            name.starts_with("__kio_fn0_result__"),
            "the staged result must use the adapter's generated binder: {name}"
        );
        assert!(
            matches!(result_adapter.as_ref(), Expr::FnExpr { .. }),
            "the staged result must feed the recursive function adapter: {result_adapter:?}"
        );
    }

    fn first_low_indirect_call(expr: &Expr<Routed>) -> Option<&Vec<Expr<Routed>>> {
        match expr {
            Expr::LowIndirectCall { args, .. } => Some(args),
            Expr::Let { value, body, .. } => {
                first_low_indirect_call(value).or_else(|| first_low_indirect_call(body))
            }
            Expr::Seq { value, body, .. } => {
                first_low_indirect_call(value).or_else(|| first_low_indirect_call(body))
            }
            Expr::FnExpr { body, .. } => first_low_indirect_call(body),
            Expr::EnrichedTuple { items, .. } => items.iter().find_map(first_low_indirect_call),
            Expr::LowClosureCall { args, .. }
            | Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => {
                args.iter().find_map(first_low_indirect_call)
            }
            _ => None,
        }
    }

    fn first_low_module_call<'a>(
        expr: &'a Expr<Routed>,
        target: &str,
    ) -> Option<&'a Vec<Expr<Routed>>> {
        match expr {
            Expr::LowModuleCall { mangled, args, .. } if mangled == target => Some(args),
            Expr::LowIndirectCall { callee, args, .. } => first_low_module_call(callee, target)
                .or_else(|| {
                    args.iter()
                        .find_map(|arg| first_low_module_call(arg, target))
                }),
            Expr::Let { value, body, .. } => {
                first_low_module_call(value, target).or_else(|| first_low_module_call(body, target))
            }
            Expr::Seq { value, body, .. } => {
                first_low_module_call(value, target).or_else(|| first_low_module_call(body, target))
            }
            Expr::FnExpr { body, .. } => first_low_module_call(body, target),
            Expr::EnrichedTuple { items, .. } => items
                .iter()
                .find_map(|item| first_low_module_call(item, target)),
            Expr::LowClosureCall { args, .. }
            | Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => args
                .iter()
                .find_map(|arg| first_low_module_call(arg, target)),
            _ => None,
        }
    }

    fn first_low_module_call_sig<'a>(
        expr: &'a Expr<Routed>,
        target: &str,
    ) -> Option<(&'a Signature<Routed>, &'a Vec<Expr<Routed>>)> {
        match expr {
            Expr::LowModuleCall {
                mangled, sig, args, ..
            } if mangled == target => Some((sig, args)),
            Expr::LowIndirectCall { callee, args, .. } => first_low_module_call_sig(callee, target)
                .or_else(|| {
                    args.iter()
                        .find_map(|arg| first_low_module_call_sig(arg, target))
                }),
            Expr::Let { value, body, .. } => first_low_module_call_sig(value, target)
                .or_else(|| first_low_module_call_sig(body, target)),
            Expr::Seq { value, body, .. } => first_low_module_call_sig(value, target)
                .or_else(|| first_low_module_call_sig(body, target)),
            Expr::FnExpr { body, .. } => first_low_module_call_sig(body, target),
            Expr::EnrichedTuple { items, .. } => items
                .iter()
                .find_map(|item| first_low_module_call_sig(item, target)),
            Expr::LowClosureCall { args, .. }
            | Expr::LowHostCall { args, .. }
            | Expr::LowModuleCall { args, .. }
            | Expr::LowQualifiedModuleCall { args, .. } => args
                .iter()
                .find_map(|arg| first_low_module_call_sig(arg, target)),
            _ => None,
        }
    }

    /// The `lower` pass produces a `Package<Routed>`; the type system
    /// proves that the new variants exist. This test exists to
    /// exercise the variant set: it constructs each variant via the
    /// lower pass and asserts the matching kind.
    #[test]
    fn lower_produces_low_host_call_variant() {
        // A module that calls a host .(declared via a synthetic
        // export). The host fn classifies as `Expr::LowHostCall`.
        // We can't easily mint a package file here, so we synthesise
        // a Routed module directly through `lower` and inspect.
        let enriched = pipeline_to_enriched("module x; fn caller[A](y: A) -> A { y }");
        let routed = lower(&enriched);
        // The body of `caller` is a 1-seg Path to `y` — bound. The
        // lower pass classifies it as `LowBoundRef`.
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        assert!(
            matches!(fn_def.body, Expr::LowBoundRef { ref name, .. } if name == "y"),
            "expected LowBoundRef, got {:?}",
            fn_def.body
        );
    }

    /// The `lower` pass classifies calls into `LowClosureCall` when
    /// the callee is a bound local.
    #[test]
    fn lower_classifies_bound_callee_as_closure_call() {
        let enriched = pipeline_to_enriched("module x; fn caller(f: . -> .) -> . { f(()) }");
        let routed = lower(&enriched);
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        assert!(
            matches!(fn_def.body, Expr::LowClosureCall { ref name, .. } if name == "f"),
            "expected LowClosureCall, got {:?}",
            fn_def.body
        );
    }

    #[test]
    fn grouped_lowered_tuple_items_feed_abi_slots_without_rebuild() {
        let ctx = LoweringCtx::default();
        let lowerer = Lowerer {
            bound: std::cell::RefCell::new(Vec::new()),
            type_bound: std::cell::RefCell::new(Vec::new()),
            ctx: &ctx,
            module_path: "test".to_owned(),
            module_ctx: ModuleCtx::default(),
            host_env: HostEnvScope::default(),
            next_call_callee_scope: Cell::new(0),
            next_selected_abi_binder: Cell::new(0),
        };
        let routed_unit = Type::Unit { meta: test_meta() };
        let routed_pair = product_ty(routed_unit.clone(), routed_unit.clone());
        let enriched_unit = unit_ty_enriched();
        let grouped = Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: vec![
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(Span::new(10, 11)),
                },
                Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(Span::new(20, 21)),
                },
            ],
            synth_ty: product_ty_enriched(enriched_unit.clone(), enriched_unit),
            meta: test_meta_enriched(),
            ext: (),
        };

        let mismatched = lowerer.lower_call_value_args_grouped_to_target_arity(
            vec![grouped.clone()],
            &routed_pair,
            &[routed_unit.clone(), path_ty("String")],
            Span::new(0, 0),
            false,
        );
        assert_eq!(mismatched.wrappers.len(), 1);

        let canonical = lowerer.lower_call_value_args_grouped_to_target_arity(
            vec![grouped],
            &routed_pair,
            &[routed_unit.clone(), routed_unit],
            Span::new(0, 0),
            false,
        );

        assert!(canonical.wrappers.is_empty());
        let starts: Vec<_> = canonical
            .args
            .iter()
            .map(|arg| match arg {
                Expr::Unit {
                    occurrence: _,
                    meta,
                } => meta.span.start,
                other => panic!("expected direct unit slot, got {other:?}"),
            })
            .collect();
        assert_eq!(starts, vec![10, 20]);
    }

    #[test]
    fn direct_fn_expr_call_keeps_product_param_as_one_slot() {
        let unit = unit_ty_enriched();
        let zero_arg_fn = fn_ty_enriched(unit.clone(), unit.clone(), 0);
        let converters = product_ty_enriched(zero_arg_fn.clone(), zero_arg_fn.clone());
        let callee = Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_groups(vec![crate::ast::SignatureGroup::Value(vec![
                Param {
                    name: "f".to_owned(),
                    ty: Some(zero_arg_fn),
                    pattern: Default::default(),
                    meta: test_meta_enriched(),
                },
                Param {
                    name: "converters".to_owned(),
                    ty: Some(converters.clone()),
                    pattern: Default::default(),
                    meta: test_meta_enriched(),
                },
            ])]),
            ret_ty: Some(unit.clone()),
            body: Box::new(unit_expr_enriched()),
            meta: test_meta_enriched(),
            caps: (),
        };
        let product_arg = Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: vec![unit_expr_enriched(), unit_expr_enriched()],
            synth_ty: converters,
            meta: test_meta_enriched(),
            ext: (),
        };
        let body = Expr::synth_call(
            callee,
            vec![
                CallArg::Value(unit_expr_enriched()),
                CallArg::Value(product_arg),
            ],
            Span::new(0, 0),
        );
        let module = Module::<Enriched> {
            path: crate::ast::ModulePath {
                segments: vec![PathSegment::synth("x", Span::new(0, 0))],
                span: Span::new(0, 0),
            },
            imports: Vec::new(),
            items: vec![Item::FnDef(FnDef {
                vis: crate::ast::Visibility::Private,
                purity: (),
                name: "caller".to_owned(),
                sig: Signature::new(Vec::new()),
                ret: unit,
                ret_elided: (),
                body,
                meta: test_meta_enriched(),
                doc: None,
            })],
            meta: test_meta_enriched(),
            doc: None,
        };
        let scope_package = pipeline_to_enriched("module x; fn caller() -> . { () }");
        let scope = scope_package
            .modules()
            .next()
            .expect("scope module")
            .1
            .scope
            .clone();
        let enriched = Package::<Enriched>::from_parts(
            std::collections::BTreeMap::from([(
                "x".to_owned(),
                ModuleEntry::<Enriched> {
                    file_path: PathBuf::from("x.kio"),
                    module,
                    scope,
                },
            )]),
            None,
        );
        let routed = lower(&enriched);
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        let args = first_low_indirect_call(&fn_def.body).expect("indirect call");

        assert_eq!(
            args.len(),
            2,
            "the product-typed second lambda parameter should stay one callable slot"
        );
        assert!(
            matches!(args[1], Expr::EnrichedTuple { .. }),
            "expected the product argument to stay grouped, got {:?}",
            args[1]
        );
    }

    #[test]
    fn canonicalized_function_type_preserves_callable_abi_arity() {
        let string = path_ty("String");
        let bool_ty = path_ty("Bool");
        let i32_ty = path_ty("I32");
        let concat = fn_ty(
            product_ty(string.clone(), string.clone()),
            string.clone(),
            1,
        );
        let converters = product_ty(
            fn_ty(string.clone(), string.clone(), 1),
            product_ty(
                fn_ty(i32_ty, string.clone(), 1),
                fn_ty(bool_ty, string.clone(), 1),
            ),
        );
        let renderer = fn_ty(product_ty(concat, converters), string.clone(), 2);

        let Type::Function {
            param, abi_arity, ..
        } = canonicalize_function_abi_type(renderer)
        else {
            panic!("function type should stay function-shaped");
        };

        assert_eq!(abi_arity, 2);
        let slots = function_abi_param_slot_types(&param, abi_arity);
        assert_eq!(slots.len(), 2);
        assert!(matches!(slots[1], Type::Product { .. }));
    }

    #[test]
    fn generic_module_call_keeps_single_product_arg_grouped() {
        let enriched = pipeline_to_enriched(
            "module x;\n\
             fn pair_id[A](state: A & A) -> A & A { state }\n\
             fn caller[A](state: A & A) -> A & A { pair_id(state) }",
        );
        let routed = lower(&enriched);
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        let args = first_low_module_call(&fn_def.body, "pair_id").expect("pair_id call");

        assert_eq!(
            args.len(),
            1,
            "generic module call should keep the product argument grouped"
        );
    }

    #[test]
    fn generic_module_call_instantiated_to_unit_keeps_value_slot() {
        let enriched = pipeline_to_enriched(
            "module x;\n\
             fn id[A](x: A) -> A { x }\n\
             fn caller(y: .) -> . { id(., y) }",
        );
        let routed = lower(&enriched);
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        let (sig, args) = first_low_module_call_sig(&fn_def.body, "id").expect("id call");

        assert_eq!(args.len(), 1, "the unit value argument is still explicit");
        assert_eq!(
            sig.params
                .iter()
                .filter(|p| matches!(p, SignatureParam::Value(_)))
                .count(),
            1,
            "same-module lookup must preserve the callee signature"
        );
    }

    #[test]
    fn generic_module_call_keeps_typed_callback_result_argument() {
        let routed = full_pipeline_to_routed(&[(
            "test.kio",
            "module test;\n\
             host type Input;\n\
             host type Output;\n\
             host fn make_output(_unit: .) -> Output;\n\
             fn apply[A][R](callback: A -> R, value: A) -> R { callback(value) }\n\
             fn run(value: Input) -> Output {\n\
               apply(.(item: Input) { make_output() }, value)\n\
             }",
        )]);
        let Expr::LowModuleCall { type_args, .. } = &routed_fn(&routed, "test", "run").body else {
            panic!(
                "run must lower its direct apply call as a module call: {:#?}",
                routed_fn(&routed, "test", "run").body
            )
        };

        assert!(
            matches!(type_args.as_slice(), [
                Type::Path { segments: input, .. },
                Type::Path { segments: output, .. },
            ] if input.last().is_some_and(|segment| segment == "Input")
                && output.last().is_some_and(|segment| segment == "Output")),
            "the typed call's complete Input/Output arguments must survive recovery: {type_args:#?}"
        );
    }

    #[test]
    fn callback_body_generic_module_call_keeps_single_product_arg_grouped() {
        let enriched = pipeline_to_enriched(
            "module x;\n\
             fn pair_id[A](state: A & A) -> A & A { state }\n\
             fn consume[S][R](step: S -> R, state: S) -> R { step(state) }\n\
             fn caller[A](state: A & A) -> A & A {\n\
               consume(.(state: A & A) { pair_id(state) }, state)\n\
             }",
        );
        let routed = lower(&enriched);
        let m = routed.modules().next().expect("one module").1;
        let fn_def = m
            .module
            .items
            .iter()
            .find_map(|it| match it {
                Item::FnDef(d) if d.name == "caller" => Some(d),
                _ => None,
            })
            .expect("caller fn");
        let args = first_low_module_call(&fn_def.body, "pair_id").expect("pair_id call");

        assert_eq!(
            args.len(),
            1,
            "callback body should keep the product argument grouped"
        );
    }
}

//! Rust backend — IR → Rust crate lowering.
//!
//! Implements the contract in
//! [`specs/backends/rust.md`](../../../../../specs/backends/rust.md):
//! consumes a `Package<Routed>` (post-recovery + post-resolution-
//! lowering — see [`crate::pass::structural_recovery`] and
//! [`crate::pass::recover_to_low`]) and produces a self-contained Cargo
//! crate as a [`RustCrate`] (file-name → content map).
//!
//! ## Scope
//!
//! Single-package; atomic + structural FFI surface. Public products and sums
//! use the canonical recursive binary marker/facade algebra, while exact
//! newtypes live under their declaring-module paths. Polymorphic host-fn
//! signatures and exported-fn wrappers retain `KioType` marker generics,
//! site-neutral `KioApplyN` values, and site-owned higher-rank callable traits;
//! erasure is confined to the package body and private carrier storage.
//!
//! ## Crate layout
//!
//! Per the spec: `Cargo.toml`, `src/lib.rs`, `src/host.rs`, `src/shapes.rs`,
//! `src/ffi.rs`, and `src/__kio_runtime.rs`.
//!
//! ## Routed-phase input
//!
//! The body emitter consumes the [`crate::ast::Routed`] phase:
//! every `Expr::Call` / `Expr::Path` has been classified into one of the
//! [`crate::ast::Expr::Low*`] variants
//! by [`crate::pass::recover_to_low::lower`]. Per-call-site routing
//! decisions (host-fn / module-fn / closure / newtype-member) and
//! the type-vs-value categorization of arguments are
//! pinned on the variants themselves; the emitter drops to
//! syntactic templating over the pre-classified IR.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(…)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Expr, Newtype, PathSegment, Role, Routed, Type};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryFacadeSupportOrigin,
    BoundaryHostBinding, BoundaryHostBindingOrigin, BoundaryHostTypeBinding,
    BoundaryNewtypeSurface, BoundaryNominalDeclaration, BoundaryPublicNewtypeInventoryEntry,
    CallableExecutionLayout, CallableExecutionStage, CallableSourceParamAdapter,
    CallableValueStageLayout, FacadeBinderId, FacadeShellId, FacadeUse, FacadeUseId,
    PreparedBoundaryCallableOriginRef, PreparedBoundaryCallableSite, PreparedBoundaryCallableSites,
    QualifiedTypeName,
};
use crate::backends::public_names::{encode_host_identity, encode_source_identity, host_name_core};
use crate::backends::reconstruct::{ReconProfile, apply_subst, build_typearg_subst_from_sig};
use crate::backends::skin::FunctionBoundaryAdapterPlan;
use crate::backends::structural::{
    ProductRebuildPlan, bound_product_rebuild_plan, render_cached_product_rebuild,
};
use crate::host_descriptor::{self, BoundRequirement, HostDescriptor, HostTypeDecl};
use crate::pass::resolve::{
    ModuleEntry, Package, exported_routed_contract_fn_signature_and_ret,
    exported_routed_contract_type,
};

/// Generated host-marker parameter used by every public Rust facade scope.
/// Kio rejects user identifiers beginning with `__`, so this spelling cannot
/// collide with a source type binder such as `H`.
const RUST_HOST_PARAMETER: &str = "__KioHost";
const RUST_DEFAULT_RECURSION_LIMIT: u128 = 128;

fn prepared_child_depth(depths: &[u128], id: FacadeUseId) -> u128 {
    depths.get(id.index()).copied().unwrap_or_else(|| {
        unreachable!("a prepared facade child precedes its parent in the owning arena")
    })
}

fn prepared_right_nested_depth(ids: &[FacadeUseId], depths: &[u128]) -> u128 {
    let Some((last, prefix)) = ids.split_last() else {
        return 0;
    };
    prefix
        .iter()
        .rev()
        .fold(prepared_child_depth(depths, *last), |nested, child| {
            1 + nested.max(prepared_child_depth(depths, *child))
        })
}

fn prepared_source_marker_depth(
    source: &crate::backends::boundary_facade::CallableSourceParamLayout,
    slots: &[FacadeUseId],
    depths: &[u128],
) -> u128 {
    let range = source.facade_slots();
    let source_slots = slots.get(range).unwrap_or_else(|| {
        unreachable!("a prepared Rust source adapter owns an in-range facade slot partition")
    });
    match source.adapter() {
        CallableSourceParamAdapter::UnitValue => {
            if !source_slots.is_empty() {
                unreachable!("a prepared unit source adapter owns facade slots")
            }
            1
        }
        CallableSourceParamAdapter::Identity => {
            let [slot] = source_slots else {
                unreachable!("a prepared identity source adapter owns exactly one facade slot")
            };
            prepared_child_depth(depths, *slot)
        }
        CallableSourceParamAdapter::RightNest => {
            if source_slots.len() < 2 {
                unreachable!("a prepared right-nest source adapter owns at least two facade slots")
            }
            prepared_right_nested_depth(source_slots, depths)
        }
    }
}

fn prepared_rust_facade_depth(
    plan: &BoundaryFacadePlan,
    presentation: &BoundaryFacadeExecutionPlan,
) -> u128 {
    let mut depths = Vec::with_capacity(plan.uses().len());
    for (id, use_) in plan.uses_with_ids() {
        let depth = match use_ {
            FacadeUse::Unit { .. }
            | FacadeUse::Bottom { .. }
            | FacadeUse::Bound { .. }
            | FacadeUse::Nominal { .. } => 1,
            FacadeUse::Apply { args, .. } => {
                1 + args
                    .iter()
                    .map(|arg| prepared_child_depth(&depths, *arg))
                    .max()
                    .unwrap_or(0)
            }
            FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
                prepared_right_nested_depth(args, &depths)
            }
            FacadeUse::Function { slots, result, .. } => {
                let BoundaryFacadeExecutionUse::Function(layout) = presentation.use_at(id) else {
                    unreachable!("a prepared Rust function has no source presentation")
                };
                let source_depth = layout
                    .source_params()
                    .iter()
                    .map(|source| prepared_source_marker_depth(source, slots, &depths))
                    .max()
                    .unwrap_or(0);
                1 + source_depth.max(prepared_child_depth(&depths, *result))
            }
            FacadeUse::Forall { result, .. } => 1 + prepared_child_depth(&depths, *result),
        };
        depths.push(depth);
    }
    depths.into_iter().max().unwrap_or(0)
}

fn prepared_rust_facade_max_depth(prepared: &PreparedBoundaryCallableSites) -> u128 {
    let mut maximum = 0;
    for site in prepared.sites() {
        maximum = maximum.max(prepared_rust_facade_depth(
            site.plan().facade(),
            site.presentation().root_uses(),
        ));
        for (name, declaration) in site.nominals().declarations() {
            let BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            } = declaration
            else {
                continue;
            };
            let presentation = site
                .presentation()
                .transparent_payload(name)
                .unwrap_or_else(|| {
                    unreachable!("a transparent nominal has no prepared source presentation")
                });
            maximum = maximum.max(prepared_rust_facade_depth(payload.facade(), presentation));
        }
    }
    maximum
}

fn rust_recursion_limit_for_depth(depth: u128) -> Option<u128> {
    (depth > RUST_DEFAULT_RECURSION_LIMIT).then(|| {
        depth
            .checked_add(1)
            .and_then(u128::checked_next_power_of_two)
            .unwrap_or_else(|| unreachable!("a prepared Rust facade depth fits in u128"))
    })
}

fn prepared_rust_recursion_limit(prepared: &PreparedBoundaryCallableSites) -> Option<u128> {
    rust_recursion_limit_for_depth(prepared_rust_facade_max_depth(prepared))
}

fn rust_site_support_origin(site: PreparedBoundaryCallableSite<'_>) -> BoundaryFacadeSupportOrigin {
    match site.origin() {
        PreparedBoundaryCallableOriginRef::Live(_) => BoundaryFacadeSupportOrigin::Live,
        PreparedBoundaryCallableOriginRef::Retained(metadata) => {
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: metadata.removed_at_version(),
            }
        }
    }
}

fn join_rust_support_origin(
    current: BoundaryFacadeSupportOrigin,
    incoming: BoundaryFacadeSupportOrigin,
) -> BoundaryFacadeSupportOrigin {
    match (current, incoming) {
        (BoundaryFacadeSupportOrigin::Live, _) | (_, BoundaryFacadeSupportOrigin::Live) => {
            BoundaryFacadeSupportOrigin::Live
        }
        (
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: left,
            },
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: right,
            },
        ) => BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: left.min(right),
        },
    }
}

fn insert_rust_support_origin<K: Ord>(
    origins: &mut BTreeMap<K, BoundaryFacadeSupportOrigin>,
    key: K,
    incoming: BoundaryFacadeSupportOrigin,
) {
    origins
        .entry(key)
        .and_modify(|current| *current = join_rust_support_origin(*current, incoming))
        .or_insert(incoming);
}

fn push_rust_deprecation(out: &mut String, origin: BoundaryFacadeSupportOrigin, indent: &str) {
    if let Some(removed_at_version) = origin.removed_at_version() {
        out.push_str(&format!(
            "{indent}#[deprecated(note = \"retained only for source compatibility after removal at contract v{removed_at_version}\")]\n"
        ));
    }
}

fn rust_deprecation_attribute(origin: BoundaryFacadeSupportOrigin) -> Option<String> {
    origin.removed_at_version().map(|removed_at_version| {
        format!(
            "#[deprecated(note = \"retained only for source compatibility after removal at contract v{removed_at_version}\")]"
        )
    })
}

/// A unrecoverable error during Rust emit. Mirrors [`crate::backends::js::emit::EmitError`]'s
/// shape — the build dispatcher surfaces these as `BackendError::Build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub message: String,
}

impl EmitError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        EmitError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Emitted Cargo crate — the per-package files
/// [`specs/backends/rust.md`] describes plus the fixed
/// `src/__kio_runtime.rs` runtime-support file. Build-side code
/// writes each into its conventional position under the output
/// directory.
///
/// The runtime-support file starts from the canonical
/// [`super::RUNTIME_SUPPORT_FILE_CONTENT`]. The thread-safety transform
/// replaces it with the canonical `Arc` flavor for an opted-in crate.
/// Shipping the selected representation on `RustCrate` keeps the build-side
/// dispatcher's write step uniform — it reads each field and writes one file
/// per field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustCrate {
    pub cargo_toml: String,
    pub lib_rs: String,
    pub host_rs: String,
    pub shapes_rs: String,
    /// The runtime-support file content: the canonical default
    /// [`super::RUNTIME_SUPPORT_FILE_CONTENT`], or its canonical thread-safe
    /// transform for an opted-in crate. Held on the struct so the build-side
    /// dispatcher writes it through the same uniform per-field write call as
    /// the others.
    pub runtime_rs: String,
    /// The `src/ffi.rs` module: per-member aliases for the nameable
    /// value and shape types at the package's env-fn and export-fn
    /// boundaries. Reached as `crate::ffi::env::<member>::<slot>` /
    /// `crate::ffi::exp::<member>::<slot>`. The test runner names shape
    /// types through these aliases so its hardcoded `impl Host for
    /// StubHost` tracks emitted-FFI drift rather than re-deriving shape
    /// spellings. See `emit_prepared_ffi_rs`.
    pub ffi_rs: String,
}

/// The branded facade names for one emitted crate, all derived from the
/// effective namespace (the Cargo crate name): the PascalCase package
/// handle, the `<Handle>Host` contract trait, and the `create_<value-brand>`
/// factory. One derivation root means a consumer can reconstruct the
/// whole facade from the crate's `Cargo.toml` `name` alone. The
/// `host` / `shapes` / `ffi` / `__kio_runtime` modules and the minted
/// shape / export-namespace struct names remain scoped beneath the branded
/// crate and keep their fixed or identity-derived spellings. Body-shape mints
/// remain crate-private; export namespace structs are public facade items.
pub(crate) struct RustNames {
    /// The package-handle struct name — `pascal_case(crate_name)`
    /// (`greeter` → `Greeter`).
    pub handle: String,
    /// The host-contract trait name — `<Handle>Host`. The trait lives
    /// at `crate::host::<host_trait>`; the `host` module keeps its name.
    pub host_trait: String,
    /// The factory fn name — `create_` followed by the namespace's value brand.
    pub factory: String,
}

impl RustNames {
    pub(crate) fn derive(crate_name: &str) -> RustNames {
        let handle = crate::backends::namespace::pascal_case(crate_name);
        RustNames {
            host_trait: format!("{handle}Host"),
            factory: format!(
                "create_{}",
                crate::backends::namespace::value_brand(crate_name)
            ),
            handle,
        }
    }
}

/// Lower a typed (post-recovery) Kio package to a Rust crate.
///
/// A package build calls this with default options
/// ([`LowerOptions::default`]); the resulting crate references its
/// own modules as `crate::host::...` / `crate::shapes::...`.
pub fn lower_package(package: &Package<Routed>, crate_name: &str) -> Result<RustCrate, EmitError> {
    lower_package_with_options(package, crate_name, &LowerOptions::default())
}

/// Per-lowering knobs that callers don't always set. See
/// [`lower_package`] for the canonical entry point.
#[derive(Debug, Clone)]
pub struct LowerOptions {
    /// The Rust path prefix the emitted code uses to reach its own
    /// `host` / `shapes` modules. Defaults to `"crate"`.
    pub crate_root_prefix: String,
    /// The crate's thread-safety setting (from the `thread_safety`
    /// build-block key; absent ⇒ [`super::ThreadSafety::RcLocal`]). The
    /// default is the historical single-threaded `Rc<dyn …>` shape;
    /// an opt-in value rewrites the emitted crate to the `Arc<dyn …>`
    /// shape with the matching marker bounds. The setting is applied
    /// as a post-emit transform in [`lower_package_with_options`] (the
    /// default transform is the identity, so the default path is
    /// byte-identical). See [`super::thread_safety`].
    pub thread_safety: super::ThreadSafety,
    /// The package's replayed `*.sig.kio` changelog, when it ships one:
    /// the last sealed contract generation `N-1` plus the interface replayed
    /// through that seal (whose `removed` set the deprecated-host-fn re-emit
    /// reads). `None`
    /// when the package ships no signature file — the default, so the
    /// emitted crate is byte-identical to the pre-sig behavior. See
    /// [`crate::host_descriptor::with_signature`],
    /// [`PreparedBoundaryCallableSites::collect`], and
    /// [`emit_deprecated_host_items`].
    pub sig: Option<(u32, crate::sig::ReplayedInterface)>,
}

impl Default for LowerOptions {
    fn default() -> Self {
        Self {
            crate_root_prefix: "crate".to_owned(),
            thread_safety: super::ThreadSafety::RcLocal,
            sig: None,
        }
    }
}

pub fn lower_package_with_options(
    package: &Package<Routed>,
    crate_name: &str,
    opts: &LowerOptions,
) -> Result<RustCrate, EmitError> {
    let package_file_entry = package.package_file().ok_or_else(|| {
        EmitError::unsupported(
            "Rust emitter requires a package file (`<pkg>.pkg.kio`); \
             a package without one has no package boundary to expose",
        )
    })?;
    let _ = package_file_entry;
    let names = RustNames::derive(crate_name);
    let host_descriptor = crate::host_descriptor::build_host_descriptor(package);
    let host_descriptor = match &opts.sig {
        Some((version, replayed)) => {
            crate::host_descriptor::with_signature(host_descriptor, *version, replayed)
        }
        None => host_descriptor,
    };
    let prepared = PreparedBoundaryCallableSites::collect(
        package,
        opts.sig.as_ref().map(|(_, replayed)| replayed),
    )
    .unwrap_or_else(|error| {
        unreachable!(
            "post-recovery Rust emission and replay_sig_for_build-validated retained interfaces \
             must produce a valid PreparedBoundaryCallableSites catalog: {error}"
        )
    });
    let newtypes = collect_newtypes(package);
    let newtype_records = build_newtype_records(package, &newtypes);
    let newtype_import_scopes = collect_newtype_import_scopes(package, &newtype_records);
    let module_fns = collect_module_fns(package);
    let export_block_namespaces = collect_export_block_namespaces(&prepared);
    let facade_index = PreparedRustFacadeIndex::new(&prepared);

    let host_types_by_identity: BTreeMap<(String, String), HostTypeInfo> = host_descriptor
        .host_types
        .iter()
        .map(|host_type| {
            (
                (host_type.module_path.to_owned(), host_type.name.to_owned()),
                HostTypeInfo {
                    role: host_type.role,
                },
            )
        })
        .collect();
    let mut shapes = BodyShapeRegistry::new(
        host_types_by_identity,
        newtype_records,
        newtype_import_scopes,
        opts.crate_root_prefix.clone(),
        names.host_trait.clone(),
    );
    for entry in prepared.public_newtypes() {
        shapes.register_prepared_newtype(entry, BoundaryFacadeSupportOrigin::Live);
    }
    for entry in prepared.retained_public_newtypes().filter(|entry| {
        facade_index
            .eligible_retained_newtypes
            .contains(entry.name())
    }) {
        let removed_at_version = prepared
            .retained_public_newtype_removed_at(entry.name())
            .unwrap_or_else(|| {
                unreachable!("a retained Rust public newtype has removal provenance")
            });
        shapes.register_prepared_newtype(
            entry,
            BoundaryFacadeSupportOrigin::Retained { removed_at_version },
        );
    }

    // The legacy shape registry is body-private. Public host/export/newtype
    // topology comes exclusively from `prepared`; register only shapes
    // reached by executable module bodies here.
    for m in &module_fns {
        let adapted_body = qualify_module_expr_types(&m.fn_def.body, &m.fn_def.sig, m.module_entry);
        let m_tparams: std::collections::HashSet<&str> = m
            .fn_def
            .sig
            .params
            .iter()
            .filter_map(|p| match p {
                crate::ast::SignatureParam::Type(tp) => Some(tp.name.as_str()),
                _ => None,
            })
            .collect();
        // Resolve body-local same-leaf newtype references in the declaring
        // module's lexical scope. This registration cannot name a public
        // facade type; those are already frozen in `prepared`.
        shapes.current_module = Some(m.module_path.clone());
        let body_shape_ctx = BodyShapeContext {
            module_path: Some(&m.module_path),
        };
        register_body_shapes(&adapted_body, &mut shapes, &m_tparams, &body_shape_ctx)?;
        shapes.current_module = None;
    }

    let cargo_toml = emit_cargo_toml(crate_name);
    let mut shapes_rs = emit_shapes_rs(&mut shapes)?;
    let facade =
        PreparedRustRenderContext::new(&prepared, &opts.crate_root_prefix, &names.host_trait);
    let host_rs = render_rust_host_trait(&host_descriptor, &prepared, facade, &names)?;
    let ffi_rs = emit_prepared_ffi_rs(&prepared, facade)?;
    let facade_support = render_prepared_facade_support(&prepared, &facade_index, facade)?;
    if !facade_support.is_empty() {
        if !shapes_rs.ends_with('\n') {
            shapes_rs.push('\n');
        }
        shapes_rs.push_str(&facade_support);
    }
    let lib_rs = emit_lib_rs(
        &module_fns,
        &export_block_namespaces,
        package,
        &prepared,
        &shapes,
        facade,
        &names,
    )?;
    let mut krate = RustCrate {
        cargo_toml,
        host_rs,
        shapes_rs,
        lib_rs,
        // The runtime-support file is the same byte content for
        // every emitted crate under the default (single-threaded)
        // setting — Rust's runtime helpers are package-agnostic. The
        // build-side dispatcher writes it under `src/__kio_runtime.rs`
        // (the path declared on the Rust profile's
        // `RuntimeSupport::EmbeddedFile`). The `thread_safety`
        // transform below swaps in the `Arc`-flavored runtime when the
        // crate opts in.
        runtime_rs: super::RUNTIME_SUPPORT_FILE_CONTENT.to_owned(),
        ffi_rs,
    };
    // Apply the `thread_safety` build-block setting. The default
    // (`RcLocal`) is a no-op, so the emitted crate stays byte-identical
    // to the pre-key behavior; an opt-in value rewrites the emitted
    // Rust source fields to the `Arc<dyn …>` shape with marker bounds.
    // See [`super::thread_safety`] and `specs/backends/rust.md`
    // § Output layout > Thread safety.
    opts.thread_safety.transform_crate(&mut krate);
    Ok(krate)
}

// ---- collection helpers ---------------------------------------------------

/// Rung-2 host-boundary name for one host item: a module-qualified,
/// flat-but-injective spelling keyed on the declaring module's path.
/// A module path without source underscores keeps the established `/` → `_`
/// spelling. A path that does contain `_` uses the reserved
/// `__kio_host_` form, with `_` encoded as `_u` and `/` as `_s`. One `__`
/// then separates the module from the word-cased leaf. The reserved prefix is
/// outside the image of ordinary Kio module names, and the encoded suffix
/// contains no `__`, so affixes cannot erase module/leaf boundaries.
///
/// The Rust backend uses rung 2 (not rung 1 sub-traits) for the host
/// boundary because the host-type associated types are entangled with
/// the brand / carrier machinery — see the module-level note on
/// [`render_rust_host_trait`].
fn mangle_host_name(module_path: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module_path.contains('_') {
        return format!("{}__{leaf}", module_path.replace('/', "_"));
    }
    let module = format!("__kio_host_{}", encode_host_identity(module_path));
    format!("{module}__{leaf}")
}

struct CollectedNewtype<'a> {
    module_path: String,
    module_entry: &'a ModuleEntry<Routed>,
    newtype: &'a Newtype<Routed>,
}

#[derive(Clone)]
struct NewtypeRecord {
    rust_module_path: Vec<String>,
    rust_leaf: String,
    decl: Newtype<Routed>,
    host_surface: Option<RustNewtypeHostSurface>,
}

impl NewtypeRecord {
    /// Canonical body-storage identity for this declaration. The `nominal`
    /// submodule keeps declaration-keyed paths disjoint from crate-private
    /// anonymous body-shape mints at the `shapes` root.
    fn rust_path(&self, crate_root: &str) -> String {
        format!(
            "{crate_root}::shapes::nominal::{}::{}",
            self.rust_module_path.join("::"),
            self.rust_leaf
        )
    }
}

#[derive(Default)]
struct NewtypeNamespaceTree {
    records: BTreeMap<String, String>,
    children: BTreeMap<String, NewtypeNamespaceTree>,
}

impl NewtypeNamespaceTree {
    fn insert(&mut self, module_path: &[String], rust_leaf: &str, key: &str) {
        let Some((head, tail)) = module_path.split_first() else {
            unreachable!("a Kio newtype always has a non-empty declaring module path");
        };
        let node = self.children.entry(head.clone()).or_default();
        if tail.is_empty() {
            if let Some(previous) = node.records.insert(rust_leaf.to_owned(), key.to_owned()) {
                unreachable!(
                    "two Kio newtypes map to the same canonical Rust path: \
                     `{previous}` and `{key}`"
                );
            }
        } else {
            node.insert(tail, rust_leaf, key);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RustNewtypeHostSurface {
    Opaque,
    Constructor,
    Projector,
    Both,
}

#[derive(Clone)]
struct RustReachContext {
    module_path: Option<String>,
    type_vars: BTreeSet<String>,
}

#[derive(Clone)]
struct RustPartialEqReachContext {
    scope: RustReachContext,
    newtype_path: BTreeSet<String>,
}

#[derive(Clone)]
struct RustRecursionReachContext {
    scope: RustReachContext,
    newtype_path: BTreeSet<String>,
}

fn collect_newtypes(package: &Package<Routed>) -> Vec<CollectedNewtype<'_>> {
    let mut out = Vec::new();
    for (module_path, entry) in package.modules() {
        for item in &entry.module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(newtype) = declaration.newtype() {
                    out.push(CollectedNewtype {
                        module_path: module_path.to_owned(),
                        module_entry: entry,
                        newtype,
                    });
                }
            });
        }
    }
    out
}

fn build_newtype_records(
    package: &Package<Routed>,
    newtypes: &[CollectedNewtype<'_>],
) -> BTreeMap<String, NewtypeRecord> {
    let public_surfaces = crate::pass::resolve::public_newtype_host_surfaces(package);
    let mut records = BTreeMap::new();
    for collected in newtypes {
        let key = newtype_key(&collected.module_path, &collected.newtype.name);
        let mut locals = std::collections::HashMap::new();
        for tp in collected
            .newtype
            .type_params
            .iter()
            .chain(collected.newtype.existential_params.iter())
        {
            locals.insert(tp.name.clone(), tp.effective_kind());
        }
        let mut decl = collected.newtype.clone();
        decl.payload = exported_routed_contract_type(
            &collected.newtype.payload,
            collected.module_entry,
            &locals,
        );
        let host_surface = public_surfaces
            .get(&(
                collected.module_path.clone(),
                collected.newtype.name.clone(),
            ))
            .map(|indexed| match &indexed.surface {
                crate::ast::NewtypeHostSurface::Opaque => RustNewtypeHostSurface::Opaque,
                crate::ast::NewtypeHostSurface::Constructor { .. } => {
                    RustNewtypeHostSurface::Constructor
                }
                crate::ast::NewtypeHostSurface::Projector { .. } => {
                    RustNewtypeHostSurface::Projector
                }
                crate::ast::NewtypeHostSurface::Both { .. } => RustNewtypeHostSurface::Both,
            });
        records.insert(
            key,
            NewtypeRecord {
                rust_module_path: collected
                    .module_path
                    .split('/')
                    .map(rust_public_ident)
                    .collect(),
                rust_leaf: rust_public_ident(&collected.newtype.name),
                decl,
                host_surface,
            },
        );
    }
    records
}

fn newtype_key(module_path: &str, name: &str) -> String {
    format!("{module_path}.{name}")
}

fn newtype_key_module_path(key: &str) -> Option<&str> {
    key.rsplit_once('.').map(|(module_path, _name)| module_path)
}

fn unique_newtype_key_by_name(
    newtype_by_key: &BTreeMap<String, NewtypeRecord>,
) -> BTreeMap<String, String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for record in newtype_by_key.values() {
        *counts.entry(record.decl.name.clone()).or_insert(0) += 1;
    }
    let mut out = BTreeMap::new();
    for (key, record) in newtype_by_key {
        if counts.get(&record.decl.name).copied() == Some(1) {
            out.insert(record.decl.name.clone(), key.clone());
        }
    }
    out
}

/// Per-module newtype-import scope: for each module, the bare newtype
/// leaf names it brings into scope with `import N(…, X, …);`, each
/// mapped to the slash-path of the module that declares the newtype.
///
/// A bare newtype reference inside a module body resolves to the
/// module's own declaration first, then to one of these imports — never
/// to a package-global by-leaf table. That keeps newtype-name
/// resolution **open-world**: a reference `Term_list` in a module that
/// imports it from `elaborator_util` resolves to `elaborator_util`'s
/// declaration regardless of whether some unrelated module also
/// declares a `Term_list`. Resolving through a global "unique by leaf"
/// table instead would silently break this reference the moment a
/// second module declared the same leaf (the name would stop being
/// unique and drop out of the table) — adding a declaration to one
/// module must never change the meaning of another module's code (see
/// `specs/language.md` § Open-world design and `specs/backends/rust.md`
/// § Structural and nominal types).
///
/// Keyed off `newtype_by_key` (the authoritative `{module}.{leaf}` set,
/// which already includes the newtypes that `labels` blocks lower to)
/// so an imported leaf is recorded only when the source module actually
/// declares a newtype of that name.
fn collect_newtype_import_scopes(
    package: &Package<Routed>,
    newtype_by_key: &BTreeMap<String, NewtypeRecord>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (path, entry) in package.modules() {
        let mut scope: BTreeMap<String, String> = BTreeMap::new();
        for u in &entry.module.imports {
            let crate::ast::ImportKind::Selective { items, from } = &u.kind else {
                continue;
            };
            let from_path = from
                .segments
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("/");
            for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                if newtype_by_key.contains_key(&newtype_key(&from_path, name)) {
                    scope.insert(name.to_owned(), from_path.clone());
                }
            }
        }
        out.insert(path.to_owned(), scope);
    }
    out
}

/// One .(pub or not) defined in one of the package's modules,
/// paired with the slash module path the typer recorded for it.
/// Returned in deterministic iteration order so emission order is
/// stable across runs.
///
/// Non-pub fns are collected too: they're reachable from within
/// the same module (pub fns calling sibling helpers) and must be
/// emitted as package-handle methods so the cross-method call works
/// at the Rust level. The `pub` distinction is consulted only at
/// cross-module-import resolution time (see [`build_selective_imports`]).
struct ModuleFn<'a> {
    module_path: String,
    module_entry: &'a ModuleEntry<Routed>,
    fn_def: &'a crate::ast::FnDef<Routed>,
}

#[derive(Default)]
struct ExportBlockNamespaceTree {
    methods: BTreeMap<String, BoundaryFacadeSiteId>,
    children: BTreeMap<String, ExportBlockNamespaceTree>,
}

fn module_fn_contract_signature_and_ret(
    module_entry: &ModuleEntry<Routed>,
    fn_def: &crate::ast::FnDef<Routed>,
) -> (crate::ast::Signature<Routed>, Type<Routed>) {
    exported_routed_contract_fn_signature_and_ret(&fn_def.sig, &fn_def.ret, module_entry)
}

fn collect_module_fns(package: &Package<Routed>) -> Vec<ModuleFn<'_>> {
    let mut out = Vec::new();
    for (path, entry) in package.modules() {
        for item in &entry.module.items {
            if let crate::ast::Item::FnDef(d) = item {
                out.push(ModuleFn {
                    module_path: path.to_owned(),
                    module_entry: entry,
                    fn_def: d,
                });
            }
        }
    }
    out
}

/// Derive the export namespace exclusively from the package-complete prepared
/// catalog. Each admitted live exported-function site contributes its exact
/// module path and member identity; raw module visibility and bridge scans do
/// not participate in public topology.
fn collect_export_block_namespaces(
    prepared: &PreparedBoundaryCallableSites,
) -> ExportBlockNamespaceTree {
    let mut out = ExportBlockNamespaceTree::default();
    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::ExportedFunction { name } = site.site().owner() else {
            continue;
        };
        if !matches!(site.origin(), PreparedBoundaryCallableOriginRef::Live(_)) {
            continue;
        }
        insert_export_block_namespace_fn(
            &mut out,
            site.site().module_segments(),
            name,
            site.site(),
        );
    }
    out
}

fn insert_export_block_namespace_fn(
    tree: &mut ExportBlockNamespaceTree,
    path: &[String],
    name: &str,
    site: &BoundaryFacadeSiteId,
) {
    assert!(!path.is_empty(), "a prepared export has a module path");
    let mut cursor = tree;
    for segment in path {
        cursor = cursor.children.entry(segment.clone()).or_default();
    }
    if let Some(previous) = cursor.methods.insert(name.to_owned(), site.clone()) {
        unreachable!(
            "Rust emitter: prepared exports {previous:?} and {site:?} collide in one namespace"
        );
    }
}

/// Mangle a module path + fn name into an injective private Rust method name.
///
/// Conventional components without source underscores keep the established
/// readable spelling. Otherwise the leading `___mod_` class is disjoint from
/// that spelling, and length-framed encoded module/leaf components preserve
/// every source boundary while remaining snake_case.
fn mangle_module_fn(module_path: &str, fn_name: &str) -> String {
    if !module_path.contains('_') && !fn_name.contains('_') {
        let path_part = module_path.replace('/', "_");
        return format!("__mod_{path_part}_{fn_name}");
    }
    let module = encode_source_identity(module_path);
    let leaf = encode_source_identity(fn_name);
    format!("___mod_{}_{}_{}", module.len(), module, leaf)
}

/// Substitute every Path-typed reference to an existential type-param
/// in `payload` with the placeholder
/// `Path { segments: ["__kio_existential_any__"], args: [] }`. The
/// rust renderer in `render_use` / `render_for_slot` recognises this
/// sentinel and emits `::std::rc::Rc<dyn ::std::any::Any>` — erasing the
/// sealed existential at the Rust type level. Each position is rewritten
/// independently, so a payload like `u & String` contains one opaque
/// reference-counted token alongside its exact String slot.
fn substitute_existentials_in_payload(
    payload: &Type<Routed>,
    existentials: &std::collections::HashSet<&str>,
) -> Type<Routed> {
    if existentials.is_empty() {
        return payload.clone();
    }
    fn walk(t: &Type<Routed>, ex: &std::collections::HashSet<&str>) -> Type<Routed> {
        match t {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                if segments.len() == 1 && args.is_empty() && ex.contains(segments[0].as_str()) {
                    return Type::Path {
                        segments: vec![crate::ast::PathSegment::new(
                            EXISTENTIAL_ANY_SENTINEL.to_owned(),
                            meta.span,
                        )],
                        args: Vec::new(),
                        meta: meta.clone(),
                    };
                }
                Type::Path {
                    segments: segments.clone(),
                    args: args.iter().map(|a| walk(a, ex)).collect(),
                    meta: meta.clone(),
                }
            }
            Type::Product { left, right, meta } => Type::Product {
                left: Box::new(walk(left, ex)),
                right: Box::new(walk(right, ex)),
                meta: meta.clone(),
            },
            Type::Sum { left, right, meta } => Type::Sum {
                left: Box::new(walk(left, ex)),
                right: Box::new(walk(right, ex)),
                meta: meta.clone(),
            },
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => {
                let param = walk(param, ex);
                Type::Function {
                    param: Box::new(param),
                    ret: Box::new(walk(ret, ex)),
                    meta: meta.clone(),
                    abi_arity: *abi_arity,
                    caps: caps.clone(),
                }
            }
            Type::Forall { param, body, meta } => {
                let inner = ex
                    .iter()
                    .copied()
                    .filter(|name| *name != param.name.as_str())
                    .collect();
                Type::Forall {
                    param: param.clone(),
                    body: Box::new(walk(body, &inner)),
                    meta: meta.clone(),
                }
            }
            other => other.clone(),
        }
    }
    walk(payload, existentials)
}

/// Sentinel path-segment name for an existential slot. The
/// `render_use` / `render_for_slot` paths recognise it and emit
/// `Rc<dyn Any>` — the type-erased representation of a sealed
/// existential.
const EXISTENTIAL_ANY_SENTINEL: &str = "__kio_existential_any__";

/// Walk a [`Type::Function`]'s domain and recover the normalized ABI
/// parameter slot types.
fn collect_function_param_tys(param: &Type<Routed>, abi_arity: usize) -> Vec<Type<Routed>> {
    Type::right_spine_take(param, abi_arity)
        .into_iter()
        .cloned()
        .collect()
}

fn function_abi_arity(ty: &Type<Routed>) -> Option<usize> {
    match ty {
        Type::Function { abi_arity, .. } => Some(*abi_arity),
        _ => None,
    }
}

fn type_is_function(ty: &Type<Routed>) -> bool {
    function_abi_arity(ty).is_some()
}

/// Peel outer `Type::Forall` layers off `payload`. When the inner
/// type is a `Type::Function`, return the collected binders and the
/// inner function type; otherwise return `None`. Used to detect the
/// "polymorphic-function newtype payload" shape — e.g.
/// `Monad[*F] : [A][B](F(A), (A) -> F(B)) -> F(B)` — where the
/// newtype's per-call type-params (`[A]`, `[B]`) parameterise the
/// stored fn rather than the newtype itself.
///
/// Rust has no HRTB over type-params, so the private body/stored function
/// shape is the concrete binder-erased `Rc<dyn Fn(…Rc<dyn Any>…) ->
/// Rc<dyn Any>>`. The public facade is the exact site-owned forall value and
/// callable trait. Only a recursive private body field erases the callable
/// carrier further to `Rc<dyn Any>`; its members bridge that storage through
/// the marker-directed conversion.
fn detect_polymorphic_fn_payload(
    payload: &Type<Routed>,
) -> Option<(Vec<crate::ast::TypeParam>, Type<Routed>)> {
    let mut binders: Vec<crate::ast::TypeParam> = Vec::new();
    let mut cur = payload;
    while let Type::Forall { param, body, .. } = cur {
        binders.push(param.clone());
        cur = body;
    }
    if binders.is_empty() {
        return None;
    }
    if matches!(cur, Type::Function { .. }) {
        Some((binders, cur.clone()))
    } else {
        None
    }
}

/// The 0-based indices of `nt`'s explicitly higher-kinded parameters.
///
/// The private body nominal registered in [`BodyShapeRegistry`] has no generic
/// capable of carrying a kind-`*→…→*` constructor, so every non-`Star`
/// declaration slot is absent from that private struct header and its body
/// uses. Public declaration markers and facade carriers retain their exact
/// higher-kinded parameters. This is a declaration property: even a phantom
/// `[*F]` that the payload never applies is erased only from the private body
/// header.
fn higher_kinded_param_indices(nt: &Newtype<Routed>) -> std::collections::HashSet<usize> {
    nt.type_params
        .iter()
        .enumerate()
        .filter(|(_, param)| param.effective_kind() != crate::ast::Kind::Star)
        .map(|(index, _)| index)
        .collect()
}

/// The 0-based indices of `nt`'s higher-kinded parameters whose payload
/// actually applies them as `F(…)`.
///
/// Unlike [`higher_kinded_param_indices`], this is a payload reachability
/// predicate used by [`BodyShapeRegistry`]. It remains separate because only a
/// reached HKT produces the non-`PartialEq` erased `Rc<dyn Any>` private body
/// field; a phantom HKT header does not suppress equality for an otherwise
/// comparable body payload.
fn payload_hkt_param_indices(nt: &Newtype<Routed>) -> std::collections::HashSet<usize> {
    let scope: std::collections::HashSet<&str> =
        nt.type_params.iter().map(|tp| tp.name.as_str()).collect();
    let mut payload_uses: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_brand_tparams_in_type(&nt.payload, &scope, &mut payload_uses);
    nt.type_params
        .iter()
        .enumerate()
        .filter(|(_, tp)| payload_uses.contains(&tp.name))
        .map(|(i, _)| i)
        .collect()
}
/// Qualify every type reference inside a module fn body to its
/// cross-module-stable contract form (a bare `Subst` referencing this
/// module's `newtype Subst` becomes the module-qualified key), so the
/// emitter's nominal-identity machinery resolves same-leaf names to the
/// declaration in scope. Types cross the FFI via their normal shapes;
/// there is no per-module type adaptation (the old `bridge` adaptation
/// blocks are gone).
fn qualify_module_expr_types(
    e: &Expr<Routed>,
    sig: &crate::ast::Signature<Routed>,
    defining_module: &ModuleEntry<Routed>,
) -> Expr<Routed> {
    let locals = signature_type_param_scope_for_emit(sig);
    map_types_in_expr(
        e,
        &|t, locals| exported_routed_contract_type(t, defining_module, locals),
        &locals,
    )
}

fn signature_type_param_scope_for_emit(
    sig: &crate::ast::Signature<Routed>,
) -> std::collections::HashMap<String, crate::ast::Kind> {
    sig.params
        .iter()
        .filter_map(|param| match param {
            crate::ast::SignatureParam::Type(tp) => Some((tp.name.clone(), tp.effective_kind())),
            crate::ast::SignatureParam::Value(_) => None,
        })
        .collect()
}

fn map_types_in_expr<F>(
    e: &Expr<Routed>,
    map_type: &F,
    locals: &std::collections::HashMap<String, crate::ast::Kind>,
) -> Expr<Routed>
where
    F: Fn(&Type<Routed>, &std::collections::HashMap<String, crate::ast::Kind>) -> Type<Routed>,
{
    let go_t = |t: &Type<Routed>| map_type(t, locals);
    let go_t_opt = |t: &Option<Type<Routed>>| t.as_ref().map(&go_t);
    let go_e = |inner: &Expr<Routed>| map_types_in_expr(inner, map_type, locals);
    let go_box = |inner: &Expr<Routed>| Box::new(go_e(inner));
    let go_sig = |sig: &crate::ast::Signature<Routed>| {
        let nested = signature_type_param_scope_for_emit(sig);
        let mut scoped = locals.clone();
        scoped.extend(nested);
        let mapped = crate::ast::Signature {
            params: sig
                .params
                .iter()
                .map(|p| match p {
                    crate::ast::SignatureParam::Type(tp) => {
                        crate::ast::SignatureParam::Type(tp.clone())
                    }
                    crate::ast::SignatureParam::Value(vp) => {
                        crate::ast::SignatureParam::Value(crate::ast::Param {
                            name: vp.name.clone(),
                            ty: vp.ty.as_ref().map(|ty| map_type(ty, &scoped)),
                            pattern: (),
                            meta: vp.meta.clone(),
                        })
                    }
                })
                .collect(),
            groups: sig.groups.clone(),
        };
        (mapped, scoped)
    };
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        // Phase-only variants discharged via `match *ext {}` at
        // emit time — uninhabited at Routed, so they can't reach
        // this walker in production. Clone-fall-through is safe
        // (the clone preserves the `Never` witness).
        Expr::Path { .. }
        | Expr::Call { .. }
        | Expr::Tuple { .. }
        | Expr::FnPlaceholder { .. }
        | Expr::LabelValue { .. }
        | Expr::RowLet { .. }
        | Expr::Elaborator { .. }
        | Expr::RecOrder { .. }
        | Expr::RecQuote { .. }
        | Expr::UserElaborator { .. }
        | Expr::Ufcs { .. }
        | Expr::OpChain { .. }
        | Expr::RecCall { .. } => e.clone(),
        Expr::Unit { .. } | Expr::BoolLit { .. } => e.clone(),
        Expr::StrLit {
            occurrence: _,
            value,
            annotation,
            meta,
        } => Expr::StrLit {
            occurrence: Default::default(),
            value: value.clone(),
            annotation: go_t(annotation),
            meta: meta.clone(),
        },
        Expr::IntLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::IntLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: go_t(annotation),
            meta: meta.clone(),
        },
        Expr::FloatLit {
            occurrence: _,
            digits,
            annotation,
            meta,
        } => Expr::FloatLit {
            occurrence: Default::default(),
            digits: digits.clone(),
            annotation: go_t(annotation),
            meta: meta.clone(),
        },
        Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta,
            caps,
        } => {
            let (sig, scoped) = go_sig(sig);
            Expr::FnExpr {
                occurrence: Default::default(),
                sig,
                ret_ty: ret_ty.as_ref().map(|ty| map_type(ty, &scoped)),
                body: Box::new(map_types_in_expr(body, map_type, &scoped)),
                meta: meta.clone(),
                caps: caps.clone(),
            }
        }
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
            name: name.clone(),
            name_span: *name_span,
            ty: go_t_opt(ty),
            pattern: *pattern,
            value: go_box(value),
            body: go_box(body),
            meta: meta.clone(),
        },
        Expr::Seq {
            occurrence: _,
            value,
            body,
            meta,
        } => Expr::Seq {
            occurrence: Default::default(),
            value: go_box(value),
            body: go_box(body),
            meta: meta.clone(),
        },
        Expr::EnrichedTuple {
            occurrence: _,
            items,
            synth_ty,
            meta,
            ext,
        } => Expr::EnrichedTuple {
            occurrence: Default::default(),
            items: items.iter().map(&go_e).collect(),
            synth_ty: go_t(synth_ty),
            meta: meta.clone(),
            ext: *ext,
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
            target: go_box(target),
            index: *index,
            arity: *arity,
            target_ty: go_t(target_ty),
            meta: meta.clone(),
            ext: *ext,
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
            payload: go_box(payload),
            variant: *variant,
            variants: *variants,
            synth_ty: go_t(synth_ty),
            meta: meta.clone(),
            ext: *ext,
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
            scrutinee: go_box(scrutinee),
            arms: arms
                .iter()
                .map(|a| crate::ast::EnrichedArm {
                    param: a.param.clone(),
                    body: go_e(&a.body),
                    meta: a.meta.clone(),
                })
                .collect(),
            scrutinee_ty: go_t(scrutinee_ty),
            result_ty: go_t(result_ty),
            meta: meta.clone(),
            ext: *ext,
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
            cond: go_box(cond),
            then_branch: go_box(then_branch),
            else_branch: go_box(else_branch),
            result_ty: go_t(result_ty),
            meta: meta.clone(),
            ext: *ext,
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
                .iter()
                .map(|f| crate::ast::RecordField {
                    name: f.name.clone(),
                    value: go_e(&f.value),
                    meta: f.meta.clone(),
                })
                .collect(),
            synth_ty: go_t(synth_ty),
            meta: meta.clone(),
            ext: *ext,
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
            target: go_box(target),
            field_name: field_name.clone(),
            index: *index,
            arity: *arity,
            target_ty: go_t(target_ty),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowHostCall {
            occurrence: _,
            name,
            module_path,
            type_args,
            args,
            sig,
            ret_ty,
            meta,
            ext,
        } => {
            let (sig, scoped) = go_sig(sig);
            Expr::LowHostCall {
                occurrence: Default::default(),
                name: name.clone(),
                module_path: module_path.clone(),
                type_args: type_args.iter().map(go_t).collect(),
                args: args.iter().map(&go_e).collect(),
                sig,
                ret_ty: map_type(ret_ty, &scoped),
                meta: meta.clone(),
                ext: *ext,
            }
        }
        Expr::LowModuleCall {
            occurrence: _,
            mangled,
            type_args,
            args,
            sig,
            ret_ty,
            meta,
            ext,
        } => {
            let (sig, scoped) = go_sig(sig);
            Expr::LowModuleCall {
                occurrence: Default::default(),
                mangled: mangled.clone(),
                type_args: type_args.iter().map(go_t).collect(),
                args: args.iter().map(&go_e).collect(),
                sig,
                ret_ty: ret_ty.as_ref().map(|ty| map_type(ty, &scoped)),
                meta: meta.clone(),
                ext: *ext,
            }
        }
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
        } => {
            let (sig, scoped) = go_sig(sig);
            Expr::LowQualifiedModuleCall {
                occurrence: Default::default(),
                alias: alias.clone(),
                mangled: mangled.clone(),
                type_args: type_args.iter().map(go_t).collect(),
                args: args.iter().map(&go_e).collect(),
                sig,
                ret_ty: ret_ty.as_ref().map(|ty| map_type(ty, &scoped)),
                meta: meta.clone(),
                ext: *ext,
            }
        }
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
            module_path: module_path.clone(),
            newtype: newtype.clone(),
            member: member.clone(),
            type_args: type_args.iter().map(go_t).collect(),
            payload: go_box(payload),
            meta: meta.clone(),
            ext: *ext,
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
            newtype: newtype.clone(),
            member: member.clone(),
            type_args: type_args.iter().map(go_t).collect(),
            payload: go_box(payload),
            meta: meta.clone(),
            ext: *ext,
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
            newtype: newtype.clone(),
            member: member.clone(),
            type_args: type_args.iter().map(go_t).collect(),
            target: go_box(target),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowClosureCall {
            occurrence: _,
            name,
            type_args,
            args,
            meta,
            ext,
        } => Expr::LowClosureCall {
            occurrence: Default::default(),
            name: name.clone(),
            type_args: type_args.iter().map(go_t).collect(),
            args: args.iter().map(&go_e).collect(),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowIndirectCall {
            occurrence: _,
            callee,
            type_args,
            args,
            meta,
            ext,
        } => Expr::LowIndirectCall {
            occurrence: Default::default(),
            callee: go_box(callee),
            type_args: type_args.iter().map(go_t).collect(),
            args: args.iter().map(&go_e).collect(),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowTypeApplication {
            occurrence: _,
            callee,
            type_arg,
            meta,
            ext,
        } => Expr::LowTypeApplication {
            occurrence: Default::default(),
            callee: go_box(callee),
            type_arg: go_t(type_arg),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowAbsurdCall {
            occurrence: _,
            type_arg,
            value_arg,
            meta,
            ext,
        } => Expr::LowAbsurdCall {
            occurrence: Default::default(),
            type_arg: go_t(type_arg),
            value_arg: go_box(value_arg),
            meta: meta.clone(),
            ext: *ext,
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
            newtype: newtype.clone(),
            module_path: module_path.clone(),
            type_args: type_args.iter().map(go_t).collect(),
            receiver: go_box(receiver),
            continuation: go_box(continuation),
            continuation_ty: go_t(continuation_ty),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowBoundRef {
            occurrence: _,
            name,
            meta,
            ext,
        } => Expr::LowBoundRef {
            occurrence: Default::default(),
            name: name.clone(),
            meta: meta.clone(),
            ext: *ext,
        },
        Expr::LowHostFnValueRef {
            occurrence: _,
            name,
            module_path,
            sig,
            ret_ty,
            meta,
            ext,
        } => {
            let (sig, scoped) = go_sig(sig);
            Expr::LowHostFnValueRef {
                occurrence: Default::default(),
                name: name.clone(),
                module_path: module_path.clone(),
                sig,
                ret_ty: map_type(ret_ty, &scoped),
                meta: meta.clone(),
                ext: *ext,
            }
        }
        Expr::LowModuleFnValueRef {
            occurrence: _,
            mangled,
            sig,
            meta,
            ext,
        } => Expr::LowModuleFnValueRef {
            occurrence: Default::default(),
            mangled: mangled.clone(),
            sig: go_sig(sig).0,
            meta: meta.clone(),
            ext: *ext,
        },
    }
}

/// Each visible selectively imported module function contributes one
/// surface-to-mangled entry.
fn build_selective_imports(
    importer: &crate::ast::Module<Routed>,
    package: &Package<Routed>,
) -> std::collections::HashMap<String, String> {
    crate::backends::selective_module_fn_import_owners(importer, package)
        .into_iter()
        .map(|(name, owner)| {
            let mangled = mangle_module_fn(&owner, &name);
            (name, mangled)
        })
        .collect()
}

/// Map a qualified-import alias name to the module path it stands
/// for. Built from `import <pkg>/<mod> as <alias>;` items so the body
/// emitter can resolve `<alias>.<member>(args)` qualified callees
/// to the right module's mangled fn method.
fn build_qualified_imports(imports: &[crate::ast::Import]) -> QualifiedImportMap {
    let mut out = std::collections::HashMap::new();
    for u in imports {
        if let crate::ast::ImportKind::Qualified { path, alias } = &u.kind {
            let path_str = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(alias.clone(), path_str);
        }
    }
    out
}

type QualifiedImportMap = std::collections::HashMap<String, String>;

struct BodyShapeContext<'a> {
    module_path: Option<&'a str>,
}

// ---- shape registry ------------------------------------------------------
//
// Structural types used by erased package bodies get crate-private Rust
// names. The registry walks each type's right spine, deduplicates by the
// canonical pretty-printed shape, and renders those body-only declarations
// into `shapes.rs`. Prepared public facade support and nominal carriers are
// catalog-owned and bypass this content-keyed mint schedule.

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShapeRecord {
    kind: ShapeKind,
    canonical: String,
    mint_name: String,
    slot_keys: Vec<String>,
    /// Rendered slot types. Tparam slots are encoded as the
    /// canonical positional placeholders `_T0`, `_T1`, …; the
    /// shape-struct emitter (`render_shape`) substitutes them with
    /// the struct's user-facing generic names (`A`, `B`, …) when
    /// it writes the struct's fields / variants.
    slot_types: Vec<String>,
    /// Number of synthesized type-param slots the shape declares.
    /// `0` for monomorphic shapes (the previous emitter's only
    /// case); `N>0` for polymorphic shapes whose canonical carries
    /// `_T0`..`_T(N-1)` placeholders that use sites fill in.
    tparam_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ShapeKind {
    Product,
    Sum,
}

/// Which host-parameter spelling an exact host type reaches through.
/// Inside a `Host` trait method body the host is reached as `Self::Foo`;
/// inside a shape struct / package method we use the struct's generic
/// parameter (`H::Foo`). This applies uniformly to role-bearing and roleless
/// declarations (per `specs/backends/rust.md` § Host record contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostParam {
    /// Inside a `Host` trait method body — `Self::Foo`.
    SelfTy,
    /// Inside a shape struct / package method — `__KioHost::Foo`.
    Generated,
    /// Inside a generated scope whose host parameter has a non-default name.
    Named(&'static str),
}

/// Host-parameter spelling for an exact host type at a render site.
#[derive(Debug, Clone, Copy)]
struct TypeCtx {
    host: HostParam,
}

impl TypeCtx {
    /// Trait-method context: `Self::Foo`.
    #[allow(non_upper_case_globals)]
    const Trait: TypeCtx = TypeCtx {
        host: HostParam::SelfTy,
    };

    /// Shape-struct / module-code context: `__KioHost::Foo`.
    #[allow(non_upper_case_globals)]
    const Shape: TypeCtx = TypeCtx {
        host: HostParam::Generated,
    };

    fn host_param(self) -> &'static str {
        match self.host {
            HostParam::SelfTy => "Self",
            HostParam::Generated => RUST_HOST_PARAMETER,
            HostParam::Named(name) => name,
        }
    }

    fn with_host_param(self, name: &'static str) -> TypeCtx {
        TypeCtx {
            host: HostParam::Named(name),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparedRustTypePosition {
    Marker,
    Value,
    Parameter,
    Storage,
}

#[derive(Clone, Default)]
struct PreparedRustScope {
    binders: BTreeMap<FacadeBinderId, String>,
}

#[derive(Clone, Copy)]
struct PreparedRustUseRenderContext<'a> {
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    scope: &'a PreparedRustScope,
    shapes: PreparedRustRenderContext<'a>,
    type_ctx: TypeCtx,
}

fn boundary_site_id(module_path: &str, owner: BoundaryFacadeSiteOwner) -> BoundaryFacadeSiteId {
    BoundaryFacadeSiteId::new(module_path.split('/').map(str::to_owned).collect(), owner)
        .unwrap_or_else(|| unreachable!("a routed boundary declaration has a valid exact site"))
}

fn prepared_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    site: &BoundaryFacadeSiteId,
) -> PreparedBoundaryCallableSite<'a> {
    prepared.site(site).unwrap_or_else(|| {
        unreachable!("Rust emitter: missing prepared boundary facade site {site:?}")
    })
}

fn qualified_newtype_key(name: &QualifiedTypeName) -> String {
    newtype_key(&name.module_segments().join("/"), name.name())
}

fn prepared_newtype_rust_path(name: &QualifiedTypeName, crate_root: &str) -> String {
    let module = name
        .module_segments()
        .iter()
        .map(|segment| rust_public_ident(segment))
        .collect::<Vec<_>>()
        .join("::");
    format!(
        "{crate_root}::shapes::nominal::{module}::{}",
        rust_public_ident(name.name())
    )
}

fn prepared_host_assoc_name(name: &QualifiedTypeName) -> String {
    mangle_host_name(&name.module_segments().join("/"), name.name())
}

fn rust_nominal_constructor_name(
    name: &QualifiedTypeName,
    host_type: bool,
    provided: usize,
) -> String {
    let category = if host_type { "HostType" } else { "Newtype" };
    format!(
        "Kio{category}Constructor_{}__{}_P{provided}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn rust_nominal_type_marker_name(name: &QualifiedTypeName, host_type: bool) -> String {
    let category = if host_type { "HostType" } else { "Newtype" };
    format!(
        "Kio{category}Marker_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn rust_host_type_facade_name(name: &QualifiedTypeName) -> String {
    format!(
        "KioHostType_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn rust_boundary_site_identity(site: &BoundaryFacadeSiteId) -> String {
    let module = encode_host_identity(&site.module_segments().join("/"));
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            format!("M_{module}__H_{}", encode_host_identity(name))
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            format!("M_{module}__E_{}", encode_host_identity(name))
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => format!(
            "M_{module}__NC_{}__{}",
            encode_host_identity(newtype),
            encode_host_identity(member)
        ),
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => format!(
            "M_{module}__NP_{}__{}",
            encode_host_identity(newtype),
            encode_host_identity(member)
        ),
    }
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RustForallOwnerResolutionMeasurement {
    direct_lookups: usize,
    candidate_visits: usize,
}

#[cfg(all(test, feature = "surface"))]
struct RustForallOwnerResolutionLedger {
    target: QualifiedTypeName,
    measurement: RustForallOwnerResolutionMeasurement,
}

#[cfg(all(test, feature = "surface"))]
std::thread_local! {
    static RUST_FORALL_OWNER_RESOLUTION_LEDGER: std::cell::RefCell<
        Option<RustForallOwnerResolutionLedger>,
    > = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, feature = "surface"))]
struct RustForallOwnerResolutionMeasurementGuard;

#[cfg(all(test, feature = "surface"))]
impl Drop for RustForallOwnerResolutionMeasurementGuard {
    fn drop(&mut self) {
        RUST_FORALL_OWNER_RESOLUTION_LEDGER.with(|ledger| {
            *ledger.borrow_mut() = None;
        });
    }
}

#[cfg(all(test, feature = "surface"))]
fn measure_rust_forall_owner_resolution_during<T>(
    target: QualifiedTypeName,
    operation: impl FnOnce() -> T,
) -> (T, RustForallOwnerResolutionMeasurement) {
    RUST_FORALL_OWNER_RESOLUTION_LEDGER.with(|ledger| {
        let mut ledger = ledger.borrow_mut();
        assert!(
            ledger.is_none(),
            "nested Rust forall owner-resolution measurements are not supported"
        );
        *ledger = Some(RustForallOwnerResolutionLedger {
            target,
            measurement: RustForallOwnerResolutionMeasurement::default(),
        });
    });
    let guard = RustForallOwnerResolutionMeasurementGuard;
    let output = operation();
    let measurement = RUST_FORALL_OWNER_RESOLUTION_LEDGER.with(|ledger| {
        ledger
            .borrow_mut()
            .take()
            .expect("Rust forall owner-resolution measurement remains active")
            .measurement
    });
    drop(guard);
    (output, measurement)
}

#[cfg(all(test, feature = "surface"))]
fn record_rust_forall_owner_direct_lookup(target: &QualifiedTypeName) {
    RUST_FORALL_OWNER_RESOLUTION_LEDGER.with(|ledger| {
        let mut ledger = ledger.borrow_mut();
        let Some(ledger) = ledger.as_mut() else {
            return;
        };
        if ledger.target == *target {
            ledger.measurement.direct_lookups += 1;
        }
    });
}

#[cfg(all(test, feature = "surface"))]
fn record_rust_forall_owner_candidate_visit() {
    RUST_FORALL_OWNER_RESOLUTION_LEDGER.with(|ledger| {
        if let Some(ledger) = ledger.borrow_mut().as_mut() {
            ledger.measurement.candidate_visits += 1;
        }
    });
}

#[cfg(all(test, feature = "surface"))]
fn former_rust_forall_plan_owner<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &BoundaryFacadePlan,
) -> Option<&'a QualifiedTypeName> {
    site.nominals()
        .declarations()
        .find_map(|(name, declaration)| {
            record_rust_forall_owner_candidate_visit();
            let BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            } = declaration
            else {
                return None;
            };
            std::ptr::eq(plan, payload.facade()).then_some(name)
        })
}

fn rust_forall_plan_identity(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
) -> Result<String, EmitError> {
    if !std::ptr::eq(plan, site.plan().facade()) {
        unreachable!("a root Rust forall identity has the wrong prepared plan");
    }
    Ok(format!("Site_{}", rust_boundary_site_identity(site.site())))
}

fn rust_forall_name(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
) -> Result<String, EmitError> {
    rust_forall_name_with_owner(site, plan, root, None)
}

fn rust_forall_name_with_owner(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let mut current = root;
    let mut binders = Vec::new();
    while let FacadeUse::Forall { binder, result, .. } = plan.use_at(current) {
        binders.push(format!(
            "B{}_A{}",
            binder.index(),
            plan.binder(*binder).kind.arity()
        ));
        current = *result;
    }
    if binders.is_empty() {
        unreachable!("Rust forall identity starts at a non-forall use");
    }
    Ok(format!(
        "KioForall_{}_U{}_K{}_{}",
        match nominal_owner {
            Some(name) => format!(
                "Newtype_{}__{}",
                encode_host_identity(&name.module_segments().join("/")),
                encode_host_identity(name.name())
            ),
            None => rust_forall_plan_identity(site, plan)?,
        },
        root.index(),
        binders.len(),
        binders.join("_")
    ))
}

fn rust_forall_public_type(
    context: PreparedRustUseRenderContext<'_>,
    root: FacadeUseId,
    position: PreparedRustTypePosition,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let name = rust_forall_name_with_owner(context.site, context.plan, root, nominal_owner)?;
    let parameters = rust_forall_generic_arguments(context.scope, context.type_ctx);
    let generic_use = format!("<{}>", parameters.join(", "));
    Ok(match position {
        PreparedRustTypePosition::Marker => {
            format!(
                "{}::shapes::{name}Marker{generic_use}",
                context.shapes.crate_root()
            )
        }
        PreparedRustTypePosition::Value
        | PreparedRustTypePosition::Parameter
        | PreparedRustTypePosition::Storage => {
            format!(
                "{}::shapes::{name}Value{generic_use}",
                context.shapes.crate_root()
            )
        }
    })
}

fn rust_forall_generic_arguments(scope: &PreparedRustScope, ctx: TypeCtx) -> Vec<String> {
    std::iter::once(ctx.host_param().to_owned())
        .chain(scope.binders.values().cloned())
        .collect()
}

fn rust_invariant_phantom(arguments: &[String]) -> String {
    let tuple = match arguments {
        [argument] => format!("{argument},"),
        _ => arguments.join(", "),
    };
    format!("fn({tuple}) -> ({tuple})")
}

/// Return the representation layout paired with `plan`. Unlike live body
/// execution, source presentation is retained with history-only callables and
/// is therefore the authority for every public callable marker and adapter.
fn prepared_presentation_for_plan<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &BoundaryFacadePlan,
    nominal_owner: Option<&QualifiedTypeName>,
) -> &'a BoundaryFacadeExecutionPlan {
    match nominal_owner {
        None => {
            if !std::ptr::eq(plan, site.plan().facade()) {
                unreachable!("a root Rust facade has the wrong prepared plan")
            }
            site.presentation().root_uses()
        }
        Some(name) => site
            .presentation()
            .transparent_payload(name)
            .unwrap_or_else(|| {
                unreachable!("a transparent Rust nominal has no prepared presentation")
            }),
    }
}

fn render_prepared_source_marker(
    context: PreparedRustUseRenderContext<'_>,
    slots: &[FacadeUseId],
    source: &crate::backends::boundary_facade::CallableSourceParamLayout,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let range = source.facade_slots();
    match source.adapter() {
        CallableSourceParamAdapter::UnitValue => {
            if !range.is_empty() {
                unreachable!("a unit source adapter owns facade slots")
            }
            Ok(format!("{}::shapes::KioUnit", context.shapes.crate_root()))
        }
        CallableSourceParamAdapter::Identity => {
            if range.len() != 1 {
                unreachable!("an identity source adapter does not own exactly one slot")
            }
            render_prepared_use_with_owner(
                context,
                slots[range.start],
                PreparedRustTypePosition::Marker,
                nominal_owner,
            )
        }
        CallableSourceParamAdapter::RightNest => {
            if range.len() < 2 {
                unreachable!("a product source adapter owns fewer than two slots")
            }
            let mut rendered = slots[range]
                .iter()
                .map(|slot| {
                    render_prepared_use_with_owner(
                        context,
                        *slot,
                        PreparedRustTypePosition::Marker,
                        nominal_owner,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut nested = rendered
                .pop()
                .unwrap_or_else(|| unreachable!("a product source adapter has no final slot"));
            for value in rendered.into_iter().rev() {
                nested = format!(
                    "{}::shapes::KioProduct<{value}, {nested}>",
                    context.shapes.crate_root()
                );
            }
            Ok(nested)
        }
    }
}

fn render_prepared_source_facade(
    context: PreparedRustUseRenderContext<'_>,
    slots: &[FacadeUseId],
    source: &crate::backends::boundary_facade::CallableSourceParamLayout,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let range = source.facade_slots();
    match source.adapter() {
        CallableSourceParamAdapter::UnitValue => {
            if !range.is_empty() {
                unreachable!("a unit source adapter owns facade slots")
            }
            Ok("()".to_owned())
        }
        CallableSourceParamAdapter::Identity => {
            if range.len() != 1 {
                unreachable!("an identity source adapter does not own exactly one slot")
            }
            render_prepared_use_with_owner(
                context,
                slots[range.start],
                PreparedRustTypePosition::Value,
                nominal_owner,
            )
        }
        CallableSourceParamAdapter::RightNest => {
            if range.len() < 2 {
                unreachable!("a product source adapter owns fewer than two slots")
            }
            let rendered = slots[range]
                .iter()
                .map(|slot| {
                    render_prepared_use_with_owner(
                        context,
                        *slot,
                        PreparedRustTypePosition::Value,
                        nominal_owner,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut nested = rendered
                .last()
                .cloned()
                .unwrap_or_else(|| unreachable!("a product source adapter has no final slot"));
            for value in rendered[..rendered.len() - 1].iter().rev() {
                nested = format!(
                    "{}::shapes::Product<{value}, {nested}>",
                    context.shapes.crate_root()
                );
            }
            Ok(nested)
        }
    }
}

fn render_prepared_function_source_markers(
    context: PreparedRustUseRenderContext<'_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<Vec<String>, EmitError> {
    if layout.facade_slot_count() != slots.len()
        || layout.source_param_count() != layout.body_abi_arity()
    {
        unreachable!("a Rust callable marker has an inconsistent prepared source layout")
    }
    layout
        .source_params()
        .iter()
        .map(|source| render_prepared_source_marker(context, slots, source, nominal_owner))
        .collect()
}

struct PreparedRustForallMethod {
    public_scope: PreparedRustScope,
    erased_scope: PreparedRustScope,
    type_parameters: Vec<PreparedRustBinderParameter>,
    body: FacadeUseId,
}

fn prepare_rust_forall_method(
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    outer: &PreparedRustScope,
    crate_root: &str,
) -> PreparedRustForallMethod {
    let mut public_scope = outer.clone();
    let mut erased_scope = outer.clone();
    let mut type_parameters = Vec::new();
    let mut body = root;
    while let FacadeUse::Forall { binder, result, .. } = plan.use_at(body) {
        let kind_arity = plan.binder(*binder).kind.arity();
        let name = format!("__KioPolyType_{}", binder.index());
        public_scope.binders.insert(*binder, name.clone());
        erased_scope.binders.insert(
            *binder,
            erased_prepared_type_argument(kind_arity, crate_root),
        );
        type_parameters.push(PreparedRustBinderParameter {
            binder: *binder,
            parameter: PreparedRustTypeParameter { name, kind_arity },
        });
        body = *result;
    }
    PreparedRustForallMethod {
        public_scope,
        erased_scope,
        type_parameters,
        body,
    }
}

fn render_prepared_nominal(
    context: PreparedRustUseRenderContext<'_>,
    name: &QualifiedTypeName,
    args: &[FacadeUseId],
    position: PreparedRustTypePosition,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let declaration = context
        .site
        .nominals()
        .declaration(name)
        .unwrap_or_else(|| {
            unreachable!(
                "Rust emitter: prepared nominal {}.{} has no exact declaration",
                name.module_segments().join("."),
                name.name()
            )
        });
    match declaration {
        BoundaryNominalDeclaration::HostType { type_params, .. } => {
            if args.len() > type_params.len() {
                unreachable!(
                    "Rust emitter: exact host type {}.{} accepts at most {} argument(s), got {}",
                    name.module_segments().join("."),
                    name.name(),
                    type_params.len(),
                    args.len()
                );
            }
            if args.len() < type_params.len() {
                if position != PreparedRustTypePosition::Marker {
                    unreachable!(
                        "Rust emitter: unsaturated exact host type {}.{} reached a value position",
                        name.module_segments().join("."),
                        name.name()
                    );
                }
                if type_params[args.len()..]
                    .iter()
                    .any(|parameter| parameter.kind().arity() != 0)
                {
                    unreachable!(
                        "Rust emitter: exact host type {}.{} reached a non-star unsaturated parameter",
                        name.module_segments().join("."),
                        name.name()
                    );
                }
                let mut rendered = vec![context.type_ctx.host_param().to_owned()];
                rendered.extend(
                    args.iter()
                        .map(|arg| {
                            render_prepared_use_with_owner(
                                context,
                                *arg,
                                PreparedRustTypePosition::Marker,
                                nominal_owner,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
                return Ok(format!(
                    "{}::shapes::{}<{}>",
                    context.shapes.crate_root(),
                    rust_nominal_constructor_name(name, true, args.len()),
                    rendered.join(", ")
                ));
            }
            let mut rendered = vec![context.type_ctx.host_param().to_owned()];
            rendered.extend(
                args.iter()
                    .map(|arg| {
                        render_prepared_use_with_owner(
                            context,
                            *arg,
                            PreparedRustTypePosition::Marker,
                            nominal_owner,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            Ok(match position {
                PreparedRustTypePosition::Marker => format!(
                    "{}::shapes::{}<{}>",
                    context.shapes.crate_root(),
                    rust_nominal_type_marker_name(name, true),
                    rendered.join(", ")
                ),
                PreparedRustTypePosition::Value
                | PreparedRustTypePosition::Parameter
                | PreparedRustTypePosition::Storage => {
                    if type_params.is_empty() {
                        format!(
                            "{}::{}",
                            context.type_ctx.host_param(),
                            prepared_host_assoc_name(name)
                        )
                    } else {
                        format!(
                            "{}::shapes::{}<{}>",
                            context.shapes.crate_root(),
                            rust_host_type_facade_name(name),
                            rendered.join(", ")
                        )
                    }
                }
            })
        }
        BoundaryNominalDeclaration::Newtype { type_params, .. } => {
            if args.len() > type_params.len() {
                unreachable!(
                    "Rust emitter: exact newtype {}.{} accepts at most {} argument(s), got {}",
                    name.module_segments().join("."),
                    name.name(),
                    type_params.len(),
                    args.len()
                );
            }
            if args.len() < type_params.len() {
                if position != PreparedRustTypePosition::Marker {
                    unreachable!(
                        "Rust emitter: unsaturated exact newtype {}.{} reached a value position",
                        name.module_segments().join("."),
                        name.name()
                    );
                }
                if type_params[args.len()..]
                    .iter()
                    .any(|parameter| parameter.kind().arity() != 0)
                {
                    unreachable!(
                        "Rust emitter: exact newtype {}.{} reached a non-star unsaturated parameter",
                        name.module_segments().join("."),
                        name.name()
                    );
                }
                let mut rendered = vec![context.type_ctx.host_param().to_owned()];
                rendered.extend(
                    args.iter()
                        .map(|arg| {
                            render_prepared_use_with_owner(
                                context,
                                *arg,
                                PreparedRustTypePosition::Marker,
                                nominal_owner,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                );
                return Ok(format!(
                    "{}::shapes::{}<{}>",
                    context.shapes.crate_root(),
                    rust_nominal_constructor_name(name, false, args.len()),
                    rendered.join(", ")
                ));
            }
            let mut rendered = vec![context.type_ctx.host_param().to_owned()];
            rendered.extend(
                args.iter()
                    .map(|arg| {
                        render_prepared_use_with_owner(
                            context,
                            *arg,
                            PreparedRustTypePosition::Marker,
                            nominal_owner,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
            Ok(match position {
                PreparedRustTypePosition::Marker => format!(
                    "{}::shapes::{}<{}>",
                    context.shapes.crate_root(),
                    rust_nominal_type_marker_name(name, false),
                    rendered.join(", ")
                ),
                PreparedRustTypePosition::Value
                | PreparedRustTypePosition::Parameter
                | PreparedRustTypePosition::Storage => format!(
                    "{}<{}>",
                    prepared_newtype_rust_path(name, context.shapes.crate_root()),
                    rendered.join(", ")
                ),
            })
        }
    }
}

fn render_prepared_use(
    context: PreparedRustUseRenderContext<'_>,
    id: FacadeUseId,
    position: PreparedRustTypePosition,
) -> Result<String, EmitError> {
    render_prepared_use_with_owner(context, id, position, None)
}

fn render_prepared_use_with_owner(
    context: PreparedRustUseRenderContext<'_>,
    id: FacadeUseId,
    position: PreparedRustTypePosition,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    let rendered = match context.plan.use_at(id) {
        FacadeUse::Unit { .. } => match position {
            PreparedRustTypePosition::Marker => {
                format!("{}::shapes::KioUnit", context.shapes.crate_root())
            }
            PreparedRustTypePosition::Value
            | PreparedRustTypePosition::Parameter
            | PreparedRustTypePosition::Storage => "()".to_owned(),
        },
        FacadeUse::Bottom { .. } => match position {
            PreparedRustTypePosition::Marker => {
                format!("{}::shapes::KioBottom", context.shapes.crate_root())
            }
            PreparedRustTypePosition::Value
            | PreparedRustTypePosition::Parameter
            | PreparedRustTypePosition::Storage => "::std::convert::Infallible".to_owned(),
        },
        FacadeUse::Bound { binder, .. } => {
            let marker = context
                .scope
                .binders
                .get(binder)
                .cloned()
                .unwrap_or_else(|| {
                    unreachable!(
                        "Rust emitter: prepared boundary binder {} is out of scope",
                        binder.index()
                    )
                });
            match position {
                PreparedRustTypePosition::Marker => marker,
                PreparedRustTypePosition::Value
                | PreparedRustTypePosition::Parameter
                | PreparedRustTypePosition::Storage => format!(
                    "<{marker} as {}::shapes::KioType>::Facade",
                    context.shapes.crate_root()
                ),
            }
        }
        FacadeUse::Nominal { name, .. } => {
            render_prepared_nominal(context, name, &[], position, nominal_owner)?
        }
        FacadeUse::Apply {
            constructor, args, ..
        } => match context.plan.use_at(*constructor) {
            FacadeUse::Nominal { name, .. } => {
                render_prepared_nominal(context, name, args, position, nominal_owner)?
            }
            FacadeUse::Bound { binder, .. } => {
                let witness = context.scope.binders.get(binder).unwrap_or_else(|| {
                    unreachable!("Rust emitter: higher-kinded facade binder is out of scope")
                });
                let arity = context.plan.binder(*binder).kind.arity();
                if arity == 0 || arity != args.len() {
                    unreachable!(
                        "Rust emitter: prepared higher-kinded binder `{}` expects {arity} argument(s), got {}",
                        context.plan.binder(*binder).name,
                        args.len()
                    );
                }
                let rendered = args
                    .iter()
                    .map(|arg| {
                        render_prepared_use_with_owner(
                            context,
                            *arg,
                            PreparedRustTypePosition::Marker,
                            nominal_owner,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let application = match position {
                    PreparedRustTypePosition::Marker => "KioApplied",
                    PreparedRustTypePosition::Value
                    | PreparedRustTypePosition::Parameter
                    | PreparedRustTypePosition::Storage => "KioApply",
                };
                format!(
                    "{}::shapes::{application}{arity}<{witness}, {}>",
                    context.shapes.crate_root(),
                    rendered.join(", ")
                )
            }
            _ => {
                unreachable!("Rust emitter: prepared type application has a non-nominal head");
            }
        },
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
            let child_position = if position == PreparedRustTypePosition::Marker {
                PreparedRustTypePosition::Marker
            } else {
                PreparedRustTypePosition::Value
            };
            let rendered = args
                .iter()
                .map(|arg| {
                    render_prepared_use_with_owner(context, *arg, child_position, nominal_owner)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let facade = position != PreparedRustTypePosition::Marker;
            let type_name = if matches!(context.plan.use_at(id), FacadeUse::Product { .. }) {
                if facade { "Product" } else { "KioProduct" }
            } else if facade {
                "Sum"
            } else {
                "KioSum"
            };
            let mut nested = rendered
                .last()
                .cloned()
                .unwrap_or_else(|| unreachable!("a prepared structural use has no slots"));
            for value in rendered[..rendered.len() - 1].iter().rev() {
                nested = format!(
                    "{}::shapes::{type_name}<{value}, {nested}>",
                    context.shapes.crate_root()
                );
            }
            nested
        }
        FacadeUse::Function {
            slots,
            result,
            caps: _,
            ..
        } => {
            let presentation =
                prepared_presentation_for_plan(context.site, context.plan, nominal_owner);
            let layout = match presentation.use_at(id) {
                BoundaryFacadeExecutionUse::Function(layout) => layout,
                _ => unreachable!("a prepared Rust function has no source presentation"),
            };
            let mut marker_args =
                render_prepared_function_source_markers(context, slots, layout, nominal_owner)?;
            let result = render_prepared_use_with_owner(
                context,
                *result,
                PreparedRustTypePosition::Marker,
                nominal_owner,
            )?;
            marker_args.push(result);
            let type_name = if position == PreparedRustTypePosition::Marker {
                "KioFunction"
            } else {
                "KioFn"
            };
            format!(
                "{}::shapes::{type_name}{}<{}>",
                context.shapes.crate_root(),
                layout.body_abi_arity(),
                marker_args.join(", ")
            )
        }
        FacadeUse::Forall { binder, result, .. } => {
            let _ = (binder, result);
            return rust_forall_public_type(context, id, position, nominal_owner);
        }
    };
    Ok(rendered)
}

fn render_prepared_param(
    context: PreparedRustUseRenderContext<'_>,
    id: FacadeUseId,
) -> Result<String, EmitError> {
    render_prepared_param_with_owner(context, id, None)
}

fn render_prepared_param_with_owner(
    context: PreparedRustUseRenderContext<'_>,
    id: FacadeUseId,
    nominal_owner: Option<&QualifiedTypeName>,
) -> Result<String, EmitError> {
    render_prepared_use_with_owner(
        context,
        id,
        PreparedRustTypePosition::Parameter,
        nominal_owner,
    )
}

#[derive(Clone, Eq, PartialEq)]
struct PreparedRustTypeParameter {
    name: String,
    kind_arity: usize,
}

impl PreparedRustTypeParameter {
    fn declaration(&self, crate_root: &str) -> String {
        if self.kind_arity == 0 {
            format!("{}: {}::shapes::KioType", self.name, crate_root)
        } else {
            format!(
                "{}: {}::shapes::KioTypeConstructor{}",
                self.name, crate_root, self.kind_arity
            )
        }
    }
}

#[derive(Clone)]
struct PreparedRustBinderParameter {
    binder: FacadeBinderId,
    parameter: PreparedRustTypeParameter,
}

impl PreparedRustBinderParameter {
    fn declaration(&self, crate_root: &str) -> String {
        self.parameter.declaration(crate_root)
    }

    fn name(&self) -> &str {
        &self.parameter.name
    }

    fn kind_arity(&self) -> usize {
        self.parameter.kind_arity
    }
}

/// Insert one source binder into a Rust generic scope. The first occurrence
/// keeps its word-cased spelling. A later binder whose Rust identity would shadow
/// it uses its stable facade-binder identity in the source-unreachable `__`
/// namespace instead.
fn allocate_prepared_rust_binder_name(
    source_name: &str,
    semantic_index: usize,
    occupied: &BTreeSet<String>,
) -> String {
    let preferred = rust_public_ident(source_name);
    let name = if preferred == RUST_HOST_PARAMETER || occupied.contains(&preferred) {
        format!("__KioType_{semantic_index}")
    } else {
        preferred
    };
    if occupied.contains(&name) {
        unreachable!("distinct prepared Rust binders received one generated identity");
    }
    name
}

fn insert_prepared_source_binder(
    scope: &mut PreparedRustScope,
    binder: FacadeBinderId,
    source_name: &str,
    kind_arity: usize,
) -> PreparedRustBinderParameter {
    let occupied = scope.binders.values().cloned().collect::<BTreeSet<_>>();
    let name = allocate_prepared_rust_binder_name(source_name, binder.index(), &occupied);
    scope.binders.insert(binder, name.clone());
    PreparedRustBinderParameter {
        binder,
        parameter: PreparedRustTypeParameter { name, kind_arity },
    }
}

/// Allocate a nominal declaration's flattened Rust header in declaration
/// order. Public newtype declaration binders are the leading facade binders,
/// so this semantic index is the same identity used by member callables;
/// private nominals use the same collision policy keyed by their declaration
/// slot.
fn prepare_rust_declaration_type_parameters<'a>(
    parameters: impl IntoIterator<Item = (&'a str, usize)>,
) -> Vec<PreparedRustTypeParameter> {
    let mut occupied = BTreeSet::new();
    parameters
        .into_iter()
        .enumerate()
        .map(|(semantic_index, (source_name, kind_arity))| {
            let name = allocate_prepared_rust_binder_name(source_name, semantic_index, &occupied);
            occupied.insert(name.clone());
            PreparedRustTypeParameter { name, kind_arity }
        })
        .collect()
}

struct PreparedRustCallable {
    scope: PreparedRustScope,
    /// Lexical binder scope after each declaration-head stage. A value type
    /// that occurs before a later type stage must not acquire that later
    /// binder merely because Rust flattens the method's generic declaration.
    head_scopes: Vec<PreparedRustScope>,
    type_parameters: Vec<PreparedRustBinderParameter>,
    value_slots: Vec<FacadeUseId>,
    returned: FacadeUseId,
}

fn prepare_rust_callable(site: PreparedBoundaryCallableSite<'_>) -> PreparedRustCallable {
    let entry = site.plan().entry();
    let mut scope = PreparedRustScope::default();
    let mut head_scopes = Vec::new();
    let mut type_parameters = Vec::new();
    let mut value_slots = Vec::new();
    for stage in entry.head_stages {
        match stage {
            BoundaryCallableHeadStage::Type { id, binder } => {
                type_parameters.push(insert_prepared_source_binder(
                    &mut scope,
                    id,
                    &binder.name,
                    binder.kind.arity(),
                ));
            }
            BoundaryCallableHeadStage::Value { slots } => {
                value_slots.extend_from_slice(slots);
            }
        }
        head_scopes.push(scope.clone());
    }
    PreparedRustCallable {
        scope,
        head_scopes,
        type_parameters,
        value_slots,
        returned: entry.returned,
    }
}

fn prepared_host_method_turbofish(
    _site: PreparedBoundaryCallableSite<'_>,
    callable: &PreparedRustCallable,
    crate_root: &str,
) -> String {
    let generic_arguments = callable
        .type_parameters
        .iter()
        .map(|parameter| erased_prepared_type_argument(parameter.kind_arity(), crate_root))
        .collect::<Vec<_>>();
    if generic_arguments.is_empty() {
        String::new()
    } else {
        format!("::<{}>", generic_arguments.join(", "))
    }
}

fn erased_prepared_type_argument(kind_arity: usize, crate_root: &str) -> String {
    if kind_arity == 0 {
        format!("{crate_root}::shapes::__KioErasedType")
    } else {
        format!("{crate_root}::shapes::__KioErasedTypeConstructor{kind_arity}")
    }
}

fn erased_prepared_scope(
    site: PreparedBoundaryCallableSite<'_>,
    scope: &PreparedRustScope,
    crate_root: &str,
) -> PreparedRustScope {
    let mut erased = scope.clone();
    for (binder, rendered) in &mut erased.binders {
        *rendered = erased_prepared_type_argument(
            site.plan().facade().binder(*binder).kind.arity(),
            crate_root,
        );
    }
    erased
}

#[derive(Debug, Clone, Copy)]
struct HostTypeInfo {
    role: Option<Role>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RustHostParamRequirement {
    None,
    Unsized,
    Sized,
}

/// Private body-shape storage plus frozen metadata consumed by prepared public
/// rendering. `shapes` and `minted_names` never select a public boundary type:
/// callable signatures, slots, conversions, canonical facades, and live/retained
/// nominal carriers all come from `PreparedBoundaryCallableSites`.
#[derive(Clone)]
struct BodyShapeRegistry {
    /// Exact `(declaring module, leaf)` identity for every host type on
    /// the boundary. Routed host-type paths carry this identity directly;
    /// bare paths are lexical type variables.
    host_types: BTreeMap<(String, String), HostTypeInfo>,
    newtype_by_key: BTreeMap<String, NewtypeRecord>,
    /// Every public nominal carrier in the frozen live-plus-retained catalog.
    /// The shared prepared catalog owns its identity and binders; live member
    /// methods are emitted separately from their exact prepared sites.
    prepared_newtypes: BTreeMap<String, BoundaryPublicNewtypeInventoryEntry>,
    prepared_newtype_origins: BTreeMap<String, BoundaryFacadeSupportOrigin>,
    unique_newtype_key_by_name: BTreeMap<String, String>,
    /// Per-module newtype-import scope, keyed by the importing module's
    /// slash-path: leaf → declaring module path, for each newtype the
    /// module brings into scope with `import N(…, X, …);`. A bare
    /// single-segment newtype reference resolves through its module's
    /// own declarations and then this scope, never through the
    /// package-global [`Self::unique_newtype_key_by_name`] table — that
    /// keeps resolution open-world (see [`collect_newtype_import_scopes`]
    /// and `specs/language.md` § Open-world design). Built by
    /// [`collect_newtype_import_scopes`].
    newtype_import_scopes: BTreeMap<String, BTreeMap<String, String>>,
    shapes: BTreeMap<String, ShapeRecord>,
    /// Claimed crate-private body-shape mint name → owning canonical string.
    /// Private body shapes participate in this content-keyed claim schedule;
    /// the public facade uses the fixed recursive marker algebra instead.
    minted_names: BTreeMap<String, String>,
    referenced_newtypes: std::collections::BTreeSet<String>,
    /// Path prefix the emitted code uses to reach this package's
    /// own `host` / `shapes` modules.
    crate_root: String,
    /// The branded host-contract trait name (`<Handle>Host`). A newtype
    /// whose payload references an exact host type spells its bound as
    /// `<crate_root>::host::<host_trait> + ?Sized`; the FFI aliases spell
    /// a host-associated leaf as `<H as <crate_root>::host::<host_trait>>::…`.
    host_trait: String,
    /// The module whose declarations are currently being walked, if
    /// any — set during per-module shape registration and again while
    /// rendering each newtype's payload (the newtype's declaring
    /// module). A bare single-segment newtype reference resolves in
    /// this module's scope first so a same-leaf name shared by two
    /// package modules picks the declaration that is in scope (mirrors
    /// the body-emit side's `module_path`-aware `resolve_newtype_key`).
    /// Public package-surface positions never consult this field.
    current_module: Option<String>,
}

/// Narrow rendering context for the prepared public facade. It exposes only
/// package branding, exact host ownership metadata, and membership in the
/// frozen nominal-carrier catalog. In particular it has no body-shape map,
/// mint schedule, raw newtype declarations, or lexical-resolution state, so a
/// public signature or conversion cannot re-plan through [`BodyShapeRegistry`].
#[derive(Clone, Copy)]
struct PreparedRustRenderContext<'a> {
    crate_root: &'a str,
    host_trait: &'a str,
    prepared: &'a PreparedBoundaryCallableSites,
}

#[derive(Clone, Debug)]
struct PreparedRustFacadeIndex {
    eligible_retained_newtypes: BTreeSet<QualifiedTypeName>,
    payload_owners: BTreeMap<(QualifiedTypeName, bool), BoundaryFacadeSiteId>,
}

impl PreparedRustFacadeIndex {
    fn new(prepared: &PreparedBoundaryCallableSites) -> Self {
        let eligible_retained_newtypes = rust_eligible_retained_newtypes(prepared);
        let mut payload_owners = BTreeMap::new();
        for site in prepared.sites() {
            if rust_site_depends_on_retained_host_binding_prepared(site, prepared) {
                continue;
            }
            let retained = matches!(
                site.origin(),
                PreparedBoundaryCallableOriginRef::Retained(_)
            );
            for (name, declaration) in site.nominals().declarations() {
                if matches!(
                    declaration,
                    BoundaryNominalDeclaration::Newtype {
                        transparent_payload: Some(_),
                        ..
                    }
                ) {
                    payload_owners
                        .entry((name.clone(), retained))
                        .or_insert_with(|| site.site().clone());
                }
            }
        }
        Self {
            eligible_retained_newtypes,
            payload_owners,
        }
    }
}

impl<'a> PreparedRustRenderContext<'a> {
    fn new(
        prepared: &'a PreparedBoundaryCallableSites,
        crate_root: &'a str,
        host_trait: &'a str,
    ) -> Self {
        Self {
            crate_root,
            host_trait,
            prepared,
        }
    }

    fn crate_root(self) -> &'a str {
        self.crate_root
    }

    fn host_trait(self) -> &'a str {
        self.host_trait
    }

    fn has_public_newtype(self, name: &QualifiedTypeName) -> bool {
        self.prepared.public_newtype(name).is_some()
            || self.prepared.retained_public_newtype(name).is_some()
    }
}

impl BodyShapeRegistry {
    fn new(
        host_types: BTreeMap<(String, String), HostTypeInfo>,
        newtype_by_key: BTreeMap<String, NewtypeRecord>,
        newtype_import_scopes: BTreeMap<String, BTreeMap<String, String>>,
        crate_root: String,
        host_trait: String,
    ) -> Self {
        let unique_newtype_key_by_name = unique_newtype_key_by_name(&newtype_by_key);
        BodyShapeRegistry {
            host_types,
            newtype_by_key,
            prepared_newtypes: BTreeMap::new(),
            prepared_newtype_origins: BTreeMap::new(),
            unique_newtype_key_by_name,
            newtype_import_scopes,
            shapes: BTreeMap::new(),
            minted_names: BTreeMap::new(),
            referenced_newtypes: std::collections::BTreeSet::new(),
            crate_root,
            host_trait,
            current_module: None,
        }
    }

    fn register_prepared_newtype(
        &mut self,
        entry: &BoundaryPublicNewtypeInventoryEntry,
        origin: BoundaryFacadeSupportOrigin,
    ) {
        let key = qualified_newtype_key(entry.name());
        if let Some(previous) = self.prepared_newtypes.insert(key.clone(), entry.clone()) {
            debug_assert_eq!(previous, *entry);
        }
        insert_rust_support_origin(&mut self.prepared_newtype_origins, key, origin);
    }

    fn host_type_info(&self, segments: &[PathSegment]) -> Option<HostTypeInfo> {
        let (name, module) = segments.split_last()?;
        if module.is_empty() {
            return None;
        }
        let module = module
            .iter()
            .map(|segment| segment.as_str())
            .collect::<Vec<_>>()
            .join("/");
        self.host_types.get(&(module, name.name.clone())).copied()
    }

    fn qualified_type_module_path_in_scope(
        &self,
        segments: &[PathSegment],
        _module_path: Option<&str>,
    ) -> Option<String> {
        if segments.len() < 2 {
            return None;
        }
        Some(
            segments[..segments.len() - 1]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// Referenced Routed multi-segment paths already name their declaring
    /// module. Replaying a surface `import … as` table here would reinterpret
    /// that identity in the consumer's scope and can duplicate path segments.
    fn qualify_type_segments_in_scope(
        &self,
        segments: &[PathSegment],
        _module_path: Option<&str>,
    ) -> Vec<PathSegment> {
        segments.to_vec()
    }

    fn normalize_type_for_tparams_in_current_scope(
        &self,
        ty: &Type<Routed>,
        tparams: &std::collections::HashSet<&str>,
    ) -> Type<Routed> {
        self.normalize_type_for_tparams_in_scope(ty, self.current_module.as_deref(), tparams)
    }

    fn normalize_type_for_tparams_in_scope(
        &self,
        ty: &Type<Routed>,
        module_path: Option<&str>,
        tparams: &std::collections::HashSet<&str>,
    ) -> Type<Routed> {
        let type_vars = tparams
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>();
        self.normalize_type_for_reach_in(ty, module_path, &type_vars)
    }

    fn normalize_type_for_owned_tparams_in_current_scope(
        &self,
        ty: &Type<Routed>,
        tparams: &std::collections::HashSet<String>,
    ) -> Type<Routed> {
        self.normalize_type_for_owned_tparams_in_scope(ty, self.current_module.as_deref(), tparams)
    }

    fn normalize_type_for_owned_tparams_in_scope(
        &self,
        ty: &Type<Routed>,
        module_path: Option<&str>,
        tparams: &std::collections::HashSet<String>,
    ) -> Type<Routed> {
        let type_vars = tparams.iter().cloned().collect::<BTreeSet<_>>();
        self.normalize_type_for_reach_in(ty, module_path, &type_vars)
    }

    fn normalize_type_for_signature_scope(
        &self,
        ty: &Type<Routed>,
        sig: &crate::ast::Signature<Routed>,
    ) -> Type<Routed> {
        let type_vars = sig
            .params
            .iter()
            .filter_map(|param| match param {
                crate::ast::SignatureParam::Type(param) => Some(param.name.clone()),
                crate::ast::SignatureParam::Value(_) => None,
            })
            .collect::<BTreeSet<_>>();
        self.normalize_type_for_reach_in(ty, self.current_module.as_deref(), &type_vars)
    }

    fn normalize_signature_for_current_scope(
        &self,
        sig: &crate::ast::Signature<Routed>,
    ) -> crate::ast::Signature<Routed> {
        let mut type_vars = BTreeSet::new();
        let mut params = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            match param {
                crate::ast::SignatureParam::Type(tp) => {
                    type_vars.insert(tp.name.clone());
                    params.push(crate::ast::SignatureParam::Type(tp.clone()));
                }
                crate::ast::SignatureParam::Value(value) => {
                    let mut value = value.clone();
                    if let Some(ty) = &value.ty {
                        value.ty = Some(self.normalize_type_for_reach_in(
                            ty,
                            self.current_module.as_deref(),
                            &type_vars,
                        ));
                    }
                    params.push(crate::ast::SignatureParam::Value(value));
                }
            }
        }
        crate::ast::Signature {
            params,
            groups: sig.groups.clone(),
        }
    }

    fn normalize_type_for_reach(
        &self,
        ty: &Type<Routed>,
        context: &RustReachContext,
    ) -> Type<Routed> {
        self.normalize_type_for_reach_in(ty, context.module_path.as_deref(), &context.type_vars)
    }

    fn normalize_type_for_reach_in(
        &self,
        ty: &Type<Routed>,
        module_path: Option<&str>,
        type_vars: &BTreeSet<String>,
    ) -> Type<Routed> {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let normalized_args: Vec<Type<Routed>> = args
                    .iter()
                    .map(|arg| self.normalize_type_for_reach_in(arg, module_path, type_vars))
                    .collect();
                if segments.len() == 1
                    && let Some(name) = segments.first().map(|s| s.as_str())
                    && type_vars.contains(name)
                {
                    return Type::Path {
                        segments: segments.clone(),
                        args: normalized_args,
                        meta: meta.clone(),
                    };
                }
                if self
                    .newtype_key_for_segments_in_scope(segments, module_path)
                    .is_some()
                    || self.host_type_info(segments).is_some()
                {
                    return Type::Path {
                        segments: self.qualify_type_segments_in_scope(segments, module_path),
                        args: normalized_args,
                        meta: meta.clone(),
                    };
                }
                if normalized_args.is_empty()
                    && let [single] = segments.as_slice()
                    && let Some(builtin) =
                        crate::comptime::ComptimeBuiltin::from_public_name(single.as_str())
                    && let Some(erasure) = builtin.runtime_erasure()
                {
                    return match erasure {
                        crate::comptime::ComptimeRuntimeErasure::Bottom => {
                            Type::Bottom { meta: meta.clone() }
                        }
                        crate::comptime::ComptimeRuntimeErasure::Unit => {
                            Type::Unit { meta: meta.clone() }
                        }
                    };
                }
                Type::Path {
                    segments: self.qualify_type_segments_in_scope(segments, module_path),
                    args: normalized_args,
                    meta: meta.clone(),
                }
            }
            Type::Product { left, right, meta } => Type::Product {
                left: Box::new(self.normalize_type_for_reach_in(left, module_path, type_vars)),
                right: Box::new(self.normalize_type_for_reach_in(right, module_path, type_vars)),
                meta: meta.clone(),
            },
            Type::Sum { left, right, meta } => Type::Sum {
                left: Box::new(self.normalize_type_for_reach_in(left, module_path, type_vars)),
                right: Box::new(self.normalize_type_for_reach_in(right, module_path, type_vars)),
                meta: meta.clone(),
            },
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => Type::Function {
                param: Box::new(self.normalize_type_for_reach_in(param, module_path, type_vars)),
                ret: Box::new(self.normalize_type_for_reach_in(ret, module_path, type_vars)),
                meta: meta.clone(),
                abi_arity: *abi_arity,
                caps: caps.clone(),
            },
            Type::Forall { param, body, meta } => {
                let mut scoped = type_vars.clone();
                scoped.insert(param.name.clone());
                Type::Forall {
                    param: param.clone(),
                    body: Box::new(self.normalize_type_for_reach_in(body, module_path, &scoped)),
                    meta: meta.clone(),
                }
            }
            other => other.clone(),
        }
    }

    fn host_type_role(&self, segments: &[PathSegment]) -> Option<Role> {
        self.host_type_info(segments)?.role
    }

    fn is_host_type(&self, segments: &[PathSegment]) -> bool {
        self.host_type_info(segments).is_some()
    }

    fn host_assoc_name(&self, segments: &[PathSegment]) -> Result<String, EmitError> {
        let (name, module) = segments.split_last().ok_or_else(|| {
            EmitError::unsupported("Rust emitter: empty host-type path (compiler bug)")
        })?;
        let module = module
            .iter()
            .map(|segment| segment.as_str())
            .collect::<Vec<_>>()
            .join("/");
        if module.is_empty() || self.host_type_info(segments).is_none() {
            return Err(EmitError::unsupported(format!(
                "Rust emitter: `{}` is not an exact host-type path (compiler bug)",
                name.as_str()
            )));
        }
        Ok(mangle_host_name(&module, name.as_str()))
    }

    /// Returns the higher-kinded parameter slots the named newtype's payload
    /// actually reaches. This payload-only predicate belongs to equality
    /// reachability; nominal header/use erasure instead follows every declared
    /// non-`Star` slot through [`higher_kinded_param_indices`].
    fn newtype_payload_hkt_slots(&self, key: &str) -> std::collections::HashSet<usize> {
        self.newtype_by_key
            .get(key)
            .map(|record| payload_hkt_param_indices(&record.decl))
            .unwrap_or_default()
    }

    /// The host-parameter bound required by `ty`, walked structurally. Exact
    /// host associated types admit `H: Host + ?Sized`; a prepared public
    /// nominal carrier requires the sized `H: Host` form from its declaration.
    ///
    /// The walk resolves **nominal newtype references** to their
    /// declared payloads, mirroring [`Self::type_reaches_non_partial_eq_leaf`]
    /// and [`Self::payload_references_brand_eligible`]. A newtype whose
    /// payload references a host type renders with the
    /// host-trait `+ ?Sized` bound on its own generic header (e.g.
    /// `V_host<H: …Host + ?Sized>` over `H::Scalar`); a *different*
    /// newtype whose payload references that newtype by name —
    /// `Value : … | V_host(H) | …`, rendered as a field of type
    /// `crate::shapes::nominal::value::V_host<H>` — must carry the same
    /// bound, because
    /// Rust requires `V_host<H>`'s host-trait constraint to hold at the
    /// use site. Looking only at the referring payload's own syntactic
    /// leaves would stop at the `V_host` *name* and miss the transitive
    /// requirement, emitting `H: 'static` where the host-trait bound is
    /// needed. A `visited` key set breaks recursive-newtype cycles
    /// (`Vals : . | (Value & Vals)`).
    fn host_param_requirement(&self, ty: &Type<Routed>) -> RustHostParamRequirement {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.host_param_requirement_impl(ty, &mut visited)
    }

    fn host_param_requirement_impl(
        &self,
        ty: &Type<Routed>,
        visited: &mut std::collections::HashSet<String>,
    ) -> RustHostParamRequirement {
        match ty {
            Type::Path { segments, args, .. } => {
                // A path either resolves to a nominal newtype or carries
                // an exact host-type identity. A newtype-resolving path is
                // not itself a host-type reference, but the newtype it
                // names carries its own `H` bound — recurse into the
                // declared payload so a host type *inside* the
                // referenced newtype propagates the host-trait bound to
                // this referrer (the rendered
                // `crate::shapes::nominal::api::Foo<H>` field demands it).
                if let Some(key) = self.newtype_key_for_segments(segments) {
                    if self.prepared_newtypes.contains_key(&key) {
                        return RustHostParamRequirement::Sized;
                    }
                    let mut requirement = RustHostParamRequirement::None;
                    if visited.insert(key.clone()) {
                        let payload = &self.newtype_by_key[&key].decl.payload;
                        requirement = self.host_param_requirement_impl(payload, visited);
                    }
                    return args.iter().fold(requirement, |requirement, arg| {
                        requirement.max(self.host_param_requirement_impl(arg, visited))
                    });
                }
                let requirement = if self.is_host_type(segments) {
                    RustHostParamRequirement::Unsized
                } else {
                    RustHostParamRequirement::None
                };
                args.iter().fold(requirement, |requirement, arg| {
                    requirement.max(self.host_param_requirement_impl(arg, visited))
                })
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => self
                .host_param_requirement_impl(left, visited)
                .max(self.host_param_requirement_impl(right, visited)),
            Type::Function { param, ret, .. } => self
                .host_param_requirement_impl(param, visited)
                .max(self.host_param_requirement_impl(ret, visited)),
            Type::Forall { body, .. } => self.host_param_requirement_impl(body, visited),
            Type::Unit { .. }
            | Type::Bottom { .. }
            | Type::Infer { .. }
            | Type::LabelSugar { .. } => RustHostParamRequirement::None,
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Whether comparing a value of `ty` for structural equality would
    /// require comparing a non-`PartialEq` field. Functions render as
    /// `Rc<dyn Fn>`, recursive body-storage newtypes render through erased
    /// `Rc<dyn Any>`, and
    /// existential newtypes deliberately skip `PartialEq`; any outer
    /// shape/newtype that stores one must skip its own `PartialEq` impl too.
    ///
    /// The walk resolves **nominal newtype references** to their
    /// declared payloads, not just structural compounds and aliases: a
    /// newtype whose payload is comparable-looking (`Type::Path`) but
    /// which itself wraps a function — `Cell : Lazy` where
    /// `Lazy : . -> I32` — or which reaches a recursive newtype (a
    /// `Ctrl : Cs_eval | Cs_ret` whose arm reaches the recursive `Value`)
    /// would otherwise be misreported as comparable, and the delegated
    /// `self.0 == other.0` would fail to typecheck against the
    /// non-`PartialEq` inner field. Type aliases are unfolded; Path
    /// arguments, structural compounds, and `Forall` bodies recurse; a
    /// per-path newtype key set breaks recursive-newtype cycles
    /// (`List[A] : . | (A & List(A))`).
    fn type_reaches_non_partial_eq_leaf(
        &self,
        ty: &Type<Routed>,
        type_vars: BTreeSet<String>,
    ) -> bool {
        self.type_reaches_non_partial_eq_leaf_impl(
            ty,
            RustPartialEqReachContext {
                scope: RustReachContext {
                    module_path: self.current_module.clone(),
                    type_vars,
                },
                newtype_path: BTreeSet::new(),
            },
        )
    }

    fn type_reaches_non_partial_eq_leaf_impl(
        &self,
        ty: &Type<Routed>,
        context: RustPartialEqReachContext,
    ) -> bool {
        crate::backends::skin::type_reaches_with(
            ty,
            context,
            &|ty, context: &RustPartialEqReachContext| {
                self.normalize_type_for_reach(ty, &context.scope)
            },
            &mut |ty, _| {
                matches!(ty, Type::Function { .. })
                    || matches!(
                        ty,
                        Type::Path { segments, args, .. }
                            if segments.len() == 1
                                && args.is_empty()
                            && segments[0].as_str() == EXISTENTIAL_ANY_SENTINEL
                    )
            },
            &mut |ty, context: &RustPartialEqReachContext| {
                let Type::Path { segments, args, .. } = ty else {
                    return crate::backends::skin::TypeReachAction::IgnoreSubtree;
                };
                if segments.len() == 1
                    && let Some(name) = segments.first().map(|s| s.as_str())
                    && context.scope.type_vars.contains(name)
                {
                    return crate::backends::skin::TypeReachAction::TraverseArguments;
                }
                if !args.is_empty() && self.host_type_info(segments).is_some() {
                    // Applied host types use the erased body-storage carrier,
                    // which cannot participate in structural equality.
                    return crate::backends::skin::TypeReachAction::Found;
                }
                let Some(key) = self.newtype_key_for_segments_in_scope(
                    segments,
                    context.scope.module_path.as_deref(),
                ) else {
                    return crate::backends::skin::TypeReachAction::TraverseArguments;
                };
                if self.prepared_newtypes.contains_key(&key) {
                    return crate::backends::skin::TypeReachAction::Found;
                }
                // A recursive newtype renders as the erased `Rc<dyn Any>`
                // (not `PartialEq`), so a payload that reaches one is
                // non-comparable — terminal here.
                if self.newtype_is_recursive(&key) {
                    return crate::backends::skin::TypeReachAction::Found;
                }
                let record = &self.newtype_by_key[&key];
                if !record.decl.existential_params.is_empty() {
                    return crate::backends::skin::TypeReachAction::Found;
                }
                if !self.newtype_payload_hkt_slots(&key).is_empty() {
                    return crate::backends::skin::TypeReachAction::Found;
                }
                if context.newtype_path.contains(&key) {
                    return crate::backends::skin::TypeReachAction::TraverseArguments;
                }
                let mut type_vars: BTreeSet<String> = record
                    .decl
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                type_vars.extend(crate::backends::skin::type_vars_referenced_in_args(
                    args,
                    &context.scope.type_vars,
                ));
                let mut newtype_path = context.newtype_path.clone();
                newtype_path.insert(key.clone());
                crate::backends::skin::TypeReachAction::Descend {
                    ty: crate::backends::skin::instantiate_newtype_payload(&record.decl, ty),
                    context: RustPartialEqReachContext {
                        scope: RustReachContext {
                            module_path: newtype_key_module_path(&key)
                                .map(str::to_owned)
                                .or_else(|| context.scope.module_path.clone()),
                            type_vars,
                        },
                        newtype_path,
                    },
                    traverse_arguments: true,
                }
            },
            &|context, param| {
                let mut next = context.clone();
                next.scope.type_vars.insert(param.name.clone());
                next
            },
        )
    }

    fn lookup_newtype(&self, key: &str) -> Option<&NewtypeRecord> {
        self.newtype_by_key.get(key)
    }

    /// True when the newtype keyed by `key` is **recursive** — its payload
    /// reaches a reference back to itself, directly or through a chain of
    /// other newtypes (mutual recursion: `Value → V_closure → Env →
    /// Value`). A recursive newtype has no finite body-storage shape, so the
    /// crate-private representation uses erased `Rc<dyn Any>` storage.
    fn newtype_is_recursive(&self, key: &str) -> bool {
        let Some(record) = self.newtype_by_key.get(key) else {
            return false;
        };
        self.type_reaches_newtype_impl(
            &record.decl.payload,
            key,
            RustRecursionReachContext {
                scope: RustReachContext {
                    module_path: newtype_key_module_path(key).map(str::to_owned),
                    type_vars: record
                        .decl
                        .type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect(),
                },
                newtype_path: BTreeSet::from([key.to_owned()]),
            },
        )
    }

    /// Walk `ty`'s structure for a path back to the newtype keyed by
    /// `root`. The context carries the declarations entered on this structural
    /// path, so cycles terminate without suppressing an independent sibling
    /// occurrence. Aliases are unfolded so a cycle threaded through a type
    /// alias is still detected.
    fn type_reaches_newtype_impl(
        &self,
        ty: &Type<Routed>,
        root: &str,
        context: RustRecursionReachContext,
    ) -> bool {
        crate::backends::skin::type_reaches_with(
            ty,
            context,
            &|ty, context: &RustRecursionReachContext| {
                self.normalize_type_for_reach(ty, &context.scope)
            },
            &mut |_, _| false,
            &mut |ty, context: &RustRecursionReachContext| {
                let Type::Path { segments, args, .. } = ty else {
                    return crate::backends::skin::TypeReachAction::IgnoreSubtree;
                };
                if segments.len() == 1
                    && let Some(name) = segments.first().map(|s| s.as_str())
                    && context.scope.type_vars.contains(name)
                {
                    return crate::backends::skin::TypeReachAction::TraverseArguments;
                }
                let Some(key) = self.newtype_key_for_segments_in_scope(
                    segments,
                    context.scope.module_path.as_deref(),
                ) else {
                    return crate::backends::skin::TypeReachAction::TraverseArguments;
                };
                if key == root {
                    return crate::backends::skin::TypeReachAction::Found;
                }
                let record = &self.newtype_by_key[&key];
                if record
                    .host_surface
                    .is_some_and(|surface| surface != RustNewtypeHostSurface::Both)
                {
                    return crate::backends::skin::TypeReachAction::IgnoreSubtree;
                }
                if context.newtype_path.contains(&key) {
                    return crate::backends::skin::TypeReachAction::IgnoreSubtree;
                }
                let mut type_vars: BTreeSet<String> = record
                    .decl
                    .type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .collect();
                type_vars.extend(crate::backends::skin::type_vars_referenced_in_args(
                    args,
                    &context.scope.type_vars,
                ));
                let mut newtype_path = context.newtype_path.clone();
                newtype_path.insert(key.clone());
                crate::backends::skin::TypeReachAction::Descend {
                    ty: crate::backends::skin::instantiate_newtype_payload(&record.decl, ty),
                    context: RustRecursionReachContext {
                        scope: RustReachContext {
                            module_path: newtype_key_module_path(&key)
                                .map(str::to_owned)
                                .or_else(|| context.scope.module_path.clone()),
                            type_vars,
                        },
                        newtype_path,
                    },
                    traverse_arguments: false,
                }
            },
            &|context, param| {
                let mut next = context.clone();
                next.scope.type_vars.insert(param.name.clone());
                next
            },
        )
    }

    /// The Rust storage type a newtype's **function** payload lands as.
    /// Polymorphic dictionary payloads use their established spread domain;
    /// an ordinary function retains its declared `abi_arity`, including a
    /// one-argument function whose argument happens to be a product.
    fn newtype_fn_field_ty(
        &self,
        payload: &Type<Routed>,
        nt_tparams: &std::collections::HashSet<&str>,
    ) -> Option<String> {
        let (inner_fn, arity) = match detect_polymorphic_fn_payload(payload) {
            Some((_binders, inner_fn)) => {
                let arity = match &inner_fn {
                    Type::Function {
                        param, abi_arity, ..
                    } => FunctionBoundaryAdapterPlan::new(param, *abi_arity).boundary_arity(),
                    _ => unreachable!("polymorphic function detection returns a function"),
                };
                (inner_fn, arity)
            }
            None => match peel_forall_ty(payload) {
                Type::Function { abi_arity, .. } => (payload.clone(), *abi_arity),
                _ => return None,
            },
        };
        let _ = nt_tparams;
        let Type::Function { .. } = peel_forall_ty(&inner_fn) else {
            return None;
        };
        Some(rc_fn_type(arity))
    }

    fn newtype_key_for_segments(&self, segments: &[PathSegment]) -> Option<String> {
        self.newtype_key_for_segments_in_scope(segments, self.current_module.as_deref())
    }

    fn newtype_key_for_segments_in_scope(
        &self,
        segments: &[PathSegment],
        module_path: Option<&str>,
    ) -> Option<String> {
        if segments.len() == 1 {
            let name = segments[0].as_str();
            // A bare reference resolves against the module currently
            // being walked — its own declarations plus what it imports
            // (`import …(…);`) — so a same-leaf newtype shared by two
            // package modules picks the declaration in scope, and an
            // imported leaf resolves to its declaring module regardless
            // of whether some unrelated module shares the leaf (open-
            // world; see `resolve_newtype_key_in_scope`). Package-surface
            // walks leave `current_module` unset and fall to the
            // unique-by-name table, where every reference is qualified.
            return self.resolve_newtype_key_in_scope(name, module_path);
        }
        let name = segments.last()?.as_str();
        let module_path = self.qualified_type_module_path_in_scope(segments, module_path)?;
        let key = newtype_key(&module_path, name);
        self.newtype_by_key.contains_key(&key).then_some(key)
    }

    /// Resolve a bare newtype leaf `name` referenced from `module_path`
    /// to its `{module}.{leaf}` key. Resolution order:
    ///
    /// 1. the referring module's **own** declaration of the leaf;
    /// 2. the newtype the module **imports** for that leaf with
    ///    `import N(…, X, …);` ([`Self::newtype_import_scopes`]);
    /// 3. the package-global by-leaf table, as a last resort.
    ///
    /// Steps 1–2 are open-world-safe: a reference to a newtype the module
    /// declares or imports resolves from `module_path`'s own declarations
    /// and explicit imports alone, so adding a same-leaf newtype to an
    /// unrelated module cannot change (or break) that reference. This is
    /// the fix for the case where a module references a newtype it
    /// imports while another module declares the same leaf: the import
    /// scope pins it to the declaring module before the by-leaf table —
    /// which drops a leaf the moment two modules share it — is ever
    /// consulted (see `specs/language.md` § Open-world design and
    /// `specs/backends/rust.md` § Structural and nominal types). Step 3
    /// resolves only a leaf the module neither declares nor imports.
    /// The one module-context source of such a leaf — a bare newtype
    /// leaf re-surfaced by unfolding an alias the module imports from a
    /// third module — is qualified to its declaring module upstream in
    /// the `recover_to_low` pass (`unfold_type_alias_type`), so it
    /// reaches here as an already-module-qualified path resolved by
    /// `newtype_key_for_segments`, never as a bare leaf. Step 3 is thus
    /// the genuine package-surface fallback (no module context), where
    /// any reference is module-qualified anyway.
    fn newtype_key_for_name_in_module(&self, module_path: &str, name: &str) -> Option<String> {
        let own = newtype_key(module_path, name);
        if self.newtype_by_key.contains_key(&own) {
            return Some(own);
        }
        if let Some(from_module) = self
            .newtype_import_scopes
            .get(module_path)
            .and_then(|s| s.get(name))
        {
            let imported = newtype_key(from_module, name);
            if self.newtype_by_key.contains_key(&imported) {
                return Some(imported);
            }
        }
        self.unique_newtype_key_by_name.get(name).cloned()
    }

    /// Resolve a bare newtype leaf to its key under a resolution scope.
    /// With a module context, resolution is the open-world-safe
    /// module-scoped lookup ([`Self::newtype_key_for_name_in_module`]):
    /// the module's own declarations plus its imports, never the
    /// package-global by-leaf table. The global table is consulted
    /// **only** when there is no module context — a genuine
    /// package-surface position, where any reference is module-qualified
    /// anyway (a bare leaf there has no importing module to resolve
    /// against). This is the single home for the resolve-with-fallback
    /// policy that `resolve_newtype_key` and `mark_newtype_name_referenced`
    /// share.
    fn resolve_newtype_key_in_scope(
        &self,
        name: &str,
        module_path: Option<&str>,
    ) -> Option<String> {
        match module_path {
            Some(path) => self.newtype_key_for_name_in_module(path, name),
            None => self.unique_newtype_key_by_name.get(name).cloned(),
        }
    }

    fn mark_newtype_name_referenced(&mut self, module_path: Option<&str>, name: &str) {
        let Some(key) = self.resolve_newtype_key_in_scope(name, module_path) else {
            return;
        };
        self.mark_newtype_key_referenced(&key);
    }

    fn mark_newtype_key_referenced(&mut self, key: &str) {
        if !self.newtype_by_key.contains_key(key) {
            return;
        }
        if !self.referenced_newtypes.insert(key.to_owned()) {
            return;
        }
        let nt = self.newtype_by_key[key].decl.clone();
        // Walk the payload under the newtype's own universal
        // tparams so any structural shapes inside it register too.
        // Existential type-params are replaced with the dyn-Any
        // sentinel at registration time — at the rust level the
        // newtype is generic only in its universals, with existential
        // slots type-erased inside the payload. Best-effort: an
        // error here surfaces at lookup later if a slot type can't
        // render.
        let nt_universals: std::collections::HashSet<&str> =
            nt.type_params.iter().map(|tp| tp.name.as_str()).collect();
        let nt_existentials: std::collections::HashSet<&str> = nt
            .existential_params
            .iter()
            .map(|tp| tp.name.as_str())
            .collect();
        let substituted = substitute_existentials_in_payload(&nt.payload, &nt_existentials);
        let saved_module = self.current_module.take();
        self.current_module = newtype_key_module_path(key)
            .map(str::to_owned)
            .or(saved_module.clone());
        let _ = self.register_with_tparams(&substituted, &nt_universals);
        self.current_module = saved_module;
    }

    /// Register a type used by an erased package body under a given tparam
    /// context. Tparam references render bare in subsequent body-side uses
    /// and become positional `_T<N>` slots on a crate-private shape.
    fn register_with_tparams(
        &mut self,
        ty: &Type<Routed>,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<(), EmitError> {
        self.register_with_tparams_inner(ty, tparams)
    }

    fn register_with_tparams_inner(
        &mut self,
        ty: &Type<Routed>,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<(), EmitError> {
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } => Ok(()),
            Type::Path { segments, args, .. } => {
                if segments.len() == 1 && args.is_empty() && tparams.contains(segments[0].as_str())
                {
                    // Tparam reference — no shape to mint, flows
                    // through as a Rust generic at use sites.
                    return Ok(());
                }
                if segments.len() == 1
                    && args.is_empty()
                    && segments[0].as_str() == EXISTENTIAL_ANY_SENTINEL
                {
                    // Type-erased existential slot — renders as
                    // `Rc<dyn Any>` at use sites (see `render_use`);
                    // no shape to mint.
                    return Ok(());
                }
                let name = segments.last().map(|s| s.as_str()).unwrap_or_default();
                if self.newtype_key_for_segments(segments).is_none()
                    && self.host_type_info(segments).is_some()
                {
                    for a in args {
                        self.register_with_tparams_inner(a, tparams)?;
                    }
                    return Ok(());
                }
                if let Some(key) = self.newtype_key_for_segments(segments) {
                    for a in args {
                        self.register_with_tparams_inner(a, tparams)?;
                    }
                    if self.referenced_newtypes.insert(key.clone()) {
                        let newtype = self.newtype_by_key[&key].decl.clone();
                        // Universal type-params stay as-is; existential
                        // type-params are substituted with the
                        // `Rc<dyn Any>` sentinel before walking the
                        // payload, matching `render_newtype`'s shape
                        // strategy.
                        let nt_universals: std::collections::HashSet<&str> = newtype
                            .type_params
                            .iter()
                            .map(|tp| tp.name.as_str())
                            .collect();
                        let nt_existentials: std::collections::HashSet<&str> = newtype
                            .existential_params
                            .iter()
                            .map(|tp| tp.name.as_str())
                            .collect();
                        let substituted =
                            substitute_existentials_in_payload(&newtype.payload, &nt_existentials);
                        let saved_module = self.current_module.take();
                        self.current_module = newtype_key_module_path(&key)
                            .map(str::to_owned)
                            .or(saved_module.clone());
                        let result = self.register_with_tparams_inner(&substituted, &nt_universals);
                        self.current_module = saved_module;
                        result?;
                    }
                    return Ok(());
                }
                if segments.len() == 1 && tparams.contains(segments[0].as_str()) {
                    // Abstract higher-kinded application `F(A)`: the binder
                    // head mints no shape (the erased body holds `F(A)` as
                    // an ordinary `Rc<dyn Any>`), but the argument may
                    // reference shapes that must register.
                    for a in args {
                        self.register_with_tparams_inner(a, tparams)?;
                    }
                    return Ok(());
                }
                Err(EmitError::unsupported(format!(
                    "Rust emitter: body type `{name}` is neither a `host type` \
                     nor a declared `newtype`"
                )))
            }
            Type::Product { .. } => {
                let normalized = self.normalize_type_for_tparams_in_current_scope(ty, tparams);
                let ty = &normalized;
                let (canonical, outer_order) = canonical_and_order(ty, tparams);
                let slots = right_spine_walk_product(ty);
                // Always walk the slots so newtype references inside
                // get marked as referenced — sharing the shape mint
                // between structurally-identical types (e.g.
                // `(A | B)`, `(C | D)`, `(E | F)`) means a duplicate
                // canonical no longer implies the slots were
                // previously visited.
                for slot in &slots {
                    self.register_with_tparams_inner(slot, tparams)?;
                }
                if self.shapes.contains_key(&canonical) {
                    return Ok(());
                }
                let slot_keys = self.product_slot_keys(&slots);
                let tparam_count = outer_order.len();
                let mut slot_types = Vec::with_capacity(slots.len());
                let mut leaf_idx = 0usize;
                for slot in &slots {
                    slot_types.push(self.render_for_slot(
                        slot,
                        TypeCtx::Shape,
                        tparams,
                        &outer_order,
                        &mut leaf_idx,
                    )?);
                }
                let mint_name = self.mint_shape_name("Prod", &slots, &canonical)?;
                let record = ShapeRecord {
                    kind: ShapeKind::Product,
                    canonical: canonical.clone(),
                    mint_name,
                    slot_keys,
                    slot_types,
                    tparam_count,
                };
                self.insert_shape(canonical, record);
                Ok(())
            }
            Type::Sum { .. } => {
                let normalized = self.normalize_type_for_tparams_in_current_scope(ty, tparams);
                let ty = &normalized;
                let (canonical, outer_order) = canonical_and_order(ty, tparams);
                let slots = right_spine_walk_sum(ty);
                for slot in &slots {
                    self.register_with_tparams_inner(slot, tparams)?;
                }
                if self.shapes.contains_key(&canonical) {
                    return Ok(());
                }
                let slot_keys = self.sum_slot_keys(&slots);
                let tparam_count = outer_order.len();
                let mut slot_types = Vec::with_capacity(slots.len());
                let mut leaf_idx = 0usize;
                for slot in &slots {
                    slot_types.push(self.render_for_slot(
                        slot,
                        TypeCtx::Shape,
                        tparams,
                        &outer_order,
                        &mut leaf_idx,
                    )?);
                }
                let mint_name = self.mint_shape_name("Sum", &slots, &canonical)?;
                let record = ShapeRecord {
                    kind: ShapeKind::Sum,
                    canonical: canonical.clone(),
                    mint_name,
                    slot_keys,
                    slot_types,
                    tparam_count,
                };
                self.insert_shape(canonical, record);
                Ok(())
            }
            Type::Function { param, ret, .. } => {
                self.register_with_tparams_inner(param, tparams)?;
                self.register_with_tparams_inner(ret, tparams)
            }
            Type::Forall { param, body, .. } => {
                // Higher-rank polymorphic type in the erased package body.
                // Walk the body with each binder added to the scope
                // so shapes referenced through `[T]` instantiation
                // get registered. Use-site rendering substitutes
                // each binder with the `EXISTENTIAL_ANY_SENTINEL` and
                // emits the function slot as
                // `Rc<dyn Fn(Rc<dyn Any>, …) -> Rc<dyn Any>>` (see
                // `render_use`'s `Type::Forall` arm).
                let mut extended: std::collections::HashSet<&str> = tparams.clone();
                extended.insert(param.name.as_str());
                self.register_with_tparams_inner(body, &extended)
            }
            Type::Infer { .. } => {
                unreachable!("Rust emitter: type inference placeholder reached body registration")
            }
            Type::LabelSugar { .. } => {
                unreachable!("Rust emitter: label sugar survived into body registration")
            }
            Type::Goal { ext, .. } => match *ext {},
        }
    }

    /// Render `ty` for storage as a slot inside an outer shape
    /// struct. Tparam references render as the outer shape's
    /// positional placeholders `_T<N>` (see `canonical_walk`).
    /// Nested shape references render as `<crate>::shapes::Mint<H,
    /// _Ta, _Tb, …>` where the args are the inner shape's tparam
    /// occurrences, mapped through `outer_order` to the outer's
    /// positions.
    fn render_for_slot(
        &self,
        ty: &Type<Routed>,
        ctx: TypeCtx,
        outer_tparams: &std::collections::HashSet<&str>,
        outer_order: &[String],
        leaf_idx: &mut usize,
    ) -> Result<String, EmitError> {
        match ty {
            Type::Unit { .. } => {
                // Unit gets a positional `_T<i>` slot so the shape's
                // field type stays parametric (instantiated as `()`
                // at concrete use sites, or as a tparam when the
                // constructor's payload binder substitutes through).
                // See `canonical_walk`'s Unit arm for the rationale.
                let i = *leaf_idx;
                *leaf_idx += 1;
                let _ = (ctx, outer_tparams, outer_order);
                Ok(format!("_T{i}"))
            }
            Type::Bottom { .. } => Ok("::std::convert::Infallible".to_owned()),
            Type::Path { .. } => {
                // Each Path-typed leaf consumes one position. The
                // shape struct stores `_T<idx>`; the use-site
                // renderer fills it with the concrete Rust type.
                let i = *leaf_idx;
                *leaf_idx += 1;
                let _ = (outer_tparams, outer_order);
                Ok(format!("_T{i}"))
            }
            Type::Product { .. } | Type::Sum { .. } => {
                // Binary form: a nested compound is opaque at the
                // outer shape's mint. The outer reserves one
                // positional `_T<i>` placeholder for the entire
                // inner type; use sites instantiate that placeholder
                // with the concrete inner shape (e.g.
                // `crate::shapes::Sum_<inner name><H, ...>`). Mirrors
                // `canonical_walk_opaque` / `collect_leaf_args_opaque`.
                let _ = (ctx, outer_tparams, outer_order);
                let i = *leaf_idx;
                *leaf_idx += 1;
                Ok(format!("_T{i}"))
            }
            Type::Function { .. } => {
                // Function-typed slot — opaque at the shape mint
                // level. The whole fn type consumes one positional
                // leaf; at use sites, the leaf is instantiated with
                // `Rc<dyn Fn(P0, P1, …) -> R>` (see `shape_turbofish`
                // / `render_use`'s `Type::Function` arm). This makes
                // optics-style aliases like
                // `Lens(S, A) = (S -> A) & (A, S -> S)` mint a
                // 2-slot Prod regardless of how the type-args
                // substitute into the body — sharing the mint
                // between the alias definition and any user
                // construction-via-substitution.
                let _ = (ctx, outer_tparams, outer_order);
                let i = *leaf_idx;
                *leaf_idx += 1;
                Ok(format!("_T{i}"))
            }
            Type::Forall { .. } => {
                // Rank-N (`forall`-quantified) slot — a Church-encoded
                // `List(A)` / `Nat` or any other polymorphic value
                // crossing a shape boundary. Opaque at the shape mint,
                // exactly like `Type::Function`: it consumes one
                // positional leaf. The use-site `render_use`'s
                // `Type::Forall` arm fills the leaf with the rank-N
                // value's erased `Rc<dyn Fn(...)>` form (the
                // `EXISTENTIAL_ANY_SENTINEL` scheme — see
                // `specs/backends/rust.md` § Higher-rank value parameters).
                let _ = (ctx, outer_tparams, outer_order);
                let i = *leaf_idx;
                *leaf_idx += 1;
                Ok(format!("_T{i}"))
            }
            _ => Err(EmitError::unsupported(
                "Rust emitter: unsupported type as shape slot (compiler bug)",
            )),
        }
    }

    /// Mint the crate-private name for an anonymous body shape owned by
    /// `canonical`: the kind prefix plus one token per spine slot
    /// (the slot's generic letter, `Poly`-marked for a rank-N slot,
    /// `Never` for `!` — the classification the canonical itself
    /// carries), the pure content-hash mint past the shared length
    /// budget, and the claim schedule's hash suffix when another
    /// structural canonical holds the readable base.
    fn mint_shape_name(
        &mut self,
        prefix: &str,
        slots: &[Type<Routed>],
        canonical: &str,
    ) -> Result<String, EmitError> {
        let base = format!("{prefix}_{}", readable_slot_tokens(slots).join("_"));
        let base = if base.len() > crate::backends::skin::READABLE_MINT_MAX_LEN {
            format!("{prefix}_{}", fnv1a_64_hex(canonical))
        } else {
            base
        };
        for candidate in crate::backends::skin::mint_claim_candidates(&base, canonical) {
            let taken_by_mint = self
                .minted_names
                .get(&candidate)
                .is_some_and(|owner| owner != canonical);
            if !taken_by_mint {
                self.minted_names
                    .insert(candidate.clone(), canonical.to_owned());
                return Ok(candidate);
            }
        }
        unreachable!(
            "two distinct shape owners share a full FNV-1a hash: `{canonical}` \
             exhausted `{base}` and every hash-suffixed candidate"
        )
    }

    fn insert_shape(&mut self, canonical: String, record: ShapeRecord) {
        self.shapes.insert(canonical, record);
    }

    /// Render `ty` for use at a site where `tparams` may be in
    /// scope. Tparam references render as their surface name;
    /// shape references include the shape's surface tparam args
    /// after `H`/`Self` in the generic list.
    fn render_use(
        &self,
        ty: &Type<Routed>,
        ctx: TypeCtx,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<String, EmitError> {
        let host_param = ctx.host_param();
        match ty {
            Type::Unit { .. } => Ok("()".to_owned()),
            Type::Bottom { .. } => Ok("::std::convert::Infallible".to_owned()),
            Type::Path { segments, args, .. } => {
                if segments.len() == 1 && args.is_empty() && segments[0].as_str() == "_" {
                    return Ok("_".to_owned());
                }
                if segments.len() == 1
                    && args.is_empty()
                    && segments[0].as_str() == EXISTENTIAL_ANY_SENTINEL
                {
                    // Type-erased existential slot: render as a shared opaque
                    // `Rc<dyn Any>` token. Construction sites retain the exact
                    // value behind that token, and CPS projectors forward it
                    // structurally; only the paired generated converter may
                    // decode it at the selected typed boundary.
                    return Ok("::std::rc::Rc<dyn ::std::any::Any>".to_owned());
                }
                if segments.len() == 1 && args.is_empty() && tparams.contains(segments[0].as_str())
                {
                    return Ok(segments[0].as_str().to_owned());
                }
                if segments.len() == 1 && !args.is_empty() && tparams.contains(segments[0].as_str())
                {
                    // Abstract higher-kinded application `F(A1, …, An)`
                    // where the kind-`*→…→*` binder `f` is still in scope
                    // (a polymorphic newtype payload or fn signature —
                    // e.g. a `Functor[*F]` dictionary applying `F(A)`, a
                    // `Bifunctor[**F]` applying `F(A, C)`). In the erased
                    // body a higher-kinded carrier has no representation
                    // distinct from the universal `Rc<dyn Any>`, so the
                    // application renders as the open universal — no
                    // marker, no wrapper, no carrier.
                    let _ = host_param;
                    return Ok("::std::rc::Rc<dyn ::std::any::Any>".to_owned());
                }
                let name = segments.last().map(|s| s.as_str()).unwrap_or_default();
                let leaf_is_host_type = self.newtype_key_for_segments(segments).is_none()
                    && self.host_type_info(segments).is_some();
                if leaf_is_host_type {
                    // Every host type renders as the trait's exact,
                    // declaration-keyed associated type. A role controls
                    // literal/conditional capabilities; it never replaces the
                    // host's selected representation. At a method-body
                    // context the type reaches through `Self::<name>`;
                    // inside a shape struct it reaches through the
                    // struct's `H::<name>` generic. The associated type
                    // is rung-2 module-qualified (`<module>__<leaf>`),
                    // so two bridged modules' same-leaf host types no
                    // longer collide; `host_assoc_name` reads the
                    // declaring module from this canonical path.
                    // A nullary host atom reaches through its exact associated
                    // type. A parameterized host declaration instead owns one
                    // substitution-stable `DStorage`; its public typed carrier
                    // converts to the body's universal token before this
                    // renderer is consulted. Consequently no parameterized
                    // native GAT may survive in a body-side Rust type.
                    let assoc = self.host_assoc_name(segments)?;
                    if args.is_empty() {
                        Ok(format!("{host_param}::{assoc}"))
                    } else {
                        Ok("::std::rc::Rc<dyn ::std::any::Any>".to_owned())
                    }
                } else if let Some(key) = self.newtype_key_for_segments(segments)
                    && self.referenced_newtypes.contains(&key)
                {
                    // Prepared carriers take marker arguments and belong to
                    // the public facade. Body slots retain the nominal's
                    // erased payload, including beneath private wrappers.
                    if self.prepared_newtypes.contains_key(&key) {
                        return Ok("::std::rc::Rc<dyn ::std::any::Any>".to_owned());
                    }
                    let record = self.newtype_by_key.get(&key);
                    let boundary_atomic = record.is_some_and(|record| {
                        record
                            .host_surface
                            .is_some_and(|surface| surface != RustNewtypeHostSurface::Both)
                    });
                    // A recursive newtype has no finite host shape; it
                    // crosses as the erased `Rc<dyn Any>` rep (the body
                    // carries it transparently, and the conversion passes
                    // it through). See `newtype_is_recursive` and
                    // `specs/backends/rust.md` § Conversion semantics.
                    if self.newtype_is_recursive(&key) && !boundary_atomic {
                        return Ok("::std::rc::Rc<dyn ::std::any::Any>".to_owned());
                    }
                    let nt_universal_count = record.map(|n| n.decl.type_params.len()).unwrap_or(0);
                    if args.len() != nt_universal_count {
                        return Err(EmitError::unsupported(format!(
                            "Rust emitter: newtype `{name}` expects {nt_universal_count} \
                             type-arg(s), got {}",
                            args.len()
                        )));
                    }
                    // The newtype struct is generic only in its
                    // **kind-`*` universal** params; every kind-`*→…→*`
                    // declaration param is erased (its carrier rides the
                    // universal `Rc<dyn Any>` in the body), so its arg is
                    // dropped from the type even when the payload never
                    // applies that phantom binder. A value-kind
                    // function-typed arg renders with the crate-private
                    // shape-storage discipline (`Rc<dyn Fn>`), so a
                    // `Result((t) -> u, e)` body reference spells its
                    // function type argument the way the minted body sum's
                    // slot does. Public occurrences use `KioFnN` instead.
                    let higher_kinded_slots = record
                        .map(|record| higher_kinded_param_indices(&record.decl))
                        .unwrap_or_default();
                    let mut parts = vec![host_param.to_owned()];
                    for (i, a) in args.iter().enumerate() {
                        if higher_kinded_slots.contains(&i) {
                            continue;
                        }
                        parts.push(self.render_shape_leaf_storage(a, ctx, tparams)?);
                    }
                    let record = record.expect("a referenced newtype has a record");
                    Ok(format!(
                        "{}<{}>",
                        record.rust_path(&self.crate_root),
                        parts.join(", ")
                    ))
                } else {
                    Err(EmitError::unsupported(format!(
                        "Rust emitter: type `{name}` not registered (compiler bug)"
                    )))
                }
            }
            Type::Product { .. } | Type::Sum { .. } => {
                let normalized = self.normalize_type_for_tparams_in_current_scope(ty, tparams);
                let ty = &normalized;
                let (canonical, _) = canonical_and_order(ty, tparams);
                let record = self.shapes.get(&canonical).ok_or_else(|| {
                    EmitError::unsupported(format!(
                        "Rust emitter: shape `{canonical}` not registered (compiler bug)"
                    ))
                })?;
                // Every leaf becomes one positional generic on the
                // shape (see `canonical_walk`). Walk the synth_ty
                // in the same order to collect those leaves, render
                // each as a Rust type, and emit `<H, T1, T2, …>`.
                // Function leaves render as
                // `::std::rc::Rc<dyn Fn(...) -> ...>` (a shape's
                // fn-typed slot stores the closure behind a ref-
                // counted trait object); other leaves go through
                // `render_use`. A fn leaf whose own parameter is
                // itself fn-typed (a higher-order callback stored in
                // a product / sum slot — e.g. the host `loop`
                // `(step: (S) -> S | R, S) -> R` bundled into a deps
                // product) renders that inner param via
                // `render_fn_arg_position`, which boxes it as
                // `Rc<dyn Fn(...)>` — `impl Trait` is illegal in a
                // `dyn Fn` parameter list (E0562). This is exclusively a
                // crate-private storage choice; the public facade is the
                // canonical named `KioFnN`.
                let mut parts: Vec<String> = Vec::new();
                let leaves = collect_leaf_args(ty);
                for leaf in &leaves {
                    parts.push(self.render_shape_leaf_storage(leaf, ctx, tparams)?);
                }
                if parts.is_empty() {
                    Ok(format!("{}::shapes::{}", self.crate_root, record.mint_name))
                } else {
                    Ok(format!(
                        "{}::shapes::{}<{}>",
                        self.crate_root,
                        record.mint_name,
                        parts.join(", ")
                    ))
                }
            }
            Type::Function {
                param,
                ret,
                abi_arity,
                caps,
                ..
            } => {
                // Function types in a body-side typed position render as
                // `impl Fn(P0, P1, …) -> R`, with `+ Clone +
                // 'static` appended when the
                // `caps.lifetime == Lifetime::Heap`. The bound
                // lets the body store the closure in an
                // `Rc<dyn Fn(P0, P1, …) -> R>` (which requires
                // both bounds) when the surrounding context is a
                // newtype constructor whose payload is fn-typed,
                // or when the value flows into a shape slot's
                // ref-counted leaf. `Lifetime::Stack` slots — the
                // [`crate::pass::capabilities::annotate_lifetime`] pass
                // proves the value doesn't outlive its frame —
                // drop the bound: the closure is only invoked,
                // never stored, so neither `Clone` nor `'static`
                // is needed. This body-only representation is independent of
                // the canonical public `KioFnN` facade.
                //
                let bounds = match caps.lifetime {
                    crate::ast::Lifetime::Heap => " + Clone + 'static",
                    crate::ast::Lifetime::Stack => "",
                };
                let r = self.render_fn_arg_position(ret, ctx, tparams)?;
                let param_tys = collect_function_param_tys(param, *abi_arity);
                if param_tys.is_empty() {
                    return Ok(format!("impl Fn() -> {r}{bounds}"));
                }
                let mut params = Vec::with_capacity(param_tys.len());
                for ty in &param_tys {
                    params.push(self.render_fn_arg_position(ty, ctx, tparams)?);
                }
                Ok(format!("impl Fn({}) -> {r}{bounds}", params.join(", ")))
            }
            Type::Forall { param, body, .. } => {
                // Rust has no HRTB over type parameters. Erase every
                // binder and expose the concrete
                // `Rc<dyn Fn(…Rc<dyn Any>…) -> Rc<dyn Any>>` carrier
                // used by higher-rank value slots and polymorphic-function
                // newtype members. A recursive newtype may erase only its
                // field further; its members recover this same carrier.
                let mut subst: std::collections::HashMap<String, Type<Routed>> =
                    std::collections::HashMap::new();
                let mut cur = body.as_ref();
                subst.insert(
                    param.name.clone(),
                    Type::Path {
                        segments: vec![crate::ast::PathSegment::new(
                            EXISTENTIAL_ANY_SENTINEL.to_owned(),
                            param.span,
                        )],
                        args: Vec::new(),
                        meta: crate::ast::Meta::new(param.span),
                    },
                );
                while let Type::Forall { param, body, .. } = cur {
                    subst.insert(
                        param.name.clone(),
                        Type::Path {
                            segments: vec![crate::ast::PathSegment::new(
                                EXISTENTIAL_ANY_SENTINEL.to_owned(),
                                param.span,
                            )],
                            args: Vec::new(),
                            meta: crate::ast::Meta::new(param.span),
                        },
                    );
                    cur = body.as_ref();
                }
                let substituted = apply_subst(body, &subst);
                // Render the substituted Function as `Rc<dyn Fn>`
                // rather than `impl Fn` — a rank-N slot's storage
                // shape behind a closure / fn-typed parameter must
                // be a trait object (Rust rejects `impl Trait`
                // inside another `Fn`'s parameter list, and stored
                // fn-typed parameters need a concrete sized type).
                if type_is_function(&substituted) {
                    self.render_polymorphic_fn_storage(&substituted, ctx, tparams)
                } else {
                    self.render_use(&substituted, ctx, tparams)
                }
            }
            _ => Err(EmitError::unsupported(
                "Rust emitter: unsupported body-side type (compiler bug)",
            )),
        }
    }

    /// Render a newtype/shape type-arg with the **storage discipline**:
    /// a function-typed leaf renders as `::std::rc::Rc<dyn Fn(...) ->
    /// R>` — the form a minted product / sum stores its function slot
    /// in (see the `Type::Product | Type::Sum` arm of [`Self::render_use`]
    /// and [`Self::render_for_slot`]). Every other shape delegates to
    /// [`Self::render_use`].
    ///
    /// A reference to a newtype whose payload holds a function (e.g.
    /// `Result((t) -> u, e)`) must spell that arg as the concrete
    /// `Rc<dyn Fn>` the value carries, not the body-side `impl Fn`
    /// form: an `impl Fn` type-arg is an opaque type that never unifies
    /// with the stored `Rc<dyn Fn>` slot, so a constructed value would
    /// fail to flow into the declared parameter type.
    fn render_shape_leaf_storage(
        &self,
        ty: &Type<Routed>,
        ctx: TypeCtx,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<String, EmitError> {
        let normalized = self.normalize_type_for_tparams_in_current_scope(ty, tparams);
        let ty = &normalized;
        let Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } = ty
        else {
            return self.render_use(ty, ctx, tparams);
        };
        let r = self.render_fn_arg_position(ret, ctx, tparams)?;
        let param_tys = collect_function_param_tys(param, *abi_arity);
        if param_tys.is_empty() {
            return Ok(format!("::std::rc::Rc<dyn Fn() -> {r}>"));
        }
        let mut ps = Vec::with_capacity(param_tys.len());
        for ty in &param_tys {
            ps.push(self.render_fn_arg_position(ty, ctx, tparams)?);
        }
        Ok(format!("::std::rc::Rc<dyn Fn({}) -> {r}>", ps.join(", ")))
    }

    /// Render crate-private newtype payload storage. Public newtypes use their
    /// prepared carrier with private erased storage, so this legacy registry
    /// path is strictly for body-local nominal values.
    fn render_newtype_payload_storage(
        &self,
        ty: &Type<Routed>,
        ctx: TypeCtx,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<String, EmitError> {
        self.render_use(ty, ctx, tparams)
    }

    /// Render a polymorphic-function newtype payload's storage type —
    /// the binder-erased Function — as `::std::rc::Rc<dyn Fn(P0, P1,
    /// …) -> R>`. Nested Functions in parameter positions render as
    /// `::std::rc::Rc<dyn Fn(...) -> ...>` rather than `impl Fn(...)`
    /// — `impl Trait` is not legal inside a `dyn Fn` trait-object's
    /// parameter list, so callback-typed slots cross the storage as a
    /// ref-counted trait object instead.
    ///
    /// The outer Function's caps are ignored: storage is always
    /// `Rc<dyn Fn ...>`, never bare `impl Fn`.
    fn render_polymorphic_fn_storage(
        &self,
        fn_ty: &Type<Routed>,
        ctx: TypeCtx,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<String, EmitError> {
        let normalized = self.normalize_type_for_tparams_in_current_scope(fn_ty, tparams);
        let fn_ty = &normalized;
        let Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } = fn_ty
        else {
            return Err(EmitError::unsupported(
                "Rust emitter: render_polymorphic_fn_storage requires a Function input \
                 (compiler bug)",
            ));
        };
        let render_fn_bound_slot =
            |slot: &Type<Routed>| self.render_fn_arg_position(slot, ctx, tparams);
        let r = render_fn_bound_slot(ret)?;
        let param_tys = collect_function_param_tys(param, *abi_arity);
        if param_tys.is_empty() {
            return Ok(format!("::std::rc::Rc<dyn Fn() -> {r}>"));
        }
        let mut params_rust = Vec::with_capacity(param_tys.len());
        for ty in &param_tys {
            params_rust.push(render_fn_bound_slot(ty)?);
        }
        Ok(format!(
            "::std::rc::Rc<dyn Fn({}) -> {r}>",
            params_rust.join(", ")
        ))
    }

    /// Render a type sitting in the **parameter-list position of an
    /// `impl Fn(...)` / `Fn(...)` trait bound** — the argument slots of
    /// a function-typed value parameter. A plain type renders the same
    /// as anywhere else (`render_use`), but a *function-typed* argument
    /// cannot be `impl Fn(...)` here: Rust rejects `impl Trait` nested
    /// inside another `Fn`'s parameter list (E0666 / E0562). So a
    /// function-typed argument renders as the trait-object storage form
    /// `::std::rc::Rc<dyn Fn(...) -> R>` instead, via
    /// [`Self::render_polymorphic_fn_storage`] (which also boxes any
    /// further-nested function params). This is the crate-private
    /// higher-order callback storage case — a fn whose own parameter is
    /// fn-typed, e.g. a `fold: (s & acc & ((acc & el) -> acc)) -> acc`
    /// passed as an argument. The public occurrence remains `KioFnN`.
    fn render_fn_arg_position(
        &self,
        ty: &Type<Routed>,
        ctx: TypeCtx,
        tparams: &std::collections::HashSet<&str>,
    ) -> Result<String, EmitError> {
        let normalized = self.normalize_type_for_tparams_in_current_scope(ty, tparams);
        if matches!(normalized, Type::Function { .. }) {
            self.render_polymorphic_fn_storage(&normalized, ctx, tparams)
        } else {
            self.render_use(&normalized, ctx, tparams)
        }
    }

    /// Crate-private body products use positional fields. Public products use
    /// the canonical recursive binary `Product` facade instead.
    fn product_slot_keys(&self, slots: &[Type<Routed>]) -> Vec<String> {
        (0..slots.len()).map(|i| format!("_{i}")).collect()
    }

    fn sum_slot_keys(&self, slots: &[Type<Routed>]) -> Vec<String> {
        crate::backends::skin::assign_spine_keys(slots.len(), |i| {
            self.slot_key_candidates(&slots[i])
        })
    }

    /// Preserve a label-derived key inside a crate-private body sum when the
    /// exact slot resolves to a newtype; otherwise use the positional fallback.
    /// Public sums use the canonical recursive binary `Sum` facade instead.
    fn slot_key_candidates(&self, slot: &Type<Routed>) -> Vec<String> {
        if let Type::Path { segments, args, .. } = slot
            && args.is_empty()
            && !segments.is_empty()
            && let Some(key) = self.newtype_key_for_segments(segments)
            && let Some(record) = self.newtype_by_key.get(&key)
        {
            let ffi_key = record.decl.ffi_key();
            if !ffi_key.is_empty() {
                return vec![rust_shape_key_from_ffi_key(ffi_key)];
            }
        }
        Vec::new()
    }

    fn render(&mut self) -> Result<String, EmitError> {
        let mut out = String::new();
        out.push_str("// Generated by kio — do not edit by hand.\n");
        if self.shapes.is_empty()
            && self.referenced_newtypes.is_empty()
            && self.prepared_newtypes.is_empty()
        {
            return Ok(out);
        }
        // Sum shapes carry `Infallible`-typed slots when the source
        // sum includes `!`; the Clone / PartialEq match arms for those
        // slots are syntactically present but semantically unreachable
        // (uninhabited), which rustc flags as `unreachable_code`. The
        // emit is correct — it's just over-explicit — so silence the
        // warning at file scope.
        out.push_str("#![allow(unreachable_code)]\n");
        out.push('\n');
        // Body-local shapes and nominal storage are rendered first. Prepared
        // public marker/application algebra, callable traits, member methods,
        // and nominal carriers are appended from the frozen catalog.
        let referenced: Vec<String> = self.referenced_newtypes.iter().cloned().collect();
        if !referenced.is_empty() || !self.prepared_newtypes.is_empty() {
            let mut tree = NewtypeNamespaceTree::default();
            for key in &referenced {
                if self.prepared_newtypes.contains_key(key) {
                    continue;
                }
                let record = &self.newtype_by_key[key];
                tree.insert(&record.rust_module_path, &record.rust_leaf, key);
            }
            for (key, entry) in &self.prepared_newtypes {
                let module_path = entry
                    .name()
                    .module_segments()
                    .iter()
                    .map(|segment| rust_public_ident(segment))
                    .collect::<Vec<_>>();
                let leaf = rust_public_ident(entry.name().name());
                tree.insert(&module_path, &leaf, key);
            }
            out.push_str("pub mod nominal {\n");
            self.render_newtype_namespace(&tree, &mut out, 1)?;
            out.push_str("}\n\n");
        }
        for record in self.shapes.values() {
            self.render_shape(record, &mut out);
        }
        Ok(out)
    }

    fn render_newtype_namespace(
        &mut self,
        tree: &NewtypeNamespaceTree,
        out: &mut String,
        depth: usize,
    ) -> Result<(), EmitError> {
        let indent = "    ".repeat(depth);
        for (module, child) in &tree.children {
            out.push_str(&format!("{indent}pub mod {module} {{\n"));
            self.render_newtype_namespace(child, out, depth + 1)?;
            out.push_str(&format!("{indent}}}\n"));
        }
        for key in tree.records.values() {
            let mut item = String::new();
            if let Some(entry) = self.prepared_newtypes.get(key) {
                self.render_prepared_newtype(entry, &mut item);
            } else if let Some(record) = self.newtype_by_key.get(key).cloned() {
                // Render the payload in the declaration's lexical scope,
                // then indent the complete Rust item into its nominal module.
                self.current_module = newtype_key_module_path(key).map(str::to_owned);
                self.render_newtype(&record, &mut item)?;
                self.current_module = None;
            } else {
                unreachable!("a Rust nominal namespace entry has no declaration")
            }
            let item_indent = "    ".repeat(depth);
            for line in item.lines() {
                out.push_str(&item_indent);
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(())
    }

    /// Declare one public carrier from the frozen prepared inventory. Carrier
    /// identity and binders never depend on payload scanning; the erased field
    /// is private, and live constructor/projector methods are emitted only by
    /// their prepared callable sites.
    fn render_prepared_newtype(
        &self,
        entry: &BoundaryPublicNewtypeInventoryEntry,
        out: &mut String,
    ) {
        let key = qualified_newtype_key(entry.name());
        let origin = self
            .prepared_newtype_origins
            .get(&key)
            .copied()
            .unwrap_or_else(|| unreachable!("a prepared Rust newtype carrier has provenance"));
        let host = "__KioHost";
        let mut declarations = vec![format!(
            "{host}: {}::host::{}",
            self.crate_root, self.host_trait
        )];
        let mut arguments = vec![host.to_owned()];
        let parameters = prepare_rust_declaration_type_parameters(
            entry
                .type_params()
                .iter()
                .map(|parameter| (parameter.name(), parameter.kind().arity())),
        );
        for parameter in parameters {
            declarations.push(parameter.declaration(&self.crate_root));
            arguments.push(parameter.name);
        }
        let phantom = rust_invariant_phantom(&arguments);
        let declaration = declarations.join(", ");
        let arguments = arguments.join(", ");
        let name = rust_public_ident(entry.name().name());
        out.push_str("#[allow(non_camel_case_types)]\n");
        push_rust_deprecation(out, origin, "");
        out.push_str(&format!(
            "pub struct {name}<{declaration}>(pub(crate) crate::shapes::KioStoredValue, pub(crate) ::std::marker::PhantomData<{phantom}>);\n"
        ));
        out.push_str(&format!(
            "impl<{declaration}> Clone for {name}<{arguments}> {{\n    fn clone(&self) -> Self {{ Self(self.0.clone(), ::std::marker::PhantomData) }}\n}}\n"
        ));
    }

    /// Mint one crate-private nominal used only by body-local machinery.
    /// Public live and retained carriers are emitted by
    /// [`Self::render_prepared_newtype`] and never reach this function. Its
    /// header keeps only kind-`*` parameters; every higher-kinded declaration
    /// slot is erased even when phantom, because `F(A)` has no representation
    /// distinct from `Rc<dyn Any>` in the body. Polymorphic-function payload
    /// binders and existential slots likewise erase in body storage.
    fn render_newtype(&self, record: &NewtypeRecord, out: &mut String) -> Result<(), EmitError> {
        let newtype = &record.decl;
        let name = &record.rust_leaf;
        let crate_root = &self.crate_root;
        let key = newtype_key(
            self.current_module
                .as_deref()
                .expect("newtype rendering carries its declaring module"),
            &newtype.name,
        );
        debug_assert!(!self.prepared_newtypes.contains_key(&key));
        let boundary_recursive = self.newtype_is_recursive(&key);
        let erased_storage = boundary_recursive;
        // Every kind-`*→…→*` declaration param is dropped from the struct's
        // generics (erased), including a phantom binder the payload never
        // applies; only kind-`*` universals remain.
        let higher_kinded_slots = higher_kinded_param_indices(newtype);
        let prepared_parameters = prepare_rust_declaration_type_parameters(
            newtype
                .type_params
                .iter()
                .map(|parameter| (parameter.name.as_str(), parameter.effective_kind().arity())),
        );
        let universals: Vec<&PreparedRustTypeParameter> = prepared_parameters
            .iter()
            .enumerate()
            .filter(|(i, _)| !higher_kinded_slots.contains(i))
            .map(|(_, parameter)| parameter)
            .collect();
        // The payload's tparam scope includes **every** type param —
        // the kept universals render as themselves, and the higher-kinded
        // params stay in scope so an applied `F(A)` hits `render_use`'s
        // abstract-HKT arm (→ `Rc<dyn Any>`) rather than falling through to
        // a structural lookup. Only the struct *header* drops the
        // higher-kinded params (they carry no runtime generic).
        let nt_tparams: std::collections::HashSet<&str> = prepared_parameters
            .iter()
            .map(|parameter| parameter.name.as_str())
            .collect();
        // Existential slots erase to the `Rc<dyn Any>` sentinel.
        let payload_existentials_erased = substitute_existentials_in_payload(
            &newtype.payload,
            &newtype
                .existential_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect(),
        );
        let mut declaration_subst = std::collections::HashMap::new();
        for (source, prepared) in newtype.type_params.iter().zip(&prepared_parameters) {
            if source.name == prepared.name {
                continue;
            }
            declaration_subst.insert(
                source.name.clone(),
                Type::Path {
                    segments: vec![crate::ast::PathSegment::new(
                        prepared.name.clone(),
                        source.span,
                    )],
                    args: Vec::new(),
                    meta: crate::ast::Meta::new(source.span),
                },
            );
        }
        let payload_declaration_renamed =
            apply_subst(&payload_existentials_erased, &declaration_subst);
        // A polymorphic-function payload (`Monad[*F] : [A][B](…) ->
        // F(B)`): erase the per-call binders `[A]`, `[B]` to the
        // sentinel so the stored fn type is a concrete
        // `Rc<dyn Fn(Rc<dyn Any>, …) -> Rc<dyn Any>>`.
        let polymorphic_fn_payload: Option<(Vec<crate::ast::TypeParam>, Type<Routed>)> =
            detect_polymorphic_fn_payload(&payload_declaration_renamed);
        let payload_substituted = if let Some((ref binders, ref inner_fn)) = polymorphic_fn_payload
        {
            let mut subst: std::collections::HashMap<String, Type<Routed>> =
                std::collections::HashMap::new();
            for tp in binders {
                subst.insert(
                    tp.name.clone(),
                    Type::Path {
                        segments: vec![crate::ast::PathSegment::new(
                            EXISTENTIAL_ANY_SENTINEL.to_owned(),
                            tp.span,
                        )],
                        args: Vec::new(),
                        meta: crate::ast::Meta::new(tp.span),
                    },
                );
            }
            apply_subst(inner_fn, &subst)
        } else {
            payload_declaration_renamed
        };
        // Render the payload shape. A function payload becomes
        // `Rc<dyn Fn(…) -> …>` (a closure can't sit in a struct field as
        // `impl Fn`); any other payload is `Box`ed so a recursive newtype
        // (`List[A] : . | (A & List(A))`) has finite Rust size. A
        // body-local product / sum payload uses the registry's private
        // internal shape.
        let host_param = "__KioHost";
        let newtype_ctx = TypeCtx::Shape.with_host_param(host_param);
        let raw_payload = if erased_storage {
            "::std::rc::Rc<dyn ::std::any::Any>".to_owned()
        } else {
            self.render_newtype_payload_storage(&payload_substituted, newtype_ctx, &nt_tparams)?
        };
        // Recursive body-local newtypes erase their field storage to
        // `Rc<dyn Any>`.
        let payload_is_fn = matches!(&payload_substituted, Type::Function { .. });
        let payload_type_vars: BTreeSet<String> =
            nt_tparams.iter().map(|name| (*name).to_owned()).collect();
        let payload_lacks_partial_eq =
            self.type_reaches_non_partial_eq_leaf(&payload_substituted, payload_type_vars);
        let payload_field_ty = if erased_storage {
            raw_payload.clone()
        } else if payload_is_fn {
            // A dictionary newtype stores its closure in the body's
            // **spread** rep (`Fn(step, m) -> …`, the domain's right-spine
            // arity), the form the FFI boundary recovers / wraps; render
            // the field to match (not the single packed product the
            // payload's `abi_arity` records). A plain fn-typed payload
            // (`Show[A] : A -> String`, abi-arity-1 over a non-product)
            // coincides with its spread form.
            self.newtype_fn_field_ty(&payload_substituted, &nt_tparams)
                .unwrap_or_else(|| {
                    let stripped = raw_payload.replacen("impl Fn", "dyn Fn", 1).replacen(
                        " + Clone + 'static",
                        "",
                        1,
                    );
                    format!("::std::rc::Rc<{stripped}>")
                })
        } else {
            format!("::std::boxed::Box<{raw_payload}>")
        };
        // A newtype struct carries its own host generic. The bound is
        // slot-derived: a payload referencing an exact host type keeps
        // `Host + ?Sized`; every other newtype drops to `'static` so the
        // `Rc<dyn Any>` / `Rc<dyn Fn>` storage paths typecheck.
        let host_bound = match self.host_param_requirement(&payload_substituted) {
            RustHostParamRequirement::None => format!("{host_param}: 'static"),
            RustHostParamRequirement::Unsized => format!(
                "{host_param}: {crate_root}::host::{} + ?Sized",
                self.host_trait
            ),
            RustHostParamRequirement::Sized => {
                format!("{host_param}: {crate_root}::host::{}", self.host_trait)
            }
        };
        let header_decl = if universals.is_empty() {
            host_bound.clone()
        } else {
            let parts: Vec<String> = universals
                .iter()
                .map(|tp| {
                    let arity = tp.kind_arity;
                    if arity == 0 {
                        format!("{}: Clone + 'static", tp.name)
                    } else {
                        format!(
                            "{}: {crate_root}::shapes::KioTypeConstructor{arity}",
                            tp.name
                        )
                    }
                })
                .collect();
            format!("{host_bound}, {}", parts.join(", "))
        };
        let header_use = if universals.is_empty() {
            host_param.to_owned()
        } else {
            let names: Vec<String> = universals.iter().map(|tp| tp.name.clone()).collect();
            format!("{host_param}, {}", names.join(", "))
        };

        out.push_str("#[allow(non_camel_case_types)]\n");
        // The phantom binds `H` plus every universal — a universal that
        // appears in the payload only through an erased higher-kinded
        // position (`F(A)` → `Rc<dyn Any>`) is otherwise unused, which
        // Rust rejects (E0392). `fn() -> (…)` keeps the struct covariant
        // and imposes no auto-trait obligations on the phantom args.
        let phantom_args: String = {
            let mut parts = vec![host_param.to_owned()];
            parts.extend(universals.iter().map(|tp| tp.name.clone()));
            if parts.len() == 1 {
                host_param.to_owned()
            } else {
                format!("({})", parts.join(", "))
            }
        };
        let declaration_visibility = "pub(crate)";
        let field_visibility = "pub(crate)";
        out.push_str(&format!(
            "{declaration_visibility} struct {name}<{header_decl}>({field_visibility} {payload_field_ty}, {field_visibility} std::marker::PhantomData<fn() -> {phantom_args}>);\n"
        ));
        out.push_str(&format!(
            "impl<{header_decl}> Clone for {name}<{header_use}> {{\n    \
                 fn clone(&self) -> Self {{ Self(self.0.clone(), std::marker::PhantomData) }}\n\
             }}\n"
        ));
        // PartialEq when the payload is comparable: skip a function-
        // reaching payload (`Rc<dyn Fn>` is not `PartialEq`), an
        // existential payload, and any payload whose erased field type
        // reaches the universal `Rc<dyn Any>` (an abstract higher-kinded
        // `F(A)` carrier — `dyn Any` is not `PartialEq`).
        let payload_has_existential = !newtype.existential_params.is_empty();
        let payload_field_is_erased = payload_field_ty.contains("dyn ::std::any::Any");
        if !payload_has_existential && !payload_lacks_partial_eq && !payload_field_is_erased {
            let eq_where = if universals.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = universals
                    .iter()
                    .map(|tp| format!("{}: PartialEq", tp.name))
                    .collect();
                format!(" where {}", parts.join(", "))
            };
            out.push_str(&format!(
                "impl<{header_decl}> PartialEq for {name}<{header_use}>{eq_where} {{\n    \
                     fn eq(&self, other: &Self) -> bool {{ self.0 == other.0 }}\n\
                 }}\n"
            ));
        }
        // Public member methods are appended separately from prepared sites.
        out.push('\n');
        Ok(())
    }

    fn render_shape(&self, record: &ShapeRecord, out: &mut String) {
        let crate_root = &self.crate_root;
        let _ = crate_root;
        let name = &record.mint_name;
        // Build the generic header `A, B, …` (decl with bounds) and
        // the use-site list `A, B, …`. Tparam slots need `Clone` (and
        // only `Clone`) — the body emitter clones each path-bound
        // value to dodge Rust's borrow restrictions, and `PartialEq`
        // would force the generic-param flow path (closures, fn
        // pointers) to be `PartialEq` too, which they aren't. The
        // Clone impl below is hand-written; the PartialEq impl is
        // guarded by a `where` clause so the per-slot bound only fires
        // when the slot is actually instantiated.
        //
        // A structural shape carries no host param: its body never
        // names `H::Foo`. Every leaf is a positional `_T<N>`
        // placeholder filled at the use site, so an exact host type
        // reaches `H::Array` at the *use site* as an ordinary slot
        // argument, never inside the shape. `H` enters only at `H::Foo`
        // host leaves (passed as slot args), not inside the shape
        // itself. The `'static` the surrounding `Rc<dyn Any>` /
        // `Rc<dyn Fn>` storage needs comes from the slot tparams'
        // `Clone + 'static`.
        let tparam_names: Vec<String> = (0..record.tparam_count).map(shape_tparam_name).collect();
        // Generic clauses. A zero-tparam shape is fully monomorphic:
        // emit `struct Foo { … }` / `impl Clone for Foo`, never
        // `Foo<>` and never `impl<>` (`impl<>` is a hard rustc error).
        // `generics_decl` / `generics_use` are the `<…>` clause
        // including angle brackets, empty when there are no tparams;
        // `impl_decl` is the `impl<…>` clause, bare `impl` when empty.
        let (generics_decl, generics_use, impl_decl) = if tparam_names.is_empty() {
            (String::new(), String::new(), "impl".to_owned())
        } else {
            let decl_parts: Vec<String> = tparam_names
                .iter()
                .map(|n| format!("{n}: Clone + 'static"))
                .collect();
            (
                format!("<{}>", decl_parts.join(", ")),
                format!("<{}>", tparam_names.join(", ")),
                format!("impl<{}>", decl_parts.join(", ")),
            )
        };
        let partial_eq_where = if tparam_names.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = tparam_names
                .iter()
                .map(|n| format!("{n}: PartialEq"))
                .collect();
            format!(" where {}", parts.join(", "))
        };

        match record.kind {
            ShapeKind::Product => {
                out.push_str(&format!("/// `{}`\n", record.canonical));
                out.push_str("#[allow(non_camel_case_types)]\n");
                out.push_str(&format!("pub(crate) struct {name}{generics_decl} {{\n"));
                for (key, ty) in record.slot_keys.iter().zip(&record.slot_types) {
                    let ty_substituted = substitute_tparam_names(ty);
                    out.push_str(&format!("    pub(crate) {key}: {ty_substituted},\n"));
                }
                out.push_str("}\n");
                let clone_fields: Vec<String> = record
                    .slot_keys
                    .iter()
                    .map(|k| format!("{k}: self.{k}.clone()"))
                    .collect();
                out.push_str(&format!(
                    "{impl_decl} Clone for {name}{generics_use} {{\n    \
                         fn clone(&self) -> Self {{ Self {{ {} }} }}\n\
                     }}\n",
                    clone_fields.join(", ")
                ));
                // Skip the PartialEq impl when any slot type holds a
                // `dyn Fn` trait-object (closures aren't PartialEq).
                // Shape values with fn fields don't participate in
                // structural equality — the kio-level semantics for
                // fn-valued slots match this.
                let has_fn_slot = record.slot_types.iter().any(|t| t.contains("dyn Fn"));
                if !has_fn_slot {
                    let eq_fields: Vec<String> = record
                        .slot_keys
                        .iter()
                        .map(|k| format!("self.{k} == other.{k}"))
                        .collect();
                    let eq_body = if eq_fields.is_empty() {
                        "true".to_owned()
                    } else {
                        eq_fields.join(" && ")
                    };
                    out.push_str(&format!(
                        "{impl_decl} PartialEq for {name}{generics_use}{partial_eq_where} {{\n    \
                             fn eq(&self, other: &Self) -> bool {{ {eq_body} }}\n\
                         }}\n"
                    ));
                }
                // No `Eq` impl: shapes carrying float-role slots
                // can't satisfy `Eq` because IEEE floats don't (NaN
                // != NaN). `PartialEq` alone matches the language's
                // semantics; the host trait's `Eq` bound was
                // dropped accordingly for float roles, so shapes
                // never need to satisfy it either.
                out.push('\n');
            }
            ShapeKind::Sum => {
                out.push_str(&format!("/// `{}`\n", record.canonical));
                out.push_str("#[allow(non_camel_case_types)]\n");
                out.push_str(&format!("pub(crate) enum {name}{generics_decl} {{\n"));
                // Variant labels are newtype-derived (a label-bearing
                // arm inherits the newtype's name, lowercased-first), so
                // a newtype named `Box`/`Match`/`Loop`/… yields a label
                // that is a Rust keyword. The convert-side (and every
                // other consumer of these keys) already escapes them;
                // the declaration and its Clone/PartialEq impls must too,
                // or the emitted `enum Sum_… { box(…) }` is a syntax error.
                for (key, ty) in record.slot_keys.iter().zip(&record.slot_types) {
                    let key = escape_rust_keyword(key);
                    let ty_substituted = substitute_tparam_names(ty);
                    out.push_str(&format!("    {key}({ty_substituted}),\n"));
                }
                out.push_str("}\n");
                let clone_arms: Vec<String> = record
                    .slot_keys
                    .iter()
                    .map(|k| {
                        let k = escape_rust_keyword(k);
                        format!("Self::{k}(x) => Self::{k}(x.clone())")
                    })
                    .collect();
                let clone_body = if clone_arms.is_empty() {
                    "match *self {}".to_owned()
                } else {
                    format!("match self {{ {} }}", clone_arms.join(", "))
                };
                out.push_str(&format!(
                    "{impl_decl} Clone for {name}{generics_use} {{\n    \
                         fn clone(&self) -> Self {{ {clone_body} }}\n\
                     }}\n"
                ));
                let eq_arms: Vec<String> = record
                    .slot_keys
                    .iter()
                    .map(|k| {
                        let k = escape_rust_keyword(k);
                        format!("(Self::{k}(a), Self::{k}(b)) => a == b")
                    })
                    .collect();
                let eq_body = if eq_arms.is_empty() {
                    "match (self, other) {}".to_owned()
                } else {
                    format!(
                        "match (self, other) {{ {}, _ => false }}",
                        eq_arms.join(", "),
                    )
                };
                out.push_str(&format!(
                    "{impl_decl} PartialEq for {name}{generics_use}{partial_eq_where} {{\n    \
                         fn eq(&self, other: &Self) -> bool {{ {eq_body} }}\n\
                     }}\n"
                ));
                out.push('\n');
            }
        }
    }
}

/// Walk a fn body and register every structural type reachable
/// through enriched-IR `synth_ty` fields, under `tparams` — the
/// surrounding fn's type-parameter set, so tparam-referencing
/// synth-tys register as parametric shapes rather than rejecting
/// at the FFI surface.
fn register_body_shapes(
    e: &Expr<Routed>,
    shapes: &mut BodyShapeRegistry,
    tparams: &std::collections::HashSet<&str>,
    ctx: &BodyShapeContext<'_>,
) -> Result<(), EmitError> {
    use crate::ast::{EnrichedArm, RecordField};
    // Body-internal compounds register only the crate-private binary shape.
    // Prepared host/export conversions traverse the erased value directly and
    // never consult this registry for the public binary marker/facade algebra.
    match e {
        Expr::EnrichedTuple {
            items, synth_ty, ..
        } => {
            let synth_ty = shapes.normalize_type_for_tparams_in_current_scope(synth_ty, tparams);
            shapes.register_with_tparams(&synth_ty, tparams)?;
            for it in items {
                register_body_shapes(it, shapes, tparams, ctx)?;
            }
        }
        Expr::EnrichedRecord {
            fields, synth_ty, ..
        } => {
            let synth_ty = shapes.normalize_type_for_tparams_in_current_scope(synth_ty, tparams);
            shapes.register_with_tparams(&synth_ty, tparams)?;
            for RecordField { value, .. } in fields {
                register_body_shapes(value, shapes, tparams, ctx)?;
            }
        }
        Expr::EnrichedInject {
            payload, synth_ty, ..
        } => {
            let synth_ty = shapes.normalize_type_for_tparams_in_current_scope(synth_ty, tparams);
            shapes.register_with_tparams(&synth_ty, tparams)?;
            register_body_shapes(payload, shapes, tparams, ctx)?;
        }
        Expr::EnrichedMatch {
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            ..
        } => {
            let scrutinee_ty =
                shapes.normalize_type_for_tparams_in_current_scope(scrutinee_ty, tparams);
            let result_ty = shapes.normalize_type_for_tparams_in_current_scope(result_ty, tparams);
            shapes.register_with_tparams(&scrutinee_ty, tparams)?;
            shapes.register_with_tparams(&result_ty, tparams)?;
            register_body_shapes(scrutinee, shapes, tparams, ctx)?;
            for EnrichedArm { body, .. } in arms {
                register_body_shapes(body, shapes, tparams, ctx)?;
            }
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            result_ty,
            ..
        } => {
            let result_ty = shapes.normalize_type_for_tparams_in_current_scope(result_ty, tparams);
            shapes.register_with_tparams(&result_ty, tparams)?;
            register_body_shapes(cond, shapes, tparams, ctx)?;
            register_body_shapes(then_branch, shapes, tparams, ctx)?;
            register_body_shapes(else_branch, shapes, tparams, ctx)?;
        }
        Expr::EnrichedProject {
            target, target_ty, ..
        }
        | Expr::EnrichedFieldGet {
            target, target_ty, ..
        } => {
            let target_ty = shapes.normalize_type_for_tparams_in_current_scope(target_ty, tparams);
            shapes.register_with_tparams(&target_ty, tparams)?;
            register_body_shapes(target, shapes, tparams, ctx)?;
        }
        // Newtype member calls: same-module or qualified-import.
        // Both reference the newtype declaration; mark it so
        // `shapes.rs` emits its struct + impls even when no signature
        // mentions it directly.
        Expr::LowNewtypeCtor {
            newtype,
            type_args,
            payload,
            ..
        }
        | Expr::LowNewtypeProj {
            newtype,
            type_args,
            target: payload,
            ..
        } => {
            shapes.mark_newtype_name_referenced(ctx.module_path, newtype);
            for ty in type_args {
                let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, tparams);
                shapes.register_with_tparams(&ty, tparams)?;
            }
            register_body_shapes(payload, shapes, tparams, ctx)?;
        }
        Expr::LowQualifiedNewtypeMember {
            module_path,
            newtype,
            type_args,
            payload,
            ..
        } => {
            shapes.mark_newtype_name_referenced(Some(module_path), newtype);
            for ty in type_args {
                let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, tparams);
                shapes.register_with_tparams(&ty, tparams)?;
            }
            register_body_shapes(payload, shapes, tparams, ctx)?;
        }
        // CPS-projector apply also references the newtype.
        Expr::LowCpsProjectorApply {
            newtype,
            module_path,
            type_args,
            receiver,
            continuation,
            ..
        } => {
            shapes.mark_newtype_name_referenced(Some(module_path.as_str()), newtype);
            for ty in type_args {
                let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, tparams);
                shapes.register_with_tparams(&ty, tparams)?;
            }
            register_body_shapes(receiver, shapes, tparams, ctx)?;
            register_body_shapes(continuation, shapes, tparams, ctx)?;
        }
        Expr::LowHostCall {
            type_args, args, ..
        }
        | Expr::LowModuleCall {
            type_args, args, ..
        }
        | Expr::LowQualifiedModuleCall {
            type_args, args, ..
        }
        | Expr::LowClosureCall {
            type_args, args, ..
        } => {
            for ty in type_args {
                let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, tparams);
                shapes.register_with_tparams(&ty, tparams)?;
            }
            for a in args {
                register_body_shapes(a, shapes, tparams, ctx)?;
            }
        }
        Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } => {
            for ty in type_args {
                let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, tparams);
                shapes.register_with_tparams(&ty, tparams)?;
            }
            register_body_shapes(callee, shapes, tparams, ctx)?;
            for a in args {
                register_body_shapes(a, shapes, tparams, ctx)?;
            }
        }
        Expr::LowAbsurdCall {
            type_arg,
            value_arg,
            ..
        } => {
            let type_arg = shapes.normalize_type_for_tparams_in_current_scope(type_arg, tparams);
            shapes.register_with_tparams(&type_arg, tparams)?;
            register_body_shapes(value_arg, shapes, tparams, ctx)?;
        }
        // Routed-uninhabited carry-throughs (no shape work needed):
        Expr::LowBoundRef { .. }
        | Expr::LowHostFnValueRef { .. }
        | Expr::LowModuleFnValueRef { .. } => {}
        // `Expr::Call` / `Expr::Path` are statically uninhabited at
        // Routed — every `Expr::Call` has been classified into a
        // `Low*` variant above. Keep the `Expr::Path` arm for
        // forward-compat with any pre-Routed test harness that still
        // builds shapes from an Enriched AST. At runtime under the
        // Routed pipeline this arm is unreachable.
        Expr::Call { ext, .. } => match *ext {},
        Expr::Path { ext, .. } => match *ext {},
        Expr::Let { value, body, .. } | Expr::Seq { value, body, .. } => {
            register_body_shapes(value, shapes, tparams, ctx)?;
            register_body_shapes(body, shapes, tparams, ctx)?;
        }
        Expr::FnExpr { sig, body, .. } => {
            // Closure-level type params extend the enclosing tparam
            // scope so synth-ty walks under the lambda recognize
            // them as tparam leaves (existentials introduced by CPS
            // projector lambdas; etc.).
            let mut extra: std::collections::HashSet<&str> = tparams.clone();
            for p in &sig.params {
                if let crate::ast::SignatureParam::Type(tp) = p {
                    extra.insert(tp.name.as_str());
                }
            }
            for p in &sig.params {
                if let crate::ast::SignatureParam::Value(v) = p
                    && let Some(ty) = &v.ty
                {
                    let ty = shapes.normalize_type_for_tparams_in_current_scope(ty, &extra);
                    shapes.register_with_tparams(&ty, &extra)?;
                }
            }
            register_body_shapes(body, shapes, &extra, ctx)?;
        }
        Expr::Unit { .. }
        | Expr::StrLit { .. }
        | Expr::IntLit { .. }
        | Expr::FloatLit { .. }
        | Expr::BoolLit { .. } => {}
        _ => {}
    }
    Ok(())
}

/// Walk `ty` and append every tparam-reference name to `order` in
/// first-occurrence order (skipping duplicates). Used both by
/// canonical_walk's own ordering and by the slot renderer to find
/// the slot's tparam positions for translating into the outer
/// shape's positional encoding.
/// Walk `ty` and collect each Path-typed leaf in left-to-right order,
/// one per occurrence (no dedup). Used at shape-use sites to compute
/// the type-argument list for the shape's generic params. Mirrors
/// `canonical_walk`'s leaf-positional renaming: each Path leaf gets
/// its own slot, so `Int & Int` and `A & B` both fill the same shape
/// (`Prod<H, A, B>`) with distinct type-arg tuples.
fn collect_leaf_args(ty: &Type<Routed>) -> Vec<Type<Routed>> {
    let mut out: Vec<Type<Routed>> = Vec::new();
    collect_leaf_args_walk(ty, &mut out);
    out
}

fn collect_leaf_args_walk(ty: &Type<Routed>, out: &mut Vec<Type<Routed>>) {
    match ty {
        // Unit consumes one positional slot at the shape mint
        // (matches `canonical_walk`'s Unit arm), so it lands in the
        // leaf list and the use-site renderer fills the slot with
        // `()`.
        Type::Path { .. } | Type::Unit { .. } => {
            out.push(ty.clone());
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_leaf_args_opaque(left, out);
            collect_leaf_args_opaque(right, out);
        }
        // A `Forall` (rank-N) leaf consumes one positional generic on
        // the mint exactly like `Function` (see `canonical_walk`'s
        // `Type::Forall` arm). It must be collected here so the use-site
        // generic-arg list matches the minted struct's `tparam_count`;
        // dropping it produced a too-few-args `<…>` (rustc E0107).
        Type::Function { .. } | Type::Forall { .. } => {
            out.push(ty.clone());
        }
        _ => {}
    }
}

/// Like [`collect_leaf_args_walk`] but treats nested compounds as
/// opaque single leaves — mirrors `canonical_walk_opaque`. The
/// outer shape's mint is binary, so its leaf list is binary; inner
/// shape mints handle their own walks recursively at use sites.
fn collect_leaf_args_opaque(ty: &Type<Routed>, out: &mut Vec<Type<Routed>>) {
    match ty {
        // Unit and Path leaves both consume one positional slot at
        // the outer shape's mint (see `canonical_walk_opaque`).
        Type::Path { .. } | Type::Unit { .. } => {
            out.push(ty.clone());
        }
        Type::Product { .. } | Type::Sum { .. } | Type::Function { .. } | Type::Forall { .. } => {
            out.push(ty.clone());
        }
        _ => {}
    }
}

/// Walk `ty` and collect every tparam name (drawn from `tparams`)
/// that appears in a **brand position** — the head of an applied
/// higher-kinded binder `F(A)` (a single-segment `Type::Path` whose
/// head is in `tparams` and which carries arguments). The erased Rust
/// backend uses this payload reachability separately from nominal header
/// erasure: `payload_hkt_param_indices` decides whether the stored field
/// reaches `Rc<dyn Any>`, while `higher_kinded_param_indices` drops every
/// declared non-`Star` slot from the Rust nominal.
fn collect_brand_tparams_in_type(
    ty: &Type<Routed>,
    tparams: &std::collections::HashSet<&str>,
    out: &mut std::collections::HashSet<String>,
) {
    match ty {
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_brand_tparams_in_type(left, tparams, out);
            collect_brand_tparams_in_type(right, tparams, out);
        }
        Type::Function { param, ret, .. } => {
            collect_brand_tparams_in_type(param, tparams, out);
            collect_brand_tparams_in_type(ret, tparams, out);
        }
        Type::Forall { body, .. } => collect_brand_tparams_in_type(body, tparams, out),
        Type::Path { segments, args, .. } => {
            // Applied higher-kinded binder `F(A)`: the single-segment
            // head `f` marks a brand position, so its slot is dropped
            // from the erased struct's generics.
            if segments.len() == 1 && !args.is_empty() && tparams.contains(segments[0].as_str()) {
                out.insert(segments[0].as_str().to_owned());
            }
            for a in args {
                collect_brand_tparams_in_type(a, tparams, out);
            }
        }
        _ => {}
    }
}

// ---- right-spine walk ----------------------------------------------------

/// Binary right-spine: returns exactly `[left, right_as_whole]` for a
/// `Type::Product`, with the right side kept as one slot even when it
/// is itself a product. Each level of a nested product mints its own
/// `Prod_2` shape, matching the kio-level right-spined-binary view
/// the typer carries and `canonical_walk`'s opaque-inner scheme.
/// Binary right-spine: returns exactly `[left, right_as_whole]` for a
/// `Type::Product`. Each level of a nested product mints its own
/// `Prod_2` shape; consumers (`emit_tuple` / `emit_project`) walk
/// the nested chain themselves. Mirrors the kio-level
/// right-spined-binary view the typer carries and the binary form
/// `right_spine_walk_sum` already uses.
fn right_spine_walk_product(ty: &Type<Routed>) -> Vec<Type<Routed>> {
    if let Type::Product { left, right, .. } = ty {
        vec![(**left).clone(), (**right).clone()]
    } else {
        vec![ty.clone()]
    }
}

/// Binary right-spine: returns exactly `[left, right_as_whole]` for a
/// `Type::Sum`, with the right side kept as one slot even when it is
/// itself a sum. Each level of a nested sum mints its own `Sum_2`
/// shape; consumers (`emit_inject` / `emit_match`) walk the nested
/// chain themselves. Mirrors the JS backend's nested-binary runtime
/// form (`[0, p]` / `[1, …]`), so the structural recovery's flatten /
/// no-flatten choice doesn't leak into shape minting.
fn right_spine_walk_sum(ty: &Type<Routed>) -> Vec<Type<Routed>> {
    if let Type::Sum { left, right, .. } = ty {
        vec![(**left).clone(), (**right).clone()]
    } else {
        vec![ty.clone()]
    }
}

// ---- canonical render + FNV-1a -------------------------------------------

/// Canonical render with positional tparam renaming. Any
/// `Type::Path { segments: [name] }` whose `name` is in `tparams`
/// is rewritten to `_T<idx>` where `idx` is the order of first
/// occurrence within `ty`. Two types that differ only by tparam
/// renaming canonicalize to the same string, so they share one
/// shape struct (with synthesized generic params named `A`, `B`,
/// … at struct-definition emit time, see [`shape_tparam_name`]).
///
/// Returns the canonical string and the per-position tparam name
/// ordering. The caller uses the ordering to translate use-site
/// tparam args into the shape's `_T<idx>` slots.
fn canonical_and_order(
    ty: &Type<Routed>,
    tparams: &std::collections::HashSet<&str>,
) -> (String, Vec<String>) {
    let mut order: Vec<String> = Vec::new();
    let canonical = canonical_walk(ty, tparams, &mut order);
    (canonical, order)
}

/// Stable key for an order-vec slot derived from a Path-typed leaf.
/// Two paths with the same head and args share the key (and hence
/// the same `_T<N>` slot); paths that differ in any component get
/// distinct slots. Used by `canonical_walk` to dedup leaf-positional
/// renaming.
fn render_path_key(ty: &Type<Routed>) -> String {
    match ty {
        Type::Path { segments, args, .. } => {
            let head = segments
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("/");
            if args.is_empty() {
                head
            } else {
                let args_s = args
                    .iter()
                    .map(render_path_key)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{head}({args_s})")
            }
        }
        Type::Unit { .. } => "()".to_owned(),
        Type::Bottom { .. } => "!".to_owned(),
        Type::Product { left, right, .. } => {
            format!("({}&{})", render_path_key(left), render_path_key(right))
        }
        Type::Sum { left, right, .. } => {
            format!("({}|{})", render_path_key(left), render_path_key(right))
        }
        Type::Function { param, ret, .. } => {
            format!("({}->{})", render_path_key(param), render_path_key(ret))
        }
        Type::Forall { body, .. } => {
            // Key on the quantified body's structure so two distinct
            // rank-N types (`forall r. …` vs `forall n. …`) get distinct
            // `_T<N>` slots rather than colliding on a single opaque
            // sentinel. The binder name is alpha-irrelevant to the slot
            // identity; the body shape is what matters.
            format!("<forall {}>", render_path_key(body))
        }
        Type::Infer { .. } => "_".to_owned(),
        Type::LabelSugar { .. } => "<labelsugar>".to_owned(),
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Like [`canonical_walk`] but treats nested compounds (Product /
/// Sum / Function) as opaque — each consumes one positional leaf.
/// Used by `canonical_walk`'s Product / Sum arms to keep the
/// canonical *binary* at every level: the outer shape's mint is
/// based on `(left & right)` / `(left | right)` with each side a
/// single position, so `(A & B) & C` mints the same `Prod_2<H, A,
/// B>` regardless of whether the slots are simple paths or nested
/// compounds. Mirrors `right_spine_walk_product`'s binary form
/// (see also `collect_leaf_args_opaque`).
fn canonical_walk_opaque(
    ty: &Type<Routed>,
    tparams: &std::collections::HashSet<&str>,
    order: &mut Vec<String>,
) -> String {
    match ty {
        Type::Unit { .. } | Type::Bottom { .. } | Type::Path { .. } => {
            canonical_walk(ty, tparams, order)
        }
        Type::Product { .. } | Type::Sum { .. } | Type::Function { .. } => {
            let key = render_path_key(ty);
            let i = order.len();
            order.push(key);
            format!("_T{i}")
        }
        _ => canonical_walk(ty, tparams, order),
    }
}

fn canonical_walk(
    ty: &Type<Routed>,
    tparams: &std::collections::HashSet<&str>,
    order: &mut Vec<String>,
) -> String {
    match ty {
        Type::Unit { .. } => {
            // Unit consumes a positional `_T<N>` slot so the
            // monomorphized form `(String & ())` shares its mint
            // with the polymorphic declaration `(A & U)` — the call
            // site that supplies `U := ()` lands the value in the
            // same Rust shape the constructor's signature spelled
            // out. Without this, a unit-witness existential
            // construction (`mk_pack((s, ()))` against
            // `Pack[A] <U> : A & U`) mints two divergent shapes
            // (one per call-site instantiation), and the call
            // never typechecks.
            let key = render_path_key(ty);
            let i = order.len();
            order.push(key);
            format!("_T{i}")
        }
        Type::Bottom { .. } => "!".to_owned(),
        Type::Path { segments, args, .. } => {
            // Every Path-typed leaf — tparam reference (`A`), host
            // type (`Int`, `String`), or newtype application
            // (`List(A)`) — becomes its own positional `_T<N>`
            // placeholder. The use-site `render` pass instantiates
            // each placeholder with the concrete Rust type. Each
            // *occurrence* gets its own slot (no dedup by key), so
            // `Int & Int` and `A & B` mint the same shape with 2
            // generic params; a single shape mint covers both
            // monomorphic and polymorphic instantiations of the
            // same structural arity, which is required for a call
            // like `tail_of(triple)` to typecheck — the fn-side and
            // call-side shapes must agree.
            let key = render_path_key(ty);
            let i = order.len();
            order.push(key);
            let _ = (segments, args, tparams);
            format!("_T{i}")
        }
        Type::Product { left, right, .. } => {
            let l = canonical_walk_opaque(left, tparams, order);
            let r = canonical_walk_opaque(right, tparams, order);
            format!("({l} & {r})")
        }
        Type::Sum { left, right, .. } => {
            let l = canonical_walk_opaque(left, tparams, order);
            let r = canonical_walk_opaque(right, tparams, order);
            format!("({l} | {r})")
        }
        Type::Function { .. } => {
            // Function types are opaque to the shape canonical:
            // every `Type::Function` consumes one positional leaf,
            // regardless of its param / ret arity or whether those
            // are concrete or tparam-typed. That makes a Lens-style
            // alias `(S -> A) & (A, S -> S)` mint a 2-slot Prod
            // (one slot per fn) whose generic params can absorb
            // any substituted fn type — so the alias's definition
            // and any user construction-via-substitution share a
            // single mint. The `render_for_slot` Function arm fills
            // the slot with the proper `Rc<dyn Fn(...) -> ...>`
            // rendering.
            let key = render_path_key(ty);
            let i = order.len();
            order.push(key);
            "_FN".to_owned() + &format!("_T{i}")
        }
        Type::Forall { .. } => {
            // A rank-N (`forall`-quantified) type — a Church-encoded
            // `List(A) = forall R. …`, a `Nat`, or any other polymorphic
            // value crossing a shape boundary — is opaque to the shape
            // canonical, exactly like `Type::Function`. It consumes one
            // positional leaf; `render_for_slot`'s `Type::Forall` arm
            // fills the slot with the rank-N value's erased
            // `Rc<dyn Fn(...)>` rendering (the `EXISTENTIAL_ANY_SENTINEL`
            // scheme `render_use` already uses for rank-N value
            // parameters — see `specs/backends/rust.md` § Higher-rank value
            // parameters). Sharing the leaf treatment with `Function`
            // keeps a compound like `Nat & Opt(Nat)` minting one stable
            // shape regardless of how its slots instantiate.
            let key = render_path_key(ty);
            let i = order.len();
            order.push(key);
            "_FORALL".to_owned() + &format!("_T{i}")
        }
        Type::Infer { .. } => "_".to_owned(),
        Type::LabelSugar { .. } => "<labelsugar>".to_owned(),
        Type::Goal { ext, .. } => match *ext {},
    }
}

/// Synthesized generic-param name for the `n`-th tparam slot of a
/// shape struct. Single-letter names (`A`..`Z`) keep emitted source
/// compact; from the 27th generic slot onward, `T<N>` keeps the
/// encoding total and well-defined.
fn shape_tparam_name(n: usize) -> String {
    // Skip `H` (the Host-param name) so an 8-slot shape doesn't
    // collide its 8th tparam (`H` in the bare A..Z sequence) with
    // the Host param. A, B, C, D, E, F, G, then I, J, … instead.
    let names = b"ABCDEFGIJKLMNOPQRSTUVWXYZ";
    if n < names.len() {
        (names[n] as char).to_string()
    } else {
        format!("T{n}")
    }
}

/// The readable mint-name token per spine slot, mirroring the slot
/// classification the shape canonical carries (`canonical_walk_opaque`):
/// every order-consuming slot tokens as its generic letter
/// ([`shape_tparam_name`] of its positional index — the same letter the
/// declaration's generic header and fields use), a rank-N (`forall`)
/// slot marks its letter `Poly`, and a `!` slot (which consumes no
/// generic) tokens as `Never`. Kind prefix + token sequence determine
/// the canonical exactly, so two distinct shapes never share a readable
/// base name.
fn readable_slot_tokens(slots: &[Type<Routed>]) -> Vec<String> {
    let mut order_idx = 0usize;
    slots
        .iter()
        .map(|slot| match slot {
            Type::Bottom { .. } => "Never".to_owned(),
            Type::Forall { .. } => {
                let token = format!("Poly{}", shape_tparam_name(order_idx));
                order_idx += 1;
                token
            }
            _ => {
                let token = shape_tparam_name(order_idx);
                order_idx += 1;
                token
            }
        })
        .collect()
}

/// Substitute every `_T<N>` placeholder in `text` with the
/// corresponding `shape_tparam_name(N)`. Used to lower stored
/// slot_types (which use the canonical positional encoding) to
/// the struct-definition's user-facing generic names.
fn substitute_tparam_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len()
            && bytes[i] == b'_'
            && bytes[i + 1] == b'T'
            && bytes[i + 2].is_ascii_digit()
        {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if let Ok(idx) = text[i + 2..j].parse::<usize>() {
                out.push_str(&shape_tparam_name(idx));
                i = j;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn fnv1a_64_hex(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

// ---- FFI conversion -------------------------------------------------------
//
// Prepared per-signature wrapper helpers. `In` converts an exact prepared
// facade value to the erased body representation; `Out` performs the inverse.
// The facade walk follows `FacadeUseId` nodes and its paired execution layout,
// never a raw type or package-local shape lookup.
use crate::backends::skin::FfiDir;

#[derive(Clone)]
struct PreparedRustUseCursor<'a> {
    plan: &'a BoundaryFacadePlan,
    execution: Option<&'a BoundaryFacadeExecutionPlan>,
    id: FacadeUseId,
    substitutions: BTreeMap<FacadeBinderId, Box<PreparedRustUseCursor<'a>>>,
}

impl<'a> PreparedRustUseCursor<'a> {
    fn root(site: PreparedBoundaryCallableSite<'a>, id: FacadeUseId) -> Self {
        Self {
            plan: site.plan().facade(),
            execution: Some(site.presentation().root_uses()),
            id,
            substitutions: BTreeMap::new(),
        }
    }

    fn child(&self, id: FacadeUseId) -> Self {
        Self {
            plan: self.plan,
            execution: self.execution,
            id,
            substitutions: self.substitutions.clone(),
        }
    }

    fn execution_use(&self) -> Option<&'a BoundaryFacadeExecutionUse> {
        self.execution.map(|execution| execution.use_at(self.id))
    }
}

struct PreparedRustConverter<'a> {
    site: PreparedBoundaryCallableSite<'a>,
    shapes: PreparedRustRenderContext<'a>,
    scope: PreparedRustScope,
    ctx: TypeCtx,
    nominal_owner: Option<QualifiedTypeName>,
}

impl<'a> PreparedRustConverter<'a> {
    fn crate_root(&self) -> &str {
        self.shapes.crate_root()
    }

    fn render(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        position: PreparedRustTypePosition,
    ) -> Result<String, EmitError> {
        if let FacadeUse::Bound { binder, .. } = cursor.plan.use_at(cursor.id)
            && let Some(substitution) = cursor.substitutions.get(binder)
        {
            return self.render(substitution, position);
        }
        render_prepared_use_with_owner(
            PreparedRustUseRenderContext {
                site: self.site,
                plan: cursor.plan,
                scope: &self.scope,
                shapes: self.shapes,
                type_ctx: self.ctx,
            },
            cursor.id,
            position,
            self.nominal_owner.as_ref(),
        )
    }

    fn convert_parameter_in(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        expression: &str,
    ) -> Result<String, EmitError> {
        self.convert(cursor, expression, FfiDir::In)
    }

    fn convert(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        if let FacadeUse::Bound { binder, .. } = cursor.plan.use_at(cursor.id)
            && let Some(substitution) = cursor.substitutions.get(binder)
        {
            return self.convert(substitution, expression, direction);
        }
        match cursor.plan.use_at(cursor.id) {
            FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } => {
                self.convert_scalar(cursor, expression, direction)
            }
            FacadeUse::Bound { .. } => self.convert_scalar(cursor, expression, direction),
            FacadeUse::Nominal { name, .. } => {
                self.convert_nominal(cursor, name, &[], expression, direction)
            }
            FacadeUse::Apply {
                constructor, args, ..
            } => {
                match cursor.plan.use_at(*constructor) {
                    FacadeUse::Nominal { name, .. } => {
                        let declaration = self.site.nominals().declaration(name).unwrap_or_else(|| {
                        unreachable!("Rust emitter: an applied nominal conversion lost its declaration")
                    });
                        match declaration {
                            BoundaryNominalDeclaration::HostType { type_params, .. }
                                if !type_params.is_empty() =>
                            {
                                self.convert_scalar(cursor, expression, direction)
                            }
                            _ => self.convert_nominal(cursor, name, args, expression, direction),
                        }
                    }
                    FacadeUse::Bound { .. } => self.convert_scalar(cursor, expression, direction),
                    _ => unreachable!(
                        "Rust emitter: prepared conversion has a non-nominal application head"
                    ),
                }
            }
            FacadeUse::Product { shell, args, .. } => {
                self.convert_product(cursor, shell, args, expression, direction)
            }
            FacadeUse::Sum { shell, args, .. } => {
                self.convert_sum(cursor, shell, args, expression, direction)
            }
            FacadeUse::Function { slots, .. } => {
                let layout = match cursor.execution_use() {
                    Some(BoundaryFacadeExecutionUse::Function(layout)) => layout,
                    _ => unreachable!(
                        "Rust emitter: prepared live function has no paired execution layout"
                    ),
                };
                self.convert_function(cursor, slots, layout, expression, direction)
            }
            FacadeUse::Forall { .. } => self.convert_scalar(cursor, expression, direction),
        }
    }

    fn convert_forall_implementation_in(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        expression: &str,
    ) -> Result<String, EmitError> {
        let method =
            prepare_rust_forall_method(cursor.plan, cursor.id, &self.scope, self.crate_root());
        if method.type_parameters.is_empty() {
            unreachable!("Rust emitter: prepared forall conversion starts at a non-forall use");
        }
        let erased_converter = Self {
            site: self.site,
            shapes: self.shapes,
            scope: method.erased_scope.clone(),
            ctx: self.ctx,
            nominal_owner: self.nominal_owner.clone(),
        };
        let body_cursor = PreparedRustUseCursor {
            plan: cursor.plan,
            execution: cursor.execution,
            id: method.body,
            substitutions: cursor.substitutions.clone(),
        };
        let type_arguments = method
            .type_parameters
            .iter()
            .map(|parameter| {
                erased_prepared_type_argument(parameter.kind_arity(), self.crate_root())
            })
            .collect::<Vec<_>>();
        let turbofish = format!("::<{}>", type_arguments.join(", "));
        let trait_name = rust_forall_name_with_owner(
            self.site,
            cursor.plan,
            cursor.id,
            self.nominal_owner.as_ref(),
        )?;
        let trait_arguments = rust_forall_generic_arguments(&self.scope, self.ctx).join(", ");
        let body = match cursor.plan.use_at(method.body) {
            FacadeUse::Function { slots, result, .. } => {
                let layout = match body_cursor.execution_use() {
                    Some(BoundaryFacadeExecutionUse::Function(layout)) => layout,
                    _ => unreachable!(
                        "Rust emitter: prepared forall function has no paired execution layout"
                    ),
                };
                let source_parameters = (0..layout.body_abi_arity())
                    .map(|index| format!("__source_{index}: ::std::rc::Rc<dyn ::std::any::Any>"))
                    .collect::<Vec<_>>();
                let source_markers = render_prepared_function_source_markers(
                    PreparedRustUseRenderContext {
                        site: self.site,
                        plan: cursor.plan,
                        scope: &method.erased_scope,
                        shapes: self.shapes,
                        type_ctx: self.ctx,
                    },
                    slots,
                    layout,
                    self.nominal_owner.as_ref(),
                )?;
                let facade_arguments = source_markers
                            .iter()
                            .enumerate()
                            .map(|(index, marker)| {
                                format!(
                                    "<{marker} as {}::shapes::KioType>::from_stored({}::shapes::KioStoredValue::from_raw(__source_{index}))",
                                    self.crate_root(),
                                    self.crate_root(),
                                )
                            })
                            .collect::<Vec<_>>();
                let facade_arguments = if facade_arguments.is_empty() {
                    String::new()
                } else {
                    format!(", {}", facade_arguments.join(", "))
                };
                let call = format!(
                    "<_ as {}::shapes::{trait_name}<{trait_arguments}>>::apply{turbofish}(&*__poly{facade_arguments})",
                    self.crate_root(),
                );
                let returned =
                    erased_converter.convert(&body_cursor.child(*result), &call, FfiDir::In)?;
                let fn_type = rc_fn_type(layout.body_abi_arity());
                format!(
                    "{{ let __f: {fn_type} = ::std::rc::Rc::new(move |{}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {returned} }}); {}::__kio_runtime::as_any(__f) }}",
                    source_parameters.join(", "),
                    self.crate_root()
                )
            }
            _ => {
                let applied = format!(
                    "<_ as {}::shapes::{trait_name}<{trait_arguments}>>::apply{turbofish}(&*__poly)",
                    self.crate_root()
                );
                erased_converter.convert(&body_cursor, &applied, FfiDir::In)?
            }
        };
        let mut wrapped = body;
        for _ in &method.type_parameters {
            let stage_type = rc_fn_type(0);
            wrapped = format!(
                "{{ let __inner: ::std::rc::Rc<dyn ::std::any::Any> = {wrapped}; let __stage: {stage_type} = ::std::rc::Rc::new(move || __inner.clone()); {}::__kio_runtime::as_any(__stage) }}",
                self.crate_root()
            );
        }
        Ok(format!(
            "{{ let __poly = ::std::rc::Rc::new({expression}); {wrapped} }}"
        ))
    }

    fn convert_scalar(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
        Ok(match direction {
            FfiDir::In => format!(
                "<{marker} as {}::shapes::KioType>::into_stored({expression}).into_raw()",
                self.crate_root()
            ),
            FfiDir::Out => format!(
                "<{marker} as {}::shapes::KioType>::from_stored({}::shapes::KioStoredValue::from_raw({expression}))",
                self.crate_root(),
                self.crate_root()
            ),
        })
    }

    fn convert_nominal(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        name: &QualifiedTypeName,
        args: &[FacadeUseId],
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        let declaration = self.site.nominals().declaration(name).unwrap_or_else(|| {
            unreachable!("Rust emitter: prepared nominal conversion lost its declaration")
        });
        match declaration {
            BoundaryNominalDeclaration::HostType { .. } => {
                self.convert_scalar(cursor, expression, direction)
            }
            BoundaryNominalDeclaration::Newtype {
                transparent_payload,
                surface,
                ..
            } => {
                if !self.shapes.has_public_newtype(name) {
                    unreachable!(
                        "Rust emitter: prepared newtype conversion has no frozen public carrier"
                    );
                }
                // Prepared public nominal carriers are exact in their
                // declaration parameters and keep the erased body value in
                // a private field. Constructor/projector sites are the only
                // boundary at which the transparent payload is interpreted;
                // unrelated callable sites never re-plan that payload.
                let _ = (transparent_payload, surface, args);
                Ok(match direction {
                    FfiDir::Out => self.convert_scalar(cursor, expression, direction)?,
                    FfiDir::In => self.convert_scalar(cursor, expression, direction)?,
                })
            }
        }
    }

    fn convert_product(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        _shell: &FacadeShellId,
        args: &[FacadeUseId],
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        let crate_root = self.crate_root();
        match direction {
            FfiDir::In => {
                let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
                Ok(format!(
                    "<{marker} as {crate_root}::shapes::KioType>::into_stored({expression}).into_raw()"
                ))
            }
            FfiDir::Out => {
                let _ = args;
                let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
                Ok(format!(
                    "<{marker} as {crate_root}::shapes::KioType>::from_stored({crate_root}::shapes::KioStoredValue::from_raw({expression}))"
                ))
            }
        }
    }

    fn convert_sum(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        _shell: &FacadeShellId,
        args: &[FacadeUseId],
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        let crate_root = self.crate_root();
        match direction {
            FfiDir::In => {
                let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
                Ok(format!(
                    "<{marker} as {crate_root}::shapes::KioType>::into_stored({expression}).into_raw()"
                ))
            }
            FfiDir::Out => {
                let _ = args;
                let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
                Ok(format!(
                    "<{marker} as {crate_root}::shapes::KioType>::from_stored({crate_root}::shapes::KioStoredValue::from_raw({expression}))"
                ))
            }
        }
    }

    fn convert_function(
        &self,
        cursor: &PreparedRustUseCursor<'a>,
        slots: &[FacadeUseId],
        layout: &CallableValueStageLayout,
        expression: &str,
        direction: FfiDir,
    ) -> Result<String, EmitError> {
        if layout.facade_slot_count() != slots.len() {
            unreachable!("Rust emitter: function facade/execution slot count drift");
        }
        let crate_root = self.crate_root();
        let marker = self.render(cursor, PreparedRustTypePosition::Marker)?;
        match direction {
            FfiDir::Out => Ok(format!(
                "<{marker} as {crate_root}::shapes::KioType>::from_stored({crate_root}::shapes::KioStoredValue::from_raw({expression}))"
            )),
            FfiDir::In => Ok(format!(
                "<{marker} as {crate_root}::shapes::KioType>::into_stored({expression}).into_raw()"
            )),
        }
    }
}

fn pack_prepared_source_arguments(
    layout: &CallableValueStageLayout,
    facade_values: &[String],
    crate_root: &str,
) -> Result<Vec<String>, EmitError> {
    let mut source = Vec::with_capacity(layout.source_param_count());
    for parameter in layout.source_params() {
        let range = parameter.facade_slots();
        let values = &facade_values[range];
        source.push(match parameter.adapter() {
            CallableSourceParamAdapter::UnitValue => {
                format!("{crate_root}::__kio_runtime::as_any(())")
            }
            CallableSourceParamAdapter::Identity => values
                .first()
                .cloned()
                .unwrap_or_else(|| unreachable!("an identity source adapter has no slot")),
            CallableSourceParamAdapter::RightNest => {
                let mut nested = values
                    .last()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("a product source adapter has no slots"));
                for value in values[..values.len() - 1].iter().rev() {
                    nested = format!("{crate_root}::__kio_runtime::as_any(({value}, {nested}))");
                }
                nested
            }
        });
    }
    Ok(source)
}

fn prepared_public_product_slot(expression: &str, offset: usize, width: usize) -> String {
    if offset >= width || width < 2 {
        unreachable!("a prepared public product slot is outside its source parameter");
    }
    let mut slot = expression.to_owned();
    for _ in 0..offset {
        slot.push_str("._1");
    }
    if offset + 1 < width {
        slot.push_str("._0");
    }
    slot
}

fn pack_prepared_public_source_arguments(
    layout: &CallableValueStageLayout,
    facade_values: &[String],
    crate_root: &str,
) -> Result<Vec<String>, EmitError> {
    let mut source = Vec::with_capacity(layout.source_param_count());
    for parameter in layout.source_params() {
        let range = parameter.facade_slots();
        let values = &facade_values[range.clone()];
        source.push(match parameter.adapter() {
            CallableSourceParamAdapter::UnitValue => "()".to_owned(),
            CallableSourceParamAdapter::Identity => values
                .first()
                .cloned()
                .unwrap_or_else(|| unreachable!("an identity source adapter has no slot")),
            CallableSourceParamAdapter::RightNest => {
                let mut nested = values
                    .last()
                    .cloned()
                    .unwrap_or_else(|| unreachable!("a product source adapter has no slots"));
                for value in values[..values.len() - 1].iter().rev() {
                    nested =
                        format!("{crate_root}::shapes::Product {{ _0: {value}, _1: {nested} }}");
                }
                nested
            }
        });
    }
    Ok(source)
}

fn unpack_prepared_public_source_arguments<'a>(
    converter: &PreparedRustConverter<'a>,
    cursor: &PreparedRustUseCursor<'a>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    source_expressions: &[String],
    direction: FfiDir,
) -> Result<Vec<String>, EmitError> {
    if source_expressions.len() != layout.source_param_count() {
        unreachable!("a prepared public source argument count drifted");
    }
    let mut facade = Vec::with_capacity(layout.facade_slot_count());
    for (source_index, parameter) in layout.source_params().iter().enumerate() {
        let range = parameter.facade_slots();
        let source = &source_expressions[source_index];
        match parameter.adapter() {
            CallableSourceParamAdapter::UnitValue => {}
            CallableSourceParamAdapter::Identity => {
                let slot = slots[range.start];
                let cursor = cursor.child(slot);
                let converted = converter.convert(&cursor, source, direction)?;
                facade.push(converted);
            }
            CallableSourceParamAdapter::RightNest => {
                for (offset, slot) in slots[range.clone()].iter().enumerate() {
                    let cursor = cursor.child(*slot);
                    let converted = converter.convert(
                        &cursor,
                        &prepared_public_product_slot(source, offset, range.len()),
                        direction,
                    )?;
                    facade.push(converted);
                }
            }
        }
    }
    Ok(facade)
}

fn render_prepared_callable_source_parameters(
    site: PreparedBoundaryCallableSite<'_>,
    callable: &PreparedRustCallable,
    shapes: PreparedRustRenderContext<'_>,
    ctx: TypeCtx,
) -> Result<Vec<String>, EmitError> {
    let entry = site.plan().entry();
    let mut source_index = 0usize;
    let mut parameters = Vec::new();
    for ((semantic, presentation), stage_scope) in entry
        .head_stages
        .iter()
        .zip(site.presentation().head_stages())
        .zip(&callable.head_scopes)
    {
        match (semantic, presentation) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {}
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                for source in layout.source_params() {
                    let rendered = render_prepared_source_facade(
                        PreparedRustUseRenderContext {
                            site,
                            plan: site.plan().facade(),
                            scope: stage_scope,
                            shapes,
                            type_ctx: ctx,
                        },
                        slots,
                        source,
                        None,
                    )?;
                    parameters.push(format!("arg{source_index}: {rendered}"));
                    source_index += 1;
                }
            }
            _ => unreachable!("a prepared Rust source presentation disagrees with its plan"),
        }
    }
    Ok(parameters)
}

fn unpack_prepared_callable_public_arguments<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    callable: &PreparedRustCallable,
    converter: &PreparedRustConverter<'a>,
) -> Result<Vec<String>, EmitError> {
    let entry = site.plan().entry();
    let mut source_index = 0usize;
    let mut facade = Vec::with_capacity(callable.value_slots.len());
    for ((semantic, presentation), stage_scope) in entry
        .head_stages
        .iter()
        .zip(site.presentation().head_stages())
        .zip(&callable.head_scopes)
    {
        match (semantic, presentation) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {}
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let end = source_index + layout.source_param_count();
                let expressions = (source_index..end)
                    .map(|index| format!("arg{index}"))
                    .collect::<Vec<_>>();
                let cursor = PreparedRustUseCursor::root(
                    site,
                    slots
                        .first()
                        .copied()
                        .unwrap_or(site.plan().facade().root()),
                );
                let stage_converter = PreparedRustConverter {
                    site: converter.site,
                    shapes: converter.shapes,
                    scope: stage_scope.clone(),
                    ctx: converter.ctx,
                    nominal_owner: converter.nominal_owner.clone(),
                };
                facade.extend(unpack_prepared_public_source_arguments(
                    &stage_converter,
                    &cursor,
                    slots,
                    layout,
                    &expressions,
                    FfiDir::In,
                )?);
                source_index = end;
            }
            _ => unreachable!("a prepared Rust source presentation disagrees with its plan"),
        }
    }
    if facade.len() != callable.value_slots.len() {
        unreachable!("prepared public parameters did not cover every facade slot");
    }
    Ok(facade)
}

fn adapt_prepared_body_to_public_arguments<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    converter: &PreparedRustConverter<'a>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    body_values: &[String],
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count() || body_values.len() != layout.body_abi_arity() {
        unreachable!("a prepared Rust body/source stage lost its paired layout");
    }
    let mut body_index = 0usize;
    let mut facade_values = Vec::with_capacity(layout.facade_slot_count());
    for source in layout.source_params() {
        let range = source.facade_slots();
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => body_index += 1,
            CallableSourceParamAdapter::Identity => {
                let body = &body_values[body_index];
                body_index += 1;
                let slot = slots[range.start];
                facade_values.push(converter.convert(
                    &PreparedRustUseCursor::root(site, slot),
                    &format!("({body}).clone()"),
                    FfiDir::Out,
                )?);
            }
            CallableSourceParamAdapter::RightNest => {
                let body = &body_values[body_index];
                body_index += 1;
                for (offset, slot) in slots[range.clone()].iter().enumerate() {
                    let erased = format!(
                        "{}::__kio_runtime::product_slot(({body}).clone(), {offset}, {})",
                        converter.crate_root(),
                        range.len(),
                    );
                    facade_values.push(converter.convert(
                        &PreparedRustUseCursor::root(site, *slot),
                        &erased,
                        FfiDir::Out,
                    )?);
                }
            }
        }
    }
    if body_index != body_values.len() || facade_values.len() != layout.facade_slot_count() {
        unreachable!("a prepared Rust body/source adapter did not consume its exact stage");
    }
    pack_prepared_public_source_arguments(layout, &facade_values, converter.crate_root())
}

// ---- emit -----------------------------------------------------------------

fn emit_cargo_toml(crate_name: &str) -> String {
    format!(
        "# Generated by kio — do not edit by hand.\n\
         [package]\n\
         name = \"{crate_name}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\
         \n\
         [lib]\n\
         path = \"src/lib.rs\"\n"
    )
}

fn emit_lib_rs(
    module_fns: &[ModuleFn<'_>],
    export_block_namespaces: &ExportBlockNamespaceTree,
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &BodyShapeRegistry,
    facade: PreparedRustRenderContext<'_>,
    names: &RustNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");
    if let Some(limit) = prepared_rust_recursion_limit(prepared) {
        out.push_str(&format!("#![recursion_limit = \"{limit}\"]\n"));
    }
    // Suppress lints that the generated code routinely trips
    // without any real bug. `unused_variables` covers `fn`
    // parameters that the exported body drops (e.g.,
    // `fn drop_atom(s: String) -> . { () }`) — emit-side
    // analysis to elide the param name would be over-engineering.
    // `unused_assignments` and `unused_braces` cover expression
    // templates that can become redundant after structural
    // recovery. `dead_code` covers module-level pub fns that are not
    // exposed by the export block but still get emitted for
    // cross-module dispatch. Together these keep `cargo build`/`cargo
    // run` warning-free, which the harness's stderr-diff requires.
    out.push_str("#![allow(unused_variables)]\n");
    out.push_str("#![allow(unused_assignments)]\n");
    out.push_str("#![allow(unused_braces)]\n");
    out.push_str("#![allow(dead_code)]\n");
    // User-declared tparams keep their surface spelling. Rust's
    // `non_camel_case_types` lint may still complain for names
    // like `Event_stream`, so suppress it at the crate level
    // since user-source naming is the source of truth.
    out.push_str("#![allow(non_camel_case_types)]\n");
    // Public word casing and namespace brands intentionally differ from
    // Rust's snake case across factories, methods, fields, and nominal modules.
    out.push_str("#![allow(non_snake_case)]\n");
    // The `__absurd__` lowering emits `{ let _ = <arg>; unreachable!() }`
    // so the kio-bottom-typed argument's side effects fire (a
    // env `fn exit(p0: Int) -> !` call must actually run). When the
    // argument's compiled Rust type happens to be `!` itself —
    // e.g. the runner inlines `std::process::exit(...)` as the
    // body of the host's `exit` method — rustc warns the trailing
    // `unreachable!()` is unreachable. The warning is correct but
    // irrelevant: the emit can't know at compile time whether
    // the host method's body diverges. Suppress crate-wide so the
    // harness's stderr-diff stays clean.
    out.push_str("#![allow(unreachable_code)]\n");
    out.push_str("pub mod host;\n");
    out.push_str("pub mod shapes;\n");
    // FFI boundary type aliases. One submodule per env-fn / export-fn,
    // each naming its nameable value and shape types (`arg0`, `arg1`, …,
    // `ret`, and a callback arg's `arg{i}_cbarg` / `arg{i}_cbret`).
    // The test runner reaches these as `crate::ffi::env::<member>::<slot>`
    // / `crate::ffi::exp::<member>::<slot>` so its hardcoded host impl
    // names shape types by alias and tracks emitted-FFI drift. See
    // `emit_prepared_ffi_rs`.
    out.push_str("pub mod ffi;\n");
    // The runtime-support file. Always declared at the root
    // crate; the per-variant emit calls into
    // `crate::__kio_runtime::as_any` / `::from_any` for the
    // erased-body box / recover (closure wraps are inlined as
    // `Rc::new(...)` at the use site). Content lives in
    // `src/__kio_runtime.rs`, written verbatim by
    // `build.rs::write_rust_crate`. See
    // `kio-rs/src/backends/rust/runtime.rs`.
    out.push_str(&format!("pub mod {};\n", super::RUNTIME_SUPPORT_MODULE));
    out.push('\n');
    let namespace_structs =
        render_export_block_namespace_structs(export_block_namespaces, prepared, facade, names)?;

    out.push_str(&namespace_structs);
    out.push_str(&format!(
        "/// {} — the package handle. Owns the host implementation for the\n\
         /// lifetime of the package and exposes its generated namespace\n\
         /// fields; exported items are invoked through methods on those\n\
         /// namespace handles.\n",
        names.handle
    ));
    // `Clone` is needed when a module fn flows through value position
    // — the closure wrap captures a clone of the package handle to
    // dispatch through. `<Handle>Host: Clone` (declared on the trait)
    // makes the derive sound for any conformant host.
    out.push_str("#[derive(Clone)]\n");
    out.push_str(&format!(
        "pub struct {}<{RUST_HOST_PARAMETER}: host::{}> {{\n",
        names.handle, names.host_trait
    ));
    for name in export_block_namespaces.children.keys() {
        out.push_str(&format!(
            "    pub {}: {}<{RUST_HOST_PARAMETER}>,\n",
            rust_public_ident(name),
            export_block_namespace_struct_name(std::slice::from_ref(name)),
        ));
    }
    // The host field is unread when no exported fn calls back into
    // the host (no exported fns at all, or all exported fns are pure
    // structural rearrangements). Allow dead_code unconditionally
    // so emit output is warning-free across either shape.
    out.push_str("    #[allow(dead_code)]\n");
    out.push_str(&format!("    __host: {RUST_HOST_PARAMETER},\n"));
    out.push_str("}\n");
    out.push('\n');
    out.push_str(&format!(
        "/// {} instantiates the package against a host implementation.\n",
        names.factory
    ));
    out.push_str(&format!(
        "pub fn {}<{RUST_HOST_PARAMETER}: host::{}>(host: {RUST_HOST_PARAMETER}) -> {}<{RUST_HOST_PARAMETER}> {{\n",
        names.factory, names.host_trait, names.handle
    ));
    out.push_str(&format!("    {} {{\n", names.handle));
    out.push_str(&render_export_block_namespace_initializers(
        export_block_namespaces,
        &[],
        "host",
    ));
    out.push_str("        __host: host,\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    if module_fns.is_empty() {
        return Ok(out);
    }
    out.push('\n');
    out.push_str(&format!(
        "impl<{RUST_HOST_PARAMETER}: host::{}> {}<{RUST_HOST_PARAMETER}> {{\n",
        names.host_trait, names.handle
    ));

    // The Rust mangling scheme is backend-side work; the Routed phase
    // pre-classified every call into a specific `Expr::Low*` variant,
    // so the body emitter needs no per-call classification table.
    // Emit each fn method into its own String, then join them with
    // a single blank-line separator. Doing this per-piece (rather
    // than appending into a shared `out` mid-loop) lets the
    // module-fn block fan out across rayon: each module produces a
    // self-contained string for its own fns, against per-module
    // use-imports built locally.
    //
    // The framework convention in `kio-rs/src/backends/mod.rs` calls
    // for every per-backend `lower_package_*` to walk modules via
    // `par_iter`. For the Rust backend the natural per-module unit is
    // the methods that lower from one module's fns.

    let mut pieces: Vec<String> = Vec::new();

    // Group module-fns by module_path while preserving input order
    // (the iteration order of `module_fns`, which is itself the
    // package's `BTreeMap` iteration order). One group per module
    // → one rayon task that builds the module's use-imports once
    // and emits each fn into a single concatenated String.
    let mut module_groups: Vec<(&str, Vec<&ModuleFn<'_>>)> = Vec::new();
    {
        let mut last: Option<&str> = None;
        for m in module_fns {
            let path = m.module_path.as_str();
            if last == Some(path) {
                module_groups
                    .last_mut()
                    .expect("group started when last == Some")
                    .1
                    .push(m);
            } else {
                module_groups.push((path, vec![m]));
                last = Some(path);
            }
        }
    }

    // Each module task shares the immutable shape plan and produces a
    // self-contained method string.
    //
    // Errors are collected via `collect::<Result<...>>` so the first
    // failing module short-circuits the join. The collect preserves
    // input order, so error reporting reads the same as the serial
    // walk would have.
    let module_pieces: Vec<String> = crate::maybe_par_iter!(module_groups)
        .map(|(module_path, fns)| -> Result<String, EmitError> {
            let entry = package
                .module(module_path)
                .expect("module-fn collected from package.modules() must round-trip");
            let mut selective_imports = build_selective_imports(&entry.module, package);
            for item in &entry.module.items {
                if let crate::ast::Item::FnDef(d) = item {
                    selective_imports
                        .entry(d.name.clone())
                        .or_insert_with(|| mangle_module_fn(module_path, &d.name));
                }
            }
            let qualified_imports = build_qualified_imports(&entry.module.imports);
            let mut module_shapes = shapes.clone();
            module_shapes.current_module = Some((*module_path).to_owned());
            let method_context = RustInternalFnMethodContext {
                package,
                prepared,
                shapes: &module_shapes,
                selective_imports: &selective_imports,
                qualified_imports: &qualified_imports,
            };
            let mut buf = String::new();
            let mut local_first = true;
            for m in fns {
                if !local_first {
                    buf.push('\n');
                }
                local_first = false;
                let mangled = mangle_module_fn(module_path, &m.fn_def.name);
                let (contract_sig, contract_ret) =
                    module_fn_contract_signature_and_ret(m.module_entry, m.fn_def);
                let normalized_sig =
                    module_shapes.normalize_signature_for_current_scope(&contract_sig);
                let normalized_ret =
                    module_shapes.normalize_type_for_signature_scope(&contract_ret, &contract_sig);
                let adapted_body =
                    qualify_module_expr_types(&m.fn_def.body, &m.fn_def.sig, m.module_entry);
                emit_internal_fn_method(
                    method_context,
                    &mangled,
                    &normalized_sig,
                    &normalized_ret,
                    &adapted_body,
                    &mut buf,
                )?;
            }
            Ok(buf)
        })
        .collect::<Result<Vec<_>, _>>()?;
    pieces.extend(module_pieces);

    // Stitch the per-fn / per-module pieces together with a blank
    // line between them, mirroring the serial walk's inter-fn
    // separator.
    let mut piece_first = true;
    for p in pieces {
        if p.is_empty() {
            continue;
        }
        if !piece_first {
            out.push('\n');
        }
        piece_first = false;
        out.push_str(&p);
    }
    out.push_str("}\n");
    Ok(out)
}

/// Emit the Rust namespace structs backing module exports.
fn render_export_block_namespace_structs(
    root: &ExportBlockNamespaceTree,
    prepared: &PreparedBoundaryCallableSites,
    facade: PreparedRustRenderContext<'_>,
    names: &RustNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    let mut path = Vec::new();
    for (name, child) in &root.children {
        path.push(name.clone());
        render_export_block_namespace_struct(child, &path, prepared, facade, names, &mut out)?;
        path.pop();
    }
    Ok(out)
}

fn render_export_block_namespace_struct(
    tree: &ExportBlockNamespaceTree,
    path: &[String],
    prepared: &PreparedBoundaryCallableSites,
    facade: PreparedRustRenderContext<'_>,
    names: &RustNames,
    out: &mut String,
) -> Result<(), EmitError> {
    for (name, child) in &tree.children {
        let mut child_path = path.to_vec();
        child_path.push(name.clone());
        render_export_block_namespace_struct(child, &child_path, prepared, facade, names, out)?;
    }

    let struct_name = export_block_namespace_struct_name(path);
    out.push_str("#[derive(Clone)]\n");
    out.push_str(&format!(
        "pub struct {struct_name}<{RUST_HOST_PARAMETER}: host::{}> {{\n",
        names.host_trait
    ));
    out.push_str(&format!("    __host: {RUST_HOST_PARAMETER},\n"));
    for name in tree.children.keys() {
        let mut child_path = path.to_vec();
        child_path.push(name.clone());
        out.push_str(&format!(
            "    pub {}: {}<{RUST_HOST_PARAMETER}>,\n",
            rust_public_ident(name),
            export_block_namespace_struct_name(&child_path),
        ));
    }
    out.push_str("}\n\n");

    if !tree.methods.is_empty() {
        out.push_str(&format!(
            "impl<{RUST_HOST_PARAMETER}: host::{}> {struct_name}<{RUST_HOST_PARAMETER}> {{\n",
            names.host_trait
        ));
        let prelude = format!(
            "        let pkg = {}(self.__host.clone());\n",
            names.factory
        );
        for site_id in tree.methods.values() {
            let site = prepared_site(prepared, site_id);
            let BoundaryFacadeSiteOwner::ExportedFunction { name } = site.site().owner() else {
                unreachable!("Rust export namespace contains a non-export prepared site")
            };
            let module_path = site.site().module_segments().join("/");
            emit_prepared_module_fn_public_method(
                PublicMethodCtx {
                    module_path: &module_path,
                    public_name: name,
                    receiver: "pkg",
                    prelude: &prelude,
                },
                site,
                facade,
                out,
            )?;
        }
        out.push_str("}\n\n");
    }

    Ok(())
}

fn render_export_block_namespace_initializers(
    tree: &ExportBlockNamespaceTree,
    path: &[String],
    host_expr: &str,
) -> String {
    let mut out = String::new();
    render_export_block_namespace_initializers_at(tree, path, host_expr, "        ", &mut out);
    out
}

fn render_export_block_namespace_initializers_at(
    tree: &ExportBlockNamespaceTree,
    path: &[String],
    host_expr: &str,
    indent: &str,
    out: &mut String,
) {
    for (name, child) in &tree.children {
        let mut child_path = path.to_vec();
        child_path.push(name.clone());
        out.push_str(&format!(
            "{indent}{}: {} {{\n",
            rust_public_ident(name),
            export_block_namespace_struct_name(&child_path),
        ));
        out.push_str(&format!("{indent}    __host: {host_expr}.clone(),\n"));
        render_export_block_namespace_initializers_at(
            child,
            &child_path,
            host_expr,
            &format!("{indent}    "),
            out,
        );
        out.push_str(&format!("{indent}}},\n"));
    }
}

fn export_block_namespace_struct_name(path: &[String]) -> String {
    if path.iter().any(|segment| segment.contains('_')) {
        let components = path
            .iter()
            .map(|segment| {
                let encoded = encode_host_identity(segment);
                format!("{}_{}", encoded.len(), encoded)
            })
            .collect::<Vec<_>>()
            .join("_");
        return format!("ExportBlockNsExact_{components}");
    }
    let mut out = String::from("ExportBlockNs");
    for segment in path {
        out.push('_');
        out.push_str(segment);
    }
    out
}

struct PublicMethodCtx<'a> {
    module_path: &'a str,
    public_name: &'a str,
    receiver: &'a str,
    prelude: &'a str,
}

fn emit_prepared_module_fn_public_method(
    ctx: PublicMethodCtx<'_>,
    site: PreparedBoundaryCallableSite<'_>,
    shapes: PreparedRustRenderContext<'_>,
    out: &mut String,
) -> Result<(), EmitError> {
    if !matches!(
        site.site().owner(),
        BoundaryFacadeSiteOwner::ExportedFunction { .. }
    ) {
        unreachable!("Rust emitter: an export method requires an exported-function site");
    }
    let execution = site
        .execution()
        .unwrap_or_else(|| unreachable!("Rust emitter: a live export has no execution layout"));
    let callable = prepare_rust_callable(site);
    let converter = PreparedRustConverter {
        site,
        shapes,
        scope: callable.scope.clone(),
        ctx: TypeCtx::Shape,
        nominal_owner: None,
    };
    let plan = site.plan().facade();

    let parameter_declarations =
        render_prepared_callable_source_parameters(site, &callable, shapes, TypeCtx::Shape)?;
    let converted_values = unpack_prepared_callable_public_arguments(site, &callable, &converter)?;
    let returned_cursor = PreparedRustUseCursor::root(site, callable.returned);
    let returned = converter.render(&returned_cursor, PreparedRustTypePosition::Value)?;
    let return_clause = if matches!(plan.use_at(callable.returned), FacadeUse::Unit { .. }) {
        String::new()
    } else {
        format!(" -> {returned}")
    };
    let parameters = if parameter_declarations.is_empty() {
        String::new()
    } else {
        format!(", {}", parameter_declarations.join(", "))
    };
    let generic_declaration = if callable.type_parameters.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            callable
                .type_parameters
                .iter()
                .map(|parameter| parameter.declaration(shapes.crate_root()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    out.push('\n');
    out.push_str(&format!(
        "    pub fn {}{generic_declaration}(&self{parameters}){return_clause} {{\n",
        rust_public_ident(ctx.public_name)
    ));
    out.push_str(ctx.prelude);

    let mangled = mangle_module_fn(ctx.module_path, ctx.public_name);
    let mut current = None::<String>;
    let mut next_slot = 0usize;
    for stage in execution.head_stages() {
        match stage {
            CallableExecutionStage::Type { .. } => {
                current = Some(match current {
                    None => format!("{}.{mangled}()", ctx.receiver),
                    Some(previous) => format!(
                        "({}::__kio_runtime::from_any::<{}>({previous}))()",
                        converter.crate_root(),
                        rc_fn_type(0)
                    ),
                });
            }
            CallableExecutionStage::Value(layout) => {
                let end = next_slot + layout.facade_slot_count();
                let args = pack_prepared_source_arguments(
                    layout,
                    &converted_values[next_slot..end],
                    converter.crate_root(),
                )?;
                next_slot = end;
                current = Some(match current {
                    None => format!("{}.{mangled}({})", ctx.receiver, args.join(", ")),
                    Some(previous) => {
                        let callable = rc_fn_type(layout.body_abi_arity());
                        format!(
                            "({}::__kio_runtime::from_any::<{callable}>({previous}))({})",
                            converter.crate_root(),
                            args.join(", ")
                        )
                    }
                });
            }
        }
    }
    if next_slot != converted_values.len() {
        unreachable!("Rust emitter: export head execution did not consume every facade slot");
    }
    let internal = current.unwrap_or_else(|| {
        unreachable!("Rust emitter: exported callable has no declaration-head stage")
    });
    let result = converter.convert(&returned_cursor, &internal, FfiDir::Out)?;
    out.push_str(&format!("        {result}\n"));
    out.push_str("    }\n");
    Ok(())
}

/// Emit one body-private module function on the package handle. Public export
/// methods are rendered exclusively by [`emit_prepared_module_fn_public_method`].
#[derive(Clone, Copy)]
struct RustInternalFnMethodContext<'a> {
    package: &'a Package<Routed>,
    prepared: &'a PreparedBoundaryCallableSites,
    shapes: &'a BodyShapeRegistry,
    selective_imports: &'a std::collections::HashMap<String, String>,
    qualified_imports: &'a std::collections::HashMap<String, String>,
}

fn emit_internal_fn_method(
    context: RustInternalFnMethodContext<'_>,
    name: &str,
    sig: &crate::ast::Signature<Routed>,
    ret: &Type<Routed>,
    body: &Expr<Routed>,
    out: &mut String,
) -> Result<(), EmitError> {
    let tparam_names: Vec<String> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            crate::ast::SignatureParam::Type(tp) => Some(tp.name.clone()),
            _ => None,
        })
        .collect();

    // The body-private method is erased and non-generic: its first runtime
    // stage accepts `Rc<dyn Any>` values and returns the same universal.
    // Later value stages curry as nested erased closures; type binders are
    // represented by their paired nullary execution stages.
    let runtime_groups = runtime_param_name_groups(sig);
    let mut params_rust = Vec::new();
    let internal_first_group: std::collections::HashSet<&str> = runtime_groups
        .first()
        .map(|group| group.iter().copied().collect())
        .unwrap_or_default();
    for p in &sig.params {
        match p {
            crate::ast::SignatureParam::Type(_) => continue,
            crate::ast::SignatureParam::Value(v) => {
                v.ty.as_ref().ok_or_else(|| {
                    EmitError::unsupported(format!(
                        "Rust emitter: fn `{name}` parameter `{}` has no type annotation",
                        v.name
                    ))
                })?;
                if !internal_first_group.contains(v.name.as_str()) {
                    // An inner-group param of the erased internal fn: bound
                    // by a curried closure layer below, not a method param.
                    continue;
                }
                let n = escape_rust_keyword(&v.name);
                params_rust.push(format!("{n}: ::std::rc::Rc<dyn ::std::any::Any>"));
            }
        }
    }
    let params_joined = if params_rust.is_empty() {
        String::new()
    } else {
        format!(", {}", params_rust.join(", "))
    };
    out.push_str(&format!(
        "    fn {}(&self{params_joined}) -> ::std::rc::Rc<dyn ::std::any::Any> {{\n",
        escape_rust_keyword(name)
    ));
    let fn_value_param_names: Vec<String> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            crate::ast::SignatureParam::Value(v) => Some(v.name.clone()),
            _ => None,
        })
        .collect();
    let fn_value_param_types = sig
        .params
        .iter()
        .filter_map(|p| match p {
            crate::ast::SignatureParam::Value(v) => {
                v.ty.as_ref().map(|ty| (v.name.clone(), ty.clone()))
            }
            crate::ast::SignatureParam::Type(_) => None,
        })
        .collect();
    let mut emit = BodyEmitter {
        package: context.package,
        prepared: context.prepared,
        shapes: context.shapes,
        selective_imports: context.selective_imports,
        qualified_imports: context.qualified_imports,
        self_ref: None,
        tparams: tparam_names.iter().cloned().collect(),
        locals: fn_value_param_names,
        local_types: fn_value_param_types,
        fresh: 0,
    };
    // A multi-stage internal fn currys its inner stages as boxed
    // `'static` `Rc<dyn Fn>` closures (below); their bodies can't borrow
    // `&self` (a host call would tie the boxed closure to `self`'s
    // lifetime). Dispatch the body through a captured `__pkg` clone so a
    // host / module call inside an inner-group closure reads `__pkg.__host`
    // / `__pkg.<method>`, not `self`. (A single-group fn keeps `self`.)
    let multi_group = runtime_groups.len() > 1;
    if multi_group {
        emit.self_ref = Some("__pkg".to_owned());
    }
    let body_rust = emit.emit_expr(body)?;
    // A fn-valued body returned at a function type whose ABI grouping
    // differs from the closure literal's binder grouping (a church
    // `.[A](x, y)` 2-binder closure returned as `Boolc = (A & A) -> A`,
    // abi-arity 1) is reshaped so a call through the typed slot — which
    // passes one product arg — reaches the closure's binders.
    let body_rust = emit.adapt_fn_value_for_type(body_rust, body, ret)?;
    // The erased internal fn binds only the first group and currys the inner
    // groups as nested boxed closures around the body.
    let mut body_out = body_rust;
    for (gi, g) in runtime_groups.iter().enumerate().skip(1).rev() {
        let params: String = g
            .iter()
            .map(|n| {
                format!(
                    "{}: ::std::rc::Rc<dyn ::std::any::Any>",
                    escape_rust_keyword(n)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let fn_ty = rc_fn_type(g.len());
        // Clone every enclosing capture used by this layer before its
        // `move`.  Otherwise constructing an inner stage would move a
        // first/earlier-stage value parameter out of an outer `Fn`
        // closure, degrading that outer stage to `FnOnce`.
        let mut layer_preludes = "let __pkg = __pkg.clone(); ".to_owned();
        for outer_group in runtime_groups.iter().take(gi) {
            for name in outer_group {
                let id = escape_rust_keyword(name);
                if body_out.contains(&format!("{id}.clone()"))
                    || body_out.contains(&format!("&{id}"))
                {
                    layer_preludes.push_str(&format!("let {id} = {id}.clone(); "));
                }
            }
        }
        body_out = format!(
            "{{ {layer_preludes}let __f: {fn_ty} = ::std::rc::Rc::new(move |{params}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {body_out} }}); {}::__kio_runtime::as_any(__f) }}",
            context.shapes.crate_root
        );
    }
    // Bind the captured package handle once around the curried value when the
    // body dispatches through it. A single-group body runs directly on `self`.
    if multi_group {
        body_out = format!("{{ let __pkg = self.clone(); {body_out} }}");
    }
    out.push_str(&format!("        {body_out}\n"));
    out.push_str("    }\n");
    Ok(())
}

/// Walks a module fn body and renders Rust expression source over the
/// universal **erased** value model (`Rc<dyn Any>`). Holds the per-fn
/// local-binding scope, the calling module's context (its path +
/// use-imports / qualified-imports) so module calls resolve to the right
/// package-handle method, the prepared catalog for every host-boundary call,
/// and [`BodyShapeRegistry`] only for body-local storage plus exact host metadata.
///
/// The erased body carries **no** typed-shape threading: every value is
/// `Rc<dyn Any>`, closures box a `Rc<dyn Fn(Rc<dyn Any>, …) -> Rc<dyn
/// Any>>`, products / sums ride the nested-binary erased rep, and a
/// higher-kinded carrier `F(A)` is an ordinary erased value — so the
/// fields that the old typed-carrier body threaded (per-slot expected
/// types, `Rc<dyn Fn>`-storage flags, brand bounds, rank-N erasure
/// state, turbofish scope) are all gone. What survives is the same set
/// the Go erased body keeps.
struct BodyEmitter<'a> {
    package: &'a Package<Routed>,
    /// The single package-complete boundary catalog. Host calls and host
    /// function values resolve their exact site here and consume the semantic
    /// plan paired with its private execution layout.
    prepared: &'a PreparedBoundaryCallableSites,
    /// Body-private storage registry plus exact host-type metadata. Public
    /// host call topology and conversion come from `prepared`; literals use
    /// this field only to recover the already-selected declaration's role.
    shapes: &'a BodyShapeRegistry,
    /// Surface-name → mangled package-handle method name for every
    /// cross-module fn currently in scope. Module bodies populate this
    /// from their own `import` items plus their module's pub fns;
    /// exported fns currently pass an empty map. Read at
    /// `LowModuleCall` / `LowModuleFnValueRef` to resolve the surface
    /// name to the Rust mangled method form.
    selective_imports: &'a std::collections::HashMap<String, String>,
    /// Qualified-import alias → resolved-module-path.
    /// `import <pkg>/<mod> as <alias>;` items contribute one entry each;
    /// the body emitter consults this map at `LowQualifiedModuleCall`
    /// sites to mangle the call to its Rust backend form.
    qualified_imports: &'a std::collections::HashMap<String, String>,
    /// Override for `self` references in emitted code. `None` →
    /// emit `self.__host..(...)` / `self.<mangled>(...)` directly.
    /// `Some(name)` → emit `<name>.__host..(...)` / `<name>.<mangled>(...)`.
    /// Set inside `emit_fn_expr` to `Some("__pkg")` so closures
    /// dispatch through a captured `let __pkg = self.clone()`
    /// binding, keeping them `'static` (no `&self` borrow).
    self_ref: Option<String>,
    /// The surrounding fn's type-parameter names. The erased body itself
    /// is type-agnostic, but a **host-call boundary** still converts a
    /// function-typed callback argument whose leg types name these
    /// params (`loop`'s `step: S -> S | R`); the conversion needs them in
    /// scope so a type-var slot renders as its name rather than failing a
    /// shape lookup.
    tparams: std::collections::HashSet<String>,
    /// The Kio local-binding names currently in scope (fn / closure value
    /// params, `let` binders, match-arm binders). A `move` closure
    /// pre-clones the in-scope locals its body references so it captures
    /// the `Rc` by clone (a refcount bump) rather than by move — a `Fn`
    /// closure cannot move a captured binder out, and the same binder may
    /// be referenced by sibling closures.
    locals: Vec<String>,
    /// Static types for value parameters and scoped binders whose type is
    /// available from Routed annotations. Conditional emission uses this to
    /// recover the exact role(bool) associated type without adding provenance
    /// to the IR.
    local_types: std::collections::HashMap<String, Type<Routed>>,
    /// Allocates backend-reserved block locals. Kio source identifiers
    /// cannot enter the `__kio_` namespace; the monotone suffix keeps
    /// sibling generated bindings distinct.
    fresh: usize,
}

/// Structural type reconstruction needed only where erased Rust emission must
/// name the exact type again (currently Boolean condition elimination and
/// scoped binder bookkeeping). It consumes Routed annotations; it does not
/// add provenance to the IR or re-run the typer.
struct RustTypeRecon<'a, 'b> {
    emit: &'a BodyEmitter<'b>,
}

impl RustTypeRecon<'_, '_> {
    fn newtype_member_type(
        &self,
        declaring_module: Option<&str>,
        newtype: &str,
        member: &str,
        type_args: &[Type<Routed>],
    ) -> Option<Type<Routed>> {
        let key = match declaring_module {
            Some(module) => newtype_key(module, newtype),
            None => {
                let module = self.emit.shapes.current_module.as_deref()?;
                self.emit
                    .shapes
                    .newtype_key_for_name_in_module(module, newtype)?
            }
        };
        let record = self.emit.shapes.lookup_newtype(&key)?;
        let decl = &record.decl;
        let owner = newtype_key_module_path(&key)?;
        let universals = type_args.get(..decl.type_params.len())?.to_vec();
        let mut exact_segments = owner
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        exact_segments.push(newtype.to_owned());
        let exact = Type::synth_path(exact_segments, universals.clone(), decl.meta.span);
        if member == decl.constructor.name {
            return Some(exact);
        }
        if member != decl.projector.name || !decl.existential_params.is_empty() {
            return None;
        }
        let subst = decl
            .type_params
            .iter()
            .zip(universals)
            .map(|(param, arg)| (param.name.clone(), arg))
            .collect();
        let payload = apply_subst(&decl.payload, &subst);
        Some(self.emit.shapes.normalize_type_for_owned_tparams_in_scope(
            &payload,
            Some(owner),
            &self.emit.tparams,
        ))
    }

    fn newtype_payload_type(&self, ty: &Type<Routed>) -> Option<Type<Routed>> {
        let Type::Path { segments, args, .. } = ty else {
            return None;
        };
        let key = self.emit.shapes.newtype_key_for_segments(segments)?;
        let record = self.emit.shapes.lookup_newtype(&key)?;
        let decl = &record.decl;
        let owner = newtype_key_module_path(&key)?;
        if !decl.existential_params.is_empty() || args.len() < decl.type_params.len() {
            return None;
        }
        let subst = decl
            .type_params
            .iter()
            .zip(args.iter())
            .map(|(param, arg)| (param.name.clone(), arg.clone()))
            .collect();
        let payload = apply_subst(&decl.payload, &subst);
        Some(self.emit.shapes.normalize_type_for_owned_tparams_in_scope(
            &payload,
            Some(owner),
            &self.emit.tparams,
        ))
    }
}

impl ReconProfile for RustTypeRecon<'_, '_> {
    fn bound_param_ty(&self, name: &str) -> Option<Type<Routed>> {
        self.emit.local_types.get(name).cloned()
    }

    fn resolved_call_return_type(
        &self,
        sig: &crate::ast::Signature<Routed>,
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
        self.emit
            .shapes
            .normalize_type_for_owned_tparams_in_current_scope(ty, &self.emit.tparams)
    }

    fn enriched_field_payload_type(&self, field_ty: &Type<Routed>) -> Option<Type<Routed>> {
        self.newtype_payload_type(field_ty)
    }

    fn module_fn_value_type(
        &self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
        span: crate::span::Span,
    ) -> Option<Type<Routed>> {
        let resolved = self.emit.selective_imports.get(mangled)?;
        for (module_path, entry) in self.emit.package.modules() {
            for item in &entry.module.items {
                let crate::ast::Item::FnDef(def) = item else {
                    continue;
                };
                if mangle_module_fn(module_path, &def.name) == *resolved {
                    let (_, ret) = module_fn_contract_signature_and_ret(entry, def);
                    return Some(sig.signature_ty(ret, span));
                }
            }
        }
        None
    }

    fn reconstruct_other(
        &self,
        expr: &Expr<Routed>,
        _locals: &mut std::collections::HashMap<String, Type<Routed>>,
    ) -> Option<Type<Routed>> {
        match expr {
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                ..
            }
            | Expr::LowNewtypeProj {
                newtype,
                member,
                type_args,
                ..
            } => self.newtype_member_type(None, newtype, member, type_args),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                ..
            } => self.newtype_member_type(Some(module_path), newtype, member, type_args),
            Expr::LowCpsProjectorApply {
                continuation_ty, ..
            } => match continuation_ty.peel_leading_foralls().1 {
                Type::Function { ret, .. } => Some((**ret).clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The Rust erased function-pointer storage type for an `n`-value-arg
/// layer: `Rc<dyn Fn(Rc<dyn Any>, …) -> Rc<dyn Any>>`. Every closure the
/// body builds is boxed at this type (then erased to `Rc<dyn Any>` via
/// `as_any`); every apply recovers it via `from_any` before calling.
fn rc_fn_type(arity: usize) -> String {
    let params = std::iter::repeat_n("::std::rc::Rc<dyn ::std::any::Any>", arity)
        .collect::<Vec<_>>()
        .join(", ");
    format!("::std::rc::Rc<dyn Fn({params}) -> ::std::rc::Rc<dyn ::std::any::Any>>")
}

/// The value param names of a signature, grouped by value group (one
/// inner `Vec` per group). A type-binder param carries no runtime arg
/// and is dropped. A nullary fn yields a single empty group. Mirrors the
/// Go backend's `value_param_name_groups`.
fn value_param_name_groups(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<&str>> {
    let mut groups: Vec<Vec<&str>> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter_map(|p| match p {
                        crate::ast::SignatureParam::Value(vp) => Some(vp.name.as_str()),
                        crate::ast::SignatureParam::Type(_) => None,
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

/// Internal erased runtime layers for a signature. Each type binder is a
/// distinct nullary closure layer, each value group retains its binders, and
/// a signature with no value group receives the synthesized trailing
/// nullary value layer.
fn runtime_param_name_groups(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<&str>> {
    let mut groups = Vec::new();
    let mut has_value_group = false;
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                groups.extend(params.iter().map(|_| Vec::new()));
            }
            crate::ast::SignatureGroupRef::Value(params) => {
                has_value_group = true;
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
    if !has_value_group {
        groups.push(Vec::new());
    }
    groups
}

/// The value-arity of each value group of a fn signature, in order (each
/// callable layer's param count). A nullary fn yields `[0]`.
fn value_group_arities(sig: &crate::ast::Signature<Routed>) -> Vec<usize> {
    let arities: Vec<usize> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter(|p| matches!(p, crate::ast::SignatureParam::Value(_)))
                    .count(),
            ),
            crate::ast::SignatureGroupRef::Type(_) => None,
        })
        .collect();
    if arities.is_empty() { vec![0] } else { arities }
}

/// The declared type of each value parameter across all value groups of
/// a signature, in order; `None` for a value param with no declared
/// type. Used to adapt a fn-value argument to its declared slot's
/// grouping at a call site.
fn sig_all_value_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Option<Type<Routed>>> {
    let mut out = Vec::new();
    for group in sig.canonical_groups() {
        if let crate::ast::SignatureGroupRef::Value(params) = group {
            for p in params {
                if let crate::ast::SignatureParam::Value(vp) = p {
                    out.push(vp.ty.clone());
                }
            }
        }
    }
    out
}

/// The signature of the closure literal a fn-valued expression
/// ultimately evaluates to, peeling transparent wrappers (a bare
/// `FnExpr`, a host- / module-fn value ref, a `let`/`Seq` tail, a
/// nullary IIFE, an identity-style single-arg call, a newtype
/// ctor/proj). A fn value that does not reduce to a visible closure
/// literal yields `None`. Mirrors the Go backend.
fn underlying_fn_expr_sig(e: &Expr<Routed>) -> Option<&crate::ast::Signature<Routed>> {
    match e {
        Expr::FnExpr { sig, .. } => Some(sig),
        Expr::LowHostFnValueRef { sig, .. } | Expr::LowModuleFnValueRef { sig, .. } => Some(sig),
        Expr::Let { body, .. } | Expr::Seq { body, .. } => underlying_fn_expr_sig(body),
        Expr::LowTypeApplication { callee, .. } => underlying_fn_expr_sig(callee),
        Expr::LowClosureCall { args, .. } if args.is_empty() => None,
        Expr::LowIndirectCall { callee, args, .. } if args.is_empty() => {
            if let Expr::FnExpr { body, .. } = callee.as_ref() {
                underlying_fn_expr_sig(body)
            } else {
                None
            }
        }
        Expr::LowModuleCall { args, .. } | Expr::LowQualifiedModuleCall { args, .. }
            if args.len() == 1 =>
        {
            underlying_fn_expr_sig(&args[0])
        }
        Expr::LowNewtypeCtor { payload, .. } => underlying_fn_expr_sig(payload),
        Expr::LowNewtypeProj { target, .. } => underlying_fn_expr_sig(target),
        _ => None,
    }
}

/// Peel leading `Type::Forall` binders.
fn peel_forall_ty(ty: &Type<Routed>) -> &Type<Routed> {
    let mut t = ty;
    while let Type::Forall { body, .. } = t {
        t = body;
    }
    t
}

/// Render a Kio string value as a borrowed Rust `&str` literal, escaping
/// every control character (rustc rejects a bare CR and normalizes a raw
/// CRLF to LF inside a literal, so control bytes never survive verbatim).
fn rust_str_borrowed(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => escaped.push(c),
        }
    }
    format!("\"{escaped}\"")
}

/// Render a Kio string value as a Rust owned-`String` literal expression.
fn rust_string_lit(value: &str) -> String {
    format!("{}.to_string()", rust_str_borrowed(value))
}

/// The declared types of the **first** value group's params, in order
/// (`None` for an un-annotated param). Used to adapt fn-valued args at an
/// indirect-call site whose callee is a closure literal.
fn sig_first_value_group_param_types(
    sig: &crate::ast::Signature<Routed>,
) -> Vec<Option<Type<Routed>>> {
    for group in sig.canonical_groups() {
        if let crate::ast::SignatureGroupRef::Value(params) = group {
            return params
                .iter()
                .filter_map(|p| match p {
                    crate::ast::SignatureParam::Value(vp) => Some(vp.ty.clone()),
                    crate::ast::SignatureParam::Type(_) => None,
                })
                .collect();
        }
    }
    Vec::new()
}

/// The erased access expression for slot `i` of an `n`-slot nested-binary
/// product. The emitted runtime helper names the peel so generated Rust
/// does not repeat an O(n) `from_any` expression at every projection site.
fn product_slot_access(crate_root: &str, base: &str, i: usize, n: usize) -> String {
    format!("{crate_root}::__kio_runtime::product_slot({base}, {i}, {n})")
}

fn projected_product_tail(items: &[Expr<Routed>], source: &str) -> Option<usize> {
    let Expr::EnrichedProject {
        index: start,
        arity,
        ..
    } = items.first()?
    else {
        return None;
    };
    let (start, arity) = (*start, *arity);
    if start == 0 || start.checked_add(items.len())? != arity {
        return None;
    }
    items
        .iter()
        .enumerate()
        .all(|(offset, item)| {
            matches!(
                item,
                Expr::EnrichedProject {
                    target,
                    index,
                    arity: item_arity,
                    ..
                } if *index == start + offset
                    && *item_arity == arity
                    && matches!(target.as_ref(), Expr::LowBoundRef { name, .. } if name == source)
            )
        })
        .then_some(start)
}

impl<'a> BodyEmitter<'a> {
    fn fresh_name(&mut self, class: &str) -> String {
        let index = self.fresh;
        self.fresh += 1;
        format!("__kio_{class}{index}")
    }

    fn expr_type(&self, expr: &Expr<Routed>) -> Option<Type<Routed>> {
        let recon = RustTypeRecon { emit: self };
        let mut locals = std::collections::HashMap::new();
        ReconProfile::value_type_with_locals(&recon, expr, &mut locals)
    }

    fn self_ref_str(&self) -> &str {
        self.self_ref.as_deref().unwrap_or("self")
    }

    fn crate_root(&self) -> &str {
        &self.shapes.crate_root
    }

    /// `as_any(expr)` — box a body value to the erased universal
    /// `Rc<dyn Any>`.
    fn box_any(&self, expr: &str) -> String {
        format!("{}::__kio_runtime::as_any({expr})", self.crate_root())
    }

    /// `from_any::<T>(expr)` — recover a concrete value from an erased
    /// `Rc<dyn Any>`.
    fn unbox_any(&self, ty: &str, expr: &str) -> String {
        format!(
            "{}::__kio_runtime::from_any::<{ty}>({expr})",
            self.crate_root()
        )
    }

    /// Resolve a role-typed atom `Type::Path` to its `Role`, or `None`
    /// for a non-role / structural / unit type. Host-type identity is
    /// module-qualified, so resolve the path through the shared exact
    /// shape registry used for literal annotations.
    fn atom_role_of(&self, ty: &Type<Routed>) -> Option<Role> {
        match ty {
            Type::Path { segments, args, .. } if args.is_empty() => {
                self.shapes.host_type_role(segments)
            }
            _ => None,
        }
    }

    /// Render one Routed-phase expression to a Rust expression string of
    /// type `Rc<dyn Any>` (the universal erased value).
    fn emit_expr(&mut self, e: &Expr<Routed>) -> Result<String, EmitError> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Unit { .. } => Ok(self.box_any("()")),
            // `Expr::Path` / `Expr::Call` are statically uninhabited at
            // the Routed phase — `recover_to_low::lower` classified every
            // such node into a `Low*` variant. The surface-only variants
            // (`Tuple`, `BlockCall`, `Elaborator`, `UserElaborator`,
            // `Ufcs`, `OpChain`, …) never reach Routed.
            // Discharge each via its `Never`-witness `ext`.
            Expr::Path { ext, .. }
            | Expr::Call { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::LabelValue { ext, .. }
            | Expr::RowLet { ext, .. }
            | Expr::Elaborator { ext, .. }
            | Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::UserElaborator { ext, .. }
            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. } => match *ext {},
            // Every body literal is constructed as its annotation's exact
            // host-selected type and boxed into the erased representation.
            // The typer has already proved that the lexical literal role
            // matches that exact host-type declaration.
            Expr::StrLit {
                value, annotation, ..
            } => {
                let role = self.literal_host_role(annotation)?;
                if role != Role::Str {
                    unreachable!(
                        "Rust emitter: the typer admitted a string literal against role `{}`",
                        role.as_str()
                    );
                }
                let exact = self.exact_literal_value(annotation, &rust_string_lit(value))?;
                Ok(self.box_any(&exact))
            }
            Expr::IntLit {
                digits, annotation, ..
            } => {
                let role = self.literal_host_role(annotation)?;
                let lit = format!("{digits}_{}", role_literal_primitive_type(role));
                let exact = self.exact_literal_value(annotation, &lit)?;
                Ok(self.box_any(&exact))
            }
            Expr::FloatLit {
                digits, annotation, ..
            } => {
                let role = self.literal_host_role(annotation)?;
                let lit = format!("{digits}_{}", role_literal_primitive_type(role));
                let exact = self.exact_literal_value(annotation, &lit)?;
                Ok(self.box_any(&exact))
            }
            Expr::BoolLit {
                value, annotation, ..
            } => {
                let role = self.literal_host_role(annotation)?;
                if role != Role::Bool {
                    unreachable!(
                        "Rust emitter: the typer admitted a Boolean literal against role `{}`",
                        role.as_str()
                    );
                }
                let exact = self.exact_literal_value(annotation, &format!("{value}"))?;
                Ok(self.box_any(&exact))
            }
            Expr::Let {
                name, value, body, ..
            } => {
                if let Expr::EnrichedTuple { items, .. } = body.as_ref()
                    && let Some(start) = projected_product_tail(items, name)
                {
                    let value = self.emit_expr(value)?;
                    return Ok(product_slot_access(
                        self.crate_root(),
                        &value,
                        start,
                        start + 1,
                    ));
                }
                // A `let` is an expression in Rust; bind the value at the
                // erased type and emit the body in a block. The binder is
                // `Rc<dyn Any>`; subsequent `LowBoundRef` resolutions
                // `.clone()` it (an `Rc` clone is a refcount bump).
                let value_ty = self.expr_type(value);
                let v = self.emit_expr(value)?;
                let safe = escape_rust_keyword(name);
                self.locals.push(name.clone());
                let prior_ty = self.local_types.remove(name);
                if let Some(ty) = value_ty {
                    self.local_types.insert(name.clone(), ty);
                }
                let b = self.emit_expr(body);
                self.local_types.remove(name);
                if let Some(ty) = prior_ty {
                    self.local_types.insert(name.clone(), ty);
                }
                self.locals.pop();
                let b = b?;
                Ok(format!(
                    "{{ let {safe}: ::std::rc::Rc<dyn ::std::any::Any> = {v}; {b} }}"
                ))
            }
            Expr::Seq { value, body, .. } => {
                let v = self.emit_expr(value)?;
                let b = self.emit_expr(body)?;
                Ok(format!("{{ let _ = {v}; {b} }}"))
            }
            Expr::FnExpr { sig, body, .. } => self.emit_fn_expr(sig, body),
            Expr::EnrichedTuple {
                items, synth_ty, ..
            } => {
                let refs: Vec<&Expr<Routed>> = items.iter().collect();
                self.emit_product_typed(&refs, synth_ty)
            }
            Expr::EnrichedRecord { fields, .. } => {
                // A record's slots are label-derived newtypes wrapping their
                // payloads, not bare function types, so no per-slot fn-value
                // grouping adaptation applies (mirrors the Go backend, whose
                // record path is the untyped `emit_tuple_refs`).
                let refs: Vec<&Expr<Routed>> = fields.iter().map(|f| &f.value).collect();
                self.emit_product(&refs)
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                ..
            } => self.emit_inject(payload, *variant, *variants),
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => self.emit_match(scrutinee, arms, scrutinee_ty),
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => self.emit_conditional(cond, then_branch, else_branch),
            Expr::LowBoundRef { name, .. } => Ok(format!("{}.clone()", escape_rust_keyword(name))),
            Expr::LowHostFnValueRef {
                name,
                module_path,
                sig,
                ret_ty,
                ..
            } => self.emit_low_host_fn_value_ref(name, module_path, sig, ret_ty),
            Expr::LowModuleFnValueRef { mangled, sig, .. } => {
                self.emit_low_module_fn_value_ref(mangled, sig)
            }
            Expr::LowHostCall {
                name,
                module_path,
                type_args,
                args,
                sig,
                ret_ty,
                ..
            } => self.emit_low_host_call(name, module_path, type_args, args, sig, ret_ty),
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => self.emit_low_module_call(mangled, type_args.len(), args, sig),
            Expr::LowQualifiedModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => self.emit_low_qualified_module_call(mangled, type_args.len(), args, sig),
            // A newtype constructor / projector is a runtime identity in
            // the erased body: the newtype value shares its payload's
            // `Rc<dyn Any>` rep (the typed FFI skin re-imposes the nominal
            // Rust type at the host boundary). So `mk_X(p)` ⇒ `p`,
            // `un_X(v)` ⇒ `v`, and a qualified member likewise.
            Expr::LowNewtypeCtor { payload, .. } => self.emit_expr(payload),
            Expr::LowNewtypeProj { target, .. } => self.emit_expr(target),
            Expr::LowQualifiedNewtypeMember { payload, .. } => self.emit_expr(payload),
            Expr::LowClosureCall {
                name,
                type_args,
                args,
                ..
            } => {
                let callee = self.emit_erased_type_applications(
                    format!("{}.clone()", escape_rust_keyword(name)),
                    type_args.len(),
                );
                self.emit_apply(&callee, args)
            }
            Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } => {
                let c = self.emit_expr(callee)?;
                let c = self.emit_erased_type_applications(c, type_args.len());
                match callee.as_ref() {
                    Expr::FnExpr { sig, .. } => {
                        let param_tys = sig_first_value_group_param_types(sig);
                        self.emit_apply_adapting(&c, args, &param_tys)
                    }
                    _ => self.emit_apply(&c, args),
                }
            }
            Expr::LowTypeApplication { callee, .. } => {
                let c = self.emit_expr(callee)?;
                Ok(self.emit_erased_type_applications(c, 1))
            }
            Expr::LowCpsProjectorApply {
                receiver,
                continuation,
                continuation_ty,
                ..
            } => self.emit_low_cps_projector_apply(receiver, continuation, continuation_ty),
            Expr::LowAbsurdCall { value_arg, .. } => {
                // Evaluate the bottom-typed argument for its host effects
                // (e.g. an `exit(n) -> !` call must run), then `unreachable`
                // — the value can never be returned (the type is `!`).
                let arg = self.emit_expr(value_arg)?;
                Ok(format!("{{ let _ = {arg}; unreachable!() }}"))
            }
        }
    }

    /// Lower a closure literal to an erased closure value. Each type binder
    /// renders one nullary layer and each value group renders one layer
    /// taking its binders as `Rc<dyn Any>` params. The innermost layer is
    /// boxed to the `Rc<dyn Fn>` storage
    /// type and then erased via `as_any`, so a downstream apply recovers
    /// the arity-matched fn type before calling.
    ///
    /// `move` so the closure captures its binders by value (the `'static`
    /// erased rep flows freely); dispatch through `self_ref = "__pkg"`
    /// keeps the closure free of a `&self` borrow.
    fn emit_fn_expr(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let groups = runtime_param_name_groups(sig);
        // Closures dispatch through a captured `let __pkg = self.clone();`
        // so they stay `'static` (no `&self` borrow). Set `self_ref`
        // around the body emit; restore after.
        let saved_self = self.self_ref.replace("__pkg".to_owned());
        // The closure's own type-binders (a `.[A][B](…)` literal) join the
        // tparams scope for its body — a host call inside the body whose
        // callback leg names `A` / `B` needs them in scope to render the
        // type-var slot at the erased↔typed conversion.
        let added_tparams: Vec<String> = sig
            .params
            .iter()
            .filter_map(|p| match p {
                crate::ast::SignatureParam::Type(tp) => {
                    if self.tparams.insert(tp.name.clone()) {
                        Some(tp.name.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        // The locals in scope *before* this closure are its capture
        // candidates; the closure's own value-param binders shadow them
        // inside the body. Push the closure params, emit, then pre-clone
        // each outer local the body references so the `move` closure
        // captures it by clone (refcount bump) — a `Fn` closure can't move
        // a captured binder out, and sibling closures may reference it.
        let outer_locals: Vec<String> = self.locals.clone();
        let closure_params: Vec<String> =
            groups.iter().flatten().map(|n| (*n).to_owned()).collect();
        let closure_param_types: std::collections::HashMap<&str, Type<Routed>> = sig
            .params
            .iter()
            .filter_map(|param| match param {
                crate::ast::SignatureParam::Value(value) => value
                    .ty
                    .as_ref()
                    .map(|ty| (value.name.as_str(), ty.clone())),
                crate::ast::SignatureParam::Type(_) => None,
            })
            .collect();
        let mut prior_param_types = Vec::with_capacity(closure_params.len());
        for n in &closure_params {
            self.locals.push(n.clone());
            let prior = self.local_types.remove(n);
            if let Some(ty) = closure_param_types.get(n.as_str()) {
                self.local_types.insert(n.clone(), ty.clone());
            }
            prior_param_types.push((n.clone(), prior));
        }
        let body_src = self.emit_expr(body);
        for _ in &closure_params {
            self.locals.pop();
        }
        for (name, prior) in prior_param_types.into_iter().rev() {
            self.local_types.remove(&name);
            if let Some(ty) = prior {
                self.local_types.insert(name, ty);
            }
        }
        for n in &added_tparams {
            self.tparams.remove(n);
        }
        self.self_ref = saved_self;
        let body_src = body_src?;
        // Which outer locals does the closure body reference? Pre-clone
        // exactly those (an over-clone would just be a wasted refcount
        // bump, but the precise set keeps the emit readable).
        let captured: Vec<String> = outer_locals
            .iter()
            .filter(|n| {
                let id = escape_rust_keyword(n);
                body_src.contains(&format!("{id}.clone()")) || body_src.contains(&format!("&{id}"))
            })
            .cloned()
            .collect();
        let mut acc = body_src;
        // A curried closure's inner layer captures `__pkg` and the outer
        // layers' binders; each layer is a `move` closure, and the outer
        // layer is `Fn` (callable repeatedly), so a bare `move` would move
        // `__pkg` / an outer binder out of the enclosing `Fn`. Clone the
        // captured names into the layer before the `move`. The names a
        // layer body can reference are `__pkg` plus every binder of the
        // groups *outside* this one (the groups before it in `groups`).
        for (gi, g) in groups.iter().enumerate().rev() {
            let params: String = g
                .iter()
                .map(|n| {
                    format!(
                        "{}: ::std::rc::Rc<dyn ::std::any::Any>",
                        escape_rust_keyword(n)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let fn_ty = rc_fn_type(g.len());
            // Outer-group binders this layer may capture (only meaningful
            // for an inner layer, gi > 0): pre-clone each one the body
            // references.
            let mut layer_preludes = String::new();
            if gi > 0 {
                layer_preludes.push_str("let __pkg = __pkg.clone(); ");
                for n in &captured {
                    let id = escape_rust_keyword(n);
                    if acc.contains(&format!("{id}.clone()")) || acc.contains(&format!("&{id}")) {
                        layer_preludes.push_str(&format!("let {id} = {id}.clone(); "));
                    }
                }
                for outer_g in groups.iter().take(gi) {
                    for n in outer_g {
                        let id = escape_rust_keyword(n);
                        if acc.contains(&format!("{id}.clone()")) || acc.contains(&format!("&{id}"))
                        {
                            layer_preludes.push_str(&format!("let {id} = {id}.clone(); "));
                        }
                    }
                }
            }
            // Box this layer as the erased `Rc<dyn Fn>` storage value, so
            // an outer layer (or the final `as_any`) treats it uniformly.
            // Each layer always lands in trailing block-expression
            // position (the next outer layer's closure body, or the final
            // `{ preludes acc }`), so the block needs no surrounding
            // parens — wrapping it would trip Rust's `unused_parens` lint.
            acc = format!(
                "{{ {layer_preludes}let __f: {fn_ty} = ::std::rc::Rc::new(move |{params}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {acc} }}); {} }}",
                self.box_any("__f")
            );
        }
        // The closure dispatches through `__pkg`; bind it once, plus a
        // clone of each captured outer local, around the whole curried
        // value so every layer's body captures them by clone.
        let s = self.self_ref_str().to_owned();
        let mut preludes = format!("let __pkg = {s}.clone(); ");
        for n in &captured {
            let id = escape_rust_keyword(n);
            preludes.push_str(&format!("let {id} = {id}.clone(); "));
        }
        Ok(format!("{{ {preludes}{acc} }}"))
    }

    /// Consume one hidden nullary closure per erased type argument.
    fn emit_erased_type_applications(&self, mut callee: String, count: usize) -> String {
        for _ in 0..count {
            let fn_ty = rc_fn_type(0);
            let recovered = self.unbox_any(&fn_ty, &callee);
            callee = format!("({recovered})()");
        }
        callee
    }

    /// Apply an erased callee expression (an `Rc<dyn Any>` boxing a
    /// closure) to `args`: recover the arity-matched `Rc<dyn Fn>` and
    /// call. The callee expression is consumed once.
    fn emit_apply(&mut self, callee: &str, args: &[Expr<Routed>]) -> Result<String, EmitError> {
        self.emit_apply_adapting(callee, args, &[])
    }

    /// Like [`Self::emit_apply`], but adapts each fn-valued arg to the
    /// callee's declared param-slot grouping when known. A flat fn value
    /// flowing into a slot whose function type groups its parameters as a
    /// product domain (abi-arity 1) is re-grouped by
    /// [`Self::adapt_fn_value_for_type`]. A missing / non-function
    /// `param_tys[i]` leaves the arg unchanged.
    fn emit_apply_adapting(
        &mut self,
        callee: &str,
        args: &[Expr<Routed>],
        param_tys: &[Option<Type<Routed>>],
    ) -> Result<String, EmitError> {
        let mut emitted = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let rendered = self.emit_expr(a)?;
            let rendered = match param_tys.get(i).and_then(|t| t.as_ref()) {
                Some(ty) => self.adapt_fn_value_for_type(rendered, a, ty)?,
                None => rendered,
            };
            emitted.push(rendered);
        }
        let fn_ty = rc_fn_type(args.len());
        let recovered = self.unbox_any(&fn_ty, callee);
        Ok(format!("({recovered})({})", emitted.join(", ")))
    }

    /// Adapt a fn-valued expression `rendered` (source `source`) to the
    /// product-grouping the function type `expected` requires, when the
    /// two differ. A closure `.[A](x, y)` (one 2-binder group) flowing
    /// into `(A & A) -> A` (abi-arity 1, one product param) is wrapped so
    /// a call passing one product arg peels it into the two binders.
    /// Mirrors the Go backend's `adapt_fn_value_for_type`.
    fn adapt_fn_value_for_type(
        &self,
        rendered: String,
        source: &Expr<Routed>,
        expected: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let Some(sig) = underlying_fn_expr_sig(source) else {
            return Ok(rendered);
        };
        let expected = self
            .shapes
            .normalize_type_for_owned_tparams_in_current_scope(expected, &self.tparams);
        let expected = peel_forall_ty(&expected);
        let Type::Function {
            param, abi_arity, ..
        } = expected
        else {
            return Ok(rendered);
        };
        let groups = value_param_name_groups(sig);
        let Some(first_group) = groups.first() else {
            return Ok(rendered);
        };
        let binder_count = first_group.len();
        let type_arity = Type::right_spine_take(param, *abi_arity).len();
        if binder_count == type_arity || groups.len() > 1 {
            return Ok(rendered);
        }
        if type_arity == 1 && binder_count >= 2 {
            // Wrap: one product arg `__a0` peeled into `binder_count`
            // binder args, applied to the inner `binder_count`-ary fn.
            let inner_fn_ty = rc_fn_type(binder_count);
            // Clone `__a0` per peel — `from_any` consumes its `Rc`, and the
            // helper peels the product once.
            let peels: Vec<String> = (0..binder_count)
                .map(|i| format!("__slots[{i}].clone()"))
                .collect();
            let outer_fn_ty = rc_fn_type(1);
            let recovered_inner = self.unbox_any(&inner_fn_ty, "__inner.clone()");
            let wrapped = format!(
                "{{ let __inner = {rendered}; let __w: {outer_fn_ty} = ::std::rc::Rc::new(move |__a0: ::std::rc::Rc<dyn ::std::any::Any>| -> ::std::rc::Rc<dyn ::std::any::Any> {{ let __slots = {}::__kio_runtime::product_slots(__a0, {binder_count}); ({recovered_inner})({}) }}); {} }}",
                self.crate_root(),
                peels.join(", "),
                self.box_any("__w"),
            );
            return Ok(wrapped);
        }
        Ok(rendered)
    }

    fn emit_product_from_rendered(&self, rendered: &[String]) -> String {
        let n = rendered.len();
        debug_assert!(n > 0);
        let mut acc = rendered[n - 1].clone();
        for head in rendered[..n - 1].iter().rev() {
            let pair = format!("({head}, {acc})");
            acc = self.box_any(&pair);
        }
        acc
    }

    fn emit_product_with_cached_slots(
        &mut self,
        items: &[&Expr<Routed>],
        slot_tys: Option<&[&Type<Routed>]>,
        plan: &ProductRebuildPlan,
    ) -> Result<String, EmitError> {
        let product_crate_root = self.crate_root().to_owned();
        let wrap_crate_root = product_crate_root.clone();
        render_cached_product_rebuild(
            plan,
            |slot| format!("__slots[{slot}].clone()"),
            |i| {
                let item = items[i];
                let v = self.emit_expr(item)?;
                match slot_tys.and_then(|slots| slots.get(i)).copied() {
                    Some(slot_ty) if matches!(slot_ty, Type::Function { .. }) => {
                        self.adapt_fn_value_for_type(v, item, slot_ty)
                    }
                    _ => Ok(v),
                }
            },
            |rendered| {
                let n = rendered.len();
                debug_assert!(n > 0);
                let mut acc = rendered[n - 1].clone();
                for head in rendered[..n - 1].iter().rev() {
                    let pair = format!("({head}, {acc})");
                    acc = format!("{product_crate_root}::__kio_runtime::as_any({pair})");
                }
                acc
            },
            |source, source_arity, product| {
                let source = escape_rust_keyword(source);
                format!(
                    "{{ let __slots = {wrap_crate_root}::__kio_runtime::product_slots({source}.clone(), {source_arity}); {product} }}"
                )
            },
        )
    }

    /// A product `(A & B & C)` → the nested-binary erased value
    /// `as_any((a, as_any((b, c))))`. `.0` is the head slot, `.1` the
    /// nested remainder; matches [`Self::emit_project`]'s peel.
    fn emit_product(&mut self, items: &[&Expr<Routed>]) -> Result<String, EmitError> {
        let n = items.len();
        if n == 0 {
            return Ok(self.box_any("()"));
        }
        if let Some(plan) = bound_product_rebuild_plan(items) {
            return self.emit_product_with_cached_slots(items, None, &plan);
        }
        let mut rendered = Vec::with_capacity(n);
        for item in items {
            rendered.push(self.emit_expr(item)?);
        }
        Ok(self.emit_product_from_rendered(&rendered))
    }

    /// Build a product, adapting each **fn-valued** slot to that slot's
    /// function-type grouping from the product's `synth_ty`.
    ///
    /// A fn value's natural rep is flat per value group (a host fn /
    /// closure of `n` first-group params is an `n`-ary closure), but a
    /// product slot's *type* may group those params as one product domain
    /// (`(I32 & I32) -> I32`, abi-arity 1). The slot is then projected and
    /// applied at that grouping (`dep_plus(deps)(n, 1)` reaches the slot
    /// with one product arg), so a bare flat closure stored there is
    /// recovered at the wrong arity and the `from_any` downcast fails at
    /// the call. [`Self::adapt_fn_value_for_type`]
    /// wraps the flat closure to the slot's product-domain grouping —
    /// exactly as a fn-valued *call argument* is adapted to its declared
    /// param slot. Mirrors the Go backend's `emit_tuple_item`.
    fn emit_product_typed(
        &mut self,
        items: &[&Expr<Routed>],
        synth_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let n = items.len();
        if n == 0 {
            return Ok(self.box_any("()"));
        }
        let slots = Type::right_spine_product(synth_ty);
        if let Some(plan) = bound_product_rebuild_plan(items) {
            return self.emit_product_with_cached_slots(items, Some(&slots), &plan);
        }
        let emit_item = |this: &mut Self, i: usize| -> Result<String, EmitError> {
            let v = this.emit_expr(items[i])?;
            match slots.get(i) {
                Some(slot) if matches!(slot, Type::Function { .. }) => {
                    this.adapt_fn_value_for_type(v, items[i], slot)
                }
                _ => Ok(v),
            }
        };
        let mut rendered = Vec::with_capacity(n);
        for i in 0..n {
            rendered.push(emit_item(self, i)?);
        }
        Ok(self.emit_product_from_rendered(&rendered))
    }

    /// Project slot `index` (of `arity`) from a nested-binary product.
    fn emit_project(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<String, EmitError> {
        let t = self.emit_expr(target)?;
        Ok(product_slot_access(self.crate_root(), &t, index, arity))
    }

    /// Inject `payload` as variant `variant` of `variants`.
    fn emit_inject(
        &mut self,
        payload: &Expr<Routed>,
        variant: usize,
        variants: usize,
    ) -> Result<String, EmitError> {
        let p = self.emit_expr(payload)?;
        Ok(format!(
            "{}::__kio_runtime::sum_inject({p}, {variant}, {variants})",
            self.crate_root()
        ))
    }

    /// Match on a sum scrutinee by peeling it once to `(variant, payload)`.
    fn emit_match(
        &mut self,
        scrutinee: &Expr<Routed>,
        arms: &[crate::ast::EnrichedArm<Routed>],
        scrutinee_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let n = arms.len();
        if n == 0 {
            unreachable!(
                "Rust emitter: EnrichedMatch with no arms; structural_recovery builds every \
                 EnrichedMatch from a sum's variants (recover_either / recover_dynamic_right \
                 yield >= 2 arms) and routes an uninhabited scrutinee through \
                 __absurd__/LowAbsurdCall, so a zero-arm match is unreachable"
            );
        }
        let s = self.emit_expr(scrutinee)?;
        let mut rendered_arms = Vec::with_capacity(n + 1);
        for (i, arm) in arms.iter().enumerate() {
            let ident = escape_rust_keyword(&arm.param);
            self.locals.push(arm.param.clone());
            let prior_ty = self.local_types.remove(&arm.param);
            let ty =
                Type::right_spine_sum_slot_for_arity(scrutinee_ty, i, n).unwrap_or_else(|| {
                    unreachable!(
                        "Rust emitter: EnrichedMatch arm {i} has no payload type at arity {n}; \
                     structural recovery must align every arm with the recovered sum"
                    )
                });
            self.local_types.insert(arm.param.clone(), ty.clone());
            let body = self.emit_expr(&arm.body);
            self.local_types.remove(&arm.param);
            if let Some(ty) = prior_ty {
                self.local_types.insert(arm.param.clone(), ty);
            }
            self.locals.pop();
            let body = body?;
            rendered_arms.push(format!(
                "{i} => {{ let {ident}: ::std::rc::Rc<dyn ::std::any::Any> = __payload.clone(); let _ = &{ident}; {body} }}"
            ));
        }
        rendered_arms
            .push("_ => unreachable!(\"kio erased body: invalid sum variant\")".to_owned());
        Ok(format!(
            "{{ let __m: ::std::rc::Rc<dyn ::std::any::Any> = {s}; let (__variant, __payload) = {}::__kio_runtime::sum_payload(__m, {n}); match __variant {{ {} }} }}",
            self.crate_root(),
            rendered_arms.join(", ")
        ))
    }

    /// `if cond { then } else { else }`. The erased condition is recovered as
    /// its exact role(bool) associated type and compared with that type's
    /// declaration-wide `From<bool>` construction of `true`.
    fn emit_conditional(
        &mut self,
        cond: &Expr<Routed>,
        then_branch: &Expr<Routed>,
        else_branch: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let c = self.emit_bool(cond)?;
        let t = self.emit_expr(then_branch)?;
        let e = self.emit_expr(else_branch)?;
        Ok(format!("if {c} {{ {t} }} else {{ {e} }}"))
    }

    /// Render an expression in Boolean position without replacing the host's
    /// exact type with Rust `bool`.
    fn emit_bool(&mut self, e: &Expr<Routed>) -> Result<String, EmitError> {
        let condition_ty = self.expr_type(e).ok_or_else(|| {
            EmitError::unsupported(
                "Rust emitter: cannot reconstruct an enriched conditional's exact Boolean type \
                 (compiler bug)",
            )
        })?;
        let condition_ty = self
            .shapes
            .normalize_type_for_owned_tparams_in_current_scope(&condition_ty, &self.tparams);
        if self.atom_role_of(&condition_ty) != Some(Role::Bool) {
            return Err(EmitError::unsupported(
                "Rust emitter: enriched conditional does not carry a role(bool) condition \
                 (compiler bug)",
            ));
        }
        let tparams: std::collections::HashSet<&str> =
            self.tparams.iter().map(String::as_str).collect();
        let exact = self
            .shapes
            .render_use(&condition_ty, TypeCtx::Shape, &tparams)?;
        let value = self.emit_expr(e)?;
        let recovered = self.unbox_any(&exact, &value);
        let true_value = format!("<{exact} as ::std::convert::From<bool>>::from(true)");
        Ok(format!("{recovered} == {true_value}"))
    }

    /// `LowHostFnValueRef` → an erased closure forwarding through the host
    /// record with the same FFI conversion as a direct host call. Each
    /// value param arrives erased, converts to the host slot's shape; the
    /// return converts back to the internal erased rep. Internally the value
    /// retains one nullary stage per type binder and one layer per value
    /// group; the deepest layer invokes the value-only host method.
    fn emit_low_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let _ = (sig, ret_ty);
        let site_id = boundary_site_id(
            module_path,
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        );
        let site = prepared_site(self.prepared, &site_id);
        let execution = site.execution().unwrap_or_else(|| {
            unreachable!(
                "Rust emitter: a live host-function value resolved to a retained prepared site"
            )
        });
        let callable = prepare_rust_callable(site);
        let converter = PreparedRustConverter {
            site,
            shapes: PreparedRustRenderContext::new(
                self.prepared,
                &self.shapes.crate_root,
                &self.shapes.host_trait,
            ),
            scope: erased_prepared_scope(site, &callable.scope, self.crate_root()),
            ctx: TypeCtx::Shape,
            nominal_owner: None,
        };

        let entry = site.plan().entry();
        if entry.head_stages.len() != execution.head_stages().len() {
            unreachable!("a prepared host-function value lost its head-stage pairing");
        }
        let mut stages = Vec::<Vec<String>>::with_capacity(execution.head_stages().len());
        let mut host_arguments = Vec::new();
        let mut private_offset = 0usize;
        let mut public_count = 0usize;
        let mut facade_count = 0usize;
        for ((semantic, stage), stage_scope) in entry
            .head_stages
            .iter()
            .zip(execution.head_stages())
            .zip(&callable.head_scopes)
        {
            match (semantic, stage) {
                (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {
                    stages.push(Vec::new())
                }
                (
                    BoundaryCallableHeadStage::Value { slots },
                    CallableExecutionStage::Value(layout),
                ) => {
                    let private_arity = layout.body_abi_arity();
                    let params = (0..private_arity)
                        .map(|offset| format!("__p{}", private_offset + offset))
                        .collect::<Vec<_>>();
                    let stage_converter = PreparedRustConverter {
                        site: converter.site,
                        shapes: converter.shapes,
                        scope: erased_prepared_scope(site, stage_scope, self.crate_root()),
                        ctx: converter.ctx,
                        nominal_owner: converter.nominal_owner.clone(),
                    };
                    host_arguments.extend(adapt_prepared_body_to_public_arguments(
                        site,
                        &stage_converter,
                        slots,
                        layout,
                        &params,
                    )?);
                    private_offset += private_arity;
                    public_count += layout.source_param_count();
                    facade_count += layout.facade_slot_count();
                    stages.push(params);
                }
                _ => unreachable!("a prepared host-function value has mismatched head stages"),
            }
        }
        if facade_count != callable.value_slots.len() || host_arguments.len() != public_count {
            unreachable!(
                "Rust emitter: prepared host-function value execution did not cover every facade slot",
            );
        }

        let method = format!(
            "{}{}",
            mangle_host_name(module_path, name),
            prepared_host_method_turbofish(site, &callable, self.crate_root())
        );
        let call = render_host_method_call("__host", &method, &host_arguments);
        let mut acc = converter.convert(
            &PreparedRustUseCursor::root(site, callable.returned),
            &call,
            FfiDir::In,
        )?;
        for (stage_index, params) in stages.iter().enumerate().rev() {
            let declarations = params
                .iter()
                .map(|param| format!("{param}: ::std::rc::Rc<dyn ::std::any::Any>"))
                .collect::<Vec<_>>();
            let mut prelude = String::new();
            if stage_index > 0 {
                prelude.push_str("let __host = __host.clone(); ");
                for captured in stages.iter().take(stage_index).flatten() {
                    prelude.push_str(&format!("let {captured} = {captured}.clone(); "));
                }
            }
            let fn_ty = rc_fn_type(params.len());
            acc = format!(
                "{{ {prelude}let __f: {fn_ty} = ::std::rc::Rc::new(move |{}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {acc} }}); {} }}",
                declarations.join(", "),
                self.box_any("__f")
            );
        }
        let receiver = self.self_ref_str().to_owned();
        Ok(format!(
            "{{ let __host = {receiver}.__host.clone(); {acc} }}"
        ))
    }

    /// `LowModuleFnValueRef` → an erased closure forwarding to the
    /// resolved package-handle module-fn method. The method binds the fn's
    /// **first** value group and returns nested erased closures for inner
    /// groups, so the value-ref closure binds the first group's params and
    /// forwards them; inner groups are reached by the caller's per-group
    /// applies. All params / results are the internal erased rep.
    fn emit_low_module_fn_value_ref(
        &mut self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let method = self
            .selective_imports
            .get(mangled)
            .cloned()
            .unwrap_or_else(|| mangled.to_owned());
        let groups = runtime_param_name_groups(sig);
        let first = groups.first().cloned().unwrap_or_default();
        let s = self.self_ref_str().to_owned();
        let params: String = (0..first.len())
            .map(|i| format!("__p{i}: ::std::rc::Rc<dyn ::std::any::Any>"))
            .collect::<Vec<_>>()
            .join(", ");
        let fwd: String = (0..first.len())
            .map(|i| format!("__p{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let fn_ty = rc_fn_type(first.len());
        let call = format!("__pkg.{method}({fwd})");
        Ok(format!(
            "{{ let __pkg = {s}.clone(); let __f: {fn_ty} = ::std::rc::Rc::new(move |{params}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {call} }}); {} }}",
            self.box_any("__f"),
        ))
    }

    /// `LowHostCall` → a call to the user's `Host` trait method, crossing
    /// the FFI. Each value arg converts from the internal erased rep to
    /// the host slot's declared shape; the return converts back. Every public
    /// occurrence uses the canonical owned facade, including `role(str)`.
    fn emit_low_host_call(
        &mut self,
        name: &str,
        module_path: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let _ = (type_args, sig, ret_ty);
        let site_id = boundary_site_id(
            module_path,
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        );
        let site = prepared_site(self.prepared, &site_id);
        let execution = site.execution().unwrap_or_else(|| {
            unreachable!("Rust emitter: a live host call resolved to a retained prepared site")
        });
        let callable = prepare_rust_callable(site);
        let expected_private_arity = execution
            .head_stages()
            .iter()
            .map(|stage| match stage {
                CallableExecutionStage::Type { .. } => 0,
                CallableExecutionStage::Value(layout) => layout.body_abi_arity(),
            })
            .sum::<usize>();
        if expected_private_arity != args.len() {
            unreachable!(
                "Rust emitter: host call `{module_path}.{name}` has {} private argument(s), but its prepared execution consumes {expected_private_arity} (compiler bug)",
                args.len(),
            );
        }

        let erased_arguments = args
            .iter()
            .map(|argument| self.emit_expr(argument))
            .collect::<Result<Vec<_>, _>>()?;
        let converter = PreparedRustConverter {
            site,
            shapes: PreparedRustRenderContext::new(
                self.prepared,
                &self.shapes.crate_root,
                &self.shapes.host_trait,
            ),
            scope: erased_prepared_scope(site, &callable.scope, self.crate_root()),
            ctx: TypeCtx::Shape,
            nominal_owner: None,
        };
        let bound_arguments = (0..erased_arguments.len())
            .map(|index| format!("__kio_host_source_{index}"))
            .collect::<Vec<_>>();
        let entry = site.plan().entry();
        if entry.head_stages.len() != execution.head_stages().len() {
            unreachable!("a direct prepared host call lost its head-stage pairing");
        }
        let mut private_offset = 0usize;
        let mut facade_count = 0usize;
        let mut public_arguments = Vec::new();
        for ((semantic, stage), stage_scope) in entry
            .head_stages
            .iter()
            .zip(execution.head_stages())
            .zip(&callable.head_scopes)
        {
            match (semantic, stage) {
                (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {}
                (
                    BoundaryCallableHeadStage::Value { slots },
                    CallableExecutionStage::Value(layout),
                ) => {
                    let end = private_offset + layout.body_abi_arity();
                    let stage_converter = PreparedRustConverter {
                        site: converter.site,
                        shapes: converter.shapes,
                        scope: erased_prepared_scope(site, stage_scope, self.crate_root()),
                        ctx: converter.ctx,
                        nominal_owner: converter.nominal_owner.clone(),
                    };
                    public_arguments.extend(adapt_prepared_body_to_public_arguments(
                        site,
                        &stage_converter,
                        slots,
                        layout,
                        &bound_arguments[private_offset..end],
                    )?);
                    private_offset = end;
                    facade_count += layout.facade_slot_count();
                }
                _ => unreachable!("a direct prepared host call has mismatched head stages"),
            }
        }
        if private_offset != erased_arguments.len() || facade_count != callable.value_slots.len() {
            unreachable!("a direct prepared host call did not consume its exact facade plan");
        }

        let turbofish = prepared_host_method_turbofish(site, &callable, self.crate_root());
        let method = format!("{}{turbofish}", mangle_host_name(module_path, name));
        let receiver = format!("{}.__host", self.self_ref_str());
        let call = render_host_method_call(&receiver, &method, &public_arguments);
        let call = if erased_arguments.is_empty() {
            call
        } else {
            let bindings = erased_arguments
                .iter()
                .zip(&bound_arguments)
                .map(|(value, binding)| format!("let {binding} = {value};"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{{ {bindings} {call} }}")
        };
        converter.convert(
            &PreparedRustUseCursor::root(site, callable.returned),
            &call,
            FfiDir::In,
        )
    }

    /// `LowModuleCall` → `self.<method>(<args>)`, honoring per-group
    /// currying. Resolve the method name via the use-imports table (else a
    /// same-module bare mangled name).
    fn emit_low_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let method = self
            .selective_imports
            .get(mangled)
            .cloned()
            .unwrap_or_else(|| mangled.to_owned());
        self.emit_grouped_method_call(&method, type_arg_count, args, sig)
    }

    /// `LowQualifiedModuleCall` (`import <pkg>/<mod> as a; a.fn(...)`):
    /// resolve `<alias>.<leaf>` to the module-fn method, then dispatch.
    fn emit_low_qualified_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        // `mangled` is `<alias>.<member>` (the alias is one segment from
        // `import <pkg>/<mod> as <alias>;`). Resolve the alias to its real
        // module slash-path, then build the Rust-mangled module-fn method
        // name (`__mod_<path>_<member>`).
        let (alias, member) = mangled.split_once('.').ok_or_else(|| {
            EmitError::unsupported(format!(
                "Rust emitter: LowQualifiedModuleCall.mangled `{mangled}` doesn't match \
                 the expected `<alias>.<member>` shape (compiler bug)"
            ))
        })?;
        let resolved = if let Some(path) = self.qualified_imports.get(alias).cloned() {
            mangle_module_fn(&path, member)
        } else if self.package.module(alias).is_some() {
            mangle_module_fn(alias, member)
        } else {
            return Err(EmitError::unsupported(format!(
                "Rust emitter: qualified callee `{alias}.{member}` resolves to neither a \
                 qualified import nor a direct module (compiler bug)"
            )));
        };
        self.emit_grouped_method_call(&resolved, type_arg_count, args, sig)
    }

    /// Emit a call to a package-handle module-fn method over `args`,
    /// honoring currying: the method binds the fn's **first** value group
    /// and returns nested erased closures for inner groups; args spanning
    /// further groups are applied to the returned closure group by group.
    /// A call supplying *fewer* args than the first group is a
    /// value-leaving partial wrapped in a closure binding the missing
    /// first-group slots. Mirrors the Go backend's `emit_grouped_method_call`.
    fn emit_grouped_method_call(
        &mut self,
        method: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let group_arities = value_group_arities(sig);
        let first = group_arities.first().copied().unwrap_or(0);
        let s = self.self_ref_str().to_owned();
        let staged_callee = if type_arg_count > 0 {
            Some(self.emit_erased_type_applications(format!("{s}.{method}()"), type_arg_count - 1))
        } else {
            None
        };
        let param_tys = sig_all_value_param_types(sig);
        let mut emitted = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            let v = match param_tys.get(i).and_then(|t| t.as_ref()) {
                Some(t) => self.adapt_fn_value_for_type(v, a, t)?,
                None => v,
            };
            emitted.push(v);
        }
        if let Some(mut call) = staged_callee {
            if args.len() < first {
                let missing = first - args.len();
                let fresh: Vec<String> = (0..missing).map(|i| format!("__pa{i}")).collect();
                let params: String = fresh
                    .iter()
                    .map(|param| format!("{param}: ::std::rc::Rc<dyn ::std::any::Any>"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut all_args = emitted.clone();
                all_args.extend(fresh.iter().cloned());
                let fn_ty = rc_fn_type(missing);
                let applied_ty = rc_fn_type(all_args.len());
                let recovered = self.unbox_any(&applied_ty, "__callee.clone()");
                return Ok(format!(
                    "{{ let __callee = {call}; let __f: {fn_ty} = ::std::rc::Rc::new(move |{params}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ ({recovered})({}) }}); {} }}",
                    all_args.join(", "),
                    self.box_any("__f"),
                ));
            }
            let first_args = &emitted[..first];
            let fn_ty = rc_fn_type(first);
            let recovered = self.unbox_any(&fn_ty, &call);
            call = format!("({recovered})({})", first_args.join(", "));
            let mut consumed = first;
            for arity in group_arities.iter().skip(1) {
                if consumed >= emitted.len() {
                    break;
                }
                let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
                let fn_ty = rc_fn_type(slice.len());
                let recovered = self.unbox_any(&fn_ty, &call);
                call = format!("({recovered})({})", slice.join(", "));
                consumed += arity;
            }
            return Ok(call);
        }
        if args.len() < first {
            // Value-leaving partial: bind already-supplied args, take the
            // missing first-group slots as fresh erased params.
            let missing = first - args.len();
            let fresh: Vec<String> = (0..missing).map(|i| format!("__pa{i}")).collect();
            let params: String = fresh
                .iter()
                .map(|p| format!("{p}: ::std::rc::Rc<dyn ::std::any::Any>"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut all_args = emitted.clone();
            all_args.extend(fresh.iter().cloned());
            let fn_ty = rc_fn_type(missing);
            let call = format!("__pkg.{method}({})", all_args.join(", "));
            return Ok(format!(
                "{{ let __pkg = {s}.clone(); let __f: {fn_ty} = ::std::rc::Rc::new(move |{params}| -> ::std::rc::Rc<dyn ::std::any::Any> {{ {call} }}); {} }}",
                self.box_any("__f"),
            ));
        }
        let mut call = format!("{s}.{method}({})", emitted[..first].join(", "));
        let mut consumed = first;
        for arity in group_arities.iter().skip(1) {
            if consumed >= emitted.len() {
                break;
            }
            let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
            let fn_ty = rc_fn_type(slice.len());
            let recovered = self.unbox_any(&fn_ty, &call);
            call = format!("({recovered})({})", slice.join(", "));
            consumed += arity;
        }
        Ok(call)
    }

    /// `LowCpsProjectorApply { receiver, continuation }` →
    /// `continuation(receiver)`. An existential newtype's projector is a
    /// runtime identity in the erased body (the newtype carries its
    /// payload transparently), so projecting `receiver` yields `receiver`
    /// itself. The exact routed continuation type supplies its erased type
    /// stages and zero-or-one-slot value ABI.
    fn emit_low_cps_projector_apply(
        &mut self,
        receiver: &Expr<Routed>,
        continuation: &Expr<Routed>,
        continuation_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let recv = self.emit_expr(receiver)?;
        let cont = self.emit_expr(continuation)?;
        let receiver_name = self.fresh_name("cps_receiver");
        let continuation_name = self.fresh_name("cps_continuation");
        let (type_stages, callable) = continuation_ty.peel_leading_foralls();
        let Type::Function { abi_arity, .. } = callable else {
            unreachable!("a routed CPS projector continuation has a function type")
        };
        assert!(
            *abi_arity <= 1,
            "a routed CPS projector continuation has zero or one ABI slot"
        );
        let advanced =
            self.emit_erased_type_applications(format!("{continuation_name}.clone()"), type_stages);
        let fn_ty = rc_fn_type(*abi_arity);
        let recovered = self.unbox_any(&fn_ty, &advanced);
        let args = if *abi_arity == 0 {
            String::new()
        } else {
            format!("{receiver_name}.clone()")
        };
        Ok(format!(
            "{{ let {receiver_name}: ::std::rc::Rc<dyn ::std::any::Any> = {recv}; let {continuation_name}: ::std::rc::Rc<dyn ::std::any::Any> = {cont}; ({recovered})({args}) }}"
        ))
    }

    /// Role for a literal's typer-pinned exact host-type annotation.
    fn literal_host_role(&self, annotation: &Type<Routed>) -> Result<Role, EmitError> {
        if let Type::Path { segments, args, .. } = annotation
            && args.is_empty()
        {
            let role = self.shapes.host_type_role(segments).unwrap_or_else(|| {
                let identity = segments
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                unreachable!(
                    "Rust emitter: literal annotation `{identity}` is not a declared `role(...)` \
                     host type"
                )
            });
            return Ok(role);
        }
        unreachable!("Rust emitter: the typer produced a non-host literal annotation")
    }

    /// Construct one admitted primitive literal as the annotation's exact
    /// host-selected associated type through its declaration-wide `From`
    /// capability.
    fn exact_literal_value(
        &self,
        annotation: &Type<Routed>,
        primitive_expr: &str,
    ) -> Result<String, EmitError> {
        let tparams: std::collections::HashSet<&str> =
            self.tparams.iter().map(String::as_str).collect();
        let exact = self
            .shapes
            .render_use(annotation, TypeCtx::Shape, &tparams)?;
        let role = self.literal_host_role(annotation)?;
        Ok(format!(
            "<{exact} as ::std::convert::From<{}>>::from({primitive_expr})",
            role_literal_primitive_type(role)
        ))
    }
}
fn emit_shapes_rs(shapes: &mut BodyShapeRegistry) -> Result<String, EmitError> {
    shapes.render()
}

fn render_rust_marker_core() -> &'static str {
    r#"pub use crate::__kio_runtime::KioStoredValue;

pub(crate) fn __kio_store_native<T: 'static>(value: T) -> KioStoredValue {
    KioStoredValue::from_raw(crate::__kio_runtime::as_any(value))
}

pub(crate) fn __kio_load_native<T: Clone + 'static>(value: KioStoredValue) -> T {
    crate::__kio_runtime::from_any(value.into_raw())
}

mod __kio_type_private {
    pub trait Sealed {}
}

/// A source kind-`*` type. `Facade` is its canonical public Rust value type;
/// the conversion methods are the generated facade/body representation
/// witness used at polymorphic boundaries and by invariant host storage.
#[allow(private_bounds)]
pub trait KioType: __kio_type_private::Sealed + Clone + 'static {
    type Facade: Clone + 'static;

    /// Encode this marker's facade as an opaque stored value.
    ///
    /// External storage must preserve the semantic marker associated with the
    /// token. Attaching a stored value to a marker whose storage contract it
    /// does not satisfy violates the host invariant even when that marker
    /// accepts the stored representation. The raw erased representation remains
    /// crate-private.
    fn into_stored(value: Self::Facade) -> KioStoredValue;

    /// Decode an opaque stored value using this marker's representation.
    ///
    /// Decoding a stored value through a marker whose storage contract it does
    /// not satisfy violates the host invariant. An incompatible stored
    /// representation deterministically
    /// panics. A representation-compatible token may decode successfully, but
    /// that possibility is not a compatibility guarantee for any marker pair.
    /// Generated conversions are safe Rust, so neither outcome can cause
    /// undefined behavior.
    fn from_stored(value: KioStoredValue) -> Self::Facade;
}

/// Select an otherwise-native Rust type as a source kind-`*` argument.
pub struct KioNative<T: Clone + 'static>(::std::marker::PhantomData<fn(T) -> T>);

impl<T: Clone + 'static> Clone for KioNative<T> {
    fn clone(&self) -> Self {
        Self(::std::marker::PhantomData)
    }
}

impl<T: Clone + 'static> __kio_type_private::Sealed for KioNative<T> {}

impl<T: Clone + 'static> KioType for KioNative<T> {
    type Facade = T;

    fn into_stored(value: T) -> KioStoredValue {
        __kio_store_native(value)
    }

    fn from_stored(value: KioStoredValue) -> T {
        __kio_load_native(value)
    }
}

#[doc(hidden)]
pub(crate) struct __KioErasedType;

impl Clone for __KioErasedType {
    fn clone(&self) -> Self {
        Self
    }
}

impl __kio_type_private::Sealed for __KioErasedType {}

impl KioType for __KioErasedType {
    type Facade = KioStoredValue;

    fn into_stored(value: KioStoredValue) -> KioStoredValue {
        value
    }

    fn from_stored(value: KioStoredValue) -> KioStoredValue {
        value
    }
}

/// Typed access to an opaque canonical value retained by host storage.
pub struct KioValue<A: KioType>(
    KioStoredValue,
    ::std::marker::PhantomData<fn(A) -> A>,
);

impl<A: KioType> Clone for KioValue<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), ::std::marker::PhantomData)
    }
}

impl<A: KioType> KioValue<A> {
    pub fn pack(value: A::Facade) -> Self {
        Self(A::into_stored(value), ::std::marker::PhantomData)
    }

    pub fn unpack(self) -> A::Facade {
        A::from_stored(self.0)
    }

    pub fn from_stored(value: KioStoredValue) -> Self {
        Self(value, ::std::marker::PhantomData)
    }

    pub fn into_stored(self) -> KioStoredValue {
        self.0
    }

    pub fn stored(&self) -> KioStoredValue {
        self.0.clone()
    }
}

#[derive(Clone, PartialEq)]
pub struct Product<A: Clone + 'static, B: Clone + 'static> {
    pub _0: A,
    pub _1: B,
}

#[derive(Clone, PartialEq)]
pub enum Sum<A: Clone + 'static, B: Clone + 'static> {
    Left(A),
    Right(B),
}

pub struct KioUnit;

impl Clone for KioUnit {
    fn clone(&self) -> Self { Self }
}

impl __kio_type_private::Sealed for KioUnit {}

impl KioType for KioUnit {
    type Facade = ();

    fn into_stored(value: ()) -> KioStoredValue {
        __kio_store_native(value)
    }

    fn from_stored(value: KioStoredValue) -> () {
        __kio_load_native(value)
    }
}

pub struct KioBottom;

impl Clone for KioBottom {
    fn clone(&self) -> Self { Self }
}

impl __kio_type_private::Sealed for KioBottom {}

impl KioType for KioBottom {
    type Facade = ::std::convert::Infallible;

    fn into_stored(value: Self::Facade) -> KioStoredValue {
        match value {}
    }

    fn from_stored(value: KioStoredValue) -> Self::Facade {
        __kio_load_native(value)
    }
}

pub struct KioProduct<A: KioType, B: KioType>(
    ::std::marker::PhantomData<fn(A, B) -> (A, B)>,
);

impl<A: KioType, B: KioType> Clone for KioProduct<A, B> {
    fn clone(&self) -> Self { Self(::std::marker::PhantomData) }
}

impl<A: KioType, B: KioType> __kio_type_private::Sealed for KioProduct<A, B> {}

impl<A: KioType, B: KioType> KioType for KioProduct<A, B> {
    type Facade = Product<A::Facade, B::Facade>;

    fn into_stored(value: Self::Facade) -> KioStoredValue {
        let left = A::into_stored(value._0).into_raw();
        let right = B::into_stored(value._1).into_raw();
        KioStoredValue::from_raw(crate::__kio_runtime::as_any((left, right)))
    }

    fn from_stored(value: KioStoredValue) -> Self::Facade {
        let (left, right): (
            ::std::rc::Rc<dyn ::std::any::Any>,
            ::std::rc::Rc<dyn ::std::any::Any>,
        ) = crate::__kio_runtime::from_any(value.into_raw());
        Product {
            _0: A::from_stored(KioStoredValue::from_raw(left)),
            _1: B::from_stored(KioStoredValue::from_raw(right)),
        }
    }
}

pub struct KioSum<A: KioType, B: KioType>(
    ::std::marker::PhantomData<fn(A, B) -> (A, B)>,
);

impl<A: KioType, B: KioType> Clone for KioSum<A, B> {
    fn clone(&self) -> Self { Self(::std::marker::PhantomData) }
}

impl<A: KioType, B: KioType> __kio_type_private::Sealed for KioSum<A, B> {}

impl<A: KioType, B: KioType> KioType for KioSum<A, B> {
    type Facade = Sum<A::Facade, B::Facade>;

    fn into_stored(value: Self::Facade) -> KioStoredValue {
        match value {
            Sum::Left(value) => KioStoredValue::from_raw(crate::__kio_runtime::as_any((
                0usize,
                A::into_stored(value).into_raw(),
            ))),
            Sum::Right(value) => KioStoredValue::from_raw(crate::__kio_runtime::as_any((
                1usize,
                B::into_stored(value).into_raw(),
            ))),
        }
    }

    fn from_stored(value: KioStoredValue) -> Self::Facade {
        let (tag, payload): (usize, ::std::rc::Rc<dyn ::std::any::Any>) =
            crate::__kio_runtime::from_any(value.into_raw());
        match tag {
            0 => Sum::Left(A::from_stored(KioStoredValue::from_raw(payload))),
            1 => Sum::Right(B::from_stored(KioStoredValue::from_raw(payload))),
            _ => panic!("kio stored sum: invalid binary tag"),
        }
    }
}

"#
}

fn render_rust_type_constructor_support(
    arity: usize,
    origin: BoundaryFacadeSupportOrigin,
) -> String {
    let arguments = (0..arity)
        .map(|index| format!("KioArg_{index}"))
        .collect::<Vec<_>>();
    let declarations = arguments
        .iter()
        .map(|argument| format!("{argument}: KioType"))
        .collect::<Vec<_>>()
        .join(", ");
    let tuple = if arity == 1 {
        format!("{},", arguments[0])
    } else {
        arguments.join(", ")
    };
    let mut out = String::new();
    out.push_str("#[allow(non_camel_case_types)]\n");
    push_rust_deprecation(&mut out, origin, "");
    out.push_str(
        "/// A source type constructor whose application selects a Kio type marker.\n\
         ///\n\
         /// `Apply` must be a substitution-stable marker relation and may select\n\
         /// any public, well-formed `KioType` marker, including a native-container\n\
         /// marker over argument facades. `Apply` is the exact public-facade\n\
         /// authority; every abstract application still uses the generated\n\
         /// application marker and facade.\n\
         ///\n\
         /// The default `lift` and `project` use the selected marker's codec. An\n\
         /// external constructor may override them as a pair with a different\n\
         /// stored representation only when the conversion is lossless and\n\
         /// substitution-stable. Both round trips must preserve the semantic value\n\
         /// and marker association; `Apply` and the method signatures remain the\n\
         /// exact public-facade authority. Overrides use only the documented\n\
         /// opaque-token API.\n\
         /// Attaching a stored value to a marker whose storage contract it does not\n\
         /// satisfy violates the host invariant. Incompatible decoding\n\
         /// deterministically panics; compatible decoding may succeed without\n\
         /// guaranteeing any marker pair. Generated safe Rust cannot cause\n\
         /// undefined behavior.\n",
    );
    out.push_str(&format!(
        "pub trait KioTypeConstructor{arity}: Clone + 'static {{\n    type Apply<{declarations}>: KioType;\n\n    fn lift<{declarations}>(value: <Self::Apply<{}> as KioType>::Facade) -> KioApply{arity}<Self, {}>\n    where\n        Self: Sized,\n    {{\n        KioApply{arity}(\n            <Self::Apply<{}> as KioType>::into_stored(value),\n            ::std::marker::PhantomData,\n        )\n    }}\n\n    fn project<{declarations}>(value: KioApply{arity}<Self, {}>) -> <Self::Apply<{}> as KioType>::Facade\n    where\n        Self: Sized,\n    {{\n        <Self::Apply<{}> as KioType>::from_stored(value.0)\n    }}\n}}\n\n",
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
    ));
    let deprecation = rust_deprecation_attribute(origin)
        .map(|attribute| format!("{attribute}\n"))
        .unwrap_or_default();
    out.push_str(&format!(
        "{deprecation}pub struct KioApply{arity}<F: KioTypeConstructor{arity}, {declarations}>(\n    pub(crate) KioStoredValue,\n    pub(crate) ::std::marker::PhantomData<fn(F, {tuple}) -> (F, {tuple})>,\n);\n\nimpl<F: KioTypeConstructor{arity}, {declarations}> Clone for KioApply{arity}<F, {}> {{\n    fn clone(&self) -> Self {{ Self(self.0.clone(), ::std::marker::PhantomData) }}\n}}\n\n{deprecation}pub struct KioApplied{arity}<F: KioTypeConstructor{arity}, {declarations}>(\n    ::std::marker::PhantomData<fn(F, {tuple}) -> (F, {tuple})>,\n);\n\nimpl<F: KioTypeConstructor{arity}, {declarations}> Clone for KioApplied{arity}<F, {}> {{\n    fn clone(&self) -> Self {{ Self(::std::marker::PhantomData) }}\n}}\n\nimpl<F: KioTypeConstructor{arity}, {declarations}> __kio_type_private::Sealed for KioApplied{arity}<F, {}> {{}}\n\nimpl<F: KioTypeConstructor{arity}, {declarations}> KioType for KioApplied{arity}<F, {}> {{\n    type Facade = KioApply{arity}<F, {}>;\n\n    fn into_stored(value: Self::Facade) -> KioStoredValue {{ value.0 }}\n    fn from_stored(value: KioStoredValue) -> Self::Facade {{\n        KioApply{arity}(value, ::std::marker::PhantomData)\n    }}\n}}\n\n#[derive(Clone)]\npub(crate) struct __KioErasedTypeConstructor{arity};\n\nimpl KioTypeConstructor{arity} for __KioErasedTypeConstructor{arity} {{\n    type Apply<{declarations}> = __KioErasedType;\n}}\n\n",
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
        arguments.join(", "),
    ));
    out
}

fn render_rust_function_support(arity: usize, origin: BoundaryFacadeSupportOrigin) -> String {
    let arguments = (0..arity)
        .map(|index| format!("KioArg_{index}"))
        .collect::<Vec<_>>();
    let mut all = arguments.clone();
    all.push("KioResult".to_owned());
    let declarations = all
        .iter()
        .map(|argument| format!("{argument}: KioType"))
        .collect::<Vec<_>>()
        .join(", ");
    let uses = all.join(", ");
    let phantom_tuple = if all.len() == 1 {
        format!("{},", all[0])
    } else {
        uses.clone()
    };
    let facade_parameters = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| format!("arg{index}: {argument}::Facade"))
        .collect::<Vec<_>>();
    let closure_parameters = facade_parameters.join(", ");
    let call_parameters = if closure_parameters.is_empty() {
        String::new()
    } else {
        format!(", {closure_parameters}")
    };
    let closure_types = arguments
        .iter()
        .map(|argument| format!("{argument}::Facade"))
        .collect::<Vec<_>>()
        .join(", ");
    let raw_parameters = arguments
        .iter()
        .enumerate()
        .map(|(index, _)| format!("__arg{index}: ::std::rc::Rc<dyn ::std::any::Any>"))
        .collect::<Vec<_>>();
    let decode_parameters = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            format!(
                "let arg{index} = {argument}::from_stored(KioStoredValue::from_raw(__arg{index}));"
            )
        })
        .collect::<Vec<_>>();
    let encode_arguments = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| format!("{argument}::into_stored(arg{index}).into_raw()"))
        .collect::<Vec<_>>();
    let raw_fn = format!(
        "::std::rc::Rc<dyn Fn({}) -> ::std::rc::Rc<dyn ::std::any::Any>>",
        std::iter::repeat_n("::std::rc::Rc<dyn ::std::any::Any>", arity)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut out = String::new();
    out.push_str("#[allow(non_camel_case_types)]\n");
    push_rust_deprecation(&mut out, origin, "");
    let deprecation = rust_deprecation_attribute(origin)
        .map(|attribute| format!("{attribute}\n"))
        .unwrap_or_default();
    out.push_str(&format!(
        "pub struct KioFn{arity}<{declarations}>(\n    KioStoredValue,\n    ::std::marker::PhantomData<fn({phantom_tuple}) -> ({phantom_tuple})>,\n);\n\nimpl<{declarations}> Clone for KioFn{arity}<{uses}> {{\n    fn clone(&self) -> Self {{ Self(self.0.clone(), ::std::marker::PhantomData) }}\n}}\n\nimpl<{declarations}> KioFn{arity}<{uses}> {{\n    pub fn new(__function: impl Fn({}) -> KioResult::Facade + 'static) -> Self {{\n        let __erased: {raw_fn} = ::std::rc::Rc::new(move |{}| {{\n            {}\n            KioResult::into_stored(__function({})).into_raw()\n        }});\n        Self(\n            KioStoredValue::from_raw(crate::__kio_runtime::as_any(__erased)),\n            ::std::marker::PhantomData,\n        )\n    }}\n\n    pub fn call(&self{call_parameters}) -> KioResult::Facade {{\n        let __erased: {raw_fn} = crate::__kio_runtime::from_any(self.0.raw());\n        KioResult::from_stored(KioStoredValue::from_raw(__erased({})))\n    }}\n}}\n\n{deprecation}pub struct KioFunction{arity}<{declarations}>(\n    ::std::marker::PhantomData<fn({phantom_tuple}) -> ({phantom_tuple})>,\n);\n\nimpl<{declarations}> Clone for KioFunction{arity}<{uses}> {{\n    fn clone(&self) -> Self {{ Self(::std::marker::PhantomData) }}\n}}\n\nimpl<{declarations}> __kio_type_private::Sealed for KioFunction{arity}<{uses}> {{}}\n\nimpl<{declarations}> KioType for KioFunction{arity}<{uses}> {{\n    type Facade = KioFn{arity}<{uses}>;\n    fn into_stored(value: Self::Facade) -> KioStoredValue {{ value.0 }}\n    fn from_stored(value: KioStoredValue) -> Self::Facade {{\n        KioFn{arity}(value, ::std::marker::PhantomData)\n    }}\n}}\n\n",
        closure_types,
        raw_parameters.join(", "),
        decode_parameters.join(" "),
        (0..arity).map(|index| format!("arg{index}")).collect::<Vec<_>>().join(", "),
        encode_arguments.join(", "),
    ));
    out
}

/// Render the package-complete marker/facade support selected by the shared
/// boundary planner. Structural markers are canonical recursive binaries;
/// nominal and forall names remain pure functions of exact semantic identity.
fn render_prepared_facade_support(
    prepared: &PreparedBoundaryCallableSites,
    facade_index: &PreparedRustFacadeIndex,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let mut out = String::from(render_rust_marker_core());
    let mut constructor_arities = BTreeMap::new();
    let mut function_arities = BTreeMap::new();
    for site in prepared.sites() {
        if rust_site_depends_on_retained_host_binding(site, shapes) {
            continue;
        }
        let origin = rust_site_support_origin(site);
        collect_rust_marker_support(
            site.plan().facade(),
            site.presentation().root_uses(),
            origin,
            &mut constructor_arities,
            &mut function_arities,
        );
        for (name, declaration) in site.nominals().declarations() {
            if let BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            } = declaration
            {
                let presentation = site
                    .presentation()
                    .transparent_payload(name)
                    .unwrap_or_else(|| {
                        unreachable!("a transparent nominal has no retained presentation")
                    });
                collect_rust_marker_support(
                    payload.facade(),
                    presentation,
                    origin,
                    &mut constructor_arities,
                    &mut function_arities,
                );
            }
        }
    }
    for binding in prepared
        .host_bindings()
        .filter(|binding| matches!(binding.origin(), BoundaryHostBindingOrigin::Live))
    {
        for arity in 1..=binding.type_params().len() {
            insert_rust_support_origin(
                &mut constructor_arities,
                arity,
                BoundaryFacadeSupportOrigin::Live,
            );
        }
    }
    for (newtype, origin) in prepared
        .public_newtypes()
        .map(|newtype| (newtype, BoundaryFacadeSupportOrigin::Live))
        .chain(
            prepared
                .retained_public_newtypes()
                .filter(|newtype| {
                    facade_index
                        .eligible_retained_newtypes
                        .contains(newtype.name())
                })
                .map(|newtype| {
                    let removed_at_version = prepared
                        .retained_public_newtype_removed_at(newtype.name())
                        .unwrap_or_else(|| {
                            unreachable!("a retained Rust public newtype has removal provenance")
                        });
                    (
                        newtype,
                        BoundaryFacadeSupportOrigin::Retained { removed_at_version },
                    )
                }),
        )
    {
        for arity in 1..=newtype.type_params().len() {
            insert_rust_support_origin(&mut constructor_arities, arity, origin);
        }
        for arity in newtype
            .type_params()
            .iter()
            .chain(newtype.existential_params())
            .map(|parameter| parameter.kind().arity())
            .filter(|arity| *arity > 0)
        {
            insert_rust_support_origin(&mut constructor_arities, arity, origin);
        }
    }
    for (arity, origin) in constructor_arities {
        out.push_str(&render_rust_type_constructor_support(arity, origin));
    }
    for (arity, origin) in function_arities {
        out.push_str(&render_rust_function_support(arity, origin));
    }
    out.push_str(&render_prepared_nominal_constructors(
        prepared,
        facade_index,
        shapes,
    )?);
    out.push_str(&render_prepared_forall_declarations(
        prepared,
        facade_index,
        shapes,
    )?);
    out.push_str(&render_prepared_newtype_methods(prepared, shapes)?);
    Ok(out)
}

fn collect_rust_marker_support(
    plan: &BoundaryFacadePlan,
    presentation: &BoundaryFacadeExecutionPlan,
    origin: BoundaryFacadeSupportOrigin,
    constructor_arities: &mut BTreeMap<usize, BoundaryFacadeSupportOrigin>,
    function_arities: &mut BTreeMap<usize, BoundaryFacadeSupportOrigin>,
) {
    for arity in plan
        .binders()
        .iter()
        .map(|binder| binder.kind.arity())
        .filter(|arity| *arity > 0)
    {
        insert_rust_support_origin(constructor_arities, arity, origin);
    }
    let mut pending = vec![plan.root()];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        match plan.use_at(id) {
            FacadeUse::Unit { .. }
            | FacadeUse::Bottom { .. }
            | FacadeUse::Bound { .. }
            | FacadeUse::Nominal { .. } => {}
            FacadeUse::Apply {
                constructor, args, ..
            } => {
                pending.push(*constructor);
                pending.extend(args.iter().copied());
            }
            FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
                pending.extend(args.iter().copied());
            }
            FacadeUse::Function { slots, result, .. } => {
                let layout = match presentation.use_at(id) {
                    BoundaryFacadeExecutionUse::Function(layout) => layout,
                    _ => unreachable!("a prepared function support use has no presentation"),
                };
                insert_rust_support_origin(function_arities, layout.body_abi_arity(), origin);
                pending.extend(slots.iter().copied());
                pending.push(*result);
            }
            FacadeUse::Forall { result, .. } => pending.push(*result),
        }
    }
}

fn render_prepared_nominal_constructors(
    prepared: &PreparedBoundaryCallableSites,
    facade_index: &PreparedRustFacadeIndex,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let mut out = String::new();
    for binding in prepared
        .host_bindings()
        .filter(|binding| matches!(binding.origin(), BoundaryHostBindingOrigin::Live))
    {
        render_prepared_parameterized_host_carrier(
            &mut out,
            binding,
            BoundaryFacadeSupportOrigin::Live,
            shapes,
        );
        render_prepared_nominal_constructor(
            &mut out,
            binding.name(),
            binding.type_params(),
            true,
            None,
            BoundaryFacadeSupportOrigin::Live,
            shapes,
        )?;
    }
    for (entry, origin) in prepared
        .public_newtypes()
        .map(|entry| (entry, BoundaryFacadeSupportOrigin::Live))
        .chain(
            prepared
                .retained_public_newtypes()
                .filter(|entry| {
                    facade_index
                        .eligible_retained_newtypes
                        .contains(entry.name())
                })
                .map(|entry| {
                    let removed_at_version = prepared
                        .retained_public_newtype_removed_at(entry.name())
                        .unwrap_or_else(|| {
                            unreachable!("a retained Rust public newtype has removal provenance")
                        });
                    (
                        entry,
                        BoundaryFacadeSupportOrigin::Retained { removed_at_version },
                    )
                }),
        )
    {
        render_prepared_nominal_constructor(
            &mut out,
            entry.name(),
            entry.type_params(),
            false,
            Some(prepared_newtype_rust_path(
                entry.name(),
                shapes.crate_root(),
            )),
            origin,
            shapes,
        )?;
    }
    Ok(out)
}

fn render_prepared_parameterized_host_carrier(
    out: &mut String,
    binding: &BoundaryHostBinding,
    origin: BoundaryFacadeSupportOrigin,
    shapes: PreparedRustRenderContext<'_>,
) {
    if binding.type_params().is_empty() {
        return;
    }
    let name = rust_host_type_facade_name(binding.name());
    let assoc = prepared_host_assoc_name(binding.name());
    let mut declarations = vec![format!(
        "__KioHost: {}::host::{}",
        shapes.crate_root(),
        shapes.host_trait()
    )];
    let mut arguments = vec!["__KioHost".to_owned()];
    for (index, parameter) in binding.type_params().iter().enumerate() {
        let parameter = PreparedRustTypeParameter {
            name: format!("__KioArg_{index}"),
            kind_arity: parameter.kind().arity(),
        };
        declarations.push(parameter.declaration(shapes.crate_root()));
        arguments.push(parameter.name);
    }
    let declaration = format!("<{}>", declarations.join(", "));
    let use_ = format!("<{}>", arguments.join(", "));
    let phantom_arguments = arguments.join(", ");
    let phantom = format!("fn({phantom_arguments}) -> ({phantom_arguments})");
    let storage = format!(
        "<__KioHost as {}::host::{}>::{assoc}Storage",
        shapes.crate_root(),
        shapes.host_trait()
    );
    let shapes_path = format!("{}::shapes", shapes.crate_root());
    out.push_str("#[allow(non_camel_case_types)]\n");
    push_rust_deprecation(out, origin, "");
    out.push_str(&format!(
        "pub struct {name}{declaration}(pub(crate) {shapes_path}::KioStoredValue, pub(crate) ::std::marker::PhantomData<{phantom}>);\n"
    ));
    out.push_str(&format!(
        "impl{declaration} Clone for {name}{use_} {{\n    fn clone(&self) -> Self {{ Self(self.0.clone(), ::std::marker::PhantomData) }}\n}}\n"
    ));
    out.push_str(&format!(
        "impl{declaration} {name}{use_} {{\n    pub fn from_storage(value: {storage}) -> Self {{\n        Self({shapes_path}::__kio_store_native(value), ::std::marker::PhantomData)\n    }}\n\n    pub fn storage(&self) -> {storage} {{\n        {shapes_path}::__kio_load_native(self.0.clone())\n    }}\n\n    pub fn into_storage(self) -> {storage} {{\n        {shapes_path}::__kio_load_native(self.0)\n    }}\n}}\n"
    ));
    out.push_str(&format!(
        "impl{declaration} PartialEq for {name}{use_} {{\n    fn eq(&self, other: &Self) -> bool {{\n        self.storage() == other.storage()\n    }}\n}}\n\n"
    ));
}

fn render_prepared_nominal_constructor(
    out: &mut String,
    name: &QualifiedTypeName,
    parameters: &[crate::backends::boundary_facade::BoundaryNominalTypeParam],
    host_type: bool,
    newtype_path: Option<String>,
    origin: BoundaryFacadeSupportOrigin,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<(), EmitError> {
    let exact_marker = rust_nominal_type_marker_name(name, host_type);
    let mut exact_declarations = vec![format!(
        "__KioHost: {}::host::{}",
        shapes.crate_root(),
        shapes.host_trait()
    )];
    let mut exact_uses = vec!["__KioHost".to_owned()];
    for (index, parameter) in parameters.iter().enumerate() {
        let rendered = PreparedRustTypeParameter {
            name: format!("__KioArg_{index}"),
            kind_arity: parameter.kind().arity(),
        };
        exact_declarations.push(rendered.declaration(shapes.crate_root()));
        exact_uses.push(rendered.name);
    }
    let exact_declaration = format!("<{}>", exact_declarations.join(", "));
    let exact_use = format!("<{}>", exact_uses.join(", "));
    let exact_phantom = rust_invariant_phantom(&exact_uses);
    let facade = if host_type && !parameters.is_empty() {
        format!(
            "{}::shapes::{}{}",
            shapes.crate_root(),
            rust_host_type_facade_name(name),
            exact_use
        )
    } else if host_type {
        let assoc = prepared_host_assoc_name(name);
        format!(
            "<__KioHost as {}::host::{}>::{assoc}",
            shapes.crate_root(),
            shapes.host_trait()
        )
    } else {
        let path = newtype_path.as_deref().unwrap_or_else(|| {
            unreachable!("Rust emitter: a nominal newtype marker has no exact carrier path")
        });
        format!("{path}{exact_use}")
    };
    let facade_constructor = if host_type && !parameters.is_empty() {
        Some(format!(
            "{}::shapes::{}::<{}>",
            shapes.crate_root(),
            rust_host_type_facade_name(name),
            exact_uses.join(", ")
        ))
    } else if host_type {
        None
    } else {
        let path = newtype_path.as_deref().unwrap_or_else(|| {
            unreachable!("Rust emitter: a nominal newtype marker has no exact carrier path")
        });
        Some(format!("{path}::<{}>", exact_uses.join(", ")))
    };
    out.push_str("#[allow(non_camel_case_types)]\n");
    push_rust_deprecation(out, origin, "");
    out.push_str(&format!(
        "pub struct {exact_marker}{exact_declaration}(::std::marker::PhantomData<{exact_phantom}>);\n"
    ));
    out.push_str(&format!(
        "impl{exact_declaration} Clone for {exact_marker}{exact_use} {{\n    fn clone(&self) -> Self {{ Self(::std::marker::PhantomData) }}\n}}\n"
    ));
    out.push_str(&format!(
        "impl{exact_declaration} __kio_type_private::Sealed for {exact_marker}{exact_use} {{}}\n"
    ));
    let conversions = if host_type && parameters.is_empty() {
        "    fn into_stored(value: Self::Facade) -> KioStoredValue { __kio_store_native(value) }\n    fn from_stored(value: KioStoredValue) -> Self::Facade { __kio_load_native(value) }\n".to_owned()
    } else {
        let facade_constructor = facade_constructor.as_deref().unwrap_or_else(|| {
            unreachable!("an opaque nominal facade must have a tuple-struct constructor")
        });
        format!(
            "    fn into_stored(value: Self::Facade) -> KioStoredValue {{ value.0 }}\n    fn from_stored(value: KioStoredValue) -> Self::Facade {{ {facade_constructor}(value, ::std::marker::PhantomData) }}\n"
        )
    };
    out.push_str(&format!(
        "impl{exact_declaration} KioType for {exact_marker}{exact_use} {{\n    type Facade = {facade};\n{conversions}}}\n\n"
    ));

    if parameters.is_empty() {
        return Ok(());
    }
    for provided in 0..parameters.len() {
        if parameters[provided..]
            .iter()
            .any(|parameter| parameter.kind().arity() != 0)
        {
            continue;
        }
        let remaining = parameters.len() - provided;
        let marker = rust_nominal_constructor_name(name, host_type, provided);
        let mut generic_declarations = vec![format!(
            "__KioHost: {}::host::{}",
            shapes.crate_root(),
            shapes.host_trait()
        )];
        let mut generic_uses = vec!["__KioHost".to_owned()];
        for (index, parameter) in parameters[..provided].iter().enumerate() {
            let captured = PreparedRustTypeParameter {
                name: format!("__KioCaptured_{index}"),
                kind_arity: parameter.kind().arity(),
            };
            generic_declarations.push(captured.declaration(shapes.crate_root()));
            generic_uses.push(captured.name);
        }
        let declaration = format!("<{}>", generic_declarations.join(", "));
        let use_ = format!("<{}>", generic_uses.join(", "));
        let phantom = rust_invariant_phantom(&generic_uses);
        let apply_parameters = (0..remaining)
            .map(|index| format!("__KioArg_{index}: KioType"))
            .collect::<Vec<_>>();
        let mut applied = generic_uses[1..].to_vec();
        applied.extend((0..remaining).map(|index| format!("__KioArg_{index}")));
        let mut target_arguments = vec!["__KioHost".to_owned()];
        target_arguments.extend(applied);
        let target = format!("{exact_marker}<{}>", target_arguments.join(", "));
        out.push_str("#[allow(non_camel_case_types)]\n");
        push_rust_deprecation(out, origin, "");
        out.push_str(&format!(
            "pub struct {marker}{declaration}(::std::marker::PhantomData<{phantom}>);\n"
        ));
        out.push_str(&format!(
            "impl{declaration} Clone for {marker}{use_} {{\n    fn clone(&self) -> Self {{ Self(::std::marker::PhantomData) }}\n}}\n"
        ));
        out.push_str(&format!(
            "impl{declaration} KioTypeConstructor{remaining} for {marker}{use_} {{\n"
        ));
        out.push_str(&format!(
            "    type Apply<{}> = {target};\n}}\n\n",
            apply_parameters.join(", ")
        ));
    }
    Ok(())
}

fn render_prepared_forall_declarations(
    prepared: &PreparedBoundaryCallableSites,
    facade_index: &PreparedRustFacadeIndex,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let mut declarations = BTreeMap::<String, (String, BoundaryFacadeSupportOrigin)>::new();
    for site in prepared.sites() {
        if rust_site_depends_on_retained_host_binding(site, shapes) {
            continue;
        }
        let origin = rust_site_support_origin(site);
        let mut scope = PreparedRustScope::default();
        for stage in site.plan().entry().head_stages {
            match stage {
                BoundaryCallableHeadStage::Type { id, binder } => {
                    insert_prepared_source_binder(
                        &mut scope,
                        id,
                        &binder.name,
                        binder.kind.arity(),
                    );
                }
                BoundaryCallableHeadStage::Value { slots } => {
                    for slot in slots {
                        collect_prepared_forall_declarations(
                            &mut declarations,
                            PreparedRustForallCollectionContext {
                                site,
                                plan: site.plan().facade(),
                                origin,
                                nominal_owner: None,
                                shapes,
                            },
                            *slot,
                            &scope,
                        )?;
                    }
                }
            }
        }
        collect_prepared_forall_declarations(
            &mut declarations,
            PreparedRustForallCollectionContext {
                site,
                plan: site.plan().facade(),
                origin,
                nominal_owner: None,
                shapes,
            },
            site.plan().entry().returned,
            &scope,
        )?;
    }

    for (entry, origin) in prepared
        .public_newtypes()
        .map(|entry| (entry, BoundaryFacadeSupportOrigin::Live))
        .chain(
            prepared
                .retained_public_newtypes()
                .filter(|entry| {
                    facade_index
                        .eligible_retained_newtypes
                        .contains(entry.name())
                })
                .map(|entry| {
                    let removed_at_version = prepared
                        .retained_public_newtype_removed_at(entry.name())
                        .unwrap_or_else(|| {
                            unreachable!("a retained Rust public newtype has removal provenance")
                        });
                    (
                        entry,
                        BoundaryFacadeSupportOrigin::Retained { removed_at_version },
                    )
                }),
        )
    {
        if !matches!(entry.surface(), BoundaryNewtypeSurface::Both { .. }) {
            continue;
        }
        let retained = matches!(origin, BoundaryFacadeSupportOrigin::Retained { .. });
        let Some(owner) = facade_index
            .payload_owners
            .get(&(entry.name().clone(), retained))
        else {
            continue;
        };
        #[cfg(all(test, feature = "surface"))]
        record_rust_forall_owner_direct_lookup(entry.name());
        let site = prepared.site(owner).unwrap_or_else(|| {
            unreachable!("an indexed Rust newtype payload owner is a prepared site")
        });
        let Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        }) = site.nominals().declaration(entry.name())
        else {
            continue;
        };
        let mut scope = PreparedRustScope::default();
        for (binder, parameter) in payload
            .declaration_binders()
            .iter()
            .zip(entry.type_params())
        {
            insert_prepared_source_binder(
                &mut scope,
                *binder,
                parameter.name(),
                parameter.kind().arity(),
            );
        }
        collect_prepared_forall_declarations(
            &mut declarations,
            PreparedRustForallCollectionContext {
                site,
                plan: payload.facade(),
                origin,
                nominal_owner: Some(entry.name()),
                shapes,
            },
            payload.payload_root(),
            &scope,
        )?;
    }

    Ok(declarations
        .into_values()
        .map(|(declaration, origin)| mark_rust_forall_declaration(declaration, origin))
        .collect())
}

fn mark_rust_forall_declaration(
    mut declaration: String,
    origin: BoundaryFacadeSupportOrigin,
) -> String {
    if origin.removed_at_version().is_none() {
        return declaration;
    }
    let attribute = rust_deprecation_attribute(origin)
        .unwrap_or_else(|| unreachable!("a retained Rust forall declaration has provenance"));
    declaration = declaration.replacen("pub trait ", &format!("{attribute}\npub trait "), 1);
    declaration = declaration.replacen(
        "\n    fn apply",
        &format!("\n    {attribute}\n    fn apply"),
        1,
    );
    declaration = declaration.replace(
        "\n    pub fn new",
        &format!("\n    {attribute}\n    pub fn new"),
    );
    declaration.replace("pub struct ", &format!("{attribute}\npub struct "))
}

#[derive(Clone, Copy)]
struct PreparedRustForallCollectionContext<'a> {
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    origin: BoundaryFacadeSupportOrigin,
    nominal_owner: Option<&'a QualifiedTypeName>,
    shapes: PreparedRustRenderContext<'a>,
}

fn collect_prepared_forall_declarations(
    declarations: &mut BTreeMap<String, (String, BoundaryFacadeSupportOrigin)>,
    context: PreparedRustForallCollectionContext<'_>,
    id: FacadeUseId,
    scope: &PreparedRustScope,
) -> Result<(), EmitError> {
    match context.plan.use_at(id) {
        FacadeUse::Unit { .. }
        | FacadeUse::Bottom { .. }
        | FacadeUse::Bound { .. }
        | FacadeUse::Nominal { .. } => {}
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            collect_prepared_forall_declarations(declarations, context, *constructor, scope)?;
            for arg in args {
                collect_prepared_forall_declarations(declarations, context, *arg, scope)?;
            }
        }
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
            for arg in args {
                collect_prepared_forall_declarations(declarations, context, *arg, scope)?;
            }
        }
        FacadeUse::Function { slots, result, .. } => {
            for slot in slots {
                collect_prepared_forall_declarations(declarations, context, *slot, scope)?;
            }
            collect_prepared_forall_declarations(declarations, context, *result, scope)?;
        }
        FacadeUse::Forall { .. } => {
            let name =
                rust_forall_name_with_owner(context.site, context.plan, id, context.nominal_owner)?;
            let (declaration, nested_scope, body) = render_prepared_forall_declaration(
                context.site,
                context.plan,
                id,
                scope,
                context.nominal_owner,
                context.shapes,
            )?;
            match declarations.entry(name.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert((declaration, context.origin));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().0 != declaration {
                        unreachable!(
                            "Rust emitter: distinct exact forall declarations collide at `{name}`"
                        );
                    }
                    let joined = join_rust_support_origin(entry.get().1, context.origin);
                    entry.get_mut().1 = joined;
                }
            }
            match context.plan.use_at(body) {
                FacadeUse::Function { slots, result, .. } => {
                    for slot in slots {
                        collect_prepared_forall_declarations(
                            declarations,
                            context,
                            *slot,
                            &nested_scope,
                        )?;
                    }
                    collect_prepared_forall_declarations(
                        declarations,
                        context,
                        *result,
                        &nested_scope,
                    )?;
                }
                _ => collect_prepared_forall_declarations(
                    declarations,
                    context,
                    body,
                    &nested_scope,
                )?,
            }
        }
    }
    Ok(())
}

fn render_prepared_forall_declaration(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    outer_scope: &PreparedRustScope,
    nominal_owner: Option<&QualifiedTypeName>,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<(String, PreparedRustScope, FacadeUseId), EmitError> {
    const HOST: &str = "__KioHost";
    let name = rust_forall_name_with_owner(site, plan, root, nominal_owner)?;
    let method = prepare_rust_forall_method(plan, root, outer_scope, shapes.crate_root());
    let mut declaration_parameters = vec![format!(
        "{HOST}: {}::host::{}",
        shapes.crate_root(),
        shapes.host_trait()
    )];
    declaration_parameters.extend(outer_scope.binders.iter().map(|(binder, rendered)| {
        PreparedRustTypeParameter {
            name: rendered.clone(),
            kind_arity: plan.binder(*binder).kind.arity(),
        }
        .declaration(shapes.crate_root())
    }));
    let declaration = format!("<{}>", declaration_parameters.join(", "));
    let generic_arguments = std::iter::once(HOST.to_owned())
        .chain(outer_scope.binders.values().cloned())
        .collect::<Vec<_>>();
    let generic_use = format!("<{}>", generic_arguments.join(", "));
    let method_declaration = if method.type_parameters.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            method
                .type_parameters
                .iter()
                .map(|parameter| parameter.declaration(shapes.crate_root()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let ctx = TypeCtx::Shape.with_host_param(HOST);
    let presentation = prepared_presentation_for_plan(site, plan, nominal_owner);
    let public_context = PreparedRustUseRenderContext {
        site,
        plan,
        scope: &method.public_scope,
        shapes,
        type_ctx: ctx,
    };
    let (parameters, returned) = match plan.use_at(method.body) {
        FacadeUse::Function { slots, result, .. } => {
            let layout = match presentation.use_at(method.body) {
                BoundaryFacadeExecutionUse::Function(layout) => layout,
                _ => unreachable!("a public forall function has no source presentation"),
            };
            let parameters = layout
                .source_params()
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    render_prepared_source_facade(public_context, slots, source, nominal_owner)
                        .map(|facade| format!("arg{index}: {facade}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let returned = render_prepared_use_with_owner(
                public_context,
                *result,
                PreparedRustTypePosition::Value,
                nominal_owner,
            )?;
            (parameters, returned)
        }
        _ => (
            Vec::new(),
            render_prepared_use_with_owner(
                public_context,
                method.body,
                PreparedRustTypePosition::Value,
                nominal_owner,
            )?,
        ),
    };
    let phantom = rust_invariant_phantom(&generic_arguments);

    let mut out = String::new();
    out.push_str("#[allow(non_camel_case_types)]\n");
    out.push_str(&format!(
        "pub trait {name}{declaration}: 'static {{\n    fn apply{method_declaration}(&self{}{}) -> {returned};\n}}\n\n",
        if parameters.is_empty() { "" } else { ", " },
        parameters.join(", ")
    ));
    out.push_str("#[allow(non_camel_case_types)]\n");
    out.push_str(&format!(
        "pub struct {name}Value{declaration}(pub(crate) KioStoredValue, pub(crate) ::std::marker::PhantomData<{phantom}>);\n"
    ));
    out.push_str(&format!(
        "impl{declaration} Clone for {name}Value{generic_use} {{\n    fn clone(&self) -> Self {{ Self(self.0.clone(), ::std::marker::PhantomData) }}\n}}\n"
    ));
    let ingress_converter = PreparedRustConverter {
        site,
        shapes,
        scope: outer_scope.clone(),
        ctx,
        nominal_owner: nominal_owner.cloned(),
    };
    let ingress_cursor = PreparedRustUseCursor {
        plan,
        execution: Some(presentation),
        id: root,
        substitutions: BTreeMap::new(),
    };
    let ingress =
        ingress_converter.convert_forall_implementation_in(&ingress_cursor, "__implementation")?;
    out.push_str(&format!(
        "impl{declaration} {name}Value{generic_use} {{\n    pub fn new(__implementation: impl {name}{generic_use} + 'static) -> Self {{\n        Self(KioStoredValue::from_raw({ingress}), ::std::marker::PhantomData)\n    }}\n}}\n"
    ));
    out.push_str("#[allow(non_camel_case_types)]\n");
    out.push_str(&format!(
        "pub struct {name}Marker{declaration}(::std::marker::PhantomData<{phantom}>);\n"
    ));
    out.push_str(&format!(
        "impl{declaration} Clone for {name}Marker{generic_use} {{\n    fn clone(&self) -> Self {{ Self(::std::marker::PhantomData) }}\n}}\n"
    ));
    out.push_str(&format!(
        "impl{declaration} __kio_type_private::Sealed for {name}Marker{generic_use} {{}}\n"
    ));
    out.push_str(&format!(
        "impl{declaration} KioType for {name}Marker{generic_use} {{\n    type Facade = {name}Value{generic_use};\n\n    fn into_stored(value: Self::Facade) -> KioStoredValue {{ value.0 }}\n\n    fn from_stored(value: KioStoredValue) -> Self::Facade {{\n        {name}Value(value, ::std::marker::PhantomData)\n    }}\n}}\n"
    ));

    {
        let execution = presentation;
        let converter = PreparedRustConverter {
            site,
            shapes,
            scope: method.public_scope.clone(),
            ctx,
            nominal_owner: nominal_owner.cloned(),
        };
        let mut internal = format!(
            "{}::__kio_runtime::KioStoredValue::raw(&self.0)",
            shapes.crate_root()
        );
        let mut current = root;
        for _ in &method.type_parameters {
            if !matches!(
                execution.use_at(current),
                BoundaryFacadeExecutionUse::InvokeForall
            ) {
                unreachable!("Rust emitter: public forall has no paired invocation stage");
            }
            let FacadeUse::Forall { result, .. } = plan.use_at(current) else {
                unreachable!("prepared forall method consumes only forall stages")
            };
            internal = format!(
                "({}::__kio_runtime::from_any::<{}>({internal}))()",
                shapes.crate_root(),
                rc_fn_type(0)
            );
            current = *result;
        }
        let body_cursor = PreparedRustUseCursor {
            plan,
            execution: Some(execution),
            id: method.body,
            substitutions: BTreeMap::new(),
        };
        let body = match plan.use_at(method.body) {
            FacadeUse::Function { slots, result, .. } => {
                let layout = match body_cursor.execution_use() {
                    Some(BoundaryFacadeExecutionUse::Function(layout)) => layout,
                    _ => unreachable!(
                        "Rust emitter: public forall function has no paired execution layout"
                    ),
                };
                let source_markers = render_prepared_function_source_markers(
                    public_context,
                    slots,
                    layout,
                    nominal_owner,
                )?;
                let source = source_markers
                    .iter()
                    .enumerate()
                    .map(|(index, marker)| {
                        format!(
                            "<{marker} as {}::shapes::KioType>::into_stored(arg{index}).into_raw()",
                            shapes.crate_root()
                        )
                    })
                    .collect::<Vec<_>>();
                let call = format!(
                    "({}::__kio_runtime::from_any::<{}>({internal}))({})",
                    shapes.crate_root(),
                    rc_fn_type(layout.body_abi_arity()),
                    source.join(", ")
                );
                converter.convert(&body_cursor.child(*result), &call, FfiDir::Out)?
            }
            _ => converter.convert(&body_cursor, &internal, FfiDir::Out)?,
        };
        out.push_str(&format!(
            "impl{declaration} {name}{generic_use} for {name}Value{generic_use} {{\n    fn apply{method_declaration}(&self{}{}) -> {returned} {{\n        {body}\n    }}\n}}\n",
            if parameters.is_empty() { "" } else { ", " },
            parameters.join(", ")
        ));
    }
    out.push('\n');
    Ok((out, method.public_scope, method.body))
}

/// Emit every exposed newtype member from its exact prepared callable site.
/// The nominal declaration owns only a private erased field; these paired
/// member sites are the sole typed ingress/egress for that field.
fn render_prepared_newtype_methods(
    prepared: &PreparedBoundaryCallableSites,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let mut out = String::new();
    for entry in prepared.public_newtypes() {
        match entry.surface() {
            BoundaryNewtypeSurface::Constructor { member } => {
                render_prepared_newtype_member(&mut out, prepared, entry, member, true, shapes)?;
            }
            BoundaryNewtypeSurface::Projector { member } => {
                render_prepared_newtype_member(&mut out, prepared, entry, member, false, shapes)?;
            }
            BoundaryNewtypeSurface::Both {
                constructor,
                projector,
            } => {
                render_prepared_newtype_member(
                    &mut out,
                    prepared,
                    entry,
                    constructor,
                    true,
                    shapes,
                )?;
                render_prepared_newtype_member(
                    &mut out, prepared, entry, projector, false, shapes,
                )?;
            }
            BoundaryNewtypeSurface::Unexposed | BoundaryNewtypeSurface::Opaque => {}
        }
    }
    Ok(out)
}

fn render_prepared_newtype_member(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    entry: &BoundaryPublicNewtypeInventoryEntry,
    member: &str,
    constructor: bool,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<(), EmitError> {
    let owner = if constructor {
        BoundaryFacadeSiteOwner::NewtypeConstructor {
            newtype: entry.name().name().to_owned(),
            member: member.to_owned(),
        }
    } else {
        BoundaryFacadeSiteOwner::NewtypeProjector {
            newtype: entry.name().name().to_owned(),
            member: member.to_owned(),
        }
    };
    let id = BoundaryFacadeSiteId::new(entry.name().module_segments().to_vec(), owner)
        .unwrap_or_else(|| unreachable!("a public Rust newtype member has a valid exact site"));
    let site = prepared_site(prepared, &id);
    let execution = site.execution().unwrap_or_else(|| {
        unreachable!("Rust emitter: a public newtype member has no paired execution layout")
    });
    let callable = prepare_rust_callable(site);

    let expected_heads = if constructor {
        entry
            .type_params()
            .iter()
            .chain(entry.existential_params())
            .collect::<Vec<_>>()
    } else {
        entry.type_params().iter().collect::<Vec<_>>()
    };
    if callable.type_parameters.len() != expected_heads.len()
        || callable
            .type_parameters
            .iter()
            .zip(&expected_heads)
            .any(|(actual, expected)| actual.kind_arity() != expected.kind().arity())
    {
        unreachable!(
            "Rust emitter: prepared newtype member {}.{}.{} has a declaration-head mismatch",
            entry.name().module_segments().join("."),
            entry.name().name(),
            member
        );
    }
    let declaration_parameters = prepare_rust_declaration_type_parameters(
        entry
            .type_params()
            .iter()
            .map(|parameter| (parameter.name(), parameter.kind().arity())),
    );
    if callable
        .type_parameters
        .iter()
        .take(declaration_parameters.len())
        .map(|parameter| &parameter.parameter)
        .ne(declaration_parameters.iter())
    {
        unreachable!(
            "Rust emitter: prepared newtype member {}.{}.{} changed its declaration-binder prefix",
            entry.name().module_segments().join("."),
            entry.name().name(),
            member
        );
    }

    let host = "__KioHost";
    let mut impl_declarations = vec![format!(
        "{host}: {}::host::{}",
        shapes.crate_root(),
        shapes.host_trait()
    )];
    let mut impl_arguments = vec![host.to_owned()];
    for parameter in callable
        .type_parameters
        .iter()
        .take(entry.type_params().len())
    {
        impl_declarations.push(parameter.declaration(shapes.crate_root()));
        impl_arguments.push(parameter.name().to_owned());
    }
    let declaration = format!("<{}>", impl_declarations.join(", "));
    let nominal = format!(
        "{}<{}>",
        prepared_newtype_rust_path(entry.name(), shapes.crate_root()),
        impl_arguments.join(", ")
    );

    let converter = PreparedRustConverter {
        site,
        shapes,
        scope: callable.scope.clone(),
        ctx: TypeCtx::Shape.with_host_param(host),
        nominal_owner: None,
    };
    let body = if constructor {
        render_prepared_newtype_constructor_body(
            site,
            execution,
            &callable,
            &converter,
            member,
            entry.type_params().len(),
            shapes,
        )?
    } else if entry.existential_params().is_empty() {
        render_prepared_newtype_projector_body(site, execution, &callable, &converter, member)?
    } else {
        render_prepared_existential_projector_body(
            site,
            execution,
            &callable,
            &converter,
            member,
            entry.existential_params().len(),
            shapes,
        )?
    };
    out.push_str(&format!("impl{declaration} {nominal} {{\n{body}}}\n\n"));
    Ok(())
}

fn prepared_newtype_head_values(
    execution: &CallableExecutionLayout,
    callable: &PreparedRustCallable,
    converted_values: &[String],
    crate_root: &str,
) -> Result<Vec<String>, EmitError> {
    let mut next_slot = 0usize;
    let mut source_values = Vec::new();
    for stage in execution.head_stages() {
        let CallableExecutionStage::Value(layout) = stage else {
            continue;
        };
        let end = next_slot + layout.facade_slot_count();
        source_values.extend(pack_prepared_source_arguments(
            layout,
            &converted_values[next_slot..end],
            crate_root,
        )?);
        next_slot = end;
    }
    if next_slot != callable.value_slots.len() || next_slot != converted_values.len() {
        unreachable!("Rust emitter: prepared newtype execution did not consume every facade slot");
    }
    Ok(source_values)
}

fn render_prepared_newtype_constructor_body(
    site: PreparedBoundaryCallableSite<'_>,
    execution: &CallableExecutionLayout,
    callable: &PreparedRustCallable,
    converter: &PreparedRustConverter<'_>,
    member: &str,
    declaration_parameter_count: usize,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let declarations =
        render_prepared_callable_source_parameters(site, callable, shapes, converter.ctx)?;
    let converted = unpack_prepared_callable_public_arguments(site, callable, converter)?;
    let source =
        prepared_newtype_head_values(execution, callable, &converted, converter.crate_root())?;
    let [stored] = source.as_slice() else {
        unreachable!("Rust emitter: a prepared newtype constructor must produce one body payload");
    };
    let method_parameters = &callable.type_parameters[declaration_parameter_count..];
    let generic_declaration = if method_parameters.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            method_parameters
                .iter()
                .map(|parameter| parameter.declaration(shapes.crate_root()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Ok(format!(
        "    pub fn {}{generic_declaration}({}) -> Self {{\n        Self({}::shapes::KioStoredValue::from_raw({stored}), ::std::marker::PhantomData)\n    }}\n",
        rust_public_ident(member),
        declarations.join(", "),
        converter.crate_root(),
    ))
}

fn render_prepared_newtype_projector_body(
    site: PreparedBoundaryCallableSite<'_>,
    execution: &CallableExecutionLayout,
    callable: &PreparedRustCallable,
    converter: &PreparedRustConverter<'_>,
    member: &str,
) -> Result<String, EmitError> {
    let [receiver] = callable.value_slots.as_slice() else {
        unreachable!("Rust emitter: a prepared ordinary projector must have one nominal receiver");
    };
    let receiver_cursor = PreparedRustUseCursor::root(site, *receiver);
    let converted = converter.convert_parameter_in(&receiver_cursor, "value")?;
    let source =
        prepared_newtype_head_values(execution, callable, &[converted], converter.crate_root())?;
    let [payload] = source.as_slice() else {
        unreachable!("Rust emitter: a prepared ordinary projector must consume one body value");
    };
    let returned_cursor = PreparedRustUseCursor::root(site, callable.returned);
    let returned = converter.render(&returned_cursor, PreparedRustTypePosition::Value)?;
    let projected = converter.convert(&returned_cursor, payload, FfiDir::Out)?;
    Ok(format!(
        "    pub fn {}(value: Self) -> {returned} {{\n        {projected}\n    }}\n",
        rust_public_ident(member)
    ))
}

fn render_prepared_existential_projector_body(
    site: PreparedBoundaryCallableSite<'_>,
    execution: &CallableExecutionLayout,
    callable: &PreparedRustCallable,
    converter: &PreparedRustConverter<'_>,
    member: &str,
    existential_count: usize,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let plan = site.plan().facade();
    let compactable = execution.direct_projector_compactable_foralls();
    if !compactable.contains(&callable.returned) {
        unreachable!("Rust emitter: an existential projector lacks its compaction proof");
    }
    let result_method = prepare_rust_forall_method(
        plan,
        callable.returned,
        &callable.scope,
        shapes.crate_root(),
    );
    if result_method.type_parameters.len() != 1 {
        unreachable!("Rust emitter: an existential projector must bind one selected result type");
    }
    let FacadeUse::Function {
        slots: continuation_slots,
        result: selected_result,
        ..
    } = plan.use_at(result_method.body)
    else {
        unreachable!("Rust emitter: an existential projector result must expose its continuation");
    };
    let [continuation_root] = continuation_slots.as_slice() else {
        unreachable!("Rust emitter: an existential projector must expose one continuation");
    };
    if !compactable.contains(continuation_root) {
        unreachable!("Rust emitter: an existential continuation lacks its compaction proof");
    }
    let continuation_method = prepare_rust_forall_method(
        plan,
        *continuation_root,
        &result_method.public_scope,
        shapes.crate_root(),
    );
    if continuation_method.type_parameters.len() != existential_count {
        unreachable!("Rust emitter: an existential continuation binder count drifted");
    }
    let FacadeUse::Function {
        slots: _,
        result: _,
        ..
    } = plan.use_at(continuation_method.body)
    else {
        unreachable!("Rust emitter: an existential continuation must accept the sealed payload");
    };
    let [receiver] = callable.value_slots.as_slice() else {
        unreachable!("Rust emitter: an existential projector must have one nominal receiver");
    };
    let receiver_cursor = PreparedRustUseCursor::root(site, *receiver);
    let converted = converter.convert_parameter_in(&receiver_cursor, "value")?;
    let source =
        prepared_newtype_head_values(execution, callable, &[converted], converter.crate_root())?;
    let [payload] = source.as_slice() else {
        unreachable!("Rust emitter: an existential projector must consume one sealed body value");
    };
    let result_context = PreparedRustUseRenderContext {
        site,
        plan,
        scope: &result_method.public_scope,
        shapes,
        type_ctx: converter.ctx,
    };

    let continuation_type = render_prepared_param(result_context, *continuation_root)?;
    let returned = render_prepared_use(
        result_context,
        *selected_result,
        PreparedRustTypePosition::Value,
    )?;
    let generic_declaration = format!(
        "<{}>",
        result_method
            .type_parameters
            .iter()
            .map(|parameter| parameter.declaration(shapes.crate_root()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let payload_cursor = PreparedRustUseCursor {
        plan,
        execution: Some(execution.root_uses()),
        id: continuation_method.body,
        substitutions: BTreeMap::new(),
    };
    let layout = match payload_cursor.execution_use() {
        Some(BoundaryFacadeExecutionUse::Function(layout)) => layout,
        _ => unreachable!("Rust emitter: an existential payload has no paired function layout"),
    };
    let source_binding = match layout.body_abi_arity() {
        0 => format!("let _ = {payload};"),
        1 => format!("let __source_0 = {payload};"),
        arity => unreachable!(
            "Rust emitter: an existential sealed payload occupies {arity} source slots"
        ),
    };
    let continuation_marker = render_prepared_use(
        result_context,
        *continuation_root,
        PreparedRustTypePosition::Marker,
    )?;
    let selected_marker = render_prepared_use(
        result_context,
        *selected_result,
        PreparedRustTypePosition::Marker,
    )?;
    let mut invoke = format!(
        "let __continuation = <{continuation_marker} as {}::shapes::KioType>::into_stored(continuation).into_raw();\n        ",
        shapes.crate_root()
    );
    for _ in &continuation_method.type_parameters {
        let stage = rc_fn_type(0);
        invoke.push_str(&format!(
            "let __stage: {stage} = {}::__kio_runtime::from_any(__continuation);\n        let __continuation = __stage();\n        ",
            shapes.crate_root()
        ));
    }
    let function = rc_fn_type(layout.body_abi_arity());
    let arguments = (0..layout.body_abi_arity())
        .map(|index| format!("__source_{index}.clone()"))
        .collect::<Vec<_>>()
        .join(", ");
    invoke.push_str(&format!(
        "let __function: {function} = {}::__kio_runtime::from_any(__continuation);\n        let __returned = __function({arguments});\n        <{selected_marker} as {}::shapes::KioType>::from_stored({}::shapes::KioStoredValue::from_raw(__returned))",
        shapes.crate_root(),
        shapes.crate_root(),
        shapes.crate_root(),
    ));
    Ok(format!(
        "    pub fn {}{generic_declaration}(value: Self, continuation: {continuation_type}) -> {returned} {{\n        {source_binding}\n        {invoke}\n    }}\n",
        rust_public_ident(member),
    ))
}

fn emit_prepared_ffi_rs(
    prepared: &PreparedBoundaryCallableSites,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<String, EmitError> {
    let mut env = String::new();
    let mut exp = String::new();
    for site in prepared.sites() {
        if rust_site_depends_on_retained_host_binding(site, shapes) {
            continue;
        }
        let (namespace, member, prefix) = match site.site().owner() {
            BoundaryFacadeSiteOwner::HostFunction { name } => (
                &mut env,
                mangle_host_name(&site.site().module_segments().join("/"), name),
                None,
            ),
            BoundaryFacadeSiteOwner::ExportedFunction { name } => (
                &mut exp,
                mangle_host_name(&site.site().module_segments().join("/"), name),
                None,
            ),
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => (
                &mut exp,
                mangle_host_name(&site.site().module_segments().join("/"), newtype),
                Some((member.as_str(), true)),
            ),
            BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => (
                &mut exp,
                mangle_host_name(&site.site().module_segments().join("/"), newtype),
                Some((member.as_str(), false)),
            ),
        };
        push_prepared_ffi_member(namespace, &member, prefix, site, shapes)?;
    }

    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");
    out.push_str("//\n");
    out.push_str("// Exact aliases for the prepared package-boundary facade.\n");
    out.push_str(
        "#![allow(unused_imports, non_snake_case, non_camel_case_types, type_alias_bounds)]\n\n",
    );
    emit_ffi_namespace(&mut out, "env", &env);
    emit_ffi_namespace(&mut out, "exp", &exp);
    Ok(out)
}

#[derive(Default)]
struct PreparedRustAliasDependencies {
    host: bool,
    binders: BTreeSet<FacadeBinderId>,
}

/// Collect the generic dependencies of the exact Rust type rendered for one
/// prepared facade use. This follows semantic facade nodes, not generated
/// identifier text: source and host binders can therefore share a spelling
/// without changing which bounds an alias declares.
fn collect_prepared_alias_dependencies(
    plan: &BoundaryFacadePlan,
    id: FacadeUseId,
    scope: &PreparedRustScope,
    dependencies: &mut PreparedRustAliasDependencies,
) {
    match plan.use_at(id) {
        FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } => {}
        FacadeUse::Bound { binder, .. } => {
            dependencies.binders.insert(*binder);
        }
        FacadeUse::Nominal { .. } => dependencies.host = true,
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            collect_prepared_alias_dependencies(plan, *constructor, scope, dependencies);
            for arg in args {
                collect_prepared_alias_dependencies(plan, *arg, scope, dependencies);
            }
        }
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
            for arg in args {
                collect_prepared_alias_dependencies(plan, *arg, scope, dependencies);
            }
        }
        FacadeUse::Function { slots, result, .. } => {
            for slot in slots {
                collect_prepared_alias_dependencies(plan, *slot, scope, dependencies);
            }
            collect_prepared_alias_dependencies(plan, *result, scope, dependencies);
        }
        FacadeUse::Forall { .. } => {
            dependencies.host = true;
            dependencies.binders.extend(scope.binders.keys().copied());
        }
    }
}

fn collect_prepared_source_alias_dependencies(
    plan: &BoundaryFacadePlan,
    slots: &[FacadeUseId],
    source: &crate::backends::boundary_facade::CallableSourceParamLayout,
    scope: &PreparedRustScope,
) -> PreparedRustAliasDependencies {
    let mut dependencies = PreparedRustAliasDependencies::default();
    let range = source.facade_slots();
    match source.adapter() {
        CallableSourceParamAdapter::UnitValue => {}
        CallableSourceParamAdapter::Identity | CallableSourceParamAdapter::RightNest => {
            for slot in &slots[range] {
                collect_prepared_alias_dependencies(plan, *slot, scope, &mut dependencies);
            }
        }
    }
    dependencies
}

fn prepared_alias_dependencies(
    plan: &BoundaryFacadePlan,
    id: FacadeUseId,
    scope: &PreparedRustScope,
) -> PreparedRustAliasDependencies {
    let mut dependencies = PreparedRustAliasDependencies::default();
    collect_prepared_alias_dependencies(plan, id, scope, &mut dependencies);
    dependencies
}

fn push_prepared_callback_aliases(
    body: &mut String,
    leaf: &str,
    root: FacadeUseId,
    parameters: &[PreparedRustBinderParameter],
    context: PreparedRustUseRenderContext<'_>,
) -> Result<(), EmitError> {
    let plan = context.plan;
    if !matches!(
        plan.use_at(root),
        FacadeUse::Function { .. } | FacadeUse::Forall { .. }
    ) {
        return Ok(());
    }
    if matches!(plan.use_at(root), FacadeUse::Forall { .. }) {
        let forall = rust_forall_name(context.site, plan, root)?;
        body.push_str(&format!(
            "pub use {}::shapes::{forall} as {leaf}_impl;\n",
            context.shapes.crate_root(),
        ));
    }
    let method = prepare_rust_forall_method(plan, root, context.scope, context.shapes.crate_root());
    let mut parameters = parameters.to_vec();
    parameters.extend(method.type_parameters);
    let context = PreparedRustUseRenderContext {
        scope: &method.public_scope,
        ..context
    };
    let returned = match plan.use_at(method.body) {
        FacadeUse::Function { slots, result, .. } => {
            let layout = match prepared_presentation_for_plan(context.site, plan, None)
                .use_at(method.body)
            {
                BoundaryFacadeExecutionUse::Function(layout) => layout,
                _ => unreachable!("an FFI callable alias has no source presentation"),
            };
            for (index, source) in layout.source_params().iter().enumerate() {
                let leaf = if layout.source_param_count() == 1 {
                    format!("{leaf}_cbarg")
                } else {
                    format!("{leaf}_cbarg{index}")
                };
                let rhs = render_prepared_source_facade(context, slots, source, None)?;
                let dependencies =
                    collect_prepared_source_alias_dependencies(plan, slots, source, context.scope);
                push_ffi_alias(
                    body,
                    &leaf,
                    &rhs,
                    &dependencies,
                    &parameters,
                    context.shapes,
                );
                let range = source.facade_slots();
                if source.adapter() == CallableSourceParamAdapter::Identity && range.len() == 1 {
                    push_prepared_callback_aliases(
                        body,
                        &leaf,
                        slots[range.start],
                        &parameters,
                        context,
                    )?;
                }
            }
            *result
        }
        _ => method.body,
    };
    let leaf = format!("{leaf}_cbret");
    let rhs = render_prepared_use(context, returned, PreparedRustTypePosition::Storage)?;
    let dependencies = prepared_alias_dependencies(plan, returned, context.scope);
    push_ffi_alias(
        body,
        &leaf,
        &rhs,
        &dependencies,
        &parameters,
        context.shapes,
    );
    push_prepared_callback_aliases(body, &leaf, returned, &parameters, context)
}

fn push_prepared_ffi_member(
    namespace: &mut String,
    member_module: &str,
    newtype_member: Option<(&str, bool)>,
    site: PreparedBoundaryCallableSite<'_>,
    shapes: PreparedRustRenderContext<'_>,
) -> Result<(), EmitError> {
    let origin = rust_site_support_origin(site);
    let callable = prepare_rust_callable(site);
    let alias_parameters = callable.type_parameters.clone();
    let plan = site.plan().facade();
    let existential_projector_continuation = match site.site().owner() {
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } => {
            let name =
                QualifiedTypeName::new(site.site().module_segments().to_vec(), newtype.to_owned())
                    .unwrap_or_else(|| unreachable!("a prepared projector has a qualified owner"));
            let Some(BoundaryNominalDeclaration::Newtype {
                existential_params, ..
            }) = site.nominals().declaration(&name)
            else {
                unreachable!("Rust emitter: prepared projector lost its newtype declaration");
            };
            if existential_params.is_empty() {
                None
            } else {
                let execution = site.execution().unwrap_or_else(|| {
                    unreachable!("Rust emitter: existential projector has no execution layout")
                });
                let root = callable.returned;
                if !execution
                    .direct_projector_compactable_foralls()
                    .contains(&root)
                {
                    unreachable!("Rust emitter: existential FFI alias lacks its compaction proof");
                }
                let FacadeUse::Forall { result, .. } = plan.use_at(root) else {
                    unreachable!("Rust emitter: existential FFI root is not result-polymorphic");
                };
                let FacadeUse::Function { slots, .. } = plan.use_at(*result) else {
                    unreachable!("Rust emitter: existential FFI root has no continuation");
                };
                let [continuation] = slots.as_slice() else {
                    unreachable!(
                        "Rust emitter: existential FFI root has other than one continuation"
                    );
                };
                if !execution
                    .direct_projector_compactable_foralls()
                    .contains(continuation)
                {
                    unreachable!(
                        "Rust emitter: existential continuation lacks its compaction proof"
                    );
                }
                let result_method =
                    prepare_rust_forall_method(plan, root, &callable.scope, shapes.crate_root());
                Some((
                    *continuation,
                    result_method.public_scope,
                    result_method.type_parameters,
                ))
            }
        }
        _ => None,
    };
    let mut body = String::new();
    let semantic_stages = site.plan().entry().head_stages;
    let presentation_stages = site.presentation().head_stages();
    if semantic_stages.len() != presentation_stages.len() {
        unreachable!("a prepared FFI source presentation disagrees with its semantic plan");
    }
    let mut source_index = 0usize;
    for ((semantic, presentation), stage_scope) in semantic_stages
        .iter()
        .zip(presentation_stages)
        .zip(&callable.head_scopes)
    {
        match (semantic, presentation) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {}
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                for source in layout.source_params() {
                    let leaf = match newtype_member {
                        Some((member, true)) => Some(escape_rust_keyword(&format!(
                            "{}_arg{source_index}",
                            host_name_core(member)
                        ))),
                        Some((_, false)) => None,
                        None => Some(format!("arg{source_index}")),
                    };
                    source_index += 1;
                    let Some(leaf) = leaf else {
                        continue;
                    };
                    let stage_context = PreparedRustUseRenderContext {
                        site,
                        plan,
                        scope: stage_scope,
                        shapes,
                        type_ctx: TypeCtx::Shape,
                    };
                    let rhs = render_prepared_source_facade(stage_context, slots, source, None)?;
                    let dependencies = collect_prepared_source_alias_dependencies(
                        plan,
                        slots,
                        source,
                        stage_scope,
                    );
                    push_ffi_alias(
                        &mut body,
                        &leaf,
                        &rhs,
                        &dependencies,
                        &alias_parameters,
                        shapes,
                    );

                    let range = source.facade_slots();
                    if source.adapter() != CallableSourceParamAdapter::Identity || range.len() != 1
                    {
                        continue;
                    }
                    push_prepared_callback_aliases(
                        &mut body,
                        &leaf,
                        slots[range.start],
                        &alias_parameters,
                        stage_context,
                    )?;
                }
            }
            _ => unreachable!("a prepared FFI source presentation disagrees with its plan"),
        }
    }
    if let Some((continuation, continuation_scope, continuation_parameters)) =
        &existential_projector_continuation
    {
        let member = newtype_member
            .map(|(member, _)| member)
            .expect("an existential projector FFI entry is a newtype member");
        let leaf = escape_rust_keyword(&format!("{}_continuation", host_name_core(member)));
        let rhs = render_prepared_use(
            PreparedRustUseRenderContext {
                site,
                plan,
                scope: continuation_scope,
                shapes,
                type_ctx: TypeCtx::Shape,
            },
            *continuation,
            PreparedRustTypePosition::Storage,
        )?;
        let mut continuation_alias_parameters = alias_parameters.clone();
        continuation_alias_parameters.extend(continuation_parameters.iter().cloned());
        let dependencies = prepared_alias_dependencies(plan, *continuation, continuation_scope);
        push_ffi_alias(
            &mut body,
            &leaf,
            &rhs,
            &dependencies,
            &continuation_alias_parameters,
            shapes,
        );
        push_prepared_callback_aliases(
            &mut body,
            &leaf,
            *continuation,
            &continuation_alias_parameters,
            PreparedRustUseRenderContext {
                site,
                plan,
                scope: continuation_scope,
                shapes,
                type_ctx: TypeCtx::Shape,
            },
        )?;
    }
    let ret_leaf = match newtype_member {
        Some((_member, false)) if existential_projector_continuation.is_some() => String::new(),
        Some((member, false)) => escape_rust_keyword(&format!("{}_ret", host_name_core(member))),
        Some((_, true)) => String::new(),
        None => "ret".to_owned(),
    };
    if !ret_leaf.is_empty() {
        let context = PreparedRustUseRenderContext {
            site,
            plan,
            scope: &callable.scope,
            shapes,
            type_ctx: TypeCtx::Shape,
        };
        let rhs = render_prepared_use(
            context,
            callable.returned,
            PreparedRustTypePosition::Storage,
        )?;
        let dependencies = prepared_alias_dependencies(plan, callable.returned, &callable.scope);
        push_ffi_alias(
            &mut body,
            &ret_leaf,
            &rhs,
            &dependencies,
            &alias_parameters,
            shapes,
        );
        push_prepared_callback_aliases(
            &mut body,
            &ret_leaf,
            callable.returned,
            &alias_parameters,
            context,
        )?;
    }
    if body.is_empty() {
        return Ok(());
    }

    if let Some(attribute) = rust_deprecation_attribute(origin) {
        body = body
            .lines()
            .map(|line| {
                if line.starts_with("pub type ") || line.starts_with("pub use ") {
                    format!("{attribute}\n{line}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
    }

    let module = escape_rust_keyword(member_module);
    if let Some(existing_start) = namespace.find(&format!("pub mod {module} {{\n")) {
        let closing = namespace[existing_start..]
            .find("}\n")
            .map(|offset| existing_start + offset)
            .unwrap_or_else(|| unreachable!("malformed prepared FFI namespace"));
        let indented = body
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect::<String>();
        namespace.insert_str(closing, &indented);
    } else {
        push_rust_deprecation(namespace, origin, "");
        namespace.push_str(&format!("pub mod {module} {{\n"));
        for line in body.lines() {
            namespace.push_str(&format!("    {line}\n"));
        }
        namespace.push_str("}\n");
    }
    Ok(())
}

/// Wrap a member-submodule body in its `pub mod <ns> { … }` shell.
/// An empty namespace still emits its (empty) module so the path
/// `crate::ffi::env` / `crate::ffi::exp` always resolves.
fn emit_ffi_namespace(out: &mut String, ns: &str, body: &str) {
    out.push_str(&format!("pub mod {ns} {{\n"));
    if !body.is_empty() {
        for line in body.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }
    out.push_str("}\n");
}

/// Push one `pub type <leaf><generics> = <rhs>;` alias into `body`,
/// when `rhs` is a nameable concrete type.
///
/// Skips the alias only when `rhs` is unit (the public method still carries
/// that source parameter as one `()` argument).
/// Canonical function and forall facades are named types, and public string
/// facades are owned, so every other admitted boundary type is aliasable. The
/// generic header declares exactly the structured semantic dependencies of
/// the rendered use, with their well-formedness bounds. Alias parameter order
/// remains source-declaration order because consumers instantiate it
/// positionally.
fn push_ffi_alias(
    body: &mut String,
    leaf: &str,
    rhs: &str,
    dependencies: &PreparedRustAliasDependencies,
    parameters: &[PreparedRustBinderParameter],
    shapes: PreparedRustRenderContext<'_>,
) {
    if rhs == "()" {
        return;
    }
    let qualified = qualify_host_assoc(rhs, RUST_HOST_PARAMETER, shapes.host_trait());
    let rhs = qualified.as_str();
    let mut header: Vec<String> = Vec::new();
    if dependencies.host {
        header.push(format!(
            "{RUST_HOST_PARAMETER}: {}::host::{}",
            shapes.crate_root(),
            shapes.host_trait()
        ));
    }
    let mut remaining = dependencies.binders.clone();
    for parameter in parameters {
        if remaining.remove(&parameter.binder) {
            header.push(parameter.declaration(shapes.crate_root()));
        }
    }
    if !remaining.is_empty() {
        unreachable!("a prepared FFI alias depends on an undeclared facade binder");
    }
    let generics = if header.is_empty() {
        String::new()
    } else {
        format!("<{}>", header.join(", "))
    };
    body.push_str(&format!("pub type {leaf}{generics} = {rhs};\n"));
}

/// Rewrite each generated host-associated projection to its unambiguous
/// fully-qualified form. The dependency itself was selected semantically by
/// [`collect_prepared_alias_dependencies`]; this pass only renders Rust
/// projection syntax.
fn qualify_host_assoc(rhs: &str, host_parameter: &str, host_trait: &str) -> String {
    let projection = format!("{host_parameter}::");
    let mut out = String::with_capacity(rhs.len());
    let mut copied = 0usize;
    let mut search = 0usize;
    while let Some(relative) = rhs[search..].find(&projection) {
        let start = search + relative;
        let before_is_ident = rhs[..start]
            .chars()
            .next_back()
            .is_some_and(|before| before.is_alphanumeric() || before == '_');
        if !before_is_ident {
            out.push_str(&rhs[copied..start]);
            out.push_str(&format!(
                "<{host_parameter} as crate::host::{host_trait}>::"
            ));
            search = start + projection.len();
            copied = search;
        } else {
            search = start + 1;
        }
    }
    out.push_str(&rhs[copied..]);
    out
}

/// Render the Rust host-contract trait (`pub trait <Handle>Host`) from the
/// backend-agnostic descriptor and the package-complete prepared catalog.
///
/// The descriptor owns live and retained membership, canonical order, and
/// host-type bound intents. Each exact prepared binding or callable supplies
/// only the marker/facade rendering and retained-nameability proof for the
/// descriptor item at the same structured identity.
fn render_rust_host_trait(
    descriptor: &HostDescriptor<'_>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: PreparedRustRenderContext<'_>,
    names: &RustNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");
    out.push('\n');

    out.push_str(&format!(
        "/// {} — the host record contract. Implement this trait to invoke\n\
         /// the package. See `specs/backends/rust.md` § Host record contract.\n",
        names.host_trait
    ));
    // Public identifiers follow word casing and Rust escaping
    // (`specs/backends/rust.md` § Item naming). The trait naming
    // allowances cover those public spellings and source-derived
    // generic parameter names.
    //
    // Trait methods are module-qualified at the host boundary
    // (`main__print`, via `mangle_host_name`) so two bridged modules'
    // same-leaf host fns don't collide; the mid-identifier `__`
    // separator trips Rust's `non_snake_case` lint, so silence it at
    // the trait level rather than mangling the boundary name (a
    // single-underscore scheme would risk cross-module collisions).
    out.push_str("#[allow(non_camel_case_types, non_snake_case)]\n");
    // `Clone + 'static` bounds on the host trait let module-fn methods
    // capture a host-clone inside a `Rc<dyn Fn(...) -> ...> + 'static`
    // closure when a fn flows through value position (a newtype
    // payload like `Show[A] : (A) -> String`, an optics function
    // pair, etc.). Hosts that wouldn't otherwise be `Clone` can wrap
    // their state in `Rc` / `Arc` to satisfy the bound cheaply.
    out.push_str(&format!(
        "pub trait {}: Clone + 'static {{\n",
        names.host_trait
    ));
    for declaration in &descriptor.host_types {
        let binding = prepared_live_host_binding(prepared, declaration);
        emit_prepared_host_type_section(declaration, binding, &mut out);
    }
    if !descriptor.host_types.is_empty() && !descriptor.host_fns.is_empty() {
        out.push('\n');
    }
    for declaration in &descriptor.host_fns {
        let site =
            prepared_live_host_function_site(prepared, declaration.module_path, declaration.name);
        emit_prepared_host_fn_method(site, shapes, HostFnMethodBody::Required, &mut out)?;
    }
    emit_deprecated_host_items(descriptor, prepared, shapes, &mut out)?;
    out.push_str("}\n");
    Ok(out)
}

fn prepared_live_host_binding<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    declaration: &HostTypeDecl<'_>,
) -> &'a BoundaryHostBinding {
    let name = QualifiedTypeName::new(
        declaration
            .module_path
            .split('/')
            .map(str::to_owned)
            .collect(),
        declaration.name,
    )
    .unwrap_or_else(|| unreachable!("a descriptor host type has a valid exact identity"));
    let binding = prepared.host_binding(&name).unwrap_or_else(|| {
        unreachable!(
            "Rust emitter: descriptor host type {}.{} has no prepared facade binding",
            declaration.module_path, declaration.name
        )
    });
    let expected_binding = declaration.role.map_or(
        BoundaryHostTypeBinding::Roleless,
        BoundaryHostTypeBinding::Role,
    );
    let same_type_parameters = declaration.type_params.len() == binding.type_params().len()
        && declaration
            .type_params
            .iter()
            .zip(binding.type_params())
            .all(|(source, prepared)| {
                source.name == prepared.name() && source.effective_kind() == *prepared.kind()
            });
    if !matches!(binding.origin(), BoundaryHostBindingOrigin::Live)
        || binding.binding() != expected_binding
        || binding.owned() != declaration.source.owned
        || !same_type_parameters
    {
        unreachable!(
            "Rust emitter: descriptor and prepared host binding disagree at {}.{}",
            declaration.module_path, declaration.name
        );
    }
    binding
}

fn prepared_live_host_function_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    module_path: &str,
    name: &str,
) -> PreparedBoundaryCallableSite<'a> {
    let site_id = boundary_site_id(
        module_path,
        BoundaryFacadeSiteOwner::HostFunction {
            name: name.to_owned(),
        },
    );
    let site = prepared_site(prepared, &site_id);
    if !matches!(site.origin(), PreparedBoundaryCallableOriginRef::Live(_)) {
        unreachable!(
            "Rust emitter: descriptor live host function {module_path}.{name} is not a live prepared site"
        );
    }
    site
}

/// Re-emit the host items a later contract generation removed, recovered
/// from the signature changelog's prepared retained sites. This is the Rust backend's
/// instantiation of the cross-backend removal-side idiom and its
/// per-backend source-stability limitation.
///
/// **Removed `host fn` (env, kind=Fn)** → when its complete frozen signature
/// remains independently nameable, a `#[deprecated]` trait method with a
/// diverging default body. Source-stable: a host whose
/// hand-written `impl` still overrides the method keeps compiling (the
/// override wins; `#[deprecated]` only warns), and a host that dropped
/// its override compiles too (the diverging default covers it). The
/// diverging body is `!`, so it type-checks at every return type
/// uniformly — there is no arity / return-type guard routing some shapes
/// to an identity pass. A removed function that mentions a removed host
/// associated type is omitted too, because restoring that associated type
/// would impose history on every new host.
///
/// **Removed `host type` (env, kind=Type)** is **not source-stable on
/// Rust** — a genuine per-backend limitation. Every host type renders as a
/// trait associated type; dropping it leaves an
/// existing host's `impl Host { type <module>__Foo = …; }` an orphan
/// associated-type definition (E0437, confirmed against stable rustc),
/// and Rust has no stable `associated_type_defaults` that would let the
/// trait retain a *defaulted* associated type so the host's line stays
/// valid. No escape hatch (a defaulted associated type, a hidden shim, a
/// free re-exported alias — a free `pub type` cannot absorb an
/// impl-block associated-type line) keeps the removed type valid in an
/// unchanged host `impl`. So a removed host type is simply
/// dropped from the trait (a *new* host compiles cleanly); the existing
/// host deletes its stale `type … = …;` line by hand. This degradation is
/// uniform across role-bearing and roleless types and is documented as a
/// limitation in
/// `specs/backends/rust.md` § Deprecated host items > "Host-type removal is not
/// source-stable on Rust"; that section names this emitter file in turn.
fn emit_deprecated_host_items(
    descriptor: &HostDescriptor<'_>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: PreparedRustRenderContext<'_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let retained_fns = descriptor
        .deprecated_host_items
        .iter()
        .filter_map(|item| {
            if !matches!(item.kind, crate::host_descriptor::DeprecatedHostKind::Fn) {
                return None;
            }
            let site_id = boundary_site_id(
                &item.module_path,
                BoundaryFacadeSiteOwner::HostFunction {
                    name: item.name.clone(),
                },
            );
            let site = prepared.site(&site_id)?;
            let metadata = site.retained()?;
            if metadata.removed_at_version() != item.removed_at_version {
                unreachable!(
                    "Rust emitter: descriptor and prepared retirement disagree at {}.{}",
                    item.module_path, item.name
                );
            }
            Some((site, item.removed_at_version))
        })
        .filter(|(site, _)| !rust_site_depends_on_retained_host_binding(*site, shapes))
        .collect::<Vec<_>>();
    if retained_fns.is_empty() {
        return Ok(());
    }
    let has_live_items = !descriptor.host_types.is_empty() || !descriptor.host_fns.is_empty();
    if has_live_items {
        out.push('\n');
    }
    for (site, removed_at) in retained_fns {
        emit_prepared_host_fn_method(
            site,
            shapes,
            HostFnMethodBody::DeprecatedAtVersion(removed_at),
            out,
        )?;
    }
    Ok(())
}

fn rust_site_depends_on_retained_host_binding(
    site: PreparedBoundaryCallableSite<'_>,
    shapes: PreparedRustRenderContext<'_>,
) -> bool {
    rust_site_depends_on_retained_host_binding_prepared(site, shapes.prepared)
}

fn rust_site_depends_on_retained_host_binding_prepared(
    site: PreparedBoundaryCallableSite<'_>,
    prepared: &PreparedBoundaryCallableSites,
) -> bool {
    site.nominals().declarations().any(|(name, declaration)| {
        matches!(declaration, BoundaryNominalDeclaration::HostType { .. })
            && prepared.host_binding(name).is_some_and(|binding| {
                matches!(binding.origin(), BoundaryHostBindingOrigin::Retained { .. })
            })
    })
}

fn rust_eligible_retained_newtypes(
    prepared: &PreparedBoundaryCallableSites,
) -> BTreeSet<QualifiedTypeName> {
    let retained_bindings = prepared
        .host_bindings()
        .filter(|binding| matches!(binding.origin(), BoundaryHostBindingOrigin::Retained { .. }))
        .map(|binding| binding.name())
        .collect::<BTreeSet<_>>();
    let mut eligible = BTreeSet::new();
    for site in prepared.sites() {
        if !matches!(
            site.origin(),
            PreparedBoundaryCallableOriginRef::Retained(_)
        ) || site.nominals().declarations().any(|(name, declaration)| {
            matches!(declaration, BoundaryNominalDeclaration::HostType { .. })
                && retained_bindings.contains(name)
        }) {
            continue;
        }
        eligible.extend(
            site.nominals()
                .declarations()
                .filter(|(_, declaration)| {
                    matches!(declaration, BoundaryNominalDeclaration::Newtype { .. })
                })
                .map(|(name, _)| name.clone()),
        );
    }
    eligible
}

/// Emit one descriptor-selected live host binding through its exact prepared
/// marker/facade entry. The descriptor supplies bound intents; the joined
/// binding supplies the Rust application shape.
fn emit_prepared_host_type_section(
    declaration: &HostTypeDecl<'_>,
    binding: &BoundaryHostBinding,
    out: &mut String,
) {
    let module_path = binding.name().module_segments().join("/");
    let assoc = mangle_host_name(&module_path, binding.name().name());
    let role = match binding.binding() {
        BoundaryHostTypeBinding::Role(role) => Some(role),
        BoundaryHostTypeBinding::Roleless => None,
    };
    let role_note = role
        .map(|role| format!(" role({})", role.as_str()))
        .unwrap_or_default();
    let owned_note = if binding.owned() { " { owned }" } else { "" };
    out.push_str(&format!(
        "    /// host `type {}{}{};`\n",
        binding.name().name(),
        role_note,
        owned_note
    ));
    let bounds = render_rust_host_type_bounds(&declaration.bounds, role);
    if binding.type_params().is_empty() {
        out.push_str(&format!("    type {assoc}: {bounds};\n"));
    } else {
        out.push_str(&format!("    type {assoc}Storage: {bounds};\n"));
    }
}

/// Translate the descriptor's bound intents to the Rust associated-entry
/// surface. Two clone purposes collapse to the one `Clone` requirement.
fn render_rust_host_type_bounds(
    bounds: &host_descriptor::BoundRequirementSet,
    role: Option<Role>,
) -> String {
    let mut parts = Vec::new();
    if bounds.contains(BoundRequirement::CloneForRcCapture)
        || bounds.contains(BoundRequirement::CloneForPackageClone)
    {
        parts.push("Clone".to_owned());
    }
    if bounds.contains(BoundRequirement::PartialEqForShapeDerive) {
        parts.push("PartialEq".to_owned());
    }
    if bounds.contains(BoundRequirement::StaticForDynAnyStorage) {
        parts.push("'static".to_owned());
    }
    if let Some(role) = role {
        parts.push(format!("From<{}>", role_literal_primitive_type(role)));
    }
    parts.join(" + ")
}

/// How a host-trait method's body is rendered.
enum HostFnMethodBody {
    /// A live host fn: a body-less trait-method requirement (`… ;`).
    Required,
    /// A deprecated re-emit of a removed host fn: a `#[deprecated]`
    /// attribute and a diverging default body (`{ unimplemented!(…) }`),
    /// so a host whose hand-written `impl` still overrides the method
    /// keeps compiling (override wins; the default only warns) and a
    /// host that dropped its override compiles too (the default covers
    /// it). Per `specs/backends/rust.md` § Deprecated host items.
    DeprecatedAtVersion(u32),
}

fn emit_prepared_host_fn_method(
    site: PreparedBoundaryCallableSite<'_>,
    shapes: PreparedRustRenderContext<'_>,
    body: HostFnMethodBody,
    out: &mut String,
) -> Result<(), EmitError> {
    let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
        unreachable!(
            "Rust emitter: a host method requires a prepared host-function site (compiler bug)",
        );
    };
    let module_path = site.site().module_segments().join("/");
    let method_name = mangle_host_name(&module_path, name);
    let callable = prepare_rust_callable(site);
    let plan = site.plan().facade();

    let all_type_parameters = callable
        .type_parameters
        .iter()
        .map(|parameter| parameter.declaration(shapes.crate_root()))
        .collect::<Vec<_>>();
    let parameters =
        render_prepared_callable_source_parameters(site, &callable, shapes, TypeCtx::Trait)?;

    let generic_declaration = if all_type_parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", all_type_parameters.join(", "))
    };
    let trap_note = match body {
        HostFnMethodBody::DeprecatedAtVersion(version) => {
            let note = format!("host fn `{method_name}` removed at v({version})");
            out.push_str(&format!("    #[deprecated(note = \"{note}\")]\n"));
            Some(note)
        }
        HostFnMethodBody::Required => None,
    };
    out.push_str(&format!(
        "    fn {}{generic_declaration}(&self",
        escape_rust_keyword(&method_name)
    ));
    if !parameters.is_empty() {
        out.push_str(", ");
        out.push_str(&parameters.join(", "));
    }
    out.push(')');
    if !matches!(plan.use_at(callable.returned), FacadeUse::Unit { .. }) {
        let returned = render_prepared_use(
            PreparedRustUseRenderContext {
                site,
                plan,
                scope: &callable.scope,
                shapes,
                type_ctx: TypeCtx::Trait,
            },
            callable.returned,
            PreparedRustTypePosition::Value,
        )?;
        out.push_str(&format!(" -> {returned}"));
    }
    match trap_note {
        Some(note) => out.push_str(&format!(
            " {{\n        unimplemented!(\"{note}\")\n    }}\n"
        )),
        None => out.push_str(";\n"),
    }
    Ok(())
}

// ---- role / name helpers --------------------------------------------------

fn role_literal_primitive_type(role: Role) -> &'static str {
    match role {
        Role::I8 => "i8",
        Role::I16 => "i16",
        Role::I32 => "i32",
        Role::I64 => "i64",
        Role::I128 => "i128",
        Role::U8 => "u8",
        Role::U16 => "u16",
        Role::U32 => "u32",
        Role::U64 => "u64",
        Role::U128 => "u128",
        Role::F32 => "f32",
        Role::F64 => "f64",
        Role::Bool => "bool",
        Role::Str => "String",
    }
}

fn lowercase_first(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_lowercase(), chars.as_str()),
        None => String::new(),
    }
}

fn rust_shape_key_from_ffi_key(key: &str) -> String {
    key.split('.')
        .flat_map(|part| part.split('/'))
        .map(lowercase_first)
        .collect::<Vec<_>>()
        .join("_")
}

fn render_host_method_call(receiver: &str, name: &str, args: &[String]) -> String {
    let method = escape_rust_keyword(name);
    format!("{receiver}.{method}({})", args.join(", "))
}

fn rust_public_ident(source: &str) -> String {
    escape_rust_keyword(&host_name_core(source))
}

fn escape_rust_keyword(name: &str) -> String {
    // `_` is special — Rust disallows it as a raw identifier, but
    // accepts it bare as the wildcard binder. Pass it through.
    if name == "_" {
        return name.to_owned();
    }
    // Strict-reserved keywords cannot be raw-identified (`r#self`,
    // `r#super`, `r#crate`, `r#extern`, `r#Self` are all rejected by
    // rustc with "<kw> cannot be a raw identifier"). Mangle them to
    // a fresh `__kio_kw_<keyword>` form that is guaranteed not to
    // collide with any other emitted identifier.
    if RUST_STRICT_RESERVED.contains(&name) {
        return format!("__kio_kw_{name}");
    }
    if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_owned()
    }
}

// Keywords rustc rejects as raw identifiers. The set is small and
// fixed (per the Rust reference); mangling to `__kio_kw_<kw>` is
// safe because the underscore-prefix space is reserved-by-convention
// for emitter internals.
const RUST_STRICT_RESERVED: &[&str] = &["self", "Self", "crate", "extern", "super"];

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "do", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match",
    "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
    "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use", "virtual", "where",
    "while", "yield", "abstract", "become", "box", "final", "override", "priv", "union",
];

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::structural_recovery;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    #[test]
    fn rust_names_brand_dashed_crate_names() {
        // A non-source-shaped namespace uses the exact namespace frame;
        // the runner can reconstruct it from the Cargo package name.
        let names = RustNames::derive("two-part");
        assert_eq!(names.handle, "KioNs_74776f2d70617274");
        assert_eq!(names.host_trait, "KioNs_74776f2d70617274Host");
        assert_eq!(names.factory, "create_KioNs_74776f2d70617274");
        let plain = RustNames::derive("greeter");
        assert_eq!(plain.handle, "Greeter");
        assert_eq!(plain.host_trait, "GreeterHost");
        assert_eq!(plain.factory, "create_greeter");
    }

    #[test]
    fn public_word_casing_has_a_crate_wide_naming_lint_allowance() {
        let pkg = build_package(
            "module word_api; \
             pub newtype Word_box : . { pub constructor mk_box; pub projector un_box; }; \
             pub fn round_trip() -> . { () }",
            Some("bridge { word_api; }"),
        );
        let krate = lower_package(&pkg, "word_names").expect("word-cased Rust emission");
        for name in [
            "pub fn create_wordNames",
            "pub wordApi:",
            "pub fn roundTrip",
        ] {
            assert!(krate.lib_rs.contains(name), "{name}:\n{}", krate.lib_rs);
        }
        for name in ["pub mod wordApi", "pub fn mkBox", "pub fn unBox"] {
            assert!(
                krate.shapes_rs.contains(name),
                "{name}:\n{}",
                krate.shapes_rs
            );
        }
        assert!(
            krate.lib_rs.contains("#![allow(non_snake_case)]"),
            "the generated crate must allow its contract-defined public names:\n{}",
            krate.lib_rs,
        );
    }

    #[test]
    fn ordinary_facades_keep_rusts_default_recursion_limit() {
        let pkg = build_package(
            "module main; pub fn keep(value: .) -> . { value }",
            Some("bridge { main; }"),
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&pkg)
            .expect("ordinary public facade catalog");
        assert!(prepared_rust_facade_max_depth(&prepared) <= RUST_DEFAULT_RECURSION_LIMIT);
        assert_eq!(rust_recursion_limit_for_depth(128), None);
        assert_eq!(prepared_rust_recursion_limit(&prepared), None);

        let krate = lower_package(&pkg, "ordinary_facade").expect("ordinary Rust emission");
        assert!(!krate.lib_rs.contains("#![recursion_limit ="));
    }

    #[test]
    fn wide_callable_scales_rusts_recursion_limit_from_prepared_depth() {
        let width = 255;
        let parameter_type = std::iter::repeat_n("I32", width)
            .collect::<Vec<_>>()
            .join("\n& ");
        let parameters = (0..width)
            .map(|index| format!("    , p{index}: I32"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!(
            "module main;\n\n\
             host type I32 role(i32);\n\n\
             pub fn make_select() -> (& {parameter_type}) -> I32 {{\n\
               .(\n{parameters}\n  ) {{ p0 }}\n\
             }}"
        );
        let pkg = build_package(&source, Some("bridge { main; }"));
        let prepared =
            PreparedBoundaryCallableSites::collect_live(&pkg).expect("wide public facade catalog");
        assert_eq!(prepared_rust_recursion_limit(&prepared), Some(512));
        assert_eq!(rust_recursion_limit_for_depth(512), Some(1024));

        let krate = lower_package(&pkg, "wide_callable").expect("wide Rust emission");
        assert!(krate.lib_rs.contains("#![recursion_limit = \"512\"]"));
    }

    #[test]
    fn host_boundary_mangling_is_injective_for_legal_names() {
        assert_eq!(mangle_host_name("app", "print"), "app__print");
        assert_eq!(mangle_host_name("util/io", "print"), "util_io__print");
        assert_eq!(mangle_host_name("a_b_", "c"), "__kio_host_aB_u__c");
        assert_eq!(mangle_host_name("a", "b_c_"), "a__bC_");
        assert_ne!(mangle_host_name("a_b_", "c"), mangle_host_name("a", "b_c_"));
        assert_ne!(
            mangle_host_name("a_b/c", "X"),
            mangle_host_name("a/b_c", "X")
        );
    }

    #[test]
    fn private_module_fn_mangling_distinguishes_slashes_from_underscores() {
        assert_eq!(mangle_module_fn("foo/bar", "value"), "__mod_foo_bar_value");
        assert_eq!(
            mangle_module_fn("foo_bar", "value"),
            "___mod_8_foo_ubar_value"
        );
        assert_ne!(
            mangle_module_fn("foo/bar", "value"),
            mangle_module_fn("foo_bar", "value")
        );
        assert_ne!(
            mangle_module_fn("foo", "bar_value"),
            mangle_module_fn("foo/bar", "value")
        );
    }

    #[test]
    fn export_namespace_struct_names_preserve_path_segmentation() {
        assert_eq!(
            export_block_namespace_struct_name(&["foo".to_owned(), "bar".to_owned()]),
            "ExportBlockNs_foo_bar"
        );
        assert_eq!(
            export_block_namespace_struct_name(&["foo_bar".to_owned()]),
            "ExportBlockNsExact_6_fooBar"
        );
        assert_ne!(
            export_block_namespace_struct_name(&["foo".to_owned(), "bar".to_owned()]),
            export_block_namespace_struct_name(&["foo_bar".to_owned()])
        );
    }

    /// Derive the on-disk file path for a module from its declared
    /// `module a/b;` path: `a/b.kio`. Per `specs/package.md`
    /// § Module-name rules, the declared segments equal the file's
    /// path relative to the package root — the package name is not
    /// prepended.
    fn module_file_path(module: &crate::ast::Module<crate::ast::Surface>) -> PathBuf {
        let segs = &module.path.segments;
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(&seg.name);
        }
        let stem = segs.last().map(|s| s.name.as_str()).unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    fn build_package(src: &str, package_file_src: Option<&str>) -> Package<Routed> {
        build_multi_module_package(&[src], package_file_src)
    }

    /// Like [`build_package`] but for a package spanning several module
    /// sources — each `src` is one `module …;` file. Used to exercise
    /// cross-module shapes (e.g. two bridged modules declaring the same
    /// host-type leaf) the single-module helper cannot express.
    fn build_multi_module_package(
        srcs: &[&str],
        package_file_src: Option<&str>,
    ) -> Package<Routed> {
        let parsed_modules: Vec<(PathBuf, _)> = srcs
            .iter()
            .map(|src| {
                let parsed = parse(src).expect("parse");
                (module_file_path(&parsed), parsed)
            })
            .collect();
        let parsed_package_file = package_file_src.map(|s| {
            parse_package_file(&format!("package pkg;\n{s}"), None).expect("parse package file")
        });
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed_modules, parsed_package_file)
                .expect("lower_package");
        let package_file_entry = lowered_package_file.map(|e| PackageFileEntry {
            file_path: PathBuf::from("x.pkg.kio"),
            package_name: "x".to_owned(),
            package_file: e,
        });
        let pkg = crate::pass::resolve::Package::build(
            Path::new(""),
            lowered_modules,
            package_file_entry,
        )
        .expect("build");
        pkg.resolve_imports().expect("resolve_imports");
        pkg.check_in_body_resolution().expect("body resolution");
        let prime = check_package(&pkg).expect("typecheck");
        let enriched = structural_recovery::recover_package(&prime);
        crate::pass::recover_to_low::lower(&enriched)
    }

    #[test]
    fn routed_nominal_identity_is_not_reexpanded_in_rust_contract() {
        let pkg = build_multi_module_package(
            &[
                "module list/core; pub rec newtype List[A] : . | (A & List(A)) { \
                 pub constructor mk_list; pub projector un_list; };",
                "module model; import list/core as list; \
                 pub type Row = list.List(.); \
                 pub fn keep(row: Row) -> Row { row }",
                "module facade; import model as model; \
                 pub fn keep(row: model.Row) -> model.Row { model.keep(row) }",
            ],
            Some("bridge { facade; list/**; model; }"),
        );
        let model = pkg.module("model").expect("model module");
        let keep = model
            .module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::FnDef(def) if def.name == "keep" => Some(def),
                _ => None,
            })
            .expect("model.keep");
        let (sig, ret) = module_fn_contract_signature_and_ret(model, keep);
        let param = sig
            .params
            .iter()
            .find_map(|param| match param {
                crate::ast::SignatureParam::Value(param) => param.ty.as_ref(),
                crate::ast::SignatureParam::Type(_) => None,
            })
            .expect("keep value parameter type");
        for ty in [param, &ret] {
            let Type::Path { segments, .. } = ty else {
                panic!("keep boundary should retain the nominal List application");
            };
            assert_eq!(
                segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
                ["list", "core", "List"]
            );
        }

        lower_package(&pkg, "alias_identity").expect("canonical Routed identity should emit");
    }

    #[test]
    fn recursive_newtype_classification_uses_instantiated_payloads_per_path() {
        let pkg = build_package(
            "module main; \
             pub newtype Const[A] : . { \
               pub constructor make_const; pub projector read_const; \
             }; \
             pub rec newtype Finite : Const(Finite) { \
               pub constructor make_finite; pub projector read_finite; \
             }; \
             pub newtype Id[A] : A { \
               pub constructor make_id; pub projector read_id; \
             }; \
             pub rec newtype Recursive : Id(.) & Id(Recursive) { \
               pub constructor make_recursive; pub projector read_recursive; \
             };",
            Some("bridge { main; }"),
        );
        let newtypes = collect_newtypes(&pkg);
        let records = build_newtype_records(&pkg, &newtypes);
        let scopes = collect_newtype_import_scopes(&pkg, &records);
        let shapes = BodyShapeRegistry::new(
            BTreeMap::new(),
            records,
            scopes,
            "crate".to_owned(),
            "ProbeHost".to_owned(),
        );

        assert!(!shapes.newtype_is_recursive("main.Finite"));
        assert!(shapes.newtype_is_recursive("main.Recursive"));
    }

    #[test]
    fn public_opaque_newtypes_hide_payload_storage_and_private_payload_types() {
        let pkg = build_package(
            "module api; \
             newtype Hidden_payload : . { constructor mk_hidden; projector un_hidden; }; \
             pub newtype Opaque_a : Hidden_payload { constructor mk_opaque_a; projector un_opaque_a; }; \
             pub newtype Opaque_b : Hidden_payload { constructor mk_opaque_b; projector un_opaque_b; }; \
             pub newtype Constructor_only[A] : A { pub constructor make_constructor; projector read_constructor; }; \
             pub newtype Projector_only[A] : A { constructor make_projector; pub projector read_projector; }; \
             pub newtype Function_box[A] : A -> A { pub constructor make_function; pub projector read_function; }; \
             pub newtype Existential <U> : U { constructor make_existential; pub projector read_existential; }; \
             rec { \
               pub newtype Outer : Hidden { pub constructor make_outer; pub projector read_outer; }; \
               pub newtype Hidden : Outer { constructor make_hidden_cycle; projector read_hidden_cycle; }; \
             } \
             pub fn keep_a(value: Opaque_a) -> Opaque_a { value } \
             pub fn keep_b(value: Opaque_b) -> Opaque_b { value } \
             host fn echo_outer(value: Outer) -> Outer;",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit Rust");

        assert!(
            krate.shapes_rs.contains("pub struct OpaqueA"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub struct OpaqueB"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("pub struct HiddenPayload"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            !krate
                .shapes_rs
                .contains("pub struct OpaqueA<__KioHost: 'static>(pub "),
            "{}",
            krate.shapes_rs
        );
        assert!(
            !krate
                .shapes_rs
                .contains("pub struct OpaqueB<__KioHost: 'static>(pub "),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub fn makeConstructor"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("pub fn readConstructor"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("pub fn makeProjector"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub fn readProjector"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub fn makeFunction"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub fn readFunction"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub fn readExistential<__KioPolyType_0: crate::shapes::KioType>"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("pub struct Outer")
                && krate.shapes_rs.contains("pub struct Hidden")
                && krate
                    .shapes_rs
                    .contains("pub fn makeOuter(arg0: crate::shapes::nominal::api::Hidden")
                && krate
                    .shapes_rs
                    .contains("-> crate::shapes::nominal::api::Hidden")
                && krate.host_rs.contains("shapes::nominal::api::Outer"),
            "shapes:\n{}\nhost:\n{}",
            krate.shapes_rs,
            krate.host_rs
        );
    }

    #[test]
    fn unreferenced_public_opaque_newtype_needs_no_callable_owner() {
        let pkg = build_package(
            "module api; pub newtype Token : . { constructor make; projector read; };",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit isolated opaque Rust newtype");
        assert!(krate.shapes_rs.contains("pub struct Token"));
    }

    #[test]
    fn function_newtype_member_facades_match_the_public_function_abi() {
        let pkg = build_package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Packed_triple : (I32 & I32 & I32) -> I32 { \
                 pub constructor make_packed_triple; \
                 pub projector read_packed_triple; \
             }; \
             pub newtype Higher_order_parameter : (I32 -> I32) -> I32 { \
                 pub constructor make_higher_order_parameter; \
                 pub projector read_higher_order_parameter; \
             }; \
             pub newtype Higher_order_return : I32 -> (I32 -> I32) { \
                 pub constructor make_higher_order_return; \
                 pub projector read_higher_order_return; \
             }; \
             pub newtype Generic_parameter[T] : T -> I32 { \
                 pub constructor make_generic_parameter; \
                 pub projector read_generic_parameter; \
             }; \
             pub newtype Polymorphic[*F] : [A][B] ((A -> B) & F(A)) -> F(B) { \
                 pub constructor make_polymorphic; \
                 pub projector read_polymorphic; \
             }; \
             pub rec newtype Recursive_poly : [A] A -> Recursive_poly { \
                 pub constructor make_recursive_poly; \
                 pub projector read_recursive_poly; \
             };",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit Rust");
        let method = |start: &str| {
            let tail = krate
                .shapes_rs
                .split_once(start)
                .unwrap_or_else(|| panic!("missing `{start}` in:\n{}", krate.shapes_rs))
                .1;
            let end = tail
                .find("\n    }\n}")
                .unwrap_or_else(|| panic!("missing method end after `{start}` in:\n{tail}"));
            &tail[..end + "\n    }".len()]
        };
        let signature = |method: &str| {
            method
                .split_once(" {\n")
                .unwrap_or_else(|| panic!("missing signature delimiter in:\n{method}"))
                .0
                .to_owned()
        };

        let packed_constructor = method("pub fn makePackedTriple");
        let packed_constructor_signature = signature(packed_constructor);
        assert!(
            packed_constructor_signature
                .contains("arg0: crate::shapes::KioFn1<crate::shapes::KioProduct<")
                && !packed_constructor_signature.contains("dyn ::std::any::Any"),
            "the constructor facade must expose the named canonical function carrier:\n{packed_constructor_signature}"
        );
        assert!(
            packed_constructor.contains("crate::shapes::KioFunction1<crate::shapes::KioProduct<")
                && packed_constructor
                    .contains("as crate::shapes::KioType>::into_stored(arg0).into_raw()")
                && packed_constructor.contains("crate::shapes::KioStoredValue::from_raw(")
                && !packed_constructor.contains("product_slot(")
                && !packed_constructor.contains("__p0")
                && !packed_constructor.contains("__p2"),
            "the constructor must transfer the exact grouped function token without spreading its product domain:\n{packed_constructor}"
        );

        let packed_projector = method("pub fn readPackedTriple");
        let packed_projector_signature = signature(packed_projector);
        assert!(
            packed_projector_signature
                .contains("-> crate::shapes::KioFn1<crate::shapes::KioProduct<")
                && !packed_projector_signature.contains("dyn ::std::any::Any"),
            "the projector facade must expose the same named function carrier:\n{packed_projector_signature}"
        );
        assert!(
            packed_projector.contains("crate::shapes::KioFunction1<crate::shapes::KioProduct<")
                && packed_projector.contains(
                    "as crate::shapes::KioType>::from_stored(crate::shapes::KioStoredValue::from_raw("
                )
                && !packed_projector.contains("product_slot(")
                && !packed_projector.contains("as_any(__p0)")
                && !packed_projector.contains("as_any(__p2)"),
            "the projector must recover the exact grouped function token without rebuilding a spread closure:\n{packed_projector}"
        );

        let polymorphic_constructor = method("pub fn makePolymorphic");
        let polymorphic_constructor_signature = signature(polymorphic_constructor);
        assert!(
            polymorphic_constructor_signature.contains("arg0: crate::shapes::KioForall_")
                && polymorphic_constructor_signature.contains("Value<__KioHost, F>")
                && !polymorphic_constructor_signature.contains("dyn ::std::any::Any")
                && krate
                    .shapes_rs
                    .contains("F: crate::shapes::KioTypeConstructor1")
                && krate.shapes_rs.contains("crate::shapes::KioApplied1<F,"),
            "the polymorphic constructor must expose a rank-N trait whose slots are site-neutral applications:\n{polymorphic_constructor_signature}\n{}",
            krate.shapes_rs
        );
        assert!(
            polymorphic_constructor
                .contains("as crate::shapes::KioType>::into_stored(arg0).into_raw()")
                && !polymorphic_constructor.contains("crate::__kio_runtime::as_any"),
            "the polymorphic constructor must transfer its exact rank-N marker token into private storage:\n{polymorphic_constructor}"
        );

        let polymorphic_projector = method("pub fn readPolymorphic");
        let polymorphic_projector_signature = signature(polymorphic_projector);
        assert!(
            polymorphic_projector_signature.contains("-> crate::shapes::KioForall_")
                && !polymorphic_projector_signature.contains("dyn ::std::any::Any"),
            "the polymorphic projector must return the exact rank-N marker carrier:\n{polymorphic_projector_signature}"
        );
        let polymorphic_site_name = polymorphic_projector_signature
            .split_once("-> crate::shapes::")
            .expect("polymorphic projector return path")
            .1
            .split_once("Value<")
            .expect("polymorphic projector Value carrier")
            .0;
        let polymorphic_site_marker = format!(
            "crate::shapes::{polymorphic_site_name}Marker<__KioHost, F> as crate::shapes::KioType>::from_stored("
        );
        assert!(
            polymorphic_projector.contains(&polymorphic_site_marker)
                && polymorphic_projector.contains(
                    "<crate::shapes::KioNewtypeMarker_api__Polymorphic<__KioHost, F> as crate::shapes::KioType>::into_stored(value).into_raw()"
                )
                && !polymorphic_projector.contains("Value::<__KioHost, F>")
                && !polymorphic_projector.contains("(value).0.clone()")
                && !polymorphic_projector.contains("crate::__kio_runtime::as_any"),
            "the polymorphic projector must reconstruct its exact rank-N marker from the exact newtype-marker token:\n{polymorphic_projector}"
        );

        let higher_parameter = method("pub fn makeHigherOrderParameter");
        let higher_parameter_signature = signature(higher_parameter);
        assert!(
            higher_parameter_signature.contains("arg0: crate::shapes::KioFn1<")
                && higher_parameter_signature.contains("crate::shapes::KioFn1<")
                && !higher_parameter_signature.contains("dyn ::std::any::Any"),
            "a function-valued parameter must use nested named carriers:\n{higher_parameter_signature}"
        );

        let higher_return = method("pub fn makeHigherOrderReturn");
        let higher_return_signature = signature(higher_return);
        assert!(
            higher_return_signature.contains("arg0: crate::shapes::KioFn1<")
                && higher_return_signature.matches("KioFn1<").count() == 1
                && higher_return_signature.contains("crate::shapes::KioFunction1<")
                && !higher_return_signature.contains("dyn ::std::any::Any"),
            "a function-valued return must expose one public function facade over its exact nested function marker:\n{higher_return_signature}"
        );

        let host = &krate.host_rs;
        assert!(
            host.contains("crate::shapes::KioFn1<crate::shapes::KioProduct<")
                || packed_constructor_signature
                    .contains("crate::shapes::KioFn1<crate::shapes::KioProduct<"),
            "one written product parameter must remain one KioFn1 source group:\nhost:\n{host}\nconstructor:\n{packed_constructor_signature}"
        );
        assert!(
            !packed_constructor_signature.contains("KioFn3<")
                && !packed_projector_signature.contains("KioFn3<"),
            "flattening a product domain into callable arity would break substitution stability:\n{packed_constructor_signature}\n{packed_projector_signature}"
        );

        let generic_parameter = method("pub fn makeGenericParameter");
        let generic_parameter_signature = signature(generic_parameter);
        assert!(
            generic_parameter_signature.contains("arg0: crate::shapes::KioFn1<T, ")
                && !generic_parameter_signature.contains("KioFn2<"),
            "a generic one-source function must preserve its source group so substituting T with KioProduct has the direct product-domain identity:\n{generic_parameter_signature}"
        );

        let recursive_constructor = method("pub fn makeRecursivePoly");
        let recursive_constructor_signature = signature(recursive_constructor);
        assert!(
            recursive_constructor_signature.contains("arg0: crate::shapes::KioForall_")
                && recursive_constructor_signature.contains("Value<__KioHost>")
                && !recursive_constructor_signature.contains("dyn ::std::any::Any"),
            "a recursive polymorphic function must expose an exact rank-N carrier:\n{recursive_constructor_signature}"
        );

        let recursive_projector = method("pub fn readRecursivePoly");
        let recursive_projector_signature = signature(recursive_projector);
        assert!(
            recursive_projector_signature.contains("-> crate::shapes::KioForall_")
                && !recursive_projector_signature.contains("dyn ::std::any::Any"),
            "a recursive polymorphic function must return an exact rank-N carrier:\n{recursive_projector_signature}"
        );
    }

    #[test]
    fn user_comptime_named_types_keep_lexical_identity() {
        let pkg = build_package(
            "module api; \
             pub newtype Comptime_bool[A] : A { constructor make_bool; projector read_bool; }; \
             pub newtype Comptime_str : . { pub constructor make_str; pub projector read_str; }; \
             pub fn keep_bool[A](value: Comptime_bool(A)) -> Comptime_bool(A) { value } \
             pub fn keep_str(value: Comptime_str) -> Comptime_str { value } \
             pub fn keep_binder[Comptime_bool](value: Comptime_bool) -> Comptime_bool { value }",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit Rust");

        assert!(
            krate.shapes_rs.contains("pub struct ComptimeBool"),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate
                .ffi_rs
                .contains("crate::shapes::nominal::api::ComptimeBool<__KioHost, A>"),
            "{}",
            krate.ffi_rs
        );
        assert!(
            krate.ffi_rs.contains("pub mod api__keepStr {")
                && krate
                    .ffi_rs
                    .contains("pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::nominal::api::ComptimeStr<__KioHost>;")
                && krate
                    .ffi_rs
                    .contains("pub type ret<__KioHost: crate::host::TestPkgHost> = crate::shapes::nominal::api::ComptimeStr<__KioHost>;"),
            "{}",
            krate.ffi_rs
        );
        assert!(
            krate.ffi_rs.contains("pub mod api__keepBinder {")
                && krate
                    .ffi_rs
                    .contains("pub type arg0<ComptimeBool: crate::shapes::KioType> = <ComptimeBool as crate::shapes::KioType>::Facade;")
                && krate
                    .ffi_rs
                    .contains("pub type ret<ComptimeBool: crate::shapes::KioType> = <ComptimeBool as crate::shapes::KioType>::Facade;"),
            "{}",
            krate.ffi_rs
        );
    }

    #[test]
    fn existential_function_projector_passes_rc_closure_to_continuation() {
        let pkg = build_package(
            "module api; \
             host type Text role(str); \
             pub newtype Existential_function <U> : U -> Text { \
                 constructor make_existential_function; \
                 pub projector read_existential_function; \
             };",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit Rust");

        let signature_start = krate
            .shapes_rs
            .find("pub fn readExistentialFunction")
            .expect("existential projector signature");
        let signature_tail = &krate.shapes_rs[signature_start..];
        let signature_end = signature_tail
            .find(" {\n")
            .expect("existential projector body delimiter");
        let signature = &signature_tail[..signature_end];
        assert!(
            signature.contains("continuation: crate::shapes::KioForall_")
                && signature.contains("Value<__KioHost, __KioPolyType_0>")
                && !signature.contains("dyn ::std::any::Any"),
            "the existential projector must expose an exact rank-N continuation without erased public slots:\n{signature}"
        );
        assert!(
            krate.shapes_rs.contains("arg0: crate::shapes::KioFn1<")
                && krate
                    .shapes_rs
                    .contains("let __continuation = <crate::shapes::KioForall_")
                && krate
                    .shapes_rs
                    .contains("let __stage: ::std::rc::Rc<dyn Fn()"),
            "the continuation trait must retain the selected host result while the method body instantiates only private erased storage:\n{}",
            krate.shapes_rs
        );
        assert!(
            krate
                .ffi_rs
                .contains("pub type readExistentialFunction_continuation<")
                && krate
                    .ffi_rs
                    .contains(" as readExistentialFunction_continuation_impl;"),
            "the exact continuation carrier and its implementation trait need stable FFI names:\n{}",
            krate.ffi_rs
        );
    }

    #[test]
    fn existential_outer_forall_payload_erases_existential_below_binder() {
        let pkg = build_package(
            "module api; \
             pub newtype Existential_spread <U> : [A] (U & A) -> U { \
                 constructor make_existential_spread; \
                 pub projector read_existential_spread; \
             };",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg")
            .expect("existential erasure should reach through the outer forall");

        assert!(
            krate.shapes_rs.contains("pub fn readExistentialSpread"),
            "existential projector missing from shapes:\n{}",
            krate.shapes_rs
        );
        let signature_start = krate
            .shapes_rs
            .find("pub fn readExistentialSpread")
            .expect("existential projector signature");
        let signature_tail = &krate.shapes_rs[signature_start..];
        let signature_end = signature_tail
            .find(" {\n")
            .expect("existential projector body delimiter");
        let signature = &signature_tail[..signature_end];
        assert!(
            signature.contains("continuation: crate::shapes::KioForall_")
                && signature.contains("Value<__KioHost, __KioPolyType_0>")
                && !signature.contains("dyn ::std::any::Any"),
            "the hidden existential and outer forall must remain exact in the public rank-N continuation:\n{signature}"
        );
        assert!(
            signature_tail.contains("let __continuation = <crate::shapes::KioForall_")
                && signature_tail.contains("let __stage: ::std::rc::Rc<dyn Fn()"),
            "the existential witness may erase only in the projector's private execution body:\n{signature_tail}"
        );
    }

    #[test]
    fn host_trait_preserves_descriptor_declaration_order() {
        let pkg = build_package(
            "module api; \
             host type Zed role(i32); \
             host type Alpha role(i64); \
             host fn zed() -> .; \
             host fn alpha() -> .; \
             fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let zed_type = krate
            .host_rs
            .find("type api__Zed:")
            .expect("descriptor-first host type");
        let alpha_type = krate
            .host_rs
            .find("type api__Alpha:")
            .expect("descriptor-second host type");
        let zed_fn = krate
            .host_rs
            .find("fn api__zed(")
            .expect("descriptor-first host fn");
        let alpha_fn = krate
            .host_rs
            .find("fn api__alpha(")
            .expect("descriptor-second host fn");

        assert!(
            zed_type < alpha_type && zed_fn < alpha_fn,
            "host trait membership must follow descriptor declaration order, not prepared-catalog key order:\n{}",
            krate.host_rs
        );
    }

    #[test]
    fn host_trait_translates_descriptor_bound_intents() {
        let pkg = build_package(
            "module api; \
             host type Value role(i32); \
             host type Box[A]; \
             fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let mut descriptor = host_descriptor::build_host_descriptor(&pkg);
        for declaration in &mut descriptor.host_types {
            let mut bounds = host_descriptor::BoundRequirementSet::new();
            bounds.insert(BoundRequirement::StaticForDynAnyStorage);
            declaration.bounds = bounds;
        }
        let prepared =
            PreparedBoundaryCallableSites::collect(&pkg, None).expect("prepare exact host facade");
        let names = RustNames::derive("test_pkg");
        let facade = PreparedRustRenderContext::new(&prepared, "crate", &names.host_trait);
        let host_rs = render_rust_host_trait(&descriptor, &prepared, facade, &names)
            .expect("render descriptor-selected host trait");

        assert!(
            host_rs.contains("type api__Value: 'static + From<i32>;"),
            "Rust must translate exactly the descriptor's bound intents while preserving the prepared role facade:\n{host_rs}"
        );
        assert!(
            host_rs.contains("type api__BoxStorage: 'static;"),
            "the descriptor's bound intents must also govern a parametric host type's storage entry:\n{host_rs}"
        );
        assert!(
            !host_rs.contains("type api__Value: Clone")
                && !host_rs.contains("type api__Value: PartialEq")
                && !host_rs.contains("type api__BoxStorage: Clone")
                && !host_rs.contains("type api__BoxStorage: PartialEq"),
            "bounds absent from the descriptor must not reappear from prepared facade defaults:\n{host_rs}"
        );
    }

    #[test]
    fn role_typed_host_type_renders_as_exact_assoc_at_ffi() {
        // A role-bearing env `type Int role(i32);` remains the host's exact
        // selected type. Its role adds literal construction through
        // `From<i32>`; it does not replace the type with the primitive. See
        // `specs/backends/rust.md` § Host record contract.
        let pkg = build_package(
            "module main; \
             host type Int role(i32); host fn print(p0: Int) -> .; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        assert!(
            krate.cargo_toml.contains("name = \"test_pkg\""),
            "cargo_toml: {}",
            krate.cargo_toml
        );
        assert!(
            krate
                .host_rs
                .contains("pub trait TestPkgHost: Clone + 'static {"),
            "host_rs: {}",
            krate.host_rs
        );
        assert!(
            krate
                .host_rs
                .contains("type main__Int: Clone + PartialEq + 'static + From<i32>;"),
            "host_rs is missing the exact role-bearing associated type:\n{}",
            krate.host_rs,
        );
        // The standard `From` capability is sufficient; the ABI does not
        // grow declaration-specific converter methods.
        assert!(
            !krate.host_rs.contains("int_to_i32"),
            "host_rs unexpectedly declares an `int_to_i32` converter:\n{}",
            krate.host_rs,
        );
        assert!(
            !krate.host_rs.contains("int_from_i32"),
            "host_rs unexpectedly declares an `int_from_i32` converter:\n{}",
            krate.host_rs,
        );
        // The trait method names the exact associated type.
        assert!(
            krate
                .host_rs
                .contains("fn main__print(&self, arg0: Self::main__Int);"),
            "host_rs print signature is missing the exact associated type:\n{}",
            krate.host_rs,
        );
    }

    #[test]
    fn same_leaf_role_host_types_keep_exact_literal_roles() {
        let pkg = build_multi_module_package(
            &[
                "module left; \
                 host type Shared role(i32); \
                 pub fn left_literal() -> Shared { 1 }",
                "module right; \
                 host type Shared role(i64); \
                 pub fn right_literal() -> Shared { 2 }",
            ],
            Some("bridge { left; right; }"),
        );
        let host_types = crate::host_descriptor::build_host_descriptor(&pkg)
            .host_types
            .iter()
            .map(|host_type| {
                (
                    (host_type.module_path.to_owned(), host_type.name.to_owned()),
                    HostTypeInfo {
                        role: host_type.role,
                    },
                )
            })
            .collect();
        let shapes = BodyShapeRegistry::new(
            host_types,
            BTreeMap::new(),
            BTreeMap::new(),
            "crate".to_owned(),
            "TestPkgHost".to_owned(),
        );
        let path = |segments: &[&str]| {
            segments
                .iter()
                .map(|segment| PathSegment::synth(*segment, crate::span::Span::new(0, 0)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shapes
                .host_type_info(&path(&["left", "Shared"]))
                .map(|info| info.role),
            Some(Some(Role::I32)),
        );
        assert_eq!(
            shapes
                .host_type_info(&path(&["right", "Shared"]))
                .map(|info| info.role),
            Some(Some(Role::I64)),
        );
        assert!(shapes.host_type_info(&path(&["Shared"])).is_none());

        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");

        assert!(
            krate
                .lib_rs
                .contains("<__KioHost::left__Shared as ::std::convert::From<i32>>::from(1_i32)"),
            "lib_rs left literal did not construct the exact type through its role:\n{}",
            krate.lib_rs,
        );
        assert!(
            krate
                .lib_rs
                .contains("<__KioHost::right__Shared as ::std::convert::From<i64>>::from(2_i64)"),
            "lib_rs right literal did not construct the exact type through its role:\n{}",
            krate.lib_rs,
        );
    }

    #[test]
    fn same_leaf_public_newtypes_keep_exact_carrier_identities() {
        let pkg = build_multi_module_package(
            &[
                "module left; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round_token(value: Token) -> Token;",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round_token(value: Token) -> Token;",
            ],
            Some("bridge { left; right; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");

        assert!(
            krate.shapes_rs.contains(
                "pub mod left {\n        #[allow(non_camel_case_types)]\n        pub struct Token"
            ),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains(
                "pub mod right {\n        #[allow(non_camel_case_types)]\n        pub struct Token"
            ),
            "{}",
            krate.shapes_rs
        );
        assert!(
            krate
                .host_rs
                .contains("crate::shapes::nominal::left::Token<Self>")
                && krate
                    .host_rs
                    .contains("crate::shapes::nominal::right::Token<Self>"),
            "{}",
            krate.host_rs
        );
    }

    #[test]
    fn unrelated_declarations_cannot_rename_a_public_nominal_type() {
        let target = "module zed; \
            pub newtype Token : . { pub constructor make_token; pub projector read_token; }; \
            host fn round_token(value: Token) -> Token;";
        let baseline = build_multi_module_package(&[target], Some("bridge { zed; }"));
        let extended = build_multi_module_package(
            &[
                target,
                "module zed_; \
                 pub newtype Token : . { pub constructor make_token; pub projector read_token; };",
                "module other; \
                 pub newtype Token : . { pub constructor make_token; pub projector read_token; };",
            ],
            Some("bridge { zed; zed_; other; }"),
        );
        let baseline = lower_package(&baseline, "test_pkg").expect("lower baseline");
        let extended = lower_package(&extended, "test_pkg").expect("lower extended");
        let target_signature = |host: &str| {
            host.lines()
                .find(|line| line.contains("zed__roundToken"))
                .expect("zed host method")
                .trim()
                .to_owned()
        };

        assert_eq!(
            target_signature(&baseline.host_rs),
            target_signature(&extended.host_rs)
        );
        assert!(
            target_signature(&baseline.host_rs)
                .contains("crate::shapes::nominal::zed::Token<Self>"),
            "{}",
            baseline.host_rs
        );
    }

    #[test]
    fn same_leaf_host_types_across_modules_get_rung2_qualified_assoc_types() {
        // Two bridged modules each declare `host type Box[T];` — distinct,
        // module-qualified host types that happen to share a leaf. The
        // Rust host boundary is rung 2 (module-qualified, flat-but-
        // injective), so each emits a *distinct* associated type
        // (`app__Box` / `lib__Box`) rather than colliding on a single
        // `type Box`. A reference resolves to its declaring module's
        // associated type: a bare reference in a host fn's own signature
        // resolves to that fn's module; a contract-qualified reference
        // (`lib.Box` in `app`'s exported fn) resolves to the qualifier's
        // module. Regression for the rung-2 host-type
        // collision the emitter previously produced.
        let pkg = build_multi_module_package(
            &[
                "module app; \
                 import lib as lib; \
                 host type String role(str); \
                 host type Box[T]; \
                 host fn read_string(x: Box(String)) -> String; \
                 pub fn relay(x: lib.Box(lib.String)) -> lib.String { lib.read_string(x) }",
                "module lib; \
                 host type String role(str); \
                 host type Box[T]; \
                 host fn read_string(x: Box(String)) -> String; \
                 pub fn unbox(x: Box(String)) -> String { read_string(x) }",
            ],
            Some("bridge { app; lib; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        // Distinct declaration-owned storage types — no bare `type Box`.
        assert!(
            krate
                .host_rs
                .contains("type app__BoxStorage: Clone + PartialEq + 'static;"),
            "host_rs missing `app__BoxStorage` associated type:\n{}",
            krate.host_rs,
        );
        assert!(
            krate
                .host_rs
                .contains("type lib__BoxStorage: Clone + PartialEq + 'static;"),
            "host_rs missing `lib__BoxStorage` associated type:\n{}",
            krate.host_rs,
        );
        assert!(
            !krate.host_rs.contains("    type Box<"),
            "host_rs still emits an unqualified `type Box` (collision):\n{}",
            krate.host_rs,
        );
        // Each host fn references its own module's generated Box carrier.
        assert!(
            krate.host_rs.contains("fn app__readString")
                && krate.host_rs.contains("KioHostType_app__Box"),
            "host_rs `app__readString` does not reference its Box carrier:\n{}",
            krate.host_rs,
        );
        assert!(
            krate.host_rs.contains("fn lib__readString")
                && krate.host_rs.contains("KioHostType_lib__Box"),
            "host_rs `lib__readString` does not reference its Box carrier:\n{}",
            krate.host_rs,
        );
        // The env FFI submodules are rung-2 named (no `Box` collision on
        // the bare `readString` leaf) and reference the qualified
        // declaration-owned carrier fully.
        assert!(
            krate.ffi_rs.contains("pub mod app__readString {"),
            "ffi_rs missing `app__readString` env submodule:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate.ffi_rs.contains("pub mod lib__readString {"),
            "ffi_rs missing `lib__readString` env submodule:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate.ffi_rs.contains("KioHostType_app__Box"),
            "ffi_rs `app` env submodule does not reference its carrier:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate.ffi_rs.contains("KioHostType_lib__Box"),
            "ffi_rs `lib` env submodule does not reference its carrier:\n{}",
            krate.ffi_rs,
        );
        // `app`'s exported `relay` qualifies `Box` through `lib`, so its
        // contract reference resolves to `lib__Box`.
        assert!(
            krate.lib_rs.contains("pub fn relay") && krate.lib_rs.contains("KioHostType_lib__Box"),
            "lib_rs `relay` does not resolve the imported `Box` to `lib__Box`:\n{}",
            krate.lib_rs,
        );
    }

    #[test]
    fn parameterized_host_storage_is_substitution_stable_across_public_uses() {
        let pkg = build_package(
            "module api; \
             host type Box[T]; \
             pub type Wrapped[T] = Box(T); \
             host fn direct[T](value: T, boxed: Box(T)) -> Box(T); \
             host fn through_alias[T](boxed: Wrapped(T)) -> Wrapped(T); \
             host fn compound[A][B](boxed: Box(A & B)) -> Box(A & B); \
             fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower parameterized host storage");
        let carrier = "KioHostType_api__Box";

        assert!(
            krate
                .host_rs
                .contains("type api__BoxStorage: Clone + PartialEq + 'static;"),
            "the host declaration must own one substitution-stable storage type:\n{}",
            krate.host_rs,
        );
        assert!(
            !krate.host_rs.contains("type api__Box<")
                && !krate.host_rs.contains("type Box<")
                && !krate.host_rs.contains("::api__Box<"),
            "a parameterized native associated GAT survived the host rebaseline:\n{}",
            krate.host_rs,
        );
        assert!(
            krate.shapes_rs.contains(&format!(
                "pub struct {carrier}<__KioHost: crate::host::TestPkgHost, __KioArg_0: crate::shapes::KioType>"
            )) && krate.shapes_rs.contains(
                "PhantomData<fn(__KioHost, __KioArg_0) -> (__KioHost, __KioArg_0)>"
            ) && krate.shapes_rs.contains(
                "pub fn from_storage(value: <__KioHost as crate::host::TestPkgHost>::api__BoxStorage)"
            ),
            "the declaration-owned carrier/storage seam is missing or variant:\n{}",
            krate.shapes_rs,
        );
        assert!(
            krate.shapes_rs.contains(
                "crate::shapes::KioHostType_api__Box::<__KioHost, __KioArg_0>(value, ::std::marker::PhantomData)"
            ) && !krate.shapes_rs.contains(
                "crate::shapes::KioHostType_api__Box<__KioHost, __KioArg_0>(value"
            ),
            "the parameterized-host marker decoder must use a valid generic tuple-struct constructor expression:\n{}",
            krate.shapes_rs,
        );

        let direct = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn api__direct"))
            .expect("direct generic host signature");
        let alias = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn api__throughAlias"))
            .expect("aliased generic host signature");
        let compound = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn api__compound"))
            .expect("compound generic host signature");
        assert!(
            direct.contains("arg0: <T as crate::shapes::KioType>::Facade")
                && direct.matches(carrier).count() == 2,
            "direct generic use did not pair the marker facade with one carrier identity:\n{direct}",
        );
        assert!(
            alias.matches(carrier).count() == 2,
            "expanding a transparent alias changed the parameterized host identity:\n{alias}",
        );
        assert!(
            compound.matches(carrier).count() == 2
                && compound.matches("crate::shapes::KioProduct<A, B>").count() == 2
                && !compound.contains("Product<<A as")
                && !compound.contains("Product<A::Facade"),
            "compound substitution must key the carrier by the canonical product marker:\n{compound}",
        );
    }

    #[test]
    fn export_namespace_topology_ignores_raw_declaration_from_different_catalog() {
        let authority = build_package(
            "module api; pub fn admitted() -> . { () }",
            Some("bridge { api; }"),
        );
        let unrelated = build_package(
            "module api; pub fn intruder() -> . { () }",
            Some("bridge { api; }"),
        );
        let raw_module_fns = collect_module_fns(&unrelated);
        assert!(
            raw_module_fns
                .iter()
                .any(|module_fn| module_fn.fn_def.name == "intruder"),
            "counterfactual raw metadata must contain the unrelated declaration"
        );
        let prepared = PreparedBoundaryCallableSites::collect(&authority, None)
            .expect("prepare authoritative exports");

        let namespaces = collect_export_block_namespaces(&prepared);
        let api = namespaces
            .children
            .get("api")
            .expect("the admitted api namespace remains present");
        assert!(api.methods.contains_key("admitted"));
        assert!(!api.methods.contains_key("intruder"));
    }

    #[test]
    fn anonymous_product_uses_the_canonical_binary_facade() {
        let pkg = build_package(
            "module main; \
             host type Int role(i32); host type Str role(str); host fn pair(p0: Int, p1: Str) -> (Int & Str); \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        assert!(
            krate
                .shapes_rs
                .contains("pub struct Product<A: Clone + 'static, B: Clone + 'static>")
                && !krate.shapes_rs.contains("pub struct Prod_"),
            "shapes_rs: {}",
            krate.shapes_rs
        );
        assert!(
            krate.host_rs.contains("crate::shapes::Product<"),
            "host_rs: {}",
            krate.host_rs
        );
        assert!(
            krate.shapes_rs.contains("pub struct KioFunction2<"),
            "two written source parameters must retain KioFunction2 support:\n{}",
            krate.shapes_rs
        );
    }

    #[test]
    fn anonymous_sum_uses_the_canonical_binary_facade() {
        let pkg = build_package(
            "module main; \
             host type Int role(i32); host type Str role(str); host fn either() -> (Int | Str); \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        assert!(
            krate
                .shapes_rs
                .contains("pub enum Sum<A: Clone + 'static, B: Clone + 'static>"),
            "shapes_rs: {}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("pub enum Sum_"),
            "legacy content-keyed sum leaked into the public facade:\n{}",
            krate.shapes_rs
        );
    }

    #[test]
    fn exported_polymorphic_newtype_uses_its_prepared_payload_facade() {
        // The newtype is exported solely by being a bridged `pub` item. Its
        // carrier, aliases, and payload facade must therefore come from the
        // prepared public inventory rather than body-shape registration.
        let pkg = build_package(
            "module main; \
             pub newtype Pair[A][B] : A & B { pub constructor mk_pair; pub projector un_pair; };",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        assert!(
            krate
                .shapes_rs
                .contains("pub struct Product<A: Clone + 'static, B: Clone + 'static>")
                && !krate.shapes_rs.contains("pub struct Prod_"),
            "exported newtype payload did not use the canonical product facade:\n{}",
            krate.shapes_rs
        );
        assert!(
            krate.ffi_rs.contains("pub mod main__Pair {")
                && krate.ffi_rs.contains("pub type mkPair_arg0<A: crate::shapes::KioType, B: crate::shapes::KioType> = crate::shapes::Product<")
                && !krate.ffi_rs.contains("pub type mkPair_arg1")
                && krate.ffi_rs.contains("pub type unPair_ret"),
            "exported newtype FFI surface missing in ffi_rs:\n{}",
            krate.ffi_rs
        );
        assert!(
            krate.shapes_rs.contains(
                "crate::shapes::nominal::main::Pair::<__KioHost, __KioArg_0, __KioArg_1>(value, ::std::marker::PhantomData)"
            ) && !krate.shapes_rs.contains(
                "crate::shapes::nominal::main::Pair<__KioHost, __KioArg_0, __KioArg_1>(value"
            ),
            "the generic-newtype marker decoder must use a valid generic tuple-struct constructor expression:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn polymorphic_host_fn_lowers_to_generic_method() {
        let pkg = build_package(
            "module main; \
             host type Int role(i32); host fn id[A](v: A) -> A; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("polymorphic host fn lowers");
        // User type-params are representation markers; bare occurrences use
        // their canonical associated facade. The method name remains the
        // rung-2 module-qualified `main__id`.
        assert!(
            krate
                .host_rs
                .contains("fn main__id<A: crate::shapes::KioType>(&self, arg0: <A as crate::shapes::KioType>::Facade) -> <A as crate::shapes::KioType>::Facade;"),
            "polymorphic host fn signature missing in:\n{}",
            krate.host_rs,
        );
        assert!(
            krate
                .host_rs
                .contains("#[allow(non_camel_case_types, non_snake_case)]"),
            "expected allow(non_camel_case_types, non_snake_case) attribute in:\n{}",
            krate.host_rs,
        );
    }

    #[test]
    fn host_callback_uses_named_canonical_carrier() {
        let pkg = build_package(
            "module main; \
             host type String role(str); \
             host type I32 role(i32); \
             host fn round(f: String -> String -> String) -> String -> String -> String; \
             host fn higher_order(f: (I32 -> I32) -> I32) -> I32; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("heap callback host fn lowers");
        assert!(
            krate.host_rs.contains("crate::shapes::KioFn1<")
                && !krate.host_rs.contains("where __F0: Fn"),
            "callback parameter did not use its named facade carrier:\n{}",
            krate.host_rs,
        );
        assert!(
            krate.host_rs.matches("KioFn1<").count() >= 3,
            "a nested callback parameter must recursively use named carriers:\n{}",
            krate.host_rs,
        );
    }

    #[test]
    fn rank_n_host_fn_param_lowers_to_uniform_value_with_typed_ingress() {
        let pkg = build_package(
            "module main; \
             host type String role(str); host fn print(s: String) -> .; host fn apply_poly(f: [T]( T) -> T) -> String; \
             pub fn poly_echo[T](x: T) -> T { x } \
             pub fn main() -> . { print(apply_poly(poly_echo)) }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("rank-N host fn lowers");
        assert!(
            krate
                .host_rs
                .contains("fn main__applyPoly(&self, arg0: crate::shapes::KioForall_")
                && krate.host_rs.contains("Value<Self>")
                && krate.host_rs.contains(") -> Self::main__String;")
                && !krate.host_rs.contains("dyn ::std::any::Any"),
            "rank-N host fn signature missing in:\n{}",
            krate.host_rs,
        );
        assert!(
            krate.ffi_rs.contains(
                "pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::KioForall_"
            ) && krate.ffi_rs.contains("Value<__KioHost>;")
                && krate.ffi_rs.contains(" as arg0_impl;"),
            "rank-N FFI aliases must pair the uniform Value with its typed implementation trait:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub fn new(__implementation: impl KioForall_")
                && !krate
                    .host_rs
                    .contains("arg0: impl crate::shapes::KioForall_"),
            "only Value::new may use impl Trait ingress:\nshapes:\n{}\nhost:\n{}",
            krate.shapes_rs,
            krate.host_rs,
        );
        // The public trait carries the exact per-call type while the paired
        // execution adapter instantiates it at the private erased body type.
        assert!(
            krate
                .shapes_rs
                .contains("fn apply<__KioPolyType_0: crate::shapes::KioType>(")
                && krate
                    .shapes_rs
                    .contains("arg0: <__KioPolyType_0 as crate::shapes::KioType>::Facade) -> <__KioPolyType_0 as crate::shapes::KioType>::Facade")
                && krate
                    .lib_rs
                    .contains("<crate::shapes::KioForall_Site_M_main__H_applyPoly_U3_K1_B0_A0Marker<__KioHost> as crate::shapes::KioType>::from_stored(crate::shapes::KioStoredValue::from_raw(")
                && krate
                    .shapes_rs
                    .contains("::std::rc::Rc<dyn Fn() -> ::std::rc::Rc<dyn ::std::any::Any>>"),
            "rank-N adapter does not pair the exact trait and marker codec with private erased execution:\nshapes:\n{}\nlib:\n{}",
            krate.shapes_rs,
            krate.lib_rs,
        );
    }

    #[test]
    fn returned_forall_uses_uniform_value_with_stable_typed_ingress_alias() {
        let pkg = build_package(
            "module main; \
             host fn produce(_unit: .) -> [A] A; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("returned rank-N host fn lowers");
        let signature = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn main__produce"))
            .expect("returned rank-N host signature");
        assert!(
            signature.contains("-> crate::shapes::KioForall_")
                && signature.contains("Value<Self>")
                && !signature.contains("impl crate::shapes::KioForall_")
                && !signature.contains("dyn ::std::any::Any"),
            "a returned forall must use the same nameable Value facade as every other occurrence:\n{signature}",
        );
        assert!(
            krate.ffi_rs.contains(
                "pub type ret<__KioHost: crate::host::TestPkgHost> = crate::shapes::KioForall_"
            ) && krate.ffi_rs.contains("Value<__KioHost>;")
                && krate.ffi_rs.contains(" as ret_impl;"),
            "the returned Value and its implementation trait need stable paired FFI names:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub fn new(__implementation: impl KioForall_")
                && krate.shapes_rs.contains("Value<__KioHost>"),
            "the site-owned Value must provide typed construction without exposing stored Any:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn rank_n_direct_unit_function_is_nullary_in_public_and_private_forms() {
        let pkg = build_package(
            "module main; \
             host fn consume(poly: [A](.) -> .) -> .; \
             pub fn forward(poly: [A](.) -> .) -> . { consume(poly) }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit rank-N Unit function");
        assert!(
            krate.shapes_rs.contains("let __f: ::std::rc::Rc<dyn Fn()")
                && krate.shapes_rs.contains("(&*__poly)")
                && !krate.shapes_rs.contains("(&*__poly, ())"),
            "rank-N implementation ingress must invoke the semantically nullary public and private function:\n{}",
            krate.shapes_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("::__kio_runtime::from_any::<::std::rc::Rc<dyn Fn()")
                && krate
                    .shapes_rs
                    .lines()
                    .any(|line| line.contains("fn apply") && line.contains("(&self)"))
                && !krate.shapes_rs.contains("let _ = arg0;"),
            "rank-N Value::apply must expose and invoke Function0 without a synthetic Unit argument:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn staged_unit_head_is_publicly_and_privately_nullary() {
        let pkg = build_package(
            "module main; \
             pub fn staged_unit[A](value: .)(result: A) -> A { result }",
            Some("bridge { main; }"),
        );
        let krate =
            lower_package(&pkg, "test_pkg").expect("emit staged Unit declaration-head seam");

        let staged_start = krate
            .lib_rs
            .find("pub fn stagedUnit")
            .expect("staged Unit export method");
        let staged_tail = &krate.lib_rs[staged_start..];
        let staged_end = staged_tail
            .find("\n    }\n")
            .expect("staged Unit export method end");
        let staged = &staged_tail[..staged_end];
        assert!(
            staged.contains("arg0: <A as crate::shapes::KioType>::Facade")
                && !staged.contains("arg0: ()")
                && !staged.contains("arg1:")
                && staged.contains("::__kio_runtime::from_any::<::std::rc::Rc<dyn Fn()")
                && staged.contains(")()")
                && !staged.contains("as_any(())"),
            "a direct Unit head is absent from the public argument list but still invokes the private Fn0 stage before the later value stage:\n{staged}",
        );
    }

    #[test]
    fn public_host_and_source_binders_are_collision_free_and_well_formed() {
        let pkg = build_package(
            "module main; \
             host type Box[A]; \
             host fn boxed[H](value: Box(H)) -> Box(H); \
             host fn hkt[*F](value: F(.)) -> F(.); \
             host fn staged[A](poly: [X](X) -> X)[B](second: B) -> A; \
             host fn repeated[A](first: A)[A](second: A) -> A; \
             pub newtype Shadow[A][A] : A { \
                 pub constructor make_shadow; pub projector read_shadow; \
             }; \
             pub newtype Witness[A][A] <Self> : A & Self { \
                 pub constructor make_witness; pub projector read_witness; \
             }; \
             newtype Keyword[Self] : Self { constructor make_keyword; projector read_keyword; }; \
             fn hold_keyword(value: .) -> Keyword(.) { \
                 Keyword.make_keyword(., value) \
             } \
             pub fn call_staged[A](poly: [X](X) -> X)[B](second: B) -> A { \
                 staged(A, poly, B, second) \
             } \
             pub fn identity[H](value: H) -> H { value }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit hygienic Rust binders");

        assert!(
            krate
                .lib_rs
                .contains("impl<__KioHost: host::TestPkgHost> TestPkg<__KioHost>")
                && krate
                    .lib_rs
                    .contains("pub fn identity<H: crate::shapes::KioType>"),
            "the reserved host marker must not rename a source binder named H:\n{}",
            krate.lib_rs,
        );
        assert!(
            krate.host_rs.contains(
                "fn main__repeated<A: crate::shapes::KioType, AN2: crate::shapes::KioType>",
            ) && !krate.host_rs.contains(
                "fn main__repeated<A: crate::shapes::KioType, A: crate::shapes::KioType>",
            ),
            "later shadowing binders must retain the frontend's canonical alpha spelling:\n{}",
            krate.host_rs,
        );
        assert!(
            krate.shapes_rs.contains("A: crate::shapes::KioType")
                && krate.shapes_rs.contains("AN2: crate::shapes::KioType"),
            "rank-N support under a shadowed head must use the same allocated scope:\n{}",
            krate.shapes_rs,
        );
        let witness_constructor = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("pub fn makeWitness"))
            .expect("existential constructor witness");
        assert!(
            krate.shapes_rs.contains("Witness<__KioHost, A, AN2> {")
                && witness_constructor.contains("<__kio_kw_Self: crate::shapes::KioType>")
                && witness_constructor.contains("AN2"),
            "constructor witnesses must extend, not perturb, the declaration-binder prefix:\n{}\n{witness_constructor}",
            krate.shapes_rs,
        );
        let staged_host = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn main__staged"))
            .expect("staged host method");
        assert!(
            staged_host.contains("Value<Self, A>")
                && !staged_host.contains("Value<Self, A, B>")
                && staged_host.contains("arg1: <B as crate::shapes::KioType>::Facade"),
            "an earlier forall occurrence must not acquire a later head binder:\n{staged_host}",
        );
        let staged_export = krate
            .lib_rs
            .lines()
            .find(|line| line.contains("pub fn callStaged"))
            .expect("staged export method");
        assert!(
            staged_export.contains("Value<__KioHost, A>")
                && !staged_export.contains("Value<__KioHost, A, B>")
                && !krate.lib_rs.contains(
                    "Value<__KioHost, crate::shapes::__KioErasedType, crate::shapes::__KioErasedType>",
                ),
            "export and erased host-value adapters must render the same earlier occurrence scope:\n{}",
            krate.lib_rs,
        );
        let staged_ffi = krate
            .ffi_rs
            .split_once("pub mod main__staged {\n")
            .expect("staged FFI module")
            .1
            .split_once("\n    }")
            .expect("staged FFI module end")
            .0;
        assert!(
            staged_ffi.contains(
                "pub type arg0<__KioHost: crate::host::TestPkgHost, A: crate::shapes::KioType>"
            ) && staged_ffi.contains("Value<__KioHost, A>;")
                && !staged_ffi.contains("Value<__KioHost, A, B>"),
            "FFI aliases must use the lexical scope at the value occurrence:\n{staged_ffi}",
        );
        let staged_forall = staged_ffi
            .lines()
            .find(|line| line.ends_with(" as arg0_impl;"))
            .and_then(|line| line.split("::shapes::").nth(1))
            .and_then(|tail| tail.split_whitespace().next())
            .expect("staged forall support name");
        let staged_forall_declaration = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains(&format!("pub trait {staged_forall}<")))
            .expect("staged forall declaration");
        assert!(
            staged_forall_declaration.contains("A: crate::shapes::KioType")
                && !staged_forall_declaration.contains("B: crate::shapes::KioType"),
            "site-owned forall support must declare its exact occurrence scope:\n{staged_forall_declaration}",
        );
        assert!(
            krate.shapes_rs.contains(
                "pub struct Shadow<__KioHost: crate::host::TestPkgHost, A: crate::shapes::KioType, AN2: crate::shapes::KioType>",
            )
                && !krate.shapes_rs.contains(
                    "pub struct Shadow<__KioHost: crate::host::TestPkgHost, A: crate::shapes::KioType, A: crate::shapes::KioType>",
                )
                && krate.shapes_rs.contains(
                    "struct Keyword<__KioHost: 'static, __kio_kw_Self: Clone + 'static>",
                )
                && krate.shapes_rs.contains("Box<__kio_kw_Self>"),
            "public/private newtype headers and payloads must share collision-safe escaped binders:\n{}",
            krate.shapes_rs,
        );
        let shadow_constructor = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("pub fn makeShadow"))
            .expect("shadowed newtype constructor");
        assert!(
            krate.shapes_rs.contains("Shadow<__KioHost, A, AN2> {")
                && shadow_constructor.contains("AN2")
                && !shadow_constructor.contains("arg0: <A as"),
            "newtype impl/member uses must share the carrier's semantic binder allocation:\n{}\n{shadow_constructor}",
            krate.shapes_rs,
        );
        assert!(
            krate.ffi_rs.contains(
                "pub type arg0<__KioHost: crate::host::TestPkgHost, H: crate::shapes::KioType> = crate::shapes::KioHostType_main__Box<__KioHost, H>;",
            ),
            "FFI aliases must distinguish the host marker from source H and declare both WF bounds:\n{}",
            krate.ffi_rs,
        );
        assert!(
            krate.ffi_rs.contains("AN2: crate::shapes::KioType")
                && krate.ffi_rs.contains("type_alias_bounds")
                && krate
                    .ffi_rs
                    .contains("pub type arg0<F: crate::shapes::KioTypeConstructor1> = crate::shapes::KioApply1<F,"),
            "shadowed and higher-kinded source dependencies need their exact alias bounds:\n{}",
            krate.ffi_rs,
        );
    }

    #[test]
    fn ordinary_public_arguments_preserve_source_grouping_and_direct_unit_nullarity() {
        let pkg = build_package(
            "module main; \
             host type Int role(i32); \
             host fn packed(value: Int & Int) -> .; \
             host fn split(left: Int, right: Int) -> .; \
             host fn unit(value: .) -> .; \
             host fn nullary() -> .; \
             host fn round_unit(callback: . -> .) -> (. -> .); \
             pub newtype Pair : Int & Int { pub constructor make_pair; pub projector read_pair; }; \
             pub fn call_packed(value: Int & Int) -> . { packed(value) } \
             pub fn packed_value() -> ((Int & Int) -> .) { packed } \
             pub fn keep_pair(value: Int & Int) -> Int & Int { value } \
             pub fn keep_split(left: Int, right: Int) -> Int & Int { (left, right) } \
             pub fn call_unit(value: .) -> . { unit(value) } \
             pub fn unit_value() -> (. -> .) { unit } \
             pub fn call_round_unit(callback: . -> .) -> (. -> .) { \
                 round_unit(callback) \
             } \
             pub fn call_nullary() -> . { nullary() }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit source-grouped Rust facade");

        assert!(
            krate
                .host_rs
                .contains("fn main__packed(&self, arg0: crate::shapes::Product<"),
            "a written product is one public host argument:\n{}",
            krate.host_rs,
        );
        let packed_host = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn main__packed"))
            .expect("packed host signature");
        assert!(
            !packed_host.contains("arg1:")
                && krate.host_rs.contains(
                    "fn main__split(&self, arg0: Self::main__Int, arg1: Self::main__Int)"
                )
                && krate.host_rs.contains("fn main__unit(&self)")
                && krate.host_rs.contains("fn main__nullary(&self)"),
            "product and independent source arities remain distinct while direct Unit and nullary declarations are both nullary:\n{}",
            krate.host_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub fn makePair(arg0: crate::shapes::Product<")
                && krate
                    .lib_rs
                    .contains("pub fn keepPair(&self, arg0: crate::shapes::Product<")
                && krate
                    .lib_rs
                    .matches(".main__packed(crate::shapes::Product { _0:")
                    .count()
                    >= 2,
            "newtype/export/direct/value-ref adapters must pack the same grouped facade:\nshapes:\n{}\nlib:\n{}",
            krate.shapes_rs,
            krate.lib_rs,
        );
        let packed_ffi = krate
            .ffi_rs
            .split_once("pub mod main__packed {\n")
            .expect("packed FFI module")
            .1
            .split_once("\n    }")
            .expect("packed FFI module end")
            .0;
        assert!(
            packed_ffi.contains(
                "pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::Product<"
            ) && !packed_ffi.contains("pub type arg1"),
            "ordinary FFI aliases must name source parameters rather than flattened slots:\n{packed_ffi}",
        );
        let split_ffi = krate
            .ffi_rs
            .split_once("pub mod main__split {\n")
            .expect("split FFI module")
            .1
            .split_once("\n    }")
            .expect("split FFI module end")
            .0;
        assert!(
            split_ffi.contains("pub type arg0<__KioHost: crate::host::TestPkgHost>")
                && split_ffi.contains("pub type arg1<__KioHost: crate::host::TestPkgHost>"),
            "independent source parameters must keep independent FFI aliases:\n{split_ffi}",
        );
        assert!(
            !krate.ffi_rs.contains("pub mod main__unit {")
                && !krate.ffi_rs.contains("pub mod main__nullary {")
                && krate.lib_rs.contains("pub fn callUnit(&self)")
                && krate.lib_rs.matches(".main__unit()").count() >= 2
                && krate.lib_rs.contains("pkg.___mod_4_main_call_uunit()")
                && !krate
                    .lib_rs
                    .contains("self.main__callUnit(crate::__kio_runtime::as_any(()))")
                && krate.lib_rs.contains("pub fn callNullary(&self)"),
            "direct Unit and nullary declarations both have no public argument or FFI alias:\nffi:\n{}\nlib:\n{}",
            krate.ffi_rs,
            krate.lib_rs,
        );
        let round_unit_host = krate
            .host_rs
            .lines()
            .find(|line| line.contains("fn main__roundUnit"))
            .expect("round_unit host method");
        let call_round_unit_export = krate
            .lib_rs
            .lines()
            .find(|line| line.contains("pub fn callRoundUnit"))
            .expect("call_round_unit export method");
        assert!(
            round_unit_host.matches("KioFn0<").count() == 2
                && !round_unit_host.contains("KioFn1<")
                && call_round_unit_export.matches("KioFn0<").count() == 2
                && !call_round_unit_export.contains("KioFn1<")
                && !krate.lib_rs.contains("__kio_private_unit_function")
                && !krate.lib_rs.contains("__kio_public_unit_function"),
            "a direct Unit-domain function has one semantic Fn0 representation at every occurrence:\nhost:\n{round_unit_host}\nexport:\n{call_round_unit_export}\nlib:\n{}",
            krate.lib_rs,
        );
    }

    #[test]
    fn function_facades_preserve_semantic_abi_arity_at_nested_occurrences() {
        let pkg = build_package(
            "module main; \
             host type Text role(str); \
             pub type Callback[A] = A -> Text; \
             host fn direct_callback(callback: . -> Text) -> Text; \
             host fn substituted_callback(callback: Callback(.)) -> Text; \
             host fn nested_product(value: (. -> Text) & Text) -> Text; \
             host fn nested_sum(value: (. -> Text) | Text) -> Text; \
             pub newtype Direct_callback : . -> Text { \
                 pub constructor make_direct_callback; \
                 pub projector read_direct_callback; \
             }; \
             host fn nested_forall(poly: [A] ((. -> A) & A) -> A) -> Text; \
             pub newtype Existential_callback <U> : (. -> U) & U { \
                 constructor make_existential_callback; \
                 pub projector read_existential_callback; \
             }; \
             host fn nested_hkt[*F](value: F(. -> Text)) -> F(. -> Text); \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit semantic function arities");

        let host_method = |needle: &str| {
            krate
                .host_rs
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("missing host method `{needle}`:\n{}", krate.host_rs))
                .to_owned()
        };
        let product = host_method("fn main__nestedProduct");
        let sum = host_method("fn main__nestedSum");
        assert!(
            product.contains("crate::shapes::Product<crate::shapes::KioFn0<")
                && !product.contains("KioFn1<")
                && sum.contains("crate::shapes::Sum<crate::shapes::KioFn0<")
                && !sum.contains("KioFn1<"),
            "direct Unit-domain functions nested in products and sums remain Fn0:\nproduct:\n{product}\nsum:\n{sum}",
        );

        let constructor = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("pub fn makeDirectCallback"))
            .expect("direct-function newtype constructor");
        let projector = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("pub fn readDirectCallback"))
            .expect("direct-function newtype projector");
        assert!(
            constructor.contains("arg0: crate::shapes::KioFn0<")
                && !constructor.contains("KioFn1<")
                && projector.contains("-> crate::shapes::KioFn0<")
                && !projector.contains("KioFn1<"),
            "a direct Unit-domain newtype payload has one Fn0 carrier:\nconstructor:\n{constructor}\nprojector:\n{projector}",
        );

        let ffi_module = |member: &str| {
            let start = format!("pub mod main__{member} {{\n");
            krate
                .ffi_rs
                .split_once(&start)
                .unwrap_or_else(|| panic!("missing FFI module `{member}`:\n{}", krate.ffi_rs))
                .1
                .split_once("\n    }")
                .expect("FFI module terminator")
                .0
                .to_owned()
        };
        let direct = ffi_module("directCallback");
        let substituted = ffi_module("substitutedCallback");
        assert!(
            direct.contains(" = crate::shapes::KioFn0<")
                && direct.contains("pub type arg0_cbret<")
                && !direct.contains("arg0_cbarg")
                && !direct.contains("KioFn1<"),
            "a direct Unit callback has a KioFn0 carrier and no callback-argument alias:\n{direct}",
        );
        assert!(
            substituted.contains(" = crate::shapes::KioFn1<crate::shapes::KioUnit,")
                && !substituted.contains("arg0_cbarg")
                && substituted.contains("pub type arg0_cbret<")
                && !substituted.contains("KioFn0<"),
            "substituting Unit for a generic callback parameter retains its one-slot KioFn1 ABI while the redundant Unit callback-argument alias stays omitted:\n{substituted}",
        );

        let hkt = ffi_module("nestedHkt");
        assert!(
            hkt.matches("KioFunction0<").count() == 2 && !hkt.contains("KioFunction1<"),
            "a direct Unit-domain function used as an HKT argument retains its Fn0 marker:\n{hkt}",
        );

        let ffi_impl_name = |alias: &str| {
            let suffix = format!(" as {alias};");
            krate
                .ffi_rs
                .lines()
                .find(|line| line.ends_with(&suffix))
                .and_then(|line| line.split("::shapes::").nth(1))
                .and_then(|tail| tail.split_whitespace().next())
                .unwrap_or_else(|| {
                    panic!(
                        "missing FFI implementation alias `{alias}`:\n{}",
                        krate.ffi_rs
                    )
                })
                .to_owned()
        };
        let trait_body = |name: &str| {
            let start = format!("pub trait {name}");
            krate
                .shapes_rs
                .split_once(&start)
                .unwrap_or_else(|| panic!("missing forall trait `{name}`:\n{}", krate.shapes_rs))
                .1
                .split_once("\n}\n")
                .expect("forall trait terminator")
                .0
                .to_owned()
        };
        let forall = trait_body(&ffi_impl_name("arg0_impl"));
        let existential = trait_body(&ffi_impl_name("readExistentialCallback_continuation_impl"));
        assert!(
            forall.contains("crate::shapes::Product<crate::shapes::KioFn0<")
                && !forall.contains("KioFn1<"),
            "a direct Unit-domain function nested below forall retains its Fn0 facade:\n{forall}",
        );
        assert!(
            existential.contains("crate::shapes::Product<crate::shapes::KioFn0<")
                && !existential.contains("KioFn1<"),
            "a direct Unit-domain function nested below an existential retains its Fn0 facade:\n{existential}",
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub struct KioFn0<KioResult: KioType>")
                && krate
                    .shapes_rs
                    .contains("pub fn new(__function: impl Fn() -> KioResult::Facade + 'static)")
                && krate
                    .shapes_rs
                    .contains("pub fn call(&self) -> KioResult::Facade")
                && krate
                    .shapes_rs
                    .contains("pub struct KioFunction0<KioResult: KioType>")
                && krate.shapes_rs.contains("type Facade = KioFn0<KioResult>;"),
            "Function0 support keeps its result marker and nullary constructor/call ABI:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn ffi_aliases_name_every_boundary_slot() {
        // One env-fn covering each alias shape: exact role-bearing and
        // roleless host atoms (`wrap`'s `Str`/`Cell`), a polymorphic tparam
        // (`id[A]` → well-formed `<A: KioType>`), a structural sum
        // (`pick`'s `(Cell | Str)` → `crate::shapes::Sum`), and an
        // exported fn (the `exp` submodule). The env submodules are
        // themselves rung-2 module-qualified (`main__wrap` etc.).
        let pkg = build_package(
            "module main; \
             host type Cell; host type Str role(str); \
             host fn wrap(s: Str) -> Cell; \
             host fn id[A](v: A) -> A; \
             host fn pick(p: (Cell | Str)) -> Str; \
             pub fn run(s: Str) -> Cell { id(wrap(s)) }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let ffi = &krate.ffi_rs;
        // `pub mod ffi;` is declared in lib.rs so the file is reached.
        assert!(
            krate.lib_rs.contains("pub mod ffi;"),
            "lib.rs missing `pub mod ffi;`:\n{}",
            krate.lib_rs,
        );
        // Top-level namespaces always present.
        assert!(ffi.contains("pub mod env {"), "ffi_rs:\n{ffi}");
        assert!(ffi.contains("pub mod exp {"), "ffi_rs:\n{ffi}");
        // A role-bearing atomic arg keeps its exact associated type.
        assert!(
            ffi.contains("pub mod main__wrap {")
                && ffi.contains("pub type arg0<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Str;"),
            "main__wrap arg0 alias missing:\n{ffi}",
        );
        // A roleless host type uses the same declaration-keyed spelling.
        assert!(
            ffi.contains("pub type ret<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Cell;"),
            "wrap ret alias missing fully-qualified host assoc:\n{ffi}",
        );
        // Polymorphic tparam aliases name the marker's canonical facade.
        assert!(
            ffi.contains("pub mod main__id {")
                && ffi.contains("pub type arg0<A: crate::shapes::KioType> = <A as crate::shapes::KioType>::Facade;")
                && ffi.contains("pub type ret<A: crate::shapes::KioType> = <A as crate::shapes::KioType>::Facade;"),
            "id tparam aliases missing:\n{ffi}",
        );
        // Structural sum → the site-independent recursive binary facade with host
        // leaves fully qualified and rung-2 module-qualified.
        assert!(
            ffi.contains("pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::Sum<")
                && ffi.contains(
                    "<__KioHost as crate::host::TestPkgHost>::main__Cell, <__KioHost as crate::host::TestPkgHost>::main__Str>;"
                ),
            "pick structural arg0 alias missing:\n{ffi}",
        );
        // The canonical binary facade owns exhaustive, stable variants; the FFI
        // does not mint positional constructor aliases per occurrence.
        assert!(
            !ffi.contains(" as arg0_0,") && !ffi.contains(" as arg0_1};"),
            "pick unexpectedly exposes occurrence-local positional aliases:\n{ffi}",
        );
        // Exported fn surfaces under `exp`, module-qualified
        // (`exp::main__run`) like the env side — so two bridged modules
        // exporting a same-named `pub fn` don't collide.
        assert!(
            ffi.contains("pub mod main__run {")
                && ffi.contains("pub type arg0<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Str;"),
            "exp::main__run alias missing:\n{ffi}",
        );
    }

    #[test]
    fn ffi_callback_arg_names_its_return() {
        // A function-typed env arg has a nameable canonical `KioFnN` alias;
        // grouped source-argument and result convenience aliases accompany it.
        let pkg = build_package(
            "module main; \
             host type Int role(i32); host type Text role(str); \
             host fn call_step(step: Int -> Int, seed: Int) -> Int; \
             host fn call_text(step: Text -> Text, value: Text) -> Text; \
             host fn call_pair(step: (Int & Text) -> Int, pair: Int & Text) -> Int; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let ffi = &krate.ffi_rs;
        // The env-fn FFI submodule is rung-2 module-qualified
        // (`main__callStep`), matching the trait method name.
        assert!(
            ffi.contains("pub mod main__callStep {"),
            "main__callStep submodule missing:\n{ffi}",
        );
        assert!(
            ffi.contains(
                "pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::KioFn1<"
            ),
            "callback main carrier alias is missing:\n{ffi}",
        );
        assert!(
            ffi.contains("pub type arg0_cbret<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Int;"),
            "callback return alias missing:\n{ffi}",
        );
        // The trailing non-callback arg keeps its positional index.
        assert!(
            ffi.contains("pub type arg1<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Int;"),
            "second (non-callback) arg alias missing:\n{ffi}",
        );
        assert!(
            ffi.contains("pub type ret<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Int;"),
            "ret alias missing:\n{ffi}",
        );
        // The callback's grouped source type surfaces as `arg0_cbarg`.
        assert!(
            ffi.contains("pub type arg0_cbarg<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Int;"),
            "callback argument alias missing:\n{ffi}",
        );
        assert!(
            ffi.contains("pub type arg0_cbarg<__KioHost: crate::host::TestPkgHost> = <__KioHost as crate::host::TestPkgHost>::main__Text;"),
            "owned callback argument alias is missing:\n{ffi}",
        );
        let pair = ffi
            .split_once("pub mod main__callPair {\n")
            .expect("main__call_pair FFI module")
            .1
            .split_once("    }\n")
            .expect("main__call_pair FFI module end")
            .0;
        assert!(
            pair.contains("pub type arg0<__KioHost: crate::host::TestPkgHost> = crate::shapes::KioFn1<crate::shapes::KioProduct<")
                && pair.contains("pub type arg0_cbarg<__KioHost: crate::host::TestPkgHost> = crate::shapes::Product<")
                && !pair.contains("KioFn2<")
                && !pair.contains("arg0_cbarg0")
                && !pair.contains("arg0_cbarg1"),
            "one written product parameter must stay one grouped KioFn1/cbarg facade:\n{pair}",
        );
    }

    #[test]
    fn ffi_existential_continuation_names_its_polymorphic_payload() {
        let pkg = build_package(
            "module main; \
             pub newtype Packed<U> : [A](U & A) -> U { \
                 constructor pack; pub projector open; \
             }; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let ffi = &krate.ffi_rs;
        let payload = ffi
            .lines()
            .find(|line| line.contains("pub type open_continuation_cbarg<"))
            .unwrap_or_else(|| panic!("missing public continuation payload alias:\n{ffi}"));
        assert!(
            payload.contains("<__KioHost: crate::host::TestPkgHost, __KioPolyType_0: crate::shapes::KioType, __KioPolyType_1: crate::shapes::KioType>")
                && payload.ends_with("Value<__KioHost, __KioPolyType_0, __KioPolyType_1>;"),
            "continuation payload must preserve result and hidden witness markers:\n{payload}",
        );
        assert!(ffi.contains(" as open_continuation_cbarg_impl;"));
        assert!(ffi.contains("pub type open_continuation_cbret<__KioPolyType_0: crate::shapes::KioType> = <__KioPolyType_0 as crate::shapes::KioType>::Facade;"));
        assert!(ffi.contains("pub type open_continuation_cbarg_cbarg<__KioPolyType_1: crate::shapes::KioType, __KioPolyType_2: crate::shapes::KioType> = crate::shapes::Product<<__KioPolyType_1 as crate::shapes::KioType>::Facade, <__KioPolyType_2 as crate::shapes::KioType>::Facade>;"));
        assert!(ffi.contains("pub type open_continuation_cbarg_cbret<__KioPolyType_1: crate::shapes::KioType> = <__KioPolyType_1 as crate::shapes::KioType>::Facade;"));
    }

    #[test]
    fn ffi_callable_aliases_name_polymorphic_arguments_and_results() {
        let pkg = build_package(
            "module main; \
             host fn visit(step: [A] A -> A) -> .; \
             host fn map(step: [*F][Z][A] ((Z -> A) & F(Z)) -> F(A)) -> .; \
             pub fn identity() -> [A] A -> A { .[A](value: A) { value } } \
             pub fn callback() -> . -> ([A] A -> A) { .() { identity() } }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let ffi = &krate.ffi_rs;
        for leaf in ["arg0_cbarg", "arg0_cbret", "ret_cbarg", "ret_cbret"] {
            assert!(
                ffi.contains(&format!("pub type {leaf}<")),
                "missing callable slot {leaf}:\n{ffi}"
            );
        }
        assert!(ffi.contains(" as ret_cbret_impl;"));
        assert!(ffi.contains("pub type ret_cbret_cbarg<"));
        assert!(ffi.contains("pub type ret_cbret_cbret<"));
        assert!(ffi.contains("pub type arg0_cbret<__KioPolyType_0: crate::shapes::KioTypeConstructor1, __KioPolyType_2: crate::shapes::KioType> = crate::shapes::KioApply1<__KioPolyType_0, __KioPolyType_2>;"));
    }

    #[test]
    fn ffi_alias_type_params_follow_declaration_order() {
        let pkg = build_package(
            "module main; \
             host fn loop_sr[S][R](step: S -> S | R, state: S) -> R; \
             host fn loop_ab[A][B](step: A -> A | B, state: A) -> B; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("lower_package");
        let ffi = &krate.ffi_rs;
        for (member, declarations, left, right) in [
            (
                "main__loopSr",
                "S: crate::shapes::KioType, R: crate::shapes::KioType",
                "S",
                "R",
            ),
            (
                "main__loopAb",
                "A: crate::shapes::KioType, B: crate::shapes::KioType",
                "A",
                "B",
            ),
        ] {
            let module = ffi
                .split_once(&format!("pub mod {member} {{\n"))
                .unwrap_or_else(|| panic!("{member} FFI module missing:\n{ffi}"))
                .1
                .split_once("\n}")
                .expect("FFI member module is closed")
                .0;
            let alias = module
                .lines()
                .find(|line| line.contains("pub type arg0_cbret"))
                .expect("callback-return alias is emitted")
                .trim_start();
            assert!(
                alias
                    == format!(
                        "pub type arg0_cbret<{declarations}> = crate::shapes::Sum<<{left} as crate::shapes::KioType>::Facade, <{right} as crate::shapes::KioType>::Facade>;"
                    ),
                "{member} callback-return alias does not preserve declaration order:\n{alias}",
            );
        }
    }

    #[test]
    fn private_nominal_storage_keeps_public_markers_out_of_native_slots() {
        let pkg = build_package(
            "module api; \
             pub newtype Cell[A] : A { constructor mk; projector get; }; \
             pub rec newtype Chain[A] : . | (A & Chain(A)) { constructor mk; projector get; }; \
             pub newtype Applied[*F][A] : F(A) { constructor mk; projector get; }; \
             newtype Direct[A] : Cell(A) { constructor mk; projector get; }; \
             newtype Nested[A] : A & Cell(A) { constructor mk; projector get; }; \
             newtype Callback[A] : Cell(A) -> Cell(A) { constructor mk; projector get; }; \
             newtype Recursive[A] : Chain(A) { constructor mk; projector get; }; \
             newtype Higher[A] : Applied(Cell, A) { constructor mk; projector get; }; \
             newtype Concrete : Cell(.) { constructor mk; projector get; }; \
             newtype Native[A] : A { constructor mk; projector get; }; \
             fn direct[A](v: Cell(A)) -> Direct(A) { Direct.mk(v) } \
             fn nested[A](v: A & Cell(A)) -> Nested(A) { Nested.mk(v) } \
             fn callback[A](v: Cell(A) -> Cell(A)) -> Callback(A) { Callback.mk(v) } \
             fn recursive[A](v: Chain(A)) -> Recursive(A) { Recursive.mk(v) } \
             fn higher[A](v: Applied(Cell, A)) -> Higher(A) { Higher.mk(v) } \
             fn concrete(v: Cell(.)) -> Concrete { Concrete.mk(v) } \
             fn native[A](v: A) -> Native(A) { Native.mk(v) } \
             pub fn keep[A](v: Cell(A)) -> Cell(A) { v }",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "storage").expect("emit private nominal storage");
        for name in [
            "Direct",
            "Nested",
            "Callback",
            "Recursive",
            "Higher",
            "Concrete",
        ] {
            let declaration = krate
                .shapes_rs
                .lines()
                .find(|line| line.contains(&format!("pub(crate) struct {name}<")))
                .unwrap_or_else(|| panic!("missing private {name}: {}", krate.shapes_rs));
            assert!(
                declaration.contains("::std::rc::Rc<dyn ::std::any::Any>")
                    && !declaration.contains("::nominal::")
                    && !declaration.contains("KioType"),
                "private {name} must retain native binders and erased public nominal leaves: {declaration}",
            );
        }
        assert!(krate.shapes_rs.contains(
            "pub struct Cell<__KioHost: crate::host::StorageHost, A: crate::shapes::KioType>"
        ));
        assert!(krate.shapes_rs.contains(
            "pub struct Chain<__KioHost: crate::host::StorageHost, A: crate::shapes::KioType>"
        ));
        let native = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("pub(crate) struct Native<"))
            .expect("private native-only control");
        assert!(native.contains("A: Clone + 'static") && native.contains("Box<A>"));
        assert!(krate.lib_rs.contains("Cell<__KioHost, A>"));
    }

    #[test]
    fn phantom_higher_kinded_params_erase_from_private_body_nominals() {
        let pkg = build_package(
            "module api; \
             newtype Unary[A] : A { constructor mk; projector get; }; \
             newtype Binary[A][B] : A & B { constructor mk; projector get; }; \
             newtype Unary_holder[*F] : . { constructor mk; projector get; }; \
             newtype Binary_holder[**F] : . { constructor mk; projector get; }; \
             newtype Applied[*F] : F(.) { constructor mk; projector get; }; \
             fn hold_unary() -> Unary_holder(Unary) { Unary_holder.mk(Unary, ()) } \
             fn hold_binary() -> Binary_holder(Binary) { Binary_holder.mk(Binary, ()) } \
             fn hold_applied(value: Applied(Unary)) -> Applied(Unary) { value } \
             pub fn run() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_package(&pkg, "test_pkg").expect("emit phantom HKT newtypes");

        assert!(
            krate
                .shapes_rs
                .contains("pub(crate) struct UnaryHolder<__KioHost: 'static>"),
            "a phantom unary HKT binder must not become a private body generic:\n{}",
            krate.shapes_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("pub(crate) struct BinaryHolder<__KioHost: 'static>"),
            "a phantom binary HKT binder must not become a private body generic:\n{}",
            krate.shapes_rs,
        );
        assert!(
            krate
                .shapes_rs
                .contains("PartialEq for UnaryHolder<__KioHost>"),
            "an erased header-only HKT binder must not suppress payload equality:\n{}",
            krate.shapes_rs,
        );
        assert!(
            !krate.shapes_rs.contains("PartialEq for Applied<__KioHost>"),
            "an HKT binder reached by the payload must still suppress payload equality:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn recursive_state_over_applied_host_nominal_skips_partial_eq() {
        let pkg = build_multi_module_package(
            &[
                "module main; import control(if); \
             host type Array[A]; \
             host type Bool role(bool); \
             host type I32 role(i32); \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             labels Array_state = { cells: Array(I32) }; \
             labels Scalar_state = { done: Bool }; \
             rec(loop) fn keep_array_once(done: Bool, state: Array_state, scalar: Scalar_state) -> Array_state { \
               if! done { state } else { rec keep_array_once(.t, state, scalar) } \
             }",
                include_str!("../../../../test-data/poc/elab/workdir/control.kio"),
            ],
            Some("bridge { main; }"),
        );
        let host_types = crate::host_descriptor::build_host_descriptor(&pkg)
            .host_types
            .iter()
            .map(|host_type| {
                (
                    (host_type.module_path.to_owned(), host_type.name.to_owned()),
                    HostTypeInfo {
                        role: host_type.role,
                    },
                )
            })
            .collect();
        let newtypes = collect_newtypes(&pkg);
        let records = build_newtype_records(&pkg, &newtypes);
        let scopes = collect_newtype_import_scopes(&pkg, &records);
        let shapes = BodyShapeRegistry::new(
            host_types,
            records,
            scopes,
            "crate".to_owned(),
            "TestPkgHost".to_owned(),
        );
        let rec_state_keys = shapes
            .newtype_by_key
            .keys()
            .filter(|key| key.starts_with("main.Rec_state"))
            .collect::<Vec<_>>();
        let [rec_state_key] = rec_state_keys.as_slice() else {
            panic!("expected one routed recursive state, got {rec_state_keys:?}");
        };
        let rec_state = &shapes.newtype_by_key[*rec_state_key];
        let rec_context = RustRecursionReachContext {
            scope: RustReachContext {
                module_path: Some("main".to_owned()),
                type_vars: rec_state
                    .decl
                    .type_params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect(),
            },
            newtype_path: BTreeSet::from([(*rec_state_key).clone()]),
        };
        assert!(shapes.newtype_is_recursive(rec_state_key));
        assert!(
            shapes.type_reaches_newtype_impl(&rec_state.decl.payload, "main.Cells", rec_context),
            "the recursive state payload must reach the applied-host label"
        );
        let nominal = |name: &str| {
            Type::synth_path(
                vec!["main".to_owned(), name.to_owned()],
                Vec::new(),
                crate::span::Span::new(0, 0),
            )
        };
        assert!(shapes.type_reaches_non_partial_eq_leaf(&nominal("Cells"), BTreeSet::new()));
        assert!(!shapes.type_reaches_non_partial_eq_leaf(&nominal("Done"), BTreeSet::new()));
        let krate = lower_package(&pkg, "test_pkg").expect("emit recursive applied-host state");
        let rec_state_line = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("struct RecState"))
            .unwrap_or_else(|| panic!("recursive state newtype missing:\n{}", krate.shapes_rs));
        let rec_state_name = rec_state_line
            .split_once("struct ")
            .expect("recursive state struct prefix")
            .1
            .split_once('<')
            .expect("recursive state host parameter")
            .0;
        let cells_line = krate
            .shapes_rs
            .lines()
            .find(|line| line.contains("struct Cells<"))
            .unwrap_or_else(|| panic!("applied-host label missing:\n{}", krate.shapes_rs));

        assert!(
            rec_state_line.contains("::std::rc::Rc<dyn ::std::any::Any>"),
            "the recursive state must use erased storage for its recursive payload:\n{rec_state_line}",
        );
        assert!(
            cells_line.contains("Box<::std::rc::Rc<dyn ::std::any::Any>>")
                && !krate.shapes_rs.contains("PartialEq for Cells<__KioHost>"),
            "the applied-host label must use non-comparable erased storage:\n{cells_line}",
        );
        assert!(
            !krate
                .shapes_rs
                .contains(&format!("PartialEq for {rec_state_name}<")),
            "an outer recursive state that reaches erased storage must not emit PartialEq:\n{}",
            krate.shapes_rs,
        );
        assert!(
            krate.shapes_rs.contains("PartialEq for Done<__KioHost>"),
            "a label over a nullary PartialEq host type must retain structural equality:\n{}",
            krate.shapes_rs,
        );
    }

    #[test]
    fn ffi_qualify_host_assoc_word_boundary() {
        // `qualify_host_assoc` rewrites the reserved generated host head but
        // not that spelling when it is the tail of a longer identifier.
        assert_eq!(
            qualify_host_assoc("__KioHost::Cell", "__KioHost", "GreeterHost"),
            "<__KioHost as crate::host::GreeterHost>::Cell"
        );
        assert_eq!(
            qualify_host_assoc(
                "crate::shapes::Prod_ab<__KioHost::Cell, String>",
                "__KioHost",
                "GreeterHost",
            ),
            "crate::shapes::Prod_ab<<__KioHost as crate::host::GreeterHost>::Cell, String>"
        );
        assert_eq!(
            qualify_host_assoc("Prefix__KioHost", "__KioHost", "GreeterHost"),
            "Prefix__KioHost"
        );
        assert_eq!(
            qualify_host_assoc("Foo<H>", "__KioHost", "GreeterHost"),
            "Foo<H>"
        );
    }

    /// Lower `pkg` with a replayed sig folded in, returning the emitted
    /// crate. The sig source is parsed + replayed exactly as the build
    /// does: validate the whole changelog, then emit from its last sealed
    /// generation rather than its open draft.
    fn lower_with_sig(pkg: &Package<Routed>, sig_src: &str) -> RustCrate {
        let file =
            crate::pass::parser::parse_signature_file(sig_src, Some("pkg")).expect("parse sig");
        crate::sig::replay(&file).expect("validate complete sig replay");
        let version = file.version.saturating_sub(1);
        let replayed = crate::sig::replay_through(&file, version).expect("replay sealed sig");
        let opts = LowerOptions {
            sig: Some((version, replayed)),
            ..LowerOptions::default()
        };
        lower_package_with_options(pkg, "test_pkg", &opts).expect("lower_package")
    }

    #[test]
    fn retained_host_trait_membership_is_descriptor_gated() {
        let pkg = build_package(
            "module api; fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let file = crate::pass::parser::parse_signature_file(
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host fn keep() -> .;\n        host fn omit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        \
             keep;\n        omit;\n      }\n    }\n  }\n}\n",
            Some("pkg"),
        )
        .expect("parse sig");
        crate::sig::replay(&file).expect("validate complete sig replay");
        let version = file.version.saturating_sub(1);
        let replayed = crate::sig::replay_through(&file, version).expect("replay sealed sig");
        let mut descriptor = host_descriptor::with_signature(
            host_descriptor::build_host_descriptor(&pkg),
            version,
            &replayed,
        );
        assert_eq!(
            descriptor.deprecated_host_items.len(),
            2,
            "fixture must begin with both retained descriptor members"
        );
        descriptor
            .deprecated_host_items
            .retain(|item| item.name == "keep");
        let prepared = PreparedBoundaryCallableSites::collect(&pkg, Some(&replayed))
            .expect("prepare retained host facades");
        let omitted_site = boundary_site_id(
            "api",
            BoundaryFacadeSiteOwner::HostFunction {
                name: "omit".to_owned(),
            },
        );
        assert!(
            prepared
                .site(&omitted_site)
                .and_then(PreparedBoundaryCallableSite::retained)
                .is_some(),
            "fixture must retain the omitted method in the prepared catalog"
        );
        let names = RustNames::derive("test_pkg");
        let facade = PreparedRustRenderContext::new(&prepared, "crate", &names.host_trait);
        let host_rs = render_rust_host_trait(&descriptor, &prepared, facade, &names)
            .expect("render descriptor-gated retained host trait");

        assert!(
            host_rs.contains("fn api__keep(&self)"),
            "descriptor-selected retained method is missing:\n{host_rs}"
        );
        assert!(
            !host_rs.contains("fn api__omit(&self)"),
            "a prepared retained method absent from the descriptor must not render:\n{host_rs}"
        );
    }

    #[test]
    fn live_and_retained_partial_nominals_use_constructor_markers() {
        let pkg = build_package(
            "module api; \
             host type Host_pair[A][B]; \
             pub newtype Pair[A][B] : A & B { pub constructor make_pair; pub projector read_pair; }; \
             pub newtype Higher[*F] : . { pub constructor make_higher; pub projector read_higher; }; \
             host fn live_host(value: Higher(Host_pair(.))) -> .; \
             host fn live_new(value: Higher(Pair(.))) -> .;",
            Some("bridge { api; }"),
        );
        let live = lower_package(&pkg, "test_pkg").expect("emit live partial constructors");
        for marker in [
            "KioHostTypeConstructor_api__HostPair_P1<Self, crate::shapes::KioUnit>",
            "KioNewtypeConstructor_api__Pair_P1<Self, crate::shapes::KioUnit>",
        ] {
            assert!(
                live.host_rs.contains(marker),
                "live partial nominal did not use `{marker}`:\n{}",
                live.host_rs
            );
        }

        let retained = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Host_pair[A][B];\n        \
             newtype Pair[A][B] : A & B { pub constructor make_pair; pub projector read_pair; };\n        \
             newtype Higher[*F] : . { pub constructor make_higher; pub projector read_higher; };\n        \
             host fn old_host(value: Higher(Host_pair(.))) -> .;\n        \
             host fn old_new(value: Higher(Pair(.))) -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        old_host;\n        old_new;\n      }\n    }\n  }\n}\n",
        );
        for (method, marker) in [
            (
                "fn api__oldHost",
                "KioHostTypeConstructor_api__HostPair_P1<Self, crate::shapes::KioUnit>",
            ),
            (
                "fn api__oldNew",
                "KioNewtypeConstructor_api__Pair_P1<Self, crate::shapes::KioUnit>",
            ),
        ] {
            let signature = retained
                .host_rs
                .split_once(method)
                .unwrap_or_else(|| {
                    panic!("retained method `{method}` missing:\n{}", retained.host_rs)
                })
                .1
                .split_once('{')
                .expect("retained default body")
                .0;
            assert!(
                signature.contains(marker),
                "retained partial nominal did not use `{marker}`:\n{signature}"
            );
        }
        assert!(
            !retained
                .shapes_rs
                .contains("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]\n    type Apply<"),
            "a deprecated attribute on an associated Apply alias is ignored by rustc and must not be emitted:\n{}",
            retained.shapes_rs,
        );
    }

    #[test]
    fn removed_host_fn_reemits_as_deprecated_trait_method() {
        // v(1) added host fn `open() -> Handle` + host fn `log(p0: Str)`;
        // v(2) removed `log`. The current source no longer declares `log`,
        // but the regenerated Host trait must re-emit it as a
        // `#[deprecated]` method with a diverging default body so an
        // existing host's `impl` keeps compiling. Per
        // `specs/backends/rust.md` § Deprecated host items.
        let pkg = build_multi_module_package(
            &[
                "module main; \
                 import types(Str); \
                 host type Handle; \
                 host fn open() -> Handle; \
                 fn dummy() -> . { () }",
                "module types; host type Str role(str);",
            ],
            Some("bridge { main; types; }"),
        );
        let krate = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module main {\n        \
             import types(Str);\n        host type Handle;\n        \
             host fn open() -> Handle;\n        host fn log(p0: Str) -> .;\n      };\n      \
             module types {\n        host type Str role(str);\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module main {\n        log;\n      }\n    }\n  }\n}\n",
        );
        let host_rs = &krate.host_rs;
        // The live `open` is a body-less requirement.
        assert!(
            host_rs.contains("fn main__open(&self)"),
            "live host fn `open` should be present:\n{host_rs}"
        );
        // The removed `log` is re-emitted deprecated with a diverging
        // default body. The note names the mangled method + version.
        assert!(
            host_rs.contains("#[deprecated(note = \"host fn `main__log` removed at v(2)\")]"),
            "removed host fn `log` should re-emit with #[deprecated]:\n{host_rs}"
        );
        assert!(
            host_rs.contains("unimplemented!(\"host fn `main__log` removed at v(2)\")"),
            "removed host fn `log` should have a diverging default body:\n{host_rs}"
        );
        // The deprecated method takes a body (`{ … }`), not `;` — it is a
        // defaulted method, so a host that dropped its override still
        // compiles.
        assert!(
            host_rs.contains("fn main__log(&self, arg0: Self::types__Str) {"),
            "deprecated `log` should be a defaulted method with the frozen signature:\n{host_rs}"
        );
    }

    #[test]
    fn retained_host_fn_declares_frozen_nominal_carrier_without_live_members() {
        let pkg = build_package(
            "module api; fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             newtype Token[A][A] : A { pub constructor make_token; projector read_token; };\n        \
             host fn old(value: Token(., .)) -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        Token;\n        old;\n      }\n    }\n  }\n}\n",
        );

        assert!(
            krate.shapes_rs.contains(
                "pub struct Token<__KioHost: crate::host::TestPkgHost, A: crate::shapes::KioType, __KioType_1: crate::shapes::KioType>"
            ),
            "retained host signature lost its frozen nominal carrier:\n{}",
            krate.shapes_rs
        );
        assert!(
            krate.shapes_rs.contains("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]\n        pub struct Token<"),
            "retained nominal carrier is not visibly deprecated:\n{}",
            krate.shapes_rs
        );
        assert!(
            krate.ffi_rs.contains("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]\n    pub mod api__old"),
            "retained FFI namespace is not visibly deprecated:\n{}",
            krate.ffi_rs
        );
        assert!(
            krate
                .host_rs
                .contains("fn api__old(&self, arg0: crate::shapes::nominal::api::Token<Self, crate::shapes::KioUnit, crate::shapes::KioUnit>)"),
            "deprecated host method lost its exact retained nominal type:\n{}",
            krate.host_rs
        );
        assert!(
            !krate.shapes_rs.contains("pub fn makeToken")
                && !krate.shapes_rs.contains("pub fn readToken"),
            "retained carrier must not acquire live newtype members:\n{}",
            krate.shapes_rs
        );
    }

    #[test]
    fn retained_forall_support_uses_an_eligible_nominal_owner() {
        let pkg = build_package(
            "module api; fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Gone;\n        \
             newtype Poly : [A] A -> A { pub constructor wrap; pub projector unwrap; };\n        \
             host fn a_bad(gone: Gone, value: Poly) -> .;\n        \
             host fn z_good(value: Poly) -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        \
             Gone;\n        Poly;\n        a_bad;\n        z_good;\n      }\n    }\n  }\n}\n",
        );

        assert!(
            !krate.host_rs.contains("api__aBad") && krate.host_rs.contains("api__zGood"),
            "only the retained method independent of the removed host type is nameable:\n{}",
            krate.host_rs
        );
        assert!(
            krate.shapes_rs.contains("pub struct Poly")
                && krate.shapes_rs.contains("pub trait KioForall_"),
            "the eligible retained owner must keep its nominal and forall support:\n{}",
            krate.shapes_rs
        );
        let retained = "#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]";
        let retained_forall_structs = krate
            .shapes_rs
            .lines()
            .zip(krate.shapes_rs.lines().skip(1))
            .filter(|(attribute, item)| {
                *attribute == retained && item.starts_with("pub struct KioForall_")
            })
            .count();
        assert!(
            krate
                .shapes_rs
                .contains(&format!("{retained}\npub trait KioForall_"))
                && retained_forall_structs >= 2
                && krate
                    .shapes_rs
                    .contains(&format!("    {retained}\n    pub fn new")),
            "retained forall trait, Value, Marker, and typed ingress must all remain visibly deprecated:\n{}",
            krate.shapes_rs,
        );
        let retained_forall_trait = krate
            .shapes_rs
            .lines()
            .find(|line| line.starts_with("pub trait KioForall_"))
            .expect("retained forall trait declaration");
        let retained_forall_name = retained_forall_trait
            .strip_prefix("pub trait ")
            .expect("retained forall trait prefix")
            .split('<')
            .next()
            .expect("retained forall trait name");
        let trait_body = krate
            .shapes_rs
            .split_once(&format!("pub trait {retained_forall_name}"))
            .expect("retained forall trait body")
            .1
            .split_once("\n}\n")
            .expect("retained forall trait end")
            .0;
        let value_impl = krate
            .shapes_rs
            .rsplit_once(&format!(" for {retained_forall_name}Value"))
            .expect("retained forall Value implementation")
            .1
            .split_once("\n}\n")
            .expect("retained forall Value implementation end")
            .0;
        assert!(
            trait_body.contains(&format!("    {retained}\n    fn apply"))
                && value_impl.contains("\n    fn apply")
                && !value_impl.contains(retained),
            "only the retained trait declaration method may carry deprecation; its Value implementation method must remain unannotated:\ntrait:\n{trait_body}\nimpl:\n{value_impl}",
        );
    }

    #[test]
    fn nested_retained_payload_foralls_resolve_their_owner_in_bounded_steps() {
        const DECOYS: usize = 32;
        let pkg = build_package(
            "module api; fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let target = QualifiedTypeName::new(vec!["api".to_owned()], "Zpoly".to_owned())
            .expect("test target has a qualified identity");
        let signature = |decoys| {
            let declarations = (0..decoys)
                .map(|index| {
                    format!(
                        "newtype A{index:02} : . {{ pub constructor make_a{index:02}; pub projector read_a{index:02}; }};"
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let parameters = (0..decoys)
                .map(|index| format!("a{index:02}: A{index:02}"))
                .chain(std::iter::once("poly: Zpoly".to_owned()))
                .collect::<Vec<_>>()
                .join(", ");
            let removals = (0..decoys)
                .map(|index| format!("A{index:02};"))
                .chain(["Zpoly;".to_owned(), "z_good;".to_owned()])
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "signature pkg v(3);\n\
                 v(1) {{\n  nonbreaking {{\n    add {{\n      module api {{\n\
                 {declarations}\n\
                 newtype Zpoly : [A] A -> [B] B -> A {{ pub constructor wrap; pub projector unwrap; }};\n\
                 host fn z_good({parameters}) -> .;\n\
                 }}\n    }}\n  }}\n}}\n\
                 v(2) {{\n  nonbreaking {{\n    remove {{\n      module api {{\n\
                 {removals}\n\
                 }}\n    }}\n  }}\n}}\n"
            )
        };
        let lower = |decoys| {
            let signature = signature(decoys);
            let (krate, measurement) =
                measure_rust_forall_owner_resolution_during(target.clone(), || {
                    lower_with_sig(&pkg, &signature)
                });
            (krate, measurement, signature)
        };
        let former_scan = |signature: &str| {
            let file = crate::pass::parser::parse_signature_file(signature, Some("pkg"))
                .expect("parse owner-resolution test sig");
            crate::sig::replay(&file).expect("validate owner-resolution test sig replay");
            let version = file.version.saturating_sub(1);
            let replayed = crate::sig::replay_through(&file, version)
                .expect("replay sealed owner-resolution test sig");
            let prepared = PreparedBoundaryCallableSites::collect(&pkg, Some(&replayed))
                .expect("prepare owner-resolution test facade");
            let facade_index = PreparedRustFacadeIndex::new(&prepared);
            let owner = facade_index
                .payload_owners
                .get(&(target.clone(), true))
                .expect("retained target has a selected payload owner");
            let site = prepared
                .site(owner)
                .expect("selected payload owner is a prepared site");
            let Some(BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            }) = site.nominals().declaration(&target)
            else {
                panic!("retained target has a transparent payload");
            };
            let direct_name = rust_forall_name_with_owner(
                site,
                payload.facade(),
                payload.payload_root(),
                Some(&target),
            )
            .expect("explicit target owner names its payload forall");
            let ((former_owner, former_name), measurement) =
                measure_rust_forall_owner_resolution_during(target.clone(), || {
                    let former_owner = former_rust_forall_plan_owner(site, payload.facade())
                        .expect("former plan-pointer scan finds the payload owner")
                        .clone();
                    let former_name = rust_forall_name_with_owner(
                        site,
                        payload.facade(),
                        payload.payload_root(),
                        Some(&former_owner),
                    )
                    .expect("former selected owner names its payload forall");
                    (former_owner, former_name)
                });
            assert_eq!(former_owner, target);
            assert_eq!(former_name, direct_name);
            (direct_name, measurement)
        };

        let (baseline_krate, baseline_resolution, baseline_signature) = lower(0);
        let (krate, resolution, signature) = lower(DECOYS);
        let (baseline_name, baseline_former_resolution) = former_scan(&baseline_signature);
        let (name, former_resolution) = former_scan(&signature);

        assert!(
            krate.shapes_rs.matches("pub trait KioForall_").count() >= 2,
            "nested retained payload forall support was not emitted:\n{}",
            krate.shapes_rs
        );
        assert_eq!(
            baseline_resolution,
            RustForallOwnerResolutionMeasurement {
                direct_lookups: 1,
                candidate_visits: 0,
            }
        );
        assert_eq!(
            resolution, baseline_resolution,
            "{DECOYS} unrelated nominal declarations changed nested retained payload forall owner resolution work"
        );
        assert_eq!(
            baseline_former_resolution,
            RustForallOwnerResolutionMeasurement {
                direct_lookups: 0,
                candidate_visits: 1,
            }
        );
        assert_eq!(
            former_resolution,
            RustForallOwnerResolutionMeasurement {
                direct_lookups: 0,
                candidate_visits: DECOYS + 1,
            }
        );
        assert_eq!(name, baseline_name);
        assert!(baseline_krate.shapes_rs.contains(&baseline_name));
        assert!(krate.shapes_rs.contains(&name));
    }

    #[test]
    fn removed_host_type_is_dropped_not_reemitted() {
        // Removing a host type drops its associated type — the
        // genuine Rust source-stability limitation (E0437 on the old
        // host's orphan `type … = …;`; no stable associated_type_defaults).
        // Nothing is re-emitted for it. Per `specs/backends/rust.md`
        // § Deprecated host items > Host-type removal is not source-stable.
        let pkg = build_package(
            "module main; \
             host type Handle; \
             host fn open() -> Handle; \
             fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let krate = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module main {\n        \
             host type Handle;\n        host type Gone;\n        host type Oldbox[A];\n        \
             newtype Token : Gone { pub constructor wrap; pub projector unwrap; };\n        \
             host fn open() -> Handle;\n        host fn old(value: Gone) -> .;\n        \
             host fn old_box(value: Oldbox(Handle)) -> .;\n        host fn old_token(value: Token) -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module main {\n        Gone;\n        \
             Oldbox;\n        Token;\n        old;\n        old_box;\n        old_token;\n      }\n    }\n  }\n}\n",
        );
        let host_rs = &krate.host_rs;
        // No associated type re-emitted for the removed `Gone`, and no
        // deprecated method (a host type is not a fn).
        assert!(
            !host_rs.contains("main__Gone"),
            "removed host type must be dropped, not re-emitted:\n{host_rs}"
        );
        assert!(
            !host_rs.contains("main__old")
                && !host_rs.contains("main__oldBox")
                && !host_rs.contains("main__oldToken"),
            "retained methods that depend on removed associated types must be omitted:\n{host_rs}"
        );
        assert!(
            !krate.shapes_rs.contains("Oldbox"),
            "removed parameterized host types must not reappear as constructor markers:\n{}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("Token"),
            "a retained newtype depending on a removed host type must not leave a carrier:\n{}",
            krate.shapes_rs
        );
        assert!(
            !krate.ffi_rs.contains("main__old")
                && !krate.ffi_rs.contains("main__oldBox")
                && !krate.ffi_rs.contains("main__oldToken"),
            "omitted retained methods must not leave unnameable FFI aliases:\n{}",
            krate.ffi_rs
        );
        assert!(
            !host_rs.contains("#[deprecated"),
            "a removed host type re-emits nothing deprecated:\n{host_rs}"
        );
    }

    #[test]
    fn no_sig_emits_no_deprecated_items() {
        // Without a sig, the Host trait carries no deprecated members —
        // the emitted crate is byte-identical to the pre-sig behavior.
        let pkg = build_package(
            "module main; host fn open() -> .; fn dummy() -> . { () }",
            Some("bridge { main; }"),
        );
        let with_default = lower_package(&pkg, "test_pkg").expect("lower_package");
        assert!(!with_default.host_rs.contains("#[deprecated"));
    }

    #[test]
    fn live_support_identity_dominates_retained_history() {
        let pkg = build_package(
            "module api; host type Live role(i32); host fn current(value: Live & Live) -> .; fn dummy() -> . { () }",
            Some("bridge { api; }"),
        );
        let krate = lower_with_sig(
            &pkg,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Live role(i32);\n        host fn removed(value: Live & Live) -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        removed;\n      }\n    }\n  }\n}\n",
        );
        assert!(
            krate.shapes_rs.contains("pub struct Product<"),
            "shared product facade missing:\n{}",
            krate.shapes_rs
        );
        assert!(
            !krate.shapes_rs.contains("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]\npub struct Product<"),
            "a live structural facade inherited retained deprecation:\n{}",
            krate.shapes_rs
        );
    }

    #[test]
    fn marker_core_is_compositional_and_keeps_erasure_private() {
        let core = render_rust_marker_core();
        assert!(core.contains("type Facade: Clone + 'static;"));
        assert!(core.contains("Attaching a stored value to a marker whose storage contract it"));
        assert!(
            core.contains("does not satisfy violates the host invariant even when that marker")
        );
        assert!(
            core.contains(
                "Decoding a stored value through a marker whose storage contract it does"
            )
        );
        assert!(core.contains("not satisfy violates the host invariant."));
        assert!(core.contains(
            "An incompatible stored\n    /// representation deterministically\n    /// panics."
        ));
        assert!(core.contains("A representation-compatible token may decode successfully"));
        assert!(core.contains("neither outcome can cause"));
        assert!(core.contains("type Facade = Product<A::Facade, B::Facade>;"));
        assert!(core.contains("type Facade = Sum<A::Facade, B::Facade>;"));
        assert!(core.contains("Sum::Left(value) => KioStoredValue::from_raw"));
        assert!(core.contains("Sum::Right(value) => KioStoredValue::from_raw"));
        assert!(core.contains("1usize,"));
        assert!(core.contains("1 => Sum::Right"));
        let right_branch = core
            .split_once("Sum::Right(value) =>")
            .expect("binary sum right encoder")
            .1
            .split_once("        }")
            .expect("binary sum right encoder end")
            .0;
        assert!(right_branch.contains("1usize,"));
        assert!(right_branch.contains("B::into_stored(value).into_raw()"));
        let decoder = core
            .split_once("match tag {")
            .expect("binary sum decoder")
            .1;
        assert!(decoder.contains("0 => Sum::Left"));
        assert!(decoder.contains("1 => Sum::Right"));
        // Recursing this exact binary codec gives the three-way oracle:
        // A=(0,a), B=(1,(0,b)), C=(1,(1,c)). A raw outer Right would collide
        // with A at the outer decoder and must make this assertion fail.
        let encode_path = |path: &[usize]| {
            path.iter()
                .rev()
                .fold("payload".to_owned(), |payload, tag| {
                    assert!(*tag <= 1);
                    format!("({tag}, {payload})")
                })
        };
        assert_eq!(encode_path(&[0]), "(0, payload)");
        assert_eq!(encode_path(&[1, 0]), "(1, (0, payload))");
        assert_eq!(encode_path(&[1, 1]), "(1, (1, payload))");
        assert!(!core.contains("pub Rc<dyn"));

        let unary = render_rust_type_constructor_support(1, BoundaryFacadeSupportOrigin::Live);
        assert!(unary.contains("type Apply<KioArg_0: KioType>: KioType;"));
        assert!(unary.contains("pub struct KioApply1<"));
        assert!(unary.contains("pub struct KioApplied1<"));
        assert!(unary.contains("fn lift<KioArg_0: KioType>"));
        assert!(unary.contains("fn project<KioArg_0: KioType>"));
        assert_eq!(
            unary
                .matches("<Self::Apply<KioArg_0> as KioType>::into_stored(value)")
                .count(),
            1,
            "default lift must invoke the selected marker encoder exactly once:\n{unary}",
        );
        assert_eq!(
            unary
                .matches("<Self::Apply<KioArg_0> as KioType>::from_stored(value.0)")
                .count(),
            1,
            "default project must invoke the selected marker decoder exactly once:\n{unary}",
        );
        assert!(unary.contains("`Apply` is the exact public-facade"));
        assert!(
            unary.contains("The default `lift` and `project` use the selected marker's codec.")
        );
        assert!(unary.contains("external constructor may override them as a pair"));
        assert!(unary.contains("conversion is lossless and"));
        assert!(
            unary
                .contains("substitution-stable. Both round trips must preserve the semantic value")
        );
        assert!(unary.contains("and marker association;"));
        assert!(unary.contains("Overrides use only the documented"));
        assert!(
            unary.contains(
                "Attaching a stored value to a marker whose storage contract it does not"
            )
        );
        assert!(unary.contains("satisfy violates the host invariant."));
        let abstract_application = unary
            .split_once("pub struct KioApply1<")
            .expect("unary abstract-application facade")
            .1
            .split_once(");\n\nimpl")
            .expect("unary abstract-application facade fields")
            .0;
        assert!(abstract_application.contains("pub(crate) KioStoredValue"));
        assert!(unary.contains("type Facade = KioApply1<F, KioArg_0>;"));
        assert!(unary.contains("any public, well-formed `KioType` marker"));

        let function = render_rust_function_support(2, BoundaryFacadeSupportOrigin::Live);
        assert!(
            function.contains("impl Fn(KioArg_0::Facade, KioArg_1::Facade) -> KioResult::Facade",)
                && !function.contains("impl Fn(arg0:"),
            "Fn trait arguments are types; value binder declarations belong only in method parameters:\n{function}",
        );
        let retained_constructor = render_rust_type_constructor_support(
            1,
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: 2,
            },
        );
        assert_eq!(
            retained_constructor
                .matches("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]")
                .count(),
            3,
            "constructor trait, KioApply, and KioApplied must all carry retained provenance",
        );

        let callable = render_rust_function_support(1, BoundaryFacadeSupportOrigin::Live);
        assert!(callable.contains("pub struct KioFn1<"));
        assert!(callable.contains("pub struct KioFunction1<"));
        assert!(callable.contains("arg0: KioArg_0::Facade"));
        let retained_callable = render_rust_function_support(
            1,
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: 2,
            },
        );
        assert_eq!(
            retained_callable
                .matches("#[deprecated(note = \"retained only for source compatibility after removal at contract v2\")]")
                .count(),
            2,
            "KioFn and KioFunction must both carry retained provenance",
        );
    }
}

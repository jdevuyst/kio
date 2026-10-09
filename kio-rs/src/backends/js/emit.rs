//! Kio' → JavaScript lowering.
//!
//! Consumes `Module<Routed>` — the post-resolution-lowering IR —
//! and emits JS. The Routed phase (see [`crate::pass::recover_to_low`])
//! pre-classifies each call site into one of fifteen `Expr::Low*`
//! variants — host fn vs module fn vs qualified module fn
//! vs newtype ctor / projector vs closure vs indirect vs `__absurd__` — so the JS
//! per-variant render arm reads the call kind off the AST node and
//! drops to syntactic templating. Structural recovery
//! (`crate::pass::structural_recovery`) ran upstream, so the AST also
//! carries the seven `Expr::Enriched*` n-ary structural nodes
//! (records / tuples / projections / sums / match / conditional) that replace
//! saturated, directly recoverable intrinsic roots. Residual applications —
//! including partially applied and type-only application stages — and indirect
//! uses remain generic through recovery and follow ordinary routing.
//! `__absurd__` has no structural form and instead receives the dedicated
//! [`crate::ast::Expr::LowAbsurdCall`] route.
//!
//! Type arguments are erased, but each first-class type binder retains one
//! `.advance()` stage. Functions take one semantic domain, including `null`
//! for Unit. Named native views follow the prepared source parameter groups.
//!
//! Runtime data layout (the only guarantees JS code emitted by this
//! pass relies on):
//!
//! - **Sums** `(a | b)`: `{ _0: x }` or `{ _1: x }`.
//! - **Products** `(A & B)`: `{ _0: x, _1: y }`.
//! - **Unit** `()`: the JS value `null` (chosen over `undefined` to
//!   make the unit projection explicit in emitted source).
//! - **Bottom** `!`: no values; `__absurd__` throws if reached.
//! - **Newtypes**: prepared visible payload wrappers or declaration-specific
//!   opaque handles, shared by body execution and public values.
//!
//! Shared value/protection plans govern concrete snapshots and deferred
//! callable protection. The iterative runtime realizes those plans without
//! inspecting erased abstract or host-constructor payloads.

// `maybe_par_iter!` / `maybe_into_par_iter!` (`src/par.rs`) leave the
// trailing `.map(â¦)` to resolve `ParallelIterator` at the call site,
// so the prelude is imported here under `parallel`; with the feature
// off the sequential arm needs no import and rayon is absent.
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};

// `backends::js::emit` consumes the `Routed` phase: the resolution-lowering pass
// (`crate::pass::recover_to_low`) has classified every call into one of the
// fifteen `Expr::Low*` variants, and `crate::pass::structural_recovery`'s
// `Expr::Enriched*` nodes carry through unchanged. Local aliases pin
// every parametrized AST type to `Routed`.
use crate::ast::{ImportKind, Routed};
use crate::backends::boundary_facade::{
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryNewtypeSurface,
    BoundaryNominalDeclaration, BoundaryPublicNewtypeInventoryEntry, CallableExecutionStage,
    FacadeBinderId, FacadeUse, FacadeUseId, PreparedBoundaryCallableSite,
    PreparedBoundaryCallableSites, QualifiedTypeName,
};
use crate::backends::public_names::{
    FacadeSelector, encode_source_identity, host_module_key, host_name_core, module_facade_path,
};
use crate::backends::structural::{
    NominalValueRepresentation, ProductRebuildPlan, bound_product_rebuild_plan,
    render_cached_product_rebuild,
};
#[cfg(feature = "cli")]
use crate::cache::keys::{CacheTarget, EmitInputFingerprint, EmitTargetProfileFingerprint};
type FnDef = crate::ast::FnDef<Routed>;
type Newtype = crate::ast::Newtype<Routed>;
type PackageFile = crate::ast::PackageFile<Routed>;
type Expr = crate::ast::Expr<Routed>;
type EnrichedArm = crate::ast::EnrichedArm<Routed>;
type Item = crate::ast::Item<Routed>;
type Module = crate::ast::Module<Routed>;
type Signature = crate::ast::Signature<Routed>;
type SignatureGroupRef<'a> = crate::ast::SignatureGroupRef<'a, Routed>;
type SignatureParam = crate::ast::SignatureParam<Routed>;
type Type = crate::ast::Type<Routed>;
#[cfg(feature = "cli")]
type JsEmitCache = crate::cache::emit::EmitCache;
#[cfg(not(feature = "cli"))]
type JsEmitCache = ();

#[cfg(feature = "cli")]
fn enabled_js_emit_cache(cache: Option<&JsEmitCache>) -> Option<&JsEmitCache> {
    cache.filter(|cache| cache.is_enabled())
}

#[cfg(not(feature = "cli"))]
fn enabled_js_emit_cache(cache: Option<&JsEmitCache>) -> Option<&JsEmitCache> {
    cache
}

#[cfg(all(test, feature = "cli"))]
mod emit_cache_work_counters {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(super) struct Counts {
        pub(super) context_renders: usize,
        pub(super) module_serializations: usize,
        pub(super) keys: usize,
    }

    thread_local! {
        static COUNTS: Cell<Counts> = const { Cell::new(Counts {
            context_renders: 0,
            module_serializations: 0,
            keys: 0,
        }) };
    }

    #[cfg(feature = "surface")]
    pub(super) fn reset() {
        COUNTS.set(Counts::default());
    }

    #[cfg(feature = "surface")]
    pub(super) fn snapshot() -> Counts {
        COUNTS.get()
    }

    pub(super) fn record_context_render() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.context_renders += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_module_serialization() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.module_serializations += 1;
            cell.set(counts);
        });
    }

    pub(super) fn record_key() {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            counts.keys += 1;
            cell.set(counts);
        });
    }
}

// =========================================================================
// JS reserved-identifier mangling
// =========================================================================

/// JS reserved words plus restricted-name candidates. Any Kio user
/// identifier that would lower verbatim into a binder position
/// (top-level function name, arrow-function parameter, let local) and
/// happens to spell one of these words gets rewritten to
/// `__kio_<name>__`. The `__name__` shape is reserved by the language
/// (user identifiers may not begin with two consecutive underscores),
/// so the rewritten form cannot collide with anything user-written.
///
/// The list covers ECMAScript 2024 keywords, future-reserved words,
/// strict-mode reserved words, and the two restricted identifiers
/// (`arguments`, `eval`) that can't be rebound in strict-mode
/// contexts. Reserved words used purely as property names (after `.`)
/// don't need mangling — JS allows reserved words in member
/// positions — so `<NS>.<name>` and `<Type>.<member>` reference sites
/// pass the suffix through verbatim.
const JS_RESERVED_WORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "async",
    "await",
    "implements",
    "interface",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "arguments",
    "eval",
];

/// If `name` collides with a JS reserved word (or a restricted
/// identifier), return the mangled form `__kio_<name>__`. Otherwise
/// return `name` unchanged. The mangling is **deterministic and
/// reversible**: every declaration and every reference site within
/// the same scope routes through this function, so a Kio binder named
/// `default` consistently lowers to `__kio_default__` everywhere it
/// appears.
fn mangle_js_ident(name: &str) -> std::borrow::Cow<'_, str> {
    if JS_RESERVED_WORDS.contains(&name) {
        std::borrow::Cow::Owned(format!("__kio_{name}__"))
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

// =========================================================================
// Branded facade names
// =========================================================================

/// The branded facade names for one emitted package, all derived from
/// the effective namespace — the artifact stem (`<ns>.js`), which
/// defaults to the package name and is set by the `namespace` key. The
/// JS module exports only the `factory` (`create<Handle>`); JS is
/// dynamic, so no handle / host *type* exists at its surface. The
/// `.d.ts` skin ([`crate::backends::ts`]) reuses this same derivation
/// for its typed `<Handle>` / `<Handle>Host` names and its
/// `create<Handle>` declaration, so the `.js` factory and the `.d.ts`
/// always agree. One derivation root means a consumer reconstructs the
/// whole surface from the artifact stem alone (`specs/backends/js.md`
/// § Package API, `specs/backends/README.md` § Branded naming).
pub(crate) struct Brand {
    pub(crate) handle: String,
    pub(crate) host_ty: String,
    pub(crate) factory: String,
}

impl Brand {
    pub(crate) fn derive(ns: &str) -> Brand {
        let handle = crate::backends::namespace::pascal_case(ns);
        Brand {
            host_ty: format!("{handle}Host"),
            factory: format!("create{handle}"),
            handle,
        }
    }
}

// =========================================================================
// Public API
// =========================================================================

/// Emit the package as a single ES module. The module exports one
/// function — the branded `create<Handle>(host)` factory, where
/// `<Handle>` is the PascalCase of the artifact stem `ns` — whose call
/// returns the package's exposed surface. Every Kio module gets inlined
/// as a closure-scoped `const <NS> = (() => { ... })();` block inside
/// the factory body, so cross-module references resolve via lexical
/// scope rather than `globalThis`.
///
/// Construction-time validation is deliberately minimal: the factory
/// checks that every declared `host fn` is present on `host` and
/// throws `missing host item: <name>` otherwise. Whether the value
/// is actually a function is not checked — the host's first call
/// will surface a JS `TypeError` if the value is wrong-shaped, which
/// is the cleanest possible signal. Extra properties on `host` are
/// tolerated. The factory copies the items it cares about into a
/// frozen internal record so caller-side mutations after
/// construction don't leak into module code.
///
/// `host type` declarations are erased at runtime — JS has no type
/// system at the FFI boundary — so no validation runs for them.
///
/// `ns` is the artifact stem the build arm resolved (the `namespace`
/// key, else the package name); the exported factory is `create<Handle>`
/// for `<Handle>` = PascalCase of `ns` (see [`Brand`]).
pub fn lower_package_to_factory_module(
    package: &crate::pass::resolve::Package<Routed>,
    ns: &str,
) -> Result<String, EmitError> {
    lower_package_to_factory_module_cached(package, ns, None)
}

pub fn lower_package_to_factory_module_cached(
    package: &crate::pass::resolve::Package<Routed>,
    ns: &str,
    emit_cache: Option<&JsEmitCache>,
) -> Result<String, EmitError> {
    let prepared = prepare_js_boundary(package);
    let emit_cache = enabled_js_emit_cache(emit_cache);
    let brand = Brand::derive(ns);
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");
    out.push_str(include_str!("value_runtime.js"));
    out.push_str(&format!(
        "\nexport {{ __kioLift as lift{}Callable, __kioLiftPoly as lift{}Poly }};\n",
        brand.handle, brand.handle,
    ));
    let mut host_docs = Vec::new();
    let mut export_docs = Vec::new();
    for site in prepared.sites() {
        if let Some(doc) = super::super::boundary_docs::callable(package, site) {
            let module_path = site.site().module_segments().join("/");
            let (docs, path) = match site.site().owner() {
                BoundaryFacadeSiteOwner::HostFunction { name } => (
                    &mut host_docs,
                    format!(
                        "host.{}.{}",
                        host_module_key(&module_path),
                        host_name_core(name)
                    ),
                ),
                BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                    let mut path = module_facade_path(&module_path)
                        .iter()
                        .enumerate()
                        .map(|(index, selector)| selector.facade_name(index == 0))
                        .collect::<Vec<_>>();
                    path.push(host_name_core(name));
                    (&mut export_docs, format!("package.{}", path.join(".")))
                }
                _ => unreachable!("callable docs belong only to host and exported functions"),
            };
            docs.push(format!(
                "### `{path}`\n\n{}",
                super::super::doc::render(doc, super::super::doc::Dialect::Markdown)
            ));
        }
    }
    for entry in prepared.public_newtypes() {
        if let Some(doc) = super::super::boundary_docs::newtype(package, entry) {
            let module_path = entry.name().module_segments().join("/");
            let mut path = module_facade_path(&module_path)
                .iter()
                .enumerate()
                .map(|(index, selector)| selector.facade_name(index == 0))
                .collect::<Vec<_>>();
            path.push(FacadeSelector::Type(entry.name().name().to_owned()).facade_name(false));
            export_docs.push(format!(
                "### `package.{}`\n\n{}",
                path.join("."),
                super::super::doc::render(doc, super::super::doc::Dialect::Markdown)
            ));
        }
    }
    let mut sections = Vec::new();
    if !host_docs.is_empty() {
        sections.push(format!("Host record\n\n{}", host_docs.join("\n\n")));
    }
    if !export_docs.is_empty() {
        sections.push(format!("Package exports\n\n{}", export_docs.join("\n\n")));
    }
    out.push_str(&super::super::doc::jsdoc(&sections.join("\n\n")));
    out.push_str(&format!("export function {}(host) {{\n", brand.factory));
    out.push_str(&lower_package_factory_body(package, emit_cache, &prepared)?);
    out.push_str("}\n");
    Ok(out)
}

fn prepare_js_boundary(
    package: &crate::pass::resolve::Package<Routed>,
) -> PreparedBoundaryCallableSites {
    PreparedBoundaryCallableSites::collect(package, None).unwrap_or_else(|error| {
        unreachable!(
            "a typed Routed package must have a complete JavaScript boundary facade: {error}"
        )
    })
}

/// Emit the body of one package's factory — everything between the
/// `{` and `}` of the `create<Handle>` factory. The body, in order: the
/// `host` default, host-presence validation, the frozen `__host__`
/// record, every Kio module's IIFE, the export record, and the
/// `return`.
fn lower_package_factory_body(
    package: &crate::pass::resolve::Package<Routed>,
    emit_cache: Option<&JsEmitCache>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("  if (host === undefined || host === null) host = {};\n");
    let value_plans = super::super::serialized_runtime_ir::boundary_value_plans_json(
        package,
        prepared,
        super::super::value_protection::ValueProtectionPolicy::Snapshot,
    )
    .map_err(EmitError::internal)?;
    out.push_str(&format!("  const __kioBoundaryPlans = {value_plans};\n"));
    out.push_str("  const __kioBoundarySites = new Map(__kioBoundaryPlans.sites.map(site => [site.key, site]));\n");
    let body_plans = package
        .package_file()
        .map(|_| {
            super::super::serialized_runtime_ir::package_ir(package).map_err(EmitError::internal)
        })
        .transpose()?;
    if let Some(plans) = &body_plans {
        let values = plans
            .value_plans_json(super::super::value_protection::ValueProtectionPolicy::Snapshot)
            .map_err(EmitError::internal)?;
        out.push_str(&format!("  const __kioBodyPlans = {values};\n"));
    }

    if package.package_file().is_some() {
        out.push_str(&render_prepared_js_host_factory_prelude(prepared));
    } else {
        // No package file → no host items to validate or freeze.
        // Still emit the empty frozen record so generated references to
        // `__host__` resolve to a defined value.
        out.push_str("  const __host__ = Object.freeze({});\n");
    }
    // Each Kio module's IIFE wrapped as a closure-scoped const.
    // Module insertion order matches the package's traversal order,
    // which already respects the no-forward-references invariant the
    // resolver enforces. `lower_module` is pure with respect to its
    // inputs (the recovered `Package<Routed>` has no interior mutability),
    // so the per-module string production fans out across rayon and
    // re-collects in input order — concatenated output is identical
    // to the serial walk.
    let package_file = package.package_file().map(|e| &e.package_file);
    let package_name = package.package_file().map(|e| e.package_name.as_str());
    let emit_context = emit_cache.map(|_| {
        #[cfg(all(test, feature = "cli"))]
        emit_cache_work_counters::record_context_render();
        format!("{package:?}")
    });
    let module_pieces: Result<Vec<String>, EmitError> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(module_key, entry)| {
                // Pass the storage key so the JS namespace matches the
                // form consumer-side `import <pkg>/<mod>(...);`
                // references resolve to (per `specs/package.md`
                // § Module-name rules).
                lower_module_with_key_cached(
                    &entry.module,
                    module_key,
                    package_file,
                    package_name,
                    Some(package),
                    (Some(prepared), body_plans.as_ref()),
                    (emit_cache, emit_context.as_deref()),
                )
                .map(|js| indent_block(&js, "  "))
            })
            .collect();
    let mut generated_body = String::new();
    for piece in module_pieces? {
        generated_body.push_str(&piece);
    }
    let mut structural_helpers = js_uses_structural_helpers(&generated_body);
    let mut opaque_helpers = body_plans.is_some() || js_uses_opaque_helpers(&generated_body);

    // Export record. Only an explicit `export` exposes package items.
    if let Some(entry) = package.package_file()
        && let Some(body) = render_package_file_prepared(
            &entry.package_file,
            &entry.package_name,
            package,
            prepared,
        )?
    {
        structural_helpers |= body.structural_helpers;
        opaque_helpers |= body.opaque_helpers;
        generated_body.push_str(&indent_block(&body.source, "  "));
    } else {
        generated_body.push_str("  const __export_record__ = {};\n");
    }

    // The package's host-facing surface is the union of declared
    // export items (by name) and a stable, additive set of
    // package-level utilities. No extra utilities are defined beyond the
    // export record; `Object.assign` lets contract-authorized entries join the
    // same assembly shape.
    generated_body.push_str("  return Object.assign(Object.create(null), __export_record__);\n");

    if structural_helpers {
        out.push_str(render_js_structural_helpers());
    }
    if opaque_helpers {
        out.push_str(render_js_opaque_helpers());
    }
    if let Some(plans) = &body_plans {
        out.push_str(&render_js_value_context(plans, prepared)?);
    }
    out.push_str(&generated_body);

    Ok(out)
}

fn render_js_value_context(
    plans: &super::super::serialized_runtime_ir::RuntimePackageIr,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<String, EmitError> {
    let inventory = plans
        .nominal_inventory_json()
        .map_err(EmitError::internal)?;
    let mut views = std::collections::BTreeMap::new();
    for site in prepared.sites().filter(|site| site.execution().is_some()) {
        for (name, declaration) in site.nominals().declarations() {
            if matches!(declaration, BoundaryNominalDeclaration::Newtype { .. }) {
                let key = serde_json::to_string(&(name.module_segments(), name.name()))
                    .expect("nominal components serialize");
                views.entry(key).or_insert_with(|| {
                    serde_json::json!({
                        "key": host_name_core(name.name()),
                        "sealed": declaration.public_structural_payload().is_none() || prepared_js_newtype_is_recursive(site, name),
                    })
                });
            }
        }
    }
    let views = serde_json::to_string(&views.into_iter().collect::<Vec<_>>())
        .expect("prepared nominal views serialize");
    Ok(format!(
        "  const __kioBodyContext = __kioValueContext({inventory}, __kioBoundaryPlans, new Map({views}), __kioOpaqueIn, __kioOpaqueOut);\n"
    ))
}

/// Render the JS factory prelude: host-record defaulting, presence-check throws
/// for declared host fns, followed by the frozen nested `__host__` record.
///
/// JS has no host-type binding syntax to discharge. Its callable inventory
/// still comes from the same package-complete prepared transaction as the
/// call and function-value adapters below.
///
/// **Namespaced host boundary (rung 1).** The package boundary is
/// module-qualified, so the host record is *nested* by the declaring
/// module's exact namespace key: a `host fn print` declared in module
/// `app` reaches the host as `__host__.app.print`. Slash-only paths keep
/// lowercase components; a source underscore selects the exact
/// `KioModule_...` class. The host (user) supplies the matching nested
/// record (`host.app.print`).
///
/// Each declared `host fn` produces one presence-check throw plus one
/// entry in its module's frozen sub-record.
/// JavaScript has no exact host-type binding syntax, so the runtime host
/// prelude needs only callable names. Their inventory nevertheless comes from
/// the same package-complete preparation transaction as every call adapter;
/// no second AST walk may admit, omit, or regroup a host callable.
fn render_prepared_js_host_factory_prelude(prepared: &PreparedBoundaryCallableSites) -> String {
    let mut by_namespace: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
            continue;
        };
        let module_path = site.site().module_segments().join("/");
        by_namespace
            .entry(host_module_key(&module_path))
            .or_default()
            .push(host_name_core(name));
    }
    render_js_host_factory_prelude_names(&by_namespace)
}

fn render_js_host_factory_prelude_names(
    by_namespace: &std::collections::BTreeMap<String, Vec<String>>,
) -> String {
    let mut out = String::new();

    // Construction-time validation: every declared `host fn` must be
    // present (under its module sub-record) before any module code runs.
    for (ns, names) in by_namespace {
        for name in names {
            out.push_str(&format!(
                "  if (host.{ns} === undefined || host.{ns}.{name} === undefined) \
                 throw new Error(\"missing host item: {ns}.{name}\");\n"
            ));
        }
    }

    out.push_str("  const __host__ = Object.freeze({");
    let mut first_ns = true;
    for (ns, names) in by_namespace {
        if !first_ns {
            out.push(',');
        }
        first_ns = false;
        out.push_str(&format!(" {ns}: Object.freeze({{"));
        let mut first = true;
        for name in names {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(" {name}: host.{ns}.{name}"));
        }
        if !first {
            out.push(' ');
        }
        out.push_str("})");
    }
    if !first_ns {
        out.push(' ');
    }
    out.push_str("});\n");
    out
}

fn render_js_structural_helpers() -> &'static str {
    "  function __kioProductSlots(v, arity) {\n\
       const slots = [];\n\
       let cur = v;\n\
       for (let i = 0; i < arity; i++) {\n\
         if (i + 1 === arity) {\n\
           slots.push(cur);\n\
           break;\n\
         }\n\
         slots.push(cur._0);\n\
         cur = cur._1;\n\
       }\n\
       return slots;\n\
     }\n\
     function __kioProductSlot(v, index, arity) {\n\
       if (index < 0 || index >= arity) throw new Error(\"product slot out of bounds\");\n\
       let cur = v;\n\
       for (let i = 0; i < index; i++) cur = cur._1;\n\
       return index + 1 === arity ? cur : cur._0;\n\
     }\n\
     function __kioSumPayload(v, variants) {\n\
       if (variants <= 0) throw new Error(\"sum arity out of bounds\");\n\
       let cur = v;\n\
       for (let i = 0; i < variants; i++) {\n\
         if (i + 1 === variants) return [i, cur];\n\
         if (\"_0\" in cur) return [i, cur._0];\n\
         cur = cur._1;\n\
       }\n\
       throw new Error(\"sum payload out of bounds\");\n\
     }\n\
     function __kioSumInject(payload, variant, variants) {\n\
       if (variants <= 0 || variant < 0 || variant >= variants) {\n\
         throw new Error(\"sum variant out of bounds\");\n\
       }\n\
       let cur = payload;\n\
       if (variant + 1 < variants) cur = { _0: cur };\n\
       for (let i = 0; i < variant; i++) cur = { _1: cur };\n\
       return cur;\n\
     }\n"
}

fn js_uses_structural_helpers(src: &str) -> bool {
    src.contains("__kioProductSlots(")
        || src.contains("__kioProductSlot(")
        || src.contains("__kioSumPayload(")
        || src.contains("__kioSumInject(")
}

/// Private runtime authority for public newtypes that hide either boundary
/// operation. Each exact declaration identity owns a separate weak table. The
/// host sees only a frozen, null-prototype handle: it can
/// shuttle that handle through public functions but cannot forge one from the
/// payload, inspect the payload, or substitute a same-shaped handle minted for
/// another declaration.
fn render_js_opaque_helpers() -> &'static str {
    "  const __kioOpaqueStores = new Map();\n\
     const __kioOpaqueWeakMap = WeakMap;\n\
     const __kioOpaqueCreate = Object.create;\n\
     const __kioOpaqueFreeze = Object.freeze;\n\
     function __kioOpaqueOut(identity, payload) {\n\
       let store = __kioOpaqueStores.get(identity);\n\
       if (store === undefined) {\n\
         store = new __kioOpaqueWeakMap();\n\
         __kioOpaqueStores.set(identity, store);\n\
       }\n\
       const handle = __kioOpaqueFreeze(__kioOpaqueCreate(null));\n\
       store.set(handle, payload);\n\
       return handle;\n\
     }\n\
     function __kioOpaqueIn(identity, handle) {\n\
       const store = __kioOpaqueStores.get(identity);\n\
       if (store === undefined || !store.has(handle)) {\n\
         throw new TypeError(\"invalid opaque newtype value\");\n\
       }\n\
       return store.get(handle);\n\
     }\n"
}

fn js_uses_opaque_helpers(src: &str) -> bool {
    src.contains("__kioOpaqueOut(") || src.contains("__kioOpaqueIn(")
}

/// Indent every non-empty line of `src` with `prefix`. Empty lines
/// pass through unchanged so the output reads cleanly when the
/// caller composes multiple inlined blocks.
fn indent_block(src: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(src.len() + 32);
    for line in src.split_inclusive('\n') {
        if line == "\n" {
            out.push('\n');
        } else {
            out.push_str(prefix);
            out.push_str(line);
        }
    }
    out
}

/// Emit JavaScript source for one Kio' module. Returns the rendered
/// string on success, or a build-error message describing the
/// unsupported construct.
///
/// `package_file` and `package_name` carry the package's package file (when
/// present) and its filename-stem name. They're consulted for host
/// literal roles and when validating the `<pkg>` of
/// `import <pkg>(...);` cross-module imports against the package
/// boundary.
///
/// Each module's emitted JS wraps its declarations in a per-module
/// IIFE assigned to a closure-scoped namespace identifier inside the
/// factory body:
///
/// ```js
/// const PKG_HELPER = (() => {
///   function greet() { /* ... */ }
///   return { greet };
/// })();
/// ```
///
/// A module path without source underscores keeps the readable uppercase
/// slash-to-underscore identifier (`pkg/helper` → `PKG_HELPER`). A path with
/// source underscores uses the disjoint `KioInternalModule_...` class, with
/// `_` encoded as `_u` and `/` as `_s`, so distinct source paths cannot
/// collapse to the same closure binding.
/// Cross-module references (`import pkg/helper(foo); ... foo()`)
/// lower to the corresponding closure-scoped const at runtime.
pub fn lower_module(
    module: &Module,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> Result<String, EmitError> {
    let key = synthesize_storage_key(module, package_name);
    lower_module_with_key(module, &key, package_file, package_name, package)
}

/// As [`lower_module`], but uses an explicit storage key for the
/// emitted namespace. Producer sites that already have the
/// `Package::build` storage key on hand (e.g. `package.modules()`'s
/// keyed iteration) should call this directly so the JS namespace
/// matches the form consumer-side references resolve to.
pub fn lower_module_with_key(
    module: &Module,
    module_key: &str,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> Result<String, EmitError> {
    let prepared = package.map(prepare_js_boundary);
    lower_module_with_key_prepared(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared.as_ref(),
        None,
    )
}

fn lower_module_with_key_prepared(
    module: &Module,
    module_key: &str,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    prepared: Option<&PreparedBoundaryCallableSites>,
    body_plans: Option<&super::super::serialized_runtime_ir::RuntimePackageIr>,
) -> Result<String, EmitError> {
    // Walk the module's `import` statements:
    //   - `import __intrinsics__;` brings the eight intrinsics into scope;
    //     emission is inline at each call site (see
    //     `emit_intrinsic_call`), so the import statement itself produces
    //     no JS.
    //   - Selective `import <module>(<names>);` against a same-package
    //     module is the cross-module path lowered here.
    for u in &module.imports {
        match &u.kind {
            ImportKind::Intrinsics => {}
            ImportKind::Comptime => {}
            ImportKind::Selective { from, .. } => {
                // Cross-module within the package — handled below via
                // `cross_module_imports`. The resolver has already
                // validated that the target module exists and the
                // names are pub.
                let _ = from;
            }
            ImportKind::Qualified { .. } => {
                // Qualified imports register an alias → namespace
                // mapping in `build_top_level`, then `lower_path`
                // emits `<NS>.<member>` at reference sites.
            }
        }
    }

    let mut top_level = build_top_level(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared,
    );
    top_level.body_plans = body_plans;
    let module_ns = module_namespace_from_key(module_key);
    let mut out = String::new();
    out.push_str("const ");
    out.push_str(&module_ns);
    out.push_str(" = (() => {\n");
    let item_pieces: Result<Vec<Vec<ModuleItemJs>>, EmitError> =
        crate::maybe_par_iter!(module.items)
            .map(|item| lower_module_item(item, &top_level))
            .collect();
    let mut wrote_decl = false;
    let mut exported: Vec<String> = Vec::new();
    for piece in item_pieces?.into_iter().flatten() {
        if wrote_decl {
            out.push('\n');
        }
        out.push_str(&piece.decl);
        exported.push(piece.exported);
        wrote_decl = true;
    }
    if wrote_decl {
        out.push('\n');
    }
    out.push_str("return { ");
    for (i, n) in exported.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // Object shorthand `{ name }` — `name` lives in *expression*
        // position here, so we must mangle reserved-word collisions.
        // The IIFE's lowered declarations were also mangled, so the
        // shorthand resolves to the same binding.
        out.push_str(&mangle_js_ident(n));
    }
    out.push_str(" };\n})();\n");
    Ok(out)
}

#[cfg(feature = "cli")]
fn lower_module_with_key_cached(
    module: &Module,
    module_key: &str,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    plans: (
        Option<&PreparedBoundaryCallableSites>,
        Option<&super::super::serialized_runtime_ir::RuntimePackageIr>,
    ),
    emit: (Option<&JsEmitCache>, Option<&str>),
) -> Result<String, EmitError> {
    let (prepared, body_plans) = plans;
    let (emit_cache, emit_context) = emit;
    let emit_cache = enabled_js_emit_cache(emit_cache);
    let Some(cache) = emit_cache else {
        return lower_module_with_key_prepared(
            module,
            module_key,
            package_file,
            package_name,
            package,
            prepared,
            body_plans,
        );
    };
    #[cfg(all(test, feature = "cli"))]
    emit_cache_work_counters::record_module_serialization();
    let module_bytes = postcard::to_allocvec(module)
        .map_err(|e| EmitError::unsupported(format!("cannot encode JS emit cache key: {e}")))?;
    let profile = format!("target=js;package={}", package_name.unwrap_or(""));
    let context = emit_context.unwrap_or("");
    #[cfg(all(test, feature = "cli"))]
    emit_cache_work_counters::record_key();
    let key = crate::cache::emit::EmitCacheKey::new(
        CacheTarget::new("js"),
        EmitTargetProfileFingerprint::from_bytes(profile.as_bytes()),
        EmitInputFingerprint::from_parts(&[
            module_key.as_bytes(),
            context.as_bytes(),
            &module_bytes,
        ]),
    );
    if let Some(js) = cache.lookup(&key) {
        return Ok(js);
    }
    let js = lower_module_with_key_prepared(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared,
        body_plans,
    )?;
    cache.store(&key, &js);
    Ok(js)
}

#[cfg(not(feature = "cli"))]
fn lower_module_with_key_cached(
    module: &Module,
    module_key: &str,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    plans: (
        Option<&PreparedBoundaryCallableSites>,
        Option<&super::super::serialized_runtime_ir::RuntimePackageIr>,
    ),
    _emit: (Option<&JsEmitCache>, Option<&str>),
) -> Result<String, EmitError> {
    let (prepared, body_plans) = plans;
    lower_module_with_key_prepared(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared,
        body_plans,
    )
}

struct ModuleItemJs {
    exported: String,
    decl: String,
}

fn lower_module_item(item: &Item, top: &TopLevel) -> Result<Vec<ModuleItemJs>, EmitError> {
    match item {
        Item::FnDef(d) => {
            let mut decl = String::new();
            lower_fn_def(d, top, &mut decl)?;
            Ok(vec![ModuleItemJs {
                exported: d.name.clone(),
                decl,
            }])
        }
        // Type aliases are erased — no runtime presence.
        Item::TypeAlias(_) => Ok(Vec::new()),
        // Host items are declaration-only: the host supplies them, so
        // a module declaring `host type` / `host fn` emits nothing into
        // its own IIFE. Their boundary surface is rendered in the
        // factory prelude (`render_prepared_js_host_factory_prelude`).
        Item::HostType(_) | Item::HostFn(_) => Ok(Vec::new()),
        Item::Newtype(d) => {
            let mut decl = String::new();
            lower_newtype(d, top, &mut decl);
            Ok(vec![ModuleItemJs {
                exported: d.name.clone(),
                decl,
            }])
        }
        Item::TypeRecGroup(group) => {
            let mut pieces = Vec::new();
            for member in &group.members {
                match member {
                    crate::ast::TypeRecMember::TypeAlias(_) => {}
                    crate::ast::TypeRecMember::Newtype(newtype) => {
                        let mut decl = String::new();
                        lower_newtype(newtype, top, &mut decl);
                        pieces.push(ModuleItemJs {
                            exported: newtype.name.clone(),
                            decl,
                        });
                    }
                    crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                }
            }
            Ok(pieces)
        }
        // Statically uninhabited in `Lowered`.
        Item::LiteralAlias(_, ext) => match *ext {},
        Item::Labels(_, ext) | Item::LabelForward(_, ext) => match *ext {},
        // Statically uninhabited in `Prime`; substitution filters
        // `Equiv` items before codegen.
        Item::Equiv(_, ext) => match *ext {},
        Item::Elaborator(_, ext) => match *ext {},
        // Statically uninhabited past `Surface`; desugar consumes
        // `op` items.
        Item::Op(_, ext) => match *ext {},
        Item::VariadicOperator(_, ext) => match *ext {},
        Item::RecGroup(_, ext) => match *ext {},
    }
}

/// Emit the inner body of the package factory's export surface: the
/// `const __export_record__ = …;` line that produces the package's
/// host-facing surface.
///
/// Returns `Ok(None)` if the package declares no `bridge` block (no
/// host-facing surface to expose).
///
/// The export surface is **derived**: every `pub` item of every module
/// matched by the package `bridge` globs becomes a host-invocable
/// entry, named under its declaring module's preserved namespace
/// (`<module-path>.<item>`). The runtime value lives at the source
/// module's closure-scoped IIFE const (`<MODULE_NS>.<item>`). An entry whose
/// prepared facade is already representation-identical points there directly;
/// otherwise the record points at its generated public adapter.
pub fn lower_package_file(
    package_file: &PackageFile,
    _package_name: &str,
    package: &crate::pass::resolve::Package<Routed>,
) -> Result<Option<String>, EmitError> {
    let prepared = prepare_js_boundary(package);
    lower_package_file_prepared(package_file, _package_name, package, &prepared)
}

fn lower_package_file_prepared(
    package_file: &PackageFile,
    package_name: &str,
    package: &crate::pass::resolve::Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<Option<String>, EmitError> {
    Ok(
        render_package_file_prepared(package_file, package_name, package, prepared)?
            .map(|body| body.source),
    )
}

struct RenderedExportRecord {
    source: String,
    structural_helpers: bool,
    opaque_helpers: bool,
}

fn render_package_file_prepared(
    package_file: &PackageFile,
    _package_name: &str,
    package: &crate::pass::resolve::Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<Option<RenderedExportRecord>, EmitError> {
    if package_file.bridge.is_none() {
        return Ok(None);
    }

    let mut tree = ReturnTree::default();
    let mut wrappers = String::new();
    let mut wrapper_count = 0usize;

    for newtype in prepared.public_newtypes() {
        let module_path = newtype.name().module_segments().join("/");
        let module_ns = module_namespace_from_key(&module_path);
        let mut path = module_facade_path(&module_path)
            .iter()
            .enumerate()
            .map(|(index, selector)| selector.facade_name(index == 0))
            .collect::<Vec<_>>();
        path.push(FacadeSelector::Type(newtype.name().name().to_owned()).facade_name(false));
        let value = emit_prepared_export_newtype_namespace(
            newtype,
            &module_ns,
            prepared,
            &mut wrappers,
            &mut wrapper_count,
        )?;
        tree.insert(
            &path,
            value,
            super::super::boundary_docs::newtype(package, newtype),
        );
    }

    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::ExportedFunction { name } = site.site().owner() else {
            continue;
        };
        let module_path = site.site().module_segments().join("/");
        let module_ns = module_namespace_from_key(&module_path);
        let mut path = module_facade_path(&module_path)
            .iter()
            .enumerate()
            .map(|(index, selector)| selector.facade_name(index == 0))
            .collect::<Vec<_>>();
        path.push(host_name_core(name));
        let internal = format!("{module_ns}.{}", mangle_js_ident(name));
        let value = maybe_emit_prepared_export_value_wrapper(
            site,
            &internal,
            &mut wrappers,
            &mut wrapper_count,
        )?
        .unwrap_or(internal);
        tree.insert(
            &path,
            value,
            super::super::boundary_docs::callable(package, site),
        );
    }

    let mut out = String::new();
    out.push_str(&wrappers);
    // Bind the export record to a const rather than returning it
    // directly; the factory's outer wrapper appends the generic
    // intrinsic API methods (`__pair__` etc.) to the same record
    // and emits the final `return` once both halves are in place.
    out.push_str("const __export_record__ = ");
    if tree.is_empty() {
        out.push_str("{}");
    } else {
        tree.emit(&mut out);
    }
    out.push_str(";\n");
    Ok(Some(RenderedExportRecord {
        source: out,
        structural_helpers: js_uses_structural_helpers(&wrappers) || tree.structural_helpers,
        opaque_helpers: js_uses_opaque_helpers(&wrappers) || tree.opaque_helpers,
    }))
}

/// Tree of property paths the package's export record exposes.
/// A leaf is a runtime expression (a JS identifier or a
/// `<NAMESPACE>.X.Y` path that resolves to a closure-scoped const
/// in the factory body); a subtree is a nested object literal.
/// Built from prepared export entries: function and type entries add leaves at
/// the top level, while module selectors add the enclosing subtrees.
#[derive(Debug, Default)]
struct ReturnTree {
    entries: std::collections::BTreeMap<String, ReturnEntry>,
    structural_helpers: bool,
    opaque_helpers: bool,
}

#[derive(Debug)]
enum ReturnEntry {
    Leaf(String, Option<String>),
    Subtree(ReturnTree),
}

impl ReturnTree {
    /// Insert a `value` at a role-framed facade `path`.
    ///
    /// Module and type edges occupy disjoint reserved selector classes, so a
    /// leaf/subtree collision is an internal bug rather than a precedence
    /// decision.
    fn insert(&mut self, path: &[String], value: String, doc: Option<&crate::ast::DocComment>) {
        debug_assert!(!path.is_empty());
        if path.len() == 1 {
            self.structural_helpers |= js_uses_structural_helpers(&value);
            self.opaque_helpers |= js_uses_opaque_helpers(&value);
            assert!(
                self.entries
                    .insert(
                        path[0].clone(),
                        ReturnEntry::Leaf(
                            value,
                            doc.map(|doc| super::super::doc::jsdoc(&super::super::doc::render(
                                doc,
                                super::super::doc::Dialect::Markdown
                            )))
                        )
                    )
                    .is_none(),
                "resolved facade path is unique"
            );
            return;
        }
        let head = path[0].clone();
        let rest = &path[1..];
        let entry = self
            .entries
            .entry(head)
            .or_insert_with(|| ReturnEntry::Subtree(ReturnTree::default()));
        let ReturnEntry::Subtree(sub) = entry else {
            unreachable!("role-framed facade path cannot cross a value leaf")
        };
        sub.insert(rest, value, doc);
        self.structural_helpers |= sub.structural_helpers;
        self.opaque_helpers |= sub.opaque_helpers;
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Emit the tree as a JS object literal. Properties at the
    /// current level are sorted by name (the BTreeMap iteration is
    /// already lex-sorted, which is the canonical style).
    ///
    /// Property keys are emitted **verbatim** (no `mangle_js_ident`)
    /// because JS allows reserved words at property-access positions
    /// — `pkg.return`, `pkg.class`, `pkg.delete` are all legal — and
    /// `specs/backends/js.md` § Item naming pins exported items to their
    /// declared Kio name verbatim. The *value* (the variable holding
    /// the function/value) is still mangled, since the identifier
    /// lives in a JS-syntax position where reserved words are
    /// rejected.
    fn emit(&self, out: &mut String) {
        out.push('{');
        let mut first = true;
        for (key, entry) in &self.entries {
            if !first {
                out.push_str(", ");
            }
            first = false;
            match entry {
                ReturnEntry::Leaf(value, doc) => {
                    if let Some(doc) = doc {
                        out.push('\n');
                        out.push_str(doc);
                    }
                    out.push_str(key);
                    // Property shorthand only applies when the
                    // (verbatim) key matches the value identifier
                    // exactly; otherwise emit the full `key: value`
                    // form. Reserved-word keys never shorthand,
                    // because the value is mangled and the key is
                    // verbatim, so the two strings differ.
                    if value.as_str() != key.as_str() {
                        out.push_str(": ");
                        out.push_str(value);
                    }
                }
                ReturnEntry::Subtree(sub) => {
                    out.push_str(key);
                    out.push_str(": ");
                    sub.emit(out);
                }
            }
        }
        out.push('}');
    }
}

/// Emit the host-facing namespace for one public newtype. The internal module
/// object always carries both member implementations because Kio code may use
/// private members; this derived object exposes only the declaration's public
/// host projection. Each exposed member uses the same per-signature boundary
/// wrapper as a public function. An opaque public newtype therefore surfaces
/// as an empty namespace rather than leaking its internal members.
fn emit_prepared_export_newtype_namespace(
    newtype: &BoundaryPublicNewtypeInventoryEntry,
    module_ns: &str,
    prepared: &PreparedBoundaryCallableSites,
    wrappers: &mut String,
    wrapper_count: &mut usize,
) -> Result<String, EmitError> {
    let mut members = Vec::new();
    let mut add_member = |member: &str, owner: BoundaryFacadeSiteOwner| {
        let internal_type = format!("{module_ns}.{}", newtype.name().name());
        let internal = js_member_access(&internal_type, member);
        let site_id = BoundaryFacadeSiteId::new(newtype.name().module_segments().to_vec(), owner)
            .expect("a prepared newtype owns a valid facade identity");
        let site = find_prepared_js_site(prepared, &site_id)?;
        let value =
            maybe_emit_prepared_export_value_wrapper(site, &internal, wrappers, wrapper_count)?
                .unwrap_or(internal);
        members.push(format!(
            "{}: {value}",
            js_object_key(&host_name_core(member))
        ));
        Ok::<(), EmitError>(())
    };

    match newtype.surface() {
        BoundaryNewtypeSurface::Unexposed => {
            return Err(EmitError::internal(format!(
                "prepared public newtype has no host surface: {:?}",
                newtype.name()
            )));
        }
        BoundaryNewtypeSurface::Opaque => {}
        BoundaryNewtypeSurface::Constructor { member } => {
            add_member(
                member,
                BoundaryFacadeSiteOwner::NewtypeConstructor {
                    newtype: newtype.name().name().to_owned(),
                    member: member.clone(),
                },
            )?;
        }
        BoundaryNewtypeSurface::Projector { member } => {
            add_member(
                member,
                BoundaryFacadeSiteOwner::NewtypeProjector {
                    newtype: newtype.name().name().to_owned(),
                    member: member.clone(),
                },
            )?;
        }
        BoundaryNewtypeSurface::Both {
            constructor,
            projector,
        } => {
            add_member(
                constructor,
                BoundaryFacadeSiteOwner::NewtypeConstructor {
                    newtype: newtype.name().name().to_owned(),
                    member: constructor.clone(),
                },
            )?;
            add_member(
                projector,
                BoundaryFacadeSiteOwner::NewtypeProjector {
                    newtype: newtype.name().name().to_owned(),
                    member: projector.clone(),
                },
            )?;
        }
    }

    Ok(format!(
        "{{{}}}",
        if members.is_empty() {
            String::new()
        } else {
            format!(" {} ", members.join(", "))
        }
    ))
}

fn find_prepared_js_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    id: &BoundaryFacadeSiteId,
) -> Result<PreparedBoundaryCallableSite<'a>, EmitError> {
    prepared
        .site(id)
        .ok_or_else(|| EmitError::internal(format!("missing prepared facade site {id:?}")))
}

fn prepared_js_execution(
    site: PreparedBoundaryCallableSite<'_>,
) -> Result<&crate::backends::boundary_facade::CallableExecutionLayout, EmitError> {
    site.execution().ok_or_else(|| {
        EmitError::internal(format!(
            "retained facade site reached live emission: {:?}",
            site.site()
        ))
    })
}

fn prepared_js_newtype_is_recursive(
    site: PreparedBoundaryCallableSite<'_>,
    root: &QualifiedTypeName,
) -> bool {
    let Some(payload) = site
        .nominals()
        .declaration(root)
        .and_then(BoundaryNominalDeclaration::public_structural_payload)
    else {
        return false;
    };
    let mut visited = BTreeSet::from([root.clone()]);
    let mut binder_frames = Vec::new();
    prepared_js_use_reaches_newtype(
        site,
        payload.facade(),
        payload.payload_root(),
        root,
        &mut visited,
        &mut binder_frames,
    )
}

#[derive(Clone, Copy)]
struct PreparedJsSemanticUseRef<'a> {
    plan: &'a BoundaryFacadePlan,
    id: FacadeUseId,
}

struct PreparedJsSemanticBinderFrame<'a> {
    plan: &'a BoundaryFacadePlan,
    substitutions: Vec<(FacadeBinderId, PreparedJsSemanticUseRef<'a>)>,
}

fn prepared_js_nominal_reaches_newtype<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    name: &QualifiedTypeName,
    args: &[PreparedJsSemanticUseRef<'a>],
    root: &QualifiedTypeName,
    visited: &mut BTreeSet<QualifiedTypeName>,
    binder_frames: &mut Vec<PreparedJsSemanticBinderFrame<'a>>,
) -> bool {
    if name == root {
        return true;
    }
    let Some(BoundaryNominalDeclaration::Newtype { type_params, .. }) =
        site.nominals().declaration(name)
    else {
        return false;
    };
    let Some(payload) = site
        .nominals()
        .declaration(name)
        .and_then(BoundaryNominalDeclaration::public_structural_payload)
    else {
        return false;
    };
    // A higher-kinded nominal constructor has no payload value until it is
    // saturated. Conversely, only declaration type parameters are supplied by
    // an application; existential binders remain abstract in the payload.
    if args.len() != type_params.len() || !visited.insert(name.clone()) {
        return false;
    }
    let substitutions = payload
        .declaration_binders()
        .iter()
        .take(type_params.len())
        .copied()
        .zip(args.iter().copied())
        .collect();
    binder_frames.push(PreparedJsSemanticBinderFrame {
        plan: payload.facade(),
        substitutions,
    });
    let found = prepared_js_use_reaches_newtype(
        site,
        payload.facade(),
        payload.payload_root(),
        root,
        visited,
        binder_frames,
    );
    binder_frames.pop();
    visited.remove(name);
    found
}

fn prepared_js_use_reaches_newtype<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    id: FacadeUseId,
    root: &QualifiedTypeName,
    visited: &mut BTreeSet<QualifiedTypeName>,
    binder_frames: &mut Vec<PreparedJsSemanticBinderFrame<'a>>,
) -> bool {
    match plan.use_at(id) {
        FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } => false,
        FacadeUse::Bound { binder, .. } => binder_frames
            .iter()
            .rev()
            .find(|frame| std::ptr::eq(frame.plan, plan))
            .and_then(|frame| {
                frame
                    .substitutions
                    .iter()
                    .find_map(|(candidate, use_ref)| (*candidate == *binder).then_some(*use_ref))
            })
            .is_some_and(|use_ref| {
                prepared_js_use_reaches_newtype(
                    site,
                    use_ref.plan,
                    use_ref.id,
                    root,
                    visited,
                    binder_frames,
                )
            }),
        FacadeUse::Nominal { name, .. } => {
            prepared_js_nominal_reaches_newtype(site, name, &[], root, visited, binder_frames)
        }
        FacadeUse::Apply {
            constructor, args, ..
        } => match plan.use_at(*constructor) {
            FacadeUse::Nominal { name, .. } => {
                let args = args
                    .iter()
                    .map(|id| PreparedJsSemanticUseRef { plan, id: *id })
                    .collect::<Vec<_>>();
                prepared_js_nominal_reaches_newtype(site, name, &args, root, visited, binder_frames)
            }
            // Host types, hidden-carrier newtypes, and abstract type
            // constructors are atomic at the JS boundary. Their type
            // arguments cannot expose a recursive payload edge.
            _ => false,
        },
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => args.iter().any(|arg| {
            prepared_js_use_reaches_newtype(site, plan, *arg, root, visited, binder_frames)
        }),
        FacadeUse::Function { slots, result, .. } => {
            slots.iter().any(|slot| {
                prepared_js_use_reaches_newtype(site, plan, *slot, root, visited, binder_frames)
            }) || prepared_js_use_reaches_newtype(site, plan, *result, root, visited, binder_frames)
        }
        FacadeUse::Forall { result, .. } => {
            prepared_js_use_reaches_newtype(site, plan, *result, root, visited, binder_frames)
        }
    }
}

fn maybe_emit_prepared_export_value_wrapper(
    site: PreparedBoundaryCallableSite<'_>,
    internal: &str,
    wrappers: &mut String,
    wrapper_count: &mut usize,
) -> Result<Option<String>, EmitError> {
    let execution = prepared_js_execution(site)?;
    let count = execution
        .head_stages()
        .iter()
        .filter_map(|stage| match stage {
            CallableExecutionStage::Type { .. } => None,
            CallableExecutionStage::Value(layout) => Some(layout.public_parameter_count()),
        })
        .sum();
    let values = (0..count)
        .map(|index| format!("__kioExportArg{index}"))
        .collect::<Vec<_>>();
    let wrapper_name = format!("__export_fn_{}", *wrapper_count);
    *wrapper_count += 1;
    wrappers.push_str(&format!(
        "function {wrapper_name}({}) {{ return ",
        values.join(", ")
    ));
    let mut applied = protect_js_boundary(site, internal.to_owned(), false);
    let mut cursor = 0;
    for stage in execution.head_stages() {
        match stage {
            CallableExecutionStage::Type { .. } => {
                applied = format!("({applied}).advance()");
            }
            CallableExecutionStage::Value(layout) => {
                let end = cursor + layout.public_parameter_count();
                let domain = canonical_js_domain(&values[cursor..end]);
                applied = format!("({applied})({domain})");
                cursor = end;
            }
        }
    }
    assert_eq!(
        cursor,
        values.len(),
        "prepared native head consumes its exact source parameters"
    );
    wrappers.push_str(&applied);
    wrappers.push_str("; }\n");
    Ok(Some(wrapper_name))
}

/// Synthesize the canonical storage key the resolver's
/// `Package::build` produces from a module and the package name.
fn synthesize_storage_key(module: &Module, package_name: Option<&str>) -> String {
    let declared: String = module
        .path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    let _ = package_name;
    declared
}

/// Internal JS identifier for one Kio module.
///
/// Slash-only paths keep the established upper-case spelling. A source
/// underscore selects a mixed-case reserved class whose payload preserves
/// `_` and `/` separately.
fn module_namespace(path: &crate::ast::ModulePath) -> String {
    module_namespace_from_key(
        &path
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// As [`module_namespace`], but builds the namespace from the
/// `Package::build` synthesized storage key. Used at producer sites
/// so the JS namespace matches the form imports resolve to.
pub(crate) fn module_namespace_from_key(storage_key: &str) -> String {
    if !storage_key.contains('_') {
        return storage_key.replace('/', "_").to_ascii_uppercase();
    }
    format!("KioInternalModule_{}", encode_source_identity(storage_key))
}

// =========================================================================
// Per-module rendering context
// =========================================================================

/// JS-specific rendering context built per module from the module's
/// `import` declarations. Carries the namespace-identifier mappings the
/// per-variant render arms need to spell cross-module / qualified-
/// import references at the JS surface — the **classification** the
/// AST node represents (host call vs module call vs newtype member,
/// etc.) is already on the `Expr::Low*` variant the renderer
/// dispatches over.
///
/// Call classification lives on the `Low*` variants. This context only
/// carries names that affect emitted JS module qualification.
struct TopLevel<'m> {
    module: &'m Module,
    /// The current module's exact package storage key. Routed nodes carry
    /// declaring-module keys in the same form, so owner-qualified references
    /// can distinguish a local binding from a sibling-module binding without
    /// consulting the module's import set.
    module_key: &'m str,
    /// Exact declarations for newtypes owned by `module_key`. Public
    /// single-module lowering has no package context, so local CPS
    /// projector emission resolves through this table instead.
    local_newtypes: HashMap<&'m str, &'m Newtype>,
    /// Local name → uppercased module-namespace identifier (e.g.,
    /// `("greet", "PKG_HELPER")`). Populated from same-package
    /// `import pkg/helper(foo);` statements. The variant
    /// [`Expr::LowModuleCall`] / [`Expr::LowModuleFnValueRef`] reads
    /// this map to decide whether the surface name should resolve
    /// against the local IIFE (no entry) or against a sibling
    /// module's namespace (an entry exists).
    cross_module_imports: HashMap<&'m str, String>,
    /// Qualified-import alias → module-namespace identifier (e.g.,
    /// `("h", "PKG_HELPER")`). Populated from same-package
    /// `import pkg/helper as h;` statements and read by
    /// [`Expr::LowQualifiedModuleCall`]. Qualified newtype members carry
    /// their exact declaring-module identity directly.
    qualified_imports: HashMap<&'m str, String>,
    /// Cross-module newtype imports — `import m(Foo);` brings a
    /// newtype that lives in `m`'s
    /// IIFE namespace. The [`Expr::LowNewtypeCtor`] /
    /// [`Expr::LowNewtypeProj`] render arms consult this map to
    /// decide whether the surface newtype name resolves to a local
    /// const (no entry) or to a `<NS>.<NT>` sibling-module qualifier
    /// (an entry exists).
    cross_module_newtypes: HashMap<&'m str, String>,
    /// The package the emitter is operating on. Package-boundary conversion
    /// uses `prepared`; this reference remains for expression lowering and
    /// the standalone-module conversion helpers exercised without a public
    /// package facade.
    package: Option<&'m crate::pass::resolve::Package<Routed>>,
    /// Package-complete semantic callable plans and their paired live
    /// execution layouts. Every host-boundary render in package mode looks up
    /// its exact structured site here; standalone module tests have no public
    /// package boundary and therefore carry `None`.
    prepared: Option<&'m PreparedBoundaryCallableSites>,
    body_plans: Option<&'m super::super::serialized_runtime_ir::RuntimePackageIr>,
    /// Runtime bindings emitted in this module's IIFE, keyed by their
    /// source names. A local `let` that reuses one of these names must
    /// not emit `const name = name(...)`, because JavaScript's temporal
    /// dead zone would make the initializer read the new local instead
    /// of the outer module binding.
    local_runtime_bindings: HashSet<&'m str>,
}

fn build_top_level<'m>(
    module: &'m Module,
    module_key: &'m str,
    _package_file: Option<&'m PackageFile>,
    _package_name: Option<&str>,
    package: Option<&'m crate::pass::resolve::Package<Routed>>,
    prepared: Option<&'m PreparedBoundaryCallableSites>,
) -> TopLevel<'m> {
    let mut cross_module_imports: HashMap<&'m str, String> = HashMap::new();
    let mut qualified_imports: HashMap<&'m str, String> = HashMap::new();
    let mut cross_module_newtypes: HashMap<&'m str, String> = HashMap::new();
    let mut local_newtypes: HashMap<&'m str, &'m Newtype> = HashMap::new();
    let mut local_runtime_bindings: HashSet<&'m str> = HashSet::new();
    for item in &module.items {
        if let Item::FnDef(definition) = item {
            local_runtime_bindings.insert(definition.name.as_str());
        }
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype() {
                local_newtypes.insert(newtype.name.as_str(), newtype);
                local_runtime_bindings.insert(newtype.name.as_str());
            }
        });
    }
    for u in &module.imports {
        match &u.kind {
            // Cross-module same-package import. Each imported name
            // might resolve to a `fn` (lowered as `<NS>.foo` lookup)
            // or a `newtype` (whose member calls need
            // `<NS>.Foo.<member>` lowering). Track both so the codegen
            // can distinguish at call sites.
            ImportKind::Selective { items, from } => {
                let ns = module_namespace(from);
                for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                    cross_module_imports.insert(name, ns.clone());
                    if let Some(pkg) = package {
                        let from_path = from
                            .segments
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("/");
                        if let Some(target) = pkg.module(&from_path) {
                            let mut imports_newtype = false;
                            for item in &target.module.items {
                                crate::pass::resolve::for_each_item_declaration(
                                    item,
                                    |declaration| {
                                        imports_newtype |= declaration
                                            .newtype()
                                            .is_some_and(|newtype| newtype.name == name);
                                    },
                                );
                            }
                            if imports_newtype {
                                cross_module_newtypes.insert(name, ns.clone());
                            }
                        }
                    }
                }
            }
            // Qualified import: `import pkg/helper as h;` registers `h`
            // → `PKG_HELPER`. Read by the qualified-call /
            // qualified-newtype-member render arms.
            ImportKind::Qualified { path, alias } => {
                let ns = module_namespace(path);
                qualified_imports.insert(alias.as_str(), ns.clone());
            }
            _ => {}
        }
    }
    TopLevel {
        module,
        module_key,
        local_newtypes,
        cross_module_imports,
        cross_module_newtypes,
        qualified_imports,
        package,
        prepared,
        body_plans: None,
        local_runtime_bindings,
    }
}

// =========================================================================
// Errors
// =========================================================================

/// A build-time error from the JS lowering pass — mapped to a non-zero
/// exit code by the caller.
#[derive(Debug)]
pub struct EmitError {
    pub message: String,
}

impl EmitError {
    #[cfg(feature = "cli")]
    fn unsupported(detail: impl Into<String>) -> Self {
        EmitError {
            message: detail.into(),
        }
    }

    fn internal(detail: impl Into<String>) -> Self {
        EmitError {
            message: format!("internal JavaScript emitter error: {}", detail.into()),
        }
    }
}

/// Collision-free generated identity for one nominal declaration. The NUL
/// frame makes `(module, name)` injective before hexadecimal encoding, and the
/// encoded form is safe both as a JavaScript string key and inside a generated
/// TypeScript identifier.
pub(crate) fn encoded_newtype_identity(module_path: &str, name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity((module_path.len() + name.len() + 1) * 2);
    for byte in module_path
        .bytes()
        .chain(std::iter::once(0))
        .chain(name.bytes())
    {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Render a property access for a JS object, using dot syntax when
/// the key is a legal JS identifier and bracket syntax otherwise.
fn js_member_access(base: &str, key: &str) -> String {
    if is_legal_js_identifier(key) {
        format!("{base}.{key}")
    } else {
        format!("{base}[\"{key}\"]")
    }
}

/// Render an object-literal key, with quoting only when needed.
fn js_object_key(key: &str) -> String {
    if is_legal_js_identifier(key) {
        key.to_owned()
    } else {
        format!("\"{key}\"")
    }
}

fn is_legal_js_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

// =========================================================================
// Lowering: declarations
// =========================================================================

fn lower_fn_def(d: &FnDef, top: &TopLevel, out: &mut String) -> Result<(), EmitError> {
    let runtime_groups = runtime_signature_groups(&d.sig);
    out.push_str("const ");
    out.push_str(&mangle_js_ident(&d.name));
    out.push_str(" = ");
    let mut locals: Vec<String> = runtime_groups
        .iter()
        .flat_map(|group| group.param_names().iter().map(|s| (*s).to_owned()))
        .collect();
    let mut body_js = String::new();
    lower_expr(&d.body, top, &mut locals, &mut body_js)?;
    let plan = top
        .body_plans
        .map(|plans| plans.function_plan(top.module_key, &d.name));
    out.push_str(&protect_js_value(
        plan,
        wrap_js_signature_stages(&runtime_groups, body_js),
    ));
    out.push_str(";\n");
    Ok(())
}

fn protect_js_value(plan: Option<usize>, value: String) -> String {
    match plan {
        Some(plan) => format!(
            "__kioProtect(__kioValueRef(__kioBodyPlans[{plan}], __kioBodyPlans[{plan}].root, __kioBodyContext), {value}, false)"
        ),
        None => value,
    }
}

#[derive(Debug, Clone)]
enum JsRuntimeSignatureGroup<'a> {
    Type,
    Value(Vec<&'a str>),
}

impl JsRuntimeSignatureGroup<'_> {
    fn param_names(&self) -> &[&str] {
        match self {
            Self::Type => &[],
            Self::Value(names) => names,
        }
    }
}

fn runtime_signature_groups(sig: &Signature) -> Vec<JsRuntimeSignatureGroup<'_>> {
    let mut groups = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            SignatureGroupRef::Type(params) => {
                groups.extend(params.iter().map(|_| JsRuntimeSignatureGroup::Type));
            }
            SignatureGroupRef::Value(params) => {
                groups.push(JsRuntimeSignatureGroup::Value(value_param_names(params)));
            }
        }
    }
    if groups.is_empty() {
        groups.push(JsRuntimeSignatureGroup::Value(Vec::new()));
    }
    groups
}

fn wrap_js_signature_stages(groups: &[JsRuntimeSignatureGroup<'_>], mut body: String) -> String {
    for group in groups.iter().rev() {
        body = match group {
            JsRuntimeSignatureGroup::Type => format!("__kioPoly(() => {body}, null)"),
            JsRuntimeSignatureGroup::Value(names) => {
                let names = mangle_param_list(names);
                match names.as_slice() {
                    [] => format!("__kioLift(__kioUnit => {body})"),
                    [name] => format!("__kioLift(({name}) => {body})"),
                    _ => format!(
                        "__kioLift(__kioDomain => {{ const [{}] = __kioProductSlots(__kioDomain, {}); return {body}; }})",
                        names.join(", "),
                        names.len(),
                    ),
                }
            }
        };
    }
    body
}

fn value_param_names(params: &[SignatureParam]) -> Vec<&str> {
    params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(vp) => Some(vp.name.as_str()),
            SignatureParam::Type(_) => None,
        })
        .collect()
}

/// Render a value-parameter name list into the JS identifiers that
/// appear in the emitted parameter list.
///
/// Each name routes through [`mangle_js_ident`], except the wildcard
/// `_`: JS forbids two parameters with the same name in one list
/// (`(_, _) => …` is a `SyntaxError`), but Kio admits any number of
/// `_` slots in one binder list (`fn f((_: a, _: b), _: c)`). A
/// wildcard is non-referenceable in Kio, so the body never names it;
/// we therefore freshen each `_` to a distinct `_<i>` identifier,
/// keeping the parameter list well-formed while preserving the
/// "nothing reads this slot" intent. The `__name__` shape stays
/// reserved, so `_<i>` can never collide with a user identifier.
fn mangle_param_list(value_params: &[&str]) -> Vec<String> {
    value_params
        .iter()
        .enumerate()
        .map(|(i, n)| {
            if *n == "_" {
                format!("_{i}")
            } else {
                mangle_js_ident(n).into_owned()
            }
        })
        .collect()
}

fn js_nominal_operation(top: &TopLevel, frame: &str, value: &str, wrap: bool) -> String {
    let identity = serde_json::to_string(frame).expect("nominal identity serializes");
    if top.body_plans.is_some() {
        let method = if wrap { "wrap" } else { "unwrap" };
        format!("__kioBodyContext.nominals.get({identity}).{method}({value})")
    } else {
        let helper = if wrap {
            "__kioOpaqueOut"
        } else {
            "__kioOpaqueIn"
        };
        format!("{helper}({identity}, {value})")
    }
}

fn wrap_js_type_stages(mut value: String, count: usize) -> String {
    for _ in 0..count {
        value = format!("__kioPoly(() => ({value}), null)");
    }
    value
}

fn lower_newtype(d: &Newtype, top: &TopLevel, out: &mut String) {
    let name = QualifiedTypeName::new(
        top.module_key.split('/').map(str::to_owned).collect(),
        &d.name,
    )
    .expect("a routed newtype has an exact declaring module");
    let frame = super::super::serialized_runtime_ir::exact_nominal_identity_frame(&name);
    let ctor = format!(
        "__kioLift(__kioPayload => ({}))",
        js_nominal_operation(top, &frame, "__kioPayload", true)
    );
    let ctor = wrap_js_type_stages(ctor, d.type_params.len() + d.existential_params.len());
    let payload = js_nominal_operation(top, &frame, "__kioNominal", false);
    let projected = if d.existential_params.is_empty() {
        payload
    } else {
        let continuation =
            advance_erased_type_stages("__kioContinuation".to_owned(), d.existential_params.len());
        format!(
            "(() => {{ const __kioPayload = {payload}; return __kioPoly(() => __kioLift(__kioContinuation => ({continuation})(__kioPayload)), null); }})()"
        )
    };
    let projector = wrap_js_type_stages(
        format!("__kioLift(__kioNominal => ({projected}))"),
        d.type_params.len(),
    );
    out.push_str(&format!(
        "const {} = {{ {}: {ctor}, {}: {projector} }};\n",
        d.name, d.constructor.name, d.projector.name
    ));
}

// =========================================================================
// Lowering: expressions
// =========================================================================

/// Whether a literal's resolved `(Type)` annotation names a host type
/// whose role is wide-int (`i64` / `i128` / `u64` / `u128`) — the JS
/// backend emits those as BigInt. Post-substitute every literal
/// carries its resolved host type in `annotation`.
fn literal_annotation_is_wide_int(annotation: &crate::ast::Type<Routed>, top: &TopLevel) -> bool {
    let role = match top.package {
        Some(package) => crate::host_descriptor::exact_host_type_role(package, annotation),
        None => {
            if !matches!(annotation, Type::Path { args, .. } if args.is_empty()) {
                return false;
            }
            let Some((module_path, name)) =
                crate::host_descriptor::routed_host_type_identity(annotation)
            else {
                return false;
            };
            if module_path != top.module_key {
                return false;
            }
            top.module.items.iter().find_map(|item| match item {
                Item::HostType(host_type) if host_type.name == name => {
                    host_type.role.map(|role| role.role)
                }
                _ => None,
            })
        }
    };
    role.is_some_and(|role| role.is_wide_int())
}

fn lower_expr(
    e: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    out: &mut String,
) -> Result<(), EmitError> {
    match e {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::Unit { .. } => {
            out.push_str("null");
            Ok(())
        }
        Expr::Let {
            name, value, body, ..
        } => {
            // Non-shadow:  `(() => { const <name> = <value>; return <body>; })()`.
            //
            // Shadow case (an outer local or module-level runtime binding
            // of the same Kio name is visible): `<value>` references to the
            // outer `<name>` would resolve to the inner `const <name>` and
            // hit JS's TDZ (`ReferenceError: access of uninitialized
            // binding`), since the inner binder is in scope throughout the
            // IIFE — even during its own initializer.
            //
            // Fix: hoist the RHS into an outer-scope temp before
            // opening the IIFE that introduces the inner binder.
            //
            //   (() => {
            //     const __kio_let_init = <value>;     // outer scope: outer <name> visible
            //     return (() => {
            //       const <name> = __kio_let_init;     // inner scope: shadows outer <name>
            //       return <body>;
            //     })();
            //   })()
            //
            // The temp name `__kio_let_init` is safe at any nesting depth
            // because each shadow let opens its own IIFE pair, so siblings
            // and nested shadows never share a scope. User identifiers
            // cannot begin with `__`, so no collision with source names.
            let is_shadow = locals.iter().any(|n| n == name)
                || top.local_runtime_bindings.contains(name.as_str());
            if is_shadow {
                out.push_str("(() => { const __kio_let_init = ");
                lower_expr(value, top, locals, out)?;
                out.push_str("; return (() => { const ");
                out.push_str(&mangle_js_ident(name));
                out.push_str(" = __kio_let_init; return ");
                locals.push(name.clone());
                let body_result = lower_expr(body, top, locals, out);
                locals.pop();
                body_result?;
                out.push_str("; })(); })()");
            } else {
                out.push_str("(() => { const ");
                out.push_str(&mangle_js_ident(name));
                out.push_str(" = ");
                lower_expr(value, top, locals, out)?;
                out.push_str("; return ");
                locals.push(name.clone());
                let body_result = lower_expr(body, top, locals, out);
                locals.pop();
                body_result?;
                out.push_str("; })()");
            }
            Ok(())
        }
        Expr::Seq { value, body, .. } => {
            // `e;` discards the value of `e` (typer ensures `e : .`).
            // Codegen is identical to `let <fresh> = e;` minus the
            // binder — `(() => { <value>; return <body>; })()` — no
            // shadowing concern because there's no binding name.
            out.push_str("(() => { ");
            lower_expr(value, top, locals, out)?;
            out.push_str("; return ");
            lower_expr(body, top, locals, out)?;
            out.push_str("; })()");
            Ok(())
        }
        Expr::FnExpr { sig, body, .. } => {
            // Wrap the arrow function in parentheses so an outer `Call`
            // applies to the arrow itself rather than to its body —
            // `((x) => x)(null)`, not the precedence-broken
            // `(x) => x(null)`.
            let runtime_groups = runtime_signature_groups(sig);
            let mark = locals.len();
            for group in &runtime_groups {
                for n in group.param_names() {
                    locals.push((*n).to_owned());
                }
            }
            let mut body_js = String::new();
            let body_result = lower_expr(body, top, locals, &mut body_js);
            locals.truncate(mark);
            let plan = top
                .body_plans
                .map(|plans| plans.creation_plan(top.module_key, e));
            out.push_str(&protect_js_value(
                plan,
                wrap_js_signature_stages(&runtime_groups, body_js),
            ));
            body_result
        }
        Expr::StrLit { value, .. } => {
            out.push('"');
            push_js_escaped(value, out);
            out.push('"');
            Ok(())
        }
        Expr::IntLit {
            digits, annotation, ..
        } => {
            // Widths up to 32 bits fit in JS Number; ≥64-bit roles emit
            // BigInt literal syntax (`<digits>n`) so the runtime keeps
            // exact precision. Underscores in `digits` carry through —
            // ES2021 numeric separators are valid in both Number and
            // BigInt literal forms. Post-substitute, every literal
            // carries its resolved host type in `annotation`; the
            // role's width decides Number vs BigInt.
            out.push_str(digits);
            if literal_annotation_is_wide_int(annotation, top) {
                out.push('n');
            }
            Ok(())
        }
        Expr::FloatLit { digits, .. } => {
            out.push_str(digits);
            Ok(())
        }
        Expr::BoolLit { value, .. } => {
            out.push_str(if *value { "true" } else { "false" });
            Ok(())
        }
        // `Routed` carries forward Prime's uninhabited witnesses for every
        // surface-only and elaboration-bearing variant. Earlier passes either
        // remove the syntax or install its recorded elaboration at the
        // `Lowered → Prime` boundary. Each arm is a `match *ext {}` proof over
        // `Never`.
        Expr::Tuple { ext, .. } => match *ext {},
        // `Expr::FnPlaceholder` is uninhabited (the desugar pass
        // lowered every `.stem. { e }` to a regular `fn`).
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        // `Expr::LabelValue` is uninhabited (`pass/label_elab` lowered
        // every `{f = e}` to a constructor-member call).
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        // `Expr::Elaborator` is uninhabited: the
        // typer's substitution pass (`pass/substitute`) replaced each
        // surface node with its elaborated Kio' tree, so codegen
        // sees only the elaborated form. Each arm here is a
        // type-system proof.
        Expr::Elaborator { ext, .. } => match *ext {},
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { ext, .. } => match *ext {},
        // `Expr::Ufcs` is uninhabited: typechecking records the
        // public-type-directed replacement for every ordinary or bang-suffix
        // dot-splice, and `Lowered → Prime` substitution installs it.
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        // The seven enriched structural nodes — recovered from
        // right-leaning intrinsic chains by `structural_recovery`.
        // Their lowerings emit the structural form directly over the
        // same nested-binary runtime rep the intrinsic chains use,
        // so they are observationally identical to the chains they
        // replace — just without the per-`__either__` IIFE wrappers
        // and the `__if_then_else__` thunk pair.
        Expr::EnrichedTuple { items, .. } => lower_enriched_tuple(items, top, locals, out),
        Expr::EnrichedProject {
            target,
            index,
            arity,
            ..
        } => lower_enriched_project(target, *index, *arity, top, locals, out),
        Expr::EnrichedInject {
            payload,
            variant,
            variants,
            ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            lower_enriched_inject(
                InjectEmit {
                    payload,
                    variant: *variant,
                    variants: *variants,
                },
                &mut ctx,
                out,
            )
        }
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            lower_enriched_match(MatchEmit { scrutinee, arms }, &mut ctx, out)
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            lower_enriched_conditional(
                ConditionalEmit {
                    cond,
                    then_branch,
                    else_branch,
                },
                &mut ctx,
                out,
            )
        }
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            target_ty,
            ..
        } => {
            let mut selected = String::new();
            lower_enriched_project(target, *index, *arity, top, locals, &mut selected)?;
            let frame = if let Some(plans) = top.body_plans {
                plans.field_nominal_frame(top.module_key, e).to_owned()
            } else {
                let slot = super::super::reconstruct::nth_product_slot_ty_for_arity(
                    target_ty, *index, *arity,
                )
                .expect("a routed field projection fits its resolved product arity");
                let Type::Path { segments, .. } = slot else {
                    unreachable!("a routed field projection selects a nominal slot")
                };
                let (name, module) = segments.split_last().expect("a nominal path is nonempty");
                let module = if module.is_empty() {
                    top.module_key.split('/').map(str::to_owned).collect()
                } else {
                    module.iter().map(|part| part.name.clone()).collect()
                };
                let nominal = QualifiedTypeName::new(module, &name.name)
                    .expect("a field has an exact nominal identity");
                super::super::serialized_runtime_ir::exact_nominal_identity_frame(&nominal)
            };
            out.push_str(&js_nominal_operation(top, &frame, &selected, false));
            Ok(())
        }
        // Low-IR variants — produced by `recover_to_low::lower`. Each
        // variant dispatches to its per-variant emit helper; the
        // helpers below carry no classification logic, only the
        // syntactic templating for the JS surface.
        Expr::LowBoundRef { name, .. } => emit_bound_ref(name, out),
        Expr::LowHostFnValueRef {
            name,
            module_path,
            sig,
            ..
        } => {
            let value = emit_host_fn_value_ref(name, module_path, sig, top)?;
            let plan = top
                .body_plans
                .map(|plans| plans.creation_plan(top.module_key, e));
            out.push_str(&protect_js_value(plan, value));
            Ok(())
        }
        Expr::LowModuleFnValueRef { mangled, .. } => {
            emit_module_fn_value_ref(mangled, top, out);
            Ok(())
        }
        Expr::LowHostCall {
            name,
            module_path,
            type_args,
            args,
            sig,
            ..
        } => {
            let callee = emit_host_fn_value_ref(name, module_path, sig, top)?;
            let mut ctx = ExprLowerCtx { top, locals };
            emit_canonical_call(e, callee, type_args, args, &mut ctx, out)
        }
        Expr::LowModuleCall {
            mangled,
            type_args,
            args,
            ..
        } => {
            let mut callee = String::new();
            emit_module_fn_value_ref(mangled, top, &mut callee);
            let mut ctx = ExprLowerCtx { top, locals };
            emit_canonical_call(e, callee, type_args, args, &mut ctx, out)
        }
        Expr::LowQualifiedModuleCall {
            alias,
            mangled,
            type_args,
            args,
            ..
        } => {
            let member = mangled
                .strip_prefix(&format!("{alias}."))
                .unwrap_or(mangled);
            let namespace = top.qualified_imports.get(alias.as_str()).cloned().unwrap_or_else(|| {
                assert!(
                    top.package.is_some_and(|package| package.module(alias).is_some()),
                    "qualified call `{alias}.{member}` is absent from its resolved module-import map"
                );
                module_namespace_from_key(alias)
            });
            let callee = format!("{namespace}.{}", mangle_js_ident(member));
            let mut ctx = ExprLowerCtx { top, locals };
            emit_canonical_call(e, callee, type_args, args, &mut ctx, out)
        }
        Expr::LowQualifiedNewtypeMember {
            module_path,
            newtype,
            member,
            type_args,
            payload,
            ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            emit_qualified_newtype_member(
                QualifiedNewtypeMemberEmit {
                    module_path,
                    newtype,
                    member,
                    type_args,
                    payload,
                },
                &mut ctx,
                out,
            )
        }
        Expr::LowNewtypeCtor {
            newtype,
            member,
            type_args,
            payload,
            ..
        } => emit_newtype_member(newtype, member, type_args, payload, top, locals, out),
        Expr::LowNewtypeProj {
            newtype,
            member,
            type_args,
            target,
            ..
        } => emit_newtype_member(newtype, member, type_args, target, top, locals, out),
        Expr::LowClosureCall {
            name,
            type_args,
            args,
            ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            emit_canonical_call(
                e,
                mangle_js_ident(name).into_owned(),
                type_args,
                args,
                &mut ctx,
                out,
            )
        }
        Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } => {
            let mut callable = String::new();
            lower_expr(callee, top, locals, &mut callable)?;
            let mut ctx = ExprLowerCtx { top, locals };
            emit_canonical_call(e, callable, type_args, args, &mut ctx, out)
        }
        Expr::LowTypeApplication { callee, .. } => {
            let plan = top
                .body_plans
                .map(|plans| plans.application_plan(top.module_key, e));
            let mut callable = String::new();
            lower_expr(callee, top, locals, &mut callable)?;
            out.push_str(&protect_js_value(plan, format!("({callable}).advance()")));
            Ok(())
        }
        Expr::LowAbsurdCall { value_arg, .. } => emit_absurd_call(value_arg, top, locals, out),
        Expr::LowCpsProjectorApply {
            newtype,
            module_path,
            type_args,
            receiver,
            continuation,
            continuation_ty,
            ..
        } => {
            let mut ctx = ExprLowerCtx { top, locals };
            emit_cps_projector_apply(
                e,
                (newtype, module_path, type_args),
                receiver,
                continuation,
                continuation_ty,
                &mut ctx,
                out,
            )
        }
        // Routed-uninhabited variants: `Expr::Path` / `Expr::Call`
        // have been routed through `Low*` variants by
        // `recover_to_low::lower`. At Routed, their `ext` witness is
        // `Never`, so each arm discharges via `match *ext {}`.
        Expr::Path { ext, .. } => match *ext {},
        Expr::Call { ext, .. } => match *ext {},
    }
}

// =========================================================================
// Lowering: enriched structural nodes
// =========================================================================
//
// Structural nodes retain binary topology. Recovered n-ary roots only
// compress traversal: their final slot remains the complete right tail.

struct ExprLowerCtx<'a, 'm> {
    top: &'a TopLevel<'m>,
    locals: &'a mut Vec<String>,
}

impl ExprLowerCtx<'_, '_> {
    fn lower_expr(&mut self, expr: &Expr, out: &mut String) -> Result<(), EmitError> {
        lower_expr(expr, self.top, &mut *self.locals, out)
    }
}

/// Rebuild the recovered right spine from canonical child values.
fn lower_enriched_tuple(
    items: &[Expr],
    top: &TopLevel,
    locals: &mut Vec<String>,
    out: &mut String,
) -> Result<(), EmitError> {
    // The recovery guarantees `items.len() >= 2`.
    let n = items.len();
    let mut ctx = ExprLowerCtx { top, locals };
    let item_refs: Vec<&Expr> = items.iter().collect();
    if let Some(plan) = bound_product_rebuild_plan(&item_refs, NominalValueRepresentation::Retained)
    {
        return lower_enriched_product_with_cached_slots(&item_refs, &plan, &mut ctx, out);
    }
    let mut rendered = Vec::with_capacity(n);
    for item in items {
        let mut value = String::new();
        ctx.lower_expr(item, &mut value)?;
        rendered.push(value);
    }
    lower_enriched_product_from_rendered(&rendered, out);
    Ok(())
}

fn lower_enriched_product_from_rendered(rendered: &[String], out: &mut String) {
    let n = rendered.len();
    debug_assert!(n > 0);
    for item in &rendered[..n - 1] {
        out.push_str("({ _0: ");
        out.push_str(item);
        out.push_str(", _1: ");
    }
    out.push_str(&rendered[n - 1]);
    for _ in 0..n - 1 {
        out.push_str(" })");
    }
}

fn canonical_js_domain(values: &[String]) -> String {
    if values.is_empty() {
        return "null".to_owned();
    }
    let mut domain = String::new();
    lower_enriched_product_from_rendered(values, &mut domain);
    domain
}

fn lower_enriched_product_with_cached_slots(
    items: &[&Expr],
    plan: &ProductRebuildPlan,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let rendered = render_cached_product_rebuild(
        plan,
        |slot| format!("__slots[{slot}]"),
        |i| {
            let item = items[i];
            let mut v = String::new();
            ctx.lower_expr(item, &mut v)?;
            Ok(v)
        },
        |slots| {
            let mut product = String::new();
            lower_enriched_product_from_rendered(slots, &mut product);
            product
        },
        |source, source_arity, product| {
            let mut rendered = String::new();
            rendered.push_str("(() => { const __slots = __kioProductSlots(");
            rendered.push_str(&mangle_js_ident(source));
            rendered.push_str(&format!(", {source_arity}); return "));
            rendered.push_str(&product);
            rendered.push_str("; })()");
            rendered
        },
    )?;
    out.push_str(&rendered);
    Ok(())
}

/// `EnrichedProject(target, index, arity)` → a bounded runtime peel
/// into `target`'s nested-binary product.
fn lower_enriched_project(
    target: &Expr,
    index: usize,
    arity: usize,
    top: &TopLevel,
    locals: &mut Vec<String>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push_str("__kioProductSlot((");
    lower_expr(target, top, locals, out)?;
    out.push_str(&format!("), {index}, {arity})"));
    Ok(())
}

/// `EnrichedInject(payload, variant, variants)` → helper-built
/// nested-binary tagged form.
struct InjectEmit<'a> {
    payload: &'a Expr,
    variant: usize,
    variants: usize,
}

fn lower_enriched_inject(
    inject: InjectEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let InjectEmit {
        payload,
        variant,
        variants,
    } = inject;
    let mut rendered = String::new();
    ctx.lower_expr(payload, &mut rendered)?;
    out.push_str("__kioSumInject(");
    out.push_str(&rendered);
    out.push_str(&format!(", {variant}, {variants})"));
    Ok(())
}

/// `EnrichedConditional(cond, then, else)` → a JS ternary
/// `(<cond> ? <then> : <else>)`. The `__if_then_else__` IIFE wrapper
/// and the two branch thunks are gone; the ternary keeps the same
/// one-branch-only evaluation.
struct ConditionalEmit<'a> {
    cond: &'a Expr,
    then_branch: &'a Expr,
    else_branch: &'a Expr,
}

fn lower_enriched_conditional(
    conditional: ConditionalEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let ConditionalEmit {
        cond,
        then_branch,
        else_branch,
    } = conditional;
    out.push('(');
    ctx.lower_expr(cond, out)?;
    out.push_str(" ? ");
    let mut then_rendered = String::new();
    ctx.lower_expr(then_branch, &mut then_rendered)?;
    out.push_str(&then_rendered);
    out.push_str(" : ");
    let mut else_rendered = String::new();
    ctx.lower_expr(else_branch, &mut else_rendered)?;
    out.push_str(&else_rendered);
    out.push(')');
    Ok(())
}

/// `EnrichedMatch(scrutinee, arms)` → one payload decode followed by
/// a variant switch. This collapses both the old per-`__either__`
/// IIFE chain and the repeated nested-binary peel expressions.
struct MatchEmit<'a> {
    scrutinee: &'a Expr,
    arms: &'a [EnrichedArm],
}

fn lower_enriched_match(
    match_emit: MatchEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let MatchEmit { scrutinee, arms } = match_emit;
    // The recovery guarantees `arms.len() >= 2`.
    let n = arms.len();
    out.push_str("(() => { const __sum = __kioSumPayload(");
    ctx.lower_expr(scrutinee, out)?;
    out.push_str(&format!(", {n}); switch (__sum[0]) {{ "));
    for (k, arm) in arms.iter().enumerate() {
        out.push_str(&format!("case {k}: return "));
        lower_enriched_match_arm(
            MatchArmEmit {
                arm,
                payload_expr: "__sum[1]",
            },
            ctx,
            out,
        )?;
        out.push_str("; ");
    }
    out.push_str("default: throw new Error(\"sum payload out of bounds\"); } })()");
    Ok(())
}

/// Emit one match arm as `((<param>) => <body>)(<payload_expr>)` —
/// the handler applied to the variant's payload.
struct MatchArmEmit<'a> {
    arm: &'a EnrichedArm,
    payload_expr: &'a str,
}

fn lower_enriched_match_arm(
    arm_emit: MatchArmEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let MatchArmEmit { arm, payload_expr } = arm_emit;
    out.push_str("((");
    out.push_str(&mangle_js_ident(&arm.param));
    out.push_str(") => ");
    ctx.locals.push(arm.param.clone());
    let mut body = String::new();
    let body_result = ctx.lower_expr(&arm.body, &mut body);
    ctx.locals.pop();
    body_result?;
    out.push_str(&body);
    out.push_str(")(");
    out.push_str(payload_expr);
    out.push(')');
    Ok(())
}

/// Append `s` to `out` with JS-string-literal escapes applied. Handles
/// the narrow set: `\\`, `"`, `\n`, `\r`, `\t`, and bare control
/// characters via `\xNN`. Other characters pass through as-is.
fn push_js_escaped(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

// =========================================================================
// Per-variant emit helpers
// =========================================================================
//
// These render the `Expr::Low*` variants the Routed phase produces.
// Each helper is a small piece of syntactic templating — the call
// kind (host vs module vs newtype-member vs closure vs ...) was
// classified upstream in `recover_to_low::lower`, and the helper
// simply writes the corresponding JS surface.

/// `LowBoundRef { name }` → bare identifier reference. The name is a
/// fn parameter, a let local, or a match-arm payload binder. JS
/// reserved-word mangling applies in identifier position.
///
/// Returns an emit error when `name` is one of the eight intrinsics
/// (`__pair__` / `__fst__` / `__snd__` / `__left__` /
/// `__right__` / `__either__` / `__if_then_else__` / `__absurd__`):
/// the typer rejects an intrinsic used in value position (the
/// polymorphic value isn't monomorphisable), and the recovery pass is
/// total over them at call position, so anything reaching here is a
/// pipeline regression, so fail as a compiler bug rather than fall through to
/// an undefined-symbol JS source.
fn emit_bound_ref(name: &str, out: &mut String) -> Result<(), EmitError> {
    if is_intrinsic_name(name) {
        unreachable!("intrinsic `{name}` reached JavaScript lowering in value position");
    }
    out.push_str(&mangle_js_ident(name));
    Ok(())
}

/// True for the eight intrinsic names. The seven structural
/// intrinsics are collapsed into `Enriched*` nodes by
/// `crate::pass::structural_recovery`; `__absurd__` is classified into
/// `Expr::LowAbsurdCall` by `crate::pass::recover_to_low::lower`. Either
/// one reaching the JS lowering as a bare reference is a pipeline
/// regression — see [`emit_bound_ref`].
fn is_intrinsic_name(name: &str) -> bool {
    matches!(
        name,
        "__left__"
            | "__right__"
            | "__either__"
            | "__pair__"
            | "__fst__"
            | "__snd__"
            | "__if_then_else__"
            | "__absurd__"
    )
}

/// `LowHostFnValueRef { name, sig, ret_ty }` → a JS function that
/// forwards through the host record with the same FFI conversion as a
/// direct host call. The host record keys use the public word-case rendering of names
/// declared in the package host, so they pass through
/// unmangled.
/// Render the namespaced host-fn accessor for the rung-1 nested host
/// record: `__host__.<MODULE_KEY>.<name>`. The exact module key matches
/// [`host_module_key`], so source underscores remain distinct from path
/// separators. See [`render_prepared_js_host_factory_prelude`].
fn host_fn_accessor(name: &str, module_path: &str) -> String {
    format!(
        "__host__.{}.{}",
        host_module_key(module_path),
        host_name_core(name)
    )
}

fn prepared_js_host_site_id(
    module_path: &str,
    name: &str,
) -> Result<BoundaryFacadeSiteId, EmitError> {
    Ok(BoundaryFacadeSiteId::new(
        module_path.split('/').map(str::to_owned).collect(),
        BoundaryFacadeSiteOwner::HostFunction {
            name: name.to_owned(),
        },
    )
    .unwrap_or_else(|| {
        unreachable!("invalid JavaScript host facade identity {module_path}.{name}")
    }))
}

fn prepared_js_host_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    module_path: &str,
    name: &str,
) -> Result<PreparedBoundaryCallableSite<'a>, EmitError> {
    let id = prepared_js_host_site_id(module_path, name)?;
    find_prepared_js_site(prepared, &id)
}

fn has_prepared_js_host_site(
    prepared: &PreparedBoundaryCallableSites,
    module_path: &str,
    name: &str,
) -> Result<bool, EmitError> {
    let id = prepared_js_host_site_id(module_path, name)?;
    Ok(prepared.site(&id).is_some())
}

fn protect_js_boundary(
    site: PreparedBoundaryCallableSite<'_>,
    value: String,
    incoming: bool,
) -> String {
    let key = super::super::serialized_runtime_ir::boundary_site_key(site.site());
    let key = serde_json::to_string(&key).expect("prepared site key serializes");
    format!(
        "__kioProtect(__kioBoundaryValueRef(__kioBoundarySites, {key}, __kioBodyContext), {value}, {incoming})"
    )
}

/// Lift a native named declaration into its already prepared ordered stages.
/// Each value group captures one canonical semantic domain; the native call
/// receives the original source-grouped parameters only when its head is complete.
fn render_js_native_host_value(accessor: &str, stages: &[Option<usize>]) -> String {
    let count = stages.iter().flatten().sum();
    let names = (0..count)
        .map(|index| format!("__kioHostArg{index}"))
        .collect::<Vec<_>>();
    let mut cursor = 0;
    let groups = stages
        .iter()
        .map(|stage| match stage {
            None => JsRuntimeSignatureGroup::Type,
            Some(count) => {
                let end = cursor + count;
                let group = JsRuntimeSignatureGroup::Value(
                    names[cursor..end].iter().map(String::as_str).collect(),
                );
                cursor = end;
                group
            }
        })
        .collect::<Vec<_>>();
    let body = format!("{accessor}({})", names.join(", "));
    wrap_js_signature_stages(&groups, body)
}

fn emit_host_fn_value_ref(
    name: &str,
    module_path: &str,
    sig: &Signature,
    top: &TopLevel<'_>,
) -> Result<String, EmitError> {
    let accessor = host_fn_accessor(name, module_path);
    if let Some(prepared) = top.prepared
        && has_prepared_js_host_site(prepared, module_path, name)?
    {
        let site = prepared_js_host_site(prepared, module_path, name)?;
        let execution = prepared_js_execution(site)?;
        let stages = execution
            .head_stages()
            .iter()
            .map(|stage| match stage {
                CallableExecutionStage::Type { .. } => None,
                CallableExecutionStage::Value(layout) => Some(layout.public_parameter_count()),
            })
            .collect::<Vec<_>>();
        return Ok(protect_js_boundary(
            site,
            render_js_native_host_value(&accessor, &stages),
            true,
        ));
    }
    // Unbridged declarations cannot be supplied by this package's host, but
    // their unreachable bodies still compile under open-world extension.
    // Their resolved signature supplies execution stages, not facade authority.
    let stages = runtime_signature_groups(sig)
        .iter()
        .map(|stage| match stage {
            JsRuntimeSignatureGroup::Type => None,
            JsRuntimeSignatureGroup::Value(names) => Some(names.len()),
        })
        .collect::<Vec<_>>();
    Ok(render_js_native_host_value(&accessor, &stages))
}

/// `LowModuleFnValueRef { mangled }` → cross-module-aware reference.
/// When `mangled` matches a cross-module-import entry in
/// `top.cross_module_imports`, emits `<NS>.<mangled>`; otherwise
/// emits the local-IIFE-scoped identifier (mangled for reserved-
/// word collision).
fn emit_module_fn_value_ref(mangled: &str, top: &TopLevel, out: &mut String) {
    if let Some(ns) = top.cross_module_imports.get(mangled) {
        out.push_str(ns);
        out.push('.');
        out.push_str(&mangle_js_ident(mangled));
        return;
    }
    out.push_str(&mangle_js_ident(mangled));
}

/// Apply one already-routed semantic value stage. The callee and each argument
/// are evaluated once, in source order; explicit type stages run before values.
/// Concrete specialization protection is installed before the value call can
/// return or store the specialized callable through an abstract slot.
fn emit_canonical_call(
    source: &Expr,
    callee: String,
    type_args: &[Type],
    args: &[Expr],
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let mut callable = advance_erased_type_stages(callee, type_args.len());
    if !type_args.is_empty() {
        let plan = ctx
            .top
            .body_plans
            .map(|plans| plans.application_plan(ctx.top.module_key, source));
        callable = protect_js_value(plan, callable);
    }
    // Qualified type-only declaration references retain their forwarding call
    // in Routed. They select a value but do not consume a value stage.
    if matches!(source,
        Expr::LowHostCall { sig, .. } | Expr::LowModuleCall { sig, .. } | Expr::LowQualifiedModuleCall { sig, .. }
        if sig.has_type_params() && !sig.has_value_group()
    ) {
        assert!(
            args.is_empty(),
            "a routed type-only declaration call has no value arguments"
        );
        out.push_str(&callable);
        return Ok(());
    }
    out.push('(');
    out.push_str(&callable);
    out.push_str(")(");
    let mut values = Vec::with_capacity(args.len());
    for argument in args {
        let mut value = String::new();
        ctx.lower_expr(argument, &mut value)?;
        values.push(value);
    }
    out.push_str(&canonical_js_domain(&values));
    out.push(')');
    Ok(())
}

/// `LowQualifiedNewtypeMember { module_path, newtype, member, payload }` →
/// `<NS>.<newtype>.<member>(<payload>)`.
struct QualifiedNewtypeMemberEmit<'a> {
    module_path: &'a str,
    newtype: &'a str,
    member: &'a str,
    type_args: &'a [Type],
    payload: &'a Expr,
}

fn emit_qualified_newtype_member(
    member_emit: QualifiedNewtypeMemberEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let QualifiedNewtypeMemberEmit {
        module_path,
        newtype,
        member,
        type_args,
        payload,
    } = member_emit;
    let ns = module_namespace_from_key(module_path);
    out.push_str(&ns);
    out.push('.');
    out.push_str(newtype);
    out.push('.');
    out.push_str(member);
    for _ in type_args {
        out.push_str(".advance()");
    }
    out.push('(');
    ctx.lower_expr(payload, out)?;
    out.push(')');
    Ok(())
}

/// `LowNewtypeCtor { newtype, member, payload }` and
/// `LowNewtypeProj { newtype, member, target }` (both single-payload
/// shape) → `<NewtypeNS>.<member>(<payload>)`. The NewtypeNS routes
/// to the local IIFE for same-module newtypes and to `<NS>.<NT>` for
/// cross-module imports.
fn emit_newtype_member(
    newtype: &str,
    member: &str,
    type_args: &[Type],
    payload: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    out: &mut String,
) -> Result<(), EmitError> {
    if let Some(ns) = top.cross_module_newtypes.get(newtype) {
        out.push_str(ns);
        out.push('.');
    }
    out.push_str(newtype);
    out.push('.');
    out.push_str(member);
    for _ in type_args {
        out.push_str(".advance()");
    }
    out.push('(');
    lower_expr(payload, top, locals, out)?;
    out.push(')');
    Ok(())
}

/// `LowAbsurdCall { value_arg }` → an IIFE that throws on the bottom
/// value. Per spec, a value of `!` can only ever come from a host fn
/// declared `-> !`, which in practice never returns; the throw is
/// belt-and-braces if a misbehaving host violates that.
fn emit_absurd_call(
    value_arg: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push_str("((__bottom__) => { throw new Error('Kio: __absurd__ on bottom value'); })(");
    lower_expr(value_arg, top, locals, out)?;
    out.push(')');
    Ok(())
}

fn emit_cps_projector_apply(
    occurrence: &Expr,
    owner: (&str, &str, &[Type]),
    receiver: &Expr,
    continuation: &Expr,
    continuation_ty: &Type,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let (newtype, module_path, type_args) = owner;
    let declaration = cps_projector_newtype(newtype, module_path, ctx.top)?;
    let mut receiver_js = String::new();
    ctx.lower_expr(receiver, &mut receiver_js)?;
    let mut continuation_js = String::new();
    ctx.lower_expr(continuation, &mut continuation_js)?;
    let (_, callable) = continuation_ty.peel_leading_foralls();
    let Type::Function { abi_arity, .. } = callable else {
        unreachable!("a routed CPS projector continuation has a function type")
    };
    assert!(
        *abi_arity <= 1,
        "a routed CPS projector continuation has zero or one ABI slot"
    );

    let plan = ctx
        .top
        .body_plans
        .map(|plans| plans.continuation_plan(ctx.top.module_key, occurrence));
    let continuation_js = protect_js_value(plan, continuation_js);
    out.push_str("(() => { const __kio_cps_receiver = ");
    out.push_str(&receiver_js);
    out.push_str("; const __kio_cps_continuation = ");
    out.push_str(&continuation_js);
    out.push_str("; return ");
    if module_path != ctx.top.module_key {
        out.push_str(&module_namespace_from_key(module_path));
        out.push('.');
    }
    out.push_str(newtype);
    out.push('.');
    out.push_str(&declaration.projector.name);
    for _ in type_args {
        out.push_str(".advance()");
    }
    out.push_str("(__kio_cps_receiver).advance()(__kio_cps_continuation); })()");
    Ok(())
}

fn advance_erased_type_stages(rendered: String, stages: usize) -> String {
    if stages == 0 {
        return rendered;
    }
    let mut advanced = String::with_capacity(rendered.len() + 2 + stages * 2);
    advanced.push('(');
    advanced.push_str(&rendered);
    advanced.push(')');
    for _ in 0..stages {
        advanced.push_str(".advance()");
    }
    advanced
}

fn cps_projector_newtype<'m>(
    newtype: &str,
    module_path: &str,
    top: &TopLevel<'m>,
) -> Result<&'m Newtype, EmitError> {
    if module_path == top.module_key {
        return Ok(top.local_newtypes.get(newtype).copied().unwrap_or_else(|| {
            unreachable!("CPS projector owner `{module_path}.{newtype}` is absent")
        }));
    }
    let package = top.package.unwrap_or_else(|| {
        unreachable!(
            "CPS projector owner module `{module_path}` reached lowering without package context"
        )
    });
    let entry = package
        .module(module_path)
        .unwrap_or_else(|| unreachable!("CPS projector owner module `{module_path}` is absent"));
    for item in &entry.module.items {
        let mut found = None;
        crate::pass::resolve::for_each_item_declaration(item, |declaration| {
            if let Some(candidate) = declaration.newtype()
                && candidate.name == newtype
            {
                found = Some(candidate);
            }
        });
        if let Some(candidate) = found {
            return Ok(candidate);
        }
    }
    unreachable!("CPS projector owner `{module_path}.{newtype}` is absent")
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::pass::parser::parse;
    use std::path::{Path, PathBuf};

    fn emitted_boundary(js: &str) -> serde_json::Value {
        let line = js
            .lines()
            .find_map(|line| line.trim().strip_prefix("const __kioBoundaryPlans = "))
            .expect("emitted shared boundary plans");
        serde_json::from_str(line.strip_suffix(';').expect("plan terminator"))
            .expect("serialized shared plans")
    }

    fn emitted_nominal_view(js: &str, module: &str, name: &str) -> serde_json::Value {
        let line = js
            .lines()
            .find(|line| line.trim().starts_with("const __kioBodyContext = "))
            .expect("body context");
        let views = line
            .split(", new Map(")
            .nth(1)
            .unwrap()
            .split("), __kioOpaqueIn")
            .next()
            .unwrap();
        let views: serde_json::Value = serde_json::from_str(views).expect("nominal view inventory");
        let key = serde_json::to_string(&serde_json::json!([[module], name])).unwrap();
        views
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry[0] == key)
            .expect("exact nominal view")[1]
            .clone()
    }

    fn emitted_site<'a>(
        plans: &'a serde_json::Value,
        module: &str,
        name: &str,
    ) -> &'a serde_json::Value {
        plans["sites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|site| {
                site["module"] == serde_json::json!([module]) && site["owner"]["name"] == name
            })
            .expect("exact callable site")
    }

    fn emitted_nominal<'a>(
        plans: &'a serde_json::Value,
        module: &str,
        name: &str,
    ) -> &'a serde_json::Value {
        plans["sites"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|site| site["nominals"].as_array().unwrap())
            .find(|item| {
                item["name"]["module"] == serde_json::json!([module])
                    && item["name"]["name"] == name
            })
            .expect("exact prepared nominal")
    }

    fn assert_recursive_view(
        package: &crate::pass::resolve::Package<Routed>,
        module: &str,
        name: &str,
        recursive: bool,
    ) {
        let prepared = prepare_js_boundary(package);
        let name =
            QualifiedTypeName::new(module.split('/').map(str::to_owned).collect(), name).unwrap();
        let site = prepared
            .sites()
            .find(|site| site.nominals().declaration(&name).is_some())
            .expect("exact recursive view declaration");
        assert_eq!(prepared_js_newtype_is_recursive(site, &name), recursive);
    }

    #[test]
    fn internal_and_host_module_names_preserve_semantic_paths() {
        assert_eq!(module_namespace_from_key("foo/bar"), "FOO_BAR");
        assert_eq!(
            module_namespace_from_key("foo_bar"),
            "KioInternalModule_foo_ubar"
        );
        assert_ne!(
            module_namespace_from_key("foo/bar"),
            module_namespace_from_key("foo_bar")
        );
        assert_eq!(host_module_key("foo/bar"), "foo_bar");
        assert_eq!(host_module_key("foo_bar"), "KioModule_fooBar");
    }

    #[test]
    fn canonical_calls_pack_one_domain_and_forward_function_values() {
        let js = lower_typed(
            "module main; \
             fn two(a: ., b: .) -> . { a } \
             fn slot(f: (. & .) -> .) -> ((. & .) -> .) { f } \
             fn relay() -> ((. & .) -> .) { slot(two) } \
             fn invoke() -> . { two((), ()) } \
             fn use_unit() -> ((. & .) -> .) { relay() }",
        );
        assert!(
            js.contains("const [a, b] = __kioProductSlots(__kioDomain, 2)"),
            "{js}"
        );
        // Recovery has already represented the declaration-arity bridge as an
        // ordinary lambda. Emission forwards that canonical function once.
        assert_eq!(js.matches("(slot)(").count(), 1, "{js}");
        assert_eq!(js.matches(" = two;").count(), 1, "{js}");
        assert!(js.contains("(two)(({ _0: null, _1: null }))"), "{js}");
        assert!(js.contains("(relay)(null)"), "{js}");
        assert!(
            !js.contains("__inner"),
            "canonical function values need no ABI adapter: {js}"
        );
    }

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

    /// Run a Surface module through desugar + label-elab to reach
    /// `Lowered`, re-phase to `Prime` with empty elaborations, run
    /// structural recovery to reach `Enriched`, then route through
    /// `recover_to_low::lower` to reach `Routed` — the phase
    /// `lower_module` consumes. Bypasses the typer because most
    /// `backends::js::emit` tests exercise pure Kio'-shape lowering and don't
    /// need (or have host types in scope for) literal-type checking.
    /// Tests that rely on substitution of `Expr::Elaborator` or
    /// `Expr::UserElaborator` carriers — surface bang calls such as `iso!`,
    /// `into!`, `onto!`, or `match!` — use the full pipeline via end-to-end
    /// golden cases instead.
    fn prime_package_routed(
        module_src: &str,
        package_file_src: Option<&str>,
    ) -> crate::pass::resolve::Package<Routed> {
        use crate::ast::Prime;
        use crate::pass::resolve::{ModuleEntry, Package, PackageFileEntry};
        use std::path::PathBuf;

        // Surface → Desugared → Lowered (typer-input).
        let m = parse(module_src).expect("parse");
        let m = crate::pass::desugar::desugar_module(m).expect("desugar");
        let parsed_modules = vec![(PathBuf::from("test.kio"), m)];

        let parsed_package_file = package_file_src.map(|src| {
            let e = crate::pass::parser::parse_package_file(&format!("package pkg;\n{src}"), None)
                .expect("parse package file");
            crate::pass::desugar::desugar_package_file(e).expect("desugar package file")
        });

        let (mut lowered_modules, lowered_package_file) =
            crate::pass::label_elab::elaborate_package(parsed_modules, parsed_package_file)
                .expect("elaborate");
        let lowered_module = lowered_modules.pop().expect("one module").1;

        // Lowered → Prime via empty elaborations.
        let elabs = crate::pass::typecheck_full::Elaborations::new();
        let normalized = crate::pass::alpha_normalize::normalize_module(&lowered_module);
        let prime_module = crate::pass::substitute::substitute_module(normalized.module(), &elabs);
        let prime_package_file = lowered_package_file
            .map(|e| crate::pass::substitute::substitute_package_file(&e, &elabs));

        // Build a Prime package, recover to Enriched, then lower to Routed.
        let scope = crate::pass::resolve::TopLevelScope::build(&prime_module).expect("scope");
        let module_path = prime_module
            .path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let modules: std::collections::BTreeMap<String, ModuleEntry<Prime>> = std::iter::once((
            module_path,
            ModuleEntry::<Prime> {
                file_path: PathBuf::from("test.kio"),
                module: prime_module,
                scope,
            },
        ))
        .collect();
        let package_file_entry = prime_package_file.map(|e| PackageFileEntry::<Prime> {
            file_path: PathBuf::from("test.pkg.kio"),
            package_name: "test".to_owned(),
            package_file: e,
        });
        let prime_pkg = Package::<Prime>::from_parts(modules, package_file_entry);

        let enriched = crate::pass::structural_recovery::recover_package(&prime_pkg);
        crate::pass::recover_to_low::lower(&enriched)
    }

    fn lower(src: &str) -> String {
        let pkg = prime_package_routed(src, None);
        let (_, entry) = pkg.modules().next().expect("one module");
        lower_module(&entry.module, None, None, None).expect("lower")
    }

    fn typed_package_routed_with_package(
        module_src: &str,
        package_file_src: Option<&str>,
    ) -> crate::pass::resolve::Package<Routed> {
        typed_package_routed_many(&[module_src], package_file_src)
    }

    fn typed_package_routed_many(
        module_srcs: &[&str],
        package_file_src: Option<&str>,
    ) -> crate::pass::resolve::Package<Routed> {
        use crate::pass::full::FullPipeline;
        use crate::pass::resolve::{Package, PackageFileEntry};
        use crate::pass::structural_recovery;
        use crate::pass::typecheck_full::check_package;
        use crate::pipeline::Pipeline;

        let parsed_modules = module_srcs
            .iter()
            .map(|module_src| {
                let parsed = parse(module_src).expect("parse");
                (module_file_path(&parsed), parsed)
            })
            .collect();
        let parsed_package_file = package_file_src.map(|src| {
            crate::pass::parser::parse_package_file(&format!("package pkg;\n{src}"), None)
                .expect("parse package file")
        });
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed_modules, parsed_package_file)
                .expect("lower_package");
        let package_file_entry = lowered_package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), lowered_modules, package_file_entry)
            .expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        let prime = check_package(&package).expect("typecheck");
        let enriched = structural_recovery::recover_package(&prime);
        crate::pass::recover_to_low::lower(&enriched)
    }

    fn typed_package_routed(module_src: &str) -> crate::pass::resolve::Package<Routed> {
        typed_package_routed_with_package(module_src, None)
    }

    fn lower_typed(src: &str) -> String {
        let pkg = typed_package_routed(src);
        let (_, entry) = pkg.modules().next().expect("one module");
        lower_module(&entry.module, None, None, None).expect("lower")
    }

    fn lower_typed_module(module_srcs: &[&str], module_path: &str) -> String {
        let pkg = typed_package_routed_many(module_srcs, None);
        let entry = pkg.module(module_path).expect("selected module");
        lower_module_with_key(&entry.module, module_path, None, None, None).expect("lower")
    }

    #[test]
    fn nested_specialization_keeps_prepared_occurrence_identity() {
        let package = typed_package_routed_with_package(
            "module main; \
             newtype N : . { constructor make; projector get; }; \
             fn id[A](value: A) -> A { value } \
             fn generic() -> [A] A -> A { id } \
             fn consume(function: . -> (N -> N)) -> . -> (N -> N) { function } \
             fn nested() -> . -> (N -> N) { consume(.() { generic()(N) }) }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&package, "pkg")
            .expect("rendering borrows the exact occurrences prepared for specialization");
        assert_eq!(js.matches(").advance(), false)").count(), 1, "{js}");
    }

    #[cfg(all(feature = "cli", feature = "surface"))]
    #[test]
    fn disabled_emit_cache_skips_context_render_serialization_and_key_work() {
        let package = prime_package_routed("module main; fn unit() -> . { () }", None);
        let uncached = lower_package_to_factory_module(&package, "pkg").expect("uncached emit");
        let disabled = crate::cache::emit::EmitCache::disabled();

        emit_cache_work_counters::reset();
        let disabled_output =
            lower_package_to_factory_module_cached(&package, "pkg", Some(&disabled))
                .expect("disabled-cache emit");
        assert_eq!(disabled_output, uncached);
        assert_eq!(
            emit_cache_work_counters::snapshot(),
            emit_cache_work_counters::Counts::default()
        );

        let (module_key, entry) = package.modules().next().expect("one module");
        let uncached_module =
            lower_module_with_key_prepared(&entry.module, module_key, None, None, None, None, None)
                .expect("uncached module emit");
        emit_cache_work_counters::reset();
        let disabled_module = lower_module_with_key_cached(
            &entry.module,
            module_key,
            None,
            None,
            None,
            (None, None),
            (Some(&disabled), Some("context")),
        )
        .expect("disabled-cache module emit");
        assert_eq!(disabled_module, uncached_module);
        assert_eq!(
            emit_cache_work_counters::snapshot(),
            emit_cache_work_counters::Counts::default()
        );

        let cache_root = tempfile::tempdir().expect("temporary emit cache");
        let active = crate::cache::emit::EmitCache::open(cache_root.path().join("cache"))
            .expect("open emit cache");
        emit_cache_work_counters::reset();
        let active_module = lower_module_with_key_cached(
            &entry.module,
            module_key,
            None,
            None,
            None,
            (None, None),
            (Some(&active), Some("context")),
        )
        .expect("active-cache module emit");
        assert_eq!(active_module, uncached_module);
        let active_counts = emit_cache_work_counters::snapshot();
        assert_eq!(active_counts.module_serializations, 1);
        assert_eq!(active_counts.keys, 1);

        emit_cache_work_counters::reset();
        let active_output = lower_package_to_factory_module_cached(&package, "pkg", Some(&active))
            .expect("active-cache package emit");
        assert_eq!(active_output, uncached);
        assert_eq!(emit_cache_work_counters::snapshot().context_renders, 1);
    }

    #[test]
    fn empty_module_emits_no_decls() {
        // The factory's "Generated by kio" header lives on the
        // `lower_package_to_factory_module` wrapper; `lower_module`
        // alone emits the inlined IIFE only. An empty module
        // produces an IIFE that returns an empty record — no
        // function declarations.
        let js = lower("module x;");
        assert!(js.contains("(() => {"), "got: {js}");
        assert!(!js.contains("function"), "got: {js}");
    }

    #[test]
    fn unit_returning_fn_def() {
        let js = lower("module x; fn empty() -> . { () }");
        assert!(js.contains("const empty = __kioLift(__kioUnit => null)"));
    }

    #[test]
    fn fn_def_with_value_param() {
        let js = lower(
            "module main; \
             host type T role(str); \
             fn first(x: T) -> T { x }",
        );
        assert!(
            js.contains("const first = __kioLift((x) => x)"),
            "got: {js}"
        );
    }

    #[test]
    fn polymorphic_fn_def_keeps_an_erased_type_stage() {
        let js = lower("module x; fn id[A](x: A) -> A { x }");
        assert!(
            js.contains("const id = __kioPoly(() => __kioLift((x) => x), null)"),
            "got: {js}"
        );
        assert_eq!(js.matches("__kioPoly(").count(), 1, "got: {js}");
        assert!(!js.contains("<A>"));
        assert!(!js.contains("(A,"));
    }

    #[test]
    fn call_consumes_the_erased_type_stage() {
        let js = lower(
            "module x; \
             fn id[A](x: A) -> A { x } \
             fn caller[A](y: A) -> A { id(A, y) }",
        );
        assert!(js.contains("const caller = __kioPoly("), "got: {js}");
        assert!(js.contains("((id).advance())(y)"), "got: {js}");
    }

    #[test]
    fn higher_kinded_applied_values_keep_forall_stages_and_erase_the_brand() {
        let js = lower_typed(
            "module main; \
             fn id_app[*F][A](x: F(A)) -> F(A) { x } \
             fn first_app[*F][A][B](x: F(A), y: F(B)) -> F(A) { x }",
        );
        assert!(
            js.contains("const id_app = __kioPoly(() => __kioPoly(() => __kioLift((x) => x)"),
            "got: {js}"
        );
        assert!(
            js.contains("const first_app = __kioPoly(() => __kioPoly(() => __kioPoly(")
                && js.contains("const [x, y] = __kioProductSlots(__kioDomain, 2); return x;"),
            "got: {js}"
        );
    }

    #[test]
    fn host_fn_value_ref_keeps_type_and_value_stages() {
        let js = lower_typed(
            "module x; \
             host fn host_id[A](value: A) -> A; \
             fn expose() -> [A] A -> A { host_id }",
        );
        assert!(
            js.contains("__kioPoly(() => __kioLift((__kioHostArg0) => __host__.x.hostId(__kioHostArg0)), null)"),
            "got: {js}"
        );
    }

    #[test]
    fn monomorphic_call_emits_all_args() {
        let js = lower(
            "module main; \
             host type T role(str); \
             fn helper(x: T) -> T { x } \
             fn caller(y: T) -> T { helper(y) }",
        );
        assert!(
            js.contains("const caller = __kioLift((y) => (helper)(y))"),
            "got: {js}"
        );
    }

    #[test]
    fn source_empty_and_written_unit_calls_share_zero_slot_abi_erasure() {
        let js = lower_typed(
            "module main; \
             host type T role(str); \
             host fn observe() -> .; \
             fn staged[A]()(x: A) -> A { x } \
             fn staged_type_only() -> (. -> T -> T) { staged(T) } \
             fn staged_implicit() -> (T -> T) { staged() } \
             fn staged_explicit() -> (T -> T) { staged(T, ()) } \
             fn staged_effectful() -> (T -> T) { staged(T, observe()) } \
             fn identity[A](value: A) -> A { value } \
             fn identity_non_unit_residual() -> (T -> T) { identity(T) } \
             fn identity_selected() -> (. -> .) { identity(.) } \
             fn identity_saturated() -> . { identity(., ()) }",
        );

        let function_body = |name: &str, next: &str| {
            js.split(&format!("const {name} = "))
                .nth(1)
                .unwrap_or_else(|| panic!("missing `{name}` in: {js}"))
                .split(&format!("const {next} = "))
                .next()
                .expect("the function section has a terminator")
        };
        let staged_type_only = function_body("staged_type_only", "staged_implicit");
        let staged_implicit = function_body("staged_implicit", "staged_explicit");
        let staged_explicit = function_body("staged_explicit", "staged_effectful");
        assert!(
            staged_type_only
                .contains("const _kg0 = (staged).advance(); return __kioLift(__kioUnit =>")
                && staged_type_only.contains("return (_kg0)(null);"),
            "a written type-only packet must retain the selected Unit layer: {js}"
        );
        assert_eq!(staged_type_only.matches("(staged).advance()").count(), 1);
        assert_eq!(staged_type_only.matches("(_kg0)(").count(), 1);
        assert!(
            staged_implicit.contains("__kioLift(__kioUnit => ((staged).advance())(null))"),
            "a source-empty call must supply its Unit value: {js}"
        );
        assert!(
            staged_explicit.contains("__kioLift(__kioUnit => ((staged).advance())(null))"),
            "an explicit Unit value must saturate that same layer: {js}"
        );

        for stage in [staged_implicit, staged_explicit] {
            assert_eq!(
                stage.matches("__kioLift(").count(),
                1,
                "a saturated call must not return another value layer: {stage}"
            );
        }

        let identity_non_unit_residual =
            function_body("identity_non_unit_residual", "identity_selected");
        let identity_selected = function_body("identity_selected", "identity_saturated");
        let identity_saturated = js
            .split("const identity_saturated = ")
            .nth(1)
            .expect("identity_saturated wrapper is emitted");
        assert!(
            identity_non_unit_residual.contains("const _kg0 = (identity).advance();")
                && identity_non_unit_residual.contains("=> (_kg0)(_kg1)"),
            "a selected non-Unit identity domain must remain residual: {js}"
        );
        assert!(
            identity_selected.contains("const _kg0 = (identity).advance();")
                && identity_selected.contains("const _kg1 = null; return (_kg0)(_kg1)"),
            "the type-selected Unit identity call must remain residual: {js}"
        );
        assert!(
            identity_saturated.contains("__kioLift(__kioUnit => ((identity).advance())(null))"),
            "the explicit Unit identity call must be saturated: {js}"
        );

        assert_eq!(
            identity_saturated.matches("__kioLift(").count(),
            1,
            "a saturated identity call must not return another value layer: {identity_saturated}"
        );

        let effectful = js
            .split("const staged_effectful = ")
            .nth(1)
            .expect("effectful wrapper is emitted");
        let observe = effectful
            .find("observe()")
            .expect("explicit Unit operand is evaluated");
        let apply = effectful
            .find("(staged).advance()")
            .expect("the empty value group is applied");
        assert!(
            observe < apply,
            "Unit operand must be sequenced first: {js}"
        );
        assert_eq!(
            effectful.matches("observe()").count(),
            1,
            "the explicit Unit operand must be evaluated exactly once: {js}"
        );
    }

    #[test]
    fn let_binding_iife() {
        let js = lower("module x; fn f(x: .) -> . { let y = x; y }");
        assert!(js.contains("const y = x"), "got: {js}");
        assert!(js.contains("return y"), "got: {js}");
    }

    #[test]
    fn fn_expression_arrow() {
        let js = lower(
            "module main; \
             host type T role(str); \
             fn make() -> (T -> T) { .(x: T) -> T { x } }",
        );
        assert!(js.contains("((x) => x)"), "got: {js}");
    }

    #[test]
    fn fn_expression_called_inline() {
        let js = lower(
            "module main; \
             host type T role(str); \
             fn use_it(y: T) -> T { (.(x: T) -> T { x })(y) }",
        );
        // The lifted inline callee stays parenthesized at the indirect
        // call site, preserving the arrow's value before applying `y`.
        assert!(js.contains("(__kioLift((x) => x))(y)"), "got: {js}");
    }

    #[test]
    fn import_intrinsics_emits_no_runtime_glue() {
        // `import __intrinsics__;` is recognized but produces no JS by
        // itself — intrinsic emission happens inline at each call site.
        // The factory wrapper carries the "Generated by kio" header;
        // the inlined module IIFE is the only thing `lower_module`
        // produces here.
        let js = lower("module x; import __intrinsics__;");
        assert!(js.contains("(() => {"), "got: {js}");
        assert!(!js.contains("function"), "got: {js}");
    }

    #[test]
    fn cross_module_import_lowers_to_namespace_member() {
        let js = lower_typed_module(
            &[
                "module pkg/helper; pub fn greet() -> . { () }",
                "module pkg/main; \
                 import pkg/helper(greet); \
                 fn say() -> . { greet() }",
            ],
            "pkg/main",
        );
        // Reference site routes through `PKG_HELPER.greet`, which
        // resolves to a closure-scoped const inside the factory body
        // (declared by pkg/helper's inlined IIFE).
        assert!(js.contains("(PKG_HELPER.greet)(null)"), "got: {js}");
    }

    #[test]
    fn module_emits_iife_namespace() {
        let js = lower("module pkg/helper; pub fn greet() -> . { () }");
        assert!(js.contains("const PKG_HELPER = (() => {"), "got: {js}");
        assert!(
            js.contains("const greet = __kioLift(__kioUnit => null)"),
            "got: {js}"
        );
        assert!(js.contains("return { greet };"), "got: {js}");
    }

    #[test]
    fn qualified_import_path_lowers_via_namespace() {
        // `import pkg/helper as h; ... h.greet()` lowers `h.greet` to
        // `PKG_HELPER.greet` at the JS level.
        let js = lower_typed_module(
            &[
                "module pkg/helper; pub fn greet() -> . { () }",
                "module pkg/main; \
                 import pkg/helper as h; \
                 fn say() -> . { h.greet() }",
            ],
            "pkg/main",
        );
        assert!(js.contains("(PKG_HELPER.greet)(null)"), "got: {js}");
    }

    // ---- Intrinsics ----------------------------------------------------

    #[test]
    fn mk_pair_emits_binary_object() {
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn pair_of(x: .) -> (. & .) { __pair__(., ., x, x) }",
        );
        assert!(js.contains("({ _0: x, _1: x })"), "got: {js}");
    }

    #[test]
    fn fst_emits_index_zero() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn first(p: (T & T)) -> T { __fst__(T, T, p) }",
        );
        assert!(js.contains("const first = __kioLift((p) =>"), "got: {js}");
        assert!(js.contains("__kioProductSlot((p), 0, 2)"), "got: {js}");
    }

    #[test]
    fn snd_emits_index_one() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn second(p: (T & T)) -> T { __snd__(T, T, p) }",
        );
        assert!(js.contains("const second = __kioLift((p) =>"), "got: {js}");
        assert!(js.contains("__kioProductSlot((p), 1, 2)"), "got: {js}");
    }

    #[test]
    fn fst_of_mk_pair_composes() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn first_of_pair(x: T) -> T { __fst__(T, T, __pair__(T, T, x, x)) }",
        );
        assert!(
            js.contains("const first_of_pair = __kioLift((x) =>"),
            "got: {js}"
        );
        assert!(
            js.contains("__kioProductSlot((({ _0: x, _1: x })), 0, 2)"),
            "got: {js}"
        );
    }

    #[test]
    fn mk_left_emits_zero_tag() {
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn left(x: .) -> (. | .) { __left__(., ., x) }",
        );
        assert!(js.contains("__kioSumInject(x, 0, 2)"), "got: {js}");
    }

    #[test]
    fn mk_right_emits_one_tag() {
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn right(x: .) -> (. | .) { __right__(., ., x) }",
        );
        assert!(js.contains("__kioSumInject(x, 1, 2)"), "got: {js}");
    }

    #[test]
    fn either_recovers_to_inlined_dispatch() {
        // Structural recovery (which `prime_module` runs) collapses
        // the `__either__` chain into an `EnrichedMatch`; codegen
        // lowers that to one payload decode and dispatch — no
        // per-`__either__` IIFE wrapper.
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn dispatch(s: (. | .)) -> . { \
               __either__(., ., ., s, .(a) { a }, .(b) { b }) \
             }",
        );
        assert!(
            js.contains("__kioSumPayload(s, 2); switch (__sum[0])"),
            "got: {js}"
        );
        // The old `__either__` IIFE wrapper is gone.
        assert!(
            !js.contains("__s, __fl, __fr"),
            "IIFE wrapper survived: {js}"
        );
    }

    #[test]
    fn absurd_emits_throwing_iife() {
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn dead[A](x: !) -> A { __absurd__(A, x) }",
        );
        assert!(
            js.contains("((__bottom__) => { throw new Error("),
            "got: {js}"
        );
        // Argument flows through.
        assert!(js.contains("})(x)"), "got: {js}");
    }

    #[test]
    fn prepared_protection_binds_compound_and_staged_sources_once() {
        for returned in [
            "T & T",
            "T | T | T",
            "T & (T | T)",
            "[A] [B] .",
            ". -> [A] .",
        ] {
            let source =
                format!("module main; host type T role(str); host fn receive() -> {returned};");
            let package = typed_package_routed_with_package(&source, Some("bridge { main; }"));
            let prepared = prepare_js_boundary(&package);
            let site = prepared
                .sites()
                .find(|site| {
                    matches!(site.site().owner(),
                BoundaryFacadeSiteOwner::HostFunction { name } if name == "receive")
                })
                .expect("prepared host site");
            for expression in ["__source.value", "__source[\"value\"]", "__effect__()"] {
                let rendered = protect_js_boundary(site, expression.to_owned(), true);
                assert_eq!(rendered.matches(expression).count(), 1, "{rendered}");
                assert!(
                    !rendered.contains(".advance()"),
                    "transport must not enter a type stage: {rendered}"
                );
                assert!(rendered.ends_with(", true)"), "{rendered}");
            }
        }
    }

    #[test]
    fn nominal_declarations_preserve_existential_stages_and_storage() {
        let js = lower(
            "module x; pub newtype Box[A] <B> : B { pub constructor pack; pub projector open; };",
        );
        assert!(
            js.contains("pack: __kioPoly(() => (__kioPoly(() => (__kioLift"),
            "{js}"
        );
        assert!(js.contains("__kioOpaqueOut("), "{js}");
        assert!(js.contains("__kioOpaqueIn("), "{js}");
        assert!(js.contains("return __kioPoly(() => __kioLift(__kioContinuation => ((__kioContinuation).advance())(__kioPayload)), null)"), "{js}");
        assert!(!js.contains("...__cps_args__"), "{js}");
    }

    #[test]
    fn ffi_conversion_observes_ordered_and_nested_binder_scope() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             host type Str role(str); \
             pub newtype T : Str { pub constructor make_t; pub projector un_t; }; \
             host fn apply_poly(f: [T] T -> T) -> Str; \
             host fn opaque_return[A](value: A) -> A; \
             pub fn poly_echo[T](x: T) -> T { x } \
             pub fn run() -> Str { apply_poly(poly_echo) } \
             fn return_t(value: T) -> T { opaque_return(T, value) } \
             pub fn ordered(value: T)[T](later: T) -> T { later }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "greeter").expect("lower");

        let plans = emitted_boundary(&js);
        let ordered = &emitted_site(&plans, "main", "ordered")["callable"]["values"];
        let uses = ordered["uses"].as_array().unwrap();
        let root = &uses[ordered["root"].as_u64().unwrap() as usize];
        assert_eq!(root["kind"], "function");
        assert_eq!(
            uses[root["domain"].as_u64().unwrap() as usize]["name"]["name"],
            "T"
        );
        let quantified = &uses[root["result"].as_u64().unwrap() as usize];
        assert_eq!(quantified["kind"], "forall");
        let later = &uses[quantified["result"].as_u64().unwrap() as usize];
        let later_domain = &uses[later["domain"].as_u64().unwrap() as usize];
        assert_eq!(later_domain["kind"], "bound");
        assert_eq!(later_domain["binder"], quantified["binder"]);
        assert!(
            js.contains("__host__.main.applyPoly(__kioHostArg0)"),
            "{js}"
        );
        assert!(js.contains("true))(poly_echo)"), "{js}");
        assert!(
            js.contains("MAIN.ordered, false))(__kioExportArg0)).advance())(__kioExportArg1)"),
            "{js}"
        );
        assert!(
            js.contains("MAIN.poly_echo, false)).advance())(__kioExportArg0)"),
            "{js}"
        );
        assert!(
            js.contains("__host__.main.opaqueReturn(__kioHostArg0)"),
            "{js}"
        );
    }

    #[test]
    fn prepared_product_source_layout_preserves_one_canonical_argument() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             import __intrinsics__; \
             host type I32 role(i32); \
             pub fn first(pair: I32 & I32) -> I32 { __fst__(I32, I32, pair) }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0(__kioExportArg0)")
                && js.contains("MAIN.first, false))(__kioExportArg0)"),
            "{js}"
        );
        assert!(js.contains("__kioProductSlot((pair), 0, 2)"), "{js}");
        let plans = emitted_boundary(&js);
        let site = emitted_site(&plans, "main", "first");
        let layout = &site["execution"]["head"][0]["layout"];
        assert_eq!(layout["source_param_count"], 1);
        assert_eq!(layout["source_params"][0]["adapter"], "right_nest");
        assert_eq!(layout["source_params"][0]["end"], 2);
    }

    #[test]
    fn prepared_returned_function_preserves_product_source_parameter() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             host type I32 role(i32); \
             pub fn make() -> (& I32 & I32) -> I32 { \
               .(, left: I32, right: I32) { left } \
             }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(js.contains("MAIN.make, false))(null)"), "{js}");
        assert!(
            js.contains("const [left, right] = __kioProductSlots(__kioDomain, 2)"),
            "{js}"
        );
        let plans = emitted_boundary(&js);
        let site = emitted_site(&plans, "main", "make");
        let values = &site["callable"]["values"];
        let uses = values["uses"].as_array().unwrap();
        let returned = uses[values["root"].as_u64().unwrap() as usize]["result"]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(uses[returned]["kind"], "function");
        assert_eq!(
            uses[uses[returned]["domain"].as_u64().unwrap() as usize]["kind"],
            "product"
        );
        assert_eq!(site["callable"]["head"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn prepared_nullary_unit_source_layout_has_no_public_argument() {
        let pkg = typed_package_routed_with_package(
            "module main; pub fn make_unit() -> . { () }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0()") && js.contains("MAIN.make_unit, false))(null)"),
            "{js}"
        );
        assert!(
            !js.contains("function __export_fn_0(__kioExportArg"),
            "{js}"
        );
    }

    #[test]
    fn prepared_direct_unit_source_layout_has_no_public_argument() {
        let pkg = typed_package_routed_with_package(
            "module main; pub fn keep_unit(value: .) -> . { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0()") && js.contains("MAIN.keep_unit, false))(null)"),
            "{js}"
        );
        assert!(
            !js.contains("function __export_fn_0(__kioExportArg"),
            "{js}"
        );
    }

    #[test]
    fn prepared_generic_function_keeps_its_slot_for_later_unit_selection() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             fn identity[A](value: A) -> A { value } \
             pub fn selected[A]() -> (A -> A) { identity(A) }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("__kioPoly(() => __kioLift((value) => value), null)"),
            "{js}"
        );
        assert!(
            js.contains("(identity).advance(), false)")
                && !js.contains("(identity).advance())(null)"),
            "{js}"
        );
        assert!(
            js.contains("MAIN.selected, false)).advance())(null)"),
            "{js}"
        );
    }

    #[test]
    fn prepared_bottom_inside_a_product_is_not_comptime_erased() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             host type I32 role(i32); \
             pub fn impossible(value: I32 & !) -> I32 & ! { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0(__kioExportArg0)")
                && js.contains("MAIN.impossible, false))(__kioExportArg0)"),
            "{js}"
        );
        let plans = emitted_boundary(&js);
        let values = &emitted_site(&plans, "main", "impossible")["callable"]["values"];
        let uses = values["uses"].as_array().unwrap();
        let root = &uses[values["root"].as_u64().unwrap() as usize];
        for slot in ["domain", "result"] {
            let product = &uses[root[slot].as_u64().unwrap() as usize];
            assert_eq!(product["kind"], "product");
            assert_eq!(
                uses[product["right"].as_u64().unwrap() as usize]["kind"],
                "bottom"
            );
        }
    }

    #[test]
    fn prepared_curried_value_stages_preserve_source_groups() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             import __intrinsics__; \
             host type I32 role(i32); \
             pub fn choose(pair: I32 & I32)(fallback: I32) -> I32 { \
               __fst__(I32, I32, pair) \
             }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0(__kioExportArg0, __kioExportArg1)")
                && js.contains("MAIN.choose, false))(__kioExportArg0))(__kioExportArg1)"),
            "{js}"
        );
        let plans = emitted_boundary(&js);
        let head = emitted_site(&plans, "main", "choose")["execution"]["head"]
            .as_array()
            .unwrap();
        assert_eq!(head.len(), 2);
        assert_eq!(head[0]["layout"]["source_param_count"], 1);
        assert_eq!(head[0]["layout"]["facade_slot_count"], 2);
        assert_eq!(head[1]["layout"]["source_param_count"], 1);
        assert_eq!(head[1]["layout"]["facade_slot_count"], 1);
    }

    #[test]
    fn public_newtype_members_use_boundary_shapes_and_hide_private_members() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             host type I32 role(i32); \
             pub newtype Opaque : I32 { constructor make_opaque; projector read_opaque; }; \
             pub newtype Constructor_only : I32 { \
               pub constructor make_constructor_only; projector read_constructor_only; \
             }; \
             pub newtype Projector_only : I32 { \
               constructor make_projector_only; pub projector read_projector_only; \
             }; \
             pub newtype Both_public : I32 { \
               pub constructor make_both_public; pub projector read_both_public; \
             }; \
             pub fn make_opaque_value(value: I32) -> Opaque { Opaque.make_opaque(value) } \
             pub fn read_opaque_value(value: Opaque) -> I32 { Opaque.read_opaque(value) }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        let export_record = js
            .lines()
            .find(|line| line.trim_start().starts_with("const __export_record__ = "))
            .expect("export record");
        let compact = export_record.replace(' ', "");

        assert!(compact.contains("KioType_Opaque:{}"), "{export_record}");
        assert!(
            compact.contains("KioType_ConstructorOnly:{makeConstructorOnly:__export_fn_"),
            "{export_record}"
        );
        assert!(
            compact.contains("KioType_ProjectorOnly:{readProjectorOnly:__export_fn_"),
            "{export_record}"
        );
        assert!(
            compact.contains("KioType_BothPublic:{makeBothPublic:__export_fn_")
                && compact.contains(",readBothPublic:__export_fn_"),
            "{export_record}"
        );
        assert!(
            !compact.contains("KioType_Opaque:{make_opaque"),
            "{export_record}"
        );
        assert!(
            !compact.contains("KioType_Opaque:{read_opaque"),
            "{export_record}"
        );
        let plans = emitted_boundary(&js);
        for name in ["Opaque", "Constructor_only", "Projector_only"] {
            assert!(
                emitted_nominal(&plans, "main", name)["declaration"]["transparent_payload"]
                    .is_null(),
                "{name}"
            );
        }
        assert!(
            !emitted_nominal(&plans, "main", "Both_public")["declaration"]["transparent_payload"]
                .is_null()
        );
        assert!(
            js.contains("__kioBodyContext.nominals.get(\"V1_M1_C4_mainN6_Opaque\").wrap")
                && js.contains("__kioBodyContext.nominals.get(\"V1_M1_C4_mainN6_Opaque\").unwrap"),
            "{js}"
        );
        assert!(
            js.contains("MAIN.Constructor_only.make_constructor_only, false)")
                && js.contains("MAIN.Projector_only.read_projector_only, false)"),
            "{js}"
        );
        assert!(
            js.contains("MAIN.Both_public.make_both_public, false)")
                && js.contains("MAIN.Both_public.read_both_public, false)"),
            "{js}"
        );
    }

    #[test]
    fn public_existential_projector_preserves_result_and_continuation_stages() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Hidden <U> <V> : U & V { \
               constructor make_hidden; pub projector reveal_hidden; \
             };",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        let wrapper = js
            .lines()
            .find(|line| line.contains("function __export_fn_0"))
            .unwrap();
        assert!(
            wrapper.contains("MAIN.Hidden.reveal_hidden, false))(__kioExportArg0)"),
            "{wrapper}"
        );
        assert!(
            !wrapper.contains(".advance()"),
            "the named head must not enter its returned result stage: {wrapper}"
        );
        assert!(js.contains("return __kioPoly(() => __kioLift(__kioContinuation => ((__kioContinuation).advance().advance())(__kioPayload)), null)"), "{js}");
        let plans = emitted_boundary(&js);
        let nominal = emitted_nominal(&plans, "main", "Hidden");
        assert_eq!(
            nominal["declaration"]["existential_params"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(nominal["declaration"]["transparent_payload"].is_null());
    }

    #[test]
    fn public_newtype_member_wrappers_cover_general_member_schemes_and_exact_owners() {
        let pkg = typed_package_routed_many(
            &[
                "module left; \
                 host type I32 role(i32); \
                 pub newtype Shared : I32 { \
                   pub constructor make_shared; pub projector read_shared; \
                 };",
                "module right; \
                 host type I32 role(i32); \
                 pub newtype Shared : I32 { \
                   pub constructor make_shared; pub projector read_shared; \
                 };",
                "module shapes; \
                 host type I32 role(i32); \
                 pub newtype Nested : I32 & (I32 | I32) { \
                   pub constructor make_nested; pub projector read_nested; \
                 }; \
                 pub newtype Box[A] : A { \
                   pub constructor make_box; pub projector read_box; \
                 }; \
                 pub newtype Packed <A> : A { \
                   pub constructor make_packed; pub projector read_packed; \
                 }; \
                 pub labels { field: I32 }; \
                 pub rec newtype Recursive : . | (I32 & Recursive) { \
                   pub constructor make_recursive; pub projector read_recursive; \
                 };",
            ],
            Some("bridge { left; right; shapes; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        let plans = emitted_boundary(&js);
        let left = emitted_nominal(&plans, "left", "Shared");
        let right = emitted_nominal(&plans, "right", "Shared");
        assert_ne!(left["name"]["frame"], right["name"]["frame"]);
        for (module, name) in [
            ("left", "Shared"),
            ("right", "Shared"),
            ("shapes", "Nested"),
            ("shapes", "Box"),
            ("shapes", "Field"),
        ] {
            assert!(
                !emitted_nominal(&plans, module, name)["declaration"]["transparent_payload"]
                    .is_null(),
                "{module}.{name}"
            );
        }
        let packed = &emitted_nominal(&plans, "shapes", "Packed")["declaration"];
        assert_eq!(packed["existential_params"].as_array().unwrap().len(), 1);
        assert!(
            !packed["transparent_payload"].is_null(),
            "CPS typing retains its exact payload"
        );
        assert_eq!(
            emitted_nominal_view(&js, "shapes", "Packed")["sealed"],
            true
        );
        assert_recursive_view(&pkg, "shapes", "Recursive", true);
        for member in [
            "LEFT.Shared.make_shared",
            "RIGHT.Shared.make_shared",
            "SHAPES.Nested.make_nested",
            "SHAPES.Box.make_box",
            "SHAPES.Packed.make_packed",
            "SHAPES.Field.mk",
            "SHAPES.Recursive.make_recursive",
            "SHAPES.Recursive.read_recursive",
        ] {
            assert!(js.contains(&format!("{member}, false)")), "{member}");
        }
    }

    #[test]
    fn prepared_generic_newtype_payload_uses_exact_application_arguments() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             host type I32 role(i32); \
             pub newtype Box[A] : A { \
               pub constructor make_box; pub projector read_box; \
             }; \
             pub fn round(value: Box(I32 & I32)) -> Box(I32 & I32) { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        let plans = emitted_boundary(&js);
        let payload = &emitted_nominal(&plans, "main", "Box")["declaration"]["transparent_payload"];
        assert_eq!(payload["declaration_binders"].as_array().unwrap().len(), 1);
        let payload_values = &payload["values"];
        let payload_root = payload_values["source_uses"]
            [payload["payload_root"].as_u64().unwrap() as usize]
            .as_u64()
            .unwrap() as usize;
        assert_eq!(payload_values["uses"][payload_root]["kind"], "bound");
        assert_eq!(
            payload_values["uses"][payload_root]["binder"],
            payload["declaration_binders"][0]
        );
        let values = &emitted_site(&plans, "main", "round")["callable"]["values"];
        let uses = values["uses"].as_array().unwrap();
        let domain = uses[values["root"].as_u64().unwrap() as usize]["domain"]
            .as_u64()
            .unwrap() as usize;
        let application = &uses[domain]["application"];
        assert_eq!(application["head"]["name"]["name"], "Box");
        let argument = application["args"][0].as_u64().unwrap() as usize;
        assert_eq!(uses[argument]["kind"], "product");
        assert_eq!(
            uses[uses[argument]["left"].as_u64().unwrap() as usize]["name"]["name"],
            "I32"
        );
        assert_eq!(
            uses[uses[argument]["right"].as_u64().unwrap() as usize]["name"]["name"],
            "I32"
        );
        assert!(js.contains("MAIN.round, false))(__kioExportArg0)"), "{js}");
    }

    #[test]
    fn substituted_nominals_keep_wrappers_in_original_abstract_slots() {
        for payload in ["A & .", "A | .", "F(.) & .", "F(.) | ."] {
            let (parameters, argument) = if payload.starts_with("F") {
                ("*F", "Box")
            } else {
                ("A", "Box(.)")
            };
            let pkg = typed_package_routed_with_package(
                &format!(
                    "module main; \
                     pub newtype Box[A] : A {{ \
                       pub constructor make_box; pub projector read_box; \
                     }}; \
                     pub newtype Container[{parameters}] : {payload} {{ \
                       pub constructor make_container; pub projector read_container; \
                     }}; \
                     pub fn round(value: Container({argument})) -> Container({argument}) {{ value }}"
                ),
                Some("bridge { main; }"),
            );
            let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
            let plans = emitted_boundary(&js);
            let nominal = emitted_nominal(&plans, "main", "Container");
            let payload_plan = &nominal["declaration"]["transparent_payload"];
            let values = &payload_plan["values"];
            let root = values["source_uses"]
                [payload_plan["payload_root"].as_u64().unwrap() as usize]
                .as_u64()
                .unwrap() as usize;
            let left = values["uses"][root]["left"].as_u64().unwrap() as usize;
            assert_eq!(
                values["uses"][root]["kind"],
                if payload.contains('&') {
                    "product"
                } else {
                    "sum"
                }
            );
            if parameters == "*F" {
                assert_eq!(values["uses"][left]["application"]["head"]["kind"], "bound");
            } else {
                assert_eq!(values["uses"][left]["kind"], "bound");
            }
            let round = &emitted_site(&plans, "main", "round")["callable"]["values"];
            let domain = round["uses"][round["root"].as_u64().unwrap() as usize]["domain"]
                .as_u64()
                .unwrap() as usize;
            let argument = round["uses"][domain]["application"]["args"][0]
                .as_u64()
                .unwrap() as usize;
            assert_eq!(
                round["uses"][argument]["application"]["head"]["name"]["name"],
                "Box"
            );
            assert!(
                !emitted_nominal(&plans, "main", "Box")["declaration"]["transparent_payload"]
                    .is_null()
            );
            assert!(js.contains("MAIN.round, false))(__kioExportArg0)"), "{js}");
        }
    }

    #[test]
    fn unbridged_host_site_does_not_enter_the_public_prepared_catalog() {
        let pkg = typed_package_routed_many(
            &[
                "module main; pub fn run() -> . { () }",
                "module unused; host fn ping() -> .; fn call() -> . { ping() }",
            ],
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(js.contains("__host__.unused.ping()"), "{js}");
        assert!(!js.contains("missing host item: unused.ping"), "{js}");
        assert!(
            js.contains("run: __export_fn_") && js.contains("MAIN.run, false))(null)"),
            "{js}"
        );
    }

    #[test]
    fn prepared_catalog_is_the_sole_exported_function_inventory() {
        let catalog_package = typed_package_routed_with_package(
            "module main; pub fn keep() -> . { () }",
            Some("bridge { main; }"),
        );
        let prepared = prepare_js_boundary(&catalog_package);
        let raw_package = typed_package_routed_with_package(
            "module main; \
             pub fn keep() -> . { () } \
             pub fn raw_only() -> . { () }",
            Some("bridge { main; }"),
        );
        let package_file = &raw_package
            .package_file()
            .expect("package file")
            .package_file;

        let js = lower_package_file_prepared(package_file, "pkg", &raw_package, &prepared)
            .expect("lower prepared export catalog")
            .expect("bridged export record");

        assert!(
            js.contains("keep: __export_fn_") && js.contains("MAIN.keep, false))(null)"),
            "{js}"
        );
        assert!(!js.contains("raw_only"), "{js}");
    }

    #[test]
    fn prepared_catalog_is_the_sole_public_newtype_inventory() {
        let catalog_package = typed_package_routed_with_package(
            "module main; \
             pub newtype Kept : . { \
               pub constructor make_kept; pub projector read_kept; \
             };",
            Some("bridge { main; }"),
        );
        let prepared = prepare_js_boundary(&catalog_package);
        let raw_package = typed_package_routed_with_package(
            "module main; \
             pub newtype Kept : . { \
               pub constructor make_kept; pub projector read_kept; \
             }; \
             pub newtype Raw_only : . { \
               pub constructor make_raw_only; pub projector read_raw_only; \
             };",
            Some("bridge { main; }"),
        );
        let package_file = &raw_package
            .package_file()
            .expect("package file")
            .package_file;

        let js = lower_package_file_prepared(package_file, "pkg", &raw_package, &prepared)
            .expect("lower prepared newtype catalog")
            .expect("bridged export record");

        assert!(js.contains("Kept:"), "{js}");
        assert!(!js.contains("Raw_only"), "{js}");
    }

    #[test]
    fn opaque_newtype_conversion_retains_exact_private_stores() {
        let pkg = typed_package_routed_many(
            &[
                "module left; \
                 newtype Secret : . { constructor make_secret; projector read_secret; }; \
                 pub newtype Token : Secret { constructor make_token; projector read_token; }; \
                 pub fn echo_token(value: Token) -> Token { value } \
                 pub fn echo_pair(value: Token & .) -> Token & . { value }",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 pub fn echo_token(value: Token) -> Token { value }",
            ],
            Some("bridge { left; right; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        let wrappers = js
            .split("  function __export_fn_0")
            .nth(1)
            .and_then(|tail| tail.split("  const __export_record__").next())
            .expect("export wrappers");

        assert!(
            wrappers.contains("LEFT.echo_token, false)")
                && wrappers.contains("RIGHT.echo_token, false)"),
            "{wrappers}"
        );
        assert!(!wrappers.contains("Secret"), "{wrappers}");
        let plans = emitted_boundary(&js);
        let left = emitted_nominal(&plans, "left", "Token");
        let right = emitted_nominal(&plans, "right", "Token");
        assert_ne!(left["name"]["frame"], right["name"]["frame"]);
        assert!(left["declaration"]["transparent_payload"].is_null());
        assert!(right["declaration"]["transparent_payload"].is_null());
        assert!(js.contains("const __kioOpaqueStores = new Map();"), "{js}");
        assert!(js.contains("store = new __kioOpaqueWeakMap();"), "{js}");
        assert!(
            js.contains("const handle = __kioOpaqueFreeze(__kioOpaqueCreate(null));"),
            "{js}"
        );
        assert!(js.contains("!store.has(handle)"), "{js}");
    }

    #[test]
    fn same_leaf_opaque_newtypes_stay_exact_across_host_calls() {
        let pkg = typed_package_routed_many(
            &[
                "module left; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn keep(value: Token) -> Token; \
                 pub fn call(value: Token) -> Token { keep(value) }",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn keep(value: Token) -> Token; \
                 pub fn call(value: Token) -> Token { keep(value) }",
            ],
            Some("bridge { left; right; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        let plans = emitted_boundary(&js);
        for module in ["left", "right"] {
            let site = emitted_site(&plans, module, "keep");
            let values = &site["callable"]["values"];
            let uses = values["uses"].as_array().unwrap();
            let root = &uses[values["root"].as_u64().unwrap() as usize];
            for position in ["domain", "result"] {
                let nominal = &uses[root[position].as_u64().unwrap() as usize];
                assert_eq!(nominal["name"]["module"], serde_json::json!([module]));
                assert_eq!(nominal["name"]["name"], "Token");
            }
            assert!(
                emitted_nominal(&plans, module, "Token")["declaration"]["transparent_payload"]
                    .is_null()
            );
            assert!(
                js.contains(&format!("__host__.{module}.keep(__kioHostArg0)")),
                "{js}"
            );
            let key = serde_json::to_string(&serde_json::json!([[module], ["host", "keep"]]))
                .expect("exact host site key");
            let key = serde_json::to_string(&key).expect("JavaScript string literal");
            assert!(js.contains(&format!(
                "__kioBoundaryValueRef(__kioBoundarySites, {key}, __kioBodyContext), __kioLift((__kioHostArg0) => __host__.{module}.keep(__kioHostArg0)), true)"
            )), "the exact nominal plan must guard its own host call: {js}");
        }
        assert_ne!(
            emitted_nominal(&plans, "left", "Token")["name"]["frame"],
            emitted_nominal(&plans, "right", "Token")["name"]["frame"]
        );
    }

    #[test]
    fn hidden_carrier_stops_enclosing_newtype_recursion() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             rec { \
               pub newtype Outer : Hidden { pub constructor make_outer; pub projector read_outer; }; \
               pub newtype Hidden : Outer { constructor make_hidden; projector read_hidden; }; \
             } \
             pub fn echo(value: Outer) -> Outer { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        let plans = emitted_boundary(&js);
        assert_recursive_view(&pkg, "main", "Outer", false);
        assert!(
            !emitted_nominal(&plans, "main", "Outer")["declaration"]["transparent_payload"]
                .is_null()
        );
        assert!(
            emitted_nominal(&plans, "main", "Hidden")["declaration"]["transparent_payload"]
                .is_null()
        );
        assert!(js.contains("MAIN.echo, false))(__kioExportArg0)"), "{js}");
    }

    #[test]
    fn hidden_generic_argument_does_not_make_enclosing_newtype_recursive() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Hidden[A] : . { constructor make_hidden; projector read_hidden; }; \
             pub rec newtype Root : Hidden(Root) { \
               pub constructor make_root; pub projector read_root; \
             }; \
             pub fn keep(value: Root) -> Root { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        assert_recursive_view(&pkg, "main", "Root", false);

        assert!(js.contains("keep: __export_fn_"), "{js}");
        assert!(!js.contains("keep: MAIN.keep"), "{js}");
    }

    #[test]
    fn alias_hidden_recursive_payload_uses_sealed_nominal_view() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Unit_rep : . { pub constructor mk_unit_rep; pub projector un_unit_rep; }; \
             rec { \
               pub newtype Pair_rep : Type_rep & Type_rep { \
                 pub constructor mk_pair_rep; pub projector un_pair_rep; \
               }; \
               pub type Type_rep_tail = Pair_rep; \
               pub newtype Type_rep : Unit_rep | Type_rep_tail { \
                 pub constructor mk_type_rep; pub projector un_type_rep; \
               }; \
             } \
             pub fn keep(rep: Type_rep) -> Type_rep { rep }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert_recursive_view(&pkg, "main", "Type_rep", true);
        assert!(
            js.contains("keep: __export_fn_") && js.contains("MAIN.keep, false))(__kioExportArg0)"),
            "{js}"
        );
        assert!(
            js.contains("KioType_TypeRep: { mkTypeRep: __export_fn_")
                && js.contains("unTypeRep: __export_fn_"),
            "{js}"
        );
    }

    #[test]
    fn transparent_phantom_argument_does_not_make_enclosing_newtype_recursive() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Phantom[A] : . { \
               pub constructor make_phantom; pub projector read_phantom; \
             }; \
             pub rec newtype Root : Phantom(Root) { \
               pub constructor make_root; pub projector read_root; \
             }; \
             pub fn keep(value: Root) -> Root { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        assert_recursive_view(&pkg, "main", "Root", false);

        assert!(
            js.contains("keep: __export_fn_"),
            "a transparent generic expands only the declaration binders its payload actually uses: {js}"
        );
        assert!(!js.contains("keep: MAIN.keep"), "{js}");
    }

    #[test]
    fn alias_ignored_recursive_argument_keeps_finite_export_conversion() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub type Const[A] = .; \
             pub rec newtype Token : Const(Token) { \
               pub constructor mk_token; pub projector un_token; \
             }; \
             pub fn keep(value: Token) -> Token { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        assert_recursive_view(&pkg, "main", "Token", false);

        assert!(js.contains("keep: __export_fn_"), "{js}");
        assert!(!js.contains("keep: MAIN.keep"), "{js}");
    }

    #[test]
    fn generic_newtype_sibling_descent_still_finds_recursive_instantiation() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Unit_rep : . { pub constructor mk_unit_rep; pub projector un_unit_rep; }; \
             pub newtype Id[A] : A { pub constructor mk_id; pub projector un_id; }; \
             rec { \
               pub type Tail = Id(.) & Id(Node); \
               pub newtype Node : Unit_rep | Tail { \
                 pub constructor mk_node; pub projector un_node; \
               }; \
             } \
             pub fn keep(value: Node) -> Node { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert_recursive_view(&pkg, "main", "Node", true);
        assert!(
            js.contains("keep: __export_fn_") && js.contains("MAIN.keep, false))(__kioExportArg0)"),
            "{js}"
        );
        assert!(
            js.contains("KioType_Node: { mkNode: __export_fn_")
                && js.contains("unNode: __export_fn_"),
            "{js}"
        );
    }

    #[test]
    fn transparent_used_argument_preserves_enclosing_newtype_recursion() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Relay[A] : A { \
               pub constructor make_relay; pub projector read_relay; \
             }; \
             pub rec newtype Root : Relay(Root) { \
               pub constructor make_root; pub projector read_root; \
             }; \
             pub fn keep(value: Root) -> Root { value }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert_recursive_view(&pkg, "main", "Root", true);
        assert!(
            js.contains("keep: __export_fn_") && js.contains("MAIN.keep, false))(__kioExportArg0)"),
            "{js}"
        );
    }

    #[test]
    fn recursive_newtype_identity_ignores_cross_module_same_leaf_shadow() {
        let pkg = typed_package_routed_many(
            &[
                "module list/core; \
                 pub rec newtype List[A] : . | (A & List(A)) { \
                   pub constructor make_list; pub projector read_list; \
                 };",
                "module facade; \
                 newtype List : . { constructor make_list; projector read_list; };",
            ],
            Some("bridge { facade; list/**; }"),
        );
        let prepared = prepare_js_boundary(&pkg);
        let name =
            QualifiedTypeName::new(vec!["list".to_owned(), "core".to_owned()], "List").unwrap();
        let site = prepared
            .sites()
            .find(|site| site.nominals().declaration(&name).is_some())
            .expect("exact list/core.List boundary declaration");
        assert!(prepared_js_newtype_is_recursive(site, &name));
    }

    #[test]
    fn comptime_prefixed_user_types_keep_exact_boundary_identity() {
        let pkg = typed_package_routed_many(
            &[
                "module visible; \
                 host type I32 role(i32); \
                 pub newtype Comptime_bool : I32 { \
                   pub constructor make_bool; pub projector read_bool; \
                 }; \
                 pub fn round_visible(value: Comptime_bool) -> Comptime_bool { value }",
                "module hidden; \
                 host type I32 role(i32); \
                 pub newtype Comptime_bool : I32 { \
                   constructor make_bool; projector read_bool; \
                 }; \
                 pub fn round_hidden(value: Comptime_bool) -> Comptime_bool { value }",
                "module generic; \
                 pub fn keep[Comptime_bool](value: Comptime_bool) -> Comptime_bool { value }",
                "module hostish; \
                 host type Comptime_bool role(i32); \
                 pub newtype Wrapped : Comptime_bool { \
                   pub constructor make_wrapped; pub projector read_wrapped; \
                 }; \
                 pub fn round_host(value: Comptime_bool) -> Comptime_bool { value } \
                 pub fn round_wrapped(value: Wrapped) -> Wrapped { value }",
                "module aliasing; \
                 host type I32 role(i32); \
                 pub type Comptime_bool = I32; \
                 pub fn round_alias(value: Comptime_bool) -> Comptime_bool { value }",
            ],
            Some("bridge { visible; hidden; generic; hostish; aliasing; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");
        let plans = emitted_boundary(&js);
        assert!(!emitted_nominal(&plans, "visible", "Comptime_bool")["declaration"]["transparent_payload"].is_null());
        assert!(emitted_nominal(&plans, "hidden", "Comptime_bool")["declaration"]["transparent_payload"].is_null());
        assert_ne!(
            emitted_nominal(&plans, "visible", "Comptime_bool")["name"]["frame"],
            emitted_nominal(&plans, "hidden", "Comptime_bool")["name"]["frame"]
        );
        assert_eq!(
            emitted_nominal(&plans, "hostish", "Comptime_bool")["declaration"]["kind"],
            "host_type"
        );
        let generic = &emitted_site(&plans, "generic", "keep")["callable"]["values"];
        let uses = generic["uses"].as_array().unwrap();
        assert_eq!(
            uses[generic["root"].as_u64().unwrap() as usize]["kind"],
            "forall"
        );
        assert_eq!(
            uses.iter().filter(|use_| use_["kind"] == "bound").count(),
            2
        );
        let alias = &emitted_site(&plans, "aliasing", "round_alias")["callable"]["values"];
        assert!(
            alias["uses"]
                .as_array()
                .unwrap()
                .iter()
                .any(|use_| use_["name"]["name"] == "I32")
        );
        assert!(
            js.contains("GENERIC.keep, false)).advance())(__kioExportArg0)"),
            "{js}"
        );
        for function in [
            "VISIBLE.round_visible",
            "HIDDEN.round_hidden",
            "HOSTISH.round_host",
            "HOSTISH.round_wrapped",
            "ALIASING.round_alias",
        ] {
            assert!(
                js.contains(&format!("{function}, false))(__kioExportArg0)")),
                "{function}"
            );
        }
    }

    #[test]
    fn visible_newtype_payloads_use_exact_module_identity_and_stay_open_world() {
        let zed = "module zed; \
                   host type I32 role(i32); \
                   pub newtype Nested : I32 { \
                     pub constructor make_nested; pub projector read_nested; \
                   }; \
                   pub newtype Outer : Nested { \
                     pub constructor make_outer; pub projector read_outer; \
                   }; \
                   pub fn round(value: Outer) -> Outer { value }";
        let base = typed_package_routed_many(&[zed], Some("bridge { zed; }"));
        let base_js = lower_package_to_factory_module(&base, "facade").expect("lower base");
        let extended = typed_package_routed_many(
            &[
                "module aaa; \
                 pub newtype Nested : . { constructor make_nested; projector read_nested; };",
                zed,
            ],
            Some("bridge { zed; }"),
        );
        let js = lower_package_to_factory_module(&extended, "facade").expect("lower extended");
        let unrelated_identity =
            crate::backends::serialized_runtime_ir::exact_nominal_identity_frame(
                &QualifiedTypeName::new(vec!["aaa".to_owned()], "Nested").unwrap(),
            );
        let round_adapter = |source: &str| {
            source
                .split("function __export_fn_")
                .skip(1)
                .find(|wrapper| wrapper.contains("ZED.round"))
                .and_then(|wrapper| wrapper.split("\n}").next())
                .expect("round wrapper")
                .to_owned()
        };

        let plans = emitted_boundary(&js);
        let outer = emitted_nominal(&plans, "zed", "Outer");
        let payload = &outer["declaration"]["transparent_payload"]["values"];
        let root = payload["root"].as_u64().unwrap() as usize;
        assert_eq!(
            payload["uses"][root]["name"]["module"],
            serde_json::json!(["zed"])
        );
        assert_eq!(payload["uses"][root]["name"]["name"], "Nested");
        assert_ne!(payload["uses"][root]["name"]["frame"], unrelated_identity);
        assert_eq!(
            emitted_nominal(&emitted_boundary(&base_js), "zed", "Outer"),
            outer
        );
        assert_eq!(
            round_adapter(&base_js),
            round_adapter(&js),
            "adding an unrelated same-leaf declaration must leave the existing exact adapter byte-identical"
        );
    }

    // ---- Newtype member emission ---------------------------------------

    #[test]
    fn nullary_newtype_emits_namespace_object() {
        let js =
            lower("module x; newtype Foo : . { pub constructor mk_foo; pub projector un_foo; };");
        assert!(js.contains("const Foo = {"), "got: {js}");
        assert!(js.contains("mk_foo: __kioLift(__kioPayload => (__kioOpaqueOut(\"V1_M1_C1_xN3_Foo\", __kioPayload)))"), "got: {js}");
        assert!(js.contains("un_foo: __kioLift(__kioNominal => (__kioOpaqueIn(\"V1_M1_C1_xN3_Foo\", __kioNominal)))"), "got: {js}");
    }

    #[test]
    fn parametric_newtype_preserves_stages_and_exact_storage() {
        // Erased binders retain explicit stages; all instances share their
        // declaration's private nominal store.
        let js = lower(
            "module x; newtype Box[A] : A { pub constructor mk_box; pub projector un_box; };",
        );
        assert!(js.contains("const Box = {"), "got: {js}");
        assert!(
            js.contains("mk_box: __kioPoly(() => (__kioLift"),
            "got: {js}"
        );
        assert!(
            js.contains("un_box: __kioPoly(() => (__kioLift"),
            "got: {js}"
        );
        assert!(
            js.contains("__kioOpaqueOut(\"V1_M1_C1_xN3_Box\", __kioPayload)")
                && js.contains("__kioOpaqueIn(\"V1_M1_C1_xN3_Box\", __kioNominal)"),
            "got: {js}"
        );
    }

    #[test]
    fn nullary_newtype_constructor_call() {
        let js = lower(
            "module x; \
             newtype Foo : . { pub constructor mk_foo; pub projector un_foo; }; \
             fn make() -> Foo { Foo.mk_foo(()) }",
        );
        assert!(js.contains("Foo.mk_foo(null)"), "got: {js}");
    }

    #[test]
    fn nullary_newtype_projector_call() {
        let js = lower(
            "module x; \
             newtype Foo : . { pub constructor mk_foo; pub projector un_foo; }; \
             fn open(w: Foo) -> . { Foo.un_foo(w) }",
        );
        assert!(js.contains("Foo.un_foo(w)"), "got: {js}");
    }

    #[test]
    fn parametric_newtype_member_drops_type_args() {
        // The written type argument advances once without a runtime type value.
        let js = lower_typed(
            "module x; \
             newtype Box[A] : A { pub constructor mk_box; pub projector un_box; }; \
             fn wrap[A](x: A) -> Box(A) { Box.mk_box(A, x) }",
        );
        assert!(js.contains("Box.mk_box.advance()(x)"), "got: {js}");
        assert!(!js.contains("Box.mk_box(A"), "got: {js}");
    }

    #[test]
    fn qualified_newtype_member_uses_canonical_owner_without_package_context() {
        let package = typed_package_routed_many(
            &[
                "module actual; \
                 pub newtype Box[A] : . { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
                "module selected; \
                 pub newtype Box[A] : A { \
                     pub constructor mk_box; pub projector un_box; \
                 };",
                "module consumer; \
                 import selected as actual; \
                 import actual as selected; \
                 fn wrap[A](value: A) -> actual.Box(A) { \
                     actual.Box.mk_box(A, value) \
                 }",
            ],
            None,
        );
        let consumer = package.module("consumer").expect("consumer module");
        let js = lower_module_with_key(&consumer.module, "consumer", None, None, None)
            .expect("lower consumer without ambient package context");

        assert!(
            js.contains("SELECTED.Box.mk_box.advance()(value)"),
            "got: {js}"
        );
        assert!(
            !js.contains("ACTUAL.Box.mk_box.advance()(value)"),
            "got: {js}"
        );
    }

    #[test]
    fn local_cps_projector_uses_exact_declaration_without_package_context() {
        let js = lower_typed(
            "module x; \
             newtype Box <A> : A & . { constructor mk_box; projector inspect; }; \
             fn open(v: Box) -> . { \
               Box.inspect(v)(.[A](_value: A, _unit: .) { () }) \
             }",
        );
        let unique_position = |needle: &str| {
            let positions = js
                .match_indices(needle)
                .map(|(position, _)| position)
                .collect::<Vec<_>>();
            assert_eq!(
                positions.len(),
                1,
                "expected one `{needle}` marker, got {}: {js}",
                positions.len()
            );
            positions[0]
        };

        let receiver_binding = unique_position("const __kio_cps_receiver = v;");
        let continuation_binding = unique_position("const __kio_cps_continuation =");
        let callee_binding = unique_position("const __kio_call_callee");
        let callee_name_start = callee_binding + "const ".len();
        let callee_name_end = callee_name_start
            + js[callee_name_start..]
                .find(" =")
                .expect("callee binding has an initializer");
        let callee_name = &js[callee_name_start..callee_name_end];
        let type_stage_binding = unique_position("const __kio_fn0_type");
        let type_stage_name_start = type_stage_binding + "const ".len();
        let type_stage_name_end = type_stage_name_start
            + js[type_stage_name_start..]
                .find(" =")
                .expect("type-stage binding has an initializer");
        let type_stage_name = &js[type_stage_name_start..type_stage_name_end];
        let callee_evaluation = unique_position(&format!("({callee_name}).advance();"));
        let type_stage_evaluation = unique_position(&format!("({type_stage_name})("));
        let first_product_slot = unique_position(", 0, 2)");
        let second_product_slot = unique_position(", 1, 2)");
        let projector_call = unique_position(
            "return Box.inspect(__kio_cps_receiver).advance()(__kio_cps_continuation);",
        );

        assert_eq!(js.matches("__kio_cps_receiver").count(), 2, "got: {js}");
        assert_eq!(js.matches("__kio_cps_continuation").count(), 2, "got: {js}");
        assert_eq!(js.matches(callee_name).count(), 2, "got: {js}");
        assert_eq!(js.matches(type_stage_name).count(), 2, "got: {js}");
        assert_eq!(js.matches("__kioProductSlot(").count(), 2, "got: {js}");
        assert!(
            receiver_binding < continuation_binding
                && continuation_binding < callee_binding
                && callee_binding < type_stage_binding
                && type_stage_binding < callee_evaluation
                && callee_evaluation < type_stage_evaluation
                && type_stage_evaluation < first_product_slot
                && first_product_slot < second_product_slot
                && second_product_slot < projector_call,
            "CPS expression construction must preserve receiver-before-continuation order: {js}"
        );
        assert!(js.contains("return __kioPoly(() => __kioLift(__kioContinuation => ((__kioContinuation).advance())(__kioPayload)), null)"), "{js}");
        assert!(!js.contains("Box.box("), "got: {js}");
    }

    // ---- Host-item codegen + literals + __if_then_else__ ---------------

    #[test]
    fn string_literal_lowers_to_js_string() {
        let js = lower(
            "module main; \
             host type String role(str); \
             fn greeting() -> String { \"hello\" }",
        );
        assert!(js.contains("\"hello\""), "got: {js}");
    }

    #[test]
    fn string_literal_escapes_special_chars() {
        let js = lower(
            "module main; \
             host type String role(str); \
             fn quoted() -> String { \"a\\\"b\\nc\" }",
        );
        // "a\"b\nc" — outer JS quotes plus escaped inner quote and newline.
        assert!(js.contains(r#""a\"b\nc""#), "got: {js}");
    }

    #[test]
    fn int_and_float_and_bool_literals_lower() {
        // Each fn body is a bare literal in tail position. The int
        // literal is genuinely ambiguous — its `Int` role shape admits
        // both the `i32` and `f64` host types — so it needs the typer's
        // tier-2 (expected-return) resolution, not just tier-3; hence
        // `lower_typed`, which runs the elaboration-aware typer that
        // pins each literal's host type before the Kio' boundary.
        let js = lower_typed(
            "module main; \
             host type I32 role(i32); host type F64 role(f64); host type Bool role(bool); \
             fn make_int() -> I32 { 42 } \
             fn make_float() -> F64 { 3.14 } \
             fn make_bool() -> Bool { .t }",
        );
        assert!(
            js.contains("const make_int = __kioLift(__kioUnit => 42)"),
            "got: {js}"
        );
        assert!(
            js.contains("const make_float = __kioLift(__kioUnit => 3.14)"),
            "got: {js}"
        );
        assert!(
            js.contains("const make_bool = __kioLift(__kioUnit => true)"),
            "got: {js}"
        );
    }

    #[test]
    fn wide_int_literals_emit_bigint_suffix() {
        // Five int-shaped host types in the module, so a
        // bare literal would be ambiguous — each carries an explicit
        // `(Type)` annotation. The `i64` / `u64` / `i128` / `u128`
        // roles emit BigInt syntax; `i32` stays a plain Number.
        let js = lower_typed(
            "module main; \
             host type I32 role(i32); host type I64 role(i64); host type U64 role(u64); host type I128 role(i128); host type U128 role(u128); \
             fn small() -> I32 { 7(I32) } \
             fn s64() -> I64 { 9_000_000_000(I64) } \
             fn u64_v() -> U64 { 9_000_000_000(U64) } \
             fn s128() -> I128 { 170_141_183_460_469_231_731_687_303_715_884_105_727(I128) } \
             fn u128_v() -> U128 { 0(U128) }",
        );
        assert!(
            js.contains("const small = __kioLift(__kioUnit => 7)"),
            "i32 stays bare: {js}"
        );
        assert!(
            js.contains("const s64 = __kioLift(__kioUnit => 9000000000n)"),
            "i64 gets n: {js}"
        );
        assert!(
            js.contains("const u64_v = __kioLift(__kioUnit => 9000000000n)"),
            "u64 gets n: {js}"
        );
        assert!(
            js.contains(
                "const s128 = __kioLift(__kioUnit => 170141183460469231731687303715884105727n)"
            ),
            "i128 gets n: {js}"
        );
        assert!(
            js.contains("const u128_v = __kioLift(__kioUnit => 0n)"),
            "u128 gets n even at 0: {js}"
        );
    }

    #[test]
    fn same_leaf_host_types_keep_exact_literal_roles() {
        let package = typed_package_routed_many(
            &[
                "module left; \
                 host type Shared role(i32); \
                 pub fn value_left() -> Shared { 7(Shared) }",
                "module right; \
                 host type Shared role(i64); \
                 pub fn value_right() -> Shared { 9(Shared) }",
            ],
            Some("bridge { left; right; }"),
        );
        let js = lower_package_to_factory_module(&package, "pkg").expect("lower package");

        let left = js
            .split("const value_left =")
            .nth(1)
            .expect("left module value is emitted")
            .split("const RIGHT =")
            .next()
            .expect("left module has a terminator");
        let right = js
            .split("const value_right =")
            .nth(1)
            .expect("right module value is emitted");
        assert!(
            left.contains("__kioLift(__kioUnit => 7)"),
            "left.Shared stays Number: {js}"
        );
        assert!(
            right.contains("__kioLift(__kioUnit => 9n)"),
            "right.Shared is BigInt: {js}"
        );
    }

    #[test]
    fn host_fn_def_call_lowers_to_host_record() {
        let js = lower(
            "module main; \
             host type String role(str); host fn print(s: String) -> .; \
             fn say(s: String) -> . { print(s) }",
        );
        // Rung-1 nested host record: the lifted host leaf still reaches
        // `main.print`, and the caller supplies its original `s` value.
        assert!(
            js.contains("(__kioLift((__kioHostArg0) => __host__.main.print(__kioHostArg0)))(s)"),
            "got: {js}"
        );
    }

    #[test]
    fn host_fn_def_value_position_lowers_to_host_record() {
        // Bind the host print to a local; the local then calls through.
        let js = lower(
            "module main; \
             host type String role(str); host fn print(s: String) -> .; \
             fn forward(s: String) -> . { let p = print; p(s) }",
        );
        // `let p = print` binds a lifted wrapper that preserves host FFI
        // conversions; `print` reaches the rung-1 nested host record.
        assert!(
            js.contains(
                "const p = __kioLift((__kioHostArg0) => __host__.main.print(__kioHostArg0))"
            ),
            "got: {js}"
        );
        assert!(js.contains("(p)(s)"), "got: {js}");
    }

    #[test]
    fn if_then_else_recovers_to_ternary() {
        // Structural recovery collapses `__if_then_else__` with
        // literal `.()` branch thunks into an `EnrichedConditional`;
        // codegen lowers that to a plain JS ternary — the IIFE
        // wrapper and the two branch thunks are gone.
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type Bool role(bool); \
             fn pick(c: Bool) -> . { \
               __if_then_else__(., c, .() { () }, .() { () }) \
             }",
        );
        assert!(js.contains("(c ? null : null)"), "got: {js}");
        assert!(!js.contains("__c, __t, __e"), "IIFE wrapper survived: {js}");
    }

    #[test]
    fn type_alias_skipped_silently() {
        let js = lower("module x; type Nothing = .;");
        // TypeAliases have no runtime presence — module emits only the header.
        assert!(!js.contains("function"), "got: {js}");
    }

    #[test]
    fn intrinsic_reference_in_body_rejected() {
        // Typecheck would fail without `import __intrinsics__;`; we don't
        // run the typer here, so confirm that bypassing that earlier contract
        // is classified as a compiler bug rather than a user-facing backend
        // limitation.
        let panic = std::panic::catch_unwind(|| lower("module x; fn f() -> . { __pair__ }"))
            .expect_err("an intrinsic value reference violates a typer invariant");
        let message = if let Some(message) = panic.downcast_ref::<String>() {
            message.as_str()
        } else if let Some(message) = panic.downcast_ref::<&str>() {
            message
        } else {
            panic!("panic payload was not a string")
        };
        assert!(message.contains("intrinsic `__pair__`"), "got: {message}");
    }

    // ---- JS reserved-identifier mangling ------------------------------

    #[test]
    fn fn_def_with_reserved_name_mangles_at_decl_and_reference() {
        // `default` is a JS reserved word. A Kio fn named `default`
        // must lower to the `__kio_default__` binding, and any
        // reference site (here: the body of `caller`) must use the
        // same mangled spelling. Without mangling, V8/QuickJS rejects
        // the emitted JS at parse time.
        let js = lower(
            "module x; \
             fn default[A](y: A) -> A { y } \
             fn caller[A](z: A) -> A { default(A, z) }",
        );
        assert!(
            js.contains("const __kio_default__ = __kioPoly(() => __kioLift((y) => y), null)"),
            "expected mangled decl, got: {js}"
        );
        assert!(
            js.contains("((__kio_default__).advance())(z)"),
            "expected mangled reference, got: {js}"
        );
        assert!(
            !js.contains("const default ="),
            "expected no unmangled `default` decl, got: {js}"
        );
    }

    #[test]
    fn fn_param_with_reserved_name_mangles_at_decl_and_use() {
        // Reserved name as a value parameter — both the lifted arrow's
        // parameter list and the body's reference must mangle.
        let js = lower("module x; fn id[A](class: A) -> A { class }");
        assert!(
            js.contains(
                "const id = __kioPoly(() => __kioLift((__kio_class__) => __kio_class__), null)"
            ),
            "expected mangled param, got: {js}"
        );
        assert!(
            js.contains("=> __kio_class__)"),
            "expected mangled body reference, got: {js}"
        );
    }

    #[test]
    fn let_binding_with_reserved_name_mangles() {
        let js = lower("module x; fn f(x: .) -> . { let return = x; return }");
        assert!(
            js.contains("const __kio_return__ = x"),
            "expected mangled let, got: {js}"
        );
        assert!(
            js.contains("return __kio_return__"),
            "expected mangled body reference, got: {js}"
        );
    }

    #[test]
    fn export_shorthand_uses_mangled_name() {
        // `return` as a fn name must round-trip through the IIFE
        // shorthand-export. Object shorthand `{ return }` would be a
        // syntax error in JS — `return` in expression position is the
        // reserved word — so the export must read `{ __kio_return__ }`.
        let js = lower("module x; pub fn return(y: .) -> . { y }");
        assert!(
            js.contains("return { __kio_return__"),
            "expected mangled export shorthand, got: {js}"
        );
    }

    #[test]
    fn non_reserved_identifier_passes_through() {
        // Sanity check: a name that doesn't collide with any reserved
        // word stays unchanged through the same code path.
        let js = lower("module x; fn helper[A](y: A) -> A { y }");
        assert!(
            js.contains("const helper = __kioPoly(() => __kioLift((y) => y), null)"),
            "got: {js}"
        );
        assert!(
            !js.contains("__kio_helper__") && !js.contains("__kio_y__"),
            "got: {js}"
        );
    }
}

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
//! Polymorphism is **type-erased at the host surface**: type arguments are
//! never JS values. Internal Kio callables retain one nullary callable stage
//! per type binder so interleaved declaration groups and evaluation order stay
//! observable. The Routed phase has already split type arguments from value
//! arguments on every `Low*` call variant; the renderer advances the erased
//! stages separately and passes only value arguments. The unit value `()`
//! becomes `null`.
//!
//! Runtime data layout (the only guarantees JS code emitted by this
//! pass relies on):
//!
//! - **Sums** `(a | b)`: `[0, x]` for left, `[1, x]` for right. The tag
//!   lives at index 0; the payload at index 1.
//! - **Products** `(A & B)`: `[x, y]`. First component at index 0,
//!   second at index 1.
//! - **Unit** `()`: the JS value `null` (chosen over `undefined` to
//!   make the unit projection explicit in emitted source).
//! - **Bottom** `!`: no values; `__absurd__` throws if reached.
//! - **`newtype Foo`**: the iso-recursive wrap/unwrap is type-erased,
//!   so a value of `Foo` has the **same runtime shape as its payload**.
//!   The constructor and projector are runtime identities — the
//!   nominal distinction is purely a compile-time check.
//!
//! The JS that's emitted is intentionally plain: `function name(...)
//! { return <body>; }` for top-level fn_defs, IIFE arrow functions for
//! `let` bindings, arrow functions for lambda expressions, the
//! enriched-node lowerings (n-ary tuple → nested-binary array, n-arm
//! match → tag-dispatch ternary chain, conditional → ternary), the
//! `__absurd__` throwing-IIFE template, and per-newtype namespace
//! objects (`const Foo = { mk_foo: (x) => x, un_foo: (x) => x };`).
//! No minification, no source maps, no module-system glue.

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
use crate::ast::{ImportKind, NewtypeHostSurface, Routed};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryNewtypeSurface,
    BoundaryNominalDeclaration, BoundaryPublicNewtypeInventoryEntry, CallableExecutionStage,
    CallableSourceParamAdapter, CallableValueStageLayout, FacadeBinderId, FacadeUse, FacadeUseId,
    PreparedBoundaryCallableSite, PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};
use crate::backends::public_names::{
    FacadeSelector, encode_source_identity, host_module_key, host_name_core, module_facade_path,
};
use crate::backends::reconstruct::{infer_fn_param_tys_from_body, nth_product_slot_ty_for_arity};
use crate::backends::structural::{
    ProductRebuildPlan, bound_product_rebuild_plan, render_cached_product_rebuild,
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
                    Some(prepared),
                    (emit_cache, emit_context.as_deref()),
                )
                .map(|js| indent_block(&js, "  "))
            })
            .collect();
    let mut generated_body = String::new();
    for piece in module_pieces? {
        generated_body.push_str(&piece);
    }

    // Export record. Only an explicit `export` exposes package items.
    if let Some(entry) = package.package_file()
        && let Some(body) = lower_package_file_prepared(
            &entry.package_file,
            &entry.package_name,
            package,
            prepared,
        )?
    {
        generated_body.push_str(&indent_block(&body, "  "));
    } else {
        generated_body.push_str("  const __export_record__ = {};\n");
    }

    // The package's host-facing surface is the union of declared
    // export items (by name) and a stable, additive set of
    // package-level utilities. No extra utilities are defined beyond the
    // export record; `Object.assign` lets contract-authorized entries join the
    // same assembly shape.
    generated_body.push_str("  return Object.assign(Object.create(null), __export_record__);\n");

    if js_uses_structural_helpers(&generated_body) {
        out.push_str(render_js_structural_helpers());
    }
    if js_uses_opaque_helpers(&generated_body) {
        out.push_str(render_js_opaque_helpers());
    }
    out.push_str(&generated_body);

    Ok(out)
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
         slots.push(cur[0]);\n\
         cur = cur[1];\n\
       }\n\
       return slots;\n\
     }\n\
     function __kioProductSlot(v, index, arity) {\n\
       if (index < 0 || index >= arity) throw new Error(\"product slot out of bounds\");\n\
       let cur = v;\n\
       for (let i = 0; i < index; i++) cur = cur[1];\n\
       return index + 1 === arity ? cur : cur[0];\n\
     }\n\
     function __kioSumPayload(v, variants) {\n\
       if (variants <= 0) throw new Error(\"sum arity out of bounds\");\n\
       let cur = v;\n\
       for (let i = 0; i < variants; i++) {\n\
         if (i + 1 === variants) return [i, cur];\n\
         if (cur[0] === 0) return [i, cur[1]];\n\
         cur = cur[1];\n\
       }\n\
       throw new Error(\"sum payload out of bounds\");\n\
     }\n\
     function __kioSumInject(payload, variant, variants) {\n\
       if (variants <= 0 || variant < 0 || variant >= variants) {\n\
         throw new Error(\"sum variant out of bounds\");\n\
       }\n\
       let cur = payload;\n\
       if (variant + 1 < variants) cur = [0, cur];\n\
       for (let i = 0; i < variant; i++) cur = [1, cur];\n\
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
    )
}

fn lower_module_with_key_prepared(
    module: &Module,
    module_key: &str,
    package_file: Option<&PackageFile>,
    package_name: Option<&str>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    prepared: Option<&PreparedBoundaryCallableSites>,
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

    let top_level = build_top_level(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared,
    );
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
    prepared: Option<&PreparedBoundaryCallableSites>,
    emit: (Option<&JsEmitCache>, Option<&str>),
) -> Result<String, EmitError> {
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
    prepared: Option<&PreparedBoundaryCallableSites>,
    _emit: (Option<&JsEmitCache>, Option<&str>),
) -> Result<String, EmitError> {
    lower_module_with_key_prepared(
        module,
        module_key,
        package_file,
        package_name,
        package,
        prepared,
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
            lower_newtype(d, &mut decl);
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
                        lower_newtype(newtype, &mut decl);
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
    _package_name: &str,
    _package: &crate::pass::resolve::Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<Option<String>, EmitError> {
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
        tree.insert(&path, value);
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
        tree.insert(&path, value);
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
    Ok(Some(out))
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
}

#[derive(Debug)]
enum ReturnEntry {
    Leaf(String),
    Subtree(ReturnTree),
}

impl ReturnTree {
    /// Insert a `value` at a role-framed facade `path`.
    ///
    /// Module and type edges occupy disjoint reserved selector classes, so a
    /// leaf/subtree collision is an internal bug rather than a precedence
    /// decision.
    fn insert(&mut self, path: &[String], value: String) {
        debug_assert!(!path.is_empty());
        if path.len() == 1 {
            assert!(
                self.entries
                    .insert(path[0].clone(), ReturnEntry::Leaf(value))
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
        sub.insert(rest, value);
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
                ReturnEntry::Leaf(value) => {
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

/// Qualify the fallback signature used only by standalone host-call lowering.
/// Package mode selects the exact prepared site instead. Routed preserves
/// module-local bare spellings, so even this fallback must never recover
/// nominal identity by a package-wide same-leaf scan. Type binders remain
/// lexical and shadow module declarations.
fn qualify_boundary_signature_and_ret(
    sig: &Signature,
    ret: &Type,
    module: &Module,
) -> (Signature, Type) {
    let mut locals = HashMap::new();
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(param) => {
                locals.insert(param.name.clone(), param.effective_kind());
                SignatureParam::Type(param.clone())
            }
            SignatureParam::Value(param) => SignatureParam::Value(crate::ast::Param {
                name: param.name.clone(),
                ty: param.ty.as_ref().map(|ty| {
                    crate::pass::resolve::qualify_routed_contract_type_in_module(
                        ty, module, &locals,
                    )
                }),
                pattern: param.pattern,
                meta: param.meta.clone(),
            }),
        })
        .collect();
    let sig = Signature {
        params,
        groups: sig.groups.clone(),
    };
    let ret = crate::pass::resolve::qualify_routed_contract_type_in_module(ret, module, &locals);
    (sig, ret)
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum PreparedJsDir {
    In,
    Out,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PreparedJsNominalContext {
    Standalone,
    StructuralSlot,
}

fn semantic_js_key(key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name } => host_name_core(name),
        SemanticKey::Qualified {
            module_segments,
            name,
        } => format!(
            "{}.{}",
            module_segments
                .iter()
                .map(|part| host_name_core(part))
                .collect::<Vec<_>>()
                .join("/"),
            host_name_core(name)
        ),
        SemanticKey::Positional { index } => format!("_{index}"),
    }
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
    let Some(BoundaryNominalDeclaration::Newtype {
        transparent_payload: Some(payload),
        ..
    }) = site.nominals().declaration(root)
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
    let Some(BoundaryNominalDeclaration::Newtype {
        type_params,
        transparent_payload: Some(payload),
        ..
    }) = site.nominals().declaration(name)
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

fn prepared_js_nominal_is_passthrough(
    site: PreparedBoundaryCallableSite<'_>,
    name: &QualifiedTypeName,
) -> bool {
    match site.nominals().declaration(name) {
        Some(BoundaryNominalDeclaration::HostType { .. }) => true,
        None => false,
        Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: None,
            ..
        }) => false,
        Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(_),
            ..
        }) => prepared_js_newtype_is_recursive(site, name),
    }
}

#[derive(Clone, Copy)]
struct PreparedJsConversionUseRef<'a> {
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    id: FacadeUseId,
}

struct PreparedJsConversionBinderFrame<'a> {
    plan: &'a BoundaryFacadePlan,
    substitutions: Vec<(FacadeBinderId, PreparedJsConversionUseRef<'a>)>,
}

fn prepared_js_conversion_substitution<'a>(
    plan: &'a BoundaryFacadePlan,
    binder: FacadeBinderId,
    binder_frames: &[PreparedJsConversionBinderFrame<'a>],
) -> Option<PreparedJsConversionUseRef<'a>> {
    binder_frames
        .iter()
        .rev()
        .find(|frame| std::ptr::eq(frame.plan, plan))
        .and_then(|frame| {
            frame
                .substitutions
                .iter()
                .find_map(|(candidate, use_ref)| (*candidate == binder).then_some(*use_ref))
        })
}

fn prepared_js_resolve_nominal_application<'a>(
    constructor: PreparedJsConversionUseRef<'a>,
    args: &[PreparedJsConversionUseRef<'a>],
    binder_frames: &[PreparedJsConversionBinderFrame<'a>],
) -> Option<(&'a QualifiedTypeName, Vec<PreparedJsConversionUseRef<'a>>)> {
    fn resolve<'a>(
        use_ref: PreparedJsConversionUseRef<'a>,
        binder_frames: &[PreparedJsConversionBinderFrame<'a>],
        visited: &mut HashSet<(usize, usize)>,
    ) -> Option<(&'a QualifiedTypeName, Vec<PreparedJsConversionUseRef<'a>>)> {
        let identity = (
            use_ref.plan as *const BoundaryFacadePlan as usize,
            use_ref.id.index(),
        );
        if !visited.insert(identity) {
            return None;
        }
        match use_ref.plan.use_at(use_ref.id) {
            FacadeUse::Nominal { name, .. } => Some((name, Vec::new())),
            FacadeUse::Bound { binder, .. } => {
                prepared_js_conversion_substitution(use_ref.plan, *binder, binder_frames)
                    .and_then(|substitution| resolve(substitution, binder_frames, visited))
            }
            FacadeUse::Apply {
                constructor, args, ..
            } => {
                let constructor = PreparedJsConversionUseRef {
                    plan: use_ref.plan,
                    execution: use_ref.execution,
                    id: *constructor,
                };
                let (name, mut applied) = resolve(constructor, binder_frames, visited)?;
                applied.extend(args.iter().map(|id| PreparedJsConversionUseRef {
                    plan: use_ref.plan,
                    execution: use_ref.execution,
                    id: *id,
                }));
                Some((name, applied))
            }
            FacadeUse::Unit { .. }
            | FacadeUse::Bottom { .. }
            | FacadeUse::Product { .. }
            | FacadeUse::Sum { .. }
            | FacadeUse::Function { .. }
            | FacadeUse::Forall { .. } => None,
        }
    }

    let (name, mut applied) = resolve(constructor, binder_frames, &mut HashSet::new())?;
    applied.extend_from_slice(args);
    Some((name, applied))
}

fn prepared_js_use_is_passthrough<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    id: FacadeUseId,
    binder_frames: &[PreparedJsConversionBinderFrame<'a>],
) -> bool {
    match plan.use_at(id) {
        FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } => true,
        FacadeUse::Bound { binder, .. } => {
            prepared_js_conversion_substitution(plan, *binder, binder_frames).is_none_or(
                |use_ref| {
                    prepared_js_use_is_passthrough(
                        site,
                        use_ref.plan,
                        use_ref.execution,
                        use_ref.id,
                        binder_frames,
                    )
                },
            )
        }
        FacadeUse::Nominal { name, .. } => prepared_js_nominal_is_passthrough(site, name),
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            let constructor = PreparedJsConversionUseRef {
                plan,
                execution,
                id: *constructor,
            };
            let args = args
                .iter()
                .map(|id| PreparedJsConversionUseRef {
                    plan,
                    execution,
                    id: *id,
                })
                .collect::<Vec<_>>();
            prepared_js_resolve_nominal_application(constructor, &args, binder_frames)
                .is_none_or(|(name, _)| prepared_js_nominal_is_passthrough(site, name))
        }
        FacadeUse::Product { .. } | FacadeUse::Sum { .. } => false,
        FacadeUse::Function { slots, result, .. } => {
            let BoundaryFacadeExecutionUse::Function(layout) = execution.use_at(id) else {
                return false;
            };
            layout
                .source_params()
                .iter()
                .all(|source| source.adapter() == CallableSourceParamAdapter::Identity)
                && slots.iter().all(|slot| {
                    prepared_js_use_is_passthrough(site, plan, execution, *slot, binder_frames)
                })
                && prepared_js_use_is_passthrough(site, plan, execution, *result, binder_frames)
        }
        FacadeUse::Forall { result, .. } => {
            matches!(
                execution.use_at(id),
                BoundaryFacadeExecutionUse::NoAction
                    | BoundaryFacadeExecutionUse::DeclarationBinder
            ) && prepared_js_use_is_passthrough(site, plan, execution, *result, binder_frames)
        }
    }
}

fn prepared_js_callable_needs_wrapper(
    site: PreparedBoundaryCallableSite<'_>,
) -> Result<bool, EmitError> {
    if matches!(
        site.site().owner(),
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
            | BoundaryFacadeSiteOwner::NewtypeProjector { .. }
    ) {
        return Ok(true);
    }
    let execution = prepared_js_execution(site)?;
    let entry = site.plan().entry();
    let value_stage_count = entry
        .head_stages
        .iter()
        .filter(|stage| matches!(stage, BoundaryCallableHeadStage::Value { .. }))
        .count();
    if value_stage_count > 1
        || entry
            .head_stages
            .iter()
            .any(|stage| matches!(stage, BoundaryCallableHeadStage::Type { .. }))
    {
        return Ok(true);
    }
    for stage in execution.head_stages() {
        if let CallableExecutionStage::Value(layout) = stage
            && layout
                .source_params()
                .iter()
                .any(|source| source.adapter() != CallableSourceParamAdapter::Identity)
        {
            return Ok(true);
        }
    }
    let plan = site.plan().facade();
    let root_execution = execution.root_uses();
    for stage in entry.head_stages {
        if let BoundaryCallableHeadStage::Value { slots } = stage
            && slots
                .iter()
                .any(|slot| !prepared_js_use_is_passthrough(site, plan, root_execution, *slot, &[]))
        {
            return Ok(true);
        }
    }
    Ok(!prepared_js_use_is_passthrough(
        site,
        plan,
        root_execution,
        entry.returned,
        &[],
    ))
}

fn prepared_js_source_public_count(layout: &CallableValueStageLayout) -> usize {
    layout
        .source_params()
        .iter()
        .filter(|source| source.adapter() != CallableSourceParamAdapter::UnitValue)
        .count()
}

fn prepared_js_right_nest(values: &[String]) -> Result<String, EmitError> {
    let Some((last, prefix)) = values.split_last() else {
        unreachable!("a prepared JavaScript product adapter has no values");
    };
    Ok(prefix
        .iter()
        .rev()
        .fold(last.clone(), |tail, value| format!("[{value}, {tail}]")))
}

#[allow(clippy::too_many_arguments)]
fn prepared_js_public_to_body_args<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    public_values: &[String],
    compacted_foralls: &BTreeSet<FacadeUseId>,
    binder_frames: &mut Vec<PreparedJsConversionBinderFrame<'a>>,
    depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count()
        || public_values.len() != prepared_js_source_public_count(layout)
    {
        unreachable!("prepared JavaScript source/public slot alignment drift");
    }
    let mut public_cursor = 0usize;
    let mut body = Vec::with_capacity(layout.body_abi_arity());
    for source in layout.source_params() {
        let range = source.facade_slots();
        let source_slots = slots
            .get(range.clone())
            .unwrap_or_else(|| unreachable!("prepared JavaScript source range drift"));
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => {
                body.push("null".to_owned());
            }
            CallableSourceParamAdapter::Identity => {
                let value = public_values
                    .get(public_cursor)
                    .unwrap_or_else(|| unreachable!("missing JavaScript public source value"));
                let [slot] = source_slots else {
                    unreachable!("JavaScript identity source does not own one facade slot");
                };
                body.push(convert_prepared_js_use(
                    site,
                    plan,
                    execution,
                    *slot,
                    value,
                    PreparedJsDir::In,
                    PreparedJsNominalContext::Standalone,
                    compacted_foralls,
                    binder_frames,
                    depth + 1,
                )?);
                public_cursor += 1;
            }
            CallableSourceParamAdapter::RightNest => {
                let value = public_values
                    .get(public_cursor)
                    .unwrap_or_else(|| unreachable!("missing JavaScript public product value"));
                let shell = source.product_shell().unwrap_or_else(|| {
                    unreachable!("JavaScript RightNest source has no prepared shell")
                });
                if shell.ordered_keys().len() != source_slots.len() {
                    unreachable!("JavaScript prepared product key/slot drift");
                }
                let converted = source_slots
                    .iter()
                    .zip(shell.ordered_keys())
                    .map(|(slot, key)| {
                        convert_prepared_js_use(
                            site,
                            plan,
                            execution,
                            *slot,
                            &js_member_access(value, &semantic_js_key(key)),
                            PreparedJsDir::In,
                            PreparedJsNominalContext::StructuralSlot,
                            compacted_foralls,
                            binder_frames,
                            depth + 1,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                body.push(prepared_js_right_nest(&converted)?);
                public_cursor += 1;
            }
        }
    }
    if public_cursor != public_values.len() || body.len() != layout.body_abi_arity() {
        unreachable!("prepared JavaScript source adapter did not consume its exact domain");
    }
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
fn prepared_js_body_to_public_args<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    body_values: &[String],
    compacted_foralls: &BTreeSet<FacadeUseId>,
    binder_frames: &mut Vec<PreparedJsConversionBinderFrame<'a>>,
    depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count() || body_values.len() != layout.body_abi_arity() {
        unreachable!("prepared JavaScript body/source slot alignment drift");
    }
    let mut public = Vec::with_capacity(prepared_js_source_public_count(layout));
    for (source, body_value) in layout.source_params().iter().zip(body_values) {
        let range = source.facade_slots();
        let source_slots = slots
            .get(range)
            .unwrap_or_else(|| unreachable!("prepared JavaScript source range drift"));
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => {}
            CallableSourceParamAdapter::Identity => {
                let [slot] = source_slots else {
                    unreachable!("JavaScript identity source does not own one facade slot");
                };
                public.push(convert_prepared_js_use(
                    site,
                    plan,
                    execution,
                    *slot,
                    body_value,
                    PreparedJsDir::Out,
                    PreparedJsNominalContext::Standalone,
                    compacted_foralls,
                    binder_frames,
                    depth + 1,
                )?);
            }
            CallableSourceParamAdapter::RightNest => {
                let shell = source.product_shell().unwrap_or_else(|| {
                    unreachable!("JavaScript RightNest source has no prepared shell")
                });
                if shell.ordered_keys().len() != source_slots.len() {
                    unreachable!("JavaScript prepared product key/slot drift");
                }
                let mut object = String::from("{ ");
                for (index, (slot, key)) in
                    source_slots.iter().zip(shell.ordered_keys()).enumerate()
                {
                    if index > 0 {
                        object.push_str(", ");
                    }
                    object.push_str(&js_object_key(&semantic_js_key(key)));
                    object.push_str(": ");
                    object.push_str(&convert_prepared_js_use(
                        site,
                        plan,
                        execution,
                        *slot,
                        &format!(
                            "__kioProductSlot({body_value}, {index}, {})",
                            source_slots.len()
                        ),
                        PreparedJsDir::Out,
                        PreparedJsNominalContext::StructuralSlot,
                        compacted_foralls,
                        binder_frames,
                        depth + 1,
                    )?);
                }
                object.push_str(" }");
                public.push(object);
            }
        }
    }
    Ok(public)
}

#[allow(clippy::too_many_arguments)]
fn convert_prepared_js_nominal<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    name: &QualifiedTypeName,
    args: &[PreparedJsConversionUseRef<'a>],
    expression: &str,
    direction: PreparedJsDir,
    context: PreparedJsNominalContext,
    compacted_foralls: &BTreeSet<FacadeUseId>,
    binder_frames: &mut Vec<PreparedJsConversionBinderFrame<'a>>,
    depth: usize,
) -> Result<String, EmitError> {
    let declaration = site.nominals().declaration(name).unwrap_or_else(|| {
        unreachable!(
            "prepared JavaScript facade is missing nominal {}.{}",
            name.module_segments().join("."),
            name.name()
        )
    });
    match declaration {
        BoundaryNominalDeclaration::HostType { .. } => Ok(expression.to_owned()),
        BoundaryNominalDeclaration::Newtype {
            transparent_payload: None,
            ..
        } => {
            let identity = encoded_newtype_identity(&name.module_segments().join("/"), name.name());
            let helper = match direction {
                PreparedJsDir::In => "__kioOpaqueIn",
                PreparedJsDir::Out => "__kioOpaqueOut",
            };
            Ok(format!("{helper}(\"{identity}\", {expression})"))
        }
        BoundaryNominalDeclaration::Newtype {
            type_params,
            transparent_payload: Some(payload),
            ..
        } => {
            if prepared_js_newtype_is_recursive(site, name) {
                return Ok(expression.to_owned());
            }
            let execution = prepared_js_execution(site)?;
            let payload_execution = execution.transparent_payload(name).unwrap_or_else(|| {
                unreachable!(
                    "prepared JavaScript facade has no payload execution for {}.{}",
                    name.module_segments().join("."),
                    name.name()
                )
            });
            let payload_expression = if direction == PreparedJsDir::In
                && context == PreparedJsNominalContext::Standalone
            {
                js_member_access(expression, &host_name_core(name.name()))
            } else {
                expression.to_owned()
            };
            if args.len() != type_params.len() {
                unreachable!(
                    "prepared JavaScript nominal {}.{} expected {} type arguments, got {}",
                    name.module_segments().join("."),
                    name.name(),
                    type_params.len(),
                    args.len()
                );
            }
            let substitutions = payload
                .declaration_binders()
                .iter()
                .take(type_params.len())
                .copied()
                .zip(args.iter().copied())
                .collect();
            binder_frames.push(PreparedJsConversionBinderFrame {
                plan: payload.facade(),
                substitutions,
            });
            let converted = convert_prepared_js_use(
                site,
                payload.facade(),
                payload_execution,
                payload.payload_root(),
                &payload_expression,
                direction,
                PreparedJsNominalContext::Standalone,
                compacted_foralls,
                binder_frames,
                depth + 1,
            );
            binder_frames.pop();
            let converted = converted?;
            if direction == PreparedJsDir::Out && context == PreparedJsNominalContext::Standalone {
                Ok(format!(
                    "{{ {}: {converted} }}",
                    js_object_key(&host_name_core(name.name()))
                ))
            } else {
                Ok(converted)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn convert_prepared_js_function<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    id: FacadeUseId,
    slots: &[FacadeUseId],
    result: FacadeUseId,
    expression: &str,
    direction: PreparedJsDir,
    compacted_foralls: &BTreeSet<FacadeUseId>,
    binder_frames: &mut Vec<PreparedJsConversionBinderFrame<'a>>,
    depth: usize,
) -> Result<String, EmitError> {
    let BoundaryFacadeExecutionUse::Function(layout) = execution.use_at(id) else {
        unreachable!("prepared JavaScript function has no paired execution layout");
    };
    let inner = format!("__kioBoundaryInner{depth}_{}", id.index());
    match direction {
        PreparedJsDir::Out => {
            let params = (0..prepared_js_source_public_count(layout))
                .map(|index| format!("__kioBoundaryPublic{depth}_{index}"))
                .collect::<Vec<_>>();
            let body_args = prepared_js_public_to_body_args(
                site,
                plan,
                execution,
                slots,
                layout,
                &params,
                compacted_foralls,
                binder_frames,
                depth + 1,
            )?;
            let call = format!("{inner}({})", body_args.join(", "));
            let returned = convert_prepared_js_use(
                site,
                plan,
                execution,
                result,
                &call,
                direction,
                PreparedJsNominalContext::Standalone,
                compacted_foralls,
                binder_frames,
                depth + 1,
            )?;
            Ok(format!(
                "(({inner}) => ({}) => ({returned}))({expression})",
                params.join(", ")
            ))
        }
        PreparedJsDir::In => {
            let params = (0..layout.body_abi_arity())
                .map(|index| format!("__kioBoundaryBody{depth}_{index}"))
                .collect::<Vec<_>>();
            let public_args = prepared_js_body_to_public_args(
                site,
                plan,
                execution,
                slots,
                layout,
                &params,
                compacted_foralls,
                binder_frames,
                depth + 1,
            )?;
            let call = format!("{inner}({})", public_args.join(", "));
            let returned = convert_prepared_js_use(
                site,
                plan,
                execution,
                result,
                &call,
                direction,
                PreparedJsNominalContext::Standalone,
                compacted_foralls,
                binder_frames,
                depth + 1,
            )?;
            Ok(format!(
                "(({inner}) => ({}) => ({returned}))({expression})",
                params.join(", ")
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn convert_prepared_js_use<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    execution: &'a BoundaryFacadeExecutionPlan,
    id: FacadeUseId,
    expression: &str,
    direction: PreparedJsDir,
    context: PreparedJsNominalContext,
    compacted_foralls: &BTreeSet<FacadeUseId>,
    binder_frames: &mut Vec<PreparedJsConversionBinderFrame<'a>>,
    depth: usize,
) -> Result<String, EmitError> {
    if prepared_js_use_is_passthrough(site, plan, execution, id, binder_frames) {
        return Ok(expression.to_owned());
    }
    // Only a nominal head in the original slot owns its enclosing key.
    // Substitution preserves the shell, so an abstract slot keeps the full
    // standalone representation of its substituted argument.
    let mut original_head = id;
    while let FacadeUse::Apply { constructor, .. } = plan.use_at(original_head) {
        original_head = *constructor;
    }
    let context = if matches!(plan.use_at(original_head), FacadeUse::Nominal { .. }) {
        context
    } else {
        PreparedJsNominalContext::Standalone
    };
    match plan.use_at(id) {
        FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } => Ok(expression.to_owned()),
        FacadeUse::Bound { binder, .. } => {
            let use_ref = prepared_js_conversion_substitution(plan, *binder, binder_frames)
                .unwrap_or_else(|| {
                    unreachable!("non-passthrough JavaScript binder has no prepared substitution")
                });
            convert_prepared_js_use(
                site,
                use_ref.plan,
                use_ref.execution,
                use_ref.id,
                expression,
                direction,
                context,
                compacted_foralls,
                binder_frames,
                depth + 1,
            )
        }
        FacadeUse::Nominal { name, .. } => convert_prepared_js_nominal(
            site,
            name,
            &[],
            expression,
            direction,
            context,
            compacted_foralls,
            binder_frames,
            depth,
        ),
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            let constructor = PreparedJsConversionUseRef {
                plan,
                execution,
                id: *constructor,
            };
            let args = args
                .iter()
                .map(|id| PreparedJsConversionUseRef {
                    plan,
                    execution,
                    id: *id,
                })
                .collect::<Vec<_>>();
            if let Some((name, args)) =
                prepared_js_resolve_nominal_application(constructor, &args, binder_frames)
            {
                convert_prepared_js_nominal(
                    site,
                    name,
                    &args,
                    expression,
                    direction,
                    context,
                    compacted_foralls,
                    binder_frames,
                    depth,
                )
            } else {
                Ok(expression.to_owned())
            }
        }
        FacadeUse::Product { shell, args, .. } => {
            let value = format!("__kioBoundaryProduct{depth}_{}", id.index());
            match direction {
                PreparedJsDir::In => {
                    let converted = args
                        .iter()
                        .zip(shell.ordered_keys())
                        .map(|(arg, key)| {
                            convert_prepared_js_use(
                                site,
                                plan,
                                execution,
                                *arg,
                                &js_member_access(&value, &semantic_js_key(key)),
                                direction,
                                PreparedJsNominalContext::StructuralSlot,
                                compacted_foralls,
                                binder_frames,
                                depth + 1,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(format!(
                        "(({value}) => {})({expression})",
                        prepared_js_right_nest(&converted)?
                    ))
                }
                PreparedJsDir::Out => {
                    let mut object = String::from("{ ");
                    for (index, (arg, key)) in args.iter().zip(shell.ordered_keys()).enumerate() {
                        if index > 0 {
                            object.push_str(", ");
                        }
                        object.push_str(&js_object_key(&semantic_js_key(key)));
                        object.push_str(": ");
                        object.push_str(&convert_prepared_js_use(
                            site,
                            plan,
                            execution,
                            *arg,
                            &format!("{value}[{index}]"),
                            direction,
                            PreparedJsNominalContext::StructuralSlot,
                            compacted_foralls,
                            binder_frames,
                            depth + 1,
                        )?);
                    }
                    object.push_str(" }");
                    Ok(format!(
                        "(() => {{ const {value} = __kioProductSlots({expression}, {}); return {object}; }})()",
                        args.len()
                    ))
                }
            }
        }
        FacadeUse::Sum { shell, args, .. } => {
            let value = format!("__kioBoundarySum{depth}_{}", id.index());
            match direction {
                PreparedJsDir::In => {
                    if args.is_empty() {
                        unreachable!("prepared JavaScript sum has no alternatives");
                    }
                    let mut alternatives = String::new();
                    for (index, (arg, key)) in args.iter().zip(shell.ordered_keys()).enumerate() {
                        let converted = convert_prepared_js_use(
                            site,
                            plan,
                            execution,
                            *arg,
                            &js_member_access(&value, &semantic_js_key(key)),
                            direction,
                            PreparedJsNominalContext::StructuralSlot,
                            compacted_foralls,
                            binder_frames,
                            depth + 1,
                        )?;
                        if index + 1 == args.len() {
                            alternatives.push_str(&format!(
                                "__kioSumInject({converted}, {index}, {})",
                                args.len()
                            ));
                        } else {
                            alternatives.push_str(&format!(
                                "{} ? __kioSumInject({converted}, {index}, {}) : ",
                                format_in_check(&value, &semantic_js_key(key)),
                                args.len()
                            ));
                        }
                    }
                    Ok(format!("(({value}) => {alternatives})({expression})"))
                }
                PreparedJsDir::Out => {
                    let mut cases = String::new();
                    for (index, (arg, key)) in args.iter().zip(shell.ordered_keys()).enumerate() {
                        let converted = convert_prepared_js_use(
                            site,
                            plan,
                            execution,
                            *arg,
                            &format!("{value}[1]"),
                            direction,
                            PreparedJsNominalContext::StructuralSlot,
                            compacted_foralls,
                            binder_frames,
                            depth + 1,
                        )?;
                        cases.push_str(&format!(
                            "case {index}: return {{ {}: {converted} }}; ",
                            js_object_key(&semantic_js_key(key))
                        ));
                    }
                    Ok(format!(
                        "(() => {{ const {value} = __kioSumPayload({expression}, {}); switch ({value}[0]) {{ {cases}default: throw new Error(\"sum payload out of bounds\"); }} }})()",
                        args.len()
                    ))
                }
            }
        }
        FacadeUse::Function { slots, result, .. } => convert_prepared_js_function(
            site,
            plan,
            execution,
            id,
            slots,
            *result,
            expression,
            direction,
            compacted_foralls,
            binder_frames,
            depth,
        ),
        FacadeUse::Forall { result, .. } => {
            let compacted =
                std::ptr::eq(plan, site.plan().facade()) && compacted_foralls.contains(&id);
            if compacted
                || matches!(
                    execution.use_at(id),
                    BoundaryFacadeExecutionUse::NoAction
                        | BoundaryFacadeExecutionUse::DeclarationBinder
                )
            {
                return convert_prepared_js_use(
                    site,
                    plan,
                    execution,
                    *result,
                    expression,
                    direction,
                    context,
                    compacted_foralls,
                    binder_frames,
                    depth + 1,
                );
            }
            if !matches!(
                execution.use_at(id),
                BoundaryFacadeExecutionUse::InvokeForall
            ) {
                unreachable!("prepared JavaScript forall has no execution action");
            }
            match direction {
                PreparedJsDir::Out => convert_prepared_js_use(
                    site,
                    plan,
                    execution,
                    *result,
                    &format!("({expression})()"),
                    direction,
                    context,
                    compacted_foralls,
                    binder_frames,
                    depth + 1,
                ),
                PreparedJsDir::In => {
                    let stage = format!("__kioBoundaryStage{depth}_{}", id.index());
                    let converted = convert_prepared_js_use(
                        site,
                        plan,
                        execution,
                        *result,
                        &stage,
                        direction,
                        context,
                        compacted_foralls,
                        binder_frames,
                        depth + 1,
                    )?;
                    Ok(format!("(({stage}) => () => {converted})({expression})"))
                }
            }
        }
    }
}

fn maybe_emit_prepared_export_value_wrapper(
    site: PreparedBoundaryCallableSite<'_>,
    internal: &str,
    wrappers: &mut String,
    wrapper_count: &mut usize,
) -> Result<Option<String>, EmitError> {
    if !prepared_js_callable_needs_wrapper(site)? {
        return Ok(None);
    }
    let execution = prepared_js_execution(site)?;
    let plan = site.plan().facade();
    let entry = site.plan().entry();
    if entry.head_stages.len() != execution.head_stages().len() {
        unreachable!("prepared JavaScript callable head alignment drift");
    }
    // The public facade stays canonical. Only the fixed compact private ABI of
    // a direct existential projector consumes this prepared exact-ID set.
    let compacted_foralls = execution.direct_projector_compactable_foralls();
    let public_count = execution
        .head_stages()
        .iter()
        .filter_map(|stage| match stage {
            CallableExecutionStage::Type { .. } => None,
            CallableExecutionStage::Value(layout) => Some(prepared_js_source_public_count(layout)),
        })
        .sum::<usize>();
    let public_values = (0..public_count)
        .map(|index| format!("__a{index}"))
        .collect::<Vec<_>>();

    let wrapper_name = format!("__export_fn_{}", *wrapper_count);
    *wrapper_count += 1;
    wrappers.push_str(&format!(
        "function {wrapper_name}({}) {{\n",
        public_values.join(", ")
    ));

    let direct_newtype = matches!(
        site.site().owner(),
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
            | BoundaryFacadeSiteOwner::NewtypeProjector { .. }
    );
    let mut applied = internal.to_owned();
    let mut public_cursor = 0usize;
    let mut converted_index = 0usize;
    let mut binder_frames = Vec::new();
    for (semantic, runtime) in entry.head_stages.iter().zip(execution.head_stages()) {
        match (semantic, runtime) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {
                if !direct_newtype {
                    applied.push_str("()");
                }
            }
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let count = prepared_js_source_public_count(layout);
                let end = public_cursor + count;
                let stage_public = public_values.get(public_cursor..end).unwrap_or_else(|| {
                    unreachable!("prepared JavaScript wrapper public range drift")
                });
                let mut body_args = prepared_js_public_to_body_args(
                    site,
                    plan,
                    execution.root_uses(),
                    slots,
                    layout,
                    stage_public,
                    compacted_foralls,
                    &mut binder_frames,
                    0,
                )?;
                for body_arg in &mut body_args {
                    if body_arg != "null" && !is_legal_js_identifier(body_arg) {
                        let converted = format!("__c{converted_index}");
                        converted_index += 1;
                        wrappers.push_str(&format!("  const {converted} = {body_arg};\n"));
                        *body_arg = converted;
                    }
                }
                applied.push('(');
                applied.push_str(&body_args.join(", "));
                applied.push(')');
                public_cursor = end;
            }
            _ => {
                unreachable!("prepared JavaScript callable stage kind drift");
            }
        }
    }
    if public_cursor != public_values.len() {
        unreachable!("prepared JavaScript wrapper left public arguments unconsumed");
    }
    wrappers.push_str(&format!("  const __body__ = {applied};\n  return "));
    wrappers.push_str(&convert_prepared_js_use(
        site,
        plan,
        execution.root_uses(),
        entry.returned,
        "__body__",
        PreparedJsDir::Out,
        PreparedJsNominalContext::Standalone,
        compacted_foralls,
        &mut binder_frames,
        0,
    )?);
    wrappers.push_str(";\n}\n");
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

// =========================================================================
// FFI value conversion (JS-shape ↔ internal-rep at the boundary)
// =========================================================================
//
// Package-mode host/export/newtype boundaries are rendered from the prepared
// facade block above. The Type-based helpers below remain for standalone
// module lowering and focused representation tests. Both translate between
// the same two reps:
//
// - **Internal rep** is what module code sees: nested binary arrays
//   for products and sums (`[a, b]` for `(A & B)`, `[0, x]` /
//   `[1, x]` for `(A | B)`), and the underlying payload value for
//   ordinary newtypes whose iso-recursive wrap is type-erased.
//
// - **JS-shape** is what the host sees: a plain JS object whose
//   keys reflect the source-level structure.
//
// The conversion is per-signature (no runtime type table) — codegen
// inlines the spine walk at every wrapper / host-call site, knowing
// the type at every position. Atomic role-typed values, function
// values whose own legs need no conversion, unit, and bottom flow
// as JS-native and don't get wrapped.
//
// See `specs/backends/js.md` § Structural and nominal types for the
// shape contract; the rules below implement that contract.
//
// Direction. The shared [`FfiDir`] names the two sides of the
// boundary: `In` is JS-shape (host's hand) → internal-rep (Kio's hand),
// used at exported-fn parameter entry and at the return of host calls;
// `Out` is internal-rep → JS-shape, used at exported-fn return and at
// host-call args. The boundary-wrapper *walk* — the dispatch and the
// recursion this section's helpers slot into — is the shared
// [`crate::backends::skin::convert`] driver; the helpers below are the
// JS skin profile's leaf renderers.

use crate::backends::skin::{FfiDir, FunctionBoundaryAdapterPlan, SkinProfile};

/// The JS skin profile: the per-backend leaves of the boundary-wrapper
/// walk for the JS backend's value representation (nested binary arrays
/// internally, plain tagged objects at the host boundary).
///
/// Carries the resolved package so the leaf hooks can resolve newtype
/// references; the shared driver
/// ([`crate::backends::skin::convert`]) handles the dispatch and
/// each hook recurses back through [`SkinProfile::convert`].
struct JsSkin<'p> {
    package: Option<&'p crate::pass::resolve::Package<Routed>>,
    /// Nesting depth of the emitted `__slots` / `__sum` binding scopes.
    /// A nested product / sum conversion re-binds inside the enclosing
    /// IIFE, and a same-named inner `const` would shadow the enclosing
    /// binder its own initializer reads (a temporal-dead-zone
    /// `ReferenceError`, not a capture), so nested binders are minted
    /// depth-suffixed.
    bind_depth: std::cell::Cell<usize>,
}

impl<'p> JsSkin<'p> {
    fn new(package: Option<&'p crate::pass::resolve::Package<Routed>>) -> Self {
        JsSkin {
            package,
            bind_depth: std::cell::Cell::new(0),
        }
    }

    /// The binder name for `base` at the current nesting depth: bare at
    /// the top level (`__slots`), depth-suffixed when nested
    /// (`__slots1`, `__slots2`, …) so it never collides with an
    /// enclosing binder.
    fn depth_name(&self, base: &str) -> String {
        match self.bind_depth.get() {
            0 => base.to_string(),
            d => format!("{base}{d}"),
        }
    }

    /// Run `f` (a conversion that emits inside a just-opened binding
    /// scope) one binder-nesting level deeper.
    fn with_nested_binders<T>(&self, f: impl FnOnce() -> T) -> T {
        let depth = self.bind_depth.get();
        self.bind_depth.set(depth + 1);
        let result = f();
        self.bind_depth.set(depth);
        result
    }
}

fn bind_erased_type_stage(converted: String) -> String {
    format!("((__stage) => () => __stage)({converted})")
}

impl SkinProfile for JsSkin<'_> {
    type Err = EmitError;

    fn convert(&self, ty: &Type, expr: &str, dir: FfiDir) -> Result<String, EmitError> {
        if let Type::Forall { body, .. } = ty {
            return match dir {
                // The host ABI erases type binders, but the internal body
                // retains one nullary stage per binder. Bind before exposing
                // that stage so boundary effects keep call-by-value timing.
                FfiDir::In => Ok(bind_erased_type_stage(self.convert(body, expr, dir)?)),
                FfiDir::Out => self.convert(body, &format!("({expr})()"), dir),
            };
        }
        crate::backends::skin::convert(self, ty, expr, dir)
    }

    fn is_passthrough(&self, ty: &Type) -> bool {
        ffi_is_passthrough(ty, self.package)
    }

    fn convert_newtype(
        &self,
        ty: &Type,
        expr: &str,
        dir: FfiDir,
    ) -> Result<Option<String>, EmitError> {
        let (d, module_path) =
            resolve_newtype_with_module(ty, self.package).expect("ruled out by ffi_is_passthrough");
        if newtype_uses_hidden_host_carrier(d) {
            let identity = encoded_newtype_identity(&module_path, &d.name);
            let helper = match dir {
                FfiDir::Out => "__kioOpaqueOut",
                FfiDir::In => "__kioOpaqueIn",
            };
            return Ok(Some(format!("{helper}(\"{identity}\", {expr})")));
        }

        // A newtype with both members public has its structural boundary shape —
        // `{ <ffi_key>: payload }` on `Out`, unwrapped on `In`.
        // `is_passthrough` already ruled out recursive / comptime newtypes, so
        // a reference reaching this arm always resolves and always wraps.
        let info = newtype_key_info_for(ty, self.package).expect("resolved newtype has an FFI key");
        // The newtype's payload is the inner type; the iso-recursive
        // wrap is type-erased, so internal-rep is the payload itself.
        // Substitute the newtype's type-parameter args into its payload
        // to get the slot's actual payload type.
        let payload = instantiate_js_newtype_payload(d, ty, &module_path, self.package);
        let mut out = String::new();
        match dir {
            FfiDir::Out => {
                // internal -> {bare: convert_out(payload, expr)}
                out.push_str("{ ");
                out.push_str(&js_object_key(&info.bare));
                out.push_str(": ");
                out.push_str(&self.convert_newtype_payload(&payload, expr, FfiDir::Out)?);
                out.push_str(" }");
            }
            FfiDir::In => {
                // {bare: payload} -> convert_in(payload, expr.bare)
                let inner = js_member_access(expr, &info.bare);
                out.push_str(&self.convert_newtype_payload(&payload, &inner, FfiDir::In)?);
            }
        }
        Ok(Some(out))
    }

    fn convert_product(
        &self,
        _ty: &Type,
        slots: &[&Type],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let keys = assign_spine_keys(slots, self.package);
        let mut out = String::new();
        match dir {
            FfiDir::Out => self.emit_product_out(slots, &keys, expr, &mut out)?,
            FfiDir::In => self.emit_product_in(slots, &keys, expr, &mut out)?,
        }
        Ok(out)
    }

    fn convert_sum(
        &self,
        _ty: &Type,
        slots: &[&Type],
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let keys = assign_spine_keys(slots, self.package);
        let mut out = String::new();
        match dir {
            FfiDir::Out => self.emit_sum_out(slots, &keys, expr, &mut out)?,
            FfiDir::In => self.emit_sum_in(slots, &keys, expr, &mut out)?,
        }
        Ok(out)
    }

    fn convert_function(
        &self,
        param: &Type,
        ret: &Type,
        abi_arity: usize,
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let mut out = String::new();
        self.emit_function_convert(param, ret, abi_arity, expr, dir, &mut out)?;
        Ok(out)
    }

    fn convert_poly_fn_payload(
        &self,
        param: &Type,
        ret: &Type,
        plan: &FunctionBoundaryAdapterPlan<'_>,
        type_stages: usize,
        expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        self.emit_polymorphic_function_convert(param, ret, plan, type_stages, expr, dir)
    }
}

pub(crate) fn function_param_slots(param: &Type, abi_arity: usize) -> Vec<&Type> {
    Type::right_spine_take(param, abi_arity)
}

/// Bare and qualified host keys for a slot whose type resolves to an
/// ordinary newtype. The qualified key is `<modulepath>.<bare>`
/// against the declaring module.
pub(crate) struct NewtypeKeyInfo {
    pub(crate) bare: String,
    pub(crate) qualified: String,
}

/// Resolve a `Type::Path` reference to the newtype it names, plus
/// the module path that declared it. Returns `None` if the path
/// names anything else (alias, type variable, host type,
/// not-found, etc.).
pub(crate) fn resolve_newtype_with_module<'p>(
    t: &Type,
    package: Option<&'p crate::pass::resolve::Package<Routed>>,
) -> Option<(&'p Newtype, String)> {
    let pkg = package?;
    let segments = match t {
        Type::Path { segments, .. } => segments,
        _ => return None,
    };
    if segments.is_empty() {
        return None;
    }
    if segments.len() == 1 {
        // A bare nominal carries no declaring-module identity. Guessing by
        // package scan would let an unrelated same-leaf declaration change an
        // existing boundary conversion, violating open-world monotonicity.
        // Package-boundary paths use exact prepared QualifiedTypeName values;
        // this legacy Type helper therefore accepts qualified paths only.
        return None;
    }
    // Multi-segment: `<m...>.<TypeName>` — last segment names the
    // newtype, earlier segments form the module path.
    let last = segments.last().unwrap();
    let module_path = segments[..segments.len() - 1].join("/");
    let entry = pkg.module(&module_path)?;
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

/// Whether the public boundary must hide the newtype payload behind a
/// declaration-specific carrier. A structural payload object would expose
/// both directions: accepting it constructs the nominal and returning it
/// projects the payload. Therefore only a newtype with both members public
/// may use that structural representation.
pub(crate) fn newtype_uses_hidden_host_carrier(d: &Newtype) -> bool {
    matches!(
        d.host_surface(),
        Some(
            NewtypeHostSurface::Opaque
                | NewtypeHostSurface::Constructor { .. }
                | NewtypeHostSurface::Projector { .. }
        )
    )
}

/// Host-key information from a resolved newtype's ordinary FFI identity.
pub(crate) fn newtype_key_info_for(
    t: &Type,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> Option<NewtypeKeyInfo> {
    let (d, module_path) = resolve_newtype_with_module(t, package)?;
    let bare = d.ffi_key().to_owned();
    if bare.is_empty() {
        return None;
    }
    let qualified = format!("{module_path}.{bare}");
    Some(NewtypeKeyInfo { bare, qualified })
}

/// Compute the FFI key for each slot of a spine via the README's
/// 3-step fallback: bare label key → canonical qualified key →
/// positional `_<n>`. First non-colliding candidate wins.
///
/// JS keys every structural slot directly (no content hash), so it
/// uses the full canonical [`skin::three_step_candidates`] ordering
/// — its object keys accept the unmangled `<modulepath>.<F>`
/// spelling verbatim — and the shared
/// [`skin::assign_spine_keys`] driver does the collision resolution.
pub(crate) fn assign_spine_keys(
    slots: &[&Type],
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> Vec<String> {
    crate::backends::skin::assign_spine_keys(slots.len(), |i| {
        let resolved =
            newtype_key_info_for(slots[i], package).map(|info| (info.bare, info.qualified));
        crate::backends::skin::three_step_candidates(resolved, i)
    })
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

/// `true` if a `Type` carries no FFI-shape obligation — atomic role
/// types, type variables, function values whose legs need no
/// conversion, unit, and bottom all flow as JS-native and need no
/// conversion at the boundary.
///
/// `Type::Path` falls into this bucket when it's a type variable,
/// a host (`role`-tagged or otherwise) type, or an alias. Every resolved
/// newtype that reaches conversion produces either its FFI-keyed structural
/// wrapper or its declaration-specific hidden carrier.
///
/// `Type::Forall` is handled by recursing on the body while inserting or
/// consuming one hidden nullary runtime stage per binder, so this function
/// returns `false` for `Forall` and the caller performs that conversion.
/// Inline forall types in nested positions are unusual at the FFI
/// surface (you'd need a host item taking a polymorphic argument),
/// so `Forall` showing up here means "convert the body."
pub(crate) fn ffi_is_passthrough(
    t: &Type,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> bool {
    // Compile-time / intrinsic types (`__Type__`, `__Checked_term__`,
    // `__Comptime__`, `Comptime_bool`, …) never cross the JS FFI
    // boundary as a structured host value — an elaborator's reflected
    // term / type is opaque to the host. A structural type built over
    // them (e.g. `Dnf_branch | .`, a `Slot` product of `__Type__ &
    // __Checked_term__`) is therefore passthrough as a whole; converting
    // it would walk the elaborator suite's deep reflected-type spines for
    // no host-observable benefit.
    if type_involves_comptime(t, package, &mut BTreeSet::new(), JsReachContext::default()) {
        return true;
    }
    match t {
        Type::Unit { .. } | Type::Bottom { .. } => true,
        Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => function_ffi_is_passthrough(param, ret, *abi_arity, package),
        Type::Path { segments, .. } => {
            // Value-shaped single-segment paths cannot name newtypes.
            if segments.len() == 1 && crate::naming::is_value_name(&segments[0]) {
                return true;
            }
            // A resolved newtype normally wraps (`{<ffi_key>: payload}`),
            // so it is not passthrough. A member-hidden public newtype also
            // needs a conversion even when recursive: the conversion treats
            // it as an atomic private handle and never descends into the
            // hidden payload.
            // The remaining exception is a *recursive payload-visible*
            // newtype — one whose payload transitively names itself
            // (`Slot_list : . | (Slot & Slot_list)`). Its FFI wrap would
            // never terminate, and an iso-recursive value crosses the
            // boundary as its type-erased internal rep regardless, so it
            // is passthrough. Anything that doesn't resolve to a newtype
            // is a host type / alias / unresolved path — passthrough.
            match resolve_newtype_with_module(t, package) {
                None => true,
                Some((d, _)) if newtype_uses_hidden_host_carrier(d) => false,
                Some((d, module_path)) => newtype_is_recursive(d, &module_path, package),
            }
        }
        // Products and sums need spine-walking conversion.
        Type::Product { .. } | Type::Sum { .. } => false,
        // A forall needs a hidden nullary stage at the internal boundary.
        Type::Forall { .. } => false,
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn function_ffi_is_passthrough(
    param: &Type,
    ret: &Type,
    abi_arity: usize,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> bool {
    function_param_slots(param, abi_arity)
        .into_iter()
        .all(|p| ffi_is_passthrough(p, package))
        && ffi_is_passthrough(ret, package)
}

/// `true` if newtype `d` (declared in `module_path`) is recursive — its
/// payload, walked through products / sums / functions / forall and
/// other newtypes, transitively references `d` itself. Used to keep the
/// FFI-shape conversion (which descends into a newtype's payload) from
/// recursing without bound on iso-recursive data newtypes such as
/// `Type_list` / `Slot_list`.
fn newtype_is_recursive(
    d: &Newtype,
    module_path: &str,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> bool {
    let root = format!("{module_path}.{}", d.name);
    let mut visited = BTreeSet::new();
    visited.insert(root.clone());
    let payload = package.map_or_else(
        || d.payload.clone(),
        |package| {
            crate::backends::skin::qualify_newtype_declaration_payload(d, module_path, package)
        },
    );
    type_reaches_newtype(
        &payload,
        package,
        &root,
        &mut visited,
        JsReachContext {
            type_vars: d
                .type_params
                .iter()
                .chain(&d.existential_params)
                .map(|p| p.name.clone())
                .collect(),
        },
    )
}

/// `true` if `name` is a compile-time / intrinsic type name: an
/// `__…__` intrinsic (`__Type__`, `__Checked_term__`, `__Comptime__`,
/// `__Diagnostic_text__`, …) or one of the `Comptime_*` reflected
/// scalars. These are opaque at the JS FFI boundary.
fn is_comptime_type_name(name: &str) -> bool {
    (name.starts_with("__") && name.ends_with("__")) || name.starts_with("Comptime_")
}

/// `true` if `t` transitively mentions a compile-time / intrinsic type
/// — directly, through a structural product / sum / function / forall,
/// or through a newtype's payload. `visited` carries newtype keys
/// already entered so a recursive newtype terminates the walk.
fn type_involves_comptime(
    t: &Type,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    visited: &mut BTreeSet<String>,
    context: JsReachContext,
) -> bool {
    match t {
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            type_involves_comptime(left, package, visited, context.clone())
                || type_involves_comptime(right, package, visited, context)
        }
        Type::Function { param, ret, .. } => {
            type_involves_comptime(param, package, visited, context.clone())
                || type_involves_comptime(ret, package, visited, context)
        }
        Type::Forall { param, body, .. } => {
            let mut next = context;
            next.type_vars.insert(param.name.clone());
            type_involves_comptime(body, package, visited, next)
        }
        // `Bottom` here is the erased residue of a compile-time /
        // intrinsic type (`__Checked_term__`, `__Diagnostic_text__`, …)
        // — they lower to `Type::Bottom` at Routed. A structural shape
        // over them (`Elaborator_result = __Checked_term__ |
        // __Diagnostic_text__` lowers to `Sum { Bottom, Bottom }`) is an
        // opaque comptime value at the FFI boundary.
        Type::Bottom { .. } => true,
        Type::Path { segments, args, .. } => {
            let type_var_head = segments.len() == 1
                && segments
                    .first()
                    .is_some_and(|name| context.type_vars.contains(name.as_str()));
            if type_var_head {
                return args
                    .iter()
                    .any(|arg| type_involves_comptime(arg, package, visited, context.clone()));
            }
            if let Some((d, module_path)) = resolve_newtype_with_module(t, package) {
                // The payload is not part of a member-hidden declaration's
                // host contract. Treat the nominal as an atomic boundary leaf:
                // hidden compile-time-shaped payload details cannot turn an
                // enclosing host value into a passthrough value.
                if newtype_uses_hidden_host_carrier(d) {
                    return false;
                }
                let key = format!("{module_path}.{}", d.name);
                if !visited.insert(key.clone()) {
                    return false;
                }
                let found = type_involves_comptime(
                    &instantiate_js_newtype_payload(d, t, &module_path, package),
                    package,
                    visited,
                    js_newtype_reach_context(d, args, &context),
                );
                visited.remove(&key);
                return found;
            }
            if segments.len() == 1
                && let Some(last) = segments.last()
                && is_comptime_type_name(last.as_str())
            {
                return true;
            }
            args.iter()
                .any(|arg| type_involves_comptime(arg, package, visited, context.clone()))
        }
        Type::Unit { .. } => false,
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

#[derive(Clone, Default)]
struct JsReachContext {
    type_vars: BTreeSet<String>,
}

/// Walk `t`'s structure looking for a path back to the newtype keyed by
/// `root` (qualified `module.Name`). `visited` carries the newtype keys
/// already entered on this walk so a *different* recursive newtype in
/// the payload terminates instead of looping.
fn type_reaches_newtype(
    t: &Type,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    root: &str,
    visited: &mut BTreeSet<String>,
    context: JsReachContext,
) -> bool {
    match t {
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            type_reaches_newtype(left, package, root, visited, context.clone())
                || type_reaches_newtype(right, package, root, visited, context)
        }
        Type::Function { param, ret, .. } => {
            type_reaches_newtype(param, package, root, visited, context.clone())
                || type_reaches_newtype(ret, package, root, visited, context)
        }
        Type::Forall { param, body, .. } => {
            let mut next = context;
            next.type_vars.insert(param.name.clone());
            type_reaches_newtype(body, package, root, visited, next)
        }
        Type::Path { segments, args, .. } => {
            let type_var_head = segments.len() == 1
                && segments
                    .first()
                    .is_some_and(|name| context.type_vars.contains(name.as_str()));
            if !type_var_head
                && let Some((d, module_path)) = resolve_newtype_with_module(t, package)
            {
                // A hidden carrier is an atomic host-boundary leaf. Its
                // payload cannot make an enclosing payload-visible newtype
                // recursive, because boundary conversion never enters it.
                if newtype_uses_hidden_host_carrier(d) {
                    return false;
                }
                let key = format!("{module_path}.{}", d.name);
                if key == root {
                    return true;
                }
                if !visited.insert(key.clone()) {
                    return false;
                }
                let found = type_reaches_newtype(
                    &instantiate_js_newtype_payload(d, t, &module_path, package),
                    package,
                    root,
                    visited,
                    js_newtype_reach_context(d, args, &context),
                );
                visited.remove(&key);
                return found;
            }
            args.iter()
                .any(|arg| type_reaches_newtype(arg, package, root, visited, context.clone()))
        }
        Type::Unit { .. } | Type::Bottom { .. } => false,
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn js_newtype_reach_context(
    d: &Newtype,
    args: &[Type],
    context: &JsReachContext,
) -> JsReachContext {
    let mut type_vars: BTreeSet<String> = d.type_params.iter().map(|p| p.name.clone()).collect();
    type_vars.extend(crate::backends::skin::type_vars_referenced_in_args(
        args,
        &context.type_vars,
    ));
    JsReachContext { type_vars }
}

/// Emit a JS expression that converts `expr_in` (a JS expression
/// already lowered into `out`-suffix form, e.g. a parameter name or
/// a property access) according to `dir` and the structural shape
/// of `t`. Appends the converted expression to `out`.
///
/// The conversion is monomorphic — no runtime type tag inspection.
/// Polymorphic positions (`[A]`-quantified) are passthrough because
/// the host doesn't tell the package what `A` was instantiated to.
fn emit_ffi_convert(
    t: &Type,
    expr_in: &str,
    dir: FfiDir,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    out: &mut String,
) -> Result<(), EmitError> {
    let t = erase_scoped_type_vars(t, &BTreeSet::new());
    out.push_str(&JsSkin::new(package).convert(&t, expr_in, dir)?);
    Ok(())
}

/// Replace type-variable references that are lexically bound at an FFI
/// occurrence with an impossible path. Routed uses the same `Type::Path`
/// shape for type variables and nominal declarations, so the backend must
/// reconstruct this scope from signature / `forall` binders before doing
/// package-wide nominal lookup.
fn erase_scoped_type_vars(t: &Type, scope: &BTreeSet<String>) -> Type {
    match t {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            if let [name] = segments.as_slice()
                && scope.contains(name.as_str())
            {
                Type::synth_path(vec!["\0".to_owned()], Vec::new(), meta.span)
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
        Type::Unit { .. } | Type::Bottom { .. } => t.clone(),
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

/// Instantiate a newtype payload for JS boundary conversion and qualify its
/// nominal heads when the declaring package is available.
fn instantiate_js_newtype_payload(
    d: &Newtype,
    t_path: &Type,
    module_path: &str,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> Type {
    match package {
        Some(package) => {
            crate::backends::skin::qualify_boundary_newtype_payload(d, t_path, module_path, package)
        }
        None => crate::backends::skin::instantiate_boundary_newtype_payload(d, t_path),
    }
}

/// Emit `convert_out` for a product. Internal rep is right-nested
/// binary (`[a, [b, c]]` for 3 slots); JS shape is flat
/// keyed (`{k0: a, k1: b, k2: c}`).
/// Resolve the *value* type at a spine slot. When the slot is a
/// payload-visible newtype, its key has already been consumed as the spine
/// key, so the value at that key is its payload. A member-hidden newtype stays
/// an atomic handle at that key. Returns the payload type only in the first
/// case (with type-arg substitution); otherwise returns the slot unchanged.
///
/// This avoids double-wrapping: a `Product(Path(T1), Path(T2))`
/// where both resolve to newtypes would otherwise emit
/// `{t1: {t1: …}, t2: {t2: …}}`, which is wrong — the FFI shape
/// is `{t1: …, t2: …}` per `specs/backends/js.md`
/// § Structural and nominal types.
///
/// Shared with the TS skin's `render_ts_type`, so the `.d.ts` product
/// and sum types absorb each spine slot's key exactly as this JS body
/// does — the declaration file and the runtime bytes agree.
pub(crate) fn slot_value_type<'a>(
    slot: &'a Type,
    package: Option<&'a crate::pass::resolve::Package<Routed>>,
) -> std::borrow::Cow<'a, Type> {
    // A passthrough newtype (recursive — `Slot_list` — or comptime-payloaded
    // — `Type_list`) keeps its key's value opaque. A member-hidden newtype
    // likewise keeps its handle intact. Unwrapping a recursive payload here
    // would re-descend without end; unwrapping a hidden one would expose the
    // operation its declaration withheld.
    if !ffi_is_passthrough(slot, package)
        && let Some((d, module_path)) = resolve_newtype_with_module(slot, package)
        && !newtype_uses_hidden_host_carrier(d)
    {
        return std::borrow::Cow::Owned(instantiate_js_newtype_payload(
            d,
            slot,
            &module_path,
            package,
        ));
    }
    std::borrow::Cow::Borrowed(slot)
}

impl JsSkin<'_> {
    fn emit_polymorphic_function_convert(
        &self,
        _param: &Type,
        ret: &Type,
        plan: &FunctionBoundaryAdapterPlan<'_>,
        type_stages: usize,
        fn_expr: &str,
        dir: FfiDir,
    ) -> Result<String, EmitError> {
        let mut source = fn_expr.to_owned();
        if dir == FfiDir::Out {
            for _ in 0..type_stages {
                source = format!("({source})()");
            }
        }

        let outer_arity = match dir {
            FfiDir::Out => plan.boundary_arity(),
            FfiDir::In => plan.internal_arity(),
        };
        let params: Vec<String> = (0..outer_arity).map(|i| format!("__p{i}")).collect();
        let mut slot_binding = None;
        let call_args = match dir {
            FfiDir::Out => {
                let mut converted = Vec::with_capacity(plan.boundary_arity());
                for (ty, value) in plan.boundary_slots().iter().zip(&params) {
                    converted.push(self.convert(ty, value, FfiDir::In)?);
                }
                plan.boundary_to_internal_args(&converted, |left, right| {
                    format!("[{left}, {right}]")
                })
            }
            FfiDir::In => {
                let slots_name = self.depth_name("__poly_slots");
                let rebuilt = plan.internal_to_boundary_args(
                    &params,
                    |left, right| format!("[{left}, {right}]"),
                    |_value, index, _arity| format!("{slots_name}[{index}]"),
                );
                if plan.boundary_arity() > 1 {
                    let product =
                        plan.internal_product(&params, |left, right| format!("[{left}, {right}]"));
                    slot_binding = Some(format!(
                        "const {slots_name} = __kioProductSlots({product}, {}); ",
                        plan.boundary_arity()
                    ));
                }
                let mut converted = Vec::with_capacity(plan.boundary_arity());
                for (ty, value) in plan.boundary_slots().iter().zip(&rebuilt) {
                    converted.push(self.convert(ty, value, FfiDir::Out)?);
                }
                converted
            }
        };
        let call = format!("__inner({})", call_args.join(", "));
        let ret_conv = self.convert(ret, &call, dir)?;
        let body = match slot_binding {
            Some(binding) => format!("{{ {binding}return {ret_conv}; }}"),
            None => format!("({ret_conv})"),
        };
        let mut converted = format!("((__inner) => ({}) => {body})({source})", params.join(", "));
        if dir == FfiDir::In {
            for _ in 0..type_stages {
                converted = bind_erased_type_stage(converted);
            }
        }
        Ok(converted)
    }

    fn emit_function_convert(
        &self,
        param: &Type,
        ret: &Type,
        abi_arity: usize,
        fn_expr: &str,
        dir: FfiDir,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let param_tys = function_param_slots(param, abi_arity);
        let param_dir = dir.flip();

        out.push_str("((__inner) => (");
        for i in 0..param_tys.len() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("__p{i}"));
        }
        out.push_str(") => (");

        let mut call = String::new();
        call.push_str("__inner(");
        for (i, pty) in param_tys.iter().enumerate() {
            if i > 0 {
                call.push_str(", ");
            }
            let p = format!("__p{i}");
            call.push_str(&self.convert(pty, &p, param_dir)?);
        }
        call.push(')');
        out.push_str(&self.convert(ret, &call, dir)?);
        out.push(')');
        out.push_str(")(");
        out.push_str(fn_expr);
        out.push(')');
        Ok(())
    }

    fn emit_product_out(
        &self,
        slots: &[&Type],
        keys: &[String],
        v_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let slots_name = self.depth_name("__slots");
        out.push_str("(() => { const ");
        out.push_str(&slots_name);
        out.push_str(" = __kioProductSlots(");
        out.push_str(v_expr);
        out.push_str(&format!(", {}); return ", slots.len()));
        out.push_str("{ ");
        for (i, (slot, key)) in slots.iter().zip(keys.iter()).enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&js_object_key(key));
            out.push_str(": ");
            let access = format!("{slots_name}[{i}]");
            let value_type = slot_value_type(slot, self.package);
            let converted =
                self.with_nested_binders(|| self.convert(&value_type, &access, FfiDir::Out))?;
            out.push_str(&converted);
        }
        out.push_str(" }; })()");
        Ok(())
    }

    /// Emit `convert_in` for a product. JS shape is flat keyed; build
    /// internal right-nested binary.
    ///
    /// As with `emit_sum_in`, the spine walk repeats `s_expr` at every
    /// slot, so a side-effecting source is bound once via an IIFE
    /// before the multi-access dispatch.
    fn emit_product_in(
        &self,
        slots: &[&Type],
        keys: &[String],
        s_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        if js_expr_needs_binding(s_expr) {
            out.push_str("(() => { const __s = ");
            out.push_str(s_expr);
            out.push_str("; return ");
            self.emit_product_in_unchecked(slots, keys, "__s", out)?;
            out.push_str("; })()");
            Ok(())
        } else {
            self.emit_product_in_unchecked(slots, keys, s_expr, out)
        }
    }

    fn emit_product_in_unchecked(
        &self,
        slots: &[&Type],
        keys: &[String],
        s_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let n = slots.len();
        debug_assert!(n >= 2);
        out.push('[');
        let head_access = js_member_access(s_expr, &keys[0]);
        let head_type = slot_value_type(slots[0], self.package);
        out.push_str(&self.convert(&head_type, &head_access, FfiDir::In)?);
        out.push_str(", ");
        if n == 2 {
            let tail_access = js_member_access(s_expr, &keys[1]);
            let tail_type = slot_value_type(slots[1], self.package);
            out.push_str(&self.convert(&tail_type, &tail_access, FfiDir::In)?);
        } else {
            self.emit_product_in_unchecked(&slots[1..], &keys[1..], s_expr, out)?;
        }
        out.push(']');
        Ok(())
    }
}

/// Compute the JS access expression for slot `i` of an `n`-slot
/// right-nested binary product.
fn product_slot_access(v_expr: &str, i: usize, n: usize) -> String {
    format!("__kioProductSlot({v_expr}, {i}, {n})")
}

impl JsSkin<'_> {
    /// Emit `convert_out` for a sum. Internal rep is right-nested
    /// binary tagged (`[0, x]` for left of binary; `[1, [0, x]]` for
    /// middle of 3-way; `[1, [1, x]]` for right of 3-way). JS shape is
    /// an object with exactly one key set.
    ///
    /// As with `emit_sum_in`, the multi-access spine walk repeats
    /// `v_expr` per slot, so a side-effecting source is bound once
    /// via an IIFE before the dispatch.
    fn emit_sum_out(
        &self,
        slots: &[&Type],
        keys: &[String],
        v_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let n = slots.len();
        debug_assert!(n >= 2);
        let sum_name = self.depth_name("__sum");
        out.push_str("(() => { const ");
        out.push_str(&sum_name);
        out.push_str(" = __kioSumPayload(");
        out.push_str(v_expr);
        out.push_str(&format!(", {n}); switch ({sum_name}[0]) {{ "));
        let payload_access = format!("{sum_name}[1]");
        for (i, (slot, key)) in slots.iter().zip(keys.iter()).enumerate() {
            out.push_str(&format!("case {i}: return {{ "));
            out.push_str(&js_object_key(key));
            out.push_str(": ");
            let value_type = slot_value_type(slot, self.package);
            let converted = self
                .with_nested_binders(|| self.convert(&value_type, &payload_access, FfiDir::Out))?;
            out.push_str(&converted);
            out.push_str(" }; ");
        }
        out.push_str("default: throw new Error(\"sum payload out of bounds\"); } })()");
        Ok(())
    }

    /// Emit `convert_in` for a sum. JS shape has exactly one key set;
    /// build internal right-nested binary tagged form.
    ///
    /// The spine walk repeats `s_expr` at every slot — once for the
    /// `<key> in <s_expr>` discriminator check and once for each member
    /// access. When `s_expr` is a side-effecting JS expression (a host
    /// call that mutates / consumes, like `array_pop_back`), the naive
    /// inline form re-evaluates it per access and fires the effect
    /// `slots * 2` times. Bind the source to a fresh local via an IIFE
    /// when it isn't already a simple non-effecting form (identifier or
    /// dotted access).
    fn emit_sum_in(
        &self,
        slots: &[&Type],
        keys: &[String],
        s_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let n = slots.len();
        debug_assert!(n >= 2);
        if js_expr_needs_binding(s_expr) {
            out.push_str("(() => { const __s = ");
            out.push_str(s_expr);
            out.push_str("; return ");
            self.emit_sum_in_unchecked(slots, keys, "__s", out)?;
            out.push_str("; })()");
            Ok(())
        } else {
            self.emit_sum_in_unchecked(slots, keys, s_expr, out)
        }
    }

    fn emit_sum_in_unchecked(
        &self,
        slots: &[&Type],
        keys: &[String],
        s_expr: &str,
        out: &mut String,
    ) -> Result<(), EmitError> {
        let n = slots.len();
        debug_assert!(n >= 2);
        out.push('(');
        out.push_str(&format_in_check(s_expr, &keys[0]));
        out.push_str(" ? [0, ");
        let head_access = js_member_access(s_expr, &keys[0]);
        let head_type = slot_value_type(slots[0], self.package);
        out.push_str(&self.convert(&head_type, &head_access, FfiDir::In)?);
        out.push_str("] : [1, ");
        if n == 2 {
            let tail_access = js_member_access(s_expr, &keys[1]);
            let tail_type = slot_value_type(slots[1], self.package);
            out.push_str(&self.convert(&tail_type, &tail_access, FfiDir::In)?);
        } else {
            self.emit_sum_in_unchecked(&slots[1..], &keys[1..], s_expr, out)?;
        }
        out.push_str("])");
        Ok(())
    }
}

/// True iff `s` looks like a non-trivial JS expression that should
/// not be repeated verbatim across multi-access spine walks. A bare
/// identifier or a dotted access (`name`, `obj.field`,
/// `obj["key"]`) is side-effect-free and cheap to repeat; anything
/// containing a `(` (call) or operator could fire host effects or
/// re-do work each access. Used by `emit_sum_in` / `emit_sum_out` /
/// `emit_product_in` / `emit_product_out` to gate the IIFE bind.
fn js_expr_needs_binding(s: &str) -> bool {
    s.bytes()
        .any(|b| matches!(b, b'(' | b'?' | b'+' | b'-' | b'*' | b'/' | b'%' | b'='))
}

fn format_in_check(s_expr: &str, key: &str) -> String {
    format!("(\"{key}\" in {s_expr})")
}

// =========================================================================
// Lowering: declarations
// =========================================================================

fn lower_fn_def(d: &FnDef, top: &TopLevel, out: &mut String) -> Result<(), EmitError> {
    let runtime_groups = runtime_signature_groups(&d.sig);
    let outer_params = runtime_groups
        .first()
        .expect("runtime_signature_groups always returns at least one group")
        .param_names();
    let mangled_params = mangle_param_list(outer_params);
    out.push_str("function ");
    out.push_str(&mangle_js_ident(&d.name));
    out.push('(');
    for (i, n) in mangled_params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(n);
    }
    out.push_str(") {\n  return ");
    for group in runtime_groups.iter().skip(1) {
        let params = mangle_param_list(group.param_names());
        out.push('(');
        for (i, n) in params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(n);
        }
        out.push_str(") => ");
    }

    // Track in-scope locals (params + lets) so `Path` lowering can tell
    // a top-level callee from a local one.
    let mut locals: Vec<String> = runtime_groups
        .iter()
        .flat_map(|group| group.param_names().iter().map(|s| (*s).to_owned()))
        .collect();
    let mut local_tys = fn_signature_value_param_types(&d.sig);
    let mut body_js = String::new();
    lower_expr(&d.body, top, &mut locals, &mut local_tys, &mut body_js)?;
    body_js = adapt_js_fn_value_for_slot(body_js, &d.body, &d.ret, &mut local_tys);
    out.push_str(&body_js);
    out.push_str(";\n}\n");
    Ok(())
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
    if !groups
        .iter()
        .any(|group| matches!(group, JsRuntimeSignatureGroup::Value(_)))
    {
        groups.push(JsRuntimeSignatureGroup::Value(Vec::new()));
    }
    groups
}

fn value_group_prefix_count(sig: &Signature, arg_count: usize) -> Option<usize> {
    let groups = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            SignatureGroupRef::Value(params) => Some(value_param_names(params).len()),
            SignatureGroupRef::Type(_) => None,
        })
        .collect::<Vec<_>>();
    let total: usize = groups.iter().sum();
    let mut seen = 0usize;
    for (i, len) in groups.iter().enumerate() {
        seen += len;
        if seen == arg_count && seen < total {
            return Some(i + 1);
        }
    }
    None
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

fn fn_signature_value_param_types(sig: &Signature) -> HashMap<String, Type> {
    sig.params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(vp) => vp.ty.clone().map(|ty| (vp.name.clone(), ty)),
            SignatureParam::Type(_) => None,
        })
        .collect()
}

/// Whether two path segment vectors match for JS shape purposes: one
/// is a suffix of the other, compared by segment name. Equal lengths
/// reduce to full equality; a bare imported alias (`[Int]`) matches its
/// fully-qualified form (`[testapi, Int]`) because the bare name is a
/// one-segment suffix, while two same-leaf paths from different modules
/// (`[m1, Foo]` / `[m2, Foo]`) do not match. Used by
/// [`js_type_shape_equiv`].
fn segment_path_suffix_match(a: &[crate::ast::PathSegment], b: &[crate::ast::PathSegment]) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        // A path always has at least one segment; treat an empty vector
        // as matching only another empty one rather than as a suffix of
        // everything.
        return long.is_empty();
    }
    short
        .iter()
        .rev()
        .zip(long.iter().rev())
        .all(|(s, l)| s.name == l.name)
}

fn js_type_shape_equiv(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
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
            // A path matches when one segment vector is a suffix of the
            // other (equal lengths reduce to full equality). The same
            // type reaches two call sites spelled two ways — a bare
            // imported alias (`Int`, from `import testapi(Int);`) at a
            // host fn's own signature, and the fully-qualified
            // `testapi/Int` after a type-arg substitution — so a bare
            // `[Int]` must read as the same shape as the qualified
            // `[testapi, Int]`. The suffix test accepts that pair while
            // still distinguishing same-leaf types from different
            // modules (`[m1, Foo]` vs `[m2, Foo]` do not match), since a
            // bare alias is always a one-segment suffix of its qualified
            // form.
            segment_path_suffix_match(a_segments, b_segments)
                && a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args.iter())
                    .all(|(a, b)| js_type_shape_equiv(a, b))
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
        ) => js_type_shape_equiv(a_left, b_left) && js_type_shape_equiv(a_right, b_right),
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
        ) => js_type_shape_equiv(a_param, b_param) && js_type_shape_equiv(a_ret, b_ret),
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
        ) => a_param.name == b_param.name && js_type_shape_equiv(a_body, b_body),
        (Type::Infer { .. }, Type::Infer { .. }) => true,
        _ => false,
    }
}

fn js_runtime_representation_equiv(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
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
            segment_path_suffix_match(a_segments, b_segments)
                && a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args)
                    .all(|(a, b)| js_runtime_representation_equiv(a, b))
        }
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
            a_abi_arity == b_abi_arity
                && js_runtime_representation_equiv(a_param, b_param)
                && js_runtime_representation_equiv(a_ret, b_ret)
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
        ) => {
            js_runtime_representation_equiv(a_left, b_left)
                && js_runtime_representation_equiv(a_right, b_right)
        }
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
        ) => a_param.name == b_param.name && js_runtime_representation_equiv(a_body, b_body),
        (Type::Infer { .. }, Type::Infer { .. }) => true,
        _ => false,
    }
}

fn fn_expr_value_param_type_pairs(sig: &Signature, body: &Expr) -> Vec<(String, Option<Type>)> {
    let value_param_names: Vec<String> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(vp) => Some(vp.name.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect();
    let mut value_tys: Vec<Option<Type>> = sig
        .params
        .iter()
        .filter_map(|p| match p {
            SignatureParam::Value(vp) => Some(vp.ty.clone()),
            SignatureParam::Type(_) => None,
        })
        .collect();
    infer_fn_param_tys_from_body(&value_param_names, &mut value_tys, body);
    value_param_names.into_iter().zip(value_tys).collect()
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

/// Lower a `newtype` declaration to a JS namespace object whose
/// constructor and projector members are runtime-identity functions.
/// The iso-recursive wrap/unwrap is type-erased — the nominal
/// distinction between `Foo` and its payload is purely a compile-time
/// check, so at runtime there is nothing for these functions to do.
///
/// ```js
/// const Foo = {
///   mk_foo: (x) => x,
///   un_foo: (x) => x,
/// };
/// ```
///
/// **Existential-bearing newtypes** have a curried CPS projector at
/// the type level — `un_box(v)` returns a function that takes a
/// continuation. At runtime that's still an identity-flavored
/// operation: erasure drops the newtype wrap, then the inner CPS
/// thunk hands the payload value to the continuation. Emitted shape:
///
/// ```js
/// const Box = {
///   mk_box: (x) => x,
///   un_box: (x) => (k) => k(x),
/// };
/// ```
///
/// Newtype names begin with an uppercase letter, optionally after one
/// underscore, so they cannot collide with a JS reserved word and need no
/// mangling at the binding site. Member names (constructor / projector) live as
/// property keys after `.` — JS allows reserved words in member-key
/// position, so they pass through verbatim too.
fn lower_newtype(d: &Newtype, out: &mut String) {
    out.push_str("const ");
    out.push_str(&d.name);
    out.push_str(" = {\n  ");
    out.push_str(&d.constructor.name);
    out.push_str(": (x) => x,\n  ");
    out.push_str(&d.projector.name);
    if d.existential_params.is_empty() {
        out.push_str(": (x) => x,\n};\n");
    } else {
        // CPS projector: takes the newtyped value, returns a CPS
        // thunk that applies the continuation to the payload. The
        // thunk uses rest-args so the type-argument `[R]`, whether
        // erased by the kio-rs codegen or preserved through a
        // kio-prime round-trip as a value-shaped path, is harmlessly
        // absorbed — the continuation is always the last argument.
        let args = cps_payload_args(&d.payload, "x");
        out.push_str(": (x) => (...__cps_args__) => { const __k = __cps_args__[__cps_args__.length - 1]; return __k(");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(arg);
        }
        out.push_str("); },\n};\n");
    }
}

fn cps_payload_args(payload: &Type, value_expr: &str) -> Vec<String> {
    if matches!(payload, Type::Unit { .. }) {
        return Vec::new();
    }
    vec![value_expr.to_owned()]
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

fn js_expr_value_type(expr: &Expr, local_tys: &mut HashMap<String, Type>) -> Option<Type> {
    match expr {
        Expr::LowHostCall { ret_ty, .. } => Some(ret_ty.clone()),
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
        } => {
            js_returned_fn_arg_source_type(sig, type_args, args, ret_ty, local_tys).or_else(|| {
                Some(js_instantiated_call_return_type(
                    sig, type_args, args, ret_ty,
                ))
            })
        }
        Expr::LowBoundRef { name, .. } => local_tys.get(name).cloned(),
        Expr::Let {
            name, value, body, ..
        } => {
            let value_ty = js_expr_value_type(value, local_tys);
            let prior = value_ty
                .as_ref()
                .map(|ty| local_tys.insert(name.clone(), ty.clone()));
            let body_ty = js_expr_value_type(body, local_tys);
            if let Some(prior) = prior {
                match prior {
                    Some(ty) => {
                        local_tys.insert(name.clone(), ty);
                    }
                    None => {
                        local_tys.remove(name);
                    }
                }
            }
            body_ty
        }
        Expr::Seq { body, .. } => js_expr_value_type(body, local_tys),
        Expr::LowIndirectCall { callee, args, .. } if args.is_empty() => match callee.as_ref() {
            Expr::FnExpr {
                sig, body, ret_ty, ..
            } if !sig
                .params
                .iter()
                .any(|p| matches!(p, SignatureParam::Value(_))) =>
            {
                js_expr_value_type(body, local_tys).or_else(|| ret_ty.clone())
            }
            _ => None,
        },
        Expr::LowTypeApplication {
            callee, type_arg, ..
        } => {
            let Type::Forall { param, body, .. } = js_expr_value_type(callee, local_tys)? else {
                return None;
            };
            Some(crate::pass::typecheck_core::subst_type(
                &body,
                &HashMap::from([(param.name, type_arg.clone())]),
            ))
        }
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
                js_direct_enriched_slot_source_type(target, *index, *arity, local_tys)
                && js_type_is_function(&source_ty)
            {
                return Some(source_ty);
            }
            nth_product_slot_ty_for_arity(target_ty, *index, *arity).cloned()
        }
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            target_ty,
            ..
        } => {
            if let Some(source_ty) =
                js_direct_enriched_slot_source_type(target, *index, *arity, local_tys)
                && js_type_is_function(&source_ty)
            {
                return Some(source_ty);
            }
            nth_product_slot_ty_for_arity(target_ty, *index, *arity).cloned()
        }
        Expr::FnExpr {
            sig,
            body,
            ret_ty,
            meta,
            ..
        } => {
            let value_tys = fn_expr_value_param_type_pairs(sig, body);
            let mut typed_sig = sig.clone();
            let mut value_tys = value_tys.into_iter();
            for param in &mut typed_sig.params {
                if let SignatureParam::Value(value) = param {
                    value.ty = value_tys.next()?.1;
                    value.ty.as_ref()?;
                }
            }
            let ret = ret_ty.clone().unwrap_or_else(|| Type::Unit {
                meta: crate::ast::Meta::new(meta.span),
            });
            Some(typed_sig.signature_ty(ret, meta.span))
        }
        // A `host fn` in value position has function value type
        // `(p0 & p1 & …) -> ret` at its own abi-arity. The fn-adapter
        // wrap needs this to bridge a multi-arg host fn
        // (`string_concat(Str, Str) -> Str`, abi-arity 2) into a
        // product-domain slot (`(Str & Str) -> Str`, abi-arity 1):
        // it spreads the product into the host fn's separate args.
        // Without the source type the JS adapter would rebundle the
        // destructured product into one array argument, and the
        // host fn (a multi-arg arrow) would see the array as its
        // first param and `undefined` as the rest.
        Expr::LowHostFnValueRef {
            sig, ret_ty, meta, ..
        } => {
            let value_tys = sig
                .params
                .iter()
                .filter_map(|p| match p {
                    SignatureParam::Value(v) => Some(v.ty.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Type::synth_function(value_tys, ret_ty.clone(), meta.span))
        }
        _ => None,
    }
}

fn js_direct_enriched_slot_source_type(
    target: &Expr,
    index: usize,
    arity: usize,
    local_tys: &mut HashMap<String, Type>,
) -> Option<Type> {
    match target {
        Expr::EnrichedTuple { items, .. } if items.len() == arity && index < arity => {
            js_expr_value_type(&items[index], local_tys)
        }
        Expr::EnrichedRecord { fields, .. } if fields.len() == arity && index < arity => {
            js_expr_value_type(&fields[index].value, local_tys)
        }
        _ => None,
    }
}

fn js_type_is_function(ty: &Type) -> bool {
    matches!(js_forall_body(ty), Type::Function { .. })
}

fn js_returned_fn_arg_source_type(
    sig: &Signature,
    type_args: &[Type],
    args: &[Expr],
    ret_ty: &Type,
    local_tys: &mut HashMap<String, Type>,
) -> Option<Type> {
    let (type_args, args) = js_canonical_call_type_and_value_args(sig, type_args, args);
    let subst = js_typearg_subst_from_sig(sig, &type_args);
    let ret_ty = crate::pass::typecheck_core::subst_type(ret_ty, &subst);
    if !js_type_is_function(&ret_ty) {
        return None;
    }
    let mut value_idx = 0usize;
    for param in &sig.params {
        let SignatureParam::Value(value) = param else {
            continue;
        };
        let slot_ty = value
            .ty
            .as_ref()
            .map(|ty| crate::pass::typecheck_core::subst_type(ty, &subst));
        let arg = args.get(value_idx);
        value_idx += 1;
        let (Some(slot_ty), Some(arg)) = (slot_ty, arg) else {
            continue;
        };
        if js_type_shape_equiv(&slot_ty, &ret_ty)
            && let Some(source_ty) = js_expr_value_type(arg, local_tys)
                .or_else(|| js_fn_expr_source_type_from_target(arg, &slot_ty))
            && js_type_is_function(&source_ty)
        {
            return Some(source_ty);
        }
    }
    None
}

fn js_instantiated_call_return_type(
    sig: &Signature,
    type_args: &[Type],
    args: &[Expr],
    ret_ty: &Type,
) -> Type {
    let (type_args, _) = js_canonical_call_type_and_value_args(sig, type_args, args);
    let subst = js_typearg_subst_from_sig(sig, &type_args);
    crate::pass::typecheck_core::subst_type(ret_ty, &subst)
}

fn js_canonical_call_type_and_value_args(
    sig: &Signature,
    type_args: &[Type],
    args: &[Expr],
) -> (Vec<Type>, Vec<Expr>) {
    split_value_shaped_type_args_for_sig(sig, type_args, args)
        .unwrap_or_else(|| (type_args.to_vec(), args.to_vec()))
}

fn js_typearg_subst_from_sig(sig: &Signature, type_args: &[Type]) -> HashMap<String, Type> {
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

fn lower_expr(
    e: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
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
            let value_ty = js_expr_value_type(value, local_tys);
            if is_shadow {
                out.push_str("(() => { const __kio_let_init = ");
                lower_expr(value, top, locals, local_tys, out)?;
                out.push_str("; return (() => { const ");
                out.push_str(&mangle_js_ident(name));
                out.push_str(" = __kio_let_init; return ");
                locals.push(name.clone());
                let prior_ty = value_ty
                    .as_ref()
                    .map(|ty| local_tys.insert(name.clone(), ty.clone()));
                let body_result = lower_expr(body, top, locals, local_tys, out);
                if let Some(prior_ty) = prior_ty {
                    match prior_ty {
                        Some(ty) => {
                            local_tys.insert(name.clone(), ty);
                        }
                        None => {
                            local_tys.remove(name);
                        }
                    }
                }
                locals.pop();
                body_result?;
                out.push_str("; })(); })()");
            } else {
                out.push_str("(() => { const ");
                out.push_str(&mangle_js_ident(name));
                out.push_str(" = ");
                lower_expr(value, top, locals, local_tys, out)?;
                out.push_str("; return ");
                locals.push(name.clone());
                let prior_ty = value_ty
                    .as_ref()
                    .map(|ty| local_tys.insert(name.clone(), ty.clone()));
                let body_result = lower_expr(body, top, locals, local_tys, out);
                if let Some(prior_ty) = prior_ty {
                    match prior_ty {
                        Some(ty) => {
                            local_tys.insert(name.clone(), ty);
                        }
                        None => {
                            local_tys.remove(name);
                        }
                    }
                }
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
            lower_expr(value, top, locals, local_tys, out)?;
            out.push_str("; return ");
            lower_expr(body, top, locals, local_tys, out)?;
            out.push_str("; })()");
            Ok(())
        }
        Expr::FnExpr { sig, body, .. } => {
            // Wrap the arrow function in parentheses so an outer `Call`
            // applies to the arrow itself rather than to its body —
            // `((x) => x)(null)`, not the precedence-broken
            // `(x) => x(null)`.
            let runtime_groups = runtime_signature_groups(sig);
            out.push('(');
            for group in &runtime_groups {
                let mangled_params = mangle_param_list(group.param_names());
                out.push('(');
                for (i, n) in mangled_params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(n);
                }
                out.push_str(") => ");
            }
            let mark = locals.len();
            let mut prior_tys = Vec::new();
            for group in &runtime_groups {
                for n in group.param_names() {
                    locals.push((*n).to_owned());
                }
            }
            for (name, ty) in fn_expr_value_param_type_pairs(sig, body) {
                if let Some(ty) = ty {
                    prior_tys.push((name.clone(), local_tys.insert(name, ty)));
                }
            }
            let body_result = lower_expr(body, top, locals, local_tys, out);
            for (name, prior) in prior_tys.into_iter().rev() {
                match prior {
                    Some(ty) => {
                        local_tys.insert(name, ty);
                    }
                    None => {
                        local_tys.remove(&name);
                    }
                }
            }
            locals.truncate(mark);
            out.push(')');
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
        Expr::EnrichedTuple {
            items, synth_ty, ..
        } => lower_enriched_tuple(items, synth_ty, top, locals, local_tys, out),
        Expr::EnrichedProject {
            target,
            index,
            arity,
            ..
        } => lower_enriched_project(target, *index, *arity, top, locals, local_tys, out),
        Expr::EnrichedInject {
            payload,
            variant,
            variants,
            synth_ty,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            lower_enriched_inject(
                InjectEmit {
                    payload,
                    variant: *variant,
                    variants: *variants,
                    synth_ty,
                },
                &mut ctx,
                out,
            )
        }
        Expr::EnrichedMatch {
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            lower_enriched_match(
                MatchEmit {
                    scrutinee,
                    arms,
                    scrutinee_ty,
                    result_ty,
                },
                &mut ctx,
                out,
            )
        }
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            result_ty,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            lower_enriched_conditional(
                ConditionalEmit {
                    cond,
                    then_branch,
                    else_branch,
                    result_ty,
                },
                &mut ctx,
                out,
            )
        }
        // Records and named-field access lower to the same nested-
        // binary positional runtime rep as `EnrichedTuple` /
        // `EnrichedProject`. The named info in the IR is for targets
        // that have native records (e.g. Rust structs); JS doesn't, so
        // the per-signature FFI wrapper at the package boundary
        // surfaces the label keys (per `specs/backends/js.md`).
        Expr::EnrichedRecord {
            fields, synth_ty, ..
        } => lower_enriched_record(fields, synth_ty, top, locals, local_tys, out),
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            ..
        } => lower_enriched_project(target, *index, *arity, top, locals, local_tys, out),
        // Low-IR variants — produced by `recover_to_low::lower`. Each
        // variant dispatches to its per-variant emit helper; the
        // helpers below carry no classification logic, only the
        // syntactic templating for the JS surface.
        Expr::LowBoundRef { name, .. } => emit_bound_ref(name, out),
        Expr::LowHostFnValueRef {
            name,
            module_path,
            sig,
            ret_ty,
            ..
        } => emit_host_fn_value_ref(name, module_path, sig, ret_ty, top, out),
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
            ret_ty,
            ..
        } => emit_host_call(
            name,
            module_path,
            HostCallEmit {
                type_args,
                args,
                sig,
                ret_ty,
                top,
            },
            locals,
            local_tys,
            out,
        ),
        Expr::LowModuleCall {
            mangled,
            type_args,
            args,
            sig,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            emit_module_call(
                ModuleCallEmit {
                    mangled,
                    type_args,
                    args,
                    sig,
                },
                &mut ctx,
                out,
            )
        }
        Expr::LowQualifiedModuleCall {
            alias,
            mangled,
            type_args,
            args,
            sig,
            ..
        } => emit_qualified_module_call(
            alias, mangled, type_args, args, sig, top, locals, local_tys, out,
        ),
        Expr::LowQualifiedNewtypeMember {
            module_path,
            newtype,
            member,
            payload,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            emit_qualified_newtype_member(
                QualifiedNewtypeMemberEmit {
                    module_path,
                    newtype,
                    member,
                    payload,
                },
                &mut ctx,
                out,
            )
        }
        Expr::LowNewtypeCtor {
            newtype,
            member,
            payload,
            ..
        } => emit_newtype_member(newtype, member, payload, top, locals, local_tys, out),
        Expr::LowNewtypeProj {
            newtype,
            member,
            target,
            ..
        } => emit_newtype_member(newtype, member, target, top, locals, local_tys, out),
        Expr::LowClosureCall {
            name,
            type_args,
            args,
            ..
        } => emit_closure_call(name, type_args, args, top, locals, local_tys, out),
        Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } => emit_indirect_call(callee, type_args, args, top, locals, local_tys, out),
        Expr::LowTypeApplication { callee, .. } => {
            out.push('(');
            lower_expr(callee, top, locals, local_tys, out)?;
            out.push_str(")()");
            Ok(())
        }
        Expr::LowAbsurdCall { value_arg, .. } => {
            emit_absurd_call(value_arg, top, locals, local_tys, out)
        }
        // The variant's `type_args` is consumed by the Rust backend's
        // turbofish-aware emit; JS is type-erased, so the JS emitter
        // ignores it.
        Expr::LowCpsProjectorApply {
            newtype,
            module_path,
            receiver,
            continuation,
            continuation_ty,
            ..
        } => {
            let mut ctx = ExprLowerCtx {
                top,
                locals,
                local_tys,
            };
            emit_cps_projector_apply(
                newtype,
                module_path,
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
// All seven lower over the **same nested-binary runtime rep** the
// intrinsic chains use — `[a, b]` for products, `[tag, payload]` for
// sums — so the emitted code is observationally identical to the
// intrinsic chain it was recovered from. The win is structural: the
// per-`__either__` dispatch IIFE and the `__if_then_else__` thunk
// pair are gone, and a chain of intrinsic calls is one node.

struct ExprLowerCtx<'a, 'm> {
    top: &'a TopLevel<'m>,
    locals: &'a mut Vec<String>,
    local_tys: &'a mut HashMap<String, Type>,
}

impl ExprLowerCtx<'_, '_> {
    fn lower_expr(&mut self, expr: &Expr, out: &mut String) -> Result<(), EmitError> {
        lower_expr(expr, self.top, &mut *self.locals, &mut *self.local_tys, out)
    }

    fn adapt_fn_value_for_slot(
        &mut self,
        rendered: String,
        source: &Expr,
        target_ty: &Type,
    ) -> String {
        adapt_js_fn_value_for_slot(rendered, source, target_ty, &mut *self.local_tys)
    }
}

/// `EnrichedTuple([e0, …, e(n-1)])` → the right-nested array
/// `[e0, [e1, [ … e(n-1)]]]` — byte-identical to what the recovered
/// `__pair__` chain emitted.
fn lower_enriched_tuple(
    items: &[Expr],
    synth_ty: &Type,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    // The recovery guarantees `items.len() >= 2`.
    let n = items.len();
    let mut ctx = ExprLowerCtx {
        top,
        locals,
        local_tys,
    };
    let slots = Type::right_spine_product(synth_ty);
    let item_refs: Vec<&Expr> = items.iter().collect();
    if let Some(plan) = bound_product_rebuild_plan(&item_refs) {
        return lower_enriched_product_with_cached_slots(
            &item_refs,
            Some(&slots),
            &plan,
            &mut ctx,
            out,
        );
    }
    let mut rendered = Vec::with_capacity(n);
    for (index, item) in items.iter().enumerate() {
        rendered.push(render_enriched_product_item(
            ProductItemEmit {
                item,
                synth_ty,
                index,
                arity: n,
            },
            &mut ctx,
        )?);
    }
    lower_enriched_product_from_rendered(&rendered, out);
    Ok(())
}

/// `EnrichedRecord([f0, f1, …, fn-1])` → the same nested-binary
/// runtime array `[v0, [v1, [… [vn-2, vn-1] …]]]` as the
/// positional `EnrichedTuple`. The field names live in the IR for
/// backends with native records; JS surfaces them via the FFI
/// wrapper at the package boundary, not in the internal rep.
fn lower_enriched_record(
    fields: &[crate::ast::RecordField<crate::ast::Routed>],
    synth_ty: &Type,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    let n = fields.len();
    let mut ctx = ExprLowerCtx {
        top,
        locals,
        local_tys,
    };
    let slots = Type::right_spine_product(synth_ty);
    let item_refs: Vec<&Expr> = fields.iter().map(|field| &field.value).collect();
    if let Some(plan) = bound_product_rebuild_plan(&item_refs) {
        return lower_enriched_product_with_cached_slots(
            &item_refs,
            Some(&slots),
            &plan,
            &mut ctx,
            out,
        );
    }
    let mut rendered = Vec::with_capacity(n);
    for (index, f) in fields.iter().enumerate() {
        rendered.push(render_enriched_product_item(
            ProductItemEmit {
                item: &f.value,
                synth_ty,
                index,
                arity: n,
            },
            &mut ctx,
        )?);
    }
    lower_enriched_product_from_rendered(&rendered, out);
    Ok(())
}

struct ProductItemEmit<'a> {
    item: &'a Expr,
    synth_ty: &'a Type,
    index: usize,
    arity: usize,
}

fn render_enriched_product_item(
    item_emit: ProductItemEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
) -> Result<String, EmitError> {
    let ProductItemEmit {
        item,
        synth_ty,
        index,
        arity,
    } = item_emit;
    let mut rendered = String::new();
    ctx.lower_expr(item, &mut rendered)?;
    if let Some(slot_ty) = nth_product_slot_ty_for_arity(synth_ty, index, arity) {
        rendered = ctx.adapt_fn_value_for_slot(rendered, item, slot_ty);
    }
    Ok(rendered)
}

fn lower_enriched_product_from_rendered(rendered: &[String], out: &mut String) {
    let n = rendered.len();
    debug_assert!(n > 0);
    for item in &rendered[..n - 1] {
        out.push('[');
        out.push_str(item);
        out.push_str(", ");
    }
    out.push_str(&rendered[n - 1]);
    for _ in 0..n - 1 {
        out.push(']');
    }
}

fn lower_enriched_product_with_cached_slots(
    items: &[&Expr],
    slot_tys: Option<&[&Type]>,
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
            Ok(match slot_tys.and_then(|slots| slots.get(i)).copied() {
                Some(slot_ty) => ctx.adapt_fn_value_for_slot(v, item, slot_ty),
                None => v,
            })
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
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push_str("__kioProductSlot((");
    lower_expr(target, top, locals, local_tys, out)?;
    out.push_str(&format!("), {index}, {arity})"));
    Ok(())
}

/// `EnrichedInject(payload, variant, variants)` → helper-built
/// nested-binary tagged form.
struct InjectEmit<'a> {
    payload: &'a Expr,
    variant: usize,
    variants: usize,
    synth_ty: &'a Type,
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
        synth_ty,
    } = inject;
    let mut rendered = String::new();
    ctx.lower_expr(payload, &mut rendered)?;
    if let Some(slot_ty) = Type::right_spine_sum_slot_for_arity(synth_ty, variant, variants) {
        rendered = ctx.adapt_fn_value_for_slot(rendered, payload, slot_ty);
    }
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
    result_ty: &'a Type,
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
        result_ty,
    } = conditional;
    out.push('(');
    ctx.lower_expr(cond, out)?;
    out.push_str(" ? ");
    let mut then_rendered = String::new();
    ctx.lower_expr(then_branch, &mut then_rendered)?;
    then_rendered = ctx.adapt_fn_value_for_slot(then_rendered, then_branch, result_ty);
    out.push_str(&then_rendered);
    out.push_str(" : ");
    let mut else_rendered = String::new();
    ctx.lower_expr(else_branch, &mut else_rendered)?;
    else_rendered = ctx.adapt_fn_value_for_slot(else_rendered, else_branch, result_ty);
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
    scrutinee_ty: &'a Type,
    result_ty: &'a Type,
}

fn lower_enriched_match(
    match_emit: MatchEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let MatchEmit {
        scrutinee,
        arms,
        scrutinee_ty,
        result_ty,
    } = match_emit;
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
                payload_ty: Type::right_spine_sum_slot_for_arity(scrutinee_ty, k, n),
                result_ty,
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
    payload_ty: Option<&'a Type>,
    result_ty: &'a Type,
}

fn lower_enriched_match_arm(
    arm_emit: MatchArmEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let MatchArmEmit {
        arm,
        payload_expr,
        payload_ty,
        result_ty,
    } = arm_emit;
    out.push_str("((");
    out.push_str(&mangle_js_ident(&arm.param));
    out.push_str(") => ");
    ctx.locals.push(arm.param.clone());
    let prior_ty = payload_ty.map(|ty| ctx.local_tys.insert(arm.param.clone(), ty.clone()));
    let mut body = String::new();
    let body_result = ctx.lower_expr(&arm.body, &mut body);
    if body_result.is_ok() {
        body = ctx.adapt_fn_value_for_slot(body, &arm.body, result_ty);
    }
    if let Some(prior_ty) = prior_ty {
        match prior_ty {
            Some(ty) => {
                ctx.local_tys.insert(arm.param.clone(), ty);
            }
            None => {
                ctx.local_tys.remove(&arm.param);
            }
        }
    }
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

fn emit_prepared_host_fn_value_ref(
    name: &str,
    module_path: &str,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<String, EmitError> {
    let site = prepared_js_host_site(prepared, module_path, name)?;
    let execution = prepared_js_execution(site)?;
    let entry = site.plan().entry();
    if entry.head_stages.len() != execution.head_stages().len() {
        unreachable!("prepared JavaScript host value head alignment drift");
    }
    let plan = site.plan().facade();
    let compacted = BTreeSet::new();
    let mut binder_frames = Vec::new();
    let mut raw_index = 0usize;
    let mut public_args = Vec::new();
    let mut stages = Vec::with_capacity(entry.head_stages.len());
    for (semantic, runtime) in entry.head_stages.iter().zip(execution.head_stages()) {
        match (semantic, runtime) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {
                stages.push(Vec::new())
            }
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let raw = (0..layout.body_abi_arity())
                    .map(|_| {
                        let name = format!("__p{raw_index}");
                        raw_index += 1;
                        name
                    })
                    .collect::<Vec<_>>();
                public_args.extend(prepared_js_body_to_public_args(
                    site,
                    plan,
                    execution.root_uses(),
                    slots,
                    layout,
                    &raw,
                    &compacted,
                    &mut binder_frames,
                    0,
                )?);
                stages.push(raw);
            }
            _ => {
                unreachable!("prepared JavaScript host value stage kind drift");
            }
        }
    }
    let call = format!(
        "{}({})",
        host_fn_accessor(name, module_path),
        public_args.join(", ")
    );
    let mut body = convert_prepared_js_use(
        site,
        plan,
        execution.root_uses(),
        entry.returned,
        &call,
        PreparedJsDir::In,
        PreparedJsNominalContext::Standalone,
        &compacted,
        &mut binder_frames,
        0,
    )?;
    for params in stages.into_iter().rev() {
        body = format!("({}) => {body}", params.join(", "));
    }
    Ok(body)
}

fn emit_host_fn_value_ref(
    name: &str,
    module_path: &str,
    sig: &Signature,
    ret_ty: &Type,
    top: &TopLevel<'_>,
    out: &mut String,
) -> Result<(), EmitError> {
    // The package-complete prepared catalog contains exactly the public
    // bridge contract. A wholly internal, non-bridged host declaration may
    // still occur in an unreachable module body; preserving that module's
    // compilation is the open-world fallback below. Every bridged occurrence
    // must and does take this exact-site branch.
    if let Some(prepared) = top.prepared
        && has_prepared_js_host_site(prepared, module_path, name)?
    {
        out.push_str(&emit_prepared_host_fn_value_ref(
            name,
            module_path,
            prepared,
        )?);
        return Ok(());
    }
    let (sig, ret_ty) =
        qualify_host_boundary_signature_and_ret(sig, ret_ty, module_path, top.package);
    let runtime_groups = runtime_signature_groups(&sig);
    let value_param_types = signature_value_param_type_pairs(&sig, &[]);
    let ret_ty = signature_ffi_return_type(&sig, &ret_ty);
    let param_names: Vec<String> = (0..value_param_types.len())
        .map(|i| format!("__p{i}"))
        .collect();
    let call_str = {
        let mut buf = String::new();
        buf.push_str(&host_fn_accessor(name, module_path));
        buf.push('(');
        for (count, param_name) in param_names.iter().enumerate() {
            if count > 0 {
                buf.push_str(", ");
            }
            match value_param_types.get(count) {
                Some((Some(raw), Some(instantiated))) => {
                    emit_host_arg_convert(raw, instantiated, param_name, top.package, &mut buf)?
                }
                Some((Some(raw), None)) => {
                    emit_ffi_convert(raw, param_name, FfiDir::Out, top.package, &mut buf)?
                }
                Some((None, _)) | None => buf.push_str(param_name),
            }
        }
        buf.push(')');
        buf
    };
    let mut body = String::new();
    emit_ffi_convert(&ret_ty, &call_str, FfiDir::In, top.package, &mut body)?;

    // The host object keeps its documented value-only ABI. Internally, a
    // host-fn value has the same ordered stage shape as an ordinary Kio fn:
    // one nullary layer per type binder and one layer per value group. The
    // deepest layer forwards all accumulated value arguments to the host.
    let mut offset = 0usize;
    let mut stage_params = Vec::with_capacity(runtime_groups.len());
    for group in &runtime_groups {
        match group {
            JsRuntimeSignatureGroup::Type => stage_params.push(Vec::new()),
            JsRuntimeSignatureGroup::Value(names) => {
                let end = offset + names.len();
                stage_params.push(param_names[offset..end].to_vec());
                offset = end;
            }
        }
    }
    debug_assert_eq!(offset, param_names.len());
    for params in stage_params.into_iter().rev() {
        body = format!("({}) => {body}", params.join(", "));
    }
    out.push_str(&body);
    Ok(())
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

/// `LowHostCall { name, args, sig, ret_ty }` → host-fn call with FFI
/// conversion on each value-arg and on the return.
struct HostCallEmit<'a, 'm> {
    type_args: &'a [Type],
    args: &'a [Expr],
    sig: &'a Signature,
    ret_ty: &'a Type,
    top: &'a TopLevel<'m>,
}

fn emit_prepared_host_call(
    name: &str,
    module_path: &str,
    args: &[Expr],
    top: &TopLevel<'_>,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
) -> Result<String, EmitError> {
    let prepared = top
        .prepared
        .unwrap_or_else(|| unreachable!("prepared JavaScript host call has no package catalog"));
    let site = prepared_js_host_site(prepared, module_path, name)?;
    let execution = prepared_js_execution(site)?;
    let entry = site.plan().entry();
    if entry.head_stages.len() != execution.head_stages().len() {
        unreachable!("prepared JavaScript host call head alignment drift");
    }
    let mut rendered = Vec::with_capacity(args.len());
    for arg in args {
        let mut value = String::new();
        lower_expr(arg, top, locals, local_tys, &mut value)?;
        rendered.push(value);
    }
    let bound = (0..rendered.len())
        .map(|index| format!("__kioBoundaryArg{index}"))
        .collect::<Vec<_>>();
    let plan = site.plan().facade();
    let compacted = BTreeSet::new();
    let mut binder_frames = Vec::new();
    let mut raw_cursor = 0usize;
    let mut public_args = Vec::new();
    for (semantic, runtime) in entry.head_stages.iter().zip(execution.head_stages()) {
        match (semantic, runtime) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {}
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let end = raw_cursor + layout.body_abi_arity();
                let raw = bound.get(raw_cursor..end).unwrap_or_else(|| {
                    unreachable!("prepared JavaScript host-call argument range drift")
                });
                public_args.extend(prepared_js_body_to_public_args(
                    site,
                    plan,
                    execution.root_uses(),
                    slots,
                    layout,
                    raw,
                    &compacted,
                    &mut binder_frames,
                    0,
                )?);
                raw_cursor = end;
            }
            _ => {
                unreachable!("prepared JavaScript host-call stage kind drift");
            }
        }
    }
    if raw_cursor != rendered.len() {
        unreachable!(
            "prepared JavaScript host call expected {raw_cursor} value arguments, got {}",
            rendered.len()
        );
    }
    let call = format!(
        "{}({})",
        host_fn_accessor(name, module_path),
        public_args.join(", ")
    );
    let converted = convert_prepared_js_use(
        site,
        plan,
        execution.root_uses(),
        entry.returned,
        &call,
        PreparedJsDir::In,
        PreparedJsNominalContext::Standalone,
        &compacted,
        &mut binder_frames,
        0,
    )?;
    if rendered.is_empty() {
        Ok(converted)
    } else {
        Ok(format!(
            "(({}) => {converted})({})",
            bound.join(", "),
            rendered.join(", ")
        ))
    }
}

fn emit_host_call(
    name: &str,
    module_path: &str,
    call: HostCallEmit<'_, '_>,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    let HostCallEmit {
        type_args,
        args,
        sig,
        ret_ty,
        top,
    } = call;

    if let Some(prepared) = top.prepared
        && has_prepared_js_host_site(prepared, module_path, name)?
    {
        out.push_str(&emit_prepared_host_call(
            name,
            module_path,
            args,
            top,
            locals,
            local_tys,
        )?);
        return Ok(());
    }

    let (sig, ret_ty) =
        qualify_host_boundary_signature_and_ret(sig, ret_ty, module_path, top.package);

    // Pair each value-arg with its declared parameter type so the
    // FFI conversion can spine-walk per slot. The Routed phase has
    // already filtered type-args out of `args`, so the per-position
    // alignment is value-arg-only.
    let value_param_types = signature_value_param_type_pairs(&sig, type_args);

    // Pre-render each value arg into its own buffer; we need the
    // string form to feed `emit_ffi_convert` (whose spine walk may
    // bind the source to a local to avoid re-evaluating the arg).
    let mut arg_strs: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        let mut buf = String::new();
        lower_expr(a, top, locals, local_tys, &mut buf)?;
        arg_strs.push(buf);
    }

    // Build the host call body: `__host__.<MODULE_NS>.<name>(<conv arg0>, ...)`.
    let call_str = {
        let mut buf = String::new();
        buf.push_str(&host_fn_accessor(name, module_path));
        buf.push('(');
        for (count, arg_str) in arg_strs.iter().enumerate() {
            if count > 0 {
                buf.push_str(", ");
            }
            match value_param_types.get(count) {
                Some((Some(raw), Some(instantiated))) => {
                    emit_host_arg_convert(raw, instantiated, arg_str, top.package, &mut buf)?
                }
                Some((Some(raw), None)) => {
                    emit_ffi_convert(raw, arg_str, FfiDir::Out, top.package, &mut buf)?
                }
                Some((None, _)) | None => buf.push_str(arg_str),
            }
        }
        buf.push(')');
        buf
    };

    // FFI-convert the result back into internal-rep.
    let ret_ty = signature_ffi_return_type(&sig, &ret_ty);
    emit_ffi_convert(&ret_ty, &call_str, FfiDir::In, top.package, out)
}

fn qualify_host_boundary_signature_and_ret(
    sig: &Signature,
    ret: &Type,
    module_path: &str,
    package: Option<&crate::pass::resolve::Package<Routed>>,
) -> (Signature, Type) {
    let Some(module) = package.and_then(|package| package.module(module_path)) else {
        return (sig.clone(), ret.clone());
    };
    qualify_boundary_signature_and_ret(sig, ret, &module.module)
}

fn signature_value_param_type_pairs(
    sig: &crate::ast::Signature<Routed>,
    type_args: &[Type],
) -> Vec<(Option<Type>, Option<Type>)> {
    let mut subst = HashMap::new();
    let mut raw_scope = BTreeSet::new();
    let mut unresolved_scope = BTreeSet::new();
    let mut next_type_arg = 0usize;
    let mut pairs = Vec::new();
    for param in &sig.params {
        match param {
            crate::ast::SignatureParam::Type(tp) => {
                raw_scope.insert(tp.name.clone());
                if let Some(arg) = type_args.get(next_type_arg) {
                    subst.insert(tp.name.clone(), arg.clone());
                } else {
                    unresolved_scope.insert(tp.name.clone());
                }
                next_type_arg += 1;
            }
            crate::ast::SignatureParam::Value(v) => {
                let raw =
                    v.ty.as_ref()
                        .map(|ty| erase_scoped_type_vars(ty, &raw_scope));
                let instantiated = v.ty.as_ref().map(|ty| {
                    let ty = crate::pass::typecheck_core::subst_type(ty, &subst);
                    erase_scoped_type_vars(&ty, &unresolved_scope)
                });
                pairs.push((raw, instantiated));
            }
        }
    }
    pairs
}

/// A signature binder is an opaque slot in the host ABI even when a call site
/// supplies a concrete type argument. Preserve conversion for structure around
/// the slot, but never convert the slot itself as its call-site type.
fn signature_ffi_return_type(sig: &Signature, ret: &Type) -> Type {
    let mut binder_scope = BTreeSet::new();
    for param in &sig.params {
        if let SignatureParam::Type(param) = param {
            binder_scope.insert(param.name.clone());
        }
    }
    erase_scoped_type_vars(ret, &binder_scope)
}

fn type_arg_from_routed_expr(e: &Expr) -> Option<Type> {
    match e {
        Expr::LowBoundRef { name, meta, .. } => Some(Type::Path {
            segments: vec![crate::ast::PathSegment::new(name.clone(), meta.span)],
            args: Vec::new(),
            meta: meta.clone(),
        }),
        _ => None,
    }
}

fn split_value_shaped_type_args_for_sig(
    sig: &Signature,
    type_args: &[Type],
    args: &[Expr],
) -> Option<(Vec<Type>, Vec<Expr>)> {
    if !type_args.is_empty() || args.len() != sig.params.len() {
        return None;
    }
    let mut out_type_args = Vec::new();
    let mut out_value_args = Vec::new();
    for (param, arg) in sig.params.iter().zip(args.iter()) {
        match param {
            SignatureParam::Type(_) => out_type_args.push(type_arg_from_routed_expr(arg)?),
            SignatureParam::Value(_) => out_value_args.push(arg.clone()),
        }
    }
    Some((out_type_args, out_value_args))
}

fn emit_host_arg_convert(
    raw_ty: &Type,
    instantiated_ty: &Type,
    arg_expr: &str,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    out: &mut String,
) -> Result<(), EmitError> {
    match (raw_ty, instantiated_ty) {
        (
            Type::Function {
                param: raw_param,
                ret: raw_ret,
                abi_arity: raw_abi_arity,
                ..
            },
            Type::Function {
                param: instantiated_param,
                abi_arity: instantiated_abi_arity,
                ..
            },
        ) => emit_host_function_arg_convert(
            HostFunctionArgConvert {
                raw_param,
                raw_ret,
                raw_abi_arity: *raw_abi_arity,
                instantiated_param,
                instantiated_abi_arity: *instantiated_abi_arity,
                fn_expr: arg_expr,
            },
            package,
            out,
        ),
        _ => emit_ffi_convert(raw_ty, arg_expr, FfiDir::Out, package, out),
    }
}

struct HostFunctionArgConvert<'a> {
    raw_param: &'a Type,
    raw_ret: &'a Type,
    raw_abi_arity: usize,
    instantiated_param: &'a Type,
    instantiated_abi_arity: usize,
    fn_expr: &'a str,
}

fn emit_host_function_arg_convert(
    convert: HostFunctionArgConvert<'_>,
    package: Option<&crate::pass::resolve::Package<Routed>>,
    out: &mut String,
) -> Result<(), EmitError> {
    let HostFunctionArgConvert {
        raw_param,
        raw_ret,
        raw_abi_arity,
        instantiated_param,
        instantiated_abi_arity,
        fn_expr,
    } = convert;
    let raw_param_tys = function_param_slots(raw_param, raw_abi_arity);
    let instantiated_param_tys = function_param_slots(instantiated_param, instantiated_abi_arity);

    if raw_param_tys.len() != 1 || instantiated_param_tys.len() == 1 {
        return JsSkin::new(package).emit_function_convert(
            raw_param,
            raw_ret,
            raw_abi_arity,
            fn_expr,
            FfiDir::Out,
            out,
        );
    }

    out.push_str("((__inner) => (__p0) => (");
    let mut call = String::new();
    call.push_str("__inner(");
    if instantiated_param_tys.is_empty() {
        // Zero-arg internal callback.
    } else if matches!(instantiated_param, Type::Product { .. }) {
        for i in 0..instantiated_param_tys.len() {
            if i > 0 {
                call.push_str(", ");
            }
            call.push_str(&product_slot_access(
                "__p0",
                i,
                instantiated_param_tys.len(),
            ));
        }
    } else {
        call.push_str("__p0");
    }
    call.push(')');
    emit_ffi_convert(raw_ret, &call, FfiDir::Out, package, out)?;
    out.push(')');
    out.push_str(")(");
    out.push_str(fn_expr);
    out.push(')');
    Ok(())
}

/// Append one hidden nullary application for every erased type binder.
/// The thunks preserve System-F stage order even though the type value itself
/// has no JavaScript representation.
fn emit_erased_type_applications(type_arg_count: usize, out: &mut String) {
    for _ in 0..type_arg_count {
        out.push_str("()");
    }
}

/// `LowModuleCall { mangled, type_args, args }` → same-package module-fn
/// application. Cross-module imports route through `<NS>.<mangled>`;
/// same-module references emit the bare mangled identifier.
struct ModuleCallEmit<'a> {
    mangled: &'a str,
    type_args: &'a [Type],
    args: &'a [Expr],
    sig: &'a Signature,
}

fn emit_module_call(
    call: ModuleCallEmit<'_>,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    let ModuleCallEmit {
        mangled,
        type_args,
        args,
        sig,
    } = call;
    if let Some(ns) = ctx.top.cross_module_imports.get(mangled) {
        out.push_str(ns);
        out.push('.');
        out.push_str(&mangle_js_ident(mangled));
    } else {
        out.push_str(&mangle_js_ident(mangled));
    }
    emit_erased_type_applications(type_args.len(), out);
    emit_value_args_with_types(
        args,
        sig,
        type_args,
        ctx.top,
        &mut *ctx.locals,
        &mut *ctx.local_tys,
        out,
    )
}

/// `LowQualifiedModuleCall { alias, mangled, args }` →
/// `<NS>.<member>(<args>)` where `<NS>` is the module-namespace
/// identifier registered for `<alias>` and `<member>` is the bare
/// member name (the `mangled` field carries `<alias>.<member>` —
/// trim the alias prefix).
#[allow(clippy::too_many_arguments)]
fn emit_qualified_module_call(
    alias: &str,
    mangled: &str,
    type_args: &[Type],
    args: &[Expr],
    sig: &Signature,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    let member = mangled
        .strip_prefix(&format!("{alias}."))
        .unwrap_or(mangled);
    // A value-leaving call (type-args only) yields the curried fn value;
    // a partial call against a multi-value-group signature is handled by
    // `emit_value_args_with_types` — the plain `emit_value_args` would
    // drop both cases and emit a too-few-argument call, the same gap
    // `emit_module_call` avoids.
    if let Some(ns) = top.qualified_imports.get(alias) {
        out.push_str(ns);
        out.push('.');
        out.push_str(&mangle_js_ident(member));
        emit_erased_type_applications(type_args.len(), out);
        return emit_value_args_with_types(args, sig, type_args, top, locals, local_tys, out);
    }
    if top
        .package
        .is_some_and(|package| package.module(alias).is_some())
    {
        out.push_str(&module_namespace_from_key(alias));
        out.push('.');
        out.push_str(&mangle_js_ident(member));
        emit_erased_type_applications(type_args.len(), out);
        return emit_value_args_with_types(args, sig, type_args, top, locals, local_tys, out);
    }
    unreachable!("qualified call `{alias}.{member}` is absent from its resolved module-import map")
}

/// `LowQualifiedNewtypeMember { module_path, newtype, member, payload }` →
/// `<NS>.<newtype>.<member>(<payload>)`.
struct QualifiedNewtypeMemberEmit<'a> {
    module_path: &'a str,
    newtype: &'a str,
    member: &'a str,
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
        payload,
    } = member_emit;
    let ns = module_namespace_from_key(module_path);
    out.push_str(&ns);
    out.push('.');
    out.push_str(newtype);
    out.push('.');
    out.push_str(member);
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
    payload: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    if let Some(ns) = top.cross_module_newtypes.get(newtype) {
        out.push_str(ns);
        out.push('.');
    }
    out.push_str(newtype);
    out.push('.');
    out.push_str(member);
    out.push('(');
    lower_expr(payload, top, locals, local_tys, out)?;
    out.push(')');
    Ok(())
}

/// `LowClosureCall { name, args }` → `<mangled name>(<args>)`. The
/// callee is a bound local of fn type — emit it as a bare reference.
fn emit_closure_call(
    name: &str,
    type_args: &[Type],
    args: &[Expr],
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push_str(&mangle_js_ident(name));
    emit_erased_type_applications(type_args.len(), out);
    emit_value_args(args, top, locals, local_tys, out)
}

/// `LowIndirectCall { callee, args }` → `(<callee>)(<args>)`. The
/// callee is itself an expression (an arrow fn, an inner call's
/// result, …); lower it inline, parenthesized. The parens are
/// load-bearing when the callee renders as a bare arrow function
/// (e.g. a host-fn value reference `(p0, p1) => __host__…(p0, p1)`):
/// without them the JS grammar binds the call-args to the innermost
/// call inside the arrow body (`f(p0, p1)(args)`) rather than applying
/// the arrow itself (`((p0, p1) => f(p0, p1))(args)`), which silently
/// returns the inner closure instead of the call result. Wrapping is a
/// no-op for the identifier / member-access / call callees that make up
/// the rest of the corpus.
fn emit_indirect_call(
    callee: &Expr,
    type_args: &[Type],
    args: &[Expr],
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push('(');
    lower_expr(callee, top, locals, local_tys, out)?;
    out.push(')');
    emit_erased_type_applications(type_args.len(), out);
    // When the callee is an arrow whose value-params carry types, adapt
    // each arg to its param slot. A positional multi-param host fn value
    // (`string_concat`, abi-arity 2) passed where the param is a
    // product-domain arrow `(A & B) -> R` (abi-arity 1) must be spread
    // through `adapt_js_fn_value_for_slot`, which keys off the host-fn
    // value-ref's own arity-2 source type. Binding it raw stores the
    // arity-2 value under the arity-1 param, and the later
    // product-domain call rebundles the destructured product into one
    // array argument — the host fn then sees the array as `p0` and
    // `undefined` as the rest. The slots are read from the callee arrow
    // itself, so the matched-arity majority is unaffected.
    if let Expr::FnExpr { sig, .. } = callee {
        let slots: Vec<Option<Type>> = sig
            .params
            .iter()
            .filter_map(|p| match p {
                SignatureParam::Value(v) => Some(v.ty.clone()),
                _ => None,
            })
            .collect();
        if slots.len() == args.len() && slots.iter().any(|s| s.is_some()) {
            out.push('(');
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let mut rendered = String::new();
                lower_expr(a, top, locals, local_tys, &mut rendered)?;
                if let Some(Some(slot)) = slots.get(i) {
                    rendered = adapt_js_fn_value_for_slot(rendered, a, slot, local_tys);
                }
                out.push_str(&rendered);
            }
            out.push(')');
            return Ok(());
        }
    }
    emit_value_args(args, top, locals, local_tys, out)
}

/// `LowAbsurdCall { value_arg }` → an IIFE that throws on the bottom
/// value. Per spec, a value of `!` can only ever come from a host fn
/// declared `-> !`, which in practice never returns; the throw is
/// belt-and-braces if a misbehaving host violates that.
fn emit_absurd_call(
    value_arg: &Expr,
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push_str("((__bottom__) => { throw new Error('Kio: __absurd__ on bottom value'); })(");
    lower_expr(value_arg, top, locals, local_tys, out)?;
    out.push(')');
    Ok(())
}

/// `LowCpsProjectorApply { newtype, module_path, receiver, continuation,
/// continuation_ty }` evaluates the receiver and continuation in that order,
/// advances the continuation's erased type stages, then invokes
/// `<NewtypeNS>.<member>(<receiver>)(<continuation>)`, where `<member>` is the
/// newtype's declared projector name. Minted by
/// `recover_to_low::lower` from the `LowIndirectCall { callee:
/// LowNewtypeProj, … }` shape when the newtype is existential-bearing.
/// JS is type-erased, so the node's universal newtype instantiation affects
/// only `continuation_ty`; it emits no runtime type value.
fn emit_cps_projector_apply(
    newtype: &str,
    module_path: &str,
    receiver: &Expr,
    continuation: &Expr,
    continuation_ty: &Type,
    ctx: &mut ExprLowerCtx<'_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    // This lookup supplies only the emitted projector member spelling. The
    // continuation's stages and ABI come exclusively from `continuation_ty`.
    let declaration = cps_projector_newtype(newtype, module_path, ctx.top)?;
    let mut receiver_js = String::new();
    ctx.lower_expr(receiver, &mut receiver_js)?;
    let mut continuation_js = String::new();
    ctx.lower_expr(continuation, &mut continuation_js)?;
    let (type_stages, callable) = continuation_ty.peel_leading_foralls();
    let Type::Function { abi_arity, .. } = callable else {
        unreachable!("a routed CPS projector continuation has a function type")
    };
    assert!(
        *abi_arity <= 1,
        "a routed CPS projector continuation has zero or one ABI slot"
    );

    // Each generated binding lives in this private IIFE. User identifiers
    // cannot begin with `__`, so the fixed names are outside the source
    // namespace; nested CPS applications open distinct scopes.
    out.push_str("(() => { const __kio_cps_receiver = ");
    out.push_str(&receiver_js);
    out.push_str("; const __kio_cps_continuation = ");
    out.push_str(&continuation_js);
    out.push_str("; const __kio_cps_ready = ");
    out.push_str(&advance_erased_type_stages(
        "__kio_cps_continuation".to_owned(),
        type_stages,
    ));
    out.push_str("; return ");
    if module_path != ctx.top.module_key {
        out.push_str(&module_namespace_from_key(module_path));
        out.push('.');
    }
    out.push_str(newtype);
    out.push('.');
    out.push_str(&declaration.projector.name);
    out.push_str("(__kio_cps_receiver)(__kio_cps_ready); })()");
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
        advanced.push_str("()");
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

/// Helper: emit a parenthesised, comma-separated list of value
/// arguments. The Routed phase has already separated type-args from
/// value-args on every `Low*` call variant, so no type-arg dropping
/// happens here.
fn emit_value_args(
    args: &[Expr],
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    out.push('(');
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        lower_expr(a, top, locals, local_tys, out)?;
    }
    out.push(')');
    Ok(())
}

fn emit_value_args_with_types(
    args: &[Expr],
    sig: &Signature,
    type_args: &[Type],
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    let reclassified = split_value_shaped_type_args_for_sig(sig, type_args, args);
    let (owned_type_args, owned_args);
    let (type_args, args) = if let Some((tys, vals)) = reclassified {
        owned_type_args = tys;
        owned_args = vals;
        (owned_type_args.as_slice(), owned_args.as_slice())
    } else {
        (type_args, args)
    };
    let value_param_types = signature_value_param_type_pairs(sig, type_args);
    if args.len() != value_param_types.len() && value_group_prefix_count(sig, args.len()).is_none()
    {
        let target_slots: Option<Vec<Type>> = value_param_types
            .iter()
            .map(|(_, instantiated)| instantiated.clone())
            .collect();
        if let Some(target_slots) = target_slots {
            return emit_args_for_function_slots(args, &target_slots, top, locals, local_tys, out);
        }
    }
    out.push('(');
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let mut rendered = String::new();
        lower_expr(a, top, locals, local_tys, &mut rendered)?;
        if let Some((_, Some(expected))) = value_param_types.get(i) {
            rendered = adapt_js_fn_value_for_slot(rendered, a, expected, local_tys);
        }
        out.push_str(&rendered);
    }
    out.push(')');
    Ok(())
}

fn emit_args_for_function_slots(
    args: &[Expr],
    target_slots: &[Type],
    top: &TopLevel,
    locals: &mut Vec<String>,
    local_tys: &mut HashMap<String, Type>,
    out: &mut String,
) -> Result<(), EmitError> {
    let mut emitted = Vec::with_capacity(target_slots.len());
    let mut cursor = 0usize;
    for slot in target_slots {
        let normalized = slot.clone();
        if matches!(normalized, Type::Product { .. }) {
            let width = Type::right_spine_product(&normalized).len();
            if width > 1 && cursor + width <= args.len() {
                let mut rendered = String::new();
                lower_enriched_tuple(
                    &args[cursor..cursor + width],
                    &normalized,
                    top,
                    locals,
                    local_tys,
                    &mut rendered,
                )?;
                emitted.push(rendered);
                cursor += width;
                continue;
            }
        }
        let Some(arg) = args.get(cursor) else {
            unreachable!("function call has fewer value arguments than its resolved target slots");
        };
        let mut rendered = String::new();
        lower_expr(arg, top, locals, local_tys, &mut rendered)?;
        rendered = adapt_js_fn_value_for_slot(rendered, arg, slot, local_tys);
        emitted.push(rendered);
        cursor += 1;
    }
    if cursor != args.len() {
        unreachable!("function call has more value arguments than its resolved target slots");
    }
    out.push('(');
    out.push_str(&emitted.join(", "));
    out.push(')');
    Ok(())
}

fn adapt_js_fn_value_for_slot(
    rendered: String,
    source: &Expr,
    target_ty: &Type,
    local_tys: &mut HashMap<String, Type>,
) -> String {
    let (target_type_stages, target_ty) = target_ty.peel_leading_foralls();
    let Type::Function {
        param, abi_arity, ..
    } = target_ty
    else {
        return rendered;
    };
    let target_slots = function_param_slots(param, *abi_arity);
    let target_flat: Vec<(String, Type)> = target_slots
        .iter()
        .enumerate()
        .map(|(i, ty)| (format!("__a{i}"), (*ty).clone()))
        .collect();
    let Some(source_ty) = js_expr_value_type(source, local_tys)
        .or_else(|| js_fn_expr_source_type_from_target(source, target_ty))
    else {
        return rendered;
    };
    let (source_type_stages, source_ty) = source_ty.peel_leading_foralls();
    if source_type_stages != target_type_stages {
        return rendered;
    }
    // Rebuilding an already-identical product parameter is not a no-op for a
    // function value: it composes another closure each time the value moves
    // through a recursive state slot.
    if js_runtime_representation_equiv(source_ty, target_ty) {
        return rendered;
    }
    let Some(call_args) = js_call_args_from_source_fn_to_target_slots(source_ty, &target_flat)
    else {
        return rendered;
    };
    let direct = target_flat.len() == call_args.len()
        && target_flat
            .iter()
            .zip(call_args.iter())
            .all(|((name, _), arg)| name == arg);
    if direct {
        return rendered;
    }
    let binders = target_flat
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut source = "__inner".to_owned();
    for _ in 0..source_type_stages {
        source = format!("({source})()");
    }
    let mut adapter = format!("({binders}) => ({source})({})", call_args.join(", "));
    for _ in 0..target_type_stages {
        adapter = format!("() => {adapter}");
    }
    format!("((__inner) => {adapter})({rendered})")
}

fn js_fn_expr_source_type_from_target(source: &Expr, target: &Type) -> Option<Type> {
    let Expr::FnExpr {
        sig, ret_ty, meta, ..
    } = source
    else {
        return None;
    };
    let value_count = sig
        .params
        .iter()
        .filter(|p| matches!(p, SignatureParam::Value(_)))
        .count();
    let target = js_forall_body(target);
    let Type::Function { param, ret, .. } = target else {
        return None;
    };
    let params = match value_count {
        0 => Vec::new(),
        1 => vec![param.as_ref().clone()],
        _ => {
            let leaves = Type::right_spine_take(param, value_count);
            if leaves.len() != value_count {
                return None;
            }
            leaves.into_iter().cloned().collect()
        }
    };
    let ret = ret_ty.as_ref().cloned().unwrap_or((**ret).clone());
    let mut typed_sig = sig.clone();
    let mut params = params.into_iter();
    for param in &mut typed_sig.params {
        if let SignatureParam::Value(value) = param {
            value.ty = Some(params.next()?);
        }
    }
    Some(typed_sig.signature_ty(ret, meta.span))
}

fn js_forall_body(mut ty: &Type) -> &Type {
    while let Type::Forall { body, .. } = ty {
        ty = body;
    }
    ty
}

fn js_call_args_from_source_fn_to_target_slots(
    source_ty: &Type,
    target_flat: &[(String, Type)],
) -> Option<Vec<String>> {
    let Type::Function {
        param, abi_arity, ..
    } = source_ty
    else {
        return None;
    };
    let source_slots = function_param_slots(param, *abi_arity);
    if source_slots.is_empty()
        && target_flat
            .iter()
            .all(|(_, ty)| matches!(ty, Type::Unit { .. }))
    {
        return Some(Vec::new());
    }
    let target_flat = js_expand_target_slots_for_source_call(target_flat);
    let mut cursor = 0usize;
    let mut call_args = Vec::with_capacity(source_slots.len());
    for slot in &source_slots {
        let arg = js_source_arg_from_target_slots(&target_flat, &mut cursor, slot)?;
        call_args.push(arg);
    }
    (cursor == target_flat.len()).then_some(call_args)
}

fn js_expand_target_slots_for_source_call(target_flat: &[(String, Type)]) -> Vec<(String, Type)> {
    let mut out = Vec::new();
    for (name, ty) in target_flat {
        js_expand_target_slot_for_source_call(name, ty, &mut out);
    }
    out
}

fn js_expand_target_slot_for_source_call(value: &str, ty: &Type, out: &mut Vec<(String, Type)>) {
    let Type::Product { left, right, .. } = ty else {
        out.push((value.to_owned(), ty.clone()));
        return;
    };
    let left_value = format!("({value})[0]");
    let right_value = format!("({value})[1]");
    js_expand_target_slot_for_source_call(&left_value, left, out);
    js_expand_target_slot_for_source_call(&right_value, right, out);
}

fn js_source_arg_from_target_slots(
    target_flat: &[(String, Type)],
    cursor: &mut usize,
    source_slot: &Type,
) -> Option<String> {
    if let Some((name, ty)) = target_flat.get(*cursor)
        && js_type_shape_equiv(ty, source_slot)
    {
        *cursor += 1;
        return Some(name.clone());
    }
    let start = *cursor;
    let Type::Product { left, right, .. } = source_slot else {
        return None;
    };
    let left_value = js_source_arg_from_target_slots(target_flat, cursor, left)?;
    let Some(right_value) = js_source_arg_from_target_slots(target_flat, cursor, right) else {
        *cursor = start;
        return None;
    };
    Some(format!("[{left_value}, {right_value}]"))
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::pass::parser::parse;
    use std::path::{Path, PathBuf};

    #[test]
    fn qualified_public_keys_case_components_and_keep_canonical_module_paths() {
        let key = SemanticKey::Qualified {
            module_segments: vec!["word_api".to_owned(), "other_nodes".to_owned()],
            name: "_Word_box__".to_owned(),
        };
        assert_eq!(semantic_js_key(&key), "wordApi/otherNodes._WordBox__");
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

    fn segs(names: &[&str]) -> Vec<crate::ast::PathSegment> {
        names
            .iter()
            .map(|n| crate::ast::PathSegment {
                name: (*n).to_owned(),
                span: crate::span::Span::new(0, 0),
            })
            .collect()
    }

    #[test]
    fn segment_path_suffix_match_accepts_bare_alias_vs_qualified() {
        // A host fn's bare imported `Int` and the qualified `testapi/Int`
        // it resolves to after substitution must read as one shape (the
        // fn-value adapter relies on this — see the `exec_host_*` fold
        // goldens). Order-independent.
        assert!(segment_path_suffix_match(
            &segs(&["Int"]),
            &segs(&["testapi", "Int"])
        ));
        assert!(segment_path_suffix_match(
            &segs(&["testapi", "Int"]),
            &segs(&["Int"])
        ));
    }

    #[test]
    fn segment_path_suffix_match_rejects_cross_module_same_leaf() {
        // Two `Foo`s from different modules are distinct types and must
        // not collapse to one shape — the suffix test stops at the
        // diverging module segment.
        assert!(!segment_path_suffix_match(
            &segs(&["m1", "Foo"]),
            &segs(&["m2", "Foo"])
        ));
    }

    #[test]
    fn segment_path_suffix_match_full_equality_and_mismatch() {
        assert!(segment_path_suffix_match(
            &segs(&["testapi", "Int"]),
            &segs(&["testapi", "Int"])
        ));
        assert!(!segment_path_suffix_match(&segs(&["Int"]), &segs(&["Str"])));
    }

    fn leaf_type(name: &str) -> Type {
        let span = crate::span::Span::new(0, 0);
        Type::synth_path(vec![name.to_owned()], Vec::new(), span)
    }

    fn product_type(left: Type, right: Type) -> Type {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: crate::ast::Meta::new(crate::span::Span::new(0, 0)),
        }
    }

    fn callable_type(param: Type, abi_arity: usize) -> Type {
        let span = crate::span::Span::new(0, 0);
        Type::Function {
            param: Box::new(param),
            ret: Box::new(leaf_type("R")),
            meta: crate::ast::Meta::new(span),
            abi_arity,
            caps: Default::default(),
        }
    }

    #[test]
    fn equal_product_callable_slot_does_not_synthesize_adapter() {
        let span = crate::span::Span::new(0, 0);
        let continuation_ty = callable_type(product_type(leaf_type("A"), leaf_type("B")), 1);
        let continuation = Expr::LowBoundRef {
            occurrence: Default::default(),
            name: "continuation".to_owned(),
            meta: crate::ast::Meta::new(span),
            ext: (),
        };
        let mut local_tys = HashMap::from([("continuation".to_owned(), continuation_ty.clone())]);

        assert_eq!(
            adapt_js_fn_value_for_slot(
                "continuation".to_owned(),
                &continuation,
                &continuation_ty,
                &mut local_tys,
            ),
            "continuation"
        );
    }

    #[test]
    fn product_callable_slot_representation_checks_nested_callable_arity() {
        let nested_param = product_type(leaf_type("A"), leaf_type("B"));
        let source_ty = callable_type(
            product_type(callable_type(nested_param.clone(), 2), leaf_type("C")),
            1,
        );
        let target_ty = callable_type(
            product_type(callable_type(nested_param, 1), leaf_type("C")),
            1,
        );

        assert!(js_type_shape_equiv(&source_ty, &target_ty));
        assert!(!js_runtime_representation_equiv(&source_ty, &target_ty));
    }

    #[test]
    fn differing_product_callable_slot_arity_still_synthesizes_adapter() {
        let span = crate::span::Span::new(0, 0);
        let param = product_type(leaf_type("A"), leaf_type("B"));
        let source_ty = callable_type(param.clone(), 2);
        let target_ty = callable_type(param, 1);
        let continuation = Expr::LowBoundRef {
            occurrence: Default::default(),
            name: "continuation".to_owned(),
            meta: crate::ast::Meta::new(span),
            ext: (),
        };
        let mut local_tys = HashMap::from([("continuation".to_owned(), source_ty)]);

        assert_eq!(
            adapt_js_fn_value_for_slot(
                "continuation".to_owned(),
                &continuation,
                &target_ty,
                &mut local_tys,
            ),
            "((__inner) => (__a0) => (__inner)((__a0)[0], (__a0)[1]))(continuation)"
        );
    }

    #[test]
    fn equal_arity_different_product_callable_slot_layout_still_synthesizes_adapter() {
        let span = crate::span::Span::new(0, 0);
        let source_ty = callable_type(
            product_type(product_type(leaf_type("A"), leaf_type("B")), leaf_type("C")),
            1,
        );
        let target_ty = callable_type(
            product_type(leaf_type("A"), product_type(leaf_type("B"), leaf_type("C"))),
            1,
        );
        let continuation = Expr::LowBoundRef {
            occurrence: Default::default(),
            name: "continuation".to_owned(),
            meta: crate::ast::Meta::new(span),
            ext: (),
        };
        let mut local_tys = HashMap::from([("continuation".to_owned(), source_ty)]);

        assert_eq!(
            adapt_js_fn_value_for_slot(
                "continuation".to_owned(),
                &continuation,
                &target_ty,
                &mut local_tys,
            ),
            "((__inner) => (__a0) => (__inner)([[(__a0)[0], ((__a0)[1])[0]], ((__a0)[1])[1]]))(continuation)"
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
            lower_module_with_key_prepared(&entry.module, module_key, None, None, None, None)
                .expect("uncached module emit");
        emit_cache_work_counters::reset();
        let disabled_module = lower_module_with_key_cached(
            &entry.module,
            module_key,
            None,
            None,
            None,
            None,
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
            None,
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
        assert!(js.contains("function empty()"));
        assert!(js.contains("return null"));
    }

    #[test]
    fn fn_def_with_value_param() {
        let js = lower(
            "module main; \
             host type T role(str); \
             fn first(x: T) -> T { x }",
        );
        assert!(js.contains("function first(x)"), "got: {js}");
        assert!(js.contains("return x"), "got: {js}");
    }

    #[test]
    fn polymorphic_fn_def_keeps_an_erased_type_stage() {
        let js = lower("module x; fn id[A](x: A) -> A { x }");
        assert!(js.contains("function id()"), "got: {js}");
        assert!(js.contains("return (x) => x"), "got: {js}");
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
        assert!(js.contains("function caller()"), "got: {js}");
        assert!(js.contains("id()(y)"), "got: {js}");
    }

    #[test]
    fn higher_kinded_applied_values_keep_forall_stages_and_erase_the_brand() {
        let js = lower_typed(
            "module main; \
             fn id_app[*F][A](x: F(A)) -> F(A) { x } \
             fn first_app[*F][A][B](x: F(A), y: F(B)) -> F(A) { x }",
        );
        assert!(
            js.contains("function id_app()") && js.contains("return () => (x) => x;"),
            "got: {js}"
        );
        assert!(
            js.contains("function first_app()") && js.contains("return () => () => (x, y) => x;"),
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
            js.contains("() => (__p0) => __host__.x.hostId(__p0)"),
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
        assert!(js.contains("helper(y)"), "got: {js}");
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
            js.split(&format!("function {name}()"))
                .nth(1)
                .unwrap_or_else(|| panic!("missing `{name}` in: {js}"))
                .split(&format!("function {next}()"))
                .next()
                .expect("the function section has a terminator")
        };
        let staged_type_only = function_body("staged_type_only", "staged_implicit");
        let staged_implicit = function_body("staged_implicit", "staged_explicit");
        let staged_explicit = function_body("staged_explicit", "staged_effectful");
        assert!(
            staged_type_only.contains("const _kg0 = (staged)();")
                && staged_type_only.contains("=> _kg0("),
            "a written type-only packet must retain the selected Unit layer: {js}"
        );
        assert!(
            staged_implicit.contains("return staged()();")
                && !staged_implicit.contains("=> staged()()"),
            "a source-empty call must supply its Unit value: {js}"
        );
        assert!(
            staged_explicit.contains("return staged()();")
                && !staged_explicit.contains("=> staged()()"),
            "an explicit Unit value must saturate that same layer: {js}"
        );

        let identity_non_unit_residual =
            function_body("identity_non_unit_residual", "identity_selected");
        let identity_selected = function_body("identity_selected", "identity_saturated");
        let identity_saturated = js
            .split("function identity_saturated()")
            .nth(1)
            .expect("identity_saturated wrapper is emitted");
        assert!(
            identity_non_unit_residual.contains("const _kg0 = (identity)();")
                && identity_non_unit_residual.contains("=> _kg0("),
            "a selected non-Unit identity domain must remain residual: {js}"
        );
        assert!(
            identity_selected.contains("const _kg0 = (identity)();")
                && identity_selected.contains("_kg0(_kg1)"),
            "the type-selected Unit identity call must remain residual: {js}"
        );
        assert!(
            identity_saturated.contains("return identity()(null);")
                && !identity_saturated.contains("=> identity()"),
            "the explicit Unit identity call must be saturated: {js}"
        );

        let effectful = js
            .split("function staged_effectful()")
            .nth(1)
            .expect("effectful wrapper is emitted");
        let observe = effectful
            .find("observe()")
            .expect("explicit Unit operand is evaluated");
        let apply = effectful
            .find("staged()")
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
        // The callee is parenthesized at the indirect-call site
        // (`(<callee>)(<args>)`), so an inline-applied arrow renders as
        // `(((x) => x))(y)`. The grouping is load-bearing when the
        // callee is a bare arrow — see `emit_indirect_call`.
        assert!(js.contains("(((x) => x))(y)"), "got: {js}");
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
        assert!(js.contains("PKG_HELPER.greet()"), "got: {js}");
    }

    #[test]
    fn module_emits_iife_namespace() {
        let js = lower("module pkg/helper; pub fn greet() -> . { () }");
        assert!(js.contains("const PKG_HELPER = (() => {"), "got: {js}");
        assert!(js.contains("function greet()"), "got: {js}");
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
        assert!(js.contains("PKG_HELPER.greet()"), "got: {js}");
    }

    // ---- Intrinsics ----------------------------------------------------

    #[test]
    fn mk_pair_emits_array_literal() {
        let js = lower(
            "module x; \
             import __intrinsics__; \
             fn pair_of(x: .) -> (. & .) { __pair__(., ., x, x) }",
        );
        assert!(js.contains("[x, x]"), "got: {js}");
    }

    #[test]
    fn fst_emits_index_zero() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn first(p: (T & T)) -> T { __fst__(T, T, p) }",
        );
        assert!(js.contains("function first(p)"), "got: {js}");
        assert!(
            js.contains("return __kioProductSlot((p), 0, 2)"),
            "got: {js}"
        );
    }

    #[test]
    fn snd_emits_index_one() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn second(p: (T & T)) -> T { __snd__(T, T, p) }",
        );
        assert!(js.contains("function second(p)"), "got: {js}");
        assert!(
            js.contains("return __kioProductSlot((p), 1, 2)"),
            "got: {js}"
        );
    }

    #[test]
    fn fst_of_mk_pair_composes() {
        let js = lower(
            "module main; \
             import __intrinsics__; \
             host type T role(str); \
             fn first_of_pair(x: T) -> T { __fst__(T, T, __pair__(T, T, x, x)) }",
        );
        assert!(js.contains("function first_of_pair(x)"), "got: {js}");
        assert!(
            js.contains("return __kioProductSlot(([x, x]), 0, 2)"),
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

    // ---- FFI skin binder nesting ---------------------------------------

    /// Emit the export-boundary `Out` conversion for `fn_name`'s return
    /// type, the way the facade's export wrapper does
    /// (`emit_ffi_convert(&d.ret, "__body__", FfiDir::Out, …)`).
    fn export_ret_out_conversion(src: &str, fn_name: &str) -> String {
        let pkg = prime_package_routed(src, None);
        let ret = {
            let (_, entry) = pkg.modules().next().expect("one module");
            entry
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::FnDef(d) if d.name == fn_name => Some(d.ret.clone()),
                    _ => None,
                })
                .expect("fn present")
        };
        let mut out = String::new();
        emit_ffi_convert(&ret, "__body__", FfiDir::Out, Some(&pkg), &mut out).expect("convert");
        out
    }

    fn host_ret_in_conversion(src: &str, fn_name: &str, expr: &str) -> String {
        let pkg = prime_package_routed(src, None);
        let ret = {
            let (_, entry) = pkg.modules().next().expect("one module");
            entry
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::HostFn(d) if d.name == fn_name => Some(d.ret.clone()),
                    _ => None,
                })
                .expect("host fn present")
        };
        let mut out = String::new();
        emit_ffi_convert(&ret, expr, FfiDir::In, Some(&pkg), &mut out).expect("convert");
        out
    }

    #[test]
    fn incoming_nested_foralls_bind_each_source_once() {
        let src = "module x; host fn receive() -> [A] [B] .;";
        for value in [
            "__effect__()",
            "__source__.value",
            "(__left__(), __right__())",
        ] {
            let out = host_ret_in_conversion(src, "receive", value);
            assert_eq!(out.matches(value).count(), 1, "source duplicated: {out}");
            assert_eq!(
                out.matches("((__stage) => () => __stage)").count(),
                2,
                "one internal stage must remain per forall: {out}"
            );
            assert_eq!(
                out,
                format!("((__stage) => () => __stage)(((__stage) => () => __stage)({value}))")
            );
        }
    }

    #[test]
    fn prepared_compound_input_preserves_one_member_source() {
        for returned in ["T & T", "T | T | T", "T & (T | T)"] {
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
                .expect("host-return site");
            let execution = prepared_js_execution(site).expect("host execution");
            for expression in ["__source.value", "__source[\"value\"]", "__effect__()"] {
                let rendered = convert_prepared_js_use(
                    site,
                    site.plan().facade(),
                    execution.root_uses(),
                    site.plan().entry().returned,
                    expression,
                    PreparedJsDir::In,
                    PreparedJsNominalContext::Standalone,
                    &BTreeSet::new(),
                    &mut Vec::new(),
                    0,
                )
                .expect("convert actual prepared input");
                assert_eq!(rendered.matches(expression).count(), 1, "{rendered}");
            }
        }
    }

    #[test]
    fn incoming_callback_binds_its_forall_return_eagerly() {
        let out = host_ret_in_conversion(
            "module x; host fn receive() -> . -> [A] .;",
            "receive",
            "__effect__()",
        );
        assert_eq!(
            out.matches("__effect__()").count(),
            1,
            "callback source duplicated: {out}"
        );
        assert!(
            out.contains("((__stage) => () => __stage)(__inner())"),
            "callback result is not bound before its type stage: {out}"
        );
        assert!(
            !out.contains("() => __inner()"),
            "callback invocation was deferred into its type stage: {out}"
        );
    }

    #[test]
    fn nested_named_product_out_mints_depth_suffixed_binders() {
        // A named newtype product nested inside another named product
        // re-enters the product `Out` conversion inside the enclosing
        // `__slots` binding scope. `const` is TDZ-scoped over the whole
        // arrow body, so a same-named inner binder would capture the
        // enclosing-binder reference in its own initializer — a runtime
        // `ReferenceError`, not an outer-scope read. Nested binders must
        // be depth-suffixed. End-to-end shape:
        // `00_success/ffi_export_nested_product_roundtrip`.
        let out = export_ret_out_conversion(
            "module x; \
             host type T role(str); \
             pub labels { inner: T & T }; \
             pub labels { outer: T & Inner }; \
             pub fn make(v: Outer) -> Outer { v }",
            "make",
        );
        assert!(
            !out.contains("const __slots = __kioProductSlots(__slots"),
            "inner binder shadows the enclosing binder it reads: {out}"
        );
        assert!(
            out.contains("const __slots1 = __kioProductSlots(__slots["),
            "expected a depth-suffixed nested binder: {out}"
        );
    }

    #[test]
    fn nested_sum_out_mints_depth_suffixed_binders() {
        // The sum sibling of the nested-product case: a nested sum arm
        // re-enters the sum `Out` conversion inside the enclosing
        // `__sum` binding scope.
        let out = export_ret_out_conversion(
            "module x; \
             host type T role(str); \
             pub fn pick(v: ((T | T) | T)) -> ((T | T) | T) { v }",
            "pick",
        );
        assert!(
            !out.contains("const __sum = __kioSumPayload(__sum"),
            "inner binder shadows the enclosing binder it reads: {out}"
        );
        assert!(
            out.contains("const __sum1 = __kioSumPayload(__sum["),
            "expected a depth-suffixed nested binder: {out}"
        );
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

        assert!(
            js.contains("__host__.main.applyPoly((__kioBoundaryArg0)())")
                && js.contains(")(poly_echo)"),
            "{js}"
        );
        assert!(
            js.contains("const __c0 = __a0.T;")
                && js.contains("MAIN.ordered(__c0)()(__a1)")
                && js.contains("return __body__;"),
            "{js}"
        );
        assert!(!js.contains("apply_poly(((__inner)"), "{js}");
        assert!(
            js.contains(
                "return ((__kioBoundaryArg0) => __host__.main.opaqueReturn(__kioBoundaryArg0))(value);"
            ),
            "{js}"
        );
        assert!(
            js.contains("const __body__ = MAIN.poly_echo()(__a0);")
                && js.contains("polyEcho: __export_fn_"),
            "{js}"
        );
    }

    #[test]
    fn prepared_product_source_layout_reconstructs_one_body_argument() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             import __intrinsics__; \
             host type I32 role(i32); \
             pub fn first(pair: I32 & I32) -> I32 { __fst__(I32, I32, pair) }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("function __export_fn_0(__a0)")
                && js.contains("MAIN.first(__c0)")
                && js.contains("__a0._0")
                && js.contains("__a0._1"),
            "the JS product shell stays grouped while its body argument is reconstructed from the paired prepared layout: {js}"
        );
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

        assert!(
            js.contains("=> (__kioBoundaryPublic0_0) =>")
                && js.contains("__kioBoundaryPublic0_0._0")
                && js.contains("__kioBoundaryPublic0_0._1"),
            "a first-class product-domain function preserves its one source parameter while its prepared adapter rebuilds the private body product: {js}"
        );
    }

    #[test]
    fn prepared_nullary_unit_source_layout_has_no_public_argument() {
        let pkg = typed_package_routed_with_package(
            "module main; pub fn make_unit() -> . { () }",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            !js.contains("function __export_fn_0") && js.contains("makeUnit: MAIN.make_unit"),
            "a directly nullary prepared source has neither a public slot nor a body argument: {js}"
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
            !js.contains("function __export_fn_0") && js.contains("keepUnit: MAIN.keep_unit"),
            "a directly written Unit source is nullary at both the public and normalized body boundary: {js}"
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
            js.contains("function identity()")
                && js.contains("return (value) => value;")
                && !js.contains("identity()(null)"),
            "the generic value slot remains residual so a later Unit selection still supplies one explicit runtime value: {js}"
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
            js.contains("function __export_fn_0(__a0)")
                && js.contains("__a0._0")
                && js.contains("__a0._1"),
            "Bottom is an ordinary impossible slot inside the public product topology: {js}"
        );
    }

    #[test]
    fn prepared_curried_value_stages_flatten_semantic_slots_once() {
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
            js.contains("function __export_fn_0(__a0, __a1)")
                && js.contains("MAIN.choose(__c0)(__a1)")
                && js.contains("__a0._0")
                && js.contains("__a0._1"),
            "curried declaration stages must use the prepared semantic-slot cut and paired source layouts: {js}"
        );
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
        assert!(
            js.contains("return __kioOpaqueOut(\"6d61696e004f7061717565\", __body__)"),
            "{js}"
        );
        assert!(
            js.contains("const __c0 = __kioOpaqueIn(\"6d61696e004f7061717565\", __a0);"),
            "{js}"
        );
        assert!(!js.contains("return { Opaque: __body__ }"), "{js}");
        assert!(
            js.contains(
                "return __kioOpaqueOut(\"6d61696e00436f6e7374727563746f725f6f6e6c79\", __body__)"
            ),
            "{js}"
        );
        assert!(
            js.contains(
                "const __c0 = __kioOpaqueIn(\"6d61696e0050726f6a6563746f725f6f6e6c79\", __a0);"
            ),
            "{js}"
        );
        assert!(!js.contains("return { ConstructorOnly: __body__ }"), "{js}");
        assert!(!js.contains("__a0.ProjectorOnly"), "{js}");
        assert!(js.contains("return { BothPublic: __body__ }"), "{js}");
        assert!(js.contains("const __c0 = __a0.BothPublic;"), "{js}");
    }

    #[test]
    fn public_existential_projector_uses_its_compact_direct_member_shape() {
        let pkg = typed_package_routed_with_package(
            "module main; \
             pub newtype Hidden <U> <V> : U & V { \
               constructor make_hidden; pub projector reveal_hidden; \
             };",
            Some("bridge { main; }"),
        );
        let js = lower_package_to_factory_module(&pkg, "facade").expect("lower");

        assert!(
            js.contains("const __body__ = MAIN.Hidden.reveal_hidden(__c0);"),
            "{js}"
        );
        assert!(
            js.contains("__kioProductSlot(__kioBoundaryBody")
                && js.contains(", 0, 2)")
                && js.contains(", 1, 2)"),
            "the payload conversion must remain intact: {js}"
        );
        assert!(
            !js.contains("(__body__)()") && !js.contains("() => () => __p0"),
            "the direct member has no erased result or continuation type stage: {js}"
        );
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

        assert_eq!(js.matches("return { Shared: __body__ }").count(), 2, "{js}");
        assert_eq!(js.matches("const __c0 = __a0.Shared;").count(), 2, "{js}");
        assert!(js.contains("return { Nested:"), "{js}");
        assert!(js.contains("const __c0 = [__a0._0"), "{js}");
        assert!(js.contains("return { Box: __body__ }"), "{js}");
        assert!(js.contains("const __c0 = __a0.Box;"), "{js}");
        assert!(js.contains("return { Packed: __body__ }"), "{js}");
        assert!(js.contains("const __c0 = __a0.Packed;"), "{js}");
        assert!(js.contains("return { Field: __body__ }"), "{js}");
        assert!(js.contains("const __c0 = __a0.Field;"), "{js}");
        assert!(js.contains("KioType_Recursive: { makeRecursive:"), "{js}");
        assert!(js.contains("readRecursive:"), "{js}");
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

        assert!(
            js.contains(")(__a0.Box)")
                && js.contains("__kioProductSlots(__body__, 2)")
                && js.contains("return { Box:"),
            "the prepared payload binder must resolve to the exact structural application argument: {js}"
        );
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
            assert!(
                js.contains("._0.Box") && js.contains("_0: { Box:"),
                "the original abstract slot retains its substituted nominal wrapper ({payload}): {js}"
            );
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
        assert!(js.contains("run: MAIN.run"), "{js}");
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

        assert!(js.contains("keep: MAIN.keep"), "{js}");
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
    fn opaque_newtype_conversion_is_exact_atomic_and_state_free() {
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

        assert!(wrappers.contains("\"6c65667400546f6b656e\""), "{wrappers}");
        assert!(
            wrappers.contains("\"726967687400546f6b656e\""),
            "{wrappers}"
        );
        assert!(!wrappers.contains("Secret"), "{wrappers}");
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
        let left = "6c65667400546f6b656e";
        let right = "726967687400546f6b656e";

        assert!(
            js.contains(&format!("__kioOpaqueIn(\"{left}\", __host__.left.keep("))
                && js.contains(&format!(
                    "__host__.left.keep(__kioOpaqueOut(\"{left}\", __kioBoundaryArg0))"
                )),
            "{js}"
        );
        assert!(
            js.contains(&format!("__kioOpaqueIn(\"{right}\", __host__.right.keep("))
                && js.contains(&format!(
                    "__host__.right.keep(__kioOpaqueOut(\"{right}\", __kioBoundaryArg0))"
                )),
            "{js}"
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
        let hidden = "6d61696e0048696464656e";

        assert!(
            js.contains(&format!(
                "const __c0 = __kioOpaqueIn(\"{hidden}\", __a0.Outer);"
            )),
            "{js}"
        );
        assert!(
            js.contains(&format!(
                "return {{ Outer: __kioOpaqueOut(\"{hidden}\", __body__) }};"
            )),
            "{js}"
        );
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

        assert!(js.contains("keep: __export_fn_"), "{js}");
        assert!(!js.contains("keep: MAIN.keep"), "{js}");
    }

    #[test]
    fn alias_hidden_recursive_payload_uses_passthrough_export_conversion() {
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

        assert!(js.contains("keep: MAIN.keep"), "{js}");
        assert!(!js.contains("keep: __export_fn_"), "{js}");
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

        assert!(js.contains("keep: MAIN.keep"), "{js}");
        assert!(!js.contains("keep: __export_fn_"), "{js}");
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

        assert!(
            js.contains("keep: MAIN.keep"),
            "a transparent generic must substitute the exact used declaration binder before testing recursion: {js}"
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
        let declaration = pkg
            .module("list/core")
            .and_then(|entry| {
                entry.module.items.iter().find_map(|item| match item {
                    Item::Newtype(declaration) if declaration.name == "List" => Some(declaration),
                    _ => None,
                })
            })
            .expect("list/core.List declaration");

        assert!(newtype_is_recursive(declaration, "list/core", Some(&pkg)));
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
        let hidden_identity = encoded_newtype_identity("hidden", "Comptime_bool");

        assert!(
            js.contains("const __c0 = __a0.ComptimeBool;")
                && js.contains("return { ComptimeBool: __body__ };"),
            "{js}"
        );
        assert!(
            js.contains(&format!("__kioOpaqueIn(\"{hidden_identity}\", __a0)"))
                && js.contains(&format!("__kioOpaqueOut(\"{hidden_identity}\", __body__)")),
            "{js}"
        );
        assert!(
            js.contains("const __body__ = GENERIC.keep()(__a0);")
                && js.contains("keep: __export_fn_"),
            "{js}"
        );
        assert!(
            js.contains("roundHost: HOSTISH.round_host")
                && js.contains("roundAlias: ALIASING.round_alias")
                && js.contains("return { Wrapped: __body__ };"),
            "{js}"
        );
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
        let unrelated_identity = encoded_newtype_identity("aaa", "Nested");
        let round_adapter = |source: &str| {
            source
                .split("function __export_fn_")
                .skip(1)
                .find(|wrapper| wrapper.contains("ZED.round"))
                .and_then(|wrapper| wrapper.split("\n}").next())
                .expect("round wrapper")
                .to_owned()
        };

        assert!(
            js.contains("const __c0 = __a0.Outer.Nested;")
                && js.contains("return { Outer: { Nested: __body__ } };"),
            "{js}"
        );
        assert!(
            !js.contains(&format!(
                "__kioOpaqueIn(\"{unrelated_identity}\", __a0.Outer)"
            )) && !js.contains(&format!(
                "__kioOpaqueOut(\"{unrelated_identity}\", __body__)"
            )),
            "{js}"
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
        assert!(js.contains("mk_foo: (x) => x"), "got: {js}");
        assert!(js.contains("un_foo: (x) => x"), "got: {js}");
    }

    #[test]
    fn parametric_newtype_emits_runtime_identity() {
        // Type params don't affect the runtime shape — type erasure
        // means `Box[A]` looks identical to `Foo` at runtime.
        let js = lower(
            "module x; newtype Box[A] : A { pub constructor mk_box; pub projector un_box; };",
        );
        assert!(js.contains("const Box = {"), "got: {js}");
        assert!(js.contains("mk_box: (x) => x"), "got: {js}");
        assert!(js.contains("un_box: (x) => x"), "got: {js}");
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
        // `Box.mk_box(A, x)` — the `A` type-arg drops, only `x` flows
        // through to the JS namespace member.
        let js = lower_typed(
            "module x; \
             newtype Box[A] : A { pub constructor mk_box; pub projector un_box; }; \
             fn wrap[A](x: A) -> Box(A) { Box.mk_box(A, x) }",
        );
        assert!(js.contains("Box.mk_box(x)"), "got: {js}");
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

        assert!(js.contains("SELECTED.Box.mk_box(value)"), "got: {js}");
        assert!(!js.contains("ACTUAL.Box.mk_box(value)"), "got: {js}");
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
        let callee_evaluation = unique_position(&format!("({callee_name})();"));
        let type_stage_evaluation = unique_position(&format!("({type_stage_name})("));
        let first_product_slot = unique_position(", 0, 2)");
        let second_product_slot = unique_position(", 1, 2)");
        let ready_binding = unique_position("const __kio_cps_ready =");
        let continuation_evaluation = unique_position("(__kio_cps_continuation)()");
        let projector_call =
            unique_position("return Box.inspect(__kio_cps_receiver)(__kio_cps_ready);");

        assert_eq!(js.matches("__kio_cps_receiver").count(), 2, "got: {js}");
        assert_eq!(js.matches("__kio_cps_continuation").count(), 2, "got: {js}");
        assert_eq!(js.matches(callee_name).count(), 2, "got: {js}");
        assert_eq!(js.matches(type_stage_name).count(), 2, "got: {js}");
        assert_eq!(js.matches("__kio_cps_ready").count(), 2, "got: {js}");
        assert_eq!(js.matches("__kioProductSlot(").count(), 2, "got: {js}");
        assert!(
            receiver_binding < continuation_binding
                && continuation_binding < callee_binding
                && callee_binding < type_stage_binding
                && type_stage_binding < callee_evaluation
                && callee_evaluation < type_stage_evaluation
                && type_stage_evaluation < first_product_slot
                && first_product_slot < second_product_slot
                && second_product_slot < ready_binding
                && ready_binding < continuation_evaluation
                && continuation_evaluation < projector_call,
            "CPS stages were not emitted in evaluation order: {js}"
        );
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
        assert!(js.contains("return 42"), "got: {js}");
        assert!(js.contains("return 3.14"), "got: {js}");
        assert!(js.contains("return true"), "got: {js}");
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
        assert!(js.contains("return 7;"), "i32 stays bare: {js}");
        assert!(js.contains("return 9000000000n;"), "i64 gets n: {js}");
        assert!(js.contains("return 9000000000n;"), "u64 gets n: {js}");
        assert!(
            js.contains("return 170141183460469231731687303715884105727n;"),
            "i128 gets n: {js}"
        );
        assert!(js.contains("return 0n;"), "u128 gets n even at 0: {js}");
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

        assert!(js.contains("return 7;"), "left.Shared stays Number: {js}");
        assert!(js.contains("return 9n;"), "right.Shared is BigInt: {js}");
    }

    #[test]
    fn host_fn_def_call_lowers_to_host_record() {
        let js = lower(
            "module main; \
             host type String role(str); host fn print(s: String) -> .; \
             fn say(s: String) -> . { print(s) }",
        );
        // Rung-1 nested host record: `print` declared in module `main`
        // is reached through its module namespace `MAIN`.
        assert!(js.contains("__host__.main.print(s)"), "got: {js}");
    }

    #[test]
    fn host_fn_def_value_position_lowers_to_host_record() {
        // Bind the host print to a local; the local then calls through.
        let js = lower(
            "module main; \
             host type String role(str); host fn print(s: String) -> .; \
             fn forward(s: String) -> . { let p = print; p(s) }",
        );
        // `let p = print` binds a wrapper that preserves host FFI
        // conversions; `print` (module `main`) reaches the rung-1
        // nested host record through its namespace `MAIN`.
        assert!(
            js.contains("const p = (__p0) => __host__.main.print(__p0)"),
            "got: {js}"
        );
        assert!(js.contains("p(s)"), "got: {js}");
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
        // must lower to `function __kio_default__(...)`, and any
        // reference site (here: the body of `caller`) must use the
        // same mangled spelling. Without mangling, V8/QuickJS rejects
        // the emitted JS at parse time.
        let js = lower(
            "module x; \
             fn default[A](y: A) -> A { y } \
             fn caller[A](z: A) -> A { default(A, z) }",
        );
        assert!(
            js.contains("function __kio_default__()") && js.contains("return (y) => y"),
            "expected mangled decl, got: {js}"
        );
        assert!(
            js.contains("__kio_default__()(z)"),
            "expected mangled reference, got: {js}"
        );
        assert!(
            !js.contains("function default("),
            "expected no unmangled `default` decl, got: {js}"
        );
    }

    #[test]
    fn fn_param_with_reserved_name_mangles_at_decl_and_use() {
        // Reserved name as a value parameter — both the arrow's
        // parameter list and the body's reference must mangle.
        let js = lower("module x; fn id[A](class: A) -> A { class }");
        assert!(
            js.contains("function id()") && js.contains("return (__kio_class__) =>"),
            "expected mangled param, got: {js}"
        );
        assert!(
            js.contains("=> __kio_class__;"),
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
            js.contains("function helper()") && js.contains("return (y) => y"),
            "got: {js}"
        );
        assert!(!js.contains("__kio_"), "got: {js}");
    }
}

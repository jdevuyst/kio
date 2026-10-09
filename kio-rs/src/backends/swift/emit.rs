//! Swift backend — IR → Swift package lowering.
//!
//! Consumes a `Package<Routed>` (post-recovery + post-resolution-lowering)
//! and produces a self-contained Swift package as a [`SwiftPackage`].
//!
//! Swift's **body** carries canonical native values through erased (`Any`)
//! execution interfaces, retaining private type witnesses at lexical stages.
//! The **skin** is
//! the host's exact typed contract — the `Host` protocol, generic semantic
//! product/native-sum shells, declaration-owned nominal and application
//! carriers, exact callable carriers, and the exported surface. One shared
//! [`PreparedBoundaryCallableSites`] transaction supplies every public type,
//! slot, source layout, and body adapter. Every top-level name is branded
//! from the package's namespace ([`SwiftNames`]): the handle `<Handle>`, the
//! host contract `<Handle>Host`, the factory `create<Handle>`.

use crate::backends::{boundary_docs, doc};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};

use crate::ast::{Expr, Role, Routed, Type};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner,
    BoundaryFacadeSupportOrigin, BoundaryHostBindingOrigin, CallableExecutionLayout,
    CallableExecutionStage, CallableSourceParamAdapter, CallableValueStageLayout, FacadeBinderId,
    FacadeUse, FacadeUseId, PreparedBoundaryCallableSite, PreparedBoundaryCallableSites,
};
use crate::backends::public_names::{
    FacadeSelector, encode_host_identity, encode_source_identity, has_readable_snake_components,
    host_name_core,
};
use crate::backends::reconstruct::RoutedBodyTypes;
use crate::backends::skin::exact_nullary_host_type_table;
use crate::backends::structural::{
    NominalValueRepresentation, bound_product_rebuild_plan, render_cached_product_rebuild,
};
use crate::backends::value_plan::{ValuePlan, ValueUse};
use crate::host_descriptor;
use crate::pass::resolve::Package;

/// An unrecoverable error during Swift emit.
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

    pub(super) fn internal(message: impl Into<String>) -> Self {
        EmitError {
            message: format!("internal Swift emitter error: {}", message.into()),
        }
    }
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Emitted Swift package — the per-package source files plus the fixed
/// `kio_runtime.swift` runtime-support file. Every top-level name lives
/// under the package's namespace, imposed at compile time via
/// `-module-name` (Swift sources carry no module declaration); the
/// `pkg.swift` marker line publishes that namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwiftPackage {
    /// `pkg.swift` — the package's invocable surface: the `// kio-swift-module:`
    /// marker, the package-handle class, the `create<Handle>(host:)`
    /// factory, and every module fn as a method.
    pub pkg_swift: String,
    /// `host.swift` — the `<Handle>Host` protocol (current requirements plus
    /// optional deprecated defaults retained from sealed history).
    pub host_swift: String,
    /// `shapes.swift` — exact nominal and rank-N callable declarations.
    pub shapes_swift: String,
    /// `ffi.swift` — per-member boundary-type aliases.
    pub ffi_swift: String,
    /// The runtime-support file content.
    pub runtime_swift: String,
}

/// The branded facade names for one emitted Swift package, all derived
/// from the effective namespace: the emitted module name (`ns` — the
/// `// kio-swift-module:` marker's value and the `-module-name` a host
/// builds with), the PascalCase package handle, the `<Handle>Host`
/// contract, and the `create<Handle>` factory. One derivation root means
/// a consumer can reconstruct the whole surface from the marker alone.
///
/// Swift sources carry **no** module declaration — the module name is
/// imposed at compile time — so the namespace lives in the `pkg.swift`
/// marker line, not a per-file clause (contrast Go's `package <ns>` on
/// every file). For a single-segment PascalCase namespace the handle type
/// name **equals** the module name; a host `import <Ns>` sees the type
/// `<Ns>`, which shadows the module name in the host's scope, so the
/// factory and types resolve unqualified (verified: qualifying
/// `<Ns>.create<Ns>` instead binds `<Ns>` to the type and fails).
pub(crate) struct SwiftNames {
    pub ns: String,
    pub handle: String,
    pub host_ty: String,
    pub factory: String,
}

// Facade-internal value members use Kio's compiler-reserved `__` class.
// User identifiers cannot begin with `__`, so no exported function or root
// module selector can collide with these members.
const PACKAGE_BACKREF_MEMBER: &str = "__kio_pkg";
const HOST_STORAGE_MEMBER: &str = "__kio_host";
const MODULE_FN_MEMBER_PREFIX: &str = "__kio_mod_";

impl SwiftNames {
    pub(crate) fn derive(ns: &str) -> SwiftNames {
        let handle = ns.to_owned();
        SwiftNames {
            ns: ns.to_owned(),
            host_ty: format!("{handle}Host"),
            factory: format!("create{handle}"),
            handle,
        }
    }
}

/// Lower a typed (post-recovery) Kio package to a Swift package whose
/// emitted module name (the `pkg.swift` marker + the `-module-name` a
/// host builds with) is `ns`.
pub fn lower_package(package: &Package<Routed>, ns: &str) -> Result<SwiftPackage, EmitError> {
    lower_package_with_signature(package, ns, None)
}

/// Lower one package while retaining the deprecated, optional source facade
/// reached by removed host-function signatures. Retained sites never
/// contribute a package member, live host adapter, or body-dispatch route.
pub fn lower_package_with_signature(
    package: &Package<Routed>,
    ns: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<SwiftPackage, EmitError> {
    package.package_file().ok_or_else(|| {
        EmitError::unsupported(
            "Swift emitter requires a package file (`<pkg>.pkg.kio`); \
             a package without one has no package boundary to expose",
        )
    })?;

    let names = SwiftNames::derive(ns);
    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .map_err(|error| EmitError::internal(format!("facade planning failed: {error}")))?;
    let host_desc = host_descriptor::build_host_descriptor(package);
    let exact_host_types = exact_nullary_host_type_table(&host_desc);

    // Expression literals need only their exact nullary host identity. Public
    // facade planning and every callable adapter consume `prepared` below.
    let literal_shapes = super::skin::SwiftLiteralContext::new(package, exact_host_types);
    let facade_shapes = super::skin::PreparedSwiftShapes::new(&prepared, &names.host_ty, package);

    let host_swift = render_host_protocol(package, &prepared, &facade_shapes, &names)?;
    let pkg_swift = render_pkg_swift(package, &prepared, &facade_shapes, &literal_shapes, &names)?;
    let ffi_swift = render_ffi_swift(&prepared, &facade_shapes, &names)?;
    let shapes_swift = facade_shapes.render_shapes(package, &prepared);

    Ok(SwiftPackage {
        pkg_swift: finalize_swift_file(pkg_swift),
        host_swift: finalize_swift_file(host_swift),
        shapes_swift: finalize_swift_file(shapes_swift),
        ffi_swift: finalize_swift_file(ffi_swift),
        // The runtime-support file references no branded or module name
        // (Swift files carry no module clause), so its content is fixed —
        // byte-identical across packages — unlike Go's, which binds the
        // `package <ns>` line. See `super::runtime`.
        runtime_swift: super::RUNTIME_SUPPORT_FILE_CONTENT.to_owned(),
    })
}

/// Normalize an emitted Swift file: strip trailing blank lines, end with
/// exactly one newline. The emitted surface uses Swift standard-library
/// constructs and needs no import injection.
fn finalize_swift_file(mut s: String) -> String {
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

/// The exact Swift boundary member name for a `host fn`.
fn host_member_name(module_path: &str, leaf: &str) -> String {
    host_member_with_rendered_leaf(module_path, &host_name_core(leaf))
}

fn host_member_with_rendered_leaf(module_path: &str, leaf: &str) -> String {
    if !module_path.contains('_') {
        return format!("{}__{leaf}", module_path.replace('/', "_"));
    }
    format!("KioItem_{}__{leaf}", encode_host_identity(module_path))
}

/// The exact boundary-alias member for one public newtype constructor or
/// projector. The reserved leading `__` keeps this role distinct from an
/// ordinary exported function, while the separately encoded newtype and
/// member components keep `(A_b, c)` distinct from `(A, b_c)`.
fn export_newtype_member_name(module_path: &str, newtype_name: &str, member_name: &str) -> String {
    let leaf = format!(
        "__KioType_{}__KioItem_{}",
        encode_host_identity(newtype_name),
        encode_host_identity(member_name)
    );
    host_member_with_rendered_leaf(module_path, &leaf)
}

/// Exact host-type member name. Paths without underscores keep the readable
/// slash-to-underscore form. Paths containing underscores use a reserved,
/// injective encoding so source-word and module boundaries remain distinct.
pub(crate) fn host_type_member_name(module_path: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module_path.contains('_') {
        return format!("{}__{leaf}", module_path.replace('/', "_"));
    }
    let module = format!("__kio_host_{}", encode_host_identity(module_path));
    format!("{module}__{leaf}")
}

#[derive(Clone, Default)]
struct SwiftFacadeScope {
    binders: BTreeMap<FacadeBinderId, String>,
}

impl SwiftFacadeScope {
    fn bind(&mut self, id: FacadeBinderId) -> String {
        let rendered = format!("KioType_{}", id.index());
        self.binders.insert(id, rendered.clone());
        rendered
    }
}

struct PreparedSwiftCallable {
    generic_params: Vec<String>,
    param_types: Vec<String>,
    ret_type: String,
    ret_use: FacadeUseId,
    scope: SwiftFacadeScope,
}

/// Exact semantic and source-presentation views for one prepared callable
/// site. Retained sites can render this pair without acquiring live package
/// execution authority; conversion callers separately prove the site live.
#[derive(Clone, Copy)]
struct PreparedSwiftRender<'site, 'shapes> {
    site: PreparedBoundaryCallableSite<'site>,
    plan: &'site BoundaryFacadePlan,
    shapes: &'shapes super::skin::PreparedSwiftShapes<'shapes>,
}

fn prepared_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    site: &BoundaryFacadeSiteId,
) -> Result<PreparedBoundaryCallableSite<'a>, EmitError> {
    prepared.site(site).ok_or_else(|| {
        EmitError::internal(format!(
            "Swift emitter is missing prepared facade site {site:?}"
        ))
    })
}

fn host_function_site_id(module_path: &str, name: &str) -> BoundaryFacadeSiteId {
    BoundaryFacadeSiteId::new(
        module_path.split('/').map(str::to_owned).collect(),
        BoundaryFacadeSiteOwner::HostFunction {
            name: name.to_owned(),
        },
    )
    .expect("a routed host function has a non-empty exact site identity")
}

fn retained_host_function_deprecation(site: PreparedBoundaryCallableSite<'_>) -> Option<String> {
    let metadata = site.retained()?;
    let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
        unreachable!("only removed host functions own retained Swift facade sites")
    };
    Some(format!(
        "Kio host fn `{}.{name}` was removed at contract v{}",
        site.site().module_segments().join("."),
        metadata.removed_at_version()
    ))
}

fn live_execution(
    site: PreparedBoundaryCallableSite<'_>,
) -> Result<&CallableExecutionLayout, EmitError> {
    site.execution().ok_or_else(|| {
        EmitError::internal(format!(
            "retained Swift facade site reached live emission: {:?}",
            site.site()
        ))
    })
}

fn render_prepared_source_param_type(
    render: PreparedSwiftRender<'_, '_>,
    slots: &[FacadeUseId],
    source: &crate::backends::boundary_facade::CallableSourceParamLayout,
    scope: &SwiftFacadeScope,
    host_ref: &str,
) -> Result<String, EmitError> {
    let range = source.facade_slots();
    let source_slots = slots.get(range).ok_or_else(|| {
        EmitError::internal("Swift prepared source parameter exceeds its semantic stage")
    })?;
    match source.adapter() {
        CallableSourceParamAdapter::UnitValue => Ok("KioUnit".to_owned()),
        CallableSourceParamAdapter::Identity => {
            let [slot] = source_slots else {
                return Err(EmitError::internal(
                    "Swift identity source parameter does not own one semantic slot",
                ));
            };
            render_prepared_use(render, *slot, scope, host_ref)
        }
        CallableSourceParamAdapter::RightNest => {
            let values = prepared_values(render);
            values
                .plan
                .native_parameter_domain(source, slots, |step| match step {
                    crate::backends::value_plan::ValueDomainStep::Value(id) => {
                        values.native(id, &scope.binders, host_ref)
                    }
                    crate::backends::value_plan::ValueDomainStep::Product(left, right) => {
                        Ok(format!("Product<{left}, {right}>"))
                    }
                })?
                .ok_or_else(|| {
                    EmitError::internal("a prepared product source parameter has a domain")
                })
        }
    }
}

fn prepared_values<'a, 'p>(render: PreparedSwiftRender<'a, 'p>) -> super::value::Types<'a, 'p>
where
    'p: 'a,
{
    super::value::Types {
        plan: render.site.plan().value_plan(),
        shapes: render.shapes,
        retained_nominals: Some(render.site.nominals()),
        origin: render
            .site
            .retained()
            .map_or(BoundaryFacadeSupportOrigin::Live, |metadata| {
                BoundaryFacadeSupportOrigin::Retained {
                    removed_at_version: metadata.removed_at_version(),
                }
            }),
    }
}

fn render_prepared_use(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    scope: &SwiftFacadeScope,
    host_ref: &str,
) -> Result<String, EmitError> {
    let values = prepared_values(render);
    values.native(values.plan.source(id), &scope.binders, host_ref)
}

fn render_prepared_callable(
    site: PreparedBoundaryCallableSite<'_>,
    shapes: &super::skin::PreparedSwiftShapes,
    host_ref: &str,
) -> Result<PreparedSwiftCallable, EmitError> {
    let plan = site.plan();
    let presentation = site.presentation();
    let render = PreparedSwiftRender {
        site,
        plan: plan.facade(),
        shapes,
    };
    let entry = plan.entry();
    if entry.head_stages.len() != presentation.head_stages().len() {
        return Err(EmitError::internal(
            "Swift callable semantic/source head stages have different lengths",
        ));
    }
    let mut scope = SwiftFacadeScope::default();
    let mut generic_params = Vec::new();
    let mut param_types = Vec::new();
    for (semantic, source) in entry.head_stages.iter().zip(presentation.head_stages()) {
        match (semantic, source) {
            (BoundaryCallableHeadStage::Type { id, .. }, CallableExecutionStage::Type { .. }) => {
                generic_params.push(scope.bind(*id))
            }
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                for source in layout.source_params() {
                    param_types.push(render_prepared_source_param_type(
                        render, slots, source, &scope, host_ref,
                    )?);
                }
            }
            _ => {
                return Err(EmitError::internal(
                    "Swift callable semantic/source stage kind drift",
                ));
            }
        }
    }
    let ret_type = render_prepared_use(render, entry.returned, &scope, host_ref)?;
    Ok(PreparedSwiftCallable {
        generic_params,
        param_types,
        ret_type,
        ret_use: entry.returned,
        scope,
    })
}

/// Render `ffi.swift`: stable-named type aliases for every compound type
/// at a host-fn / export-fn boundary slot.
fn render_ffi_swift(
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n");

    for site in prepared.sites() {
        let prefix = match site.site().owner() {
            BoundaryFacadeSiteOwner::HostFunction { name } => format!(
                "Env_{}",
                host_member_name(&site.site().module_segments().join("/"), name)
            ),
            BoundaryFacadeSiteOwner::ExportedFunction { name } => format!(
                "Exp_{}",
                host_member_name(&site.site().module_segments().join("/"), name)
            ),
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
            | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => format!(
                "Exp_{}",
                export_newtype_member_name(
                    &site.site().module_segments().join("/"),
                    newtype,
                    member,
                )
            ),
        };
        let callable = render_prepared_callable(site, shapes, "H")?;
        let deprecation = retained_host_function_deprecation(site);
        for (index, ty) in callable.param_types.iter().enumerate() {
            emit_prepared_ffi_alias(
                &format!("{prefix}_arg{index}"),
                ty,
                &callable.generic_params,
                names,
                deprecation.as_deref(),
                &mut out,
            );
        }
        emit_prepared_ffi_alias(
            &format!("{prefix}_ret"),
            &callable.ret_type,
            &callable.generic_params,
            names,
            deprecation.as_deref(),
            &mut out,
        );
        emit_prepared_nested_callable_aliases(
            site,
            &prefix,
            &callable,
            shapes,
            names,
            deprecation.as_deref(),
            &mut out,
        )?;
    }
    Ok(out)
}

fn emit_prepared_ffi_alias(
    alias: &str,
    ty: &str,
    callable_generics: &[String],
    names: &SwiftNames,
    deprecation_message: Option<&str>,
    out: &mut String,
) {
    let mut generics = Vec::new();
    if swift_type_mentions_identifier(ty, "H") {
        generics.push(format!("H: {}", names.host_ty));
    }
    generics.extend(
        callable_generics
            .iter()
            .filter(|generic| swift_type_mentions_identifier(ty, generic))
            .cloned(),
    );
    let deprecated = deprecation_message
        .map(super::skin::swift_deprecated_attribute)
        .unwrap_or_default();
    out.push_str(&format!(
        "\n{deprecated}public typealias {alias}{} = {ty}\n",
        super::skin::swift_generic_use(&generics)
    ));
}

fn swift_type_mentions_identifier(ty: &str, identifier: &str) -> bool {
    ty.match_indices(identifier).any(|(start, _)| {
        let before = ty[..start].chars().next_back();
        let after = ty[start + identifier.len()..].chars().next();
        !before.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && !after.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

fn swift_public_method_generics(callable: &PreparedSwiftCallable) -> Vec<String> {
    callable.generic_params.clone()
}

fn swift_method_type_selectors(callable: &PreparedSwiftCallable) -> Vec<(usize, &str)> {
    callable
        .generic_params
        .iter()
        .enumerate()
        .filter_map(|(index, generic)| {
            let inferred = callable
                .param_types
                .iter()
                .chain(std::iter::once(&callable.ret_type))
                .any(|ty| swift_type_mentions_identifier(ty, generic));
            (!inferred).then_some((index, generic.as_str()))
        })
        .collect()
}

#[derive(Clone, Copy)]
struct SwiftNestedAliasContext<'scope, 'generics, 'names, 'deprecation> {
    scope: &'scope SwiftFacadeScope,
    callable_generics: &'generics [String],
    names: &'names SwiftNames,
    deprecation: Option<&'deprecation str>,
}

fn emit_prepared_nested_callable_aliases(
    site: PreparedBoundaryCallableSite<'_>,
    prefix: &str,
    callable: &PreparedSwiftCallable,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
    deprecation_message: Option<&str>,
    out: &mut String,
) -> Result<(), EmitError> {
    let presentation = site.presentation();
    let render = PreparedSwiftRender {
        site,
        plan: site.plan().facade(),
        shapes,
    };
    let entry = site.plan().entry();
    let mut public_index = 0usize;
    for (semantic, source) in entry.head_stages.iter().zip(presentation.head_stages()) {
        let (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) =
            (semantic, source)
        else {
            continue;
        };
        for source in layout.source_params() {
            if source.adapter() == CallableSourceParamAdapter::Identity {
                let range = source.facade_slots();
                let [slot] = &slots[range] else {
                    return Err(EmitError::internal(
                        "Swift FFI alias identity source does not own one slot",
                    ));
                };
                emit_prepared_nested_use_aliases(
                    render,
                    *slot,
                    &format!("{prefix}_arg{public_index}"),
                    SwiftNestedAliasContext {
                        scope: &callable.scope,
                        callable_generics: &callable.generic_params,
                        names,
                        deprecation: deprecation_message,
                    },
                    out,
                )?;
            }
            public_index += 1;
        }
    }
    emit_prepared_nested_use_aliases(
        render,
        callable.ret_use,
        &format!("{prefix}_ret"),
        SwiftNestedAliasContext {
            scope: &callable.scope,
            callable_generics: &callable.generic_params,
            names,
            deprecation: deprecation_message,
        },
        out,
    )
}

fn emit_prepared_nested_use_aliases(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    alias: &str,
    context: SwiftNestedAliasContext<'_, '_, '_, '_>,
    out: &mut String,
) -> Result<(), EmitError> {
    match render.plan.use_at(id) {
        FacadeUse::Function { slots, result, .. } => {
            let values = prepared_values(render);
            let ValueUse::Function { domain, .. } = values.plan.use_at(values.plan.source(id))
            else {
                unreachable!()
            };
            let ty = values.native(*domain, &context.scope.binders, "H")?;
            let nested_alias = format!("{alias}_cbarg0");
            emit_prepared_ffi_alias(
                &nested_alias,
                &ty,
                context.callable_generics,
                context.names,
                context.deprecation,
                out,
            );
            if let [slot] = slots.as_slice() {
                emit_prepared_nested_use_aliases(render, *slot, &nested_alias, context, out)?;
            }
            let ret = render_prepared_use(render, *result, context.scope, "H")?;
            let nested_alias = format!("{alias}_cbret");
            emit_prepared_ffi_alias(
                &nested_alias,
                &ret,
                context.callable_generics,
                context.names,
                context.deprecation,
                out,
            );
            emit_prepared_nested_use_aliases(render, *result, &nested_alias, context, out)?;
        }
        FacadeUse::Forall { binder, result, .. } => {
            let values = prepared_values(render);
            let carrier = values.forall_name(values.plan.source(id))?;
            let deprecated = context
                .deprecation
                .map(super::skin::swift_deprecated_attribute)
                .unwrap_or_default();
            out.push_str(&format!(
                "\n{deprecated}public typealias {alias}Implementation = {carrier}Implementation\n"
            ));
            let mut nested_scope = context.scope.clone();
            let mut nested_generics = context.callable_generics.to_vec();
            nested_generics.push(nested_scope.bind(*binder));
            let nested_alias = format!("{alias}_typeRet");
            let ty = render_prepared_use(render, *result, &nested_scope, "H")?;
            emit_prepared_ffi_alias(
                &nested_alias,
                &ty,
                &nested_generics,
                context.names,
                context.deprecation,
                out,
            );
            emit_prepared_nested_use_aliases(
                render,
                *result,
                &nested_alias,
                SwiftNestedAliasContext {
                    scope: &nested_scope,
                    callable_generics: &nested_generics,
                    names: context.names,
                    deprecation: context.deprecation,
                },
                out,
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// Render `host.swift`: the current `<Handle>Host` requirements plus optional
/// deprecated, trapping removal defaults from sealed signature history.
fn render_host_protocol(
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n\n");
    out.push_str(&format!(
        "// {} is the host record contract. Conform to it to invoke the\n\
         // package. See `specs/backends/swift.md` § Host record contract.\n",
        names.host_ty
    ));
    out.push_str(&format!("public protocol {} {{\n", names.host_ty));
    for host_type in shapes.live_host_bindings() {
        if host_type.type_arity != 0 {
            continue;
        }
        let binding = prepared
            .host_binding(&host_type.name)
            .expect("a live Swift host binding belongs to the prepared boundary");
        push_doc(&mut out, boundary_docs::host_type(package, binding), "\t");
        let assoc = host_type_member_name(
            &host_type.name.module_segments().join("/"),
            host_type.name.name(),
        );
        let constraints = swift_host_type_constraints(host_type.role);
        out.push_str(&format!("\tassociatedtype {assoc}{constraints}\n"));
    }
    let retained_host_types = shapes
        .retained_host_bindings()
        .filter(|binding| binding.type_arity == 0)
        .collect::<Vec<_>>();
    for host_type in &retained_host_types {
        let BoundaryHostBindingOrigin::Retained { removed_at_version } = host_type.origin else {
            unreachable!("the retained Swift host-binding iterator returned a live binding")
        };
        let qualified = format!(
            "{}.{}",
            host_type.name.module_segments().join("."),
            host_type.name.name()
        );
        let assoc = host_type_member_name(
            &host_type.name.module_segments().join("/"),
            host_type.name.name(),
        );
        let constraints = swift_host_type_constraints(host_type.role);
        let default = swift_host_type_default(host_type.role);
        out.push_str(&format!(
            "\t{}\tassociatedtype {assoc}{constraints} = {default}\n",
            super::skin::swift_deprecated_attribute(&format!(
                "Kio host type `{qualified}` is retained only for removed host declarations from contract v{removed_at_version}"
            ))
        ));
    }
    let host_sites = prepared
        .sites()
        .filter(|site| {
            matches!(
                site.site().owner(),
                BoundaryFacadeSiteOwner::HostFunction { .. }
            )
        })
        .collect::<Vec<_>>();
    if (shapes.live_host_bindings().next().is_some() || !retained_host_types.is_empty())
        && !host_sites.is_empty()
    {
        out.push('\n');
    }
    let mut retained_defaults = Vec::new();
    for site in host_sites {
        let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
            unreachable!()
        };
        let method = host_member_name(&site.site().module_segments().join("/"), name);
        let callable = render_prepared_callable(site, shapes, "Self")?;
        let generic = super::skin::swift_generic_use(&swift_public_method_generics(&callable));
        let mut params = callable
            .param_types
            .iter()
            .enumerate()
            .map(|(index, ty)| format!("_ arg{index}: {}", escaping_param_ty(ty)))
            .collect::<Vec<_>>();
        params.extend(
            swift_method_type_selectors(&callable)
                .into_iter()
                .map(|(index, ty)| format!("asKioType{index} _: {ty}.Type")),
        );
        let params = params.join(", ");
        let ret = if matches!(
            site.plan().facade().use_at(callable.ret_use),
            FacadeUse::Unit { .. }
        ) {
            String::new()
        } else {
            format!(" -> {}", callable.ret_type)
        };
        let signature = format!("func {method}{generic}({params}){ret}");
        push_doc(&mut out, boundary_docs::callable(package, site), "\t");
        if let Some(message) = retained_host_function_deprecation(site) {
            out.push_str(&format!(
                "\t{}\t{signature}\n",
                super::skin::swift_deprecated_attribute(&message)
            ));
            retained_defaults.push((message, signature));
        } else {
            out.push_str(&format!("\t{signature}\n"));
        }
    }
    out.push_str("}\n");
    if !retained_defaults.is_empty() {
        out.push_str(&format!("\npublic extension {} {{\n", names.host_ty));
        for (message, signature) in retained_defaults {
            out.push_str(&format!(
                "\t{}\t{signature} {{\n\t\tfatalError(\"{message}\")\n\t}}\n",
                super::skin::swift_deprecated_attribute(&message)
            ));
        }
        out.push_str("}\n");
    }
    Ok(out)
}

fn swift_host_type_constraints(role: Option<Role>) -> &'static str {
    match role {
        None => "",
        Some(
            Role::I8
            | Role::I16
            | Role::I32
            | Role::I64
            | Role::I128
            | Role::U8
            | Role::U16
            | Role::U32
            | Role::U64
            | Role::U128,
        ) => ": ExpressibleByIntegerLiteral",
        Some(Role::F32 | Role::F64) => ": ExpressibleByFloatLiteral",
        Some(Role::Bool) => ": ExpressibleByBooleanLiteral, Equatable",
        Some(Role::Str) => ": ExpressibleByStringLiteral",
    }
}

fn swift_host_type_default(role: Option<Role>) -> &'static str {
    match role {
        None => "KioUnit",
        Some(
            Role::I8
            | Role::I16
            | Role::I32
            | Role::I64
            | Role::I128
            | Role::U8
            | Role::U16
            | Role::U32
            | Role::U64
            | Role::U128,
        ) => "Swift.Int",
        Some(Role::F32 | Role::F64) => "Swift.Double",
        Some(Role::Bool) => "Swift.Bool",
        Some(Role::Str) => "Swift.String",
    }
}

/// A function-typed Swift parameter must be `@escaping` when it can outlive
/// the call. Generated top-level closure types start with `(` and contain
/// ` -> `; a named Product can contain that arrow only in a nested generic
/// argument and must not receive the parameter-only attribute.
fn escaping_param_ty(rendered: &str) -> String {
    if rendered.starts_with('(') && rendered.contains(" -> ") {
        format!("@escaping {rendered}")
    } else {
        rendered.to_owned()
    }
}

/// Render `pkg.swift`: the `// kio-swift-module:` marker, the package-handle
/// class, the export-namespace classes, the branded factory, and every
/// module fn as a method.
///
/// The marker is the file's **first line** — Swift sources carry no module
/// declaration (the module name is imposed at compile time via
/// `-module-name`), so the marker is the one published fact a host's
/// toolchain reads to learn the module name to build with. See
/// `specs/backends/swift.md` § Output layout.
fn render_pkg_swift(
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    literal_shapes: &super::skin::SwiftLiteralContext<'_>,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str(&format!("// kio-swift-module: {}\n", names.ns));
    out.push_str("// Generated by kio — do not edit by hand.\n\n");

    let export_tree = collect_export_namespaces(prepared);
    out.push_str(&render_export_namespace_types(
        package,
        &export_tree,
        prepared,
        shapes,
        names,
    )?);
    out.push_str(&render_package_class(
        &export_tree,
        package,
        prepared,
        shapes,
        literal_shapes,
        names,
    )?);
    out.push_str(&render_create_package(names));
    Ok(out)
}

/// One namespace segment in the export tree.
#[derive(Default)]
struct ExportTree {
    children: BTreeMap<FacadeSelector, ExportTree>,
    exports: Vec<ExportEntry>,
}

struct ExportEntry {
    leaf: String,
    site: BoundaryFacadeSiteId,
    body: ExportBody,
}

enum ExportBody {
    ModuleFn { method: String },
    NewtypeConstructor,
    NewtypeProjector,
}

fn collect_export_namespaces(prepared: &PreparedBoundaryCallableSites) -> ExportTree {
    let mut root = ExportTree::default();
    for newtype in prepared.public_newtypes() {
        export_ns_node(&mut root, &newtype.name().module_segments().join("/"))
            .children
            .entry(FacadeSelector::Type(newtype.name().name().to_owned()))
            .or_default();
    }
    for site in prepared.sites() {
        let module = site.site().module_segments().join("/");
        let (leaf, body, newtype) = match site.site().owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. } => continue,
            BoundaryFacadeSiteOwner::ExportedFunction { name } => (
                name.clone(),
                ExportBody::ModuleFn {
                    method: module_fn_method_name(&module, name),
                },
                None,
            ),
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => (
                member.clone(),
                ExportBody::NewtypeConstructor,
                Some(newtype.clone()),
            ),
            BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => (
                member.clone(),
                ExportBody::NewtypeProjector,
                Some(newtype.clone()),
            ),
        };
        let node = export_ns_node(&mut root, &module);
        let node = if let Some(newtype) = newtype {
            node.children
                .entry(FacadeSelector::Type(newtype))
                .or_default()
        } else {
            node
        };
        node.exports.push(ExportEntry {
            leaf,
            site: site.site().clone(),
            body,
        });
    }
    root
}

fn export_ns_node<'t>(root: &'t mut ExportTree, module_key: &str) -> &'t mut ExportTree {
    let mut node = root;
    for seg in module_key.split('/') {
        node = node
            .children
            .entry(FacadeSelector::Module(seg.to_owned()))
            .or_default();
    }
    node
}

/// The Swift method name on the package handle for a module fn.
fn module_fn_method_name(module_key: &str, leaf: &str) -> String {
    if !module_key.contains('_') && !leaf.contains('_') {
        return format!(
            "{MODULE_FN_MEMBER_PREFIX}{}_{leaf}",
            module_key.replace('/', "_")
        );
    }
    format!(
        "{MODULE_FN_MEMBER_PREFIX}KioItem_{}__{leaf}",
        encode_source_identity(module_key)
    )
}

/// The exact Swift class name for one role-bearing export-namespace path.
fn export_ns_type_name(path: &[FacadeSelector]) -> String {
    if path.is_empty() {
        "ExportNs".to_owned()
    } else {
        let components: Vec<String> = path
            .iter()
            .map(|selector| {
                let role = match selector {
                    FacadeSelector::Module(_) => "M",
                    FacadeSelector::Type(_) => "T",
                };
                format!("{role}_{}", encode_host_identity(selector.source()))
            })
            .collect();
        format!("ExportNs_{}", components.join("__"))
    }
}

/// Render the Swift class for each non-root export-namespace node.
fn render_export_namespace_types(
    package: &Package<Routed>,
    root: &ExportTree,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    render_export_ns_type_rec(
        package,
        root,
        &mut Vec::new(),
        prepared,
        shapes,
        names,
        &mut out,
    )?;
    Ok(out)
}

fn render_export_ns_type_rec(
    package: &Package<Routed>,
    node: &ExportTree,
    path: &mut Vec<FacadeSelector>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
    out: &mut String,
) -> Result<(), EmitError> {
    for (selector, child) in &node.children {
        path.push(selector.clone());
        render_export_ns_type_rec(package, child, path, prepared, shapes, names, out)?;
        path.pop();
    }
    if path.is_empty() {
        // The root node's exports (a `pub fn` in a top-level module) become
        // methods on the package handle directly — rendered there.
        return Ok(());
    }
    let ty = export_ns_type_name(path);
    out.push_str(&format!(
        "public final class {ty}<H: {}> {{\n",
        names.host_ty
    ));
    out.push_str(&format!(
        "\tvar {PACKAGE_BACKREF_MEMBER}: {}<H>!\n",
        names.handle
    ));
    for selector in node.children.keys() {
        let mut child_path = path.clone();
        child_path.push(selector.clone());
        out.push_str(&format!(
            "\tpublic let {}: {}<H>\n",
            export_selector_name(selector, false),
            export_ns_type_name(&child_path)
        ));
    }
    // Init: construct child namespaces (the back-reference is wired by the
    // package handle's initializer).
    out.push_str("\tinit() {\n");
    for selector in node.children.keys() {
        let mut child_path = path.clone();
        child_path.push(selector.clone());
        out.push_str(&format!(
            "\t\tself.{} = {}<H>()\n",
            export_selector_name(selector, false),
            export_ns_type_name(&child_path)
        ));
    }
    out.push_str("\t}\n");
    for e in &node.exports {
        out.push_str(&render_export_method(package, e, prepared, shapes)?);
    }
    out.push_str("}\n\n");
    Ok(())
}

fn swift_product_expression(values: &[String], witnesses: &[String]) -> String {
    assert_eq!(
        values.len(),
        witnesses.len(),
        "each Swift product slot has its exact type witness"
    );
    format!(
        "__kioProduct([{}], [{}])",
        values.join(", "),
        witnesses.join(", ")
    )
}

fn prepared_public_to_body_args(
    render: PreparedSwiftRender<'_, '_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    public_values: &[String],
    scope: &SwiftFacadeScope,
    _depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count()
        || public_values.len() != layout.source_param_count()
    {
        return Err(EmitError::internal(
            "Swift prepared public/source stage alignment drift",
        ));
    }
    layout
        .source_params()
        .iter()
        .zip(public_values)
        .map(|(source, value)| {
            let ty = render_prepared_source_param_type(render, slots, source, scope, "H")?;
            Ok(format!("(({value} as {ty}) as Any)"))
        })
        .collect()
}

fn prepared_body_to_public_args(
    render: PreparedSwiftRender<'_, '_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    body_values: &[String],
    scope: &SwiftFacadeScope,
    _depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count() || body_values.len() != layout.body_abi_arity() {
        return Err(EmitError::internal(
            "Swift prepared body/source stage alignment drift",
        ));
    }
    layout
        .source_params()
        .iter()
        .zip(body_values)
        .map(|(source, value)| {
            let ty = render_prepared_source_param_type(render, slots, source, scope, "H")?;
            Ok(format!("__kioChecked({value}, as: ({ty}).self)"))
        })
        .collect()
}

/// Open the hidden type stages retained by one nominal existential packet,
/// then invoke its continuation at the exact payload type.
fn render_existential_continuation(
    types: &super::value::Types<'_, '_>,
    mut current: crate::backends::value_plan::ValueUseId,
    scope: &super::value::Scope,
    continuation: &str,
    packet: &str,
) -> Result<String, EmitError> {
    let mut scope = scope.clone();
    let mut advanced = continuation.to_owned();
    let mut index = 0;
    while let ValueUse::Forall { binder, result } = types.plan.use_at(current) {
        let witness = format!("{packet}.witnesses[{index}]");
        scope.insert(*binder, witness.clone());
        advanced = format!("__kioAdvance({advanced}, {witness})");
        current = *result;
        index += 1;
    }
    if !matches!(types.plan.use_at(current), ValueUse::Function { .. }) {
        return Err(EmitError::internal(
            "Swift existential continuation lost its typed value stage",
        ));
    }
    let witness = types.witness(current, &scope, "H")?;
    Ok(format!(
        "{{ () -> Any in precondition({packet}.witnesses.count == {index}); let __kioContinuation: Any = {advanced}; return __kioChecked({witness}, as: (any __KioFunctionWitness).self).invoke(__kioContinuation, {packet}.payload) }}()"
    ))
}

fn render_existential_projection(
    types: &super::value::Types<'_, '_>,
    root: crate::backends::value_plan::ValueUseId,
    scope: &super::value::Scope,
    target: &str,
) -> Result<String, EmitError> {
    let mut scope = scope.clone();
    let mut current = root;
    let mut stages = Vec::new();
    while let ValueUse::Forall { binder, result } = types.plan.use_at(current) {
        let witness = types.witness(current, &scope, "H")?;
        let argument = format!("__kioSelected{}", stages.len());
        scope.insert(*binder, argument.clone());
        stages.push((witness, argument));
        current = *result;
    }
    let ValueUse::Function { domain, .. } = types.plan.use_at(current) else {
        return Err(EmitError::internal(
            "Swift existential projection lost its continuation stage",
        ));
    };
    let call = render_existential_continuation(
        types,
        *domain,
        &scope,
        "__kioContinuation",
        "__kioPacket",
    )?;
    let witness = types.witness(current, &scope, "H")?;
    let mut body = format!(
        "__kioChecked({witness}, as: (any __KioFunctionWitness).self).construct {{ __kioContinuation in return {call} }}"
    );
    for (witness, argument) in stages.into_iter().rev() {
        body = format!(
            "__kioChecked({witness}, as: (any __KioForallWitness).self).construct {{ {argument} in return {body} }}"
        );
    }
    Ok(format!(
        "{{ () -> Any in let __kioPacket = __kioChecked(__kioChecked({target}, as: (any __KioNominalValue).self).__kioValue, as: __KioExistential.self); return {body} }}()"
    ))
}

/// Render one exported method from its exact prepared callable plan and paired
/// source execution layout.
fn render_export_method(
    package: &Package<Routed>,
    entry: &ExportEntry,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
) -> Result<String, EmitError> {
    let site = prepared_site(prepared, &entry.site)?;
    let execution = live_execution(site)?;
    let render = PreparedSwiftRender {
        site,
        plan: site.plan().facade(),
        shapes,
    };
    let callable = render_prepared_callable(site, shapes, "H")?;
    let generic = super::skin::swift_generic_use(&swift_public_method_generics(&callable));
    let mut params = callable
        .param_types
        .iter()
        .enumerate()
        .map(|(index, ty)| format!("_ arg{index}: {}", escaping_param_ty(ty)))
        .collect::<Vec<_>>();
    params.extend(
        swift_method_type_selectors(&callable)
            .into_iter()
            .map(|(index, ty)| format!("asKioType{index} _: {ty}.Type = {ty}.self")),
    );
    let public_values = (0..callable.param_types.len())
        .map(|index| format!("arg{index}"))
        .collect::<Vec<_>>();
    let mut public_cursor = 0usize;
    let mut stage_body_args = Vec::<Vec<String>>::new();
    let semantic_entry = site.plan().entry();
    for (semantic, runtime) in semantic_entry
        .head_stages
        .iter()
        .zip(execution.head_stages())
    {
        match (semantic, runtime) {
            (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {
                stage_body_args.push(Vec::new());
            }
            (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) => {
                let end = public_cursor + layout.source_param_count();
                let values = public_values.get(public_cursor..end).ok_or_else(|| {
                    EmitError::internal("Swift export left its public parameter range")
                })?;
                stage_body_args.push(prepared_public_to_body_args(
                    render,
                    slots,
                    layout,
                    values,
                    &callable.scope,
                    0,
                )?);
                public_cursor = end;
            }
            _ => {
                return Err(EmitError::internal(
                    "Swift export semantic/runtime stage kind drift",
                ));
            }
        }
    }
    if public_cursor != public_values.len() {
        return Err(EmitError::internal(
            "Swift export did not consume its exact public parameters",
        ));
    }

    let values = prepared_values(render);
    let witness_scope = callable
        .scope
        .binders
        .iter()
        .map(|(binder, ty)| (*binder, format!("__KioNativeWitness<{ty}>()")))
        .collect::<super::value::Scope>();
    let internal = match &entry.body {
        ExportBody::ModuleFn { method } => {
            let mut applied = format!("self.{PACKAGE_BACKREF_MEMBER}.{method}()");
            let mut current = values.plan.root();
            for (runtime, args) in execution.head_stages().iter().zip(&stage_body_args) {
                match (runtime, values.plan.use_at(current)) {
                    (CallableExecutionStage::Type { .. }, ValueUse::Forall { binder, result }) => {
                        applied = format!("__kioAdvance({applied}, {})", witness_scope[binder]);
                        current = *result;
                    }
                    (
                        CallableExecutionStage::Value(_),
                        ValueUse::Function { domain, result, .. },
                    ) => {
                        let parts = values.plan.right_spine_parts(*domain, args.len(), false);
                        let witnesses = parts
                            .into_iter()
                            .map(|id| values.witness(id, &witness_scope, "H"))
                            .collect::<Result<Vec<_>, _>>()?;
                        let domain = if args.is_empty() {
                            "KioUnit()".to_owned()
                        } else {
                            format!(
                                "__kioProduct([{}], [{}])",
                                args.join(", "),
                                witnesses.join(", ")
                            )
                        };
                        let witness = values.witness(current, &witness_scope, "H")?;
                        applied = format!(
                            "__kioChecked({witness}, as: (any __KioFunctionWitness).self).invoke({applied}, {domain})"
                        );
                        current = *result;
                    }
                    _ => {
                        return Err(EmitError::internal(
                            "Swift export execution disagrees with its canonical value stages",
                        ));
                    }
                }
            }
            applied
        }
        ExportBody::NewtypeConstructor => {
            let payload = stage_body_args
                .iter()
                .flatten()
                .next()
                .cloned()
                .unwrap_or_else(|| "KioUnit()".to_owned());
            let result = values.plan.source(callable.ret_use);
            let ambient = witness_scope.keys().copied().collect::<Vec<_>>();
            let universal = values.plan.free_binders(result, &ambient);
            let hidden = ambient
                .iter()
                .filter(|binder| !universal.contains(binder))
                .map(|binder| witness_scope[binder].clone())
                .collect::<Vec<_>>();
            let payload = if hidden.is_empty() {
                payload
            } else {
                format!(
                    "__KioExistential(payload: {payload}, witnesses: [{}])",
                    hidden.join(", ")
                )
            };
            let witness = values.witness(result, &witness_scope, "H")?;
            format!(
                "__kioChecked({witness}, as: (any __KioNominalWitness).self).construct({payload})"
            )
        }
        ExportBody::NewtypeProjector => {
            let args = stage_body_args
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            let [target] = args.as_slice() else {
                return Err(EmitError::internal(
                    "Swift projector must consume one exact receiver",
                ));
            };
            if execution.direct_projector_compactable_foralls().is_empty() {
                format!("__kioChecked({target}, as: (any __KioNominalValue).self).__kioValue")
            } else {
                render_existential_projection(
                    &values,
                    values.plan.source(callable.ret_use),
                    &witness_scope,
                    target,
                )?
            }
        }
    };
    let method_name = export_method_name(&entry.leaf);
    let returned = if matches!(
        site.plan().facade().use_at(callable.ret_use),
        FacadeUse::Unit { .. }
    ) {
        format!("_ = {internal}; return KioUnit()")
    } else {
        let ty = render_prepared_use(render, callable.ret_use, &callable.scope, "H")?;
        format!("return __kioChecked({internal}, as: ({ty}).self)")
    };
    let mut out = String::new();
    push_doc(&mut out, boundary_docs::callable(package, site), "\t");
    out.push_str(&format!(
        "\tpublic func {method_name}{generic}({}) -> {} {{\n\t\t{returned}\n\t}}\n",
        params.join(", "),
        callable.ret_type,
    ));
    Ok(out)
}

pub(super) fn push_doc(out: &mut String, comment: Option<&crate::ast::DocComment>, indent: &str) {
    if let Some(comment) = comment {
        doc::push_line_comments(
            out,
            &doc::render(comment, doc::Dialect::SwiftDoc),
            indent,
            "///",
        );
    }
}

/// The exported property name for one role-bearing facade edge.
fn export_selector_name(selector: &FacadeSelector, root: bool) -> String {
    swift_ident(&selector.facade_name(root))
}

/// The exported method name for an export fn `leaf`, lowerCamelCased per
/// Swift's method-naming convention: the natural Kio snake_case
/// `make_pair` surfaces as `makePair`. Visibility is `public`, but the
/// *name* still follows Swift idiom — unlike Go, which capitalizes for
/// export.
fn export_method_name(leaf: &str) -> String {
    if has_readable_snake_components(leaf) {
        swift_ident(&host_name_core(leaf))
    } else {
        swift_ident(&format!("KioItem_{}", encode_host_identity(leaf)))
    }
}

/// A Kio identifier as a Swift identifier (keyword-escaped).
fn swift_ident(s: &str) -> String {
    if super::skin::is_swift_keyword(s) {
        format!("`{s}`")
    } else {
        s.to_owned()
    }
}

/// Render the top-level package-handle class: the host, the root
/// export-namespace properties, the init (wiring namespace back-pointers),
/// the root-level export methods, and every module fn as a method.
fn render_package_class(
    root: &ExportTree,
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    literal_shapes: &super::skin::SwiftLiteralContext<'_>,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str(&format!(
        "// {} is the package handle. It owns the host conformance for\n\
         // the lifetime of the package; exported items are invoked through\n\
         // its namespace properties.\n",
        names.handle
    ));
    out.push_str(&format!(
        "public final class {}<H: {}> {{\n",
        names.handle, names.host_ty
    ));
    out.push_str(&format!("\tlet {HOST_STORAGE_MEMBER}: H\n"));
    for selector in root.children.keys() {
        out.push_str(&format!(
            "\tpublic let {}: {}<H>\n",
            export_selector_name(selector, true),
            export_ns_type_name(std::slice::from_ref(selector))
        ));
    }
    // Init: store host, construct namespaces, wire back-pointers.
    out.push_str("\tinit(host: H) {\n");
    out.push_str(&format!("\t\tself.{HOST_STORAGE_MEMBER} = host\n"));
    for selector in root.children.keys() {
        out.push_str(&format!(
            "\t\tself.{} = {}<H>()\n",
            export_selector_name(selector, true),
            export_ns_type_name(std::slice::from_ref(selector))
        ));
    }
    wire_namespace_pointers(root, &mut Vec::new(), &mut out);
    out.push_str("\t}\n");

    // Root-level export methods (a `pub fn` in a top-level module).
    for e in &root.exports {
        out.push_str(&render_export_method(package, e, prepared, shapes)?);
    }

    // Module fns become methods on the package handle. Fan out per module.
    let module_pieces: Result<Vec<String>, EmitError> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(module_key, entry)| {
                render_module_fns(module_key, entry, package, prepared, shapes, literal_shapes)
            })
            .collect();
    let mut pieces = module_pieces?;
    pieces.sort();
    for piece in pieces {
        out.push_str(&piece);
    }

    out.push_str("}\n\n");
    Ok(out)
}

fn wire_namespace_pointers(node: &ExportTree, path: &mut Vec<FacadeSelector>, out: &mut String) {
    for (selector, child) in &node.children {
        path.push(selector.clone());
        let access: Vec<String> = path
            .iter()
            .enumerate()
            .map(|(index, edge)| export_selector_name(edge, index == 0))
            .collect();
        out.push_str(&format!(
            "\t\tself.{}.{PACKAGE_BACKREF_MEMBER} = self\n",
            access.join(".")
        ));
        wire_namespace_pointers(child, path, out);
        path.pop();
    }
}

/// Render the branded `create<Handle>` factory: instantiate the package
/// against a host.
fn render_create_package(names: &SwiftNames) -> String {
    format!(
        "// {factory} instantiates the package against a host conformance.\n\
         public func {factory}<H: {host_ty}>(host: H) -> {handle}<H> {{\n\
         \treturn {handle}<H>(host: host)\n\
         }}\n",
        factory = names.factory,
        host_ty = names.host_ty,
        handle = names.handle,
    )
}

/// Render every module fn in `entry` as a method on the package handle.
fn render_module_fns(
    module_key: &str,
    entry: &crate::pass::resolve::ModuleEntry<Routed>,
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    literal_shapes: &super::skin::SwiftLiteralContext<'_>,
) -> Result<String, EmitError> {
    let selective_imports = build_selective_imports(&entry.module, package);
    let qualified_imports = build_qualified_imports(&entry.module.imports);
    let mut out = String::new();
    for item in &entry.module.items {
        if let crate::ast::Item::FnDef(f) = item {
            let method = module_fn_method_name(module_key, &f.name);
            let mut emitter = BodyEmitter::new(
                module_key,
                &selective_imports,
                &qualified_imports,
                prepared,
                shapes,
                literal_shapes,
            );
            let full_type = f.sig.signature_ty(f.ret.clone(), f.meta.span);
            let body = emitter.callable_value(&f.sig, &full_type, &f.body)?;
            out.push_str(&format!(
                "\tfunc {method}() -> Any {{\n\t\treturn {body}\n\t}}\n"
            ));
        }
    }
    Ok(out)
}

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

type SelectiveImports = BTreeMap<String, String>;
type QualifiedImports = BTreeMap<String, String>;

fn build_qualified_imports(imports: &[crate::ast::Import]) -> QualifiedImports {
    let mut out = BTreeMap::new();
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

fn build_selective_imports(
    importer: &crate::ast::Module<Routed>,
    package: &Package<Routed>,
) -> SelectiveImports {
    crate::backends::selective_module_fn_import_owners(importer, package)
}

/// Walks a module fn body and renders Swift expression source over the
/// universal erased value model (`Any`). The enclosing method's `self` is
/// the package handle, so host / module calls reach the reserved host storage
/// member / `self.<method>`.
struct BodyEmitter<'a, 'p> {
    locals: Vec<String>,
    local_types: HashMap<String, Type<Routed>>,
    type_binders: Vec<(crate::ast::TypeParam, String)>,
    fresh: usize,
    module_key: &'a str,
    selective_imports: &'a SelectiveImports,
    qualified_imports: &'a QualifiedImports,
    prepared: &'a PreparedBoundaryCallableSites,
    facade_shapes: &'a super::skin::PreparedSwiftShapes<'p>,
    shapes: &'a super::skin::SwiftLiteralContext<'p>,
}

impl<'a, 'p> BodyEmitter<'a, 'p> {
    fn new(
        module_key: &'a str,
        selective_imports: &'a SelectiveImports,
        qualified_imports: &'a QualifiedImports,
        prepared: &'a PreparedBoundaryCallableSites,
        facade_shapes: &'a super::skin::PreparedSwiftShapes<'p>,
        shapes: &'a super::skin::SwiftLiteralContext<'p>,
    ) -> Self {
        BodyEmitter {
            locals: Vec::new(),
            local_types: HashMap::new(),
            type_binders: Vec::new(),
            fresh: 0,
            module_key,
            selective_imports,
            qualified_imports,
            prepared,
            facade_shapes,
            shapes,
        }
    }

    fn fresh_name(&mut self, class: &str) -> String {
        let index = self.fresh;
        self.fresh += 1;
        format!("__kio_{class}{index}")
    }

    fn expr_type(&self, expr: &Expr<Routed>) -> Option<Type<Routed>> {
        let type_params = self
            .type_binders
            .iter()
            .map(|(param, _)| param.clone())
            .collect::<Vec<_>>();
        RoutedBodyTypes {
            package: self.shapes.package(),
            module: &self.shapes.package().module(self.module_key)?.module,
            type_params: &type_params,
            values: &self.local_types,
        }
        .value_type(expr)
    }

    fn value_plan(&self, ty: &Type<Routed>) -> Result<ValuePlan, EmitError> {
        let module = &self
            .shapes
            .package()
            .module(self.module_key)
            .expect("a routed Swift body belongs to a package module")
            .module;
        let ambient = self
            .type_binders
            .iter()
            .map(|(param, _)| param.clone())
            .collect::<Vec<_>>();
        ValuePlan::from_routed_type(self.shapes.package(), module, &ambient, ty).map_err(|error| {
            EmitError::internal(format!("Swift body value planning failed: {error}"))
        })
    }

    fn witness(&self, ty: &Type<Routed>) -> Result<String, EmitError> {
        let plan = self.value_plan(ty)?;
        let scope = plan
            .ambient_binders()
            .iter()
            .copied()
            .zip(self.type_binders.iter().map(|(_, value)| value.clone()))
            .collect();
        super::value::Types {
            plan: &plan,
            shapes: self.facade_shapes,
            retained_nominals: None,
            origin: BoundaryFacadeSupportOrigin::Live,
        }
        .witness(plan.root(), &scope, "H")
    }

    fn callable_value(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        full_type: &Type<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let plan = self.value_plan(full_type)?;
        let types = super::value::Types {
            plan: &plan,
            shapes: self.facade_shapes,
            retained_nominals: None,
            origin: BoundaryFacadeSupportOrigin::Live,
        };
        let mut scope = plan
            .ambient_binders()
            .iter()
            .copied()
            .zip(self.type_binders.iter().map(|(_, value)| value.clone()))
            .collect::<super::value::Scope>();
        let saved_binders = self.type_binders.len();
        let saved_locals = self.locals.len();
        let saved_types = self.local_types.clone();
        let mut current = plan.root();
        let mut stages = Vec::new();
        for group in sig.canonical_groups() {
            match group {
                crate::ast::SignatureGroupRef::Type(params) => {
                    for param in params {
                        let crate::ast::SignatureParam::Type(param) = param else {
                            unreachable!("a canonical type group contains type parameters")
                        };
                        let ValueUse::Forall { binder, result } = plan.use_at(current) else {
                            return Err(EmitError::internal(
                                "Swift signature type stage disagrees with its value plan",
                            ));
                        };
                        let witness = types.witness(current, &scope, "H")?;
                        let local = self.fresh_name("binder");
                        stages.push((true, witness, local.clone(), Vec::new()));
                        scope.insert(*binder, local.clone());
                        self.type_binders.push((param.clone(), local));
                        current = *result;
                    }
                }
                crate::ast::SignatureGroupRef::Value(params) => {
                    let ValueUse::Function { result, .. } = plan.use_at(current) else {
                        return Err(EmitError::internal(
                            "Swift signature value stage disagrees with its value plan",
                        ));
                    };
                    let witness = types.witness(current, &scope, "H")?;
                    let local = self.fresh_name("domain");
                    let mut names = Vec::new();
                    for param in params {
                        if let crate::ast::SignatureParam::Value(param) = param {
                            self.locals.push(param.name.clone());
                            if let Some(ty) = &param.ty {
                                self.local_types.insert(param.name.clone(), ty.clone());
                            }
                            names.push(swift_local_ident(&param.name));
                        }
                    }
                    stages.push((false, witness, local, names));
                    current = *result;
                }
            }
        }
        if stages.is_empty() {
            let ValueUse::Function { .. } = plan.use_at(current) else {
                return Err(EmitError::internal(
                    "an empty Swift function signature has a Unit domain",
                ));
            };
            stages.push((
                false,
                types.witness(current, &scope, "H")?,
                self.fresh_name("domain"),
                Vec::new(),
            ));
        }
        let emitted = self.emit_expr(body);
        self.type_binders.truncate(saved_binders);
        self.locals.truncate(saved_locals);
        self.local_types = saved_types;
        let mut result = emitted?;
        for (type_stage, witness, argument, names) in stages.into_iter().rev() {
            if type_stage {
                result = format!(
                    "__kioChecked({witness}, as: (any __KioForallWitness).self).construct {{ {argument} in return {result} }}"
                );
            } else {
                let mut bindings = String::new();
                for (index, name) in names.iter().enumerate() {
                    let value = if names.len() == 1 {
                        argument.clone()
                    } else {
                        format!("kioProductSlot({argument}, {index}, {})", names.len())
                    };
                    bindings.push_str(&format!("let {name}: Any = {value}; _ = {name}; "));
                }
                result = format!(
                    "__kioChecked({witness}, as: (any __KioFunctionWitness).self).construct {{ {argument} in {bindings}return {result} }}"
                );
            }
        }
        Ok(result)
    }

    /// Render one Routed-phase expression to a Swift expression string.
    fn emit_expr(&mut self, e: &Expr<Routed>) -> Result<String, EmitError> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Unit { .. } => Ok("(KioUnit())".to_owned()),
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
            // A literal as a general body value is boxed to `Any`.
            Expr::StrLit {
                value, annotation, ..
            } => {
                let role = self.literal_role(Some(annotation))?;
                if role != Role::Str {
                    return Err(EmitError::unsupported(
                        "Swift emitter: string literal annotation is not role(str)",
                    ));
                }
                let exact = self.exact_host_type_name(annotation)?;
                Ok(format!("(({} as {exact}) as Any)", swift_string_lit(value)))
            }
            Expr::IntLit {
                digits, annotation, ..
            } => Ok(format!(
                "({} as Any)",
                self.emit_int_lit(digits, Some(annotation))?
            )),
            Expr::FloatLit {
                digits, annotation, ..
            } => Ok(format!(
                "({} as Any)",
                self.emit_float_lit(digits, Some(annotation))?
            )),
            Expr::BoolLit {
                value, annotation, ..
            } => {
                let role = self.literal_role(Some(annotation))?;
                if role != Role::Bool {
                    return Err(EmitError::unsupported(
                        "Swift emitter: Boolean literal annotation is not role(bool)",
                    ));
                }
                let exact = self.exact_host_type_name(annotation)?;
                Ok(format!("(({value} as {exact}) as Any)"))
            }
            Expr::Let {
                name, value, body, ..
            } => {
                let value_ty = self.expr_type(value);
                let v = self.emit_expr(value)?;
                let ident = swift_local_ident(name);
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
                // An immediately-invoked closure so a `let` is an
                // expression. The binder is `Any` (the universal erased
                // value); `_ = ident` suppresses an unused-binding warning.
                Ok(format!(
                    "{{ () -> Any in let {ident}: Any = {v}; _ = {ident}; return {b} }}()"
                ))
            }
            Expr::Seq { value, body, .. } => {
                let v = self.emit_expr(value)?;
                let b = self.emit_expr(body)?;
                Ok(format!("{{ () -> Any in _ = {v}; return {b} }}()"))
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
            Expr::LowBoundRef { name, .. } => Ok(swift_local_ident(name)),
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => self.emit_conditional(cond, then_branch, else_branch),
            Expr::EnrichedTuple {
                items, synth_ty, ..
            } => self.emit_tuple_typed(items, synth_ty),
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                synth_ty,
                ..
            } => self.emit_inject(payload, *variant, *variants, synth_ty),
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => self.emit_match(scrutinee, arms, scrutinee_ty),
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                ..
            } => {
                let field = self.emit_project(target, *index, *arity)?;
                Ok(format!(
                    "__kioChecked({field}, as: (any __KioNominalValue).self).__kioValue"
                ))
            }
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                meta,
                ..
            } => {
                let module = self
                    .selective_imports
                    .get(mangled)
                    .map(String::as_str)
                    .unwrap_or(self.module_key);
                self.emit_module_call(module, mangled, type_args, args, sig, meta.span)
            }
            Expr::LowQualifiedModuleCall {
                mangled,
                type_args,
                args,
                sig,
                meta,
                ..
            } => {
                let (alias, leaf) = mangled.split_once('.').ok_or_else(|| {
                    EmitError::internal("a qualified Swift module call has an import prefix")
                })?;
                let module = self.qualified_imports.get(alias).ok_or_else(|| {
                    EmitError::internal("a qualified Swift module call has a resolved import")
                })?;
                self.emit_module_call(module, leaf, type_args, args, sig, meta.span)
            }
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                payload,
                ..
            } => self.emit_newtype_member(None, newtype, member, type_args, payload),
            Expr::LowNewtypeProj {
                newtype,
                member,
                type_args,
                target,
                ..
            } => self.emit_newtype_member(None, newtype, member, type_args, target),
            Expr::LowAbsurdCall { value_arg, .. } => {
                let arg = self.emit_expr(value_arg)?;
                Ok(format!(
                    "{{ () -> Any in _ = {arg}; fatalError(\"kio: __absurd__ reached\") }}()"
                ))
            }
            Expr::FnExpr { sig, body, .. } => {
                let full_type = self
                    .expr_type(e)
                    .ok_or_else(|| EmitError::internal("Swift closure has no phase-owned type"))?;
                self.callable_value(sig, &full_type, body)
            }
            Expr::LowClosureCall {
                name,
                type_args,
                args,
                ..
            } => {
                let callee = swift_local_ident(name);
                let ty = self
                    .local_types
                    .get(name)
                    .ok_or_else(|| EmitError::internal("Swift closure call has no lexical type"))?;
                let ty = crate::backends::reconstruct::instantiate_type_stages(ty, type_args)
                    .ok_or_else(|| {
                        EmitError::internal("Swift closure type applications cross a value stage")
                    })?;
                let callee = self.emit_type_applications(callee, type_args)?;
                self.emit_value_call(&callee, &ty, args)
            }
            Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } => {
                let c = self.emit_expr(callee)?;
                let ty = self.expr_type(callee).ok_or_else(|| {
                    EmitError::internal("Swift indirect call has no phase-owned callee type")
                })?;
                let ty = crate::backends::reconstruct::instantiate_type_stages(&ty, type_args)
                    .ok_or_else(|| {
                        EmitError::internal("Swift indirect type applications cross a value stage")
                    })?;
                let c = self.emit_type_applications(c, type_args)?;
                self.emit_value_call(&c, &ty, args)
            }
            Expr::LowTypeApplication {
                callee, type_arg, ..
            } => {
                let c = self.emit_expr(callee)?;
                self.emit_type_applications(c, std::slice::from_ref(type_arg))
            }
            Expr::LowCpsProjectorApply {
                receiver,
                continuation,
                continuation_ty,
                ..
            } => self.emit_cps_projector_apply(receiver, continuation, continuation_ty),
            Expr::LowHostFnValueRef {
                name,
                module_path,
                sig,
                ret_ty,
                ..
            } => self.emit_host_fn_value_ref(name, module_path, sig, ret_ty),
            Expr::LowModuleFnValueRef { mangled, sig, .. } => {
                self.emit_module_fn_value_ref(mangled, sig)
            }
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                payload,
                ..
            } => self.emit_newtype_member(Some(module_path), newtype, member, type_args, payload),
        }
    }

    fn emit_newtype_member(
        &mut self,
        module: Option<&str>,
        name: &str,
        member: &str,
        arguments: &[Type<Routed>],
        payload: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let caller_module = &self
            .shapes
            .package()
            .module(self.module_key)
            .expect("a routed Swift body belongs to a package module")
            .module;
        let type_params = self
            .type_binders
            .iter()
            .map(|(param, _)| param.clone())
            .collect::<Vec<_>>();
        let types = RoutedBodyTypes {
            package: self.shapes.package(),
            module: caller_module,
            type_params: &type_params,
            values: &self.local_types,
        };
        let mut segments = module
            .map(|module| module.split('/').map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        segments.push(name.to_owned());
        let qualified = types
            .nominal(&Type::synth_path(segments, Vec::new(), payload.span()))
            .ok_or_else(|| EmitError::internal("a resolved Swift member has a nominal owner"))?;
        let arguments = arguments
            .iter()
            .map(|argument| types.canonical(argument))
            .collect::<Vec<_>>();
        let module = qualified.module_segments().join("/");
        let Some(crate::backends::value_plan::ValueNominalDeclaration::Newtype(declaration)) =
            crate::backends::value_plan::nominal_declaration(self.shapes.package(), &qualified)
        else {
            return Err(EmitError::internal(
                "a resolved Swift newtype member lost its declaration",
            ));
        };
        let result_ty = crate::backends::reconstruct::newtype_member_result(
            self.shapes.package(),
            &module,
            declaration,
            member,
            &arguments,
        )
        .ok_or_else(|| EmitError::internal("Swift newtype member lost its instantiated result"))?;
        let value = self.emit_expr(payload)?;
        if member == declaration.constructor.name {
            let hidden = arguments
                .get(declaration.type_params.len()..)
                .ok_or_else(|| {
                    EmitError::internal("Swift constructor lost its universal arguments")
                })?;
            let payload = if hidden.is_empty() {
                value
            } else {
                let witnesses = hidden
                    .iter()
                    .map(|ty| self.witness(ty))
                    .collect::<Result<Vec<_>, _>>()?;
                format!(
                    "__KioExistential(payload: {value}, witnesses: [{}])",
                    witnesses.join(", ")
                )
            };
            let witness = self.witness(&result_ty)?;
            Ok(format!(
                "__kioChecked({witness}, as: (any __KioNominalWitness).self).construct({payload})"
            ))
        } else if member == declaration.projector.name {
            if declaration.existential_params.is_empty() {
                Ok(format!(
                    "__kioChecked({value}, as: (any __KioNominalValue).self).__kioValue"
                ))
            } else {
                let plan = self.value_plan(&result_ty)?;
                let scope = plan
                    .ambient_binders()
                    .iter()
                    .copied()
                    .zip(self.type_binders.iter().map(|(_, value)| value.clone()))
                    .collect();
                let types = super::value::Types {
                    plan: &plan,
                    shapes: self.facade_shapes,
                    retained_nominals: None,
                    origin: BoundaryFacadeSupportOrigin::Live,
                };
                render_existential_projection(&types, plan.root(), &scope, &value)
            }
        } else {
            Err(EmitError::internal(
                "a resolved Swift newtype member is neither its constructor nor projector",
            ))
        }
    }

    fn emit_cps_projector_apply(
        &mut self,
        receiver: &Expr<Routed>,
        continuation: &Expr<Routed>,
        continuation_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let receiver = self.emit_expr(receiver)?;
        let continuation = self.emit_expr(continuation)?;
        let plan = self.value_plan(continuation_ty)?;
        let scope = plan
            .ambient_binders()
            .iter()
            .copied()
            .zip(self.type_binders.iter().map(|(_, value)| value.clone()))
            .collect();
        let types = super::value::Types {
            plan: &plan,
            shapes: self.facade_shapes,
            retained_nominals: None,
            origin: BoundaryFacadeSupportOrigin::Live,
        };
        let call = render_existential_continuation(
            &types,
            plan.root(),
            &scope,
            "__kioContinuation",
            "__kioPacket",
        )?;
        Ok(format!(
            "{{ () -> Any in let __kioPacket = __kioChecked(__kioChecked({receiver}, as: (any __KioNominalValue).self).__kioValue, as: __KioExistential.self); let __kioContinuation: Any = {continuation}; return {call} }}()"
        ))
    }

    /// `LowHostFnValueRef` → a Swift closure forwarding through the host
    /// record with the same FFI conversion as a direct host call. The
    /// internal value keeps one nullary stage per type binder and one layer
    /// per value group; only the deepest layer invokes the value-only host
    /// method.
    fn emit_prepared_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
    ) -> Result<String, EmitError> {
        let site_id = host_function_site_id(module_path, name);
        let site = prepared_site(self.prepared, &site_id)?;
        let execution = live_execution(site)?;
        let render = PreparedSwiftRender {
            site,
            plan: site.plan().facade(),
            shapes: self.facade_shapes,
        };
        let values = prepared_values(render);
        let mut current = values.plan.root();
        let mut scope = super::value::Scope::new();
        let mut stages = Vec::new();
        let mut arguments = Vec::new();
        let mut witnesses = Vec::new();
        for stage in execution.head_stages() {
            let witness = values.witness(current, &scope, "H")?;
            let argument = self.fresh_name("host_stage");
            match (stage, values.plan.use_at(current)) {
                (CallableExecutionStage::Type { .. }, ValueUse::Forall { binder, result }) => {
                    witnesses.push(argument.clone());
                    scope.insert(*binder, argument.clone());
                    stages.push((true, witness, argument, Vec::new()));
                    current = *result;
                }
                (CallableExecutionStage::Value(layout), ValueUse::Function { result, .. }) => {
                    let names = (0..layout.body_abi_arity())
                        .map(|index| format!("{argument}_{index}"))
                        .collect::<Vec<_>>();
                    arguments.extend(names.clone());
                    stages.push((false, witness, argument, names));
                    current = *result;
                }
                _ => {
                    return Err(EmitError::internal(
                        "Swift host execution stages disagree with their canonical value plan",
                    ));
                }
            }
        }
        let mut body = self.opened_host_call(render, name, module_path, &witnesses, &arguments)?;
        for (type_stage, witness, argument, names) in stages.into_iter().rev() {
            if type_stage {
                body = format!(
                    "__kioChecked({witness}, as: (any __KioForallWitness).self).construct {{ {argument} in return {body} }}"
                );
            } else {
                let mut bindings = String::new();
                for (index, name) in names.iter().enumerate() {
                    let value = if names.len() == 1 {
                        argument.clone()
                    } else {
                        format!("kioProductSlot({argument}, {index}, {})", names.len())
                    };
                    bindings.push_str(&format!("let {name}: Any = {value}; "));
                }
                body = format!(
                    "__kioChecked({witness}, as: (any __KioFunctionWitness).self).construct {{ {argument} in {bindings}return {body} }}"
                );
            }
        }
        Ok(body)
    }

    fn opened_host_call(
        &self,
        render: PreparedSwiftRender<'_, '_>,
        name: &str,
        module_path: &str,
        witnesses: &[String],
        arguments: &[String],
    ) -> Result<String, EmitError> {
        let callable = render_prepared_callable(render.site, self.facade_shapes, "H")?;
        let execution = live_execution(render.site)?;
        let type_selectors = swift_method_type_selectors(&callable)
            .into_iter()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut scope = callable.scope;
        if scope.binders.len() != witnesses.len() {
            return Err(EmitError::internal(
                "Swift host call did not retain each declared type witness",
            ));
        }
        for (index, native) in scope.binders.values_mut().enumerate() {
            *native = format!("W{index}.Native");
        }
        let mut cursor = 0;
        let mut public_args = Vec::new();
        for (semantic, runtime) in render
            .site
            .plan()
            .entry()
            .head_stages
            .iter()
            .zip(execution.head_stages())
        {
            if let (
                BoundaryCallableHeadStage::Value { slots },
                CallableExecutionStage::Value(layout),
            ) = (semantic, runtime)
            {
                let end = cursor + layout.body_abi_arity();
                let locals = (cursor..end)
                    .map(|index| format!("argument{index}"))
                    .collect::<Vec<_>>();
                public_args.extend(prepared_body_to_public_args(
                    render, slots, layout, &locals, &scope, 0,
                )?);
                cursor = end;
            }
        }
        if cursor != arguments.len() {
            return Err(EmitError::internal(
                "Swift opened host call value arity drift",
            ));
        }
        let method = host_member_name(module_path, name);
        public_args.extend(
            type_selectors
                .into_iter()
                .map(|index| format!("asKioType{index}: W{index}.Native.self")),
        );
        let ty = render_prepared_use(render, callable.ret_use, &scope, "H")?;
        let call = format!(
            "self.{HOST_STORAGE_MEMBER}.{method}({})",
            public_args.join(", ")
        );
        let returned = if matches!(render.plan.use_at(callable.ret_use), FacadeUse::Unit { .. }) {
            format!("{call}; return KioUnit()")
        } else {
            format!("return (({call}) as {ty}) as Any")
        };
        let generic = (0..witnesses.len())
            .map(|index| format!("W{index}: __KioTypeWitness"))
            .collect::<Vec<_>>();
        let mut params = (0..witnesses.len())
            .map(|index| format!("_ witness{index}: W{index}"))
            .collect::<Vec<_>>();
        params.extend((0..arguments.len()).map(|index| format!("_ argument{index}: Any")));
        let witness_bindings = witnesses
            .iter()
            .enumerate()
            .map(|(index, witness)| {
                format!("let __kioOpenedType{index}: any __KioTypeWitness = {witness}; ")
            })
            .collect::<String>();
        let mut actual = (0..witnesses.len())
            .map(|index| format!("__kioOpenedType{index}"))
            .collect::<Vec<_>>();
        actual.extend_from_slice(arguments);
        Ok(format!(
            "{{ () -> Any in func invoke{}({}) -> Any {{ {returned} }}; {witness_bindings}return invoke({}) }}()",
            super::skin::swift_generic_use(&generic),
            params.join(", "),
            actual.join(", ")
        ))
    }

    fn emit_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
        _sig: &crate::ast::Signature<Routed>,
        _ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        self.emit_prepared_host_fn_value_ref(name, module_path)
    }

    /// `LowModuleFnValueRef` → a Swift closure forwarding to the resolved
    /// module-fn method on `self`. Params + result are all erased `Any`.
    fn emit_module_fn_value_ref(
        &mut self,
        mangled: &str,
        _sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let module = self
            .selective_imports
            .get(mangled)
            .map(String::as_str)
            .unwrap_or(self.module_key);
        let method = module_fn_method_name(module, mangled);
        Ok(format!("self.{method}()"))
    }

    fn emit_type_applications(
        &self,
        mut callee: String,
        arguments: &[Type<Routed>],
    ) -> Result<String, EmitError> {
        for argument in arguments {
            callee = format!("__kioAdvance({callee}, {})", self.witness(argument)?);
        }
        Ok(callee)
    }

    fn product_from_values(
        &self,
        ty: &Type<Routed>,
        values: &[String],
    ) -> Result<String, EmitError> {
        if values.is_empty() {
            return Ok("KioUnit()".to_owned());
        }
        let witnesses = self.structural_witnesses(ty, values.len(), false)?;
        Ok(swift_product_expression(values, &witnesses))
    }

    fn structural_witnesses(
        &self,
        ty: &Type<Routed>,
        arity: usize,
        sum: bool,
    ) -> Result<Vec<String>, EmitError> {
        let plan = self.value_plan(ty)?;
        let scope = plan
            .ambient_binders()
            .iter()
            .copied()
            .zip(self.type_binders.iter().map(|(_, value)| value.clone()))
            .collect();
        let types = super::value::Types {
            plan: &plan,
            shapes: self.facade_shapes,
            retained_nominals: None,
            origin: BoundaryFacadeSupportOrigin::Live,
        };
        plan.right_spine_parts(plan.root(), arity, sum)
            .into_iter()
            .map(|id| types.witness(id, &scope, "H"))
            .collect()
    }

    fn emit_value_call(
        &mut self,
        callee: &str,
        callee_type: &Type<Routed>,
        args: &[Expr<Routed>],
    ) -> Result<String, EmitError> {
        let Type::Function { param, .. } = callee_type else {
            return Err(EmitError::internal(
                "Swift value application must follow a resolved function stage",
            ));
        };
        let values = args
            .iter()
            .map(|arg| self.emit_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let argument = self.product_from_values(param, &values)?;
        let witness = self.witness(callee_type)?;
        Ok(format!(
            "{{ () -> Any in let __kioCallee: Any = {callee}; let __kioArgument: Any = {argument}; return __kioChecked({witness}, as: (any __KioFunctionWitness).self).invoke(__kioCallee, __kioArgument) }}()"
        ))
    }

    fn emit_module_call(
        &mut self,
        module: &str,
        name: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        span: crate::span::Span,
    ) -> Result<String, EmitError> {
        let full_type = crate::backends::reconstruct::module_function_scheme(
            self.shapes.package(),
            module,
            name,
            span,
        )
        .ok_or_else(|| {
            EmitError::internal("Swift resolved module call lost its declared scheme")
        })?;
        let ty = crate::backends::reconstruct::instantiate_type_stages(&full_type, type_args)
            .ok_or_else(|| {
                EmitError::internal("Swift module type applications cross a value stage")
            })?;
        let method = module_fn_method_name(module, name);
        let call = self.emit_type_applications(format!("self.{method}()"), type_args)?;
        let first_arity = value_group_arities(sig)[0];
        if !sig.has_value_group() || (args.is_empty() && first_arity > 0) {
            if !args.is_empty() {
                return Err(EmitError::internal(
                    "a type-only module head has no value arguments",
                ));
            }
            return Ok(call);
        }
        if args.len() != first_arity {
            return Err(EmitError::internal(
                "a typed Routed module call must complete exactly its first value group",
            ));
        }
        self.emit_value_call(&call, &ty, args)
    }

    /// `if cond { then } else { else }` → an immediately-invoked Swift
    /// closure with a real `if`/`else`.
    fn emit_conditional(
        &mut self,
        cond: &Expr<Routed>,
        then_branch: &Expr<Routed>,
        else_branch: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let c = self.emit_bool(cond)?;
        let t = self.emit_expr(then_branch)?;
        let e = self.emit_expr(else_branch)?;
        Ok(format!(
            "{{ () -> Any in if {c} {{ return {t} }} else {{ return {e} }} }}()"
        ))
    }

    /// Recover the condition's exact role(bool) host type from Routed
    /// annotations, then compare it with that type's Boolean literal.
    fn emit_bool(&mut self, e: &Expr<Routed>) -> Result<String, EmitError> {
        let ty = self.expr_type(e).ok_or_else(|| {
            EmitError::unsupported(
                "Swift emitter: cannot reconstruct an enriched conditional's exact Boolean type \
                 (compiler bug)",
            )
        })?;
        if self.atom_role_of(&ty) != Some(Role::Bool) {
            return Err(EmitError::unsupported(
                "Swift emitter: enriched conditional does not carry a role(bool) condition \
                 (compiler bug)",
            ));
        }
        let exact = self.exact_host_type_name(&ty)?;
        let value = self.emit_expr(e)?;
        Ok(format!("({value} as! {exact}) == (true as {exact})"))
    }

    /// A product `(A & B & C)` → the nested-binary erased value
    /// Exact binary native nodes carried through the erased interface.
    fn emit_tuple_typed(
        &mut self,
        items: &[Expr<Routed>],
        synth_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        if items.is_empty() {
            return Ok("KioUnit()".to_owned());
        }
        let witnesses = self.structural_witnesses(synth_ty, items.len(), false)?;
        let construct = |values: &[String]| swift_product_expression(values, &witnesses);
        let item_refs = items.iter().collect::<Vec<_>>();
        if let Some(plan) =
            bound_product_rebuild_plan(&item_refs, NominalValueRepresentation::Retained)
        {
            return render_cached_product_rebuild(
                &plan,
                |slot| format!("__slots[{slot}]"),
                |index| self.emit_expr(&items[index]),
                construct,
                |source, arity, tuple| {
                    let source = swift_local_ident(source);
                    format!(
                        "{{ () -> Any in let __slots = kioProductSlots({source}, {arity}); return {tuple} }}()"
                    )
                },
            );
        }
        let values = items
            .iter()
            .map(|item| self.emit_expr(item))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(construct(&values))
    }

    /// Project slot `index` (of `arity`) from the erased nested-binary product
    /// with the bounded runtime helper.
    fn emit_project(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
    ) -> Result<String, EmitError> {
        let t = self.emit_expr(target)?;
        Ok(format!("kioProductSlot(({t}), {index}, {arity})"))
    }

    /// Inject `payload` as variant `variant` of `variants`.
    fn emit_inject(
        &mut self,
        payload: &Expr<Routed>,
        variant: usize,
        variants: usize,
        synth_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let p = self.emit_expr(payload)?;
        let witnesses = self.structural_witnesses(synth_ty, variants, true)?;
        Ok(format!(
            "__kioInject({p}, {variant}, [{}])",
            witnesses.join(", ")
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
                "Swift emitter: EnrichedMatch with no arms; structural_recovery builds every \
                 EnrichedMatch from a sum's variants (recover_either / recover_dynamic_right \
                 yield >= 2 arms) and routes an uninhabited scrutinee through \
                 __absurd__/LowAbsurdCall, so a zero-arm match is unreachable"
            );
        }
        let s = self.emit_expr(scrutinee)?;
        let mut body = String::new();
        body.push_str(&format!("let __m = {s}; "));
        body.push_str(&format!(
            "let (__variant, __payload) = kioSumPayload(__m, {n}); switch __variant {{ "
        ));
        for (i, arm) in arms.iter().enumerate() {
            let ident = swift_local_ident(&arm.param);
            self.locals.push(arm.param.clone());
            let prior_ty = self.local_types.remove(&arm.param);
            let payload_ty = Type::right_spine_sum_slot_for_arity(scrutinee_ty, i, n)
                .unwrap_or_else(|| {
                    unreachable!(
                        "Swift emitter: enriched match arm has no corresponding payload type"
                    )
                })
                .clone();
            self.local_types.insert(arm.param.clone(), payload_ty);
            let b = self.emit_expr(&arm.body);
            self.local_types.remove(&arm.param);
            if let Some(ty) = prior_ty {
                self.local_types.insert(arm.param.clone(), ty);
            }
            self.locals.pop();
            let b = b?;
            body.push_str(&format!(
                "case {i}: let {ident} = __payload; _ = {ident}; return {b}; "
            ));
        }
        body.push_str("default: fatalError(\"kio erased body: invalid sum variant\") }");
        Ok(format!("{{ () -> Any in {body} }}()"))
    }

    /// `LowHostCall` → the reserved host-storage member's `<member>(<args>)`.
    /// Each value-arg is
    /// narrowed from the erased `Any` to the host method's concrete Swift
    /// parameter type. A `()`-returning host fn yields `KioUnit()`.
    fn emit_low_host_call(
        &mut self,
        name: &str,
        module_path: &str,
        type_args: &[Type<Routed>],
        args: &[Expr<Routed>],
        _sig: &crate::ast::Signature<Routed>,
        _ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let site_id = host_function_site_id(module_path, name);
        let site = prepared_site(self.prepared, &site_id)?;
        let render = PreparedSwiftRender {
            site,
            plan: site.plan().facade(),
            shapes: self.facade_shapes,
        };
        let witnesses = type_args
            .iter()
            .map(|ty| self.witness(ty))
            .collect::<Result<Vec<_>, _>>()?;
        let arguments = args
            .iter()
            .map(|arg| self.emit_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;
        self.opened_host_call(render, name, module_path, &witnesses, &arguments)
    }

    /// Resolve an exact nullary host atom to its literal role, if any.
    fn atom_role_of(&self, ty: &Type<Routed>) -> Option<Role> {
        self.shapes
            .exact_host_type_of(ty)
            .and_then(|(_, _, role)| role)
    }

    fn exact_host_type_name(&self, ty: &Type<Routed>) -> Result<String, EmitError> {
        let Some((module, name, _)) = self.shapes.exact_host_type_of(ty) else {
            return Err(EmitError::unsupported(
                "Swift emitter: literal/host slot does not name an exact nullary host type",
            ));
        };
        Ok(format!("H.{}", host_type_member_name(&module, &name)))
    }

    fn exact_integer_literal(&self, exact: &str, role: Option<Role>, digits: &str) -> String {
        match role {
            Some(Role::F32 | Role::F64) => format!("({digits}.0 as {exact})"),
            _ => format!("({digits} as {exact})"),
        }
    }

    fn emit_int_lit(
        &self,
        digits: &str,
        annotation: Option<&Type<Routed>>,
    ) -> Result<String, EmitError> {
        let role = self.literal_role(annotation)?;
        let annotation = annotation.ok_or_else(|| {
            EmitError::unsupported("Swift emitter: integer literal without a host type")
        })?;
        let exact = self.exact_host_type_name(annotation)?;
        Ok(self.exact_integer_literal(&exact, Some(role), digits))
    }

    fn emit_float_lit(
        &self,
        digits: &str,
        annotation: Option<&Type<Routed>>,
    ) -> Result<String, EmitError> {
        let _role = self.literal_role(annotation)?;
        let annotation = annotation.ok_or_else(|| {
            EmitError::unsupported("Swift emitter: float literal without a host type")
        })?;
        let exact = self.exact_host_type_name(annotation)?;
        Ok(format!("({digits} as {exact})"))
    }

    fn literal_role(&self, annotation: Option<&Type<Routed>>) -> Result<Role, EmitError> {
        match annotation.and_then(|ty| self.atom_role_of(ty)) {
            Some(role) => Ok(role),
            None => Err(EmitError::unsupported(
                "Swift emitter: numeric literal without a role annotation",
            )),
        }
    }
}

/// Render a Kio string value as a Swift double-quoted string literal.
fn swift_string_lit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Swift local identifier for a Kio binder `name`. Prefixed with `k_`
/// so it never collides with a Swift keyword or the emitter's own
/// identifiers.
fn swift_local_ident(name: &str) -> String {
    format!("k_{name}")
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::backends::boundary_facade::QualifiedTypeName;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::structural_recovery;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    fn assert_no_public_any(emitted: &SwiftPackage) {
        assert!(
            !emitted.host_swift.contains("Any") && !emitted.ffi_swift.contains("Any"),
            "host:\n{}\nffi:\n{}",
            emitted.host_swift,
            emitted.ffi_swift
        );
        for source in [
            &emitted.pkg_swift,
            &emitted.shapes_swift,
            &emitted.runtime_swift,
        ] {
            for line in source.lines().filter(|line| line.contains("public ")) {
                let signature = line.split(" {").next().unwrap_or(line);
                assert!(
                    !signature.contains("Any"),
                    "public Swift declaration leaks Any: {signature}\n\n{source}"
                );
            }
        }
    }

    #[test]
    fn prepared_wide_product_uses_bounded_runtime_nesting() {
        let values = (0..255)
            .map(|index| format!("v{index}"))
            .collect::<Vec<_>>();
        let witnesses = vec!["witness".to_owned(); values.len()];
        let rendered = swift_product_expression(&values, &witnesses);

        assert!(rendered.starts_with("__kioProduct([v0, v1, v2, "));
        assert!(rendered.contains(", v252, v253, v254], [witness, "));
        assert_eq!(rendered.matches('[').count(), 2, "{rendered}");
        assert!(!rendered.contains("as [Any]"), "{rendered}");
        assert!(
            super::super::runtime::RUNTIME_SUPPORT_FILE_CONTENT.contains(
                "func __kioProduct(_ slots: [Any], _ witnesses: [any __KioTypeWitness]) -> Any"
            )
        );
    }

    #[test]
    fn wide_canonical_types_and_witnesses_use_linear_output() {
        use crate::ast::Meta;
        use crate::backends::boundary_facade::PreparedBoundaryScheme;
        let package = single_api_package("module api; pub fn unit() -> . { () }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let shapes = super::super::skin::PreparedSwiftShapes::new(&prepared, "PkgHost", &package);
        let span = crate::span::Span::new(0, 0);
        let width = 2048;
        for sum in [false, true] {
            let mut ty = Type::Unit {
                meta: Meta::new(span),
            };
            for _ in 1..width {
                let left = Box::new(Type::Unit {
                    meta: Meta::new(span),
                });
                ty = if sum {
                    Type::Sum {
                        left,
                        right: Box::new(ty),
                        meta: Meta::new(span),
                    }
                } else {
                    Type::Product {
                        left,
                        right: Box::new(ty),
                        meta: Meta::new(span),
                    }
                };
            }
            let nominals = Default::default();
            let facade = BoundaryFacadePlan::from_prepared_scheme(
                PreparedBoundaryScheme::try_new(&ty, &nominals).unwrap(),
            );
            let plan = ValuePlan::from_facade(&facade);
            let types = super::super::value::Types {
                plan: &plan,
                shapes: &shapes,
                retained_nominals: None,
                origin: BoundaryFacadeSupportOrigin::Live,
            };
            let scope = Default::default();
            let native = types.native(plan.root(), &scope, "H").unwrap();
            let witness = types.witness(plan.root(), &scope, "H").unwrap();
            assert_eq!(
                native
                    .matches(if sum { "Sum<" } else { "Product<" })
                    .count(),
                width - 1
            );
            assert_eq!(
                witness
                    .matches(if sum {
                        "__kioSumType("
                    } else {
                        "__kioProductType("
                    })
                    .count(),
                width - 1
            );
            assert!(native.len() < width * 24);
            assert!(witness.len() < width * 240);
        }
    }

    #[test]
    fn escaping_parameter_type_only_marks_top_level_closures() {
        let closure = "(Swift.Int) -> Swift.Int";
        assert_eq!(escaping_param_ty(closure), format!("@escaping {closure}"));

        let product_with_closure = "Product<(Swift.Int) -> Swift.Int, Swift.Int>";
        assert_eq!(
            escaping_param_ty(product_with_closure),
            product_with_closure
        );
    }

    #[test]
    fn higher_order_domains_preserve_generic_escaping_convention() {
        let package = single_api_package(
            "module api; \
             host fn apply(value: (. -> .) -> .) -> .; \
             pub fn keep(value: (. -> .) -> .) -> (. -> .) -> . { value }",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit higher-order Swift");
        let higher_order = "(@escaping (KioUnit) -> KioUnit) -> KioUnit";
        assert!(emitted.ffi_swift.contains(&format!("= {higher_order}")));
        assert!(emitted.host_swift.contains(&format!(
            "func api__apply(_ arg0: @escaping {higher_order})"
        )));
        assert!(emitted.pkg_swift.contains(&format!(
            "public func keep(_ arg0: @escaping {higher_order}) -> {higher_order}"
        )));
    }

    #[test]
    fn native_leaf_witnesses_do_not_emit_immediate_closures() {
        use crate::ast::{Meta, PathSegment};
        let package = single_api_package("module api; host type Atom; pub fn unit() { () }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let shapes = super::super::skin::PreparedSwiftShapes::new(&prepared, "PkgHost", &package);
        let span = crate::span::Span::new(0, 0);
        for (ty, native) in [
            (
                Type::Unit {
                    meta: Meta::new(span),
                },
                "KioUnit",
            ),
            (
                Type::Bottom {
                    meta: Meta::new(span),
                },
                "Never",
            ),
            (
                Type::Path {
                    segments: vec![PathSegment::new("Atom", span)],
                    args: vec![],
                    meta: Meta::new(span),
                },
                "H.api__Atom",
            ),
        ] {
            let plan = ValuePlan::from_routed_type(
                &package,
                &package.module("api").unwrap().module,
                &[],
                &ty,
            )
            .unwrap();
            let types = super::super::value::Types {
                plan: &plan,
                shapes: &shapes,
                retained_nominals: None,
                origin: BoundaryFacadeSupportOrigin::Live,
            };
            assert_eq!(
                types
                    .witness(plan.root(), &Default::default(), "H")
                    .unwrap(),
                format!("__KioNativeWitness<{native}>()")
            );
        }
    }

    #[test]
    fn one_witness_prelude_reuses_only_exact_native_leaves() {
        use crate::ast::{Meta, PathSegment};
        let package =
            single_api_package("module api; host type Atom; host type Other; pub fn unit() { () }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let shapes = super::super::skin::PreparedSwiftShapes::new(&prepared, "PkgHost", &package);
        let span = crate::span::Span::new(0, 0);
        let atom = |name| Type::Path {
            segments: vec![PathSegment::new(name, span)],
            args: vec![],
            meta: Meta::new(span),
        };
        let mut ty = Type::Bottom {
            meta: Meta::new(span),
        };
        for left in [
            Type::Unit {
                meta: Meta::new(span),
            },
            atom("Other"),
            atom("Atom"),
            atom("Atom"),
        ] {
            ty = Type::Product {
                left: Box::new(left),
                right: Box::new(ty),
                meta: Meta::new(span),
            };
        }
        let plan =
            ValuePlan::from_routed_type(&package, &package.module("api").unwrap().module, &[], &ty)
                .unwrap();
        let types = super::super::value::Types {
            plan: &plan,
            shapes: &shapes,
            retained_nominals: None,
            origin: BoundaryFacadeSupportOrigin::Live,
        };
        let witness = types
            .witness(plan.root(), &Default::default(), "H")
            .unwrap();
        for native in ["H.api__Atom", "H.api__Other", "KioUnit", "Never"] {
            assert_eq!(
                witness
                    .matches(&format!("__KioNativeWitness<{native}>()"))
                    .count(),
                1,
                "{witness}"
            );
        }
        assert_eq!(witness.matches("__kioProductType(").count(), 4, "{witness}");
        let other_host = types
            .witness(plan.root(), &Default::default(), "OtherHost")
            .unwrap();
        assert!(other_host.contains("__KioNativeWitness<OtherHost.api__Atom>()"));
        assert!(!other_host.contains("__KioNativeWitness<H.api__Atom>()"));
    }

    #[test]
    fn boundary_and_facade_names_preserve_semantic_components() {
        assert_eq!(host_member_name("foo/bar", "read"), "foo_bar__read");
        assert_eq!(host_member_name("foo_bar", "read"), "KioItem_fooBar__read");
        let slash_module_fn = module_fn_method_name("foo/bar", "value");
        let underscore_module_fn = module_fn_method_name("foo_bar", "value");
        assert_ne!(slash_module_fn, underscore_module_fn);
        assert!(slash_module_fn.starts_with("__"));
        assert!(underscore_module_fn.starts_with("__"));
        assert_eq!(
            export_selector_name(&FacadeSelector::Module("child".to_owned()), false),
            "KioModule_child"
        );
        assert_eq!(
            export_selector_name(&FacadeSelector::Type("Child".to_owned()), false),
            "KioType_Child"
        );
        assert_eq!(
            export_newtype_member_name("main", "A_b", "c"),
            "main____KioType_AB__KioItem_c"
        );
        assert_ne!(
            export_newtype_member_name("main", "A_b", "c"),
            export_newtype_member_name("main", "A", "b_c")
        );
    }

    fn same_leaf_package() -> Package<Routed> {
        let sources = [
            "module left; host type Shared role(i32); \
             pub newtype Token : Shared { constructor mk_token; projector un_token; }; \
             host fn round_token(value: Token) -> Token; \
             pub fn left_value() -> Shared { 1 }",
            "module right; host type Shared role(i64); \
             pub newtype Token : Shared { constructor mk_token; projector un_token; }; \
             host fn round_token(value: Token) -> Token; \
             pub fn right_value() -> Shared { 2 }",
        ];
        let parsed = sources
            .iter()
            .map(|source| {
                let module = parse(source).expect("parse module");
                let path = module.path.segments.last().expect("module leaf").as_str();
                (PathBuf::from(format!("{path}.kio")), module)
            })
            .collect();
        let package_file = parse_package_file(
            "package pkg; build { target swift { out \"out/swift/\"; } } bridge { left; right; }",
            None,
        )
        .expect("parse package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check body resolution");
        let prime = check_package(&package).expect("typecheck");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    fn single_api_package(source: &str) -> Package<Routed> {
        let parsed = vec![(
            PathBuf::from("api.kio"),
            parse(source).expect("parse api module"),
        )];
        let package_file = parse_package_file(
            "package pkg; build { target swift { out \"out/swift/\"; } } bridge { api; }",
            None,
        )
        .expect("parse api package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower api package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check body resolution");
        let prime = check_package(&package).expect("typecheck");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    fn retained_carrier_signature() -> (u32, crate::sig::ReplayedInterface) {
        let source = r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Box[A];
        host type Legacy role(str);
        host type Unused[A];
    host type Unused_nullary;
        newtype Token[A] : A { pub constructor make_token; projector read_token; };
        host fn old(value: Box(Token(.)) & (. -> .)) -> [A] A -> A;
        host fn old_text(value: Legacy) -> Legacy;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Box;
        Legacy;
        Unused;
        Unused_nullary;
        Token;
        old;
        old_text;
      }
    }
  }
}
"#;
        let file = crate::pass::parser::parse_signature_file(source, None)
            .expect("parse retained Swift fixture");
        let replayed = crate::sig::replay(&file).expect("replay retained Swift fixture");
        (2, replayed)
    }

    fn reached_nominal_index_measurement(
        width: usize,
    ) -> super::super::skin::SwiftReachedNominalIndexMeasurement {
        let additions = (0..width)
            .flat_map(|index| {
                [
                    format!("host type Reach{index:02};"),
                    format!("host type Unused{index:02};"),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n");
        let parameters = (0..width)
            .map(|index| format!("r{index:02}: Reach{index:02}"))
            .collect::<Vec<_>>()
            .join(", ");
        let removals = (0..width)
            .flat_map(|index| [format!("Reach{index:02};"), format!("Unused{index:02};")])
            .chain(std::iter::once("old;".to_owned()))
            .collect::<Vec<_>>()
            .join("\n");
        let signature = format!(
            "signature pkg v(3);\n\
             v(1) {{ nonbreaking {{ add {{ module api {{\n\
             {additions}\n\
             host fn old({parameters}) -> .;\n\
             }} }} }} }}\n\
             v(2) {{ nonbreaking {{ remove {{ module api {{\n\
             {removals}\n\
             }} }} }} }}"
        );
        let file = crate::pass::parser::parse_signature_file(&signature, None)
            .expect("parse Swift reached-nominal width fixture");
        let replayed =
            crate::sig::replay(&file).expect("replay Swift reached-nominal width fixture");
        let package = single_api_package("module api; pub fn unit() -> . { () }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("prepare Swift reached-nominal width fixture");
        let shapes =
            super::super::skin::PreparedSwiftShapes::new(&prepared, "FacadeHost", &package);
        shapes.measure_reached_nominal_index(&prepared)
    }

    #[test]
    fn reached_nominal_index_bounds_structural_operations() {
        let narrow = reached_nominal_index_measurement(8);
        let wide = reached_nominal_index_measurement(32);

        for (width, measurement) in [(8, narrow), (32, wide)] {
            assert_eq!(measurement.retained_binding_count, 2 * width);
            assert_eq!(measurement.selected_binding_count, width);
            assert_eq!(measurement.index_construction_visits, width);
            assert_eq!(measurement.indexed_lookups, 2 * width);
            assert_eq!(measurement.identity_parity_checks, 2 * width);
            assert!(measurement.indexed_lookups > 0, "the nominal index fired");
            assert_eq!(
                measurement.legacy_candidate_visits,
                width * (width + 1) / 2 + width * width
            );
            assert!(
                measurement.index_construction_visits + measurement.indexed_lookups
                    < measurement.legacy_candidate_visits
            );
        }

        assert_eq!(
            wide.index_construction_visits + wide.indexed_lookups,
            4 * (narrow.index_construction_visits + narrow.indexed_lookups)
        );
        assert!(
            wide.legacy_candidate_visits > 4 * narrow.legacy_candidate_visits,
            "the repeated exact-QTN scan grows superlinearly"
        );
    }

    #[test]
    fn retained_nullary_host_type_defaults_satisfy_role_constraints() {
        for role in [
            Role::I8,
            Role::I16,
            Role::I32,
            Role::I64,
            Role::I128,
            Role::U8,
            Role::U16,
            Role::U32,
            Role::U64,
            Role::U128,
        ] {
            assert_eq!(
                swift_host_type_constraints(Some(role)),
                ": ExpressibleByIntegerLiteral"
            );
            assert_eq!(swift_host_type_default(Some(role)), "Swift.Int");
        }
        for role in [Role::F32, Role::F64] {
            assert_eq!(
                swift_host_type_constraints(Some(role)),
                ": ExpressibleByFloatLiteral"
            );
            assert_eq!(swift_host_type_default(Some(role)), "Swift.Double");
        }
        assert_eq!(
            swift_host_type_constraints(Some(Role::Bool)),
            ": ExpressibleByBooleanLiteral, Equatable"
        );
        assert_eq!(swift_host_type_default(Some(Role::Bool)), "Swift.Bool");
        assert_eq!(
            swift_host_type_constraints(Some(Role::Str)),
            ": ExpressibleByStringLiteral"
        );
        assert_eq!(swift_host_type_default(Some(Role::Str)), "Swift.String");
        assert_eq!(swift_host_type_constraints(None), "");
        assert_eq!(swift_host_type_default(None), "KioUnit");
    }

    #[test]
    fn retained_signature_emits_deprecated_optional_source_facade_only() {
        let package = single_api_package("module api; pub fn unit() -> . { () }");
        let live = lower_package(&package, "Facade").expect("emit live Swift package");
        let sig = retained_carrier_signature();
        let retained = lower_package_with_signature(&package, "Facade", Some(&sig))
            .expect("emit retained Swift package");

        let box_name = QualifiedTypeName::new(vec!["api".to_owned()], "Box")
            .expect("qualified retained host type");
        let unused_name = QualifiedTypeName::new(vec!["api".to_owned()], "Unused")
            .expect("qualified unused host type");
        let unused_nullary_name = QualifiedTypeName::new(vec!["api".to_owned()], "Unused_nullary")
            .expect("qualified unused nullary host type");
        let token_name = QualifiedTypeName::new(vec!["api".to_owned()], "Token")
            .expect("qualified retained newtype");
        let legacy_name = QualifiedTypeName::new(vec!["api".to_owned()], "Legacy")
            .expect("qualified retained nullary host type");
        let box_carrier = super::super::skin::swift_host_carrier_name(&box_name);
        let unused_carrier = super::super::skin::swift_host_carrier_name(&unused_name);
        let token_carrier = super::super::skin::swift_newtype_carrier_name(&token_name);

        assert!(
            !live.shapes_swift.contains(&box_carrier),
            "{}",
            live.shapes_swift
        );
        assert!(
            retained
                .shapes_swift
                .contains(&format!("public typealias {box_carrier}<T0> = KioApply1<")),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(&format!(
                "@available(*, deprecated, message: \"Kio host type `api.Box` is retained only for removed host declarations from contract v2\")\npublic typealias {box_carrier}<T0>"
            )),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(&format!(
                "public typealias {token_carrier}<H: FacadeHost, T0> = KioApply1<"
            )),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(&format!(
                "@available(*, deprecated, message: \"Kio newtype `api.Token` is retained only for removed host declarations from contract v2\")\npublic typealias {token_carrier}<H: FacadeHost, T0>"
            )),
            "{}",
            retained.shapes_swift
        );
        assert!(!retained.shapes_swift.contains(&unused_carrier));
        assert!(
            !retained.host_swift.contains(&host_type_member_name(
                &unused_nullary_name.module_segments().join("/"),
                unused_nullary_name.name()
            )),
            "{}",
            retained.host_swift
        );
        assert!(
            !live.host_swift.contains(&host_type_member_name(
                &legacy_name.module_segments().join("/"),
                legacy_name.name()
            )),
            "{}",
            live.host_swift
        );
        assert!(
            retained.host_swift.contains(
                "@available(*, deprecated, message: \"Kio host type `api.Legacy` is retained only for removed host declarations from contract v2\")\n\tassociatedtype api__Legacy: ExpressibleByStringLiteral = Swift.String"
            ),
            "{}",
            retained.host_swift
        );
        let old_message = "Kio host fn `api.old` was removed at contract v2";
        let old_attribute = super::super::skin::swift_deprecated_attribute(old_message);
        assert_eq!(
            retained.host_swift.matches(old_attribute.as_str()).count(),
            2,
            "the retained protocol requirement and its trapping default are equally deprecated:\n{}",
            retained.host_swift
        );
        assert!(
            retained.host_swift.contains("\tfunc api__old("),
            "{}",
            retained.host_swift
        );
        assert!(
            retained
                .host_swift
                .contains(&format!("fatalError(\"{old_message}\")")),
            "{}",
            retained.host_swift
        );
        assert!(
            retained.ffi_swift.contains(&format!(
                "{}public typealias Env_api__old_arg0",
                old_attribute
            )),
            "{}",
            retained.ffi_swift
        );
        assert!(
            retained.shapes_swift.contains("Kio callable carrier `"),
            "{}",
            retained.shapes_swift
        );
        assert!(
            !retained.runtime_swift.contains("@available(*, deprecated"),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains("public protocol KioForall_"),
            "{}",
            retained.shapes_swift
        );
        let shape_lines = retained.shapes_swift.lines().collect::<Vec<_>>();
        for (index, line) in shape_lines.iter().enumerate() {
            let declaration = line.trim_start();
            if declaration.starts_with("public ")
                || declaration.starts_with("case ")
                || declaration.starts_with("associatedtype ")
                || declaration.starts_with("func specialize<")
            {
                assert!(
                    index > 0 && shape_lines[index - 1].contains("@available(*, deprecated"),
                    "history-only Swift declaration is not deprecated: {line}\n{}",
                    retained.shapes_swift
                );
            }
        }
        let ffi_lines = retained.ffi_swift.lines().collect::<Vec<_>>();
        for (index, line) in ffi_lines.iter().enumerate().filter(|(_, line)| {
            line.starts_with("public typealias Env_api__old")
                || line.starts_with("public typealias Env_api__old_text")
        }) {
            assert!(
                index > 0 && ffi_lines[index - 1].contains("@available(*, deprecated"),
                "history-only Swift alias is not deprecated: {line}\n{}",
                retained.ffi_swift
            );
        }
        assert!(!retained.pkg_swift.contains("api__old"));
        assert!(!retained.pkg_swift.contains("KioType_Token"));
    }

    #[test]
    fn nested_callable_support_promotes_retained_origins_before_reusing_declarations() {
        let package = single_api_package("module api; host fn nested() -> [A] A -> [B] B -> A;");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let shapes = super::super::skin::PreparedSwiftShapes::new(&prepared, "PkgHost", &package);
        let site = prepared.sites().next().expect("the one host callable");
        let plan = site.plan().value_plan();
        let render = |origin| {
            let types = super::super::value::Types {
                plan,
                shapes: &shapes,
                retained_nominals: Some(site.nominals()),
                origin,
            };
            let native = types.native(plan.root(), &Default::default(), "H").unwrap();
            (native, shapes.render_shapes(&package, &prepared))
        };
        let (initial_native, initial) = render(BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 7,
        });
        assert_eq!(initial.matches("public protocol KioForall_").count(), 2);
        assert!(initial.contains("contract v7"), "{initial}");
        let (earlier_native, earlier) = render(BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 2,
        });
        assert_eq!(initial_native, earlier_native);
        assert_eq!(
            initial.matches("contract v7").count(),
            earlier.matches("contract v2").count()
        );
        assert!(!earlier.contains("contract v7"), "{earlier}");
        let (live_native, live) = render(BoundaryFacadeSupportOrigin::Live);
        assert_eq!(initial_native, live_native);
        assert!(!live.contains("@available"), "{live}");
        assert_eq!(live.matches("public protocol KioForall_").count(), 2);
        let (late_native, late) = render(BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 1,
        });
        assert_eq!(live_native, late_native);
        assert_eq!(live, late);
    }

    #[test]
    fn live_provenance_dominates_retained_swift_support_deprecation() {
        let package = single_api_package(
            "module api; \
             host type Box[A]; \
             pub newtype Token[A] : A { pub constructor make_token; projector read_token; }; \
             host fn current(value: Box(Token(.)) & (. -> .)) -> [A] A -> A; \
             pub fn unit() -> . { () }",
        );
        let sig = retained_carrier_signature();
        let emitted = lower_package_with_signature(&package, "Facade", Some(&sig))
            .expect("emit Swift package with live and retained support provenance");
        let box_name = QualifiedTypeName::new(vec!["api".to_owned()], "Box")
            .expect("qualified live host type");
        let box_carrier = super::super::skin::swift_host_carrier_name(&box_name);

        assert!(
            emitted
                .shapes_swift
                .contains(&format!("public typealias {box_carrier}<T0> = KioApply1<")),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            !emitted
                .shapes_swift
                .contains("Kio host type `api.Box` is retained only for removed host declarations"),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            !emitted
                .shapes_swift
                .contains("Kio newtype `api.Token` is retained only for removed host declarations"),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            !emitted.shapes_swift.contains(
                "Kio application carrier `KioApply1` is retained only for removed host declarations"
            ),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            !emitted.shapes_swift.contains("Kio facade shell `"),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            !emitted.shapes_swift.contains("@available(*, deprecated"),
            "the alpha-identical callable carrier also reached by live code stays current:\n{}",
            emitted.shapes_swift
        );
    }

    fn staged_type_application_package() -> Package<Routed> {
        let source = "module api; \
            host type T role(str); \
            host fn host_id[A](value: A) -> A; \
            pub fn id[A](value: A) -> A { value } \
            fn host_value() -> [A] A -> A { host_id } \
            pub fn call(value: T) -> T { id(T, value) }";
        let parsed = vec![(
            PathBuf::from("api.kio"),
            parse(source).expect("parse staged module"),
        )];
        let package_file = parse_package_file(
            "package pkg; build { target swift { out \"out/swift/\"; } } bridge { api; }",
            None,
        )
        .expect("parse staged package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower staged package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package =
            Package::build(Path::new(""), modules, package_file).expect("build staged package");
        package.resolve_imports().expect("resolve staged uses");
        package
            .check_in_body_resolution()
            .expect("check staged body resolution");
        let prime = check_package(&package).expect("typecheck staged package");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    #[test]
    fn type_binders_are_runtime_stages_and_exact_swift_method_generics() {
        let package = staged_type_application_package();
        let emitted = lower_package(&package, "Pkg").expect("emit staged Swift");
        let id_method = module_fn_method_name("api", "id");
        assert!(
            emitted
                .pkg_swift
                .contains(&format!("func {id_method}() -> Any")),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted
                .pkg_swift
                .contains(&format!("__kioAdvance(self.{id_method}(),"))
                && emitted
                    .pkg_swift
                    .contains("__KioFunctionWitness).self).invoke("),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted.pkg_swift.contains("host.api__hostId(")
                && emitted.pkg_swift.contains(".construct { __kio_host_stage")
                && emitted
                    .pkg_swift
                    .contains("let __kioOpenedType0: any __KioTypeWitness = __kio_host_stage")
                && emitted
                    .pkg_swift
                    .contains("return invoke(__kioOpenedType0,"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted.pkg_swift.contains("public func call(_ arg0:")
                && !emitted
                    .pkg_swift
                    .contains("public func call(_ arg0: Any, _ arg1:"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted.host_swift.contains("func api__hostId<KioType_")
                && emitted.host_swift.contains("_ arg0: KioType_")
                && emitted.host_swift.contains("-> KioType_"),
            "{}",
            emitted.host_swift
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn public_host_method_selects_a_staged_generic_absent_from_its_signature() {
        let package = single_api_package(
            "module api; \
             host fn staged[A](value: A)[B](unit: .) -> .; \
             pub fn call() -> . { staged(., (), ., ()) }",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit staged host function");

        assert!(
            emitted
                .host_swift
                .contains("func api__staged<KioType_0, KioType_1>(_ arg0: KioType_0, asKioType1 _: KioType_1.Type)"),
            "{}",
            emitted.host_swift
        );
        assert!(
            !emitted.host_swift.contains("asKioType0"),
            "{}",
            emitted.host_swift
        );
        assert!(
            emitted.pkg_swift.contains("self.__kio_host.api__staged(")
                && emitted.pkg_swift.contains("asKioType1: W1.Native.self"),
            "{}",
            emitted.pkg_swift
        );
    }

    #[test]
    fn public_export_method_selects_a_staged_generic_absent_from_its_signature() {
        let package = single_api_package(
            "module api; \
             pub fn staged[A](value: A)[B](unit: .) -> . { unit }",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit staged public function");

        assert!(
            emitted
                .pkg_swift
                .contains("public func staged<KioType_0, KioType_1>(_ arg0: KioType_0, asKioType1 _: KioType_1.Type = KioType_1.self) -> KioUnit"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted
                .pkg_swift
                .contains("__KioNativeWitness<KioType_1>()"),
            "{}",
            emitted.pkg_swift
        );
    }

    fn comptime_named_package() -> Package<Routed> {
        let source = "module api; \
            host type Comptime_bool role(i32); \
            pub newtype Comptime_str : . { \
              pub constructor make_str; \
              pub projector read_str; \
            }; \
            host fn round_host(value: Comptime_bool) -> Comptime_bool; \
            host fn round_newtype(value: Comptime_str) -> Comptime_str;";
        let parsed = vec![(
            PathBuf::from("api.kio"),
            parse(source).expect("parse module"),
        )];
        let package_file = parse_package_file(
            "package pkg; build { target swift { out \"out/swift/\"; } } bridge { api; }",
            None,
        )
        .expect("parse package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check body resolution");
        let prime = check_package(&package).expect("typecheck");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    #[test]
    fn comptime_like_names_keep_exact_boundary_identity() {
        let emitted = lower_package(&comptime_named_package(), "Pkg").expect("emit Swift");
        assert!(
            emitted.host_swift.contains(
                "func api__roundHost(_ arg0: Self.api__ComptimeBool) \
                 -> Self.api__ComptimeBool"
            ),
            "{}",
            emitted.host_swift
        );
        assert!(
            emitted.host_swift.contains(
                "func api__roundNewtype(_ arg0: KioNewtype_api__ComptimeStr<Self>) \
                 -> KioNewtype_api__ComptimeStr<Self>"
            ),
            "{}",
            emitted.host_swift
        );
        assert_no_public_any(&emitted);
    }

    fn opaque_newtype_package() -> Package<Routed> {
        let source = "module api; import __comptime__; \
            newtype Hidden_payload : . { constructor mk_hidden; projector un_hidden; }; \
            pub newtype Hidden_type : __Type__ { constructor make_hidden_type; projector read_hidden_type; }; \
            pub newtype Opaque_a : Hidden_payload { constructor mk_opaque_a; projector un_opaque_a; }; \
            pub newtype Opaque_b : Hidden_payload { constructor mk_opaque_b; projector un_opaque_b; }; \
            pub newtype Constructor_only : . { pub constructor make_constructor_only; projector read_constructor_only; }; \
            pub newtype Projector_only : . { constructor make_projector_only; pub projector read_projector_only; }; \
            pub newtype Generic[A] : A { pub constructor make_generic; projector read_generic; }; \
            pub newtype Existential <U> : U { constructor make_existential; pub projector read_existential; }; \
            pub newtype Transparent_both : . { pub constructor make_transparent; pub projector read_transparent; }; \
            pub newtype Transparent_spread : [A] (Hidden_type & A) -> Hidden_type { pub constructor make_transparent_spread; pub projector read_transparent_spread; }; \
            pub newtype Transparent_packed : (Hidden_type & Hidden_type) -> Hidden_type { pub constructor make_transparent_packed; pub projector read_transparent_packed; }; \
            pub newtype Constructor_spread : [A] (Hidden_type & A) -> Hidden_type { pub constructor make_constructor_spread; projector read_constructor_spread; }; \
            pub newtype Projector_spread : [A] (Hidden_type & A) -> Hidden_type { constructor make_projector_spread; pub projector read_projector_spread; }; \
            pub newtype Existential_spread <U> : [A] (U & A) -> U { constructor make_existential_spread; pub projector read_existential_spread; }; \
            pub newtype Foo : . { constructor make_foo; projector read_foo; }; \
            pub newtype Foo_host : . { constructor make_foo_host; projector read_foo_host; }; \
            rec { \
              pub newtype Outer : Hidden { pub constructor make_outer; pub projector read_outer; }; \
              pub newtype Hidden : Outer { constructor make_hidden; projector read_hidden; }; \
            } \
            pub rec newtype Direct_recursive : . | Direct_recursive { pub constructor make_direct; pub projector read_direct; }; \
            host fn round_opaque(value: Opaque_a) -> Opaque_a; \
            host fn round_generic[A](value: Generic(A)) -> Generic(A); \
            host fn round_existential(value: Existential) -> Existential; \
            host fn echo_outer(value: Outer) -> Outer; \
            host fn echo_direct(value: Direct_recursive) -> Direct_recursive; \
            host fn round_transparent_spread(value: Transparent_spread) -> Transparent_spread; \
            host fn round_transparent_packed(value: Transparent_packed) -> Transparent_packed; \
            pub fn keep_a(value: Opaque_a) -> Opaque_a { value } \
            pub fn keep_b(value: Opaque_b) -> Opaque_b { value }";
        let parsed = vec![(
            PathBuf::from("api.kio"),
            parse(source).expect("parse module"),
        )];
        let package_file = parse_package_file(
            "package pkg; build { target swift { out \"out/swift/\"; } } bridge { api; }",
            None,
        )
        .expect("parse package file");
        let (modules, package_file) =
            FullPipeline::lower_package(parsed, Some(package_file)).expect("lower package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file).expect("build package");
        package.resolve_imports().expect("resolve imports");
        package
            .check_in_body_resolution()
            .expect("check body resolution");
        let prime = check_package(&package).expect("typecheck");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    #[test]
    fn opaque_carriers_avoid_fixed_swift_facade_and_runtime_names() {
        let emitted = lower_package(&opaque_newtype_package(), "Foo").expect("emit Swift");
        assert!(
            !emitted
                .shapes_swift
                .lines()
                .any(|line| line == "public struct Foo {"),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            emitted.pkg_swift.contains("public final class Foo<"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted.host_swift.contains("public protocol FooHost"),
            "{}",
            emitted.host_swift
        );
    }
    #[test]
    fn public_newtypes_use_exact_declaration_owned_carriers() {
        let emitted = lower_package(&opaque_newtype_package(), "Pkg").expect("emit Swift");
        let carrier = |leaf: &str| {
            format!(
                "KioNewtype_api__{}",
                crate::backends::public_names::encode_host_identity(leaf)
            )
        };
        let opaque_a = carrier("Opaque_a");
        let opaque_b = carrier("Opaque_b");
        assert_ne!(opaque_a, opaque_b);

        for leaf in [
            "Hidden_type",
            "Opaque_a",
            "Opaque_b",
            "Constructor_only",
            "Projector_only",
            "Existential",
            "Transparent_both",
            "Transparent_spread",
            "Transparent_packed",
            "Constructor_spread",
            "Projector_spread",
            "Existential_spread",
            "Foo",
            "Foo_host",
            "Outer",
            "Hidden",
            "Direct_recursive",
        ] {
            assert!(
                emitted
                    .shapes_swift
                    .contains(&format!("public struct {}", carrier(leaf))),
                "missing exact carrier for {leaf}:\\n{}",
                emitted.shapes_swift
            );
        }
        assert!(
            !emitted
                .shapes_swift
                .contains(&format!("public struct {}", carrier("Hidden_payload"))),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            emitted.shapes_swift.contains(&format!(
                "public typealias {}<H: PkgHost, T0> = KioApply1<KioNewtypeMk_api__Generic<H>, T0>",
                carrier("Generic")
            )),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            emitted
                .host_swift
                .contains(&format!("_ arg0: {opaque_a}<Self>) -> {opaque_a}<Self>"))
                && emitted
                    .host_swift
                    .contains("_ arg0: KioApply1<KioNewtypeMk_api__Generic<Self>, KioType_"),
            "{}",
            emitted.host_swift
        );
        assert!(
            emitted.shapes_swift.contains("let __kioValue: Any")
                && !emitted.shapes_swift.contains("public let __kioValue"),
            "{}",
            emitted.shapes_swift
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn existential_projectors_rebuild_the_prepared_cps_body() {
        let emitted = lower_package(&opaque_newtype_package(), "Pkg").expect("emit Swift");

        assert!(
            emitted.pkg_swift.contains("as: __KioExistential.self)")
                && emitted.pkg_swift.contains(".construct { __kioSelected0 in")
                && emitted
                    .pkg_swift
                    .contains("__kioAdvance(__kioContinuation, __kioPacket.witnesses[0])")
                && emitted
                    .pkg_swift
                    .contains(".invoke(__kioContinuation, __kioPacket.payload)"),
            "{}",
            emitted.pkg_swift
        );
        assert_eq!(
            emitted
                .pkg_swift
                .matches("precondition(__kioPacket.witnesses.count == 1)")
                .count(),
            2,
            "each existential projector opens its one retained hidden witness:\n{}",
            emitted.pkg_swift
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn parameterized_host_types_use_exact_generic_carriers_and_constructor_markers() {
        let package = single_api_package(
            "module api; \
             host type Box[A]; \
             host fn round[A](value: Box(A)) -> Box(A);",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit Swift");

        assert!(
            !emitted.host_swift.contains("associatedtype api__Box")
                && emitted
                    .shapes_swift
                    .contains("public typealias KioHostTypeMk_api__Box = KioNative<KioHostTypeIdentity_api__Box>")
                && emitted
                    .shapes_swift
                    .contains("public typealias KioHostType_api__Box<T0> = KioApply1<KioHostTypeMk_api__Box, T0>")
                && emitted
                    .runtime_swift
                    .contains("public struct KioApply1<F, T>"),
            "host:\n{}\nshapes:\n{}",
            emitted.host_swift,
            emitted.shapes_swift
        );
        assert!(
            emitted
                .host_swift
                .contains("_ arg0: KioApply1<KioHostTypeMk_api__Box, KioType_")
                && emitted
                    .host_swift
                    .contains("-> KioApply1<KioHostTypeMk_api__Box, KioType_"),
            "{}",
            emitted.host_swift
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn nominal_applications_are_opaque_host_indexed_and_grouping_canonical() {
        let package = single_api_package(
            "module api; \
             pub newtype Opaque[A][B] : A & B { constructor make; projector read; }; \
             pub newtype Construct[A][B] : A & B { pub constructor make; projector read; }; \
             pub newtype Project[A][B] : A & B { constructor make; pub projector read; }; \
             pub newtype Both[A][B] : A & B { pub constructor make; pub projector read; }; \
             pub newtype Packed[A][B] <Hidden> : A & B & Hidden { pub constructor make; pub projector read; };",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit Swift");
        for name in ["Opaque", "Construct", "Project", "Both", "Packed"] {
            assert!(
                emitted
                    .shapes_swift
                    .contains(&format!("public enum KioNewtypeMk_api__{name}<H: PkgHost>"))
            );
            assert!(
                emitted
                    .shapes_swift
                    .contains(&format!("public typealias KioNewtype_api__{name}<H: PkgHost, T0, T1> = KioApply1<KioApply1<KioNewtypeMk_api__{name}<H>, T0>, T1>"))
            );
        }
        assert!(!emitted.pkg_swift.contains("KioNewtypeApplication_"));
        assert!(
            emitted.shapes_swift.contains(
                "public typealias KioApply2<F, T0, T1> = KioApply1<KioApply1<F, T0>, T1>"
            )
        );
        let application = emitted
            .runtime_swift
            .split("public struct KioApply1")
            .nth(1)
            .unwrap();
        let application = application.split('}').next().unwrap();
        assert!(!application.contains("public init") && !application.contains("public func value"));
        assert!(emitted.runtime_swift.contains("fileprivate init() {}"));
        assert_no_public_any(&emitted);
    }

    #[test]
    fn forall_sites_expose_exact_callable_and_implementation_protocols() {
        let package = single_api_package(
            "module api; \
             host type Text role(str); \
             host fn apply(poly: [A] A -> A, value: Text) -> Text; \
             host fn produce(_unit: .) -> [A] A;",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit Swift");

        assert!(
            emitted.shapes_swift.contains("public protocol KioForall_")
                && emitted
                    .shapes_swift
                    .contains("Implementation {\n    associatedtype H: PkgHost")
                && emitted.shapes_swift.contains(".Type")
                && emitted
                    .shapes_swift
                    .contains("func specialize<T>(_: T.Type) -> (T) -> T")
                && emitted
                    .shapes_swift
                    .contains("public func specialize<T>(asKioType _: T.Type = T.self)")
                && !emitted.shapes_swift.contains("public func call<"),
            "{}",
            emitted.shapes_swift
        );
        assert!(
            emitted.host_swift.contains("_ arg0: KioForall_")
                && emitted.host_swift.contains("-> KioForall_"),
            "{}",
            emitted.host_swift
        );
        assert!(
            emitted
                .ffi_swift
                .contains("public typealias Env_api__apply_arg0Implementation = KioForall_")
                && emitted
                    .ffi_swift
                    .contains("public typealias Env_api__produce_retImplementation = KioForall_"),
            "{}",
            emitted.ffi_swift
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn unrelated_live_declarations_do_not_perturb_existing_facade_names_or_types() {
        let base = lower_package(
            &single_api_package(
                "module api; host type Box[A]; \
                 host fn round[A](value: Box(A)) -> Box(A);",
            ),
            "Pkg",
        )
        .expect("emit base Swift");
        let extended = lower_package(
            &single_api_package(
                "module api; host type Box[A]; \
                 host fn round[A](value: Box(A)) -> Box(A); \
                 host type Extra; host fn extra(value: Extra) -> Extra;",
            ),
            "Pkg",
        )
        .expect("emit extended Swift");

        for needle in [
            "api__round",
            "KioHostType_api__Box",
            "KioHostTypeMk_api__Box",
            "KioApply1",
        ] {
            let relevant = |source: &str| {
                source
                    .lines()
                    .filter(|line| line.contains(needle))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                relevant(&base.host_swift),
                relevant(&extended.host_swift),
                "host facade changed for {needle}"
            );
            assert_eq!(
                relevant(&base.shapes_swift),
                relevant(&extended.shapes_swift),
                "shape facade changed for {needle}"
            );
            assert_eq!(
                relevant(&base.ffi_swift),
                relevant(&extended.ffi_swift),
                "FFI aliases changed for {needle}"
            );
        }
    }

    #[test]
    fn same_leaf_literal_roles_use_exact_descriptor_identities() {
        let package = same_leaf_package();
        let descriptor = host_descriptor::build_host_descriptor(&package);
        let exact = exact_nullary_host_type_table(&descriptor);
        assert_eq!(
            exact.get(&("left".to_owned(), "Shared".to_owned())),
            Some(&Some(Role::I32))
        );
        assert_eq!(
            exact.get(&("right".to_owned(), "Shared".to_owned())),
            Some(&Some(Role::I64))
        );

        let emitted = lower_package(&package, "Pkg").expect("emit Swift");
        assert!(
            emitted
                .host_swift
                .contains("associatedtype left__Shared: ExpressibleByIntegerLiteral"),
            "{}",
            emitted.host_swift,
        );
        assert!(
            emitted
                .host_swift
                .contains("associatedtype right__Shared: ExpressibleByIntegerLiteral"),
            "{}",
            emitted.host_swift,
        );
        assert!(
            emitted.pkg_swift.contains("(1 as H.left__Shared)"),
            "{}",
            emitted.pkg_swift,
        );
        assert!(
            emitted.pkg_swift.contains("(2 as H.right__Shared)"),
            "{}",
            emitted.pkg_swift,
        );
        assert!(!emitted.pkg_swift.contains("(1 as Int32)"));
        assert!(!emitted.pkg_swift.contains("(2 as Int64)"));

        let left = "KioNewtype_left__Token<Self>";
        let right = "KioNewtype_right__Token<Self>";
        assert_ne!(left, right);
        assert!(
            emitted
                .host_swift
                .contains(&format!("_ arg0: {left}) -> {left}"))
        );
        assert!(
            emitted
                .host_swift
                .contains(&format!("_ arg0: {right}) -> {right}"))
        );
        assert_no_public_any(&emitted);
    }

    #[test]
    fn exact_host_type_member_names_are_injective() {
        assert_eq!(host_type_member_name("a/b", "c"), "a_b__c");
        assert_eq!(host_type_member_name("a_b_", "c"), "__kio_host_aB_u__c");
        assert_ne!(
            host_type_member_name("a_b_", "c"),
            host_type_member_name("a", "b_c_")
        );
        assert_ne!(
            host_type_member_name("a_b/c", "X"),
            host_type_member_name("a/b_c", "X")
        );
    }
}

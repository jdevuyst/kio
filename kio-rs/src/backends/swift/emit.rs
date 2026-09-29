//! Swift backend — IR → Swift package lowering.
//!
//! Consumes a `Package<Routed>` (post-recovery + post-resolution-lowering)
//! and produces a self-contained Swift package as a [`SwiftPackage`].
//!
//! Swift's **body** computes over the universal erased value (`Any`), the
//! JS dynamic body's shape on a statically-typed host: scalar polymorphism
//! and higher-kinded carriers alike flow as erased values. The **skin** is
//! the host's exact typed contract — the `Host` protocol, generic semantic
//! product/native-sum shells, declaration-owned nominal and application
//! carriers, exact callable carriers, and the exported surface. One shared
//! [`PreparedBoundaryCallableSites`] transaction supplies every public type,
//! slot, source layout, and body adapter. Every top-level name is branded
//! from the package's namespace ([`SwiftNames`]): the handle `<Handle>`, the
//! host contract `<Handle>Host`, the factory `create<Handle>`.

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};

use crate::ast::{Expr, Role, Routed, Type};
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryHostBindingOrigin,
    BoundaryNominalDeclaration, BoundaryNominalDependencies, CallableExecutionLayout,
    CallableExecutionStage, CallableSourceParamAdapter, CallableValueStageLayout, FacadeBinderId,
    FacadeUse, FacadeUseId, PreparedBoundaryCallableSite, PreparedBoundaryCallableSites,
    QualifiedTypeName,
};
use crate::backends::public_names::{
    FacadeSelector, encode_host_identity, encode_source_identity, has_readable_snake_components,
    host_name_core,
};
use crate::backends::reconstruct::{ReconProfile, apply_subst, build_typearg_subst_from_sig};
use crate::backends::skin::{
    exact_nullary_host_type_table, instantiate_boundary_newtype_payload, resolve_newtype,
};
use crate::backends::structural::{
    ProductRebuildPlan, bound_product_rebuild_plan, render_cached_product_rebuild,
};
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

    fn internal(message: impl Into<String>) -> Self {
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
    /// `shapes.swift` — generic semantic shells plus exact nominal,
    /// application, and rank-N callable carriers the FFI surfaces.
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
    let facade_shapes = super::skin::PreparedSwiftShapes::new(&prepared, &names.host_ty);

    let host_swift = render_host_protocol(&prepared, &facade_shapes, &names)?;
    let pkg_swift = render_pkg_swift(package, &prepared, &facade_shapes, &literal_shapes, &names)?;
    let ffi_swift = render_ffi_swift(&prepared, &facade_shapes, &names)?;
    let shapes_swift = facade_shapes.render_shapes(&prepared);

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

    fn lookup(&self, id: FacadeBinderId) -> Result<&str, EmitError> {
        self.binders.get(&id).map(String::as_str).ok_or_else(|| {
            EmitError::internal(format!(
                "Swift facade refers to unbound type binder {id:?}; bound binders: {:?}",
                self.binders.keys().collect::<Vec<_>>()
            ))
        })
    }

    fn erased(&self) -> Self {
        Self {
            binders: self
                .binders
                .keys()
                .copied()
                .map(|binder| (binder, "Any".to_owned()))
                .collect(),
        }
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
    presentation: &'site BoundaryFacadeExecutionPlan,
    shapes: &'shapes super::skin::PreparedSwiftShapes,
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

fn render_swift_application(head: &str, args: &[String]) -> String {
    if args.is_empty() {
        return head.to_owned();
    }
    let mut parameters = Vec::with_capacity(args.len() + 1);
    parameters.push(head.to_owned());
    parameters.extend(args.iter().cloned());
    format!(
        "{}{}",
        super::skin::swift_apply_carrier_name(args.len()),
        super::skin::swift_generic_use(&parameters)
    )
}

fn render_prepared_nominal(
    name: &QualifiedTypeName,
    args: &[String],
    nominals: &BoundaryNominalDependencies,
    shapes: &super::skin::PreparedSwiftShapes,
    host_ref: &str,
) -> Result<String, EmitError> {
    let declaration = nominals.declaration(name).ok_or_else(|| {
        EmitError::internal(format!(
            "Swift facade is missing nominal dependency {}.{}",
            name.module_segments().join("."),
            name.name()
        ))
    })?;
    match declaration {
        BoundaryNominalDeclaration::HostType { type_params, .. } => {
            if args.len() > type_params.len() {
                return Err(EmitError::internal(
                    "Swift prepared host-type application exceeds its declaration arity",
                ));
            }
            let binding = shapes.host_binding(name).ok_or_else(|| {
                EmitError::internal("Swift prepared host nominal has no exact binding")
            })?;
            if args.len() < type_params.len() {
                let marker = super::skin::swift_host_constructor_name(name);
                return Ok(render_swift_application(&marker, args));
            }
            if binding.type_arity == 0 {
                return Ok(format!(
                    "{host_ref}.{}",
                    host_type_member_name(&name.module_segments().join("/"), name.name())
                ));
            }
            Ok(format!(
                "{}{}",
                super::skin::swift_host_carrier_name(name),
                super::skin::swift_generic_use(args)
            ))
        }
        BoundaryNominalDeclaration::Newtype { type_params, .. } => {
            if args.len() > type_params.len() {
                return Err(EmitError::internal(
                    "Swift prepared newtype application exceeds its declaration arity",
                ));
            }
            if args.len() < type_params.len() {
                let marker = format!(
                    "{}<{host_ref}>",
                    super::skin::swift_newtype_constructor_name(name)
                );
                return Ok(render_swift_application(&marker, args));
            }
            let mut parameters = vec![host_ref.to_owned()];
            parameters.extend(args.iter().cloned());
            Ok(format!(
                "{}{}",
                super::skin::swift_newtype_carrier_name(name),
                super::skin::swift_generic_use(&parameters)
            ))
        }
    }
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
            let shell = source.product_shell().ok_or_else(|| {
                EmitError::internal("Swift right-nested source parameter has no exact shell")
            })?;
            let args = source_slots
                .iter()
                .map(|slot| render_prepared_use(render, *slot, scope, host_ref))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!(
                "{}{}",
                super::skin::swift_facade_shell_name(shell),
                super::skin::swift_generic_use(&args)
            ))
        }
    }
}

fn render_prepared_use(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    scope: &SwiftFacadeScope,
    host_ref: &str,
) -> Result<String, EmitError> {
    Ok(match render.plan.use_at(id) {
        FacadeUse::Unit { .. } => "KioUnit".to_owned(),
        FacadeUse::Bottom { .. } => "Never".to_owned(),
        FacadeUse::Bound { binder, .. } => scope.lookup(*binder)?.to_owned(),
        FacadeUse::Nominal { name, .. } => {
            render_prepared_nominal(name, &[], render.site.nominals(), render.shapes, host_ref)?
        }
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            let rendered_args = args
                .iter()
                .map(|arg| render_prepared_use(render, *arg, scope, host_ref))
                .collect::<Result<Vec<_>, _>>()?;
            match render.plan.use_at(*constructor) {
                FacadeUse::Nominal { name, .. } => render_prepared_nominal(
                    name,
                    &rendered_args,
                    render.site.nominals(),
                    render.shapes,
                    host_ref,
                )?,
                _ => {
                    let head = render_prepared_use(render, *constructor, scope, host_ref)?;
                    render_swift_application(&head, &rendered_args)
                }
            }
        }
        FacadeUse::Product { shell, args, .. } | FacadeUse::Sum { shell, args, .. } => {
            let args = args
                .iter()
                .map(|arg| render_prepared_use(render, *arg, scope, host_ref))
                .collect::<Result<Vec<_>, _>>()?;
            format!(
                "{}{}",
                super::skin::swift_facade_shell_name(shell),
                super::skin::swift_generic_use(&args)
            )
        }
        FacadeUse::Function { slots, result, .. } => {
            let BoundaryFacadeExecutionUse::Function(layout) = render.presentation.use_at(id)
            else {
                return Err(EmitError::internal(
                    "Swift prepared function has no paired source presentation",
                ));
            };
            let params = layout
                .source_params()
                .iter()
                .map(|source| {
                    render_prepared_source_param_type(render, slots, source, scope, host_ref)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result = render_prepared_use(render, *result, scope, host_ref)?;
            format!("({}) -> {result}", params.join(", "))
        }
        FacadeUse::Forall { .. } => render_prepared_forall_type(render, id, scope, host_ref)?,
    })
}

fn render_prepared_forall_type(
    render: PreparedSwiftRender<'_, '_>,
    root: FacadeUseId,
    scope: &SwiftFacadeScope,
    host_ref: &str,
) -> Result<String, EmitError> {
    let name = super::skin::swift_forall_carrier_name(render.site.site(), root);
    let implementation_protocol = format!("{name}Implementation");
    let free_binders = scope.binders.keys().copied().collect::<Vec<_>>();
    let mut carrier_parameters = vec![host_ref.to_owned()];
    carrier_parameters.extend(scope.binders.values().cloned());
    let public_type = format!(
        "{name}{}",
        super::skin::swift_generic_use(&carrier_parameters)
    );

    // The declaration is a pure function of binder identities, independent of
    // the particular rendering context (`KioType_*`, `Any`, ... ) in which the
    // same exact use is mentioned. Its application below uses that context's
    // actual arguments.
    let mut public_scope = SwiftFacadeScope {
        binders: free_binders
            .iter()
            .copied()
            .map(|binder| (binder, format!("KioOuterType_{}", binder.index())))
            .collect(),
    };
    let mut implementation_scope = public_scope.clone();
    let mut method_parameters = Vec::new();
    let mut current = root;
    let mut stage_count = 0usize;
    while let FacadeUse::Forall { binder, result, .. } = render.plan.use_at(current) {
        if !matches!(
            render.presentation.use_at(current),
            BoundaryFacadeExecutionUse::InvokeForall
        ) {
            return Err(EmitError::internal(
                "Swift public forall carrier has no callable source type stage",
            ));
        }
        method_parameters.push(public_scope.bind(*binder));
        implementation_scope
            .binders
            .insert(*binder, "Any".to_owned());
        current = *result;
        stage_count += 1;
    }

    // Swift has no explicit generic-function application syntax. Optional
    // metatype witnesses let ordinary value arguments infer the quantified
    // types while still allowing a caller to select a binder that occurs only
    // in a result (or not at all). The implementation protocol always receives
    // the witnesses, so the erased adapter can specialize every binder to
    // `Any` without inference holes.
    let public_type_witnesses = method_parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            format!("asKioType{index} _: {parameter}.Type = {parameter}.self")
        })
        .collect::<Vec<_>>();
    let implementation_type_witnesses = method_parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| format!("_ __kioType{index}: {parameter}.Type"))
        .collect::<Vec<_>>();
    let erased_type_witnesses = method_parameters
        .iter()
        .map(|_| "Any.self".to_owned())
        .collect::<Vec<_>>();

    let mut raw = "self.__kioValue".to_owned();
    for _ in 0..stage_count {
        raw = format!("({raw} as! (() -> Any))()");
    }
    let (value_params, returned, ret_type, implementation_value) = match render.plan.use_at(current)
    {
        FacadeUse::Function { slots, result, .. } => {
            let BoundaryFacadeExecutionUse::Function(layout) = render.presentation.use_at(current)
            else {
                return Err(EmitError::internal(
                    "Swift forall carrier function has no paired source presentation",
                ));
            };
            let params = layout
                .source_params()
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    Ok(format!(
                        "_ arg{index}: {}",
                        escaping_param_ty(&render_prepared_source_param_type(
                            render,
                            slots,
                            source,
                            &public_scope,
                            "H",
                        )?)
                    ))
                })
                .collect::<Result<Vec<_>, EmitError>>()?;
            let values = (0..layout.source_param_count())
                .map(|index| format!("arg{index}"))
                .collect::<Vec<_>>();
            let body_args =
                prepared_public_to_body_args(render, slots, layout, &values, &public_scope, 0)?;
            let call = format!(
                "({raw} as! {})({})",
                swift_closure_type(layout.body_abi_arity()),
                body_args.join(", ")
            );
            let returned = convert_prepared_use(
                render,
                *result,
                PreparedSwiftConversion::new(&call, PreparedSwiftDir::Out, &public_scope, 0),
            )?;
            let ret_type = render_prepared_use(render, *result, &public_scope, "H")?;

            let implementation_body_values = (0..layout.body_abi_arity())
                .map(|index| format!("__kioBodyArg{index}"))
                .collect::<Vec<_>>();
            let implementation_public_args = prepared_body_to_public_args(
                render,
                slots,
                layout,
                &implementation_body_values,
                &implementation_scope,
                0,
            )?;
            let mut implementation_args = erased_type_witnesses.clone();
            implementation_args.extend(implementation_public_args);
            let implementation_call =
                format!("implementation.call({})", implementation_args.join(", "));
            let implementation_returned = convert_prepared_use(
                render,
                *result,
                PreparedSwiftConversion::new(
                    &implementation_call,
                    PreparedSwiftDir::In,
                    &implementation_scope,
                    0,
                ),
            )?;
            let implementation_params = implementation_body_values
                .iter()
                .map(|value| format!("{value}: Any"))
                .collect::<Vec<_>>()
                .join(", ");
            let implementation_value = format!(
                "{{ ({implementation_params}) -> Any in return {implementation_returned} }}"
            );
            (params, returned, ret_type, implementation_value)
        }
        _ => {
            let returned = convert_prepared_use(
                render,
                current,
                PreparedSwiftConversion::new(&raw, PreparedSwiftDir::Out, &public_scope, 0),
            )?;
            let ret_type = render_prepared_use(render, current, &public_scope, "H")?;
            let implementation_call =
                format!("implementation.call({})", erased_type_witnesses.join(", "));
            let implementation_value = convert_prepared_use(
                render,
                current,
                PreparedSwiftConversion::new(
                    &implementation_call,
                    PreparedSwiftDir::In,
                    &implementation_scope,
                    0,
                ),
            )?;
            (Vec::new(), returned, ret_type, implementation_value)
        }
    };
    let mut stored = implementation_value;
    for _ in 0..stage_count {
        stored = format!("{{ () -> Any in return {stored} }}");
    }
    let mut declaration_parameters = vec![format!("H: {}", render.shapes.host_protocol())];
    declaration_parameters.extend(
        free_binders
            .iter()
            .map(|binder| format!("KioOuterType_{}", binder.index())),
    );
    let declaration_generic = super::skin::swift_generic_use(&declaration_parameters);
    let method_generic = super::skin::swift_generic_use(&method_parameters);
    let mut public_params = public_type_witnesses;
    public_params.extend(value_params.iter().cloned());
    let mut implementation_params = implementation_type_witnesses;
    implementation_params.extend(value_params);
    let deprecation = retained_host_function_deprecation(render.site);
    let deprecated = deprecation
        .as_deref()
        .map(super::skin::swift_deprecated_attribute)
        .unwrap_or_default();
    let member_deprecated = deprecation
        .as_deref()
        .map(|message| format!("\t{}", super::skin::swift_deprecated_attribute(message)))
        .unwrap_or_default();
    let mut implementation_associated_types = vec![format!(
        "{member_deprecated}\tassociatedtype H: {}\n",
        render.shapes.host_protocol()
    )];
    implementation_associated_types.extend(free_binders.iter().map(|binder| {
        format!(
            "{member_deprecated}\tassociatedtype KioOuterType_{}\n",
            binder.index()
        )
    }));
    let mut implementation_constraints = vec!["Implementation.H == H".to_owned()];
    implementation_constraints.extend(free_binders.iter().map(|binder| {
        format!(
            "Implementation.KioOuterType_{} == KioOuterType_{}",
            binder.index(),
            binder.index()
        )
    }));
    let implementation_where = format!(" where {}", implementation_constraints.join(", "));
    let declaration = format!(
        "\n{deprecated}public protocol {implementation_protocol} {{\n{}{member_deprecated}\tfunc call{method_generic}({}) -> {ret_type}\n}}\n\n{deprecated}public struct {name}{declaration_generic} {{\n\tlet __kioValue: Any\n\tinit(__kioBody value: Any) {{ self.__kioValue = value }}\n{member_deprecated}\tpublic init<Implementation: {implementation_protocol}>(_ implementation: Implementation){implementation_where} {{ self.__kioValue = {stored} }}\n{member_deprecated}\tpublic func call{method_generic}({}) -> {ret_type} {{\n\t\treturn {returned}\n\t}}\n}}\n",
        implementation_associated_types.concat(),
        implementation_params.join(", "),
        public_params.join(", ")
    );
    render.shapes.register_forall(name, declaration);
    Ok(public_type)
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
        presentation: presentation.root_uses(),
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
    callable
        .generic_params
        .iter()
        .filter(|generic| {
            callable
                .param_types
                .iter()
                .chain(std::iter::once(&callable.ret_type))
                .any(|ty| swift_type_mentions_identifier(ty, generic))
        })
        .cloned()
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
        presentation: presentation.root_uses(),
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
            let BoundaryFacadeExecutionUse::Function(layout) = render.presentation.use_at(id)
            else {
                return Err(EmitError::internal(
                    "Swift FFI function alias has no source presentation",
                ));
            };
            for (index, source) in layout.source_params().iter().enumerate() {
                let ty =
                    render_prepared_source_param_type(render, slots, source, context.scope, "H")?;
                let nested_alias = format!("{alias}_cbarg{index}");
                emit_prepared_ffi_alias(
                    &nested_alias,
                    &ty,
                    context.callable_generics,
                    context.names,
                    context.deprecation,
                    out,
                );
                if source.adapter() == CallableSourceParamAdapter::Identity {
                    let range = source.facade_slots();
                    let [slot] = &slots[range] else {
                        return Err(EmitError::internal(
                            "Swift nested FFI alias identity source does not own one slot",
                        ));
                    };
                    emit_prepared_nested_use_aliases(render, *slot, &nested_alias, context, out)?;
                }
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
        FacadeUse::Forall { .. } => {
            let carrier = super::skin::swift_forall_carrier_name(render.site.site(), id);
            let deprecated = context
                .deprecation
                .map(super::skin::swift_deprecated_attribute)
                .unwrap_or_default();
            out.push_str(&format!(
                "\n{deprecated}public typealias {alias}Implementation = {carrier}Implementation\n"
            ));
            let mut nested_scope = context.scope.clone();
            let mut nested_generics = context.callable_generics.to_vec();
            let mut current = id;
            while let FacadeUse::Forall { binder, result, .. } = render.plan.use_at(current) {
                nested_generics.push(nested_scope.bind(*binder));
                current = *result;
            }
            emit_prepared_nested_use_aliases(
                render,
                current,
                alias,
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
        let params = callable
            .param_types
            .iter()
            .enumerate()
            .map(|(index, ty)| format!("_ arg{index}: {}", escaping_param_ty(ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = if matches!(
            site.plan().facade().use_at(callable.ret_use),
            FacadeUse::Unit { .. }
        ) {
            String::new()
        } else {
            format!(" -> {}", callable.ret_type)
        };
        let signature = format!("func {method}{generic}({params}){ret}");
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

/// Every value group's param types, in declaration order — one inner
/// vec per group. The exported facade flattens these across groups
/// (`specs/backends/README.md` § Function-type FFI canonicalization)
/// while the internal body stays curried one layer per group.
fn value_group_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Vec<Option<Type<Routed>>>> {
    crate::backends::skin::erase_signature_value_groups(sig)
}

fn first_value_group_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Option<Type<Routed>>> {
    value_group_param_types(sig)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn signature_ret_type(sig: &crate::ast::Signature<Routed>, ret: &Type<Routed>) -> Type<Routed> {
    crate::backends::skin::erase_signature_ret(sig, ret)
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
    root: &ExportTree,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    render_export_ns_type_rec(root, &mut Vec::new(), prepared, shapes, names, &mut out)?;
    Ok(out)
}

fn render_export_ns_type_rec(
    node: &ExportTree,
    path: &mut Vec<FacadeSelector>,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
    names: &SwiftNames,
    out: &mut String,
) -> Result<(), EmitError> {
    for (selector, child) in &node.children {
        path.push(selector.clone());
        render_export_ns_type_rec(child, path, prepared, shapes, names, out)?;
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
        out.push_str(&render_export_method(e, prepared, shapes)?);
    }
    out.push_str("}\n\n");
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PreparedSwiftDir {
    In,
    Out,
}

#[derive(Clone, Copy)]
struct PreparedSwiftConversion<'a> {
    expression: &'a str,
    direction: PreparedSwiftDir,
    scope: &'a SwiftFacadeScope,
    depth: usize,
}

impl<'a> PreparedSwiftConversion<'a> {
    fn new(
        expression: &'a str,
        direction: PreparedSwiftDir,
        scope: &'a SwiftFacadeScope,
        depth: usize,
    ) -> Self {
        Self {
            expression,
            direction,
            scope,
            depth,
        }
    }
}

fn swift_right_nest(values: &[String]) -> Result<String, EmitError> {
    if values.is_empty() {
        return Err(EmitError::internal(
            "Swift prepared product adapter has no values",
        ));
    }
    Ok(format!("kioProductNest([{}])", values.join(", ")))
}

fn prepared_public_to_body_args(
    render: PreparedSwiftRender<'_, '_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    public_values: &[String],
    scope: &SwiftFacadeScope,
    depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count()
        || public_values.len() != layout.source_param_count()
    {
        return Err(EmitError::internal(
            "Swift prepared public/source stage alignment drift",
        ));
    }
    let mut body = Vec::with_capacity(layout.body_abi_arity());
    for (source, public) in layout.source_params().iter().zip(public_values) {
        let range = source.facade_slots();
        let source_slots = slots.get(range).ok_or_else(|| {
            EmitError::internal("Swift prepared source range exceeds facade slots")
        })?;
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => body.push("KioUnit()".to_owned()),
            CallableSourceParamAdapter::Identity => {
                let [slot] = source_slots else {
                    return Err(EmitError::internal(
                        "Swift identity source parameter does not own one slot",
                    ));
                };
                body.push(convert_prepared_use(
                    render,
                    *slot,
                    PreparedSwiftConversion::new(public, PreparedSwiftDir::In, scope, depth + 1),
                )?);
            }
            CallableSourceParamAdapter::RightNest => {
                let shell = source.product_shell().ok_or_else(|| {
                    EmitError::internal("Swift RightNest source has no prepared shell")
                })?;
                let converted = source_slots
                    .iter()
                    .zip(shell.ordered_keys())
                    .map(|(slot, key)| {
                        convert_prepared_use(
                            render,
                            *slot,
                            PreparedSwiftConversion::new(
                                &format!("{public}.{}", super::skin::swift_semantic_member(key)),
                                PreparedSwiftDir::In,
                                scope,
                                depth + 1,
                            ),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                body.push(swift_right_nest(&converted)?);
            }
        }
    }
    Ok(body)
}

fn prepared_body_to_public_args(
    render: PreparedSwiftRender<'_, '_>,
    slots: &[FacadeUseId],
    layout: &CallableValueStageLayout,
    body_values: &[String],
    scope: &SwiftFacadeScope,
    depth: usize,
) -> Result<Vec<String>, EmitError> {
    if slots.len() != layout.facade_slot_count() || body_values.len() != layout.body_abi_arity() {
        return Err(EmitError::internal(
            "Swift prepared body/source stage alignment drift",
        ));
    }
    let mut public = Vec::with_capacity(layout.source_param_count());
    for (source, body) in layout.source_params().iter().zip(body_values) {
        let range = source.facade_slots();
        let source_slots = slots.get(range).ok_or_else(|| {
            EmitError::internal("Swift prepared source range exceeds facade slots")
        })?;
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => public.push("KioUnit()".to_owned()),
            CallableSourceParamAdapter::Identity => {
                let [slot] = source_slots else {
                    return Err(EmitError::internal(
                        "Swift identity source parameter does not own one slot",
                    ));
                };
                public.push(convert_prepared_use(
                    render,
                    *slot,
                    PreparedSwiftConversion::new(body, PreparedSwiftDir::Out, scope, depth + 1),
                )?);
            }
            CallableSourceParamAdapter::RightNest => {
                let shell = source.product_shell().ok_or_else(|| {
                    EmitError::internal("Swift RightNest source has no prepared shell")
                })?;
                let value = format!("__kioSourceSlots{depth}");
                let fields = source_slots
                    .iter()
                    .zip(shell.ordered_keys())
                    .enumerate()
                    .map(|(index, (slot, key))| {
                        Ok(format!(
                            "{}: {}",
                            super::skin::swift_semantic_member(key),
                            convert_prepared_use(
                                render,
                                *slot,
                                PreparedSwiftConversion::new(
                                    &format!("{value}[{index}]"),
                                    PreparedSwiftDir::Out,
                                    scope,
                                    depth + 1,
                                ),
                            )?
                        ))
                    })
                    .collect::<Result<Vec<_>, EmitError>>()?;
                let ty = render_prepared_source_param_type(render, slots, source, scope, "H")?;
                public.push(format!(
                    "{{ () -> {ty} in let {value} = kioProductSlots({body}, {}); return {}({}) }}()",
                    source_slots.len(),
                    super::skin::swift_facade_shell_name(shell),
                    fields.join(", ")
                ));
            }
        }
    }
    Ok(public)
}

fn nominal_application<'a>(
    plan: &'a BoundaryFacadePlan,
    constructor: FacadeUseId,
    args: &'a [FacadeUseId],
) -> Option<(&'a QualifiedTypeName, &'a [FacadeUseId])> {
    match plan.use_at(constructor) {
        FacadeUse::Nominal { name, .. } => Some((name, args)),
        _ => None,
    }
}

fn convert_prepared_use(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    conversion: PreparedSwiftConversion<'_>,
) -> Result<String, EmitError> {
    let PreparedSwiftConversion {
        expression,
        direction,
        scope,
        depth,
    } = conversion;
    match render.plan.use_at(id) {
        FacadeUse::Unit { .. } => Ok(format!(
            "{{ () -> KioUnit in _ = {expression}; return KioUnit() }}()"
        )),
        FacadeUse::Bottom { .. } => Ok(match direction {
            PreparedSwiftDir::In => format!("({expression} as Any)"),
            PreparedSwiftDir::Out => format!("({expression} as! Never)"),
        }),
        FacadeUse::Bound { .. } => match direction {
            PreparedSwiftDir::In => Ok(format!("({expression} as Any)")),
            PreparedSwiftDir::Out => Ok(format!(
                "({expression} as! {})",
                render_prepared_use(render, id, scope, "H")?
            )),
        },
        FacadeUse::Nominal { name, .. } => {
            convert_prepared_nominal(render, id, name, &[], conversion)
        }
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            if let Some((name, args)) = nominal_application(render.plan, *constructor, args) {
                convert_prepared_nominal(render, id, name, args, conversion)
            } else {
                let ty = render_prepared_use(render, id, scope, "H")?;
                Ok(match direction {
                    PreparedSwiftDir::In => format!("{expression}.__kioValue"),
                    PreparedSwiftDir::Out => format!("{ty}({expression})"),
                })
            }
        }
        FacadeUse::Product { shell, args, .. } => {
            let ty = render_prepared_use(render, id, scope, "H")?;
            let value = format!("__kioProduct{depth}_{}", id.index());
            match direction {
                PreparedSwiftDir::In => {
                    let converted = args
                        .iter()
                        .zip(shell.ordered_keys())
                        .map(|(arg, key)| {
                            convert_prepared_use(
                                render,
                                *arg,
                                PreparedSwiftConversion::new(
                                    &format!("{value}.{}", super::skin::swift_semantic_member(key)),
                                    direction,
                                    scope,
                                    depth + 1,
                                ),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(format!(
                        "{{ () -> Any in let {value} = {expression}; return {} }}()",
                        swift_right_nest(&converted)?
                    ))
                }
                PreparedSwiftDir::Out => {
                    let fields = args
                        .iter()
                        .zip(shell.ordered_keys())
                        .enumerate()
                        .map(|(index, (arg, key))| {
                            Ok(format!(
                                "{}: {}",
                                super::skin::swift_semantic_member(key),
                                convert_prepared_use(
                                    render,
                                    *arg,
                                    PreparedSwiftConversion::new(
                                        &format!("{value}[{index}]"),
                                        direction,
                                        scope,
                                        depth + 1,
                                    ),
                                )?
                            ))
                        })
                        .collect::<Result<Vec<_>, EmitError>>()?;
                    Ok(format!(
                        "{{ () -> {ty} in let {value} = kioProductSlots({expression}, {}); return {}({}) }}()",
                        args.len(),
                        super::skin::swift_facade_shell_name(shell),
                        fields.join(", ")
                    ))
                }
            }
        }
        FacadeUse::Sum { shell, args, .. } => {
            if args.is_empty() {
                return Err(EmitError::internal(
                    "Swift prepared sum has no alternatives",
                ));
            }
            let ty = render_prepared_use(render, id, scope, "H")?;
            match direction {
                PreparedSwiftDir::In => {
                    let cases = args
                        .iter()
                        .zip(shell.ordered_keys())
                        .enumerate()
                        .map(|(index, (arg, key))| {
                            Ok(format!(
                                "case .{}(let __payload): return kioSumInject({}, {index}, {})",
                                super::skin::swift_semantic_member(key),
                                convert_prepared_use(
                                    render,
                                    *arg,
                                    PreparedSwiftConversion::new(
                                        "__payload",
                                        direction,
                                        scope,
                                        depth + 1,
                                    ),
                                )?,
                                args.len()
                            ))
                        })
                        .collect::<Result<Vec<_>, EmitError>>()?;
                    Ok(format!(
                        "{{ () -> Any in switch {expression} {{ {} }} }}()",
                        cases.join(" ")
                    ))
                }
                PreparedSwiftDir::Out => {
                    let cases = args
                        .iter()
                        .zip(shell.ordered_keys())
                        .enumerate()
                        .map(|(index, (arg, key))| {
                            Ok(format!(
                                "case {index}: return .{}({})",
                                super::skin::swift_semantic_member(key),
                                convert_prepared_use(
                                    render,
                                    *arg,
                                    PreparedSwiftConversion::new(
                                        "__payload",
                                        direction,
                                        scope,
                                        depth + 1,
                                    ),
                                )?
                            ))
                        })
                        .collect::<Result<Vec<_>, EmitError>>()?;
                    Ok(format!(
                        "{{ () -> {ty} in let (__variant, __payload) = kioSumPayload({expression}, {}); switch __variant {{ {} default: fatalError(\"kio: invalid prepared sum variant\") }} }}()",
                        args.len(),
                        cases.join(" ")
                    ))
                }
            }
        }
        FacadeUse::Function { slots, result, .. } => {
            convert_prepared_function(render, id, slots, *result, conversion)
        }
        FacadeUse::Forall { .. } => {
            let ty = render_prepared_forall_type(render, id, scope, "H")?;
            Ok(match direction {
                PreparedSwiftDir::In => format!("{expression}.__kioValue"),
                PreparedSwiftDir::Out => format!("{ty}(__kioBody: {expression})"),
            })
        }
    }
}

fn convert_prepared_nominal(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    name: &QualifiedTypeName,
    _args: &[FacadeUseId],
    conversion: PreparedSwiftConversion<'_>,
) -> Result<String, EmitError> {
    let declaration = render.site.nominals().declaration(name).ok_or_else(|| {
        EmitError::internal("Swift prepared conversion omitted a nominal dependency")
    })?;
    let ty = render_prepared_use(render, id, conversion.scope, "H")?;
    let expression = conversion.expression;
    let direction = conversion.direction;
    match declaration {
        BoundaryNominalDeclaration::HostType { type_params, .. } if type_params.is_empty() => {
            Ok(match direction {
                PreparedSwiftDir::In => format!("({expression} as Any)"),
                PreparedSwiftDir::Out => format!("({expression} as! {ty})"),
            })
        }
        BoundaryNominalDeclaration::HostType { .. } => Ok(match direction {
            PreparedSwiftDir::In => format!("{expression}.__kioValue"),
            PreparedSwiftDir::Out => format!("{ty}({expression})"),
        }),
        BoundaryNominalDeclaration::Newtype { .. } => Ok(match direction {
            PreparedSwiftDir::In => format!("{expression}.__kioValue"),
            PreparedSwiftDir::Out => format!("{ty}({expression})"),
        }),
    }
}

fn convert_prepared_function(
    render: PreparedSwiftRender<'_, '_>,
    id: FacadeUseId,
    slots: &[FacadeUseId],
    result: FacadeUseId,
    conversion: PreparedSwiftConversion<'_>,
) -> Result<String, EmitError> {
    let PreparedSwiftConversion {
        expression,
        direction,
        scope,
        depth,
    } = conversion;
    let BoundaryFacadeExecutionUse::Function(layout) = render.presentation.use_at(id) else {
        return Err(EmitError::internal(
            "Swift prepared function has no execution layout",
        ));
    };
    let public_ty = render_prepared_use(render, id, scope, "H")?;
    let body_ty = swift_closure_type(layout.body_abi_arity());
    let inner = format!("__kioBoundaryFn{depth}_{}", id.index());
    match direction {
        PreparedSwiftDir::Out => {
            let params = layout
                .source_params()
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    Ok(format!(
                        "__p{index}: {}",
                        render_prepared_source_param_type(render, slots, source, scope, "H",)?
                    ))
                })
                .collect::<Result<Vec<_>, EmitError>>()?;
            let values = (0..layout.source_param_count())
                .map(|index| format!("__p{index}"))
                .collect::<Vec<_>>();
            let body_args =
                prepared_public_to_body_args(render, slots, layout, &values, scope, depth + 1)?;
            let call = format!("({inner} as! {body_ty})({})", body_args.join(", "));
            let returned = convert_prepared_use(
                render,
                result,
                PreparedSwiftConversion::new(&call, PreparedSwiftDir::Out, scope, depth + 1),
            )?;
            Ok(format!(
                "{{ ({inner}: Any) -> {public_ty} in return {{ ({}) in return {returned} }} }}({expression})",
                params.join(", ")
            ))
        }
        PreparedSwiftDir::In => {
            let params = (0..layout.body_abi_arity())
                .map(|index| format!("__b{index}: Any"))
                .collect::<Vec<_>>();
            let values = (0..layout.body_abi_arity())
                .map(|index| format!("__b{index}"))
                .collect::<Vec<_>>();
            let public_args =
                prepared_body_to_public_args(render, slots, layout, &values, scope, depth + 1)?;
            let call = format!("({inner} as! {public_ty})({})", public_args.join(", "));
            let returned = convert_prepared_use(
                render,
                result,
                PreparedSwiftConversion::new(&call, PreparedSwiftDir::In, scope, depth + 1),
            )?;
            Ok(format!(
                "{{ ({inner}: Any) -> {body_ty} in return {{ ({}) -> Any in return {returned} }} }}({expression} as Any)",
                params.join(", ")
            ))
        }
    }
}

/// Rebuild the private erased CPS value behind one direct existential
/// projector. The public facade retains every quantified stage; only this
/// declaration-owned body adapter consumes the prepared exact-ID compaction
/// proof.
fn render_prepared_existential_projector_body(
    render: PreparedSwiftRender<'_, '_>,
    returned: FacadeUseId,
    payload: &str,
) -> Result<String, EmitError> {
    let compactable = live_execution(render.site)?.direct_projector_compactable_foralls();
    if !compactable.contains(&returned) {
        return Err(EmitError::internal(
            "Swift existential projector lacks its prepared direct-compaction proof",
        ));
    }

    let mut selected_stage_count = 0usize;
    let mut selected_body = returned;
    while compactable.contains(&selected_body) {
        let FacadeUse::Forall { result, .. } = render.plan.use_at(selected_body) else {
            return Err(EmitError::internal(
                "Swift existential projector compaction proof does not name a forall",
            ));
        };
        selected_stage_count += 1;
        selected_body = *result;
    }
    let FacadeUse::Function {
        slots: continuation_slots,
        ..
    } = render.plan.use_at(selected_body)
    else {
        return Err(EmitError::internal(
            "Swift existential projector selected-result stage is not callable",
        ));
    };
    let BoundaryFacadeExecutionUse::Function(selected_layout) =
        render.presentation.use_at(selected_body)
    else {
        return Err(EmitError::internal(
            "Swift existential projector selected-result stage has no execution layout",
        ));
    };
    let [continuation_root] = continuation_slots.as_slice() else {
        return Err(EmitError::internal(
            "Swift existential projector does not expose one continuation",
        ));
    };
    let [continuation_source] = selected_layout.source_params() else {
        return Err(EmitError::internal(
            "Swift existential projector continuation has no exact source layout",
        ));
    };
    if selected_layout.source_param_count() != 1
        || selected_layout.body_abi_arity() != 1
        || selected_layout.facade_slot_count() != 1
        || continuation_source.facade_slots() != (0..1)
        || continuation_source.adapter() != CallableSourceParamAdapter::Identity
    {
        return Err(EmitError::internal(
            "Swift existential projector continuation layout drifted",
        ));
    }

    let mut hidden_stage_count = 0usize;
    let mut payload_function = *continuation_root;
    while compactable.contains(&payload_function) {
        let FacadeUse::Forall { result, .. } = render.plan.use_at(payload_function) else {
            return Err(EmitError::internal(
                "Swift existential continuation compaction proof does not name a forall",
            ));
        };
        hidden_stage_count += 1;
        payload_function = *result;
    }
    if selected_stage_count + hidden_stage_count != compactable.len() {
        return Err(EmitError::internal(
            "Swift existential projector left a prepared compaction stage unconsumed",
        ));
    }
    let FacadeUse::Function { .. } = render.plan.use_at(payload_function) else {
        return Err(EmitError::internal(
            "Swift existential projector continuation is not callable",
        ));
    };
    let BoundaryFacadeExecutionUse::Function(payload_layout) =
        render.presentation.use_at(payload_function)
    else {
        return Err(EmitError::internal(
            "Swift existential projector payload has no execution layout",
        ));
    };

    let continuation = "__kioContinuation";
    let mut advanced = continuation.to_owned();
    for _ in 0..hidden_stage_count {
        advanced = format!("({advanced} as! (() -> Any))()");
    }
    let call = match payload_layout.body_abi_arity() {
        0 if payload_layout.source_param_count() == 0 => {
            format!("({advanced} as! (() -> Any))()")
        }
        1 if payload_layout.source_param_count() == 1 => {
            format!("({advanced} as! ((Any) -> Any))({payload})")
        }
        arity => {
            return Err(EmitError::internal(format!(
                "Swift existential projector payload occupies {arity} body values"
            )));
        }
    };
    let mut body = format!("{{ ({continuation}: Any) -> Any in return {call} }}");
    for _ in 0..selected_stage_count {
        body = format!("{{ () -> Any in return {body} }}");
    }
    Ok(body)
}

/// Render one exported method from its exact prepared callable plan and paired
/// source execution layout.
fn render_export_method(
    entry: &ExportEntry,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &super::skin::PreparedSwiftShapes,
) -> Result<String, EmitError> {
    let site = prepared_site(prepared, &entry.site)?;
    let execution = live_execution(site)?;
    let render = PreparedSwiftRender {
        site,
        plan: site.plan().facade(),
        presentation: site.presentation().root_uses(),
        shapes,
    };
    let callable = render_prepared_callable(site, shapes, "H")?;
    let generic = super::skin::swift_generic_use(&swift_public_method_generics(&callable));
    let params = callable
        .param_types
        .iter()
        .enumerate()
        .map(|(index, ty)| format!("_ arg{index}: {}", escaping_param_ty(ty)))
        .collect::<Vec<_>>();
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

    let internal = match &entry.body {
        ExportBody::ModuleFn { method } => {
            let mut applied = format!("self.{PACKAGE_BACKREF_MEMBER}.{method}");
            for (index, (runtime, args)) in execution
                .head_stages()
                .iter()
                .zip(&stage_body_args)
                .enumerate()
            {
                match runtime {
                    CallableExecutionStage::Type { .. } if index == 0 => applied.push_str("()"),
                    CallableExecutionStage::Type { .. } => {
                        applied = format!("({applied} as! (() -> Any))()")
                    }
                    CallableExecutionStage::Value(layout) if index == 0 => {
                        debug_assert_eq!(layout.body_abi_arity(), args.len());
                        applied.push('(');
                        applied.push_str(&args.join(", "));
                        applied.push(')');
                    }
                    CallableExecutionStage::Value(layout) => {
                        debug_assert_eq!(layout.body_abi_arity(), args.len());
                        applied = format!(
                            "({applied} as! {})({})",
                            swift_closure_type(layout.body_abi_arity()),
                            args.join(", ")
                        );
                    }
                }
            }
            applied
        }
        ExportBody::NewtypeConstructor => stage_body_args
            .iter()
            .flatten()
            .next()
            .cloned()
            .unwrap_or_else(|| "KioUnit()".to_owned()),
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
                target.clone()
            } else {
                render_prepared_existential_projector_body(render, callable.ret_use, target)?
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
        format!(
            "return {}",
            convert_prepared_use(
                render,
                callable.ret_use,
                PreparedSwiftConversion::new(&internal, PreparedSwiftDir::Out, &callable.scope, 0,),
            )?
        )
    };
    Ok(format!(
        "\tpublic func {method_name}{generic}({}) -> {} {{\n\t\t{returned}\n\t}}\n",
        params.join(", "),
        callable.ret_type,
    ))
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

/// Render a Swift `struct` with a stored property + public memberwise init
/// per field. `fields` is a `(name, type)` list.
pub(crate) fn render_swift_struct(
    name: &str,
    fields: &[(String, String)],
    deprecation_message: Option<&str>,
) -> String {
    let deprecated = deprecation_message
        .map(super::skin::swift_deprecated_attribute)
        .unwrap_or_default();
    let member_deprecated = deprecation_message
        .map(|message| format!("\t{}", super::skin::swift_deprecated_attribute(message)))
        .unwrap_or_default();
    let mut out = format!("{deprecated}public struct {name} {{\n");
    for (fname, ftype) in fields {
        out.push_str(&format!(
            "{member_deprecated}\tpublic let {fname}: {ftype}\n"
        ));
    }
    let init_params: Vec<String> = fields
        .iter()
        .map(|(n, t)| format!("{n}: {}", escaping_param_ty(t)))
        .collect();
    out.push_str(&format!(
        "{member_deprecated}\tpublic init({}) {{\n",
        init_params.join(", ")
    ));
    for (fname, _) in fields {
        out.push_str(&format!("\t\tself.{fname} = {fname}\n"));
    }
    out.push_str("\t}\n}\n");
    out
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
        out.push_str(&render_export_method(e, prepared, shapes)?);
    }
    for entry in prepared
        .public_newtypes()
        .filter(|entry| !entry.type_params().is_empty())
    {
        let args = (0..entry.type_params().len())
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>();
        let generics = super::skin::swift_generic_use(&args);
        let mut nominal_args = vec!["H".to_owned()];
        nominal_args.extend(args.iter().cloned());
        let nominal = format!(
            "{}{}",
            super::skin::swift_newtype_carrier_name(entry.name()),
            super::skin::swift_generic_use(&nominal_args)
        );
        let marker = format!(
            "{}<H>",
            super::skin::swift_newtype_constructor_name(entry.name())
        );
        let application = render_swift_application(&marker, &args);
        let stem = format!(
            "KioNewtypeApplication_{}",
            super::skin::swift_nominal_identity(entry.name())
        );
        out.push_str(&format!(
            "\tpublic func {stem}_lift{generics}(_ value: {nominal}) -> {application} {{ .init(value.__kioValue) }}\n\
             \tpublic func {stem}_project{generics}(_ value: {application}) -> {nominal} {{ .init(value.__kioValue) }}\n"
        ));
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
            let runtime_groups = runtime_param_name_groups(&f.sig);
            let mut emitter = BodyEmitter::new(
                module_key,
                &selective_imports,
                &qualified_imports,
                prepared,
                shapes,
                literal_shapes,
            );
            for param in &f.sig.params {
                if let crate::ast::SignatureParam::Value(value) = param
                    && let Some(ty) = &value.ty
                {
                    emitter.local_types.insert(value.name.clone(), ty.clone());
                }
            }
            for g in &runtime_groups {
                for n in g {
                    emitter.locals.push((*n).to_owned());
                }
            }
            let body = emitter.emit_expr(&f.body)?;
            let body = emitter.adapt_fn_value_for_type(body, &f.body, &f.ret);
            let outer = runtime_groups.first().cloned().unwrap_or_default();
            let outer_list: String = outer
                .iter()
                .map(|n| format!("_ {}: Any", swift_local_ident(n)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut acc = body;
            for g in runtime_groups.iter().skip(1).rev() {
                let params: String = g
                    .iter()
                    .map(|n| format!("{}: Any", swift_local_ident(n)))
                    .collect::<Vec<_>>()
                    .join(", ");
                acc = format!("{{ ({params}) -> Any in return {acc} }}");
            }
            out.push_str(&format!(
                "\tfunc {method}({outer_list}) -> Any {{\n\t\treturn {acc}\n\t}}\n"
            ));
        }
    }
    Ok(out)
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

/// Runtime closure layers for a signature. Each type binder becomes its own
/// empty layer, each value group keeps its parameter names, and a signature
/// with no value group receives the language's synthesized trailing nullary
/// value stage.
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

fn runtime_group_arities(sig: &crate::ast::Signature<Routed>) -> Vec<usize> {
    runtime_param_name_groups(sig)
        .into_iter()
        .map(|group| group.len())
        .collect()
}

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
        .into_iter()
        .map(|(name, owner)| {
            let method = module_fn_method_name(&owner, &name);
            (name, method)
        })
        .collect()
}

/// Walks a module fn body and renders Swift expression source over the
/// universal erased value model (`Any`). The enclosing method's `self` is
/// the package handle, so host / module calls reach the reserved host storage
/// member / `self.<method>`.
struct BodyEmitter<'a, 'p> {
    locals: Vec<String>,
    local_types: HashMap<String, Type<Routed>>,
    fresh: usize,
    module_key: &'a str,
    selective_imports: &'a SelectiveImports,
    qualified_imports: &'a QualifiedImports,
    prepared: &'a PreparedBoundaryCallableSites,
    facade_shapes: &'a super::skin::PreparedSwiftShapes,
    shapes: &'a super::skin::SwiftLiteralContext<'p>,
}

struct SwiftTypeRecon<'e, 'a, 'p> {
    emit: &'e BodyEmitter<'a, 'p>,
}

impl SwiftTypeRecon<'_, '_, '_> {
    fn newtype_member_type(
        &self,
        module_path: &str,
        newtype: &str,
        member: &str,
        type_args: &[Type<Routed>],
    ) -> Option<Type<Routed>> {
        let entry = self.emit.shapes.package().module(module_path)?;
        let mut found = None;
        for item in &entry.module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(candidate) = declaration.newtype()
                    && candidate.name == newtype
                {
                    found = Some(candidate);
                }
            });
            if found.is_some() {
                break;
            }
        }
        let decl = found?;
        let universals = type_args.get(..decl.type_params.len())?.to_vec();
        let mut segments = module_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        segments.push(newtype.to_owned());
        if member == decl.constructor.name {
            return Some(Type::synth_path(segments, universals, decl.meta.span));
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
        Some(apply_subst(&decl.payload, &subst))
    }
}

impl ReconProfile for SwiftTypeRecon<'_, '_, '_> {
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
        ty.clone()
    }

    fn enriched_field_payload_type(&self, field_ty: &Type<Routed>) -> Option<Type<Routed>> {
        let (decl, _) = resolve_newtype(field_ty, self.emit.shapes.package())?;
        Some(instantiate_boundary_newtype_payload(decl, field_ty))
    }

    fn module_fn_value_type(
        &self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
        span: crate::span::Span,
    ) -> Option<Type<Routed>> {
        let method = self
            .emit
            .selective_imports
            .get(mangled)
            .cloned()
            .unwrap_or_else(|| module_fn_method_name(self.emit.module_key, mangled));
        for (module_path, entry) in self.emit.shapes.package().modules() {
            for item in &entry.module.items {
                let crate::ast::Item::FnDef(def) = item else {
                    continue;
                };
                if module_fn_method_name(module_path, &def.name) == method {
                    return Some(sig.signature_ty(signature_ret_type(&def.sig, &def.ret), span));
                }
            }
        }
        None
    }

    fn reconstruct_other(
        &self,
        expr: &Expr<Routed>,
        _locals: &mut HashMap<String, Type<Routed>>,
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
            } => self.newtype_member_type(self.emit.module_key, newtype, member, type_args),
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                ..
            } => self.newtype_member_type(module_path, newtype, member, type_args),
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

impl<'a, 'p> BodyEmitter<'a, 'p> {
    fn new(
        module_key: &'a str,
        selective_imports: &'a SelectiveImports,
        qualified_imports: &'a QualifiedImports,
        prepared: &'a PreparedBoundaryCallableSites,
        facade_shapes: &'a super::skin::PreparedSwiftShapes,
        shapes: &'a super::skin::SwiftLiteralContext<'p>,
    ) -> Self {
        BodyEmitter {
            locals: Vec::new(),
            local_types: HashMap::new(),
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
        let recon = SwiftTypeRecon { emit: self };
        ReconProfile::value_type_with_locals(&recon, expr, &mut HashMap::new())
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
                args,
                sig,
                ret_ty,
                ..
            } => self.emit_low_host_call(name, module_path, args, sig, ret_ty),
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
                ..
            } => self.emit_inject(payload, *variant, *variants),
            Expr::EnrichedMatch {
                scrutinee,
                arms,
                scrutinee_ty,
                ..
            } => self.emit_match(scrutinee, arms, scrutinee_ty),
            Expr::EnrichedRecord { fields, .. } => {
                let items: Vec<&Expr<Routed>> = fields.iter().map(|f| &f.value).collect();
                self.emit_tuple_refs(&items)
            }
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                ..
            } => self.emit_project(target, *index, *arity),
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => self.emit_module_call(mangled, type_args.len(), args, sig),
            Expr::LowQualifiedModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => self.emit_qualified_module_call(mangled, type_args.len(), args, sig),
            Expr::LowNewtypeCtor { payload, .. } => self.emit_expr(payload),
            Expr::LowNewtypeProj { target, .. } => self.emit_expr(target),
            Expr::LowAbsurdCall { value_arg, .. } => {
                let arg = self.emit_expr(value_arg)?;
                Ok(format!(
                    "{{ () -> Any in _ = {arg}; fatalError(\"kio: __absurd__ reached\") }}()"
                ))
            }
            Expr::FnExpr { sig, body, .. } => self.emit_fn_expr(sig, body),
            Expr::LowClosureCall {
                name,
                type_args,
                args,
                ..
            } => {
                let callee = swift_local_ident(name);
                let callee = self.emit_erased_type_applications(callee, type_args.len());
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
                        let param_tys = first_value_group_param_types(sig);
                        self.emit_apply_adapting(&c, args, &param_tys)
                    }
                    _ => self.emit_apply_erased(&c, args),
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
            Expr::LowQualifiedNewtypeMember { payload, .. } => self.emit_expr(payload),
        }
    }

    fn emit_cps_projector_apply(
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
        let advanced = self.emit_erased_type_applications(continuation_name.clone(), type_stages);
        let args = if *abi_arity == 0 {
            String::new()
        } else {
            receiver_name.clone()
        };
        Ok(format!(
            "{{ () -> Any in let {receiver_name} = {recv}; let {continuation_name} = {cont}; return ({advanced} as! {})({args}) }}()",
            swift_closure_type(*abi_arity)
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
            presentation: site.presentation().root_uses(),
            shapes: self.facade_shapes,
        };
        let callable = render_prepared_callable(site, self.facade_shapes, "H")?;
        let body_scope = callable.scope.erased();
        let method = host_member_name(module_path, name);
        let entry = site.plan().entry();
        let mut stage_body_values = Vec::<Vec<String>>::new();
        let mut public_args = Vec::new();
        let mut body_index = 0usize;
        for (semantic, runtime) in entry.head_stages.iter().zip(execution.head_stages()) {
            match (semantic, runtime) {
                (BoundaryCallableHeadStage::Type { .. }, CallableExecutionStage::Type { .. }) => {
                    stage_body_values.push(Vec::new());
                }
                (
                    BoundaryCallableHeadStage::Value { slots },
                    CallableExecutionStage::Value(layout),
                ) => {
                    let values = (0..layout.body_abi_arity())
                        .map(|_| {
                            let value = format!("__p{body_index}");
                            body_index += 1;
                            value
                        })
                        .collect::<Vec<_>>();
                    public_args.extend(prepared_body_to_public_args(
                        render,
                        slots,
                        layout,
                        &values,
                        &body_scope,
                        0,
                    )?);
                    stage_body_values.push(values);
                }
                _ => {
                    return Err(EmitError::internal(
                        "Swift host value reference has mismatched prepared stages",
                    ));
                }
            }
        }
        let call = format!(
            "self.{HOST_STORAGE_MEMBER}.{method}({})",
            public_args.join(", ")
        );
        let mut body = if matches!(
            site.plan().facade().use_at(callable.ret_use),
            FacadeUse::Unit { .. }
        ) {
            format!("{{ () -> Any in {call}; return KioUnit() }}()")
        } else {
            let ret_type = render_prepared_use(render, callable.ret_use, &body_scope, "H")?;
            let call = format!("({call} as {ret_type})");
            convert_prepared_use(
                render,
                callable.ret_use,
                PreparedSwiftConversion::new(&call, PreparedSwiftDir::In, &body_scope, 0),
            )?
        };
        let mut body_cursor = body_index;
        for (runtime, values) in execution.head_stages().iter().zip(&stage_body_values).rev() {
            match runtime {
                CallableExecutionStage::Type { .. } => {
                    body = format!("{{ () -> Any in return {body} }}")
                }
                CallableExecutionStage::Value(layout) => {
                    body_cursor -= layout.body_abi_arity();
                    let params = values
                        .iter()
                        .map(|value| format!("{value}: Any"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    body = format!("{{ ({params}) -> Any in return {body} }}");
                }
            }
        }
        debug_assert_eq!(body_cursor, 0);
        Ok(format!("({body} as Any)"))
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
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let method = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_method_name(self.module_key, mangled),
        };
        // The package-handle method binds the fn's **first** value group and
        // returns nested closures for the inner groups (see
        // `render_module_fns`). So the value-ref closure binds only the
        // first group's params and forwards them to `self.method(...)`;
        // the method's own currying supplies the inner-group layers, which
        // a per-group application (`get()(x)(y)`) reaches through the
        // erased-call casts (`(… as! ClosureType)(…)`). Binding *every*
        // group's params in one flat closure and passing them all to
        // `self.method` is wrong on two counts — the method takes only the
        // first group, and the flat closure can't be applied one group at
        // a time — so a multi-group module-fn value failed at runtime.
        let first_group = runtime_group_arities(sig).first().copied().unwrap_or(0);
        let params: Vec<String> = (0..first_group).map(|i| format!("__p{i}: Any")).collect();
        let fwd: Vec<String> = (0..first_group).map(|i| format!("__p{i}")).collect();
        Ok(format!(
            "({{ ({}) -> Any in return self.{method}({}) }} as Any)",
            params.join(", "),
            fwd.join(", ")
        ))
    }

    /// Lower a Kio closure `.(x, y) { body }` to a Swift closure. A value
    /// group with `n ≥ 2` binders destructures a single product param; the
    /// closure literal stays multi-param and a flow site adapts it.
    fn emit_fn_expr(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let groups = runtime_param_name_groups(sig);
        let param_types: HashMap<&str, Type<Routed>> = sig
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
        let mut pushed = 0usize;
        let mut prior_types = Vec::new();
        for g in &groups {
            for n in g {
                self.locals.push((*n).to_owned());
                let prior = self.local_types.remove(*n);
                if let Some(ty) = param_types.get(*n) {
                    self.local_types.insert((*n).to_owned(), ty.clone());
                }
                prior_types.push(((*n).to_owned(), prior));
                pushed += 1;
            }
        }
        let body_src = self.emit_expr(body);
        for _ in 0..pushed {
            self.locals.pop();
        }
        for (name, prior) in prior_types.into_iter().rev() {
            self.local_types.remove(&name);
            if let Some(ty) = prior {
                self.local_types.insert(name, ty);
            }
        }
        let body_src = body_src?;
        let mut acc = body_src;
        for g in groups.iter().rev() {
            let params: String = g
                .iter()
                .map(|n| format!("{}: Any", swift_local_ident(n)))
                .collect::<Vec<_>>()
                .join(", ");
            acc = format!("{{ ({params}) -> Any in return {acc} }}");
        }
        Ok(format!("({acc} as Any)"))
    }

    /// Adapt a fn-valued expression `rendered` to the product-grouping the
    /// function type `expected` requires, when the two differ.
    fn adapt_fn_value_for_type(
        &self,
        rendered: String,
        source: &Expr<Routed>,
        expected: &Type<Routed>,
    ) -> String {
        let Some(sig) = underlying_fn_expr_sig(source) else {
            return rendered;
        };
        let expected = peel_forall(expected);
        let Type::Function {
            param, abi_arity, ..
        } = expected
        else {
            return rendered;
        };
        let groups = value_param_name_groups(sig);
        let Some(first_group) = groups.first() else {
            return rendered;
        };
        let binder_count = first_group.len();
        let type_arity = Type::right_spine_take(param, *abi_arity).len();
        if binder_count == type_arity || groups.len() > 1 {
            return rendered;
        }
        if type_arity == 1 && binder_count >= 2 {
            let peels: Vec<String> = (0..binder_count)
                .map(|i| product_param_slot_access("__a0", i, binder_count))
                .collect();
            return format!(
                "({{ (__a0: Any) -> Any in return ({rendered} as! {})({}) }} as Any)",
                swift_closure_type(binder_count),
                peels.join(", ")
            );
        }
        rendered
    }

    fn emit_apply(&mut self, callee: &str, args: &[Expr<Routed>]) -> Result<String, EmitError> {
        self.emit_apply_erased(callee, args)
    }

    /// Consume one hidden nullary closure per erased type application.
    /// Each application remains an ordinary runtime call boundary.
    fn emit_erased_type_applications(&self, mut callee: String, count: usize) -> String {
        for _ in 0..count {
            callee = format!("({callee} as! (() -> Any))()")
        }
        callee
    }

    fn emit_apply_erased(
        &mut self,
        callee: &str,
        args: &[Expr<Routed>],
    ) -> Result<String, EmitError> {
        self.emit_apply_adapting(callee, args, &[])
    }

    /// Apply an `Any`-typed callee by casting it to the arity-matched Swift
    /// closure type `(Any, …) -> Any` and calling. Each fn-valued arg is
    /// adapted to the callee's declared param type when known.
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
                Some(ty) => self.adapt_fn_value_for_type(rendered, a, ty),
                None => rendered,
            };
            emitted.push(rendered);
        }
        let fn_ty = swift_closure_type(args.len());
        Ok(format!("({callee} as! {fn_ty})({})", emitted.join(", ")))
    }

    fn emit_qualified_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let (alias, leaf) = mangled.split_once('.').ok_or_else(|| {
            EmitError::unsupported(format!(
                "Swift emitter: qualified module call `{mangled}` is not `<alias>.<leaf>`"
            ))
        })?;
        let module_key = self.qualified_imports.get(alias).ok_or_else(|| {
            EmitError::unsupported(format!(
                "Swift emitter: qualified-call alias `{alias}` has no `import … as {alias};` mapping"
            ))
        })?;
        let method = module_fn_method_name(module_key, leaf);
        self.emit_grouped_method_call(&method, type_arg_count, args, sig)
    }

    /// Emit a call to a package-handle module-fn method `method` over `args`,
    /// honoring currying.
    fn emit_grouped_method_call(
        &mut self,
        method: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let group_arities = value_group_arities(sig);
        let first = group_arities.first().copied().unwrap_or(0);
        let param_tys = sig_all_value_param_types(sig);
        let mut emitted = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            let v = match param_tys.get(i).and_then(|t| t.as_ref()) {
                Some(t) => self.adapt_fn_value_for_type(v, a, t),
                None => v,
            };
            emitted.push(v);
        }
        // A direct module call carries its leading type applications on
        // this node. The method itself is the first nullary type stage;
        // further binders are nullary closures returned from it. Bind the
        // staged callee before evaluating any value argument so the
        // observable type-stage effects stay ordered ahead of them.
        if type_arg_count > 0 {
            let staged =
                self.emit_erased_type_applications(format!("self.{method}()"), type_arg_count - 1);
            if args.len() < first {
                let missing = first - args.len();
                let fresh: Vec<String> = (0..missing).map(|i| format!("__pa{i}")).collect();
                let params = fresh
                    .iter()
                    .map(|p| format!("{p}: Any"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut all_args = emitted.clone();
                all_args.extend(fresh.iter().cloned());
                let applied_ty = swift_closure_type(all_args.len());
                return Ok(format!(
                    "{{ () -> Any in let __kioTypeCallee: Any = {staged}; return ({{ ({params}) -> Any in return (__kioTypeCallee as! {applied_ty})({}) }} as Any) }}()",
                    all_args.join(", ")
                ));
            }
            let first_args = &emitted[..first];
            let mut call = format!(
                "(__kioTypeCallee as! {})({})",
                swift_closure_type(first),
                first_args.join(", ")
            );
            let mut consumed = first;
            for arity in group_arities.iter().skip(1) {
                if consumed >= emitted.len() {
                    break;
                }
                let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
                call = format!(
                    "({call} as! {})({})",
                    swift_closure_type(slice.len()),
                    slice.join(", ")
                );
                consumed += arity;
            }
            return Ok(format!(
                "{{ () -> Any in let __kioTypeCallee: Any = {staged}; return {call} }}()"
            ));
        }
        if args.len() < first {
            let missing = first - args.len();
            let fresh: Vec<String> = (0..missing).map(|i| format!("__pa{i}")).collect();
            let params: String = fresh
                .iter()
                .map(|p| format!("{p}: Any"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut all_args = emitted.clone();
            all_args.extend(fresh.iter().cloned());
            return Ok(format!(
                "({{ ({params}) -> Any in return self.{method}({}) }} as Any)",
                all_args.join(", ")
            ));
        }
        let mut call = format!("self.{method}({})", emitted[..first].join(", "));
        let mut consumed = first;
        for arity in group_arities.iter().skip(1) {
            if consumed >= emitted.len() {
                break;
            }
            let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
            let fn_ty = swift_closure_type(slice.len());
            call = format!("({call} as! {fn_ty})({})", slice.join(", "));
            consumed += arity;
        }
        Ok(call)
    }

    fn emit_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let method = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_method_name(self.module_key, mangled),
        };
        self.emit_grouped_method_call(&method, type_arg_count, args, sig)
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
    /// `[a, [b, c]] as [Any]`.
    fn emit_tuple_typed(
        &mut self,
        items: &[Expr<Routed>],
        synth_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let n = items.len();
        if n == 0 {
            return Ok("(KioUnit())".to_owned());
        }
        let slots = Type::right_spine_product(synth_ty);
        let item_refs: Vec<&Expr<Routed>> = items.iter().collect();
        if let Some(plan) = bound_product_rebuild_plan(&item_refs) {
            return self.emit_tuple_with_cached_slots(&item_refs, Some(&slots), &plan);
        }
        let mut rendered = Vec::with_capacity(n);
        for i in 0..n {
            rendered.push(self.emit_tuple_item(items, &slots, i)?);
        }
        Ok(Self::emit_tuple_from_rendered(&rendered))
    }

    fn emit_tuple_item(
        &mut self,
        items: &[Expr<Routed>],
        slots: &[&Type<Routed>],
        i: usize,
    ) -> Result<String, EmitError> {
        let v = self.emit_expr(&items[i])?;
        match slots.get(i) {
            Some(slot) if matches!(slot, Type::Function { .. }) => {
                Ok(self.adapt_fn_value_for_type(v, &items[i], slot))
            }
            _ => Ok(v),
        }
    }

    fn emit_tuple_refs(&mut self, items: &[&Expr<Routed>]) -> Result<String, EmitError> {
        let n = items.len();
        if n == 0 {
            return Ok("(KioUnit())".to_owned());
        }
        if let Some(plan) = bound_product_rebuild_plan(items) {
            return self.emit_tuple_with_cached_slots(items, None, &plan);
        }
        let mut rendered = Vec::with_capacity(n);
        for item in items {
            rendered.push(self.emit_expr(item)?);
        }
        Ok(Self::emit_tuple_from_rendered(&rendered))
    }

    fn emit_tuple_from_rendered(rendered: &[String]) -> String {
        let n = rendered.len();
        debug_assert!(n > 0);
        let mut acc = rendered[n - 1].clone();
        for head in rendered[..n - 1].iter().rev() {
            acc = format!("[{head}, {acc}] as [Any]");
        }
        format!("({acc} as Any)")
    }

    fn emit_tuple_with_cached_slots(
        &mut self,
        items: &[&Expr<Routed>],
        slot_tys: Option<&[&Type<Routed>]>,
        plan: &ProductRebuildPlan,
    ) -> Result<String, EmitError> {
        render_cached_product_rebuild(
            plan,
            |slot| format!("__slots[{slot}]"),
            |i| {
                let item = items[i];
                let v = self.emit_expr(item)?;
                match slot_tys.and_then(|slots| slots.get(i)).copied() {
                    Some(slot_ty) if matches!(slot_ty, Type::Function { .. }) => {
                        Ok(self.adapt_fn_value_for_type(v, item, slot_ty))
                    }
                    _ => Ok(v),
                }
            },
            Self::emit_tuple_from_rendered,
            |source, source_arity, tuple| {
                let source = swift_local_ident(source);
                format!(
                    "{{ () -> Any in let __slots = kioProductSlots({source}, {source_arity}); return {tuple} }}()"
                )
            },
        )
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
    ) -> Result<String, EmitError> {
        let p = self.emit_expr(payload)?;
        Ok(format!("kioSumInject({p}, {variant}, {variants})"))
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
        args: &[Expr<Routed>],
        _sig: &crate::ast::Signature<Routed>,
        _ret_ty: &Type<Routed>,
    ) -> Result<String, EmitError> {
        let site_id = host_function_site_id(module_path, name);
        let site = prepared_site(self.prepared, &site_id)?;
        let execution = live_execution(site)?;
        let render = PreparedSwiftRender {
            site,
            plan: site.plan().facade(),
            presentation: site.presentation().root_uses(),
            shapes: self.facade_shapes,
        };
        let callable = render_prepared_callable(site, self.facade_shapes, "H")?;
        let body_scope = callable.scope.erased();
        let method = host_member_name(module_path, name);
        let body_values = args
            .iter()
            .map(|arg| self.emit_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;
        let entry = site.plan().entry();
        let expected_body_arity = execution
            .head_stages()
            .iter()
            .map(|stage| match stage {
                CallableExecutionStage::Type { .. } => 0,
                CallableExecutionStage::Value(layout) => layout.body_abi_arity(),
            })
            .sum::<usize>();
        if expected_body_arity != body_values.len() {
            return Err(EmitError::unsupported(format!(
                "Swift emitter: host fn `{name}` arity mismatch ({} params, {} args)",
                expected_body_arity,
                body_values.len()
            )));
        }
        let mut body_cursor = 0usize;
        let mut public_args = Vec::new();
        for (semantic, runtime) in entry.head_stages.iter().zip(execution.head_stages()) {
            let (BoundaryCallableHeadStage::Value { slots }, CallableExecutionStage::Value(layout)) =
                (semantic, runtime)
            else {
                continue;
            };
            let end = body_cursor + layout.body_abi_arity();
            public_args.extend(prepared_body_to_public_args(
                render,
                slots,
                layout,
                &body_values[body_cursor..end],
                &body_scope,
                0,
            )?);
            body_cursor = end;
        }
        let call = format!(
            "self.{HOST_STORAGE_MEMBER}.{method}({})",
            public_args.join(", ")
        );
        if matches!(
            site.plan().facade().use_at(callable.ret_use),
            FacadeUse::Unit { .. }
        ) {
            Ok(format!("{{ () -> Any in {call}; return KioUnit() }}()"))
        } else {
            let ret_type = render_prepared_use(render, callable.ret_use, &body_scope, "H")?;
            let call = format!("({call} as {ret_type})");
            let converted = convert_prepared_use(
                render,
                callable.ret_use,
                PreparedSwiftConversion::new(&call, PreparedSwiftDir::In, &body_scope, 0),
            )?;
            Ok(format!("{{ () -> Any in return {converted} }}()"))
        }
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

fn sig_all_value_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Option<Type<Routed>>> {
    host_sig_value_param_types(sig)
}

fn host_sig_value_param_types(sig: &crate::ast::Signature<Routed>) -> Vec<Option<Type<Routed>>> {
    value_group_param_types(sig).into_iter().flatten().collect()
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

/// The Swift function type a body closure of value-arity `arity` is cast to
/// before application: `(Any, …, Any) -> Any`.
fn swift_closure_type(arity: usize) -> String {
    let params = vec!["Any"; arity].join(", ");
    format!("({params}) -> Any")
}

fn peel_forall(ty: &Type<Routed>) -> &Type<Routed> {
    let mut t = ty;
    while let Type::Forall { body, .. } = t {
        t = body;
    }
    t
}

/// The Swift access expression for slot `i` of an `n`-slot right-nested
/// binary product held in `v_expr` (an `Any`).
fn product_param_slot_access(v_expr: &str, i: usize, n: usize) -> String {
    format!("kioProductSlot({v_expr}, {i}, {n})")
}

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

    fn assert_no_public_any(emitted: &SwiftPackage) {
        assert!(
            !emitted.host_swift.contains("Any") && !emitted.ffi_swift.contains("Any"),
            "host:\n{}\nffi:\n{}",
            emitted.host_swift,
            emitted.ffi_swift
        );
        for source in [&emitted.pkg_swift, &emitted.shapes_swift] {
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
        let rendered = swift_right_nest(&values).expect("render wide prepared product");

        assert!(rendered.starts_with("kioProductNest([v0, v1, v2, "));
        assert!(rendered.ends_with(", v252, v253, v254])"));
        assert_eq!(rendered.matches('[').count(), 1, "{rendered}");
        assert!(!rendered.contains("as [Any]"), "{rendered}");
        assert!(
            super::super::runtime::RUNTIME_SUPPORT_FILE_CONTENT
                .contains("func kioProductNest(_ slots: [Any]) -> Any")
        );
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
        let shapes = super::super::skin::PreparedSwiftShapes::new(&prepared, "FacadeHost");
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
                .contains(&format!("public struct {box_carrier}<T0>")),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(&format!(
                "@available(*, deprecated, message: \"Kio host type `api.Box` is retained only for removed host declarations from contract v2\")\npublic struct {box_carrier}<T0>"
            )),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained
                .shapes_swift
                .contains(&format!("public struct {token_carrier}<H: FacadeHost, T0>")),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(&format!(
                "@available(*, deprecated, message: \"Kio newtype `api.Token` is retained only for removed host declarations from contract v2\")\npublic struct {token_carrier}<H: FacadeHost, T0>"
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
            retained.shapes_swift.contains("Kio facade shell `"),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained.shapes_swift.contains(
                "Kio application carrier `KioApply1` is retained only for removed host declarations from contract v2"
            ),
            "{}",
            retained.shapes_swift
        );
        assert!(
            retained
                .shapes_swift
                .contains(&format!("{}public protocol KioForall_", old_attribute)),
            "{}",
            retained.shapes_swift
        );
        let shape_lines = retained.shapes_swift.lines().collect::<Vec<_>>();
        for (index, line) in shape_lines.iter().enumerate() {
            let declaration = line.trim_start();
            if declaration.starts_with("public ")
                || declaration.starts_with("case ")
                || declaration.starts_with("associatedtype ")
                || declaration.starts_with("func ")
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
                .contains(&format!("public struct {box_carrier}<T0>")),
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
            emitted
                .shapes_swift
                .contains("Kio host fn `api.old` was removed at contract v2"),
            "the site-specific retained rank-N carrier remains deprecated:\n{}",
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
            emitted.pkg_swift.contains(&format!("self.{id_method}()"))
                && emitted.pkg_swift.contains("as! (Any) -> Any"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            emitted.pkg_swift.contains("host.api__hostId(")
                && emitted.pkg_swift.contains("{ () -> Any in return"),
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
    fn public_host_method_omits_a_staged_generic_absent_from_its_signature() {
        let package = single_api_package(
            "module api; \
             host fn staged[A](value: A)[B](unit: .) -> .; \
             pub fn call() -> . { staged(., (), ., ()) }",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit staged host function");

        assert!(
            emitted
                .host_swift
                .contains("func api__staged<KioType_0>(_ arg0: KioType_0)"),
            "{}",
            emitted.host_swift
        );
        assert!(
            !emitted
                .host_swift
                .contains("api__staged<KioType_0, KioType_1>"),
            "{}",
            emitted.host_swift
        );
        assert!(
            emitted.pkg_swift.contains("self.__kio_host.api__staged(")
                && emitted.pkg_swift.contains("as! (() -> Any))()"),
            "{}",
            emitted.pkg_swift
        );
    }

    #[test]
    fn public_export_method_omits_a_staged_generic_absent_from_its_signature() {
        let package = single_api_package(
            "module api; \
             pub fn staged[A](value: A)[B](unit: .) -> . { unit }",
        );
        let emitted = lower_package(&package, "Pkg").expect("emit staged public function");

        assert!(
            emitted
                .pkg_swift
                .contains("public func staged<KioType_0>(_ arg0: KioType_0) -> KioUnit"),
            "{}",
            emitted.pkg_swift
        );
        assert!(
            !emitted
                .pkg_swift
                .contains("public func staged<KioType_0, KioType_1>"),
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
            "Generic",
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
                "public struct {}<H: PkgHost, T0>",
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
                    .contains(&format!("_ arg0: {}<Self, KioType_", carrier("Generic"))),
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
            emitted
                .pkg_swift
                .contains("{ () -> Any in return { (__kioContinuation: Any) -> Any in return")
                && emitted.pkg_swift.contains(
                    "((__kioContinuation as! (() -> Any))() as! ((Any) -> Any))(arg0.__kioValue)"
                ),
            "{}",
            emitted.pkg_swift
        );
        assert_eq!(
            emitted.pkg_swift.matches("__kioContinuation: Any").count(),
            2,
            "only the two existential projector bodies compact prepared forall stages:\n{}",
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
                    .contains("public struct KioHostType_api__Box<T0>")
                && emitted
                    .shapes_swift
                    .contains("public struct KioApply1<F, T0>"),
            "host:\n{}\nshapes:\n{}",
            emitted.host_swift,
            emitted.shapes_swift
        );
        assert!(
            emitted
                .host_swift
                .contains("_ arg0: KioHostType_api__Box<KioType_")
                && emitted
                    .host_swift
                    .contains("-> KioHostType_api__Box<KioType_"),
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
            for direction in ["lift", "project"] {
                assert!(emitted.pkg_swift.contains(&format!(
                    "KioNewtypeApplication_api__{name}_{direction}<T0, T1>"
                )));
            }
            assert!(
                emitted
                    .pkg_swift
                    .contains(&format!("KioApply2<KioNewtypeMk_api__{name}<H>, T0, T1>"))
            );
        }
        assert!(
            emitted.shapes_swift.contains(
                "public typealias KioApply2<F, T0, T1> = KioApply1<KioApply1<F, T0>, T1>"
            )
        );
        let application = emitted
            .shapes_swift
            .split("public struct KioApply1")
            .nth(1)
            .unwrap();
        let application = application.split('}').next().unwrap();
        assert!(!application.contains("public init") && !application.contains("public func value"));
        assert!(emitted.shapes_swift.contains("fileprivate init() {}"));
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
                    .contains("Implementation {\n\tassociatedtype H: PkgHost")
                && emitted.shapes_swift.contains(".Type")
                && emitted.shapes_swift.contains(" = KioType_")
                && emitted.shapes_swift.contains("public func call<KioType_"),
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

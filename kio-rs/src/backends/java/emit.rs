//! Kio' -> Java lowering: a typed facade over the serialized-IR
//! interpreter body.
//!
//! The emitted artifact is four Java source files under the package
//! namespace (`specs/backends/java.md` § Output layout): the branded
//! package handle (`<Handle>.java` — nominal namespace classes with one
//! typed method per export), the host contract (`<Handle>Host.java` — a
//! nominal interface with one method per `host fn`), the boundary shape
//! declarations (`Shapes.java`), and the interpreter
//! (`KioRuntime.java`, the verbatim `runtime.java` wrapped
//! package-private with a generated bootstrap).
//!
//! The interpreter already converts every boundary crossing to its
//! documented map shapes (`runtime.java`'s `convert`), so the facade's
//! generated conversions bridge typed Java values to those *boundary*
//! shapes — keyed maps for products / sums / newtypes, `KioFn` closures
//! for function values, passthrough scalars — never to the internal
//! nested-pair rep.

use std::collections::BTreeMap;

use crate::ast::Routed;
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryFacadeSupportOrigin,
    BoundaryHostBindingOrigin, BoundaryHostTypeBinding, BoundaryNewtypePayloadPlan,
    BoundaryNewtypeSurface, BoundaryNominalDeclaration, BoundaryNominalDependencies,
    BoundaryNominalTypeParam, CallableExecutionStage, CallableSourceParamAdapter,
    CallableValueStageLayout, FacadeBinderId, FacadeKind, FacadeShellId, FacadeUse, FacadeUseId,
    PreparedBoundaryCallableOriginRef, PreparedBoundaryCallableSite, PreparedBoundaryCallableSites,
    QualifiedTypeName, SemanticKey,
};
use crate::backends::namespace::pascal_case;
use crate::backends::public_names::{
    FacadeSelector, encode_host_identity, encode_source_identity, host_module_key, host_name_core,
};
use crate::backends::skin::{READABLE_MINT_MAX_LEN, fnv1a_64_hex};
use crate::pass::resolve::Package;

use super::skin::{
    JAVA_INSTANCE_REFERENCE_SLOT_LIMIT, JavaHostBinding, JavaShapes, java_semantic_member,
    role_to_java_type, sanitize_java_member,
};

const RUNTIME: &str = include_str!("runtime.java");

fn java_site_support_origin(site: PreparedBoundaryCallableSite<'_>) -> BoundaryFacadeSupportOrigin {
    match site.origin() {
        PreparedBoundaryCallableOriginRef::Live(_) => BoundaryFacadeSupportOrigin::Live,
        PreparedBoundaryCallableOriginRef::Retained(metadata) => {
            BoundaryFacadeSupportOrigin::Retained {
                removed_at_version: metadata.removed_at_version(),
            }
        }
    }
}

fn join_java_support_origin(
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

fn insert_java_support_origin<K: Ord>(
    origins: &mut BTreeMap<K, BoundaryFacadeSupportOrigin>,
    key: K,
    incoming: BoundaryFacadeSupportOrigin,
) {
    origins
        .entry(key)
        .and_modify(|current| *current = join_java_support_origin(*current, incoming))
        .or_insert(incoming);
}

fn push_java_deprecation(out: &mut String, origin: BoundaryFacadeSupportOrigin, indent: &str) {
    if let Some(removed_at) = origin.removed_at_version() {
        out.push_str(&format!(
            "{indent}/** @deprecated Retained only for source compatibility after removal at contract v{removed_at}. */\n{indent}@Deprecated\n"
        ));
    }
}

fn java_deprecation_annotation(origin: BoundaryFacadeSupportOrigin) -> &'static str {
    if origin.removed_at_version().is_some() {
        "@Deprecated "
    } else {
        ""
    }
}

fn java_generic_declaration(parameters: &[String]) -> String {
    if parameters.is_empty() {
        String::new()
    } else {
        format!("<{}>", parameters.join(", "))
    }
}

fn java_generic_use(parameters: &[String]) -> String {
    java_generic_declaration(parameters)
}

fn java_applied_type(base: &str, parameters: &[String]) -> String {
    format!("{base}{}", java_generic_use(parameters))
}

fn java_host_binding_parameters(binding: &JavaHostBinding) -> Vec<String> {
    (0..binding.type_arity)
        .map(|index| format!("KioHostArg_{index}"))
        .collect()
}

fn java_host_binding_public_type(binding: &JavaHostBinding, parameters: &[String]) -> String {
    if binding.retained_at.is_some() {
        return format!(
            "Shapes.{}{}",
            binding.carrier(),
            java_generic_use(parameters)
        );
    }
    binding.type_parameter().unwrap_or_else(|| {
        format!(
            "Shapes.{}{}",
            binding.carrier(),
            java_generic_use(parameters)
        )
    })
}

fn java_host_binding_body_type(binding: &JavaHostBinding) -> String {
    binding
        .role
        .map(role_to_java_type)
        .map(boxed_java_type)
        .unwrap_or_else(|| "Object".to_owned())
}

#[derive(Debug)]
pub struct EmitError {
    pub message: String,
}

impl EmitError {
    fn internal(detail: impl Into<String>) -> Self {
        Self {
            message: detail.into(),
        }
    }
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The emitted four-file Java package.
pub struct JavaPackage {
    /// The branded handle type name (`Greeter`) — PascalCase of the
    /// namespace's final segment.
    pub handle: String,
    /// `<Handle>.java` content.
    pub handle_java: String,
    /// `<Handle>Host.java` content.
    pub host_java: String,
    /// `Shapes.java` content.
    pub shapes_java: String,
    /// `KioRuntime.java` content.
    pub runtime_java: String,
}

pub fn lower_package(
    package: &Package<Routed>,
    namespace: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<JavaPackage, EmitError> {
    let handle = pascal_case(namespace.rsplit('.').next().unwrap_or(namespace));

    let ir =
        crate::backends::serialized_runtime_ir::package_ir(package).map_err(EmitError::internal)?;
    let ir_json = serde_json::to_string(&ir)
        .map_err(|e| EmitError::internal(format!("cannot encode Java IR: {e}")))?;

    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .map_err(|error| EmitError::internal(error.to_string()))?;
    let mut shapes = JavaShapes::new(package, &prepared);

    let tree = collect_export_tree(&prepared, &mut shapes)?;

    let handle_java = render_handle(namespace, &handle, &tree, &prepared, &mut shapes, &ir_json)?;
    let host_java = render_host_interface(namespace, &handle, &prepared, &mut shapes)?;
    let shapes_java = render_shapes_java(namespace, &shapes, &prepared)?;
    let runtime_java = render_runtime(namespace);

    Ok(JavaPackage {
        handle,
        handle_java,
        host_java,
        shapes_java,
        runtime_java,
    })
}

// =========================================================================
// Export tree.
// =========================================================================

#[derive(Default)]
struct ExportNode {
    children: BTreeMap<String, ExportNode>,
    fns: Vec<ExportFn>,
    newtypes: Vec<ExportNewtypeHandle>,
}

struct ExportFn {
    leaf: String,
    /// Every value parameter's boundary Java type, across all value
    /// groups in declaration order — the interpreter's exported fn takes
    /// them flat (`runtime.java`'s `exportFn`).
    param_javas: Vec<String>,
    ret_java: String,
    site: BoundaryFacadeSiteId,
    param_uses: Vec<FacadeUseId>,
    ret_use: FacadeUseId,
    method_type_parameters: Vec<String>,
    wide_param_shell: Option<FacadeShellId>,
}

struct ExportNewtypeHandle {
    type_name: String,
    ctor: Option<ExportNewtypeMember>,
    projector: Option<ExportNewtypeMember>,
    existential: bool,
}

struct ExportNewtypeMember {
    leaf: String,
    site: BoundaryFacadeSiteId,
}

struct JavaPreparedCallable {
    method_type_parameters: Vec<String>,
    param_javas: Vec<String>,
    param_uses: Vec<FacadeUseId>,
    ret_java: String,
    ret_use: FacadeUseId,
    wide_param_shell: Option<FacadeShellId>,
}

fn prepared_site<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    id: &BoundaryFacadeSiteId,
) -> Result<PreparedBoundaryCallableSite<'a>, EmitError> {
    prepared
        .site(id)
        .ok_or_else(|| EmitError::internal(format!("missing prepared Java facade site {id:?}")))
}

fn render_prepared_callable(
    site: PreparedBoundaryCallableSite<'_>,
    shapes: &JavaShapes<'_>,
) -> Result<JavaPreparedCallable, EmitError> {
    let plan = site.plan();
    let entry = plan.entry();
    let ret_use = entry.returned;
    let mut binders = BTreeMap::new();
    let mut method_type_parameters = Vec::new();
    let mut param_javas = Vec::new();
    let mut param_uses = Vec::new();
    for stage in entry.head_stages {
        match stage {
            BoundaryCallableHeadStage::Type { id, .. } => {
                let rendered = format!("KioCallType_{}", id.index());
                binders.insert(id, rendered.clone());
                method_type_parameters.push(rendered);
            }
            BoundaryCallableHeadStage::Value { slots } => {
                for slot in slots {
                    param_javas.push(render_prepared_use(
                        site,
                        plan.facade(),
                        site.nominals(),
                        *slot,
                        &binders,
                        shapes,
                    )?);
                    param_uses.push(*slot);
                }
            }
        }
    }
    let ret_java = if matches!(plan.facade().use_at(ret_use), FacadeUse::Unit { .. }) {
        "void".to_owned()
    } else {
        render_prepared_use(
            site,
            plan.facade(),
            site.nominals(),
            ret_use,
            &binders,
            shapes,
        )?
    };
    let wide_param_shell = if param_javas.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
        Some(
            plan.head_value_shell()
                .ok_or_else(|| {
                    EmitError::internal(
                        "wide Java callable has no exact prepared declaration-head shell",
                    )
                })?
                .clone(),
        )
    } else {
        None
    };
    Ok(JavaPreparedCallable {
        method_type_parameters,
        param_javas,
        param_uses,
        ret_java,
        ret_use,
        wide_param_shell,
    })
}

fn java_callable_parameter_declarations(
    parameter_types: &[String],
    wide_shell: Option<&FacadeShellId>,
    prefix: &str,
) -> Result<Vec<String>, EmitError> {
    if let Some(shell) = wide_shell {
        return Ok(vec![format!(
            "{} {prefix}0",
            java_product_shell_type(shell, parameter_types)?
        )]);
    }
    Ok(parameter_types
        .iter()
        .enumerate()
        .map(|(index, ty)| format!("{ty} {prefix}{index}"))
        .collect())
}

fn java_callable_flat_expressions(
    parameter_count: usize,
    wide_shell: Option<&FacadeShellId>,
    prefix: &str,
) -> Result<Vec<String>, EmitError> {
    if let Some(shell) = wide_shell {
        if shell.ordered_keys().len() != parameter_count {
            return Err(EmitError::internal(
                "wide Java callable shell/parameter arity drift",
            ));
        }
        return Ok(shell
            .ordered_keys()
            .iter()
            .map(|key| format!("{prefix}0.{}()", java_semantic_member(key)))
            .collect());
    }
    Ok((0..parameter_count)
        .map(|index| format!("{prefix}{index}"))
        .collect())
}

fn java_product_shell_type(
    shell: &FacadeShellId,
    parameter_types: &[String],
) -> Result<String, EmitError> {
    if shell.kind() != FacadeKind::Product || shell.ordered_keys().len() != parameter_types.len() {
        return Err(EmitError::internal(
            "prepared Java wide-call shell is not its exact product",
        ));
    }
    Ok(format!(
        "Shapes.{}{}",
        java_facade_shell_name(shell),
        java_generic_use(
            &parameter_types
                .iter()
                .map(|ty| boxed_java_type(ty))
                .collect::<Vec<_>>()
        )
    ))
}

fn render_java_product_shell_value(
    shell: &FacadeShellId,
    parameter_types: &[String],
    values: &[String],
) -> Result<String, EmitError> {
    if shell.kind() != FacadeKind::Product
        || shell.ordered_keys().len() != parameter_types.len()
        || shell.ordered_keys().len() != values.len()
    {
        return Err(EmitError::internal(
            "prepared Java product shell/value arity drift",
        ));
    }
    let name = java_facade_shell_name(shell);
    if values.len() <= JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
        return Ok(format!("new Shapes.{name}<>({})", values.join(", ")));
    }
    let chain = shell
        .ordered_keys()
        .iter()
        .zip(values)
        .map(|(key, value)| format!(".{}({value})", java_semantic_member(key)))
        .collect::<String>();
    let parameters = parameter_types
        .iter()
        .map(|ty| boxed_java_type(ty))
        .collect::<Vec<_>>();
    Ok(format!(
        "Shapes.{name}.<{}>builder(){chain}.build()",
        parameters.join(", ")
    ))
}

fn render_prepared_use(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    nominals: &BoundaryNominalDependencies,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    Ok(match plan.use_at(id) {
        FacadeUse::Unit { .. } => "Shapes.Unit".to_owned(),
        FacadeUse::Bottom { .. } => "Void".to_owned(),
        FacadeUse::Bound { binder, .. } => binders
            .get(binder)
            .cloned()
            .unwrap_or_else(|| format!("KioBoundType_{}", binder.index())),
        FacadeUse::Nominal { name, .. } => render_prepared_nominal(name, &[], nominals, shapes)?,
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            let rendered_args = args
                .iter()
                .map(|arg| render_prepared_use(site, plan, nominals, *arg, binders, shapes))
                .collect::<Result<Vec<_>, _>>()?;
            match plan.use_at(*constructor) {
                FacadeUse::Nominal { name, .. } => {
                    render_prepared_nominal(name, &rendered_args, nominals, shapes)?
                }
                FacadeUse::Bound { binder, .. } => {
                    let constructor = binders
                        .get(binder)
                        .cloned()
                        .unwrap_or_else(|| format!("KioBoundType_{}", binder.index()));
                    render_java_application(&constructor, &rendered_args)
                }
                _ => {
                    let constructor =
                        render_prepared_use(site, plan, nominals, *constructor, binders, shapes)?;
                    render_java_application(&constructor, &rendered_args)
                }
            }
        }
        FacadeUse::Product { shell, args, .. } | FacadeUse::Sum { shell, args, .. } => {
            let rendered_args = args
                .iter()
                .map(|arg| render_prepared_use(site, plan, nominals, *arg, binders, shapes))
                .map(|result| result.map(|ty| boxed_java_type(&ty)))
                .collect::<Result<Vec<_>, _>>()?;
            format!(
                "Shapes.{}{}",
                java_facade_shell_name(shell),
                java_generic_use(&rendered_args)
            )
        }
        FacadeUse::Function { slots, result, .. } => {
            let mut arguments = slots
                .iter()
                .map(|slot| render_prepared_use(site, plan, nominals, *slot, binders, shapes))
                .map(|result| result.map(|ty| boxed_java_type(&ty)))
                .collect::<Result<Vec<_>, _>>()?;
            let result = render_prepared_use(site, plan, nominals, *result, binders, shapes)?;
            if arguments.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
                let shell = plan.function_shell(id).ok_or_else(|| {
                    EmitError::internal("wide Java function has no exact prepared stage shell")
                })?;
                arguments = vec![java_product_shell_type(shell, &arguments)?];
            }
            let prefix = if result == "Shapes.Unit" {
                format!("Proc{}", arguments.len())
            } else {
                arguments.push(boxed_java_type(&result));
                format!("Fn{}", arguments.len() - 1)
            };
            format!("Shapes.{prefix}{}", java_generic_use(&arguments))
        }
        FacadeUse::Forall { .. } => render_java_forall_type(site, plan, id, binders, shapes)?,
    })
}

fn render_prepared_nominal(
    name: &QualifiedTypeName,
    args: &[String],
    nominals: &BoundaryNominalDependencies,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    let declaration = nominals.declaration(name).ok_or_else(|| {
        EmitError::internal(format!(
            "prepared Java facade is missing nominal {}.{}",
            name.module_segments().join("."),
            name.name()
        ))
    })?;
    match declaration {
        BoundaryNominalDeclaration::HostType {
            type_params,
            binding: _,
        } => {
            if args.len() > type_params.len() {
                return Err(EmitError::internal("prepared Java host-type arity drift"));
            }
            let binding = shapes.host_binding(name).ok_or_else(|| {
                EmitError::internal("prepared Java host nominal has no exact root binding")
            })?;
            if args.len() < type_params.len() {
                let constructor = render_nominal_constructor_marker_use(
                    name,
                    JavaNominalConstructorKind::HostType,
                    shapes,
                );
                return Ok(if args.is_empty() {
                    constructor
                } else {
                    render_java_application(&constructor, args)
                });
            }
            if args.is_empty() {
                return Ok(if binding.retained_at.is_some() {
                    format!("Shapes.{}", binding.carrier())
                } else {
                    binding.type_parameter().ok_or_else(|| {
                        EmitError::internal("nullary Java host nominal has no root type parameter")
                    })?
                });
            }
            Ok(format!(
                "Shapes.{}{}",
                binding.carrier(),
                java_generic_use(args)
            ))
        }
        BoundaryNominalDeclaration::Newtype { type_params, .. } => {
            if args.len() > type_params.len() {
                return Err(EmitError::internal("prepared Java newtype arity drift"));
            }
            if args.len() < type_params.len() {
                let constructor = render_nominal_constructor_marker_use(
                    name,
                    JavaNominalConstructorKind::Newtype,
                    shapes,
                );
                return Ok(if args.is_empty() {
                    constructor
                } else {
                    render_java_application(&constructor, args)
                });
            }
            let mut parameters = shapes.live_root_type_parameters().to_vec();
            parameters.extend(args.iter().cloned());
            Ok(format!(
                "Shapes.{}{}",
                java_newtype_carrier(name),
                java_generic_use(&parameters)
            ))
        }
    }
}

fn java_newtype_carrier(name: &QualifiedTypeName) -> String {
    format!(
        "KioNewtype_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum JavaNominalConstructorKind {
    HostType,
    Newtype,
}

fn java_nominal_constructor_marker(
    name: &QualifiedTypeName,
    kind: JavaNominalConstructorKind,
) -> String {
    let category = match kind {
        JavaNominalConstructorKind::HostType => "HostType",
        JavaNominalConstructorKind::Newtype => "Newtype",
    };
    format!(
        "Kio{category}Mk_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn render_nominal_constructor_marker_use(
    name: &QualifiedTypeName,
    kind: JavaNominalConstructorKind,
    shapes: &JavaShapes<'_>,
) -> String {
    let parameters = match kind {
        JavaNominalConstructorKind::HostType => Vec::new(),
        JavaNominalConstructorKind::Newtype => shapes.live_root_type_parameters().to_vec(),
    };
    format!(
        "Shapes.{}{}",
        java_nominal_constructor_marker(name, kind),
        java_generic_use(&parameters)
    )
}

fn java_newtype_conversion_helper(name: &QualifiedTypeName, direction: JavaFfiDirection) -> String {
    let direction = match direction {
        JavaFfiDirection::In => "in",
        JavaFfiDirection::Out => "out",
    };
    format!(
        "__newtype_{direction}_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn java_newtype_internal_conversion_helper(
    name: &QualifiedTypeName,
    direction: JavaFfiDirection,
) -> String {
    let direction = match direction {
        JavaFfiDirection::In => "in",
        JavaFfiDirection::Out => "out",
    };
    format!(
        "__newtype_body_{direction}_{}__{}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn render_java_application(constructor: &str, args: &[String]) -> String {
    let mut parameters = Vec::with_capacity(args.len() + 1);
    parameters.push(boxed_java_type(constructor));
    parameters.extend(args.iter().map(|arg| boxed_java_type(arg)));
    format!(
        "Shapes.Apply{}{}",
        args.len(),
        java_generic_use(&parameters)
    )
}

fn java_application_body_helper(arity: usize, direction: JavaFfiDirection) -> String {
    let direction = match direction {
        JavaFfiDirection::In => "toBody",
        JavaFfiDirection::Out => "fromBody",
    };
    format!("Shapes.__apply{arity}_{direction}")
}

fn java_boundary_site_identity(site: &BoundaryFacadeSiteId, encode: fn(&str) -> String) -> String {
    let module = encode(&site.module_segments().join("/"));
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            format!("M_{module}__H_{}", encode(name))
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            format!("M_{module}__E_{}", encode(name))
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
            format!("M_{module}__NC_{}__{}", encode(newtype), encode(member))
        }
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
            format!("M_{module}__NP_{}__{}", encode(newtype), encode(member))
        }
    }
}

fn java_forall_plan_identity(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    shapes: &JavaShapes<'_>,
    encode: fn(&str) -> String,
) -> Result<String, EmitError> {
    if std::ptr::eq(plan, site.plan().facade()) {
        return Ok(format!(
            "Site_{}",
            java_boundary_site_identity(site.site(), encode)
        ));
    }
    shapes
        .newtype_payload_plan_owner(plan)
        .map(|name| {
            format!(
                "Newtype_{}__{}",
                encode(&name.module_segments().join("/")),
                encode(name.name())
            )
        })
        .ok_or_else(|| EmitError::internal("Java forall use has no exact prepared plan owner"))
}

fn java_forall_exact_identity(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    java_forall_identity(site, plan, root, shapes, encode_source_identity)
}

fn java_forall_identity(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    shapes: &JavaShapes<'_>,
    encode: fn(&str) -> String,
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
        return Err(EmitError::internal(
            "Java forall interface identity starts at a non-forall use",
        ));
    }
    Ok(format!(
        "KioForall_{}_Binders_K{}_{}",
        java_forall_plan_identity(site, plan, shapes, encode)?,
        binders.len(),
        binders.join("_")
    ))
}

fn java_forall_interface_name(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    let readable = java_forall_identity(site, plan, root, shapes, encode_host_identity)?;
    Ok(if readable.len() <= READABLE_MINT_MAX_LEN {
        readable
    } else {
        let identity = java_forall_exact_identity(site, plan, root, shapes)?;
        format!("KioForall_H{}", fnv1a_64_hex(&identity))
    })
}

fn java_forall_free_parameters(
    binders: &BTreeMap<FacadeBinderId, String>,
    shapes: &JavaShapes<'_>,
) -> Vec<String> {
    shapes
        .live_root_type_parameters()
        .iter()
        .cloned()
        .chain(binders.values().cloned())
        .collect()
}

fn render_java_forall_type(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    Ok(format!(
        "Shapes.{}{}",
        java_forall_interface_name(site, plan, root, shapes)?,
        java_generic_use(&java_forall_free_parameters(binders, shapes))
    ))
}

struct JavaForallMethod {
    public_binders: BTreeMap<FacadeBinderId, String>,
    erased_binders: BTreeMap<FacadeBinderId, String>,
    type_parameters: Vec<String>,
    body: FacadeUseId,
}

fn prepare_java_forall_method(
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
) -> JavaForallMethod {
    let mut public_binders = binders.clone();
    let mut erased_binders = binders.clone();
    let mut type_parameters = Vec::new();
    let mut body = root;
    while let FacadeUse::Forall { binder, result, .. } = plan.use_at(body) {
        let parameter = format!("KioPolyType_{}", binder.index());
        public_binders.insert(*binder, parameter.clone());
        erased_binders.insert(*binder, "Object".to_owned());
        type_parameters.push(parameter);
        body = *result;
    }
    JavaForallMethod {
        public_binders,
        erased_binders,
        type_parameters,
        body,
    }
}

fn java_nominal_application_adapter(
    name: &QualifiedTypeName,
    kind: JavaNominalConstructorKind,
    direction: JavaFfiDirection,
) -> String {
    let category = match kind {
        JavaNominalConstructorKind::HostType => "HostType",
        JavaNominalConstructorKind::Newtype => "Newtype",
    };
    let direction = match direction {
        JavaFfiDirection::In => "lift",
        JavaFfiDirection::Out => "project",
    };
    format!(
        "Kio{category}Application_{}__{}_{direction}",
        encode_host_identity(&name.module_segments().join("/")),
        encode_host_identity(name.name())
    )
}

fn boxed_java_type(ty: &str) -> String {
    match ty {
        "boolean" => "Boolean".to_owned(),
        "byte" => "Byte".to_owned(),
        "short" => "Short".to_owned(),
        "int" => "Integer".to_owned(),
        "long" => "Long".to_owned(),
        "float" => "Float".to_owned(),
        "double" => "Double".to_owned(),
        other => other.to_owned(),
    }
}

#[derive(Clone, Copy)]
enum JavaFfiDirection {
    In,
    Out,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum JavaNominalContext {
    Standalone,
    TransparentSlot,
    DirectNewtypeMember,
}

#[derive(Clone, Copy)]
struct JavaConversionContext<'site, 'shapes, 'package> {
    site: PreparedBoundaryCallableSite<'site>,
    plan: &'site BoundaryFacadePlan,
    shapes: &'shapes JavaShapes<'package>,
    execution: Option<&'site BoundaryFacadeExecutionPlan>,
}

fn convert_prepared_use(
    site: PreparedBoundaryCallableSite<'_>,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    convert_prepared_use_in_context(
        site,
        id,
        binders,
        expression,
        direction,
        shapes,
        JavaNominalContext::Standalone,
    )
}

fn convert_prepared_use_in_context(
    site: PreparedBoundaryCallableSite<'_>,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
    shapes: &JavaShapes<'_>,
    requested_context: JavaNominalContext,
) -> Result<String, EmitError> {
    let nominal_context = if direct_newtype_member_root(site, id) {
        JavaNominalContext::DirectNewtypeMember
    } else {
        requested_context
    };
    convert_facade_use(
        JavaConversionContext {
            site,
            plan: site.plan().facade(),
            shapes,
            execution: site.execution().map(|execution| execution.root_uses()),
        },
        id,
        binders,
        expression,
        direction,
        nominal_context,
    )
}

fn direct_newtype_member_root(site: PreparedBoundaryCallableSite<'_>, id: FacadeUseId) -> bool {
    let entry = site.plan().entry();
    match site.site().owner() {
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. } => id == entry.returned,
        BoundaryFacadeSiteOwner::NewtypeProjector { .. } => entry
            .head_stages
            .iter()
            .find_map(|stage| match stage {
                BoundaryCallableHeadStage::Value { slots } => slots.first().copied(),
                BoundaryCallableHeadStage::Type { .. } => None,
            })
            .is_some_and(|newtype| id == newtype),
        BoundaryFacadeSiteOwner::HostFunction { .. }
        | BoundaryFacadeSiteOwner::ExportedFunction { .. } => false,
    }
}

fn convert_facade_use(
    context: JavaConversionContext<'_, '_, '_>,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
    nominal_context: JavaNominalContext,
) -> Result<String, EmitError> {
    let JavaConversionContext {
        site,
        plan,
        shapes,
        execution,
    } = context;
    let internal_body = execution.is_some() && !std::ptr::eq(plan, site.plan().facade());
    Ok(match plan.use_at(id) {
        FacadeUse::Unit { .. } => match direction {
            JavaFfiDirection::In => "null".to_owned(),
            JavaFfiDirection::Out => "new Shapes.Unit()".to_owned(),
        },
        FacadeUse::Bottom { .. } => match direction {
            JavaFfiDirection::In => expression.to_owned(),
            JavaFfiDirection::Out => format!("((Void) {expression})"),
        },
        FacadeUse::Bound { binder, .. } => match direction {
            JavaFfiDirection::In => expression.to_owned(),
            JavaFfiDirection::Out => format!(
                "(({}) {expression})",
                binders
                    .get(binder)
                    .cloned()
                    .unwrap_or_else(|| "Object".to_owned())
            ),
        },
        FacadeUse::Nominal { name, .. } => convert_prepared_nominal(
            context,
            name,
            &[],
            expression,
            direction,
            binders,
            nominal_context,
        )?,
        FacadeUse::Apply {
            constructor, args, ..
        } => match plan.use_at(*constructor) {
            FacadeUse::Nominal { name, .. } => convert_prepared_nominal(
                context,
                name,
                args,
                expression,
                direction,
                binders,
                nominal_context,
            )?,
            _ => format!(
                "{}({expression})",
                java_application_body_helper(args.len(), direction)
            ),
        },
        FacadeUse::Product { shell, args, .. } => {
            let members = shell
                .ordered_keys()
                .iter()
                .map(java_semantic_member)
                .collect::<Vec<_>>();
            match direction {
                JavaFfiDirection::In => {
                    let input = format!("__input_{}", id.index());
                    let values = args
                        .iter()
                        .zip(&members)
                        .map(|(arg, member)| {
                            convert_facade_use(
                                context,
                                *arg,
                                binders,
                                &format!("{input}.{member}()"),
                                direction,
                                JavaNominalContext::TransparentSlot,
                            )
                        })
                        .collect::<Result<Vec<_>, EmitError>>()?;
                    let converted = if internal_body {
                        render_internal_product(&values)?
                    } else {
                        render_product_map(shell, &values)?
                    };
                    format!("__convert({expression}, {input} -> {converted})")
                }
                JavaFfiDirection::Out => {
                    let raw = format!("__raw_{}", id.index());
                    let raw_values: Vec<String> = if internal_body {
                        render_internal_product_slots(&raw, args.len())?
                    } else {
                        shell
                            .ordered_keys()
                            .iter()
                            .map(|key| {
                                format!(
                                    "((java.util.Map<?, ?>) {raw}).get(\"{}\")",
                                    escape_java(&semantic_runtime_key(key))
                                )
                            })
                            .collect()
                    };
                    let values = args
                        .iter()
                        .zip(raw_values)
                        .map(|(arg, raw_value)| {
                            convert_facade_use(
                                context,
                                *arg,
                                binders,
                                &raw_value,
                                direction,
                                JavaNominalContext::TransparentSlot,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let rendered_args = args
                        .iter()
                        .map(|arg| {
                            render_prepared_use(site, plan, site.nominals(), *arg, binders, shapes)
                                .map(|ty| boxed_java_type(&ty))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let converted =
                        render_java_product_shell_value(shell, &rendered_args, &values)?;
                    format!("__convert({expression}, {raw} -> {converted})")
                }
            }
        }
        FacadeUse::Sum { shell, args, .. } => match direction {
            JavaFfiDirection::In => {
                let case_binder = format!("__case_{}", id.index());
                let cases = args
                    .iter()
                    .zip(shell.ordered_keys())
                    .enumerate()
                    .map(|(index, (arg, key))| {
                        let value = convert_facade_use(
                            context,
                            *arg,
                            binders,
                            &case_binder,
                            direction,
                            JavaNominalContext::TransparentSlot,
                        )?;
                        let injected = if internal_body {
                            render_internal_sum(&value, index, args.len())?
                        } else {
                            format!(
                                "KioRuntime.map(\"{}\", {value})",
                                escape_java(&semantic_runtime_key(key))
                            )
                        };
                        Ok(format!("{case_binder} -> {injected}"))
                    })
                    .collect::<Result<Vec<_>, EmitError>>()?;
                let cases = if args.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
                    let case_types = args
                        .iter()
                        .map(|arg| {
                            render_prepared_use(site, plan, site.nominals(), *arg, binders, shapes)
                                .map(|ty| {
                                    format!("Shapes.SumCase<{}, Object>", boxed_java_type(&ty))
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    vec![render_java_product_shell_value(
                        &shell.product_companion(),
                        &case_types,
                        &cases,
                    )?]
                } else {
                    cases
                };
                format!("({expression}).<Object>KioMatch({})", cases.join(", "))
            }
            JavaFfiDirection::Out => {
                let rendered =
                    render_prepared_use(site, plan, site.nominals(), id, binders, shapes)?;
                let name = java_facade_shell_name(shell);
                let rendered_args = args
                    .iter()
                    .map(|arg| {
                        render_prepared_use(site, plan, site.nominals(), *arg, binders, shapes)
                            .map(|ty| boxed_java_type(&ty))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let generic_use = java_generic_use(&rendered_args);
                let raw = format!("__raw_{}", id.index());
                if internal_body {
                    let mut cursor = raw.clone();
                    let mut alternatives = String::new();
                    for (index, (arg, key)) in args.iter().zip(shell.ordered_keys()).enumerate() {
                        let last = index + 1 == args.len();
                        let payload = if last {
                            cursor.clone()
                        } else {
                            format!("((java.util.List<?>) {cursor}).get(1)")
                        };
                        let member = java_semantic_member(key);
                        let value = convert_facade_use(
                            context,
                            *arg,
                            binders,
                            &payload,
                            direction,
                            JavaNominalContext::TransparentSlot,
                        )?;
                        let variant = format!("new Shapes.{name}_{member}{generic_use}({value})");
                        if last {
                            alternatives.push_str(&variant);
                        } else {
                            alternatives.push_str(&format!(
                                "((Number) ((java.util.List<?>) {cursor}).get(0)).intValue() == 0 ? {variant} : "
                            ));
                            cursor = payload;
                        }
                    }
                    if alternatives.is_empty() {
                        return Err(EmitError::internal(
                            "prepared Java internal sum has no alternatives",
                        ));
                    }
                    return Ok(format!(
                        "__convert({expression}, {raw} -> (({rendered}) ({alternatives})))"
                    ));
                }
                let alternative_parts = args
                    .iter()
                    .zip(shell.ordered_keys())
                    .map(|(arg, key)| {
                        let member = java_semantic_member(key);
                        Ok(format!(
                            "((java.util.Map<?, ?>) {raw}).containsKey(\"{key}\") ? new Shapes.{name}_{member}{generic_use}({value}) : ",
                            key = escape_java(&semantic_runtime_key(key)),
                            value = convert_facade_use(
                                context,
                                *arg,
                                binders,
                                &format!(
                                    "((java.util.Map<?, ?>) {raw}).get(\"{}\")",
                                    escape_java(&semantic_runtime_key(key))
                                ),
                                direction,
                                JavaNominalContext::TransparentSlot,
                            )?
                        ))
                    })
                    .collect::<Result<Vec<_>, EmitError>>()?;
                let mut alternatives = alternative_parts.concat();
                alternatives.push_str("__missingSum()");
                format!("__convert({expression}, {raw} -> (({rendered}) ({alternatives})))")
            }
        },
        FacadeUse::Function { slots, result, .. } => {
            convert_prepared_function(context, id, slots, *result, binders, expression, direction)?
        }
        FacadeUse::Forall { .. } => {
            convert_prepared_forall(context, id, binders, expression, direction, nominal_context)?
        }
    })
}

fn convert_prepared_forall(
    context: JavaConversionContext<'_, '_, '_>,
    root: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
    nominal_context: JavaNominalContext,
) -> Result<String, EmitError> {
    let JavaConversionContext {
        site,
        plan,
        shapes,
        execution,
    } = context;
    let method = prepare_java_forall_method(plan, root, binders);
    let interface = render_java_forall_type(site, plan, root, binders, shapes)?;
    let mut type_stages = Vec::new();
    if let Some(execution) = execution.filter(|_| !std::ptr::eq(plan, site.plan().facade())) {
        let mut current = root;
        while let FacadeUse::Forall { result, .. } = plan.use_at(current) {
            match execution.use_at(current) {
                BoundaryFacadeExecutionUse::InvokeForall => type_stages.push(current),
                BoundaryFacadeExecutionUse::DeclarationBinder => {}
                _ => {
                    return Err(EmitError::internal(
                        "internal Java forall has no paired type-stage action",
                    ));
                }
            }
            current = *result;
        }
    }
    match direction {
        JavaFfiDirection::In => {
            let captured = format!("__forall_value_{}", root.index());
            let source = if type_stages.is_empty() {
                expression
            } else {
                &captured
            };
            let mut converted = match plan.use_at(method.body) {
                FacadeUse::Function { slots, result, .. } => convert_prepared_function(
                    context,
                    method.body,
                    slots,
                    *result,
                    &method.erased_binders,
                    source,
                    direction,
                )?,
                _ => convert_facade_use(
                    context,
                    method.body,
                    &method.erased_binders,
                    &format!("({source}).apply()"),
                    direction,
                    nominal_context,
                )?,
            };
            if type_stages.is_empty() {
                return Ok(converted);
            }
            for stage in type_stages.iter().rev() {
                converted = format!(
                    "(KioRuntime.KioFn) __forall_args_{} -> {converted}",
                    stage.index()
                );
            }
            Ok(format!(
                "__convert({expression}, {captured} -> {converted})"
            ))
        }
        JavaFfiDirection::Out => {
            let method_prefix = if method.type_parameters.is_empty() {
                String::new()
            } else {
                format!("{} ", java_generic_declaration(&method.type_parameters))
            };
            let raw = format!("__forall_raw_{}", root.index());
            let mut entered = raw.clone();
            for _ in &type_stages {
                entered = format!("((KioRuntime.KioFn) {entered}).call()");
            }
            let (parameters, ret, body) = match plan.use_at(method.body) {
                FacadeUse::Function { slots, result, .. } => {
                    let parameter_types = slots
                        .iter()
                        .map(|slot| {
                            render_prepared_use(
                                site,
                                plan,
                                site.nominals(),
                                *slot,
                                &method.public_binders,
                                shapes,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let wide_shell = (slots.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                        .then(|| {
                            plan.function_shell(method.body).cloned().ok_or_else(|| {
                                EmitError::internal(
                                    "generic Java callable has no exact prepared stage shell",
                                )
                            })
                        })
                        .transpose()?;
                    let parameters = java_callable_parameter_declarations(
                        &parameter_types,
                        wide_shell.as_ref(),
                        "p",
                    )?;
                    let arguments = if wide_shell.is_some() {
                        vec!["p0".to_owned()]
                    } else {
                        (0..slots.len())
                            .map(|index| format!("p{index}"))
                            .collect::<Vec<_>>()
                    };
                    let function_type = render_prepared_use(
                        site,
                        plan,
                        site.nominals(),
                        method.body,
                        &method.public_binders,
                        shapes,
                    )?;
                    let function = convert_prepared_function(
                        context,
                        method.body,
                        slots,
                        *result,
                        &method.public_binders,
                        &entered,
                        direction,
                    )?;
                    let apply = format!(
                        "(({function_type}) ({function})).apply({})",
                        arguments.join(", ")
                    );
                    if matches!(plan.use_at(*result), FacadeUse::Unit { .. }) {
                        (parameters, "void".to_owned(), format!("{{ {apply}; }}"))
                    } else {
                        let ret = render_prepared_use(
                            site,
                            plan,
                            site.nominals(),
                            *result,
                            &method.public_binders,
                            shapes,
                        )?;
                        (parameters, ret, format!("{{ return {apply}; }}"))
                    }
                }
                _ => {
                    let ret = render_prepared_use(
                        site,
                        plan,
                        site.nominals(),
                        method.body,
                        &method.public_binders,
                        shapes,
                    )?;
                    let converted = convert_facade_use(
                        context,
                        method.body,
                        &method.public_binders,
                        &entered,
                        direction,
                        nominal_context,
                    )?;
                    (Vec::new(), ret, format!("{{ return {converted}; }}"))
                }
            };
            Ok(format!(
                "__convert({expression}, {raw} -> new {interface}() {{ @Override public {method_prefix}{ret} apply({}) {body} }})",
                parameters.join(", ")
            ))
        }
    }
}

fn convert_prepared_nominal(
    context: JavaConversionContext<'_, '_, '_>,
    name: &QualifiedTypeName,
    args: &[FacadeUseId],
    expression: &str,
    direction: JavaFfiDirection,
    binders: &BTreeMap<FacadeBinderId, String>,
    nominal_context: JavaNominalContext,
) -> Result<String, EmitError> {
    let JavaConversionContext {
        site,
        plan,
        shapes,
        execution,
    } = context;
    let internal_body = execution.is_some() && !std::ptr::eq(plan, site.plan().facade());
    let rendered_args = args
        .iter()
        .map(|arg| render_prepared_use(site, plan, site.nominals(), *arg, binders, shapes))
        .collect::<Result<Vec<_>, _>>()?;
    render_prepared_nominal(name, &rendered_args, site.nominals(), shapes)?;
    let declaration = site
        .nominals()
        .declaration(name)
        .expect("a prepared Java nominal conversion has one declaration");
    Ok(match declaration {
        BoundaryNominalDeclaration::HostType { binding, .. } => {
            let host_binding = shapes.host_binding(name).ok_or_else(|| {
                EmitError::internal("prepared Java host nominal has no root binding")
            })?;
            let from_body = host_binding.adapter_from_body();
            let to_body = host_binding.adapter_to_body();
            match (binding, direction) {
                (BoundaryHostTypeBinding::Role(role), JavaFfiDirection::In) => to_kio(
                    role_to_java_type(*role),
                    &format!("__host.{to_body}({expression})"),
                ),
                (BoundaryHostTypeBinding::Role(role), JavaFfiDirection::Out) => format!(
                    "__host.{from_body}({})",
                    from_kio(role_to_java_type(*role), expression)
                ),
                (BoundaryHostTypeBinding::Roleless, JavaFfiDirection::In) => {
                    format!("__host.{to_body}({expression})")
                }
                (BoundaryHostTypeBinding::Roleless, JavaFfiDirection::Out) => {
                    format!("__host.{from_body}({expression})")
                }
            }
        }
        BoundaryNominalDeclaration::Newtype {
            transparent_payload,
            ..
        } => {
            let internal_payload =
                internal_body || nominal_context == JavaNominalContext::DirectNewtypeMember;
            let transparent_slot = transparent_payload.is_some()
                && nominal_context == JavaNominalContext::TransparentSlot;
            let runtime_wrapper = !internal_payload && !transparent_slot;
            let helper = if transparent_payload.is_some() && internal_payload {
                java_newtype_internal_conversion_helper(name, direction)
            } else {
                java_newtype_conversion_helper(name, direction)
            };
            let value = match (direction, runtime_wrapper) {
                (JavaFfiDirection::In, _) => {
                    format!("{helper}(__host, {expression})")
                }
                (JavaFfiDirection::Out, false) => expression.to_owned(),
                (JavaFfiDirection::Out, true) => format!(
                    "((java.util.Map<?, ?>) {expression}).get(\"{}\")",
                    escape_java(name.name())
                ),
            };
            match direction {
                JavaFfiDirection::In if runtime_wrapper => {
                    format!("KioRuntime.map(\"{}\", {value})", escape_java(name.name()))
                }
                JavaFfiDirection::In => value,
                JavaFfiDirection::Out => format!("{helper}(__host, {value})"),
            }
        }
    })
}

fn convert_prepared_function(
    context: JavaConversionContext<'_, '_, '_>,
    id: FacadeUseId,
    slots: &[FacadeUseId],
    result: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
) -> Result<String, EmitError> {
    let JavaConversionContext {
        site,
        plan,
        shapes,
        execution,
        ..
    } = context;
    let internal_body = !std::ptr::eq(plan, site.plan().facade());
    let layout = execution
        .filter(|_| !direct_polymorphic_payload_function(context, id))
        .and_then(|execution| match execution.use_at(id) {
            BoundaryFacadeExecutionUse::Function(layout) => Some(layout),
            _ => None,
        });
    let wide_shell = if slots.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
        let semantic = plan.function_shell(id).ok_or_else(|| {
            EmitError::internal("wide Java callback has no exact prepared stage shell")
        })?;
        if let Some(layout) = layout
            && layout.facade_shell() != Some(semantic)
        {
            return Err(EmitError::internal(
                "wide Java callback semantic/execution shell drift",
            ));
        }
        Some(semantic.clone())
    } else {
        None
    };
    let parameter_prefix = format!("__p{}_", id.index());
    let parameters = if wide_shell.is_some() {
        vec![format!("{parameter_prefix}0")]
    } else {
        (0..slots.len())
            .map(|index| format!("{parameter_prefix}{index}"))
            .collect::<Vec<_>>()
    };
    let flat_parameters =
        java_callable_flat_expressions(slots.len(), wide_shell.as_ref(), &parameter_prefix)?;
    let nominal_contexts = source_nominal_contexts(layout, slots.len())?;
    match direction {
        JavaFfiDirection::Out => {
            let converted = slots
                .iter()
                .zip(&flat_parameters)
                .zip(&nominal_contexts)
                .map(|((slot, parameter), nominal_context)| {
                    convert_facade_use(
                        context,
                        *slot,
                        binders,
                        parameter,
                        JavaFfiDirection::In,
                        *nominal_context,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let raw_args =
                pack_prepared_source_arguments_in_domain(layout, &converted, internal_body)?;
            let call = format!(
                "((KioRuntime.KioFn) {expression}).call({})",
                raw_args.join(", ")
            );
            if matches!(plan.use_at(result), FacadeUse::Unit { .. }) {
                return Ok(format!("({}) -> {{ {call}; }}", parameters.join(", ")));
            }
            let returned = convert_facade_use(
                context,
                result,
                binders,
                &call,
                JavaFfiDirection::Out,
                JavaNominalContext::Standalone,
            )?;
            Ok(format!("({}) -> {returned}", parameters.join(", ")))
        }
        JavaFfiDirection::In => {
            let raw_count = layout.map_or(slots.len(), CallableValueStageLayout::body_abi_arity);
            let raw_parameter = format!("__a_{}", id.index());
            let raw_parameters = (0..raw_count)
                .map(|index| format!("{raw_parameter}[{index}]"))
                .collect::<Vec<_>>();
            let expanded = expand_prepared_source_arguments_in_domain(
                layout,
                slots.len(),
                &raw_parameters,
                internal_body,
            )?;
            let public_args = slots
                .iter()
                .zip(expanded)
                .zip(&nominal_contexts)
                .map(|((slot, raw), nominal_context)| {
                    convert_facade_use(
                        context,
                        *slot,
                        binders,
                        &raw,
                        JavaFfiDirection::Out,
                        *nominal_context,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let public_args = if let Some(shell) = wide_shell.as_ref() {
                let public_types = slots
                    .iter()
                    .map(|slot| {
                        render_prepared_use(site, plan, site.nominals(), *slot, binders, shapes)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                vec![render_java_product_shell_value(
                    shell,
                    &public_types,
                    &public_args,
                )?]
            } else {
                public_args
            };
            let apply = format!("({expression}).apply({})", public_args.join(", "));
            if matches!(plan.use_at(result), FacadeUse::Unit { .. }) {
                return Ok(format!(
                    "(KioRuntime.KioFn) {raw_parameter} -> {{ {apply}; return null; }}"
                ));
            }
            let returned = convert_facade_use(
                context,
                result,
                binders,
                &apply,
                JavaFfiDirection::In,
                JavaNominalContext::Standalone,
            )?;
            Ok(format!("(KioRuntime.KioFn) {raw_parameter} -> {returned}"))
        }
    }
}

fn direct_polymorphic_payload_function(
    context: JavaConversionContext<'_, '_, '_>,
    id: FacadeUseId,
) -> bool {
    let JavaConversionContext { site, plan, .. } = context;
    if !std::ptr::eq(plan, site.plan().facade()) {
        return false;
    }
    let entry = site.plan().entry();
    let mut payload = match site.site().owner() {
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. } => {
            let Some(payload) = entry.head_stages.iter().find_map(|stage| match stage {
                BoundaryCallableHeadStage::Value { slots } => slots.first().copied(),
                BoundaryCallableHeadStage::Type { .. } => None,
            }) else {
                return false;
            };
            payload
        }
        BoundaryFacadeSiteOwner::NewtypeProjector { .. } => {
            let mut payload = entry.returned;
            if let Some(execution) = site.execution() {
                let compactable = execution.direct_projector_compactable_foralls();
                if compactable.contains(&payload) {
                    // The CPS result stage accepts the continuation; its stage accepts the payload.
                    for _ in 0..2 {
                        while compactable.contains(&payload) {
                            let FacadeUse::Forall { result, .. } = plan.use_at(payload) else {
                                unreachable!(
                                    "prepared compactable projector uses are forall stages"
                                )
                            };
                            payload = *result;
                        }
                        let FacadeUse::Function { slots, .. } = plan.use_at(payload) else {
                            unreachable!("prepared projector CPS stages are functions")
                        };
                        let Some(slot) = slots.first() else {
                            return false;
                        };
                        payload = *slot;
                    }
                }
            }
            payload
        }
        BoundaryFacadeSiteOwner::HostFunction { .. }
        | BoundaryFacadeSiteOwner::ExportedFunction { .. } => return false,
    };
    if !matches!(plan.use_at(payload), FacadeUse::Forall { .. }) {
        return false;
    }
    while let FacadeUse::Forall { result, .. } = plan.use_at(payload) {
        payload = *result;
    }
    // convertNewtypePayload flattens only this value stage, not its nested callbacks.
    payload == id
}

fn pack_prepared_source_arguments(
    layout: Option<&CallableValueStageLayout>,
    converted: &[String],
) -> Result<Vec<String>, EmitError> {
    pack_prepared_source_arguments_in_domain(layout, converted, false)
}

fn source_nominal_contexts(
    layout: Option<&CallableValueStageLayout>,
    slot_count: usize,
) -> Result<Vec<JavaNominalContext>, EmitError> {
    let mut contexts = vec![JavaNominalContext::Standalone; slot_count];
    let Some(layout) = layout else {
        return Ok(contexts);
    };
    if layout.facade_slot_count() != slot_count {
        return Err(EmitError::internal(
            "prepared Java source context slot count drift",
        ));
    }
    for source in layout.source_params() {
        let context = match source.adapter() {
            CallableSourceParamAdapter::RightNest => JavaNominalContext::TransparentSlot,
            CallableSourceParamAdapter::UnitValue | CallableSourceParamAdapter::Identity => {
                JavaNominalContext::Standalone
            }
        };
        for index in source.facade_slots() {
            let slot = contexts
                .get_mut(index)
                .ok_or_else(|| EmitError::internal("prepared Java source context range drift"))?;
            *slot = context;
        }
    }
    Ok(contexts)
}

fn prepared_callable_nominal_contexts(
    site: PreparedBoundaryCallableSite<'_>,
) -> Result<Vec<JavaNominalContext>, EmitError> {
    let execution = site.execution().ok_or_else(|| {
        EmitError::internal("retained Java callable cannot own live parameter contexts")
    })?;
    let mut contexts = Vec::new();
    for stage in execution.head_stages() {
        let CallableExecutionStage::Value(layout) = stage else {
            continue;
        };
        contexts.extend(source_nominal_contexts(
            Some(layout),
            layout.facade_slot_count(),
        )?);
    }
    Ok(contexts)
}

fn pack_prepared_source_arguments_in_domain(
    layout: Option<&CallableValueStageLayout>,
    converted: &[String],
    internal_body: bool,
) -> Result<Vec<String>, EmitError> {
    let Some(layout) = layout else {
        return Ok(converted.to_vec());
    };
    layout
        .source_params()
        .iter()
        .map(|source| {
            let range = source.facade_slots();
            let values = converted
                .get(range)
                .ok_or_else(|| EmitError::internal("prepared Java source range drift"))?;
            Ok(match source.adapter() {
                CallableSourceParamAdapter::UnitValue => "null".to_owned(),
                CallableSourceParamAdapter::Identity => values
                    .first()
                    .cloned()
                    .ok_or_else(|| EmitError::internal("empty Java identity source range"))?,
                CallableSourceParamAdapter::RightNest => {
                    let shell = source.product_shell().ok_or_else(|| {
                        EmitError::internal("Java RightNest source has no prepared product shell")
                    })?;
                    if internal_body {
                        render_internal_product(values)?
                    } else {
                        render_product_map(shell, values)?
                    }
                }
            })
        })
        .collect()
}

fn prepared_callable_binders(
    site: PreparedBoundaryCallableSite<'_>,
) -> BTreeMap<FacadeBinderId, String> {
    site.plan()
        .entry()
        .head_stages
        .into_iter()
        .filter_map(|stage| match stage {
            BoundaryCallableHeadStage::Type { id, .. } => {
                Some((id, format!("KioCallType_{}", id.index())))
            }
            BoundaryCallableHeadStage::Value { .. } => None,
        })
        .collect()
}

fn pack_prepared_callable_arguments(
    site: PreparedBoundaryCallableSite<'_>,
    converted: &[String],
) -> Result<Vec<String>, EmitError> {
    let execution = site.execution().ok_or_else(|| {
        EmitError::internal("retained Java callable cannot own a live invocation adapter")
    })?;
    let mut cursor = 0usize;
    let mut raw = Vec::new();
    for stage in execution.head_stages() {
        let CallableExecutionStage::Value(layout) = stage else {
            continue;
        };
        let end = cursor
            .checked_add(layout.facade_slot_count())
            .ok_or_else(|| EmitError::internal("prepared Java facade slot count overflow"))?;
        let stage_values = converted
            .get(cursor..end)
            .ok_or_else(|| EmitError::internal("prepared Java callable slot range drift"))?;
        raw.extend(pack_prepared_source_arguments(Some(layout), stage_values)?);
        cursor = end;
    }
    if cursor != converted.len() {
        return Err(EmitError::internal(
            "prepared Java callable left semantic arguments unconsumed",
        ));
    }
    Ok(raw)
}

fn expand_prepared_callable_arguments(
    site: PreparedBoundaryCallableSite<'_>,
) -> Result<Vec<String>, EmitError> {
    let execution = site.execution().ok_or_else(|| {
        EmitError::internal("retained Java callable cannot own a live host adapter")
    })?;
    let mut raw_index = 0usize;
    let mut public = Vec::new();
    for stage in execution.head_stages() {
        let CallableExecutionStage::Value(layout) = stage else {
            continue;
        };
        let raw = (0..layout.body_abi_arity())
            .map(|offset| format!("__a[{}]", raw_index + offset))
            .collect::<Vec<_>>();
        public.extend(expand_prepared_source_arguments(
            Some(layout),
            layout.facade_slot_count(),
            &raw,
        )?);
        raw_index += layout.body_abi_arity();
    }
    Ok(public)
}

fn expand_prepared_source_arguments(
    layout: Option<&CallableValueStageLayout>,
    slot_count: usize,
    raw: &[String],
) -> Result<Vec<String>, EmitError> {
    expand_prepared_source_arguments_in_domain(layout, slot_count, raw, false)
}

fn expand_prepared_source_arguments_in_domain(
    layout: Option<&CallableValueStageLayout>,
    slot_count: usize,
    raw: &[String],
    internal_body: bool,
) -> Result<Vec<String>, EmitError> {
    let Some(layout) = layout else {
        return Ok(raw.to_vec());
    };
    let mut expanded = Vec::with_capacity(slot_count);
    for (source, raw) in layout.source_params().iter().zip(raw) {
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue => {}
            CallableSourceParamAdapter::Identity => expanded.push(raw.clone()),
            CallableSourceParamAdapter::RightNest => {
                let shell = source.product_shell().ok_or_else(|| {
                    EmitError::internal("Java RightNest source has no prepared product shell")
                })?;
                if internal_body {
                    expanded.extend(render_internal_product_slots(
                        raw,
                        shell.ordered_keys().len(),
                    )?);
                } else {
                    for key in shell.ordered_keys() {
                        expanded.push(format!(
                            "((java.util.Map<?, ?>) {raw}).get(\"{}\")",
                            escape_java(&semantic_runtime_key(key))
                        ));
                    }
                }
            }
        }
    }
    if expanded.len() != slot_count {
        return Err(EmitError::internal(
            "prepared Java source expansion disagrees with semantic slots",
        ));
    }
    Ok(expanded)
}

fn render_internal_product(values: &[String]) -> Result<String, EmitError> {
    let Some((last, prefix)) = values.split_last() else {
        return Err(EmitError::internal(
            "prepared Java internal product has no values",
        ));
    };
    Ok(prefix.iter().rev().fold(last.clone(), |tail, value| {
        format!("java.util.Arrays.asList({value}, {tail})")
    }))
}

fn render_internal_product_slots(raw: &str, arity: usize) -> Result<Vec<String>, EmitError> {
    if arity < 2 {
        return Err(EmitError::internal(
            "prepared Java internal RightNest has fewer than two slots",
        ));
    }
    let mut current = raw.to_owned();
    let mut slots = Vec::with_capacity(arity);
    for index in 0..arity {
        if index + 1 == arity {
            slots.push(current.clone());
        } else {
            slots.push(format!("((java.util.List<?>) {current}).get(0)"));
            current = format!("((java.util.List<?>) {current}).get(1)");
        }
    }
    Ok(slots)
}

fn render_internal_sum(
    value: &str,
    alternative: usize,
    alternatives: usize,
) -> Result<String, EmitError> {
    if alternatives == 0 || alternative >= alternatives {
        return Err(EmitError::internal(
            "prepared Java internal sum alternative is out of range",
        ));
    }
    let mut injected = value.to_owned();
    if alternative + 1 < alternatives {
        injected = format!("java.util.Arrays.asList(0, {injected})");
    }
    for _ in 0..alternative {
        injected = format!("java.util.Arrays.asList(1, {injected})");
    }
    Ok(injected)
}

fn render_product_map(shell: &FacadeShellId, values: &[String]) -> Result<String, EmitError> {
    if shell.ordered_keys().len() != values.len() {
        return Err(EmitError::internal(
            "prepared Java product shell/value arity drift",
        ));
    }
    let entries = shell
        .ordered_keys()
        .iter()
        .zip(values)
        .map(|(key, value)| format!("\"{}\", {value}", escape_java(&semantic_runtime_key(key))))
        .collect::<Vec<_>>();
    Ok(format!("KioRuntime.map({})", entries.join(", ")))
}

fn semantic_runtime_key(key: &SemanticKey) -> String {
    match key {
        SemanticKey::Bare { name } => name.clone(),
        SemanticKey::Qualified {
            module_segments,
            name,
        } => format!("{}.{}", module_segments.join("/"), name),
        SemanticKey::Positional { index } => format!("_{index}"),
    }
}

fn export_node<'t>(root: &'t mut ExportNode, module_key: &str) -> &'t mut ExportNode {
    let mut node = root;
    for seg in module_key.split('/') {
        node = node.children.entry(seg.to_owned()).or_default();
    }
    node
}

fn collect_export_tree(
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
) -> Result<ExportNode, EmitError> {
    let mut root = ExportNode::default();
    // The prepared transaction is the sole public export inventory; module
    // syntax is private body metadata once these sites have been admitted.
    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::ExportedFunction { name } = site.site().owner() else {
            continue;
        };
        let callable = render_prepared_callable(site, shapes)?;
        let module_key = site.site().module_segments().join("/");
        export_node(&mut root, &module_key).fns.push(ExportFn {
            leaf: name.clone(),
            param_javas: callable.param_javas,
            ret_java: callable.ret_java,
            site: site.site().clone(),
            param_uses: callable.param_uses,
            ret_use: callable.ret_use,
            method_type_parameters: callable.method_type_parameters,
            wide_param_shell: callable.wide_param_shell,
        });
    }
    for entry in prepared.public_newtypes() {
        let name = entry.name().clone();
        let module_segments = name.module_segments().to_vec();
        let member = |leaf: &str, constructor: bool| {
            let owner = if constructor {
                BoundaryFacadeSiteOwner::NewtypeConstructor {
                    newtype: name.name().to_owned(),
                    member: leaf.to_owned(),
                }
            } else {
                BoundaryFacadeSiteOwner::NewtypeProjector {
                    newtype: name.name().to_owned(),
                    member: leaf.to_owned(),
                }
            };
            ExportNewtypeMember {
                leaf: leaf.to_owned(),
                site: BoundaryFacadeSiteId::new(module_segments.clone(), owner)
                    .expect("a prepared Java newtype member has a valid exact site"),
            }
        };
        let (ctor, projector) = match entry.surface() {
            BoundaryNewtypeSurface::Unexposed => unreachable!(
                "the prepared public-newtype inventory excludes unexposed declarations"
            ),
            BoundaryNewtypeSurface::Opaque => (None, None),
            BoundaryNewtypeSurface::Constructor { member: leaf } => {
                (Some(member(leaf, true)), None)
            }
            BoundaryNewtypeSurface::Projector { member: leaf } => (None, Some(member(leaf, false))),
            BoundaryNewtypeSurface::Both {
                constructor,
                projector,
            } => (
                Some(member(constructor, true)),
                Some(member(projector, false)),
            ),
        };
        let module_key = name.module_segments().join("/");
        export_node(&mut root, &module_key)
            .newtypes
            .push(ExportNewtypeHandle {
                type_name: name.name().to_owned(),
                ctor,
                projector,
                existential: !entry.existential_params().is_empty(),
            });
    }
    Ok(root)
}

// =========================================================================
// Conversion expressions (boundary Java type ⇄ interpreter boundary value).
// =========================================================================

/// Interpreter boundary value → typed Java value of boundary type
/// `java_ty` (the facade's `Out` direction).
fn from_kio(java_ty: &str, expr: &str) -> String {
    match java_ty {
        "String" => format!("((String) {expr})"),
        "boolean" => format!("((Boolean) {expr})"),
        "byte" => format!("((Number) {expr}).byteValue()"),
        "short" => format!("((Number) {expr}).shortValue()"),
        "int" => format!("((Number) {expr}).intValue()"),
        "long" => format!("((Number) {expr}).longValue()"),
        "float" => format!("((Number) {expr}).floatValue()"),
        "double" => format!("((Number) {expr}).doubleValue()"),
        "java.math.BigInteger" => format!("__big({expr})"),
        other => unreachable!("prepared Java host role has unknown body type {other}"),
    }
}

/// Typed Java value of boundary type `java_ty` → interpreter boundary
/// value (the facade's `In` direction).
fn to_kio(java_ty: &str, expr: &str) -> String {
    match java_ty {
        "String" | "java.math.BigInteger" | "boolean" => expr.to_owned(),
        "byte" | "short" | "int" | "long" => format!("java.math.BigInteger.valueOf({expr})"),
        "float" | "double" => format!("Double.valueOf({expr})"),
        other => unreachable!("prepared Java host role has unknown body type {other}"),
    }
}

// =========================================================================
// `<Handle>.java`.
// =========================================================================

fn render_handle(
    namespace: &str,
    handle: &str,
    tree: &ExportNode,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
    ir_json: &str,
) -> Result<String, EmitError> {
    let root_parameters = shapes.live_root_type_parameters().to_vec();
    let root_declaration = java_generic_declaration(&root_parameters);
    let root_inference = if root_parameters.is_empty() { "" } else { "<>" };
    let handle_type = java_applied_type(handle, &root_parameters);
    let host_type = java_applied_type(&format!("{handle}Host"), &root_parameters);
    let mut out = String::new();
    out.push_str("// Generated by kio - do not edit by hand.\n");
    out.push_str(&format!("package {namespace};\n\n"));
    out.push_str(&format!(
        "/**\n * The package handle: the typed facade over the kio package\n * `{handle}` was built from. Obtain one from {{@link #create}}; exports\n * are reached through the namespace fields\n * (see `specs/backends/java.md` § Package API).\n */\n"
    ));
    out.push_str(&format!(
        "public final class {handle}{root_declaration} {{\n"
    ));
    out.push_str("  private static final String KIO_IR_JSON = buildKioIrJson();\n\n");
    out.push_str(&format!("  private final {host_type} __host;\n\n"));
    out.push_str("  private static String buildKioIrJson() {\n");
    out.push_str(&java_chunked_string_builder(ir_json));
    out.push_str("  }\n\n");

    // Namespace fields (root children).
    for seg in tree.children.keys() {
        let selector = FacadeSelector::Module(seg.clone());
        let field = java_facade_selector_name(&selector, true);
        let class = namespace_class_name(std::slice::from_ref(&selector));
        out.push_str(&format!(
            "  public final {} {field};\n",
            java_applied_type(&class, &root_parameters)
        ));
    }
    out.push('\n');

    // Constructor: navigate the interpreter's export tree once.
    out.push_str(&format!(
        "  private {handle}(KioRuntime.KioObject __exports, {host_type} __host) {{\n    this.__host = __host;\n"
    ));
    for seg in tree.children.keys() {
        let selector = FacadeSelector::Module(seg.clone());
        let field = java_facade_selector_name(&selector, true);
        let class = namespace_class_name(std::slice::from_ref(&selector));
        let runtime_key = selector.facade_name(true);
        out.push_str(&format!(
            "    this.{field} = new {class}{root_inference}((KioRuntime.KioObject) __exports.get(\"{}\"), __host);\n",
            escape_java(&runtime_key)
        ));
    }
    out.push_str("  }\n\n");

    // Factory.
    out.push_str(&format!(
        "  /** Instantiate the package against `host` (§ Loading protocol). */\n  public static {root_declaration} {handle_type} create({host_type} host) {{\n    return new {handle}{root_inference}(KioRuntime.createPackage(KIO_IR_JSON, __hostRecord(host)), host);\n  }}\n\n"
    ));
    let has_live_host_function = prepared.sites().any(|site| {
        matches!(
            site.site().owner(),
            BoundaryFacadeSiteOwner::HostFunction { .. }
        ) && matches!(site.origin(), PreparedBoundaryCallableOriginRef::Live(_))
    });
    let has_live_host_binding = shapes.has_live_host_binding();
    if !has_live_host_function && !has_live_host_binding {
        out.push_str(&format!(
            "  /** The package declares no host contract, so a host-less instantiation exists. */\n  public static {root_declaration} {handle_type} create() {{\n    return new {handle}{root_inference}(KioRuntime.createPackage(KIO_IR_JSON, new KioRuntime.KioObject()), null);\n  }}\n\n"
        ));
    }

    render_prepared_newtype_conversion_helpers(
        &mut out,
        handle,
        prepared,
        shapes,
        &root_parameters,
    )?;
    render_nominal_application_adapters(&mut out, prepared, shapes, &root_parameters);

    // Namespace classes, depth-first.
    let mut path: Vec<FacadeSelector> = Vec::new();
    for (seg, child) in &tree.children {
        path.push(FacadeSelector::Module(seg.clone()));
        render_ns_class(
            &mut out,
            child,
            &path,
            handle,
            prepared,
            shapes,
            &root_parameters,
        )?;
        path.pop();
    }

    // Host adapter.
    out.push_str(&render_host_adapter(handle, prepared, shapes)?);

    out.push_str(
        "  private static <T, R> R __convert(T value, java.util.function.Function<T, R> conversion) {\n    return conversion.apply(value);\n  }\n\n  private static java.math.BigInteger __big(Object value) {\n    return value instanceof java.math.BigInteger big\n        ? big\n        : java.math.BigInteger.valueOf(((Number) value).longValue());\n  }\n\n  private static <T> T __missingSum() {\n    throw new IllegalStateException(\"sum value has no recognized semantic key\");\n  }\n\n",
    );

    out.push_str("}\n");
    Ok(out)
}

fn render_prepared_newtype_conversion_helpers(
    out: &mut String,
    handle: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
    root_parameters: &[String],
) -> Result<(), EmitError> {
    let host_type = java_applied_type(&format!("{handle}Host"), root_parameters);
    for entry in prepared.public_newtypes() {
        let name = entry.name();
        let local_parameters = (0..entry.type_params().len())
            .map(|index| format!("KioNewtypeType_{index}"))
            .collect::<Vec<_>>();
        let mut method_parameters = root_parameters.to_vec();
        method_parameters.extend(local_parameters.iter().cloned());
        let declaration = java_generic_declaration(&method_parameters);
        let prefix = if declaration.is_empty() {
            String::new()
        } else {
            format!("{declaration} ")
        };
        let mut carrier_parameters = root_parameters.to_vec();
        carrier_parameters.extend(local_parameters.iter().cloned());
        let carrier = format!(
            "Shapes.{}{}",
            java_newtype_carrier(name),
            java_generic_use(&carrier_parameters)
        );
        let transparent = matches!(entry.surface(), BoundaryNewtypeSurface::Both { .. })
            && entry.existential_params().is_empty();
        let payload_owner = prepared_newtype_payload_owner(prepared, shapes, name)?;
        if transparent && payload_owner.is_none() {
            return Err(EmitError::internal(format!(
                "public Java newtype {}.{} has no prepared conversion payload",
                name.module_segments().join("."),
                name.name()
            )));
        }
        let has_payload_owner = payload_owner.is_some();
        let (public_payload, private_payload, internal_public_payload, internal_private_payload) =
            if let Some((payload, site, _)) = payload_owner
                && transparent
            {
                let binders = payload
                    .declaration_binders()
                    .iter()
                    .zip(&local_parameters)
                    .map(|(binder, parameter)| (*binder, parameter.clone()))
                    .collect::<BTreeMap<_, _>>();
                let execution = site
                    .execution()
                    .and_then(|execution| execution.transparent_payload(name));
                let boundary_context = JavaConversionContext {
                    site,
                    plan: payload.facade(),
                    shapes,
                    execution: None,
                };
                let internal_context = JavaConversionContext {
                    execution,
                    ..boundary_context
                };
                let internal_public_payload = convert_facade_use(
                    internal_context,
                    payload.payload_root(),
                    &binders,
                    "value",
                    JavaFfiDirection::Out,
                    JavaNominalContext::TransparentSlot,
                )?;
                let internal_private_payload = convert_facade_use(
                    internal_context,
                    payload.payload_root(),
                    &binders,
                    "value.value()",
                    JavaFfiDirection::In,
                    JavaNominalContext::TransparentSlot,
                )?;
                let public_payload = convert_facade_use(
                    boundary_context,
                    payload.payload_root(),
                    &binders,
                    "value",
                    JavaFfiDirection::Out,
                    JavaNominalContext::Standalone,
                )?;
                let private_payload = convert_facade_use(
                    boundary_context,
                    payload.payload_root(),
                    &binders,
                    "value.value()",
                    JavaFfiDirection::In,
                    JavaNominalContext::Standalone,
                )?;
                (
                    public_payload,
                    private_payload,
                    internal_public_payload,
                    internal_private_payload,
                )
            } else {
                (
                    "value".to_owned(),
                    "value.value()".to_owned(),
                    "value".to_owned(),
                    "value.value()".to_owned(),
                )
            };
        let out_helper = java_newtype_conversion_helper(name, JavaFfiDirection::Out);
        let in_helper = java_newtype_conversion_helper(name, JavaFfiDirection::In);
        out.push_str(&format!(
            "  private static {prefix}{carrier} {out_helper}({host_type} __host, Object value) {{\n    return new {carrier}({public_payload});\n  }}\n\n"
        ));
        out.push_str(&format!(
            "  private static {prefix}Object {in_helper}({host_type} __host, {carrier} value) {{\n    return {private_payload};\n  }}\n\n"
        ));
        if has_payload_owner {
            let internal_out_helper =
                java_newtype_internal_conversion_helper(name, JavaFfiDirection::Out);
            let internal_in_helper =
                java_newtype_internal_conversion_helper(name, JavaFfiDirection::In);
            out.push_str(&format!(
                "  private static {prefix}{carrier} {internal_out_helper}({host_type} __host, Object value) {{\n    return new {carrier}({internal_public_payload});\n  }}\n\n"
            ));
            out.push_str(&format!(
                "  private static {prefix}Object {internal_in_helper}({host_type} __host, {carrier} value) {{\n    return {internal_private_payload};\n  }}\n\n"
            ));
        }
    }
    Ok(())
}

fn render_nominal_application_adapters(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
    root_parameters: &[String],
) {
    for binding in shapes
        .host_bindings()
        .filter(|binding| binding.type_arity > 0 && binding.retained_at.is_none())
    {
        let parameters = java_host_binding_parameters(binding);
        let declaration = java_generic_declaration(&parameters);
        let prefix = if declaration.is_empty() {
            String::new()
        } else {
            format!("{declaration} ")
        };
        let public_type = java_host_binding_public_type(binding, &parameters);
        let marker = render_nominal_constructor_marker_use(
            &binding.name,
            JavaNominalConstructorKind::HostType,
            shapes,
        );
        let application = render_java_application(&marker, &parameters);
        let lift = java_nominal_application_adapter(
            &binding.name,
            JavaNominalConstructorKind::HostType,
            JavaFfiDirection::In,
        );
        let project = java_nominal_application_adapter(
            &binding.name,
            JavaNominalConstructorKind::HostType,
            JavaFfiDirection::Out,
        );
        let private = binding
            .role
            .map(|role| {
                to_kio(
                    role_to_java_type(role),
                    &format!("__host.{}(value)", binding.adapter_to_body()),
                )
            })
            .unwrap_or_else(|| format!("__host.{}(value)", binding.adapter_to_body()));
        let body = binding
            .role
            .map(|role| {
                from_kio(
                    role_to_java_type(role),
                    &format!(
                        "{}(value)",
                        java_application_body_helper(binding.type_arity, JavaFfiDirection::In)
                    ),
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "{}(value)",
                    java_application_body_helper(binding.type_arity, JavaFfiDirection::In)
                )
            });
        out.push_str(&format!(
            "  /** Lift the saturated `{qualified}` carrier into its exact constructor application. */\n  public {prefix}{application} {lift}({public_type} value) {{\n    return {from_body}({private});\n  }}\n\n  /** Project an exact `{qualified}` constructor application to its saturated carrier. */\n  public {prefix}{public_type} {project}({application} value) {{\n    return __host.{adapter_from_body}({body});\n  }}\n\n",
            qualified = format!(
                "{}.{}",
                binding.name.module_segments().join("/"),
                binding.name.name()
            ),
            from_body = java_application_body_helper(
                binding.type_arity,
                JavaFfiDirection::Out
            ),
            adapter_from_body = binding.adapter_from_body(),
        ));
    }

    for entry in prepared
        .public_newtypes()
        .filter(|entry| !entry.type_params().is_empty())
    {
        let name = entry.name();
        let parameters = (0..entry.type_params().len())
            .map(|index| format!("KioNewtypeType_{index}"))
            .collect::<Vec<_>>();
        let declaration = java_generic_declaration(&parameters);
        let prefix = if declaration.is_empty() {
            String::new()
        } else {
            format!("{declaration} ")
        };
        let mut carrier_parameters = root_parameters.to_vec();
        carrier_parameters.extend(parameters.iter().cloned());
        let carrier = format!(
            "Shapes.{}{}",
            java_newtype_carrier(name),
            java_generic_use(&carrier_parameters)
        );
        let marker = render_nominal_constructor_marker_use(
            name,
            JavaNominalConstructorKind::Newtype,
            shapes,
        );
        let application = render_java_application(&marker, &parameters);
        let lift = java_nominal_application_adapter(
            name,
            JavaNominalConstructorKind::Newtype,
            JavaFfiDirection::In,
        );
        let project = java_nominal_application_adapter(
            name,
            JavaNominalConstructorKind::Newtype,
            JavaFfiDirection::Out,
        );
        let conversion_helper = if shapes.newtype_payload_owner(name).is_some() {
            java_newtype_internal_conversion_helper
        } else {
            java_newtype_conversion_helper
        };
        let in_helper = conversion_helper(name, JavaFfiDirection::In);
        let out_helper = conversion_helper(name, JavaFfiDirection::Out);
        let from_body = java_application_body_helper(parameters.len(), JavaFfiDirection::Out);
        let to_body = java_application_body_helper(parameters.len(), JavaFfiDirection::In);
        let qualified = format!("{}.{}", name.module_segments().join("/"), name.name());
        out.push_str(&format!(
            "  /** Lift the saturated `{qualified}` carrier into its exact constructor application. */\n  public {prefix}{application} {lift}({carrier} value) {{\n    return {from_body}({in_helper}(__host, value));\n  }}\n\n  /** Project an exact `{qualified}` constructor application to its saturated carrier. */\n  public {prefix}{carrier} {project}({application} value) {{\n    return {out_helper}(__host, {to_body}(value));\n  }}\n\n"
        ));
    }
}

/// Deterministic Java class name for one role-bearing facade path.
fn namespace_class_name(path: &[FacadeSelector]) -> String {
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
    format!("Ns_{}", components.join("__"))
}

fn java_facade_selector_name(selector: &FacadeSelector, root: bool) -> String {
    let source = selector.facade_name(root);
    if root {
        java_public_item_name(&source)
    } else {
        source
    }
}

/// One public Kio value/module component as an injective Java identifier.
///
/// Word-cased Java-safe names are direct. A Java keyword or another
/// unspellable source component uses the disjoint exact `KioItem_` class.
fn java_public_item_name(source: &str) -> String {
    let rendered = host_name_core(source);
    let sanitized = sanitize_java_member(&rendered);
    if sanitized == rendered {
        rendered
    } else {
        format!("KioItem_{}", encode_host_identity(source))
    }
}

fn render_ns_class(
    out: &mut String,
    node: &ExportNode,
    path: &[FacadeSelector],
    handle: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
    root_parameters: &[String],
) -> Result<(), EmitError> {
    let class = namespace_class_name(path);
    let root_declaration = java_generic_declaration(root_parameters);
    let root_inference = if root_parameters.is_empty() { "" } else { "<>" };
    let host_type = java_applied_type(&format!("{handle}Host"), root_parameters);

    // Claim child and handle class names before rendering the body.
    let mut child_decls = Vec::new();
    for seg in node.children.keys() {
        let mut child_path = path.to_vec();
        let selector = FacadeSelector::Module(seg.clone());
        child_path.push(selector.clone());
        let child_class = namespace_class_name(&child_path);
        child_decls.push((seg.clone(), selector, child_class));
    }

    out.push_str(&format!(
        "  public static final class {class}{root_declaration} {{\n"
    ));
    out.push_str(&format!("    private final {host_type} __host;\n"));
    for (_seg, selector, child_class) in &child_decls {
        out.push_str(&format!(
            "    public final {} {};\n",
            java_applied_type(child_class, root_parameters),
            java_facade_selector_name(selector, false)
        ));
    }
    for nt in &node.newtypes {
        let handle_class = newtype_handle_class(path, &nt.type_name);
        let selector = FacadeSelector::Type(nt.type_name.clone());
        out.push_str(&format!(
            "    public final {} {};\n",
            java_applied_type(&handle_class, root_parameters),
            java_facade_selector_name(&selector, false)
        ));
    }
    for f in &node.fns {
        out.push_str(&format!(
            "    private final KioRuntime.KioFn __fn_{};\n",
            java_public_item_name(&f.leaf)
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "    private {class}(KioRuntime.KioObject __node, {host_type} __host) {{\n      this.__host = __host;\n"
    ));
    for (_seg, selector, child_class) in &child_decls {
        let field = java_facade_selector_name(selector, false);
        let runtime_key = selector.facade_name(false);
        out.push_str(&format!(
            "      this.{field} = new {child_class}{root_inference}((KioRuntime.KioObject) __node.get(\"{}\"), __host);\n",
            escape_java(&runtime_key)
        ));
    }
    for nt in &node.newtypes {
        let handle_class = newtype_handle_class(path, &nt.type_name);
        let selector = FacadeSelector::Type(nt.type_name.clone());
        let field = java_facade_selector_name(&selector, false);
        let runtime_key = selector.facade_name(false);
        out.push_str(&format!(
            "      this.{field} = new {handle_class}{root_inference}((KioRuntime.KioObject) __node.get(\"{}\"), __host);\n",
            escape_java(&runtime_key)
        ));
    }
    for f in &node.fns {
        let field = java_public_item_name(&f.leaf);
        out.push_str(&format!(
            "      this.__fn_{field} = (KioRuntime.KioFn) __node.get(\"{}\");\n",
            escape_java(&f.leaf)
        ));
    }
    out.push_str("    }\n");

    for f in &node.fns {
        let site = prepared_site(prepared, &f.site)?;
        let binders = prepared_callable_binders(site);
        let method = java_public_item_name(&f.leaf);
        let method_generics = java_generic_declaration(&f.method_type_parameters);
        let method_prefix = if method_generics.is_empty() {
            String::new()
        } else {
            format!("{method_generics} ")
        };
        let params =
            java_callable_parameter_declarations(&f.param_javas, f.wide_param_shell.as_ref(), "p")?;
        let flat_parameters =
            java_callable_flat_expressions(f.param_javas.len(), f.wide_param_shell.as_ref(), "p")?;
        let nominal_contexts = prepared_callable_nominal_contexts(site)?;
        let converted: Vec<String> = f
            .param_uses
            .iter()
            .zip(flat_parameters)
            .zip(nominal_contexts)
            .map(|((use_id, parameter), nominal_context)| {
                convert_prepared_use_in_context(
                    site,
                    *use_id,
                    &binders,
                    &parameter,
                    JavaFfiDirection::In,
                    shapes,
                    nominal_context,
                )
            })
            .collect::<Result<_, _>>()?;
        let args = pack_prepared_callable_arguments(site, &converted)?;
        let call = format!("__fn_{method}.call({})", args.join(", "));
        out.push('\n');
        if f.ret_java == "void" {
            out.push_str(&format!(
                "    public {method_prefix}void {method}({}) {{\n      {call};\n    }}\n",
                params.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "    public {method_prefix}{ret} {method}({}) {{\n      return {conv};\n    }}\n",
                params.join(", "),
                ret = f.ret_java,
                conv = convert_prepared_use(
                    site,
                    f.ret_use,
                    &binders,
                    &call,
                    JavaFfiDirection::Out,
                    shapes,
                )?
            ));
        }
    }
    out.push_str("  }\n\n");

    // Per-type handles expose exactly the prepared public constructor and
    // projector members; their conversions preserve the declaration-specific
    // carrier while invocation stays on the private interpreter boundary.
    for nt in &node.newtypes {
        render_newtype_handle(out, path, nt, handle, prepared, shapes, root_parameters)?;
    }

    for (seg, _selector, _) in &child_decls {
        let mut child_path = path.to_vec();
        child_path.push(FacadeSelector::Module(seg.clone()));
        render_ns_class(
            out,
            &node.children[seg],
            &child_path,
            handle,
            prepared,
            shapes,
            root_parameters,
        )?;
    }
    Ok(())
}

fn newtype_handle_class(path: &[FacadeSelector], type_name: &str) -> String {
    let mut full_path = path.to_vec();
    full_path.push(FacadeSelector::Type(type_name.to_owned()));
    namespace_class_name(&full_path).replacen("Ns_", "NtH_", 1)
}

fn render_newtype_handle(
    out: &mut String,
    path: &[FacadeSelector],
    nt: &ExportNewtypeHandle,
    handle: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
    root_parameters: &[String],
) -> Result<(), EmitError> {
    let class = newtype_handle_class(path, &nt.type_name);
    let root_declaration = java_generic_declaration(root_parameters);
    let host_type = java_applied_type(&format!("{handle}Host"), root_parameters);
    out.push_str(&format!(
        "  public static final class {class}{root_declaration} {{\n"
    ));
    out.push_str(&format!("    private final {host_type} __host;\n"));
    if nt.ctor.is_some() {
        out.push_str("    private final KioRuntime.KioFn __constructor;\n");
    }
    if nt.projector.is_some() {
        out.push_str("    private final KioRuntime.KioFn __projector;\n");
    }
    out.push_str(&format!(
        "    private {class}(KioRuntime.KioObject __node, {host_type} __host) {{\n      this.__host = __host;\n"
    ));
    if let Some(ctor) = &nt.ctor {
        out.push_str(&format!(
            "      this.__constructor = (KioRuntime.KioFn) __node.get(\"{}\");\n",
            escape_java(&ctor.leaf)
        ));
    }
    if let Some(projector) = &nt.projector {
        out.push_str(&format!(
            "      this.__projector = (KioRuntime.KioFn) __node.get(\"{}\");\n",
            escape_java(&projector.leaf)
        ));
    }
    out.push_str("    }\n");
    if let Some(ctor) = &nt.ctor {
        render_newtype_member_method(out, ctor, "__constructor", false, prepared, shapes)?;
    }
    if let Some(projector) = &nt.projector {
        render_newtype_member_method(
            out,
            projector,
            "__projector",
            nt.existential,
            prepared,
            shapes,
        )?;
    }
    out.push_str("  }\n\n");
    Ok(())
}

fn render_newtype_member_method(
    out: &mut String,
    member: &ExportNewtypeMember,
    runtime_field: &str,
    flatten_returned_stage: bool,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
) -> Result<(), EmitError> {
    let site = prepared_site(prepared, &member.site)?;
    let callable = render_prepared_callable(site, shapes)?;
    let binders = prepared_callable_binders(site);
    let method = java_public_item_name(&member.leaf);
    let mut method_type_parameters = callable.method_type_parameters.clone();
    let flattened = if flatten_returned_stage {
        let mut nested = binders.clone();
        let mut current = callable.ret_use;
        while let FacadeUse::Forall { binder, result, .. } = site.plan().facade().use_at(current) {
            let parameter = format!("KioCallType_{}", binder.index());
            nested.insert(*binder, parameter.clone());
            method_type_parameters.push(parameter);
            current = *result;
        }
        let FacadeUse::Function { slots, result, .. } = site.plan().facade().use_at(current) else {
            return Err(EmitError::internal(
                "existential Java projector has no returned continuation stage",
            ));
        };
        let continuation_types = slots
            .iter()
            .map(|slot| {
                render_prepared_use(
                    site,
                    site.plan().facade(),
                    site.nominals(),
                    *slot,
                    &nested,
                    shapes,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Some((nested, current, slots.to_vec(), *result, continuation_types))
    } else {
        None
    };
    let generics = java_generic_declaration(&method_type_parameters);
    let prefix = if generics.is_empty() {
        String::new()
    } else {
        format!("{generics} ")
    };
    let mut head_shell = callable.wide_param_shell.clone();
    let mut continuation_shell = None;
    if let Some((_, current, slots, _, _)) = &flattened {
        let mut public_count = if head_shell.is_some() {
            usize::from(!callable.param_javas.is_empty())
        } else {
            callable.param_javas.len()
        } + slots.len();
        if public_count > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT && slots.len() > 1 {
            continuation_shell = Some(
                site.plan()
                    .facade()
                    .function_shell(*current)
                    .ok_or_else(|| {
                        EmitError::internal(
                            "wide Java existential continuation has no prepared stage shell",
                        )
                    })?
                    .clone(),
            );
            public_count = public_count - slots.len() + 1;
        }
        if public_count > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT
            && head_shell.is_none()
            && callable.param_javas.len() > 1
        {
            head_shell = Some(
                site.plan()
                    .head_value_shell()
                    .ok_or_else(|| {
                        EmitError::internal(
                            "wide Java existential projector has no prepared head shell",
                        )
                    })?
                    .clone(),
            );
            public_count = public_count - callable.param_javas.len() + 1;
        }
        if public_count > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
            return Err(EmitError::internal(
                "prepared Java existential projector exceeds the JVM method-slot limit",
            ));
        }
    }
    let mut params =
        java_callable_parameter_declarations(&callable.param_javas, head_shell.as_ref(), "p")?;
    let flat_parameters =
        java_callable_flat_expressions(callable.param_javas.len(), head_shell.as_ref(), "p")?;
    let nominal_contexts = prepared_callable_nominal_contexts(site)?;
    let converted = callable
        .param_uses
        .iter()
        .zip(flat_parameters)
        .zip(nominal_contexts)
        .map(|((use_id, parameter), nominal_context)| {
            convert_prepared_use_in_context(
                site,
                *use_id,
                &binders,
                &parameter,
                JavaFfiDirection::In,
                shapes,
                nominal_context,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let raw_args = pack_prepared_callable_arguments(site, &converted)?;
    let call = format!("{runtime_field}.call({})", raw_args.join(", "));

    if let Some((nested, _current, slots, result, continuation_types)) = flattened {
        params.extend(java_callable_parameter_declarations(
            &continuation_types,
            continuation_shell.as_ref(),
            "k",
        )?);
        let returned = render_prepared_use(
            site,
            site.plan().facade(),
            site.nominals(),
            result,
            &nested,
            shapes,
        )?;
        let public_function = convert_prepared_use(
            site,
            callable.ret_use,
            &binders,
            &call,
            JavaFfiDirection::Out,
            shapes,
        )?;
        let args =
            if continuation_shell.is_some() && slots.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
                vec!["k0".to_owned()]
            } else {
                java_callable_flat_expressions(slots.len(), continuation_shell.as_ref(), "k")?
            }
            .join(", ");
        let apply = format!(
            "(({ty}) ({public_function})).apply({args})",
            ty = callable.ret_java
        );
        if matches!(site.plan().facade().use_at(result), FacadeUse::Unit { .. }) {
            out.push_str(&format!(
                "\n    public {prefix}void {method}({}) {{\n      {apply};\n    }}\n",
                params.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "\n    public {prefix}{returned} {method}({}) {{\n      return {apply};\n    }}\n",
                params.join(", ")
            ));
        }
        return Ok(());
    }

    let converted_return = convert_prepared_use(
        site,
        callable.ret_use,
        &binders,
        &call,
        JavaFfiDirection::Out,
        shapes,
    )?;
    if callable.ret_java == "void" {
        out.push_str(&format!(
            "\n    public {prefix}void {method}({}) {{\n      {call};\n    }}\n",
            params.join(", ")
        ));
    } else {
        out.push_str(&format!(
            "\n    public {prefix}{ret} {method}({}) {{\n      return {converted_return};\n    }}\n",
            params.join(", "),
            ret = callable.ret_java,
        ));
    }
    Ok(())
}

// =========================================================================
// Host interface + adapter.
// =========================================================================

/// The emitted host-interface method name for a `host fn`.
///
/// Slash-only module paths keep the established rung-2 spelling. A source
/// underscore selects an exact reserved form so path separators remain
/// recoverable.
fn host_member_name(module_path: &str, leaf: &str) -> String {
    let leaf = host_name_core(leaf);
    if !module_path.contains('_') {
        return format!("{}__{leaf}", module_path.replace('/', "_"));
    }
    format!("KioItem_{}__{leaf}", encode_host_identity(module_path))
}

fn render_host_interface(
    namespace: &str,
    handle: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
) -> Result<String, EmitError> {
    let root_parameters = shapes.live_root_type_parameters().to_vec();
    let root_declaration = java_generic_declaration(&root_parameters);
    let mut out = String::new();
    out.push_str("// Generated by kio - do not edit by hand.\n");
    out.push_str(&format!("package {namespace};\n\n"));
    out.push_str(&format!(
        "/**\n * The host record contract: one method per `host fn` the bridged\n * modules declare, module-qualified (rung 2). Implement it and pass an\n * instance to {{@link {handle}#create}}\n * (see `specs/backends/java.md` § Host record contract).\n */\n"
    ));
    out.push_str(&format!(
        "public interface {handle}Host{root_declaration} {{\n"
    ));
    for binding in shapes.host_bindings() {
        let parameters = java_host_binding_parameters(binding);
        let method_generics = java_generic_declaration(&parameters);
        let method_prefix = if method_generics.is_empty() {
            String::new()
        } else {
            format!("{method_generics} ")
        };
        let public_type = java_host_binding_public_type(binding, &parameters);
        let body_type = java_host_binding_body_type(binding);
        let from_body = binding.adapter_from_body();
        let to_body = binding.adapter_to_body();
        let qualified = format!(
            "{}.{}",
            binding.name.module_segments().join("/"),
            binding.name.name()
        );
        if let Some(removed_at) = binding.retained_at {
            out.push_str(&format!(
                "  /** Host type `{qualified}` was removed at contract v{removed_at}. */\n  @Deprecated\n  default {method_prefix}{public_type} {from_body}({body_type} value) {{\n    throw new IllegalStateException(\n        \"host type {qualified} was removed at contract v{removed_at}\");\n  }}\n\n  @Deprecated\n  default {method_prefix}{body_type} {to_body}({public_type} value) {{\n    throw new IllegalStateException(\n        \"host type {qualified} was removed at contract v{removed_at}\");\n  }}\n"
            ));
        } else {
            out.push_str(&format!(
                "  /** Bind the private body carrier of `{qualified}` to its exact host type. */\n  {method_prefix}{public_type} {from_body}({body_type} value);\n  {method_prefix}{body_type} {to_body}({public_type} value);\n"
            ));
        }
    }
    // The prepared transaction is likewise the sole public host-method
    // inventory. A second descriptor walk could diverge from retained-site
    // validation or admit a callable the facade planner did not authorize.
    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
            continue;
        };
        let module_path = site.site().module_segments().join("/");
        let callable = render_prepared_callable(site, shapes)?;
        let ret = callable.ret_java.clone();
        let method_generics = java_generic_declaration(&callable.method_type_parameters);
        let method_prefix = if method_generics.is_empty() {
            String::new()
        } else {
            format!("{method_generics} ")
        };
        let decls = java_callable_parameter_declarations(
            &callable.param_javas,
            callable.wide_param_shell.as_ref(),
            "p",
        )?;
        let member = host_member_name(&module_path, name);
        match site.origin() {
            PreparedBoundaryCallableOriginRef::Live(_) => out.push_str(&format!(
                "  {method_prefix}{ret} {member}({});\n",
                decls.join(", ")
            )),
            PreparedBoundaryCallableOriginRef::Retained(metadata) => {
                let removed_at = metadata.removed_at_version();
                out.push_str(&format!(
                    "\n  /** Removed at contract v{removed_at}; the package no longer calls it. */\n  @Deprecated\n  default {method_prefix}{ret} {member}({}) {{\n    throw new IllegalStateException(\n        \"host fn {module_path}.{name} was removed at contract v{removed_at}\");\n  }}\n",
                    decls.join(", "),
                ));
            }
        }
    }
    out.push_str("}\n");
    Ok(out)
}

fn render_host_adapter(
    handle: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
) -> Result<String, EmitError> {
    let root_parameters = shapes.live_root_type_parameters().to_vec();
    let root_declaration = java_generic_declaration(&root_parameters);
    let host_type = java_applied_type(&format!("{handle}Host"), &root_parameters);
    let mut out = String::new();
    out.push_str(&format!(
        "  private static {root_declaration} KioRuntime.KioObject __hostRecord({host_type} __host) {{\n    KioRuntime.KioObject __r = new KioRuntime.KioObject();\n"
    ));
    // Group live prepared sites by the interpreter's exact host-record
    // namespace key. Retained sites have no execution layout and therefore
    // cannot enter this private adapter record.
    let mut by_ns: BTreeMap<String, Vec<PreparedBoundaryCallableSite<'_>>> = BTreeMap::new();
    for site in prepared.sites() {
        let BoundaryFacadeSiteOwner::HostFunction { .. } = site.site().owner() else {
            continue;
        };
        if !matches!(site.origin(), PreparedBoundaryCallableOriginRef::Live(_)) {
            continue;
        }
        let module_path = site.site().module_segments().join("/");
        by_ns
            .entry(host_module_key(&module_path))
            .or_default()
            .push(site);
    }
    for (ns, fns) in &by_ns {
        out.push_str(&format!(
            "    KioRuntime.KioObject __ns_{ns} = new KioRuntime.KioObject();\n    __r.set(\"{ns}\", __ns_{ns});\n"
        ));
        for &site in fns {
            let BoundaryFacadeSiteOwner::HostFunction { name } = site.site().owner() else {
                unreachable!("the Java live host-site group contains only host functions")
            };
            let module_path = site.site().module_segments().join("/");
            let callable = render_prepared_callable(site, shapes)?;
            let ret = callable.ret_java.clone();
            let binders = prepared_callable_binders(site)
                .into_keys()
                .map(|binder| (binder, "Object".to_owned()))
                .collect::<BTreeMap<_, _>>();
            let member = host_member_name(&module_path, name);
            let boundary_args = expand_prepared_callable_arguments(site)?;
            let nominal_contexts = prepared_callable_nominal_contexts(site)?;
            let args: Vec<String> = callable
                .param_uses
                .iter()
                .zip(boundary_args)
                .zip(nominal_contexts)
                .map(|((use_id, expression), nominal_context)| {
                    convert_prepared_use_in_context(
                        site,
                        *use_id,
                        &binders,
                        &expression,
                        JavaFfiDirection::Out,
                        shapes,
                        nominal_context,
                    )
                })
                .collect::<Result<_, _>>()?;
            let args = if let Some(shell) = callable.wide_param_shell.as_ref() {
                vec![render_java_product_shell_value(
                    shell,
                    &callable.param_javas,
                    &args,
                )?]
            } else {
                args
            };
            let call = format!("__host.{member}({})", args.join(", "));
            let body = if ret == "void" {
                format!("{{ {call}; return null; }}")
            } else {
                convert_prepared_use(
                    site,
                    callable.ret_use,
                    &binders,
                    &call,
                    JavaFfiDirection::In,
                    shapes,
                )?
            };
            out.push_str(&format!(
                "    __ns_{ns}.set(\"{leaf}\", (KioRuntime.KioFn) __a -> {body});\n",
                leaf = escape_java(name),
            ));
        }
    }
    out.push_str("    return __r;\n  }\n\n");
    Ok(out)
}

// =========================================================================
// `Shapes.java` + `KioRuntime.java`.
// =========================================================================

fn render_shapes_java(
    namespace: &str,
    shapes: &JavaShapes<'_>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<String, EmitError> {
    validate_facade_shell_names(shapes)?;
    let mut out = String::new();
    out.push_str("// Generated by kio - do not edit by hand.\n");
    out.push_str(&format!("package {namespace};\n\n"));
    out.push_str(
        "/**\n * Boundary shape declarations: one nominal Java type per distinct\n * FFI shape (see `specs/backends/java.md` § FFI surface). Names derive\n * from exact semantic keys and declaration identities.\n */\n",
    );
    out.push_str("public final class Shapes {\n  private Shapes() {}\n");
    let sum_case_origin = shapes
        .facade_shell_origins()
        .filter(|(shell, _)| shell.kind() == FacadeKind::Sum)
        .map(|(_, origin)| origin)
        .reduce(join_java_support_origin);
    if let Some(origin) = sum_case_origin {
        out.push('\n');
        push_java_deprecation(&mut out, origin, "  ");
        out.push_str(&format!(
            "  @FunctionalInterface\n  public interface SumCase<T, R> {{\n    {}R apply(T value);\n  }}\n",
            java_deprecation_annotation(origin),
        ));
    }
    for (shell, origin) in shapes.facade_shell_origins() {
        render_facade_shell_java(&mut out, shell, origin);
    }
    render_generic_callable_support(&mut out, prepared, shapes);
    render_prepared_forall_interfaces(&mut out, prepared, shapes)?;
    render_nominal_constructor_markers(&mut out, prepared, shapes)?;
    render_parameterized_host_carriers(&mut out, shapes);
    render_prepared_newtype_carriers(&mut out, prepared, shapes)?;
    if let Some(origin) = java_unit_origin(prepared, shapes) {
        out.push('\n');
        push_java_deprecation(&mut out, origin, "  ");
        out.push_str("  /** The canonical unit value `()`. */\n  public record Unit() {\n");
        if origin.removed_at_version().is_some() {
            out.push_str("    @Deprecated public Unit {}\n");
        }
        out.push_str("  }\n");
    }
    out.push_str("}\n");
    Ok(out)
}

fn java_facade_shell_name(shell: &FacadeShellId) -> String {
    if shell.ordered_keys().len() != 2
        && shell
            .ordered_keys()
            .iter()
            .enumerate()
            .all(|(index, key)| {
                matches!(key, SemanticKey::Positional { index: actual } if *actual == index as u64)
            })
    {
        let kind = match shell.kind() {
            FacadeKind::Product => "Product",
            FacadeKind::Sum => "Sum",
        };
        return format!(
            "KioFacade_V1_{kind}_Positional_K{}",
            shell.ordered_keys().len()
        );
    }
    let readable = shell.encode_public();
    if readable.len() <= READABLE_MINT_MAX_LEN {
        readable
    } else {
        let kind = match shell.kind() {
            FacadeKind::Product => "Product",
            FacadeKind::Sum => "Sum",
        };
        format!("KioFacade_V1_{kind}_H{}", fnv1a_64_hex(&readable))
    }
}

fn validate_facade_shell_names(shapes: &JavaShapes<'_>) -> Result<(), EmitError> {
    let mut owners = BTreeMap::<String, &FacadeShellId>::new();
    for shell in shapes.facade_shells() {
        let name = java_facade_shell_name(shell);
        if let Some(previous) = owners.insert(name.clone(), shell)
            && previous != shell
        {
            return Err(EmitError::internal(format!(
                "distinct prepared Java facade shells collide at bounded name `{name}`"
            )));
        }
    }
    Ok(())
}

fn prepared_newtype_payload_owner<'a>(
    prepared: &'a PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
    name: &QualifiedTypeName,
) -> Result<
    Option<(
        &'a BoundaryNewtypePayloadPlan,
        PreparedBoundaryCallableSite<'a>,
        BoundaryFacadeSupportOrigin,
    )>,
    EmitError,
> {
    let Some(owner) = shapes.newtype_payload_owner(name) else {
        return Ok(None);
    };
    let site = prepared.site(&owner.site).ok_or_else(|| {
        EmitError::internal(format!(
            "public Java newtype {}.{} has a missing indexed payload site",
            name.module_segments().join("."),
            name.name()
        ))
    })?;
    let Some(BoundaryNominalDeclaration::Newtype {
        transparent_payload: Some(payload),
        ..
    }) = site.nominals().declaration(name)
    else {
        return Err(EmitError::internal(format!(
            "public Java newtype {}.{} has an indexed site without its payload",
            name.module_segments().join("."),
            name.name()
        )));
    };
    Ok(Some((payload, site, owner.origin)))
}

fn render_prepared_forall_interfaces(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    let mut interfaces = BTreeMap::<String, (String, String, BoundaryFacadeSupportOrigin)>::new();
    for site in prepared.sites() {
        let mut binders = BTreeMap::new();
        for stage in site.plan().entry().head_stages {
            match stage {
                BoundaryCallableHeadStage::Type { id, .. } => {
                    binders.insert(id, format!("KioCallType_{}", id.index()));
                }
                BoundaryCallableHeadStage::Value { slots } => {
                    for slot in slots {
                        collect_java_forall_interfaces(
                            &mut interfaces,
                            site,
                            site.plan().facade(),
                            *slot,
                            &binders,
                            java_site_support_origin(site),
                            shapes,
                        )?;
                    }
                }
            }
        }
        collect_java_forall_interfaces(
            &mut interfaces,
            site,
            site.plan().facade(),
            site.plan().entry().returned,
            &binders,
            java_site_support_origin(site),
            shapes,
        )?;
    }

    for (entry, origin) in prepared
        .public_newtypes()
        .map(|entry| (entry, BoundaryFacadeSupportOrigin::Live))
        .chain(prepared.retained_public_newtypes().map(|entry| {
            let removed_at_version = prepared
                .retained_public_newtype_removed_at(entry.name())
                .unwrap_or_else(|| {
                    unreachable!("a retained Java public newtype has removal provenance")
                });
            (
                entry,
                BoundaryFacadeSupportOrigin::Retained { removed_at_version },
            )
        }))
    {
        let Some((payload, site, _)) =
            prepared_newtype_payload_owner(prepared, shapes, entry.name())?
        else {
            continue;
        };
        let binders = payload
            .declaration_binders()
            .iter()
            .enumerate()
            .map(|(index, binder)| (*binder, format!("T{index}")))
            .collect::<BTreeMap<_, _>>();
        collect_java_forall_interfaces(
            &mut interfaces,
            site,
            payload.facade(),
            payload.payload_root(),
            &binders,
            origin,
            shapes,
        )?;
    }

    for (_, (_, declaration, origin)) in interfaces {
        out.push('\n');
        push_java_deprecation(out, origin, "  ");
        out.push_str(&declaration);
    }
    Ok(())
}

fn collect_java_forall_interfaces(
    interfaces: &mut BTreeMap<String, (String, String, BoundaryFacadeSupportOrigin)>,
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    origin: BoundaryFacadeSupportOrigin,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    match plan.use_at(id) {
        FacadeUse::Unit { .. } | FacadeUse::Bottom { .. } | FacadeUse::Bound { .. } => {}
        FacadeUse::Nominal { .. } => {}
        FacadeUse::Apply {
            constructor, args, ..
        } => {
            collect_java_forall_interfaces(
                interfaces,
                site,
                plan,
                *constructor,
                binders,
                origin,
                shapes,
            )?;
            for arg in args {
                collect_java_forall_interfaces(
                    interfaces, site, plan, *arg, binders, origin, shapes,
                )?;
            }
        }
        FacadeUse::Product { args, .. } | FacadeUse::Sum { args, .. } => {
            for arg in args {
                collect_java_forall_interfaces(
                    interfaces, site, plan, *arg, binders, origin, shapes,
                )?;
            }
        }
        FacadeUse::Function { slots, result, .. } => {
            for slot in slots {
                collect_java_forall_interfaces(
                    interfaces, site, plan, *slot, binders, origin, shapes,
                )?;
            }
            collect_java_forall_interfaces(
                interfaces, site, plan, *result, binders, origin, shapes,
            )?;
        }
        FacadeUse::Forall { .. } => {
            let name = java_forall_interface_name(site, plan, id, shapes)?;
            let identity = java_forall_exact_identity(site, plan, id, shapes)?;
            let (declaration, nested, current) =
                render_java_forall_interface(site, plan, id, binders, origin, shapes)?;
            if let Some((previous_identity, previous_declaration, previous_origin)) =
                interfaces.get_mut(&name)
            {
                if *previous_identity != identity || *previous_declaration != declaration {
                    return Err(EmitError::internal(format!(
                        "distinct prepared Java forall interfaces collide at bounded name `{name}`"
                    )));
                }
                *previous_origin = join_java_support_origin(*previous_origin, origin);
            } else {
                interfaces.insert(
                    name.clone(),
                    (identity.clone(), declaration.clone(), origin),
                );
            }
            match plan.use_at(current) {
                FacadeUse::Function { slots, result, .. } => {
                    for slot in slots {
                        collect_java_forall_interfaces(
                            interfaces, site, plan, *slot, &nested, origin, shapes,
                        )?;
                    }
                    collect_java_forall_interfaces(
                        interfaces, site, plan, *result, &nested, origin, shapes,
                    )?;
                }
                _ => collect_java_forall_interfaces(
                    interfaces, site, plan, current, &nested, origin, shapes,
                )?,
            }
        }
    }
    Ok(())
}

fn render_java_forall_interface(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    root: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    origin: BoundaryFacadeSupportOrigin,
    shapes: &JavaShapes<'_>,
) -> Result<(String, BTreeMap<FacadeBinderId, String>, FacadeUseId), EmitError> {
    let name = java_forall_interface_name(site, plan, root, shapes)?;
    let free_parameters = java_forall_free_parameters(binders, shapes);
    let method = prepare_java_forall_method(plan, root, binders);
    let (parameters, ret) = match plan.use_at(method.body) {
        FacadeUse::Function { slots, result, .. } => {
            let parameter_types = slots
                .iter()
                .map(|slot| {
                    render_prepared_use(
                        site,
                        plan,
                        site.nominals(),
                        *slot,
                        &method.public_binders,
                        shapes,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let wide_shell = (slots.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                .then(|| {
                    plan.function_shell(method.body).cloned().ok_or_else(|| {
                        EmitError::internal(
                            "generic Java callable has no exact prepared stage shell",
                        )
                    })
                })
                .transpose()?;
            let parameters =
                java_callable_parameter_declarations(&parameter_types, wide_shell.as_ref(), "p")?;
            let ret = if matches!(plan.use_at(*result), FacadeUse::Unit { .. }) {
                "void".to_owned()
            } else {
                render_prepared_use(
                    site,
                    plan,
                    site.nominals(),
                    *result,
                    &method.public_binders,
                    shapes,
                )?
            };
            (parameters, ret)
        }
        _ => (
            Vec::new(),
            render_prepared_use(
                site,
                plan,
                site.nominals(),
                method.body,
                &method.public_binders,
                shapes,
            )?,
        ),
    };
    let method_prefix = if method.type_parameters.is_empty() {
        String::new()
    } else {
        format!("{} ", java_generic_declaration(&method.type_parameters))
    };
    Ok((
        format!(
            "  @FunctionalInterface\n  public interface {name}{} {{\n    {deprecated}public {method_prefix}{ret} apply({});\n  }}\n",
            java_generic_declaration(&free_parameters),
            parameters.join(", "),
            deprecated = java_deprecation_annotation(origin),
        ),
        method.public_binders,
        method.body,
    ))
}

fn render_facade_shell_java(
    out: &mut String,
    shell: &FacadeShellId,
    origin: BoundaryFacadeSupportOrigin,
) {
    let name = java_facade_shell_name(shell);
    let arity = shell.ordered_keys().len();
    let parameters = (0..arity)
        .map(|index| format!("T{index}"))
        .collect::<Vec<_>>();
    let declaration = java_generic_declaration(&parameters);
    let use_ = java_generic_use(&parameters);
    let member_deprecated = java_deprecation_annotation(origin);
    let members = shell
        .ordered_keys()
        .iter()
        .map(java_semantic_member)
        .collect::<Vec<_>>();
    match shell.kind() {
        FacadeKind::Product if arity <= JAVA_INSTANCE_REFERENCE_SLOT_LIMIT => {
            let components = members
                .iter()
                .enumerate()
                .map(|(index, member)| format!("{member_deprecated}T{index} {member}"))
                .collect::<Vec<_>>();
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public record {name}{declaration}({}) {{\n",
                components.join(", ")
            ));
            if origin.removed_at_version().is_some() {
                out.push_str(&format!(
                    "    @Deprecated public {name}({}) {{\n      {}\n    }}\n",
                    members
                        .iter()
                        .enumerate()
                        .map(|(index, member)| format!("T{index} {member}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    members
                        .iter()
                        .map(|member| format!("this.{member} = {member};"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            out.push_str("  }\n");
        }
        FacadeKind::Product => {
            out.push('\n');
            out.push_str(
                "  /** A flat semantic product whose width exceeds the JVM constructor-slot limit. */\n",
            );
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public static final class {name}{declaration} {{\n    private final Object[] values;\n\n    private {name}(Object[] values) {{ this.values = values; }}\n"
            ));
            for (index, member) in members.iter().enumerate() {
                out.push_str(&format!(
                    "\n    @SuppressWarnings(\"unchecked\")\n    {member_deprecated}public T{index} {member}() {{ return (T{index}) values[{index}]; }}\n"
                ));
            }
            out.push_str(&format!(
                "\n    {member_deprecated}public static {declaration} Builder{use_} builder() {{ return new Builder<>(); }}\n\n"
            ));
            push_java_deprecation(out, origin, "    ");
            out.push_str(&format!(
                "    public static final class Builder{declaration} {{\n      private final Object[] values = new Object[{arity}];\n"
            ));
            for (index, member) in members.iter().enumerate() {
                out.push_str(&format!(
                    "\n      {member_deprecated}public Builder{use_} {member}(T{index} value) {{\n        values[{index}] = value;\n        return this;\n      }}\n"
                ));
            }
            out.push_str(&format!(
                "\n      {member_deprecated}public {name}{use_} build() {{ return new {name}<>(values.clone()); }}\n    }}\n  }}\n"
            ));
        }
        FacadeKind::Sum if arity <= JAVA_INSTANCE_REFERENCE_SLOT_LIMIT => {
            let permits = members
                .iter()
                .map(|member| format!("{name}_{member}"))
                .collect::<Vec<_>>();
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public sealed interface {name}{declaration} permits {} {{\n    {member_deprecated}<R> R KioMatch({});\n  }}\n",
                permits.join(", "),
                parameters
                    .iter()
                    .enumerate()
                    .map(|(index, parameter)| format!("SumCase<{parameter}, R> KioCase_{index}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for (index, member) in members.iter().enumerate() {
                let cases = parameters
                    .iter()
                    .enumerate()
                    .map(|(case_index, parameter)| {
                        format!("SumCase<{parameter}, R> KioCase_{case_index}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push('\n');
                push_java_deprecation(out, origin, "  ");
                out.push_str(&format!(
                    "  public record {name}_{member}{declaration}({member_deprecated}T{index} value) implements {name}{use_} {{\n    {member_deprecated}public {name}_{member}(T{index} value) {{ this.value = value; }}\n    @Override\n    {member_deprecated}public <R> R KioMatch({cases}) {{ return KioCase_{index}.apply(value); }}\n  }}\n"
                ));
            }
        }
        FacadeKind::Sum => {
            let permits = members
                .iter()
                .map(|member| format!("{name}_{member}"))
                .collect::<Vec<_>>();
            let case_types = parameters
                .iter()
                .map(|parameter| format!("SumCase<{parameter}, R>"))
                .collect::<Vec<_>>();
            let cases_type = format!(
                "{}{}",
                java_facade_shell_name(&shell.product_companion()),
                java_generic_use(&case_types)
            );
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public sealed interface {name}{declaration} permits {} {{\n    {member_deprecated}<R> R KioMatch({cases_type} KioCases);\n  }}\n",
                permits.join(", ")
            ));
            for (index, member) in members.iter().enumerate() {
                out.push('\n');
                push_java_deprecation(out, origin, "  ");
                out.push_str(&format!(
                    "  public record {name}_{member}{declaration}({member_deprecated}T{index} value) implements {name}{use_} {{\n    {member_deprecated}public {name}_{member}(T{index} value) {{ this.value = value; }}\n    @Override\n    {member_deprecated}public <R> R KioMatch({cases_type} KioCases) {{ return KioCases.{member}().apply(value); }}\n  }}\n"
                ));
            }
        }
    }
}

fn java_unit_origin(
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Option<BoundaryFacadeSupportOrigin> {
    let root_origin = prepared
        .sites()
        .filter_map(|site| {
            let uses_unit = site
                .plan()
                .facade()
                .uses()
                .iter()
                .any(|use_| matches!(use_, FacadeUse::Unit { .. }));
            uses_unit.then(|| java_site_support_origin(site))
        })
        .reduce(join_java_support_origin);
    shapes
        .newtype_payload_owners()
        .filter_map(|(name, owner)| {
            let site = prepared
                .site(&owner.site)
                .expect("a Java payload owner denotes one prepared site");
            let BoundaryNominalDeclaration::Newtype {
                transparent_payload: Some(payload),
                ..
            } = site
                .nominals()
                .declaration(name)
                .expect("a Java payload owner denotes one prepared newtype")
            else {
                unreachable!("a Java payload owner denotes a transparent newtype")
            };
            payload
                .facade()
                .uses()
                .iter()
                .any(|use_| matches!(use_, FacadeUse::Unit { .. }))
                .then_some(owner.origin)
        })
        .chain(root_origin)
        .reduce(join_java_support_origin)
}

fn render_generic_callable_support(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) {
    let mut functions = BTreeMap::new();
    let mut applications = BTreeMap::new();
    for site in prepared.sites() {
        let origin = java_site_support_origin(site);
        collect_generic_support_from_plan(
            site.plan().facade(),
            origin,
            &mut functions,
            &mut applications,
        );
    }
    for (name, owner) in shapes.newtype_payload_owners() {
        let site = prepared
            .site(&owner.site)
            .expect("a Java payload owner denotes one prepared site");
        let BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        } = site
            .nominals()
            .declaration(name)
            .expect("a Java payload owner denotes one prepared newtype")
        else {
            unreachable!("a Java payload owner denotes a transparent newtype")
        };
        collect_generic_support_from_plan(
            payload.facade(),
            owner.origin,
            &mut functions,
            &mut applications,
        );
    }
    for binding in prepared.host_bindings() {
        let arity = binding.type_params().len();
        if arity == 0 {
            continue;
        }
        let origin = match binding.origin() {
            BoundaryHostBindingOrigin::Live => BoundaryFacadeSupportOrigin::Live,
            BoundaryHostBindingOrigin::Retained { removed_at_version } => {
                BoundaryFacadeSupportOrigin::Retained { removed_at_version }
            }
        };
        insert_java_support_origin(&mut applications, arity, origin);
    }
    for entry in prepared.public_newtypes() {
        let arity = entry.type_params().len();
        if arity > 0 {
            insert_java_support_origin(&mut applications, arity, BoundaryFacadeSupportOrigin::Live);
        }
    }
    for entry in prepared.retained_public_newtypes() {
        let arity = entry.type_params().len();
        if arity == 0 {
            continue;
        }
        let removed_at_version = prepared
            .retained_public_newtype_removed_at(entry.name())
            .unwrap_or_else(|| {
                unreachable!("a retained Java public newtype has removal provenance")
            });
        insert_java_support_origin(
            &mut applications,
            arity,
            BoundaryFacadeSupportOrigin::Retained { removed_at_version },
        );
    }
    for ((arity, procedure), origin) in functions {
        let parameters = (0..arity)
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>();
        let args = parameters
            .iter()
            .enumerate()
            .map(|(index, ty)| format!("{ty} p{index}"))
            .collect::<Vec<_>>();
        if procedure {
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  @FunctionalInterface\n  public interface Proc{arity}{} {{\n    {deprecated}void apply({});\n  }}\n",
                java_generic_declaration(&parameters),
                args.join(", "),
                deprecated = java_deprecation_annotation(origin),
            ));
        } else {
            let mut declaration_parameters = parameters.clone();
            declaration_parameters.push("R".to_owned());
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  @FunctionalInterface\n  public interface Fn{arity}{} {{\n    {deprecated}R apply({});\n  }}\n",
                java_generic_declaration(&declaration_parameters),
                args.join(", "),
                deprecated = java_deprecation_annotation(origin),
            ));
        }
    }
    for (arity, origin) in applications {
        let mut parameters = vec!["F".to_owned()];
        parameters.extend((0..arity).map(|index| format!("T{index}")));
        let declaration = java_generic_declaration(&parameters);
        let from_body = java_application_body_helper(arity, JavaFfiDirection::Out)
            .strip_prefix("Shapes.")
            .expect("a Java application body helper is Shapes-local")
            .to_owned();
        let to_body = java_application_body_helper(arity, JavaFfiDirection::In)
            .strip_prefix("Shapes.")
            .expect("a Java application body helper is Shapes-local")
            .to_owned();
        out.push('\n');
        push_java_deprecation(out, origin, "  ");
        out.push_str(&format!(
            "  /** Typed application carrier; its erased body stays package-private. */\n  public static final class Apply{arity}{declaration} {{\n    private final Object __body;\n\n    private Apply{arity}(Object body) {{ this.__body = body; }}\n  }}\n\n  static {declaration} Apply{arity}{declaration} {from_body}(Object body) {{\n    return new Apply{arity}<>(body);\n  }}\n\n  static {declaration} Object {to_body}(Apply{arity}{declaration} value) {{\n    return value.__body;\n  }}\n"
        ));
    }
}

fn collect_generic_support_from_plan(
    plan: &BoundaryFacadePlan,
    origin: BoundaryFacadeSupportOrigin,
    functions: &mut BTreeMap<(usize, bool), BoundaryFacadeSupportOrigin>,
    applications: &mut BTreeMap<usize, BoundaryFacadeSupportOrigin>,
) {
    for use_ in plan.uses() {
        match use_ {
            FacadeUse::Function { slots, result, .. } => {
                insert_java_support_origin(
                    functions,
                    (
                        if slots.len() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT {
                            1
                        } else {
                            slots.len()
                        },
                        matches!(plan.use_at(*result), FacadeUse::Unit { .. }),
                    ),
                    origin,
                );
            }
            FacadeUse::Apply { args, .. } => {
                insert_java_support_origin(applications, args.len(), origin);
            }
            _ => {}
        }
    }
}

fn render_nominal_constructor_marker(
    out: &mut String,
    name: &QualifiedTypeName,
    parameters: &[BoundaryNominalTypeParam],
    kind: JavaNominalConstructorKind,
    marker_parameters: &[String],
    origin: BoundaryFacadeSupportOrigin,
) {
    if parameters.is_empty() {
        return;
    }
    let kinds = parameters
        .iter()
        .map(|parameter| parameter.kind().clone())
        .collect::<Vec<_>>();
    let qualified = format!("{}.{}", name.module_segments().join("/"), name.name());
    let kind_list = kinds
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    out.push('\n');
    push_java_deprecation(out, origin, "  ");
    out.push_str(&format!(
        "  /** Exact constructor marker for `{qualified}` (arity {}; kinds: {kind_list}). */\n  public interface {}{} {{}}\n",
        kinds.len(),
        java_nominal_constructor_marker(name, kind),
        java_generic_declaration(marker_parameters)
    ));
}

fn render_nominal_constructor_markers(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    for binding in prepared.host_bindings() {
        let origin = match binding.origin() {
            BoundaryHostBindingOrigin::Live => BoundaryFacadeSupportOrigin::Live,
            BoundaryHostBindingOrigin::Retained { removed_at_version } => {
                BoundaryFacadeSupportOrigin::Retained { removed_at_version }
            }
        };
        render_nominal_constructor_marker(
            out,
            binding.name(),
            binding.type_params(),
            JavaNominalConstructorKind::HostType,
            &[],
            origin,
        );
    }
    let root_parameters = shapes.live_root_type_parameters().to_vec();
    for entry in prepared.public_newtypes() {
        render_nominal_constructor_marker(
            out,
            entry.name(),
            entry.type_params(),
            JavaNominalConstructorKind::Newtype,
            &root_parameters,
            BoundaryFacadeSupportOrigin::Live,
        );
    }
    for entry in prepared.retained_public_newtypes() {
        let removed_at_version = prepared
            .retained_public_newtype_removed_at(entry.name())
            .unwrap_or_else(|| {
                unreachable!("a retained Java public newtype has removal provenance")
            });
        render_nominal_constructor_marker(
            out,
            entry.name(),
            entry.type_params(),
            JavaNominalConstructorKind::Newtype,
            &root_parameters,
            BoundaryFacadeSupportOrigin::Retained { removed_at_version },
        );
    }
    Ok(())
}

fn render_parameterized_host_carriers(out: &mut String, shapes: &JavaShapes<'_>) {
    for binding in shapes
        .host_bindings()
        .filter(|binding| binding.type_arity > 0 || binding.retained_at.is_some())
    {
        let parameters = (0..binding.type_arity)
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>();
        out.push('\n');
        let origin = binding
            .retained_at
            .map_or(BoundaryFacadeSupportOrigin::Live, |removed_at_version| {
                BoundaryFacadeSupportOrigin::Retained { removed_at_version }
            });
        push_java_deprecation(out, origin, "  ");
        out.push_str(&format!(
            "  /** Exact declaration-owned carrier for `{}.{}`. */\n  public interface {}{} {{}}\n",
            binding.name.module_segments().join("."),
            binding.name.name(),
            binding.carrier(),
            java_generic_declaration(&parameters)
        ));
    }
}

fn render_prepared_newtype_carriers(
    out: &mut String,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    for (entry, origin) in prepared
        .public_newtypes()
        .map(|entry| (entry, BoundaryFacadeSupportOrigin::Live))
        .chain(prepared.retained_public_newtypes().map(|entry| {
            let removed_at_version = prepared
                .retained_public_newtype_removed_at(entry.name())
                .unwrap_or_else(|| {
                    unreachable!("a retained Java public newtype has removal provenance")
                });
            (
                entry,
                BoundaryFacadeSupportOrigin::Retained { removed_at_version },
            )
        }))
    {
        let name = entry.name();
        let carrier = java_newtype_carrier(name);
        let root_parameters = shapes.live_root_type_parameters();
        let parameters = root_parameters
            .iter()
            .cloned()
            .chain((0..entry.type_params().len()).map(|index| format!("T{index}")))
            .collect::<Vec<_>>();
        let declaration = java_generic_declaration(&parameters);
        let transparent = matches!(entry.surface(), BoundaryNewtypeSurface::Both { .. })
            && entry.existential_params().is_empty();
        if transparent {
            let (payload, site, _) = prepared_newtype_payload_owner(prepared, shapes, name)?
                .ok_or_else(|| {
                    EmitError::internal(format!(
                        "public Java newtype {}.{} has no prepared payload",
                        name.module_segments().join("."),
                        name.name()
                    ))
                })?;
            let binders = payload
                .declaration_binders()
                .iter()
                .enumerate()
                .map(|(index, binder)| (*binder, format!("T{index}")))
                .collect::<BTreeMap<_, _>>();
            let payload_ty = render_prepared_use(
                site,
                payload.facade(),
                site.nominals(),
                payload.payload_root(),
                &binders,
                shapes,
            )?;
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public record {carrier}{declaration}({}{payload_ty} value) {{\n    {}public {carrier}({payload_ty} value) {{ this.value = value; }}\n  }}\n",
                java_deprecation_annotation(origin),
                java_deprecation_annotation(origin),
            ));
        } else {
            out.push('\n');
            push_java_deprecation(out, origin, "  ");
            out.push_str(&format!(
                "  public static final class {carrier}{declaration} {{\n    final Object value;\n    {carrier}(Object value) {{ this.value = value; }}\n    Object value() {{ return value; }}\n  }}\n"
            ));
        }
    }
    Ok(())
}

fn render_runtime(namespace: &str) -> String {
    let mut out = String::new();
    out.push_str("// Generated by kio - do not edit by hand.\n");
    out.push_str(&format!("package {namespace};\n\n"));
    out.push_str("import java.math.BigInteger;\n");
    out.push_str("import java.util.ArrayList;\n");
    out.push_str("import java.util.Arrays;\n");
    out.push_str("import java.util.Collections;\n");
    out.push_str("import java.util.HashMap;\n");
    out.push_str("import java.util.LinkedHashMap;\n");
    out.push_str("import java.util.LinkedHashSet;\n");
    out.push_str("import java.util.List;\n");
    out.push_str("import java.util.Map;\n");
    out.push_str("import java.util.Set;\n\n");
    out.push_str(
        "/** The serialized-IR interpreter body — internal to the package's\n * emit, not part of the host contract. */\n",
    );
    out.push_str("final class KioRuntime {\n");
    out.push_str("  private KioRuntime() {}\n\n");
    out.push_str(
        "  static KioObject createPackage(String irJson, Object host) {\n    return new CompiledPackage(Json.parseObject(irJson)).create(host);\n  }\n\n",
    );
    out.push_str(RUNTIME);
    out.push_str("\n}\n");
    out
}

fn escape_java(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::pass::parser::{parse, parse_package_file, parse_signature_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::structural_recovery;
    use std::path::{Path, PathBuf};

    #[test]
    fn facade_and_host_names_preserve_semantic_components() {
        assert_eq!(host_member_name("foo/bar", "read"), "foo_bar__read");
        assert_eq!(host_member_name("foo_bar", "read"), "KioItem_fooBar__read");
        assert_eq!(
            java_facade_selector_name(&FacadeSelector::Module("child".to_owned()), false),
            "KioModule_child"
        );
        assert_eq!(
            java_facade_selector_name(&FacadeSelector::Type("Child".to_owned()), false),
            "KioType_Child"
        );

        let long_shell = FacadeShellId::new(
            FacadeKind::Product,
            vec![SemanticKey::Bare {
                name: "A".repeat(80),
            }],
        );
        let exact_identity = long_shell.encode_public();
        let bounded = java_facade_shell_name(&long_shell);
        assert_eq!(
            bounded,
            format!("KioFacade_V1_Product_H{}", fnv1a_64_hex(&exact_identity))
        );
        assert!(bounded.len() <= READABLE_MINT_MAX_LEN);
    }

    #[test]
    fn nested_sum_input_uses_distinct_reserved_case_binders() {
        let package = build_package(
            &["module api; \
               host type Str role(str); \
               pub newtype Red : . { pub constructor red; pub projector un_red; }; \
               pub newtype Black : . { pub constructor black; pub projector un_black; }; \
               pub newtype Nested : . | ((Red | Black) & Str) { \
                 pub constructor nest; pub projector un_nest; \
               };"],
            "bridge { api; }",
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare nested public-sum facade");
        let site = prepared
            .sites()
            .find(|site| {
                matches!(
                    site.site().owner(),
                    BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
                        if newtype == "Nested" && member == "nest"
                )
            })
            .expect("nested public-sum constructor site");
        let entry = site.plan().entry();
        let [BoundaryCallableHeadStage::Value { slots }] = entry.head_stages.as_slice() else {
            panic!("nested public-sum constructor has one value stage")
        };
        let [outer] = *slots else {
            panic!("nested public-sum constructor has one payload slot")
        };
        let outer = *outer;
        let FacadeUse::Sum {
            args: outer_args, ..
        } = site.plan().facade().use_at(outer)
        else {
            panic!("constructor payload is the outer sum")
        };
        let FacadeUse::Product {
            args: product_args, ..
        } = site.plan().facade().use_at(outer_args[1])
        else {
            panic!("outer sum's right arm is the carrier product")
        };
        let inner = product_args[0];
        assert!(matches!(
            site.plan().facade().use_at(inner),
            FacadeUse::Sum { .. }
        ));

        let shapes = JavaShapes::new(&package, &prepared);
        let rendered = convert_prepared_use(
            site,
            outer,
            &BTreeMap::new(),
            "value",
            JavaFfiDirection::In,
            &shapes,
        )
        .expect("render nested public-sum input conversion");
        let outer_binder = format!("__case_{}", outer.index());
        let inner_binder = format!("__case_{}", inner.index());
        assert_ne!(outer_binder, inner_binder);
        assert_eq!(
            rendered.matches(&format!("{outer_binder} ->")).count(),
            2,
            "{rendered}"
        );
        assert_eq!(
            rendered.matches(&format!("{inner_binder} ->")).count(),
            2,
            "{rendered}"
        );
        assert!(
            rendered
                .replace(&format!("{outer_binder} ->"), "")
                .contains(&outer_binder),
            "outer declaration and recursive use diverged: {rendered}"
        );
        assert!(
            rendered
                .replace(&format!("{inner_binder} ->"), "")
                .contains(&inner_binder),
            "inner declaration and recursive use diverged: {rendered}"
        );
        assert_eq!(rendered.matches(".<Object>KioMatch(").count(), 2);
        assert!(!rendered.contains("__case ->"), "{rendered}");
    }

    fn build_package(srcs: &[&str], package_file_src: &str) -> Package<Routed> {
        use crate::pass::full::FullPipeline;
        use crate::pass::typecheck_full::check_package;
        use crate::pipeline::Pipeline;

        let parsed = srcs
            .iter()
            .map(|src| {
                let parsed = parse(src).expect("parse");
                let module = parsed
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.name.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                (PathBuf::from(format!("{module}.kio")), parsed)
            })
            .collect();
        let parsed_package_file =
            parse_package_file(&format!("package pkg;\n{package_file_src}"), None)
                .expect("parse package file");
        let (lowered_modules, lowered_package_file) =
            FullPipeline::lower_package(parsed, Some(parsed_package_file)).expect("lower_package");
        let package_file_entry = lowered_package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package =
            Package::build(Path::new(""), lowered_modules, package_file_entry).expect("build");
        package.resolve_imports().expect("resolve_imports");
        package.check_in_body_resolution().expect("body resolution");
        let prime = check_package(&package).expect("typecheck");
        let enriched = structural_recovery::recover_package(&prime);
        crate::pass::recover_to_low::lower(&enriched)
    }

    fn exact_newtype_carrier(module: &str, name: &str) -> String {
        java_newtype_carrier(
            &QualifiedTypeName::new(
                module.split('/').map(str::to_owned).collect(),
                name.to_owned(),
            )
            .expect("test newtype has a qualified identity"),
        )
    }

    fn compound_input_conversion(return_type: &str, expression: &str) -> String {
        let source = format!(
            "module api; host type Text role(str); \
             pub newtype Inner : Text & Text {{ \
               pub constructor inner; pub projector un_inner; \
             }}; host fn receive() -> {return_type};"
        );
        let package = build_package(&[&source], "bridge { api; }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare host-return facade");
        let site = prepared
            .sites()
            .find(|site| {
                matches!(site.site().owner(),
                    BoundaryFacadeSiteOwner::HostFunction { name } if name == "receive")
            })
            .expect("host-return site");
        let shapes = JavaShapes::new(&package, &prepared);
        convert_prepared_use(
            site,
            site.plan().entry().returned,
            &BTreeMap::new(),
            expression,
            JavaFfiDirection::In,
            &shapes,
        )
        .expect("convert host-return facade")
    }

    #[test]
    fn compound_input_binds_direct_host_product_producer_once() {
        let source = "__host.receive()";
        let rendered = compound_input_conversion("Text & Text", source);
        assert_eq!(rendered.matches(source).count(), 1, "{rendered}");
    }

    #[test]
    fn compound_input_binds_callback_product_producer_once() {
        let rendered = compound_input_conversion(". -> Text & Text", "callback");
        assert_eq!(rendered.matches(".apply(").count(), 1, "{rendered}");
    }

    #[test]
    fn compound_input_nested_product_preserves_one_source() {
        let source = "__host.receive()";
        let rendered = compound_input_conversion("Text & Inner", source);
        assert_eq!(rendered.matches(source).count(), 1, "{rendered}");
    }

    #[test]
    fn compound_input_atomic_preserves_existing_adapter() {
        let source = "__host.receive()";
        let rendered = compound_input_conversion("Text", source);
        assert_eq!(rendered.matches(source).count(), 1, "{rendered}");
        assert!(!rendered.contains("__convert("), "{rendered}");
    }

    fn polymorphic_payload_conversion(
        payload_type: &str,
        direction: JavaFfiDirection,
        internal: bool,
    ) -> String {
        let source = format!(
            "module api; pub newtype Dictionary[*F] : {payload_type} {{ \
             pub constructor make; pub projector read; }};"
        );
        let package = build_package(&[&source], "bridge { api; }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare polymorphic payload");
        let shapes = JavaShapes::new(&package, &prepared);
        let name = QualifiedTypeName::new(vec!["api".to_owned()], "Dictionary".to_owned())
            .expect("qualified newtype");
        let (payload, site, _) = prepared_newtype_payload_owner(&prepared, &shapes, &name)
            .expect("resolve payload owner")
            .expect("public payload owner");
        let binders = payload
            .declaration_binders()
            .iter()
            .map(|binder| (*binder, format!("KioType_{}", binder.index())))
            .collect();
        let execution = internal.then(|| {
            site.execution()
                .and_then(|execution| execution.transparent_payload(&name))
                .expect("live payload execution")
        });
        convert_facade_use(
            JavaConversionContext {
                site,
                plan: payload.facade(),
                shapes: &shapes,
                execution,
            },
            payload.payload_root(),
            &binders,
            "produce()",
            direction,
            JavaNominalContext::TransparentSlot,
        )
        .expect("convert polymorphic payload")
    }

    #[test]
    fn internal_forall_input_retains_payload_stages_not_declaration_binders() {
        let rendered = polymorphic_payload_conversion(
            "[A][B] ((A -> B) & F(A)) -> F(B)",
            JavaFfiDirection::In,
            true,
        );
        assert_eq!(
            rendered
                .matches("(KioRuntime.KioFn) __forall_args_")
                .count(),
            2,
            "{rendered}"
        );
        assert!(rendered.starts_with("__convert(produce(), "), "{rendered}");
        assert_eq!(rendered.matches("produce()").count(), 1, "{rendered}");
    }

    #[test]
    fn internal_forall_output_enters_stages_inside_public_apply() {
        let rendered = polymorphic_payload_conversion(
            "[A][B] ((A -> B) & F(A)) -> F(B)",
            JavaFfiDirection::Out,
            true,
        );
        let (capture, apply) = rendered
            .split_once(" apply(")
            .expect("public forall method");
        assert!(!capture.contains(").call()"), "{rendered}");
        assert_eq!(apply.matches(").call()").count(), 2, "{rendered}");
        assert_eq!(rendered.matches("produce()").count(), 1, "{rendered}");
    }

    #[test]
    fn internal_forall_nested_after_value_stage_keeps_its_stage() {
        let payload = "[A] (A -> A) -> [B] B -> B";
        let input = polymorphic_payload_conversion(payload, JavaFfiDirection::In, true);
        let output = polymorphic_payload_conversion(payload, JavaFfiDirection::Out, true);
        assert_eq!(
            input.matches("(KioRuntime.KioFn) __forall_args_").count(),
            2,
            "{input}"
        );
        assert_eq!(output.matches(").call()").count(), 2, "{output}");
        assert_eq!(input.matches("produce()").count(), 1, "{input}");
        assert_eq!(output.matches("produce()").count(), 1, "{output}");
    }

    #[test]
    fn boundary_forall_conversion_does_not_repeat_runtime_type_stages() {
        for direction in [JavaFfiDirection::In, JavaFfiDirection::Out] {
            let rendered = polymorphic_payload_conversion(
                "[A][B] ((A -> B) & F(A)) -> F(B)",
                direction,
                false,
            );
            assert!(!rendered.contains("__forall_args_"), "{rendered}");
            assert!(!rendered.contains(").call()"), "{rendered}");
        }
    }

    fn direct_payload_member_conversions(existentials: &str, payload: &str) -> Vec<String> {
        let source = format!(
            "module api; pub newtype Dictionary[*F] {existentials} : {payload} {{ \
             pub constructor make; pub projector read; }};"
        );
        let package = build_package(&[&source], "bridge { api; }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare newtype member facades");
        let shapes = JavaShapes::new(&package, &prepared);
        let mut conversions = Vec::new();
        for site in prepared.sites() {
            let entry = site.plan().entry();
            let (payload, direction) = match site.site().owner() {
                BoundaryFacadeSiteOwner::NewtypeConstructor { .. } => (
                    entry
                        .head_stages
                        .iter()
                        .find_map(|stage| match stage {
                            BoundaryCallableHeadStage::Value { slots } => slots.first().copied(),
                            _ => None,
                        })
                        .expect("constructor payload"),
                    JavaFfiDirection::In,
                ),
                BoundaryFacadeSiteOwner::NewtypeProjector { .. } => {
                    (entry.returned, JavaFfiDirection::Out)
                }
                _ => continue,
            };
            let rendered = convert_prepared_use(
                site,
                payload,
                &prepared_callable_binders(site),
                "value",
                direction,
                &shapes,
            )
            .expect("convert member payload");
            conversions.push(rendered);
        }
        assert_eq!(conversions.len(), 2, "both members are exercised");
        conversions
    }

    #[test]
    fn direct_polymorphic_newtype_members_use_flat_runtime_payload_slots() {
        for rendered in direct_payload_member_conversions("", "[A][B] ((A -> B) & F(A)) -> F(B)") {
            assert!(!rendered.contains("KioRuntime.map("), "{rendered}");
            assert!(!rendered.contains("java.util.Map"), "{rendered}");
        }
    }

    #[test]
    fn existential_polymorphic_payload_uses_flat_slots_inside_continuation() {
        for rendered in
            direct_payload_member_conversions("<U>", "[A][B] ((U -> A) & (A -> B)) -> B")
        {
            assert!(!rendered.contains("KioRuntime.map("), "{rendered}");
            assert!(!rendered.contains("java.util.Map"), "{rendered}");
        }
    }

    #[test]
    fn polymorphic_payload_keeps_nested_callback_grouping() {
        for rendered in
            direct_payload_member_conversions("", "[A][B] (((A & B) -> A) & F(A)) -> F(A)")
        {
            assert!(
                rendered.contains("KioRuntime.map(") || rendered.contains("java.util.Map"),
                "{rendered}"
            );
        }
    }

    #[test]
    fn ordinary_forall_and_monomorphic_members_keep_grouped_runtime_arguments() {
        let ordinary = compound_input_conversion("[A][B] (A & B) -> A", "value");
        assert!(ordinary.contains("java.util.Map"), "{ordinary}");
        for rendered in direct_payload_member_conversions("", "(F(.) & F(.)) -> F(.)") {
            assert!(
                rendered.contains("KioRuntime.map(") || rendered.contains("java.util.Map"),
                "{rendered}"
            );
        }
    }

    fn payload_owner_index_measurement(
        width: usize,
    ) -> super::super::skin::JavaPayloadOwnerIndexMeasurement {
        let declarations = (0..width)
            .map(|index| {
                format!(
                    "pub newtype N{index:02} : [A] A -> A {{ pub constructor make_n{index:02}; pub projector read_n{index:02}; }};"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let parameters = (0..width)
            .map(|index| format!("n{index:02}: N{index:02}"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("module api;\n{declarations}\nhost fn inspect({parameters}) -> .;");
        let package = build_package(&[source.as_str()], "bridge { api; }");
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare Java payload-owner width fixture");
        let shapes = JavaShapes::new(&package, &prepared);
        shapes.measure_newtype_payload_plan_owner_index(&prepared)
    }

    #[test]
    fn payload_owner_reverse_index_bounds_structural_operations() {
        let narrow = payload_owner_index_measurement(8);
        let wide = payload_owner_index_measurement(32);

        for (width, measurement) in [(8, narrow), (32, wide)] {
            assert_eq!(measurement.owner_count, width);
            assert_eq!(measurement.index_construction_visits, width);
            assert_eq!(measurement.indexed_lookups, width);
            assert_eq!(measurement.identity_parity_checks, width);
            assert!(measurement.indexed_lookups > 0, "the reverse index fired");
            assert_eq!(measurement.legacy_candidate_visits, width * (width + 1) / 2);
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
            "the exact legacy scan grows superlinearly"
        );
    }

    fn lower_with_sig(package: &Package<Routed>, source: &str) -> JavaPackage {
        let signature = parse_signature_file(source, Some("pkg")).expect("parse Java test sig");
        crate::sig::replay(&signature).expect("validate complete Java test sig");
        let version = signature.version.saturating_sub(1);
        let replayed =
            crate::sig::replay_through(&signature, version).expect("replay sealed Java test sig");
        lower_package(package, "pkg", Some(&(version, replayed))).expect("emit Java with sig")
    }

    #[test]
    fn recursive_newtype_walk_allocates_one_ancestry_set_per_root() {
        let traversal = RUNTIME
            .split_once("    private boolean newtypeIsRecursive")
            .expect("Java runtime has recursive-newtype entry point")
            .1
            .split_once("    private boolean isPassthrough")
            .expect("Java runtime has recursive-newtype traversal boundary")
            .0;

        assert_eq!(
            traversal.matches("new LinkedHashSet<>()").count(),
            1,
            "one root query must allocate exactly one ancestry set",
        );
        assert!(!traversal.contains("new LinkedHashSet<>(visited)"));
        assert!(
            traversal.contains("if (!visited.add(key))")
                && traversal.contains("finally {")
                && traversal.contains("visited.remove(key);")
        );
    }

    #[test]
    fn public_existential_newtype_transport_is_atomic_before_member_visibility() {
        let predicate = RUNTIME
            .split_once("    private boolean newtypeIsBoundaryAtomic")
            .expect("atomic newtype predicate")
            .1
            .split_once("    private boolean newtypeHasPublicHostSurface")
            .expect("atomic predicate boundary")
            .0;
        let surface_guard = predicate
            .find("if (!newtypeHasPublicHostSurface(resolved)) {\n        return false;")
            .expect("private and unbridged newtypes keep their existing handling");
        let existential = predicate
            .find("if (asBool(resolved.data().get(\"has_existentials\"))) {\n        return true;")
            .expect("existential host carriers are opaque even with both public members");
        let visibility = predicate
            .find("boolean constructorPublic")
            .expect("ordinary newtype member visibility still controls opacity");
        assert!(surface_guard < existential && existential < visibility);
    }

    #[test]
    fn exact_existential_newtype_members_receive_internal_payload_helpers() {
        let package = build_package(
            &["module api; \
               pub newtype Pack <U> : U { pub constructor mk; pub projector get; }; \
               pub newtype Loaded <U> : U & U { \
                 pub constructor mk_loaded; pub projector get_loaded; \
               };"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        assert!(
            emitted.handle_java.contains(
                "KioNewtype_api__Pack __newtype_body_out_api__Pack(PkgHost __host, Object value)"
            ) && emitted.handle_java.contains(
                "Object __newtype_body_in_api__Pack(PkgHost __host, Shapes.KioNewtype_api__Pack value)"
            ) && emitted
                .handle_java
                .matches("__newtype_body_out_api__Pack")
                .count()
                >= 2
                && emitted
                    .handle_java
                    .matches("__newtype_body_in_api__Pack")
                    .count()
                    >= 2,
            "{}",
            emitted.handle_java
        );
        let loaded_out = emitted
            .handle_java
            .split_once(" __newtype_body_out_api__Loaded(")
            .expect("exact existential product has an internal Out helper")
            .1
            .split_once("\n  }")
            .expect("internal Out helper has a bounded body")
            .0;
        let loaded_in = emitted
            .handle_java
            .split_once(" __newtype_body_in_api__Loaded(")
            .expect("exact existential product has an internal In helper")
            .1
            .split_once("\n  }")
            .expect("internal In helper has a bounded body")
            .0;
        assert!(
            loaded_out.contains("return new Shapes.KioNewtype_api__Loaded(value);")
                && loaded_in.contains("return value.value();")
                && !loaded_out.contains("java.util.List")
                && !loaded_in.contains("java.util.Arrays"),
            "out helper:{loaded_out}\nin helper:{loaded_in}"
        );
    }

    #[test]
    fn existential_projector_retains_result_binder_after_head_binders() {
        let package = build_package(
            &["module api; pub newtype Packed[A] <U> : U & A { \
               pub constructor make; pub projector read; };"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let declaration = emitted
            .handle_java
            .lines()
            .find(|line| line.contains(" read("))
            .expect("public projector method");
        assert!(
            declaration.contains("public <KioCallType_0, KioCallType_1> KioCallType_1 read("),
            "{declaration}"
        );
        assert!(!declaration.contains("Object"), "{declaration}");
    }

    #[test]
    fn bottom_values_cast_from_the_erased_body_at_java_boundaries() {
        let package = build_package(
            &["module api; \
               pub fn keep(value: !) -> ! { value } \
               host fn round(value: ! & !) -> ! & !;"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        assert!(
            emitted
                .handle_java
                .contains("return ((Void) __fn_keep.call(p0));")
                && emitted.handle_java.matches("((Void)").count() >= 3,
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn product_cutover_uses_a_builder_past_the_jvm_constructor_limit() {
        let shell = |arity| {
            FacadeShellId::new(
                FacadeKind::Product,
                (0..arity)
                    .map(|index| SemanticKey::Positional { index })
                    .collect(),
            )
        };

        let at_limit = shell(JAVA_INSTANCE_REFERENCE_SLOT_LIMIT as u64);
        let mut at_limit_java = String::new();
        render_facade_shell_java(
            &mut at_limit_java,
            &at_limit,
            BoundaryFacadeSupportOrigin::Live,
        );
        assert!(
            at_limit_java.contains(&format!(
                "public record {}<",
                java_facade_shell_name(&at_limit)
            )) && !at_limit_java.contains("Object[] values"),
            "{at_limit_java}"
        );

        let over_limit = shell((JAVA_INSTANCE_REFERENCE_SLOT_LIMIT + 1) as u64);
        assert!(java_facade_shell_name(&over_limit).len() < 100);
        let mut over_limit_java = String::new();
        render_facade_shell_java(
            &mut over_limit_java,
            &over_limit,
            BoundaryFacadeSupportOrigin::Live,
        );
        assert!(
            over_limit_java.contains(&format!(
                "public static final class {}<",
                java_facade_shell_name(&over_limit)
            )) && over_limit_java.contains("private final Object[] values;")
                && over_limit_java.contains("new Object[255]")
                && over_limit_java.contains("public T254 _254()")
                && over_limit_java.contains("Builder<T0, T1")
                && over_limit_java.contains("public Builder<")
                && over_limit_java.contains(" _254(T254 value)")
                && over_limit_java.contains("values.clone()"),
            "{over_limit_java}"
        );

        let wide_sum = FacadeShellId::new(
            FacadeKind::Sum,
            (0..=JAVA_INSTANCE_REFERENCE_SLOT_LIMIT as u64)
                .map(|index| SemanticKey::Positional { index })
                .collect(),
        );
        let mut wide_sum_java = String::new();
        render_facade_shell_java(
            &mut wide_sum_java,
            &wide_sum.product_companion(),
            BoundaryFacadeSupportOrigin::Live,
        );
        render_facade_shell_java(
            &mut wide_sum_java,
            &wide_sum,
            BoundaryFacadeSupportOrigin::Live,
        );
        assert!(
            wide_sum_java.contains("KioMatch(KioFacade_V1_Product_Positional_K255<")
                && wide_sum_java.contains("KioCases._254().apply(value)")
                && !wide_sum_java.contains("SumCase<T254, R> KioCase_254"),
            "{wide_sum_java}"
        );
    }

    #[test]
    fn wide_callable_and_callback_use_exact_prepared_shells() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let parameters = (0..=JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                    .map(|index| format!("p{index}: Str"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let arguments = (0..=JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                    .map(|index| format!("p{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let callback = (0..=JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                    .map(|_| "Str")
                    .collect::<Vec<_>>()
                    .join(" & ");
                let source = format!(
                    "module api; \
                     host type Str role(str); \
                     host fn receive({parameters}) -> .; \
                     host fn round_callback(callback: ({callback}) -> Str) -> Str; \
                     pub fn send({parameters}) -> . {{ receive({arguments}) }}"
                );
                let package = build_package(&[&source], "bridge { api; }");
                let emitted = lower_package(&package, "pkg", None).expect("emit wide Java facade");

                let wide_method = "api__receive(Shapes.";
                assert!(
                    emitted.host_java.contains(wide_method)
                        && emitted.handle_java.contains("public void send(Shapes.")
                        && emitted.handle_java.contains(".<T_api__Str")
                        && emitted.handle_java.contains("builder()")
                        && emitted.host_java.contains("Shapes.Fn1<Shapes.KioFacade_")
                        && !emitted.host_java.contains("Shapes.Fn255<")
                        && !emitted.shapes_java.contains("interface Fn255<"),
                    "host:\n{}\nhandle:\n{}\nshapes:\n{}",
                    emitted.host_java,
                    emitted.handle_java,
                    emitted.shapes_java
                );
            })
            .expect("spawn wide Java source-shape test")
            .join()
            .expect("run wide Java source-shape test");
    }

    #[test]
    fn retained_host_binding_is_optional_and_keeps_deprecated_nominal_defaults() {
        let package = build_package(
            &["module api; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Gone role(i32);\n        host fn archived(value: Gone) -> Gone;\n        \
             pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        \
             archived;\n        Gone;\n      }\n    }\n  }\n}\n",
        );

        assert!(
            emitted.host_java.contains("public interface PkgHost {")
                && !emitted.host_java.contains("PkgHost<T_api__Gone>"),
            "{}",
            emitted.host_java
        );
        assert!(
            emitted
                .host_java
                .contains("default Shapes.KioHostType_api__Gone KioHostBinding_api__Gone_fromBody(Integer value)")
                && emitted
                    .host_java
                    .contains("default Integer KioHostBinding_api__Gone_toBody(Shapes.KioHostType_api__Gone value)")
                && emitted
                    .host_java
                    .contains("default Shapes.KioHostType_api__Gone api__archived(Shapes.KioHostType_api__Gone p0)"),
            "{}",
            emitted.host_java
        );
        assert!(
            emitted
                .shapes_java
                .contains("@Deprecated\n  /** Exact declaration-owned carrier for `api.Gone`. */\n  public interface KioHostType_api__Gone {}"),
            "{}",
            emitted.shapes_java
        );
        assert_eq!(
            emitted
                .host_java
                .matches("host type api.Gone was removed at contract v2")
                .count(),
            2,
            "{}",
            emitted.host_java
        );
        assert!(
            !emitted
                .handle_java
                .contains("KioHostBinding_api__Gone_fromBody(")
                && !emitted
                    .handle_java
                    .contains("KioHostBinding_api__Gone_toBody("),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn retained_host_signature_keeps_its_frozen_newtype_carrier() {
        let package = build_package(
            &["module api; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             newtype Token[A] : A { pub constructor make_token; projector read_token; };\n        \
             host fn archived(value: Token(.)) -> .;\n        \
             pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        \
             Token;\n        archived;\n      }\n    }\n  }\n}\n",
        );
        let carrier = exact_newtype_carrier("api", "Token");

        assert!(
            emitted.shapes_java.contains(&format!(
                "@Deprecated\n  public static final class {carrier}<T0>"
            )),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .host_java
                .contains(&format!("api__archived(Shapes.{carrier}<Shapes.Unit> p0)")),
            "{}",
            emitted.host_java
        );
        assert!(
            !emitted.handle_java.contains("makeToken")
                && !emitted.handle_java.contains("readToken"),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn retained_equal_payload_epochs_ignore_spans_and_close_forall_support() {
        let package = build_package(
            &["module api; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(4);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             newtype Box[A] : [T] T -> A { pub constructor make; pub projector read; };\n        \
             host fn first(value: Box(.)) -> .;\n        \
             pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  breaking {\n    add {\n      module api {\n        \
             host fn second(value: Box(.)) -> .;\n      }\n    };\n    modify {\n      module api {\n        \
             newtype Box[A] : [T] T -> A { pub constructor make; pub projector read; };\n      }\n    };\n    remove {\n      module api {\n        first;\n      }\n    }\n  }\n}\n\
             v(3) {\n  nonbreaking {\n    remove {\n      module api {\n        Box;\n        second;\n      }\n    }\n  }\n}\n",
        );
        let carrier = exact_newtype_carrier("api", "Box");
        let forall = "KioForall_Newtype_api__Box_Binders_K1_";

        assert_eq!(
            emitted
                .shapes_java
                .matches(&format!("public record {carrier}<T0>"))
                .count(),
            1,
            "semantically equal retained payload epochs emitted multiple carriers:\n{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("@Deprecated\n  public record {carrier}<T0>"))
                && emitted.shapes_java.contains(&format!(
                    "@Deprecated\n  @FunctionalInterface\n  public interface {forall}"
                ))
                && emitted.shapes_java.contains(" T0 apply(")
                && emitted.shapes_java.contains("KioPolyType_"),
            "retained transparent payload lost its closed deprecated forall support:\n{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn retained_only_host_history_keeps_hostless_factory_and_no_runtime_adapter() {
        let package = build_package(
            &["module api; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Gone role(i32);\n        \
             host fn archived(value: Gone) -> Gone;\n        \
             pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        \
             archived;\n        Gone;\n      }\n    }\n  }\n}\n",
        );

        assert!(
            emitted
                .host_java
                .contains("default Shapes.KioHostType_api__Gone")
                && emitted.handle_java.contains("Pkg create() {")
                && emitted.handle_java.contains(
                    "KioRuntime.createPackage(KIO_IR_JSON, new KioRuntime.KioObject()), null"
                ),
            "retained-only Java host history made a current host/factory argument mandatory:\nhost:\n{}\nhandle:\n{}",
            emitted.host_java,
            emitted.handle_java
        );
        assert!(
            !emitted.handle_java.contains("__ns_API.set(\"archived\"")
                && !emitted
                    .handle_java
                    .contains("KioHostBinding_api__Gone_fromBody")
                && !emitted
                    .handle_java
                    .contains("KioHostBinding_api__Gone_toBody"),
            "retained-only Java host history entered the live runtime adapter:\n{}",
            emitted.handle_java
        );
    }

    #[test]
    fn retained_wide_host_method_keeps_one_exact_prepared_shell() {
        // The CLI uses a dedicated stack for recursive compiler passes; keep
        // this retained-wide fixture in the same context as the live-wide test.
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let parameters = (0..=JAVA_INSTANCE_REFERENCE_SLOT_LIMIT)
                    .map(|index| format!("p{index}: Gone"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let package = build_package(
                    &["module api; pub fn unit() -> . { () }"],
                    "bridge { api; }",
                );
                let signature = format!(
                    "signature pkg v(3);\n\
             v(1) {{\n  nonbreaking {{\n    add {{\n      module api {{\n        \
             host type Gone role(str);\n        host fn archived({parameters}) -> .;\n        \
             pub fn unit() -> .;\n      }}\n    }}\n  }}\n}}\n\
             v(2) {{\n  nonbreaking {{\n    remove {{\n      module api {{\n        \
             archived;\n        Gone;\n      }}\n    }}\n  }}\n}}\n"
                );
                let emitted = lower_with_sig(&package, &signature);
                let declaration = emitted
                    .host_java
                    .lines()
                    .find(|line| line.contains("default void api__archived("))
                    .expect("retained wide Java host declaration");

                assert!(
                    declaration.contains("Shapes.KioFacade_")
                        && declaration.contains(" p0)")
                        && !declaration.contains(" p1")
                        && emitted
                            .shapes_java
                            .contains("private final Object[] values;")
                        && emitted.shapes_java.contains("builder()"),
                    "host:\n{}\nshapes:\n{}",
                    emitted.host_java,
                    emitted.shapes_java
                );
                let shell = declaration
                    .split_once("Shapes.")
                    .and_then(|(_, tail)| tail.split_once('<'))
                    .map(|(name, _)| name)
                    .expect("retained wide method names its exact shell");
                assert!(
                    emitted.shapes_java.contains(&format!(
                        "@Deprecated\n  public static final class {shell}<"
                    )) && emitted.shapes_java.contains("@Deprecated public static <")
                        && emitted
                            .shapes_java
                            .contains("@Deprecated\n    public static final class Builder")
                        && emitted
                            .shapes_java
                            .contains("@Deprecated public T254 _254()")
                        && emitted.shapes_java.contains("@Deprecated public Builder<")
                        && emitted
                            .shapes_java
                            .contains(&format!("@Deprecated public {shell}<")),
                    "retained wide shell and helpers are not visibly deprecated:\n{}",
                    emitted.shapes_java
                );
            })
            .expect("spawn retained-wide Java source-shape test")
            .join()
            .expect("run retained-wide Java source-shape test");
    }

    #[test]
    fn live_support_identity_dominates_retained_history() {
        let package = build_package(
            &[
                "module api; host type Str role(str); host fn current(callback: Str -> .) -> .; pub fn unit() -> . { () }",
            ],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type Str role(str);\n        host fn removed(callback: Str -> .) -> .;\n        \
             pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        removed;\n      }\n    }\n  }\n}\n",
        );
        assert!(
            emitted.shapes_java.contains("public interface Proc1<")
                && !emitted
                    .shapes_java
                    .contains("@Deprecated\n  @FunctionalInterface\n  public interface Proc1<"),
            "live Java callable support inherited retained deprecation:\n{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn live_newtype_payload_dominates_retained_same_identity() {
        let package = build_package(
            &[
                "module api; host type Str role(str); pub newtype Token : Str { pub constructor make_token; pub projector read_token; }; host fn current(value: Token) -> .; pub fn unit() -> . { () }",
            ],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3);\n\
             v(1) {\n  nonbreaking {\n    add {\n      module api {\n        \
             host type I32 role(i32);\n        newtype Token : I32 { pub constructor make_token; pub projector read_token; };\n        \
             host fn old(value: Token) -> .;\n        pub fn unit() -> .;\n      }\n    }\n  }\n}\n\
             v(2) {\n  nonbreaking {\n    remove {\n      module api {\n        I32;\n        Token;\n        old;\n      }\n    }\n  }\n}\n",
        );
        let carrier = exact_newtype_carrier("api", "Token");
        assert!(
            emitted.shapes_java.contains(&format!(
                "public record {carrier}<T_api__Str>(T_api__Str value)"
            )) && !emitted.shapes_java.contains(&format!(
                "public record {carrier}<T_api__Str>(Shapes.KioHostType_api__I32 value)"
            )),
            "retained payload replaced the live Java newtype carrier:\n{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn type_variables_keep_ordered_head_and_generic_rank_n_scope() {
        let package = build_package(
            &["module main; \
               host type Str role(str); \
               pub newtype T : Str { pub constructor make_t; pub projector un_t; }; \
               host fn apply_poly(f: [T] T -> T) -> Str; \
               pub fn ordered(value: T)[T](later: T) -> T { later }"],
            "bridge { main; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        let rank_n = "KioForall_Site_M_main__H_applyPoly_Binders_K1_B0_A0";
        assert!(
            emitted.host_java.contains(&format!(
                "T_main__Str main__applyPoly(Shapes.{rank_n}<T_main__Str> p0);"
            )) && emitted
                .shapes_java
                .contains(&format!("public interface {rank_n}<T_main__Str>"))
                && emitted
                    .shapes_java
                    .contains("public <KioPolyType_0> KioPolyType_0 apply(KioPolyType_0 p0);")
                && !emitted.host_java.contains("Fn1<Object, Object>")
                && !emitted.shapes_java.contains("apply(Object p0)"),
            "host:\n{}\nshapes:\n{}",
            emitted.host_java,
            emitted.shapes_java
        );
        assert!(
            emitted.handle_java.contains(
                "public <KioCallType_0> KioCallType_0 ordered(Shapes.KioNewtype_main__T<T_main__Str> p0, KioCallType_0 p1)"
            ),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn rank_n_name_helper_drives_budgeted_declaration_and_use() {
        let package = build_package(
            &[
                "module testapi; host type String role(str);",
                "module testapi/arith; import testapi(String); \
                 host fn apply_poly(f: [T] T -> T) -> String; \
                 host fn apply_polymorphic_identity_with_a_deliberately_long_public_name( \
                     f: [U] U -> U \
                 ) -> String;",
            ],
            "bridge { testapi; testapi/arith; }",
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, None)
            .expect("prepare Java rank-N site");
        let shapes = JavaShapes::new(&package, &prepared);
        let site = prepared
            .sites()
            .find(|site| {
                site.site().module_segments() == ["testapi", "arith"]
                    && matches!(
                        site.site().owner(),
                        BoundaryFacadeSiteOwner::HostFunction { name } if name == "apply_poly"
                    )
            })
            .expect("rank-N host site");
        let BoundaryCallableHeadStage::Value { slots } = site.plan().entry().head_stages[0] else {
            panic!("rank-N host site starts with one value stage")
        };
        let rank_n = java_forall_interface_name(site, site.plan().facade(), slots[0], &shapes)
            .expect("name rank-N interface");
        assert_eq!(
            rank_n,
            "KioForall_Site_M_testapi_sarith__H_applyPoly_Binders_K1_B0_A0"
        );
        assert_eq!(rank_n.len(), 61);
        assert!(rank_n.len() <= READABLE_MINT_MAX_LEN);

        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        assert!(
            emitted
                .host_java
                .contains(&format!("Shapes.{rank_n}<T_testapi__String> p0"))
                && emitted
                    .shapes_java
                    .contains(&format!("public interface {rank_n}<T_testapi__String>")),
            "host:\n{}\nshapes:\n{}",
            emitted.host_java,
            emitted.shapes_java
        );

        let long_site = prepared
            .sites()
            .find(|site| {
                matches!(
                    site.site().owner(),
                    BoundaryFacadeSiteOwner::HostFunction { name }
                        if name
                            == "apply_polymorphic_identity_with_a_deliberately_long_public_name"
                )
            })
            .expect("long rank-N host site");
        let BoundaryCallableHeadStage::Value { slots } = long_site.plan().entry().head_stages[0]
        else {
            panic!("long rank-N host site starts with one value stage")
        };
        let exact =
            java_forall_exact_identity(long_site, long_site.plan().facade(), slots[0], &shapes)
                .expect("identify long rank-N interface");
        let bounded =
            java_forall_interface_name(long_site, long_site.plan().facade(), slots[0], &shapes)
                .expect("name long rank-N interface");
        assert!(exact.len() > READABLE_MINT_MAX_LEN);
        assert_eq!(bounded, format!("KioForall_H{}", fnv1a_64_hex(&exact)));
        assert!(bounded.len() <= READABLE_MINT_MAX_LEN);
        assert!(
            emitted.host_java.contains(&format!("Shapes.{bounded}<"))
                && emitted
                    .shapes_java
                    .contains(&format!("public interface {bounded}<")),
            "host:\n{}\nshapes:\n{}",
            emitted.host_java,
            emitted.shapes_java
        );
    }

    #[test]
    fn unsaturated_nominals_keep_exact_constructor_markers() {
        let package = build_package(
            &["module api; \
               pub newtype Box[A] : A { pub constructor make_box; pub projector read_box; }; \
               pub newtype Functor[*F] : [A] F(A) -> F(A) { pub constructor make_functor; pub projector read_functor; }; \
               host fn round(value: Functor(Box)) -> Functor(Box);"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let box_marker = "Shapes.KioNewtypeMk_api__Box";
        let functor = format!("Shapes.KioNewtype_api__Functor<{box_marker}>");

        assert!(
            emitted
                .shapes_java
                .contains("public interface KioNewtypeMk_api__Box {}")
                && emitted
                    .shapes_java
                    .contains("public interface KioNewtypeMk_api__Functor {}")
                && emitted
                    .shapes_java
                    .contains("public static final class Apply1<F, T0>")
                && emitted.shapes_java.contains("private final Object __body;")
                && emitted
                    .shapes_java
                    .contains("static <F, T0> Apply1<F, T0> __apply1_fromBody(Object body)")
                && emitted
                    .shapes_java
                    .contains("static <F, T0> Object __apply1_toBody(Apply1<F, T0> value)"),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .host_java
                .contains(&format!("{functor} api__round({functor} p0);")),
            "{}",
            emitted.host_java
        );
        assert!(
            emitted.handle_java.contains(
                "public <KioNewtypeType_0> Shapes.Apply1<Shapes.KioNewtypeMk_api__Box, KioNewtypeType_0> KioNewtypeApplication_api__Box_lift(Shapes.KioNewtype_api__Box<KioNewtypeType_0> value)"
            ) && emitted.handle_java.contains(
                "public <KioNewtypeType_0> Shapes.KioNewtype_api__Box<KioNewtypeType_0> KioNewtypeApplication_api__Box_project(Shapes.Apply1<Shapes.KioNewtypeMk_api__Box, KioNewtypeType_0> value)"
            ),
            "{}",
            emitted.handle_java
        );
        assert!(
            !emitted.handle_java.contains("new Pkg<>(")
                && !emitted.handle_java.contains("new Ns_M_api<>(")
                && !emitted.handle_java.contains("new NtH_M_api__T_Box<>(")
                && !emitted.handle_java.contains("new NtH_M_api__T_Functor<>("),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn nominal_application_adapters_use_body_helpers_and_preserve_opaque_storage() {
        let package = build_package(
            &["module api; \
               pub newtype Box[A] : A & . { pub constructor make; pub projector read; }; \
               pub newtype Hidden[A] : A & . { constructor make; projector read; };"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        for (name, domain) in [("Box", "body_"), ("Hidden", "")] {
            let lift = emitted
                .handle_java
                .split_once(&format!(" KioNewtypeApplication_api__{name}_lift("))
                .expect("public nominal lift")
                .1
                .split_once("\n  }")
                .expect("lift body")
                .0;
            let project = emitted
                .handle_java
                .split_once(&format!(" KioNewtypeApplication_api__{name}_project("))
                .expect("public nominal project")
                .1
                .split_once("\n  }")
                .expect("project body")
                .0;
            assert!(
                lift.contains(&format!(
                    "__apply1_fromBody(__newtype_{domain}in_api__{name}(__host, value))"
                )),
                "{lift}"
            );
            assert!(
                project.contains(&format!(
                    "__newtype_{domain}out_api__{name}(__host, Shapes.__apply1_toBody(value))"
                )),
                "{project}"
            );
            assert!(!lift.contains("KioRuntime.map("), "{lift}");
            assert!(!project.contains("java.util.Map"), "{project}");
        }
    }

    #[test]
    fn parameterized_host_type_application_uses_its_exact_adapter_seam() {
        let package = build_package(
            &["module api; \
               host type Box[A]; \
               host fn round[A](value: Box(A)) -> Box(A);"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        assert!(
            emitted
                .shapes_java
                .contains("public interface KioHostTypeMk_api__Box {}")
                && emitted.handle_java.contains(
                    "public <KioHostArg_0> Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, KioHostArg_0> KioHostTypeApplication_api__Box_lift(Shapes.KioHostType_api__Box<KioHostArg_0> value)"
                )
                && emitted.handle_java.contains(
                    "public <KioHostArg_0> Shapes.KioHostType_api__Box<KioHostArg_0> KioHostTypeApplication_api__Box_project(Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, KioHostArg_0> value)"
                )
                && !emitted.host_java.contains("Apply1<Object")
                && !emitted.handle_java.contains("Apply1<Object"),
            "shapes:\n{}\nhandle:\n{}\nhost:\n{}",
            emitted.shapes_java,
            emitted.handle_java,
            emitted.host_java
        );
    }

    #[test]
    fn procedure_callbacks_execute_on_both_boundary_directions() {
        let package = build_package(
            &["module api; \
               host type Str role(str); \
               pub fn invoke(callback: Str -> .) -> . { callback(\"called\"(Str)) } \
               pub fn supply() -> Str -> . { .(_value: Str) { () } }"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        assert!(
            emitted.handle_java.contains("(p0).apply(")
                && emitted
                    .handle_java
                    .contains("((KioRuntime.KioFn) __fn_supply.call()).call("),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn public_opaque_newtypes_are_nameable_without_public_payload_access() {
        let package = build_package(
            &["module api; import __comptime__; \
               newtype Hidden_payload : . { constructor mk_hidden; projector un_hidden; }; \
               pub newtype Opaque_a : Hidden_payload { constructor mk_opaque_a; projector un_opaque_a; }; \
               pub newtype Opaque_b : Hidden_payload { constructor mk_opaque_b; projector un_opaque_b; }; \
               pub newtype Constructor_only[A] : A { pub constructor make_constructor; projector read_constructor; }; \
               pub newtype Projector_only[A] : A { constructor make_projector; pub projector read_projector; }; \
               pub newtype Existential <U> : U { constructor make_existential; pub projector read_existential; }; \
               pub newtype Transparent_both : . { pub constructor make_transparent; pub projector read_transparent; }; \
               pub type Unit_alias = .; \
               pub newtype Constructor_unit_alias : Unit_alias { pub constructor make_constructor_unit_alias; projector read_constructor_unit_alias; }; \
               pub newtype Unit : . { constructor make_unit; projector read_unit; }; \
               rec { \
                 pub newtype Outer : Hidden { pub constructor make_outer; pub projector read_outer; }; \
                 pub newtype Hidden : Outer { constructor make_hidden_cycle; projector read_hidden_cycle; }; \
               } \
               pub rec newtype Direct_recursive : . | Direct_recursive { pub constructor make_direct; pub projector read_direct; }; \
               pub rec newtype Recursive_constructor : . | Recursive_constructor { pub constructor make_recursive; projector read_recursive_hidden; }; \
               pub rec newtype Recursive_projector : . | Recursive_projector { constructor make_recursive_hidden; pub projector read_recursive; }; \
               pub newtype Comptime_both : __Type__ { pub constructor make_comptime_both; pub projector read_comptime_both; }; \
               pub newtype Comptime_opaque : __Type__ { constructor make_comptime_opaque; projector read_comptime_opaque; }; \
               pub newtype Comptime_constructor : __Type__ { pub constructor make_comptime_constructor; projector read_comptime_constructor; }; \
               pub newtype Packed_function : (Opaque_a & Projector_only(.)) -> Opaque_a { pub constructor make_packed_function; pub projector read_packed_function; }; \
               pub newtype Spread_function : [A] (Opaque_a & A) -> Opaque_a { pub constructor make_spread_function; pub projector read_spread_function; }; \
               pub fn keep_a(value: Opaque_a) -> Opaque_a { value } \
               pub fn keep_b(value: Opaque_b) -> Opaque_b { value } \
               host fn echo_outer(value: Outer) -> Outer; \
               host fn echo_direct(value: Direct_recursive) -> Direct_recursive; \
               host fn nested_partial(value: Opaque_a & Projector_only(.)) -> Opaque_a | Projector_only(.); \
               host fn nested_both(value: Transparent_both & Transparent_both) -> Transparent_both & Transparent_both;"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");

        let opaque_a = exact_newtype_carrier("api", "Opaque_a");
        let opaque_b = exact_newtype_carrier("api", "Opaque_b");
        let constructor_only = exact_newtype_carrier("api", "Constructor_only");
        let projector_only = exact_newtype_carrier("api", "Projector_only");
        let existential = exact_newtype_carrier("api", "Existential");
        let hidden = exact_newtype_carrier("api", "Hidden");
        let recursive_constructor = exact_newtype_carrier("api", "Recursive_constructor");
        let recursive_projector = exact_newtype_carrier("api", "Recursive_projector");
        let transparent = exact_newtype_carrier("api", "Transparent_both");
        let outer = exact_newtype_carrier("api", "Outer");
        let direct_recursive = exact_newtype_carrier("api", "Direct_recursive");
        let comptime_both = exact_newtype_carrier("api", "Comptime_both");
        let comptime_opaque = exact_newtype_carrier("api", "Comptime_opaque");
        let comptime_constructor = exact_newtype_carrier("api", "Comptime_constructor");

        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {opaque_a}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {opaque_b}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            !emitted
                .shapes_java
                .contains(&format!("public record {opaque_a}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            !emitted
                .shapes_java
                .contains(&format!("public record {opaque_b}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.handle_java.contains("class NtH_M_api__T_OpaqueA"),
            "{}",
            emitted.handle_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {constructor_only}"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {projector_only}"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {existential}"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {hidden}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public record {transparent}(Shapes.Unit value)")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.shapes_java.contains("public record Unit() {")
                && !emitted
                    .shapes_java
                    .contains("public static final class Unit {"),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.handle_java.contains("makeConstructor(")
                && !emitted.handle_java.contains("readConstructor(")
                && !emitted.handle_java.contains("makeProjector(")
                && emitted.handle_java.contains("readProjector(")
                && emitted.handle_java.contains("readExistential("),
            "{}",
            emitted.handle_java
        );
        assert!(
            emitted.handle_java.contains("makeConstructorUnitAlias()"),
            "{}",
            emitted.handle_java
        );
        assert!(
            emitted
                .host_java
                .contains(&format!("Shapes.{outer} api__echoOuter(Shapes.{outer} p0)")),
            "{}",
            emitted.host_java
        );
        let recursive_constructor_method = emitted
            .handle_java
            .lines()
            .find(|line| line.contains(" makeRecursive("))
            .expect("recursive constructor method");
        let recursive_projector_method = emitted
            .handle_java
            .lines()
            .find(|line| line.contains(" readRecursive("))
            .expect("recursive projector method");
        assert!(
            recursive_constructor_method
                .contains(&format!("public Shapes.{recursive_constructor}"))
                && recursive_constructor_method.contains("(Shapes.KioFacade_")
                && recursive_projector_method.contains("public Shapes.KioFacade_")
                && recursive_projector_method
                    .contains(&format!("(Shapes.{recursive_projector} p0)")),
            "constructor: {recursive_constructor_method}\nprojector: {recursive_projector_method}\n{}",
            emitted.handle_java
        );
        assert!(
            emitted.shapes_java.contains(&format!(
                "public static final class {recursive_constructor} {{\n    final Object value;"
            )) && emitted.shapes_java.contains(&format!(
                "public static final class {recursive_projector} {{\n    final Object value;"
            )),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.host_java.contains(&format!(
                "Shapes.{direct_recursive} api__echoDirect(Shapes.{direct_recursive} p0)"
            )) && emitted.shapes_java.contains(&format!(
                "public record {direct_recursive}(Shapes.KioFacade_"
            )),
            "shapes:\n{}\nhost:\n{}",
            emitted.shapes_java,
            emitted.host_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public record {comptime_both}(Void value)"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {comptime_opaque}"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {comptime_constructor}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("Shapes.Fn2<Shapes.{opaque_a}"))
                && emitted.shapes_java.contains(&format!(
                    "Shapes.{projector_only}<Shapes.Unit>, Shapes.{opaque_a}>"
                )),
            "{}",
            emitted.shapes_java
        );
        assert!(
            !emitted
                .shapes_java
                .contains("KioNewtype_api__HiddenPayload"),
            "{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn comptime_prefixed_user_types_keep_exact_boundary_identity() {
        let package = build_package(
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
            "bridge { visible; hidden; generic; hostish; aliasing; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let visible = exact_newtype_carrier("visible", "Comptime_bool");
        let hidden = exact_newtype_carrier("hidden", "Comptime_bool");
        let wrapped = exact_newtype_carrier("hostish", "Wrapped");

        assert!(
            emitted
                .shapes_java
                .contains(&format!("public record {visible}<")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {hidden}")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.handle_java.contains(&format!(
                "Shapes.{visible}<T_aliasing__I32, T_hidden__I32, T_hostish__ComptimeBool, T_visible__I32> roundVisible(Shapes.{visible}<T_aliasing__I32, T_hidden__I32, T_hostish__ComptimeBool, T_visible__I32> p0)"
            )) && emitted.handle_java.contains(&format!(
                "Shapes.{hidden}<T_aliasing__I32, T_hidden__I32, T_hostish__ComptimeBool, T_visible__I32> roundHidden(Shapes.{hidden}<T_aliasing__I32, T_hidden__I32, T_hostish__ComptimeBool, T_visible__I32> p0)"
            ))
                && emitted
                    .handle_java
                    .contains("public <KioCallType_0> KioCallType_0 keep(KioCallType_0 p0)")
                && emitted
                    .handle_java
                    .contains("public T_hostish__ComptimeBool roundHost(T_hostish__ComptimeBool p0)")
                && emitted
                    .handle_java
                    .contains("public T_aliasing__I32 roundAlias(T_aliasing__I32 p0)")
                && emitted
                    .handle_java
                    .contains(&format!("Shapes.{wrapped}<"))
                && emitted
                    .shapes_java
                    .contains(&format!("public record {wrapped}<")),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn same_leaf_host_types_keep_exact_roles() {
        let package = build_package(
            &[
                "module left; \
                 host type Shared role(i32); \
                 host fn keep_left(value: Shared) -> Shared;",
                "module right; \
                 host type Shared role(str); \
                 host fn keep_right(value: Shared) -> Shared;",
            ],
            "bridge { left; right; }",
        );
        let ty = |segments: &[&str]| {
            Type::synth_path(
                segments
                    .iter()
                    .map(|segment| (*segment).to_owned())
                    .collect(),
                Vec::new(),
                crate::span::Span::new(0, 0),
            )
        };

        assert_eq!(
            crate::host_descriptor::exact_host_type_role(&package, &ty(&["left", "Shared"]),),
            Some(crate::ast::Role::I32),
        );
        assert_eq!(
            crate::host_descriptor::exact_host_type_role(&package, &ty(&["right", "Shared"]),),
            Some(crate::ast::Role::Str),
        );
        assert_eq!(
            crate::host_descriptor::exact_host_type_role(&package, &ty(&["Shared"])),
            None,
        );
    }

    #[test]
    fn same_leaf_public_newtypes_keep_exact_carrier_identities() {
        let package = build_package(
            &[
                "module left; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round_token(value: Token) -> Token;",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round_token(value: Token) -> Token;",
            ],
            "bridge { left; right; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let left = emitted
            .host_java
            .lines()
            .find(|line| line.contains("left__roundToken"))
            .expect("left host method");
        let right = emitted
            .host_java
            .lines()
            .find(|line| line.contains("right__roundToken"))
            .expect("right host method");

        assert_ne!(left, right, "{}", emitted.host_java);
        assert!(
            !left.contains("Object") && !right.contains("Object"),
            "{}",
            emitted.host_java
        );
        let left_carrier = exact_newtype_carrier("left", "Token");
        let right_carrier = exact_newtype_carrier("right", "Token");
        assert_ne!(left_carrier, right_carrier);
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {left_carrier}"))
                && emitted
                    .shapes_java
                    .contains(&format!("public static final class {right_carrier}")),
            "{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn partial_carrier_name_is_stable_when_an_unrelated_host_surface_claims_its_readable_base() {
        let target = "module zed; \
            pub newtype Token : . { constructor make_token; projector read_token; }; \
            host fn round_token(value: Token) -> Token;";
        let baseline = build_package(&[target], "bridge { zed; }");
        let with_earlier_shape = build_package(
            &[
                "module aaa; \
                 pub newtype Token : . { pub constructor make_token; pub projector read_token; }; \
                 host fn unrelated(value: Token) -> Token;",
                target,
            ],
            "bridge { aaa; zed; }",
        );
        let baseline = lower_package(&baseline, "pkg", None).expect("emit baseline Java");
        let with_earlier_shape =
            lower_package(&with_earlier_shape, "pkg", None).expect("emit augmented Java");
        let target_method = |host_java: &str| {
            host_java
                .lines()
                .find(|line| line.contains("zed__roundToken"))
                .expect("target host method")
                .trim()
                .to_owned()
        };

        assert_eq!(
            target_method(&baseline.host_java),
            target_method(&with_earlier_shape.host_java),
            "baseline:\n{}\nwith unrelated earlier shape:\n{}",
            baseline.host_java,
            with_earlier_shape.host_java,
        );
    }
}

fn java_string_literal(s: &str) -> String {
    format!("\"{}\"", escape_java(s))
}

fn java_chunked_string_builder(s: &str) -> String {
    const CHUNK_CHARS: usize = 4000;
    let mut out = format!("    StringBuilder out = new StringBuilder({});\n", s.len());
    let mut chunk = String::new();
    for ch in s.chars() {
        chunk.push(ch);
        if chunk.chars().count() >= CHUNK_CHARS {
            out.push_str("    out.append(");
            out.push_str(&java_string_literal(&chunk));
            out.push_str(");\n");
            chunk.clear();
        }
    }
    out.push_str("    out.append(");
    out.push_str(&java_string_literal(&chunk));
    out.push_str(");\n");
    out.push_str("    return out.toString();\n");
    out
}

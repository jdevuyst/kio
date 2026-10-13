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
//! Body and public values share canonical binary structural, nominal and
//! callable carriers. The facade only selects native declaration parameter
//! groups and casts the interpreter's erased references to their checked
//! indices; role adapters run at the exact literal and condition operations.

use std::collections::BTreeMap;

use crate::ast::Routed;
use crate::backends::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner,
    BoundaryFacadeSupportOrigin, BoundaryHostBindingOrigin, BoundaryNewtypePayloadPlan,
    BoundaryNewtypeSurface, BoundaryNominalDeclaration, BoundaryNominalDependencies,
    BoundaryNominalTypeParam, CallableExecutionStage, CallableSourceParamAdapter,
    CallableValueStageLayout, FacadeBinderId, FacadeKind, FacadeShellId, FacadeUse, FacadeUseId,
    PreparedBoundaryCallableOriginRef, PreparedBoundaryCallableSite, PreparedBoundaryCallableSites,
    QualifiedTypeName, SemanticKey,
};
use crate::backends::namespace::pascal_case;
use crate::backends::public_names::{
    FacadeSelector, encode_host_identity, host_module_key, host_name_core,
};
use crate::backends::skin::{budgeted_mint_name, fnv1a_64_hex};
use crate::backends::value_plan::{ValueCapture, ValuePlan, ValueStageShape, ValueUse, ValueUseId};
use crate::pass::resolve::Package;

use super::naming::{
    NominalKind, facade_class_name, nominal_name, nominal_path, shadows_support_package,
    storage_name,
};
use super::skin::{
    JAVA_INSTANCE_REFERENCE_SLOT_LIMIT, JavaHostBinding, JavaShapes, java_semantic_member,
    role_to_java_type, sanitize_java_member,
};

const RUNTIME: &str = include_str!("runtime.java");

#[cfg(test)]
mod runtime_tests {
    use super::RUNTIME;

    #[test]
    fn guest_binding_does_not_collect_argument_slots() {
        let guest = RUNTIME
            .split_once("    private Object layer(")
            .unwrap()
            .1
            .split_once("    private Object nextLayer(")
            .unwrap()
            .0;
        assert!(!guest.contains("productSlots("));

        let host = RUNTIME
            .split_once("    private Object hostLayer(")
            .unwrap()
            .1
            .split_once("    private Object nextHostLayer(")
            .unwrap()
            .0;
        assert!(host.contains("productSlots("));
    }
}

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
        let note = java_deprecation_note(removed_at);
        out.push_str(&format!(
            "{indent}/** @deprecated {note} */\n{indent}@java.lang.Deprecated\n"
        ));
    }
}

fn java_deprecation_note(removed_at: u32) -> String {
    format!("Retained only for source compatibility after removal at contract v{removed_at}.")
}

fn java_deprecation_annotation(origin: BoundaryFacadeSupportOrigin) -> &'static str {
    if origin.removed_at_version().is_some() {
        "@java.lang.Deprecated "
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
    if binding.type_arity != 0 {
        let marker = format!(
            "Shapes.{}",
            java_nominal_constructor_marker(&binding.name, NominalKind::Host)
        );
        render_java_application(&marker, parameters)
    } else if binding.retained_at.is_some() {
        format!("Shapes.{}", binding.carrier())
    } else {
        binding
            .type_parameter()
            .expect("nullary host binding has a selected native parameter")
    }
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

    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .map_err(|error| EmitError::internal(error.to_string()))?;
    let ir = crate::backends::serialized_runtime_ir::package_ir(
        package,
        crate::backends::serialized_runtime_ir::RuntimeStageRepresentation::Typed,
    )
    .map_err(EmitError::internal)?;
    let ir_json = serde_json::to_string(&ir)
        .map_err(|e| EmitError::internal(format!("cannot encode Java IR: {e}")))?;
    let mut shapes = JavaShapes::new(package, &prepared);

    let tree = collect_export_tree(&prepared, &mut shapes)?;

    let handle_java = render_handle(namespace, &handle, &tree, &prepared, &mut shapes, &ir_json)?;
    let host_java = render_host_interface(namespace, &handle, &prepared, &mut shapes)?;
    let shapes_java = render_shapes_java(namespace, &shapes, &prepared, ir.body_value_plans())?;
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
}

struct ExportNewtypeHandle {
    type_name: String,
    ctor: Option<ExportNewtypeMember>,
    projector: Option<ExportNewtypeMember>,
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
}

/// Java's native argument list is a presentation of prepared semantic slots.
/// The interpreter's argument list is independently selected by execution
/// provenance; retained declarations need only this presentation view.
struct JavaParameters<'a> {
    stages: Vec<&'a CallableValueStageLayout>,
    shell: Option<&'a FacadeShellId>,
    wide: bool,
}

impl<'a> JavaParameters<'a> {
    fn new(stages: Vec<&'a CallableValueStageLayout>, shell: Option<&'a FacadeShellId>) -> Self {
        let mut parameters = Self {
            stages,
            shell,
            wide: false,
        };
        parameters.wide = parameters.arity() > JAVA_INSTANCE_REFERENCE_SLOT_LIMIT;
        parameters
    }

    fn head(site: PreparedBoundaryCallableSite<'a>) -> Self {
        Self::new(
            site.presentation()
                .head_stages()
                .iter()
                .filter_map(|stage| match stage {
                    CallableExecutionStage::Value(layout) => Some(layout),
                    CallableExecutionStage::Type { .. } => None,
                })
                .collect(),
            site.plan().head_value_shell(),
        )
    }

    fn arity(&self) -> usize {
        if self.wide {
            return 1;
        }
        self.stages
            .iter()
            .map(|stage| stage.public_parameter_count())
            .sum()
    }

    fn names(&self, prefix: &str) -> Vec<String> {
        (0..self.arity())
            .map(|index| format!("{prefix}{index}"))
            .collect()
    }

    fn types(&self, slots: &[String]) -> Result<Vec<String>, EmitError> {
        if self.wide {
            return Ok(vec![java_product_shell_type(self.whole_shell()?, slots)?]);
        }
        let mut types = Vec::new();
        let mut offset = 0;
        for stage in &self.stages {
            for source in stage.source_params() {
                let range = source.facade_slots();
                let values = &slots[offset + range.start..offset + range.end];
                match source.adapter() {
                    CallableSourceParamAdapter::UnitValue => {}
                    CallableSourceParamAdapter::Identity => types.push(values[0].clone()),
                    CallableSourceParamAdapter::RightNest => {
                        types.push(java_binary_product_type(values)?)
                    }
                }
            }
            offset += stage.facade_slot_count();
        }
        assert_eq!(
            offset,
            slots.len(),
            "Java presentation covers all semantic slots"
        );
        Ok(types)
    }

    fn declarations(&self, slots: &[String], prefix: &str) -> Result<Vec<String>, EmitError> {
        Ok(self
            .types(slots)?
            .iter()
            .zip(self.names(prefix))
            .map(|(ty, name)| format!("{ty} {name}"))
            .collect())
    }

    fn expand(&self, prefix: &str) -> Result<Vec<String>, EmitError> {
        if self.wide {
            return Ok(self
                .whole_shell()?
                .ordered_keys()
                .iter()
                .map(|key| format!("{prefix}0.{}()", java_semantic_member(key)))
                .collect());
        }
        let mut slots = Vec::new();
        let mut index = 0;
        for source in self.stages.iter().flat_map(|stage| stage.source_params()) {
            match source.adapter() {
                CallableSourceParamAdapter::UnitValue => continue,
                CallableSourceParamAdapter::Identity => slots.push(format!("{prefix}{index}")),
                CallableSourceParamAdapter::RightNest => {
                    let shell = source
                        .product_shell()
                        .expect("prepared product parameter shell");
                    slots.extend(render_internal_product_slots(
                        &format!("{prefix}{index}"),
                        shell.ordered_keys().len(),
                    )?);
                }
            }
            index += 1;
        }
        Ok(slots)
    }

    fn pack(&self, types: &[String], slots: &[String]) -> Result<Vec<String>, EmitError> {
        if self.wide {
            return Ok(vec![render_java_product_shell_value(
                self.whole_shell()?,
                types,
                slots,
            )?]);
        }
        let mut args = Vec::new();
        let mut offset = 0;
        for stage in &self.stages {
            for source in stage.source_params() {
                let range = source.facade_slots();
                let range = offset + range.start..offset + range.end;
                match source.adapter() {
                    CallableSourceParamAdapter::UnitValue => {}
                    CallableSourceParamAdapter::Identity => args.push(slots[range.start].clone()),
                    CallableSourceParamAdapter::RightNest => {
                        args.push(render_internal_product(&slots[range])?)
                    }
                }
            }
            offset += stage.facade_slot_count();
        }
        assert_eq!(
            offset,
            slots.len(),
            "Java presentation covers all semantic slots"
        );
        Ok(args)
    }

    fn whole_shell(&self) -> Result<&'a FacadeShellId, EmitError> {
        self.shell
            .ok_or_else(|| EmitError::internal("wide Java callable has no prepared shell"))
    }
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
    Ok(JavaPreparedCallable {
        method_type_parameters,
        param_javas,
        param_uses,
        ret_java,
        ret_use,
    })
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

fn java_value_plan<'a>(
    site: PreparedBoundaryCallableSite<'a>,
    plan: &'a BoundaryFacadePlan,
    shapes: &JavaShapes<'_>,
) -> Result<&'a ValuePlan, EmitError> {
    if std::ptr::eq(plan, site.plan().facade()) {
        return Ok(site.plan().value_plan());
    }
    let name = shapes
        .newtype_payload_plan_owner(plan)
        .ok_or_else(|| EmitError::internal("Java value use has no exact prepared plan owner"))?;
    match site.nominals().declaration(name) {
        Some(BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        }) => Ok(payload.value_plan()),
        _ => Err(EmitError::internal(
            "Java value use omitted its prepared nominal payload",
        )),
    }
}

fn render_prepared_use(
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    nominals: &BoundaryNominalDependencies,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    let values = java_value_plan(site, plan, shapes)?;
    render_java_value_use(values, Some(nominals), values.source(id), binders, shapes)
}

fn java_value_stage_shape(
    values: &ValuePlan,
    root: ValueUseId,
    nominals: Option<&BoundaryNominalDependencies>,
    shapes: &JavaShapes<'_>,
) -> Result<ValueStageShape, EmitError> {
    values.stage_shape(root, |name| {
        if let Some(nominals) = nominals {
            nominals
                .declaration(name)
                .map(BoundaryNominalDeclaration::kind)
        } else {
            crate::backends::value_plan::nominal_declaration(shapes.package(), name)
                .map(|declaration| declaration.kind())
        }
        .ok_or_else(|| EmitError::internal("a Java stage capture omitted its exact nominal kind"))
    })
}

fn java_value_forall_name(shape: &ValuePlan, identity: &str) -> String {
    budgeted_mint_name(shape.readable_stage_symbol(), |_| {
        Ok::<_, std::convert::Infallible>(format!("KioForall_H{}", fnv1a_64_hex(identity)))
    })
    .unwrap_or_else(|never| match never {})
}

fn render_java_value_use(
    values: &ValuePlan,
    nominals: Option<&BoundaryNominalDependencies>,
    id: ValueUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    enum Task {
        Enter(ValueUseId),
        Finish(ValueUseId, usize),
        Application(usize),
        Stage(String, usize),
    }
    let mut pending = vec![Task::Enter(id)];
    let mut rendered = Vec::<String>::new();
    while let Some(task) = pending.pop() {
        match task {
            Task::Enter(id) => {
                if matches!(values.use_at(id), ValueUse::Forall { .. }) {
                    let shape = java_value_stage_shape(values, id, nominals, shapes)?;
                    let identity = shape
                        .plan
                        .identity(shape.plan.root(), shape.plan.ambient_binders());
                    pending.push(Task::Stage(
                        java_value_forall_name(&shape.plan, &identity),
                        shape.captures.len(),
                    ));
                    for capture in shape.captures.into_iter().rev() {
                        match capture {
                            ValueCapture::Use(child) => pending.push(Task::Enter(child)),
                            ValueCapture::ApplicationPrefix { constructor, args } => {
                                pending.push(Task::Application(args.len()));
                                pending.extend(args.into_iter().rev().map(Task::Enter));
                                pending.push(Task::Enter(constructor));
                            }
                        }
                    }
                    continue;
                }
                let children = match values.use_at(id) {
                    ValueUse::Apply { constructor, args } => std::iter::once(*constructor)
                        .chain(args.iter().copied())
                        .collect::<Vec<_>>(),
                    ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                        vec![*left, *right]
                    }
                    ValueUse::Function { domain, result, .. } => vec![*domain, *result],
                    _ => Vec::new(),
                };
                pending.push(Task::Finish(id, children.len()));
                pending.extend(children.into_iter().rev().map(Task::Enter));
            }
            Task::Finish(id, count) => {
                let children = rendered.split_off(rendered.len() - count);
                rendered.push(match values.use_at(id) {
                    ValueUse::Unit => "Shapes.Unit".to_owned(),
                    ValueUse::Bottom => "java.lang.Void".to_owned(),
                    ValueUse::Bound(binder) => binders.get(binder).cloned().ok_or_else(|| {
                        EmitError::internal("Java value plan refers to an unbound type parameter")
                    })?,
                    ValueUse::Nominal(name) => render_prepared_nominal(
                        name,
                        &[],
                        nominals.ok_or_else(|| {
                            EmitError::internal(
                                "a shaped Java stage must capture independent nominal leaves",
                            )
                        })?,
                        shapes,
                    )?,
                    ValueUse::Apply { .. } => render_java_application(&children[0], &children[1..]),
                    ValueUse::Product { .. } | ValueUse::Sum { .. } | ValueUse::Function { .. } => {
                        let carrier = match values.use_at(id) {
                            ValueUse::Product { .. } => "Product",
                            ValueUse::Sum { .. } => "Sum",
                            _ => "Fn1",
                        };
                        format!(
                            "Shapes.{carrier}<{}, {}>",
                            boxed_java_type(&children[0]),
                            boxed_java_type(&children[1])
                        )
                    }
                    ValueUse::Forall { .. } => unreachable!("type stages use their shared shape"),
                });
            }
            Task::Application(count) => {
                let children = rendered.split_off(rendered.len() - count - 1);
                rendered.push(render_java_application(&children[0], &children[1..]));
            }
            Task::Stage(name, count) => {
                let arguments = rendered.split_off(rendered.len() - count);
                rendered.push(format!("Shapes.{name}{}", java_generic_use(&arguments)));
            }
        }
    }
    assert_eq!(rendered.len(), 1, "one Java value root is rendered");
    Ok(rendered.pop().expect("one rendered Java type"))
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
            if !type_params.is_empty() {
                let constructor =
                    render_nominal_constructor_marker_use(name, NominalKind::Host, shapes);
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
            if !type_params.is_empty() {
                let constructor =
                    render_nominal_constructor_marker_use(name, NominalKind::Newtype, shapes);
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
    nominal_name(name, NominalKind::Newtype)
}

fn java_nominal_constructor_marker(name: &QualifiedTypeName, kind: NominalKind) -> String {
    nominal_name(name, kind)
}

fn render_nominal_constructor_marker_use(
    name: &QualifiedTypeName,
    kind: NominalKind,
    shapes: &JavaShapes<'_>,
) -> String {
    let parameters = match kind {
        NominalKind::Host => Vec::new(),
        NominalKind::Newtype => shapes.live_root_type_parameters().to_vec(),
    };
    format!(
        "Shapes.{}{}",
        java_nominal_constructor_marker(name, kind),
        java_generic_use(&parameters)
    )
}

fn render_java_application(constructor: &str, args: &[String]) -> String {
    args.iter().fold(boxed_java_type(constructor), |head, arg| {
        format!("Shapes.Apply1<{head}, {}>", boxed_java_type(arg))
    })
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

fn convert_prepared_use(
    site: PreparedBoundaryCallableSite<'_>,
    id: FacadeUseId,
    binders: &BTreeMap<FacadeBinderId, String>,
    expression: &str,
    direction: JavaFfiDirection,
    shapes: &JavaShapes<'_>,
) -> Result<String, EmitError> {
    match direction {
        JavaFfiDirection::In => Ok(expression.to_owned()),
        JavaFfiDirection::Out => {
            let ty = render_prepared_use(
                site,
                site.plan().facade(),
                site.nominals(),
                id,
                binders,
                shapes,
            )?;
            Ok(format!("(({ty}) {expression})"))
        }
    }
}

fn pack_prepared_source_arguments(
    layout: Option<&CallableValueStageLayout>,
    converted: &[String],
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
                CallableSourceParamAdapter::UnitValue => "new Shapes.Unit()".to_owned(),
                CallableSourceParamAdapter::Identity => values
                    .first()
                    .cloned()
                    .ok_or_else(|| EmitError::internal("empty Java identity source range"))?,
                CallableSourceParamAdapter::RightNest => {
                    let shell = source.product_shell().ok_or_else(|| {
                        EmitError::internal("Java RightNest source has no prepared product shell")
                    })?;
                    if shell.ordered_keys().len() != values.len() {
                        return Err(EmitError::internal("Java source product arity drift"));
                    }
                    render_internal_product(values)?
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
    _internal_body: bool,
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
                expanded.extend(render_internal_product_slots(
                    raw,
                    shell.ordered_keys().len(),
                )?);
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
        format!("new Shapes.Product<>({value}, {tail})")
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
            slots.push(format!("((Shapes.Product<?, ?>) {current}).left()"));
            current = format!("((Shapes.Product<?, ?>) {current}).right()");
        }
    }
    Ok(slots)
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
            });
    }
    Ok(root)
}

// =========================================================================
// Exact host-role operations (literal construction and condition inspection).
// =========================================================================

/// A lexical role value supplied to its exact declaration-owned adapter.
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

/// A declaration-owned role adapter's result in the interpreter's scalar form.
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
    validate_facade_class_names(handle, tree)?;
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
        "  private static java.math.BigInteger __big(Object value) {\n    return value instanceof java.math.BigInteger big\n        ? big\n        : java.math.BigInteger.valueOf(((Number) value).longValue());\n  }\n\n",
    );

    out.push_str("}\n");
    Ok(out)
}

/// Deterministic Java class name for one role-bearing facade path.
fn namespace_class_name(path: &[FacadeSelector]) -> String {
    facade_class_name(path, "Ns")
}

fn validate_facade_class_names(handle: &str, tree: &ExportNode) -> Result<(), EmitError> {
    let mut names = BTreeMap::new();
    let mut pending = vec![(tree, Vec::new())];
    while let Some((node, path)) = pending.pop() {
        for (source, child) in &node.children {
            let mut child_path = path.clone();
            child_path.push(FacadeSelector::Module(source.clone()));
            claim_facade_class_name(&mut names, handle, &child_path, "Ns")?;
            pending.push((child, child_path));
        }
        for newtype in &node.newtypes {
            let mut type_path = path.clone();
            type_path.push(FacadeSelector::Type(newtype.type_name.clone()));
            claim_facade_class_name(&mut names, handle, &type_path, "NtH")?;
        }
    }
    Ok(())
}

fn claim_facade_class_name(
    names: &mut BTreeMap<String, (String, Vec<FacadeSelector>)>,
    handle: &str,
    path: &[FacadeSelector],
    role: &str,
) -> Result<(), EmitError> {
    let class = facade_class_name(path, role);
    let key = format!("{handle}${class}.class").to_ascii_lowercase();
    let identity = (role.to_owned(), path.to_vec());
    if let Some(existing) = names.insert(key.clone(), identity.clone())
        && existing != identity
    {
        return Err(EmitError::internal(format!(
            "distinct Java facade identities collide at classfile `{key}`: {existing:?} and {identity:?}"
        )));
    }
    Ok(())
}

fn java_facade_selector_name(selector: &FacadeSelector, root: bool) -> String {
    if root
        && matches!(selector, FacadeSelector::Module(_))
        && shadows_support_package(&host_name_core(selector.source()))
    {
        return selector.facade_name(false);
    }
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
        let parameters = JavaParameters::head(site);
        let params = parameters.declarations(&f.param_javas, "p")?;
        let flat_parameters = parameters.expand("p")?;
        let converted: Vec<String> = f
            .param_uses
            .iter()
            .zip(flat_parameters)
            .map(|(use_id, parameter)| {
                convert_prepared_use(
                    site,
                    *use_id,
                    &binders,
                    &parameter,
                    JavaFfiDirection::In,
                    shapes,
                )
            })
            .collect::<Result<_, _>>()?;
        let args = pack_prepared_callable_arguments(site, &converted)?;
        let call = format!("__fn_{method}.call({})", args.join(", "));
        out.push('\n');
        push_doc(
            out,
            crate::backends::boundary_docs::callable(shapes.package(), site),
            None,
            "    ",
        );
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
    facade_class_name(&full_path, "NtH")
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
        render_newtype_member_method(out, ctor, "__constructor", prepared, shapes)?;
    }
    if let Some(projector) = &nt.projector {
        render_newtype_member_method(out, projector, "__projector", prepared, shapes)?;
    }
    out.push_str("  }\n\n");
    Ok(())
}

fn render_newtype_member_method(
    out: &mut String,
    member: &ExportNewtypeMember,
    runtime_field: &str,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &mut JavaShapes<'_>,
) -> Result<(), EmitError> {
    let site = prepared_site(prepared, &member.site)?;
    let callable = render_prepared_callable(site, shapes)?;
    let binders = prepared_callable_binders(site);
    let method = java_public_item_name(&member.leaf);
    let generics = java_generic_declaration(&callable.method_type_parameters);
    let prefix = if generics.is_empty() {
        String::new()
    } else {
        format!("{generics} ")
    };
    let head = JavaParameters::head(site);
    let params = head.declarations(&callable.param_javas, "p")?;
    let raw_args = pack_prepared_callable_arguments(site, &head.expand("p")?)?;
    let call = format!("{runtime_field}.call({})", raw_args.join(", "));
    if callable.ret_java == "void" {
        out.push_str(&format!(
            "\n    public {prefix}void {method}({}) {{\n      {call};\n    }}\n",
            params.join(", ")
        ));
    } else {
        let returned = convert_prepared_use(
            site,
            callable.ret_use,
            &binders,
            &call,
            JavaFfiDirection::Out,
            shapes,
        )?;
        out.push_str(&format!(
            "\n    public {prefix}{ret} {method}({}) {{\n      return {returned};\n    }}\n",
            params.join(", "),
            ret = callable.ret_java
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
                "  /** Host type `{qualified}` was removed at contract v{removed_at}. */\n  @java.lang.Deprecated\n  default {method_prefix}{public_type} {from_body}({body_type} value) {{\n    throw new IllegalStateException(\n        \"host type {qualified} was removed at contract v{removed_at}\");\n  }}\n\n  @java.lang.Deprecated\n  default {method_prefix}{body_type} {to_body}({public_type} value) {{\n    throw new IllegalStateException(\n        \"host type {qualified} was removed at contract v{removed_at}\");\n  }}\n"
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
        let decls = JavaParameters::head(site).declarations(&callable.param_javas, "p")?;
        let member = host_member_name(&module_path, name);
        push_doc(
            &mut out,
            crate::backends::boundary_docs::callable(shapes.package(), site),
            None,
            "  ",
        );
        match site.origin() {
            PreparedBoundaryCallableOriginRef::Live(_) => out.push_str(&format!(
                "  {method_prefix}{ret} {member}({});\n",
                decls.join(", ")
            )),
            PreparedBoundaryCallableOriginRef::Retained(metadata) => {
                let removed_at = metadata.removed_at_version();
                out.push_str(&format!(
                    "\n  /** Removed at contract v{removed_at}; the package no longer calls it. */\n  @java.lang.Deprecated\n  default {method_prefix}{ret} {member}({}) {{\n    throw new IllegalStateException(\n        \"host fn {module_path}.{name} was removed at contract v{removed_at}\");\n  }}\n",
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
    out.push_str("    KioRuntime.KioObject __fromRole = new KioRuntime.KioObject();\n    KioRuntime.KioObject __toRole = new KioRuntime.KioObject();\n    __r.set(\"__fromRole\", __fromRole);\n    __r.set(\"__toRole\", __toRole);\n");
    for binding in shapes
        .host_bindings()
        .filter(|binding| binding.retained_at.is_none())
    {
        let Some(role) = binding.role else { continue };
        assert_eq!(
            binding.type_arity, 0,
            "literal roles are nullary host types"
        );
        let role_type = role_to_java_type(role);
        let native_type = java_host_binding_public_type(binding, &[]);
        let key = escape_java(&java_nominal_runtime_key(&binding.name));
        let from = from_kio(role_type, "__a[0]");
        let to = to_kio(
            role_type,
            &format!(
                "__host.{}(({native_type}) __a[0])",
                binding.adapter_to_body()
            ),
        );
        out.push_str(&format!(
            "    __fromRole.set(\"{key}\", (KioRuntime.KioFn) __a -> __host.{adapter}({from}));\n    __toRole.set(\"{key}\", (KioRuntime.KioFn) __a -> {to});\n",
            adapter=binding.adapter_from_body(),
        ));
    }
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
            let args: Vec<String> = callable
                .param_uses
                .iter()
                .zip(boundary_args)
                .map(|(use_id, expression)| {
                    convert_prepared_use(
                        site,
                        *use_id,
                        &binders,
                        &expression,
                        JavaFfiDirection::Out,
                        shapes,
                    )
                })
                .collect::<Result<_, _>>()?;
            let parameter_types = callable
                .param_uses
                .iter()
                .map(|slot| {
                    render_prepared_use(
                        site,
                        site.plan().facade(),
                        site.nominals(),
                        *slot,
                        &binders,
                        shapes,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let args = JavaParameters::head(site).pack(&parameter_types, &args)?;
            let call = format!("__host.{member}({})", args.join(", "));
            let body = if ret == "void" {
                format!("{{ {call}; return new Shapes.Unit(); }}")
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
    body_plans: &[ValuePlan],
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio - do not edit by hand.\n");
    out.push_str(&format!("package {namespace};\n\n"));
    out.push_str(
        "/**\n * Boundary shape declarations: one nominal Java type per distinct\n * FFI shape (see `specs/backends/java.md` § FFI surface). Names derive\n * from exact semantic keys and declaration identities.\n */\n",
    );
    out.push_str("public final class Shapes {\n  private Shapes() {}\n");
    let mut wide_shells = BTreeMap::new();
    for site in prepared.sites() {
        let parameters = JavaParameters::head(site);
        if parameters.wide {
            insert_java_support_origin(
                &mut wide_shells,
                parameters.whole_shell()?.clone(),
                java_site_support_origin(site),
            );
        }
    }
    validate_facade_shell_names(wide_shells.keys())?;
    for (shell, origin) in wide_shells {
        render_facade_shell_java(&mut out, &shell, origin);
    }
    render_canonical_value_support(&mut out);
    render_prepared_forall_interfaces(&mut out, prepared, shapes, body_plans)?;
    render_body_stage_factories(&mut out, body_plans, shapes)?;
    let mut nominals = JavaNominalTree::default();
    render_nominal_constructor_markers(&mut nominals, prepared, shapes)?;
    render_parameterized_host_carriers(&mut nominals, prepared, shapes)?;
    render_prepared_newtype_carriers(&mut out, &mut nominals, prepared, shapes)?;
    nominals.render(&mut out, 1);
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
            "KioFacade_{kind}_Positional_K{}",
            shell.ordered_keys().len()
        );
    }
    let readable = shell.encode_public();
    budgeted_mint_name(readable, |identity| {
        let kind = match shell.kind() {
            FacadeKind::Product => "Product",
            FacadeKind::Sum => "Sum",
        };
        Ok::<_, std::convert::Infallible>(format!("KioFacade_{kind}_H{}", fnv1a_64_hex(identity)))
    })
    .unwrap_or_else(|never| match never {})
}

fn validate_facade_shell_names<'a>(
    shells: impl Iterator<Item = &'a FacadeShellId>,
) -> Result<(), EmitError> {
    let mut owners = BTreeMap::<String, &FacadeShellId>::new();
    for shell in shells {
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
    body_plans: &[ValuePlan],
) -> Result<(), EmitError> {
    let mut interfaces = BTreeMap::<String, (String, String, BoundaryFacadeSupportOrigin)>::new();
    for site in prepared.sites() {
        for stage in site.plan().entry().head_stages {
            match stage {
                BoundaryCallableHeadStage::Type { .. } => {}
                BoundaryCallableHeadStage::Value { slots } => {
                    for slot in slots {
                        collect_java_forall_interfaces(
                            &mut interfaces,
                            site,
                            site.plan().facade(),
                            *slot,
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
        collect_java_forall_interfaces(
            &mut interfaces,
            site,
            payload.facade(),
            payload.payload_root(),
            origin,
            shapes,
        )?;
    }

    for values in body_plans {
        collect_java_value_forall_interfaces(
            &mut interfaces,
            values,
            None,
            values.root(),
            BoundaryFacadeSupportOrigin::Live,
            shapes,
        )?;
    }

    for (_, (_, declaration, origin)) in interfaces {
        out.push('\n');
        push_java_deprecation(out, origin, "  ");
        out.push_str(&declaration.replace(
            "__KIO_MEMBER_DEPRECATION__",
            java_deprecation_annotation(origin),
        ));
    }
    Ok(())
}

fn collect_java_forall_interfaces(
    interfaces: &mut BTreeMap<String, (String, String, BoundaryFacadeSupportOrigin)>,
    site: PreparedBoundaryCallableSite<'_>,
    plan: &BoundaryFacadePlan,
    id: FacadeUseId,
    origin: BoundaryFacadeSupportOrigin,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    let values = java_value_plan(site, plan, shapes)?;
    collect_java_value_forall_interfaces(
        interfaces,
        values,
        Some(site.nominals()),
        values.source(id),
        origin,
        shapes,
    )
}

fn collect_java_value_forall_interfaces(
    interfaces: &mut BTreeMap<String, (String, String, BoundaryFacadeSupportOrigin)>,
    values: &ValuePlan,
    nominals: Option<&BoundaryNominalDependencies>,
    root: ValueUseId,
    origin: BoundaryFacadeSupportOrigin,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    let mut seen = vec![false; values.uses().len()];
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if std::mem::replace(&mut seen[id.index()], true) {
            continue;
        }
        match values.use_at(id) {
            ValueUse::Unit | ValueUse::Bottom | ValueUse::Bound(_) | ValueUse::Nominal(_) => {}
            ValueUse::Apply { constructor, args } => {
                pending.push(*constructor);
                pending.extend(args.iter().copied());
            }
            ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                pending.extend([*left, *right]);
            }
            ValueUse::Function { domain, result, .. } => {
                pending.extend([*domain, *result]);
            }
            ValueUse::Forall { result, .. } => {
                let shape = java_value_stage_shape(values, id, nominals, shapes)?;
                let identity = shape
                    .plan
                    .identity(shape.plan.root(), shape.plan.ambient_binders());
                let name = java_value_forall_name(&shape.plan, &identity);
                let mut nested = shape
                    .plan
                    .ambient_binders()
                    .iter()
                    .enumerate()
                    .map(|(index, binder)| (*binder, format!("KioCaptured_{index}")))
                    .collect::<BTreeMap<_, _>>();
                let parameters = shape
                    .plan
                    .ambient_binders()
                    .iter()
                    .map(|binder| nested[binder].clone())
                    .collect::<Vec<_>>();
                let ValueUse::Forall {
                    binder,
                    result: shaped_result,
                } = shape.plan.use_at(shape.plan.root())
                else {
                    unreachable!("a stage shape preserves its leading type binder")
                };
                nested.insert(*binder, "KioType".to_owned());
                let ret =
                    render_java_value_use(&shape.plan, None, *shaped_result, &nested, shapes)?;
                let declaration = format!(
                    "  @FunctionalInterface\n  public interface {name}{} {{\n    __KIO_MEMBER_DEPRECATION__public <KioType> {ret} advance();\n  }}\n",
                    java_generic_declaration(&parameters),
                );
                register_java_forall_interface(interfaces, name, identity, declaration, origin)?;
                pending.push(*result);
            }
        }
    }
    Ok(())
}

fn register_java_forall_interface(
    interfaces: &mut BTreeMap<String, (String, String, BoundaryFacadeSupportOrigin)>,
    name: String,
    identity: String,
    declaration: String,
    origin: BoundaryFacadeSupportOrigin,
) -> Result<(), EmitError> {
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
        interfaces.insert(name, (identity, declaration, origin));
    }
    Ok(())
}

fn render_body_stage_factories(
    out: &mut String,
    plans: &[ValuePlan],
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    let mut factories = BTreeMap::<String, (String, String)>::new();
    let mut registrations = Vec::new();
    for (plan_index, plan) in plans.iter().enumerate() {
        for (node_index, node) in plan.uses().iter().enumerate() {
            let ValueUse::Forall { .. } = node else {
                continue;
            };
            let id = plan.use_id(node_index);
            let shape = java_value_stage_shape(plan, id, None, shapes)?;
            let identity = shape
                .plan
                .identity(shape.plan.root(), shape.plan.ambient_binders());
            let name = java_value_forall_name(&shape.plan, &identity);
            let mut binders = shape
                .plan
                .ambient_binders()
                .iter()
                .map(|binder| (*binder, "Object".to_owned()))
                .collect::<BTreeMap<_, _>>();
            let ValueUse::Forall { binder, result } = shape.plan.use_at(shape.plan.root()) else {
                unreachable!("a factored Java stage retains its leading binder")
            };
            binders.insert(*binder, "KioType".to_owned());
            let result = render_java_value_use(&shape.plan, None, *result, &binders, shapes)?;
            let parameters = vec!["Object".to_owned(); shape.captures.len()];
            let carrier = java_applied_type(&name, &parameters);
            // The shared binder-dependent skeleton fixes the actual interface.
            // Its independent captures are erased only inside this private factory.
            let factory = format!(
                "  private static final class Factory_{name} implements KioRuntime.StageFactory {{\n\
                 \x20   static final KioRuntime.StageFactory INSTANCE = new Factory_{name}();\n\
                 \x20   public Object create(java.util.function.Supplier<Object> body) {{\n\
                 \x20     return new {carrier}() {{\n\
                 \x20       @SuppressWarnings(\"unchecked\")\n\
                 \x20       public <KioType> {result} advance() {{ return ({result}) body.get(); }}\n\
                 \x20     }};\n\
                 \x20   }}\n\
                 \x20   @SuppressWarnings(\"unchecked\")\n\
                 \x20   public Object advance(Object value) {{ return (({carrier}) value).<Object>advance(); }}\n\
                 \x20 }}\n"
            );
            if let Some((previous, declaration)) = factories.get(&name) {
                if previous != &identity || declaration != &factory {
                    return Err(EmitError::internal(format!(
                        "distinct Java stage factories collide at `{name}`"
                    )));
                }
            } else {
                factories.insert(name.clone(), (identity, factory));
            }
            registrations.push(format!(
                "    stages.put(KioRuntime.stageKey({plan_index}, {node_index}), Factory_{name}.INSTANCE);\n"
            ));
        }
    }
    for (_, declaration) in factories.values() {
        out.push_str(declaration);
    }
    out.push_str("  static java.util.Map<Long, KioRuntime.StageFactory> stageFactories() {\n    var stages = new java.util.HashMap<Long, KioRuntime.StageFactory>();\n");
    for index in 0..registrations.len().div_ceil(128) {
        out.push_str(&format!("    registerStages{index}(stages);\n"));
    }
    out.push_str("    return java.util.Map.copyOf(stages);\n  }\n");
    for (index, chunk) in registrations.chunks(128).enumerate() {
        out.push_str(&format!(
            "  private static void registerStages{index}(java.util.Map<Long, KioRuntime.StageFactory> stages) {{\n"
        ));
        for line in chunk {
            out.push_str(line);
        }
        out.push_str("  }\n");
    }
    Ok(())
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
                    "    @java.lang.Deprecated public {name}({}) {{\n      {}\n    }}\n",
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
        FacadeKind::Sum => unreachable!("only named wide products require a flat Java shell"),
    }
}

fn render_canonical_value_support(out: &mut String) {
    out.push_str(
        "  /** The canonical unit value. */\n  public record Unit() {}\n\n\
         \x20 public record Product<L, R>(L left, R right) {}\n\n\
         \x20 public sealed interface Sum<L, R> permits Left, Right {}\n\
         \x20 public record Left<L, R>(L value) implements Sum<L, R> {}\n\
         \x20 public record Right<L, R>(R value) implements Sum<L, R> {}\n\n\
         \x20 @FunctionalInterface\n  public interface Fn1<A, R> { R apply(A value); }\n\n\
         \x20 /** A canonical application index; storage is owned by its exact declaration. */\n\
         \x20 public static final class Apply1<F, A> {\n\
         \x20   private final String identity;\n    private final Object payload;\n\
         \x20   private Apply1(String identity, Object payload) { this.identity = identity; this.payload = payload; }\n\
         \x20 }\n\n\
         \x20 private static Object applicationPayload(Apply1<?, ?> value, String identity) {\n\
         \x20   if (!value.identity.equals(identity)) throw new IllegalArgumentException(\"wrong nominal application identity\");\n\
         \x20   return value.payload;\n  }\n\n\
         \x20 static Object createAppliedNominal(String identity, Object payload) {\n\
         \x20   return new Apply1<>(identity, payload);\n  }\n\n\
         \x20 static Object projectAppliedNominal(String identity, Object value) {\n\
         \x20   return applicationPayload((Apply1<?, ?>) value, identity);\n  }\n"
    );
}

fn java_binary_product_type(types: &[String]) -> Result<String, EmitError> {
    let Some((last, prefix)) = types.split_last() else {
        return Err(EmitError::internal(
            "a grouped Java product has at least one slot",
        ));
    };
    Ok(prefix
        .iter()
        .rev()
        .fold(boxed_java_type(last), |right, left| {
            format!("Shapes.Product<{}, {right}>", boxed_java_type(left))
        }))
}

fn java_nominal_runtime_key(name: &QualifiedTypeName) -> String {
    format!("{}\0{}", name.module_segments().join("/"), name.name())
}

#[derive(Default)]
struct JavaNominalTree {
    name: String,
    children: BTreeMap<String, JavaNominalTree>,
    declaration: Option<JavaNominalDeclaration>,
    origin: Option<BoundaryFacadeSupportOrigin>,
}

struct JavaNominalDeclaration {
    identity: (QualifiedTypeName, NominalKind),
    docs: String,
    header: String,
    body: String,
}

impl JavaNominalTree {
    fn insert(
        &mut self,
        path: &[String],
        declaration: JavaNominalDeclaration,
        origin: BoundaryFacadeSupportOrigin,
    ) -> Result<(), EmitError> {
        let mut node = self;
        for component in path {
            node = node
                .children
                .entry(component.to_ascii_lowercase())
                .or_insert_with(|| Self {
                    name: component.clone(),
                    ..Self::default()
                });
            if node.name != *component {
                return Err(EmitError::internal(format!(
                    "distinct Java nominal paths collide under case folding at Shapes.{}",
                    path.join("."),
                )));
            }
            node.origin = Some(
                node.origin
                    .map_or(origin, |current| join_java_support_origin(current, origin)),
            );
        }
        if let Some(existing) = &node.declaration {
            return Err(EmitError::internal(format!(
                "distinct Java nominal declarations collide at Shapes.{}: {:?} and {:?}",
                path.join("."),
                existing.identity,
                declaration.identity,
            )));
        }
        node.declaration = Some(declaration);
        Ok(())
    }

    fn render(&self, out: &mut String, depth: usize) {
        let indent = "  ".repeat(depth);
        for node in self.children.values() {
            let leaf = &node.name;
            if let Some(declaration) = &node.declaration {
                for line in declaration.docs.lines() {
                    out.push_str(&format!("{indent}{line}\n"));
                }
                out.push_str(&format!("{indent}{} {{\n", declaration.header));
                for line in declaration.body.lines() {
                    out.push_str(&format!("{indent}  {line}\n"));
                }
            } else {
                push_java_deprecation(
                    out,
                    node.origin
                        .expect("a nominal holder has descendant provenance"),
                    &indent,
                );
                out.push_str(&format!(
                    "{indent}public static final class {leaf} {{\n{indent}  private {leaf}() {{}}\n"
                ));
            }
            node.render(out, depth + 1);
            out.push_str(&format!("{indent}}}\n"));
        }
    }
}

fn render_nominal_constructor_marker(
    tree: &mut JavaNominalTree,
    name: &QualifiedTypeName,
    parameters: &[BoundaryNominalTypeParam],
    kind: NominalKind,
    marker_parameters: &[String],
    origin: BoundaryFacadeSupportOrigin,
    doc: Option<&crate::ast::DocComment>,
) -> Result<(), EmitError> {
    if parameters.is_empty() {
        return Ok(());
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
    let note = format!(
        "Exact constructor marker for `{qualified}` (arity {}; kinds: {kind_list}).",
        kinds.len()
    );
    let mut docs = String::new();
    push_nominal_doc(&mut docs, doc, &note, origin, "");
    let path = nominal_path(name, kind);
    tree.insert(
        &path,
        JavaNominalDeclaration {
            identity: (name.clone(), kind),
            docs,
            header: format!(
                "public interface {}{}",
                path.last().unwrap(),
                java_generic_declaration(marker_parameters)
            ),
            body: String::new(),
        },
        origin,
    )
}

fn render_nominal_constructor_markers(
    tree: &mut JavaNominalTree,
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
            tree,
            binding.name(),
            binding.type_params(),
            NominalKind::Host,
            &[],
            origin,
            crate::backends::boundary_docs::host_type(shapes.package(), binding),
        )?;
    }
    let root_parameters = shapes.live_root_type_parameters().to_vec();
    for entry in prepared.public_newtypes() {
        render_nominal_constructor_marker(
            tree,
            entry.name(),
            entry.type_params(),
            NominalKind::Newtype,
            &root_parameters,
            BoundaryFacadeSupportOrigin::Live,
            crate::backends::boundary_docs::newtype(shapes.package(), entry),
        )?;
    }
    for entry in prepared.retained_public_newtypes() {
        let removed_at_version = prepared
            .retained_public_newtype_removed_at(entry.name())
            .unwrap_or_else(|| {
                unreachable!("a retained Java public newtype has removal provenance")
            });
        render_nominal_constructor_marker(
            tree,
            entry.name(),
            entry.type_params(),
            NominalKind::Newtype,
            &root_parameters,
            BoundaryFacadeSupportOrigin::Retained { removed_at_version },
            None,
        )?;
    }
    Ok(())
}

fn render_parameterized_host_carriers(
    tree: &mut JavaNominalTree,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    for binding in shapes
        .host_bindings()
        .filter(|binding| binding.type_arity > 0 || binding.retained_at.is_some())
    {
        let parameters = java_host_binding_parameters(binding);
        let origin = binding
            .retained_at
            .map_or(BoundaryFacadeSupportOrigin::Live, |removed_at_version| {
                BoundaryFacadeSupportOrigin::Retained { removed_at_version }
            });
        let doc = prepared.host_binding(&binding.name).and_then(|binding| {
            crate::backends::boundary_docs::host_type(shapes.package(), binding)
        });
        let mut docs = String::new();
        push_nominal_doc(
            &mut docs,
            doc,
            "Storage operations for this exact host declaration.",
            origin,
            "",
        );
        let mut path = nominal_path(&binding.name, NominalKind::Host);
        if parameters.is_empty() {
            tree.insert(
                &path,
                JavaNominalDeclaration {
                    identity: (binding.name.clone(), NominalKind::Host),
                    docs,
                    header: format!("public interface {}", path.last().unwrap()),
                    body: String::new(),
                },
                origin,
            )?;
            continue;
        }
        let marker =
            render_nominal_constructor_marker_use(&binding.name, NominalKind::Host, shapes);
        let ty = render_java_application(&marker, &parameters);
        let generics = java_generic_declaration(&parameters);
        let key = escape_java(&java_nominal_runtime_key(&binding.name));
        let attribute = java_deprecation_annotation(origin);
        let carrier = storage_name(&binding.name);
        path.push(carrier.to_owned());
        tree.insert(&path, JavaNominalDeclaration {
            identity: (binding.name.clone(), NominalKind::Host),
            docs,
            header: format!("public static final class {carrier}"),
            body: format!(
                "private {carrier}() {{}}\n\
                 {attribute}public static {generics} {ty} fromNative(java.lang.Object value) {{ return new Shapes.Apply1<>(\"{key}\", value); }}\n\
                 {attribute}public static {generics} java.lang.Object toNative({ty} value) {{ return Shapes.applicationPayload(value, \"{key}\"); }}\n"
            ),
        }, origin)?;
    }
    Ok(())
}

fn render_prepared_newtype_carriers(
    out: &mut String,
    tree: &mut JavaNominalTree,
    prepared: &PreparedBoundaryCallableSites,
    shapes: &JavaShapes<'_>,
) -> Result<(), EmitError> {
    let mut registrations = Vec::new();
    for (entry, origin) in prepared
        .public_newtypes()
        .map(|entry| (entry, BoundaryFacadeSupportOrigin::Live))
        .chain(prepared.retained_public_newtypes().map(|entry| {
            let removed_at_version = prepared
                .retained_public_newtype_removed_at(entry.name())
                .expect("retained newtype has removal provenance");
            (
                entry,
                BoundaryFacadeSupportOrigin::Retained { removed_at_version },
            )
        }))
    {
        let name = entry.name();
        let key = escape_java(&java_nominal_runtime_key(name));
        if !entry.type_params().is_empty() {
            registrations.push(format!(
                "    factories.put(\"{key}\", new KioRuntime.NominalFactory() {{\n\
                 \x20     public Object create(Object payload) {{ return new Apply1<>(\"{key}\", payload); }}\n\
                 \x20     public Object project(Object value) {{ return applicationPayload((Apply1<?, ?>) value, \"{key}\"); }}\n    }});\n"
            ));
            continue;
        }
        let carrier = java_newtype_carrier(name);
        let path = nominal_path(name, NominalKind::Newtype);
        let leaf = path.last().unwrap();
        let parameters = shapes.live_root_type_parameters();
        let declaration = java_generic_declaration(parameters);
        let inference = if parameters.is_empty() { "" } else { "<>" };
        let transparent = matches!(entry.surface(), BoundaryNewtypeSurface::Both { .. })
            && entry.existential_params().is_empty();
        let doc = matches!(origin, BoundaryFacadeSupportOrigin::Live)
            .then(|| crate::backends::boundary_docs::newtype(shapes.package(), entry))
            .flatten();
        let mut docs = String::new();
        push_doc(&mut docs, doc, None, "");
        push_java_deprecation(&mut docs, origin, "");
        let mut body = format!(
            "private final java.lang.Object payload;\n\
             private {leaf}(java.lang.Object payload, boolean internal) {{ this.payload = payload; }}\n"
        );
        if transparent {
            let (payload, site, _) = prepared_newtype_payload_owner(prepared, shapes, name)?
                .ok_or_else(|| {
                    EmitError::internal("transparent Java newtype has a prepared payload")
                })?;
            let payload_ty = render_prepared_use(
                site,
                payload.facade(),
                site.nominals(),
                payload.payload_root(),
                &BTreeMap::new(),
                shapes,
            )?;
            let attribute = java_deprecation_annotation(origin);
            body.push_str(&format!(
                "{attribute}public {leaf}({payload_ty} value) {{ this.payload = value; }}\n\
                 @java.lang.SuppressWarnings(\"unchecked\")\n{attribute}public {payload_ty} value() {{ return ({payload_ty}) payload; }}\n"
            ));
        }
        tree.insert(
            &path,
            JavaNominalDeclaration {
                identity: (name.clone(), NominalKind::Newtype),
                docs,
                header: format!("public static final class {leaf}{declaration}"),
                body,
            },
            origin,
        )?;
        let erased = java_applied_type(&carrier, &vec!["?".to_owned(); parameters.len()]);
        registrations.push(format!(
            "    factories.put(\"{key}\", new KioRuntime.NominalFactory() {{\n\
             \x20     public Object create(Object payload) {{ return new {carrier}{inference}(payload, true); }}\n\
             \x20     public Object project(Object value) {{ return (({erased}) value).payload; }}\n    }});\n"
        ));
    }
    out.push_str("  static java.util.Map<String, KioRuntime.NominalFactory> nominalFactories() {\n    var factories = new java.util.HashMap<String, KioRuntime.NominalFactory>();\n");
    for index in 0..registrations.len().div_ceil(128) {
        out.push_str(&format!("    registerNominals{index}(factories);\n"));
    }
    out.push_str("    return java.util.Map.copyOf(factories);\n  }\n");
    for (index, chunk) in registrations.chunks(128).enumerate() {
        out.push_str(&format!("  private static void registerNominals{index}(java.util.Map<String, KioRuntime.NominalFactory> factories) {{\n"));
        for registration in chunk {
            out.push_str(registration);
        }
        out.push_str("  }\n");
    }
    Ok(())
}

fn push_nominal_doc(
    out: &mut String,
    doc: Option<&crate::ast::DocComment>,
    note: &str,
    origin: BoundaryFacadeSupportOrigin,
    indent: &str,
) {
    if let Some(removed_at) = origin.removed_at_version() {
        let deprecation = java_deprecation_note(removed_at);
        // Javadoc attaches only the nearest comment to the declaration.
        out.push_str(&format!(
            "{indent}/** {note}\n{indent} * @deprecated {deprecation}\n{indent} */\n{indent}@java.lang.Deprecated\n"
        ));
    } else {
        push_doc(out, doc, Some(note), indent);
    }
}

fn push_doc(
    out: &mut String,
    doc: Option<&crate::ast::DocComment>,
    note: Option<&str>,
    indent: &str,
) {
    let Some(doc) = doc else {
        if let Some(note) = note {
            out.push_str(&format!("{indent}/** {note} */\n"));
        }
        return;
    };
    let mut body = crate::backends::doc::render(doc, crate::backends::doc::Dialect::JavadocHtml);
    if let Some(note) = note {
        body.push_str("\n\n");
        body.push_str(note);
    }
    for line in crate::backends::doc::javadoc(&body).lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
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
    use crate::backends::skin::READABLE_MINT_MAX_LEN;
    use crate::pass::parser::{parse, parse_package_file, parse_signature_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::structural_recovery;
    use std::path::{Path, PathBuf};

    fn java_forall_exact_identity(
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        root: FacadeUseId,
        shapes: &JavaShapes<'_>,
    ) -> Result<String, EmitError> {
        let values = java_value_plan(site, plan, shapes)?;
        let shape =
            java_value_stage_shape(values, values.source(root), Some(site.nominals()), shapes)?;
        Ok(shape
            .plan
            .identity(shape.plan.root(), shape.plan.ambient_binders()))
    }

    fn java_forall_interface_name(
        site: PreparedBoundaryCallableSite<'_>,
        plan: &BoundaryFacadePlan,
        root: FacadeUseId,
        shapes: &JavaShapes<'_>,
    ) -> Result<String, EmitError> {
        let values = java_value_plan(site, plan, shapes)?;
        let shape =
            java_value_stage_shape(values, values.source(root), Some(site.nominals()), shapes)?;
        let identity = shape
            .plan
            .identity(shape.plan.root(), shape.plan.ambient_binders());
        Ok(java_value_forall_name(&shape.plan, &identity))
    }

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
            format!("KioFacade_Product_H{}", fnv1a_64_hex(&exact_identity))
        );
        assert!(bounded.len() <= READABLE_MINT_MAX_LEN);
    }

    #[test]
    fn nested_sum_input_preserves_canonical_value_and_exact_indices() {
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
        assert_eq!(
            rendered, "value",
            "canonical input must not traverse either sum"
        );
        let native = render_prepared_use(
            site,
            site.plan().facade(),
            site.nominals(),
            outer,
            &BTreeMap::new(),
            &shapes,
        )
        .expect("canonical nested sum index");
        assert_eq!(native.matches("Shapes.Sum<").count(), 2, "{native}");
        assert_eq!(native.matches("Shapes.Product<").count(), 1, "{native}");
        assert!(
            native.contains("api.Red") && native.contains("api.Black"),
            "{native}"
        );
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
        crate::pass::route::route_package(&enriched)
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

    #[test]
    fn support_package_reservation_applies_to_module_fields_not_methods() {
        let module = FacadeSelector::Module("java".to_owned());
        assert_eq!(java_facade_selector_name(&module, true), "KioModule_java");
        assert_eq!(module.facade_name(true), "java");
        assert_eq!(java_facade_selector_name(&module, false), "KioModule_java");
        assert_eq!(java_public_item_name("java"), "java");
        let package = build_package(
            &["module java; pub fn java() -> . { () }"],
            "bridge { java; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("module and method java");
        assert!(emitted.handle_java.contains("Ns_M_java KioModule_java;"));
        assert!(emitted.handle_java.contains("__exports.get(\"java\")"));
        assert!(emitted.handle_java.contains("public void java()"));
        assert!(emitted.handle_java.contains("__fn_java.call("));
    }

    #[test]
    fn facade_class_registry_rejects_folded_full_identity_collisions() {
        let path = vec![FacadeSelector::Module("a_".repeat(80))];
        let class = facade_class_name(&path, "Ns");
        let key = format!("Pkg${class}.class").to_ascii_lowercase();
        let mut names = BTreeMap::from([(
            key,
            (
                "Ns".to_owned(),
                vec![FacadeSelector::Module("different".to_owned())],
            ),
        )]);
        assert!(claim_facade_class_name(&mut names, "Pkg", &path, "Ns").is_err());
    }

    #[test]
    fn nominal_tree_rejects_identity_collisions_and_omits_fallback_ancestors() {
        let owner = QualifiedTypeName::new(vec!["a".to_owned(); 256], "Token".to_owned())
            .expect("nominal identity");
        let path = nominal_path(&owner, NominalKind::Newtype);
        assert_eq!(path.len(), 1);
        let declaration = |name: QualifiedTypeName| JavaNominalDeclaration {
            identity: (name, NominalKind::Newtype),
            docs: String::new(),
            header: format!("public interface {}", path[0]),
            body: String::new(),
        };
        let mut tree = JavaNominalTree::default();
        tree.insert(&path, declaration(owner), BoundaryFacadeSupportOrigin::Live)
            .expect("first identity");
        let other = QualifiedTypeName::new(vec!["different".to_owned()], "Token".to_owned())
            .expect("other nominal identity");
        assert!(
            tree.insert(&path, declaration(other), BoundaryFacadeSupportOrigin::Live)
                .is_err()
        );
        let folded = path
            .iter()
            .map(|part| part.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let other = QualifiedTypeName::new(vec!["another".to_owned()], "Token".to_owned()).unwrap();
        assert!(
            tree.insert(
                &folded,
                declaration(other),
                BoundaryFacadeSupportOrigin::Live
            )
            .is_err()
        );
        let mut rendered = String::new();
        tree.render(&mut rendered, 1);
        assert_eq!(rendered.matches("public interface ").count(), 1);
        assert!(!rendered.contains("class a") && !rendered.contains("KioModule_"));
    }

    #[test]
    fn nominal_holder_provenance_joins_descendants_with_live_dominance() {
        let add = |tree: &mut JavaNominalTree, module: &[&str], leaf: &str, origin| {
            let name = QualifiedTypeName::new(
                module.iter().map(|part| (*part).to_owned()).collect(),
                leaf.to_owned(),
            )
            .unwrap();
            let path = nominal_path(&name, NominalKind::Host);
            tree.insert(
                &path,
                JavaNominalDeclaration {
                    identity: (name, NominalKind::Host),
                    docs: String::new(),
                    header: format!("public interface {leaf}"),
                    body: String::new(),
                },
                origin,
            )
            .unwrap();
        };
        for live_first in [false, true] {
            let mut tree = JavaNominalTree::default();
            if live_first {
                add(
                    &mut tree,
                    &["shared"],
                    "Current",
                    BoundaryFacadeSupportOrigin::Live,
                );
            }
            add(
                &mut tree,
                &["shared", "removed"],
                "Gone",
                BoundaryFacadeSupportOrigin::Retained {
                    removed_at_version: 2,
                },
            );
            if !live_first {
                let mut retained = String::new();
                tree.render(&mut retained, 1);
                assert!(retained.contains(
                    "@java.lang.Deprecated\n  public static final class KioModule_shared"
                ));
                add(
                    &mut tree,
                    &["shared"],
                    "Current",
                    BoundaryFacadeSupportOrigin::Live,
                );
            }
            let mut joined = String::new();
            tree.render(&mut joined, 1);
            assert!(joined.starts_with("  public static final class KioModule_shared"));
            assert!(joined.contains(
                "@java.lang.Deprecated\n    public static final class KioModule_removed"
            ));
        }
    }

    #[test]
    fn native_docs_follow_selected_public_declarations() {
        let package = build_package(
            &["module api;\n\
            /// HOST-GENERIC-DOC\n\
            host type Box[A];\n\
            /// HOST-FN-DOC\n\
            host fn effect() -> .;\n\
            /// OPAQUE-DOC\n\
            pub newtype Token : . { pub constructor make; projector read; };\n\
            /// EXPORT-DOC\n\
            pub fn call() -> . { effect() }\n\
            /// PRIVATE-DOC\n\
            fn hidden() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit documented Java");
        assert!(emitted.host_java.contains("HOST-FN-DOC"));
        assert!(emitted.handle_java.contains("EXPORT-DOC"));
        assert!(emitted.shapes_java.contains("HOST-GENERIC-DOC"));
        assert!(emitted.shapes_java.contains("OPAQUE-DOC"));
        for source in [
            &emitted.host_java,
            &emitted.handle_java,
            &emitted.shapes_java,
        ] {
            assert!(!source.contains("PRIVATE-DOC"));
        }
    }

    fn compound_input_conversion(return_type: &str, expression: &str) -> String {
        prepared_return_conversion(return_type, expression, JavaFfiDirection::In)
    }

    fn prepared_return_conversion(
        return_type: &str,
        expression: &str,
        direction: JavaFfiDirection,
    ) -> String {
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
            direction,
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
    fn compound_callback_input_does_not_invoke_or_wrap_the_producer() {
        let rendered = compound_input_conversion(". -> (Text & Text)", "callback");
        assert_eq!(rendered, "callback");
        let output =
            prepared_return_conversion(". -> (Text & Text)", "produce()", JavaFfiDirection::Out);
        assert!(
            output.contains("Shapes.Fn1<Shapes.Unit, Shapes.Product<"),
            "{output}"
        );
        assert_eq!(output.matches("produce()").count(), 1);
        assert!(!output.contains(".apply("), "{output}");
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

    #[test]
    fn function_transport_evaluates_the_producer_once_without_invoking_it() {
        for direction in [JavaFfiDirection::In, JavaFfiDirection::Out] {
            for ty in [". -> Text", "Text -> .", "Text -> Text"] {
                let rendered = prepared_return_conversion(ty, "produce()", direction);
                assert_eq!(rendered.matches("produce()").count(), 1, "{rendered}");
                assert!(
                    !rendered.contains(" -> ")
                        && !rendered.contains(".apply(")
                        && !rendered.contains(".call("),
                    "{rendered}"
                );
                match direction {
                    JavaFfiDirection::In => assert_eq!(rendered, "produce()"),
                    JavaFfiDirection::Out => {
                        assert!(rendered.starts_with("((Shapes.Fn1<"), "{rendered}")
                    }
                }
            }
        }
    }
    #[test]
    fn returned_functions_keep_both_value_stages_without_eager_adaptation() {
        let input =
            prepared_return_conversion("Text -> Text -> Text", "produce()", JavaFfiDirection::In);
        assert_eq!(input, "produce()");
        let output =
            prepared_return_conversion("Text -> Text -> Text", "produce()", JavaFfiDirection::Out);
        assert_eq!(output.matches("Shapes.Fn1<").count(), 2, "{output}");
        assert_eq!(output.matches("produce()").count(), 1, "{output}");
        assert!(
            !output.contains(" -> ") && !output.contains(".apply("),
            "{output}"
        );
    }
    fn polymorphic_payload_source(payload: &str) -> JavaPackage {
        let source = format!(
            "module api; pub newtype Dictionary[*F] : {payload} {{ \
             pub constructor make; pub projector read; }};"
        );
        let package = build_package(&[&source], "bridge { api; }");
        lower_package(&package, "pkg", None).expect("canonical polymorphic payload")
    }

    #[test]
    fn forall_factories_defer_body_execution_until_each_advance() {
        let emitted = polymorphic_payload_source("[A][B] ((A -> B) & F(A)) -> F(B)");
        let factories: Vec<_> = emitted
            .shapes_java
            .split("private static final class Factory_")
            .skip(1)
            .collect();
        assert!(
            !factories.is_empty(),
            "actual serialized body stages require factories"
        );
        for factory in factories {
            let create = factory
                .split_once("public Object create(")
                .expect("factory creates a stage")
                .1;
            let (construction, advance) =
                create.split_once(" advance()").expect("one generic stage");
            assert!(!construction.contains("body.get()"), "{construction}");
            let advance = advance.split_once("\n    }").expect("create method end").0;
            assert_eq!(advance.matches("body.get()").count(), 1, "{advance}");
            assert!(!advance.contains(".advance()"), "{advance}");
        }
        assert!(emitted.shapes_java.contains("KioRuntime.stageKey("));
    }

    #[test]
    fn forall_payload_stages_are_one_binder_methods_with_exact_residuals() {
        let emitted = polymorphic_payload_source("[A][B] ((A -> B) & F(A)) -> F(B)");
        let public_stages: Vec<_> = emitted
            .shapes_java
            .lines()
            .filter(|line| {
                line.trim_start().starts_with("public <KioType>") && line.ends_with("advance();")
            })
            .collect();
        assert!(
            public_stages
                .iter()
                .any(|line| line.contains("Shapes.KioForall_")),
            "{public_stages:?}"
        );
        assert!(
            public_stages
                .iter()
                .any(|line| line.contains("Shapes.Fn1<")),
            "{public_stages:?}"
        );
        assert!(
            public_stages.iter().all(|line| !line.contains("Object")),
            "{public_stages:?}"
        );
        assert!(!emitted.shapes_java.contains("KioPolyType_"));
    }

    #[test]
    fn forall_after_a_value_stage_stays_inside_its_function_result() {
        let emitted = polymorphic_payload_source("[A] (A -> A) -> [B] B -> B");
        assert!(
            emitted.shapes_java.lines().any(|line| line
                .contains("Shapes.Fn1<Shapes.Fn1<KioType, KioType>, KioCaptured_0>")
                && line.ends_with("advance();")),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains("Shapes.Fn1<KioType, KioType> advance();")
        );
        assert!(
            emitted.handle_java.contains("<Shapes.KioForall_"),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn boundary_forall_transport_does_not_advance_runtime_type_stages() {
        for direction in [JavaFfiDirection::In, JavaFfiDirection::Out] {
            let rendered =
                prepared_return_conversion("[A][B] (A & B) -> A", "produce()", direction);
            assert_eq!(rendered.matches("produce()").count(), 1, "{rendered}");
            assert!(
                !rendered.contains(".advance(") && !rendered.contains(" -> "),
                "{rendered}"
            );
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
    fn direct_polymorphic_newtype_members_preserve_canonical_payloads() {
        for rendered in direct_payload_member_conversions("", "[A][B] ((A -> B) & F(A)) -> F(B)") {
            assert!(!rendered.contains("KioRuntime.map("), "{rendered}");
            assert!(!rendered.contains("java.util.Map"), "{rendered}");
        }
    }

    #[test]
    fn existential_polymorphic_payload_preserves_canonical_continuation() {
        for rendered in
            direct_payload_member_conversions("<U>", "[A][B] ((U -> A) & (A -> B)) -> B")
        {
            assert!(!rendered.contains("KioRuntime.map("), "{rendered}");
            assert!(!rendered.contains("java.util.Map"), "{rendered}");
        }
    }

    #[test]
    fn polymorphic_payload_keeps_a_single_canonical_callback_domain() {
        let emitted = polymorphic_payload_source("[A][B] (((A & B) -> A) & F(A)) -> F(A)");
        assert!(
            emitted.shapes_java.contains("Shapes.Fn1<Shapes.Product<"),
            "{}",
            emitted.shapes_java
        );
        for rendered in
            direct_payload_member_conversions("", "[A][B] (((A & B) -> A) & F(A)) -> F(A)")
        {
            assert_eq!(rendered.matches("value").count(), 1, "{rendered}");
            assert!(
                !rendered.contains(" -> ") && !rendered.contains("KioRuntime.map("),
                "{rendered}"
            );
        }
    }
    #[test]
    fn ordinary_forall_and_monomorphic_members_share_canonical_callable_indices() {
        assert_eq!(
            compound_input_conversion("[A][B] (A & B) -> A", "value"),
            "value"
        );
        let converted = direct_payload_member_conversions("", "(F(.) & F(.)) -> F(.)");
        assert!(converted.iter().any(|value| value == "value"));
        assert!(
            converted
                .iter()
                .any(|value| value.starts_with("((Shapes.Fn1<Shapes.Product<")),
            "{converted:?}"
        );
        for rendered in converted {
            assert!(
                !rendered.contains("java.util.Map") && !rendered.contains(" -> "),
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
    fn recursive_nominal_transport_uses_the_exact_factory_without_a_type_walk() {
        let package = build_package(
            &[
                "module api; pub rec newtype Chain : . | Chain { pub constructor make; pub projector read; };",
            ],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("recursive nominal");
        assert!(
            emitted
                .shapes_java
                .contains("public Shapes.Sum<Shapes.Unit, Shapes.KioModule_api.Chain> value()"),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted
                .shapes_java
                .contains("return new KioModule_api.Chain(payload, true);")
        );
        assert!(
            emitted
                .shapes_java
                .contains("return ((KioModule_api.Chain) value).payload;")
        );
        assert!(!RUNTIME.contains("newtypeIsRecursive") && !RUNTIME.contains("substType"));
        assert!(!RUNTIME.contains("new LinkedHashSet<>(visited)"));
    }
    #[test]
    fn internal_applied_nominal_storage_does_not_add_public_capabilities() {
        let package = build_package(
            &[
                "module api; pub fn unit() -> . { () }",
                "module internal; newtype Hidden[A] : . { constructor make; projector read; };",
            ],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("internal applied nominal");
        assert!(emitted.shapes_java.contains(
            "\n  static Object createAppliedNominal(String identity, Object payload) {\n    return new Apply1<>(identity, payload);"
        ));
        assert!(emitted.shapes_java.contains(
            "\n  static Object projectAppliedNominal(String identity, Object value) {\n    return applicationPayload((Apply1<?, ?>) value, identity);"
        ));
        assert!(emitted.shapes_java.contains(
            "if (!value.identity.equals(identity)) throw new IllegalArgumentException(\"wrong nominal application identity\");"
        ));
        assert!(!emitted.shapes_java.contains("public interface Hidden"));
        assert!(
            !emitted
                .shapes_java
                .contains("public static final class Hidden")
        );
        assert_eq!(
            RUNTIME
                .matches("!asList(declaration.get(\"type_params\")).isEmpty()")
                .count(),
            2
        );
        assert!(RUNTIME.contains("return new PrivateNominal(key, payload);"));
        assert!(RUNTIME.contains("if (!nominal.identity().equals(key))"));
    }

    #[test]
    fn public_existential_newtype_transport_never_exposes_its_payload() {
        let package = build_package(
            &[
                "module api; pub newtype Pack <U> : U { pub constructor make; pub projector read; };",
            ],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("existential nominal");
        let carrier = emitted
            .shapes_java
            .split_once("public static final class Pack {")
            .unwrap()
            .1
            .split_once("\n    }")
            .unwrap()
            .0;
        assert!(
            carrier.contains("private final java.lang.Object payload;"),
            "{carrier}"
        );
        assert!(
            carrier.contains("private Pack(java.lang.Object payload, boolean internal)"),
            "{carrier}"
        );
        assert!(
            !carrier.contains("public ") && !carrier.contains(" value()"),
            "{carrier}"
        );
        assert!(emitted.handle_java.contains(" make(") && emitted.handle_java.contains(" read("));
    }
    #[test]
    fn exact_existential_newtype_factories_preserve_the_payload_without_conversion() {
        let package = build_package(
            &["module api; \
               pub newtype Pack <U> : U { pub constructor mk; pub projector get; }; \
               pub newtype Loaded <U> : U & U { pub constructor mk_loaded; pub projector get_loaded; };"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        for name in ["Pack", "Loaded"] {
            assert!(
                emitted
                    .shapes_java
                    .contains(&format!("return new KioModule_api.{name}(payload, true);")),
                "{}",
                emitted.shapes_java
            );
            assert!(
                emitted
                    .shapes_java
                    .contains(&format!("return ((KioModule_api.{name}) value).payload;")),
                "{}",
                emitted.shapes_java
            );
            assert!(!emitted.shapes_java.contains(&format!("public {name}(")));
        }
        assert!(!emitted.handle_java.contains("__newtype_body_"));
        assert!(
            emitted
                .handle_java
                .lines()
                .any(|line| line.contains(" getLoaded(") && line.contains("Shapes.KioForall_"))
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
            declaration.contains("public <KioCallType_0> Shapes.KioForall_")
                && declaration.contains(
                    " read(Shapes.Apply1<Shapes.KioModule_api.Packed, KioCallType_0> p0)"
                ),
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
                .contains("return ((java.lang.Void) __fn_keep.call(p0));")
                && emitted.handle_java.matches("((java.lang.Void)").count() >= 3,
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
                        && emitted.host_java.contains("Shapes.Fn1<Shapes.Product<")
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
            emitted.host_java.contains(
                "default Shapes.KioModule_api.Gone KioHostBinding_api__Gone_fromBody(Integer value)"
            ) && emitted.host_java.contains(
                "default Integer KioHostBinding_api__Gone_toBody(Shapes.KioModule_api.Gone value)"
            ) && emitted.host_java.contains(
                "default Shapes.KioModule_api.Gone api__archived(Shapes.KioModule_api.Gone p0)"
            ),
            "{}",
            emitted.host_java
        );
        assert!(
            emitted
                .shapes_java
                .contains("/** Storage operations for this exact host declaration.\n     * @deprecated Retained only for source compatibility after removal at contract v2.\n     */\n    @java.lang.Deprecated\n    public interface Gone {"),
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

        assert!(
            !emitted.shapes_java.contains("class Token<"),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.shapes_java.contains(
                "/** Exact constructor marker for `api.Token` (arity 1; kinds: *).\n     * @deprecated Retained only for source compatibility after removal at contract v2.\n     */\n    @java.lang.Deprecated\n    public interface Token {"
            ),
            "{}",
            emitted.shapes_java
        );
        assert!(
            emitted.host_java.contains(
                "api__archived(Shapes.Apply1<Shapes.KioModule_api.Token, Shapes.Unit> p0)"
            ),
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
        assert_eq!(
            emitted
                .shapes_java
                .matches("public interface Box {")
                .count(),
            1
        );
        let stages: Vec<_> = emitted
            .shapes_java
            .lines()
            .enumerate()
            .filter(|(_, line)| line.trim_start().starts_with("public interface KioForall_"))
            .collect();
        assert_eq!(
            stages.len(),
            1,
            "equal payload epochs share one stage: {}",
            emitted.shapes_java
        );
        let lines: Vec<_> = emitted.shapes_java.lines().collect();
        let (index, _) = stages[0];
        assert_eq!(lines[index - 2].trim(), "@java.lang.Deprecated");
        assert!(lines[index + 1].contains(
            "@java.lang.Deprecated public <KioType> Shapes.Fn1<KioType, KioCaptured_0> advance();"
        ));
        assert!(!emitted.shapes_java.contains("public record Box"));
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
                .contains("default Shapes.KioModule_api.Gone")
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
                        "@java.lang.Deprecated\n  public static final class {shell}<"
                    )) && emitted
                        .shapes_java
                        .contains("@java.lang.Deprecated public static <")
                        && emitted.shapes_java.contains(
                            "@java.lang.Deprecated\n    public static final class Builder"
                        )
                        && emitted
                            .shapes_java
                            .contains("@java.lang.Deprecated public T254 _254()")
                        && emitted
                            .shapes_java
                            .contains("@java.lang.Deprecated public Builder<")
                        && emitted
                            .shapes_java
                            .contains(&format!("@java.lang.Deprecated public {shell}<")),
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
            emitted.shapes_java.contains("public interface Fn1<")
                && !emitted.shapes_java.contains(
                    "@java.lang.Deprecated\n  @FunctionalInterface\n  public interface Fn1<"
                ),
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
        let leaf = carrier.rsplit('.').next().unwrap();
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public static final class {leaf}<T_api__Str>"))
                && emitted
                    .shapes_java
                    .contains(&format!("public {leaf}(T_api__Str value)"))
                && emitted.shapes_java.contains("public T_api__Str value()")
                && !emitted
                    .shapes_java
                    .contains(&format!("public {leaf}(Shapes.KioModule_api.I32 value)")),
            "retained payload replaced the live Java newtype carrier:\n{}",
            emitted.shapes_java
        );
    }

    #[test]
    fn source_parameters_group_native_heads_without_changing_semantic_slots() {
        let package = build_package(
            &["module api; host type A; host type B; host type C; \
               pub type Pair = A & B; \
               host fn split(x: A, y: B) -> A; \
               host fn grouped(pair: Pair) -> Pair; \
               host fn mixed(x: A, pair: B & C) -> A; \
               host fn generic[T](value: T) -> T; \
               host fn unit(value: .) -> .;"],
            "bridge { api; }",
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).expect("prepare");
        let shapes = JavaShapes::new(&package, &prepared);
        for (name, semantic, native) in [
            ("split", 2, 2),
            ("grouped", 2, 1),
            ("mixed", 3, 2),
            ("generic", 1, 1),
            ("unit", 0, 0),
        ] {
            let site = prepared.sites().find(|site| {
                matches!(site.site().owner(), BoundaryFacadeSiteOwner::HostFunction { name: leaf } if leaf == name)
            }).expect("host site");
            let callable = render_prepared_callable(site, &shapes).expect("render");
            let public = JavaParameters::head(site);
            assert_eq!(callable.param_uses.len(), semantic, "{name}");
            assert_eq!(
                public.types(&callable.param_javas).expect("types").len(),
                native,
                "{name}"
            );
            assert_eq!(
                public.expand("p").expect("expand").len(),
                semantic,
                "{name}"
            );
            let values = (0..semantic)
                .map(|index| format!("value{index}"))
                .collect::<Vec<_>>();
            assert_eq!(
                public
                    .pack(&callable.param_javas, &values)
                    .expect("pack")
                    .len(),
                native,
                "{name}"
            );
        }
    }

    #[test]
    fn grouped_callbacks_returned_stages_and_generic_unit_keep_their_cut() {
        let package = build_package(
            &["module api; host type A; host type B; \
               pub fn first(x: A, y: B) -> A { x } \
               pub fn second(pair: A & B) -> A & B { pair } \
               pub fn generic[T](value: T) -> T { value } \
               pub fn unit(value: .) -> . { value } \
               pub fn returned(x: A) -> B -> A { .(y: B) { x } } \
               pub fn callback(step: (A & B) -> A, pair: A & B) -> A { step(pair) }"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit grouped Java");
        for signature in [
            "first(T_api__A p0, T_api__B p1)",
            "second(Shapes.Product<T_api__A, T_api__B> p0)",
            "<KioCallType_0> KioCallType_0 generic(KioCallType_0 p0)",
            "void unit()",
            "Shapes.Fn1<T_api__B, T_api__A> returned(T_api__A p0)",
            "callback(Shapes.Fn1<Shapes.Product<T_api__A, T_api__B>, T_api__A> p0, Shapes.Product<T_api__A, T_api__B> p1)",
        ] {
            assert!(
                emitted.handle_java.contains(signature),
                "missing {signature}:\n{}",
                emitted.handle_java
            );
        }
    }

    #[test]
    fn retained_host_parameters_keep_grouping_without_execution() {
        let package = build_package(
            &["module api; host type A; host type B; pub fn unit() -> . { () }"],
            "bridge { api; }",
        );
        let emitted = lower_with_sig(
            &package,
            "signature pkg v(3); \
             v(1) { nonbreaking { add { module api { \
             host type A; host type B; host fn archived(pair: A & B) -> A; \
             pub fn unit() -> .; } } } } \
             v(2) { nonbreaking { remove { module api { archived; } } } }",
        );
        assert!(
            emitted
                .host_java
                .contains("default T_api__A api__archived(Shapes.Product<T_api__A, T_api__B> p0)"),
            "{}",
            emitted.host_java
        );
        assert!(
            !emitted.handle_java.contains(".set(\"archived\""),
            "{}",
            emitted.handle_java
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

        let rank_n = emitted
            .shapes_java
            .lines()
            .find_map(|line| {
                line.trim_start()
                    .strip_prefix("public interface KioForall_")
                    .and_then(|tail| tail.strip_suffix(" {"))
            })
            .expect("one canonical identity stage");
        let rank_n = format!("KioForall_{rank_n}");
        assert!(
            emitted
                .host_java
                .contains(&format!("T_main__Str main__applyPoly(Shapes.{rank_n} p0);")),
            "{}",
            emitted.host_java
        );
        assert!(
            emitted
                .shapes_java
                .contains("public <KioType> Shapes.Fn1<KioType, KioType> advance();")
        );
        assert!(!emitted.host_java.contains("Fn1<Object, Object>"));
        assert!(
            emitted.handle_java.contains(
                "public <KioCallType_0> KioCallType_0 ordered(Shapes.KioModule_main.T<T_main__Str> p0, KioCallType_0 p1)"
            ),
            "{}",
            emitted.handle_java
        );
    }

    #[test]
    fn rank_n_names_follow_shared_alpha_stable_shape_not_source_sites() {
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
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        assert!(emitted.host_java.contains(&format!("Shapes.{rank_n} p0")));
        assert!(
            emitted
                .shapes_java
                .contains(&format!("public interface {rank_n} {{"))
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
        assert_eq!(
            bounded, rank_n,
            "alpha-renaming and source-site names do not change the stage"
        );
        assert_eq!(exact, "e0;q*fb0;b0;", "exact shape identity is unchanged");
        assert_eq!(bounded, "KioForall_Type_Fn_T0_T0");
        assert_eq!(
            emitted
                .shapes_java
                .matches(&format!("public interface {bounded} {{"))
                .count(),
            1
        );
    }

    #[test]
    fn wide_dependent_rank_n_shapes_keep_bounded_native_names() {
        for width in [30, 128] {
            let domain = vec!["T"; width].join(" & ");
            let source = format!("module api; host fn use_stage(f: [T] ({domain}) -> T) -> .;");
            let package = build_package(&[&source], "bridge { api; }");
            let emitted = lower_package(&package, "pkg", None).expect("emit wide Java stage");
            let identity = format!("e0;q*f{}b0;b0;", "pb0;".repeat(width - 1));
            let name = format!("KioForall_H{}", fnv1a_64_hex(&identity));
            assert!(name.len() <= crate::backends::skin::READABLE_MINT_MAX_LEN);
            assert!(
                emitted
                    .shapes_java
                    .contains(&format!("public interface {name} {{")),
                "width {width}: {}",
                emitted.shapes_java
            );
            assert!(emitted.host_java.contains(&format!("Shapes.{name} p0")));
        }
    }

    #[test]
    fn bounded_forall_registry_compares_full_identity_and_declaration() {
        let mut interfaces = BTreeMap::new();
        let name = "KioForall_H0000000000000000".to_owned();
        let retained = BoundaryFacadeSupportOrigin::Retained {
            removed_at_version: 2,
        };
        register_java_forall_interface(
            &mut interfaces,
            name.clone(),
            "complete first identity".to_owned(),
            "exact declaration".to_owned(),
            retained,
        )
        .unwrap();
        register_java_forall_interface(
            &mut interfaces,
            name.clone(),
            "complete first identity".to_owned(),
            "exact declaration".to_owned(),
            BoundaryFacadeSupportOrigin::Live,
        )
        .unwrap();
        let before = interfaces.clone();
        for (identity, declaration) in [
            ("complete different identity", "exact declaration"),
            ("complete first identity", "different declaration"),
        ] {
            assert!(
                register_java_forall_interface(
                    &mut interfaces,
                    name.clone(),
                    identity.to_owned(),
                    declaration.to_owned(),
                    retained,
                )
                .is_err()
            );
            assert_eq!(interfaces, before);
        }
        assert_eq!(interfaces[&name].2, BoundaryFacadeSupportOrigin::Live);
    }

    #[test]
    fn unsaturated_and_saturated_nominals_share_the_exact_application_index() {
        let package = build_package(
            &["module api; \
               pub newtype Box[A] : A { pub constructor make_box; pub projector read_box; }; \
               pub newtype Functor[*F] : [A] F(A) -> F(A) { pub constructor make_functor; pub projector read_functor; }; \
               host fn round(value: Functor(Box)) -> Functor(Box);"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let functor = "Shapes.Apply1<Shapes.KioModule_api.Functor, Shapes.KioModule_api.Box>";
        assert!(
            emitted
                .host_java
                .contains(&format!("{functor} api__round({functor} p0);")),
            "{}",
            emitted.host_java
        );
        for name in ["Box", "Functor"] {
            assert!(
                emitted
                    .shapes_java
                    .contains(&format!("public interface {name} {{"))
            );
            assert!(!emitted.shapes_java.contains(&format!("class {name}<")));
        }
        assert!(
            emitted
                .shapes_java
                .contains("private Apply1(String identity, Object payload)")
        );
        assert!(!emitted.shapes_java.contains("public Apply1("));
        assert!(
            !emitted.handle_java.contains("_lift(") && !emitted.handle_java.contains("_project(")
        );
        assert!(
            emitted
                .handle_java
                .contains("Shapes.Apply1<Shapes.KioModule_api.Box, KioCallType_0> makeBox("),
            "{}",
            emitted.handle_java
        );
    }
    #[test]
    fn nominal_application_indices_do_not_grant_generic_payload_access() {
        let package = build_package(
            &["module api; \
               pub newtype Box[A] : A & . { pub constructor make; pub projector read; }; \
               pub newtype Hidden[A] : A & . { constructor make; projector read; };"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        assert!(emitted.handle_java.contains(" make(") && emitted.handle_java.contains(" read("));
        let hidden = emitted
            .handle_java
            .split_once("class NtH_M_api__T_Hidden")
            .unwrap()
            .1
            .split_once("\n  }")
            .unwrap()
            .0;
        assert!(
            !hidden.contains(" make(") && !hidden.contains(" read("),
            "{hidden}"
        );
        assert!(
            !emitted.handle_java.contains("_lift(") && !emitted.handle_java.contains("_project(")
        );
        assert!(
            !emitted.shapes_java.contains("fromNative(")
                && !emitted.shapes_java.contains("toNative(")
        );
        let application = emitted
            .shapes_java
            .split_once("public static final class Apply1<F, A> {")
            .unwrap()
            .1
            .split_once("\n  }")
            .unwrap()
            .0;
        assert!(
            application.contains("private final Object payload;")
                && !application.contains("public "),
            "{application}"
        );
        assert!(
            emitted
                .shapes_java
                .contains("wrong nominal application identity")
        );
    }
    #[test]
    fn parameterized_host_type_storage_helpers_are_declaration_specific() {
        let package = build_package(
            &["module api; host type Box[A]; host fn round[A](value: Box(A)) -> Box(A);"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        let index = "Shapes.Apply1<Shapes.KioModule_api.Box, KioHostArg_0>";
        assert!(emitted.shapes_java.contains("public interface Box {"));
        assert!(
            emitted.shapes_java.contains(&format!(
                "public static <KioHostArg_0> {index} fromNative(java.lang.Object value)"
            )),
            "{}",
            emitted.shapes_java
        );
        assert!(emitted.shapes_java.contains(&format!(
            "public static <KioHostArg_0> java.lang.Object toNative({index} value)"
        )));
        assert!(
            emitted
                .shapes_java
                .contains("applicationPayload(value, \"api\\u0000Box\")")
        );
        assert!(
            !emitted.handle_java.contains("_lift(") && !emitted.handle_java.contains("_project(")
        );
        assert!(!emitted.host_java.contains("Apply1<Object"));
    }
    #[test]
    fn procedure_callbacks_keep_explicit_unit_results_in_both_directions() {
        let package = build_package(
            &["module api; host type Str role(str); \
               pub fn invoke(callback: Str -> .) -> . { callback(\"called\"(Str)) } \
               pub fn supply() -> Str -> . { .(_value: Str) { () } }"],
            "bridge { api; }",
        );
        let emitted = lower_package(&package, "pkg", None).expect("emit Java");
        assert!(
            emitted
                .handle_java
                .contains("public void invoke(Shapes.Fn1<T_api__Str, Shapes.Unit> p0)")
        );
        assert!(
            emitted
                .handle_java
                .contains("public Shapes.Fn1<T_api__Str, Shapes.Unit> supply()")
        );
        assert!(emitted.handle_java.contains("__fn_invoke.call(p0);"));
        assert_eq!(emitted.handle_java.matches("__fn_supply.call()").count(), 1);
        assert!(!emitted.handle_java.contains("__convert("));
        assert!(RUNTIME.contains("((Shapes.Fn1<Object, Object>) value).apply(domain)"));
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

        let class_body = |name: &str| {
            let header = format!("public static final class {name} {{");
            emitted
                .shapes_java
                .split_once(&header)
                .expect("public nominal carrier")
                .1
                .split_once("\n    }")
                .expect("carrier end")
                .0
        };
        for name in [
            "Opaque_a",
            "Opaque_b",
            "Existential",
            "Hidden",
            "Recursive_constructor",
            "Recursive_projector",
            "Comptime_opaque",
            "Comptime_constructor",
            "Constructor_unit_alias",
        ] {
            let body = class_body(name);
            assert!(
                body.contains("private final java.lang.Object payload;"),
                "{name}: {body}"
            );
            assert!(
                !body.contains("public ") && !body.contains(" value()"),
                "{name}: {body}"
            );
        }
        for name in [
            "Transparent_both",
            "Outer",
            "Direct_recursive",
            "Comptime_both",
            "Packed_function",
            "Spread_function",
        ] {
            let body = class_body(name);
            assert!(
                body.contains("private final java.lang.Object payload;")
                    && body.contains("public ")
                    && body.contains(" value()"),
                "{name}: {body}"
            );
            assert!(
                body.lines()
                    .filter(|line| line.trim_start().starts_with("public "))
                    .all(|line| !line.contains("Object")),
                "{name}: {body}"
            );
        }
        for name in ["Constructor_only", "Projector_only"] {
            assert!(
                emitted
                    .shapes_java
                    .contains(&format!("public interface {name} {{"))
            );
            assert!(!emitted.shapes_java.contains(&format!("class {name}<")));
        }
        assert!(emitted.shapes_java.contains("public record Unit() {}"));
        assert!(
            emitted.handle_java.contains("makeConstructor(")
                && !emitted.handle_java.contains("readConstructor(")
                && !emitted.handle_java.contains("makeProjector(")
                && emitted.handle_java.contains("readProjector(")
                && emitted.handle_java.contains("readExistential(")
        );
        assert!(emitted.handle_java.contains("makeConstructorUnitAlias()"));
        assert!(emitted.handle_java.contains("class NtH_M_api__T_Opaque_ua"));
        assert!(
            emitted.host_java.contains(
                "Shapes.KioModule_api.Outer api__echoOuter(Shapes.KioModule_api.Outer p0)"
            )
        );
        assert!(
            emitted.host_java.contains(
                "Shapes.KioModule_api.Direct_recursive api__echoDirect(Shapes.KioModule_api.Direct_recursive p0)"
            )
        );
        let constructor = emitted
            .handle_java
            .lines()
            .find(|line| line.contains(" makeRecursive("))
            .unwrap();
        let projector = emitted
            .handle_java
            .lines()
            .find(|line| line.contains(" readRecursive("))
            .unwrap();
        assert!(
            constructor
                .contains("Shapes.Sum<Shapes.Unit, Shapes.KioModule_api.Recursive_constructor>"),
            "{constructor}"
        );
        assert!(
            projector.contains("Shapes.Sum<Shapes.Unit, Shapes.KioModule_api.Recursive_projector>"),
            "{projector}"
        );
        assert!(class_body("Comptime_both").contains("public java.lang.Void value()"));
        assert!(class_body("Packed_function").contains("Shapes.Fn1<Shapes.Product<Shapes.KioModule_api.Opaque_a, Shapes.Apply1<Shapes.KioModule_api.Projector_only, Shapes.Unit>>, Shapes.KioModule_api.Opaque_a>"));
        assert!(!emitted.shapes_java.contains("KioModule_api.Hidden_payload"));
        assert!(
            !emitted
                .shapes_java
                .contains("public static final class Hidden_payload")
        );
        assert!(
            !emitted
                .shapes_java
                .contains("public interface Hidden_payload")
        );
        for line in emitted.shapes_java.lines().filter(|line| {
            line.starts_with("  public ")
                || line.trim_start().starts_with("public static final class ")
                || line.trim_start().starts_with("public interface ")
        }) {
            assert!(
                !line.contains("Object"),
                "erased payload leaks into public carrier API: {line}"
            );
        }
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

        assert_eq!(
            emitted
                .shapes_java
                .matches("public static final class Comptime_bool<")
                .count(),
            2,
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
                    .contains("public static final class Wrapped<"),
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
                .matches("public static final class Token")
                .count()
                == 2,
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

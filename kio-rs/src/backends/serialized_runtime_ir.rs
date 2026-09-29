use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use serde::Serialize;

use crate::ast::{
    Expr, ImportItem, ImportKind, Item, Kind, Module, Newtype, Routed, Signature,
    SignatureGroupKind, SignatureParam, Type, TypeParam, Visibility,
};
use crate::pass::resolve::{Package, follow_type_reexport, qualify_routed_contract_type_in_module};

use super::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteOwner, BoundaryHostBindingOrigin,
    BoundaryHostTypeBinding, BoundaryNewtypePayloadPlan, BoundaryNewtypeSurface,
    BoundaryNominalDeclaration, CallableExecutionLayout, CallableExecutionStage,
    CallableSourceParamAdapter, CallableValueStageLayout, FacadeKind, FacadeShellId, FacadeUse,
    PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};

#[derive(Serialize)]
pub(crate) struct RuntimePackageIr {
    bridged: Vec<String>,
    modules: Vec<RuntimeModuleIr>,
}

/// Python embeds the backend-neutral evaluator IR together with the exact
/// compiler-prepared host-boundary plan.  The extra field is deliberately
/// Python-only: Java continues to consume [`RuntimePackageIr`] unchanged.
#[derive(Serialize)]
pub(crate) struct PythonRuntimePackageIr {
    #[serde(flatten)]
    package: RuntimePackageIr,
    boundary: PythonBoundaryIr,
}

#[derive(Serialize)]
struct PythonBoundaryIr {
    host_bindings: Vec<PythonHostBindingIr>,
    public_newtypes: Vec<PythonPublicNewtypeIr>,
    sites: Vec<PythonBoundarySiteIr>,
}

#[derive(Serialize)]
struct PythonQualifiedNameIr {
    module: Vec<String>,
    name: String,
    frame: String,
}

#[derive(Serialize)]
struct PythonTypeParamIr {
    name: String,
    kind_arity: usize,
}

#[derive(Serialize)]
struct PythonHostBindingIr {
    name: PythonQualifiedNameIr,
    type_params: Vec<PythonTypeParamIr>,
    role: Option<&'static str>,
}

#[derive(Serialize)]
struct PythonPublicNewtypeIr {
    name: PythonQualifiedNameIr,
    type_params: Vec<PythonTypeParamIr>,
    existential_params: Vec<PythonTypeParamIr>,
    surface: PythonNewtypeSurfaceIr,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonNewtypeSurfaceIr {
    Unexposed,
    Opaque,
    Constructor {
        member: String,
    },
    Projector {
        member: String,
    },
    Both {
        constructor: String,
        projector: String,
    },
}

#[derive(Serialize)]
struct PythonBoundarySiteIr {
    module: Vec<String>,
    owner: PythonBoundaryOwnerIr,
    callable: PythonCallablePlanIr,
    nominals: Vec<PythonNominalIr>,
    execution: PythonCallableExecutionIr,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonBoundaryOwnerIr {
    HostFunction { name: String },
    ExportedFunction { name: String },
    NewtypeConstructor { newtype: String, member: String },
    NewtypeProjector { newtype: String, member: String },
}

#[derive(Serialize)]
struct PythonCallablePlanIr {
    facade: PythonFacadePlanIr,
    head: Vec<PythonCallableHeadIr>,
    returned: usize,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonCallableHeadIr {
    Type { binder: usize },
    Value { slots: Vec<usize> },
}

#[derive(Serialize)]
struct PythonFacadePlanIr {
    binders: Vec<PythonFacadeBinderIr>,
    uses: Vec<PythonFacadeUseIr>,
    root: usize,
}

#[derive(Serialize)]
struct PythonFacadeBinderIr {
    name: String,
    kind_arity: usize,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonFacadeUseIr {
    Unit,
    Bottom,
    Bound {
        binder: usize,
    },
    Nominal {
        name: PythonQualifiedNameIr,
    },
    Apply {
        constructor: usize,
        args: Vec<usize>,
    },
    Product {
        shell: PythonShellIr,
        args: Vec<usize>,
    },
    Sum {
        shell: PythonShellIr,
        args: Vec<usize>,
    },
    Function {
        slots: Vec<usize>,
        result: usize,
    },
    Forall {
        binder: usize,
        result: usize,
    },
}

#[derive(Serialize)]
struct PythonShellIr {
    name: String,
    kind: &'static str,
    keys: Vec<String>,
}

#[derive(Serialize)]
struct PythonNominalIr {
    name: PythonQualifiedNameIr,
    declaration: PythonNominalDeclarationIr,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonNominalDeclarationIr {
    HostType {
        type_params: Vec<PythonTypeParamIr>,
        role: Option<&'static str>,
    },
    Newtype {
        type_params: Vec<PythonTypeParamIr>,
        existential_params: Vec<PythonTypeParamIr>,
        transparent_payload: Option<PythonNewtypePayloadIr>,
        surface: PythonNewtypeSurfaceIr,
    },
}

#[derive(Serialize)]
struct PythonNewtypePayloadIr {
    facade: PythonFacadePlanIr,
    declaration_binders: Vec<usize>,
    payload_root: usize,
}

#[derive(Serialize)]
struct PythonCallableExecutionIr {
    head: Vec<PythonExecutionStageIr>,
    root_uses: Vec<PythonExecutionUseIr>,
    transparent_payloads: BTreeMap<String, Vec<PythonExecutionUseIr>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonExecutionStageIr {
    Type,
    Value { layout: PythonValueLayoutIr },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonExecutionUseIr {
    NoAction,
    Function { layout: PythonValueLayoutIr },
    InvokeForall,
    DeclarationBinder,
}

#[derive(Serialize)]
struct PythonValueLayoutIr {
    source_param_count: usize,
    body_abi_arity: usize,
    facade_slot_count: usize,
    source_params: Vec<PythonSourceParamIr>,
}

#[derive(Serialize)]
struct PythonSourceParamIr {
    start: usize,
    end: usize,
    adapter: &'static str,
    product_shell: Option<PythonShellIr>,
}

#[derive(Serialize)]
struct RuntimeModuleIr {
    key: String,
    module: RuntimeModule,
}

#[derive(Serialize)]
struct RuntimeModule {
    items: Vec<RuntimeItem>,
}

#[derive(Serialize)]
enum RuntimeItem {
    FnDef(RuntimeFnDef),
    Newtype(RuntimeNewtype),
    HostFn(RuntimeHostFn),
}

#[derive(Serialize)]
struct RuntimeFnDef {
    vis: RuntimeVisibility,
    name: String,
    sig: RuntimeSignature,
    ret: RuntimeType,
    body: RuntimeExpr,
}

#[derive(Serialize)]
struct RuntimeNewtype {
    vis: RuntimeVisibility,
    name: String,
    type_params: Vec<RuntimeTypeParam>,
    has_existentials: bool,
    payload: RuntimeType,
    constructor: RuntimeTypeMember,
    projector: RuntimeTypeMember,
}

#[derive(Serialize)]
struct RuntimeTypeMember {
    vis: RuntimeVisibility,
    name: String,
}

#[derive(Serialize)]
struct RuntimeHostFn {
    name: String,
}

#[derive(Serialize)]
enum RuntimeVisibility {
    Private,
    Public,
}

#[derive(Serialize)]
struct RuntimeSignature {
    params: Vec<RuntimeSignatureParam>,
    groups: Vec<RuntimeSignatureGroup>,
}

#[derive(Serialize)]
enum RuntimeSignatureParam {
    Type {},
    Value(RuntimeValueParam),
}

#[derive(Serialize)]
struct RuntimeValueParam {
    name: String,
    ty: Option<RuntimeType>,
}

#[derive(Serialize)]
enum RuntimeSignatureGroup {
    Type { len: usize },
    Value { len: usize },
}

#[derive(Serialize)]
struct RuntimeTypeParam {
    name: String,
    id: String,
}

#[derive(Serialize)]
enum RuntimeType {
    TypeVar {
        name: String,
        id: String,
        args: Vec<RuntimeType>,
    },
    Path {
        segments: Vec<String>,
        args: Vec<RuntimeType>,
    },
    Unit {},
    Bottom {},
    Function {
        param: Box<RuntimeType>,
        ret: Box<RuntimeType>,
        abi_arity: usize,
    },
    Product {
        left: Box<RuntimeType>,
        right: Box<RuntimeType>,
    },
    Sum {
        left: Box<RuntimeType>,
        right: Box<RuntimeType>,
    },
    Forall {
        param: RuntimeTypeParam,
        body: Box<RuntimeType>,
    },
}

#[derive(Clone)]
struct RuntimeTypeScope {
    owner: String,
    next_id: Rc<Cell<usize>>,
    kinds: HashMap<String, Kind>,
    binders: HashMap<String, String>,
}

impl RuntimeTypeScope {
    fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            next_id: Rc::new(Cell::new(0)),
            kinds: HashMap::new(),
            binders: HashMap::new(),
        }
    }

    fn bind(&mut self, param: &TypeParam) -> RuntimeTypeParam {
        let ordinal = self.next_id.get();
        self.next_id.set(ordinal + 1);
        let id = format!("{}\0{ordinal}", self.owner);
        self.kinds
            .insert(param.name.clone(), param.effective_kind());
        self.binders.insert(param.name.clone(), id.clone());
        RuntimeTypeParam {
            name: param.name.clone(),
            id,
        }
    }
}

#[derive(Serialize)]
enum RuntimeExpr {
    FnExpr {
        sig: RuntimeSignature,
        body: Box<RuntimeExpr>,
    },
    Let {
        name: String,
        value: Box<RuntimeExpr>,
        body: Box<RuntimeExpr>,
    },
    Seq {
        value: Box<RuntimeExpr>,
        body: Box<RuntimeExpr>,
    },
    Unit {},
    StrLit {
        value: String,
    },
    IntLit {
        digits: String,
    },
    FloatLit {
        digits: String,
    },
    BoolLit {
        value: bool,
    },
    EnrichedTuple {
        items: Vec<RuntimeExpr>,
    },
    EnrichedProject {
        target: Box<RuntimeExpr>,
        index: usize,
        arity: usize,
    },
    EnrichedInject {
        payload: Box<RuntimeExpr>,
        variant: usize,
        variants: usize,
    },
    EnrichedMatch {
        scrutinee: Box<RuntimeExpr>,
        arms: Vec<RuntimeArm>,
    },
    EnrichedConditional {
        cond: Box<RuntimeExpr>,
        then_branch: Box<RuntimeExpr>,
        else_branch: Box<RuntimeExpr>,
    },
    EnrichedRecord {
        fields: Vec<RuntimeRecordField>,
    },
    EnrichedFieldGet {
        target: Box<RuntimeExpr>,
        index: usize,
        arity: usize,
    },
    LowHostCall {
        name: String,
        module_path: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        sig: RuntimeSignature,
        ret_ty: RuntimeType,
    },
    LowModuleCall {
        mangled: String,
        module_path: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        sig: RuntimeSignature,
    },
    LowQualifiedModuleCall {
        mangled: String,
        module_path: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        sig: RuntimeSignature,
    },
    LowQualifiedNewtypeMember {
        payload: Box<RuntimeExpr>,
    },
    LowNewtypeCtor {
        payload: Box<RuntimeExpr>,
    },
    LowNewtypeProj {
        target: Box<RuntimeExpr>,
    },
    LowClosureCall {
        name: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
    },
    LowIndirectCall {
        callee: Box<RuntimeExpr>,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
    },
    LowTypeApplication {
        callee: Box<RuntimeExpr>,
    },
    LowAbsurdCall {
        value_arg: Box<RuntimeExpr>,
    },
    LowCpsProjectorApply {
        // These are the continuation type's erased leading `forall`
        // applications and first value-stage ABI arity. Keeping both on the
        // serialized node makes execution depend only on the already-routed
        // continuation contract, never on reopening its newtype declaration.
        continuation_type_stages: usize,
        continuation_abi_arity: usize,
        receiver: Box<RuntimeExpr>,
        continuation: Box<RuntimeExpr>,
    },
    LowBoundRef {
        name: String,
    },
    LowHostFnValueRef {
        name: String,
        module_path: String,
        sig: RuntimeSignature,
        ret_ty: RuntimeType,
    },
    LowModuleFnValueRef {
        mangled: String,
        module_path: String,
    },
}

#[derive(Serialize)]
struct RuntimeArm {
    param: String,
    body: RuntimeExpr,
}

#[derive(Serialize)]
struct RuntimeRecordField {
    value: RuntimeExpr,
}

pub(crate) fn package_ir(package: &Package<Routed>) -> Result<RuntimePackageIr, &'static str> {
    package
        .package_file()
        .ok_or("serialized runtime emit requires a package file")?;
    let bridged = crate::pass::resolve::bridged_module_paths(package)
        .into_iter()
        .collect();
    let modules = package
        .modules()
        .map(|(key, entry)| RuntimeModuleIr {
            key: key.to_owned(),
            module: runtime_module(package, key, &entry.module),
        })
        .collect();
    Ok(RuntimePackageIr { bridged, modules })
}

pub(crate) fn python_package_ir(
    package: &Package<Routed>,
    replayed: Option<&crate::sig::ReplayedInterface>,
) -> Result<PythonRuntimePackageIr, String> {
    let package_ir = package_ir(package).map_err(str::to_owned)?;
    let prepared = PreparedBoundaryCallableSites::collect(package, replayed)
        .map_err(|error| format!("cannot prepare Python boundary: {error}"))?;
    let boundary = python_boundary_ir(&prepared)?;
    Ok(PythonRuntimePackageIr {
        package: package_ir,
        boundary,
    })
}

fn python_boundary_ir(
    prepared: &PreparedBoundaryCallableSites,
) -> Result<PythonBoundaryIr, String> {
    let host_bindings = prepared
        .host_bindings()
        // Python 3.10's stdlib-only surface has no recognized deprecation
        // marker for generated runtime declarations. Per
        // `specs/backends/python.md` § History-only generated names are not
        // source-stable on Python, history-only bindings are omitted instead
        // of installing an unmarked carrier or imposing a removed host
        // obligation.
        .filter(|binding| matches!(binding.origin(), BoundaryHostBindingOrigin::Live))
        .map(|binding| PythonHostBindingIr {
            name: python_qualified_name(binding.name()),
            type_params: binding
                .type_params()
                .iter()
                .map(|param| PythonTypeParamIr {
                    name: param.name().to_owned(),
                    kind_arity: param.kind().arity(),
                })
                .collect(),
            role: match binding.binding() {
                BoundaryHostTypeBinding::Role(role) => Some(role.as_str()),
                BoundaryHostTypeBinding::Roleless => None,
            },
        })
        .collect();
    // This iterator is the live public inventory; retained carriers are
    // omitted per `specs/backends/python.md` § History-only generated names
    // are not source-stable on Python.
    let public_newtypes = prepared
        .public_newtypes()
        .map(|entry| PythonPublicNewtypeIr {
            name: python_qualified_name(entry.name()),
            type_params: entry
                .type_params()
                .iter()
                .map(|param| PythonTypeParamIr {
                    name: param.name().to_owned(),
                    kind_arity: param.kind().arity(),
                })
                .collect(),
            existential_params: entry
                .existential_params()
                .iter()
                .map(|param| PythonTypeParamIr {
                    name: param.name().to_owned(),
                    kind_arity: param.kind().arity(),
                })
                .collect(),
            surface: python_newtype_surface(entry.surface()),
        })
        .collect();
    // Only live execution sites enter Python runtime validation or dispatch;
    // see `specs/backends/python.md` § History-only generated names are not
    // source-stable on Python.
    let sites = prepared
        .sites()
        .filter(|site| site.execution().is_some())
        .map(|site| {
            let plan = site.plan();
            let entry = plan.entry();
            let execution = site
                .execution()
                .expect("the filtered Python boundary site is live");
            let nominals = site
                .nominals()
                .declarations()
                .map(|(name, declaration)| PythonNominalIr {
                    name: python_qualified_name(name),
                    declaration: match declaration {
                        BoundaryNominalDeclaration::HostType {
                            type_params,
                            binding,
                        } => PythonNominalDeclarationIr::HostType {
                            type_params: type_params
                                .iter()
                                .map(|param| PythonTypeParamIr {
                                    name: param.name().to_owned(),
                                    kind_arity: param.kind().arity(),
                                })
                                .collect(),
                            role: match binding {
                                BoundaryHostTypeBinding::Role(role) => Some(role.as_str()),
                                BoundaryHostTypeBinding::Roleless => None,
                            },
                        },
                        BoundaryNominalDeclaration::Newtype {
                            type_params,
                            existential_params,
                            transparent_payload,
                            surface,
                        } => PythonNominalDeclarationIr::Newtype {
                            type_params: type_params
                                .iter()
                                .map(|param| PythonTypeParamIr {
                                    name: param.name().to_owned(),
                                    kind_arity: param.kind().arity(),
                                })
                                .collect(),
                            existential_params: existential_params
                                .iter()
                                .map(|param| PythonTypeParamIr {
                                    name: param.name().to_owned(),
                                    kind_arity: param.kind().arity(),
                                })
                                .collect(),
                            transparent_payload: transparent_payload
                                .as_ref()
                                .map(python_newtype_payload),
                            surface: python_newtype_surface(surface),
                        },
                    },
                })
                .collect();
            Ok(PythonBoundarySiteIr {
                module: site.site().module_segments().to_vec(),
                owner: match site.site().owner() {
                    BoundaryFacadeSiteOwner::HostFunction { name } => {
                        PythonBoundaryOwnerIr::HostFunction { name: name.clone() }
                    }
                    BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                        PythonBoundaryOwnerIr::ExportedFunction { name: name.clone() }
                    }
                    BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
                        PythonBoundaryOwnerIr::NewtypeConstructor {
                            newtype: newtype.clone(),
                            member: member.clone(),
                        }
                    }
                    BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                        PythonBoundaryOwnerIr::NewtypeProjector {
                            newtype: newtype.clone(),
                            member: member.clone(),
                        }
                    }
                },
                callable: PythonCallablePlanIr {
                    facade: python_facade_plan(plan.facade()),
                    head: entry
                        .head_stages
                        .iter()
                        .map(|stage| match stage {
                            BoundaryCallableHeadStage::Type { id, .. } => {
                                PythonCallableHeadIr::Type { binder: id.index() }
                            }
                            BoundaryCallableHeadStage::Value { slots } => {
                                PythonCallableHeadIr::Value {
                                    slots: slots.iter().map(|id| id.index()).collect(),
                                }
                            }
                        })
                        .collect(),
                    returned: entry.returned.index(),
                },
                nominals,
                execution: python_callable_execution(
                    site.plan().facade(),
                    execution,
                    site.nominals(),
                )?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PythonBoundaryIr {
        host_bindings,
        public_newtypes,
        sites,
    })
}

fn python_qualified_name(name: &QualifiedTypeName) -> PythonQualifiedNameIr {
    PythonQualifiedNameIr {
        module: name.module_segments().to_vec(),
        name: name.name().to_owned(),
        frame: python_exact_identity_frame(name),
    }
}

fn python_exact_identity_frame(name: &QualifiedTypeName) -> String {
    use crate::backends::public_names::host_name_core;
    let mut out = format!("V1_M{}_", name.module_segments().len());
    for segment in name.module_segments() {
        let segment = host_name_core(segment);
        out.push_str(&format!("C{}_{segment}", segment.len()));
    }
    let leaf = host_name_core(name.name());
    out.push_str(&format!("N{}_{leaf}", leaf.len()));
    out
}

fn python_newtype_surface(surface: &BoundaryNewtypeSurface) -> PythonNewtypeSurfaceIr {
    match surface {
        BoundaryNewtypeSurface::Unexposed => PythonNewtypeSurfaceIr::Unexposed,
        BoundaryNewtypeSurface::Opaque => PythonNewtypeSurfaceIr::Opaque,
        BoundaryNewtypeSurface::Constructor { member } => PythonNewtypeSurfaceIr::Constructor {
            member: member.clone(),
        },
        BoundaryNewtypeSurface::Projector { member } => PythonNewtypeSurfaceIr::Projector {
            member: member.clone(),
        },
        BoundaryNewtypeSurface::Both {
            constructor,
            projector,
        } => PythonNewtypeSurfaceIr::Both {
            constructor: constructor.clone(),
            projector: projector.clone(),
        },
    }
}

fn python_newtype_payload(payload: &BoundaryNewtypePayloadPlan) -> PythonNewtypePayloadIr {
    PythonNewtypePayloadIr {
        facade: python_facade_plan(payload.facade()),
        declaration_binders: payload
            .declaration_binders()
            .iter()
            .map(|id| id.index())
            .collect(),
        payload_root: payload.payload_root().index(),
    }
}

fn python_facade_plan(plan: &BoundaryFacadePlan) -> PythonFacadePlanIr {
    PythonFacadePlanIr {
        binders: plan
            .binders()
            .iter()
            .map(|binder| PythonFacadeBinderIr {
                name: binder.name.clone(),
                kind_arity: binder.kind.arity(),
            })
            .collect(),
        uses: plan
            .uses()
            .iter()
            .map(|use_| match use_ {
                FacadeUse::Unit { .. } => PythonFacadeUseIr::Unit,
                FacadeUse::Bottom { .. } => PythonFacadeUseIr::Bottom,
                FacadeUse::Bound { binder, .. } => PythonFacadeUseIr::Bound {
                    binder: binder.index(),
                },
                FacadeUse::Nominal { name, .. } => PythonFacadeUseIr::Nominal {
                    name: python_qualified_name(name),
                },
                FacadeUse::Apply {
                    constructor, args, ..
                } => PythonFacadeUseIr::Apply {
                    constructor: constructor.index(),
                    args: args.iter().map(|id| id.index()).collect(),
                },
                FacadeUse::Product { shell, args, .. } => PythonFacadeUseIr::Product {
                    shell: python_shell(shell),
                    args: args.iter().map(|id| id.index()).collect(),
                },
                FacadeUse::Sum { shell, args, .. } => PythonFacadeUseIr::Sum {
                    shell: python_shell(shell),
                    args: args.iter().map(|id| id.index()).collect(),
                },
                FacadeUse::Function { slots, result, .. } => PythonFacadeUseIr::Function {
                    slots: slots.iter().map(|id| id.index()).collect(),
                    result: result.index(),
                },
                FacadeUse::Forall { binder, result, .. } => PythonFacadeUseIr::Forall {
                    binder: binder.index(),
                    result: result.index(),
                },
            })
            .collect(),
        root: plan.root().index(),
    }
}

fn python_shell(shell: &FacadeShellId) -> PythonShellIr {
    PythonShellIr {
        name: shell.encode_public(),
        kind: match shell.kind() {
            FacadeKind::Product => "product",
            FacadeKind::Sum => "sum",
        },
        keys: shell
            .ordered_keys()
            .iter()
            .map(python_semantic_key)
            .collect(),
    }
}

fn python_semantic_key(key: &SemanticKey) -> String {
    use crate::backends::public_names::host_name_core;
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

fn python_callable_execution(
    facade: &BoundaryFacadePlan,
    execution: &CallableExecutionLayout,
    nominals: &super::boundary_facade::BoundaryNominalDependencies,
) -> Result<PythonCallableExecutionIr, String> {
    let head = execution
        .head_stages()
        .iter()
        .map(|stage| match stage {
            CallableExecutionStage::Type { .. } => PythonExecutionStageIr::Type,
            CallableExecutionStage::Value(layout) => PythonExecutionStageIr::Value {
                layout: python_value_layout(layout),
            },
        })
        .collect();
    let root_uses = python_execution_uses(facade, execution.root_uses())?;
    let mut transparent_payloads = BTreeMap::new();
    for (name, declaration) in nominals.declarations() {
        let BoundaryNominalDeclaration::Newtype {
            transparent_payload: Some(payload),
            ..
        } = declaration
        else {
            continue;
        };
        let payload_execution = execution.transparent_payload(name).ok_or_else(|| {
            format!(
                "prepared Python boundary omitted transparent execution for {}.{}",
                name.module_segments().join("."),
                name.name()
            )
        })?;
        transparent_payloads.insert(
            python_exact_identity_frame(name),
            python_execution_uses(payload.facade(), payload_execution)?,
        );
    }
    Ok(PythonCallableExecutionIr {
        head,
        root_uses,
        transparent_payloads,
    })
}

fn python_execution_uses(
    facade: &BoundaryFacadePlan,
    execution: &BoundaryFacadeExecutionPlan,
) -> Result<Vec<PythonExecutionUseIr>, String> {
    let mut pending = vec![facade.root()];
    let mut seen = BTreeSet::new();
    let mut by_index = BTreeMap::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.index()) {
            continue;
        }
        let use_ = facade.use_at(id);
        match use_ {
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
                pending.extend(slots.iter().copied());
                pending.push(*result);
            }
            FacadeUse::Forall { result, .. } => pending.push(*result),
            FacadeUse::Unit { .. }
            | FacadeUse::Bottom { .. }
            | FacadeUse::Bound { .. }
            | FacadeUse::Nominal { .. } => {}
        }
        let serialized = match execution.use_at(id) {
            BoundaryFacadeExecutionUse::NoAction => PythonExecutionUseIr::NoAction,
            BoundaryFacadeExecutionUse::Function(layout) => PythonExecutionUseIr::Function {
                layout: python_value_layout(layout),
            },
            BoundaryFacadeExecutionUse::InvokeForall => PythonExecutionUseIr::InvokeForall,
            BoundaryFacadeExecutionUse::DeclarationBinder => {
                PythonExecutionUseIr::DeclarationBinder
            }
        };
        by_index.insert(id.index(), serialized);
    }
    if by_index.len() != facade.uses().len() {
        return Err("prepared Python facade arena contains an unreachable use".to_owned());
    }
    Ok(by_index.into_values().collect())
}

fn python_value_layout(layout: &CallableValueStageLayout) -> PythonValueLayoutIr {
    PythonValueLayoutIr {
        source_param_count: layout.source_param_count(),
        body_abi_arity: layout.body_abi_arity(),
        facade_slot_count: layout.facade_slot_count(),
        source_params: layout
            .source_params()
            .iter()
            .map(|source| {
                let range = source.facade_slots();
                PythonSourceParamIr {
                    start: range.start,
                    end: range.end,
                    adapter: match source.adapter() {
                        CallableSourceParamAdapter::UnitValue => "unit_value",
                        CallableSourceParamAdapter::Identity => "identity",
                        CallableSourceParamAdapter::RightNest => "right_nest",
                    },
                    product_shell: source.product_shell().map(python_shell),
                }
            })
            .collect(),
    }
}

fn runtime_module(package: &Package<Routed>, key: &str, module: &Module<Routed>) -> RuntimeModule {
    let items = module
        .items
        .iter()
        .flat_map(|item| runtime_items(package, key, module, item))
        .collect();
    RuntimeModule { items }
}

fn runtime_items(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    item: &Item<Routed>,
) -> Vec<RuntimeItem> {
    match item {
        Item::FnDef(def) => {
            let mut scope = RuntimeTypeScope::new(format!("fn:{module_key}:{}", def.name));
            let sig = runtime_signature(package, module, &def.sig, &mut scope);
            vec![RuntimeItem::FnDef(RuntimeFnDef {
                vis: runtime_visibility(&def.vis),
                name: def.name.clone(),
                sig,
                ret: runtime_type(package, module, &def.ret, &scope),
                body: runtime_expr(package, module_key, module, &def.body, &scope),
            })]
        }
        Item::Newtype(def) => vec![RuntimeItem::Newtype(runtime_newtype(
            package, module_key, module, def,
        ))],
        Item::HostFn(def) => vec![RuntimeItem::HostFn(RuntimeHostFn {
            name: def.name.clone(),
        })],
        Item::TypeAlias(_) | Item::HostType(_) => Vec::new(),
        Item::TypeRecGroup(group) => group
            .members
            .iter()
            .filter_map(|member| match member {
                crate::ast::TypeRecMember::TypeAlias(_) => None,
                crate::ast::TypeRecMember::Newtype(newtype) => Some(RuntimeItem::Newtype(
                    runtime_newtype(package, module_key, module, newtype),
                )),
                crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
            })
            .collect(),
        Item::RecGroup(_, ext) => match *ext {},
        Item::LiteralAlias(_, ext) => match *ext {},
        Item::Labels(_, ext) | Item::LabelForward(_, ext) => match *ext {},
        Item::Equiv(_, ext) => match *ext {},
        Item::Elaborator(_, ext) => match *ext {},
        Item::Op(_, ext) | Item::VariadicOperator(_, ext) => match *ext {},
    }
}

fn runtime_newtype(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    def: &Newtype<Routed>,
) -> RuntimeNewtype {
    let mut scope = RuntimeTypeScope::new(format!("newtype:{module_key}:{}", def.name));
    let type_params = def
        .type_params
        .iter()
        .map(|param| scope.bind(param))
        .collect();
    for param in &def.existential_params {
        scope.bind(param);
    }
    RuntimeNewtype {
        vis: runtime_visibility(&def.vis),
        name: def.name.clone(),
        type_params,
        has_existentials: !def.existential_params.is_empty(),
        payload: runtime_type(package, module, &def.payload, &scope),
        constructor: RuntimeTypeMember {
            vis: runtime_visibility(&def.constructor.vis),
            name: def.constructor.name.clone(),
        },
        projector: RuntimeTypeMember {
            vis: runtime_visibility(&def.projector.vis),
            name: def.projector.name.clone(),
        },
    }
}

fn runtime_visibility(vis: &Visibility) -> RuntimeVisibility {
    if vis.is_exported() {
        RuntimeVisibility::Public
    } else {
        RuntimeVisibility::Private
    }
}

fn runtime_signature(
    package: &Package<Routed>,
    module: &Module<Routed>,
    sig: &Signature<Routed>,
    scope: &mut RuntimeTypeScope,
) -> RuntimeSignature {
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            SignatureParam::Type(param) => {
                scope.bind(param);
                RuntimeSignatureParam::Type {}
            }
            SignatureParam::Value(param) => RuntimeSignatureParam::Value(RuntimeValueParam {
                name: param.name.clone(),
                ty: param
                    .ty
                    .as_ref()
                    .map(|ty| runtime_type(package, module, ty, scope)),
            }),
        })
        .collect();
    RuntimeSignature {
        params,
        groups: sig.groups.iter().map(runtime_signature_group).collect(),
    }
}

fn runtime_signature_group(group: &SignatureGroupKind) -> RuntimeSignatureGroup {
    match group {
        SignatureGroupKind::Type { len } => RuntimeSignatureGroup::Type { len: *len },
        SignatureGroupKind::Value { len } => RuntimeSignatureGroup::Value { len: *len },
    }
}

fn runtime_type(
    package: &Package<Routed>,
    module: &Module<Routed>,
    ty: &Type<Routed>,
    scope: &RuntimeTypeScope,
) -> RuntimeType {
    let qualified = qualify_routed_contract_type_in_module(ty, module, &scope.kinds);
    runtime_qualified_type(package, &qualified, scope)
}

fn runtime_qualified_type(
    package: &Package<Routed>,
    ty: &Type<Routed>,
    scope: &RuntimeTypeScope,
) -> RuntimeType {
    match ty {
        Type::Path { segments, args, .. } => {
            let args = args
                .iter()
                .map(|arg| runtime_qualified_type(package, arg, scope))
                .collect();
            if let [name] = segments.as_slice()
                && let Some(id) = scope.binders.get(name.as_str())
            {
                RuntimeType::TypeVar {
                    name: name.name.clone(),
                    id: id.clone(),
                    args,
                }
            } else {
                RuntimeType::Path {
                    segments: follow_type_reexport(segments, package)
                        .into_iter()
                        .map(|segment| segment.name)
                        .collect(),
                    args,
                }
            }
        }
        Type::Unit { .. } => RuntimeType::Unit {},
        Type::Bottom { .. } => RuntimeType::Bottom {},
        Type::Function {
            param,
            ret,
            abi_arity,
            ..
        } => RuntimeType::Function {
            param: Box::new(runtime_qualified_type(package, param, scope)),
            ret: Box::new(runtime_qualified_type(package, ret, scope)),
            abi_arity: *abi_arity,
        },
        Type::Product { left, right, .. } => RuntimeType::Product {
            left: Box::new(runtime_qualified_type(package, left, scope)),
            right: Box::new(runtime_qualified_type(package, right, scope)),
        },
        Type::Sum { left, right, .. } => RuntimeType::Sum {
            left: Box::new(runtime_qualified_type(package, left, scope)),
            right: Box::new(runtime_qualified_type(package, right, scope)),
        },
        Type::Forall { param, body, .. } => {
            let mut nested_scope = scope.clone();
            let runtime_param = nested_scope.bind(param);
            RuntimeType::Forall {
                param: runtime_param,
                body: Box::new(runtime_qualified_type(package, body, &nested_scope)),
            }
        }
        Type::LabelSugar { ext, .. } => match *ext {},
        Type::Infer { ext, .. } => match *ext {},
        Type::Goal { ext, .. } => match *ext {},
    }
}

fn runtime_expr(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    expr: &Expr<Routed>,
    scope: &RuntimeTypeScope,
) -> RuntimeExpr {
    let nested = |expr: &Expr<Routed>| runtime_expr(package, module_key, module, expr, scope);
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::FnExpr { sig, body, .. } => {
            let mut nested_scope = scope.clone();
            RuntimeExpr::FnExpr {
                sig: runtime_signature(package, module, sig, &mut nested_scope),
                body: Box::new(runtime_expr(
                    package,
                    module_key,
                    module,
                    body,
                    &nested_scope,
                )),
            }
        }
        Expr::Let {
            name, value, body, ..
        } => RuntimeExpr::Let {
            name: name.clone(),
            value: Box::new(nested(value)),
            body: Box::new(nested(body)),
        },
        Expr::Seq { value, body, .. } => RuntimeExpr::Seq {
            value: Box::new(nested(value)),
            body: Box::new(nested(body)),
        },
        Expr::Unit { .. } => RuntimeExpr::Unit {},
        Expr::StrLit { value, .. } => RuntimeExpr::StrLit {
            value: value.clone(),
        },
        Expr::IntLit { digits, .. } => RuntimeExpr::IntLit {
            digits: digits.clone(),
        },
        Expr::FloatLit { digits, .. } => RuntimeExpr::FloatLit {
            digits: digits.clone(),
        },
        Expr::BoolLit { value, .. } => RuntimeExpr::BoolLit { value: *value },
        Expr::EnrichedTuple { items, .. } => RuntimeExpr::EnrichedTuple {
            items: items.iter().map(nested).collect(),
        },
        Expr::EnrichedProject {
            target,
            index,
            arity,
            ..
        } => RuntimeExpr::EnrichedProject {
            target: Box::new(nested(target)),
            index: *index,
            arity: *arity,
        },
        Expr::EnrichedInject {
            payload,
            variant,
            variants,
            ..
        } => RuntimeExpr::EnrichedInject {
            payload: Box::new(nested(payload)),
            variant: *variant,
            variants: *variants,
        },
        Expr::EnrichedMatch {
            scrutinee, arms, ..
        } => RuntimeExpr::EnrichedMatch {
            scrutinee: Box::new(nested(scrutinee)),
            arms: arms
                .iter()
                .map(|arm| RuntimeArm {
                    param: arm.param.clone(),
                    body: nested(&arm.body),
                })
                .collect(),
        },
        Expr::EnrichedConditional {
            cond,
            then_branch,
            else_branch,
            ..
        } => RuntimeExpr::EnrichedConditional {
            cond: Box::new(nested(cond)),
            then_branch: Box::new(nested(then_branch)),
            else_branch: Box::new(nested(else_branch)),
        },
        Expr::EnrichedRecord { fields, .. } => RuntimeExpr::EnrichedRecord {
            fields: fields
                .iter()
                .map(|field| RuntimeRecordField {
                    value: nested(&field.value),
                })
                .collect(),
        },
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            ..
        } => RuntimeExpr::EnrichedFieldGet {
            target: Box::new(nested(target)),
            index: *index,
            arity: *arity,
        },
        Expr::LowHostCall {
            name,
            module_path,
            type_args,
            args,
            sig,
            ret_ty,
            ..
        } => {
            let target_module = module_for_key(package, module_path, module);
            let mut target_scope = RuntimeTypeScope::new(format!("host:{module_path}:{name}"));
            let sig = runtime_signature(package, target_module, sig, &mut target_scope);
            RuntimeExpr::LowHostCall {
                name: name.clone(),
                module_path: module_path.clone(),
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: args.iter().map(nested).collect(),
                sig,
                ret_ty: runtime_type(package, target_module, ret_ty, &target_scope),
            }
        }
        Expr::LowModuleCall {
            mangled,
            type_args,
            args,
            sig,
            ..
        } => {
            let target_key =
                selective_target(module, mangled).unwrap_or_else(|| module_key.to_owned());
            let target_module = module_for_key(package, &target_key, module);
            let mut target_scope = RuntimeTypeScope::new(format!("fn:{target_key}:{mangled}"));
            RuntimeExpr::LowModuleCall {
                mangled: mangled.clone(),
                module_path: target_key,
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: args.iter().map(nested).collect(),
                sig: runtime_signature(package, target_module, sig, &mut target_scope),
            }
        }
        Expr::LowQualifiedModuleCall {
            alias,
            mangled,
            type_args,
            args,
            sig,
            ..
        } => {
            let target_key = qualified_target(module, alias).unwrap_or_else(|| alias.clone());
            let target_module = module_for_key(package, &target_key, module);
            let prefix = format!("{alias}.");
            let member = mangled.strip_prefix(&prefix).unwrap_or(mangled);
            let mut target_scope = RuntimeTypeScope::new(format!("fn:{target_key}:{member}"));
            RuntimeExpr::LowQualifiedModuleCall {
                mangled: member.to_owned(),
                module_path: target_key,
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: args.iter().map(nested).collect(),
                sig: runtime_signature(package, target_module, sig, &mut target_scope),
            }
        }
        Expr::LowQualifiedNewtypeMember { payload, .. } => RuntimeExpr::LowQualifiedNewtypeMember {
            payload: Box::new(nested(payload)),
        },
        Expr::LowNewtypeCtor { payload, .. } => RuntimeExpr::LowNewtypeCtor {
            payload: Box::new(nested(payload)),
        },
        Expr::LowNewtypeProj { target, .. } => RuntimeExpr::LowNewtypeProj {
            target: Box::new(nested(target)),
        },
        Expr::LowClosureCall {
            name,
            type_args,
            args,
            ..
        } => RuntimeExpr::LowClosureCall {
            name: name.clone(),
            type_args: type_args
                .iter()
                .map(|ty| runtime_type(package, module, ty, scope))
                .collect(),
            args: args.iter().map(nested).collect(),
        },
        Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } => RuntimeExpr::LowIndirectCall {
            callee: Box::new(nested(callee)),
            type_args: type_args
                .iter()
                .map(|ty| runtime_type(package, module, ty, scope))
                .collect(),
            args: args.iter().map(nested).collect(),
        },
        Expr::LowTypeApplication { callee, .. } => RuntimeExpr::LowTypeApplication {
            callee: Box::new(nested(callee)),
        },
        Expr::LowAbsurdCall { value_arg, .. } => RuntimeExpr::LowAbsurdCall {
            value_arg: Box::new(nested(value_arg)),
        },
        Expr::LowCpsProjectorApply {
            receiver,
            continuation,
            continuation_ty,
            ..
        } => {
            let (continuation_type_stages, continuation_abi_arity) =
                cps_continuation_runtime_shape(continuation_ty);
            RuntimeExpr::LowCpsProjectorApply {
                continuation_type_stages,
                continuation_abi_arity,
                receiver: Box::new(nested(receiver)),
                continuation: Box::new(nested(continuation)),
            }
        }
        Expr::LowBoundRef { name, .. } => RuntimeExpr::LowBoundRef { name: name.clone() },
        Expr::LowHostFnValueRef {
            name,
            module_path,
            sig,
            ret_ty,
            ..
        } => {
            let target_module = module_for_key(package, module_path, module);
            let mut target_scope = RuntimeTypeScope::new(format!("host:{module_path}:{name}"));
            let sig = runtime_signature(package, target_module, sig, &mut target_scope);
            RuntimeExpr::LowHostFnValueRef {
                name: name.clone(),
                module_path: module_path.clone(),
                sig,
                ret_ty: runtime_type(package, target_module, ret_ty, &target_scope),
            }
        }
        Expr::LowModuleFnValueRef { mangled, .. } => RuntimeExpr::LowModuleFnValueRef {
            mangled: mangled.clone(),
            module_path: selective_target(module, mangled).unwrap_or_else(|| module_key.to_owned()),
        },
        Expr::Path { ext, .. } | Expr::Call { ext, .. } => match *ext {},
        Expr::RecCall { ext, .. } => match *ext {},
        Expr::RowLet { ext, .. } => match *ext {},
        Expr::Tuple { ext, .. } => match *ext {},
        Expr::Elaborator { ext, .. }
        | Expr::RecOrder { ext, .. }
        | Expr::RecQuote { ext, .. }
        | Expr::UserElaborator { ext, .. } => match *ext {},
        Expr::FnPlaceholder { ext, .. } => match *ext {},
        Expr::LabelValue { ext, .. } => match *ext {},
        Expr::Ufcs { ext, .. } => match *ext {},
        Expr::OpChain { ext, .. } => match *ext {},
    }
}

fn module_for_key<'a>(
    package: &'a Package<Routed>,
    key: &str,
    fallback: &'a Module<Routed>,
) -> &'a Module<Routed> {
    package
        .module(key)
        .map(|entry| &entry.module)
        .unwrap_or(fallback)
}

fn cps_continuation_runtime_shape(continuation_ty: &Type<Routed>) -> (usize, usize) {
    let (type_stages, callable) = continuation_ty.peel_leading_foralls();
    let Type::Function { abi_arity, .. } = callable else {
        unreachable!("a routed CPS projector continuation has a function value stage")
    };
    if *abi_arity > 1 {
        unreachable!("a routed CPS projector continuation has zero or one payload ABI slot")
    }
    (type_stages, *abi_arity)
}

fn selective_target(module: &Module<Routed>, name: &str) -> Option<String> {
    module.imports.iter().find_map(|use_| match &use_.kind {
        ImportKind::Selective { items, from }
            if items.iter().any(
                |item| matches!(item, ImportItem::Name { name: imported, .. } if imported == name),
            ) =>
        {
            Some(module_path_string(from))
        }
        _ => None,
    })
}

fn qualified_target(module: &Module<Routed>, alias: &str) -> Option<String> {
    module.imports.iter().find_map(|use_| match &use_.kind {
        ImportKind::Qualified {
            path,
            alias: imported,
        } if imported == alias => Some(module_path_string(path)),
        _ => None,
    })
}

fn module_path_string(path: &crate::ast::ModulePath) -> String {
    path.segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(all(test, feature = "surface", feature = "prime"))]
mod tests {
    use super::*;
    use crate::ast::Prime;
    use crate::backends::kio_prime::emit_module;
    use crate::pass::full::FullPipeline;
    use crate::pass::optimize::optimize_package;
    use crate::pass::parser::parse;
    use crate::pass::recover_to_low::lower;
    use crate::pass::structural_recovery::recover_package;
    use crate::pipeline::Pipeline;
    use crate::prime::pipeline::PrimePipeline;
    use std::path::{Path, PathBuf};

    fn build_package<P>(modules: Vec<(PathBuf, Module<P>)>) -> Package<P>
    where
        P: crate::ast::Phase + crate::pass::resolve::ResolvePhase + Clone,
    {
        let package = Package::build(Path::new(""), modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        package
    }

    fn full_prime_modules(sources: &[(&str, &str)]) -> Package<Prime> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    parse(source).unwrap_or_else(|error| panic!("parse `{path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) =
            FullPipeline::lower_package(parsed, None).expect("lower surface package");
        FullPipeline::typecheck(&build_package(modules)).expect("typecheck surface package")
    }

    fn full_prime(src: &str) -> Package<Prime> {
        full_prime_modules(&[("x/main.kio", src)])
    }

    fn emitted_prime(package: &Package<Prime>) -> (String, Package<Prime>) {
        let mut source = String::new();
        let parsed = package
            .modules()
            .map(|(module_path, entry)| {
                let emitted = emit_module(&entry.module);
                source.push_str(&emitted);
                source.push('\n');
                (
                    PathBuf::from(format!("{module_path}.kio")),
                    parse(&emitted)
                        .unwrap_or_else(|error| panic!("parse emitted `{module_path}`: {error:?}")),
                )
            })
            .collect();
        let (modules, _) = PrimePipeline::lower_package(parsed, None).expect("lower Kio' package");
        let prime = PrimePipeline::typecheck(&build_package(modules)).expect("typecheck Kio'");
        (source, prime)
    }

    fn routed(package: &Package<Prime>) -> Package<Routed> {
        lower(&optimize_package(recover_package(package)))
    }

    fn routed_body<'a>(
        package: &'a Package<Routed>,
        module_path: &str,
        name: &str,
    ) -> &'a Expr<Routed> {
        let module = &package.module(module_path).expect("module").module;
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(&def.body),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn `{name}` not found"))
    }

    fn canonical_body_in(
        package: &Package<Routed>,
        module_path: &str,
        name: &str,
    ) -> serde_json::Value {
        let module = &package.module(module_path).expect("module").module;
        let body = routed_body(package, module_path, name);
        serde_json::to_value(runtime_expr(
            package,
            module_path,
            module,
            body,
            &RuntimeTypeScope::new(format!("test-body:{module_path}:{name}")),
        ))
        .expect("serialize canonical body")
    }

    fn canonical_body(package: &Package<Routed>, name: &str) -> serde_json::Value {
        canonical_body_in(package, "x/main", name)
    }

    fn selective_module_fn_value_sig(package: &Package<Routed>, name: &str) -> serde_json::Value {
        let Expr::LowModuleCall { args, .. } = routed_body(package, "x/main", name) else {
            panic!("`{name}` must lower to a module call")
        };
        let Some(Expr::LowModuleFnValueRef { sig, .. }) = args.first() else {
            panic!("`{name}` must pass a selective module-fn value")
        };
        serde_json::to_value(sig).expect("serialize module-fn signature")
    }

    fn canonical_newtype(package: &Package<Routed>, name: &str) -> serde_json::Value {
        let module = &package.module("x/main").expect("module").module;
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Newtype(def) if def.name == name => Some(def),
                _ => None,
            })
            .unwrap_or_else(|| panic!("newtype `{name}` not found"));
        serde_json::to_value(runtime_newtype(package, "x/main", module, def))
            .expect("serialize canonical newtype")
    }

    fn generated_field_rhs_collision_source() -> (String, String) {
        let mut collision = "field_rhs_s0_e0_i0".to_owned();
        for _ in 0..8 {
            let source = format!(
                "module x/main; \
                 host type I32 role(i32); \
                 labels Pair_fields = {{ first: I32, second: I32 }}; \
                 type Pair = First & Second; \
                 fn t({collision}: I32, _kg0: I32, row: Pair) -> Pair {{ \
                   row.!{{first = _kg0, second = {collision}}} \
                 }}"
            );
            let start = source.find("row.!{").expect("field update start") as u32;
            let end = start
                + source[start as usize..]
                    .find('}')
                    .expect("field update end") as u32
                + 1;
            let next = format!("field_rhs_s{start}_e{end}_i0");
            if next == collision {
                return (source, collision);
            }
            collision = next;
        }
        panic!("field-update span/name fixed point did not converge")
    }

    fn prime_expr_any(expr: &Expr<Prime>, predicate: &impl Fn(&Expr<Prime>) -> bool) -> bool {
        if predicate(expr) {
            return true;
        }
        match expr {
            Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Let { value, body, .. } => {
                prime_expr_any(value, predicate) || prime_expr_any(body, predicate)
            }
            Expr::Seq { value, body, .. } => {
                prime_expr_any(value, predicate) || prime_expr_any(body, predicate)
            }
            Expr::FnExpr { body, .. } => prime_expr_any(body, predicate),
            Expr::Call { callee, args, .. } => {
                prime_expr_any(callee, predicate)
                    || args.iter().any(|arg| {
                        matches!(arg, crate::ast::CallArg::Value(value) if prime_expr_any(value, predicate))
                    })
            }
            Expr::Path { .. }
            | Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. } => false,
            Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::LabelValue { ext, .. }
            | Expr::RowLet { ext, .. }

            | Expr::Elaborator { ext, .. }
            | Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. }
            | Expr::UserElaborator { ext, .. }


            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. }
            | Expr::EnrichedTuple { ext, .. }
            | Expr::EnrichedProject { ext, .. }
            | Expr::EnrichedInject { ext, .. }
            | Expr::EnrichedMatch { ext, .. }
            | Expr::EnrichedConditional { ext, .. }
            | Expr::EnrichedRecord { ext, .. }
            | Expr::EnrichedFieldGet { ext, .. }
            | Expr::LowHostCall { ext, .. }
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

    #[test]
    fn serialized_calls_retain_every_type_application_boundary() {
        let package = routed(&full_prime(
            "module x/main; \
             host type N; \
             host fn first(_unit: .) -> N; \
             host fn second(_unit: .) -> N; \
             host fn produce(_unit: .) -> [A] A; \
             fn interleaved[A](x: A)[B](y: B) -> B { y } \
             fn closure(value: N, function: [A] A -> A) -> N { function(N, value) } \
             fn indirect() -> N { interleaved(N, first(), N, second()) } \
             fn returned() -> N { produce() }",
        ));

        let closure = canonical_body(&package, "closure");
        assert_eq!(
            closure["LowClosureCall"]["type_args"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            closure["LowClosureCall"]["type_args"][0]["Path"]["segments"],
            serde_json::json!(["x", "main", "N"])
        );

        let indirect = canonical_body(&package, "indirect");
        assert_eq!(
            indirect["LowIndirectCall"]["type_args"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            indirect["LowIndirectCall"]["type_args"][0]["Path"]["segments"],
            serde_json::json!(["x", "main", "N"])
        );

        let returned = canonical_body(&package, "returned");
        assert!(returned.get("LowTypeApplication").is_some());
    }

    #[test]
    fn serialized_cps_projector_carries_exact_continuation_runtime_shape() {
        let package = routed(&full_prime(
            "module x/main; \
             newtype Pair <Left> <Right> : Left & Right { \
               constructor make_pair; projector open_pair; \
             }; \
             fn consume(pair: Pair) -> . { \
               Pair.open_pair(pair)(.[Left][Right](_payload: Left & Right) { () }) \
             }",
        ));

        let body = canonical_body(&package, "consume");
        let cps = &body["LowCpsProjectorApply"];
        assert_eq!(cps["continuation_type_stages"], 2);
        assert_eq!(cps["continuation_abi_arity"], 1);
        assert!(cps.get("module_path").is_none());
        assert!(cps.get("newtype").is_none());
    }

    #[test]
    fn field_update_generated_binder_does_not_capture_later_rhs() {
        let (source, collision) = generated_field_rhs_collision_source();
        let full = full_prime(&source);
        let module = &full.module("x/main").expect("module").module;
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == "t" => Some(def),
                _ => None,
            })
            .expect("fn t");
        assert!(
            !prime_expr_any(&def.body, &|expr| {
                matches!(expr, Expr::Let { name, .. } if name == &collision)
            }),
            "the generated first-RHS binder captured the same-named parameter: {:#?}",
            def.body
        );
        assert!(
            !prime_expr_any(&def.body, &|expr| {
                matches!(expr, Expr::Let { name, .. } if name == "_kg0")
            }),
            "the generated binder allocator reused its occupied first candidate: {:#?}",
            def.body
        );
        assert!(
            prime_expr_any(&def.body, &|expr| {
                matches!(expr, Expr::Path { segments, .. } if segments.len() == 1 && segments[0].name == collision)
            }),
            "the later RHS lost its reference to the same-named parameter: {:#?}",
            def.body
        );

        let (emitted, prime) = emitted_prime(&full);
        assert!(emitted.contains(&format!("{collision}: I32")));
        assert!(!emitted.contains(&format!("let {collision} =")));
        assert!(!emitted.contains("let _kg0 ="));
        assert_eq!(
            canonical_body(&routed(&full), "t"),
            canonical_body(&routed(&prime), "t")
        );
    }

    #[test]
    fn scoped_type_binders_are_distinct_from_same_leaf_newtypes() {
        let full = full_prime(
            "module x/main; \
             host type Str role(str); \
             host fn id[A](value: A) -> A; \
             newtype A : Str { constructor mk_a; projector un_a; }; \
             newtype Named : A { constructor mk_named; projector un_named; }; \
             newtype Box[T] : T { constructor mk_box; projector un_box; }; \
             newtype Applied[*F][T] : F(T) { constructor mk_applied; projector un_applied; }; \
             newtype Ranked : [A] A -> A { constructor mk_ranked; projector un_ranked; }; \
             newtype Shadow[T] : [A] T -> A { constructor mk_shadow; projector un_shadow; }; \
             newtype Same[T] : T & ([T] T -> T) { constructor mk_same; projector un_same; }; \
             newtype Capture[T] : [A] A -> T { constructor mk_capture; projector un_capture; }; \
             fn t(value: Str) -> Str { id(value) }",
        );
        let (_emitted, prime) = emitted_prime(&full);
        let direct = routed(&full);
        let round_tripped = routed(&prime);

        assert_eq!(
            canonical_body(&direct, "t"),
            canonical_body(&round_tripped, "t")
        );
        for name in [
            "A", "Named", "Box", "Applied", "Ranked", "Shadow", "Same", "Capture",
        ] {
            assert_eq!(
                canonical_newtype(&direct, name),
                canonical_newtype(&round_tripped, name),
                "serialized runtime type identity drifted for `{name}`"
            );
        }

        for package in [&direct, &round_tripped] {
            let body = canonical_body(package, "t");
            let host_param_ty = &body["LowHostCall"]["sig"]["params"][1]["Value"]["ty"];
            assert_eq!(host_param_ty["TypeVar"]["name"], "A");
            let caller_a_id = host_param_ty["TypeVar"]["id"]
                .as_str()
                .expect("caller binder id");
            assert_eq!(
                host_param_ty["TypeVar"]["args"].as_array().map(Vec::len),
                Some(0)
            );
            assert_eq!(body["LowHostCall"]["ret_ty"]["TypeVar"]["name"], "A");

            let generic_payload = canonical_newtype(package, "Box");
            assert_eq!(generic_payload["payload"]["TypeVar"]["name"], "T");

            let applied_payload = canonical_newtype(package, "Applied");
            assert_eq!(applied_payload["payload"]["TypeVar"]["name"], "F");
            assert_eq!(
                applied_payload["payload"]["TypeVar"]["args"][0]["TypeVar"]["name"],
                "T"
            );

            let ranked_payload = canonical_newtype(package, "Ranked");
            assert_eq!(ranked_payload["payload"]["Forall"]["param"]["name"], "A");
            let ranked_body = &ranked_payload["payload"]["Forall"]["body"]["Function"];
            assert_eq!(ranked_body["param"]["TypeVar"]["name"], "A");
            assert_eq!(ranked_body["ret"]["TypeVar"]["name"], "A");

            let shadow_payload = canonical_newtype(package, "Shadow");
            assert_eq!(shadow_payload["payload"]["Forall"]["param"]["name"], "A");
            let shadow_body = &shadow_payload["payload"]["Forall"]["body"]["Function"];
            assert_eq!(shadow_body["param"]["TypeVar"]["name"], "T");
            assert_eq!(shadow_body["ret"]["TypeVar"]["name"], "A");

            let same_payload = canonical_newtype(package, "Same");
            let same_outer_id = same_payload["type_params"][0]["id"]
                .as_str()
                .expect("outer same-spelling binder id");
            let same_inner_id =
                same_payload["payload"]["Product"]["right"]["Forall"]["param"]["id"]
                    .as_str()
                    .expect("nested same-spelling binder id");
            assert_ne!(same_outer_id, same_inner_id);
            assert_eq!(
                same_payload["payload"]["Product"]["right"]["Forall"]["body"]["Function"]["param"]
                    ["TypeVar"]["id"],
                same_inner_id
            );

            let capture_payload = canonical_newtype(package, "Capture");
            let capture_inner_id = capture_payload["payload"]["Forall"]["param"]["id"]
                .as_str()
                .expect("capture binder id");
            assert_ne!(caller_a_id, capture_inner_id);

            let nominal_payload = canonical_newtype(package, "Named");
            assert_eq!(
                nominal_payload["payload"]["Path"]["segments"],
                serde_json::json!(["x", "main", "A"])
            );
        }
    }

    #[test]
    fn newtype_member_visibility_is_serialized() {
        let package = routed(&full_prime(
            "module x/main; \
             pub newtype Opaque : . { constructor hide; projector hidden; }; \
             pub newtype Split : . { pub constructor make; projector unmake; }; \
             pub newtype Readable : . { constructor wrap; pub projector read; };",
        ));

        let opaque = canonical_newtype(&package, "Opaque");
        assert_eq!(opaque["constructor"]["vis"], "Private");
        assert_eq!(opaque["projector"]["vis"], "Private");

        let split = canonical_newtype(&package, "Split");
        assert_eq!(split["constructor"]["vis"], "Public");
        assert_eq!(split["projector"]["vis"], "Private");

        let readable = canonical_newtype(&package, "Readable");
        assert_eq!(readable["constructor"]["vis"], "Private");
        assert_eq!(readable["projector"]["vis"], "Public");
    }

    #[test]
    fn same_leaf_newtype_boundaries_keep_exact_module_identity() {
        let package = routed(&full_prime_modules(&[
            (
                "left.kio",
                "module left; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round(value: Token) -> Token; \
                 pub fn via(value: Token) -> Token { round(value) }",
            ),
            (
                "right.kio",
                "module right; \
                 pub newtype Token : . { constructor make_token; projector read_token; }; \
                 host fn round(value: Token) -> Token; \
                 pub fn via(value: Token) -> Token { round(value) }",
            ),
        ]));
        let assert_path = |ty: &serde_json::Value, module: &str| {
            assert_eq!(
                ty["Path"]["segments"],
                serde_json::json!([module, "Token"]),
                "{ty:#}"
            );
        };

        for module_path in ["left", "right"] {
            let module = &package.module(module_path).expect("runtime module").module;
            let item = module
                .items
                .iter()
                .find(|item| matches!(item, Item::FnDef(def) if def.name == "via"))
                .unwrap_or_else(|| panic!("missing runtime function {module_path}.via"));
            let function = serde_json::to_value(
                runtime_items(&package, module_path, module, item)
                    .into_iter()
                    .next()
                    .expect("runtime function"),
            )
            .expect("serialize runtime function");
            let function = &function["FnDef"];

            assert_path(&function["sig"]["params"][0]["Value"]["ty"], module_path);
            assert_path(&function["ret"], module_path);

            let call = &function["body"]["LowHostCall"];
            assert_path(&call["sig"]["params"][0]["Value"]["ty"], module_path);
            assert_path(&call["ret_ty"], module_path);
        }
    }

    #[test]
    fn full_and_prime_routes_canonicalize_nested_later_argument_call() {
        let full = full_prime(
            "module x/main; \
             host type I32 role(i32); \
             host fn sub_i32(a: I32, b: I32) -> I32; \
             labels Pair_fields = { first: I32, second: I32 }; \
             type Pair = First & Second; \
             fn pick_second(a: I32, b: I32) -> I32 { b } \
             fn t(row: Pair) -> I32 { \
               pick_second(row.?{first}, sub_i32(row.?{second}, 1(I32))) \
             }",
        );
        let (emitted, prime) = emitted_prime(&full);
        assert!(
            emitted.contains("let _kp"),
            "expected Kio' emission to introduce an administrative let:\n{emitted}"
        );

        let direct = routed(&full);
        let round_tripped = routed(&prime);
        assert_eq!(
            canonical_body(&direct, "t"),
            canonical_body(&round_tripped, "t")
        );
    }

    #[test]
    fn full_and_prime_routes_canonicalize_callable_nominal_spelling() {
        let full = full_prime(
            "module x/main; import __intrinsics__; \
             host type I32 role(i32); \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             fn step(_n: I32) -> I32 | . { __right__(I32, ., ()) } \
             fn t(start: I32) -> . { loop(.(n: I32) { step(n) }, start) }",
        );
        let (_emitted, prime) = emitted_prime(&full);
        let direct = canonical_body(&routed(&full), "t");
        let round_tripped = canonical_body(&routed(&prime), "t");

        assert_eq!(direct, round_tripped);
        assert!(
            !direct.to_string().contains("__kio_fn"),
            "nominal path spelling introduced an ABI eta adapter: {direct}"
        );
    }

    #[test]
    fn full_and_prime_routes_use_callee_scope_for_cross_module_callables() {
        let sources = [
            (
                "x/types.kio",
                "module x/types; \
                 pub host type Actual role(bool); \
                 pub host type Other role(i32);",
            ),
            (
                "x/dep.kio",
                "module x/dep; \
                 import x/types as types; \
                 pub type Flag = types.Actual; \
                 pub host fn host_id(value: Flag) -> Flag; \
                 pub host fn true_flag() -> Flag; \
                 pub fn id(value: Flag) -> Flag { value } \
                 pub fn apply[A](f: (A & A) -> Flag, left: A, right: A) -> Flag { \
                   f(left, right) \
                 }",
            ),
            (
                "x/main.kio",
                "module x/main; \
                 import x/dep(apply, host_id, id); \
                 import x/dep as d; \
                 import x/types as types; \
                 type Actual = types.Other; \
                 newtype Token : . { constructor mk_token; projector un_token; }; \
                 fn first(_left: Token, _right: Token) -> d.Flag { d.true_flag() } \
                 fn invoke(f: d.Flag -> d.Flag, value: d.Flag) -> d.Flag { f(value) } \
                 fn local_id(value: d.Flag) -> d.Flag { value } \
                 fn selective_call(value: Token) -> d.Flag { \
                   apply(first, value, value) \
                 } \
                 fn qualified_call(value: Token) -> d.Flag { \
                   d.apply(first, value, value) \
                 } \
                 fn selective_value(value: d.Flag) -> d.Flag { \
                   invoke(id, value) \
                 } \
                 fn qualified_value(value: d.Flag) -> d.Flag { \
                   invoke(d.id, value) \
                 } \
                 fn selective_host_call(value: d.Flag) -> d.Flag { \
                   host_id(value) \
                 } \
                 fn qualified_host_call(value: d.Flag) -> d.Flag { \
                   d.host_id(value) \
                 } \
                 fn selective_host_value(value: d.Flag) -> d.Flag { \
                   invoke(host_id, value) \
                 } \
                 fn qualified_host_value(value: d.Flag) -> d.Flag { \
                   invoke(d.host_id, value) \
                 } \
                 fn local_value(value: d.Flag) -> d.Flag { \
                   invoke(local_id, value) \
                 }",
            ),
        ];
        let without_shadow = full_prime_modules(&sources);
        let full = full_prime_modules(&[
            (
                "a/shadow.kio",
                "module a/shadow; \
                 import x/types(Other); \
                 pub fn id(left: Other, right: Other) -> Other { left }",
            ),
            sources[0],
            sources[1],
            sources[2],
        ]);
        let (_emitted, prime) = emitted_prime(&full);
        let direct = routed(&full);
        let round_tripped = routed(&prime);
        let direct_without_shadow = routed(&without_shadow);

        for name in [
            "selective_call",
            "qualified_call",
            "selective_value",
            "qualified_value",
            "selective_host_call",
            "qualified_host_call",
            "selective_host_value",
            "qualified_host_value",
            "local_value",
        ] {
            assert_eq!(
                canonical_body(&direct, name),
                canonical_body(&round_tripped, name),
                "serialized runtime call shape drifted for `{name}`"
            );
        }
        assert_eq!(
            selective_module_fn_value_sig(&direct_without_shadow, "selective_value"),
            selective_module_fn_value_sig(&direct, "selective_value"),
            "an unrelated same-leaf declaration changed selective module-fn lookup"
        );
    }

    #[test]
    fn full_and_prime_routes_associate_bound_record_value_chain() {
        let full = full_prime(
            "module x/main; \
             host type I32 role(i32); \
             host fn effect_i32(value: I32) -> I32; \
             labels Pair_fields = { first: I32, second: I32 }; \
             type Pair = First & Second; \
             labels Result_fields = { stored: Pair, repeated: Pair, total: I32 }; \
             type Result = Stored & Repeated & Total; \
             fn pick_second(first: I32, second: I32) -> I32 { second } \
             fn collision(_kp0: I32, row: Pair) -> I32 { \
               pick_second(row.?{first}, _kp0) \
             } \
             fn t(x: I32) -> Result { \
               let row = { \
                 first = effect_i32(x), \
                 second = effect_i32(x) \
               }; \
               { stored = row, repeated = row, total = collision(effect_i32(x), row) } \
             }",
        );
        let (emitted, prime) = emitted_prime(&full);
        let first_generated_field_binding = emitted
            .find("let _kg")
            .expect("effectful record field should introduce a let");
        let row_binding = emitted
            .find("let row =")
            .expect("row binding should survive");
        assert!(
            emitted.matches("let _kg").count() >= 3,
            "expected both bound-record fields and the later field to hoist:\n{emitted}"
        );
        assert!(
            first_generated_field_binding < row_binding,
            "Kio' value-chain hoist did not precede the row binding:\n{emitted}"
        );
        assert!(
            emitted.contains("let _kp1 =") && !emitted.contains("let _kp0 ="),
            "Kio' call-prelude temp captured the legal `_kp0` parameter:\n{emitted}"
        );

        let direct = routed(&full);
        let round_tripped = routed(&prime);
        assert_eq!(
            canonical_body(&direct, "t"),
            canonical_body(&round_tripped, "t")
        );
        assert_eq!(
            canonical_body(&direct, "collision"),
            canonical_body(&round_tripped, "collision")
        );
    }
}

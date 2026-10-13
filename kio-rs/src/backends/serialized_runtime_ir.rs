use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use serde::Serialize;

use crate::ast::{
    Expr, ImportItem, ImportKind, Item, Kind, Module, Newtype, Routed, Signature,
    SignatureGroupKind, SignatureParam, Type, TypeParam, Visibility,
};
use crate::pass::resolve::{Package, follow_type_reexport, qualify_routed_contract_type_in_module};

use super::boundary_facade::{
    BoundaryCallableHeadStage, BoundaryFacadeExecutionPlan, BoundaryFacadeExecutionUse,
    BoundaryFacadePlan, BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, BoundaryHostBindingOrigin,
    BoundaryHostTypeBinding, BoundaryNewtypeSurface, CallableExecutionLayout,
    CallableExecutionStage, CallableSourceParamAdapter, CallableValueStageLayout, FacadeKind,
    FacadeShellId, FacadeUse, PreparedBoundaryCallableSites, QualifiedTypeName, SemanticKey,
};
use super::tail_plan::{ResultChild, TailContext, TailStage};
use super::value_plan::{ValuePlan, ValueUse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeStageRepresentation {
    /// One callable carrier implements each erased type stage.
    Erased,
    /// Type-stage factories are indexed by canonical value-plan nodes.
    Typed,
}

#[derive(Serialize)]
pub(crate) struct RuntimePackageIr {
    bridged: Vec<String>,
    modules: Vec<RuntimeModuleIr>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    value_plans: Vec<RuntimeValuePlanIr>,
    #[serde(skip)]
    body_value_plans: Vec<ValuePlan>,
}

impl RuntimePackageIr {
    pub(crate) fn body_value_plans(&self) -> &[ValuePlan] {
        &self.body_value_plans
    }
}

pub(crate) fn newtype_inventory_json(package: &Package<Routed>) -> Result<String, String> {
    let mut declarations = Vec::new();
    for (module_key, entry) in package.modules() {
        for item in &entry.module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if let Some(newtype) = declaration.newtype() {
                    let name = QualifiedTypeName::new(
                        module_key.split('/').map(str::to_owned).collect(),
                        &newtype.name,
                    )
                    .expect("a routed newtype has its exact declaring module");
                    declarations.push(python_qualified_name(&name));
                }
            });
        }
    }
    serde_json::to_string(&declarations)
        .map_err(|error| format!("cannot serialize exact newtype inventory: {error}"))
}

/// Shared evaluator IR and its compiler-prepared live execution boundaries.
/// Retained declarations remain in the native skin, never in live dispatch.
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
    key: String,
    module: Vec<String>,
    owner: PythonBoundaryOwnerIr,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc: Option<String>,
    callable: PythonCallablePlanIr,
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
struct RuntimeValuePlanIr {
    uses: Vec<RuntimeValueUseIr>,
    root: usize,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuntimeValueUseIr {
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
        left: usize,
        right: usize,
    },
    Sum {
        left: usize,
        right: usize,
    },
    Function {
        domain: usize,
        result: usize,
    },
    Forall {
        binder: usize,
        result: usize,
    },
}

#[derive(Serialize)]
struct PythonCallableExecutionIr {
    head: Vec<PythonExecutionStageIr>,
    root_uses: Vec<PythonExecutionUseIr>,
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
    HostType {
        nominal: PythonQualifiedNameIr,
        role: Option<&'static str>,
    },
}

#[derive(Serialize)]
struct RuntimeFnDef {
    vis: RuntimeVisibility,
    name: String,
    sig: RuntimeSignature,
    ret: RuntimeType,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_plan: Option<usize>,
    body: RuntimeExpr,
}

#[derive(Serialize)]
struct RuntimeNewtype {
    vis: RuntimeVisibility,
    name: String,
    nominal: PythonQualifiedNameIr,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload_plan: Option<usize>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    value_plan: Option<usize>,
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
    stages: Vec<RuntimeBodyStage>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RuntimeBodyStage {
    Type,
    Value { params: Vec<String> },
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

#[derive(Debug)]
struct BodyPlanCollector {
    stages: RuntimeStageRepresentation,
    values: Vec<ValuePlan>,
    identities: HashMap<u64, Vec<usize>>,
}

impl Default for BodyPlanCollector {
    fn default() -> Self {
        Self {
            stages: RuntimeStageRepresentation::Typed,
            values: Vec::new(),
            identities: HashMap::new(),
        }
    }
}

impl BodyPlanCollector {
    fn intern(&mut self, plan: ValuePlan) -> usize {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        plan.body_storage_identity().hash(&mut hash);
        let candidates = self.identities.entry(hash.finish()).or_default();
        if let Some(id) = candidates
            .iter()
            .find(|&&id| self.values[id].body_storage_identity() == plan.body_storage_identity())
        {
            return *id;
        }
        let id = self.values.len();
        candidates.push(id);
        self.values.push(plan);
        id
    }
}

#[derive(Clone)]
struct RuntimeTypeScope {
    owner: String,
    next_id: Rc<Cell<usize>>,
    kinds: HashMap<String, Kind>,
    binders: HashMap<String, String>,
    type_params: Vec<TypeParam>,
    values: RefCell<HashMap<String, Type<Routed>>>,
    plans: Rc<RefCell<BodyPlanCollector>>,
}

impl RuntimeTypeScope {
    fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            next_id: Rc::new(Cell::new(0)),
            kinds: HashMap::new(),
            binders: HashMap::new(),
            type_params: Vec::new(),
            values: RefCell::new(HashMap::new()),
            plans: Rc::default(),
        }
    }

    fn sharing_plans(owner: impl Into<String>, plans: &Rc<RefCell<BodyPlanCollector>>) -> Self {
        Self {
            plans: Rc::clone(plans),
            ..Self::new(owner)
        }
    }

    fn clone_type_scope(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            next_id: Rc::clone(&self.next_id),
            kinds: self.kinds.clone(),
            binders: self.binders.clone(),
            type_params: self.type_params.clone(),
            values: RefCell::default(),
            plans: Rc::clone(&self.plans),
        }
    }

    fn value_plan(
        &self,
        package: &Package<Routed>,
        module: &Module<Routed>,
        ty: &Type<Routed>,
    ) -> Option<usize> {
        if self.plans.borrow().stages == RuntimeStageRepresentation::Erased {
            return None;
        }
        let plan = ValuePlan::from_routed_type(package, module, &self.type_params, ty)
            .expect("a checked Routed type has a compositional value plan");
        Some(self.plans.borrow_mut().intern(plan))
    }

    fn applied_value_plan(
        &self,
        package: &Package<Routed>,
        module: &Module<Routed>,
        scheme: &Type<Routed>,
        type_args: &[Type<Routed>],
    ) -> Option<usize> {
        if type_args.is_empty() || self.plans.borrow().stages == RuntimeStageRepresentation::Erased
        {
            return None;
        }
        let arguments = type_args
            .iter()
            .map(|ty| {
                super::boundary_facade::canonical_routed_type(
                    package,
                    module,
                    &self.type_params,
                    ty,
                )
            })
            .collect::<Vec<_>>();
        let applied = super::reconstruct::instantiate_type_stages(scheme, &arguments)
            .expect("a routed application consumes exactly its explicit leading type stages");
        self.value_plan(package, module, &applied)
    }

    fn bind(&mut self, param: &TypeParam) -> RuntimeTypeParam {
        let ordinal = self.next_id.get();
        self.next_id.set(ordinal + 1);
        let id = format!("{}\0{ordinal}", self.owner);
        self.kinds
            .insert(param.name.clone(), param.effective_kind());
        self.binders.insert(param.name.clone(), id.clone());
        self.type_params.push(param.clone());
        RuntimeTypeParam {
            name: param.name.clone(),
            id,
        }
    }

    fn value_type(
        &self,
        package: &Package<Routed>,
        module: &Module<Routed>,
        expr: &Expr<Routed>,
    ) -> Option<Type<Routed>> {
        super::reconstruct::RoutedBodyTypes {
            package,
            module,
            type_params: &self.type_params,
            values: &self.values.borrow(),
        }
        .value_type(expr)
    }

    fn with_value<T>(&self, name: &str, ty: Option<Type<Routed>>, body: impl FnOnce() -> T) -> T {
        let previous = if let Some(ty) = ty {
            self.values.borrow_mut().insert(name.to_owned(), ty)
        } else {
            self.values.borrow_mut().remove(name)
        };
        let result = body();
        if let Some(previous) = previous {
            self.values.borrow_mut().insert(name.to_owned(), previous);
        } else {
            self.values.borrow_mut().remove(name);
        }
        result
    }
}

#[derive(Serialize)]
enum RuntimeExpr {
    FnExpr {
        sig: RuntimeSignature,
        #[serde(skip_serializing_if = "Option::is_none")]
        value_plan: Option<usize>,
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
        host_type: PythonQualifiedNameIr,
    },
    IntLit {
        digits: String,
        host_type: PythonQualifiedNameIr,
    },
    FloatLit {
        digits: String,
        host_type: PythonQualifiedNameIr,
    },
    BoolLit {
        value: bool,
        host_type: PythonQualifiedNameIr,
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
        host_type: PythonQualifiedNameIr,
        then_branch: Box<RuntimeExpr>,
        else_branch: Box<RuntimeExpr>,
    },
    EnrichedFieldGet {
        target: Box<RuntimeExpr>,
        nominal: PythonQualifiedNameIr,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowModuleCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        mangled: String,
        module_path: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        sig: RuntimeSignature,
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowQualifiedModuleCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        mangled: String,
        module_path: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        sig: RuntimeSignature,
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowQualifiedNewtypeMember {
        payload: Box<RuntimeExpr>,
        nominal: PythonQualifiedNameIr,
        constructor: bool,
    },
    LowNewtypeCtor {
        payload: Box<RuntimeExpr>,
        nominal: PythonQualifiedNameIr,
    },
    LowNewtypeProj {
        target: Box<RuntimeExpr>,
        nominal: PythonQualifiedNameIr,
    },
    LowClosureCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        name: String,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowIndirectCall {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        callee: Box<RuntimeExpr>,
        type_args: Vec<RuntimeType>,
        args: Vec<RuntimeExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowTypeApplication {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        callee: Box<RuntimeExpr>,
        #[serde(skip_serializing_if = "Option::is_none")]
        callee_plan: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        applied_plan: Option<usize>,
    },
    LowAbsurdCall {
        value_arg: Box<RuntimeExpr>,
    },
    LowCpsProjectorApply {
        #[serde(skip_serializing_if = "Option::is_none")]
        tail: Option<TailStage>,
        // These are the continuation type's erased leading `forall`
        // applications and first value-stage ABI arity. Keeping both on the
        // serialized node makes execution depend only on the already-routed
        // continuation contract, never on reopening its newtype declaration.
        continuation_type_stages: usize,
        continuation_abi_arity: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        continuation_plan: Option<usize>,
        nominal: PythonQualifiedNameIr,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        value_plan: Option<usize>,
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

pub(crate) fn package_ir(
    package: &Package<Routed>,
    stages: RuntimeStageRepresentation,
) -> Result<RuntimePackageIr, &'static str> {
    package
        .package_file()
        .ok_or("serialized runtime emit requires a package file")?;
    let bridged = crate::pass::resolve::bridged_module_paths(package)
        .into_iter()
        .collect();
    let plans = Rc::new(RefCell::new(BodyPlanCollector {
        stages,
        ..BodyPlanCollector::default()
    }));
    let modules = package
        .modules()
        .map(|(key, entry)| RuntimeModuleIr {
            key: key.to_owned(),
            module: runtime_module(package, key, &entry.module, &plans),
        })
        .collect();
    let collected = Rc::try_unwrap(plans)
        .expect("body serialization releases its lexical plan collectors")
        .into_inner();
    let body_value_plans = collected.values;
    let value_plans = body_value_plans.iter().map(runtime_value_plan).collect();
    Ok(RuntimePackageIr {
        bridged,
        modules,
        value_plans,
        body_value_plans,
    })
}

pub(crate) fn python_package_ir(
    package: &Package<Routed>,
    replayed: Option<&crate::sig::ReplayedInterface>,
) -> Result<PythonRuntimePackageIr, String> {
    let prepared = PreparedBoundaryCallableSites::collect(package, replayed)
        .map_err(|error| format!("cannot prepare Python boundary: {error}"))?;
    prepared_package_ir(package, &prepared)
}

pub(crate) fn prepared_package_ir(
    package: &Package<Routed>,
    prepared: &PreparedBoundaryCallableSites,
) -> Result<PythonRuntimePackageIr, String> {
    let package_ir =
        package_ir(package, RuntimeStageRepresentation::Erased).map_err(str::to_owned)?;
    let boundary = python_boundary_ir(package, prepared)?;
    Ok(PythonRuntimePackageIr {
        package: package_ir,
        boundary,
    })
}

fn python_boundary_ir(
    package: &Package<Routed>,
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
            Ok(PythonBoundarySiteIr {
                key: boundary_site_key(site.site()),
                module: site.site().module_segments().to_vec(),
                doc: if matches!(
                    site.site().owner(),
                    BoundaryFacadeSiteOwner::ExportedFunction { .. }
                ) {
                    super::boundary_docs::callable(package, site)
                        .map(|doc| super::doc::render(doc, super::doc::Dialect::PlainText))
                } else {
                    None
                },
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
                execution: python_callable_execution(site.plan().facade(), execution)?,
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
        frame: exact_nominal_identity_frame(name),
    }
}

pub(crate) fn exact_nominal_identity_frame(name: &QualifiedTypeName) -> String {
    use crate::backends::public_names::host_name_core;
    let mut out = format!("M{}_", name.module_segments().len());
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

pub(crate) fn boundary_site_key(site: &BoundaryFacadeSiteId) -> String {
    let owner = match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => vec!["host", name],
        BoundaryFacadeSiteOwner::ExportedFunction { name } => vec!["export", name],
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member } => {
            vec!["constructor", newtype, member]
        }
        BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
            vec!["projector", newtype, member]
        }
    };
    serde_json::to_string(&(site.module_segments(), owner))
        .expect("a prepared site identity is serializable")
}

fn runtime_value_plan(plan: &ValuePlan) -> RuntimeValuePlanIr {
    RuntimeValuePlanIr {
        uses: plan
            .uses()
            .iter()
            .map(|node| match node {
                ValueUse::Unit => RuntimeValueUseIr::Unit,
                ValueUse::Bottom => RuntimeValueUseIr::Bottom,
                ValueUse::Bound(binder) => RuntimeValueUseIr::Bound {
                    binder: binder.index(),
                },
                ValueUse::Nominal(name) => RuntimeValueUseIr::Nominal {
                    name: python_qualified_name(name),
                },
                ValueUse::Apply { constructor, args } => RuntimeValueUseIr::Apply {
                    constructor: constructor.index(),
                    args: args.iter().map(|id| id.index()).collect(),
                },
                ValueUse::Product { left, right } => RuntimeValueUseIr::Product {
                    left: left.index(),
                    right: right.index(),
                },
                ValueUse::Sum { left, right } => RuntimeValueUseIr::Sum {
                    left: left.index(),
                    right: right.index(),
                },
                ValueUse::Function { domain, result, .. } => RuntimeValueUseIr::Function {
                    domain: domain.index(),
                    result: result.index(),
                },
                ValueUse::Forall { binder, result } => RuntimeValueUseIr::Forall {
                    binder: binder.index(),
                    result: result.index(),
                },
            })
            .collect(),
        root: plan.root().index(),
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
    Ok(PythonCallableExecutionIr { head, root_uses })
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

fn runtime_module(
    package: &Package<Routed>,
    key: &str,
    module: &Module<Routed>,
    plans: &Rc<RefCell<BodyPlanCollector>>,
) -> RuntimeModule {
    let items = module
        .items
        .iter()
        .flat_map(|item| runtime_items(package, key, module, item, plans))
        .collect();
    RuntimeModule { items }
}

fn runtime_items(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    item: &Item<Routed>,
    plans: &Rc<RefCell<BodyPlanCollector>>,
) -> Vec<RuntimeItem> {
    match item {
        Item::FnDef(def) => {
            let mut scope =
                RuntimeTypeScope::sharing_plans(format!("fn:{module_key}:{}", def.name), plans);
            let value_plan = scope.value_plan(
                package,
                module,
                &def.sig.signature_ty(def.ret.clone(), def.meta.span),
            );
            let sig = runtime_signature(package, module, &def.sig, &mut scope);
            vec![RuntimeItem::FnDef(RuntimeFnDef {
                vis: runtime_visibility(&def.vis),
                name: def.name.clone(),
                sig,
                ret: runtime_type(package, module, &def.ret, &scope),
                value_plan,
                body: runtime_expr_expected(
                    package,
                    module_key,
                    module,
                    TailContext::body(&def.body),
                    &scope,
                    Some(&def.ret),
                ),
            })]
        }
        Item::Newtype(def) => vec![RuntimeItem::Newtype(runtime_newtype(
            package, module_key, module, def, plans,
        ))],
        Item::HostFn(def) => vec![RuntimeItem::HostFn(RuntimeHostFn {
            name: def.name.clone(),
        })],
        Item::HostType(def) => vec![RuntimeItem::HostType {
            nominal: python_qualified_name(
                &QualifiedTypeName::new(
                    module_key.split('/').map(str::to_owned).collect(),
                    def.name.clone(),
                )
                .expect("a checked host type has its exact declaring module"),
            ),
            role: def.role.as_ref().map(|annotation| annotation.role.as_str()),
        }],
        Item::TypeAlias(_) => Vec::new(),
        Item::TypeRecGroup(group) => group
            .members
            .iter()
            .filter_map(|member| match member {
                crate::ast::TypeRecMember::TypeAlias(_) => None,
                crate::ast::TypeRecMember::Newtype(newtype) => Some(RuntimeItem::Newtype(
                    runtime_newtype(package, module_key, module, newtype, plans),
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
    plans: &Rc<RefCell<BodyPlanCollector>>,
) -> RuntimeNewtype {
    let member_scope =
        RuntimeTypeScope::sharing_plans(format!("member:{module_key}:{}", def.name), plans);
    let member_plan = |member| {
        let scheme = super::reconstruct::newtype_member_scheme(package, module_key, def, member)
            .expect("a checked newtype member has its declaration-owned scheme");
        member_scope.value_plan(package, module, &scheme)
    };
    let mut scope =
        RuntimeTypeScope::sharing_plans(format!("newtype:{module_key}:{}", def.name), plans);
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
        nominal: python_qualified_name(
            &QualifiedTypeName::new(
                module_key.split('/').map(str::to_owned).collect(),
                def.name.clone(),
            )
            .expect("a routed declaration has an exact module path"),
        ),
        payload_plan: scope.value_plan(package, module, &def.payload),
        type_params,
        has_existentials: !def.existential_params.is_empty(),
        payload: runtime_type(package, module, &def.payload, &scope),
        constructor: RuntimeTypeMember {
            vis: runtime_visibility(&def.constructor.vis),
            name: def.constructor.name.clone(),
            value_plan: member_plan(&def.constructor.name),
        },
        projector: RuntimeTypeMember {
            vis: runtime_visibility(&def.projector.vis),
            name: def.projector.name.clone(),
            value_plan: member_plan(&def.projector.name),
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
            SignatureParam::Value(param) => {
                if let Some(ty) = &param.ty {
                    scope
                        .values
                        .get_mut()
                        .insert(param.name.clone(), ty.clone());
                } else {
                    scope.values.get_mut().remove(&param.name);
                }
                RuntimeSignatureParam::Value(RuntimeValueParam {
                    name: param.name.clone(),
                    ty: param
                        .ty
                        .as_ref()
                        .map(|ty| runtime_type(package, module, ty, scope)),
                })
            }
        })
        .collect();
    let mut stages = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                stages.extend(params.iter().map(|_| RuntimeBodyStage::Type))
            }
            crate::ast::SignatureGroupRef::Value(params) => stages.push(RuntimeBodyStage::Value {
                params: params
                    .iter()
                    .map(|param| {
                        let SignatureParam::Value(param) = param else {
                            unreachable!("a canonical value group contains value parameters")
                        };
                        param.name.clone()
                    })
                    .collect(),
            }),
        }
    }
    if stages.is_empty() {
        stages.push(RuntimeBodyStage::Value { params: Vec::new() });
    }
    RuntimeSignature {
        params,
        groups: sig.groups.iter().map(runtime_signature_group).collect(),
        stages,
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
            let mut nested_scope = scope.clone_type_scope();
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
    runtime_expr_expected(
        package,
        module_key,
        module,
        TailContext::operand(expr),
        scope,
        None,
    )
}

fn runtime_call_arguments(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    scope: &RuntimeTypeScope,
    scheme: &Type<Routed>,
    type_args: &[Type<Routed>],
    args: &[Expr<Routed>],
) -> Vec<RuntimeExpr> {
    if args.is_empty() {
        return Vec::new();
    }
    let type_args = type_args
        .iter()
        .map(|ty| {
            super::boundary_facade::canonical_routed_type(package, module, &scope.type_params, ty)
        })
        .collect::<Vec<_>>();
    let applied = super::reconstruct::instantiate_type_stages(scheme, &type_args)
        .expect("a routed call supplies exactly its leading type stages");
    let Type::Function { param, .. } = applied else {
        unreachable!("a routed value call reaches a function domain")
    };
    let slots = Type::right_spine_take(&param, args.len());
    assert_eq!(
        slots.len(),
        args.len(),
        "a routed call retains its exact argument partition"
    );
    args.iter()
        .zip(slots)
        .map(|(arg, ty)| {
            runtime_expr_expected(
                package,
                module_key,
                module,
                TailContext::operand(arg),
                scope,
                Some(ty),
            )
        })
        .collect()
}

fn runtime_expr_expected(
    package: &Package<Routed>,
    module_key: &str,
    module: &Module<Routed>,
    context: TailContext<'_>,
    scope: &RuntimeTypeScope,
    expected: Option<&Type<Routed>>,
) -> RuntimeExpr {
    let expr = context.expr();
    let tail = context.tail_stage();
    let nested = |expr: &Expr<Routed>| runtime_expr(package, module_key, module, expr, scope);
    let with_expected = |expr: &Expr<Routed>, expected: Option<&Type<Routed>>| {
        runtime_expr_expected(
            package,
            module_key,
            module,
            TailContext::operand(expr),
            scope,
            expected,
        )
    };
    let result = |edge, expected: Option<&Type<Routed>>| {
        runtime_expr_expected(
            package,
            module_key,
            module,
            context.result_child(edge),
            scope,
            expected,
        )
    };
    let empty_values = HashMap::new();
    let types = super::reconstruct::RoutedBodyTypes {
        package,
        module,
        type_params: &scope.type_params,
        values: &empty_values,
    };
    let nominal = |ty: &Type<Routed>| {
        python_qualified_name(
            &types
                .nominal(ty)
                .expect("a routed nominal operation retains its exact declaration identity"),
        )
    };
    let member_name = |owner: Option<&str>, name: &str| {
        let mut segments = owner
            .map(|owner| owner.split('/').map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        segments.push(name.to_owned());
        types
            .nominal(&Type::synth_path(segments, Vec::new(), expr.span()))
            .expect("a routed newtype operation has its exact declaration identity")
    };
    let member_argument = |name: &QualifiedTypeName, member: &str, args: &[Type<Routed>]| {
        let Some(super::value_plan::ValueNominalDeclaration::Newtype(declaration)) =
            super::value_plan::nominal_declaration(package, name)
        else {
            unreachable!("a routed newtype operation names its exact declaration")
        };
        let scheme = super::reconstruct::newtype_member_scheme(
            package,
            &name.module_segments().join("/"),
            declaration,
            member,
        )
        .expect("a checked newtype member has its declaration-owned scheme");
        let args = args
            .iter()
            .map(|ty| types.canonical(ty))
            .collect::<Vec<_>>();
        let Some(Type::Function { param, .. }) =
            super::reconstruct::instantiate_type_stages(&scheme, &args)
        else {
            unreachable!("a routed newtype operation supplies its payload value stage")
        };
        *param
    };
    match expr {
        crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
        Expr::FnExpr { sig, .. } => {
            let scheme = expected
                .map(|ty| types.canonical(ty))
                .or_else(|| scope.value_type(package, module, expr))
                .expect("a routed closure has annotations or a checked enclosing type");
            let (sig, ret) = super::reconstruct::signature_from_expected(sig, &scheme)
                .expect("a routed closure signature binds the stages of its checked type");
            let value_plan = scope.value_plan(package, module, &scheme);
            let mut nested_scope = scope.clone();
            RuntimeExpr::FnExpr {
                sig: runtime_signature(package, module, &sig, &mut nested_scope),
                value_plan,
                body: Box::new(runtime_expr_expected(
                    package,
                    module_key,
                    module,
                    context.result_child(ResultChild::FunctionBody),
                    &nested_scope,
                    Some(&ret),
                )),
            }
        }
        Expr::Let { name, value, .. } => {
            let ty = scope.value_type(package, module, value);
            RuntimeExpr::Let {
                name: name.clone(),
                value: Box::new(nested(value)),
                body: Box::new(
                    scope.with_value(name, ty, || result(ResultChild::LetBody, expected)),
                ),
            }
        }
        Expr::Seq { value, .. } => RuntimeExpr::Seq {
            value: Box::new(nested(value)),
            body: Box::new(result(ResultChild::SeqBody, expected)),
        },
        Expr::Unit { .. } => RuntimeExpr::Unit {},
        Expr::StrLit {
            value, annotation, ..
        } => RuntimeExpr::StrLit {
            value: value.clone(),
            host_type: nominal(annotation),
        },
        Expr::IntLit {
            digits, annotation, ..
        } => RuntimeExpr::IntLit {
            digits: digits.clone(),
            host_type: nominal(annotation),
        },
        Expr::FloatLit {
            digits, annotation, ..
        } => RuntimeExpr::FloatLit {
            digits: digits.clone(),
            host_type: nominal(annotation),
        },
        Expr::BoolLit {
            value, annotation, ..
        } => RuntimeExpr::BoolLit {
            value: *value,
            host_type: nominal(annotation),
        },
        Expr::EnrichedTuple {
            items, synth_ty, ..
        } => {
            let ty = types.canonical(synth_ty);
            let slots = Type::right_spine_take(&ty, items.len());
            assert_eq!(
                slots.len(),
                items.len(),
                "a routed tuple retains its exact item partition"
            );
            RuntimeExpr::EnrichedTuple {
                items: items
                    .iter()
                    .zip(slots)
                    .map(|(item, ty)| with_expected(item, Some(ty)))
                    .collect(),
            }
        }
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
            synth_ty,
            ..
        } => RuntimeExpr::EnrichedInject {
            payload: Box::new(with_expected(
                payload,
                Type::right_spine_sum_slot_for_arity(
                    &types.canonical(synth_ty),
                    *variant,
                    *variants,
                ),
            )),
            variant: *variant,
            variants: *variants,
        },
        Expr::EnrichedMatch {
            scrutinee,
            arms,
            scrutinee_ty,
            result_ty,
            ..
        } => {
            let scrutinee_ty = types.canonical(scrutinee_ty);
            RuntimeExpr::EnrichedMatch {
                scrutinee: Box::new(nested(scrutinee)),
                arms: arms
                    .iter()
                    .enumerate()
                    .map(|(index, arm)| {
                        let ty =
                            Type::right_spine_sum_slot_for_arity(&scrutinee_ty, index, arms.len())
                                .expect("a routed match arm fits its resolved sum arity");
                        RuntimeArm {
                            param: arm.param.clone(),
                            body: scope.with_value(&arm.param, Some(ty.clone()), || {
                                result(ResultChild::MatchArm(index), Some(result_ty))
                            }),
                        }
                    })
                    .collect(),
            }
        }
        Expr::EnrichedConditional {
            cond, result_ty, ..
        } => RuntimeExpr::EnrichedConditional {
            cond: Box::new(nested(cond)),
            host_type: nominal(
                &scope
                    .value_type(package, module, cond)
                    .expect("a routed condition retains its role type"),
            ),
            then_branch: Box::new(result(ResultChild::ThenBranch, Some(result_ty))),
            else_branch: Box::new(result(ResultChild::ElseBranch, Some(result_ty))),
        },
        Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            target_ty,
            ..
        } => {
            let field_nominal = nominal(
                super::reconstruct::nth_product_slot_ty_for_arity(
                    &types.canonical(target_ty),
                    *index,
                    *arity,
                )
                .expect("a routed field projection fits its resolved product arity"),
            );
            RuntimeExpr::EnrichedFieldGet {
                target: Box::new(nested(target)),
                nominal: field_nominal,
                index: *index,
                arity: *arity,
            }
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
            let target_module = module_for_key(package, module_path, module);
            let mut target_scope =
                RuntimeTypeScope::sharing_plans(format!("host:{module_path}:{name}"), &scope.plans);
            let scheme = super::boundary_facade::canonical_routed_type(
                package,
                target_module,
                &[],
                &sig.signature_ty(ret_ty.clone(), expr.span()),
            );
            let callee_plan = target_scope.value_plan(package, target_module, &scheme);
            let sig = runtime_signature(package, target_module, sig, &mut target_scope);
            RuntimeExpr::LowHostCall {
                name: name.clone(),
                module_path: module_path.clone(),
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: runtime_call_arguments(
                    package, module_key, module, scope, &scheme, type_args, args,
                ),
                sig,
                ret_ty: runtime_type(package, target_module, ret_ty, &target_scope),
                callee_plan,
                applied_plan: scope.applied_value_plan(package, module, &scheme, type_args),
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
            let mut target_scope =
                RuntimeTypeScope::sharing_plans(format!("fn:{target_key}:{mangled}"), &scope.plans);
            let scheme = super::reconstruct::module_function_scheme(
                package,
                &target_key,
                mangled,
                expr.span(),
            )
            .expect("a routed module call retains its resolved declaration scheme");
            let callee_plan = (!type_args.is_empty())
                .then(|| target_scope.value_plan(package, target_module, &scheme))
                .flatten();
            RuntimeExpr::LowModuleCall {
                tail,
                mangled: mangled.clone(),
                module_path: target_key,
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: runtime_call_arguments(
                    package, module_key, module, scope, &scheme, type_args, args,
                ),
                sig: runtime_signature(package, target_module, sig, &mut target_scope),
                callee_plan,
                applied_plan: scope.applied_value_plan(package, module, &scheme, type_args),
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
            let mut target_scope =
                RuntimeTypeScope::sharing_plans(format!("fn:{target_key}:{member}"), &scope.plans);
            let scheme = super::reconstruct::module_function_scheme(
                package,
                &target_key,
                member,
                expr.span(),
            )
            .expect("a routed qualified call retains its resolved declaration scheme");
            let callee_plan = (!type_args.is_empty())
                .then(|| target_scope.value_plan(package, target_module, &scheme))
                .flatten();
            RuntimeExpr::LowQualifiedModuleCall {
                tail,
                mangled: member.to_owned(),
                module_path: target_key,
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: runtime_call_arguments(
                    package, module_key, module, scope, &scheme, type_args, args,
                ),
                sig: runtime_signature(package, target_module, sig, &mut target_scope),
                callee_plan,
                applied_plan: scope.applied_value_plan(package, module, &scheme, type_args),
            }
        }
        Expr::LowQualifiedNewtypeMember {
            payload,
            module_path,
            newtype,
            member,
            type_args,
            ..
        } => {
            let name = member_name(Some(module_path), newtype);
            let Some(super::value_plan::ValueNominalDeclaration::Newtype(declaration)) =
                super::value_plan::nominal_declaration(package, &name)
            else {
                unreachable!("a routed newtype member names a newtype declaration")
            };
            RuntimeExpr::LowQualifiedNewtypeMember {
                payload: Box::new(with_expected(
                    payload,
                    Some(&member_argument(&name, member, type_args)),
                )),
                nominal: python_qualified_name(&name),
                constructor: member == &declaration.constructor.name,
            }
        }
        Expr::LowNewtypeCtor {
            payload,
            newtype,
            member,
            type_args,
            ..
        } => {
            let name = member_name(None, newtype);
            RuntimeExpr::LowNewtypeCtor {
                payload: Box::new(with_expected(
                    payload,
                    Some(&member_argument(&name, member, type_args)),
                )),
                nominal: python_qualified_name(&name),
            }
        }
        Expr::LowNewtypeProj {
            target,
            newtype,
            member,
            type_args,
            ..
        } => {
            let name = member_name(None, newtype);
            RuntimeExpr::LowNewtypeProj {
                target: Box::new(with_expected(
                    target,
                    Some(&member_argument(&name, member, type_args)),
                )),
                nominal: python_qualified_name(&name),
            }
        }
        Expr::LowClosureCall {
            name,
            type_args,
            args,
            ..
        } => {
            let scheme = scope
                .values
                .borrow()
                .get(name)
                .map(|ty| types.canonical(ty))
                .expect("a routed local callable has its lexical scheme");
            RuntimeExpr::LowClosureCall {
                tail,
                name: name.clone(),
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: runtime_call_arguments(
                    package, module_key, module, scope, &scheme, type_args, args,
                ),
                callee_plan: (!type_args.is_empty())
                    .then(|| scope.value_plan(package, module, &scheme))
                    .flatten(),
                applied_plan: scope.applied_value_plan(package, module, &scheme, type_args),
            }
        }
        Expr::LowIndirectCall {
            callee,
            type_args,
            args,
            ..
        } => {
            let scheme = scope
                .value_type(package, module, callee)
                .map(|ty| types.canonical(&ty))
                .expect("a routed indirect callable has its phase-owned scheme");
            RuntimeExpr::LowIndirectCall {
                tail,
                callee: Box::new(with_expected(callee, Some(&scheme))),
                type_args: type_args
                    .iter()
                    .map(|ty| runtime_type(package, module, ty, scope))
                    .collect(),
                args: runtime_call_arguments(
                    package, module_key, module, scope, &scheme, type_args, args,
                ),
                callee_plan: (!type_args.is_empty())
                    .then(|| scope.value_plan(package, module, &scheme))
                    .flatten(),
                applied_plan: scope.applied_value_plan(package, module, &scheme, type_args),
            }
        }
        Expr::LowTypeApplication {
            callee, type_arg, ..
        } => {
            let scheme = scope
                .value_type(package, module, callee)
                .map(|ty| types.canonical(&ty))
                .expect("a routed type application retains the callee's phase-owned scheme");
            RuntimeExpr::LowTypeApplication {
                tail,
                callee: Box::new(with_expected(callee, Some(&scheme))),
                callee_plan: scope.value_plan(package, module, &scheme),
                applied_plan: scope.applied_value_plan(
                    package,
                    module,
                    &scheme,
                    std::slice::from_ref(type_arg),
                ),
            }
        }
        Expr::LowAbsurdCall { value_arg, .. } => RuntimeExpr::LowAbsurdCall {
            value_arg: Box::new(nested(value_arg)),
        },
        Expr::LowCpsProjectorApply {
            newtype,
            module_path,
            receiver,
            continuation,
            continuation_ty,
            ..
        } => {
            let (continuation_type_stages, continuation_abi_arity) =
                cps_continuation_runtime_shape(continuation_ty);
            let continuation_plan = scope.value_plan(package, module, continuation_ty);
            RuntimeExpr::LowCpsProjectorApply {
                tail,
                continuation_type_stages,
                continuation_abi_arity,
                continuation_plan,
                nominal: python_qualified_name(&member_name(Some(module_path), newtype)),
                receiver: Box::new(nested(receiver)),
                continuation: Box::new(with_expected(continuation, Some(continuation_ty))),
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
            let mut target_scope =
                RuntimeTypeScope::sharing_plans(format!("host:{module_path}:{name}"), &scope.plans);
            let value_plan = target_scope.value_plan(
                package,
                target_module,
                &sig.signature_ty(ret_ty.clone(), expr.span()),
            );
            let sig = runtime_signature(package, target_module, sig, &mut target_scope);
            RuntimeExpr::LowHostFnValueRef {
                name: name.clone(),
                module_path: module_path.clone(),
                sig,
                ret_ty: runtime_type(package, target_module, ret_ty, &target_scope),
                value_plan,
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
    use crate::pass::route::route_package;
    use crate::pass::structural_recovery::recover_package;
    use crate::pipeline::Pipeline;
    use crate::prime::pipeline::PrimePipeline;
    use std::path::{Path, PathBuf};

    #[test]
    fn nominal_identity_frames_preserve_component_boundaries() {
        for (module, leaf, expected) in [
            ("api", "Box", "M1_C3_apiN3_Box"),
            ("a/b", "Box", "M2_C1_aC1_bN3_Box"),
            ("a_b", "Box", "M1_C2_aBN3_Box"),
            ("api_", "Box", "M1_C4_api_N3_Box"),
            ("data_api", "Native_count", "M1_C7_dataApiN11_NativeCount"),
        ] {
            let name = QualifiedTypeName::new(module.split('/').map(str::to_owned).collect(), leaf)
                .unwrap();
            assert_eq!(exact_nominal_identity_frame(&name), expected);
        }
    }

    #[test]
    fn erased_runtime_package_omits_admission_plans_and_keeps_execution_layouts() {
        let modules = vec![(
            PathBuf::from("x/main.kio"),
            parse(
                "module x/main; host type N; \
             pub newtype Box[A] : A { pub constructor make; pub projector get; }; \
             pub fn keep[A](value: Box(A)) -> Box(A) { value } \
             pub fn apply(callback: [A] A -> A, value: N) -> N { callback(N, value) }",
            )
            .unwrap(),
        )];
        let package_file =
            crate::pass::parser::parse_package_file("package fixture; bridge { x/main; }", None)
                .unwrap();
        let (modules, package_file) =
            FullPipeline::lower_package(modules, Some(package_file)).unwrap();
        let package = Package::build(
            Path::new(""),
            modules,
            package_file.map(|package_file| crate::pass::resolve::PackageFileEntry {
                file_path: PathBuf::from("fixture.pkg.kio"),
                package_name: "fixture".to_owned(),
                package_file,
            }),
        )
        .unwrap();
        package.resolve_imports().unwrap();
        package.check_in_body_resolution().unwrap();
        let package = routed(&FullPipeline::typecheck(&package).unwrap());
        let ir = python_package_ir(&package, None).unwrap();
        assert!(ir.package.body_value_plans.is_empty());
        let json = serde_json::to_value(ir).unwrap();
        assert!(json.get("value_plans").is_none());
        let sites = json["boundary"]["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 4);
        for site in sites {
            assert!(site.get("nominals").is_none());
            assert!(site["callable"].get("values").is_none());
            assert!(site["execution"].get("transparent_payloads").is_none());
            assert!(
                !site["callable"]["facade"]["uses"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                !site["execution"]["root_uses"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn newtype_inventory_retains_exact_identity_and_recursive_group_members() {
        let package = routed(&full_prime_modules(&[
            (
                "x/left.kio",
                "module x/left; host type H; type Alias = .; \
             newtype Mark : . { constructor make; projector get; }; \
             rec { newtype Tree : . | Forest { constructor make; projector get; }; \
             newtype Forest : . | Tree { constructor make; projector get; }; } \
             fn unit() -> . { () }",
            ),
            (
                "x/right.kio",
                "module x/right; \
             newtype Mark : . { constructor make; projector get; };",
            ),
        ]));
        let inventory: serde_json::Value =
            serde_json::from_str(&newtype_inventory_json(&package).unwrap()).unwrap();
        let entries = inventory.as_array().unwrap();
        assert_eq!(entries.len(), 4);
        let mut names = BTreeSet::new();
        let mut frames = BTreeSet::new();
        for entry in entries {
            let module = entry["module"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| part.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            names.insert((module, entry["name"].as_str().unwrap()));
            assert!(frames.insert(entry["frame"].as_str().unwrap()));
            assert_eq!(entry.as_object().unwrap().len(), 3);
        }
        assert_eq!(
            names,
            BTreeSet::from([
                ("x/left".to_owned(), "Mark"),
                ("x/right".to_owned(), "Mark"),
                ("x/left".to_owned(), "Tree"),
                ("x/left".to_owned(), "Forest"),
            ])
        );
    }

    #[test]
    fn boundary_nominal_payload_uses_bounded_storage() {
        assert!(
            std::mem::size_of::<super::super::boundary_facade::BoundaryNominalDeclaration>() <= 128
        );
    }

    #[test]
    fn body_plan_interning_retains_exact_structure_and_checks_hash_collisions() {
        let package = routed(&full_prime(
            "module x/main; fn keep[A](value: A) -> A { value } \
             fn same[A](value: A) -> A { value } fn unit() -> . { () }",
        ));
        let module = &package.module("x/main").unwrap().module;
        let collected = Rc::default();
        let emitted = runtime_module(&package, "x/main", module, &collected);
        let retained = emitted
            .items
            .iter()
            .filter_map(|item| match item {
                RuntimeItem::FnDef(def) if matches!(def.name.as_str(), "keep" | "same") => {
                    def.value_plan
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0], retained[1]);
        let plans = collected.borrow().values.clone();
        assert!(plans.len() > 1);
        let mut shared = BodyPlanCollector::default();
        for _ in 0..128 {
            assert_eq!(shared.intern(plans[0].clone()), 0);
        }
        assert_eq!(shared.values.len(), 1);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        plans[1].body_storage_identity().hash(&mut hash);
        shared.identities.insert(hash.finish(), vec![0]);
        assert_eq!(shared.intern(plans[1].clone()), 1);
        assert_eq!(shared.values, plans[..2]);
        assert_eq!(shared.intern(plans[1].clone()), 1);
    }

    #[test]
    fn typed_stage_serialization_retains_node_indices_and_execution_edges() {
        let package = routed(&full_prime(
            "module x/main; host type N role(i32); \
             fn keep[*F][A](value: (A & F(N)) | .) -> (A & F(N)) | . { value }",
        ));
        let module = &package.module("x/main").unwrap().module;
        let collected = Rc::default();
        runtime_module(&package, "x/main", module, &collected);
        for plan in &collected.borrow().values {
            let json = serde_json::to_value(runtime_value_plan(plan)).unwrap();
            assert_eq!(json.as_object().unwrap().len(), 2);
            assert_eq!(json["root"], plan.root().index());
            let uses = json["uses"].as_array().unwrap();
            assert_eq!(uses.len(), plan.uses().len());
            for (node, source) in uses.iter().zip(plan.uses()) {
                match source {
                    ValueUse::Function { domain, result, .. } => {
                        assert_eq!(node["kind"], "function");
                        assert_eq!(node["domain"], domain.index());
                        assert_eq!(node["result"], result.index());
                    }
                    ValueUse::Forall { binder, result } => {
                        assert_eq!(node["kind"], "forall");
                        assert_eq!(node["binder"], binder.index());
                        assert_eq!(node["result"], result.index());
                    }
                    ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                        assert_eq!(node["left"], left.index());
                        assert_eq!(node["right"], right.index());
                    }
                    ValueUse::Apply { constructor, args } => {
                        assert_eq!(node["constructor"], constructor.index());
                        assert_eq!(
                            node["args"],
                            serde_json::json!(args.iter().map(|id| id.index()).collect::<Vec<_>>())
                        );
                    }
                    ValueUse::Bound(binder) => assert_eq!(node["binder"], binder.index()),
                    ValueUse::Nominal(name) => {
                        assert_eq!(node["name"]["frame"], exact_nominal_identity_frame(name))
                    }
                    ValueUse::Unit => assert_eq!(node["kind"], "unit"),
                    ValueUse::Bottom => assert_eq!(node["kind"], "bottom"),
                }
            }
        }
    }

    #[test]
    fn repeated_scheme_preparation_scales_with_occurrences() {
        let product = std::iter::repeat_n("N", 16).collect::<Vec<_>>().join(" & ");
        let mut previous = None;
        for count in [8, 32, 128] {
            let mut source = String::from(
                "module x/main; host type N role(i32); fn id[T](value: T) -> T { value }\n",
            );
            for index in 0..count {
                source.push_str(&format!(
                    "fn f{index}(value: {product}) -> {product} {{ id(value) }}\n"
                ));
            }
            let package = routed(&full_prime(&source));
            let module = &package.module("x/main").unwrap().module;
            let mut measures = Vec::new();
            for stages in [
                RuntimeStageRepresentation::Erased,
                RuntimeStageRepresentation::Typed,
            ] {
                let plans = Rc::new(RefCell::new(BodyPlanCollector {
                    stages,
                    ..BodyPlanCollector::default()
                }));
                let body = runtime_module(&package, "x/main", module, &plans);
                let collected = plans.borrow();
                let serialized = collected
                    .values
                    .iter()
                    .map(runtime_value_plan)
                    .collect::<Vec<_>>();
                let json = serde_json::to_value(&body).unwrap();
                let mut pending = vec![&json];
                let mut applications = 0;
                while let Some(value) = pending.pop() {
                    match value {
                        serde_json::Value::Object(object) => {
                            if stages == RuntimeStageRepresentation::Erased {
                                assert!(object.keys().all(|key| !key.ends_with("_plan")));
                            }
                            applications += usize::from(object.contains_key("applied_plan"));
                            pending.extend(object.values());
                        }
                        serde_json::Value::Array(array) => pending.extend(array),
                        _ => {}
                    }
                }
                let plan_count = collected.values.len();
                let nodes = collected
                    .values
                    .iter()
                    .map(|plan| plan.uses().len())
                    .sum::<usize>();
                let bytes = serde_json::to_vec(&(&body, &serialized)).unwrap().len();
                if stages == RuntimeStageRepresentation::Erased {
                    assert_eq!((applications, plan_count, nodes), (0, 0, 0));
                    assert!(collected.identities.is_empty());
                } else {
                    assert_eq!(applications, count);
                    assert!(plan_count < count && nodes > plan_count);
                }
                measures.push((plan_count, nodes, bytes));
            }
            assert!(measures[0].2 < measures[1].2);
            if let Some(old) = previous {
                let old: Vec<(usize, usize, usize)> = old;
                for (current, old) in measures.iter().zip(old) {
                    assert!(current.0 <= old.0 * 5);
                    assert!(current.1 <= old.1 * 5);
                    assert!(current.2 <= old.2 * 5);
                }
            }
            previous = Some(measures);
        }
    }

    fn build_package<P>(modules: Vec<(PathBuf, Module<P>)>) -> Package<P>
    where
        P: crate::ast::Phase<FnBody = Expr<P>> + crate::pass::resolve::ResolvePhase + Clone,
    {
        let package = Package::build(Path::new(""), modules, None).expect("Package::build");
        package.resolve_imports().expect("resolve_imports");
        package
            .check_in_body_resolution()
            .expect("check_in_body_resolution");
        package
    }

    fn full_prime_modules(sources: &[(&str, &str)]) -> crate::prime::typer::ValidatedPrimePackage {
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

    fn full_prime(src: &str) -> crate::prime::typer::ValidatedPrimePackage {
        full_prime_modules(&[("x/main.kio", src)])
    }

    #[test]
    fn routed_newtype_member_schemes_preserve_nominal_identity_and_binders() {
        let package = routed(&full_prime_modules(&[
            (
                "lib/core.kio",
                "module lib/core; pub newtype Token : . { pub constructor make; pub projector read }",
            ),
            (
                "lib.kio",
                "module lib; pub newtype Token : . { pub constructor make; pub projector read }",
            ),
            (
                "api.kio",
                "module api;
                 import lib/core as lib;
                 import lib(Token);
                 pub newtype Wrapped[Param] <Hidden> : lib.Token & Token & Param & Hidden {
                   pub constructor wrap; pub projector unwrap
                 }
                 pub fn make[Param][Hidden](a: lib.Token, b: Token, p: Param, h: Hidden)
                   -> Wrapped(Param) { Wrapped.wrap((a, b, p, h)) }",
            ),
        ]));
        let module = &package.module("api").unwrap().module;
        let declaration = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Newtype(declaration) if declaration.name == "Wrapped" => Some(declaration),
                _ => None,
            })
            .unwrap();
        let constructor =
            super::super::reconstruct::newtype_member_scheme(&package, "api", declaration, "wrap")
                .unwrap();
        let projector = super::super::reconstruct::newtype_member_scheme(
            &package,
            "api",
            declaration,
            "unwrap",
        )
        .unwrap();
        let boundary_constructor =
            super::super::boundary_facade::prepare_newtype_constructor_boundary_callable(
                &package, "api", "Wrapped",
            )
            .unwrap();
        let boundary_projector =
            super::super::boundary_facade::prepare_newtype_projector_boundary_callable(
                &package, "api", "Wrapped",
            )
            .unwrap();
        assert_eq!(&constructor, boundary_constructor.exact_scheme());
        assert_eq!(&projector, boundary_projector.exact_scheme());

        let Type::Forall { param, body, .. } = &constructor else {
            panic!("universal constructor")
        };
        assert_eq!(param.name, "Param");
        let Type::Forall { param, body, .. } = body.as_ref() else {
            panic!("existential constructor")
        };
        assert_eq!(param.name, "Hidden");
        let Type::Function {
            param: constructor_payload,
            ..
        } = body.as_ref()
        else {
            panic!("constructor value stage")
        };
        let Type::Forall { param, body, .. } = &projector else {
            panic!("universal projector")
        };
        assert_eq!(param.name, "Param");
        let Type::Function { ret, .. } = body.as_ref() else {
            panic!("projector value stage")
        };
        let Type::Forall { body, .. } = ret.as_ref() else {
            panic!("CPS result binder")
        };
        let Type::Function { param, .. } = body.as_ref() else {
            panic!("CPS continuation")
        };
        let Type::Forall {
            param: hidden,
            body,
            ..
        } = param.as_ref()
        else {
            panic!("hidden continuation binder")
        };
        assert_eq!(hidden.name, "Hidden");
        let Type::Function {
            param: projector_payload,
            ..
        } = body.as_ref()
        else {
            panic!("continuation payload")
        };
        for payload in [constructor_payload, projector_payload] {
            let paths = Type::right_spine_product(payload)
                .iter()
                .map(|slot| {
                    let Type::Path { segments, args, .. } = slot else {
                        panic!("nominal or bound payload")
                    };
                    assert!(args.is_empty());
                    segments
                        .iter()
                        .map(|part| part.name.as_str())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                paths,
                [
                    vec!["lib", "core", "Token"],
                    vec!["lib", "Token"],
                    vec!["Param"],
                    vec!["Hidden"]
                ]
            );
        }
    }

    fn emitted_prime(
        package: &crate::prime::typer::ValidatedPrimePackage,
    ) -> (String, crate::prime::typer::ValidatedPrimePackage) {
        let mut source = String::new();
        let parsed = package
            .modules()
            .map(|(module_path, entry)| {
                let emitted = emit_module(entry);
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

    fn routed(package: &crate::prime::typer::ValidatedPrimePackage) -> Package<Routed> {
        route_package(&optimize_package(recover_package(package)))
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
        let def = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(def) if def.name == name => Some(def),
                _ => None,
            })
            .expect("selected definition");
        let mut scope = RuntimeTypeScope::new(format!("test-body:{module_path}:{name}"));
        let _ = runtime_signature(package, module, &def.sig, &mut scope);
        serde_json::to_value(runtime_expr_expected(
            package,
            module_path,
            module,
            TailContext::body(body),
            &scope,
            Some(&def.ret),
        ))
        .expect("serialize canonical body")
    }

    fn canonical_body(package: &Package<Routed>, name: &str) -> serde_json::Value {
        canonical_body_in(package, "x/main", name)
    }

    #[test]
    fn tail_markers_follow_checked_execution_stages_and_operand_boundaries() {
        let package = routed(&full_prime(
            "module x/main; \
             fn leaf() -> . { () } \
             fn type_leaf[A] -> . { () } \
             fn named() -> . { leaf() } \
             fn named_type() -> . { type_leaf(.) } \
             fn indirect(f: . -> .) -> . { f() } \
             fn indirect_type(f: [A] .) -> . { f(.) } \
             fn nested(f: . -> ., g: . -> .) -> . { g(f()) } \
             fn forwarded(f: . -> .) -> . -> . { .() { f() } } \
             fn hold(callback: . -> .) -> . -> . { callback } \
             fn saved(before: . -> ., after: . -> .) -> . -> . { \
               hold(.() { before(); after() }) \
             }",
        ));
        assert_eq!(
            canonical_body(&package, "named")["LowModuleCall"]["tail"],
            "value"
        );
        assert_eq!(
            canonical_body(&package, "named_type")["LowTypeApplication"]["tail"],
            "type"
        );
        assert_eq!(
            canonical_body(&package, "indirect")["LowClosureCall"]["tail"],
            "value"
        );
        assert_eq!(
            canonical_body(&package, "indirect_type")["LowTypeApplication"]["tail"],
            "type"
        );

        fn markers(value: &serde_json::Value, out: &mut Vec<(String, Option<String>)>) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (tag, data) in fields {
                        if tag == "LowClosureCall" {
                            out.push((
                                data["name"].as_str().unwrap().to_owned(),
                                data.get("tail")
                                    .map(|tail| tail.as_str().unwrap().to_owned()),
                            ));
                        }
                        markers(data, out);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        markers(value, out);
                    }
                }
                _ => {}
            }
        }
        let mut nested = Vec::new();
        markers(&canonical_body(&package, "nested"), &mut nested);
        nested.sort();
        assert_eq!(
            nested,
            [
                ("f".to_owned(), None),
                ("g".to_owned(), Some("value".to_owned()))
            ]
        );
        let forwarded = canonical_body(&package, "forwarded");
        assert_eq!(forwarded["LowBoundRef"]["name"], "f");
        let mut forwarded_calls = Vec::new();
        markers(&forwarded, &mut forwarded_calls);
        assert!(forwarded_calls.is_empty());
        let saved = canonical_body(&package, "saved");
        let closure = saved["LowModuleCall"]["args"][0]
            .get("FnExpr")
            .expect("non-forwarding closure remains a non-tail call operand");
        assert!(closure.get("tail").is_none());
        let mut saved_calls = Vec::new();
        markers(&closure["body"], &mut saved_calls);
        saved_calls.sort();
        assert_eq!(
            saved_calls,
            [
                ("after".to_owned(), Some("value".to_owned())),
                ("before".to_owned(), None)
            ]
        );
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
        serde_json::to_value(runtime_newtype(
            package,
            "x/main",
            module,
            def,
            &Rc::default(),
        ))
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
    fn monomorphic_calls_need_no_typed_stage_plan() {
        let package = routed(&full_prime(
            "module x/main; \
             newtype N : . { constructor make; projector get; }; \
             fn local(f: N -> N, value: N) -> N { f(value) } \
             fn computed(f: . -> (N -> N), value: N) -> N { f()(value) }",
        ));
        let module = &package.module("x/main").unwrap().module;
        for stages in [
            RuntimeStageRepresentation::Erased,
            RuntimeStageRepresentation::Typed,
        ] {
            let plans = Rc::new(RefCell::new(BodyPlanCollector {
                stages,
                ..BodyPlanCollector::default()
            }));
            let emitted = runtime_module(&package, "x/main", module, &plans);
            let json = serde_json::to_value(emitted).unwrap();
            let mut pending = vec![&json];
            let mut unknown = 0;
            while let Some(node) = pending.pop() {
                match node {
                    serde_json::Value::Object(object) => {
                        for tag in ["LowClosureCall", "LowIndirectCall"] {
                            if let Some(call) = object.get(tag) {
                                assert_eq!(call["type_args"], serde_json::json!([]));
                                assert!(call.get("applied_plan").is_none());
                                unknown += 1;
                            }
                        }
                        pending.extend(object.values());
                    }
                    serde_json::Value::Array(array) => pending.extend(array),
                    _ => {}
                }
            }
            assert_eq!(unknown, 3, "all and only the fixture's unknown calls fire");
            assert_eq!(
                plans.borrow().values.is_empty(),
                stages == RuntimeStageRepresentation::Erased
            );
        }
    }

    #[test]
    fn specialization_plans_retain_concrete_escaping_callable_contracts() {
        let prime = full_prime(
            "module x/main; \
             newtype N : . { constructor make; projector get; }; \
             fn id[A](value: A) -> A { value } \
             fn generic() -> [A] A -> A { id } \
             fn direct() -> N -> N { id(N) } \
             fn local(function: [A] A -> A) -> N -> N { function(N) } \
             fn computed() -> N -> N { generic()(N) }",
        );
        let (_, reparsed) = emitted_prime(&prime);
        for package in [routed(&prime), routed(&reparsed)] {
            let module = &package.module("x/main").unwrap().module;
            let plans = Rc::default();
            let emitted = runtime_module(&package, "x/main", module, &plans);
            let collected = plans.borrow();
            let plans = &collected.values;
            let mut checked = BTreeSet::new();
            for item in emitted.items {
                let RuntimeItem::FnDef(def) = item else {
                    continue;
                };
                if !matches!(def.name.as_str(), "direct" | "local" | "computed") {
                    continue;
                }
                let mut pending = vec![&def.body];
                let mut applications = Vec::new();
                let mut computed_producers = 0;
                while let Some(node) = pending.pop() {
                    match node {
                        RuntimeExpr::Let { value, body, .. } => {
                            pending.extend([value.as_ref(), body.as_ref()]);
                        }
                        RuntimeExpr::Seq { value, body } => {
                            pending.extend([value.as_ref(), body.as_ref()]);
                        }
                        RuntimeExpr::FnExpr { body, .. } => pending.push(body),
                        RuntimeExpr::LowTypeApplication {
                            callee,
                            applied_plan,
                            ..
                        } => {
                            applications.push(("type", applied_plan.unwrap()));
                            pending.push(callee);
                        }
                        RuntimeExpr::LowModuleCall {
                            mangled,
                            args,
                            applied_plan,
                            ..
                        } => {
                            computed_producers += usize::from(mangled == "generic");
                            applications.extend(applied_plan.map(|plan| ("call", plan)));
                            pending.extend(args);
                        }
                        RuntimeExpr::LowQualifiedModuleCall {
                            args, applied_plan, ..
                        }
                        | RuntimeExpr::LowClosureCall {
                            args, applied_plan, ..
                        } => {
                            applications.extend(applied_plan.map(|plan| ("call", plan)));
                            pending.extend(args);
                        }
                        RuntimeExpr::LowIndirectCall {
                            callee,
                            args,
                            applied_plan,
                            ..
                        } => {
                            applications.extend(applied_plan.map(|plan| ("indirect", plan)));
                            pending.push(callee);
                            pending.extend(args);
                        }
                        RuntimeExpr::Unit {}
                        | RuntimeExpr::LowBoundRef { .. }
                        | RuntimeExpr::LowModuleFnValueRef { .. } => {}
                        other => panic!(
                            "unexpected fixture expression: {}",
                            serde_json::to_string(other).unwrap()
                        ),
                    }
                }
                assert_eq!(
                    applications.len(),
                    1,
                    "{} must specialize once before its value stage escapes",
                    def.name
                );
                let (variant, applied) = applications[0];
                if def.name == "computed" {
                    assert_eq!(variant, "type");
                    assert_eq!(computed_producers, 1);
                    let RuntimeExpr::LowTypeApplication { callee, tail, .. } = &def.body else {
                        panic!("computed specialization executes before returning its value stage")
                    };
                    assert_eq!(*tail, Some(TailStage::Type));
                    assert!(
                        matches!(callee.as_ref(), RuntimeExpr::LowModuleCall { mangled, tail: None, .. }
                            if mangled == "generic"),
                        "the computed callee executes once as an operand before specialization"
                    );
                } else {
                    assert_eq!(computed_producers, 0);
                }
                let plan = &plans[applied];
                let ValueUse::Function { domain, result, .. } = plan.use_at(plan.root()) else {
                    panic!("specialization leaves a value stage, without executing it")
                };
                for value in [*domain, *result] {
                    let ValueUse::Nominal(name) = plan.use_at(value) else {
                        panic!("the concrete nominal must not become an erased binder")
                    };
                    assert_eq!(name.module_segments(), ["x", "main"]);
                    assert_eq!(name.name(), "N");
                }
                checked.insert(def.name);
            }
            assert_eq!(
                checked,
                BTreeSet::from([
                    "direct".to_owned(),
                    "local".to_owned(),
                    "computed".to_owned()
                ])
            );
        }
    }

    #[test]
    fn typed_stage_plans_preserve_abstract_slots_and_callable_stages() {
        let package = routed(&full_prime(
            "module x/main; \
             fn keep[*F][A](value: A & F(A)) -> A & F(A) { value }",
        ));
        let module = &package.module("x/main").unwrap().module;
        let plans = Rc::default();
        let emitted = runtime_module(&package, "x/main", module, &plans);
        let RuntimeItem::FnDef(def) = &emitted.items[0] else {
            panic!("the fixture has one function")
        };
        let collected = plans.borrow();
        let plan = &collected.values[def.value_plan.unwrap()];
        let json = serde_json::to_value(runtime_value_plan(plan)).unwrap();
        let kinds = json["uses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["kind"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert!(kinds.is_superset(&BTreeSet::from([
            "bound", "apply", "product", "function", "forall",
        ])));
    }

    #[test]
    fn private_value_plans_cover_closures_type_stages_and_nominal_payloads() {
        let prime = full_prime(
            "module x/main; \
             newtype Box[T] : T { constructor make; projector get; }; \
             fn hold[A](value: A) -> . -> A { .(_unit) { value } } \
             fn apply[A](function: [B] B -> B, value: A) -> A { function(A, value) }",
        );
        let (_, reparsed) = emitted_prime(&prime);
        for package in [routed(&prime), routed(&reparsed)] {
            let module = &package.module("x/main").unwrap().module;
            let plans = Rc::default();
            let emitted = runtime_module(&package, "x/main", module, &plans);
            let collected = plans.borrow();
            let plans = &collected.values;
            let mut saw_closure = false;
            let mut saw_application = false;
            let mut saw_payload = false;
            for item in emitted.items {
                match item {
                    RuntimeItem::FnDef(def) => {
                        assert!(matches!(
                            plans[def.value_plan.unwrap()]
                                .use_at(plans[def.value_plan.unwrap()].root()),
                            ValueUse::Forall { .. }
                        ));
                        match def.body {
                            RuntimeExpr::FnExpr { value_plan, .. } => {
                                let plan = &plans[value_plan.unwrap()];
                                assert!(matches!(
                                    plan.use_at(plan.root()),
                                    ValueUse::Function { .. }
                                ));
                                assert_eq!(
                                    plan.free_binders(plan.root(), plan.ambient_binders()).len(),
                                    1
                                );
                                saw_closure = true;
                            }
                            RuntimeExpr::LowClosureCall {
                                callee_plan: Some(id),
                                ..
                            } => {
                                let plan = &plans[id];
                                assert!(matches!(
                                    plan.use_at(plan.root()),
                                    ValueUse::Forall { .. }
                                ));
                                saw_application = true;
                            }
                            _ => {}
                        }
                    }
                    RuntimeItem::Newtype(def) => {
                        let plan = &plans[def.payload_plan.unwrap()];
                        assert_eq!(def.nominal.module, ["x", "main"]);
                        assert_eq!(def.nominal.name, "Box");
                        assert_eq!(plan.ambient_binders().len(), 1);
                        assert!(matches!(plan.use_at(plan.root()), ValueUse::Bound(_)));
                        saw_payload = true;
                    }
                    RuntimeItem::HostFn(_) | RuntimeItem::HostType { .. } => {}
                }
            }
            assert!(saw_closure && saw_application && saw_payload);
        }
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
        assert_eq!(cps["tail"], "value");
        assert_eq!(cps["continuation_type_stages"], 2);
        assert_eq!(cps["continuation_abi_arity"], 1);
        assert!(cps.get("module_path").is_none());
        assert!(cps.get("newtype").is_none());
    }

    #[test]
    fn field_update_generated_binder_does_not_capture_later_rhs() {
        let (source, collision) = generated_field_rhs_collision_source();
        let full = full_prime(&source);
        let module = full.module("x/main").expect("module").module();
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
                runtime_items(&package, module_path, module, item, &Rc::default())
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
             host fn loop[S][R](step: S -> (S | R), state: S) -> R; \
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

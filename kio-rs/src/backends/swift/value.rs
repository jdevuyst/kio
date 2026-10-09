//! Swift realization of the shared canonical value plan.
//!
//! Native types and private witnesses are two renderings of the same node.
//! Witnesses come from lexical type stages; payloads never select a type.

use std::collections::BTreeMap;

use crate::backends::boundary_facade::{
    BoundaryFacadeSupportOrigin, BoundaryNominalDeclaration, BoundaryNominalDependencies,
    FacadeBinderId, QualifiedTypeName,
};
use crate::backends::value_plan::{
    ValueCapture, ValueNominalDeclaration, ValuePlan, ValueStageShape, ValueUse, ValueUseId,
    canonical_stage_symbol as shape_carrier_name, nominal_declaration,
};

use super::emit::EmitError;
use super::skin::PreparedSwiftShapes;

pub(super) type Scope = BTreeMap<FacadeBinderId, String>;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NativeLeaf<'a> {
    Unit,
    Bottom,
    Nominal(&'a QualifiedTypeName),
}

pub(super) struct Types<'a, 'p> {
    pub plan: &'a ValuePlan,
    pub shapes: &'a PreparedSwiftShapes<'p>,
    pub retained_nominals: Option<&'a BoundaryNominalDependencies>,
    pub origin: BoundaryFacadeSupportOrigin,
}

impl Types<'_, '_> {
    fn stage(&self, root: ValueUseId) -> Result<ValueStageShape, EmitError> {
        self.plan.stage_shape(root, |name| {
            if let Some(declaration) = self
                .retained_nominals
                .and_then(|nominals| nominals.declaration(name))
            {
                let params = match declaration {
                    BoundaryNominalDeclaration::HostType { type_params, .. }
                    | BoundaryNominalDeclaration::Newtype { type_params, .. } => type_params,
                };
                return Ok(params
                    .iter()
                    .rev()
                    .fold(crate::ast::Kind::Star, |result, param| {
                        crate::ast::Kind::Arrow(Box::new(param.kind().clone()), Box::new(result))
                    }));
            }
            nominal_declaration(self.shapes.package(), name)
                .map(|declaration| declaration.kind())
                .ok_or_else(|| EmitError::internal("Swift staged value lost its nominal kind"))
        })
    }

    fn capture_native(
        &self,
        capture: &ValueCapture,
        scope: &Scope,
        host: &str,
    ) -> Result<String, EmitError> {
        match capture {
            ValueCapture::Use(id) => self.native(*id, scope, host),
            ValueCapture::ApplicationPrefix { constructor, args } => {
                let mut native = self.native(*constructor, scope, host)?;
                for arg in args {
                    native = format!("KioApply1<{native}, {}>", self.native(*arg, scope, host)?);
                }
                Ok(native)
            }
        }
    }

    fn staged_native(
        &self,
        root: ValueUseId,
        scope: &Scope,
        host: &str,
    ) -> Result<String, EmitError> {
        let shape = self.stage(root)?;
        let scope = shape
            .plan
            .ambient_binders()
            .iter()
            .copied()
            .zip(shape.captures.iter())
            .map(|(binder, capture)| Ok((binder, self.capture_native(capture, scope, host)?)))
            .collect::<Result<Scope, EmitError>>()?;
        Types {
            plan: &shape.plan,
            shapes: self.shapes,
            retained_nominals: self.retained_nominals,
            origin: self.origin,
        }
        .forall(shape.plan.root(), &scope, host)
    }

    fn nominal_shape(&self, name: &QualifiedTypeName) -> Result<(bool, usize), EmitError> {
        if let Some(declaration) = self
            .retained_nominals
            .and_then(|nominals| nominals.declaration(name))
        {
            return Ok(match declaration {
                BoundaryNominalDeclaration::HostType { type_params, .. } => {
                    (true, type_params.len())
                }
                BoundaryNominalDeclaration::Newtype { type_params, .. } => {
                    (false, type_params.len())
                }
            });
        }
        if let Some(declaration) = nominal_declaration(self.shapes.package(), name) {
            Ok(match declaration {
                ValueNominalDeclaration::Host(declaration) => (true, declaration.type_params.len()),
                ValueNominalDeclaration::Newtype(declaration) => {
                    (false, declaration.type_params.len())
                }
            })
        } else {
            Err(EmitError::internal(
                "Swift value plan lost its exact nominal declaration",
            ))
        }
    }

    fn nominal(&self, name: &QualifiedTypeName, host: &str) -> Result<String, EmitError> {
        let (is_host, arity) = self.nominal_shape(name)?;
        Ok(match (is_host, arity) {
            (true, 0) => format!(
                "{host}.{}",
                super::emit::host_type_member_name(&name.module_segments().join("/"), name.name())
            ),
            (true, _) => super::skin::swift_host_constructor_name(name),
            (false, 0) => format!("{}<{host}>", super::skin::swift_newtype_carrier_name(name)),
            (false, _) => format!(
                "{}<{host}>",
                super::skin::swift_newtype_constructor_name(name)
            ),
        })
    }

    pub fn native(&self, root: ValueUseId, scope: &Scope, host: &str) -> Result<String, EmitError> {
        enum Task {
            Use(ValueUseId),
            Text(&'static str),
        }
        let mut pending = vec![Task::Use(root)];
        let mut rendered = String::new();
        while let Some(task) = pending.pop() {
            let id = match task {
                Task::Text(text) => {
                    rendered.push_str(text);
                    continue;
                }
                Task::Use(id) => id,
            };
            match self.plan.use_at(id) {
                ValueUse::Unit => rendered.push_str("KioUnit"),
                ValueUse::Bottom => rendered.push_str("Never"),
                ValueUse::Bound(binder) => {
                    rendered.push_str(scope.get(binder).ok_or_else(|| {
                        EmitError::internal("Swift native value has an unbound lexical binder")
                    })?)
                }
                ValueUse::Nominal(name) => rendered.push_str(&self.nominal(name, host)?),
                ValueUse::Apply { constructor, args } => {
                    for arg in args.iter().rev() {
                        rendered.push_str("KioApply1<");
                        pending.push(Task::Text(">"));
                        pending.push(Task::Use(*arg));
                        pending.push(Task::Text(", "));
                    }
                    pending.push(Task::Use(*constructor));
                }
                ValueUse::Product { left, right } | ValueUse::Sum { left, right } => {
                    rendered.push_str(
                        if matches!(self.plan.use_at(id), ValueUse::Product { .. }) {
                            "Product<"
                        } else {
                            "Sum<"
                        },
                    );
                    pending.extend([
                        Task::Text(">"),
                        Task::Use(*right),
                        Task::Text(", "),
                        Task::Use(*left),
                    ]);
                }
                ValueUse::Function { domain, result, .. } => {
                    rendered.push('(');
                    // A function substituted into a generic domain is escaping.
                    // Keep the concrete spelling identical to that convention.
                    if matches!(self.plan.use_at(*domain), ValueUse::Function { .. }) {
                        rendered.push_str("@escaping ");
                    }
                    pending.extend([Task::Use(*result), Task::Text(") -> "), Task::Use(*domain)]);
                }
                ValueUse::Forall { .. } => rendered.push_str(&self.staged_native(id, scope, host)?),
            }
        }
        Ok(rendered)
    }

    fn shape_name(&self, root: ValueUseId, ambient: &[FacadeBinderId]) -> String {
        let identity = self.plan.identity(root, ambient);
        shape_carrier_name(&identity)
    }

    pub fn forall_name(&self, root: ValueUseId) -> Result<String, EmitError> {
        let shape = self.stage(root)?;
        let identity = shape
            .plan
            .identity(shape.plan.root(), shape.plan.ambient_binders());
        Ok(shape_carrier_name(&identity))
    }

    fn native_leaf(&self, id: ValueUseId) -> Result<Option<NativeLeaf<'_>>, EmitError> {
        Ok(match self.plan.use_at(id) {
            ValueUse::Unit => Some(NativeLeaf::Unit),
            ValueUse::Bottom => Some(NativeLeaf::Bottom),
            ValueUse::Nominal(name) if self.nominal_shape(name)? != (false, 0) => {
                Some(NativeLeaf::Nominal(name))
            }
            _ => None,
        })
    }

    /// Emit one private witness expression. Each local statement denotes one
    /// shared-plan node, avoiding exponentially duplicated type expressions.
    pub fn witness(
        &self,
        root: ValueUseId,
        scope: &Scope,
        host: &str,
    ) -> Result<String, EmitError> {
        if self.native_leaf(root)?.is_some() {
            return Ok(format!(
                "__KioNativeWitness<{}>()",
                self.native(root, &Scope::new(), host)?
            ));
        }
        let mut pending = vec![(root, false)];
        let mut names = BTreeMap::<usize, String>::new();
        // This prelude has one host type and lexical witness environment.
        // Empty native factories can reuse a local; binder/stage witnesses cannot.
        let mut native_leaves = BTreeMap::<NativeLeaf<'_>, String>::new();
        let mut stages = BTreeMap::<usize, ValueStageShape>::new();
        let mut statements = String::new();
        while let Some((id, ready)) = pending.pop() {
            if names.contains_key(&id.index()) {
                continue;
            }
            let native_leaf = self.native_leaf(id)?;
            if let Some(name) = native_leaf.and_then(|leaf| native_leaves.get(&leaf)) {
                names.insert(id.index(), name.clone());
                continue;
            }
            if !ready {
                pending.push((id, true));
                let mut dependencies = Vec::new();
                match self.plan.use_at(id) {
                    ValueUse::Product { left, right }
                    | ValueUse::Sum { left, right }
                    | ValueUse::Function {
                        domain: left,
                        result: right,
                        ..
                    } => {
                        dependencies.extend([*left, *right]);
                    }
                    ValueUse::Apply { constructor, args } => {
                        dependencies.push(*constructor);
                        dependencies.extend(args);
                    }
                    ValueUse::Forall { .. } => {
                        let shape = self.stage(id)?;
                        for capture in &shape.captures {
                            match capture {
                                ValueCapture::Use(id) => dependencies.push(*id),
                                ValueCapture::ApplicationPrefix { constructor, args } => {
                                    dependencies.push(*constructor);
                                    dependencies.extend(args);
                                }
                            }
                        }
                        stages.insert(id.index(), shape);
                    }
                    _ => {}
                }
                pending.extend(
                    dependencies
                        .into_iter()
                        .rev()
                        .map(|dependency| (dependency, false)),
                );
                continue;
            }
            let expression = match self.plan.use_at(id) {
                ValueUse::Bound(binder) => scope.get(binder).cloned().ok_or_else(|| {
                    EmitError::internal("Swift witness has an unbound lexical binder")
                })?,
                ValueUse::Product { left, right }
                | ValueUse::Sum { left, right }
                | ValueUse::Function {
                    domain: left,
                    result: right,
                    ..
                } => {
                    let helper = match self.plan.use_at(id) {
                        ValueUse::Product { .. } => "__kioProductType",
                        ValueUse::Sum { .. } => "__kioSumType",
                        _ => "__kioFunctionType",
                    };
                    format!(
                        "{helper}({}, {})",
                        names[&left.index()],
                        names[&right.index()]
                    )
                }
                ValueUse::Apply { constructor, args } => {
                    let mut result = names[&constructor.index()].clone();
                    for (index, arg) in args.iter().enumerate() {
                        let name = format!("__kioApply{}_{}", id.index(), index);
                        statements.push_str(&format!(
                            "let {name} = __kioApplyType({result}, {}); ",
                            names[&arg.index()]
                        ));
                        result = name;
                    }
                    result
                }
                ValueUse::Forall { .. } => {
                    let shape = stages
                        .remove(&id.index())
                        .expect("a queued forall has its factored shape");
                    let native_scope = shape
                        .plan
                        .ambient_binders()
                        .iter()
                        .enumerate()
                        .map(|(index, binder)| (*binder, format!("W{index}.Native")))
                        .collect();
                    let shaped_types = Types {
                        plan: &shape.plan,
                        shapes: self.shapes,
                        retained_nominals: self.retained_nominals,
                        origin: self.origin,
                    };
                    shaped_types.forall(shape.plan.root(), &native_scope, host)?;
                    let name =
                        shaped_types.shape_name(shape.plan.root(), shape.plan.ambient_binders());
                    let mut args = vec![format!("{host}.self")];
                    for (capture_index, capture) in shape.captures.iter().enumerate() {
                        let captured = match capture {
                            ValueCapture::Use(id) => names[&id.index()].clone(),
                            ValueCapture::ApplicationPrefix { constructor, args } => {
                                let mut captured = names[&constructor.index()].clone();
                                for (index, arg) in args.iter().enumerate() {
                                    let name = format!(
                                        "__kioCapture{}_{}_{}",
                                        id.index(),
                                        capture_index,
                                        index
                                    );
                                    statements.push_str(&format!(
                                        "let {name} = __kioApplyType({captured}, {}); ",
                                        names[&arg.index()]
                                    ));
                                    captured = name;
                                }
                                captured
                            }
                        };
                        args.push(captured);
                    }
                    format!("__kioWitness_{name}({})", args.join(", "))
                }
                ValueUse::Nominal(name) if self.nominal_shape(name)? == (false, 0) => {
                    format!(
                        "__kioWitness_{}({host}.self)",
                        super::skin::swift_newtype_carrier_name(name)
                    )
                }
                _ => format!(
                    "__KioNativeWitness<{}>()",
                    self.native(id, &Scope::new(), host)?
                ),
            };
            let name = format!("__kioType{}", id.index());
            statements.push_str(&format!(
                "let {name}: any __KioTypeWitness = {expression}; "
            ));
            if let Some(leaf) = native_leaf {
                native_leaves.insert(leaf, name.clone());
            }
            names.insert(id.index(), name);
        }
        Ok(format!(
            "{{ () -> any __KioTypeWitness in {statements}return {} }}()",
            names[&root.index()]
        ))
    }

    fn forall(&self, root: ValueUseId, scope: &Scope, host: &str) -> Result<String, EmitError> {
        let ambient = scope.keys().copied().collect::<Vec<_>>();
        let free = self.plan.free_binders(root, &ambient);
        let name = self.shape_name(root, &ambient);
        let mut actual = vec![host.to_owned()];
        actual.extend(free.iter().map(|binder| scope[binder].clone()));
        let actual = format!("{name}<{}>", actual.join(", "));
        if self.shapes.has_forall(&name, self.origin) {
            return Ok(actual);
        }
        let mut declaration_scope = Scope::new();
        let outer_types = (0..free.len())
            .map(|index| format!("Outer{index}"))
            .collect::<Vec<_>>();
        for (binder, ty) in free.iter().zip(&outer_types) {
            declaration_scope.insert(*binder, ty.clone());
        }
        let ValueUse::Forall { binder, result } = self.plan.use_at(root) else {
            unreachable!("Swift forall declarations come from a shared Forall node")
        };
        declaration_scope.insert(*binder, "T".to_owned());
        let result_type = self.native(*result, &declaration_scope, "H")?;
        let mut declarations = vec![format!("H: {}", self.shapes.host_protocol())];
        declarations.extend(outer_types.clone());
        let declaration_generic = super::skin::swift_generic_use(&declarations);
        let mut constraints = vec!["Implementation.H == H".to_owned()];
        constraints.extend(
            outer_types
                .iter()
                .map(|ty| format!("Implementation.{ty} == {ty}")),
        );
        let associated = outer_types
            .iter()
            .map(|ty| format!("    associatedtype {ty}\n"))
            .collect::<String>();
        let mut witness_decl = vec![format!("H: {}", self.shapes.host_protocol())];
        witness_decl.extend((0..free.len()).map(|index| format!("W{index}: __KioTypeWitness")));
        let mut witness_native = vec!["H".to_owned()];
        witness_native.extend((0..free.len()).map(|index| format!("W{index}.Native")));
        let mut witness_use = vec!["H".to_owned()];
        witness_use.extend((0..free.len()).map(|index| format!("W{index}")));
        let mut witness_args = vec!["_: H.Type".to_owned()];
        witness_args.extend((0..free.len()).map(|index| format!("_: W{index}")));
        let declaration = format!(
            "\npublic protocol {name}Implementation {{\n    associatedtype H: {}\n{associated}    func specialize<T>(_: T.Type) -> {result_type}\n}}\n\npublic struct {name}{declaration_generic}: __KioForallValue {{\n    private let body: (any __KioTypeWitness) -> Any\n    init(__kioBody body: @escaping (any __KioTypeWitness) -> Any) {{ self.body = body }}\n    func __kioAdvance(_ witness: any __KioTypeWitness) -> Any {{ body(witness) }}\n    public func specialize<T>(asKioType _: T.Type = T.self) -> {result_type} {{ __kioChecked(body(__KioNativeWitness<T>()), as: ({result_type}).self) }}\n    public init<Implementation: {name}Implementation>(_ implementation: Implementation) where {} {{\n        func advance<W: __KioTypeWitness>(_ witness: W) -> Any {{ implementation.specialize(W.Native.self) as Any }}\n        self.body = {{ witness in advance(witness) }}\n    }}\n}}\n\nprivate struct __KioWitness_{name}<{}>: __KioForallWitness {{\n    typealias Native = {name}<{}>\n    func construct(_ body: @escaping (any __KioTypeWitness) -> Any) -> Any {{ Native(__kioBody: body) }}\n}}\nfunc __kioWitness_{name}<{}>({}) -> any __KioTypeWitness {{ __KioWitness_{name}<{}>() }}\n",
            self.shapes.host_protocol(),
            constraints.join(", "),
            witness_decl.join(", "),
            witness_native.join(", "),
            witness_decl.join(", "),
            witness_args.join(", "),
            witness_use.join(", ")
        );
        self.shapes.register_forall(name, declaration, self.origin);
        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::shape_carrier_name;

    #[test]
    fn carrier_spelling_reversibly_frames_every_shape_byte() {
        for identity in ["", "ab", "a_b", "a/b", "A(B)", "λ"] {
            let name = shape_carrier_name(identity);
            let bytes = name.strip_prefix("KioForall_").unwrap().as_bytes();
            let decoded = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(decoded, identity.as_bytes());
        }
        assert_ne!(shape_carrier_name("a_b"), shape_carrier_name("a/b"));
    }
}

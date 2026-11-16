use super::*;

#[derive(Clone)]
pub(super) enum ProbeArgs {
    Type(ReflectedType),
    Packet(Value),
    Flattened(Vec<Value>),
}

impl ProbeArgs {
    pub fn into_eval(self) -> EvalArgs {
        match self {
            Self::Type(_) => unreachable!("type probes use the type-stage continuation"),
            Self::Packet(value) => EvalArgs::One([value]),
            Self::Flattened(values) => EvalArgs::Many(values),
        }
    }
}

#[derive(Clone, Copy)]
struct Scope {
    index: usize,
    swapped: bool,
}

#[derive(Default)]
struct Names {
    left: HashMap<String, String>,
    right: HashMap<String, String>,
    fresh: usize,
}

enum Control {
    Equal(Value, Value, Scope),
    Eta(Value),
    Value(Value),
    Bool(bool),
    Apply(Value, ProbeArgs),
}

enum Frame {
    EqualLeft {
        right: Value,
        scope: Scope,
    },
    EqualRight {
        left: Value,
        scope: Scope,
    },
    EtaDone(Option<Instant>),
    EtaProbe {
        original: Value,
        base: Value,
    },
    ScopePop,
    Pairs {
        pairs: Pairs,
        scope: Scope,
    },
    StuckArgs {
        left: EvalRef<Vec<Value>>,
        right: EvalRef<Vec<Value>>,
        scope: Scope,
    },
    CaseLeft {
        left: Value,
        right: Value,
        scope: Scope,
    },
    CaseRight {
        left: Value,
        right: Value,
        scope: Scope,
    },
    Restore {
        left: String,
        right: String,
        prior_left: Option<String>,
        prior_right: Option<String>,
        scope: Scope,
    },
    ProbeLeft {
        other: Value,
        args: ProbeArgs,
        scope: Scope,
    },
    ProbeRight {
        left: Value,
        scope: Scope,
    },
}

enum Pairs {
    Data {
        left: EvalRef<DataValue>,
        right: EvalRef<DataValue>,
        next: usize,
    },
    Seq {
        left: [EvalRef<Value>; 2],
        right: [EvalRef<Value>; 2],
        next: usize,
    },
    Stuck {
        left: Args,
        right: Args,
    },
}

enum Tail {
    Spine(EvalRef<DataValue>),
    Last(EvalRef<DataValue>),
}

struct Args {
    values: EvalRef<Vec<Value>>,
    next: usize,
    tail: Option<Tail>,
}

impl Args {
    fn new(values: EvalRef<Vec<Value>>) -> Self {
        Self {
            values,
            next: 0,
            tail: None,
        }
    }

    fn pair(value: &Value) -> Option<EvalRef<DataValue>> {
        if let Value::Data(data) = value
            && matches!(&data.identity, DataConstructorIdentity::Core(name) if name == "__pair__")
            && data.args().len() == 2
        {
            Some(data.clone())
        } else {
            None
        }
    }

    fn next(&mut self) -> Option<Value> {
        if self.next < self.values.len() {
            let value = &self.values[self.next];
            self.next += 1;
            if self.next < self.values.len() {
                return Some(value.clone());
            }
            if let Some(pair) = Self::pair(value) {
                self.tail = Some(Tail::Spine(pair));
            } else {
                return Some(value.clone());
            }
        }
        match self.tail.take()? {
            Tail::Last(pair) => Some(pair.args()[1].clone()),
            Tail::Spine(pair) => {
                let left = pair.args()[0].clone();
                self.tail = Some(match Self::pair(&pair.args()[1]) {
                    Some(next) => Tail::Spine(next),
                    None => Tail::Last(pair),
                });
                Some(left)
            }
        }
    }
}

impl Pairs {
    fn next(&mut self) -> Result<Option<(Value, Value)>, ()> {
        match self {
            Self::Data { left, right, next } => {
                let pair = left
                    .args()
                    .get(*next)
                    .zip(right.args().get(*next))
                    .map(|(a, b)| (a.clone(), b.clone()));
                *next += 1;
                Ok(pair)
            }
            Self::Seq { left, right, next } => {
                let pair = left
                    .get(*next)
                    .zip(right.get(*next))
                    .map(|(a, b)| (a.as_ref().clone(), b.as_ref().clone()));
                *next += 1;
                Ok(pair)
            }
            Self::Stuck { left, right } => match (left.next(), right.next()) {
                (Some(left), Some(right)) => Ok(Some((left, right))),
                (None, None) => Ok(None),
                _ => Err(()),
            },
        }
    }
}

pub(super) enum Outcome {
    Apply(Value, ProbeArgs),
    Done(Value),
}

pub(super) struct Engine {
    control: Option<Control>,
    frames: Vec<Frame>,
    names: Vec<Names>,
}

impl Engine {
    pub fn equal(left: Value, right: Value) -> Self {
        Self {
            control: Some(Control::Equal(
                left,
                right,
                Scope {
                    index: 0,
                    swapped: false,
                },
            )),
            frames: Vec::new(),
            names: vec![Names::default()],
        }
    }

    pub fn resume(&mut self, value: Value) {
        self.control = Some(Control::Value(value));
    }

    fn maps(
        &mut self,
        scope: Scope,
    ) -> (&mut HashMap<String, String>, &mut HashMap<String, String>) {
        let names = &mut self.names[scope.index];
        if scope.swapped {
            (&mut names.right, &mut names.left)
        } else {
            (&mut names.left, &mut names.right)
        }
    }

    fn pairs(&mut self, mut pairs: Pairs, scope: Scope) -> Control {
        match pairs.next() {
            Ok(Some((left, right))) => {
                self.frames.push(Frame::Pairs { pairs, scope });
                Control::Equal(left, right, scope)
            }
            Ok(None) => Control::Bool(true),
            Err(()) => Control::Bool(false),
        }
    }

    fn branch(&mut self, left: &Value, right: &Value, is_left: bool, scope: Scope) -> Control {
        let (
            Value::CaseSplit {
                left_payload: a_left_name,
                left: a_left,
                right_payload: a_right_name,
                right: a_right,
                ..
            },
            Value::CaseSplit {
                left_payload: b_left_name,
                left: b_left,
                right_payload: b_right_name,
                right: b_right,
                ..
            },
        ) = (left, right)
        else {
            unreachable!("case comparison retains its branches")
        };
        let (left_name, left, right_name, right) = if is_left {
            (a_left_name, a_left, b_left_name, b_left)
        } else {
            (a_right_name, a_right, b_right_name, b_right)
        };
        let names = &mut self.names[scope.index];
        let shared = format!("__casecmp__{}", names.fresh);
        names.fresh += 1;
        let (a, b) = self.maps(scope);
        let prior_left = a.insert(left_name.clone(), shared.clone());
        let prior_right = b.insert(right_name.clone(), shared);
        self.frames.push(Frame::Restore {
            left: left_name.clone(),
            right: right_name.clone(),
            prior_left,
            prior_right,
            scope,
        });
        Control::Equal(left.as_ref().clone(), right.as_ref().clone(), scope)
    }

    fn probe(&mut self, left: Value, right: Value, mut scope: Scope) -> Control {
        let type_stage = |value: &Value| {
            if let Value::EvalClosure { function, .. } = value {
                function
                    .value_groups
                    .first()
                    .and_then(|group| group.type_arity)
            } else {
                None
            }
        };
        let a_type = type_stage(&left);
        let b_type = type_stage(&right);
        if a_type.is_some() || b_type.is_some() {
            if (matches!(left, Value::EvalClosure { .. }) && a_type.is_none())
                || (matches!(right, Value::EvalClosure { .. }) && b_type.is_none())
                || matches!((a_type, b_type), (Some(a), Some(b)) if a != b)
            {
                return Control::Bool(false);
            }
            let arity = a_type.or(b_type).expect("typed probe has a type stage");
            let fresh = &mut self.names[scope.index].fresh;
            let variable = reflection_bindings::ReflectedVariable::neutral(
                format!("__alpha_type__{fresh}"),
                arity,
            );
            *fresh += 1;
            let args = ProbeArgs::Type(variable.as_type());
            self.frames.push(Frame::ProbeLeft {
                other: right,
                args: args.clone(),
                scope,
            });
            return Control::Apply(left, args);
        }
        let a_width = closure_partition_width(&left);
        let b_width = closure_partition_width(&right);
        let (left, right, args) = if let (Some(a), Some(b)) = (a_width, b_width) {
            let packet = fresh_opaque_packet(
                "__alpha__",
                a.max(b).max(1),
                &mut self.names[scope.index].fresh,
            );
            (left, right, ProbeArgs::Packet(packet))
        } else {
            let width = a_width
                .or(b_width)
                .expect("closure probe has a callable side");
            let (left, right) = if a_width.is_some() {
                (left, right)
            } else {
                scope.swapped = !scope.swapped;
                (right, left)
            };
            let fresh = &mut self.names[scope.index].fresh;
            let mut args = Vec::with_capacity(width);
            for _ in 0..width {
                args.push(Value::Atom(format!("__eta__{fresh}")));
                *fresh += 1;
            }
            (left, right, ProbeArgs::Flattened(args))
        };
        let right_args = args.clone();
        self.frames.push(Frame::ProbeLeft {
            other: right,
            args: right_args,
            scope,
        });
        Control::Apply(left, args)
    }

    fn structural(
        &mut self,
        left: Value,
        right: Value,
        scope: Scope,
        ctx: &EvalCtx<'_>,
    ) -> Control {
        if let Some(equal) = structural_recur_data_public_eq(&left, &right) {
            return Control::Bool(equal);
        }
        let equal = match (left, right) {
            (Value::Unit, Value::Unit) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::IntAtom(a, at), Value::IntAtom(b, bt))
            | (Value::FloatAtom(a, at), Value::FloatAtom(b, bt)) => a == b && at == bt,
            (Value::StrAtom(a), Value::StrAtom(b))
            | (Value::DiagnosticText(a), Value::DiagnosticText(b)) => a == b,
            (a @ Value::ReflType(_), b @ Value::ReflType(_)) => {
                matches!(
                    reflection_type_equal_value(&a, &b, ctx),
                    Some(Value::Bool(true))
                )
            }
            (Value::ReflTypeArity(a), Value::ReflTypeArity(b)) => a == b,
            (a @ Value::ReflTypeVar(_), b @ Value::ReflTypeVar(_)) => {
                matches!(
                    reflection_type_var_equal_value(&a, &b, ctx),
                    Some(Value::Bool(true))
                )
            }
            (
                Value::ReflTypeName {
                    segments: a,
                    param_arities: aa,
                },
                Value::ReflTypeName {
                    segments: b,
                    param_arities: ba,
                },
            ) => a == b && aa == ba,
            (Value::CheckedTerm(a), Value::CheckedTerm(b)) => {
                match checked_bindings::semantic_comparison(&a, &b) {
                    Ok(reflection_bindings::TypeComparison::Equal) => true,
                    Ok(
                        reflection_bindings::TypeComparison::Different
                        | reflection_bindings::TypeComparison::Undetermined,
                    ) => false,
                    Err(reflection_bindings::IncompleteOwner) => {
                        incomplete_observation_fault(ComptimeBuiltin::TypeEqual, ctx);
                        false
                    }
                }
            }
            #[cfg(feature = "surface")]
            (Value::Projected(a), Value::Projected(b)) => a.same_snapshot(&b),
            (Value::Primitive(a), Value::Primitive(b)) => a == b,
            (Value::NewtypeMember(a), Value::NewtypeMember(b)) => a.semantic_eq(&b),
            (Value::Atom(a), Value::Atom(b)) => {
                let (left, right) = self.maps(scope);
                atom_eq_under(&a, &b, left, right)
            }
            (Value::Data(left), Value::Data(right)) => {
                if !left.same_constructor(&right) || left.args().len() != right.args().len() {
                    return Control::Bool(false);
                }
                return self.pairs(
                    Pairs::Data {
                        left,
                        right,
                        next: 0,
                    },
                    scope,
                );
            }
            (Value::StuckType(left, a), Value::StuckType(right, b)) => {
                if !matches!(
                    reflection_type_equal_value(&Value::ReflType(a), &Value::ReflType(b), ctx),
                    Some(Value::Bool(true))
                ) {
                    return Control::Bool(false);
                }
                return Control::Equal(left.unwrap_or_clone(), right.unwrap_or_clone(), scope);
            }
            (Value::Stuck(left, a), Value::Stuck(right, b)) => {
                self.frames.push(Frame::StuckArgs {
                    left: a,
                    right: b,
                    scope,
                });
                return Control::Equal(left.unwrap_or_clone(), right.unwrap_or_clone(), scope);
            }
            (Value::Seq { value: a, body: ab }, Value::Seq { value: b, body: bb }) => {
                return self.pairs(
                    Pairs::Seq {
                        left: [a, ab],
                        right: [b, bb],
                        next: 0,
                    },
                    scope,
                );
            }
            (left @ Value::CaseSplit { .. }, right @ Value::CaseSplit { .. }) => {
                let (Value::CaseSplit { scrutinee: a, .. }, Value::CaseSplit { scrutinee: b, .. }) =
                    (&left, &right)
                else {
                    unreachable!()
                };
                let a = a.as_ref().clone();
                let b = b.as_ref().clone();
                self.frames.push(Frame::CaseLeft { left, right, scope });
                return Control::Equal(a, b, scope);
            }
            (left, right)
                if closure_partition_width(&left).is_some()
                    || closure_partition_width(&right).is_some() =>
            {
                return self.probe(left, right, scope);
            }
            _ => false,
        };
        Control::Bool(equal)
    }

    pub fn advance(&mut self, ctx: &EvalCtx<'_>) -> Outcome {
        let mut control = self
            .control
            .take()
            .expect("comparison has a pending operation");
        loop {
            control = match control {
                Control::Apply(callee, args) => return Outcome::Apply(callee, args),
                Control::Equal(left, right, scope) => {
                    self.frames.push(Frame::EqualLeft { right, scope });
                    Control::Eta(left)
                }
                Control::Eta(value) => {
                    self.frames.push(Frame::EtaDone(
                        ctx.metrics.as_deref().map(|_| Instant::now()),
                    ));
                    let bases = if let Value::Data(data) = &value
                        && data.ctor() == "__pair__"
                        && let [left, right] = data.args()
                    {
                        stuck_projection_base(left, "__fst__")
                            .zip(stuck_projection_base(right, "__snd__"))
                            .map(|(a, b)| (a.clone(), b.clone()))
                    } else {
                        None
                    };
                    if let Some((left, right)) = bases {
                        self.frames.push(Frame::EtaProbe {
                            original: value,
                            base: left.clone(),
                        });
                        self.frames.push(Frame::ScopePop);
                        let scope = Scope {
                            index: self.names.len(),
                            swapped: false,
                        };
                        self.names.push(Names::default());
                        Control::Equal(left, right, scope)
                    } else if let Value::CaseSplit {
                        scrutinee,
                        left_payload,
                        left,
                        right_payload,
                        right,
                    } = &value
                        && is_reinjection(left, "__left__", left_payload)
                        && is_reinjection(right, "__right__", right_payload)
                    {
                        Control::Eta(scrutinee.as_ref().clone())
                    } else {
                        Control::Value(value)
                    }
                }
                Control::Value(value) => match self.frames.pop() {
                    None => return Outcome::Done(value),
                    Some(Frame::EtaDone(started)) => {
                        if let (Some(metrics), Some(started)) = (ctx.metrics.as_deref(), started) {
                            metrics.record_eta_contract(started.elapsed());
                        }
                        Control::Value(value)
                    }
                    Some(Frame::EqualLeft { right, scope }) => {
                        self.frames.push(Frame::EqualRight { left: value, scope });
                        Control::Eta(right)
                    }
                    Some(Frame::EqualRight { left, scope }) => {
                        self.structural(left, value, scope, ctx)
                    }
                    Some(Frame::ProbeLeft { other, args, scope }) => {
                        self.frames.push(Frame::ProbeRight { left: value, scope });
                        Control::Apply(other, args)
                    }
                    Some(Frame::ProbeRight { left, scope }) => Control::Equal(left, value, scope),
                    _ => unreachable!("value result resumes eta or an application probe"),
                },
                Control::Bool(equal) => match self.frames.pop() {
                    None => return Outcome::Done(Value::Bool(equal)),
                    Some(Frame::ScopePop) => {
                        self.names.pop();
                        Control::Bool(equal)
                    }
                    Some(Frame::EtaProbe { original, base }) => {
                        if equal {
                            Control::Eta(base)
                        } else {
                            Control::Value(original)
                        }
                    }
                    Some(Frame::Pairs { pairs, scope }) => {
                        if equal {
                            self.pairs(pairs, scope)
                        } else {
                            Control::Bool(false)
                        }
                    }
                    Some(Frame::StuckArgs { left, right, scope }) => {
                        if equal {
                            self.pairs(
                                Pairs::Stuck {
                                    left: Args::new(left),
                                    right: Args::new(right),
                                },
                                scope,
                            )
                        } else {
                            Control::Bool(false)
                        }
                    }
                    Some(Frame::CaseLeft { left, right, scope }) => {
                        if equal {
                            self.frames.push(Frame::CaseRight {
                                left: left.clone(),
                                right: right.clone(),
                                scope,
                            });
                            self.branch(&left, &right, true, scope)
                        } else {
                            Control::Bool(false)
                        }
                    }
                    Some(Frame::CaseRight { left, right, scope }) => {
                        if equal {
                            self.branch(&left, &right, false, scope)
                        } else {
                            Control::Bool(false)
                        }
                    }
                    Some(Frame::Restore {
                        left,
                        right,
                        prior_left,
                        prior_right,
                        scope,
                    }) => {
                        let (a, b) = self.maps(scope);
                        restore_renaming(a, &left, prior_left);
                        restore_renaming(b, &right, prior_right);
                        Control::Bool(equal)
                    }
                    _ => unreachable!("equality result resumes a structural or eta comparison"),
                },
            };
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        while self.frames.pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalization::tests::test_eval_function;
    use reflection_bindings::{ReflectedVariable, TypeChild, semantic_type_equal};

    fn type_closure(arity: usize, body: EvalExpr) -> Value {
        let mut function = test_eval_function(&["T"], body).unwrap_or_clone();
        function.value_groups = vec![EvalParamGroup {
            type_arity: Some(arity),
            param_start: 0,
            param_count: 1,
            abi_arity: 0,
        }]
        .into();
        function.abi_arity = 0;
        Value::EvalClosure {
            function: EvalRef::new(function),
            captured: Vec::new().into(),
        }
    }

    fn type_apply(value: Value, argument: ReflectedType, ctx: &EvalCtx<'_>) -> Value {
        let operation = ctx.enter_eval_operation();
        let mut slots = EvalSlots::root(Vec::new());
        let mut machine = Machine::new(&mut slots, ctx, ());
        let control = machine.task(Task::TypeApplication {
            value,
            argument: Value::ReflType(argument),
        });
        operation.finish(machine.run(control))
    }

    fn unit() -> ReflectedType {
        ReflectedType::new_trusted(Type::Unit { meta: zero_meta() })
    }

    #[test]
    fn kinded_type_probes_keep_generic_observation_symbolic() {
        let ctx = EvalCtx::new();
        let observer = || {
            type_closure(
                0,
                EvalExpr::ReflTypeRootPredicate {
                    name: "__type_is_product_root__",
                    typ: EvalRef::new(EvalExpr::Local(0)),
                },
            )
        };
        let false_fn = || type_closure(0, EvalExpr::Bool(false));
        assert!(!nf_eq(&observer(), &false_fn(), &ctx));
        assert!(nf_eq(&observer(), &observer(), &ctx));
        assert!(nf_eq(&false_fn(), &false_fn(), &ctx));
        assert!(!nf_eq(
            &false_fn(),
            &type_closure(1, EvalExpr::Bool(false)),
            &ctx
        ));
        let product = ReflectedType::new_trusted(Type::Product {
            left: Box::new(Type::Unit { meta: zero_meta() }),
            right: Box::new(Type::Unit { meta: zero_meta() }),
            meta: zero_meta(),
        });
        assert!(matches!(
            type_apply(observer(), unit(), &ctx),
            Value::Bool(false)
        ));
        assert!(matches!(
            type_apply(observer(), product, &ctx),
            Value::Bool(true)
        ));
        let mut value_function = test_eval_function(&[], EvalExpr::Bool(false)).unwrap_or_clone();
        value_function.value_groups = vec![EvalParamGroup {
            type_arity: None,
            param_start: 0,
            param_count: 0,
            abi_arity: 0,
        }]
        .into();
        let value_function = Value::EvalClosure {
            function: EvalRef::new(value_function),
            captured: Vec::new().into(),
        };
        assert!(
            !nf_eq(&false_fn(), &value_function, &ctx),
            "empty value stage is not a type stage"
        );
    }

    #[test]
    fn generic_alpha_eta_and_body_reduction_use_actual_type_slots() {
        use crate::normalization::artifact::tests::lowered_package;
        let artifact = lowered_package(&[(
            "x.kio",
            "mod x; pure fn identity[A](x: A) -> A { x } pure fn forward[B](y: B) -> B { identity[B](y) } pure fn eta[A](f: [B] . -> (. | .)) -> [B] . -> (. | .) { .[B](x: .) -> (. | .) { f[B](x) } }",
        )]);
        let ctx = AuthenticatedEvalCtx::for_module(artifact.prepare_module("x").expect("module"));
        let identity = eval_authenticated(&path_expr("identity"), &Env::new(), &ctx);
        let forward = eval_authenticated(&path_expr("forward"), &Env::new(), &ctx);
        assert!(nf_eq(&identity, &forward, &ctx));
        let eta = eval_authenticated(&path_expr("eta"), &Env::new(), &ctx);
        let eta = type_apply(eta, unit(), &ctx);
        let opaque = Value::Atom("arbitrary".into());
        let expanded = crate::normalization::apply(eta, vec![opaque.clone()], &ctx);
        assert!(nf_eq(&expanded, &opaque, &ctx));
    }

    #[test]
    fn opaque_type_stages_retain_arguments_and_distribute_over_residuals() {
        let ctx = EvalCtx::new();
        let opaque = Value::Atom("f".into());
        let left = type_apply(opaque.clone(), unit(), &ctx);
        let right = type_apply(
            opaque.clone(),
            ReflectedType::new_trusted(Type::Bottom { meta: zero_meta() }),
            &ctx,
        );
        assert!(matches!(&left, Value::StuckType(callee, argument)
            if matches!(callee.as_ref(), Value::Atom(name) if name == "f") && matches!(argument.as_type(), Type::Unit { .. })));
        assert!(!nf_eq(&left, &right, &ctx));
        assert!(nf_eq(&left, &left, &ctx));
        assert!(!nf_eq(
            &left,
            &Value::Stuck(
                EvalRef::new(opaque.clone()),
                vec![Value::ReflType(unit())].into()
            ),
            &ctx
        ));
        assert_eq!(value_to_string(&left), "f[.]");
        assert!(!exact_call_memo_result_shareable(&left));
        let mut budget = EXACT_CALL_MEMO_KEY_BUDGET;
        assert!(value_memo_key(&left, &mut budget).is_none());
        let sequence = Value::Seq {
            value: EvalRef::new(Value::Atom("effect".into())),
            body: EvalRef::new(opaque.clone()),
        };
        let sequence = type_apply(sequence, unit(), &ctx);
        assert!(matches!(sequence, Value::Seq { value, body }
            if matches!(value.as_ref(), Value::Atom(name) if name == "effect") && nf_eq(&body, &left, &ctx)));
        let split = Value::CaseSplit {
            scrutinee: EvalRef::new(Value::Atom("sum".into())),
            left_payload: "l".into(),
            left: EvalRef::new(opaque.clone()),
            right_payload: "r".into(),
            right: EvalRef::new(opaque),
        };
        let split = type_apply(split, unit(), &ctx);
        assert!(matches!(split, Value::CaseSplit { left: a, right: b, .. }
            if nf_eq(&a, &left, &ctx) && nf_eq(&b, &left, &ctx)));
        let primitive = Value::Stuck(
            EvalRef::new(Value::Primitive(ComptimeBuiltin::TypeForallPrefixFold)),
            vec![Value::Primitive(ComptimeBuiltin::ComptimeProof)].into(),
        );
        let specialized = type_apply(primitive.clone(), unit(), &ctx);
        assert!(
            nf_eq(&primitive, &specialized, &ctx),
            "partial primitive keeps erased result-type stage"
        );
        let structural = Value::Data(structural_recur_primitive_data(
            StructuralRecurPrimitiveStage::AwaitingRuntime,
        ));
        assert!(
            matches!(type_apply(structural, unit(), &ctx), Value::Data(data)
            if matches!(data.structural_recur_identity(), Some(StructuralRecurDataIdentity::Primitive { .. })))
        );
    }

    #[test]
    fn explicit_root_type_bindings_share_neutrals_across_claim_arms() {
        let ctx = EvalCtx::new();
        let param = |name: &str, arity| TypeParam {
            name: name.into(),
            presentation: Default::default(),
            kind: Some(Box::new(crate::ast::Kind::arrow_chain(arity))),
            span: zero_span(),
        };
        let bindings = [
            EvalTypeBinding::neutral(&param("A", 0)),
            EvalTypeBinding::neutral(&param("B", 0)),
        ];
        let call = |name| {
            Expr::synth_call(
                path_expr("f"),
                vec![CallArg::Type(type_path(name))],
                zero_span(),
            )
        };
        let evaluate = |expr, env: &Env| {
            eval_with_compile_scope(
                &expr,
                env,
                &bindings,
                &ctx,
                EvalSourceGeneration::fresh(),
                false,
            )
        };
        let empty = Env::new();
        let mut local = Env::new();
        local.insert("f".into(), Value::Atom("f".into()));
        for env in [&empty, &local] {
            let a = evaluate(call("A"), env);
            let same = evaluate(call("A"), env);
            let b = evaluate(call("B"), env);
            assert!(nf_eq(&a, &same, &ctx));
            assert!(!nf_eq(&a, &b, &ctx));
            let Value::StuckType(_, actual) = a else {
                panic!("root type operand")
            };
            let Value::ReflType(expected) = &bindings[0].value else {
                unreachable!()
            };
            assert!(semantic_type_equal(&actual, expected).unwrap());
        }
    }

    #[test]
    fn neutral_observers_require_only_their_semantic_region() {
        let ctx = EvalCtx::new();
        let neutral = ReflectedVariable::neutral("A".into(), 0).as_type();
        let left = reflection_binary_type_value(
            ComptimeBuiltin::TypeProduct,
            &Value::ReflType(neutral.clone()),
            &Value::ReflType(unit()),
        )
        .unwrap();
        let right = reflection_binary_type_value(
            ComptimeBuiltin::TypeProduct,
            &Value::ReflType(unit()),
            &Value::ReflType(neutral.clone()),
        )
        .unwrap();
        let Value::ReflType(left_type) = &left else {
            unreachable!()
        };
        let Value::ReflType(right_type) = &right else {
            unreachable!()
        };
        assert!(neutral_type_observation_is_known(
            ComptimeBuiltin::TypeView,
            left_type,
            &ctx
        ));
        assert!(neutral_type_observation_is_known(
            ComptimeBuiltin::TypeProductSpineFold,
            left_type,
            &ctx
        ));
        assert!(!neutral_type_observation_is_known(
            ComptimeBuiltin::TypeProductSpineFold,
            right_type,
            &ctx
        ));
        assert!(!neutral_type_observation_is_known(
            ComptimeBuiltin::TypeDisplay,
            left_type,
            &ctx
        ));
        assert!(semantic_type_equal(&left_type.child(TypeChild::Left), &neutral).unwrap());
        for value in [Value::ReflType(neutral.clone()), left] {
            let direct = EvalExpr::ReflTypeRootPredicate {
                name: "__type_is_product_root__",
                typ: EvalRef::new(EvalExpr::Const(value.clone())),
            };
            let direct = eval_compiled_expr(&direct, &mut EvalSlots::root(Vec::new()), &ctx);
            let generic = crate::normalization::apply(
                Value::Primitive(ComptimeBuiltin::TypeIsProductRoot),
                vec![
                    Value::Primitive(ComptimeBuiltin::ComptimeProof),
                    value.clone(),
                ],
                &ctx,
            );
            if matches!(value, Value::ReflType(ref typ) if matches!(typ.as_type(), Type::Product { .. }))
            {
                assert!(matches!(direct, Value::Bool(true)));
                assert!(matches!(generic, Value::Bool(true)));
            } else {
                assert!(matches!(direct, Value::Stuck(..)));
                assert!(matches!(generic, Value::Stuck(..)));
                assert!(
                    nf_eq(&direct, &generic, &ctx),
                    "direct and authenticated generic residuals agree"
                );
                let unauthenticated = crate::normalization::apply(
                    Value::Primitive(ComptimeBuiltin::TypeIsProductRoot),
                    vec![Value::Atom("unknown_proof".into()), value],
                    &ctx,
                );
                assert!(
                    !nf_eq(&direct, &unauthenticated, &ctx),
                    "an arbitrary first operand is not a consumed proof"
                );
                assert!(comptime_fault_message(&direct, &ctx).is_none());
                assert!(comptime_fault_message(&generic, &ctx).is_none());
            }
        }
    }
    #[test]
    fn saturated_pending_fold_result_keeps_its_own_type_stage() {
        let ctx = EvalCtx::new();
        let neutral = ReflectedVariable::neutral("A".into(), 0).as_type();
        let init = type_closure(0, EvalExpr::Local(0));
        let step = Value::EvalClosure {
            function: test_eval_function(&["acc", "item"], EvalExpr::Local(0)),
            captured: Vec::new().into(),
        };
        let pending = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeProductSpineFold),
            vec![
                Value::Primitive(ComptimeBuiltin::ComptimeProof),
                Value::ReflType(neutral),
                init,
                step,
            ],
            &ctx,
        );
        assert!(matches!(&pending, Value::Stuck(_, args) if args.len() == 3));
        let selected = type_apply(pending.clone(), unit(), &ctx);
        assert!(matches!(&selected, Value::StuckType(callee, _) if nf_eq(callee, &pending, &ctx)));
        let other = type_apply(
            pending,
            ReflectedType::new_trusted(Type::Bottom { meta: zero_meta() }),
            &ctx,
        );
        assert!(!nf_eq(&selected, &other, &ctx));
    }

    #[test]
    fn first_class_reflect_type_consumes_actual_operand_after_one_proof_stage() {
        let ctx = EvalCtx::new();
        let argument = ReflectedVariable::neutral("A".into(), 0).as_type();
        for proof in [
            Value::Primitive(ComptimeBuiltin::ComptimeProof),
            Value::Atom("ct".into()),
        ] {
            let partial = crate::normalization::apply(
                Value::Primitive(ComptimeBuiltin::ReflectType),
                vec![proof],
                &ctx,
            );
            let reflected = type_apply(partial, argument.clone(), &ctx);
            let Value::ReflType(reflected) = reflected else {
                panic!("first-class actual type operand")
            };
            assert!(semantic_type_equal(&reflected, &argument).unwrap());
        }
        let ordinary = type_apply(Value::Atom("__reflect_type__".into()), argument, &ctx);
        assert!(
            matches!(ordinary, Value::StuckType(_, _)),
            "spelling carries no primitive authority"
        );
    }

    #[test]
    fn actual_spine_fold_preserves_neutral_child_carrier() {
        let ctx = EvalCtx::new();
        let neutral = ReflectedVariable::neutral("A".into(), 0).as_type();
        let source = reflection_binary_type_value(
            ComptimeBuiltin::TypeProduct,
            &Value::ReflType(neutral.clone()),
            &Value::ReflType(unit()),
        )
        .unwrap();
        let step = Value::EvalClosure {
            function: test_eval_function(
                &["acc", "item"],
                EvalExpr::ReflTypeProduct {
                    left: EvalRef::new(EvalExpr::Local(0)),
                    right: EvalRef::new(EvalExpr::Local(1)),
                },
            ),
            captured: Vec::new().into(),
        };
        let folded = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeProductSpineFold),
            vec![
                Value::Primitive(ComptimeBuiltin::ComptimeProof),
                source,
                Value::ReflType(unit()),
                step,
            ],
            &ctx,
        );
        let Value::ReflType(folded) = folded else {
            panic!("fold returns a reflected product")
        };
        let selected = folded.child(TypeChild::Left).child(TypeChild::Right);
        assert!(
            semantic_type_equal(&selected, &neutral).unwrap(),
            "fold item retains the actual lexical carrier"
        );
    }
    #[test]
    fn saturated_reflection_residuals_consume_only_authenticated_proof_stages() {
        let ctx = EvalCtx::new();
        let neutral = Value::ReflType(ReflectedVariable::neutral("A".into(), 0).as_type());
        let direct = EvalExpr::ReflTypeEqual {
            left: EvalRef::new(EvalExpr::Const(neutral.clone())),
            right: EvalRef::new(EvalExpr::Const(Value::ReflType(unit()))),
        };
        let direct = eval_compiled_expr(&direct, &mut EvalSlots::root(Vec::new()), &ctx);
        let generic = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeEqual),
            vec![
                Value::Primitive(ComptimeBuiltin::ComptimeProof),
                neutral,
                Value::ReflType(unit()),
            ],
            &ctx,
        );
        assert!(matches!(direct, Value::Stuck(_, _)));
        assert!(nf_eq(&direct, &generic, &ctx));
        let pending = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeArityFold),
            vec![
                Value::Primitive(ComptimeBuiltin::ComptimeProof),
                Value::Atom("arity".into()),
                type_closure(0, EvalExpr::Local(0)),
                Value::EvalClosure {
                    function: test_eval_function(&["acc"], EvalExpr::Local(0)),
                    captured: Vec::new().into(),
                },
            ],
            &ctx,
        );
        assert!(matches!(&pending, Value::Stuck(_, args) if args.len() == 3));
        let typed = type_apply(pending.clone(), unit(), &ctx);
        assert!(matches!(typed, Value::StuckType(_, _)));
        let applied = crate::normalization::apply(pending.clone(), vec![Value::Unit], &ctx);
        assert!(
            matches!(applied, Value::Stuck(callee, args) if args.len() == 1 && nf_eq(&callee, &pending, &ctx)),
            "applying the pending result must not retry the saturated fold"
        );
        let prefix = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeForallPrefixFold),
            vec![Value::Primitive(ComptimeBuiltin::ComptimeProof)],
            &ctx,
        );
        assert!(
            matches!(&prefix, Value::Stuck(_, args) if args.len() == 1 && is_reflection_proof(&args[0]))
        );
        let prefix = type_apply(prefix, unit(), &ctx);
        let result = crate::normalization::apply(
            prefix,
            vec![
                Value::ReflType(unit()),
                Value::Unit,
                Value::Atom("unreached_step".into()),
            ],
            &ctx,
        );
        assert!(
            matches!(result, Value::Data(data) if matches!(data.args(), [Value::Unit, Value::ReflType(typ)] if matches!(typ.as_type(), Type::Unit { .. })))
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn projected_refusal_retains_its_capability_prefix() {
        use crate::pass::typecheck_core::apply::fills::unresolved_type_reflections_for_test;
        let (_, _, goal, _, _) = unresolved_type_reflections_for_test();
        let (foreign, _, _, _, _) = unresolved_type_reflections_for_test();
        let proof = Value::Data(EvalRef::new(DataValue::new_marked_comptime_proof(foreign)));
        let result = crate::normalization::apply(
            Value::Primitive(ComptimeBuiltin::TypeView),
            vec![proof, projected_value(goal)],
            &EvalCtx::new(),
        );
        assert!(
            matches!(result, Value::Stuck(_, args) if matches!(args.as_slice(), [proof, Value::Projected(_)] if is_reflection_proof(proof)))
        );
    }
}

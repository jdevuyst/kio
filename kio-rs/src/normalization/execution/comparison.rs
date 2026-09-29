use super::*;

#[derive(Clone)]
pub(super) enum ProbeArgs {
    Packet(Value),
    Flattened(Vec<Value>),
}

impl ProbeArgs {
    pub fn into_eval(self) -> EvalArgs {
        match self {
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
                let (Some(a), Some(b)) =
                    (canonical_refl_type(&a, ctx), canonical_refl_type(&b, ctx))
                else {
                    return Control::Bool(false);
                };
                canonical_refl_type_equiv(&a, &b, ctx)
            }
            (Value::ReflTypeArity(a), Value::ReflTypeArity(b)) => a == b,
            (
                Value::ReflTypeVar { name: a, arity: aa },
                Value::ReflTypeVar { name: b, arity: ba },
            ) => a == b && aa == ba,
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
            (Value::CheckedTerm(a), Value::CheckedTerm(b)) => a == b,
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

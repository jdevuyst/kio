//! Structural CPS adoption of authenticated, closed quotation recipes.

use super::*;

type Term = EvalRef<CheckedTerm>;

fn term_type(term: &CheckedTerm) -> EvalAstType {
    type_traversal::clone_type(term.ty())
}

fn arrow(param: EvalAstType, ret: EvalAstType) -> EvalAstType {
    Type::Function {
        param: Box::new(param),
        ret: Box::new(ret),
        abi_arity: 1,
        caps: (),
        meta: zero_meta(),
    }
}

fn node(node: CheckedTermNode, ty: EvalAstType) -> Term {
    EvalRef::new(CheckedTerm::from_node(node, ty))
}

fn local(name: &str, ty: EvalAstType) -> Term {
    EvalRef::new(CheckedTerm::local(name.to_owned(), ty))
}

fn lambda(name: String, param: EvalAstType, body: Term) -> Term {
    let ty = arrow(type_traversal::clone_type(&param), term_type(&body));
    let sig = Signature::new(vec![SignatureParam::Value(Param {
        name,
        ty: Some(param),
        pattern: (),
        meta: zero_meta(),
    })]);
    node(CheckedTermNode::TermFn { sig, body }, ty)
}

fn call(function: Term, argument: Term) -> Term {
    let Type::Function { ret, .. } = function.ty() else {
        panic!("unary CPS function")
    };
    let result = type_traversal::clone_type(ret);
    let param = term_type(&argument);
    node(
        CheckedTermNode::TermCall {
            fn_value: function,
            params: vec![param],
            arg_packet: argument,
        },
        result,
    )
}

struct Adapted<'a> {
    original: &'a CheckedTerm,
    computation: Option<Term>,
}

impl Adapted<'_> {
    fn run(&self, continuation: Term) -> Term {
        match &self.computation {
            Some(computation) => call(computation.clone(), continuation),
            None => call(continuation, EvalRef::new(self.original.clone())),
        }
    }
}

#[derive(Debug, Default)]
struct WorkCount {
    visits: usize,
    lifted: usize,
    pending_peak: usize,
}

/// Input is a validated, closed checked recipe after ordinary source sharing.
/// `suspended` marks exact template identities, never declaration spellings.
fn adapt<'a>(
    root: &'a CheckedTerm,
    suspended: &[bool],
    runtime: &EvalAstType,
) -> Result<(Adapted<'a>, WorkCount), &'static str> {
    enum Work<'a> {
        Enter(&'a CheckedTerm),
        Finish(&'a CheckedTerm, usize),
    }
    let mut pending = vec![Work::Enter(root)];
    let mut ready: Vec<Adapted<'a>> = Vec::new();
    let mut count = WorkCount::default();
    while let Some(work) = pending.pop() {
        match work {
            Work::Enter(term) => {
                count.visits += 1;
                pending.push(Work::Finish(term, ready.len()));
                let start = pending.len();
                checked_traversal::children(&term.node, |child| pending.push(Work::Enter(child)));
                pending[start..].reverse();
                count.pending_peak = count.pending_peak.max(pending.len());
            }
            Work::Finish(term, base) => {
                let children: Vec<_> = ready.drain(base..).collect();
                let marked = match term.node.as_ref() {
                    CheckedTermNode::TemplateValue { index } => *suspended
                        .get(*index)
                        .expect("authenticated template inventory"),
                    _ => false,
                };
                if !marked && children.iter().all(|child| child.computation.is_none()) {
                    ready.push(Adapted {
                        original: term,
                        computation: None,
                    });
                    continue;
                }
                count.lifted += 1;
                let continuation_type = ownership::Owned::new(arrow(
                    term_type(term),
                    type_traversal::clone_type(runtime),
                ));
                let k_name = fresh_checked_term_value_name("quoted_cont");
                let k = local(&k_name, type_traversal::clone_type(&continuation_type));
                let names: Vec<_> = children
                    .iter()
                    .map(|_| fresh_checked_term_value_name("quoted_value"))
                    .collect();
                let refs: Vec<_> = children
                    .iter()
                    .zip(&names)
                    .map(|(child, name)| local(name, term_type(child.original)))
                    .collect();
                let resumed = match term.node.as_ref() {
                    CheckedTermNode::TemplateValue { index } => {
                        let computation = node(
                            CheckedTermNode::TemplateValue { index: *index },
                            arrow(
                                continuation_type.take(),
                                type_traversal::clone_type(runtime),
                            ),
                        );
                        ready.push(Adapted {
                            original: term,
                            computation: Some(computation),
                        });
                        continue;
                    }
                    CheckedTermNode::TermLet { name, .. } => {
                        let body = children[1].run(k.clone());
                        children[0].run(lambda(name.clone(), term_type(children[0].original), body))
                    }
                    CheckedTermNode::IntrinsicIfThenElse { .. } => {
                        let body = node(
                            CheckedTermNode::IntrinsicIfThenElse {
                                result_ty: type_traversal::clone_type(runtime),
                                condition: refs[0].clone(),
                                true_body: children[1].run(k.clone()),
                                false_body: children[2].run(k.clone()),
                            },
                            type_traversal::clone_type(runtime),
                        );
                        children[0].run(lambda(
                            names[0].clone(),
                            term_type(children[0].original),
                            body,
                        ))
                    }
                    CheckedTermNode::IntrinsicEither {
                        left_ty,
                        right_ty,
                        left_name,
                        right_name,
                        ..
                    } => {
                        let body = node(
                            CheckedTermNode::IntrinsicEither {
                                left_ty: type_traversal::clone_type(left_ty),
                                right_ty: type_traversal::clone_type(right_ty),
                                result_ty: type_traversal::clone_type(runtime),
                                value: refs[0].clone(),
                                left_name: left_name.clone(),
                                right_name: right_name.clone(),
                                left_body: children[1].run(k.clone()),
                                right_body: children[2].run(k.clone()),
                            },
                            type_traversal::clone_type(runtime),
                        );
                        children[0].run(lambda(
                            names[0].clone(),
                            term_type(children[0].original),
                            body,
                        ))
                    }
                    CheckedTermNode::TermFn { .. } => {
                        return Err("recursive value escapes in a function");
                    }
                    ordinary => {
                        let rebuilt = match ordinary {
                            CheckedTermNode::TermCall { params, .. } => CheckedTermNode::TermCall {
                                fn_value: refs[0].clone(),
                                params: params.iter().map(type_traversal::clone_type).collect(),
                                arg_packet: refs[1].clone(),
                            },
                            CheckedTermNode::TermTypeApp { arg, .. } => {
                                CheckedTermNode::TermTypeApp {
                                    fn_value: refs[0].clone(),
                                    arg: type_traversal::clone_type(arg),
                                }
                            }
                            CheckedTermNode::IntrinsicPair { .. } => {
                                CheckedTermNode::IntrinsicPair {
                                    left: refs[0].clone(),
                                    right: refs[1].clone(),
                                }
                            }
                            CheckedTermNode::IntrinsicProjection {
                                intrinsic,
                                left_ty,
                                right_ty,
                                ..
                            } => CheckedTermNode::IntrinsicProjection {
                                intrinsic: intrinsic.clone(),
                                left_ty: type_traversal::clone_type(left_ty),
                                right_ty: type_traversal::clone_type(right_ty),
                                value: refs[0].clone(),
                            },
                            CheckedTermNode::IntrinsicInjection {
                                intrinsic,
                                left_ty,
                                right_ty,
                                ..
                            } => CheckedTermNode::IntrinsicInjection {
                                intrinsic: intrinsic.clone(),
                                left_ty: type_traversal::clone_type(left_ty),
                                right_ty: type_traversal::clone_type(right_ty),
                                value: refs[0].clone(),
                            },
                            CheckedTermNode::IntrinsicAbsurd { .. } => {
                                CheckedTermNode::IntrinsicAbsurd {
                                    result_ty: term_type(term),
                                    bottom_value: refs[0].clone(),
                                }
                            }
                            CheckedTermNode::Raw(_)
                            | CheckedTermNode::Local { .. }
                            | CheckedTermNode::ElabError { .. } => {
                                unreachable!("an atomic ordinary node cannot contain suspension")
                            }
                            CheckedTermNode::TemplateValue { .. }
                            | CheckedTermNode::TermLet { .. }
                            | CheckedTermNode::TermFn { .. }
                            | CheckedTermNode::IntrinsicEither { .. }
                            | CheckedTermNode::IntrinsicIfThenElse { .. } => {
                                unreachable!("handled above")
                            }
                        };
                        let mut body = call(k.clone(), node(rebuilt, term_type(term)));
                        for (child, name) in children.iter().zip(names).rev() {
                            body = child.run(lambda(name, term_type(child.original), body));
                        }
                        body
                    }
                };
                ready.push(Adapted {
                    original: term,
                    computation: Some(lambda(k_name, continuation_type.take(), resumed)),
                });
            }
        }
    }
    Ok((ready.pop().expect("one root"), count))
}

impl CheckedTerm {
    /// Classify the authenticated public recipe before source sharing can hoist
    /// repeated occurrences out of a closure. Eta adds enclosing functions too.
    pub(crate) fn validate_recursive_template_scope(
        &self,
        suspended: &[bool],
        enclosed_by_eta: bool,
    ) -> Result<(), &'static str> {
        let mut pending = vec![(self, enclosed_by_eta)];
        while let Some((term, enclosed)) = pending.pop() {
            if let CheckedTermNode::TemplateValue { index } = term.node.as_ref()
                && suspended
                    .get(*index)
                    .copied()
                    .expect("authenticated template inventory")
                && enclosed
            {
                return Err("recursive value escapes in a function");
            }
            let enclosed = enclosed || matches!(term.node.as_ref(), CheckedTermNode::TermFn { .. });
            checked_traversal::children(&term.node, |child| pending.push((child, enclosed)));
        }
        Ok(())
    }

    /// Consume the ordinary sharing adapter's explicit lets and projections.
    /// A present result has type (Public -> Runtime) -> Runtime; holes selected
    /// by the same exact source inventory now hold computations of that shape.
    pub(crate) fn adapt_recursive_template_values(
        &self,
        suspended: &[bool],
        runtime: &EvalAstType,
    ) -> Result<Option<Self>, &'static str> {
        let (adapted, _) = adapt(self, suspended, runtime)?;
        Ok(adapted.computation.map(|term| (*term).clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Debug, PartialEq)]
    enum Val {
        Unit,
        Bool(bool),
        Pair(Box<Val>, Box<Val>),
        Sum(bool, Box<Val>),
        Closure(usize),
        Suspended(usize),
        Effect(usize),
        Identity,
    }

    /// Independent, iterative oracle for the emitted ordinary checked-term subset.
    fn execute(root: &CheckedTerm, templates: &[Val], payloads: &[Val]) -> (Val, Vec<usize>) {
        type EnvId = Option<usize>;
        enum Work<'a> {
            Eval(&'a CheckedTerm, EnvId),
            Let(&'a str, &'a CheckedTerm, EnvId),
            Call,
            Pair,
            Projection(bool),
            Injection(bool),
            If(&'a CheckedTerm, &'a CheckedTerm, EnvId),
            Either(&'a str, &'a CheckedTerm, &'a str, &'a CheckedTerm, EnvId),
            Apply(Val),
        }
        let mut work = vec![Work::Eval(root, None)];
        let mut values = Vec::new();
        let mut bindings: Vec<(EnvId, &str, Val)> = Vec::new();
        let mut closures: Vec<(&str, &CheckedTerm, EnvId)> = Vec::new();
        let mut events = Vec::new();
        while let Some(next) = work.pop() {
            match next {
                Work::Eval(term, env) => match term.node.as_ref() {
                    CheckedTermNode::TemplateValue { index } => {
                        values.push(templates[*index].clone())
                    }
                    CheckedTermNode::Local { name } => {
                        let mut scope = env;
                        loop {
                            let (parent, binding, value) = &bindings[scope.expect("bound local")];
                            if binding == name {
                                values.push(value.clone());
                                break;
                            }
                            scope = *parent;
                        }
                    }
                    CheckedTermNode::Raw(Expr::Unit { .. }) => values.push(Val::Unit),
                    CheckedTermNode::TermFn { sig, body } => {
                        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
                            panic!("unary oracle")
                        };
                        values.push(Val::Closure(closures.len()));
                        closures.push((&param.name, body, env));
                    }
                    CheckedTermNode::TermCall {
                        fn_value,
                        arg_packet,
                        ..
                    } => {
                        work.push(Work::Call);
                        work.push(Work::Eval(arg_packet, env));
                        work.push(Work::Eval(fn_value, env));
                    }
                    CheckedTermNode::TermLet { name, value, body } => {
                        work.push(Work::Let(name, body, env));
                        work.push(Work::Eval(value, env));
                    }
                    CheckedTermNode::IntrinsicPair { left, right } => {
                        work.push(Work::Pair);
                        work.push(Work::Eval(right, env));
                        work.push(Work::Eval(left, env));
                    }
                    CheckedTermNode::IntrinsicProjection {
                        intrinsic, value, ..
                    } => {
                        work.push(Work::Projection(intrinsic == "__fst__"));
                        work.push(Work::Eval(value, env));
                    }
                    CheckedTermNode::IntrinsicInjection {
                        intrinsic, value, ..
                    } => {
                        work.push(Work::Injection(intrinsic == "__left__"));
                        work.push(Work::Eval(value, env));
                    }
                    CheckedTermNode::IntrinsicIfThenElse {
                        condition,
                        true_body,
                        false_body,
                        ..
                    } => {
                        work.push(Work::If(true_body, false_body, env));
                        work.push(Work::Eval(condition, env));
                    }
                    CheckedTermNode::IntrinsicEither {
                        value,
                        left_name,
                        left_body,
                        right_name,
                        right_body,
                        ..
                    } => {
                        work.push(Work::Either(
                            left_name, left_body, right_name, right_body, env,
                        ));
                        work.push(Work::Eval(value, env));
                    }
                    CheckedTermNode::TermTypeApp { fn_value, .. } => {
                        work.push(Work::Eval(fn_value, env))
                    }
                    _ => panic!("oracle fixture contains another node"),
                },
                Work::Let(name, body, parent) => {
                    let env = bindings.len();
                    bindings.push((parent, name, values.pop().unwrap()));
                    work.push(Work::Eval(body, Some(env)));
                }
                Work::Call => {
                    let arg = values.pop().unwrap();
                    let function = values.pop().unwrap();
                    values.push(arg);
                    work.push(Work::Apply(function));
                }
                Work::Apply(function) => {
                    let arg = values.pop().unwrap();
                    match function {
                        Val::Closure(index) => {
                            let (name, body, parent) = closures[index];
                            let env = bindings.len();
                            bindings.push((parent, name, arg));
                            work.push(Work::Eval(body, Some(env)));
                        }
                        Val::Suspended(index) => {
                            events.push(index);
                            values.push(payloads[index].clone());
                            work.push(Work::Apply(arg));
                        }
                        Val::Effect(index) => {
                            events.push(index);
                            values.push(payloads[index].clone());
                        }
                        Val::Identity => values.push(arg),
                        _ => panic!("callable oracle value"),
                    }
                }
                Work::Pair => {
                    let right = values.pop().unwrap();
                    let left = values.pop().unwrap();
                    values.push(Val::Pair(Box::new(left), Box::new(right)));
                }
                Work::Projection(left) => {
                    let Val::Pair(a, b) = values.pop().unwrap() else {
                        panic!("pair")
                    };
                    values.push(if left { *a } else { *b });
                }
                Work::Injection(left) => {
                    let value = values.pop().unwrap();
                    values.push(Val::Sum(left, Box::new(value)));
                }
                Work::If(a, b, env) => {
                    let Val::Bool(condition) = values.pop().unwrap() else {
                        panic!("bool")
                    };
                    work.push(Work::Eval(if condition { a } else { b }, env));
                }
                Work::Either(an, a, bn, b, env) => {
                    let Val::Sum(left, value) = values.pop().unwrap() else {
                        panic!("sum")
                    };
                    let scope = bindings.len();
                    bindings.push((env, if left { an } else { bn }, *value));
                    work.push(Work::Eval(if left { a } else { b }, Some(scope)));
                }
            }
        }
        (values.pop().expect("result"), events)
    }

    fn unit() -> EvalAstType {
        Type::Unit { meta: zero_meta() }
    }
    fn boolean() -> EvalAstType {
        Type::synth_path(vec!["Bool".to_owned()], vec![], zero_span())
    }
    fn template(index: usize, ty: EvalAstType) -> Term {
        EvalRef::new(CheckedTerm::template_value(index, ty))
    }
    fn pair(a: Term, b: Term) -> Term {
        let ty = Type::Product {
            left: Box::new(a.clone_type()),
            right: Box::new(b.clone_type()),
            meta: zero_meta(),
        };
        node(CheckedTermNode::IntrinsicPair { left: a, right: b }, ty)
    }
    fn result(root: &CheckedTerm, marked: &[bool], payloads: &[Val]) -> (Val, Vec<usize>) {
        let (adapted, _) = adapt(root, marked, &root.clone_type()).unwrap();
        let name = fresh_checked_term_value_name("done");
        let done = lambda(
            name.clone(),
            root.clone_type(),
            local(&name, root.clone_type()),
        );
        let output = adapted.run(done);
        let templates: Vec<_> = marked
            .iter()
            .enumerate()
            .map(|(i, marked)| {
                if *marked {
                    Val::Suspended(i)
                } else {
                    payloads[i].clone()
                }
            })
            .collect();
        execute(&output, &templates, payloads)
    }

    #[test]
    fn closed_recipe_preserves_discard_order_multiplicity_and_let_sharing() {
        let a = template(0, boolean());
        let b = template(1, boolean());
        let payloads = [Val::Bool(true), Val::Bool(false), Val::Bool(true)];
        let marked = [true, true, false];
        assert_eq!(
            result(&template(2, boolean()), &marked, &payloads),
            (Val::Bool(true), vec![])
        );
        assert_eq!(
            result(&pair(b.clone(), a.clone()), &marked, &payloads).1,
            [1, 0]
        );
        assert_eq!(
            result(&pair(a.clone(), a.clone()), &marked, &payloads).1,
            [0, 0]
        );
        let body = pair(local("saved", boolean()), local("saved", boolean()));
        let shared = node(
            CheckedTermNode::TermLet {
                name: "saved".into(),
                value: a,
                body: body.clone(),
            },
            body.clone_type(),
        );
        assert_eq!(result(&shared, &marked, &payloads).1, [0]);
    }

    #[test]
    fn existing_source_packet_sharing_is_consumed_as_explicit_let() {
        let operand = template(0, boolean());
        let recipe = pair(operand.clone(), operand);
        let adapter = CheckedTermTemplateValueAdapter {
            sources: vec![CheckedTermTemplateSource {
                ty: boolean(),
                force_if_unused: false,
            }],
            slots: vec![CheckedTermTemplateSlot {
                pieces: vec![CheckedTermTemplatePiece {
                    source_index: 0,
                    source_slot: 0,
                    source_slot_tys: vec![boolean()],
                }],
            }],
        };
        let shared = recipe.adapt_template_values(&adapter);
        assert!(
            shared
                .adapt_recursive_template_values(&[true], &boolean())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            result(&shared, &[true], &[Val::Bool(true)]),
            (
                Val::Pair(Box::new(Val::Bool(true)), Box::new(Val::Bool(true))),
                vec![0]
            )
        );
    }

    #[test]
    fn original_recipe_escape_check_precedes_sharing_and_includes_residual_eta() {
        let repeated = pair(template(0, boolean()), template(0, boolean()));
        let closure = lambda("ignored".into(), unit(), repeated);
        assert!(
            closure
                .validate_recursive_template_scope(&[true], false)
                .is_err()
        );
        assert!(
            template(0, boolean())
                .validate_recursive_template_scope(&[true], true)
                .is_err()
        );
        let adapter = CheckedTermTemplateValueAdapter {
            sources: vec![CheckedTermTemplateSource {
                ty: boolean(),
                force_if_unused: false,
            }],
            slots: vec![CheckedTermTemplateSlot {
                pieces: vec![CheckedTermTemplatePiece {
                    source_index: 0,
                    source_slot: 0,
                    source_slot_tys: vec![boolean()],
                }],
            }],
        };
        let hoisted = closure.adapt_template_values(&adapter);
        assert!(
            hoisted
                .validate_recursive_template_scope(&[true], false)
                .is_ok()
        );
        let captured = lambda(
            "ignored".into(),
            unit(),
            pair(local("done", boolean()), local("done", boolean())),
        );
        let completed = node(
            CheckedTermNode::TermLet {
                name: "done".into(),
                value: template(0, boolean()),
                body: captured.clone(),
            },
            captured.clone_type(),
        );
        assert!(
            completed
                .validate_recursive_template_scope(&[true], false)
                .is_ok()
        );
    }

    #[test]
    fn terminal_callback_adaptation_precedes_suspension_without_double_continuation() {
        let unit_value = node(
            CheckedTermNode::Raw(Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            }),
            unit(),
        );
        let public = node(
            CheckedTermNode::IntrinsicIfThenElse {
                result_ty: boolean(),
                condition: template(0, boolean()),
                true_body: call(template(1, arrow(unit(), boolean())), unit_value),
                false_body: template(2, boolean()),
            },
            boolean(),
        );
        let terminal = public.adapt_lifted_terminal_callbacks(
            &[None, Some(arrow(unit(), unit())), None, None],
            unit(),
            template(3, arrow(boolean(), unit())),
        );
        let (adapted, _) = adapt(&terminal, &[true, false, false, false], &unit()).unwrap();
        let output = adapted.run(lambda("done".into(), unit(), local("done", unit())));
        let sources = [
            Val::Suspended(0),
            Val::Effect(1),
            Val::Bool(false),
            Val::Effect(3),
        ];
        assert_eq!(
            execute(
                &output,
                &sources,
                &[Val::Bool(true), Val::Unit, Val::Unit, Val::Unit]
            ),
            (Val::Unit, vec![0, 1])
        );
        assert_eq!(
            execute(
                &output,
                &sources,
                &[Val::Bool(false), Val::Unit, Val::Unit, Val::Unit]
            ),
            (Val::Unit, vec![0, 3])
        );
    }

    #[test]
    fn recursive_condition_and_scrutinee_run_only_the_selected_branch() {
        let branch = node(
            CheckedTermNode::IntrinsicIfThenElse {
                result_ty: boolean(),
                condition: template(0, boolean()),
                true_body: template(1, boolean()),
                false_body: template(2, boolean()),
            },
            boolean(),
        );
        assert_eq!(
            result(
                &branch,
                &[true, true, true],
                &[Val::Bool(true), Val::Bool(false), Val::Bool(true)]
            ),
            (Val::Bool(false), vec![0, 1])
        );
        let sum = Type::Sum {
            left: Box::new(boolean()),
            right: Box::new(boolean()),
            meta: zero_meta(),
        };
        let either = node(
            CheckedTermNode::IntrinsicEither {
                left_ty: boolean(),
                right_ty: boolean(),
                result_ty: boolean(),
                value: template(0, sum),
                left_name: "left".into(),
                left_body: local("left", boolean()),
                right_name: "right".into(),
                right_body: template(1, boolean()),
            },
            boolean(),
        );
        assert_eq!(
            result(
                &either,
                &[true, true],
                &[Val::Sum(true, Box::new(Val::Bool(true))), Val::Bool(false)]
            ),
            (Val::Bool(true), vec![0])
        );
    }

    #[test]
    fn captured_recursive_values_remain_rejected() {
        let closure = lambda("arg".into(), unit(), template(0, boolean()));
        assert!(matches!(
            adapt(&closure, &[true], &boolean()),
            Err("recursive value escapes in a function")
        ));
    }

    #[test]
    fn ordinary_callee_precedes_suspended_argument_and_projection_keeps_pair_effects() {
        let unit_value = node(
            CheckedTermNode::Raw(Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            }),
            unit(),
        );
        let callee = call(
            template(0, arrow(unit(), arrow(unit(), unit()))),
            unit_value,
        );
        let root = call(callee, template(1, unit()));
        let (adapted, _) = adapt(&root, &[false, true], &unit()).unwrap();
        let output = adapted.run(lambda("done".into(), unit(), local("done", unit())));
        assert_eq!(
            execute(
                &output,
                &[Val::Effect(0), Val::Suspended(1)],
                &[Val::Identity, Val::Unit]
            ),
            (Val::Unit, vec![0, 1])
        );
        let product = pair(template(0, boolean()), template(1, boolean()));
        let projected = node(
            CheckedTermNode::IntrinsicProjection {
                intrinsic: "__fst__".into(),
                left_ty: boolean(),
                right_ty: boolean(),
                value: product,
            },
            boolean(),
        );
        assert_eq!(
            result(
                &projected,
                &[true, true],
                &[Val::Bool(true), Val::Bool(false)]
            ),
            (Val::Bool(true), vec![0, 1])
        );
    }

    #[test]
    fn completed_recursive_value_can_be_captured_and_lexical_shadowing_is_preserved() {
        let closure = lambda("unused".into(), unit(), local("saved", boolean()));
        let saved = node(
            CheckedTermNode::TermLet {
                name: "saved".into(),
                value: template(0, boolean()),
                body: closure.clone(),
            },
            closure.clone_type(),
        );
        let unit_value = node(
            CheckedTermNode::Raw(Expr::Unit {
                occurrence: Default::default(),
                meta: zero_meta(),
            }),
            unit(),
        );
        let root = call(saved, unit_value);
        assert_eq!(
            result(&root, &[true], &[Val::Bool(true)]),
            (Val::Bool(true), vec![0])
        );
        let inner = node(
            CheckedTermNode::TermLet {
                name: "saved".into(),
                value: template(1, boolean()),
                body: local("saved", boolean()),
            },
            boolean(),
        );
        let body = pair(inner, local("saved", boolean()));
        let root = node(
            CheckedTermNode::TermLet {
                name: "saved".into(),
                value: template(0, boolean()),
                body: body.clone(),
            },
            body.clone_type(),
        );
        assert_eq!(
            result(&root, &[true, true], &[Val::Bool(true), Val::Bool(false)]),
            (
                Val::Pair(Box::new(Val::Bool(false)), Box::new(Val::Bool(true))),
                vec![0, 1]
            )
        );
    }

    #[test]
    fn deep_quoted_computation_transforms_executes_and_drops_iteratively() {
        std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut root = template(0, unit());
            for i in 0..10_000 {
                root = node(
                    CheckedTermNode::TermLet {
                        name: format!("v{i}"),
                        value: template(0, unit()),
                        body: root,
                    },
                    unit(),
                );
            }
            let started = Instant::now();
            let (adapted, count) = adapt(&root, &[true], &unit()).unwrap();
            let transform_elapsed = started.elapsed();
            assert_eq!(count.visits, 20_001);
            let done = lambda("done".into(), unit(), local("done", unit()));
            let output = adapted.run(done);
            let mut graph = vec![output.as_ref()];
            let mut unique = HashSet::new();
            while let Some(term) = graph.pop() {
                if unique.insert(&*term.node as *const CheckedTermNode) {
                    checked_traversal::children(&term.node, |child| graph.push(child));
                }
            }
            assert!(unique.len() <= count.visits * 8);
            let mut tree = vec![output.as_ref()];
            let mut output_occurrences = 0;
            while let Some(term) = tree.pop() {
                output_occurrences += 1;
                checked_traversal::children(&term.node, |child| tree.push(child));
            }
            assert!(output_occurrences <= count.visits * 8);
            let (value, events) = execute(&output, &[Val::Suspended(0)], &[Val::Unit]);
            assert_eq!(value, Val::Unit);
            assert_eq!(events.len(), 10_001);
            eprintln!(
                "quoted-rec prototype: {count:?}, output_nodes={}, output_occurrences={output_occurrences}, transform={transform_elapsed:?}, elapsed={:?}",
                unique.len(),
                started.elapsed()
            );
            drop(output);
            drop(adapted);
            drop(root);
        })
        .unwrap()
        .join()
        .expect("bounded stack transform, execution, and disposal");
    }

    #[test]
    fn deep_public_type_is_cloned_and_disposed_with_bounded_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut ty = unit();
                for _ in 0..2_000 {
                    ty = arrow(unit(), ty);
                }
                let root = template(0, ty);
                let (adapted, _) = adapt(&root, &[true], &unit()).unwrap();
                let output = adapted.run(lambda(
                    "ignore".into(),
                    term_type(&root),
                    node(
                        CheckedTermNode::Raw(Expr::Unit {
                            occurrence: Default::default(),
                            meta: zero_meta(),
                        }),
                        unit(),
                    ),
                ));
                drop(output);
                drop(adapted);
                drop(root);
            })
            .unwrap()
            .join()
            .expect("bounded stack type copying and disposal");
    }

    #[test]
    fn emitted_continuations_are_independently_validated_as_prime() {
        let root = node(
            CheckedTermNode::IntrinsicIfThenElse {
                result_ty: boolean(),
                condition: template(0, boolean()),
                true_body: template(1, boolean()),
                false_body: template(2, boolean()),
            },
            boolean(),
        );
        let (adapted, _) = adapt(&root, &[true, true, true], &unit()).unwrap();
        let done = EvalRef::new(CheckedTerm::new(
            path_expr("finish"),
            arrow(boolean(), unit()),
        ));
        let output = adapted.run(done);
        let values: Vec<_> = (0..3)
            .map(|index| {
                crate::ast::convert_expr::<EvalPhase, PrePrime>(&path_expr(&format!(
                    "suspend{index}"
                )))
            })
            .collect();
        let expanded = output.instantiate_template_pre_prime(
            &values,
            zero_span(),
            &mut |expr, _| crate::ast::convert_expr::<EvalPhase, PrePrime>(expr),
            &mut crate::ast::convert_type::<EvalPhase, PrePrime>,
        );
        let source = "module witness; import __intrinsics__;
        host type Bool role(bool);
        host fn finish(value: Bool) -> .;
        host fn suspend0(k: Bool -> .) -> .;
        host fn suspend1(k: Bool -> .) -> .;
        host fn suspend2(k: Bool -> .) -> .;
        fn run() -> . { () }";
        let parsed = crate::pass::parser::parse(source).unwrap();
        let mut module = crate::prime::lower::lower_module(parsed).unwrap();
        let crate::ast::Item::FnDef(def) = module.items.last_mut().unwrap() else {
            panic!("run function")
        };
        def.body = crate::ast::convert_expr::<PrePrime, crate::ast::Prime>(&expanded);
        let package = crate::pass::resolve::Package::build(
            std::path::Path::new(""),
            vec![(std::path::PathBuf::from("witness.kio"), module)],
            None,
        )
        .unwrap();
        package.resolve_imports().unwrap();
        package.check_no_value_cycles().unwrap();
        package.check_in_body_resolution().unwrap();
        crate::prime::typer::check_package(&package)
            .expect("ordinary Prime after checked-recipe CPS");
    }
}

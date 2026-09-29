use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed, TestRunner};

const MAX_DEPTH: usize = 4;
const MAX_NODES: usize = 24;
const PACKET_MAX_DEPTH: usize = 6;
const PACKET_MAX_NODES: usize = 12;

fn value_shape(value: &Value) -> (usize, usize) {
    fn combine<'a>(children: impl Iterator<Item = &'a Value>) -> (usize, usize) {
        children.fold((1, 1), |(depth, nodes), child| {
            let (child_depth, child_nodes) = value_shape(child);
            (depth.max(child_depth + 1), nodes + child_nodes)
        })
    }

    match value {
        Value::Data(data) => combine(data.args().iter()),
        Value::EvalClosure { captured, .. } => combine(captured.iter()),
        Value::StructuralRecur { step, .. } => combine(std::iter::once(step.as_ref())),
        Value::CaseSplit {
            scrutinee,
            left,
            right,
            ..
        } => combine([scrutinee.as_ref(), left.as_ref(), right.as_ref()].into_iter()),
        Value::Seq { value, body } => combine([value.as_ref(), body.as_ref()].into_iter()),
        Value::Stuck(callee, args) => combine(std::iter::once(callee.as_ref()).chain(args.iter())),
        _ => (1, 1),
    }
}

fn free_atom(prefix: &str, salt: u8) -> Value {
    Value::Atom(format!("nf_free_{prefix}_{salt}"))
}

fn stuck(head: &str, args: Vec<Value>) -> Value {
    Value::Stuck(EvalRef::new(Value::Atom(head.to_owned())), args.into())
}

fn seq(value: Value, body: Value) -> Value {
    Value::Seq {
        value: EvalRef::new(value),
        body: EvalRef::new(body),
    }
}

fn test_closure(param_names: &[String], body: EvalExpr) -> Value {
    let names = param_names.iter().map(String::as_str).collect::<Vec<_>>();
    Value::EvalClosure {
        function: test_eval_function(&names, body),
        captured: Vec::new().into(),
    }
}

fn newtype_value(identity: u8, payload: Value) -> Value {
    let name = format!("GeneratedWrap{identity}");
    let member = ResolvedNewtypeMemberValue {
        constructor: NewtypeCtorIdentity {
            module_path: "nf_eq_properties".to_owned(),
            type_name: name.clone(),
            constructor_name: "wrap".to_owned(),
        },
        kind: NewtypeMemberKind::Constructor {
            payload_abi_arity: 1,
        },
        member_name: "wrap".to_owned(),
        display: format!("{name}.wrap"),
    };
    newtype_data_value(&member, vec![payload])
}

fn base_semantic(kind: u8, salt: u8) -> Value {
    let scalar = || free_atom("token", salt);
    match kind % 7 {
        0 => Value::Unit,
        1 => Value::Bool(salt.is_multiple_of(2)),
        2 => Value::IntAtom(salt.to_string(), Some("GeneratedInt".to_owned())),
        3 => Value::FloatAtom(format!("{}.{}", salt % 7, salt), None),
        4 => Value::StrAtom(format!("text_{salt}")),
        5 => scalar(),
        _ => test_closure(&[format!("__nf_eq_arg_{salt}")], EvalExpr::Local(0)),
    }
}

fn wrap_semantic(value: Value, kind: u8, recipe: u8, salt: u8, level: usize) -> Value {
    let original = value.clone();
    let candidate = match kind % 6 {
        0 => newtype_value(recipe.wrapping_add(level as u8), value),
        1 => value_pair(value, free_atom("pair_right", salt)),
        2 => data_value(
            if recipe.is_multiple_of(2) {
                "__left__"
            } else {
                "__right__"
            },
            vec![value],
        ),
        3 => stuck(&format!("nf_host_{level}_{recipe}"), vec![value]),
        4 => seq(
            stuck(
                &format!("nf_unit_effect_{level}_{recipe}"),
                vec![free_atom("effect_arg", salt)],
            ),
            value,
        ),
        _ => {
            let left_payload = format!("__nf_eq_left_{salt}_{level}_{recipe}");
            let right_payload = format!("__nf_eq_right_{salt}_{level}_{recipe}");
            let scrutinee = if recipe.is_multiple_of(2) {
                free_atom("case_sum", salt)
            } else {
                stuck(
                    &format!("nf_case_source_{level}_{recipe}"),
                    vec![free_atom("case_source_arg", salt)],
                )
            };
            Value::CaseSplit {
                scrutinee: EvalRef::new(scrutinee),
                left_payload: left_payload.clone(),
                left: EvalRef::new(stuck(
                    &format!("nf_left_handler_{level}_{recipe}"),
                    vec![Value::Atom(left_payload), value],
                )),
                right_payload: right_payload.clone(),
                right: EvalRef::new(stuck(
                    &format!("nf_right_handler_{level}_{recipe}"),
                    vec![Value::Atom(right_payload), free_atom("right_arg", salt)],
                )),
            }
        }
    };
    let (depth, nodes) = value_shape(&candidate);
    if depth <= MAX_DEPTH && nodes <= MAX_NODES {
        candidate
    } else {
        original
    }
}

fn semantic_pair_strategy() -> impl Strategy<Value = (Value, Value)> {
    (
        0u8..7,
        proptest::collection::vec((0u8..6, any::<u8>()), 0..=4),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(|(base, wrappers, left_salt, right_salt)| {
            let build = |salt| {
                wrappers.iter().enumerate().fold(
                    base_semantic(base, salt),
                    |value, (level, (kind, recipe))| {
                        wrap_semantic(value, *kind, *recipe, salt, level)
                    },
                )
            };
            (build(left_salt), build(right_salt))
        })
}

fn assert_semantic_shape(value: &Value) {
    let (depth, nodes) = value_shape(value);
    assert!(
        depth <= MAX_DEPTH && nodes <= MAX_NODES,
        "semantic witness exceeded {MAX_DEPTH}/{MAX_NODES}: {depth}/{nodes}: {value:?}"
    );
}

fn fault_free_eq(left: &Value, right: &Value) -> bool {
    let checked =
        nf_eq_checked(left, right, &EvalCtx::new()).expect("semantic witnesses must be fault-free");
    let public = nf_eq(left, right, &EvalCtx::new());
    assert_eq!(checked, public, "checked/public disagreement");
    public
}

fn semantic_eq(left: &Value, right: &Value) -> bool {
    assert_semantic_shape(left);
    assert_semantic_shape(right);
    fault_free_eq(left, right)
}

fn all_directed(values: &[Value; 3], compare: fn(&Value, &Value) -> bool) -> bool {
    (0..3).all(|left| (0..3).all(|right| compare(&values[left], &values[right])))
}

fn eta_closure(head: &str, binder: String) -> Value {
    test_closure(
        &[binder],
        EvalExpr::CallAtom {
            name: head.to_owned(),
            args: vec![EvalRef::new(EvalExpr::Local(0))],
        },
    )
}

fn repaired_product(base: Value) -> Value {
    value_pair(
        stuck("__fst__", vec![base.clone()]),
        stuck("__snd__", vec![base]),
    )
}

fn identity_split(base: Value, suffix: &str) -> Value {
    let left = format!("__nf_eq_eta_left_{suffix}");
    let right = format!("__nf_eq_eta_right_{suffix}");
    Value::CaseSplit {
        scrutinee: EvalRef::new(base),
        left_payload: left.clone(),
        left: EvalRef::new(data_value("__left__", vec![Value::Atom(left)])),
        right_payload: right.clone(),
        right: EvalRef::new(data_value("__right__", vec![Value::Atom(right)])),
    }
}

fn alpha_case(seed: u8, variant: u8, result_head: &str) -> Value {
    let left = format!("__nf_eq_alpha_left_{seed}_{variant}");
    let right = format!("__nf_eq_alpha_right_{seed}_{variant}");
    Value::CaseSplit {
        scrutinee: EvalRef::new(free_atom("alpha_sum", seed)),
        left_payload: left.clone(),
        left: EvalRef::new(stuck(
            result_head,
            vec![Value::Bool(false), Value::Atom(left)],
        )),
        right_payload: right.clone(),
        right: EvalRef::new(stuck(
            result_head,
            vec![Value::Bool(true), Value::Atom(right)],
        )),
    }
}

fn transform_cliques(seed: u8) -> Vec<[Value; 3]> {
    let function = format!("nf_free_function_{seed}");
    let product = free_atom("product", seed);
    let sum = free_atom("sum", seed);
    vec![
        [
            test_closure(&[format!("__nf_eq_x_{seed}")], EvalExpr::Local(0)),
            test_closure(&[format!("__nf_eq_y_{seed}")], EvalExpr::Local(0)),
            test_closure(&[format!("__nf_eq_z_{seed}")], EvalExpr::Local(0)),
        ],
        [
            alpha_case(seed, 0, "nf_case_result"),
            alpha_case(seed, 1, "nf_case_result"),
            alpha_case(seed, 2, "nf_case_result"),
        ],
        [
            Value::Atom(function.clone()),
            eta_closure(&function, format!("__nf_eq_eta_x_{seed}")),
            eta_closure(&function, format!("__nf_eq_eta_y_{seed}")),
        ],
        [product.clone(), repaired_product(product.clone()), product],
        [
            sum.clone(),
            identity_split(sum.clone(), &format!("a_{seed}")),
            identity_split(sum, &format!("b_{seed}")),
        ],
    ]
}

#[derive(Clone, Copy, Debug)]
enum CongruenceSite {
    CoreData,
    NewtypeData,
    StuckCallee,
    StuckArg,
    SeqValue,
    SeqBody,
    CaseScrutinee,
    CaseLeft,
    CaseRight,
}

impl CongruenceSite {
    const ALL: [Self; 9] = [
        Self::CoreData,
        Self::NewtypeData,
        Self::StuckCallee,
        Self::StuckArg,
        Self::SeqValue,
        Self::SeqBody,
        Self::CaseScrutinee,
        Self::CaseLeft,
        Self::CaseRight,
    ];
}

fn scoped_case(scrutinee: Value, left_inner: Value, right_inner: Value, suffix: &str) -> Value {
    let left = format!("__nf_eq_scope_left_{suffix}");
    let right = format!("__nf_eq_scope_right_{suffix}");
    Value::CaseSplit {
        scrutinee: EvalRef::new(scrutinee),
        left_payload: left.clone(),
        left: EvalRef::new(stuck("nf_scoped_left", vec![Value::Atom(left), left_inner])),
        right_payload: right.clone(),
        right: EvalRef::new(stuck(
            "nf_scoped_right",
            vec![Value::Atom(right), right_inner],
        )),
    }
}

fn congruence_pair(site: CongruenceSite, seed: u8) -> (Value, Value) {
    let function = format!("nf_congruence_function_{seed}");
    let fn_pair = || {
        (
            Value::Atom(function.clone()),
            eta_closure(&function, format!("__nf_eq_congruence_arg_{seed}")),
        )
    };
    match site {
        CongruenceSite::CoreData => {
            let (left, right) = fn_pair();
            (
                data_value("__left__", vec![left]),
                data_value("__left__", vec![right]),
            )
        }
        CongruenceSite::NewtypeData => {
            let (left, right) = fn_pair();
            (newtype_value(91, left), newtype_value(91, right))
        }
        CongruenceSite::StuckCallee => {
            let left = alpha_case(seed, 7, "nf_function_branch");
            let right = alpha_case(seed, 8, "nf_function_branch");
            let arg = free_atom("callee_arg", seed);
            (
                Value::Stuck(EvalRef::new(left), vec![arg.clone()].into()),
                Value::Stuck(EvalRef::new(right), vec![arg].into()),
            )
        }
        CongruenceSite::StuckArg => {
            let (left, right) = fn_pair();
            (
                stuck("nf_use_arg", vec![left]),
                stuck("nf_use_arg", vec![right]),
            )
        }
        CongruenceSite::SeqValue => (
            seq(
                alpha_case(seed, 3, "nf_unit_branch"),
                free_atom("seq_body", seed),
            ),
            seq(
                alpha_case(seed, 4, "nf_unit_branch"),
                free_atom("seq_body", seed),
            ),
        ),
        CongruenceSite::SeqBody => {
            let (left, right) = fn_pair();
            let effect = stuck("nf_unit_effect", vec![free_atom("seq_arg", seed)]);
            (seq(effect.clone(), left), seq(effect, right))
        }
        CongruenceSite::CaseScrutinee => {
            let sum = free_atom("case_congruence_sum", seed);
            (
                scoped_case(sum.clone(), Value::Unit, Value::Unit, &format!("a_{seed}")),
                scoped_case(
                    identity_split(sum, &format!("inner_{seed}")),
                    Value::Unit,
                    Value::Unit,
                    &format!("b_{seed}"),
                ),
            )
        }
        CongruenceSite::CaseLeft => {
            let (left, right) = fn_pair();
            let sum = free_atom("case_left_sum", seed);
            (
                scoped_case(sum.clone(), left, Value::Unit, &format!("a_{seed}")),
                scoped_case(sum, right, Value::Unit, &format!("b_{seed}")),
            )
        }
        CongruenceSite::CaseRight => {
            let (left, right) = fn_pair();
            let sum = free_atom("case_right_sum", seed);
            (
                scoped_case(sum.clone(), Value::Unit, left, &format!("a_{seed}")),
                scoped_case(sum, Value::Unit, right, &format!("b_{seed}")),
            )
        }
    }
}

fn right_fold_expr(locals: &[usize]) -> EvalExpr {
    match locals {
        [last] => EvalExpr::Local(*last),
        [first, rest @ ..] => EvalExpr::CoreCtor {
            name: "__pair__",
            args: vec![
                EvalRef::new(EvalExpr::Local(*first)),
                EvalRef::new(right_fold_expr(rest)),
            ],
        },
        [] => unreachable!("packet triangles have width at least two"),
    }
}

fn closure_packet_triangle(width: usize, seed: u8) -> [Value; 3] {
    let unary = test_closure(&[format!("__nf_eq_packet_{seed}")], EvalExpr::Local(0));
    let head_tail = test_closure(
        &[
            format!("__nf_eq_head_{seed}"),
            format!("__nf_eq_tail_{seed}"),
        ],
        right_fold_expr(&[0, 1]),
    );
    let names = (0..width)
        .map(|index| format!("__nf_eq_flat_{seed}_{index}"))
        .collect::<Vec<_>>();
    let locals = (0..width).collect::<Vec<_>>();
    [
        unary,
        head_tail,
        test_closure(&names, right_fold_expr(&locals)),
    ]
}

fn stuck_packet_triangle(width: usize, seed: u8) -> [Value; 3] {
    let values = (0..width)
        .map(|index| Value::Atom(format!("nf_packet_{seed}_{index}")))
        .collect::<Vec<_>>();
    let packet = right_fold_product_values(values.clone());
    let head_tail = vec![
        values[0].clone(),
        right_fold_product_values(values[1..].to_vec()),
    ];
    let head = format!("nf_forward_{seed}");
    [
        stuck(&head, vec![packet]),
        stuck(&head, head_tail),
        stuck(&head, values),
    ]
}

fn certified_inequalities(seed: u8) -> [(Value, Value); 7] {
    let a = free_atom("negative_a", seed);
    let b = free_atom("negative_b", seed);
    let c = free_atom("negative_c", seed);
    [
        (Value::Bool(false), Value::Bool(true)),
        (a.clone(), b.clone()),
        (
            data_value("__left__", vec![Value::Unit]),
            data_value("__right__", vec![Value::Unit]),
        ),
        (
            stuck("nf_head", vec![a.clone()]),
            stuck("nf_head", vec![b.clone()]),
        ),
        (
            seq(stuck("nf_effect", vec![a.clone()]), b.clone()),
            seq(stuck("nf_effect", vec![a.clone()]), c.clone()),
        ),
        (
            scoped_case(a.clone(), b, Value::Unit, &format!("c_{seed}")),
            scoped_case(a, c, Value::Unit, &format!("d_{seed}")),
        ),
        (
            test_closure(&[format!("__nf_eq_identity_{seed}")], EvalExpr::Local(0)),
            test_closure(
                &[format!("__nf_eq_constant_{seed}")],
                EvalExpr::Atom(format!("nf_free_constant_{seed}")),
            ),
        ),
    ]
}

fn raw_value(kind: u8, seed: u8) -> Value {
    let recur = || Value::StructuralRecur {
        root_measure: 2,
        current_measure: 1,
        step: EvalRef::new(Value::Unit),
    };
    match kind % 9 {
        0 => Value::Unit,
        1 => Value::Bool(seed.is_multiple_of(2)),
        2 => free_atom("raw", seed),
        3 => data_value("__left__", vec![free_atom("raw_payload", seed)]),
        4 => stuck("nf_raw_host", vec![free_atom("raw_arg", seed)]),
        5 => seq(
            stuck("nf_raw_effect", vec![Value::Unit]),
            free_atom("raw_body", seed),
        ),
        6 => alpha_case(seed, 9, "nf_raw_branch"),
        7 => recur(),
        _ => Value::Stuck(EvalRef::new(recur()), vec![Value::Unit, Value::Unit].into()),
    }
}

fn raw_pair_strategy() -> impl Strategy<Value = (Value, Value)> {
    (0u8..9, any::<u8>(), 0u8..9, any::<u8>())
        .prop_map(|(left, ls, right, rs)| (raw_value(left, ls), raw_value(right, rs)))
}

fn raw_consistent(left: &Value, right: &Value) -> bool {
    let checked = nf_eq_checked(left, right, &EvalCtx::new());
    let public = nf_eq(left, right, &EvalCtx::new());
    match checked {
        Ok(result) => public == result,
        Err(_) => !public,
    }
}

macro_rules! property {
    ($name:ident, $cases:expr, $seed:expr, $strategy:expr, $check:expr) => {
        #[test]
        fn $name() {
            let config = ProptestConfig {
                cases: $cases,
                failure_persistence: None,
                rng_algorithm: RngAlgorithm::ChaCha,
                rng_seed: RngSeed::Fixed($seed),
                ..ProptestConfig::default()
            };
            TestRunner::new(config).run(&$strategy, $check).unwrap();
        }
    };
}

property!(
    nf_eq_prop_relation,
    192,
    0x5e01,
    semantic_pair_strategy(),
    |(left, right)| {
        prop_assert!(semantic_eq(&left, &left) && semantic_eq(&right, &right));
        prop_assert_eq!(semantic_eq(&left, &right), semantic_eq(&right, &left));
        prop_assert_eq!(
            semantic_eq(&Value::Bool(false), &Value::Bool(true)),
            semantic_eq(&Value::Bool(true), &Value::Bool(false))
        );
        Ok(())
    }
);
property!(nf_eq_prop_transforms, 96, 0x5e02, any::<u8>(), |seed| {
    for clique in transform_cliques(seed) {
        prop_assert!(all_directed(&clique, semantic_eq));
    }
    Ok(())
});
property!(nf_eq_prop_congruence, 96, 0x5e03, any::<u8>(), |seed| {
    for site in CongruenceSite::ALL {
        let (left, right) = congruence_pair(site, seed);
        prop_assert!(
            semantic_eq(&left, &right) && semantic_eq(&right, &left),
            "{site:?}"
        );
    }
    Ok(())
});
property!(nf_eq_prop_packets, 96, 0x5e04, any::<u8>(), |seed| {
    for width in 2..=5 {
        for triangle in [
            closure_packet_triangle(width, seed),
            stuck_packet_triangle(width, seed),
        ] {
            let bounded = triangle.iter().all(|value| {
                let (depth, nodes) = value_shape(value);
                depth <= PACKET_MAX_DEPTH && nodes <= PACKET_MAX_NODES
            });
            prop_assert!(bounded);
            prop_assert!(all_directed(&triangle, fault_free_eq));
        }
    }
    Ok(())
});
property!(nf_eq_prop_inequalities, 96, 0x5e05, any::<u8>(), |seed| {
    for (left, right) in certified_inequalities(seed) {
        prop_assert!(!semantic_eq(&left, &right) && !semantic_eq(&right, &left));
    }
    Ok(())
});
property!(
    nf_eq_prop_raw_consistency,
    192,
    0x5e06,
    raw_pair_strategy(),
    |(left, right)| {
        prop_assert!(raw_consistent(&left, &right));
        prop_assert!(raw_consistent(&raw_value(8, 0), &Value::Unit));
        prop_assert!(raw_consistent(&raw_value(7, 0), &Value::Unit));
        Ok(())
    }
);

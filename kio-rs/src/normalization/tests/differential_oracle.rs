use proptest::prelude::*;
use proptest::test_runner::{
    Config as ProptestConfig, RngAlgorithm, RngSeed, TestCaseError, TestRunner,
};
use std::cell::Cell;

const CAMPAIGN_CASES: usize = 256;
const MAX_TYPE_DEPTH: usize = 2;
const MAX_TERM_DEPTH: usize = 6;

const UNIT: u16 = 1 << 0;
const PRODUCT: u16 = 1 << 1;
const SUM: u16 = 1 << 2;
const ARROW: u16 = 1 << 3;
const VAR: u16 = 1 << 4;
const LAMBDA: u16 = 1 << 5;
const APPLICATION: u16 = 1 << 6;
const LET: u16 = 1 << 7;
const SEQUENCE: u16 = 1 << 8;
const PAIR: u16 = 1 << 9;
const FST: u16 = 1 << 10;
const SND: u16 = 1 << 11;
const LEFT: u16 = 1 << 12;
const RIGHT: u16 = 1 << 13;
const EITHER: u16 = 1 << 14;
const ALL_FEATURES: u16 = (1 << 15) - 1;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Unit,
    Product(Box<Ty>, Box<Ty>),
    Sum(Box<Ty>, Box<Ty>),
    Arrow(Box<Ty>, Box<Ty>),
}

impl Ty {
    fn depth(&self) -> usize {
        match self {
            Self::Unit => 0,
            Self::Product(left, right) | Self::Sum(left, right) | Self::Arrow(left, right) => {
                1 + left.depth().max(right.depth())
            }
        }
    }

    fn render(&self) -> String {
        match self {
            Self::Unit => ".".to_owned(),
            Self::Product(left, right) => format!("({} & {})", left.render(), right.render()),
            Self::Sum(left, right) => format!("({} | {})", left.render(), right.render()),
            Self::Arrow(param, result) => {
                format!("({}) -> {}", param.render(), result.render())
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Term {
    Var(usize),
    Unit,
    Lambda {
        param: Ty,
        body: Box<Term>,
    },
    Application {
        function: Box<Term>,
        argument: Box<Term>,
    },
    Let {
        value: Box<Term>,
        body: Box<Term>,
    },
    Sequence {
        value: Box<Term>,
        body: Box<Term>,
    },
    Pair(Box<Term>, Box<Term>),
    Fst(Box<Term>),
    Snd(Box<Term>),
    Left {
        value: Box<Term>,
        right: Ty,
    },
    Right {
        left: Ty,
        value: Box<Term>,
    },
    Either {
        scrutinee: Box<Term>,
        left: Box<Term>,
        right: Box<Term>,
    },
}

impl Term {
    fn depth(&self) -> usize {
        match self {
            Self::Var(_) | Self::Unit => 0,
            Self::Lambda { body, .. } | Self::Fst(body) | Self::Snd(body) => 1 + body.depth(),
            Self::Application { function, argument }
            | Self::Let {
                value: function,
                body: argument,
            }
            | Self::Sequence {
                value: function,
                body: argument,
            }
            | Self::Pair(function, argument) => 1 + function.depth().max(argument.depth()),
            Self::Left { value, .. } | Self::Right { value, .. } => 1 + value.depth(),
            Self::Either {
                scrutinee,
                left,
                right,
            } => 1 + scrutinee.depth().max(left.depth()).max(right.depth()),
        }
    }
}

fn ground_type(bits: u64, depth: usize) -> Ty {
    if depth == MAX_TYPE_DEPTH {
        return Ty::Unit;
    }
    match bits % 3 {
        0 => Ty::Unit,
        1 => Ty::Product(
            Box::new(ground_type(bits.rotate_left(11), depth + 1)),
            Box::new(ground_type(bits.rotate_right(17), depth + 1)),
        ),
        _ => Ty::Sum(
            Box::new(ground_type(bits.rotate_left(23), depth + 1)),
            Box::new(ground_type(bits.rotate_right(29), depth + 1)),
        ),
    }
}

fn ground_value(ty: &Ty, bits: u64) -> Term {
    match ty {
        Ty::Unit => Term::Unit,
        Ty::Product(left, right) => Term::Pair(
            Box::new(ground_value(left, bits.rotate_left(7))),
            Box::new(ground_value(right, bits.rotate_right(13))),
        ),
        Ty::Sum(left, right) => {
            if bits & 1 == 0 {
                Term::Left {
                    value: Box::new(ground_value(left, bits.rotate_left(19))),
                    right: right.as_ref().clone(),
                }
            } else {
                Term::Right {
                    left: left.as_ref().clone(),
                    value: Box::new(ground_value(right, bits.rotate_right(31))),
                }
            }
        }
        Ty::Arrow(_, _) => unreachable!("campaign result types are ground"),
    }
}

fn sum_value(left: bool, value: Term) -> Term {
    if left {
        Term::Left {
            value: Box::new(value),
            right: Ty::Unit,
        }
    } else {
        Term::Right {
            left: Ty::Unit,
            value: Box::new(value),
        }
    }
}

fn unit_sum(left: bool) -> Term {
    sum_value(left, Term::Unit)
}

fn beta_witness(left: bool) -> Term {
    Term::Application {
        function: Box::new(Term::Lambda {
            param: Ty::Sum(Box::new(Ty::Unit), Box::new(Ty::Unit)),
            body: Box::new(Term::Var(0)),
        }),
        argument: Box::new(unit_sum(left)),
    }
}

fn let_witness() -> Term {
    Term::Application {
        function: Box::new(Term::Lambda {
            param: Ty::Unit,
            body: Box::new(Term::Let {
                value: Box::new(unit_sum(true)),
                body: Box::new(Term::Let {
                    value: Box::new(unit_sum(false)),
                    body: Box::new(Term::Fst(Box::new(Term::Pair(
                        Box::new(Term::Var(1)),
                        Box::new(Term::Var(0)),
                    )))),
                }),
            }),
        }),
        argument: Box::new(Term::Unit),
    }
}

fn projection_witness(first: bool) -> Term {
    let pair = Term::Pair(Box::new(unit_sum(true)), Box::new(unit_sum(false)));
    if first {
        Term::Fst(Box::new(pair))
    } else {
        Term::Snd(Box::new(pair))
    }
}

fn either_witness(choose_left: bool) -> Term {
    Term::Either {
        scrutinee: Box::new(unit_sum(choose_left)),
        left: Box::new(Term::Lambda {
            param: Ty::Unit,
            body: Box::new(sum_value(true, Term::Var(0))),
        }),
        right: Box::new(Term::Lambda {
            param: Ty::Unit,
            body: Box::new(sum_value(false, Term::Var(0))),
        }),
    }
}

fn campaign_term(index: usize, type_bits: u64, value_bits: u64) -> (Term, Option<bool>) {
    let required = 1 << (index % 15);
    let (ty, forced_branch) = match required {
        UNIT => (Ty::Unit, None),
        PRODUCT | PAIR => (Ty::Product(Box::new(Ty::Unit), Box::new(Ty::Unit)), None),
        SUM => (
            Ty::Sum(Box::new(Ty::Unit), Box::new(Ty::Unit)),
            Some(value_bits & 1 == 0),
        ),
        LEFT => (Ty::Sum(Box::new(Ty::Unit), Box::new(Ty::Unit)), Some(true)),
        RIGHT => (Ty::Sum(Box::new(Ty::Unit), Box::new(Ty::Unit)), Some(false)),
        _ => (ground_type(type_bits, 0), None),
    };
    let base = match (&ty, forced_branch) {
        (Ty::Sum(left, right), Some(true)) => Term::Left {
            value: Box::new(ground_value(left, value_bits.rotate_left(5))),
            right: right.as_ref().clone(),
        },
        (Ty::Sum(left, right), Some(false)) => Term::Right {
            left: left.as_ref().clone(),
            value: Box::new(ground_value(right, value_bits.rotate_right(5))),
        },
        _ => ground_value(&ty, value_bits),
    };

    let either_direction = (required == EITHER).then_some((index / 15).is_multiple_of(2));
    let term = match required {
        ARROW | VAR | LAMBDA | APPLICATION => beta_witness(index.is_multiple_of(2)),
        LET => let_witness(),
        SEQUENCE => Term::Sequence {
            value: Box::new(Term::Unit),
            body: Box::new(unit_sum(false)),
        },
        FST => projection_witness(true),
        SND => projection_witness(false),
        EITHER => either_witness(either_direction.expect("either direction is present")),
        UNIT | PRODUCT | SUM | PAIR | LEFT | RIGHT => base,
        _ => unreachable!("the required feature is one of the fifteen campaign bits"),
    };
    (term, either_direction)
}

fn infer(term: &Term, env: &[Ty]) -> Result<Ty, String> {
    let inferred = match term {
        Term::Var(index) => env
            .get(*index)
            .cloned()
            .ok_or_else(|| format!("unbound de Bruijn index {index}")),
        Term::Unit => Ok(Ty::Unit),
        Term::Lambda { param, body } => {
            let mut nested = env.to_vec();
            nested.insert(0, param.clone());
            Ok(Ty::Arrow(
                Box::new(param.clone()),
                Box::new(infer(body, &nested)?),
            ))
        }
        Term::Application { function, argument } => {
            let function_ty = infer(function, env)?;
            let argument_ty = infer(argument, env)?;
            let Ty::Arrow(param, result) = function_ty else {
                return Err("application callee is not a function".to_owned());
            };
            if *param != argument_ty {
                return Err("application argument type mismatch".to_owned());
            }
            Ok(*result)
        }
        Term::Let { value, body } => {
            let value_ty = infer(value, env)?;
            let mut nested = env.to_vec();
            nested.insert(0, value_ty);
            infer(body, &nested)
        }
        Term::Sequence { value, body } => {
            let value_ty = infer(value, env)?;
            if value_ty != Ty::Unit {
                return Err(format!("sequence expected Unit, found {value_ty:?}"));
            }
            infer(body, env)
        }
        Term::Pair(left, right) => Ok(Ty::Product(
            Box::new(infer(left, env)?),
            Box::new(infer(right, env)?),
        )),
        Term::Fst(pair) => {
            let Ty::Product(left, _) = infer(pair, env)? else {
                return Err("fst operand is not a product".to_owned());
            };
            Ok(*left)
        }
        Term::Snd(pair) => {
            let Ty::Product(_, right) = infer(pair, env)? else {
                return Err("snd operand is not a product".to_owned());
            };
            Ok(*right)
        }
        Term::Left { value, right } => Ok(Ty::Sum(
            Box::new(infer(value, env)?),
            Box::new(right.clone()),
        )),
        Term::Right { left, value } => Ok(Ty::Sum(
            Box::new(left.clone()),
            Box::new(infer(value, env)?),
        )),
        Term::Either {
            scrutinee,
            left,
            right,
        } => {
            let Ty::Sum(left_payload, right_payload) = infer(scrutinee, env)? else {
                return Err("either scrutinee is not a sum".to_owned());
            };
            let Ty::Arrow(left_param, left_result) = infer(left, env)? else {
                return Err("either left arm is not a function".to_owned());
            };
            let Ty::Arrow(right_param, right_result) = infer(right, env)? else {
                return Err("either right arm is not a function".to_owned());
            };
            if left_param != left_payload || right_param != right_payload {
                return Err("either handler payload type mismatch".to_owned());
            }
            if left_result != right_result {
                return Err("either handler result type mismatch".to_owned());
            }
            Ok(*left_result)
        }
    }?;
    if inferred.depth() > MAX_TYPE_DEPTH {
        return Err(format!(
            "subterm type is deeper than {MAX_TYPE_DEPTH}: {inferred:?}"
        ));
    }
    Ok(inferred)
}

fn rewrite_vars(term: &Term, binder_depth: usize, rewrite: &impl Fn(usize, usize) -> Term) -> Term {
    match term {
        Term::Var(index) => rewrite(*index, binder_depth),
        Term::Unit => Term::Unit,
        Term::Lambda { param, body } => Term::Lambda {
            param: param.clone(),
            body: Box::new(rewrite_vars(body, binder_depth + 1, rewrite)),
        },
        Term::Application { function, argument } => Term::Application {
            function: Box::new(rewrite_vars(function, binder_depth, rewrite)),
            argument: Box::new(rewrite_vars(argument, binder_depth, rewrite)),
        },
        Term::Let { value, body } => Term::Let {
            value: Box::new(rewrite_vars(value, binder_depth, rewrite)),
            body: Box::new(rewrite_vars(body, binder_depth + 1, rewrite)),
        },
        Term::Sequence { value, body } => Term::Sequence {
            value: Box::new(rewrite_vars(value, binder_depth, rewrite)),
            body: Box::new(rewrite_vars(body, binder_depth, rewrite)),
        },
        Term::Pair(left, right) => Term::Pair(
            Box::new(rewrite_vars(left, binder_depth, rewrite)),
            Box::new(rewrite_vars(right, binder_depth, rewrite)),
        ),
        Term::Fst(pair) => Term::Fst(Box::new(rewrite_vars(pair, binder_depth, rewrite))),
        Term::Snd(pair) => Term::Snd(Box::new(rewrite_vars(pair, binder_depth, rewrite))),
        Term::Left { value, right } => Term::Left {
            value: Box::new(rewrite_vars(value, binder_depth, rewrite)),
            right: right.clone(),
        },
        Term::Right { left, value } => Term::Right {
            left: left.clone(),
            value: Box::new(rewrite_vars(value, binder_depth, rewrite)),
        },
        Term::Either {
            scrutinee,
            left,
            right,
        } => Term::Either {
            scrutinee: Box::new(rewrite_vars(scrutinee, binder_depth, rewrite)),
            left: Box::new(rewrite_vars(left, binder_depth, rewrite)),
            right: Box::new(rewrite_vars(right, binder_depth, rewrite)),
        },
    }
}

fn shift(term: &Term, amount: isize) -> Term {
    rewrite_vars(term, 0, &|index, depth| {
        if index < depth {
            return Term::Var(index);
        }
        let shifted = (index as isize)
            .checked_add(amount)
            .and_then(|index| usize::try_from(index).ok())
            .expect("well-scoped substitution never shifts below zero");
        Term::Var(shifted)
    })
}

fn substitute(term: &Term, target: usize, replacement: &Term) -> Term {
    rewrite_vars(term, 0, &|index, depth| {
        if index == target + depth {
            shift(replacement, depth as isize)
        } else {
            Term::Var(index)
        }
    })
}

fn substitute_top(body: &Term, replacement: &Term) -> Term {
    let lifted = shift(replacement, 1);
    shift(&substitute(body, 0, &lifted), -1)
}

fn normal_order(term: Term) -> Term {
    match term {
        term @ (Term::Var(_) | Term::Unit) => term,
        Term::Lambda { param, body } => Term::Lambda {
            param,
            body: Box::new(normal_order(*body)),
        },
        Term::Application { function, argument } => match normal_order(*function) {
            Term::Lambda { body, .. } => normal_order(substitute_top(&body, &argument)),
            function => Term::Application {
                function: Box::new(function),
                argument: Box::new(normal_order(*argument)),
            },
        },
        Term::Let { value, body } => normal_order(substitute_top(&body, &value)),
        Term::Sequence { value, body } => {
            let value = normal_order(*value);
            if matches!(&value, Term::Unit) {
                normal_order(*body)
            } else {
                Term::Sequence {
                    value: Box::new(value),
                    body: Box::new(normal_order(*body)),
                }
            }
        }
        Term::Pair(left, right) => Term::Pair(
            Box::new(normal_order(*left)),
            Box::new(normal_order(*right)),
        ),
        Term::Fst(pair) => match normal_order(*pair) {
            Term::Pair(left, _) => normal_order(*left),
            pair => Term::Fst(Box::new(pair)),
        },
        Term::Snd(pair) => match normal_order(*pair) {
            Term::Pair(_, right) => normal_order(*right),
            pair => Term::Snd(Box::new(pair)),
        },
        Term::Left { value, right } => Term::Left {
            value: Box::new(normal_order(*value)),
            right,
        },
        Term::Right { left, value } => Term::Right {
            left,
            value: Box::new(normal_order(*value)),
        },
        Term::Either {
            scrutinee,
            left,
            right,
        } => match normal_order(*scrutinee) {
            Term::Left { value, .. } => normal_order(Term::Application {
                function: left,
                argument: value,
            }),
            Term::Right { value, .. } => normal_order(Term::Application {
                function: right,
                argument: value,
            }),
            scrutinee => Term::Either {
                scrutinee: Box::new(scrutinee),
                left: Box::new(normal_order(*left)),
                right: Box::new(normal_order(*right)),
            },
        },
    }
}

fn collect_features(term: &Term, features: &mut u16) {
    match term {
        Term::Var(_) => *features |= VAR,
        Term::Unit => *features |= UNIT,
        Term::Lambda { body, .. } => {
            *features |= LAMBDA | ARROW;
            collect_features(body, features);
        }
        Term::Application { function, argument } => {
            *features |= APPLICATION;
            collect_features(function, features);
            collect_features(argument, features);
        }
        Term::Let { value, body } => {
            *features |= LET;
            collect_features(value, features);
            collect_features(body, features);
        }
        Term::Sequence { value, body } => {
            *features |= SEQUENCE;
            collect_features(value, features);
            collect_features(body, features);
        }
        Term::Pair(left, right) => {
            *features |= PAIR | PRODUCT;
            collect_features(left, features);
            collect_features(right, features);
        }
        Term::Fst(pair) => {
            *features |= FST;
            collect_features(pair, features);
        }
        Term::Snd(pair) => {
            *features |= SND;
            collect_features(pair, features);
        }
        Term::Left { value, .. } => {
            *features |= LEFT | SUM;
            collect_features(value, features);
        }
        Term::Right { value, .. } => {
            *features |= RIGHT | SUM;
            collect_features(value, features);
        }
        Term::Either {
            scrutinee,
            left,
            right,
        } => {
            *features |= EITHER;
            collect_features(scrutinee, features);
            collect_features(left, features);
            collect_features(right, features);
        }
    }
}

#[derive(Clone, Default)]
struct RenderEnv {
    names: Vec<String>,
    types: Vec<Ty>,
}

impl RenderEnv {
    fn with_binder(&self, ty: Ty) -> (Self, String) {
        let name = format!("v{}", self.names.len());
        let mut nested = self.clone();
        nested.names.insert(0, name.clone());
        nested.types.insert(0, ty);
        (nested, name)
    }
}

fn render_expr(term: &Term, env: &RenderEnv) -> String {
    match term {
        Term::Var(index) => env
            .names
            .get(*index)
            .cloned()
            .expect("well-typed render has a bound variable"),
        Term::Unit => "()".to_owned(),
        Term::Lambda { param, body } => {
            let (nested, name) = env.with_binder(param.clone());
            let result = infer(body, &nested.types).expect("well-typed lambda body");
            format!(
                ".({name}: {}) -> {} {{ {} }}",
                param.render(),
                result.render(),
                render_body(body, &nested)
            )
        }
        Term::Application { function, argument } => {
            format!(
                "{}({})",
                render_expr(function, env),
                render_expr(argument, env)
            )
        }
        Term::Let { .. } | Term::Sequence { .. } => {
            unreachable!("statement terms are rendered through a Kio body")
        }
        Term::Pair(left, right) => {
            let left_ty = infer(left, &env.types).expect("well-typed pair left");
            let right_ty = infer(right, &env.types).expect("well-typed pair right");
            format!(
                "__pair__({}, {}, {}, {})",
                left_ty.render(),
                right_ty.render(),
                render_expr(left, env),
                render_expr(right, env)
            )
        }
        Term::Fst(pair) | Term::Snd(pair) => {
            let Ty::Product(left, right) = infer(pair, &env.types).expect("well-typed projection")
            else {
                unreachable!("projection operand was checked as a product")
            };
            let intrinsic = if matches!(term, Term::Fst(_)) {
                "__fst__"
            } else {
                "__snd__"
            };
            format!(
                "{intrinsic}({}, {}, {})",
                left.render(),
                right.render(),
                render_expr(pair, env)
            )
        }
        Term::Left { value, right } => {
            let left = infer(value, &env.types).expect("well-typed left payload");
            format!(
                "__left__({}, {}, {})",
                left.render(),
                right.render(),
                render_expr(value, env)
            )
        }
        Term::Right { left, value } => {
            let right = infer(value, &env.types).expect("well-typed right payload");
            format!(
                "__right__({}, {}, {})",
                left.render(),
                right.render(),
                render_expr(value, env)
            )
        }
        Term::Either {
            scrutinee,
            left,
            right,
        } => {
            let Ty::Sum(left_payload, right_payload) =
                infer(scrutinee, &env.types).expect("well-typed either scrutinee")
            else {
                unreachable!("either scrutinee was checked as a sum")
            };
            let result = infer(term, &env.types).expect("well-typed either result");
            format!(
                "__either__({}, {}, {}, {}, {}, {})",
                left_payload.render(),
                right_payload.render(),
                result.render(),
                render_expr(scrutinee, env),
                render_expr(left, env),
                render_expr(right, env)
            )
        }
    }
}

fn render_body(term: &Term, env: &RenderEnv) -> String {
    match term {
        Term::Let { value, body } => {
            let value_ty = infer(value, &env.types).expect("well-typed let value");
            let (nested, name) = env.with_binder(value_ty);
            format!(
                "let {name} = {}; {}",
                render_expr(value, env),
                render_body(body, &nested)
            )
        }
        Term::Sequence { value, body } => {
            format!("{}; {}", render_expr(value, env), render_body(body, env))
        }
        _ => render_expr(term, env),
    }
}

fn render_module(term: &Term, result: &Ty) -> String {
    format!(
        "module main;\n\nimport __intrinsics__;\n\npure fn main() -> {} {{ {} }}\n",
        result.render(),
        render_body(term, &RenderEnv::default())
    )
}

fn render_ground(term: &Term) -> Option<String> {
    match term {
        Term::Unit => Some("()".to_owned()),
        Term::Pair(left, right) => Some(format!(
            "__pair__({}, {})",
            render_ground(left)?,
            render_ground(right)?
        )),
        Term::Left { value, .. } => Some(format!("__left__({})", render_ground(value)?)),
        Term::Right { value, .. } => Some(format!("__right__({})", render_ground(value)?)),
        _ => None,
    }
}

#[test]
fn normalization_matches_independent_normal_order_oracle() {
    let config = ProptestConfig {
        cases: 1,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0xd1ff_e12a),
        ..ProptestConfig::default()
    };
    let features = Cell::new(0);
    let either_directions = Cell::new(0);
    let executed_cases = Cell::new(0);
    let mut runner = TestRunner::new(config);
    let strategy = (any::<u64>(), any::<u64>());
    for index in 0..CAMPAIGN_CASES {
        let tree = strategy.new_tree(&mut runner).unwrap();
        runner
            .run_one(tree, |(type_bits, value_bits)| {
                let (term, either_direction) = campaign_term(index, type_bits, value_bits);
                let mut case_features = 0;
                collect_features(&term, &mut case_features);
                prop_assert!(
                    term.depth() <= MAX_TERM_DEPTH,
                    "case {index} has term depth {}: {term:?}",
                    term.depth()
                );
                let result = infer(&term, &[])
                    .map_err(|error| TestCaseError::fail(format!("case {index}: {error}")))?;

                let normal = normal_order(term.clone());
                let normal_type = infer(&normal, &[]).map_err(TestCaseError::fail)?;
                prop_assert_eq!(&normal_type, &result, "case {} changed type", index);
                let expected = render_ground(&normal).ok_or_else(|| {
                    TestCaseError::fail(format!("case {index} non-ground: {normal:?}"))
                })?;
                let module = render_module(&term, &result);
                let actual = crate::normalize_source::normalize_source(
                    "package differential;\n\nbridge { main; }\n",
                    &module,
                )
                .map_err(|error| {
                    TestCaseError::fail(format!("case {index}: {error:?}\n{module}"))
                })?;
                prop_assert_eq!(
                    actual,
                    expected,
                    "case {} disagreed with the normal-order oracle\n{}",
                    index,
                    module
                );
                features.set(features.get() | case_features);
                if let Some(left) = either_direction {
                    either_directions.set(either_directions.get() | if left { 1 } else { 2 });
                }
                executed_cases.set(executed_cases.get() + 1);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(executed_cases.get(), CAMPAIGN_CASES, "campaign case count");
    assert_eq!(features.get(), ALL_FEATURES, "campaign feature coverage");
    assert_eq!(either_directions.get(), 3, "both either directions");
}

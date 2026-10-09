use super::*;

fn visitor(elabs: &Elaborations) -> SubstituteVisitor<'_> {
    SubstituteVisitor {
        elabs,
        source_bindings: None,
        module_path: "m".to_owned(),
        module: None,
        package: None,
        prime_type_requalifier: PrimeRequalifier::Disabled,
        type_binders: Vec::new(),
        prime_requalifier_seeded: false,
        comptime_lowering_mode: ComptimeLoweringMode::RuntimeErase,
    }
}

fn unit() -> Expr<PrePrime> {
    Expr::Unit {
        source_trivia: Default::default(),
        occurrence: (),
        meta: Meta::new(Span::new(10, 20)),
    }
}

fn args(width: usize, depth: usize) -> Vec<CallArg<PrePrime>> {
    (0..width)
        .map(|index| {
            let mut value = unit();
            let mut ty = Type::Unit {
                meta: Meta::new(Span::new(index as u32, index as u32 + 1)),
            };
            for _ in 0..depth {
                value = Expr::synth_call(value, vec![CallArg::Value(unit())], Span::new(10, 20));
                ty = Type::Function {
                    param: Box::new(Type::Unit {
                        meta: Meta::new(Span::new(10, 20)),
                    }),
                    ret: Box::new(ty),
                    abi_arity: 1,
                    caps: (),
                    meta: Meta::new(Span::new(10, 20)),
                };
            }
            if index % 2 == 0 {
                CallArg::Value(value)
            } else {
                CallArg::Type(ty)
            }
        })
        .collect()
}

fn legacy_splits(
    visitor: &SubstituteVisitor<'_>,
    callee: Expr<PrePrime>,
    args: Vec<CallArg<PrePrime>>,
    span: Span,
    occurrence: Option<crate::ast::ExpressionOccurrenceId>,
) -> Expr<PrePrime> {
    let Some(splits) =
        occurrence.and_then(|id| visitor.elabs.call_splits_for(&visitor.module_path, id))
    else {
        return Expr::synth_call(callee, args, span);
    };
    let mut current = callee;
    let mut start = 0;
    for split in splits {
        if *split <= start || *split >= args.len() {
            continue;
        }
        current = Expr::synth_call(current, args[start..*split].to_vec(), span);
        start = *split;
    }
    Expr::synth_call(current, args[start..].to_vec(), span)
}

fn allocation(arg: &CallArg<PrePrime>) -> *const () {
    match arg {
        CallArg::Value(Expr::Call { callee, .. }) => std::ptr::from_ref(callee.as_ref()).cast(),
        CallArg::Type(Type::Function { ret, .. }) => std::ptr::from_ref(ret.as_ref()).cast(),
        _ => panic!("fixture argument has an owned subtree"),
    }
}

fn stage_args(expr: &Expr<PrePrime>) -> Vec<&CallArg<PrePrime>> {
    match expr {
        Expr::Call { callee, args, .. } => {
            let mut ordered = stage_args(callee);
            ordered.extend(args);
            ordered
        }
        Expr::Unit { .. } => Vec::new(),
        _ => panic!("fixture callee or stage"),
    }
}

#[test]
fn recorded_call_stages_move_each_owned_argument_and_preserve_exact_tree() {
    let span = Span::new(30, 40);
    let id = crate::ast::ExpressionOccurrence::fresh().key();
    for splits in [vec![], vec![0, 8, 12], vec![1, 3, 3, 5, 8, 12]] {
        let mut elabs = Elaborations::new();
        for split in splits {
            elabs.record_call_split("m", id, split);
        }
        let visitor = visitor(&elabs);
        for occurrence in [None, Some(id)] {
            let args = args(8, 4);
            let allocations: Vec<_> = args.iter().map(allocation).collect();
            let expected = legacy_splits(&visitor, unit(), args.clone(), span, occurrence);
            let actual = visitor.call_with_recorded_splits(unit(), args, span, occurrence);
            assert_eq!(actual, expected);
            assert_eq!(
                stage_args(&actual)
                    .into_iter()
                    .map(allocation)
                    .collect::<Vec<_>>(),
                allocations
            );
        }
    }
}

#[test]
fn checked_call_replay_preserves_written_inferred_and_reclassified_slots() {
    use crate::ast::{ExpressionOccurrence, ExpressionSite};
    use crate::pass::typecheck_core::InternedType;
    let span = Span::new(10, 20);
    let occurrence = ExpressionOccurrence::fresh();
    let site = ExpressionSite {
        id: occurrence.key(),
        span,
    };
    let ty = || Type::<Lowered>::Unit {
        meta: Meta::new(span),
    };
    let value = || Expr::<Lowered>::Unit {
        source_trivia: Default::default(),
        occurrence: ExpressionOccurrence::fresh(),
        meta: Meta::new(span),
    };
    let mut elabs = Elaborations::new();
    elabs.record_call_implicit_unit_slot("m", site.id, 0);
    // Explicit types win, inferred placeholders consume, reclassified values
    // consume, and omitted/trailing types do not consume written arguments.
    for slot in [1, 2, 3, 4, 6] {
        elabs.record_type_resolution("m", site.id, slot, InternedType::fresh(ty()));
    }
    elabs.mark_value_at_type_slot("m", site.id, 3);
    let call = Expr::Call {
        source_trivia: Default::default(),
        occurrence,
        callee: Box::new(value()),
        args: vec![
            CallArg::Type(ty()),
            CallArg::Type(Type::Infer {
                ext: (),
                meta: Meta::new(span),
            }),
            CallArg::Value(value()),
            CallArg::Value(value()),
        ],
        diagnostic: Default::default(),
        ext: (),
        meta: Meta::new(span),
    };
    let actual = visitor(&elabs).visit_expr_call(&call).unwrap();
    let Expr::Call { args, .. } = actual else {
        panic!("canonical call");
    };
    assert_eq!(args.len(), 7);
    assert_eq!(
        args.iter()
            .map(|arg| matches!(arg, CallArg::Type(_)))
            .collect::<Vec<_>>(),
        [false, true, true, true, true, false, true]
    );
}

#[test]
#[ignore = "bounded manual same-binary stage-assembly time/RSS comparison"]
fn recorded_call_stage_cost() {
    let strategy = std::env::var("KIO_DEBUG_CHECKED_CALL_STRATEGY").unwrap();
    let size = std::env::var("KIO_DEBUG_CHECKED_CALL_SIZE").unwrap();
    let (width, depth, repetitions) = match size.as_str() {
        "ordinary" => (2, 1, 20_000),
        "wide" => (16, 16, 2_000),
        _ => panic!("ordinary or wide"),
    };
    let id = crate::ast::ExpressionOccurrence::fresh().key();
    let mut elabs = Elaborations::new();
    for split in (1..width).step_by(2) {
        elabs.record_call_split("m", id, split);
    }
    let visitor = visitor(&elabs);
    let args = args(width, depth);
    let baseline = match strategy.as_str() {
        "baseline" => true,
        "candidate" => false,
        _ => panic!("baseline or candidate"),
    };
    let start = std::time::Instant::now();
    for _ in 0..repetitions {
        let args = args.clone();
        std::hint::black_box(if baseline {
            legacy_splits(&visitor, unit(), args, Span::new(30, 40), Some(id))
        } else {
            visitor.call_with_recorded_splits(unit(), args, Span::new(30, 40), Some(id))
        });
    }
    eprintln!(
        "strategy={strategy} size={size} repetitions={repetitions} elapsed_us={}",
        start.elapsed().as_micros()
    );
}

use super::*;
use crate::pass::typecheck_core::goals::GOAL_FREE_PROBES;
use crate::pass::typecheck_core::{
    InternedType, IsolatedSpecializationContextBuilder, ScopedType, TypeCtx,
};
use crate::span::Span;

fn with_frontier(
    f: impl for<'m> FnOnce(
        &mut crate::pass::typecheck_core::apply::frontier::ConnectedCallFrontier<'_, 'm>,
        &TypeCtx<'m, '_, Lowered>,
    ),
) {
    let package = fixture(
        "module boundary; type Alias = .; type Applied[A] = A; host type Token; host type Carrier[A];",
        "boundary",
    );
    let module = &package.module("boundary").unwrap().module;
    let env =
        crate::pass::typecheck_core::ModuleEnv::build(module, None, None, Some(&package)).unwrap();
    let mut elaborations = Elaborations::new();
    let tcx = TypeCtx::new(&env, &mut elaborations);
    let span = Span::new(10, 20);
    let (mut root, ()) =
        crate::pass::typecheck_core::apply::frontier::OrdinaryFrontier::begin_root_with(
            span,
            &tcx,
            |_, _| Ok(()),
        )
        .unwrap();
    let (reservation, ()) = root
        .reserve_root_child_with(
            crate::pass::typecheck_core::GoalOwnerKind::RetainedValue,
            span,
            &tcx,
            |_, _| Ok(()),
        )
        .unwrap();
    let mut child = root.open_reserved_child(reservation).unwrap();
    f(&mut root.retained_value_frontier(&mut child), &tcx);
}

fn projected(ty: ScopedType) -> ProjectedType {
    ProjectedType {
        seal: InvocationSeal::fresh(),
        specialization: ty.clone(),
        ty,
        closed: None,
        focus: None,
        contains_focus: false,
        producers: ProducerSet::default(),
    }
}

fn applied_goal<'m>(
    frontier: &mut crate::pass::typecheck_core::apply::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &TypeCtx<'m, '_, Lowered>,
    constructor: &str,
) -> Result<ScopedType, crate::error::Error> {
    use crate::pass::typecheck_core::{GoalOrigin, GoalRole, GoalSolutionPolicy};
    let span = Span::new(10, 20);
    let goal = frontier.reserve_goal_in_live_delta(
        crate::ast::Kind::arrow_chain(1),
        GoalSolutionPolicy::Monotype,
        GoalOrigin::named(span, GoalRole::TypeArgument, "F"),
    )?;
    let (store, owner, delta, _) = frontier.parts_mut();
    let head = store.scoped_goal_at(goal, Vec::new(), owner, span)?;
    let candidate = store.scoped_type(
        owner,
        InternedType::fresh(crate::ast::Type::synth_path(
            vec![constructor.to_owned()],
            Vec::new(),
            span,
        )),
        span,
    )?;
    store.constrain(delta, head, candidate, span, tcx)?;
    store.scoped_goal_at(
        goal,
        vec![crate::ast::Type::Unit {
            meta: Meta::new(span),
        }],
        owner,
        span,
    )
}

#[test]
fn higher_kinded_goal_binding_rejects_an_unsaturated_alias() {
    with_frontier(|frontier, tcx| {
        let error = applied_goal(frontier, tcx, "Applied").unwrap_err();
        assert!(
            error
                .diag()
                .1
                .contains("type alias `Applied` requires 1 type argument, but 0 were supplied")
        );
    });
}

fn legacy_materialize<'m>(
    slot: Option<ScopedType>,
    frontier: &mut crate::pass::typecheck_core::apply::frontier::ConnectedCallFrontier<'_, 'm>,
    tcx: &TypeCtx<'m, '_, Lowered>,
) -> Result<crate::pass::typecheck_full::TypedUserElaboratorTypeSlot, crate::error::Error> {
    let Some(slot) = slot else {
        return Ok(crate::pass::typecheck_full::TypedUserElaboratorTypeSlot {
            resolved: None,
            occurrence: None,
        });
    };
    let (store, _, delta, _) = frontier.parts_mut();
    let occurrence = store.zonk_for_progress(delta, slot.clone(), tcx)?;
    let resolved = store.require_goal_free_after_delta(delta, slot, tcx)?;
    Ok(crate::pass::typecheck_full::TypedUserElaboratorTypeSlot {
        resolved: Some(resolved.ty().clone()),
        occurrence: Some(occurrence.ty().clone()),
    })
}

#[test]
fn settled_projected_types_normalize_once_with_identical_scope_identity_and_errors() {
    with_frontier(|frontier, tcx| {
        let span = Span::new(10, 20);
        let open = frontier.reserve_projected_header_hole(span).unwrap();
        let mut types = vec![open.clone()];
        for name in ["Alias", "Token"] {
            types.push(
                frontier
                    .store()
                    .scoped_type(
                        frontier.owner(),
                        InternedType::fresh(crate::ast::Type::synth_path(
                            vec![name.to_owned()],
                            Vec::new(),
                            span,
                        )),
                        span,
                    )
                    .unwrap(),
            );
        }
        let applied = applied_goal(frontier, tcx, "Carrier").unwrap();
        let aliased = frontier
            .store()
            .scoped_type(
                frontier.owner(),
                InternedType::fresh(crate::ast::Type::synth_path(
                    vec!["Applied".to_owned()],
                    vec![applied.ty().clone_type()],
                    span,
                )),
                span,
            )
            .unwrap();
        // A saturated alias can contain a valid applied higher-kinded goal.
        // Snapshotting retains its original second-normalization semantics.
        let mut snapshot = projected(aliased.clone());
        snapshot_projected_type(
            &mut snapshot,
            &mut IsolatedSpecializationContextBuilder::new(tcx),
            frontier,
            tcx,
        )
        .unwrap();
        assert!(
            matches!(snapshot.closed.unwrap().as_type(), crate::ast::Type::Path { segments, args, .. }
            if segments.last().unwrap().name == "Carrier" && matches!(args.as_slice(), [crate::ast::Type::Unit { .. }]))
        );
        types.extend([applied, aliased]);
        for ty in types {
            GOAL_FREE_PROBES.set(0);

            let (store, _, delta, _) = frontier.parts_mut();
            let occurrence = store.zonk_for_progress(delta, ty.clone(), tcx).unwrap();
            let expected = store.require_goal_free_after_delta(delta, ty.clone(), tcx);
            assert_eq!(GOAL_FREE_PROBES.get(), 2);
            if let Ok(expected) = &expected {
                assert!(expected.representation_is_exact(&occurrence));
            }
            GOAL_FREE_PROBES.set(0);
            let actual = materialize_projected_type_slot(Some(ty), frontier, tcx);
            assert_eq!(GOAL_FREE_PROBES.get(), 1);
            match (actual, expected) {
                (Ok(actual), Ok(expected)) => {
                    assert!(
                        actual
                            .resolved
                            .as_ref()
                            .unwrap()
                            .representation_is_exact(expected.ty())
                    );
                    assert!(
                        actual
                            .occurrence
                            .as_ref()
                            .unwrap()
                            .representation_is_exact(occurrence.ty())
                    );
                    assert!(actual.resolved.unwrap().identity_is_canonical());
                }
                (Err(actual), Err(expected)) => {
                    assert_eq!(format!("{actual:?}"), format!("{expected:?}"))
                }
                _ => panic!("materialization readiness changed"),
            }
        }
        // Reuse is boundary-local: a later delta write must be visible.
        let unit = frontier
            .store()
            .scoped_type(
                frontier.owner(),
                InternedType::fresh_canonical(crate::ast::Type::Unit {
                    meta: Meta::new(span),
                }),
                span,
            )
            .unwrap();
        let (store, _, delta, _) = frontier.parts_mut();
        store
            .constrain(delta, open.clone(), unit, span, tcx)
            .unwrap();
        let mut settled = projected(open);
        snapshot_projected_type(
            &mut settled,
            &mut IsolatedSpecializationContextBuilder::new(tcx),
            frontier,
            tcx,
        )
        .unwrap();
        assert!(settled.closed.is_some());
        assert!(
            materialize_projected_type_slot(Some(settled.ty), frontier, tcx)
                .unwrap()
                .resolved
                .is_some()
        );
        GOAL_FREE_PROBES.set(0);
        let absent = materialize_projected_type_slot(None, frontier, tcx).unwrap();
        assert!(absent.resolved.is_none() && absent.occurrence.is_none());
        assert_eq!(GOAL_FREE_PROBES.get(), 0);
    });
}

#[test]
#[ignore = "bounded manual same-binary type-slot materialization time/RSS comparison"]
fn settled_type_slot_materialization_cost() {
    let strategy = std::env::var("KIO_DEBUG_CHECKED_CALL_STRATEGY").unwrap();
    let size = std::env::var("KIO_DEBUG_CHECKED_CALL_SIZE").unwrap();
    let (depth, repetitions) = match size.as_str() {
        "ordinary" => (1, 4_000),
        "wide" => (5, 1_000),
        _ => panic!("ordinary or wide"),
    };
    with_frontier(|frontier, tcx| {
        let span = Span::new(10, 20);
        let mut ty = crate::ast::Type::Unit {
            meta: Meta::new(span),
        };
        for _ in 0..depth {
            ty = crate::ast::Type::Product {
                left: Box::new(ty.clone()),
                right: Box::new(ty),
                meta: Meta::new(span),
            };
        }
        let scoped = frontier
            .store()
            .scoped_type(frontier.owner(), InternedType::fresh_canonical(ty), span)
            .unwrap();
        let materialize = match strategy.as_str() {
            "baseline" => legacy_materialize,
            "candidate" => materialize_projected_type_slot,
            _ => panic!("baseline or candidate"),
        };
        GOAL_FREE_PROBES.set(0);
        let start = std::time::Instant::now();
        for _ in 0..repetitions {
            std::hint::black_box(materialize(Some(scoped.clone()), frontier, tcx).unwrap());
        }
        eprintln!(
            "strategy={strategy} size={size} repetitions={repetitions} elapsed_us={} probes={}",
            start.elapsed().as_micros(),
            GOAL_FREE_PROBES.get()
        );
    });
}

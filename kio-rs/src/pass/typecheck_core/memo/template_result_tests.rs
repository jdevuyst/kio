use super::*;
use crate::ast::{Meta, PathSegment, TypeParam};
use crate::normalization::CheckedTerm;
use crate::span::Span;

fn result_type(width: usize) -> Type<UncheckedPrime> {
    let span = Span::new(19, 47);
    let mut ty = Type::Unit {
        meta: Meta::new(span),
    };
    if width == 0 {
        return ty;
    }
    for index in 0..width {
        ty = Type::Product {
            left: Box::new(Type::synth_path_segments(
                vec![PathSegment::new(format!("T{index}"), span)],
                Vec::new(),
                span,
            )),
            right: Box::new(ty),
            meta: Meta::new(span),
        };
    }
    Type::Forall {
        param: TypeParam {
            presentation: Default::default(),
            name: "T0".to_owned(),
            span,
            kind: None,
        },
        body: Box::new(Type::Function {
            param: Box::new(ty),
            ret: Box::new(Type::Unit {
                meta: Meta::new(span),
            }),
            abi_arity: width,
            caps: (),
            meta: Meta::new(span),
        }),
        meta: Meta::new(span),
    }
}

fn template(ty: Type<UncheckedPrime>) -> UserElaboratorTemplate {
    UserElaboratorTemplate {
        checked: CheckedTerm::template_value(0, ty),
    }
}

fn written_type(name: &str) -> Type<UncheckedPrime> {
    let span = Span::new(4, 5);
    Type::Forall {
        param: TypeParam {
            name: name.into(),
            span,
            kind: None,
            presentation: crate::ast::TypeParamPresentation::written(),
        },
        body: Box::new(Type::synth_path(vec![name.into()], Vec::new(), span)),
        meta: Meta::new(span),
    }
}

#[test]
fn every_template_input_guards_unrenamed_written_binders_without_growing_entries() {
    let variants: [fn(PresentedTypeMemoKey) -> UserElaboratorMemoInputKey; 5] = [
        UserElaboratorMemoInputKey::Type,
        UserElaboratorMemoInputKey::OptionalTypeKnown,
        UserElaboratorMemoInputKey::Term,
        UserElaboratorMemoInputKey::ResidualShape,
        UserElaboratorMemoInputKey::CapturedTerm,
    ];
    for variant in variants {
        for names in [["A", "B", "A"], ["B", "A", "B"]] {
            let memo = MemoCtx::<crate::ast::Lowered>::default();
            for name in names {
                let ty = written_type(name);
                let key = UserElaboratorMemoKey::new(
                    "provider".into(),
                    "entry".into(),
                    vec![variant(PresentedTypeMemoKey::from_type(&ty))],
                );
                let result = memo
                    .user_elaborator_template_or_compute(
                        key,
                        || Ok::<_, ()>(Ok(template(ty))),
                        |_| {},
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    crate::pass::typecheck_core::display_type(result.result_ty()),
                    format!("[{name}] {name}")
                );
            }
            assert_eq!(
                memo.snapshot().user_elaborator,
                CacheSnapshot {
                    misses: 2,
                    hits: 1,
                    first_writes: 1
                }
            );
            assert_eq!(memo.user_elaborator.state.lock().unwrap().values.len(), 1);
        }
    }
}

#[test]
fn cached_template_errors_guard_target_presentation() {
    let memo = MemoCtx::<crate::ast::Lowered>::default();
    for name in ["A", "B", "A", "Authored_n2", "A"] {
        let target = written_type(name);
        let key = UserElaboratorMemoKey::new("provider".into(), "entry".into(), Vec::new())
            .with_result_type(Some(PresentedTypeMemoKey::from_type(&target)));
        let message = format!(
            "target {}",
            crate::pass::typecheck_core::display_type(&target)
        );
        let result = memo
            .user_elaborator_template_or_compute(key, || Ok::<_, ()>(Err(message.clone())), |_| {})
            .unwrap();
        assert_eq!(result.unwrap_err(), message);
    }
    assert_eq!(
        memo.snapshot().user_elaborator,
        CacheSnapshot {
            misses: 3,
            hits: 2,
            first_writes: 1
        }
    );
    assert_eq!(memo.user_elaborator.state.lock().unwrap().values.len(), 1);
}

#[test]
fn retained_template_guard_storage_does_not_grow_with_written_variants() {
    let memo = MemoCtx::<crate::ast::Lowered>::default();
    for number in 0..32 {
        let ty = written_type(&format!("Source{number}"));
        let key = UserElaboratorMemoKey::new(
            "provider".into(),
            "entry".into(),
            vec![
                UserElaboratorMemoInputKey::Type(PresentedTypeMemoKey::from_type(&ty)),
                UserElaboratorMemoInputKey::Term(PresentedTypeMemoKey::from_type(&ty)),
            ],
        )
        .with_result_type(Some(PresentedTypeMemoKey::from_type(&ty)));
        memo.user_elaborator_template_or_compute(key, || Ok::<_, ()>(Ok(template(ty))), |_| {})
            .unwrap()
            .unwrap();
    }
    let state = memo.user_elaborator.state.lock().unwrap();
    assert_eq!(state.values.len(), 1);
    let key = state.values.keys().next().unwrap();
    let mut guards = vec![
        key.result_type
            .as_ref()
            .unwrap()
            .presentation_storage_counts(),
    ];
    for input in &key.inputs {
        let (UserElaboratorMemoInputKey::Type(ty) | UserElaboratorMemoInputKey::Term(ty)) = input
        else {
            unreachable!()
        };
        guards.push(ty.presentation_storage_counts());
    }
    assert!(guards.iter().all(|(entries, _)| *entries == 1));
    assert_eq!(memo.snapshot().user_elaborator.first_writes, 1);
    assert_eq!(memo.snapshot().user_elaborator.misses, 32);
    eprintln!(
        "template key: inline={} semantic_type={} presented_type={} input={} retained_guards={guards:?}",
        std::mem::size_of::<UserElaboratorMemoKey>(),
        std::mem::size_of::<super::super::TypeMemoKey>(),
        std::mem::size_of::<PresentedTypeMemoKey>(),
        std::mem::size_of::<UserElaboratorMemoInputKey>(),
    );
}

#[test]
fn singleflight_waiter_with_different_presentation_returns_its_own_error() {
    let memo = MemoCtx::<crate::ast::Lowered>::default();
    let key = |name| {
        UserElaboratorMemoKey::new("provider".into(), "entry".into(), Vec::new())
            .with_result_type(Some(PresentedTypeMemoKey::from_type(&written_type(name))))
    };
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let winner = scope.spawn(|| {
            memo.user_elaborator_template_or_compute(
                key("A"),
                move || {
                    entered_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Ok::<_, ()>(Err("target [A] A".into()))
                },
                |_| {},
            )
            .unwrap()
        });
        entered_rx.recv().unwrap();
        let waiter = scope.spawn(|| {
            memo.user_elaborator_template_or_compute(
                key("B"),
                || Ok::<_, ()>(Err("target [B] B".into())),
                |_| {},
            )
            .unwrap()
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while memo.user_elaborator.waiting.load(Ordering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "second caller must enter the singleflight wait"
            );
            std::thread::yield_now();
        }
        finish_tx.send(()).unwrap();
        assert_eq!(winner.join().unwrap().unwrap_err(), "target [A] A");
        assert_eq!(waiter.join().unwrap().unwrap_err(), "target [B] B");
    });
    assert_eq!(
        memo.snapshot().user_elaborator,
        CacheSnapshot {
            misses: 2,
            hits: 0,
            first_writes: 1
        }
    );
    assert_eq!(memo.user_elaborator.state.lock().unwrap().values.len(), 1);
}

#[test]
fn template_result_ownership_matches_the_checked_term_on_memo_hits() {
    for width in [0, 1, 64] {
        let original = result_type(width);
        let memo = MemoCtx::<crate::ast::Lowered>::default();
        let key = UserElaboratorMemoKey::new("provider".to_owned(), "entry".to_owned(), Vec::new());
        let first = memo
            .user_elaborator_template_or_compute(
                key.clone(),
                || Ok::<_, ()>(Ok(template(original.clone()))),
                |_| {},
            )
            .unwrap()
            .unwrap();
        let hit = memo
            .user_elaborator_template_or_compute(
                key,
                || -> Result<UserElaboratorTemplateResult, ()> { panic!("memo hit recomputed") },
                |_| {},
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.checked.ty(), &original);
        assert_eq!(hit.checked.ty(), &original);
        assert!(std::ptr::eq(first.result_ty(), first.checked.ty()));
        assert!(std::ptr::eq(hit.result_ty(), hit.checked.ty()));
        assert!(std::ptr::eq(first.checked.ty(), hit.checked.ty()));
        assert_eq!(memo.user_elaborator_stats.snapshot().hits, 1);
    }
    assert_eq!(
        std::mem::size_of::<UserElaboratorTemplate>(),
        std::mem::size_of::<CheckedTerm>(),
        "the immutable checked term owns the template result type"
    );
}

#[test]
fn template_diagnostic_result_keeps_its_existing_memo_path() {
    let memo = MemoCtx::<crate::ast::Lowered>::default();
    let key = UserElaboratorMemoKey::new("provider".to_owned(), "entry".to_owned(), Vec::new());
    let first = memo
        .user_elaborator_template_or_compute(
            key.clone(),
            || Ok::<_, ()>(Err("exact diagnostic".to_owned())),
            |_| {},
        )
        .unwrap();
    let hit = memo
        .user_elaborator_template_or_compute(
            key,
            || -> Result<UserElaboratorTemplateResult, ()> { panic!("memo hit recomputed") },
            |_| {},
        )
        .unwrap();
    assert_eq!(first.unwrap_err(), "exact diagnostic");
    assert_eq!(hit.unwrap_err(), "exact diagnostic");
    assert_eq!(memo.user_elaborator_stats.snapshot().hits, 1);
}

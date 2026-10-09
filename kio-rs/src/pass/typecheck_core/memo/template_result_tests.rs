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

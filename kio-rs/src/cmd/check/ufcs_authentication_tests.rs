use super::*;
use crate::ast::{CallArg, Expr, Item, Lowered, Module, Surface, Type, UfcsFlavor};
use crate::pass::typecheck_core::apply::{
    CallArgRef, call_arg_refs, splice_ufcs_arg_refs_interned,
};
use crate::pass::typecheck_core::{InternedType, ModuleEnv, call_arg_type_slot_candidate};
use crate::pass::typecheck_full::{PositionIndex, ResolvedBinder};
use crate::span::Span;

fn run_body<P: crate::ast::Phase<FnBody = Expr<P>>>(module: &Module<P>) -> &Expr<P> {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::FnDef(definition) if definition.name == "run" => Some(&definition.body),
            _ => None,
        })
        .expect("run function")
}

fn prove_calls(
    surface: &Expr<Surface>,
    lowered: &Expr<Lowered>,
    index: &PositionIndex,
    module: &str,
    env: &ModuleEnv<'_, Lowered>,
    generic: bool,
    local: Option<Span>,
) -> usize {
    let (
        Expr::Call {
            callee: sc,
            args: sa,
            ..
        },
        Expr::Call {
            callee: lc,
            args: la,
            ..
        },
    ) = (surface, lowered)
    else {
        return 0;
    };
    let Expr::Path { segments, .. } = &**lc else {
        panic!("direct callee");
    };
    assert_eq!(surface.span(), lowered.span());
    assert_eq!(sc.span(), lc.span());
    assert_eq!(sa.len(), la.len());
    for (s, l) in sa.iter().zip(la) {
        assert_eq!(
            call_arg_type_slot_candidate(s),
            call_arg_type_slot_candidate(l)
        );
    }
    let (ty, scope, binder) = index
        .authenticated_callee(
            module,
            lc.span(),
            segments.last().unwrap().span,
            lowered.span(),
        )
        .unwrap_or_else(|| {
            panic!(
                "final snapshot must authenticate {module}:{:?}; storage={:?}",
                lc.span(),
                index.refactoring_storage_counts()
            )
        });
    assert_eq!(
        matches!(ty, Type::Forall { .. }),
        generic,
        "retain original public type before explicit instantiation"
    );
    if let Some(declaration) = local {
        assert!(
            matches!(binder, ResolvedBinder::Local { decl_span: Some(span), .. } if *span == declaration)
        );
        assert!(
            !scope.0.is_empty(),
            "generic caller's lexical type binder is retained"
        );
        assert!(scope.0.values().all(Option::is_some));
    } else {
        assert!(
            matches!(binder, ResolvedBinder::Fn { module_path, name } if module_path == module && name == "f")
        );
    }
    let locals = scope.names();
    let mut ctx = env.alias_ctx();
    ctx.binder_locals = Some(&locals);
    let public_type = InternedType::fresh_with_identity(ty.clone(), true);
    let original = call_arg_refs(la);
    let values = la
        .iter()
        .enumerate()
        .filter_map(|(i, arg)| matches!(arg, CallArg::Value(_)).then_some(i))
        .collect::<Vec<_>>();
    for (slot, direction) in [
        (values[0], UfcsFlavor::ReceiverFirst),
        (*values.last().unwrap(), UfcsFlavor::ReceiverLast),
    ] {
        let CallArg::Value(receiver) = &la[slot] else {
            unreachable!()
        };
        let rest = original
            .iter()
            .enumerate()
            .filter_map(|(i, arg)| (i != slot).then_some(*arg))
            .collect::<Vec<_>>();
        let recovered = splice_ufcs_arg_refs_interned(
            &public_type,
            receiver,
            &rest,
            &rest,
            direction,
            lowered.span(),
            &ctx,
        )
        .unwrap();
        assert_eq!(recovered.len(), original.len());
        for (a, b) in recovered.into_iter().zip(&original) {
            assert!(
                match (a, b) {
                    (CallArgRef::Type(a), CallArgRef::Type(b)) => std::ptr::eq(a, *b),
                    (CallArgRef::Value(a), CallArgRef::Value(b)) => std::ptr::eq(a, *b),
                    _ => false,
                },
                "shared splice recovers identical arguments and staging order"
            );
        }
    }
    1 + sa
        .iter()
        .zip(la)
        .map(|(s, l)| match (s, l) {
            (CallArg::Value(s), CallArg::Value(l)) => {
                prove_calls(s, l, index, module, env, generic, local)
            }
            _ => 0,
        })
        .sum::<usize>()
}

fn prove_snapshot(source: &str, path: &Path, analysis: &LspAnalysis, generic: bool, local: bool) {
    let module = &analysis.file_to_module[path];
    let parsed = crate::pass::parser::parse(source).expect("source parse");
    let entry = analysis
        .root_package_lowered
        .module(module)
        .expect("retained source-spelled Lowered module");
    let env = ModuleEnv::build(
        &entry.module,
        None,
        None,
        Some(&analysis.root_package_lowered),
    )
    .unwrap();
    let declaration = local.then(|| {
        let start = source.find("f:").unwrap() as u32;
        Span::new(start, start + 1)
    });
    assert_eq!(
        prove_calls(
            run_body(&parsed),
            run_body(&entry.module),
            &analysis.position_index,
            module,
            &env,
            generic,
            declaration
        ),
        3
    );

    let Expr::Call { callee, .. } = run_body(&entry.module) else {
        panic!("run call");
    };
    let call_span = run_body(&entry.module).span();
    let Expr::Path { segments, .. } = &**callee else {
        panic!("direct path");
    };
    let terminal = segments.last().unwrap().span;
    let accepts = |index: &PositionIndex| {
        index
            .authenticated_callee(module, callee.span(), terminal, call_span)
            .is_some()
    };
    assert!(accepts(&analysis.position_index));
    let mut role = analysis.position_index.clone();
    role.record_value_at_type_slot(module, call_span, 999);
    assert!(!accepts(&role), "whole-call positive role union veto");
    let mut conflict = analysis.position_index.clone();
    let original = conflict.binder_at(module, terminal).unwrap().clone();
    conflict.record_binder(
        module,
        terminal,
        ResolvedBinder::Local {
            name: "f".into(),
            decl_span: None,
        },
    );
    conflict.record_binder(module, terminal, original);
    assert!(
        !accepts(&conflict),
        "source augmentation cannot erase missing-origin observation"
    );
    let mut incomplete = analysis.position_index.clone();
    incomplete.merge(PositionIndex::new());
    assert!(!accepts(&incomplete));
}

#[test]
fn ufcs_authentication_final_full_and_focused_first_last_mixed_chains() {
    for (generic, body) in [
        (false, "f(f(f((), ()), ()), ())"),
        (false, "f((), f((), f((), ())))"),
        (false, "f(f((), f((), ())), ())"),
        (true, "f(., f(., f(., (), ()), ()), ())"),
        (true, "f(., (), f(., (), f(., (), ())))"),
        (true, "f(., f(., (), f(., (), ())), ())"),
    ] {
        let signature = if generic {
            "fn f[A](a: A, b: A) -> A { a }"
        } else {
            "fn f(a: ., b: .) -> . { a }"
        };
        let source = format!("module pkg/main; {signature} pub fn run() -> . {{ {body} }}");
        let (dir, default, path) = super::tests::analyze_lsp_fixture(&source);
        assert!(
            default
                .position_index
                .refactoring_storage_counts()
                .is_none()
        );
        let memos = LspUserElaboratorMemos::for_editor();
        let overlay = package_collection::SourceOverlay::empty();
        let cancel = crate::lsp::cancel::CancellationToken::new();
        let full = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &cancel,
            &memos,
        )
        .unwrap()
        .unwrap();
        prove_snapshot(&source, &path, &full, generic, false);
        let focused = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &path,
            &cancel,
            &memos,
        )
        .unwrap()
        .unwrap();
        prove_snapshot(&source, &path, &focused, generic, false);
        let ordinary = crate::pass::typecheck_full::check_package_preserving_normalization(
            &full.root_package_lowered,
        )
        .unwrap();
        assert!(
            ordinary
                .elaborations()
                .position_index()
                .refactoring_storage_counts()
                .is_none(),
            "ordinary checking remains observation-free in an LSP-enabled binary"
        );
    }
}

#[test]
fn ufcs_authentication_final_local_generic_origins_and_reused_scopes() {
    eprintln!(
        "ufcs scope layout: memo_holder={} package_scope={}",
        std::mem::size_of::<LspUserElaboratorMemos>(),
        std::mem::size_of::<crate::pass::typecheck_core::PackageTypecheckScope<Lowered>>()
    );
    let source = "module pkg/main; pub fn run[A](f: (A & A) -> A, a: A, b: A, c: A, d: A) -> A { f(f(a, f(b, c)), d) }";
    let (dir, default, path) = super::tests::analyze_lsp_fixture(source);
    assert!(
        default
            .position_index
            .refactoring_storage_counts()
            .is_none()
    );
    let memos = LspUserElaboratorMemos::for_editor();
    let overlay = package_collection::SourceOverlay::empty();
    let cancel = crate::lsp::cancel::CancellationToken::new();
    for _ in 0..2 {
        let focused = analyze_module_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &path,
            &cancel,
            &memos,
        )
        .unwrap()
        .unwrap();
        prove_snapshot(source, &path, &focused, false, true);
        let full = analyze_workspace_at_with_overlay_lsp_cancellable_with_memos(
            dir.path(),
            &overlay,
            &cancel,
            &memos,
        )
        .unwrap()
        .unwrap();
        prove_snapshot(source, &path, &full, false, true);
        eprintln!(
            "ufcs final snapshot storage={:?}",
            full.position_index.refactoring_storage_counts()
        );
    }
}

#[cfg(feature = "repl-core")]
#[test]
fn ufcs_authentication_repl_refresh_keeps_observation_disabled() {
    let source = "module pkg/main; fn f(a: ., b: .) -> . { a } pub fn run() -> . { f(f(f((), ()), ()), ()) }";
    let (dir, _, _) = super::tests::analyze_lsp_fixture(source);
    let mut session = crate::repl_core::session::Session::new(dir.path().to_owned());
    session.refresh().unwrap();
    let index = &session.analysis().unwrap().position_index;
    assert!(index.type_count() > 0);
    assert!(index.refactoring_storage_counts().is_none());
}

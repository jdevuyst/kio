use super::*;
use crate::ast::{CallArg, Expr, Item, Lowered, Module, Surface, Type};
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

fn source_head(mut expr: &Expr<Surface>) -> &Expr<Surface> {
    while let Expr::Call { callee, args, .. } = expr {
        assert!(
            !args.is_empty() && args.iter().all(|arg| matches!(arg, CallArg::Type(_))),
            "only explicit type stages preserve this fixture's named callee"
        );
        expr = callee;
    }
    assert!(matches!(expr, Expr::Path { .. }));
    expr
}

fn prove_calls(
    surface: &Expr<Surface>,
    index: &PositionIndex,
    module: &str,
    generic: bool,
    local: Option<Span>,
) -> usize {
    let Expr::Call { callee, args, .. } = surface else {
        return 0;
    };
    assert_eq!(args.len(), 2, "retain the two written value arguments");
    assert!(args.iter().all(|arg| matches!(arg, CallArg::Value(_))));
    let head = source_head(callee);
    let Expr::Path { segments, .. } = head else {
        unreachable!()
    };
    let (ty, scope, binder) = index
        .authenticated_callee(
            module,
            head.span(),
            segments.last().unwrap().span,
            surface.span(),
        )
        .unwrap_or_else(|| {
            panic!(
                "final snapshot must authenticate {module}:{:?}; storage={:?}",
                head.span(),
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
    1 + args
        .iter()
        .map(|arg| match arg {
            CallArg::Value(value) => prove_calls(value, index, module, generic, local),
            CallArg::Type(_) => unreachable!(),
        })
        .sum::<usize>()
}

fn prove_snapshot(source: &str, path: &Path, analysis: &LspAnalysis, generic: bool, local: bool) {
    let module = &analysis.file_to_module[path];
    let parsed = crate::pass::parser::parse(source).expect("source parse");
    let declaration = local.then(|| {
        let start = source.find("f:").unwrap() as u32;
        Span::new(start, start + 1)
    });
    assert_eq!(
        prove_calls(
            run_body(&parsed),
            &analysis.position_index,
            module,
            generic,
            declaration
        ),
        3
    );

    let Expr::Call { callee, .. } = run_body(&parsed) else {
        panic!("run call");
    };
    let call_span = run_body(&parsed).span();
    let callee = source_head(callee);
    let Expr::Path { segments, .. } = callee else {
        panic!("direct path");
    };
    let terminal = segments.last().unwrap().span;
    let accepts = |index: &PositionIndex| {
        index
            .authenticated_callee(module, callee.span(), terminal, call_span)
            .is_some()
    };
    assert!(accepts(&analysis.position_index));
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
        (true, "f[.](f[.](f[.]((), ()), ()), ())"),
        (true, "f[.]((), f[.]((), f[.]((), ())))"),
        (true, "f[.](f[.]((), f[.]((), ())), ())"),
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
        let memos = LspUserMacroMemos::for_editor();
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
        std::mem::size_of::<LspUserMacroMemos>(),
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
    let memos = LspUserMacroMemos::for_editor();
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

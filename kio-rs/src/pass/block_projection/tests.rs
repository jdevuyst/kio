use crate::ast::{Item, Module, Surface};
use crate::pass::full::FullPipeline;
use crate::pass::parser::parse;
use crate::pass::resolve::{LocatedError, Package};
use crate::pipeline::Pipeline;
use std::path::{Path, PathBuf};

const PROVIDER: &str = r#"
module provider;
import __comptime__;
pub type { Box[A]: A { pub mk; pub get; }; }
pub pure fn box_pure[A](value: A) -> Box[A] { Box.mk(value) }
pub pure fn box_bind[A][B](value: Box[A], continuation: A -> Box[B]) -> Box[B] {
  continuation(Box.get(value))
}
pub type Bind[*F] = [A][B] (F[A] & (A -> F[B])) -> F[B];
pub type Sequence[*F][R] = Bind[F] -> F[R];
pub type Scope[R] = (. -> R) -> R;
pub type Triple = . & . & .;
pure fn identity_checked(_ct: __Comptime__, _ty: __Type__, value: __Checked_term__) -> __Checked_term__ { value }
pure fn fixed_checked(_ct: __Comptime__, value: __Checked_term__) -> __Checked_term__ { value }
pure fn pair_checked(ct: __Comptime__, second: __Checked_term__) -> __Checked_term__ { __intrinsic_pair__(ct, __term_unit__(ct), second) }
pure fn append_thunk_checked(ct: __Comptime__, packet: __Checked_term__) -> __Checked_term__ {
  let prefix = __intrinsic_fst__(ct, __term_type__(ct, packet), packet);
  let body = __intrinsic_snd__(ct, __term_type__(ct, packet), packet);
  __intrinsic_pair__(ct, __intrinsic_fst__(ct, __term_type__(ct, prefix), prefix), __intrinsic_pair__(ct, __intrinsic_snd__(ct, __term_type__(ct, prefix), prefix), body))
}
pure fn enter_checked(ct: __Comptime__, fills: __Fill_ctx__, _result: __Type__, body: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {
  (__term_call__(ct, __term_type__(ct, body), body, __term_unit__(ct)), fills)
}
pure fn sequence_checked(ct: __Comptime__, _constructor: __Type__, _result: __Type__, packet: __Checked_term__) -> __Checked_term__ {
  let receiver = __intrinsic_fst__(ct, __term_type__(ct, packet), packet);
  let body = __intrinsic_snd__(ct, __term_type__(ct, packet), packet);
  __term_call__(ct, __term_type__(ct, body), body, receiver)
}
pub macro packet : [A] A -> A { trailing product; impl identity_checked }
pub macro packet_three : Triple -> Triple { trailing product; impl fixed_checked }
pub macro curried : . -> (. & .) -> (. & .) { trailing product; impl fixed_checked }
pub macro curried_three : . -> (. & .) -> Triple { trailing product; impl pair_checked }
pub macro enter : [R] Scope[R] { trailing thunk; impl(fills) enter_checked }
pub macro sequence : [*F][R] (Bind[F] & Sequence[F][R]) -> F[R] { trailing sequence; impl sequence_checked }
"#;

fn modules(consumer: &str) -> Vec<(PathBuf, Module<Surface>)> {
    [("provider", PROVIDER), ("consumer", consumer)]
        .into_iter()
        .map(|(name, source)| {
            (
                PathBuf::from(format!("{name}.kio")),
                parse(source).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
            )
        })
        .collect()
}

fn check(consumer: &str) -> Result<crate::prime::typer::ValidatedPrimePackage, LocatedError> {
    check_modules(modules(consumer))
}

fn check_modules(
    input: Vec<(PathBuf, Module<Surface>)>,
) -> Result<crate::prime::typer::ValidatedPrimePackage, LocatedError> {
    let (lowered, _) = FullPipeline::lower_package(input, None)?;
    let package = Package::build(Path::new(""), lowered, None)?;
    package.resolve_imports()?;
    package.check_in_body_resolution()?;
    FullPipeline::typecheck(&package)
}

#[test]
fn operator_macros_preserve_direct_call_stages_and_generated_node_identity() {
    let provider = format!(
        "{PROVIDER}\n pub op macro # _ {{ impl packet; }} pub op macro $$ ._ {{ impl packet; }}"
    );
    let consumer = "module consumer; import provider(op macro # _, op macro $$ ._, macro packet);
        fn identity[A](value: A) -> A { value }
        fn run() { (# identity)[.](packet(($$ ())())) }
        fn shadow(packet: .) { ($$ packet)() }";
    let mut input = modules(consumer);
    input[0].1 = parse(&provider).unwrap();
    let prime = check_modules(input).expect("operator, ordinary macro and callable-result stages");
    prime_roundtrip(&prime);
}

#[test]
fn block_splices_project_prefix_values_before_every_block_argument() {
    use crate::ast::{CallArg, Expr};
    for (body, expected) in [
        (
            "receiver.>packet (argument) { body }",
            ["receiver", "argument", "body"],
        ),
        (
            "receiver.>>packet (argument) { body }",
            ["argument", "receiver", "body"],
        ),
        (
            "packet (argument) { body }.<<receiver",
            ["receiver", "argument", "body"],
        ),
        (
            "packet (argument) { body }.<receiver",
            ["argument", "receiver", "body"],
        ),
    ] {
        let mut input = modules(&format!(
            "module consumer; import provider(macro packet); fn result() {{ {body} }}"
        ));
        let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&input).unwrap();
        let macro_calls = crate::scope_walk::macro_calls(&input[1].1);
        super::project_module(&mut input[1].1, &scope, &macro_calls).unwrap();
        let Item::FnDef(def) = &input[1].1.items[0] else {
            panic!("function")
        };
        let Expr::UserMacro { stages, form, .. } = &def.body else {
            panic!("projected call")
        };
        assert_eq!(
            *form,
            crate::ast::UserMacroCallForm::TrailingBlocks {
                has_final_prefix: true,
            }
        );
        let [stage] = stages.as_slice() else {
            panic!("{body}: one combined value stage")
        };
        let [CallArg::Value(Expr::Tuple { items, .. })] = stage.args.as_slice() else {
            panic!("{body}: one packet contains the whole prefix and block")
        };
        let [Expr::Tuple { items: prefix, .. }, block] = items.as_slice() else {
            panic!("{body}: receiver and written prefix retain their own product")
        };
        let actual: Vec<_> = prefix
            .iter()
            .chain(std::iter::once(block))
            .map(|value| match value {
                Expr::Path { segments, .. } => segments[0].name.as_str(),
                _ => panic!("{body}: ordinary value argument"),
            })
            .collect();
        assert_eq!(actual, expected, "{body}");
    }
}

#[test]
fn block_splices_use_the_same_public_product_type_and_sequence_projection() {
    let provider = format!(
        "{PROVIDER}\npub type Packet = Box[.] & . & (. -> .);\npub macro packed : ((Box[.] & .) & (. -> .)) -> Packet {{ trailing thunk; impl append_thunk_checked }}"
    );
    for body in [
        "box_pure(()).>packed(()) { () }",
        "().>>packed(box_pure(())) { () }",
        "packed(()) { () }.<<box_pure(())",
        "packed(box_pure(())) { () }.<()",
    ] {
        let mut input = modules(&format!(
            "module consumer; import provider(macro packed, Packet, box_pure); fn result() -> Packet {{ {body} }}"
        ));
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(&check_modules(input).unwrap_or_else(|error| panic!("{body}: {error:?}")));
    }
    for body in [
        "box_bind.>sequence { box_pure(()) }",
        "box_bind.>>sequence { box_pure(()) }",
        "sequence { box_pure(()) }.<box_bind",
        "sequence { box_pure(()) }.<<box_bind",
    ] {
        let source = format!(
            "module consumer; import provider(macro sequence, box_bind, box_pure, Box); fn result() -> Box[.] {{ {body} }}"
        );
        prime_roundtrip(&check(&source).unwrap_or_else(|error| panic!("{body}: {error:?}")));
    }
}

#[test]
fn reverse_block_splice_scaffold_is_inserted_before_the_receiver() {
    let provider = format!(
        "{PROVIDER}\npub macro two : Triple -> Triple {{ trailing product; trailing product next; impl fixed_checked }}"
    );
    let source =
        "module consumer; import provider(macro two); fn result() -> . & . & . { two { () }.<() }";
    let mut input = modules(source);
    input[0].1 = parse(&provider).unwrap();
    let error = check_modules(input).unwrap_err();
    let diagnostic = error.error.diagnostic();
    let fix = diagnostic.fixes().first().expect("missing block scaffold");
    let edit = &fix.edits[0];
    assert_eq!(edit.span.start as usize, source.find(".<").unwrap());
    assert_eq!(edit.span.start, edit.span.end);
    assert_eq!(edit.replacement, " next { }");
    let mut repaired = source.to_owned();
    repaired.insert_str(edit.span.start as usize, &edit.replacement);
    let mut input = modules(&repaired);
    input[0].1 = parse(&provider).unwrap();
    check_modules(input).unwrap();
}

#[test]
fn unresolved_block_heads_keep_the_ordinary_macro_import_hint() {
    for name in ["choose", "relay"] {
        let source = format!("module consumer; fn result() -> . {{ {name} {{ () }} }}");
        let error = check(&source).expect_err("unimported macro");
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::NameRes);
        assert_eq!(error.file_path, Path::new("consumer.kio"));
        let start = u32::try_from(source.find(name).unwrap()).unwrap();
        assert_eq!(error.error.diag().0.start, start);
        assert_eq!(
            error.error.diag().0.end,
            start + u32::try_from(name.len()).unwrap()
        );
        assert_eq!(
            error.error.diagnostic().help(),
            Some(format!("import it with `import <module>(macro {name});`").as_str())
        );
    }
}

#[test]
fn three_kinds_project_through_the_full_pipeline() {
    for source in [
        "module consumer; import provider(macro packet); fn result() -> . & . { packet { (); () } }",
        "module consumer; import provider(macro packet); fn result() -> . { packet {} }",
        "module consumer; import provider(macro enter); fn result() -> . { enter {} }",
        "module consumer; import provider(macro enter); fn result() -> . { enter { let value = (); value } }",
        "module consumer; import provider(macro enter); fn result() -> . { enter { let .(a: ., b: .) = ((), ()); a; b } }",
        "module consumer; import provider(macro sequence, box_bind, box_pure, Box); fn result() -> Box[.] { sequence (box_bind) { let value <- box_pure(()); box_pure(value) } }",
        "module consumer; import provider(macro sequence, box_bind, box_pure, Box); fn result() -> Box[.] { sequence (box_bind) { let .(value: .) <- box_pure(()); box_pure(value) } }",
        "module consumer; import provider(macro sequence, box_bind, box_pure, Box); fn result() -> Box[.] { sequence (box_bind) { box_pure(()); box_pure(()) } }",
    ] {
        check(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    }
}

#[test]
fn projected_nullary_lambda_has_the_ordinary_empty_value_group() {
    use crate::ast::{Expr, SignatureGroupKind};
    let parsed = parse("module main; fn witness() { .() { () } }").unwrap();
    let Item::FnDef(function) = &parsed.items[0] else {
        panic!("function")
    };
    let Expr::FnExpr {
        sig: written,
        body,
        meta,
        ..
    } = &function.body
    else {
        panic!("lambda")
    };
    let Expr::FnExpr { sig: projected, .. } = super::lambda(Vec::new(), *body.clone(), meta.span)
    else {
        panic!("projected lambda")
    };
    assert_eq!(written.groups, vec![SignatureGroupKind::Value { len: 0 }]);
    assert_eq!(
        projected, *written,
        "projection preserves ordinary nullary call-layer identity"
    );
}

#[test]
fn sequence_projection_preserves_written_rhs_and_binding_spans() {
    use crate::ast::{CallArg, Expr, NeutralItem};
    let source = "module consumer; import provider(macro sequence, box_bind, box_pure); fn result() { sequence (box_bind) { let .(first: .) <- box_pure(()); box_pure(()); let .(second: .) <- box_pure(()); box_pure(second) } }";
    let mut input = modules(source);
    let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&input).unwrap();
    let consumer = &mut input[1].1;
    let Item::FnDef(function) = &consumer.items[0] else {
        panic!("function")
    };
    let Expr::BlockCall { blocks, .. } = &function.body else {
        panic!("block")
    };
    let originals = blocks[0]
        .items
        .iter()
        .take(3)
        .map(|item| {
            (
                item.value().span(),
                match item {
                    NeutralItem::Binding { meta, .. } => Some(meta.span),
                    _ => None,
                },
            )
        })
        .collect::<Vec<_>>();
    let macro_calls = crate::scope_walk::macro_calls(consumer);
    super::project_module(consumer, &scope, &macro_calls).unwrap();
    let Item::FnDef(function) = &consumer.items[0] else {
        panic!("function")
    };
    let Expr::UserMacro { stages, .. } = &function.body else {
        panic!("ordinary call")
    };
    let [stage] = stages.as_slice() else {
        panic!("one combined stage")
    };
    let [CallArg::Value(Expr::Tuple { items, .. })] = stage.args.as_slice() else {
        panic!("prefix and sequence packet")
    };
    let [Expr::Path { segments, .. }, Expr::FnExpr { body, .. }] = items.as_slice() else {
        panic!("whole receiver followed by the sequence function")
    };
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].name, "box_bind");
    let mut step = body.as_ref();
    for (rhs_span, binding_span) in originals {
        let Expr::Call {
            callee, args, meta, ..
        } = step
        else {
            panic!("step call")
        };
        if let Some(binding_span) = binding_span {
            assert_eq!(meta.span, binding_span);
        }
        assert_ne!(
            callee.span(),
            meta.span,
            "generated head and call have separate source identities"
        );
        let values = args
            .iter()
            .filter_map(|arg| match arg {
                CallArg::Value(value) => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(values[0].span(), rhs_span);
        let Expr::FnExpr { body, .. } = values[1] else {
            panic!("continuation")
        };
        step = body;
    }
}

#[test]
fn public_aliases_packets_curried_layers_and_function_results() {
    let partial_product = "module consumer; import provider(macro packet_three); fn result() -> (. & .) -> (. & . & .) { packet_three(()) }";
    assert_eq!(
        check(partial_product).unwrap_err().error.exit_code(),
        crate::exit_code::ExitCode::Type
    );
    for source in [
        "module consumer; import provider(macro packet_three); fn result() -> . & . & . { packet_three { (); (); () } }",
        "module consumer; import provider(macro packet_three); fn result() -> . & . & . { packet_three () { (); () } }",
        "module consumer; import provider(macro curried_three); fn result() -> . & . & . { curried_three()(((), ())) }",
        "module consumer; import provider(macro curried); fn result() -> (. & .) -> (. & .) { curried { () } }",
        "module consumer; import provider(macro curried_three); fn result() -> (. & .) -> (. & . & .) { curried_three { () } }",
        "module consumer; import provider(macro curried); fn result() -> . & . { curried()(((), ())) }",
        "module consumer; import provider(macro enter); fn result() -> . -> . { enter { .(value: .) { value } } }",
        "module consumer; import provider(macro packet); fn result() -> . -> . { packet { .(value: .) { value } } }",
        "module consumer; import provider(macro enter); fn result() { enter(.() { () }) }",
        "module consumer; import provider(macro enter); fn result() { (.() { () }).>enter }",
    ] {
        check(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    }
}

#[test]
fn block_content_and_public_completion_fail_locally() {
    for (body, message) in [
        (
            "packet { let value = (); value }",
            "independent expressions",
        ),
        ("enter { let value <- (); value }", "only in a sequence"),
        ("sequence (box_bind) {}", "requires a final expression"),
        (
            "sequence (box_bind) { let value <- box_pure(()); }",
            "requires a final expression",
        ),
        ("enter { () } else { () }", "expected 1 trailing"),
    ] {
        let source = format!(
            "module consumer; import provider(macro packet, macro enter, macro sequence, box_bind, box_pure, macro curried); fn result() {{ {body} }}"
        );
        let error = check(&source).expect_err(body);
        assert_eq!(
            error.error.exit_code(),
            crate::exit_code::ExitCode::Type,
            "{body}: {error:?}"
        );
        assert!(error.error.diag().1.contains(message), "{body}: {error:?}");
        assert_eq!(error.file_path, Path::new("consumer.kio"));
        assert!(
            error
                .error
                .diagnostic()
                .secondary()
                .iter()
                .any(|label| { label.file.as_deref() == Some(Path::new("provider.kio")) }),
            "{body}: {error:?}"
        );
    }
    let error = check("module consumer; import provider(macro curried); fn result() -> . & . { curried() { (); () } }").unwrap_err();
    assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
}

#[test]
fn eager_and_lazy_descriptor_contexts_emit_identical_prime() {
    let source = "module consumer; import provider(macro enter, macro packet); fn result() -> . & . { enter { packet { (); () } } }";
    let input = modules(source);
    let lazy = input
        .iter()
        .map(|(path, module)| {
            let rendered = crate::pretty::pretty_module(module);
            (
                path.clone(),
                crate::pass::parser::parse_lazy(&rendered).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let headers = lazy
        .iter()
        .map(|(path, module)| (path.clone(), module.module().clone()))
        .collect::<Vec<_>>();
    let context = FullPipeline::lowering_context_named(&headers, None, None).unwrap();
    let lowered = lazy
        .iter()
        .map(|(path, module)| {
            FullPipeline::lower_module_with_context(
                &context,
                path.clone(),
                module.force_all().unwrap(),
            )
            .map(|module| (path.clone(), module))
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let package = Package::build(Path::new(""), lowered, None).unwrap();
    let contextual = FullPipeline::typecheck(&package).unwrap();
    let eager = check(source).unwrap();
    for module in ["provider", "consumer"] {
        assert_eq!(
            crate::backends::kio_prime::emit_module(contextual.module(module).unwrap()),
            crate::backends::kio_prime::emit_module(eager.module(module).unwrap())
        );
    }
}

#[test]
fn projection_consumes_neutral_forms_before_desugaring() {
    let source = "module consumer; import provider(macro enter); fn result() { enter { let value = (); value } }";
    let mut input = modules(source);
    let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&input).unwrap();
    let module = &mut input.last_mut().unwrap().1;
    let macro_calls = crate::scope_walk::macro_calls(module);
    super::project_module(module, &scope, &macro_calls).unwrap();
    let Item::FnDef(def) = module.items.last().unwrap() else {
        panic!()
    };
    let crate::ast::Expr::UserMacro { stages, form, .. } = &def.body else {
        panic!()
    };
    assert_eq!(
        *form,
        crate::ast::UserMacroCallForm::TrailingBlocks {
            has_final_prefix: false,
        }
    );
    let [stage] = stages.as_slice() else {
        panic!("one projected thunk stage")
    };
    let [crate::ast::CallArg::Value(crate::ast::Expr::FnExpr { sig, body, .. })] =
        stage.args.as_slice()
    else {
        panic!()
    };
    assert!(sig.params.is_empty());
    assert!(matches!(body.as_ref(), crate::ast::Expr::Let { .. }));
}

#[test]
fn sequence_generated_bindings_remain_lexical_after_prime_roundtrip() {
    let source = "module consumer; import provider(macro sequence, box_bind, box_pure, Box); fn result() -> Box[.] { let step = (); sequence (box_bind) { let value <- box_pure(step); let step = value; sequence (box_bind) { let value <- box_pure(step); box_pure(value) } } }";
    let checked = check(source).unwrap();
    prime_roundtrip(&checked);
}

fn prime_roundtrip(checked: &crate::prime::typer::ValidatedPrimePackage) {
    let reparsed = ["provider", "consumer"]
        .into_iter()
        .map(|name| {
            let emitted = crate::backends::kio_prime::emit_module(checked.module(name).unwrap());
            (
                PathBuf::from(format!("{name}.kio")),
                parse(&emitted).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let modules = reparsed
        .into_iter()
        .map(|(path, module)| {
            let module = crate::prime::lower::lower_module(module)
                .unwrap_or_else(|error| panic!("{path:?}: {error:?}"));
            (path, module)
        })
        .collect();
    let package = Package::build(Path::new(""), modules, None).unwrap();
    crate::prime::typer::check_package(&package).unwrap();
}

#[test]
fn descriptor_labels_are_ordered_and_selected_by_exact_import() {
    let source =
        "module consumer; import provider(macro packet); fn result() -> . { packet { () } }";
    for descriptor in [
        "trailing product first;",
        "trailing product; trailing product;",
        "trailing product; trailing product next; trailing product next;",
    ] {
        let provider = PROVIDER.replace(
            "[A] A -> A { trailing product;",
            &format!("[A] A -> A {{ {descriptor}"),
        );
        let mut input = modules(source);
        input[0].1 = parse(&provider).unwrap();
        let error = FullPipeline::lower_package(input, None).unwrap_err();
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
        assert_eq!(error.file_path, Path::new("provider.kio"));
    }
    let mut input = modules(source);
    let baseline = FullPipeline::lower_package(input.clone(), None).unwrap().0;
    input.push((
        PathBuf::from("unrelated.kio"),
        parse(
            &PROVIDER
                .replace("module provider;", "module unrelated;")
                .replace(
                    "[A] A -> A { trailing product;",
                    "[A] A -> A { trailing thunk;",
                ),
        )
        .unwrap(),
    ));
    let with_unrelated = FullPipeline::lower_package(input, None).unwrap().0;
    let consumer = |modules: &[(PathBuf, Module<crate::ast::Lowered>)]| {
        serde_json::to_value(
            &modules
                .iter()
                .find(|(path, _)| path == Path::new("consumer.kio"))
                .unwrap()
                .1,
        )
        .unwrap()
    };
    assert_eq!(consumer(&baseline), consumer(&with_unrelated));
    let error = FullPipeline::lower_package(
        modules("module consumer; fn result() { packet { () } }"),
        None,
    )
    .unwrap_err();
    assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::NameRes);
}

fn check_edge_separator_variants(head: &str, body: &str, result: &str) {
    let mut failures = Vec::new();
    for (leading, trailing) in [
        ("", ""),
        (";", ";"),
        (";;;", ";;;"),
        (";; // leading\n", "; // trailing\n;;"),
    ] {
        let source = format!(
            "module consumer; import provider(macro packet, macro enter, macro sequence, box_bind, box_pure, Box); fn result() -> {result} {{ {head} {{ {leading}{body}{trailing} }} }}"
        );
        let formatted = crate::pretty::pretty_module(&parse(&source).unwrap());
        for input in [source.as_str(), formatted.as_str()] {
            match check(input) {
                Ok(checked) => prime_roundtrip(&checked),
                Err(error) => failures.push(format!("{input}\n{error:?}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn final_thunk_values_ignore_edge_separators() {
    check_edge_separator_variants("enter", "let value = .(arg: .) { arg }; value", ". -> .");
}

#[test]
fn final_sequence_values_ignore_edge_separators() {
    check_edge_separator_variants(
        "sequence(box_bind)",
        "let value <- box_pure(.(arg: .) { arg }); box_pure(value)",
        "Box[. -> .]",
    );
}

#[test]
fn product_entries_ignore_edge_separators() {
    check_edge_separator_variants("packet", "(); .(value: .) { value }", ". & (. -> .)");
}

#[test]
fn nonfinal_values_and_binding_only_sequence_keep_their_obligations() {
    for (body, result) in [
        ("enter { .(value: .) { value }; () }", "."),
        (
            "sequence (box_bind) { box_pure(.(value: .) { value }); box_pure(()) }",
            "Box[.]",
        ),
        ("sequence (box_bind) { ;;; }", "Box[.]"),
        ("sequence (box_bind) { let value = ();;; }", "Box[.]"),
        (
            "sequence (box_bind) { let value <- box_pure(());;; }",
            "Box[.]",
        ),
    ] {
        let source = format!(
            "module consumer; import provider(macro enter, macro sequence, box_bind, box_pure, Box); fn result() -> {result} {{ {body} }}"
        );
        let formatted = crate::pretty::pretty_module(&parse(&source).unwrap());
        for input in [source.as_str(), formatted.as_str()] {
            assert_eq!(
                check(input)
                    .map(|_| ())
                    .map_err(|error| error.error.exit_code()),
                Err(crate::exit_code::ExitCode::Type),
                "{input}"
            );
        }
    }
}

#[test]
fn nonempty_thunk_requires_a_final_expression() {
    for body in ["enter {}", "enter { ;;; }"] {
        let source =
            format!("module consumer; import provider(macro enter); fn result() {{ {body} }}");
        check(&source).unwrap();
    }
    let mut accepted = Vec::new();
    for body in [
        "enter { let value = (); }",
        "enter { (); let _ = ();;; }",
        "enter { let .(value: .) = (); }",
    ] {
        let source =
            format!("module consumer; import provider(macro enter); fn result() {{ {body} }}");
        let formatted = crate::pretty::pretty_module(&parse(&source).unwrap());
        for input in [source.as_str(), formatted.as_str()] {
            match check(input) {
                Ok(_) => accepted.push(input.to_owned()),
                Err(error) => assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type),
            }
        }
    }
    assert!(accepted.is_empty(), "{}", accepted.join("\n"));
}

#[test]
fn unused_descriptors_do_not_restrict_ordinary_public_calls() {
    for declaration in [
        "pub macro unused : . -> . { trailing thunk; impl unit_checked }",
        "pub macro unused : . -> . { trailing sequence; impl unit_checked }",
        "pub macro unused : (. | .) -> . { trailing thunk; impl fixed_checked }",
        "pub macro unused : (. -> .) -> . { trailing sequence; impl fixed_checked }",
        "pub type Input = . -> .; pub macro unused : Input { trailing thunk; impl unit_checked }",
        "pub macro unused : [R] Scope[R] { trailing thunk; trailing thunk else; impl(fills) enter_checked }",
    ] {
        let provider = format!(
            "{PROVIDER}\npure fn unit_checked(ct: __Comptime__) -> __Checked_term__ {{ __term_unit__(ct) }}\n{declaration}"
        );
        let mut input = modules("module consumer; fn unrelated() { () }");
        input[0].1 = parse(&provider).unwrap();
        check_modules(input).unwrap_or_else(|error| panic!("{declaration}\n{error:?}"));
    }
    let provider = format!(
        "{PROVIDER}\npure fn unit_checked(ct: __Comptime__) -> __Checked_term__ {{ __term_unit__(ct) }}\npub macro direct : . -> . {{ trailing thunk; trailing sequence next; impl unit_checked }}"
    );
    let mut input =
        modules("module consumer; import provider(macro direct); fn result() { direct() }");
    input[0].1 = parse(&provider).unwrap();
    prime_roundtrip(&check_modules(input).unwrap());
}

#[test]
fn final_prefix_and_each_projected_product_stay_whole() {
    for source in [
        "module consumer; import provider(macro packet); fn result() { packet {} }",
        "module consumer; import provider(macro packet); fn result() -> . & . { packet() {} }",
        "module consumer; import provider(macro packet); fn result() -> . & . { packet(()) {} }",
        "module consumer; import provider(macro packet); fn result() -> (. & .) & . { packet((), ()) { () } }",
        "module consumer; import provider(macro packet); fn result() -> (. & .) & . { packet(((), ())) { () } }",
        "module consumer; import provider(macro packet); fn result(pair: . & .) -> (. & .) & . { packet(pair) { () } }",
    ] {
        prime_roundtrip(&check(source).unwrap_or_else(|error| panic!("{source}\n{error:?}")));
    }
    let provider = format!(
        "{PROVIDER}\npub macro two : [A] A -> A {{ trailing product; trailing product next; impl identity_checked }}"
    );
    for (body, result) in [
        ("two {} next {}", ". & ."),
        ("two { (); () } next { (); () }", "(. & .) & (. & .)"),
        (
            "two((), ()) { (); () } next { (); () }",
            "(. & .) & (. & .) & (. & .)",
        ),
    ] {
        let mut input = modules(&format!(
            "module consumer; import provider(macro two); fn result() -> {result} {{ {body} }}"
        ));
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(&check_modules(input).unwrap_or_else(|error| panic!("{body}\n{error:?}")));
    }
}

#[test]
fn projected_lambdas_use_their_product_components_expected_types() {
    let provider = format!(
        "{PROVIDER}\npub type Packet[R] = . & (. -> R) & . & .;\npub macro packed : [R] Packet[R] -> Packet[R] {{ trailing thunk; trailing product after; impl identity_checked }}"
    );
    let mut input = modules(
        "module consumer; import provider(Packet, macro packed); fn result() -> Packet[.] { packed() { () } after { (); () } }",
    );
    input[0].1 = parse(&provider).unwrap();
    prime_roundtrip(&check_modules(input).unwrap());
}

#[test]
fn open_projected_types_keep_ordinary_call_constraints() {
    let provider = format!(
        "{PROVIDER}\npub macro open_thunk : [A] A -> A {{ trailing thunk; impl identity_checked }}\npub macro open_sequence : [A] A -> A {{ trailing sequence; impl identity_checked }}"
    );
    for consumer in [
        "module consumer; import provider(macro open_thunk); fn result() -> . -> . { open_thunk { () } }",
        "module consumer; import provider(macro open_sequence, Sequence, Box, box_pure); fn result() -> Sequence[Box][.] { open_sequence { box_pure(()) } }",
        "module consumer; import provider(macro open_thunk); fn result() { open_thunk(()) }",
        "module consumer; import provider(macro open_sequence); fn result() { open_sequence(()) }",
    ] {
        let mut input = modules(consumer);
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(
            &check_modules(input).unwrap_or_else(|error| panic!("{consumer}\n{error:?}")),
        );
    }
}

#[test]
fn zero_step_sequence_checks_its_projected_type_without_restricting_direct_calls() {
    let provider = format!(
        "{PROVIDER}\npub macro open_sequence : [A] A -> A {{ trailing sequence; impl identity_checked }}"
    );
    for (bind, result, body) in [
        (".", ".", "()"),
        ("[A] A -> A", ".", "()"),
        (
            "[A][B] (Box[A] & (A -> Box[B])) -> .",
            "Box[.]",
            "box_pure(())",
        ),
        (
            "[A][B] (Box[A] & (B -> Box[B])) -> Box[B]",
            "Box[.]",
            "box_pure(())",
        ),
    ] {
        let braced = format!(
            "module consumer; import provider(macro open_sequence, Box, box_pure); fn result() -> ({bind}) -> {result} {{ open_sequence {{ {body} }} }}"
        );
        let mut input = modules(&braced);
        input[0].1 = parse(&provider).unwrap();
        let error = check_modules(input).expect_err(&braced);
        assert_eq!(error.file_path, Path::new("consumer.kio"));
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
        assert!(
            error.error.diag().1.contains("sequence block"),
            "{braced}\n{error:?}"
        );

        let direct = format!(
            "module consumer; import provider(macro open_sequence, Box, box_pure); fn result() -> ({bind}) -> {result} {{ open_sequence(.(step: {bind}) {{ {body} }}) }}"
        );
        let mut input = modules(&direct);
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(
            &check_modules(input).unwrap_or_else(|error| panic!("{direct}\n{error:?}")),
        );
    }
}

#[test]
fn mixed_descriptors_share_ordinary_goals_but_keep_sequence_shape() {
    let provider = format!(
        "{PROVIDER}\npub type Twice[X] = X & X;\npub macro twice : [X] Twice[X] -> Twice[X] {{ trailing thunk; trailing thunk else; impl identity_checked }}\npub macro mixed : [X] Twice[X] -> Twice[X] {{ trailing thunk; trailing sequence next; impl identity_checked }}"
    );
    let mut input = modules(
        "module consumer; import provider(macro twice); fn result() -> (. -> .) & (. -> .) { twice { () } else { () } }",
    );
    input[0].1 = parse(&provider).unwrap();
    prime_roundtrip(&check_modules(input).unwrap());

    let mut input = modules(
        "module consumer; import provider(macro mixed); fn result() -> (. -> .) & (. -> .) { mixed { () } next { () } }",
    );
    input[0].1 = parse(&provider).unwrap();
    let error = check_modules(input).unwrap_err();
    assert_eq!(error.file_path, Path::new("consumer.kio"));
    assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
    assert!(error.error.diag().1.contains("sequence block"), "{error:?}");
}

#[test]
fn earlier_value_and_forall_stages_remain_distinct() {
    let provider = format!(
        "{PROVIDER}\npure fn first_checked(ct: __Comptime__, _a: __Type__, _source: __Checked_term__, _b: __Type__, packet: __Checked_term__) -> __Checked_term__ {{ __intrinsic_fst__(ct, __term_type__(ct, packet), packet) }}\npub macro staged : [A] A -> [B] (B & (. -> B)) -> B {{ trailing thunk; impl first_checked }}"
    );
    for body in [
        "staged()(box_pure(())) { box_pure(()) }",
        "staged[.](())[Box[.]](box_pure(()), .() { box_pure(()) })",
    ] {
        let mut input = modules(&format!(
            "module consumer; import provider(macro staged, Box, box_pure); fn result() -> Box[.] {{ {body} }}"
        ));
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(&check_modules(input).unwrap_or_else(|error| panic!("{body}\n{error:?}")));
    }
}

#[test]
fn thunk_and_sequence_projections_validate_before_ordinary_residualization() {
    let provider = format!(
        r#"{PROVIDER}
pure fn delayed_thunk_checked(ct: __Comptime__, _result: __Type__, packet: __Checked_term__) -> __Checked_term__ {{
  let body = __intrinsic_snd__(ct, __term_type__(ct, packet), packet);
  __term_call__(ct, __term_type__(ct, body), body, __term_unit__(ct))
}}
pure fn delayed_thunk_fills(ct: __Comptime__, fills: __Fill_ctx__, result: __Type__, packet: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {{
  (delayed_thunk_checked(ct, result, packet), fills)
}}
pure fn delayed_sequence_fills(ct: __Comptime__, fills: __Fill_ctx__, constructor: __Type__, result: __Type__, packet: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {{
  (sequence_checked(ct, constructor, result, packet), fills)
}}
pub macro thunk_late : [R] (. & (. -> R)) -> . -> R {{ trailing thunk; impl delayed_thunk_checked }}
pub macro thunk_fills : [R] (. & (. -> R)) -> . -> R {{ trailing thunk; impl(fills) delayed_thunk_fills }}
pub macro sequence_late : [*F][R] (Bind[F] & Sequence[F][R]) -> . -> F[R] {{ trailing sequence; impl sequence_checked }}
pub macro sequence_fills : [*F][R] (Bind[F] & Sequence[F][R]) -> . -> F[R] {{ trailing sequence; impl(fills) delayed_sequence_fills }}
"#
    );
    for (name, prefix) in [
        ("thunk_late", "()"),
        ("thunk_fills", "()"),
        ("sequence_late", "(box_bind)"),
        ("sequence_fills", "(box_bind)"),
    ] {
        let source = format!(
            "module consumer; import provider(macro {name}, Box, box_bind, box_pure); fn result() -> . -> Box[.] {{ {name}{prefix} {{ box_pure(()) }} }}"
        );
        let mut input = modules(&source);
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(
            &check_modules(input).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
        );
    }
}

#[test]
fn sequence_projection_accepts_identity_and_constant_carriers() {
    let provider = format!(
        "{PROVIDER}\npub type Identity_sequence[R] = ([A][B] (A & (A -> B)) -> B) -> R;\npub type Constant_sequence = ([A][B] (. & (A -> .)) -> .) -> .;\npub macro open_sequence : [A] A -> A {{ trailing sequence; impl identity_checked }}"
    );
    for (result, body) in [
        ("Identity_sequence[.]", "()"),
        ("Identity_sequence[.]", "let value <- (); value"),
        ("Constant_sequence", "()"),
    ] {
        let source = format!(
            "module consumer; import provider(macro open_sequence, Identity_sequence, Constant_sequence); fn result() -> {result} {{ open_sequence {{ {body} }} }}"
        );
        let mut input = modules(&source);
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(
            &check_modules(input).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
        );
    }
}

#[test]
fn constant_sequence_step_keeps_ordinary_unresolved_type_error() {
    let provider = format!(
        "{PROVIDER}\npub type Identity_sequence[R] = ([A][B] (A & (A -> B)) -> B) -> R;\npub type Constant_sequence = ([A][B] (. & (A -> .)) -> .) -> .;\npub macro open_sequence : [A] A -> A {{ trailing sequence; impl identity_checked }}"
    );
    // The constant carrier erases B from every argument and result equation.
    for source in [
        "module consumer; import provider(macro open_sequence, Identity_sequence, Constant_sequence); fn result() -> Constant_sequence { open_sequence { let .(value: .) <- (); () } }",
        "module consumer; import provider(macro open_sequence, Identity_sequence, Constant_sequence); fn result() -> Constant_sequence { open_sequence(.(step) { step[.]((), .(value: .) { () }) }) }",
    ] {
        let mut input = modules(source);
        input[0].1 = parse(&provider).unwrap();
        let error = check_modules(input).expect_err(source);
        assert_eq!(error.file_path, Path::new("consumer.kio"));
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
        assert!(
            error
                .error
                .diag()
                .1
                .contains("cannot infer type argument `B`"),
            "{source}\n{error:?}"
        );
    }
}

#[test]
fn projected_blocks_validate_on_ordinary_macro_output_applications() {
    let provider = format!(
        r#"{PROVIDER}
pure fn identity_fills(_ct: __Comptime__, fills: __Fill_ctx__, _ty: __Type__, value: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {{ (value, fills) }}
pub macro thunk_late : [A] A -> A {{ trailing thunk; impl identity_checked }}
pub macro thunk_fills : [A] A -> A {{ trailing thunk; impl(fills) identity_fills }}
pub macro sequence_late : [A] A -> A {{ trailing sequence; impl identity_checked }}
pub macro sequence_fills : [A] A -> A {{ trailing sequence; impl(fills) identity_fills }}
"#
    );
    for (name, block_type, body) in [
        ("thunk_late", ". -> .", "()"),
        ("thunk_fills", ". -> .", "()"),
        ("sequence_late", "Sequence[Box][.]", "box_pure(())"),
        ("sequence_fills", "Sequence[Box][.]", "box_pure(())"),
    ] {
        for (result_type, result_body) in [(".", "()"), (". -> .", ".() { () }")] {
            let source = format!(
                "module consumer; import provider(macro {name}, Box, Sequence, box_pure); fn accept(inputs: . & ({block_type})) -> {result_type} {{ {result_body} }} fn result() -> {result_type} {{ {name}(accept)() {{ {body} }} }}"
            );
            let mut input = modules(&source);
            input[0].1 = parse(&provider).unwrap();
            prime_roundtrip(
                &check_modules(input).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
            );
        }
    }
    for name in ["sequence_late", "sequence_fills"] {
        let source = format!(
            "module consumer; import provider(macro {name}); fn accept(inputs: . & (. -> .)) {{ () }} fn result() {{ {name}(accept)() {{ () }} }}"
        );
        let mut input = modules(&source);
        input[0].1 = parse(&provider).unwrap();
        let error = check_modules(input).expect_err(&source);
        assert_eq!(error.file_path, Path::new("consumer.kio"));
        assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
        assert!(
            error.error.diag().1.contains("sequence block"),
            "{source}\n{error:?}"
        );

        let direct = format!(
            "module consumer; import provider(macro {name}); fn accept(inputs: . & (. -> .)) {{ () }} fn result() {{ {name}(accept)((), .(step: .) {{ () }}) }}"
        );
        let mut input = modules(&direct);
        input[0].1 = parse(&provider).unwrap();
        prime_roundtrip(
            &check_modules(input).unwrap_or_else(|error| panic!("{direct}\n{error:?}")),
        );
    }
    for name in ["thunk_late", "thunk_fills"] {
        for body in ["()", "1[Number]"] {
            let source = format!(
                "module consumer; import provider(macro {name}); host type Number role(i32); fn accept[R](inputs: . & (. -> R)) {{ () }} fn result() {{ {name}(accept)() {{ {body} }} }}"
            );
            let mut input = modules(&source);
            input[0].1 = parse(&provider).unwrap();
            prime_roundtrip(
                &check_modules(input).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
            );
        }
        for body in ["1", "produce()"] {
            for call in [
                format!("{name}(accept)() {{ {body} }}"),
                format!("{name}(accept)((), .() {{ {body} }})"),
            ] {
                let source = format!(
                    "module consumer; import provider(macro {name}); host fn produce[A]() -> A; fn accept[R](inputs: . & (. -> R)) {{ () }} fn result() {{ {call} }}"
                );
                let mut input = modules(&source);
                input[0].1 = parse(&provider).unwrap();
                let error = check_modules(input).expect_err(&source);
                assert_eq!(error.file_path, Path::new("consumer.kio"));
                assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
            }
        }
    }
}

#[test]
fn ignored_thunk_results_keep_ordinary_source_admission() {
    let provider = format!(
        r#"{PROVIDER}
pure fn ignore_checked(ct: __Comptime__, _result: __Type__, _body: __Checked_term__) -> __Checked_term__ {{ __term_unit__(ct) }}
pure fn ignore_fills(ct: __Comptime__, fills: __Fill_ctx__, _result: __Type__, _body: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {{ (__term_unit__(ct), fills) }}
pub macro ignored_late : [R] (. -> R) -> . {{ trailing thunk; impl ignore_checked }}
pub macro ignored_fills : [R] (. -> R) -> . {{ trailing thunk; impl(fills) ignore_fills }}
"#
    );
    for name in ["ignored_late", "ignored_fills"] {
        for body in ["()", "identity(())", "1[Number]", "identity(1[Number])"] {
            let source = format!(
                "module consumer; import provider(macro {name}); host type Number role(i32); fn identity[A](value: A) -> A {{ value }} fn result() {{ {name} {{ {body} }} }}"
            );
            let mut input = modules(&source);
            input[0].1 = parse(&provider).unwrap();
            prime_roundtrip(
                &check_modules(input).unwrap_or_else(|error| panic!("{source}\n{error:?}")),
            );
        }
        for body in ["1", "identity(1)"] {
            let source = format!(
                "module consumer; import provider(macro {name}); fn identity[A](value: A) -> A {{ value }} fn result() {{ {name} {{ {body} }} }}"
            );
            let mut input = modules(&source);
            input[0].1 = parse(&provider).unwrap();
            let error = check_modules(input).expect_err(&source);
            assert_eq!(error.file_path, Path::new("consumer.kio"));
            assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
            assert!(
                error
                    .error
                    .diag()
                    .1
                    .contains("cannot determine the type of this integer literal"),
                "{source}\n{error:?}"
            );
        }
        for call in [
            format!("{name} {{ produce() }}"),
            format!("{name}(.() {{ produce() }})"),
        ] {
            let source = format!(
                "module consumer; import provider(macro {name}); host fn produce[A]() -> A; fn result() {{ {call} }}"
            );
            let mut input = modules(&source);
            input[0].1 = parse(&provider).unwrap();
            let error = check_modules(input).expect_err(&source);
            assert_eq!(error.file_path, Path::new("consumer.kio"));
            assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
        }
    }
}

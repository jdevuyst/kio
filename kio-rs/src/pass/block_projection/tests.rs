use crate::ast::{Item, Module, Prime, Surface};
use crate::pass::full::FullPipeline;
use crate::pass::parser::parse;
use crate::pass::resolve::{LocatedError, Package};
use crate::pipeline::Pipeline;
use std::path::{Path, PathBuf};

const PROVIDER: &str = r#"
module provider;
import __comptime__;
pub newtype Box[A] : A { pub constructor box; pub projector un_box }
pub pure fn box_pure[A](value: A) -> Box(A) { Box.box(value) }
pub pure fn box_bind[A][B](value: Box(A), continuation: A -> Box(B)) -> Box(B) {
  continuation(Box.un_box(value))
}
pub type Bind[*F] = [A][B] (F(A) & (A -> F(B))) -> F(B);
pub type Sequence[*F][R] = Bind(F) -> F(R);
pub type Scope[R] = (. -> R) -> R;
pub type Triple = . & . & .;
pure fn identity_checked(_ct: __Comptime__, _ty: __Type__, value: __Checked_term__) -> __Checked_term__ { value }
pure fn fixed_checked(_ct: __Comptime__, value: __Checked_term__) -> __Checked_term__ { value }
pure fn enter_checked(ct: __Comptime__, fills: __Fill_ctx__, _result: __Type__, body: __Checked_term__) -> __Checked_term__ & __Fill_ctx__ {
  (__term_call__(ct, __term_type__(ct, body), body, __term_unit__(ct)), fills)
}
pure fn sequence_checked(ct: __Comptime__, _constructor: __Type__, _result: __Type__, receiver: __Checked_term__, body: __Checked_term__) -> __Checked_term__ {
  __term_call__(ct, __term_type__(ct, body), body, receiver)
}
pub elab packet : [A] A -> A { trailing product; impl identity_checked }
pub elab packet_three : Triple -> Triple { trailing product; impl fixed_checked }
pub elab curried : . -> (. & .) -> (. & .) { trailing product; impl fixed_checked }
pub elab enter : [R] Scope(R) { trailing thunk; impl(fills) enter_checked }
pub elab sequence : [*F][R] Bind(F) -> Sequence(F, R) -> F(R) { trailing sequence; impl sequence_checked }
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

fn check(consumer: &str) -> Result<Package<Prime>, LocatedError> {
    check_modules(modules(consumer))
}

fn check_modules(input: Vec<(PathBuf, Module<Surface>)>) -> Result<Package<Prime>, LocatedError> {
    let (lowered, _) = FullPipeline::lower_package(input, None)?;
    let package = Package::build(Path::new(""), lowered, None)?;
    package.resolve_imports()?;
    package.check_in_body_resolution()?;
    FullPipeline::typecheck(&package)
}

#[test]
fn unresolved_block_heads_keep_the_ordinary_elaborator_import_hint() {
    for name in ["choose", "relay"] {
        for arguments in [" { () }", "(())"] {
            let source = format!("module consumer; fn result() -> . {{ {name}!{arguments} }}");
            let error = check(&source).expect_err("unimported elaborator");
            assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::NameRes);
            assert_eq!(error.file_path, Path::new("consumer.kio"));
            let start = u32::try_from(source.find(name).unwrap()).unwrap();
            assert_eq!(error.error.diag().0.start, start);
            if arguments.starts_with(' ') {
                assert_eq!(
                    error.error.diag().0.end,
                    start + u32::try_from(name.len()).unwrap()
                );
            }
            assert_eq!(
                error.error.diagnostic().help(),
                Some(format!("import it with `import <module>({name});`").as_str())
            );
        }
    }
}

#[test]
fn three_kinds_project_through_the_full_pipeline() {
    for source in [
        "module consumer; import provider(packet); fn result() -> . & . { packet! { (); () } }",
        "module consumer; import provider(packet); fn result() -> . { packet! {} }",
        "module consumer; import provider(enter); fn result() -> . { enter! {} }",
        "module consumer; import provider(enter); fn result() -> . { enter! { let value = (); value } }",
        "module consumer; import provider(enter); fn result() -> . { enter! { let .(a: ., b: .) = ((), ()); a; b } }",
        "module consumer; import provider(sequence, box_bind, box_pure, Box); fn result() -> Box(.) { sequence! box_bind { let value <- box_pure(()); box_pure(value) } }",
        "module consumer; import provider(sequence, box_bind, box_pure, Box); fn result() -> Box(.) { sequence! box_bind { let .(value: .) <- box_pure(()); box_pure(value) } }",
        "module consumer; import provider(sequence, box_bind, box_pure, Box); fn result() -> Box(.) { sequence! box_bind { box_pure(()); box_pure(()) } }",
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
    let source = "module consumer; import provider(sequence, box_bind, box_pure); fn result() { sequence! box_bind { let .(first: .) <- box_pure(()); box_pure(()); let .(second: .) <- box_pure(()); box_pure(second) } }";
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
    super::project_module(consumer, &scope).unwrap();
    let Item::FnDef(function) = &consumer.items[0] else {
        panic!("function")
    };
    let Expr::UserElaborator { args, .. } = &function.body else {
        panic!("ordinary call")
    };
    let CallArg::Value(Expr::FnExpr { body, .. }) = &args[1] else {
        panic!("sequence function")
    };
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
    for source in [
        "module consumer; import provider(packet_three); fn result() -> . & . & . { packet_three! { (); (); () } }",
        "module consumer; import provider(packet_three); fn result() -> . & . & . { packet_three! () { (); () } }",
        "module consumer; import provider(curried); fn result() -> . & . { curried! () { (); () } }",
        "module consumer; import provider(enter); fn result() -> . -> . { enter! { .(value: .) { value } } }",
        "module consumer; import provider(packet); fn result() -> . -> . { packet! { .(value: .) { value } } }",
    ] {
        check(source).unwrap_or_else(|error| panic!("{source}\n{error:?}"));
    }
}

#[test]
fn block_content_and_public_completion_fail_locally() {
    for (body, message) in [
        (
            "packet! { let value = (); value }",
            "independent expressions",
        ),
        ("enter! { let value <- (); value }", "only in a sequence"),
        ("sequence! box_bind {}", "requires a final expression"),
        (
            "sequence! box_bind { let value <- box_pure(()); }",
            "requires a final expression",
        ),
        (
            "enter!(.() { () })",
            "requires its declared trailing blocks",
        ),
        (
            "(.() { () }).>enter!",
            "requires its declared trailing blocks",
        ),
        ("enter! { () } else { () }", "expected 1 trailing"),
    ] {
        let source = format!(
            "module consumer; import provider(packet, enter, sequence, box_bind, box_pure, curried); fn result() {{ {body} }}"
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
    let error = check("module consumer; import provider(curried); fn result() -> (. & .) -> (. & .) { curried! { () } }").unwrap_err();
    assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
    assert!(error.error.diag().1.contains("public"), "{error:?}");
}

#[test]
fn eager_and_lazy_descriptor_contexts_emit_identical_prime() {
    let source = "module consumer; import provider(enter, packet); fn result() -> . & . { enter! { packet! { (); () } } }";
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
            crate::backends::kio_prime::emit_module(&contextual.module(module).unwrap().module),
            crate::backends::kio_prime::emit_module(&eager.module(module).unwrap().module)
        );
    }
}

#[test]
fn projection_consumes_neutral_forms_before_desugaring() {
    let source =
        "module consumer; import provider(enter); fn result() { enter! { let value = (); value } }";
    let mut input = modules(source);
    let scope = crate::pass::surface_registry::PackageBlockScope::from_modules(&input).unwrap();
    let module = &mut input.last_mut().unwrap().1;
    super::project_module(module, &scope).unwrap();
    let Item::FnDef(def) = module.items.last().unwrap() else {
        panic!()
    };
    let crate::ast::Expr::UserElaborator { args, form, .. } = &def.body else {
        panic!()
    };
    assert_eq!(*form, crate::ast::UserElaboratorCallForm::TrailingBlocks);
    let [crate::ast::CallArg::Value(crate::ast::Expr::FnExpr { sig, body, .. })] = args.as_slice()
    else {
        panic!()
    };
    assert!(sig.params.is_empty());
    assert!(matches!(body.as_ref(), crate::ast::Expr::Let { .. }));
}

#[test]
fn sequence_generated_bindings_remain_lexical_after_prime_roundtrip() {
    let source = "module consumer; import provider(sequence, box_bind, box_pure, Box); fn result() -> Box(.) { let step = (); sequence! box_bind { let value <- box_pure(step); let step = value; sequence! box_bind { let value <- box_pure(step); box_pure(value) } } }";
    let checked = check(source).unwrap();
    prime_roundtrip(&checked);
}

fn prime_roundtrip(checked: &Package<Prime>) {
    let reparsed = ["provider", "consumer"]
        .into_iter()
        .map(|name| {
            let emitted =
                crate::backends::kio_prime::emit_module(&checked.module(name).unwrap().module);
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
    let source = "module consumer; import provider(packet); fn result() -> . { packet! { () } }";
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
        modules("module consumer; fn result() { packet! { () } }"),
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
            "module consumer; import provider(packet, enter, sequence, box_bind, box_pure, Box); fn result() -> {result} {{ {head} {{ {leading}{body}{trailing} }} }}"
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
    check_edge_separator_variants("enter!", "let value = .(arg: .) { arg }; value", ". -> .");
}

#[test]
fn final_sequence_values_ignore_edge_separators() {
    check_edge_separator_variants(
        "sequence! box_bind",
        "let value <- box_pure(.(arg: .) { arg }); box_pure(value)",
        "Box(. -> .)",
    );
}

#[test]
fn product_entries_ignore_edge_separators() {
    check_edge_separator_variants("packet!", "(); .(value: .) { value }", ". & (. -> .)");
}

#[test]
fn nonfinal_values_and_binding_only_sequence_keep_their_obligations() {
    for (body, result) in [
        ("enter! { .(value: .) { value }; () }", "."),
        (
            "sequence! box_bind { box_pure(.(value: .) { value }); box_pure(()) }",
            "Box(.)",
        ),
        ("sequence! box_bind { ;;; }", "Box(.)"),
        ("sequence! box_bind { let value = ();;; }", "Box(.)"),
        (
            "sequence! box_bind { let value <- box_pure(());;; }",
            "Box(.)",
        ),
    ] {
        let source = format!(
            "module consumer; import provider(enter, sequence, box_bind, box_pure, Box); fn result() -> {result} {{ {body} }}"
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
    for body in ["enter! {}", "enter! { ;;; }"] {
        let source = format!("module consumer; import provider(enter); fn result() {{ {body} }}");
        check(&source).unwrap();
    }
    let mut accepted = Vec::new();
    for body in [
        "enter! { let value = (); }",
        "enter! { (); let _ = ();;; }",
        "enter! { let .(value: .) = (); }",
    ] {
        let source = format!("module consumer; import provider(enter); fn result() {{ {body} }}");
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
fn unused_block_declarations_reject_incompatible_public_slots() {
    let mut accepted = Vec::new();
    for declaration in [
        "pub elab bad : . -> . { trailing thunk; impl unit_checked }",
        "pub elab bad : . -> . { trailing sequence; impl unit_checked }",
        "pub elab bad : (. | .) -> . { trailing thunk; impl fixed_checked }",
        "pub elab bad : (. -> .) -> . { trailing sequence; impl fixed_checked }",
        "pub type Bad = . -> .; pub elab bad : Bad { trailing thunk; impl unit_checked }",
        "pub elab bad : [R] Scope(R) { trailing thunk; trailing thunk else; impl(fills) enter_checked }",
    ] {
        let provider = format!(
            "{PROVIDER}\npure fn unit_checked(ct: __Comptime__) -> __Checked_term__ {{ __term_unit__(ct) }}\n{declaration}"
        );
        let mut input = modules("module consumer; fn unrelated() { () }");
        input[0].1 = parse(&provider).unwrap();
        match check_modules(input) {
            Ok(_) => accepted.push(declaration),
            Err(error) => {
                assert_eq!(error.file_path, Path::new("provider.kio"));
                assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
                assert!(
                    error.error.diag().1.contains("trailing"),
                    "{declaration}\n{error:?}"
                );
            }
        }
    }
    assert!(accepted.is_empty(), "{}", accepted.join("\n"));
}

#[test]
fn declaration_slots_preserve_final_product_packet_packing() {
    let provider = format!(
        "{PROVIDER}\npub type Packet[R] = . & (. -> R) & . & .;\npub elab packed : [R] Packet(R) -> Packet(R) {{ trailing thunk; trailing product after; impl identity_checked }}"
    );
    let mut input = modules(
        "module consumer; import provider(Packet, packed); fn result() -> Packet(.) { packed! () { () } after { (); () } }",
    );
    input[0].1 = parse(&provider).unwrap();
    check_modules(input).unwrap();
}

#[test]
fn declaration_open_slots_keep_ordinary_call_constraints() {
    let provider = format!(
        "{PROVIDER}\npub elab open_thunk : [A] A -> A {{ trailing thunk; impl identity_checked }}\npub elab open_sequence : [A] A -> A {{ trailing sequence; impl identity_checked }}"
    );
    for consumer in [
        "module consumer; import provider(open_thunk); fn result() -> . -> . { open_thunk! { () } }",
        "module consumer; import provider(open_sequence, Sequence, Box, box_pure); fn result() -> Sequence(Box, .) { open_sequence! { box_pure(()) } }",
    ] {
        let mut input = modules(consumer);
        input[0].1 = parse(&provider).unwrap();
        check_modules(input).unwrap_or_else(|error| panic!("{consumer}\n{error:?}"));
    }
}

#[test]
fn declaration_sequence_relations_are_consistent_and_scoped() {
    let mut accepted = Vec::new();
    for call_ty in [
        "[X] (([A][B] (X & (A -> .)) -> !) -> .) -> .",
        "[X] (([A][B] (. & (X -> .)) -> .) -> .) -> .",
        "[X] (([A] X) -> .) -> .",
    ] {
        let provider = format!(
            "{PROVIDER}\npub elab bad : {call_ty} {{ trailing sequence; impl identity_checked }}"
        );
        let mut input = modules("module consumer; fn unrelated() { () }");
        input[0].1 = parse(&provider).unwrap();
        match check_modules(input) {
            Ok(_) => accepted.push(call_ty),
            Err(error) => {
                assert_eq!(error.file_path, Path::new("provider.kio"));
                assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
                assert!(error.error.diag().1.contains("trailing"), "{error:?}");
            }
        }
    }
    assert!(accepted.is_empty(), "{}", accepted.join("\n"));
}

#[test]
fn declaration_compatibility_shares_public_goals_without_publishing_them() {
    let provider = format!(
        "{PROVIDER}\npub type Twice[X] = X & X;\npub elab twice : [X] Twice(X) -> Twice(X) {{ trailing thunk; trailing thunk else; impl identity_checked }}\npub elab constant : [X] (([A][B] (X & (A -> .)) -> .) -> .) -> . {{ trailing sequence; impl identity_checked }}"
    );
    let mut input = modules(
        "module consumer; import provider(twice); fn result() -> (. -> .) & (. -> .) { twice! { () } else { () } }",
    );
    input[0].1 = parse(&provider).unwrap();
    check_modules(input).unwrap();

    let provider = format!(
        "{PROVIDER}\npub type Twice[X] = X & X;\npub elab bad : [X] Twice(X) -> Twice(X) {{ trailing thunk; trailing sequence next; impl identity_checked }}"
    );
    let mut input = modules("module consumer; fn unrelated() { () }");
    input[0].1 = parse(&provider).unwrap();
    let error = check_modules(input).unwrap_err();
    assert_eq!(error.file_path, Path::new("provider.kio"));
    assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
    assert!(error.error.diag().1.contains("trailing"), "{error:?}");
}

#[test]
fn declaration_source_capacity_follows_ordinary_prefix_instantiation() {
    let provider = format!(
        "{PROVIDER}\npure fn unit_two_checked(ct: __Comptime__, _ty: __Type__, _first: __Checked_term__, _second: __Checked_term__) -> __Checked_term__ {{ __term_unit__(ct) }}\npub elab expanded : [A] (A -> .) -> A -> . {{ trailing product; trailing product next; trailing product last; impl unit_two_checked }}"
    );
    let mut input = modules(
        "module consumer; import provider(expanded); fn result() { expanded!(.(x: (. -> .) & (. -> .) & (. -> .)) -> . { () }) { .(x: .) { x } } next { .(x: .) { x } } last { .(x: .) { x } } }",
    );
    input[0].1 = parse(&provider).unwrap();
    prime_roundtrip(&check_modules(input).unwrap());
}

#[test]
fn declaration_capacity_rechecks_domains_solved_by_fixed_suffixes() {
    let helpers = r#"
pure fn unit_one_checked(ct: __Comptime__, _ty: __Type__, _value: __Checked_term__) -> __Checked_term__ { __term_unit__(ct) }
pure fn unit_three_checked(ct: __Comptime__, _ty: __Type__, _first: __Checked_term__, _second: __Checked_term__, _third: __Checked_term__) -> __Checked_term__ { __term_unit__(ct) }
pub type Constant_sequence = ([A][B] (. & (A -> .)) -> .) -> .;
pub type Expanded = Constant_sequence & . & .;
pub fn constant_sequence(bind: [A][B] (. & (A -> .)) -> .) { () }
"#;
    for (declaration, accepted) in [
        (
            "pub elab bad : [A] A -> . { trailing product; trailing product next; impl unit_one_checked }",
            false,
        ),
        (
            "pub elab bad : [A] (A -> .) -> A -> (A -> .) -> . { trailing product; trailing product next; trailing product last; trailing thunk end; impl unit_three_checked }",
            false,
        ),
        (
            "pub elab good : [X] (X -> .) -> X -> (([A][B] (X & (A -> Expanded)) -> Expanded) -> Expanded) -> . { trailing sequence; trailing product next; trailing product last; trailing sequence end; impl unit_three_checked }",
            true,
        ),
        (
            "pub elab bad : [X] (X -> .) -> X -> (([A][B] (X & (A -> Expanded)) -> Expanded) -> Expanded) -> . { trailing thunk; trailing product next; trailing product last; trailing sequence end; impl unit_three_checked }",
            false,
        ),
    ] {
        let mut input = modules(if accepted {
            "module consumer; import provider(good, Expanded, constant_sequence); fn result() { good!(.(x: Expanded) -> . { () }) { () } next { () } last { () } end { (constant_sequence, (), ()) } }"
        } else {
            "module consumer; fn unrelated() { () }"
        });
        input[0].1 = parse(&format!("{PROVIDER}\n{helpers}\n{declaration}")).unwrap();
        match check_modules(input) {
            Ok(_) => assert!(accepted, "unexpectedly accepted {declaration}"),
            Err(error) => {
                assert!(!accepted, "{declaration}\n{error:?}");
                assert_eq!(error.file_path, Path::new("provider.kio"));
                assert_eq!(error.error.exit_code(), crate::exit_code::ExitCode::Type);
                assert!(error.error.diag().1.contains("trailing"), "{error:?}");
            }
        }
    }
}

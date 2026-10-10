use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::pass::resolve::{LocatedError, Package};
use crate::pass::{desugar, label_elab, parser, rec_headers};

use super::check_package_signatures;
use crate::pass::contract::NoBody;

type Contract = crate::pass::contract::Contract<Lowered>;

fn lower(sources: &[(&str, &str)]) -> Vec<(PathBuf, Module<Lowered>)> {
    let desugared = sources
        .iter()
        .map(|(file, source)| {
            (
                PathBuf::from(file),
                desugar::desugar_module(parser::parse(source).unwrap()).unwrap(),
            )
        })
        .collect();
    label_elab::elaborate_package(desugared, None).unwrap().0
}

fn project(module: &Module<Lowered>) -> Module<Contract> {
    Module {
        path: module.path.clone(),
        imports: module.imports.clone(),
        items: module
            .items
            .iter()
            .filter_map(|item| {
                Some(match item {
                    Item::FnDef(def) => Item::FnDef(FnDef {
                        vis: def.vis.clone(),
                        purity: def.purity,
                        name: def.name.clone(),
                        sig: convert_signature(&def.sig),
                        ret: convert_type(&def.ret),
                        ret_elided: def.ret_elided,
                        body: NoBody,
                        meta: convert_meta(&def.meta),
                        doc: def.doc.clone(),
                    }),
                    Item::TypeAlias(def) => Item::TypeAlias(convert_type_alias(def)),
                    Item::Newtype(def) => Item::Newtype(convert_newtype(def)),
                    Item::TypeRecGroup(def) => Item::TypeRecGroup(convert_type_rec_group(def)),
                    Item::HostFn(def) => Item::HostFn(convert_host_fn(def)),
                    Item::HostType(def) => Item::HostType(convert_host_type(def)),
                    Item::Equiv(..) | Item::Elaborator(..) => return None,
                    Item::Labels(_, witness) | Item::LabelForward(_, witness) => match *witness {},
                    Item::Op(_, witness) | Item::VariadicOperator(_, witness) => match *witness {},
                    Item::RecGroup(_, witness) | Item::LiteralAlias(_, witness) => {
                        match *witness {}
                    }
                })
            })
            .collect(),
        meta: convert_meta(&module.meta),
        doc: module.doc.clone(),
    }
}

fn packages(sources: &[(&str, &str)]) -> (Package<Lowered>, Package<Contract>) {
    let lowered = lower(sources);
    let ordinary = Package::build(Path::new("."), lowered, None).unwrap();
    ordinary.resolve_imports().unwrap();
    ordinary.check_no_value_cycles().unwrap();
    let projected = ordinary
        .modules()
        .map(|(_, entry)| (entry.file_path.clone(), project(&entry.module)))
        .collect();
    let contract = Package::build(Path::new("."), projected, None).unwrap();
    contract.resolve_imports().unwrap();
    contract.check_no_value_cycles().unwrap();
    (ordinary, contract)
}

fn error_key(error: LocatedError) -> (PathBuf, String) {
    (error.file_path, format!("{:?}", error.error))
}

#[test]
fn bodyless_ordinary_pure_generic_uses_the_shared_nominal_and_header_checks() {
    let sources = [
        (
            "provider.kio",
            "module provider; pub newtype Token : . { pub constructor mk; pub projector get; };",
        ),
        (
            "other.kio",
            "module other; newtype Token : . { constructor mk; projector get; };",
        ),
        (
            "consumer.kio",
            "module consumer; import provider as p; type Local = .; pure fn local(x: Local) -> Local { x } pub pure fn keep[A](unused: A)(x: p.Token) -> p.Token { x }",
        ),
    ];
    let (ordinary, contract) = packages(&sources);
    ordinary.check_in_body_resolution().unwrap();
    contract.check_signature_resolution().unwrap();
    check_package_signatures(&ordinary).unwrap();
    check_package_signatures(&contract).unwrap();
    let module = &contract.module("consumer").unwrap().module;
    let Item::FnDef(def) = &module.items[2] else {
        panic!("ordinary function retained")
    };
    let _: NoBody = def.body;
    assert_eq!(def.purity, Purity::Pure);
    assert_eq!(def.name, "keep");
    assert_eq!(def.sig.groups.len(), 3);
    let Type::Path { segments, .. } = &def.ret else {
        panic!("qualified nominal")
    };
    assert_eq!(
        segments.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["p", "Token"]
    );
    assert_eq!(
        def.meta.span,
        ordinary.module("consumer").unwrap().module.items[2]
            .meta()
            .span
    );
}

#[test]
fn bodyless_validation_preserves_private_nominal_and_kind_error_ownership() {
    for consumer in [
        "module consumer; import provider as p; pub pure fn bad(x: p.Hidden) -> . { () }",
        "module consumer; import provider as p; pub pure fn bad[A](x: A(.)) -> . { () }",
    ] {
        let (ordinary, contract) = packages(&[
            (
                "provider.kio",
                "module provider; newtype Hidden : . { constructor mk; projector get; };",
            ),
            ("consumer.kio", consumer),
        ]);
        ordinary.check_in_body_resolution().unwrap();
        contract.check_signature_resolution().unwrap();
        let expected = check_package_signatures(&ordinary).unwrap_err();
        let actual = check_package_signatures(&contract).unwrap_err();
        assert_eq!(error_key(actual), error_key(expected));
    }
}

#[test]
fn bodyless_phase_preserves_the_lowered_capability_signature_policy() {
    for (source, valid) in [
        (
            "module caps; import __comptime__; pub pure fn valid(ctx: __Comptime__)(ty: __Type__) -> __Type__ { ty }",
            true,
        ),
        (
            "module caps; import __comptime__; pub pure fn invalid(ty: __Type__) -> __Type__ { ty }",
            false,
        ),
        (
            "module caps; import __comptime__; pub pure fn invalid(ctx: __Comptime__)(x: .)(fills: __Fill_ctx__) -> . { () }",
            false,
        ),
    ] {
        let (ordinary, contract) = packages(&[("caps.kio", source)]);
        ordinary.check_in_body_resolution().unwrap();
        contract.check_signature_resolution().unwrap();
        let expected = check_package_signatures(&ordinary).map_err(error_key);
        let actual = check_package_signatures(&contract).map_err(error_key);
        assert_eq!(actual.is_ok(), valid);
        assert_eq!(actual, expected);
    }
}

#[test]
fn bodyless_projection_does_not_turn_a_body_into_a_signature_obligation() {
    let (ordinary, contract) = packages(&[(
        "sample.kio",
        "module sample; pub pure fn keep[A](x: A) -> A { absent_body_value }",
    )]);
    assert!(ordinary.check_in_body_resolution().is_err());
    contract.check_signature_resolution().unwrap();
    check_package_signatures(&contract).unwrap();
}

#[test]
fn bodyless_signature_errors_are_stable_under_provider_input_order() {
    let first = ("a.kio", "module a; pub pure fn bad[A](x: A(.)) -> . { () }");
    let second = ("z.kio", "module z; pub pure fn bad[B](x: B(.)) -> . { () }");
    let mut expected = None;
    for sources in [[first, second], [second, first], [first, second]] {
        let (_, contract) = packages(&sources);
        contract.check_signature_resolution().unwrap();
        let error = error_key(check_package_signatures(&contract).unwrap_err());
        if let Some(expected) = &expected {
            assert_eq!(&error, expected);
        }
        expected = Some(error);
    }
}

#[test]
fn bodyless_function_and_auxiliary_items_are_structurally_excluded_from_typing() {
    trait NotTyper<Choice> {
        fn prove() {}
    }
    impl<T> NotTyper<()> for T {}
    struct HasTyper;
    impl<T: super::TyperPhase> NotTyper<HasTyper> for T {}
    let _ = <Contract as NotTyper<_>>::prove;
    fn body(_: <Contract as Phase>::FnBody) -> NoBody {
        NoBody
    }
    fn equiv(witness: <Contract as Phase>::ItemEquiv) -> Never {
        witness
    }
    fn elaborator(witness: <Contract as Phase>::ItemElaborator) -> Never {
        witness
    }
    let _ = (body, equiv, elaborator);
    assert_eq!(std::mem::size_of::<NoBody>(), 0);
}

#[test]
fn bodyless_recursive_member_headers_reuse_the_existing_validator() {
    let mut source = parser::parse("module recursion; rec(loop) { fn first[A](x: A) -> A { x }; fn second[A](x: A) -> A { x } }").unwrap();
    let Item::RecGroup(group, _) = source.items.pop().unwrap() else {
        panic!("source group")
    };
    rec_headers::validate(&group).unwrap();
    source.items = group
        .members
        .clone()
        .into_iter()
        .map(|def| Item::FnDef(def.into_impure()))
        .collect();
    let (lowered, _) = label_elab::elaborate_package(
        vec![(
            PathBuf::from("recursion.kio"),
            desugar::desugar_module(source).unwrap(),
        )],
        None,
    )
    .unwrap();
    let projected = project(&lowered[0].1);
    let mut headers = RecGroup::<Contract> {
        loop_path: group.loop_path.clone(),
        source_trivia: None,
        members: projected
            .items
            .into_iter()
            .map(|item| {
                let Item::FnDef(def) = item else {
                    panic!("member header")
                };
                FnDef {
                    vis: def.vis,
                    purity: (),
                    name: def.name,
                    sig: def.sig,
                    ret: def.ret,
                    ret_elided: def.ret_elided,
                    body: NoBody,
                    meta: def.meta,
                    doc: def.doc,
                }
            })
            .collect(),
        meta: convert_meta(&group.meta),
    };
    rec_headers::validate(&headers).unwrap();
    headers.members[1].ret = Type::Bottom {
        meta: headers.members[1].ret.meta().clone(),
    };
    let mut invalid_source = group;
    invalid_source.members[1].ret = Type::Bottom {
        meta: invalid_source.members[1].ret.meta().clone(),
    };
    assert_eq!(
        format!("{:?}", rec_headers::validate(&headers).unwrap_err()),
        format!("{:?}", rec_headers::validate(&invalid_source).unwrap_err()),
    );
}

#[test]
fn signature_only_on_ordinary_phases_still_checks_retained_auxiliary_items() {
    for source in [
        "module claims; fn identity[A](x: A) -> A { x } equiv wrong { (); identity }",
        "module claims; import __comptime__; pure fn implementation(ct: __Comptime__) -> . { () } elab wrong : . -> . { impl implementation; };",
    ] {
        let (ordinary, contract) = packages(&[("claims.kio", source)]);
        ordinary.check_in_body_resolution().unwrap();
        assert!(check_package_signatures(&ordinary).is_err());
        contract.check_signature_resolution().unwrap();
        check_package_signatures(&contract).unwrap();
    }
}

#[cfg(feature = "lsp")]
#[test]
fn full_checks_keep_scope_backed_recording_in_both_execution_modes() {
    use super::{DeclarationPhase, PackageTypecheckScope, TypeInterner, TypecheckExecution};

    let (ordinary, _) = packages(&[(
        "recording.kio",
        "module recording; pure fn identity[A](x: A) -> A { x } fn consume() -> . { identity(()) }",
    )]);
    ordinary.check_in_body_resolution().unwrap();
    let normalized = crate::pass::alpha_normalize::normalize_package(&ordinary);
    let ordinary = normalized.package();
    let module = &ordinary.modules().next().unwrap().1.module;
    let mut counts = None;
    for execution in [
        TypecheckExecution::Sequential,
        TypecheckExecution::AllowParallel,
    ] {
        let scope = PackageTypecheckScope::fresh_for_module(
            module,
            Some(ordinary),
            std::sync::Arc::new(TypeInterner::default()),
        )
        .with_refactoring_facts();
        let mut elaborations = Lowered::new_elaborations(&scope);
        super::check_package_modules_with_typecheck_scope_collect_errors_with_execution(
            ordinary,
            &mut elaborations,
            scope,
            execution,
        )
        .unwrap();
        let index = elaborations.position_index();
        assert!(index.binder_count() > 0);
        let observed = index
            .refactoring_storage_counts()
            .expect("scope-backed function tables merge their refactoring facts");
        assert!(observed.0 + observed.1 + observed.2 + observed.3 + observed.4 > 0);
        if let Some(previous) = counts {
            assert_eq!(observed, previous);
        }
        counts = Some(observed);
    }
}

#[test]
fn ordinary_layout_and_serialization_checkpoint() {
    use std::mem::{align_of, size_of};
    println!(
        "FnDef<Surface> size={} align={}",
        size_of::<FnDef<Surface>>(),
        align_of::<FnDef<Surface>>()
    );
    println!(
        "FnDef<Prime> size={} align={}",
        size_of::<FnDef<Prime>>(),
        align_of::<FnDef<Prime>>()
    );
    println!(
        "RecGroup<Surface> size={} align={}",
        size_of::<RecGroup<Surface>>(),
        align_of::<RecGroup<Surface>>()
    );
    println!(
        "Item<Surface> size={} align={}",
        size_of::<Item<Surface>>(),
        align_of::<Item<Surface>>()
    );
    println!(
        "Module<Surface> size={} align={}",
        size_of::<Module<Surface>>(),
        align_of::<Module<Surface>>()
    );
    println!(
        "FnDef<Contract> size={} align={}",
        size_of::<FnDef<Contract>>(),
        align_of::<FnDef<Contract>>()
    );
    for modifier in ["", "pure "] {
        let source = format!("module x; pub {modifier}fn identity[A](x: A) -> A {{ x }}");
        let module = parser::parse(&source).unwrap();
        let Item::FnDef(function) = &module.items[0] else {
            panic!("ordinary function")
        };
        let json = serde_json::to_string(function).unwrap();
        let binary = postcard::to_allocvec(function).unwrap();
        assert_eq!(serde_json::from_str::<FnDef>(&json).unwrap(), *function);
        assert_eq!(postcard::from_bytes::<FnDef>(&binary).unwrap(), *function);
        println!("{json}");
        println!("{binary:?}");
    }
}

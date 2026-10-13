//! Go backend — IR → Go package lowering.
//!
//! Implements the contract in
//! [`specs/backends/go.md`](../../../../specs/backends/go.md): consumes a
//! `Package<Routed>` (post-recovery + post-resolution-lowering — see
//! [`crate::pass::structural_recovery`] and
//! [`crate::pass::route`]) and produces a self-contained Go
//! package as a [`GoPackage`] (file-name → content map).
//!
//! ## The body and the FFI skin
//!
//! Go's **body** computes over erased `any` values: products use `[]any`,
//! opaque host constructors retain their native storage, and private nominal
//! payloads stay inside declaration-owned envelopes. Public erased slots use
//! `Value`, so generic transport preserves that representation even when a
//! concrete native view needs conversion. Runtime type arguments remain erased;
//! exact boundary plans supply the native views and their role adapters.
//!
//! The **skin** is the host's typed contract — the `Host` interface, the
//! semantic facade types, the exported surface, the stable per-signature
//! aliases, and statement-oriented boundary wrappers — and stays fully typed
//! and idiomatic. One backend-neutral prepared boundary transaction is the
//! authority for every public signature and conversion.

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::BTreeMap;

use crate::ast::{Expr, Role, Routed, Type};
use crate::backends::boundary_facade::{
    BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, CallableSourceParamAdapter,
    PreparedBoundaryCallableSites, QualifiedTypeName,
};
use crate::backends::product_rebuild::{
    NominalValueRepresentation, ProductRebuildPlan, bound_product_rebuild_plan,
};
use crate::backends::skin::{
    FfiDir, HostRoleTable, exact_host_role, host_role_table, runtime_param_name_groups,
};
use crate::backends::tail_plan::{ResultChild, TailContext};
use crate::host_descriptor;
use crate::pass::resolve::Package;

use super::facade::{
    GoFacadeCatalog, GoFacadeScope, GoFacadeUseRef, GoFacadeUseView, GoLiveCallableEntry,
    GoLiveExistentialProjectorEntry, GoLiveFacadeUseRef, GoLiveFacadeUseView, GoLiveHeadStage,
    GoLiveNewtypeMemberEntry, GoLiveValueHeadStage, GoLiveValueSourceGroupView, GoPublicParameter,
};
use super::facade_skin::{self, GoFreshNames, GoStatementSink};
use super::naming::GoIdentifier;

/// An unrecoverable error during Go emit. Mirrors
/// [`crate::backends::rust::emit::EmitError`]'s shape — the build
/// dispatcher surfaces these as `BackendError::Build`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitError {
    pub message: String,
}

#[cfg(all(test, feature = "surface"))]
mod facade_consumer_tests {
    use super::*;
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file, parse_signature_file};
    use crate::pass::resolve::PackageFileEntry;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    const I32_HOST: &str = "KioHost_M1_C3_apiN3_I32";
    const TEXT_HOST: &str = "KioHost_M1_C3_apiN4_Text";

    #[test]
    fn native_docs_follow_selected_public_declarations() {
        let package = package(
            "module api;\n\
            /// HOST-GENERIC-DOC\n\
            host type Box[A];\n\
            /// HOST-FN-DOC\n\
            host fn effect() -> .;\n\
            /// OPAQUE-DOC\n\
            pub newtype Token : . { pub constructor make; projector read; };\n\
            /// EXPORT-DOC big.Int\n\
            pub fn call() -> . { effect() }\n\
            /// PRIVATE-DOC\n\
            fn hidden() -> . { () }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit documented Go");
        assert!(emitted.host_go.contains("// HOST-FN-DOC\n"));
        assert!(emitted.pkg_go.contains("// EXPORT-DOC big.Int\nfunc "));
        assert!(emitted.shapes_go.contains("// HOST-GENERIC-DOC\ntype "));
        assert!(emitted.shapes_go.contains("// OPAQUE-DOC\ntype "));
        for source in [&emitted.pkg_go, &emitted.host_go, &emitted.shapes_go] {
            assert!(!source.contains("PRIVATE-DOC"));
            assert!(
                !source.contains("\"math/big\""),
                "documentation cannot select imports: {source}"
            );
        }
        assert!(references_big(
            "// big.Int\nfunc f() *big.Int { return nil }"
        ));
        assert!(!references_big("\t// big.Int\nfunc f() {}"));
    }

    #[test]
    fn native_docs_preserve_executable_bigint_imports() {
        let package = package(
            "module api; host type Integer role(i128); pub fn identity(value: Integer) -> Integer { value }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit actual bigint binding");
        assert!(emitted.host_go.contains("\"math/big\""));
        assert!(emitted.host_go.contains("*big.Int"));
    }

    fn package(source: &str) -> Package<Routed> {
        let parsed = parse(source).expect("parse Go facade test module");
        let package_file = parse_package_file("package pkg; bridge { api; }", None)
            .expect("parse Go facade test package file");
        let (modules, package_file) = FullPipeline::lower_package(
            vec![(PathBuf::from("api.kio"), parsed)],
            Some(package_file),
        )
        .expect("lower Go facade test package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file)
            .expect("build Go facade test package");
        package
            .resolve_imports()
            .expect("resolve Go facade test uses");
        package
            .check_in_body_resolution()
            .expect("resolve Go facade test bodies");
        let prime = check_package(&package).expect("typecheck Go facade test package");
        crate::pass::route::route_package(&crate::pass::structural_recovery::recover_package(
            &prime,
        ))
    }

    #[test]
    #[should_panic(
        expected = "check_literal_annotation established an exact role-typed host annotation"
    )]
    fn routed_invariant_non_host_literal_panics() {
        let package = package("module api; pub fn unit() -> . { () }");
        let prepared = PreparedBoundaryCallableSites::collect_live(&package).unwrap();
        let facades = GoFacadeCatalog::prepare(&prepared).unwrap();
        let roles = HostRoleTable::new();
        let selective_imports = SelectiveImports::new();
        let qualified_imports = QualifiedImports::new();
        let mut emitter = BodyEmitter::new(
            &package,
            &roles,
            "api",
            &selective_imports,
            &qualified_imports,
            &facades,
        );
        let meta = crate::ast::Meta::new(crate::span::Span::new(0, 0));
        let _ = emitter.emit_into(
            &Expr::IntLit {
                occurrence: (),
                digits: "1".into(),
                annotation: Type::Unit { meta: meta.clone() },
                meta,
            },
            GoDestination::Return,
            &mut GoBlock::default(),
        );
    }

    fn replayed_interface(source: &str) -> crate::sig::ReplayedInterface {
        let signature =
            parse_signature_file(source, None).expect("parse retained Go facade test signature");
        crate::sig::replay(&signature).expect("replay retained Go facade test signature")
    }

    fn emitted_method<'a>(source: &'a str, name: &str) -> &'a str {
        let marker = if name.starts_with("CallExp_") {
            format!("func {name}[")
        } else {
            format!(") {name}(")
        };
        let marker_at = source.find(&marker).expect("emitted Go method name");
        let start = if name.starts_with("CallExp_") {
            marker_at
        } else {
            source[..marker_at]
                .rfind("func (")
                .expect("emitted Go method start")
        };
        let tail = &source[start..];
        let end = tail[1..]
            .find("\nfunc ")
            .map_or(tail.len(), |offset| offset + 1);
        &tail[..end]
    }

    fn converted_continuation_call(method: &str) -> &str {
        method
            .lines()
            .find(|line| {
                line.contains("__kio_function_call") && line.contains(" = __kio_function_inner")
            })
            .expect("converted host continuation call")
    }

    fn call_arguments(call: &str) -> &str {
        let open = call.find('(').expect("host continuation call open");
        let close = call.rfind(')').expect("host continuation call close");
        &call[open + 1..close]
    }

    fn assert_selected_result_is_returned(method: &str) {
        let selected = method
            .find(" = kioApplyValue(__kio_existential_selected_callee")
            .expect("selected CPS result call");
        let returned = method
            .rfind("\n\treturn __kio_facade_value")
            .expect("outer facade result return");
        assert!(selected < returned, "{method}");
    }

    fn assert_deprecated_declaration(source: &str, declaration: &str) {
        let declaration_at = source
            .find(declaration)
            .unwrap_or_else(|| panic!("missing Go declaration `{declaration}`\n{source}"));
        let declaration_line = source[..declaration_at]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let prefix = &source[..declaration_line];
        let previous = prefix
            .lines()
            .next_back()
            .unwrap_or_else(|| panic!("Go declaration `{declaration}` has no doc comment"));
        assert!(
            previous.trim_start().starts_with("// Deprecated:"),
            "Go declaration `{declaration}` is not immediately deprecated:\n{source}"
        );
    }

    #[test]
    fn marked_type_selector_preserves_its_exact_source_identity() {
        let ordinary = export_selector_name(&ExportSelector::Type("Box".to_owned()), false);
        let marked = export_selector_name(&ExportSelector::Type("_Box".to_owned()), false);
        assert_eq!(ordinary, "KioType_Box");
        assert_eq!(marked, "KioType__uBox");
        assert_ne!(ordinary, marked);
    }

    #[test]
    fn native_parameters_preserve_source_groups_and_function_value_domains() {
        let package = package(
            "module api; host type I32 role(i32); \
             host fn split(left: I32, right: I32) -> I32; \
             host fn packed(pair: I32 & I32) -> I32; \
             host fn mixed(left: I32, pair: I32 & I32) -> I32; \
             host fn callback(step: (I32 & I32) -> I32) -> I32; \
             pub fn first(left: I32, _right: I32) -> I32 { left } \
             pub fn second((left: I32, _right: I32)) -> I32 { left } \
             pub fn staged(left: I32)(_right: I32) -> I32 { left } \
             pub fn returned(left: I32) -> I32 -> I32 { .(_right) { left } }",
        );
        let emitted = lower_package(&package, "product").expect("emit grouped Go boundaries");
        let pair = format!("Product[{I32_HOST}, {I32_HOST}]");
        for signature in [
            format!("Api__split(arg0 {I32_HOST}, arg1 {I32_HOST}) {I32_HOST}"),
            format!("Api__packed(arg0 {pair}) {I32_HOST}"),
            format!("Api__mixed(arg0 {I32_HOST}, arg1 {pair}) {I32_HOST}"),
            format!("Api__callback(arg0 func({pair}) {I32_HOST}) {I32_HOST}"),
        ] {
            assert!(
                emitted.host_go.contains(&signature),
                "{signature}\n{}",
                emitted.host_go
            );
        }
        for signature in [
            format!("First(arg0 {I32_HOST}, arg1 {I32_HOST}) {I32_HOST}"),
            format!("Second(arg0 {pair}) {I32_HOST}"),
            format!("Staged(arg0 {I32_HOST}, arg1 {I32_HOST}) {I32_HOST}"),
            format!("Returned(arg0 {I32_HOST}) func({I32_HOST}) {I32_HOST}"),
        ] {
            assert!(
                emitted.pkg_go.contains(&signature),
                "{signature}\n{}",
                emitted.pkg_go
            );
        }
        assert!(emitted.pkg_go.contains("package product"));
        assert!(emitted.pkg_go.contains("type Package["));
        assert!(emitted.host_go.contains("type Host["));
        assert!(emitted.pkg_go.contains("func New["));
        assert!(!emitted.pkg_go.contains("CreateProduct"));
        assert!(emitted.shapes_go.contains("type Product["));
        assert!(emitted.ffi_go.contains("Env_Api__mixed_arg1"));
        assert!(emitted.ffi_go.contains("Env_Api__callback_arg0_cbarg0"));
    }

    #[test]
    fn host_unit_return_has_no_clause_and_unit_parameter_is_publicly_nullary() {
        let package = package(
            "module api; \
             host type Text role(str); \
             host fn sink(value: Text) -> .; \
             host fn unit_arg(value: .) -> Text;",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go facade package");
        assert!(
            emitted
                .host_go
                .contains(&format!("Api__sink(arg0 {TEXT_HOST})\n")),
            "{}",
            emitted.host_go
        );
        assert!(
            !emitted
                .host_go
                .contains(&format!("Api__sink(arg0 {TEXT_HOST}) Unit")),
            "{}",
            emitted.host_go
        );
        assert!(
            emitted
                .host_go
                .contains(&format!("Api__unitArg() {TEXT_HOST}")),
            "{}",
            emitted.host_go
        );
        assert!(
            !emitted.host_go.contains("Api__unitArg(arg0 Unit)"),
            "{}",
            emitted.host_go
        );
    }

    #[test]
    fn roleless_host_binding_is_exact_across_the_complete_public_root() {
        let package = package(
            "module api; \
             host type Token; \
             host fn round(value: Token) -> Token; \
             pub fn exported(value: Token) -> Token { value }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit exact Go host binding");
        let binding = "KioHost_M1_C3_apiN5_Token";

        assert!(
            emitted
                .host_go
                .contains(&format!("type Host[{binding} any] interface {{")),
            "{}",
            emitted.host_go
        );
        assert!(
            emitted
                .host_go
                .contains(&format!("Api__round(arg0 {binding}) {binding}")),
            "{}",
            emitted.host_go
        );
        assert!(
            emitted
                .pkg_go
                .contains(&format!("type Package[{binding} any] struct {{")),
            "{}",
            emitted.pkg_go
        );
        assert!(
            emitted.pkg_go.contains(&format!(
                "func New[{binding} any](host Host[{binding}]) *Package[{binding}]"
            )),
            "{}",
            emitted.pkg_go
        );
        // Go forbids a generic alias whose entire right-hand side is one of
        // the alias's type parameters. The package-root binding itself is the
        // exact public type for these atomic slots; no erased slot alias may
        // replace it.
        assert!(!emitted.ffi_go.contains("type Env_Api__round_arg0"));
        assert!(!emitted.ffi_go.contains("type Exp_Api__exported_ret"));
        for public in [&emitted.host_go, &emitted.pkg_go, &emitted.ffi_go] {
            assert!(!public.contains("Api__round(arg0 any) any"), "{public}");
        }
    }

    #[test]
    fn role_binding_uses_exact_host_type_and_declaration_keyed_adapters() {
        let package = package(
            "module api; \
             host type Count role(i32); \
             host fn round(value: Count) -> Count; \
             pub fn via(value: Count) -> Count { round(value) }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit exact Go role binding");
        let binding = "KioHost_M1_C3_apiN5_Count";
        let convert_in = "KioHostIn_api_Count";
        let convert_out = "KioHostOut_api_Count";

        for required in [
            format!("type Host[{binding} any] interface {{"),
            format!("{convert_in}(value {binding}) int32"),
            format!("{convert_out}(value int32) {binding}"),
            format!("Api__round(arg0 {binding}) {binding}"),
        ] {
            assert!(
                emitted.host_go.contains(&required),
                "{required}\n{}",
                emitted.host_go
            );
        }
        assert!(
            emitted
                .pkg_go
                .contains(&format!("pkg.host.{convert_in}(__kio_value_argument"))
        );
        assert!(emitted.pkg_go.contains(&format!("pkg.host.{convert_out}(")));
    }

    #[test]
    fn parameterized_host_binding_uses_a_declaration_owned_typed_carrier() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host type Box[A]; \
             host fn round(value: Box(I32)) -> Box(I32);",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go host carrier");
        let atom = "KioIndexNominal_M1_C3_apiN3_I32[KioHost_M1_C3_apiN3_I32]";
        let carrier = "KioHostType_api__Box";

        assert!(
            emitted.host_go.contains(&format!(
                "Api__round(arg0 {carrier}[{atom}]) {carrier}[{atom}]"
            )),
            "{}",
            emitted.host_go
        );
        for required in [
            format!("type {carrier}[T0 KioIndexKind_S] struct {{"),
            format!("func Unsafe{carrier}FromNative[T0 KioIndexKind_S](value any) {carrier}[T0]"),
            format!("func (value {carrier}[T0]) UnsafeNative() any"),
        ] {
            assert!(
                emitted.shapes_go.contains(&required),
                "{required}\n{}",
                emitted.shapes_go
            );
        }
        assert!(!emitted.host_go.contains("Api__round(arg0 any) any"));
        assert!(emitted.shapes_go.contains("[0]func(T0)"));
        assert!(emitted.ffi_go.contains("func PrepareHost_api_Box["));
    }

    #[test]
    fn stage_selection_uses_lexical_names_and_only_semantic_captures() {
        let package = package(
            "module api; \
             pub newtype Box[A] : A { pub constructor make; projector open; }; \
             pub newtype Poly : [B] B -> Box(B) { pub constructor make; pub projector open; }; \
             pub newtype Keep[A] : [C] C -> (A & C) { pub constructor make; pub projector open; }; \
             pub fn outer[A](a: A) -> Poly { Poly.make(.[B](b: B) { Box.make(b) }) } \
             pub fn sparse[A][B](a: A, b: B) -> Keep(B) { Keep.make(.[C](c: C) { (b, c) }) }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit selected transparent stages");
        assert!(
            emitted
                .ffi_go
                .contains("func SelectExp_Api__outer_ret[KioI1 KioIndexKind_S](stage ")
        );
        assert!(emitted.ffi_go.contains(
            "func SelectExp_Api__sparse_ret[KioI2 KioIndexKind_S, KioI1 KioIndexKind_S](stage "
        ));
        for prefix in ["Value", "Native", "Prepare", "Unprepare"] {
            assert!(
                emitted
                    .ffi_go
                    .contains(&format!("func {prefix}Exp_Api__outer_ret(pkg *Package,"))
            );
        }
        for line in emitted
            .ffi_go
            .lines()
            .filter(|line| line.starts_with("func Select"))
        {
            let declaration = line.split("](stage").next().unwrap();
            for index in 0..3 {
                assert!(
                    declaration
                        .matches(&format!("KioI{index} KioIndexKind_"))
                        .count()
                        <= 1,
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn contextual_sum_rows_capture_only_referenced_indices_and_prepare_host_arguments() {
        let package = package(
            "module api; host type Number role(i32); host type Vault[A]; \
             host fn choose() -> (. | Number); \
             pub fn sparse() -> [A] A -> [B] B -> (. | Number) { .[A](a: A)[B](b: B) { choose() } } \
             pub fn round[A](value: Vault(. | A)) -> Vault(. | A) { value }",
        );
        let emitted =
            lower_package(&package, "pkg").expect("emit exact contextual sum dependencies");
        for declaration in emitted
            .shapes_go
            .lines()
            .filter(|line| line.starts_with("type kioSumUse_"))
        {
            if declaration.contains("sparse") {
                assert!(!declaration.contains("KioI0"), "{declaration}");
            }
        }
        assert!(
            emitted
                .ffi_go
                .contains("type Exp_Api__round_arg0_typearg0[")
        );
        assert!(emitted.shapes_go.contains("type KioErasedIndex_S struct"));
    }

    #[test]
    fn returned_foralls_emit_one_selection_pair_per_source_stage() {
        for depth in 1..=3 {
            let names = ["A", "B", "C"];
            let mut result = ".".to_owned();
            let mut body = "()".to_owned();
            for name in names[..depth].iter().rev() {
                result = format!("[{name}] {name} -> {result}");
                body = format!(".[{name}](value: {name}) {{ {body} }}");
            }
            let package = package(&format!(
                "module api; pub fn stage() -> {result} {{ {body} }}"
            ));
            let emitted = lower_package(&package, "pkg").expect("emit nested source stages");
            assert_eq!(
                emitted
                    .ffi_go
                    .matches("func SelectExp_Api__stage_ret")
                    .count(),
                depth
            );
            assert_eq!(
                emitted
                    .ffi_go
                    .lines()
                    .filter(|line| line.starts_with("func NewExp_Api__stage_ret")
                        && line.contains("Stage("))
                    .count(),
                depth
            );
        }
    }

    #[test]
    fn boundary_view_helpers_do_not_expand_the_generic_handle_method_set() {
        let package = package(
            "module api; host type Number role(i32); host type Vault[A]; \
             host fn store[A](value: Vault(A)) -> Vault(A); \
             pub newtype Box[A] : A { pub constructor make; projector open; }; \
             pub fn round(value: Box(Number)) -> Box(Number) { value } \
             pub fn identity[A](value: A) -> A { value }",
        );
        let prepared = PreparedBoundaryCallableSites::collect(&package, None).unwrap();
        let facades = GoFacadeCatalog::prepare(&prepared).unwrap();
        let aliases = render_ffi_go(&facades, &GoNames::derive("pkg")).unwrap();
        for line in aliases
            .source
            .lines()
            .filter(|line| line.starts_with("func (pkg "))
        {
            let member = line.split_once(") ").unwrap().1;
            for prefix in ["Value", "Native", "Prepare", "Unprepare"] {
                assert!(!member.starts_with(prefix), "{line}");
            }
        }
        for name in [
            "PrepareHost_api_Number",
            "PrepareHost_api_Vault",
            "PrepareExp_Api__round_arg0",
            "UnprepareExp_Api__round_ret",
            "PrepareExp_Api__identity_arg0",
            "ValueHost_api_Number",
            "NativeHost_api_Vault",
            "ValueExp_Api__round_arg0",
            "NativeExp_Api__identity_arg0",
        ] {
            assert!(aliases.source.contains(&format!("func {name}[")), "{name}");
            assert!(aliases.package_names.iter().any(|claim| claim == name));
        }
        for claim in aliases.package_names.iter().filter(|name| {
            ["Value", "Native", "Prepare", "Unprepare"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        }) {
            assert!(
                aliases.source.contains(&format!("func {claim}[")),
                "{claim}"
            );
        }
        assert!(aliases.source.contains("KioI0 KioIndexKind_S"));
    }

    #[test]
    fn generic_public_member_preparation_is_owned_by_host_declarations() {
        let package = package(
            "module api; host type Number role(i32); host type Vault[A]; \
             pub newtype Applied[*F][A] : F(A) { pub constructor make; projector open; };",
        );
        let emitted =
            lower_package(&package, "pkg").expect("emit constructor-only host preparation");
        assert!(emitted.ffi_go.contains("func PrepareHost_api_Number["));
        assert!(emitted.ffi_go.contains("func PrepareHost_api_Vault["));
        assert!(emitted.ffi_go.contains("value KioHostType_api__Vault[KioI0]) Prepared[KioIndexApply_ASS[KioIndexNominal_M1_C3_apiN5_Vault, KioI0]]"));
        assert!(
            emitted
                .shapes_go
                .contains("type Index_api__Number[Native any] = ")
        );
        assert!(emitted.shapes_go.contains("type Index_api__Vault = "));
        assert!(!emitted.pkg_go.contains("Api__open"));
    }

    #[test]
    fn exact_host_binding_is_open_world_under_an_unrelated_declaration() {
        let before = lower_package(
            &package(
                "module api; \
                 host type Count role(i32); \
                 host fn round(value: Count) -> Count;",
            ),
            "pkg",
        )
        .expect("emit Go host binding before the unrelated declaration");
        let after = lower_package(
            &package(
                "module api; \
                 host type Count role(i32); \
                 host fn round(value: Count) -> Count; \
                 fn unrelated(value: .) -> . { value }",
            ),
            "pkg",
        )
        .expect("emit Go host binding after the unrelated declaration");

        assert_eq!(before.host_go, after.host_go);
    }

    #[test]
    fn facade_declarations_form_a_complete_go_source_file() {
        let package = package(
            "module api; host type I32 role(i32); host fn round(value: I32 | I32) -> I32 | I32;",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go facade package");
        assert!(
            emitted
                .shapes_go
                .starts_with("// Generated by kio — do not edit by hand.\n\npackage pkg\n\n"),
            "{}",
            emitted.shapes_go
        );
        assert!(emitted.shapes_go.contains("type Sum"));
    }

    #[test]
    fn interleaved_heads_flatten_value_slots_and_keep_exact_host_bindings() {
        let package = package(
            "module api; \
             host type Text role(str); \
             host fn ordered(value: Text)[T](later: T) -> Text;",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go facade package");
        assert!(
            emitted.host_go.contains(&format!(
                "Api__ordered(arg0 {TEXT_HOST}, arg1 Value) {TEXT_HOST}"
            )),
            "{}",
            emitted.host_go
        );
        let erased_alias = format!("type Env_Api__ordered_arg1[{TEXT_HOST} any] = Value");
        assert!(
            emitted.ffi_go.contains(&erased_alias),
            "{erased_alias}\n{}",
            emitted.ffi_go
        );
        assert!(!emitted.ffi_go.contains("type Env_Api__ordered_arg0"));
        assert!(!emitted.ffi_go.contains("type Env_Api__ordered_ret"));
    }

    #[test]
    fn forall_aliases_retain_stages_and_function_legs_are_recursive() {
        let package = package(
            "module api; \
             host type Text role(str); \
             host fn use_callback(callback: [T] T -> T) -> Text;",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go facade package");
        assert!(
            emitted.ffi_go.contains(&format!(
                "type Env_Api__useCallback_arg0[{TEXT_HOST} any] = TypeStage["
            )),
            "{}",
            emitted.ffi_go
        );
        assert!(
            emitted.ffi_go.contains(&format!(
                "type Env_Api__useCallback_arg0_result_cbarg0[{TEXT_HOST} any] = Value"
            )),
            "{}",
            emitted.ffi_go
        );
        assert!(
            emitted.ffi_go.contains(&format!(
                "type Env_Api__useCallback_arg0_result_cbret[{TEXT_HOST} any] = Value"
            )),
            "{}",
            emitted.ffi_go
        );
        assert!(
            emitted.ffi_go.contains(&format!(
                "type Env_Api__useCallback_arg0_result[{TEXT_HOST} any] = func(Value) Value"
            )),
            "{}",
            emitted.ffi_go
        );
        assert!(
            emitted
                .ffi_go
                .contains("func SelectEnv_Api__useCallback_arg0[KioI0 KioIndexKind_S,")
        );
        assert!(
            emitted
                .ffi_go
                .contains("func(Prepared[KioI0]) Prepared[KioI0]")
        );
    }

    #[test]
    fn value_view_helpers_share_nested_conversion_code_without_sharing_execution() {
        for depth in [4, 8] {
            let ty = format!("{}.", ". -> ".repeat(depth));
            let package = package(&format!(
                "module api; pub fn round(value: {ty}) -> {ty} {{ value }}"
            ));
            let emitted = lower_package(&package, "pkg").expect("emit nested Value views");
            // Two public roots, each with one native Out and one private In
            // closure per stage. Only the native Unit-domain closure is nullary.
            // Re-inlining every suffix would multiply these counts by the depth.
            assert_eq!(emitted.ffi_go.matches("return func()").count(), 2 * depth);
            assert_eq!(
                emitted
                    .ffi_go
                    .matches("return func(__kio_function_domain")
                    .count(),
                2 * depth
            );
            assert_eq!(emitted.ffi_go.matches("return func(").count(), 4 * depth);
            assert!(
                emitted
                    .ffi_go
                    .contains("kioValueExp_Api__round_arg0_cbretIntoBody(host,")
            );
            assert!(
                emitted
                    .ffi_go
                    .contains("kioValueExp_Api__round_arg0_cbretFromBody(host,")
            );
        }
    }

    #[test]
    fn opaque_newtype_keeps_a_handle_without_callable_members() {
        let package = package(
            "module api; pub newtype Token : . { constructor make_token; projector read_token; };",
        );
        let emitted = lower_package(&package, "pkg").expect("emit Go facade package");
        assert!(
            emitted
                .pkg_go
                .contains("type exportNs_M_api__T_Token struct"),
            "{}",
            emitted.pkg_go
        );
        assert!(
            !emitted.pkg_go.contains("func (ns exportNs_M_api__T_Token)"),
            "{}",
            emitted.pkg_go
        );
    }

    #[test]
    fn transparent_both_aliases_recurse_into_sum_and_function_payloads() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub newtype Wrapped_sum : I32 | I32 { \
               pub constructor make_sum; pub projector read_sum; \
             }; \
             pub newtype Wrapped_fn : I32 -> I32 { \
               pub constructor make_fn; pub projector read_fn; \
             }; \
             host fn consume(sum: Wrapped_sum, callback: Wrapped_fn) -> .;",
        );
        let emitted = lower_package(&package, "pkg").expect("emit transparent aliases");

        for fragment in [
            format!("type Env_Api__consume_arg0[{I32_HOST} any] = Sum["),
            format!("type Env_Api__consume_arg0_0[{I32_HOST} any] = "),
            format!("type Env_Api__consume_arg0_1[{I32_HOST} any] = "),
            format!("func NewEnv_Api__consume_arg0_0[{I32_HOST} any]("),
            format!("type Env_Api__consume_arg1[{I32_HOST} any] = func({I32_HOST}) {I32_HOST}"),
        ] {
            assert!(
                emitted.ffi_go.contains(&fragment),
                "{fragment}\n{}",
                emitted.ffi_go
            );
        }
        assert!(
            !emitted
                .ffi_go
                .contains("type Env_Api__consume_arg0_0_value")
        );
        assert!(!emitted.ffi_go.contains("type Env_Api__consume_arg1_cbarg0"));
        assert!(!emitted.ffi_go.contains("type Env_Api__consume_arg1_cbret"));
    }

    #[test]
    fn nested_product_and_sum_alias_paths_keep_every_context_constructible() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host fn nested(value: I32 & (I32 | I32)) -> ((I32 | I32) | I32);",
        );
        let emitted = lower_package(&package, "pkg").expect("emit nested aliases");

        for fragment in [
            format!("type Env_Api__nested_arg0[{I32_HOST} any] = Product["),
            format!("type Env_Api__nested_arg0_field1[{I32_HOST} any] = Sum["),
            format!("type Env_Api__nested_arg0_field1_0[{I32_HOST} any] = "),
            format!("type Env_Api__nested_arg0_field1_1[{I32_HOST} any] = "),
            format!("func NewEnv_Api__nested_arg0_field1_0[{I32_HOST} any]("),
            format!("type Env_Api__nested_ret[{I32_HOST} any] = Sum["),
            format!("type Env_Api__nested_ret_0_value[{I32_HOST} any] = Sum["),
            format!("func NewEnv_Api__nested_ret_0_value_0[{I32_HOST} any]("),
            format!("func NewEnv_Api__nested_ret_0_value_1[{I32_HOST} any]("),
        ] {
            assert!(
                emitted.ffi_go.contains(&fragment),
                "{fragment}\n{}",
                emitted.ffi_go
            );
        }
        assert!(!emitted.ffi_go.contains("Env_Api__nested_arg1"));
    }

    #[test]
    fn retained_nested_aliases_emit_without_live_methods_or_capabilities() {
        let package = package("module api;");
        let replayed = replayed_interface(
            r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old(value: . & (. | .)) -> ((. | .) | (. & .));
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        old;
      }
    }
  }
}
"#,
        );
        let signature = (2, replayed);
        let emitted = lower_package_with_signature(&package, "pkg", Some(&signature))
            .expect("emit retained nested aliases");

        assert!(
            !emitted.host_go.contains("Api__old("),
            "{}",
            emitted.host_go
        );
        assert!(!emitted.pkg_go.contains("Api__old("), "{}", emitted.pkg_go);
        for fragment in [
            "type Env_Api__old_arg0 = Product[",
            "type Env_Api__old_arg0_field0 = Unit",
            "type Env_Api__old_arg0_field1 = Sum[",
            "func NewEnv_Api__old_arg0_field1_0(",
            "type Env_Api__old_ret_0_value = Sum[",
            "type Env_Api__old_ret_1_value = Product[",
            "func NewEnv_Api__old_ret_0_value_1(",
        ] {
            assert!(
                emitted.ffi_go.contains(fragment),
                "{fragment}\n{}",
                emitted.ffi_go
            );
            assert_deprecated_declaration(&emitted.ffi_go, fragment);
        }
        for declaration in [
            "type Product[T0, T1 any] struct {",
            "F0 T0",
            "F1 T1",
            "type Sum_Row[T0, T1 any] struct {",
            "type Sum[T0, T1 any] = KioSum[",
            "type KioSum_K0_Case[R, P any] interface {",
            "Value() P",
            "func NewKioSum_K0[",
            "type KioIndex_Product[",
            "type KioIndex_Sum[",
        ] {
            assert_deprecated_declaration(&emitted.shapes_go, declaration);
        }
        assert_deprecated_declaration(&emitted.ffi_go, "type IndexEnv_Api__old_arg0 = ");
    }

    #[test]
    fn retained_host_type_is_a_deprecated_nominal_not_a_current_host_selection() {
        let package = package("module api; host type Live; host fn current() -> . & .;");
        let replayed = replayed_interface(
            r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        host type Legacy role(i32);
        host fn old() -> Legacy & .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Legacy;
        old;
      }
    }
  }
}
"#,
        );
        let signature = (2, replayed);
        let emitted = lower_package_with_signature(&package, "pkg", Some(&signature))
            .expect("emit retained Go host type");
        let live = "KioHost_M1_C3_apiN4_Live";
        let retained = "KioHost_M1_C3_apiN6_Legacy";

        assert!(
            emitted
                .host_go
                .contains(&format!("type Host[{live} any] interface {{")),
            "{}",
            emitted.host_go
        );
        assert!(!emitted.host_go.contains(retained), "{}", emitted.host_go);
        assert!(
            !emitted.host_go.contains("KioHostIn_api_Legacy")
                && !emitted.host_go.contains("KioHostOut_api_Legacy"),
            "{}",
            emitted.host_go
        );
        assert!(
            emitted.pkg_go.contains(&format!(
                "func New[{live} any](host Host[{live}]) *Package[{live}]"
            )),
            "{}",
            emitted.pkg_go
        );
        assert!(!emitted.pkg_go.contains(retained), "{}", emitted.pkg_go);
        assert_deprecated_declaration(&emitted.shapes_go, &format!("type {retained} struct {{"));

        // The product shell is reached by both `current` and retained `old`.
        // Live provenance wins for that shared declaration, while the
        // site-owned old aliases remain deprecated.
        let product = "type Product[T0, T1 any] struct {";
        assert!(emitted.shapes_go.contains(product), "{}", emitted.shapes_go);
        let product_at = emitted.shapes_go.find(product).unwrap();
        assert!(
            !emitted.shapes_go[..product_at]
                .lines()
                .next_back()
                .is_some_and(|line| line.starts_with("// Deprecated:")),
            "{}",
            emitted.shapes_go
        );
        let index = "type KioIndex_Product[";
        let index_at = emitted
            .shapes_go
            .find(index)
            .expect("shared live structural index");
        assert!(
            !emitted.shapes_go[..index_at]
                .lines()
                .next_back()
                .is_some_and(|line| line.starts_with("// Deprecated:"))
        );
        assert_deprecated_declaration(&emitted.shapes_go, "type Index_api__Legacy[Native any] = ");
        let retained_alias =
            format!("type Env_Api__old_ret[{live} any] = Product[{retained}, Unit]");
        assert_deprecated_declaration(&emitted.ffi_go, &retained_alias);
    }

    #[test]
    fn retained_newtype_carrier_is_deprecated_without_a_package_capability() {
        let package = package("module api;");
        let replayed = replayed_interface(
            r#"signature pkg v(3);
v(1) {
  nonbreaking {
    add {
      module api {
        newtype Token : . { pub constructor make_token; projector read_token; };
        host fn old(value: Token) -> .;
      }
    }
  }
}
v(2) {
  nonbreaking {
    remove {
      module api {
        Token;
        old;
      }
    }
  }
}
"#,
        );
        let signature = (2, replayed);
        let emitted = lower_package_with_signature(&package, "pkg", Some(&signature))
            .expect("emit retained Go newtype carrier");
        let carrier = "KioNewtype_M1_C3_apiN5_Token";

        assert_deprecated_declaration(&emitted.shapes_go, &format!("type {carrier} struct {{"));
        assert_deprecated_declaration(
            &emitted.ffi_go,
            "type Env_Api__old_arg0 = KioNewtype_M1_C3_apiN5_Token",
        );
        assert!(
            !emitted.host_go.contains("Api__old("),
            "{}",
            emitted.host_go
        );
        assert!(!emitted.pkg_go.contains("Token"), "{}", emitted.pkg_go);
    }

    #[test]
    fn direct_host_call_preserves_source_groups_unit_effects_and_left_to_right_order() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host fn unit_effect() -> .; \
             host fn value_effect() -> I32; \
             host fn direct(unit: ., grouped: I32 & I32) -> I32; \
             pub fn call() -> I32 { \
               direct(unit_effect(), (value_effect(), value_effect())) \
             }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit regrouped direct host call");
        assert!(
            emitted.host_go.contains(&format!(
                "Api__direct(arg0 Unit, arg1 Product[{I32_HOST}, {I32_HOST}]) {I32_HOST}"
            )),
            "{}",
            emitted.host_go
        );

        let direct = emitted
            .pkg_go
            .find("pkg.host.Api__direct")
            .expect("direct callee is materialized");
        let unit = emitted
            .pkg_go
            .find("pkg.host.Api__unitEffect")
            .expect("Unit source effect is retained");
        let first_value = emitted
            .pkg_go
            .find("pkg.host.Api__valueEffect")
            .expect("first product source effect");
        let second_value = emitted.pkg_go[first_value + 1..]
            .find("pkg.host.Api__valueEffect")
            .map(|offset| first_value + 1 + offset)
            .expect("second product source effect");
        let expansion = emitted
            .pkg_go
            .find("kioProductSlots(")
            .expect("one product source expands through its prepared RightNest group");
        assert!(
            direct < unit
                && unit < first_value
                && first_value < second_value
                && second_value < expansion,
            "{}",
            emitted.pkg_go
        );
        assert_eq!(
            emitted.pkg_go.matches("pkg.host.Api__valueEffect").count(),
            2
        );
    }

    #[test]
    fn interleaved_host_value_replays_type_heads_and_regroups_its_product_source() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             host fn staged[A](first: I32)[B](second: I32 & I32) -> I32; \
             pub fn call(first: I32, second: I32, third: I32) -> I32 { \
               staged(I32, first, I32, (second, third)) \
             }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit interleaved host value");
        assert!(
            emitted.host_go.contains(&format!(
                "Api__staged(arg0 {I32_HOST}, arg1 Product[{I32_HOST}, {I32_HOST}]) {I32_HOST}"
            )),
            "{}",
            emitted.host_go
        );
        assert_eq!(emitted.pkg_go.matches("pkg.host.Api__staged").count(), 1);
        assert!(
            emitted.pkg_go.matches("kioApplyType(").count() >= 2,
            "{}",
            emitted.pkg_go
        );
        assert!(
            emitted.pkg_go.contains("kioProductSlots("),
            "{}",
            emitted.pkg_go
        );
    }

    #[test]
    fn exported_interleaved_heads_replay_in_order_from_source_groups() {
        let package = package(
            "module api; \
             host type I32 role(i32); \
             pub fn staged[A](first: I32)[B](second: I32 & I32) -> I32 { first }",
        );
        let emitted = lower_package(&package, "pkg").expect("emit interleaved export");
        assert!(
            emitted
                .pkg_go
                .contains("func CallExp_Api__staged[KioI0 KioIndexKind_S, KioI1 KioIndexKind_S,")
                && emitted.pkg_go.contains(&format!(
                    "arg0 {I32_HOST}, arg1 Product[{I32_HOST}, {I32_HOST}]) {I32_HOST}"
                )),
            "{}",
            emitted.pkg_go
        );
        let method = emitted_method(&emitted.pkg_go, "CallExp_Api__staged");
        assert!(method.matches("kioApplyType(").count() >= 2, "{method}");
        assert!(method.contains("[]any{"), "{method}");
    }

    #[test]
    fn generic_newtype_heads_replay_and_church_payload_stays_an_ordinary_projector() {
        let package = package(
            "module api; \
             pub newtype Generic[A] : A { \
               pub constructor make_generic; pub projector read_generic; \
             }; \
             pub newtype Church : [R] ([Hidden] . -> R) -> R { \
               pub constructor make_church; pub projector read_church; \
             };",
        );
        let emitted = lower_package(&package, "pkg").expect("emit generic newtype members");

        let make_generic = emitted_method(&emitted.pkg_go, "CallExp_Api__Generic_makeGeneric");
        assert!(make_generic.contains("kioApplyType("), "{make_generic}");

        let read_church = emitted_method(&emitted.pkg_go, "ReadChurch");
        let header = read_church.lines().next().expect("Church projector header");
        assert!(header.contains("ReadChurch(arg0 "), "{header}");
        assert!(!header.contains(", arg1 "), "{header}");
        assert!(header.contains(") TypeStage["), "{header}");
        for alias in [
            "type Exp_Api__Church_readChurch_ret = TypeStage[",
            "type Exp_Api__Church_readChurch_ret_result_cbarg0 = func(",
            "type Exp_Api__Church_readChurch_ret_result_cbret = Value",
        ] {
            assert!(
                emitted.ffi_go.contains(alias),
                "{alias}\n{}",
                emitted.ffi_go
            );
        }
        assert!(
            !emitted.ffi_go.contains("Exp_Api__Church_readChurch_arg1"),
            "{}",
            emitted.ffi_go
        );
    }

    #[test]
    fn existential_projectors_share_one_grouped_signature_alias_and_invocation_cut() {
        let package = package(
            "module api; \
             pub newtype Single <Hidden> : Hidden { \
               pub constructor make_single; pub projector read_single; \
             }; \
             pub newtype Packed[A] <Hidden> <Secret> : A & Hidden & Secret { \
               pub constructor make_packed; pub projector read_packed; \
             };",
        );
        let emitted = lower_package(&package, "pkg").expect("emit existential projectors");

        let single = emitted_method(&emitted.pkg_go, "ReadSingle");
        let single_header = single.lines().next().expect("Single projector header");
        assert!(
            single_header.contains("ReadSingle(arg0 ")
                && single_header.contains(", arg1 func() func(Value) Value) Value"),
            "{single_header}"
        );
        assert_eq!(single.matches("= arg1\n").count(), 1, "{single}");
        assert_eq!(
            call_arguments(converted_continuation_call(single))
                .split(", ")
                .count(),
            1,
            "{single}"
        );
        assert!(!single.contains("kioProductSlots("), "{single}");
        assert!(
            single.contains(
                "kioTailValue(kioApplyType(__kio_member_continuation), __kio_member_arg0)"
            ),
            "{single}"
        );
        assert_selected_result_is_returned(single);

        let packed = emitted_method(&emitted.pkg_go, "CallExp_Api__Packed_readPacked");
        let packed_header = packed.lines().next().expect("Packed projector header");
        assert!(
            packed_header.contains("CallExp_Api__Packed_readPacked[")
                && packed_header.contains(
                    ", arg1 func() func() func(KioFacade_Product_K3_P0_P1_P2_[Prepared[KioI0], Value, Value]) Value) Value"
                ),
            "{packed_header}"
        );
        assert_eq!(packed.matches("= arg1\n").count(), 1, "{packed}");
        assert_eq!(packed.matches("kioProductSlots(").count(), 1, "{packed}");
        let expanded = packed.find("kioProductSlots(").expect("Packed expansion");
        let slot0 = packed[expanded..].find("[0]").expect("Packed slot 0") + expanded;
        let slot1 = packed[expanded..].find("[1]").expect("Packed slot 1") + expanded;
        let slot2 = packed[expanded..].find("[2]").expect("Packed slot 2") + expanded;
        let host_call = converted_continuation_call(packed);
        let host_call_at = packed
            .find(host_call)
            .expect("Packed host continuation call");
        assert!(
            expanded < slot0 && slot0 < slot1 && slot1 < slot2 && slot2 < host_call_at,
            "{packed}"
        );
        let host_arguments = call_arguments(host_call);
        assert!(
            host_arguments
                .starts_with("KioFacade_Product_K3_P0_P1_P2_[Prepared[KioI0], Value, Value]{"),
            "{host_call}"
        );
        assert!(host_arguments.ends_with('}'), "{host_call}");
        assert!(
            packed.contains("kioTailValue(kioApplyType(kioApplyType(__kio_member_continuation)), __kio_member_arg0)"),
            "{packed}"
        );
        assert_selected_result_is_returned(packed);
        let make_packed = emitted_method(&emitted.pkg_go, "CallExp_Api__Packed_makePacked");
        assert!(
            make_packed.matches("kioApplyType(").count() >= 3,
            "{make_packed}"
        );

        for alias in [
            "type Exp_Api__Single_readSingle_arg0 = ",
            "type Exp_Api__Single_readSingle_arg1 = func() func(Value) Value",
            "type Exp_Api__Single_readSingle_arg1_result_cbarg0 = Value",
            "type Exp_Api__Single_readSingle_arg1_result_cbret = Value",
            "type Exp_Api__Single_readSingle_ret = Value",
            "type Exp_Api__Packed_readPacked_arg0[KioI0 KioIndexKind_S] = ",
            "type Exp_Api__Packed_readPacked_arg1[KioI0 KioIndexKind_S] = func() func() func(KioFacade_Product_K3_P0_P1_P2_[Prepared[KioI0], Value, Value]) Value",
            "type Exp_Api__Packed_readPacked_arg1_result_result_cbarg0[KioI0 KioIndexKind_S] = KioFacade_Product_K3_P0_P1_P2_[Prepared[KioI0], Value, Value]",
            "type Exp_Api__Packed_readPacked_arg1_result_result_cbarg0_field0[KioI0 KioIndexKind_S] = Prepared[KioI0]",
            "type Exp_Api__Packed_readPacked_arg1_result_result_cbarg0_field1[KioI0 KioIndexKind_S] = Value",
            "type Exp_Api__Packed_readPacked_arg1_result_result_cbarg0_field2[KioI0 KioIndexKind_S] = Value",
            "type Exp_Api__Packed_readPacked_arg1_result_result_cbret[KioI0 KioIndexKind_S] = Value",
            "type Exp_Api__Packed_readPacked_ret[KioI0 KioIndexKind_S] = Value",
        ] {
            assert!(
                emitted.ffi_go.contains(alias),
                "{alias}\n{}",
                emitted.ffi_go
            );
        }
    }

    #[test]
    fn nullary_existential_payload_calls_the_private_continuation_with_unit() {
        let package = package(
            "module api; \
             pub type Erased[A] = .; \
             pub newtype Empty <Hidden> : Erased(Hidden) { \
               pub constructor make_empty; pub projector read_empty; \
             };",
        );
        let emitted = lower_package(&package, "pkg").expect("emit nullary existential projector");
        let projector = emitted_method(&emitted.pkg_go, "ReadEmpty");

        assert_eq!(
            projector
                .matches("return kioTailValue(kioApplyType(__kio_member_continuation), Unit{})")
                .count(),
            1,
            "{projector}"
        );
        assert!(
            !projector.contains("return kioTailType(kioApplyType(__kio_member_continuation))"),
            "{projector}"
        );
        assert!(
            projector.contains("ReadEmpty(arg0 Unit, arg1 func() func() Value) Value"),
            "{projector}"
        );
    }

    #[test]
    fn private_value_domains_do_not_change_executable_type_stages() {
        let mut body = super::GoBlock::default();
        body.line("return Unit{}");
        let unit = super::GoClosureGroup::Value(Vec::new()).render(&body);
        let type_stage = super::GoClosureGroup::Type.render(&body);
        let product =
            super::GoClosureGroup::Value(vec!["left".into(), "right".into()]).render(&body);
        assert!(unit.starts_with("func(_ any) any"), "{unit}");
        assert!(type_stage.starts_with("func() any"), "{type_stage}");
        assert!(
            product.starts_with("func(__kio_domain any) any"),
            "{product}"
        );
        assert_eq!(
            product.matches("kioProductSlots(__kio_domain, 2)").count(),
            1
        );
        assert!(product.contains("left := __kio_domain_slots[0]"));
        assert!(product.contains("right := __kio_domain_slots[1]"));
    }

    #[test]
    fn guest_result_transfers_leave_operands_and_host_calls_eager() {
        let package = package(
            "module api; host fn effect(); \
             fn invoke(next: . -> .) { effect(); next() } \
             pub fn forward(next: . -> .) { invoke(next) } \
             pub fn before(next: . -> .) { next(); effect() } \
             pub fn type_tail(next: [A] A -> A) -> . -> . { next(.) }",
        );
        let emitted = lower_package(&package, "pkg").expect("tail contexts emit");
        let forward = emitted_method(&emitted.pkg_go, "kioStep_mod_api_forward");
        assert!(forward.contains("owner: pkg"), "{forward}");
        assert!(
            forward.contains("invoke: kioInvoke_mod_api_invoke"),
            "{forward}"
        );
        let invoke = emitted_method(&emitted.pkg_go, "kioStep_mod_api_invoke");
        assert!(invoke.contains("return kioTailValue("), "{invoke}");
        let before = emitted_method(&emitted.pkg_go, "kioStep_mod_api_before");
        assert!(before.contains("kioApplyValue("), "{before}");
        assert!(!before.contains("kioTailValue("), "{before}");
        assert!(before.contains("pkg.host.Api__effect"), "{before}");
        let type_tail = emitted_method(
            &emitted.pkg_go,
            &format!("kioStep_{}", module_fn_method_name("api", "type_tail")),
        );
        assert!(type_tail.contains("return kioTailType("), "{type_tail}");
        assert!(!type_tail.contains("kioApplyType("), "{type_tail}");
    }

    #[test]
    fn named_tail_packets_use_nongeneric_static_entries_for_every_arity() {
        let package = package(
            "module api; host fn effect(); host type Atom; \
             fn zero() { effect() } \
             fn one(first: Atom) -> Atom { effect(); first } \
             fn two(first: Atom, second: Atom) -> Atom { effect(); second } \
             fn many(first: Atom, second: Atom, third: Atom, fourth: Atom, fifth: Atom) -> Atom { effect(); fifth } \
             pub fn forward_zero() { zero() } \
             pub fn forward_one(first: Atom) -> Atom { one(first) } \
             pub fn forward_two(first: Atom, second: Atom) -> Atom { two(first, second) } \
             pub fn forward_many(first: Atom, second: Atom, third: Atom, fourth: Atom, fifth: Atom) -> Atom { \
               many(first, second, third, fourth, fifth) \
             }",
        );
        let emitted = lower_package(&package, "pkg").expect("named argument layouts emit");
        for (name, arity) in [("zero", 0), ("one", 1), ("two", 2), ("many", 5)] {
            let method = module_fn_method_name("api", name);
            let forward = emitted_method(
                &emitted.pkg_go,
                &format!(
                    "kioStep_{}",
                    module_fn_method_name("api", &format!("forward_{name}"))
                ),
            );
            assert!(
                forward.contains(&format!("invoke: kioInvoke_{method}")),
                "{forward}"
            );
            assert!(!forward.contains("invoke: func("), "{forward}");
            assert!(!forward.contains("domain:"), "{forward}");
            assert_eq!(forward.contains("first:"), arity >= 1, "{forward}");
            assert_eq!(forward.contains("second:"), arity >= 2, "{forward}");
            assert_eq!(forward.contains("rest: []any{"), arity > 2, "{forward}");
            let signature = vec!["any"; arity].join(", ");
            let marker = format!(
                "func kioInvoke_{method}(__kio_owner any, __kio_arguments kioNamedArguments) kioStep"
            );
            let helper = emitted
                .pkg_go
                .split(&marker)
                .nth(1)
                .expect("nongeneric static helper");
            let helper = helper.split("\nfunc ").next().expect("helper body");
            assert!(
                helper.contains(&format!(
                    "interface {{ kioStep_{method}({signature}) kioStep }}"
                )),
                "{helper}"
            );
            assert!(!helper.contains("Package["), "{helper}");
            let expected = (0..arity)
                .map(|index| match index {
                    0 => "__kio_arguments.first".to_owned(),
                    1 => "__kio_arguments.second".to_owned(),
                    _ => format!("__kio_arguments.rest[{}]", index - 2),
                })
                .collect::<Vec<_>>()
                .join(", ");
            assert!(
                helper.contains(&format!(".kioStep_{method}({expected})")),
                "{helper}"
            );
            let body = emitted_method(&emitted.pkg_go, &format!("kioStep_{method}"));
            assert!(!body.contains("kioProductSlots"), "{body}");
        }
    }
}

impl EmitError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        EmitError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Emitted Go package — the per-package source files plus the
/// `kio_runtime.go` runtime-support file. Build-side code writes each
/// into the output directory; every emitted file declares the
/// package's namespace as its `package` clause.
///
/// The runtime-support file's content is canonical up to that clause
/// (see [`super::runtime_support_content`]); shipping it on the
/// `GoPackage` keeps the build-side dispatcher's write step uniform —
/// it reads each field and writes one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPackage {
    /// `pkg.go` — the package's invocable surface: the package-handle
    /// struct, the `New` factory, and every module fn as a
    /// method.
    pub pkg_go: String,
    /// `host.go` — the `Host` interface (one method per
    /// `host fn`).
    pub host_go: String,
    /// `shapes.go` — facade-owned generic product/sum declarations and
    /// concrete nominal carriers reachable from live or retained roots.
    pub shapes_go: String,
    /// `ffi.go` — per-member boundary-type aliases naming the Go type at
    /// every host-fn / export-fn boundary slot, so a host implementation
    /// can reach a shape type by alias rather than re-deriving its
    /// spelling. Mirrors Rust's `src/ffi.rs`.
    pub ffi_go: String,
    /// The runtime-support file content (always the canonical
    /// [`super::runtime_support_content`]).
    pub runtime_go: String,
}

/// Fixed facade roles qualified by the emitted package's namespace.
pub(crate) struct GoNames {
    pub ns: String,
    pub handle: String,
    pub host_ty: String,
    pub factory: String,
}

impl GoNames {
    pub(crate) fn derive(ns: &str) -> GoNames {
        GoNames {
            ns: ns.to_owned(),
            host_ty: "Host".to_owned(),
            factory: "New".to_owned(),
            handle: "Package".to_owned(),
        }
    }

    /// The `*<Handle>` receiver / return type spelling.
    fn handle_ptr(&self, type_arguments: &str) -> String {
        format!("*{}{type_arguments}", self.handle)
    }
}

/// Lower a typed (post-recovery) Kio package to a Go package whose
/// `package` clause is `ns`.
pub fn lower_package(package: &Package<Routed>, ns: &str) -> Result<GoPackage, EmitError> {
    lower_package_with_signature(package, ns, None)
}

/// Lower one package while retaining plan-only facade declarations and stable
/// aliases from an older replayed interface. Removed sites never acquire a
/// live callable capability and therefore cannot emit a host method, export,
/// package handle, or body call.
pub fn lower_package_with_signature(
    package: &Package<Routed>,
    ns: &str,
    sig: Option<&(u32, crate::sig::ReplayedInterface)>,
) -> Result<GoPackage, EmitError> {
    package.package_file().ok_or_else(|| {
        EmitError::unsupported(
            "Go emitter requires a package file (`<pkg>.pkg.kio`); \
             a package without one has no package boundary to expose",
        )
    })?;

    let names = GoNames::derive(ns);
    let host_desc = host_descriptor::build_host_descriptor(package);
    let roles = host_role_table(&host_desc);

    // Keep the owner and borrowing realization adjacent. Collection and Go
    // preparation each occur exactly once; every public-boundary consumer
    // below borrows this one catalog.
    let prepared =
        PreparedBoundaryCallableSites::collect(package, sig.map(|(_, replayed)| replayed))
            .unwrap_or_else(|error| {
                unreachable!(
                    "typed Go emission must produce a valid PreparedBoundaryCallableSites \
                     catalog: {error}"
                )
            });
    let facades = GoFacadeCatalog::prepare(&prepared).unwrap_or_else(go_facade_invariant);

    let export_tree = collect_facade_export_namespaces(&facades)?;
    let aliases = render_ffi_go(&facades, &names)?;
    validate_package_names(package, &facades, &export_tree, &aliases, &names)?;

    let host_go = render_host_interface(package, &facades, &names)?;
    let pkg_go = render_facade_pkg_go(package, &roles, &facades, &export_tree, &names)?;
    let shape_declarations = facades
        .render_declarations_with_docs(
            |binding| {
                render_doc(
                    crate::backends::boundary_docs::host_type(package, binding),
                    "",
                )
            },
            |entry| render_doc(crate::backends::boundary_docs::newtype(package, entry), ""),
        )
        .unwrap_or_else(go_facade_invariant);
    let shapes_go = format!(
        "// Generated by kio — do not edit by hand.\n\npackage {}\n\n{shape_declarations}",
        names.ns
    );
    let ffi_go = aliases.source;

    Ok(GoPackage {
        pkg_go: finalize_go_file(pkg_go, ns),
        host_go: finalize_go_file(host_go, ns),
        shapes_go: finalize_go_file(shapes_go, ns),
        ffi_go: finalize_go_file(ffi_go, ns),
        runtime_go: super::runtime_support_content(ns),
    })
}

/// Normalize an emitted Go file: inject the `math/big` import when the
/// file references `big.` (the `i128` / `u128` roles map to
/// `*big.Int`), and apply gofmt's trailing-whitespace rule (strip
/// trailing blank lines, end with exactly one newline). The emitter
/// aligns struct fields itself and uses tabs throughout, so the only
/// other gofmt drift the emit can introduce is trailing blank lines from
/// per-item `\n\n` spacers at the file tail.
fn finalize_go_file(mut s: String, ns: &str) -> String {
    if references_big(&s) {
        s = inject_import(s, "math/big", ns);
    }
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

/// True when the file uses a `math/big` symbol (`big.Int`, `big.NewInt`,
/// …). Matched on the `big.` qualifier, which only the emitted
/// `*big.Int` mapping produces.
fn references_big(s: &str) -> bool {
    s.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| line.contains("big."))
}

fn render_doc(doc: Option<&crate::ast::DocComment>, indent: &str) -> String {
    let mut out = String::new();
    if let Some(doc) = doc {
        let body = crate::backends::doc::render(doc, crate::backends::doc::Dialect::Godoc);
        crate::backends::doc::push_line_comments(&mut out, &body, indent, "//");
    }
    out
}

/// Insert `import "<path>"` immediately after the `package <name>` line.
/// The emitted files have no other imports, so a single-line import is
/// sufficient.
fn inject_import(s: String, path: &str, ns: &str) -> String {
    let marker = format!("package {ns}\n");
    if let Some(pos) = s.find(&marker) {
        let insert_at = pos + marker.len();
        let mut out = String::with_capacity(s.len() + path.len() + 12);
        out.push_str(&s[..insert_at]);
        out.push_str(&format!("\nimport \"{path}\"\n"));
        out.push_str(&s[insert_at..]);
        out
    } else {
        s
    }
}

/// The injective Go boundary member name for a `host fn` / `host type`.
/// Paths without source underscores keep the readable `/` -> `_` form.
/// Paths containing underscores use `KioItem_` followed by an encoding in
/// which `_` is `_u` and `/` is `_s`; `__` then separates the module from
/// the word-cased leaf. The internal uppercase letters in `KioItem_` are
/// outside the image of the readable mapping, whose source names are
/// lowercase, and make the fallback Go-exported without losing its reserved
/// identity. The encoded module contains no `__`, so the separator makes the
/// original module/leaf pair recoverable.
fn host_member_name(module_path: &str, leaf: &str) -> String {
    qualified_boundary_member_name(module_path, None, leaf)
}

fn host_site_id(module_path: &str, name: &str) -> BoundaryFacadeSiteId {
    BoundaryFacadeSiteId::new(
        module_path.split('/').map(str::to_owned).collect(),
        BoundaryFacadeSiteOwner::HostFunction {
            name: name.to_owned(),
        },
    )
    .unwrap_or_else(|| {
        unreachable!(
            "Routed host declaration `{module_path}.{name}` must have a valid structured facade \
             identity"
        )
    })
}

/// The injective boundary name for an item owned by `module_path` and,
/// optionally, a newtype `qualifier`.
///
/// The readable form flattens a slash-only module path, then keeps the
/// qualifier and leaf as distinct components:
/// `api` + `A` + `b_c` → `Api__A_bC`. If either owner component contains
/// a source underscore, the reserved form encodes the module path and
/// qualifier separately before the word-cased leaf:
/// `api` + `A_b` + `c` → `KioItem_api__AB__c`. The encoded owner
/// components contain no `__`, so every source boundary remains recoverable.
fn qualified_boundary_member_name(
    module_path: &str,
    qualifier: Option<&str>,
    leaf: &str,
) -> String {
    let leaf = crate::backends::public_names::host_name_core(leaf);
    let needs_exact = module_path.contains('_') || qualifier.is_some_and(|q| q.contains('_'));
    if !needs_exact {
        let flat_module = module_path.replace('/', "_");
        return match qualifier {
            Some(q) => format!("{}__{q}_{leaf}", capitalize_first(&flat_module)),
            None => format!("{}__{leaf}", capitalize_first(&flat_module)),
        };
    }

    let mut exact_owner = encode_item_module(module_path);
    if let Some(q) = qualifier {
        exact_owner.push_str("__");
        exact_owner.push_str(&encode_item_module(q));
    }
    format!("KioItem_{exact_owner}__{leaf}")
}

fn encode_item_module(module_path: &str) -> String {
    crate::backends::public_names::encode_host_identity(module_path)
}

/// Capitalize the first character of `s` (ASCII), leaving the rest
/// unchanged. Used to make a mangled boundary name Go-exported.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Complete stable alias surface. Every value slot and return receives its
/// `Env_`/`Exp_` root alias, including primitive and erased types. Forall results
/// recurse through `_result`; transparent Both newtypes retain the prefix. Function
/// legs recurse through `_cbargN`/`_cbret`, and sums delegate their positional
/// cases and constructors to the facade-owned exact contextual alias. Product
/// fields recurse through `_fieldN`; a sum arm payload recurses through
/// `_<N>_value`, so every nested contextual row remains constructible.
struct FfiAliases {
    source: String,
    package_names: Vec<String>,
}

#[derive(Clone)]
struct GoFfiAliasRootParameters {
    declaration: String,
    arguments: String,
    selected: bool,
}

impl GoFfiAliasRootParameters {
    fn with_index(&self, name: &str, constraint: &str) -> Self {
        Self {
            declaration: super::facade::append_type_parameters(
                &self.declaration,
                &[format!("{name} {constraint}")],
            ),
            arguments: super::facade::append_type_parameters(&self.arguments, &[name.to_owned()]),
            selected: true,
        }
    }

    fn selecting_declaration(&self, name: &str, constraint: &str) -> String {
        if self.declaration.is_empty() {
            format!("[{name} {constraint}]")
        } else {
            format!(
                "[{name} {constraint}, {}]",
                &self.declaration[1..self.declaration.len() - 1]
            )
        }
    }
}

fn ffi_site_parameters(
    facades: &GoFacadeCatalog<'_>,
    site: &super::facade::GoFacadeSite<'_>,
) -> GoFfiAliasRootParameters {
    let selected = !matches!(
        site.id().owner(),
        BoundaryFacadeSiteOwner::HostFunction { .. }
    );
    let entry = site.callable_entry().unwrap_or_else(go_facade_invariant);
    let mut declarations = Vec::new();
    let mut arguments = Vec::new();
    if selected {
        for stage in entry.stages().iter().filter_map(|stage| stage.type_stage()) {
            let name = format!("KioI{}", arguments.len());
            declarations.push(format!("{name} {}", stage.index_constraint()));
            arguments.push(name);
        }
    }
    GoFfiAliasRootParameters {
        declaration: super::facade::append_type_parameters(
            &facades.host_binding_declaration(),
            &declarations,
        ),
        arguments: super::facade::append_type_parameters(
            &facades.host_binding_arguments(),
            &arguments,
        ),
        selected: !arguments.is_empty(),
    }
}

fn render_ffi_go(facades: &GoFacadeCatalog<'_>, names: &GoNames) -> Result<FfiAliases, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n\n");
    out.push_str(&format!("package {}\n", names.ns));
    let mut package_names = Vec::new();
    let mut visited = BTreeMap::new();
    let root_parameters = GoFfiAliasRootParameters {
        declaration: facades.host_binding_declaration(),
        arguments: facades.host_binding_arguments(),
        selected: false,
    };
    for site in facades.sites() {
        let root_parameters = ffi_site_parameters(facades, site);
        let prefix = ffi_site_prefix(site.id());
        let removed_at_version = site.removed_at_version();
        match site.id().owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. }
            | BoundaryFacadeSiteOwner::ExportedFunction { .. } => {
                let entry = site
                    .callable_entry_with_indices(root_parameters.selected)
                    .unwrap_or_else(go_facade_invariant);
                let mut argument = 0usize;
                for stage in entry.stages() {
                    let Some(value) = stage.value_stage() else {
                        continue;
                    };
                    for parameter in value
                        .public_parameters()
                        .unwrap_or_else(go_facade_invariant)
                    {
                        emit_ffi_parameter_alias(
                            &format!("{prefix}_arg{argument}"),
                            &parameter,
                            &root_parameters,
                            &mut out,
                            &mut package_names,
                            &mut visited,
                            removed_at_version,
                        )?;
                        argument += 1;
                    }
                }
                emit_ffi_alias_tree(
                    &format!("{prefix}_ret"),
                    entry.returned(),
                    &root_parameters,
                    &mut out,
                    &mut package_names,
                    &mut visited,
                    removed_at_version,
                )?;
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
            | BoundaryFacadeSiteOwner::NewtypeProjector { .. } => {
                let live = site.live().unwrap_or_else(|| {
                    unreachable!(
                        "PreparedBoundaryCallableSites gives every public newtype member a live \
                         execution capability"
                    )
                });
                let member = (if root_parameters.selected {
                    live.selected_newtype_member_entry()
                } else {
                    live.newtype_member_entry()
                })
                .unwrap_or_else(go_facade_invariant);
                for (argument, parameter) in member
                    .public_parameters()
                    .unwrap_or_else(go_facade_invariant)
                    .iter()
                    .enumerate()
                {
                    emit_ffi_parameter_alias(
                        &format!("{prefix}_arg{argument}"),
                        parameter,
                        &root_parameters,
                        &mut out,
                        &mut package_names,
                        &mut visited,
                        removed_at_version,
                    )?;
                }
                emit_ffi_alias_tree(
                    &format!("{prefix}_ret"),
                    member.result().semantic(),
                    &root_parameters,
                    &mut out,
                    &mut package_names,
                    &mut visited,
                    removed_at_version,
                )?;
            }
        }
    }
    let mut views = GoValueViewRenderer {
        source: String::new(),
        package_names: Vec::new(),
        visited: BTreeMap::new(),
        handle_type: names.handle_ptr(&root_parameters.arguments),
        host_type: format!("{}{}", names.host_ty, root_parameters.arguments),
        host_parameters: root_parameters.clone(),
        root_parameters,
    };
    for preparation in facades
        .host_preparations()
        .unwrap_or_else(go_facade_invariant)
    {
        let outer = views.root_parameters.clone();
        for (name, constraint) in &preparation.indices {
            views.root_parameters = views.root_parameters.with_index(name, constraint);
        }
        views.render(
            &preparation.spelling,
            preparation.boundary_type.to_string(),
            facade_skin::GoValueView::Host(&preparation),
            &[],
        );
        views.root_parameters = outer;
    }
    for live in facades.live_sites() {
        views.root_parameters = ffi_site_parameters(facades, live.site());
        let prefix = ffi_site_prefix(live.site().id());
        match live.site().id().owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. }
            | BoundaryFacadeSiteOwner::ExportedFunction { .. } => {
                let entry = (if views.root_parameters.selected {
                    live.selected_callable_entry()
                } else {
                    live.callable_entry()
                })
                .unwrap_or_else(go_facade_invariant);
                views.visit_entry_arguments(&prefix, &entry)?;
                views.visit_value(&format!("{prefix}_ret"), entry.returned())?;
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
            | BoundaryFacadeSiteOwner::NewtypeProjector { .. } => {
                let member = (if views.root_parameters.selected {
                    live.selected_newtype_member_entry()
                } else {
                    live.newtype_member_entry()
                })
                .unwrap_or_else(go_facade_invariant);
                let count = views.visit_entry_arguments(&prefix, member.entry())?;
                if let GoLiveNewtypeMemberEntry::ExistentialProjector(projector) = &member {
                    views.visit_value(&format!("{prefix}_arg{count}"), projector.continuation())?;
                }
                views.visit_value(&format!("{prefix}_ret"), member.result())?;
            }
        }
    }
    out.push_str(&views.source);
    package_names.extend(views.package_names);
    Ok(FfiAliases {
        source: out,
        package_names,
    })
}

/// Native Value preparation/recovery follows the same live, contextual
/// facade topology as ordinary calls. Opaque applications expose views for
/// their static arguments, never an operation that traverses their storage.
struct GoValueViewRenderer {
    source: String,
    package_names: Vec<String>,
    visited: BTreeMap<String, String>,
    handle_type: String,
    host_type: String,
    host_parameters: GoFfiAliasRootParameters,
    root_parameters: GoFfiAliasRootParameters,
}

impl GoValueViewRenderer {
    fn render(
        &mut self,
        spelling: &str,
        ty: String,
        view: facade_skin::GoValueView<'_, '_, '_>,
        children: &[facade_skin::GoValueViewChild<'_, '_>],
    ) -> String {
        let mut parameters = self.host_parameters.clone();
        for (name, kind) in view.index_parameters().unwrap_or_else(go_skin_invariant) {
            parameters = parameters.with_index(&name, &kind);
        }
        if let Some(previous) = self.visited.insert(spelling.to_owned(), ty.clone()) {
            assert_eq!(previous, ty, "one stable Go Value view has one exact type");
            return parameters.arguments;
        }
        let (source, has_preparation) = facade_skin::render_value_view(
            view,
            children,
            spelling,
            &self.handle_type,
            &parameters.declaration,
            &parameters.arguments,
        )
        .unwrap_or_else(go_skin_invariant);
        self.source.push_str(&source);
        self.package_names
            .push(format!("kioValue{spelling}IntoBody"));
        self.package_names
            .push(format!("kioValue{spelling}FromBody"));
        self.package_names
            .extend([format!("Value{spelling}"), format!("Native{spelling}")]);
        if has_preparation {
            self.package_names
                .extend([format!("Prepare{spelling}"), format!("Unprepare{spelling}")]);
        }
        parameters.arguments
    }

    fn visit_entry_arguments(
        &mut self,
        prefix: &str,
        entry: &GoLiveCallableEntry<'_, '_>,
    ) -> Result<usize, EmitError> {
        let mut count = 0;
        for stage in entry.stages() {
            let Some(stage) = stage.value_stage() else {
                continue;
            };
            let parameters = stage
                .public_parameters()
                .unwrap_or_else(go_facade_invariant);
            let mut parameters = parameters.iter();
            for group in stage.source_groups().unwrap_or_else(go_facade_invariant) {
                let fields = match group.view() {
                    GoLiveValueSourceGroupView::UnitValue => continue,
                    GoLiveValueSourceGroupView::Identity(value) => std::slice::from_ref(value),
                    GoLiveValueSourceGroupView::RightNest(fields) => fields,
                };
                let parameter = parameters
                    .next()
                    .expect("each non-unit source group owns one public parameter");
                self.visit_parameter(&format!("{prefix}_arg{count}"), parameter, fields)?;
                count += 1;
            }
            assert!(parameters.next().is_none());
        }
        Ok(count)
    }

    fn visit_parameter<'site, 'source>(
        &mut self,
        spelling: &str,
        parameter: &GoPublicParameter<'site, 'source>,
        fields: &[GoLiveFacadeUseRef<'site, 'source>],
    ) -> Result<Vec<facade_skin::GoValueViewChild<'site, 'source>>, EmitError> {
        match parameter {
            GoPublicParameter::Value(_) => {
                assert_eq!(fields.len(), 1);
                Ok(self
                    .visit_value(spelling, &fields[0])?
                    .into_iter()
                    .collect())
            }
            GoPublicParameter::Product(product) => {
                let mut children = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    children.extend(self.visit_value(&format!("{spelling}_field{index}"), field)?);
                }
                self.render(
                    spelling,
                    product.boundary_type().to_string(),
                    facade_skin::GoValueView::Product(product, fields),
                    &children,
                );
                Ok(children)
            }
        }
    }

    fn visit_value<'site, 'source>(
        &mut self,
        spelling: &str,
        value: &GoLiveFacadeUseRef<'site, 'source>,
    ) -> Result<Option<facade_skin::GoValueViewChild<'site, 'source>>, EmitError> {
        let mut contextual = value.clone();
        let view = loop {
            match contextual.view().unwrap_or_else(go_facade_invariant) {
                GoLiveFacadeUseView::Transparent { payload } => contextual = payload,
                view => break view,
            }
        };
        match &view {
            GoLiveFacadeUseView::Bottom | GoLiveFacadeUseView::Erased { .. } => return Ok(None),
            _ => {}
        }
        let mut children = Vec::new();
        match view {
            GoLiveFacadeUseView::Product { fields, .. } => {
                for (index, field) in fields.iter().enumerate() {
                    children.extend(self.visit_value(&format!("{spelling}_field{index}"), field)?);
                }
            }
            GoLiveFacadeUseView::Sum { arms, .. } => {
                for (index, arm) in arms.iter().enumerate() {
                    children.extend(self.visit_value(&format!("{spelling}_{index}_value"), arm)?);
                }
            }
            GoLiveFacadeUseView::HostCarrier { arguments, .. } => {
                for (index, argument) in arguments.iter().enumerate() {
                    children
                        .extend(self.visit_value(&format!("{spelling}_typearg{index}"), argument)?);
                }
            }
            GoLiveFacadeUseView::Forall { result, .. } => {
                if let Some((parameter, constraint, selected)) = contextual
                    .select_forall()
                    .unwrap_or_else(go_facade_invariant)
                {
                    let outer = self.root_parameters.clone();
                    let mut captures = self.host_parameters.clone();
                    for (name, kind) in contextual
                        .semantic()
                        .index_parameters()
                        .unwrap_or_else(go_facade_invariant)
                    {
                        captures = captures.with_index(&name, &kind);
                    }
                    self.root_parameters = captures.with_index(parameter.as_str(), &constraint);
                    let selected_child =
                        self.visit_value(&format!("{spelling}_selected"), &selected)?;
                    let selected_arguments = selected_child
                        .as_ref()
                        .map(|child| child.type_arguments.as_str());
                    self.root_parameters = outer;
                    self.source.push_str(
                        &facade_skin::render_type_stage_helpers(
                            &contextual,
                            &selected,
                            spelling,
                            &self.host_type,
                            &captures.declaration,
                            &captures.selecting_declaration(parameter.as_str(), &constraint),
                            selected_arguments,
                        )
                        .unwrap_or_else(go_skin_invariant),
                    );
                    self.package_names
                        .extend([format!("New{spelling}Stage"), format!("Select{spelling}")]);
                } else {
                    children.extend(self.visit_value(&format!("{spelling}_result"), &result)?);
                }
            }
            GoLiveFacadeUseView::Function {
                semantic,
                layout,
                slots,
                result,
            } => {
                let parameters = semantic
                    .public_parameters()
                    .unwrap_or_else(go_facade_invariant);
                let mut parameters = parameters.iter().enumerate();
                for group in layout.source_params() {
                    if group.adapter() == CallableSourceParamAdapter::UnitValue {
                        continue;
                    }
                    let (index, parameter) = parameters
                        .next()
                        .expect("each non-unit function group owns one public parameter");
                    children.extend(self.visit_parameter(
                        &format!("{spelling}_cbarg{index}"),
                        parameter,
                        &slots[group.facade_slots()],
                    )?);
                }
                assert!(parameters.next().is_none());
                children.extend(self.visit_value(&format!("{spelling}_cbret"), &result)?);
            }
            _ => {}
        }
        let type_arguments = self.render(
            spelling,
            value
                .boundary_type()
                .unwrap_or_else(go_facade_invariant)
                .to_string(),
            facade_skin::GoValueView::Value(value),
            &children,
        );
        Ok(Some(facade_skin::GoValueViewChild {
            cursor: value.clone(),
            spelling: spelling.to_owned(),
            type_arguments,
        }))
    }
}

fn ffi_site_prefix(site: &BoundaryFacadeSiteId) -> String {
    let module = site.module_segments().join("/");
    match site.owner() {
        BoundaryFacadeSiteOwner::HostFunction { name } => {
            format!("Env_{}", host_member_name(&module, name))
        }
        BoundaryFacadeSiteOwner::ExportedFunction { name } => {
            format!("Exp_{}", host_member_name(&module, name))
        }
        BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
        | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => format!(
            "Exp_{}",
            qualified_boundary_member_name(&module, Some(newtype), member)
        ),
    }
}

fn emit_ffi_parameter_alias(
    spelling: &str,
    parameter: &GoPublicParameter<'_, '_>,
    root_parameters: &GoFfiAliasRootParameters,
    out: &mut String,
    package_names: &mut Vec<String>,
    visited: &mut BTreeMap<String, String>,
    removed_at_version: Option<u32>,
) -> Result<(), EmitError> {
    match parameter {
        GoPublicParameter::Value(cursor) => emit_ffi_alias_tree(
            spelling,
            cursor,
            root_parameters,
            out,
            package_names,
            visited,
            removed_at_version,
        ),
        GoPublicParameter::Product(product) => {
            if let Some(index) = product.index_type().unwrap_or_else(go_facade_invariant) {
                if let Some(version) = removed_at_version {
                    out.push_str(&format!("\n// Deprecated: retained only for source compatibility with host declarations removed at signature v{version}.\n"));
                }
                out.push_str(&format!(
                    "type Index{spelling}{} = {index}\n",
                    root_parameters.declaration
                ));
                package_names.push(format!("Index{spelling}"));
            }
            out.push('\n');
            if let Some(version) = removed_at_version {
                out.push_str(&format!(
                    "// Deprecated: retained only for source compatibility with host declarations removed at signature v{version}.\n"
                ));
            }
            out.push_str(&format!(
                "type {spelling}{} = {}\n",
                root_parameters.declaration,
                product.boundary_type()
            ));
            package_names.push(spelling.to_owned());
            for (index, field) in product.fields().iter().enumerate() {
                emit_ffi_alias_tree(
                    &format!("{spelling}_field{index}"),
                    field.payload(),
                    root_parameters,
                    out,
                    package_names,
                    visited,
                    removed_at_version,
                )?;
            }
            Ok(())
        }
    }
}

fn emit_ffi_alias_tree(
    spelling: &str,
    cursor: &GoFacadeUseRef<'_, '_>,
    root_parameters: &GoFfiAliasRootParameters,
    out: &mut String,
    package_names: &mut Vec<String>,
    visited: &mut BTreeMap<String, String>,
    removed_at_version: Option<u32>,
) -> Result<(), EmitError> {
    let alias = GoIdentifier::new(spelling.to_owned()).unwrap_or_else(|error| {
        unreachable!(
            "the fixed Go FFI alias codec must produce a valid identifier for `{spelling}`: \
             {error}"
        )
    });
    if let Some(index) = cursor
        .index_alias_type()
        .unwrap_or_else(go_facade_invariant)
    {
        let spelling = format!("Index{alias}");
        if let Some(previous) = visited.insert(spelling.clone(), index.to_string()) {
            assert_eq!(
                previous,
                index.to_string(),
                "one boundary index alias has one semantic index"
            );
        } else {
            if let Some(version) = removed_at_version {
                out.push_str(&format!("\n// Deprecated: retained only for source compatibility with host declarations removed at signature v{version}.\n"));
            }
            out.push_str(&format!(
                "type {spelling}{} = {index}\n",
                root_parameters.declaration
            ));
            package_names.push(spelling);
        }
    }
    let mut contextual = cursor.clone();
    let view = loop {
        match contextual.view().unwrap_or_else(go_facade_invariant) {
            GoFacadeUseView::Transparent { payload } => contextual = payload,
            view => break view,
        }
    };
    let cursor = &contextual;
    match view {
        GoFacadeUseView::Sum(sum) => {
            let context = format!("{:?}|{}", cursor.source(), sum.boundary_type());
            if let Some(previous) = visited.insert(spelling.to_owned(), context.clone()) {
                if previous == context {
                    return Ok(());
                }
                unreachable!(
                    "the prepared facade assigned stable Go FFI alias `{spelling}` to two \
                     contextual uses"
                );
            }
            let exact = sum
                .exact_alias(
                    &alias,
                    &root_parameters.declaration,
                    &root_parameters.arguments,
                    removed_at_version,
                )
                .unwrap_or_else(go_facade_invariant);
            out.push('\n');
            out.push_str(exact.declaration());
            package_names.extend(
                exact
                    .claims()
                    .iter()
                    .filter(|(scope, _, _)| matches!(scope, GoFacadeScope::Package))
                    .map(|(_, name, _)| name.as_str().to_owned()),
            );
            for arm in sum.arms() {
                emit_ffi_alias_tree(
                    &format!("{alias}_{}_value", arm.index()),
                    arm.payload(),
                    root_parameters,
                    out,
                    package_names,
                    visited,
                    removed_at_version,
                )?;
            }
        }
        GoFacadeUseView::ExactHost { .. } => {
            // A declaration such as `type Slot[T any] = T` is rejected by
            // Go. The exact public spelling is already either the live
            // package-root type parameter or the retained deprecated nominal,
            // so emitting an erased replacement would weaken that identity.
        }
        view => {
            let boundary_type = cursor.boundary_type().unwrap_or_else(go_facade_invariant);
            let context = format!("{:?}|{boundary_type}", cursor.source());
            if let Some(previous) = visited.insert(spelling.to_owned(), context.clone()) {
                if previous == context {
                    return Ok(());
                }
                unreachable!(
                    "the prepared facade assigned stable Go FFI alias `{spelling}` to two \
                     contextual uses"
                );
            }
            out.push('\n');
            if let Some(version) = removed_at_version {
                out.push_str(&format!(
                    "// Deprecated: retained only for source compatibility with host declarations removed at signature v{version}.\n"
                ));
            }
            let declaration = &root_parameters.declaration;
            out.push_str(&format!("type {alias}{declaration} = {boundary_type}\n"));
            package_names.push(alias.as_str().to_owned());
            match view {
                GoFacadeUseView::HostCarrier { arguments, .. } => {
                    for (index, argument) in arguments.iter().enumerate() {
                        emit_ffi_alias_tree(
                            &format!("{alias}_typearg{index}"),
                            argument,
                            root_parameters,
                            out,
                            package_names,
                            visited,
                            removed_at_version,
                        )?;
                    }
                }
                GoFacadeUseView::Forall(forall) => {
                    emit_ffi_alias_tree(
                        &format!("{alias}_result"),
                        forall.result(),
                        root_parameters,
                        out,
                        package_names,
                        visited,
                        removed_at_version,
                    )?;
                    if let Some((parameter, constraint, selected)) = forall.selection() {
                        emit_ffi_alias_tree(
                            &format!("{alias}_selected"),
                            selected,
                            &root_parameters.with_index(parameter.as_str(), &constraint),
                            out,
                            package_names,
                            visited,
                            removed_at_version,
                        )?;
                    }
                }
                GoFacadeUseView::Function(function) => {
                    for (index, parameter) in function
                        .public_parameters()
                        .unwrap_or_else(go_facade_invariant)
                        .iter()
                        .enumerate()
                    {
                        emit_ffi_parameter_alias(
                            &format!("{alias}_cbarg{index}"),
                            parameter,
                            root_parameters,
                            out,
                            package_names,
                            visited,
                            removed_at_version,
                        )?;
                    }
                    emit_ffi_alias_tree(
                        &format!("{alias}_cbret"),
                        function.result(),
                        root_parameters,
                        out,
                        package_names,
                        visited,
                        removed_at_version,
                    )?;
                }
                GoFacadeUseView::Product(product) => {
                    for (index, field) in product.fields().iter().enumerate() {
                        emit_ffi_alias_tree(
                            &format!("{alias}_field{index}"),
                            field.payload(),
                            root_parameters,
                            out,
                            package_names,
                            visited,
                            removed_at_version,
                        )?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn render_host_interface(
    package: &Package<Routed>,
    facades: &GoFacadeCatalog<'_>,
    names: &GoNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n\n");
    out.push_str(&format!("package {}\n\n", names.ns));
    out.push_str(&format!(
        "// {} is the host record contract. Implement it to invoke the\n\
         // package. See `specs/backends/go.md` § Host record contract.\n",
        names.host_ty
    ));
    out.push_str(&format!(
        "type {}{} interface {{\n",
        names.host_ty,
        facades.host_binding_declaration()
    ));
    for adapter in facades.role_adapters() {
        out.push_str(&format!(
            "\t{}(value {}) {}\n\t{}(value {}) {}\n",
            adapter.convert_in(),
            adapter.boundary_type(),
            adapter.internal_type(),
            adapter.convert_out(),
            adapter.internal_type(),
            adapter.boundary_type(),
        ));
    }
    for live in facades.live_sites() {
        let BoundaryFacadeSiteOwner::HostFunction { name } = live.site().id().owner() else {
            continue;
        };
        let module = live.site().id().module_segments().join("/");
        let method = host_member_name(&module, name);
        let entry = live.callable_entry().unwrap_or_else(go_facade_invariant);
        let mut params = Vec::new();
        for stage in entry.stages() {
            let Some(value) = stage.value_stage() else {
                continue;
            };
            for slot in value
                .public_parameters()
                .unwrap_or_else(go_facade_invariant)
            {
                let index = params.len();
                params.push(format!(
                    "arg{index} {}",
                    slot.boundary_type().unwrap_or_else(go_facade_invariant)
                ));
            }
        }
        let returned = entry
            .returned()
            .boundary_type()
            .unwrap_or_else(go_facade_invariant);
        let return_clause = if matches!(
            entry.returned().view().unwrap_or_else(go_facade_invariant),
            super::facade::GoLiveFacadeUseView::Unit
        ) {
            String::new()
        } else {
            format!(" {returned}")
        };
        out.push_str(&render_doc(
            crate::backends::boundary_docs::callable(package, live.site().prepared()),
            "\t",
        ));
        out.push_str(&format!(
            "\t{method}({}){return_clause}\n",
            params.join(", ")
        ));
    }
    out.push_str("}\n");
    Ok(out)
}

fn go_facade_invariant<T>(error: super::facade::GoFacadeError) -> T {
    unreachable!("GoFacadeCatalog realization violated its prepared-plan invariant: {error}")
}

/// The Go primitive a `role(r)` host type maps to at the FFI boundary.
/// `i128` / `u128` route through `math/big.Int` (a pointer, since
/// `big.Int` is a mutable struct), which is standard-library and so not
/// a per-backend carve-out (see `specs/backends/go.md` § Atomic types).
pub fn role_to_go_type(role: Role) -> &'static str {
    match role {
        Role::I8 => "int8",
        Role::I16 => "int16",
        Role::I32 => "int32",
        Role::I64 => "int64",
        Role::I128 => "*big.Int",
        Role::U8 => "uint8",
        Role::U16 => "uint16",
        Role::U32 => "uint32",
        Role::U64 => "uint64",
        Role::U128 => "*big.Int",
        Role::F32 => "float32",
        Role::F64 => "float64",
        Role::Bool => "bool",
        Role::Str => "string",
    }
}

#[derive(Default)]
struct FacadeExportTree {
    children: BTreeMap<ExportSelector, FacadeExportTree>,
    exports: Vec<FacadeExportEntry>,
}

struct FacadeExportEntry {
    leaf: String,
    site: BoundaryFacadeSiteId,
}

fn collect_facade_export_namespaces(
    facades: &GoFacadeCatalog<'_>,
) -> Result<FacadeExportTree, EmitError> {
    let mut root = FacadeExportTree::default();

    // Inventory entries create type handles even when the surface is opaque
    // and therefore has no callable site.
    for public in facades.public_newtypes() {
        let module = public.name().module_segments().join("/");
        facade_export_ns_node(&mut root, &module)
            .children
            .entry(ExportSelector::Type(public.name().name().to_owned()))
            .or_default();
    }

    for live in facades.live_sites() {
        let site = live.site().id();
        let module = site.module_segments().join("/");
        match site.owner() {
            BoundaryFacadeSiteOwner::HostFunction { .. } => {}
            BoundaryFacadeSiteOwner::ExportedFunction { name } => {
                facade_export_ns_node(&mut root, &module)
                    .exports
                    .push(FacadeExportEntry {
                        leaf: name.clone(),
                        site: site.clone(),
                    });
            }
            BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, member }
            | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, member } => {
                facade_export_ns_node(&mut root, &module)
                    .children
                    .entry(ExportSelector::Type(newtype.clone()))
                    .or_default()
                    .exports
                    .push(FacadeExportEntry {
                        leaf: member.clone(),
                        site: site.clone(),
                    });
            }
        }
    }

    fn normalize(node: &mut FacadeExportTree) -> Result<(), EmitError> {
        node.exports
            .sort_by(|left, right| left.site.cmp(&right.site));
        for pair in node.exports.windows(2) {
            if pair[0].leaf == pair[1].leaf {
                unreachable!(
                    "PreparedBoundaryCallableSites assigned Go export selector `{}` to two live \
                     sites",
                    pair[0].leaf
                );
            }
        }
        for child in node.children.values_mut() {
            normalize(child)?;
        }
        Ok(())
    }
    normalize(&mut root)?;
    Ok(root)
}

fn facade_export_ns_node<'a>(
    root: &'a mut FacadeExportTree,
    module: &str,
) -> &'a mut FacadeExportTree {
    let mut node = root;
    for segment in module.split('/') {
        node = node
            .children
            .entry(ExportSelector::Module(segment.to_owned()))
            .or_default();
    }
    node
}

fn validate_package_names(
    package: &Package<Routed>,
    facades: &GoFacadeCatalog<'_>,
    exports: &FacadeExportTree,
    aliases: &FfiAliases,
    names: &GoNames,
) -> Result<(), EmitError> {
    let mut claims = BTreeMap::<(String, String), String>::new();
    let mut claim = |scope: String, name: String, owner: String| -> Result<(), EmitError> {
        let key = (scope.clone(), name.clone());
        if let Some(previous) = claims.insert(key, owner.clone())
            && previous != owner
        {
            unreachable!(
                "the validated Go namespace codecs and complete immutable claim inventory must be \
                 collision-free; `{name}` in {scope} was claimed by both {previous} and {owner}"
            );
        }
        Ok(())
    };

    for (scope, name, owner) in facades.claims().iter() {
        let scope = match scope {
            GoFacadeScope::Package => "package".to_owned(),
            GoFacadeScope::ProductFields(shell) => format!("type:{shell:?}"),
        };
        claim(scope, name.as_str().to_owned(), format!("facade:{owner:?}"))?;
    }
    for (name, owner) in [
        (names.handle.as_str(), "package handle"),
        (names.host_ty.as_str(), "host interface"),
        (names.factory.as_str(), "package factory"),
        ("Value", "canonical erased boundary value"),
        ("Prepared", "statically indexed canonical boundary value"),
        ("TypeStage", "statically indexed delayed type selection"),
        ("AtomIndex", "abstract native atom index"),
        ("PrepareAtom", "statically indexed native atom construction"),
        ("Atom", "abstract native atom construction"),
        ("NativeAtom", "abstract native atom recovery"),
        ("kioAtom", "abstract native atom storage"),
        ("kioNative", "native atom recovery"),
        ("kioSealedValue", "unexposed nominal envelope"),
        ("kioNominalPayload", "resolved nominal projection"),
        ("kioProductSlots", "runtime product expansion"),
        ("kioProductSlot", "runtime product projection"),
        ("kioSumPayload", "runtime sum projection"),
        ("kioSumInject", "runtime sum injection"),
        ("kioStep", "private execution result"),
        ("kioNamedArguments", "private named argument packet"),
        ("kioValueEntry", "guest value entry"),
        ("kioTypeEntry", "guest type entry"),
        ("kioDone", "private completed result"),
        ("kioTailValue", "private value transfer"),
        ("kioTailType", "private type transfer"),
        ("kioComplete", "private execution driver"),
        ("kioApplyValue", "private value application"),
        ("kioApplyType", "private type application"),
    ] {
        claim("package".to_owned(), name.to_owned(), owner.to_owned())?;
    }
    for alias in &aliases.package_names {
        claim(
            "package".to_owned(),
            alias.clone(),
            format!("stable FFI alias `{alias}`"),
        )?;
    }
    for carrier in facades.parametric_host_carriers() {
        claim(
            "package".to_owned(),
            carrier.public_type().as_str().to_owned(),
            format!("parameterized host carrier `{:?}`", carrier.name()),
        )?;
        claim(
            "package".to_owned(),
            carrier.native_constructor().as_str().to_owned(),
            format!("parameterized host adapter `{:?}`", carrier.name()),
        )?;
    }
    for retained in facades.retained_host_types() {
        claim(
            "package".to_owned(),
            retained.public_type().as_str().to_owned(),
            format!("retained host type `{:?}`", retained.name()),
        )?;
    }

    fn walk_exports(
        node: &FacadeExportTree,
        path: &mut Vec<ExportSelector>,
        claim: &mut impl FnMut(String, String, String) -> Result<(), EmitError>,
    ) -> Result<(), EmitError> {
        let receiver = if path.is_empty() {
            "handle".to_owned()
        } else {
            export_ns_type_name(path)
        };
        if !path.is_empty() {
            claim(
                "package".to_owned(),
                receiver.clone(),
                format!("export namespace {path:?}"),
            )?;
            claim(
                format!("selector:{receiver}"),
                "pkg".to_owned(),
                "package back-pointer".to_owned(),
            )?;
        }
        for entry in &node.exports {
            claim(
                "package".to_owned(),
                format!("Call{}", ffi_site_prefix(&entry.site)),
                format!("selected export site {:?}", entry.site),
            )?;
            claim(
                format!("selector:{receiver}"),
                export_method_name(&entry.leaf),
                format!("export site {:?}", entry.site),
            )?;
        }
        for (selector, child) in &node.children {
            let field = export_selector_name(selector, path.is_empty());
            claim(
                format!("selector:{receiver}"),
                field,
                format!("export child {selector:?}"),
            )?;
            path.push(selector.clone());
            walk_exports(child, path, claim)?;
            path.pop();
        }
        Ok(())
    }
    walk_exports(exports, &mut Vec::new(), &mut claim)?;
    claim(
        "selector:handle".to_owned(),
        "host".to_owned(),
        "host storage".to_owned(),
    )?;

    for (module, entry) in package.modules() {
        for item in &entry.module.items {
            if let crate::ast::Item::FnDef(function) = item {
                let method = module_fn_method_name(module, &function.name);
                claim(
                    "selector:handle".to_owned(),
                    method.clone(),
                    format!("body function `{module}.{}`", function.name),
                )?;
                claim(
                    "selector:handle".to_owned(),
                    format!("kioStep_{method}"),
                    format!("body execution entry `{module}.{}`", function.name),
                )?;
                claim(
                    "package".to_owned(),
                    format!("kioInvoke_{method}"),
                    format!("body invocation entry `{module}.{}`", function.name),
                )?;
            }
        }
    }
    for live in facades.live_sites() {
        if let BoundaryFacadeSiteOwner::HostFunction { name } = live.site().id().owner() {
            let module = live.site().id().module_segments().join("/");
            claim(
                format!("selector:{}", names.host_ty),
                host_member_name(&module, name),
                format!("host site {:?}", live.site().id()),
            )?;
        }
    }
    for adapter in facades.role_adapters() {
        for (method, direction) in [
            (adapter.convert_in(), "into body"),
            (adapter.convert_out(), "out of body"),
        ] {
            claim(
                format!("selector:{}", names.host_ty),
                method.as_str().to_owned(),
                format!("role adapter {direction} for {:?}", adapter.name()),
            )?;
        }
    }
    Ok(())
}

fn render_facade_pkg_go(
    package: &Package<Routed>,
    roles: &HostRoleTable,
    facades: &GoFacadeCatalog<'_>,
    exports: &FacadeExportTree,
    names: &GoNames,
) -> Result<String, EmitError> {
    let mut out = String::new();
    out.push_str("// Generated by kio — do not edit by hand.\n\n");
    out.push_str(&format!("package {}\n\n", names.ns));
    render_facade_export_types(package, exports, &mut Vec::new(), facades, names, &mut out)?;
    out.push_str(&render_facade_package_struct(exports, facades, names));
    out.push_str(&render_facade_factory(exports, facades, names));

    let module_pieces: Result<Vec<String>, EmitError> =
        crate::maybe_into_par_iter!(package.modules().collect::<Vec<_>>())
            .map(|(module_key, entry)| {
                render_module_fns(module_key, entry, roles, package, facades, names)
            })
            .collect();
    let mut module_pieces = module_pieces?;
    module_pieces.sort();
    for piece in module_pieces {
        out.push_str(&piece);
    }
    Ok(out)
}

fn render_facade_export_types(
    package: &Package<Routed>,
    node: &FacadeExportTree,
    path: &mut Vec<ExportSelector>,
    facades: &GoFacadeCatalog<'_>,
    names: &GoNames,
    out: &mut String,
) -> Result<(), EmitError> {
    for (selector, child) in &node.children {
        path.push(selector.clone());
        render_facade_export_types(package, child, path, facades, names, out)?;
        path.pop();
    }

    if path.is_empty() {
        for entry in &node.exports {
            out.push_str(&render_facade_export_method(
                package,
                "pkg",
                &names.handle_ptr(&facades.host_binding_arguments()),
                entry,
                facades,
            )?);
        }
        return Ok(());
    }

    let ty = export_ns_type_name(path);
    let type_parameters = facades.host_binding_declaration();
    let type_arguments = facades.host_binding_arguments();
    let mut fields = vec![("pkg".to_owned(), names.handle_ptr(&type_arguments))];
    for selector in node.children.keys() {
        let mut child_path = path.clone();
        child_path.push(selector.clone());
        fields.push((
            export_selector_name(selector, false),
            format!("{}{type_arguments}", export_ns_type_name(&child_path)),
        ));
    }
    out.push_str(&render_go_struct(
        &format!("{ty}{type_parameters}"),
        &fields,
    ));
    out.push('\n');
    for entry in &node.exports {
        out.push_str(&render_facade_export_method(
            package,
            "ns",
            &format!("{ty}{type_arguments}"),
            entry,
            facades,
        )?);
    }
    Ok(())
}

fn render_facade_package_struct(
    root: &FacadeExportTree,
    facades: &GoFacadeCatalog<'_>,
    names: &GoNames,
) -> String {
    let type_parameters = facades.host_binding_declaration();
    let type_arguments = facades.host_binding_arguments();
    let mut fields = vec![(
        "host".to_owned(),
        format!("{}{type_arguments}", names.host_ty),
    )];
    for selector in root.children.keys() {
        fields.push((
            export_selector_name(selector, true),
            format!(
                "{}{type_arguments}",
                export_ns_type_name(std::slice::from_ref(selector))
            ),
        ));
    }
    let mut out = render_go_struct(&format!("{}{type_parameters}", names.handle), &fields);
    out.push('\n');
    out
}

fn render_facade_factory(
    root: &FacadeExportTree,
    facades: &GoFacadeCatalog<'_>,
    names: &GoNames,
) -> String {
    let type_parameters = facades.host_binding_declaration();
    let type_arguments = facades.host_binding_arguments();
    let mut out = format!(
        "func {}{}(host {}{}) {} {{\n\tpkg := &{}{}{{host: host}}\n",
        names.factory,
        type_parameters,
        names.host_ty,
        type_arguments,
        names.handle_ptr(&type_arguments),
        names.handle,
        type_arguments,
    );
    let mut path = Vec::new();
    wire_facade_namespace_pointers(root, &mut path, &mut out);
    out.push_str("\treturn pkg\n}\n\n");
    out
}

fn wire_facade_namespace_pointers(
    node: &FacadeExportTree,
    path: &mut Vec<ExportSelector>,
    out: &mut String,
) {
    for (selector, child) in &node.children {
        path.push(selector.clone());
        let selectors = path
            .iter()
            .enumerate()
            .map(|(index, selector)| export_selector_name(selector, index == 0))
            .collect::<Vec<_>>()
            .join(".");
        out.push_str(&format!("\tpkg.{selectors}.pkg = pkg\n"));
        wire_facade_namespace_pointers(child, path, out);
        path.pop();
    }
}

struct FacadeFreshNames {
    next: usize,
    host_expression: String,
}

impl FacadeFreshNames {
    fn new(host_expression: impl Into<String>) -> Self {
        Self {
            next: 0,
            host_expression: host_expression.into(),
        }
    }
}

impl GoFreshNames for FacadeFreshNames {
    fn fresh(&mut self, class: &'static str) -> String {
        let index = self.next;
        self.next += 1;
        format!("__kio_{class}{index}")
    }

    fn host_expression(&self) -> &str {
        &self.host_expression
    }
}

enum GoLiveExportEntry<'site, 'source> {
    Function(GoLiveCallableEntry<'site, 'source>),
    Newtype(GoLiveNewtypeMemberEntry<'site, 'source>),
}

impl<'site, 'source> GoLiveExportEntry<'site, 'source> {
    fn public_parameters(&self) -> Vec<GoPublicParameter<'site, 'source>> {
        match self {
            Self::Function(entry) => {
                let mut parameters = Vec::new();
                for stage in entry.stages() {
                    if let Some(value) = stage.value_stage() {
                        parameters.extend(
                            value
                                .public_parameters()
                                .unwrap_or_else(go_facade_invariant),
                        );
                    }
                }
                parameters
            }
            Self::Newtype(member) => member
                .public_parameters()
                .unwrap_or_else(go_facade_invariant),
        }
    }

    fn result(&self) -> &GoLiveFacadeUseRef<'site, 'source> {
        match self {
            Self::Function(entry) => entry.returned(),
            Self::Newtype(member) => member.result(),
        }
    }
}

fn render_facade_export_method(
    source_package: &Package<Routed>,
    receiver: &str,
    receiver_type: &str,
    export: &FacadeExportEntry,
    facades: &GoFacadeCatalog<'_>,
) -> Result<String, EmitError> {
    let site = facades.site(&export.site).unwrap_or_else(|| {
        unreachable!("the Go export tree contains only sites from its GoFacadeCatalog")
    });
    let live = site.live().unwrap_or_else(|| {
        unreachable!("the Go export tree contains only live prepared facade sites")
    });
    let semantic_entry = site.callable_entry().unwrap_or_else(go_facade_invariant);
    let indices = semantic_entry
        .stages()
        .iter()
        .filter_map(|stage| stage.type_stage())
        .enumerate()
        .map(|(index, stage)| format!("KioI{index} {}", stage.index_constraint()))
        .collect::<Vec<_>>();
    let selected = !indices.is_empty();
    let entry = match export.site.owner() {
        BoundaryFacadeSiteOwner::ExportedFunction { .. } => GoLiveExportEntry::Function(
            if selected {
                live.selected_callable_entry()
            } else {
                live.callable_entry()
            }
            .unwrap_or_else(go_facade_invariant),
        ),
        BoundaryFacadeSiteOwner::NewtypeConstructor { .. }
        | BoundaryFacadeSiteOwner::NewtypeProjector { .. } => GoLiveExportEntry::Newtype(
            (if selected {
                live.selected_newtype_member_entry()
            } else {
                live.newtype_member_entry()
            })
            .unwrap_or_else(go_facade_invariant),
        ),
        BoundaryFacadeSiteOwner::HostFunction { .. } => {
            unreachable!("Go export-tree collection excludes prepared host-function sites");
        }
    };
    let method = export_method_name(&export.leaf);
    let package = if receiver == "pkg" { "pkg" } else { "ns.pkg" };

    let mut parameters = Vec::new();
    let mut arguments = Vec::new();
    for cursor in entry.public_parameters() {
        let name = format!("arg{}", arguments.len());
        parameters.push(format!(
            "{name} {}",
            cursor.boundary_type().unwrap_or_else(go_facade_invariant)
        ));
        arguments.push(name);
    }
    let returned_type = entry
        .result()
        .boundary_type()
        .unwrap_or_else(go_facade_invariant);
    let mut body = GoBlock::default();
    let mut fresh = FacadeFreshNames::new(format!("{package}.host"));

    let internal = match &entry {
        GoLiveExportEntry::Function(callable) => {
            let BoundaryFacadeSiteOwner::ExportedFunction { name } = export.site.owner() else {
                unreachable!("only exported functions mint ordinary export entries")
            };
            let module = export.site.module_segments().join("/");
            let mut callee = format!("{package}.{}", module_fn_method_name(&module, name));
            let mut argument_offset = 0usize;
            for (stage_index, stage) in callable.stages().iter().enumerate() {
                if let Some(stage) = stage.type_stage() {
                    callee = facade_skin::invoke_type_stage(stage, &callee, &mut body, &mut fresh);
                } else if let Some(stage) = stage.value_stage() {
                    let end = argument_offset + stage.public_parameter_count();
                    let stage_arguments =
                        arguments.get(argument_offset..end).unwrap_or_else(|| {
                            unreachable!(
                                "the prepared Go export stages partition their public argument \
                                 inventory"
                            )
                        });
                    let invoke = if stage_index == 0 {
                        facade_skin::invoke_named_value_stage
                    } else {
                        facade_skin::invoke_value_stage
                    };
                    callee = invoke(stage, &callee, stage_arguments, &mut body, &mut fresh)
                        .unwrap_or_else(go_skin_invariant);
                    argument_offset = end;
                }
            }
            if argument_offset != arguments.len() {
                unreachable!(
                    "the prepared Go export stages must consume their complete public argument \
                     inventory"
                );
            }
            callee
        }
        GoLiveExportEntry::Newtype(member) => {
            let nominal = match export.site.owner() {
                BoundaryFacadeSiteOwner::NewtypeConstructor { newtype, .. }
                | BoundaryFacadeSiteOwner::NewtypeProjector { newtype, .. } => {
                    crate::backends::boundary_facade::QualifiedTypeName::new(
                        export.site.module_segments().to_vec(),
                        newtype.clone(),
                    )
                    .expect("a prepared newtype member has an exact nominal identity")
                }
                _ => unreachable!("a newtype entry has a newtype owner"),
            };
            invoke_newtype_member(member, &nominal, facades, &arguments, &mut body, &mut fresh)?
        }
    };
    let returned = facade_skin::convert(
        entry.result(),
        &internal,
        FfiDir::Out,
        &mut body,
        &mut fresh,
    )
    .unwrap_or_else(go_skin_invariant);
    body.line(format!("return {returned}"));

    let documentation = render_doc(
        crate::backends::boundary_docs::callable(source_package, site.prepared()),
        "",
    );
    let declaration = if selected {
        let mut type_parameters = indices;
        let root = facades.host_binding_declaration();
        if !root.is_empty() {
            type_parameters.push(root[1..root.len() - 1].to_owned());
        }
        parameters.insert(0, format!("{receiver} {receiver_type}"));
        format!(
            "func Call{}[{}]",
            ffi_site_prefix(&export.site),
            type_parameters.join(", ")
        )
    } else {
        format!("func ({receiver} {receiver_type}) {method}")
    };
    Ok(format!(
        "{documentation}{declaration}({}) {returned_type} {{\n{}}}\n\n",
        parameters.join(", "),
        indent_go_source(&body.source),
    ))
}

/// Invoke the erased constructor/projector body selected by one nominal-derived live
/// newtype-member capability. Declaration-owned universal and existential
/// constructor heads remain real nullary body stages and are replayed before
/// the value head. Only the capability's existential-projector variant adds
/// the validated continuation cut; an ordinary Church-shaped payload stays a
/// one-argument projector returning its function value.
fn invoke_newtype_member(
    member: &GoLiveNewtypeMemberEntry<'_, '_>,
    nominal: &crate::backends::boundary_facade::QualifiedTypeName,
    facades: &GoFacadeCatalog<'_>,
    arguments: &[String],
    body: &mut GoBlock,
    fresh: &mut FacadeFreshNames,
) -> Result<String, EmitError> {
    let entry = member.entry();
    let mut groups = Vec::with_capacity(entry.stages().len());
    let mut value_stage = None;
    for stage in entry.stages() {
        if stage.type_stage().is_some() {
            groups.push(GoClosureGroup::Type);
            continue;
        }
        let value = stage.value_stage().unwrap_or_else(|| {
            unreachable!("every prepared newtype callable head is either type- or value-valued")
        });
        if value_stage.replace(value).is_some() {
            unreachable!("a prepared newtype-member callable has exactly one value stage");
        }
        let source_groups = value.source_groups().unwrap_or_else(go_facade_invariant);
        groups.push(GoClosureGroup::Value(
            (0..source_groups.len())
                .map(|index| format!("__kio_member_arg{index}"))
                .collect(),
        ));
    }
    let Some(value_stage) = value_stage else {
        unreachable!("a prepared newtype-member callable has exactly one value stage");
    };
    let params = groups
        .iter()
        .find_map(|group| match group {
            GoClosureGroup::Value(params) => Some(params.clone()),
            GoClosureGroup::Type => None,
        })
        .unwrap_or_default();
    let result = match member {
        GoLiveNewtypeMemberEntry::Constructor(_) => facades.construct_nominal(
            nominal,
            params.first().map(String::as_str).unwrap_or("Unit{}"),
        ),
        GoLiveNewtypeMemberEntry::Projector(_) => {
            let receiver = params.first().unwrap_or_else(|| {
                unreachable!("a prepared newtype projector has one receiver source parameter")
            });
            facades.project_nominal(nominal, receiver)
        }
        GoLiveNewtypeMemberEntry::ExistentialProjector(projector) => {
            let payload = params.first().unwrap_or_else(|| {
                unreachable!("a prepared existential projector has one receiver source parameter")
            });
            render_existential_projector_value(
                projector,
                &facades.project_nominal(nominal, payload),
            )?
        }
    };
    let mut deepest = GoBlock::default();
    deepest.line(format!("return {result}"));
    let mut callee = render_curried_closure(&groups, deepest);
    let mut argument_offset = 0usize;
    let declaration_argument_count = value_stage.public_parameter_count();
    for stage in entry.stages() {
        if let Some(type_stage) = stage.type_stage() {
            callee = facade_skin::invoke_type_stage(type_stage, &callee, body, fresh);
        } else if let Some(value_stage) = stage.value_stage() {
            let end = argument_offset + value_stage.public_parameter_count();
            let stage_arguments = arguments.get(argument_offset..end).unwrap_or_else(|| {
                unreachable!(
                    "the prepared Go newtype stage partitions its declaration argument inventory"
                )
            });
            callee =
                facade_skin::invoke_value_stage(value_stage, &callee, stage_arguments, body, fresh)
                    .unwrap_or_else(go_skin_invariant);
            argument_offset = end;
        }
    }
    if argument_offset != declaration_argument_count {
        unreachable!(
            "the prepared Go newtype stages consume their complete declaration argument inventory"
        );
    }
    match member {
        GoLiveNewtypeMemberEntry::Constructor(_) | GoLiveNewtypeMemberEntry::Projector(_) => {
            if arguments.len() != declaration_argument_count {
                unreachable!(
                    "an ordinary prepared newtype member exposes exactly its declaration arguments"
                );
            }
            Ok(callee)
        }
        GoLiveNewtypeMemberEntry::ExistentialProjector(projector) => {
            let continuation_arguments = arguments
                .get(declaration_argument_count..)
                .unwrap_or_else(|| {
                    unreachable!(
                        "a prepared existential projector appends its continuation after all \
                         declaration arguments"
                    )
                });
            if continuation_arguments.len() != 1 {
                unreachable!(
                    "a prepared existential projector exposes exactly one public continuation"
                );
            }
            invoke_existential_projector_cut(
                projector,
                &callee,
                &continuation_arguments[0],
                body,
                fresh,
            )
        }
    }
}

/// Build the internal CPS result promised by a validated existential
/// projector. The public continuation is converted separately through its
/// live cursor; this erased closure applies the cursor's exact existential
/// Foralls and one-source payload Function when the CPS value is invoked.
fn render_existential_projector_value(
    projector: &GoLiveExistentialProjectorEntry<'_, '_>,
    payload: &str,
) -> Result<String, EmitError> {
    let continuation = "__kio_member_continuation".to_owned();
    let result =
        render_existential_continuation_call(projector.continuation(), &continuation, payload)?;
    let mut continuation_body = GoBlock::default();
    continuation_body.line(format!("return {result}"));
    let continuation_closure =
        GoClosureGroup::Value(vec![continuation]).render_entry(&continuation_body);

    let mut selected_body = GoBlock::default();
    selected_body.line(format!("return kioDone(any({continuation_closure}))"));
    Ok(format!(
        "any({})",
        GoClosureGroup::Type.render_entry(&selected_body)
    ))
}

fn invoke_existential_projector_cut(
    projector: &GoLiveExistentialProjectorEntry<'_, '_>,
    expression: &str,
    continuation: &str,
    body: &mut GoBlock,
    fresh: &mut FacadeFreshNames,
) -> Result<String, EmitError> {
    let outer = match projector
        .entry()
        .returned()
        .view()
        .unwrap_or_else(go_facade_invariant)
    {
        super::facade::GoLiveFacadeUseView::Forall { result, .. } => result,
        _ => {
            unreachable!(
                "a validated prepared existential projector result starts with its selected-result \
                 type stage"
            );
        }
    };
    let selected_callee = fresh.fresh("existential_selected_callee");
    body.line(format!("var {selected_callee} any"));
    body.line(format!("{selected_callee} = kioApplyType({expression})"));

    let (layout, slots) = match outer.view().unwrap_or_else(go_facade_invariant) {
        super::facade::GoLiveFacadeUseView::Function { layout, slots, .. } => (layout, slots),
        _ => {
            unreachable!(
                "a validated prepared existential projector selected-result stage is callable"
            );
        }
    };
    if layout.source_param_count() != 1
        || layout.body_abi_arity() != 1
        || layout.facade_slot_count() != 1
        || layout.source_params().len() != 1
        || layout.source_params()[0].facade_slots() != (0..1)
        || layout.source_params()[0].adapter() != CallableSourceParamAdapter::Identity
        || slots.len() != 1
    {
        unreachable!(
            "the prepared existential projector outer CPS stage has one identity continuation \
             source"
        );
    }

    let converted = facade_skin::convert(
        projector.continuation(),
        continuation,
        FfiDir::In,
        body,
        fresh,
    )
    .unwrap_or_else(go_skin_invariant);
    let result = fresh.fresh("existential_result");
    body.line(format!("var {result} any"));
    body.line(format!(
        "{result} = kioApplyValue({selected_callee}, {converted})"
    ));
    Ok(result)
}

fn render_existential_continuation_call(
    cursor: &GoLiveFacadeUseRef<'_, '_>,
    continuation: &str,
    payload: &str,
) -> Result<String, EmitError> {
    let mut cursor = cursor.clone();
    let mut callee = continuation.to_owned();
    loop {
        match cursor.view().unwrap_or_else(go_facade_invariant) {
            super::facade::GoLiveFacadeUseView::Forall { result, .. } => {
                callee = format!("kioApplyType({callee})");
                cursor = result;
            }
            super::facade::GoLiveFacadeUseView::Function { layout, slots, .. } => {
                if layout.source_param_count() == 0
                    && layout.body_abi_arity() == 0
                    && layout.facade_slot_count() == 0
                    && layout.source_params().is_empty()
                    && slots.is_empty()
                {
                    return Ok(format!("kioTailValue({callee}, Unit{{}})"));
                }
                if layout.source_param_count() != 1
                    || layout.body_abi_arity() != 1
                    || layout.facade_slot_count() != slots.len()
                    || layout.source_params().len() != 1
                    || layout.source_params()[0].facade_slots() != (0..slots.len())
                {
                    unreachable!(
                        "the prepared existential projector continuation has one source payload \
                         layout"
                    );
                }
                let argument = match layout.source_params()[0].adapter() {
                    CallableSourceParamAdapter::UnitValue if slots.is_empty() => "Unit{}",
                    CallableSourceParamAdapter::Identity if slots.len() == 1 => payload,
                    CallableSourceParamAdapter::RightNest if slots.len() > 1 => payload,
                    _ => {
                        unreachable!(
                            "the prepared existential projector continuation adapter agrees with \
                             its slots"
                        );
                    }
                };
                return Ok(format!("kioTailValue({callee}, {argument})"));
            }
            _ => {
                unreachable!("a validated prepared existential continuation is callable");
            }
        }
    }
}

fn go_skin_invariant<T>(error: facade_skin::GoFacadeSkinError) -> T {
    unreachable!("prepared Go facade conversion violated its live-layout invariant: {error}")
}

/// One edge in the Go facade tree. Keeping the declaration role beside the
/// exact source component makes cross-kind collisions unrepresentable in the
/// tree and supplies the role tag used by the generated selector.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ExportSelector {
    Module(String),
    Type(String),
}

impl ExportSelector {
    fn source(&self) -> &str {
        match self {
            Self::Module(source) | Self::Type(source) => source,
        }
    }
}
/// The Go method name on the package handle for a module function. Names
/// without source underscores retain the readable `mod_<module>_<leaf>`
/// spelling. An underscore in either source component selects the reserved
/// `mod_KioItem_<encoded-module>__<leaf>` class, keeping module separators
/// distinct from source underscores. Its internal capitals are outside the
/// readable class, and the encoded module contains no `__`, so the two source
/// components remain recoverable.
fn module_fn_method_name(module_key: &str, leaf: &str) -> String {
    if !module_key.contains('_') && !leaf.contains('_') {
        return format!("mod_{}_{leaf}", module_key.replace('/', "_"));
    }
    format!("mod_KioItem_{}__{leaf}", encode_item_module(module_key))
}

/// The internal Go type name for the export-namespace handle at `path`.
///
/// Every source component is encoded independently and prefixed by its role:
/// `module foo/bar` becomes `exportNs_M_foo__M_bar`, while a public newtype
/// `Bar` under `foo` becomes `exportNs_M_foo__T_Bar`. The source encoding
/// contains no `__`, so role and component boundaries are recoverable.
fn export_ns_type_name(path: &[ExportSelector]) -> String {
    if path.is_empty() {
        "exportNs".to_owned()
    } else {
        let components: Vec<String> = path
            .iter()
            .map(|selector| {
                let role = match selector {
                    ExportSelector::Module(_) => "M",
                    ExportSelector::Type(_) => "T",
                };
                format!("{role}_{}", encode_item_module(selector.source()))
            })
            .collect();
        format!("exportNs_{}", components.join("__"))
    }
}

/// Render an export-tree edge as a Go selector.
///
/// Root modules keep the established natural spelling (`pkg.Api`). Below a
/// root module, module and type edges occupy disjoint reserved classes, so a
/// function method, a nested module, and a public-newtype handle can share a
/// source stem without competing for one Go selector.
fn export_selector_name(selector: &ExportSelector, root: bool) -> String {
    match selector {
        ExportSelector::Module(source) if root => export_component_name(source),
        ExportSelector::Module(source) => {
            format!("KioModule_{}", encode_item_module(source))
        }
        ExportSelector::Type(source) => format!("KioType_{}", encode_item_module(source)),
    }
}

/// The exported method name for an export fn `leaf`.
fn export_method_name(leaf: &str) -> String {
    export_component_name(leaf)
}

/// Render one Kio identifier as an injective Go-exported facade component.
///
/// Conventional snake names keep the established title-cased spelling:
/// `foo_bar` → `FooBar`. Leading/trailing underscore affixes use the reserved
/// exact class: `_foo_bar` → `KioItem__ufooBar`. Readable names contain no
/// underscore, while every exact name contains the `KioItem_` separator, so
/// the two images are disjoint.
fn export_component_name(source: &str) -> String {
    if has_readable_snake_components(source) {
        title_case(source)
    } else {
        format!("KioItem_{}", encode_item_module(source))
    }
}

/// Whether every underscore in `source` introduces a non-empty lowercase
/// snake component. Unmarked Kio type names have an uppercase first byte but
/// the same lowercase tail convention, so this predicate covers readable
/// value/module names and newtype-handle names. A marked type name begins with
/// `_` and therefore takes the exact path below.
fn has_readable_snake_components(source: &str) -> bool {
    let bytes = source.as_bytes();
    !bytes.is_empty()
        && bytes[0] != b'_'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, byte)| *byte != b'_' || bytes.get(i + 1).is_some_and(u8::is_ascii_lowercase))
}

/// Title-case a Kio snake_case identifier for Go export: capitalize the
/// first letter and each letter after `_`, dropping the `_`. Callers first
/// prove that each underscore marks a recoverable lowercase component via
/// [`has_readable_snake_components`].
fn title_case(s: &str) -> String {
    capitalize_first(&crate::backends::public_names::host_name_core(s))
}

/// Render a Go `struct` body with gofmt-style field alignment: each
/// field's name is right-padded so the types line up in a column, a
/// single space separating the widest name from its type. `fields` is a
/// `(name, type)` list. Produces `type <name> struct {\n…\n}\n` with no
/// trailing blank line (the caller adds inter-item spacing).
pub(crate) fn render_go_struct(name: &str, fields: &[(String, String)]) -> String {
    let width = fields.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut out = format!("type {name} struct {{\n");
    for (fname, ftype) in fields {
        out.push_str(&format!("\t{fname:<width$} {ftype}\n"));
    }
    out.push_str("}\n");
    out
}

/// One Go function body's statement sink. The emitter writes control flow
/// directly into this block; expression-valued parents receive a fresh local
/// assigned by their child instead of an immediately-invoked closure.
#[derive(Default)]
struct GoBlock {
    source: String,
    indent: usize,
}

impl GoBlock {
    fn line(&mut self, source: impl AsRef<str>) {
        for line in source.as_ref().lines() {
            self.source.push_str(&"\t".repeat(self.indent));
            self.source.push_str(line);
            self.source.push('\n');
        }
    }

    fn open(&mut self, header: impl AsRef<str>) {
        self.line(format!("{} {{", header.as_ref()));
        self.indent += 1;
    }

    fn else_open(&mut self) {
        self.indent = self
            .indent
            .checked_sub(1)
            .expect("Go emitter block indentation underflow");
        self.line("} else {");
        self.indent += 1;
    }

    fn close(&mut self) {
        self.indent = self
            .indent
            .checked_sub(1)
            .expect("Go emitter block indentation underflow");
        self.line("}");
    }

    fn indent(&mut self) {
        self.indent += 1;
    }

    fn dedent(&mut self) {
        self.indent = self
            .indent
            .checked_sub(1)
            .expect("Go emitter block indentation underflow");
    }
}

impl GoStatementSink for GoBlock {
    fn line(&mut self, source: &str) {
        GoBlock::line(self, source);
    }

    fn open(&mut self, header: &str) {
        GoBlock::open(self, header);
    }

    fn indent(&mut self) {
        GoBlock::indent(self);
    }

    fn dedent(&mut self) {
        GoBlock::dedent(self);
    }

    fn close(&mut self) {
        GoBlock::close(self);
    }
}

#[derive(Clone, Copy)]
enum GoDestination<'a> {
    Return,
    ReturnStep,
    Assign(&'a str),
    Discard,
}

impl GoDestination<'_> {
    fn write(self, out: &mut GoBlock, value: impl AsRef<str>) {
        match self {
            GoDestination::Return => out.line(format!("return {}", value.as_ref())),
            GoDestination::ReturnStep => out.line(format!("return kioDone({})", value.as_ref())),
            GoDestination::Assign(name) => out.line(format!("{name} = {}", value.as_ref())),
            GoDestination::Discard => out.line(format!("_ = {}", value.as_ref())),
        }
    }
}

fn render_go_closure(params: &[String], body: &GoBlock) -> String {
    render_go_closure_result(params, body, "any")
}

fn go_value_call(callee: &str, domain: &str, tail: bool) -> String {
    let helper = if tail {
        "kioTailValue"
    } else {
        "kioApplyValue"
    };
    format!("{helper}({callee}, {domain})")
}

fn render_go_closure_result(params: &[String], body: &GoBlock, result: &str) -> String {
    let params = params
        .iter()
        .map(|name| format!("{name} any"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut source = format!("func({params}) {result} {{\n");
    source.push_str(&indent_go_source(&body.source));
    source.push('}');
    source
}

fn indent_go_source(source: &str) -> String {
    let mut indented = String::new();
    for line in source.lines() {
        indented.push('\t');
        indented.push_str(line);
        indented.push('\n');
    }
    indented
}

enum GoClosureGroup {
    Type,
    Value(Vec<String>),
}

impl GoClosureGroup {
    fn params(&self) -> &[String] {
        match self {
            Self::Type => &[],
            Self::Value(params) => params,
        }
    }

    fn render(&self, body: &GoBlock) -> String {
        match self {
            Self::Type => render_go_closure(&[], body),
            Self::Value(params) => render_value_closure(params, body),
        }
    }

    fn render_entry(&self, body: &GoBlock) -> String {
        match self {
            Self::Type => format!(
                "kioTypeEntry({})",
                render_go_closure_result(&[], body, "kioStep")
            ),
            Self::Value(params) => format!(
                "kioValueEntry({})",
                render_value_closure_result(params, body, "kioStep")
            ),
        }
    }
}

/// A private value stage receives its complete semantic domain, including Unit.
/// Source binders only determine how that domain is unpacked inside the body.
fn render_value_closure(params: &[String], body: &GoBlock) -> String {
    render_value_closure_result(params, body, "any")
}

fn render_value_closure_result(params: &[String], body: &GoBlock, result: &str) -> String {
    match params {
        [] => render_go_closure_result(&["_".to_owned()], body, result),
        [_] => render_go_closure_result(params, body, result),
        _ => {
            let mut unpacked = GoBlock::default();
            unpacked.line(format!(
                "__kio_domain_slots := kioProductSlots(__kio_domain, {})",
                params.len()
            ));
            for (index, param) in params.iter().enumerate() {
                unpacked.line(format!("{param} := __kio_domain_slots[{index}]"));
                unpacked.line(format!("_ = {param}"));
            }
            unpacked.source.push_str(&body.source);
            render_go_closure_result(&["__kio_domain".to_owned()], &unpacked, result)
        }
    }
}

fn signature_closure_groups(
    sig: &crate::ast::Signature<Routed>,
    bound: Vec<Vec<String>>,
) -> Vec<GoClosureGroup> {
    let mut bound = bound.into_iter();
    let mut groups = Vec::new();
    for group in sig.canonical_groups() {
        match group {
            crate::ast::SignatureGroupRef::Type(params) => {
                for _ in params {
                    assert!(bound.next().expect("bound type stage").is_empty());
                    groups.push(GoClosureGroup::Type);
                }
            }
            crate::ast::SignatureGroupRef::Value(_) => groups.push(GoClosureGroup::Value(
                bound.next().expect("bound value stage"),
            )),
        }
    }
    if groups.is_empty() {
        groups.push(GoClosureGroup::Value(
            bound.next().expect("implicit Unit stage"),
        ));
    }
    assert!(
        bound.next().is_none(),
        "all bound runtime stages are consumed"
    );
    groups
}

fn render_curried_closure(groups: &[GoClosureGroup], body: GoBlock) -> String {
    render_curried(groups, body, false)
}

fn render_curried_entries(groups: &[GoClosureGroup], body: GoBlock) -> String {
    render_curried(groups, body, true)
}

fn render_curried(groups: &[GoClosureGroup], body: GoBlock, guest: bool) -> String {
    let mut groups = groups.iter().rev();
    let innermost = groups
        .next()
        .expect("Go emitter function has at least one runtime stage");
    let render = |group: &GoClosureGroup, body: &GoBlock| {
        if guest {
            group.render_entry(body)
        } else {
            group.render(body)
        }
    };
    let mut value = render(innermost, &body);
    for group in groups {
        let mut wrapper = GoBlock::default();
        let destination = if guest {
            GoDestination::ReturnStep
        } else {
            GoDestination::Return
        };
        destination.write(&mut wrapper, format!("any({value})"));
        value = render(group, &wrapper);
    }
    format!("any({value})")
}

/// Render every module fn in `entry` as a method on the package handle.
///
/// The ordinary method retains its source parameter ABI and completes the
/// private entry's steps. Named entries bind the resolved parameter slots
/// directly; later stages retain guest entries with canonical value domains,
/// including each executing type binder.
fn render_module_fns(
    module_key: &str,
    entry: &crate::pass::resolve::ModuleEntry<Routed>,
    roles: &HostRoleTable,
    package: &Package<Routed>,
    facades: &GoFacadeCatalog<'_>,
    names: &GoNames,
) -> Result<String, EmitError> {
    let selective_imports = build_selective_imports(&entry.module, package);
    let qualified_imports = build_qualified_imports(&entry.module.imports);
    let mut out = String::new();
    for item in &entry.module.items {
        if let crate::ast::Item::FnDef(f) = item {
            let method = module_fn_method_name(module_key, &f.name);
            let runtime_groups = runtime_param_name_groups(&f.sig);
            let mut emitter = BodyEmitter::new(
                package,
                roles,
                module_key,
                &selective_imports,
                &qualified_imports,
                facades,
            );
            let bound = emitter.bind_param_groups(&runtime_groups);
            let emitted_groups = signature_closure_groups(&f.sig, bound);
            emitter.bind_type_params(&f.sig);
            let mut body = GoBlock::default();
            emitter.emit_result_into(
                TailContext::body(&f.body),
                GoDestination::ReturnStep,
                &mut body,
            )?;
            let outer = emitted_groups
                .first()
                .expect("module runtime stage")
                .params();
            let outer_list: String = outer
                .iter()
                .map(|name| format!("{name} any"))
                .collect::<Vec<_>>()
                .join(", ");
            let method_body = if emitted_groups.len() == 1 {
                indent_go_source(&body.source)
            } else {
                let closure = render_curried_entries(&emitted_groups[1..], body);
                format!("\treturn kioDone({closure})\n")
            };
            let handle = names.handle_ptr(&facades.host_binding_arguments());
            let arguments = outer.join(", ");
            out.push_str(&format!(
                "func (pkg {handle}) {method}({outer_list}) any {{\n\treturn kioComplete(pkg.kioStep_{method}({arguments}))\n}}\n\n",
            ));
            let mut unpack = GoBlock::default();
            for name in outer {
                unpack.line(format!("_ = {name}"));
            }
            let unpack = indent_go_source(&unpack.source);
            out.push_str(&format!(
                "func (pkg {handle}) kioStep_{method}({outer_list}) kioStep {{\n{unpack}{method_body}}}\n\n",
            ));
            // A function literal inside a generic method captures Go's type
            // dictionary even with an erased method-interface assertion. A
            // nongeneric package-level entry has no per-transfer environment.
            let signature = vec!["any"; outer.len()].join(", ");
            let arguments = (0..outer.len())
                .map(|index| match index {
                    0 => "__kio_arguments.first".to_owned(),
                    1 => "__kio_arguments.second".to_owned(),
                    _ => format!("__kio_arguments.rest[{}]", index - 2),
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "func kioInvoke_{method}(__kio_owner any, __kio_arguments kioNamedArguments) kioStep {{\n\treturn __kio_owner.(interface {{ kioStep_{method}({signature}) kioStep }}).kioStep_{method}({arguments})\n}}\n\n",
            ));
        }
    }
    Ok(out)
}

/// The value-arity of each value group of a fn signature, in order
/// (each callable layer's param count). A nullary fn yields `[0]`.
fn value_group_arities(sig: &crate::ast::Signature<Routed>) -> Vec<usize> {
    let arities: Vec<usize> = sig
        .canonical_groups()
        .into_iter()
        .filter_map(|group| match group {
            crate::ast::SignatureGroupRef::Value(params) => Some(
                params
                    .iter()
                    .filter(|p| matches!(p, crate::ast::SignatureParam::Value(_)))
                    .count(),
            ),
            crate::ast::SignatureGroupRef::Type(_) => None,
        })
        .collect();
    if arities.is_empty() { vec![0] } else { arities }
}

/// A use-imported surface name → the package-handle method name of the
/// module fn it resolves to (`mod_<from-key>_<leaf>`).
type SelectiveImports = BTreeMap<String, String>;

/// Alias → real module slash-path, from `import <pkg>/<mod> as a;` items,
/// so a qualified `a.fn(...)` callee resolves to the right module's
/// fn method. Mirrors Rust's `build_qualified_imports`.
type QualifiedImports = BTreeMap<String, String>;

fn build_qualified_imports(imports: &[crate::ast::Import]) -> QualifiedImports {
    let mut out = BTreeMap::new();
    for u in imports {
        if let crate::ast::ImportKind::Qualified { path, alias } = &u.kind {
            let path_str = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(alias.clone(), path_str);
        }
    }
    out
}

fn build_selective_imports(
    importer: &crate::ast::Module<Routed>,
    package: &Package<Routed>,
) -> SelectiveImports {
    crate::backends::selective_module_fn_import_owners(importer, package)
        .into_iter()
        .map(|(name, owner)| {
            let method = module_fn_method_name(&owner, &name);
            (name, method)
        })
        .collect()
}

struct GoLocalBinding {
    source: String,
    emitted: String,
}

/// Walks a module fn body and writes statement-oriented Go over the universal
/// erased value model (`any`). Every child computation is materialized before
/// the next child, preserving Kio's strict left-to-right effect order without
/// building nested call-once closures.
struct BodyEmitter<'a, 'p> {
    package: &'a Package<Routed>,
    /// Lexical type binders protect canonical resolution of annotated slots.
    type_params: Vec<crate::ast::TypeParam>,
    /// Kio source binding → unique Go local, in lexical scope order.
    locals: Vec<GoLocalBinding>,
    /// Per-function deterministic counter for bindings and temporary values.
    fresh: usize,
    /// Exact host-type identity → role for body literals and internal
    /// annotation-driven emission.
    roles: &'a HostRoleTable,
    /// The slash-path of the module whose fn body is being emitted —
    /// resolves a same-module call's bare name to its method.
    module_key: &'a str,
    /// Surface name → module-fn method name, for use-imported calls.
    selective_imports: &'a SelectiveImports,
    /// Alias → real module slash-path, for qualified `a.fn(...)` calls.
    qualified_imports: &'a QualifiedImports,
    /// Package-complete semantic facade realization. Host-call boundary
    /// semantics are looked up by structured site identity; raw expression
    /// signatures remain body-grouping metadata only.
    facades: &'a GoFacadeCatalog<'p>,
}

impl GoFreshNames for BodyEmitter<'_, '_> {
    fn fresh(&mut self, class: &'static str) -> String {
        self.fresh_name(class)
    }

    fn host_expression(&self) -> &str {
        "pkg.host"
    }
}

impl<'a, 'p> BodyEmitter<'a, 'p> {
    fn new(
        package: &'a Package<Routed>,
        roles: &'a HostRoleTable,
        module_key: &'a str,
        selective_imports: &'a SelectiveImports,
        qualified_imports: &'a QualifiedImports,
        facades: &'a GoFacadeCatalog<'p>,
    ) -> Self {
        BodyEmitter {
            package,
            type_params: Vec::new(),
            locals: Vec::new(),
            fresh: 0,
            roles,
            module_key,
            selective_imports,
            qualified_imports,
            facades,
        }
    }

    fn fresh_name(&mut self, class: &str) -> String {
        let index = self.fresh;
        self.fresh += 1;
        format!("__kio_{class}{index}")
    }

    fn bind_local(&mut self, source: &str) -> String {
        let index = self.fresh;
        self.fresh += 1;
        let emitted = format!("{}_{index}", go_local_ident(source));
        self.locals.push(GoLocalBinding {
            source: source.to_owned(),
            emitted: emitted.clone(),
        });
        emitted
    }

    fn bind_param_groups(&mut self, groups: &[Vec<&str>]) -> Vec<Vec<String>> {
        groups
            .iter()
            .map(|group| group.iter().map(|name| self.bind_local(name)).collect())
            .collect()
    }

    fn bind_type_params(&mut self, signature: &crate::ast::Signature<Routed>) {
        self.type_params.extend(
            signature
                .params
                .iter()
                .filter_map(|parameter| match parameter {
                    crate::ast::SignatureParam::Type(parameter) => Some(parameter.clone()),
                    crate::ast::SignatureParam::Value(_) => None,
                }),
        );
    }

    fn canonical_type(&self, ty: &Type<Routed>) -> Type<Routed> {
        let module = &self
            .package
            .module(self.module_key)
            .expect("a routed Go body belongs to a package module")
            .module;
        crate::backends::boundary_facade::canonical_routed_type(
            self.package,
            module,
            &self.type_params,
            ty,
        )
    }

    fn nominal_identity(&self, ty: &Type<Routed>) -> QualifiedTypeName {
        let Type::Path { segments, .. } = self.canonical_type(ty) else {
            unreachable!("a resolved nominal operation names a nominal type")
        };
        let (name, module) = segments
            .split_last()
            .expect("a resolved nominal path is nonempty");
        QualifiedTypeName::new(
            module.iter().map(|segment| segment.name.clone()).collect(),
            name.name.clone(),
        )
        .expect("a resolved nominal operation has an exact module owner")
    }

    fn local_name(&self, source: &str) -> String {
        self.locals
            .iter()
            .rev()
            .find(|binding| binding.source == source)
            .map(|binding| binding.emitted.clone())
            .unwrap_or_else(|| go_local_ident(source))
    }

    fn emit_value(
        &mut self,
        expression: &Expr<Routed>,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let value = self.fresh_name("v");
        out.line(format!("var {value} any"));
        self.emit_into(expression, GoDestination::Assign(&value), out)?;
        Ok(value)
    }

    /// Lower one Routed expression into an explicit result destination.
    fn emit_into(
        &mut self,
        e: &Expr<Routed>,
        destination: GoDestination<'_>,
        out: &mut GoBlock,
    ) -> Result<(), EmitError> {
        self.emit_result_into(TailContext::operand(e), destination, out)
    }

    fn emit_result_into(
        &mut self,
        context: TailContext<'_>,
        destination: GoDestination<'_>,
        out: &mut GoBlock,
    ) -> Result<(), EmitError> {
        let e = context.expr();
        let tail = context.tail_stage().is_some();
        let call_destination = if tail {
            GoDestination::Return
        } else {
            destination
        };
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Unit { .. } => destination.write(out, "(Unit{})"),
            // Surface-only / pre-Routed variants: discharged via the
            // `Never`-witness `ext`. The surface-form removal guarantees
            // the pipeline substituted every one before structural
            // recovery.
            Expr::Path { ext, .. }
            | Expr::Call { ext, .. }
            | Expr::Tuple { ext, .. }
            | Expr::FnPlaceholder { ext, .. }
            | Expr::LabelValue { ext, .. }
            | Expr::RowLet { ext, .. }
            | Expr::Elaborator { ext, .. }
            | Expr::RecOrder { ext, .. }
            | Expr::RecQuote { ext, .. }
            | Expr::UserElaborator { ext, .. }
            | Expr::Ufcs { ext, .. }
            | Expr::OpChain { ext, .. }
            | Expr::RecCall { ext, .. } => match *ext {},
            Expr::StrLit { value, .. } => {
                destination.write(out, format!("any({})", go_string_lit(value)))
            }
            Expr::IntLit {
                digits, annotation, ..
            } => destination.write(
                out,
                format!("any({})", self.emit_int_lit(digits, annotation)?),
            ),
            Expr::FloatLit {
                digits, annotation, ..
            } => destination.write(
                out,
                format!("any({})", self.emit_float_lit(digits, annotation)?),
            ),
            Expr::BoolLit { value, .. } => destination.write(out, format!("any({value})")),
            Expr::Let { name, value, .. } => {
                let ident = self.fresh_name("bind");
                out.line(format!("var {ident} any"));
                self.emit_into(value, GoDestination::Assign(&ident), out)?;
                out.line(format!("_ = {ident}"));
                self.locals.push(GoLocalBinding {
                    source: name.clone(),
                    emitted: ident,
                });
                self.emit_result_into(
                    context.result_child(ResultChild::LetBody),
                    destination,
                    out,
                )?;
                self.locals.pop();
            }
            Expr::Seq { value, .. } => {
                self.emit_into(value, GoDestination::Discard, out)?;
                self.emit_result_into(
                    context.result_child(ResultChild::SeqBody),
                    destination,
                    out,
                )?;
            }
            Expr::LowHostCall {
                name,
                module_path,
                args,
                ..
            } => self.emit_low_host_call(name, module_path, args, destination, out)?,
            Expr::LowBoundRef { name, .. } => destination.write(out, self.local_name(name)),
            Expr::EnrichedConditional { cond, .. } => {
                self.emit_conditional(context, cond, destination, out)?
            }
            Expr::EnrichedTuple {
                items, synth_ty, ..
            } => {
                let value = self.emit_tuple_typed(items, synth_ty, out)?;
                destination.write(out, value);
            }
            Expr::EnrichedProject {
                target,
                index,
                arity,
                ..
            } => {
                let value = self.emit_project(target, *index, *arity, out)?;
                destination.write(out, value);
            }
            Expr::EnrichedInject {
                payload,
                variant,
                variants,
                ..
            } => {
                let value = self.emit_inject(payload, *variant, *variants, out)?;
                destination.write(out, value);
            }
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => self.emit_match(context, scrutinee, arms, destination, out)?,
            Expr::EnrichedFieldGet {
                target,
                index,
                arity,
                target_ty,
                ..
            } => {
                let value = self.emit_project(target, *index, *arity, out)?;
                let target_type = self.canonical_type(target_ty);
                let slot = crate::backends::reconstruct::nth_product_slot_ty_for_arity(
                    &target_type,
                    *index,
                    *arity,
                )
                .expect("a routed field projection fits its resolved product arity");
                let nominal = self.nominal_identity(slot);
                destination.write(out, self.facades.project_nominal(&nominal, &value));
            }
            Expr::LowModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let value =
                    self.emit_module_call(mangled, type_args.len(), args, sig, tail, out)?;
                call_destination.write(out, value);
            }
            Expr::LowQualifiedModuleCall {
                mangled,
                type_args,
                args,
                sig,
                ..
            } => {
                let value = self.emit_qualified_module_call(
                    mangled,
                    type_args.len(),
                    args,
                    sig,
                    tail,
                    out,
                )?;
                call_destination.write(out, value);
            }
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                payload,
                ..
            } => {
                let value =
                    self.emit_newtype_member(None, newtype, member, type_args, payload, out)?;
                destination.write(out, value);
            }
            Expr::LowNewtypeProj {
                newtype,
                member,
                type_args,
                target,
                ..
            } => {
                let value =
                    self.emit_newtype_member(None, newtype, member, type_args, target, out)?;
                destination.write(out, value);
            }
            Expr::LowAbsurdCall { value_arg, .. } => {
                // Evaluate the bottom-typed argument for its host effects
                // (e.g. an `exit(n) -> !` call must run), then panic — the
                // value can never be returned (the type is `!`). Mirrors
                // the Rust backend's `{ let _ = arg; unreachable!() }`.
                self.emit_into(value_arg, GoDestination::Discard, out)?;
                out.line("panic(\"kio: __absurd__ reached\")");
            }
            Expr::FnExpr { sig, body, .. } => {
                let value = self.emit_fn_expr(sig, body)?;
                destination.write(out, value);
            }
            Expr::LowClosureCall {
                name,
                type_args,
                args,
                ..
            } => {
                let callee =
                    self.emit_erased_type_applications(self.local_name(name), type_args.len(), out);
                let value = self.emit_apply(&callee, args, tail, out)?;
                call_destination.write(out, value);
            }
            Expr::LowIndirectCall {
                callee,
                type_args,
                args,
                ..
            } => {
                let c = self.emit_value(callee, out)?;
                let c = self.emit_erased_type_applications(c, type_args.len(), out);
                let value = self.emit_apply(&c, args, tail, out)?;
                call_destination.write(out, value);
            }
            Expr::LowTypeApplication { callee, .. } => {
                let c = self.emit_value(callee, out)?;
                let value = if tail {
                    format!("kioTailType({c})")
                } else {
                    self.emit_erased_type_applications(c, 1, out)
                };
                call_destination.write(out, value);
            }
            Expr::LowCpsProjectorApply {
                newtype,
                module_path,
                receiver,
                continuation,
                continuation_ty,
                ..
            } => {
                let nominal = QualifiedTypeName::new(
                    module_path.split('/').map(str::to_owned).collect(),
                    newtype,
                )
                .expect("a routed CPS projector has an exact nominal owner");
                let value = self.emit_cps_projector_apply(
                    &nominal,
                    receiver,
                    continuation,
                    continuation_ty,
                    tail,
                    out,
                )?;
                call_destination.write(out, value);
            }
            Expr::LowHostFnValueRef {
                name, module_path, ..
            } => {
                let value = self.emit_host_fn_value_ref(name, module_path)?;
                destination.write(out, value);
            }
            Expr::LowModuleFnValueRef { mangled, sig, .. } => {
                let value = self.emit_module_fn_value_ref(mangled, sig)?;
                destination.write(out, value);
            }
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                payload,
                ..
            } => {
                let value = self.emit_newtype_member(
                    Some(module_path),
                    newtype,
                    member,
                    type_args,
                    payload,
                    out,
                )?;
                destination.write(out, value);
            }
        }
        Ok(())
    }

    fn emit_newtype_member(
        &mut self,
        module: Option<&str>,
        name: &str,
        member: &str,
        _type_args: &[Type<Routed>],
        payload: &Expr<Routed>,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let mut segments = module
            .map(|module| module.split('/').map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        segments.push(name.to_owned());
        let nominal =
            self.nominal_identity(&Type::synth_path(segments, Vec::new(), payload.span()));
        let Some(crate::backends::value_plan::ValueNominalDeclaration::Newtype(declaration)) =
            crate::backends::value_plan::nominal_declaration(self.package, &nominal)
        else {
            unreachable!("a resolved Go newtype member retains its declaration")
        };
        let value = self.emit_value(payload, out)?;
        if member == declaration.constructor.name {
            Ok(self.facades.construct_nominal(&nominal, &value))
        } else {
            assert_eq!(
                member, declaration.projector.name,
                "a resolved Go newtype member is its constructor or projector"
            );
            Ok(self.facades.project_nominal(&nominal, &value))
        }
    }

    /// Project exactly the resolved nominal envelope before applying the
    /// existential continuation's erased type stages and value ABI.
    fn emit_cps_projector_apply(
        &mut self,
        nominal: &QualifiedTypeName,
        receiver: &Expr<Routed>,
        continuation: &Expr<Routed>,
        continuation_ty: &Type<Routed>,
        tail: bool,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let receiver_value = self.emit_value(receiver, out)?;
        let receiver_name = self.fresh_name("cps_receiver");
        out.line(format!("var {receiver_name} any"));
        out.line(format!(
            "{receiver_name} = {}",
            self.facades.project_nominal(nominal, &receiver_value)
        ));

        let continuation_value = self.emit_value(continuation, out)?;
        let continuation_name = self.fresh_name("cps_continuation");
        out.line(format!("var {continuation_name} any"));
        out.line(format!("{continuation_name} = {continuation_value}"));

        let (type_stages, callable) = continuation_ty.peel_leading_foralls();
        let Type::Function { abi_arity, .. } = callable else {
            unreachable!("a routed CPS projector continuation has a function type")
        };
        assert!(
            *abi_arity <= 1,
            "a routed CPS projector continuation has zero or one ABI slot"
        );
        let continuation_name =
            self.emit_erased_type_applications(continuation_name, type_stages, out);
        let args = if *abi_arity == 0 {
            "Unit{}".to_owned()
        } else {
            receiver_name
        };
        Ok(go_value_call(&continuation_name, &args, tail))
    }

    /// `LowHostFnValueRef { name, module_path, .. }` → a Go closure that
    /// forwards through the host record with the same FFI conversion as a
    /// direct host call. Its internal closure shape retains every prepared
    /// runtime stage: one nullary layer per type binder and one layer per
    /// value group. The deepest layer converts all accumulated value params,
    /// invokes the value-only host ABI, and converts the return into the
    /// internal representation.
    ///
    fn emit_host_fn_value_ref(
        &mut self,
        name: &str,
        module_path: &str,
    ) -> Result<String, EmitError> {
        let catalog = self.facades;
        let site_id = host_site_id(module_path, name);
        let site = catalog.site(&site_id).unwrap_or_else(|| {
            unreachable!(
                "the Routed host-function value `{module_path}.{name}` must have a prepared live \
                 facade site"
            )
        });
        let live = site.live().unwrap_or_else(|| {
            unreachable!("a Routed host-function value cannot select a retained facade site")
        });
        let entry = live.callable_entry().unwrap_or_else(go_facade_invariant);
        let mut groups = Vec::with_capacity(entry.stages().len());
        let mut value_stages = Vec::new();
        for stage in entry.stages() {
            if let Some(type_stage) = stage.type_stage() {
                type_stage.require_invoke_nullary();
                groups.push(GoClosureGroup::Type);
                continue;
            }
            let value = stage.value_stage().unwrap_or_else(|| {
                unreachable!("every prepared host callable head is either type- or value-valued")
            });
            let source_groups = value.source_groups().unwrap_or_else(go_facade_invariant);
            let params = (0..source_groups.len())
                .map(|_| self.fresh_name("host_value_arg"))
                .collect::<Vec<_>>();
            groups.push(GoClosureGroup::Value(params));
            value_stages.push((value, source_groups));
        }
        if groups.is_empty() {
            unreachable!("a prepared host callable has at least one runtime stage");
        }

        let slots = entry
            .stages()
            .iter()
            .filter_map(GoLiveHeadStage::value_stage)
            .flat_map(|stage| {
                stage
                    .public_parameters()
                    .unwrap_or_else(go_facade_invariant)
            })
            .collect::<Vec<_>>();
        let returned_type = entry
            .returned()
            .boundary_type()
            .unwrap_or_else(go_facade_invariant);
        let returned_is_unit = matches!(
            entry.returned().view().unwrap_or_else(go_facade_invariant),
            super::facade::GoLiveFacadeUseView::Unit
        );
        let signature = format!(
            "func({}){}",
            slots
                .iter()
                .map(|slot| { slot.boundary_type().unwrap_or_else(go_facade_invariant) })
                .collect::<Vec<_>>()
                .iter()
                .map(|ty| ty.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if returned_is_unit {
                String::new()
            } else {
                format!(" {returned_type}")
            }
        );

        let mut body = GoBlock::default();
        let callee = self.fresh_name("host_value_callee");
        body.line(format!("var {callee} {signature}"));
        body.line(format!(
            "{callee} = pkg.host.{}",
            host_member_name(module_path, name)
        ));
        let mut public_arguments = Vec::with_capacity(slots.len());
        let mut value_index = 0usize;
        for (stage_index, stage) in entry.stages().iter().enumerate() {
            let Some(value) = stage.value_stage() else {
                continue;
            };
            let params = groups[stage_index].params();
            public_arguments.extend(self.expand_host_value_stage(
                value,
                &value_stages[value_index].1,
                params,
                &mut body,
            )?);
            value_index += 1;
        }
        if public_arguments.len() != slots.len() {
            unreachable!(
                "the prepared host callable source groups expand to its exact public slot \
                 inventory"
            );
        }
        let call = format!("{callee}({})", public_arguments.join(", "));
        if returned_is_unit {
            body.line(call);
            body.line("return Unit{}");
        } else {
            let returned =
                facade_skin::convert(entry.returned(), &call, FfiDir::In, &mut body, self)
                    .unwrap_or_else(go_skin_invariant);
            body.line(format!("return {returned}"));
        }
        Ok(render_curried_closure(&groups, body))
    }

    fn expand_host_value_stage(
        &mut self,
        stage: &GoLiveValueHeadStage<'_, '_>,
        source_groups: &[super::facade::GoLiveValueSourceGroup<'_, '_, '_>],
        body_arguments: &[String],
        out: &mut GoBlock,
    ) -> Result<Vec<String>, EmitError> {
        if source_groups.len() != body_arguments.len() {
            unreachable!(
                "a prepared host value stage has one body parameter per prepared source group"
            );
        }
        let mut public = Vec::with_capacity(stage.slots().len());
        for (source, body_argument) in source_groups.iter().zip(body_arguments) {
            match source.view() {
                GoLiveValueSourceGroupView::UnitValue => {}
                GoLiveValueSourceGroupView::Identity(slot) => public.push(
                    facade_skin::convert(slot, body_argument, FfiDir::Out, out, self)
                        .unwrap_or_else(go_skin_invariant),
                ),
                GoLiveValueSourceGroupView::RightNest(slots) => {
                    let expanded = self.fresh_name("host_value_slots");
                    out.line(format!(
                        "{expanded} := kioProductSlots({body_argument}, {})",
                        slots.len()
                    ));
                    for (index, slot) in slots.iter().enumerate() {
                        public.push(
                            facade_skin::convert(
                                slot,
                                &format!("{expanded}[{index}]"),
                                FfiDir::Out,
                                out,
                                self,
                            )
                            .unwrap_or_else(go_skin_invariant),
                        );
                    }
                }
            }
        }
        Ok(facade_skin::group_public_arguments(
            &stage
                .public_parameters()
                .unwrap_or_else(go_facade_invariant),
            &public,
        )
        .unwrap_or_else(go_skin_invariant))
    }

    /// Wrap the named method's first stage while retaining its direct-call ABI.
    fn emit_module_fn_value_ref(
        &mut self,
        mangled: &str,
        sig: &crate::ast::Signature<Routed>,
    ) -> Result<String, EmitError> {
        let method = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_method_name(self.module_key, mangled),
        };
        let groups = runtime_param_name_groups(sig);
        let params = groups[0]
            .iter()
            .map(|_| self.fresh_name("arg"))
            .collect::<Vec<_>>();
        let first_is_type = matches!(
            sig.canonical_groups().first(),
            Some(crate::ast::SignatureGroupRef::Type(_))
        );
        let mut body = GoBlock::default();
        body.line(format!(
            "return {}",
            self.emit_method_invocation(&method, &params, true)
        ));
        let group = if first_is_type {
            GoClosureGroup::Type
        } else {
            GoClosureGroup::Value(params)
        };
        let closure = group.render_entry(&body);
        Ok(format!("any({closure})"))
    }

    /// Render each value abstraction over one semantic domain; type binders stay nullary.
    fn emit_fn_expr(
        &mut self,
        sig: &crate::ast::Signature<Routed>,
        body: &Expr<Routed>,
    ) -> Result<String, EmitError> {
        let groups = runtime_param_name_groups(sig);
        let saved_locals = self.locals.len();
        let saved_types = self.type_params.len();
        self.bind_type_params(sig);
        let bound = self.bind_param_groups(&groups);
        let emitted_groups = signature_closure_groups(sig, bound);
        let mut closure_body = GoBlock::default();
        let result = self.emit_result_into(
            TailContext::body(body),
            GoDestination::ReturnStep,
            &mut closure_body,
        );
        self.locals.truncate(saved_locals);
        self.type_params.truncate(saved_types);
        result?;
        Ok(render_curried_entries(&emitted_groups, closure_body))
    }

    /// Complete earlier executing type stages before their results are consumed.
    fn emit_erased_type_applications(
        &mut self,
        mut callee: String,
        count: usize,
        out: &mut GoBlock,
    ) -> String {
        for _ in 0..count {
            let applied = self.fresh_name("type_stage");
            out.line(format!("var {applied} any"));
            out.line(format!("{applied} = kioApplyType({callee})"));
            callee = applied;
        }
        callee
    }

    /// Apply one private value stage after evaluating its arguments in order.
    fn emit_apply(
        &mut self,
        callee: &str,
        args: &[Expr<Routed>],
        tail: bool,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let emitted = args
            .iter()
            .map(|arg| self.emit_value(arg, out))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(go_value_call(
            callee,
            &facade_skin::value_domain(&emitted),
            tail,
        ))
    }

    /// A qualified module call (`import <pkg>/<mod> as a; a.fn(...)`): the
    /// `mangled` is the `<module-key>.<leaf>` qualified form. Resolve the
    /// `.`-joined head to a module key and the leaf to its method.
    fn emit_qualified_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        tail: bool,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        // `mangled` is `<alias>.<leaf>` (the alias is a single segment
        // from `import <pkg>/<mod> as <alias>;`). Resolve the alias to its
        // real module slash-path, then build the module-fn method name.
        let (alias, leaf) = mangled.split_once('.').ok_or_else(|| {
            EmitError::unsupported(format!(
                "Go emitter: qualified module call `{mangled}` is not `<alias>.<leaf>`"
            ))
        })?;
        let module_key = self.qualified_imports.get(alias).ok_or_else(|| {
            EmitError::unsupported(format!(
                "Go emitter: qualified-call alias `{alias}` has no `import … as {alias};` mapping"
            ))
        })?;
        let method = module_fn_method_name(module_key, leaf);
        self.emit_grouped_method_call(&method, type_arg_count, args, sig, tail, out)
    }

    /// Emit a call to a package-handle module-fn method `method` over `args`,
    /// honoring currying: the method binds the fn's **first** value group
    /// and returns nested closures for inner groups; args spanning further
    /// groups are applied to the returned closure group by group. A call
    /// supplying *fewer* args than the first group is a value-leaving
    /// partial: the call is wrapped in a closure that binds the missing
    /// first-group slots. Shared by [`Self::emit_module_call`] and
    /// [`Self::emit_qualified_module_call`].
    fn emit_grouped_method_call(
        &mut self,
        method: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        tail: bool,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let group_arities = value_group_arities(sig);
        let first = group_arities.first().copied().unwrap_or(0);
        let staged_callee = if type_arg_count > 0 {
            if !sig.has_value_group() && tail && type_arg_count == 1 {
                assert!(
                    args.is_empty(),
                    "a type-only direct call has no value arguments"
                );
                return Ok(self.emit_method_invocation(method, &[], true));
            }
            let first_stage = self.fresh_name("type_stage");
            out.line(format!("var {first_stage} any"));
            out.line(format!("{first_stage} = pkg.{method}()"));
            if !sig.has_value_group() && tail {
                let before_final =
                    self.emit_erased_type_applications(first_stage, type_arg_count - 2, out);
                return Ok(format!("kioTailType({before_final})"));
            }
            Some(self.emit_erased_type_applications(first_stage, type_arg_count - 1, out))
        } else {
            None
        };
        let emitted = args
            .iter()
            .map(|arg| self.emit_value(arg, out))
            .collect::<Result<Vec<_>, _>>()?;
        // A direct call carries leading type applications on this node. The
        // method itself is the first nullary type stage; further binders are
        // nullary closures returned from it. Value applications then begin at
        // the first value group.
        if type_arg_count > 0 {
            let mut call = staged_callee.expect("type applications produced a staged callee");
            if !sig.has_value_group() {
                assert!(
                    args.is_empty(),
                    "a routed type-only head has no value arguments"
                );
                return Ok(call);
            }
            if args.len() < first {
                let missing = first - args.len();
                let fresh: Vec<String> = (0..missing).map(|_| self.fresh_name("partial")).collect();
                let mut all_args = emitted.clone();
                all_args.extend(fresh.iter().cloned());
                let mut body = GoBlock::default();
                body.line(format!(
                    "return {}",
                    go_value_call(&call, &facade_skin::value_domain(&all_args), true)
                ));
                let closure = GoClosureGroup::Value(fresh).render_entry(&body);
                let value = format!("any({closure})");
                return Ok(if tail {
                    format!("kioDone({value})")
                } else {
                    value
                });
            }
            let first_args = &emitted[..first];
            call = go_value_call(
                &call,
                &facade_skin::value_domain(first_args),
                tail && first == emitted.len(),
            );
            let mut consumed = first;
            for arity in group_arities.iter().skip(1) {
                if consumed >= emitted.len() {
                    break;
                }
                let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
                call = go_value_call(
                    &call,
                    &facade_skin::value_domain(slice),
                    tail && consumed + slice.len() == emitted.len(),
                );
                consumed += arity;
            }
            return Ok(call);
        }

        // Value-leaving partial: fewer args than the method's first value
        // group, so the call leaves a function value. The method binds the
        // whole first group at once, so wrap it in a closure that binds the
        // already-supplied args and takes the missing first-group slots as
        // fresh params; the method's return then curries any inner groups
        // (the caller applies them to this closure's result).
        if args.len() < first {
            let missing = first - args.len();
            let fresh: Vec<String> = (0..missing).map(|_| self.fresh_name("partial")).collect();
            let mut all_args = emitted.clone();
            all_args.extend(fresh.iter().cloned());
            let mut body = GoBlock::default();
            body.line(format!(
                "return {}",
                self.emit_method_invocation(method, &all_args, true)
            ));
            let closure = GoClosureGroup::Value(fresh).render_entry(&body);
            let value = format!("any({closure})");
            return Ok(if tail {
                format!("kioDone({value})")
            } else {
                value
            });
        }
        let mut call =
            self.emit_method_invocation(method, &emitted[..first], tail && first == emitted.len());
        let mut consumed = first;
        for arity in group_arities.iter().skip(1) {
            if consumed >= emitted.len() {
                break;
            }
            let slice = &emitted[consumed..(consumed + arity).min(emitted.len())];
            call = go_value_call(
                &call,
                &facade_skin::value_domain(slice),
                tail && consumed + slice.len() == emitted.len(),
            );
            consumed += arity;
        }
        Ok(call)
    }

    fn emit_method_invocation(&self, method: &str, args: &[String], tail: bool) -> String {
        if !tail {
            return format!("pkg.{method}({})", args.join(", "));
        }
        let arguments = match args {
            [] => "kioNamedArguments{}".to_owned(),
            [first] => format!("kioNamedArguments{{first: {first}}}"),
            [first, second] => format!("kioNamedArguments{{first: {first}, second: {second}}}"),
            [first, second, rest @ ..] => format!(
                "kioNamedArguments{{first: {first}, second: {second}, rest: []any{{{}}}}}",
                rest.join(", ")
            ),
        };
        format!("kioStep{{owner: pkg, arguments: {arguments}, invoke: kioInvoke_{method}}}")
    }

    /// `LowModuleCall { mangled, args }` → `pkg.<method>(<args>)` where
    /// `<method>` is the resolved module-fn method (use-imported name via
    /// `selective_imports`, else a same-module bare name). The body is uniformly
    /// `any`, so value-args pass through with no narrowing (module-fn
    /// params are all `any`).
    ///
    /// The method takes the fn's **first** value group; inner groups are
    /// returned as nested closures (see [`render_module_fns`]). A call
    /// supplying more args than the first group spans subsequent groups —
    /// each extra group is applied to the returned closure. A call
    /// supplying *fewer* args than the first group is a value-leaving
    /// partial: it is wrapped in a closure binding the missing first-group
    /// slots (see [`Self::emit_grouped_method_call`]).
    fn emit_module_call(
        &mut self,
        mangled: &str,
        type_arg_count: usize,
        args: &[Expr<Routed>],
        sig: &crate::ast::Signature<Routed>,
        tail: bool,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let method = match self.selective_imports.get(mangled) {
            Some(m) => m.clone(),
            None => module_fn_method_name(self.module_key, mangled),
        };
        self.emit_grouped_method_call(&method, type_arg_count, args, sig, tail, out)
    }

    /// Lower an expression-valued conditional into Go statement control
    /// flow targeting the enclosing expression's destination.
    fn emit_conditional(
        &mut self,
        context: TailContext<'_>,
        cond: &Expr<Routed>,
        destination: GoDestination<'_>,
        out: &mut GoBlock,
    ) -> Result<(), EmitError> {
        let c = self.emit_bool(cond, out)?;
        out.open(format!("if {c}"));
        self.emit_result_into(
            context.result_child(ResultChild::ThenBranch),
            destination,
            out,
        )?;
        out.else_open();
        self.emit_result_into(
            context.result_child(ResultChild::ElseBranch),
            destination,
            out,
        )?;
        out.close();
        Ok(())
    }

    /// Render an expression in `Bool` position, narrowing the erased
    /// value to Go `bool`. A bool literal renders directly.
    fn emit_bool(&mut self, e: &Expr<Routed>, out: &mut GoBlock) -> Result<String, EmitError> {
        match e {
            Expr::BoolLit { value, .. } => Ok(format!("{value}")),
            other => {
                let v = self.emit_value(other, out)?;
                Ok(format!("{v}.(bool)"))
            }
        }
    }

    /// A product `(A & B & C)` → the nested-binary erased value
    /// `[]any{a, []any{b, c}}` (the JS backend's representation, in Go's
    /// universal `[]any`). `[0]` is the head slot, `[1]` the nested
    /// remainder. Callable items keep their canonical private representation.
    fn emit_tuple_typed(
        &mut self,
        items: &[Expr<Routed>],
        _synth_ty: &Type<Routed>,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let n = items.len();
        if n == 0 {
            return Ok("(Unit{})".to_owned());
        }
        let item_refs: Vec<&Expr<Routed>> = items.iter().collect();
        if let Some(plan) =
            bound_product_rebuild_plan(&item_refs, NominalValueRepresentation::Retained)
        {
            return self.emit_tuple_with_cached_slots(&item_refs, &plan, out);
        }
        let mut rendered = Vec::with_capacity(n);
        for item in items {
            rendered.push(self.emit_value(item, out)?);
        }
        Ok(Self::emit_tuple_from_rendered(&rendered))
    }

    fn emit_tuple_from_rendered(rendered: &[String]) -> String {
        let n = rendered.len();
        debug_assert!(n > 0);
        let mut acc = rendered[n - 1].clone();
        for head in rendered[..n - 1].iter().rev() {
            acc = format!("[]any{{{head}, {acc}}}");
        }
        format!("any({acc})")
    }

    fn emit_tuple_with_cached_slots(
        &mut self,
        items: &[&Expr<Routed>],
        plan: &ProductRebuildPlan,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let slots_name = self.fresh_name("slots");
        let source = self.local_name(&plan.source);
        out.line(format!(
            "{slots_name} := kioProductSlots({source}, {})",
            plan.source_arity
        ));
        let mut rendered = Vec::with_capacity(plan.slots.len());
        for (i, projected_slot) in plan.slots.iter().enumerate() {
            let value = match projected_slot {
                Some(slot) => format!("{slots_name}[{slot}]"),
                None => self.emit_value(items[i], out)?,
            };
            rendered.push(value);
        }
        Ok(Self::emit_tuple_from_rendered(&rendered))
    }

    /// Project slot `index` (of `arity`) from the erased nested-binary product
    /// with the bounded runtime helper.
    fn emit_project(
        &mut self,
        target: &Expr<Routed>,
        index: usize,
        arity: usize,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let t = self.emit_value(target, out)?;
        Ok(format!("kioProductSlot(({t}), {index}, {arity})"))
    }

    /// Inject `payload` as variant `variant` of `variants`.
    fn emit_inject(
        &mut self,
        payload: &Expr<Routed>,
        variant: usize,
        variants: usize,
        out: &mut GoBlock,
    ) -> Result<String, EmitError> {
        let p = self.emit_value(payload, out)?;
        Ok(format!("kioSumInject({p}, {variant}, {variants})"))
    }

    /// Match on a sum scrutinee by peeling it once to `(variant, payload)`.
    fn emit_match(
        &mut self,
        context: TailContext<'_>,
        scrutinee: &Expr<Routed>,
        arms: &[crate::ast::EnrichedArm<Routed>],
        destination: GoDestination<'_>,
        out: &mut GoBlock,
    ) -> Result<(), EmitError> {
        let n = arms.len();
        if n == 0 {
            unreachable!(
                "Go emitter: EnrichedMatch with no arms; structural_recovery builds every \
                 EnrichedMatch from a sum's variants (recover_either / recover_dynamic_right \
                 yield >= 2 arms) and routes an uninhabited scrutinee through \
                 __absurd__/LowAbsurdCall, so a zero-arm match is unreachable"
            );
        }
        let scrutinee = self.emit_value(scrutinee, out)?;
        let variant = self.fresh_name("variant");
        let payload = self.fresh_name("payload");
        out.line(format!(
            "{variant}, {payload} := kioSumPayload({scrutinee}, {n})"
        ));
        out.open(format!("switch {variant}"));
        for (i, arm) in arms.iter().enumerate() {
            out.line(format!("case {i}:"));
            out.indent();
            let ident = self.bind_local(&arm.param);
            out.line(format!("{ident} := {payload}"));
            out.line(format!("_ = {ident}"));
            self.emit_result_into(
                context.result_child(ResultChild::MatchArm(i)),
                destination,
                out,
            )?;
            self.locals.pop();
            out.dedent();
        }
        out.line("default:");
        out.indent();
        out.line("panic(\"kio erased body: invalid sum variant\")");
        out.dedent();
        out.close();
        Ok(())
    }

    /// `LowHostCall` → `pkg.host.<module>__<member>(<args>)`. The opaque
    /// prepared value heads map source ABI arguments through semantic slots;
    /// each source is evaluated once, then Unit/identity/right-nest groups
    /// expand through facade conversion in order. Raw expression `sig` and
    /// `ret_ty` remain irrelevant to public grouping and conversion. A
    /// Unit-returning Host method runs as a statement before the erased body
    /// receives `Unit{}`.
    fn emit_low_host_call(
        &mut self,
        name: &str,
        module_path: &str,
        args: &[Expr<Routed>],
        destination: GoDestination<'_>,
        out: &mut GoBlock,
    ) -> Result<(), EmitError> {
        let catalog = self.facades;
        let site_id = host_site_id(module_path, name);
        let site = catalog.site(&site_id).unwrap_or_else(|| {
            unreachable!(
                "the Routed host call `{module_path}.{name}` must have a prepared live facade site"
            )
        });
        let live = site.live().unwrap_or_else(|| {
            unreachable!("a Routed host call cannot select a retained facade site")
        });
        let entry = live.callable_entry().unwrap_or_else(go_facade_invariant);
        let slots = entry
            .stages()
            .iter()
            .filter_map(GoLiveHeadStage::value_stage)
            .flat_map(|stage| {
                stage
                    .public_parameters()
                    .unwrap_or_else(go_facade_invariant)
            })
            .collect::<Vec<_>>();
        let mut expected_source_arguments = 0usize;
        for stage in entry.stages() {
            if let Some(type_stage) = stage.type_stage() {
                // A direct host call already consumed its source type
                // application, while the public Host method erases it. Still
                // validate the exact opaque live action before treating that
                // stage as declaration-erased.
                type_stage.require_invoke_nullary();
            } else if let Some(value_stage) = stage.value_stage() {
                expected_source_arguments += value_stage
                    .source_groups()
                    .unwrap_or_else(go_facade_invariant)
                    .len();
            }
        }
        if args.len() != expected_source_arguments {
            unreachable!(
                "the Routed host call `{module_path}.{name}` has {} source arguments but its \
                 prepared callable layout has {expected_source_arguments}",
                args.len()
            );
        }

        let returned_type = entry
            .returned()
            .boundary_type()
            .unwrap_or_else(go_facade_invariant);
        let returned_is_unit = matches!(
            entry.returned().view().unwrap_or_else(go_facade_invariant),
            super::facade::GoLiveFacadeUseView::Unit
        );
        let slot_types = slots
            .iter()
            .map(|slot| slot.boundary_type().unwrap_or_else(go_facade_invariant))
            .collect::<Vec<_>>();
        let signature = format!(
            "func({}){}",
            slot_types
                .iter()
                .map(|ty| ty.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            if returned_is_unit {
                String::new()
            } else {
                format!(" {returned_type}")
            }
        );
        let callee = self.fresh_name("host_callee");
        out.line(format!("var {callee} {signature}"));
        out.line(format!(
            "{callee} = pkg.host.{}",
            host_member_name(module_path, name)
        ));

        let mut arguments = Vec::with_capacity(slots.len());
        let mut source_offset = 0usize;
        for stage in entry.stages() {
            let Some(value_stage) = stage.value_stage() else {
                continue;
            };
            let source_groups = value_stage
                .source_groups()
                .unwrap_or_else(go_facade_invariant);
            let end = source_offset + source_groups.len();
            let mut body_arguments = Vec::with_capacity(source_groups.len());
            for argument in &args[source_offset..end] {
                body_arguments.push(self.emit_value(argument, out)?);
            }
            arguments.extend(self.expand_host_value_stage(
                value_stage,
                &source_groups,
                &body_arguments,
                out,
            )?);
            source_offset = end;
        }
        if source_offset != args.len() || arguments.len() != slots.len() {
            unreachable!(
                "the prepared Go host-call layout consumes its exact source and public \
                 inventories"
            );
        }
        let call = format!("{callee}({})", arguments.join(", "));
        if returned_is_unit {
            out.line(call);
            destination.write(out, "Unit{}");
        } else {
            let converted = facade_skin::convert(entry.returned(), &call, FfiDir::In, out, self)
                .unwrap_or_else(go_skin_invariant);
            destination.write(out, converted);
        }
        Ok(())
    }

    /// Resolve a role-typed atom `Type::Path` to its `Role` via the role
    /// table, or `None` for a non-role / structural / unit type. Host-type
    /// identity is the Routed path's exact `(module, name)` pair.
    fn atom_role_of(&self, ty: &Type<Routed>) -> Option<Role> {
        exact_host_role(self.roles, ty)
    }

    /// Render an integer literal at its annotated role's Go type. The
    /// role comes from the checked annotation required by Routed.
    fn emit_int_lit(&self, digits: &str, annotation: &Type<Routed>) -> Result<String, EmitError> {
        let role = self.literal_role(annotation)?;
        Ok(go_typed_int(role_to_go_type(role), digits))
    }

    /// Render a float literal at its annotated role's Go type.
    fn emit_float_lit(&self, digits: &str, annotation: &Type<Routed>) -> Result<String, EmitError> {
        let role = self.literal_role(annotation)?;
        Ok(format!("{}({digits})", role_to_go_type(role)))
    }

    /// The role a literal's annotation resolves to. The annotation is a
    /// `Type::Path` to a role-typed host type.
    fn literal_role(&self, annotation: &Type<Routed>) -> Result<Role, EmitError> {
        Ok(self.atom_role_of(annotation).expect(
            "Go emitter: check_literal_annotation established an exact role-typed host annotation",
        ))
    }
}

/// Render a Go integer-typed literal. `*big.Int` (the `i128` / `u128`
/// roles) has no literal syntax and `big.NewInt` only takes an `int64`,
/// which a full-width i128 / u128 literal overflows; so a big.Int
/// literal is parsed from its exact decimal text via `SetString`, which
/// is correct at every magnitude. A fixed-width role renders as a direct
/// conversion.
fn go_typed_int(go_ty: &str, digits: &str) -> String {
    if go_ty == "*big.Int" {
        // `SetString` returns `(*big.Int, bool)`; the text is emitter-
        // controlled decimal, so the parse cannot fail — an IIFE drops
        // the ok flag and yields the value.
        format!(
            "func() *big.Int {{ v, _ := new(big.Int).SetString(\"{digits}\", 10); return v }}()"
        )
    } else {
        format!("{go_ty}({digits})")
    }
}

/// Render a Kio string value as a Go double-quoted string literal,
/// escaping the characters Go's lexer requires.
fn go_string_lit(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Go local identifier for a Kio binder `name`. Prefixed with `k_`
/// so it never collides with a Go keyword or the emitter's own `pkg` /
/// `host` identifiers, and so a Kio name spelled like a Go reserved word
/// (`type`, `range`, …) stays valid.
fn go_local_ident(name: &str) -> String {
    format!("k_{name}")
}

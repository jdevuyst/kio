use super::*;
use crate::pass::full::FullPipeline;
use crate::pipeline::Pipeline;
use std::path::{Path, PathBuf};

fn check(source: &str) -> Result<(), String> {
    let parsed = crate::pass::parser::parse(source).map_err(|e| format!("{e:?}"))?;
    let (modules, _) = FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), parsed)], None)
        .map_err(|e| format!("{:?}", e.error))?;
    let package =
        Package::build(Path::new(""), modules, None).map_err(|e| format!("{:?}", e.error))?;
    package
        .resolve_imports()
        .map_err(|e| format!("{:?}", e.error))?;
    check_package(&package)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.error))
}

fn prefix(marked: bool, required: bool, error: bool) -> String {
    let target = if required { "__Type__" } else { "." };
    let capabilities = if marked { "fills: __Fill_ctx__," } else { "" };
    let result = if marked {
        "__Checked_term__ & __Fill_ctx__"
    } else {
        "__Checked_term__"
    };
    let term = if error {
        "__macro_error__(ct, __\"implementation executed\"__(ct))"
    } else {
        "__term_unit__(ct)"
    };
    let body = if marked {
        format!("({term}, fills)")
    } else {
        term.to_owned()
    };
    let schedule = if marked { "impl(fills)" } else { "impl" };
    format!(
        "mod main; import __comptime__;
        pure fn build(ct: __Comptime__, {capabilities} _target: {target}, _unit: __Checked_term__) -> {result} {{ {body} }}
        macro produce : [R] . -> R {{ {schedule} build }}
"
    )
}

#[test]
fn ordinary_natural_result_ignores_target_then_validates_it() {
    let source = prefix(false, false, false);
    check(&format!(
        "{source}
        fn inferred() {{ let value = produce(()); value }}
        fn expected() -> . {{ produce(()) }}
        fn written() -> . {{ produce[.](()) }}"
    ))
    .unwrap();
    for body in ["produce(())", "produce[Token](())"] {
        let error = check(&format!(
            "{source} type {{ Token: . {{ mk; get }} }} fn wrong() -> Token {{ {body} }}"
        ))
        .unwrap_err();
        assert!(
            error.contains("returned") || error.contains("type mismatch"),
            "{error}"
        );
    }
}

#[test]
fn marked_natural_result_uses_closed_return_after_producers() {
    let source = prefix(true, false, false);
    check(&format!(
        "{source}
        fn inferred() {{ let value = produce(()); value }}
        fn expected() -> . {{ produce(()) }}
        fn written() -> . {{ produce[.](()) }}"
    ))
    .unwrap();
    let error = check(&format!(
        "{source} type {{ Token: . {{ mk; get }} }} fn wrong() -> Token {{ produce(()) }}"
    ))
    .unwrap_err();
    assert!(
        error.contains("type mismatch") || error.contains("expected"),
        "{error}"
    );
}

#[test]
fn required_results_wait_even_when_implementation_ignores_target() {
    for marked in [false, true] {
        let source = prefix(marked, true, true);
        let absent = check(&format!(
            "{source} fn absent() {{ let value = produce(()); value }}"
        ))
        .unwrap_err();
        assert!(absent.contains("cannot infer"), "{absent}");
        assert!(!absent.contains("implementation executed"), "{absent}");
        let present = check(&format!("{source} fn present() -> . {{ produce(()) }}")).unwrap_err();
        assert!(present.contains("implementation executed"), "{present}");
        let accepted = prefix(marked, true, false);
        check(&format!(
            "{accepted} fn present() -> . {{ produce(()) }} fn written() {{ produce[.](()) }}"
        ))
        .unwrap();
    }
}

#[test]
fn natural_eligibility_follows_alias_expansion_and_lexical_binders() {
    let declarations = "mod main; import __comptime__;
        type Ignore[T] = .; type { Box[T]: . { mk; get } }";
    let implementation = "pure fn build(ct: __Comptime__, _result: ., _value: __Checked_term__) -> __Checked_term__ { __term_unit__(ct) }";
    for signature in [
        "[R] Ignore[R] -> R",
        "[R] ([R] R -> R) -> R",
        "[*F] . -> F[.]",
    ] {
        check(&format!(
            "{declarations} {implementation} macro produce : {signature} {{ impl build }}"
        ))
        .unwrap();
    }
    for signature in [
        "[R] R -> R",
        "[R] Box[R] -> R",
        "[R] . -> .",
        "[R] ([A] R -> A) -> R",
        "[*F] F[.] -> .",
    ] {
        let error = check(&format!(
            "{declarations} {implementation} macro produce : {signature} {{ impl build }}"
        ))
        .unwrap_err();
        assert!(
            error.contains("ABI expects") && !error.contains("value parameters"),
            "{signature}: {error}"
        );
    }
    check(&format!(
        "{declarations} {implementation} macro produce : [R] Ignore[R] -> R {{ impl build }} fn run() {{ let value = produce(()); value }}"
    )).unwrap();
}

#[test]
fn optional_result_transport_is_rejected_in_both_schedules() {
    for marked in [false, true] {
        let source = prefix(marked, false, false).replace("_target: .", "_target: __Type__ | .");
        let error = check(&source).unwrap_err();
        assert!(
            error.contains("ABI expects") && !error.contains("value parameters"),
            "{error}"
        );
    }
}

#[test]
fn mixed_result_modes_preserve_required_readiness_and_validation() {
    for marked in [false, true] {
        let capabilities = if marked { "fills: __Fill_ctx__," } else { "" };
        let result = if marked {
            "__Checked_term__ & __Fill_ctx__"
        } else {
            "__Checked_term__"
        };
        let term = "__intrinsic_pair__(ct, __term_unit__(ct), __term_unit__(ct))";
        let body = if marked {
            format!("({term}, fills)")
        } else {
            term.to_owned()
        };
        let schedule = if marked { "impl(fills)" } else { "impl" };
        let source = format!("mod main; import __comptime__;
            pure fn build(ct: __Comptime__, {capabilities} _natural: ., _required: __Type__, _unit: __Checked_term__) -> {result} {{ {body} }}
            macro produce : [N][R] . -> (N & R) {{ {schedule} build }}");
        check(&format!(
            "{source} fn run() {{ let value = produce[_][.](()); let _ = value; () }}"
        ))
        .unwrap();
        let missing = check(&format!(
            "{source} fn run() {{ let value = produce[_][_](()); let _ = value; () }}"
        ))
        .unwrap_err();
        assert!(missing.contains("cannot infer"), "{missing}");
        let wrong = check(&format!("{source} type {{ Token: . {{ mk; get }} }} fn run() {{ let value = produce[_][Token](()); let _ = value; () }}")).unwrap_err();
        assert!(
            wrong.contains("type mismatch")
                || wrong.contains("returned")
                || wrong.contains("expected"),
            "{wrong}"
        );
    }
}

#[test]
fn natural_result_preserves_residual_input_binders_and_values() {
    for marked in [false, true] {
        let capabilities = if marked { "fills: __Fill_ctx__," } else { "" };
        let result = if marked {
            "__Checked_term__ & __Fill_ctx__"
        } else {
            "__Checked_term__"
        };
        let body = if marked {
            "(__term_unit__(ct), fills)"
        } else {
            "__term_unit__(ct)"
        };
        let schedule = if marked { "impl(fills)" } else { "impl" };
        check(&format!("mod main; import __comptime__;
            pure fn build(ct: __Comptime__, {capabilities} _natural: ., _unit: __Checked_term__, _input: __Type__, _second: __Checked_term__) -> {result} {{ {body} }}
            macro produce : [R] . -> [A] A -> R {{ {schedule} build }}
            fn partial() -> [A] A -> . {{ produce[.](()) }}
            fn run() {{ partial()[.](()) }}")).unwrap();
    }
}

#[test]
fn whole_result_fill_is_immutable_and_discarded_entries_have_no_effect() {
    let prefix = "mod main; import __comptime__;
        type { Token: . { mk; get } }
        pure fn build(ct: __Comptime__, fills: __Fill_ctx__, _result: ., _unit: __Checked_term__) -> (__Checked_term__ & __Fill_ctx__) {
            let appended = __fill_result__(ct, fills, __reflect_type__(ct)[Token]);
            (__term_unit__(ct), RETURNED)
        }
        macro produce : [R] . -> R { impl(fills) build }
        fn witness() { let value = produce(()); value }";
    check(&prefix.replace("RETURNED", "fills")).unwrap();
    let error = check(&prefix.replace("RETURNED", "appended")).unwrap_err();
    assert!(error.contains("type mismatch"), "{error}");
}

fn whole_result_common_prefix() -> &'static str {
    "mod main; import __comptime__; import __intrinsics__;
     pure fn build(ct: __Comptime__, fills: __Fill_ctx__,
                   _a: __Type__, first: __Checked_term__,
                   _b: __Type__, second: __Checked_term__, _result: .)
         -> (__Checked_term__ & __Fill_ctx__) {
       let left = __term_call__(ct, __term_type__(ct, first), first, __term_unit__(ct));
       let right = __term_call__(ct, __term_type__(ct, second), second, __term_unit__(ct));
       let one = __fill_result__(ct, fills, __term_type__(ct, left));
       let both = __fill_result__(ct, one, __term_type__(ct, right));
       (left, both)
     }
     macro common : [A] A -> [B] B -> [R] R { impl(fills) build }
     fn known(_u: .) -> (. | .) { __right__(()) }"
}

#[test]
fn whole_result_fill_preserves_context_and_symmetric_candidate_producers() {
    let prefix = whole_result_common_prefix();
    for call in [
        "common(.(_u: .) { __left__(()) })(known)",
        "common(known)(.(_u: .) { __left__(()) })",
    ] {
        check(&format!(
            "{prefix} fn witness() {{ let result = {call}; let .(confirmed: . | .) = result; () }}"
        ))
        .unwrap();
    }
    check(&format!("{prefix} fn witness() -> (. | .) {{ common(.(_u: .) {{ __left__(()) }})(.(_u: .) {{ __right__(()) }}) }}")).unwrap();
    let all_open = check(&format!("{prefix} fn witness() {{ let result = common(.(_u: .) {{ __left__(()) }})(.(_u: .) {{ __left__(()) }}); let _ = result; () }}")).unwrap_err();
    assert!(all_open.contains("cannot infer"), "{all_open}");
}

#[test]
fn whole_result_fill_does_not_skip_an_ignored_candidate_body() {
    let prefix = whole_result_common_prefix().replace("(left, both)", "(__term_unit__(ct), both)");
    let error = check(&format!(
        "{prefix}
        type {{ Token: . {{ mk; get }} }}
        fn witness() {{ common(.(_u: .) -> . {{ () }})(.(_u: .) -> . {{ Token.mk(()) }}) }}"
    ))
    .unwrap_err();
    assert!(
        error.contains("type mismatch") && error.contains("Token"),
        "{error}"
    );
}

#[test]
fn whole_result_fill_and_destination_fills_share_one_result_relation() {
    for reverse in [false, true] {
        for conflict in [false, true] {
            let candidate = if conflict {
                "__reflect_type__(ct)[Token]"
            } else {
                "__type_unit__(ct)"
            };
            let relations = if reverse {
                format!(
                    "__fill__(ct, __fill_result__(ct, fills, {candidate}), target, __type_unit__(ct))"
                )
            } else {
                format!(
                    "__fill_result__(ct, __fill__(ct, fills, target, __type_unit__(ct)), {candidate})"
                )
            };
            let source = format!("mod main; import __comptime__;
                type {{ Token: . {{ mk; get }} }}
                pure fn build(ct: __Comptime__, fills: __Fill_ctx__, target: __Type__, callback: __Checked_term__)
                    -> (__Checked_term__ & __Fill_ctx__) {{
                    (__term_call__(ct, __term_type__(ct, callback), callback, __term_unit__(ct)), {relations})
                }}
                macro mixed : [R] (. -> R) -> R {{ impl(fills) build }}
                fn witness() {{ let value = mixed(.(_u: .) {{ () }}); value }}");
            let result = check(&source);
            if conflict {
                let error = result.unwrap_err();
                assert!(error.contains("type mismatch"), "{error}");
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn whole_result_fill_uses_the_terminal_type_beneath_residual_binders() {
    check("mod main; import __comptime__;
        pure fn build(ct: __Comptime__, fills: __Fill_ctx__, _result: ., _unit: __Checked_term__, _input: __Type__, _value: __Checked_term__)
            -> (__Checked_term__ & __Fill_ctx__) {
            (__term_unit__(ct), __fill_result__(ct, fills, __type_unit__(ct)))
        }
        macro produce : [R] . -> [A] A -> R { impl(fills) build }
        fn partial() -> [A] A -> . { produce[.](()) }
        fn witness() { partial()[.](()) }").unwrap();
    check("mod main; import __comptime__;
        pure fn build(ct: __Comptime__, fills: __Fill_ctx__, _result: ., _unit: __Checked_term__, _input: __Type__, value: __Checked_term__)
            -> (__Checked_term__ & __Fill_ctx__) {
            let pair = __intrinsic_pair__(ct, __term_unit__(ct), value);
            (pair, __fill_result__(ct, fills, __term_type__(ct, pair)))
        }
        macro produce : [R] . -> [A] A -> (R & A) { impl(fills) build }
        fn partial() -> [A] A -> (. & A) { produce[.](()) }
        fn witness() { let _ = partial()[.](()); () }").unwrap();
}

#[test]
fn whole_result_fill_preserves_rigid_higher_kind_and_forall_candidates() {
    check("mod main; import __comptime__;
        pure fn build(ct: __Comptime__, fills: __Fill_ctx__, _input: __Type__, value: __Checked_term__, _result: .)
            -> (__Checked_term__ & __Fill_ctx__) {
            (value, __fill_result__(ct, fills, __term_type__(ct, value)))
        }
        macro pass : [T] T -> [R] R { impl(fills) build }
        fn rigid[A](value: A) -> A { pass(value) }
        fn higher[*F][A](value: F[A]) -> F[A] { pass(value) }
        fn product[A](value: A) -> (A & .) { pass((value, ())) }
        fn id[T](value: T) -> T { value }
        fn scheme() -> [T] T -> T { pass(id)[_] }").unwrap();
}

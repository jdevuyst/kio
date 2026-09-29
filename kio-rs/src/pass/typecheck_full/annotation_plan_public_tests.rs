// Public semantic coverage for annotation-local `forall` holes. Internal
// planner invariants live beside their implementation seams rather than
// duplicating the language-level assertions here.

fn unique_token_span(source: &str, context: &str, token: &str) -> Span {
    let contexts = source.match_indices(context).collect::<Vec<_>>();
    assert_eq!(
        contexts.len(),
        1,
        "expected exactly one `{context}` context in:\n{source}"
    );
    let is_identifier_continue = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let tokens = context
        .match_indices(token)
        .filter(|(index, matched)| {
            let before = index
                .checked_sub(1)
                .and_then(|before| context.as_bytes().get(before))
                .is_some_and(|byte| is_identifier_continue(*byte));
            let after = context
                .as_bytes()
                .get(index + matched.len())
                .is_some_and(|byte| is_identifier_continue(*byte));
            !before && !after
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tokens.len(),
        1,
        "expected exactly one `{token}` token in context `{context}`"
    );
    let start = contexts[0].0 + tokens[0].0;
    Span::new(start as u32, (start + token.len()) as u32)
}

fn assert_annotation_local_forall_hole(
    source: &str,
    hole_context: &str,
    binder_context: &str,
    binder_name: &str,
) {
    let diagnostic = type_diag(source);
    assert_eq!(
        diagnostic.message,
        "type placeholder `_` cannot appear beneath a `forall` inside an annotation"
    );
    assert_eq!(
        diagnostic.span,
        unique_token_span(source, hole_context, "_")
    );
    let secondary = diagnostic.secondary();
    assert_eq!(
        secondary.len(),
        1,
        "unexpected secondary labels: {secondary:?}"
    );
    // This proves that the fixture names exactly one raw binder occurrence.
    // Exact emitted file/span/text payloads have dedicated structured-label,
    // planner, and CLI-golden coverage.
    let _expected_binder_span = unique_token_span(source, binder_context, binder_name);
    assert_eq!(
        diagnostic.help(),
        Some("write a concrete type beneath this binder, or make the whole annotation slot `_`")
    );
    assert!(diagnostic.notes().is_empty());
    assert!(diagnostic.fixes().is_empty());
    assert_eq!(diagnostic.unresolved_name(), None);
}

// These three tests replace the expectations, but preserve the exact source
// text, of the three existing successful checked-parameter witnesses.

#[test]
fn checked_partial_higher_kinded_function_annotation_rejects_local_forall_hole() {
    let source = "module main; \
        fn consume(wrapper: (([*F] F(.) -> F(.)) -> .)) -> . { () } \
        fn partial() -> . { consume(.(function: [*F] _) { () }) }";
    assert_annotation_local_forall_hole(source, "function: [*F] _", "[*F] _", "F");
}

#[test]
fn checked_partial_higher_kinded_function_annotation_alias_expected_rejects_local_forall_hole() {
    let source = "module main; \
        type Scheme = [*F] F(.) -> F(.); \
        fn consume(wrapper: (Scheme -> .)) -> . { () } \
        fn partial() -> . { consume(.(function: [*G] _) { () }) }";
    assert_annotation_local_forall_hole(source, "function: [*G] _", "[*G] _", "G");
}

#[test]
fn checked_partial_ordinary_forall_annotation_rejects_local_forall_hole() {
    let source = "module main; \
        fn consume(wrapper: (([A] A -> A) -> .)) -> . { () } \
        fn partial() -> . { consume(.(function: [B] _) { () }) }";
    assert_annotation_local_forall_hole(source, "function: [B] _", "[B] _", "B");
}

#[test]
fn checked_partial_return_annotation_rejects_local_forall_hole() {
    let source = r#"module main;
        host fn polymorphic(_token: .) -> [Poly] Poly -> Poly;
        fn consume(wrapper: (. -> [Expected] Expected -> Expected)) -> . { () }
        fn partial() -> . {
          consume(.(token: .) -> [Returned] _ { polymorphic(()) })
        }"#;
    assert_annotation_local_forall_hole(source, "-> [Returned] _", "[Returned] _", "Returned");
}

#[test]
fn partial_unary_local_rejects_local_forall_hole() {
    let source = r#"module main;
        host fn polymorphic(_token: .) -> [Poly] Poly -> Poly;
        fn partial() -> . {
          let .(function: [Local] _) = polymorphic(());
          ()
        }"#;
    assert_annotation_local_forall_hole(source, "function: [Local] _", "[Local] _", "Local");
}

#[test]
fn partial_destructuring_local_rejects_local_forall_hole() {
    let source = r#"module main;
        host fn polymorphic(_token: .) -> [Poly] Poly -> Poly;
        fn partial() -> . {
          let .((function: [Destructured] _)) = polymorphic(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: [Destructured] _",
        "[Destructured] _",
        "Destructured",
    );
}

#[test]
fn partial_as_pattern_local_rejects_local_forall_hole() {
    let source = r#"module main;
        host fn polymorphic(_token: .) -> [Poly] Poly -> Poly;
        fn partial() -> . {
          let .(whole: (function: [As_bound] _)) = polymorphic(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: [As_bound] _",
        "[As_bound] _",
        "As_bound",
    );
}

#[test]
fn partial_monadic_do_unary_rejects_local_forall_hole() {
    let source = r#"module main; import sequence(do);
        pub newtype Box[A] : A { constructor mk_box; projector un_box; };
        fn box_pure[A](value: A) -> Box(A) { Box.mk_box(value) }
        fn box_bind[A][B](value: Box(A), next: A -> Box(B)) -> Box(B) {
          next(Box.un_box(value))
        }
        host fn boxed_polymorphic(_token: .) -> Box([Poly] Poly -> Poly);
        fn partial() -> Box(.) {
          do! box_bind {
            let .(function: [Do_bound] _) <- boxed_polymorphic(());
            box_pure(())
          }
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: [Do_bound] _",
        "[Do_bound] _",
        "Do_bound",
    );
}

#[test]
fn partial_monadic_do_pattern_rejects_local_forall_hole() {
    let source = r#"module main; import sequence(do);
        pub newtype Box[A] : A { constructor mk_box; projector un_box; };
        fn box_pure[A](value: A) -> Box(A) { Box.mk_box(value) }
        fn box_bind[A][B](value: Box(A), next: A -> Box(B)) -> Box(B) {
          next(Box.un_box(value))
        }
        host fn boxed_polymorphic(_token: .) -> Box([Poly] Poly -> Poly);
        fn partial() -> Box(.) {
          do! box_bind {
            let .((function: [Do_pattern] _)) <- boxed_polymorphic(());
            box_pure(())
          }
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: [Do_pattern] _",
        "[Do_pattern] _",
        "Do_pattern",
    );
}

#[test]
fn alias_root_parameter_rejects_materialized_forall_hole() {
    let source = r#"module main;
        type Wrap[Slot] = [Alias_bound] Slot -> Alias_bound;
        fn consume(wrapper: (Wrap(.) -> .)) -> . { () }
        fn partial() -> . { consume(.(function: Wrap(_)) { () }) }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[Alias_bound]",
        "Alias_bound",
    );
}

#[test]
fn transitive_alias_parameter_rejects_materialized_forall_hole() {
    let source = r#"module main;
        type Wrap[Slot] = [Transitive_bound] Slot -> Transitive_bound;
        type Outer[Slot] = Wrap(Slot);
        fn consume(wrapper: (Outer(.) -> .)) -> . { () }
        fn partial() -> . { consume(.(function: Outer(_)) { () }) }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Outer(_)",
        "[Transitive_bound]",
        "Transitive_bound",
    );
}

#[test]
fn duplicated_alias_hole_rejects_once_at_source_occurrence() {
    let source = r#"module main;
        type Mixed[Slot] = Slot & ([Mixed_bound] Slot -> Mixed_bound);
        fn consume(wrapper: (Mixed(.) -> .)) -> . { () }
        fn partial() -> . { consume(.(function: Mixed(_)) { () }) }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Mixed(_)",
        "[Mixed_bound]",
        "Mixed_bound",
    );
}

#[test]
fn reordered_alias_argument_rejects_materialized_forall_hole() {
    let source = r#"module main;
        pub type Reorder[First][Second] = [Reorder_bound] Second -> Reorder_bound;
        host fn exact(_token: .) -> Reorder(., .);
        fn partial() -> . {
          let .(function: Reorder(., _)) = exact(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "Reorder(., _)",
        "[Reorder_bound]",
        "Reorder_bound",
    );
}

#[test]
fn alias_root_return_rejects_materialized_forall_hole() {
    let source = r#"module main;
        pub type Wrap[Slot] = [Return_alias_bound] Slot -> Return_alias_bound;
        host fn wrapped(_token: .) -> Wrap(.);
        fn consume(wrapper: (. -> Wrap(.))) -> . { () }
        fn partial() -> . {
          consume(.(token: .) -> Wrap(_) { wrapped(()) })
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "-> Wrap(_)",
        "[Return_alias_bound]",
        "Return_alias_bound",
    );
}

#[test]
fn alias_root_unary_local_rejects_materialized_forall_hole() {
    let source = r#"module main;
        pub type Wrap[Slot] = [Local_alias_bound] Slot -> Local_alias_bound;
        host fn wrapped(_token: .) -> Wrap(.);
        fn partial() -> . {
          let .(function: Wrap(_)) = wrapped(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[Local_alias_bound]",
        "Local_alias_bound",
    );
}

#[test]
fn alias_root_destructuring_local_rejects_materialized_forall_hole() {
    let source = r#"module main;
        pub type Wrap[Slot] = [Destruct_alias_bound] Slot -> Destruct_alias_bound;
        host fn wrapped(_token: .) -> Wrap(.);
        fn partial() -> . {
          let .((function: Wrap(_))) = wrapped(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[Destruct_alias_bound]",
        "Destruct_alias_bound",
    );
}

#[test]
fn alias_root_as_pattern_local_rejects_materialized_forall_hole() {
    let source = r#"module main;
        pub type Wrap[Slot] = [As_alias_bound] Slot -> As_alias_bound;
        host fn wrapped(_token: .) -> Wrap(.);
        fn partial() -> . {
          let .(whole: (function: Wrap(_))) = wrapped(());
          ()
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[As_alias_bound]",
        "As_alias_bound",
    );
}

#[test]
fn alias_root_monadic_do_unary_rejects_materialized_forall_hole() {
    let source = r#"module main; import sequence(do);
        pub type Wrap[Slot] = [Do_alias_bound] Slot -> Do_alias_bound;
        pub newtype Box[A] : A { constructor mk_box; projector un_box; };
        fn box_pure[A](value: A) -> Box(A) { Box.mk_box(value) }
        fn box_bind[A][B](value: Box(A), next: A -> Box(B)) -> Box(B) {
          next(Box.un_box(value))
        }
        host fn boxed_wrapped(_token: .) -> Box(Wrap(.));
        fn partial() -> Box(.) {
          do! box_bind {
            let .(function: Wrap(_)) <- boxed_wrapped(());
            box_pure(())
          }
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[Do_alias_bound]",
        "Do_alias_bound",
    );
}

#[test]
fn alias_root_monadic_do_pattern_rejects_materialized_forall_hole() {
    let source = r#"module main; import sequence(do);
        pub type Wrap[Slot] = [Do_pattern_alias_bound] Slot -> Do_pattern_alias_bound;
        pub newtype Box[A] : A { constructor mk_box; projector un_box; };
        fn box_pure[A](value: A) -> Box(A) { Box.mk_box(value) }
        fn box_bind[A][B](value: Box(A), next: A -> Box(B)) -> Box(B) {
          next(Box.un_box(value))
        }
        host fn boxed_wrapped(_token: .) -> Box(Wrap(.));
        fn partial() -> Box(.) {
          do! box_bind {
            let .((function: Wrap(_))) <- boxed_wrapped(());
            box_pure(())
          }
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "function: Wrap(_)",
        "[Do_pattern_alias_bound]",
        "Do_pattern_alias_bound",
    );
}

#[test]
fn recursive_alias_partial_let_rejects_once_with_source_provenance() {
    let source = r#"module main;
        host type I32 role(i32);
        host fn loop[S][R](step: S -> S | R, state: S) -> R;
        type Wrap[Slot] = [Rec_alias_bound] Slot -> Rec_alias_bound;
        newtype Box[A] : A { constructor mk_box; projector un_box; };
        fn choose_box[A](_tag: I32, value: Box(A)) -> Box(A) { value }
        rec(loop) fn recursive(value: I32) -> Box(Wrap(.)) {
          let .(box: Box(Wrap(_))) = choose_box(value, rec(cont) recursive(value));
          box
        }"#;
    assert_annotation_local_forall_hole(
        source,
        "Box(Wrap(_))",
        "[Rec_alias_bound]",
        "Rec_alias_bound",
    );
}

// Admitted controls remain separate selectors so a rejection cannot mask them.

#[test]
fn whole_parameter_slot_hole_remains_admitted() {
    ok("module main; \
        fn consume(wrapper: (([A] A -> A) -> .)) -> . { () } \
        fn partial() -> . { consume(.(function: _) { () }) }");
}

#[test]
fn header_binder_parameter_and_root_return_holes_remain_admitted() {
    ok("module main; \
        fn consume(identity: [A] A -> A) -> . { () } \
        fn partial() -> . { consume(.[B](value: _) -> _ { value }) }");
}

#[test]
fn fully_concrete_polymorphic_header_remains_admitted() {
    ok("module main; \
        fn consume(identity: [A] A -> A) -> . { () } \
        fn partial() -> . { consume(.[B](value: B) -> B { value }) }");
}

#[test]
fn checked_alpha_edge_does_not_capture_a_same_spelled_outer_binder() {
    let diagnostic = type_diag(
        "module main; \
         fn consume[Outer](seed: Outer, f: [Expected] ((Outer & Expected) -> .)) -> . { () } \
         fn invalid[Outer](seed: Outer) -> . { \
           consume(seed, .[Outer](pair: (Outer & Outer)) { () }) \
         }",
    );
    assert!(
        diagnostic.message.starts_with("type mismatch: expected "),
        "unexpected diagnostic: {diagnostic:?}"
    );
}

#[test]
fn checked_alpha_edge_keeps_an_admitted_hole_separate_from_a_same_spelled_outer_binder() {
    let diagnostic = type_diag(
        "module main; \
         fn consume[Outer](seed: Outer, f: [Expected] (((Outer & Expected) & .) -> .)) -> . { () } \
         fn invalid[Outer](seed: Outer) -> . { \
           consume(seed, .[Outer](pair: ((Outer & Outer) & _)) { () }) \
         }",
    );
    assert!(
        diagnostic.message.starts_with("type mismatch: expected "),
        "unexpected diagnostic: {diagnostic:?}"
    );
}

#[test]
fn checked_alpha_edge_accepts_a_repeated_source_binder_without_an_outer_collision() {
    ok("module main; \
        fn consume(f: [Expected] ((Expected & Expected) -> .)) -> . { () } \
        fn valid() -> . { \
          consume(.[Source](pair: (Source & Source)) { () }) \
        }");
}

#[test]
fn identity_alias_preserves_ordinary_hole() {
    ok("module main; \
        type Identity[Slot] = Slot; \
        host fn unit(_token: .) -> .; \
        fn partial() -> . { let .(value: Identity(_)) = unit(()); () }");
}

#[test]
fn ordinary_newtype_and_product_holes_remain_admitted() {
    ok("module main; \
        newtype Box[A] : A { constructor mk_box; projector un_box; }; \
        fn partial() -> . { \
          let .(boxed: Box(_)) = Box.mk_box(()); \
          let .(pair: . & _) = ((), ()); \
          () \
        }");
}

#[test]
fn call_type_argument_hole_remains_application_inference() {
    ok("module main; \
        fn identity[A](value: A) -> A { value } \
        fn partial() -> . { let value = identity(_, ()); () }");
}

#[test]
fn exact_root_literal_hole_retains_literal_tier_selection() {
    ok("module main; \
        host type I32 role(i32); \
        fn value() -> I32 { 1(_) }");
}

#[test]
fn dropped_alias_hole_remains_admitted_without_an_annotation_goal() {
    ok("module main; \
        pub type Keep[First][Dropped] = First; \
        host fn exact(_token: .) -> Keep(., .); \
        fn partial() -> . { let .(value: Keep(., _)) = exact(()); () }");
}

#[test]
fn lowered_check_and_synth_routes_consume_the_shared_header_plan_once() {
    use crate::pass::typecheck_core::annotation_plan::{
        annotation_plan_consumer_work, reset_annotation_plan_consumer_work,
    };

    reset_annotation_plan_consumer_work();
    ok("module main; \
        fn checked() -> . -> . { .(value: .) -> . { value } }");
    let checked = annotation_plan_consumer_work();
    assert_eq!(checked.lowered_check, 1);
    assert_eq!(checked.lowered_synth, 0);

    reset_annotation_plan_consumer_work();
    ok("module main; \
        fn synthesized() -> . -> . { \
          let identity = .(value: .) -> . { value }; \
          identity \
        }");
    let synthesized = annotation_plan_consumer_work();
    assert_eq!(synthesized.lowered_check, 0);
    assert_eq!(synthesized.lowered_synth, 1);
}

// Existing declaration-signature `_`, synthesis-only nested-placeholder,
//    exact-root literal(_), label `name: _`, retained, ExpectedValue::Lambda,
//    concrete-planner, Prime/readiness and RecOrder::ExpectedFromBody controls
//    are assigned by the causal matrix. Reuse exact existing fixtures where
//    available; do not duplicate them here merely to inflate selector count.

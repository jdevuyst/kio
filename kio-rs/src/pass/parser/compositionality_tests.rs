use super::{parse, parse_lazy, parse_type_fragment};
use crate::ast::{CallArg, Expr, Item, Module, Surface, Type};
use crate::pretty::pretty_module;

const EXPR_HOLE: &str = "__EXPR_HOLE__";
const TYPE_HOLE: &str = "__TYPE_HOLE__";

// These exhaustive tags make a new surface AST variant an explicit matrix
// decision; normalized serialization below owns the recursive topology check.
fn type_tag(ty: &Type<Surface>) -> &'static str {
    match ty {
        Type::Path { .. } => "path",
        Type::Unit { .. } => "unit",
        Type::Bottom { .. } => "bottom",
        Type::Function { .. } => "function",
        Type::Product { .. } => "product",
        Type::Sum { .. } => "sum",
        Type::LabelSugar { .. } => "label-sugar",
        Type::Infer { .. } => "inference-hole",
        Type::Goal { ext, .. } => match *ext {},
        Type::Forall { .. } => "forall",
    }
}

fn expr_tag(expr: &Expr<Surface>) -> &'static str {
    match expr {
        Expr::BlockCall { .. } => "block-call",
        Expr::Path { .. } => "path",
        Expr::Call { .. } => "call",
        Expr::RecCall { .. } => "recursive-call",
        Expr::FnExpr { .. } => "lambda",
        Expr::Let { .. } => "let",
        Expr::RowLet { .. } => "row-let",
        Expr::Seq { .. } => "sequence",
        Expr::Unit { .. } => "unit",
        Expr::StrLit { .. } => "string-literal",
        Expr::IntLit { .. } => "integer-literal",
        Expr::FloatLit { .. } => "float-literal",
        Expr::BoolLit { .. } => "boolean-literal",
        Expr::Tuple { .. } => "tuple",
        Expr::FnPlaceholder { .. } => "placeholder-lambda",
        Expr::LabelValue { .. } => "label-value",
        Expr::Elaborator { call, .. } => match call {
            crate::ast::ElaboratorCall::FieldAccess { .. } => "field-access",
            crate::ast::ElaboratorCall::FieldUpdate { .. } => "field-update",
        },
        Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
        Expr::UserElaborator { .. } => "bang-call",
        Expr::Ufcs { .. } => "ufcs",
        Expr::OpChain { kind, .. } => match kind {
            crate::ast::OpChainKind::Normal { .. } => "operator",
            crate::ast::OpChainKind::Variadic { .. } => "variadic-operator",
        },
        Expr::EnrichedTuple { ext, .. }
        | Expr::EnrichedProject { ext, .. }
        | Expr::EnrichedInject { ext, .. }
        | Expr::EnrichedMatch { ext, .. }
        | Expr::EnrichedConditional { ext, .. }
        | Expr::EnrichedRecord { ext, .. }
        | Expr::EnrichedFieldGet { ext, .. } => match *ext {},
        Expr::LowHostCall { ext, .. }
        | Expr::LowModuleCall { ext, .. }
        | Expr::LowQualifiedModuleCall { ext, .. }
        | Expr::LowQualifiedNewtypeMember { ext, .. }
        | Expr::LowNewtypeCtor { ext, .. }
        | Expr::LowNewtypeProj { ext, .. }
        | Expr::LowClosureCall { ext, .. }
        | Expr::LowIndirectCall { ext, .. }
        | Expr::LowTypeApplication { ext, .. }
        | Expr::LowAbsurdCall { ext, .. }
        | Expr::LowCpsProjectorApply { ext, .. }
        | Expr::LowBoundRef { ext, .. }
        | Expr::LowHostFnValueRef { ext, .. }
        | Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
    }
}

fn expr_root_signature(expr: &Expr<Surface>) -> String {
    match expr {
        Expr::Ufcs {
            callee_segments,
            flavor,
            bang,
            ..
        } => format!(
            "ufcs:{}:{}:{}",
            flavor.token(),
            callee_segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("."),
            bang.is_some()
        ),
        _ => expr_tag(expr).to_owned(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Subject {
    Function,
    EquivFirst,
}

fn subject_expr(module: &Module, subject: Subject) -> &Expr {
    match subject {
        Subject::Function => module.items.iter().find_map(|item| match item {
            Item::FnDef(def) if def.name == "subject" => Some(&def.body),
            _ => None,
        }),
        Subject::EquivFirst => module.items.iter().find_map(|item| match item {
            Item::Equiv(def, _) if def.name == "subject" => {
                def.terms.first().map(|term| &term.body)
            }
            _ => None,
        }),
    }
    .expect("matrix subject expression")
}

#[derive(Clone, Copy)]
struct ExprForm {
    id: &'static str,
    grammar: &'static str,
    declarations: &'static str,
    source: &'static str,
    root_tag: &'static str,
}

#[derive(Clone, Copy)]
struct ExprContext {
    id: &'static str,
    grammar: &'static str,
    declarations: &'static str,
    item: &'static str,
    subject: Subject,
}

const EXPR_FORMS: &[ExprForm] = &[
    ExprForm {
        id: "path",
        grammar: "ExprAtom",
        declarations: "",
        source: "form_value",
        root_tag: "path",
    },
    ExprForm {
        id: "member-path",
        grammar: "ExprSuffix",
        declarations: "",
        source: "Formtype.form_member",
        root_tag: "path",
    },
    ExprForm {
        id: "unit",
        grammar: "ExprAtom",
        declarations: "",
        source: "()",
        root_tag: "unit",
    },
    ExprForm {
        id: "string-literal",
        grammar: "LiteralCall",
        declarations: "",
        source: "\"form\"",
        root_tag: "string-literal",
    },
    ExprForm {
        id: "integer-literal",
        grammar: "LiteralCall",
        declarations: "",
        source: "23",
        root_tag: "integer-literal",
    },
    ExprForm {
        id: "float-literal",
        grammar: "LiteralCall",
        declarations: "",
        source: "2.5",
        root_tag: "float-literal",
    },
    ExprForm {
        id: "boolean-literal",
        grammar: "BoolLit",
        declarations: "",
        source: ".t",
        root_tag: "boolean-literal",
    },
    ExprForm {
        id: "annotated-literal",
        grammar: "LiteralCall",
        declarations: "",
        source: "23(Formtype)",
        root_tag: "integer-literal",
    },
    ExprForm {
        id: "lambda",
        grammar: "FnExpr",
        declarations: "",
        source: ".[Formtype](form_param: Formtype) -> Formtype { form_param }",
        root_tag: "lambda",
    },
    ExprForm {
        id: "call",
        grammar: "ExprSuffix",
        declarations: "",
        source: "form_callee(form_arg)",
        root_tag: "call",
    },
    ExprForm {
        id: "tuple",
        grammar: "TupleLit",
        declarations: "",
        source: "(form_left, form_right)",
        root_tag: "tuple",
    },
    ExprForm {
        id: "empty-label-value",
        grammar: "LabelValue",
        declarations: "",
        source: "{}",
        // The language contract identifies empty label construction with unit.
        root_tag: "unit",
    },
    ExprForm {
        id: "label-value",
        grammar: "LabelValue",
        declarations: "",
        source: "{form_field = form_value}",
        root_tag: "label-value",
    },
    ExprForm {
        id: "conditional",
        grammar: "BlockElabCall",
        declarations: "",
        source: "if! form_condition { form_then } else { form_else }",
        root_tag: "block-call",
    },
    ExprForm {
        id: "scoped-block",
        grammar: "BlockElabCall",
        declarations: "",
        source: "scope! { form_value }",
        root_tag: "block-call",
    },
    ExprForm {
        id: "sequence-block",
        grammar: "BlockElabCall",
        declarations: "",
        source: "do! form_bind { let form_bound <- form_action; form_bound }",
        root_tag: "block-call",
    },
    ExprForm {
        id: "bang-call",
        grammar: "BangCall",
        declarations: "",
        source: "form_elab!(form_arg)",
        root_tag: "bang-call",
    },
    ExprForm {
        id: "placeholder-lambda",
        grammar: "PlaceholderLambda",
        declarations: "",
        source: ".x. { (x1, x1) }",
        root_tag: "placeholder-lambda",
    },
    ExprForm {
        id: "recursive-call",
        grammar: "RecCall",
        declarations: "",
        source: "rec(poly, cont) form_loop(form_arg)",
        root_tag: "recursive-call",
    },
    ExprForm {
        id: "field-access",
        grammar: "FieldAccess",
        declarations: "",
        source: "form_record.?{form_field}",
        root_tag: "field-access",
    },
    ExprForm {
        id: "field-update",
        grammar: "FieldUpdate",
        declarations: "",
        source: "form_record.!{form_field = form_value}",
        root_tag: "field-update",
    },
    ExprForm {
        id: "ufcs-receiver-first",
        grammar: "DotSpliceSuffix",
        declarations: "",
        source: "form_receiver.>form_callee(form_arg)",
        root_tag: "ufcs",
    },
    ExprForm {
        id: "ufcs-receiver-last",
        grammar: "DotSpliceSuffix",
        declarations: "",
        source: "form_receiver.>>form_callee(form_arg)",
        root_tag: "ufcs",
    },
    ExprForm {
        id: "ufcs-argument-last",
        grammar: "LeftCallSplice",
        declarations: "",
        source: "form_callee(form_arg).<form_receiver",
        root_tag: "ufcs",
    },
    ExprForm {
        id: "ufcs-argument-first",
        grammar: "LeftCallSplice",
        declarations: "",
        source: "form_callee(form_arg).<<form_receiver",
        root_tag: "ufcs",
    },
    ExprForm {
        id: "prefix-operator",
        grammar: "PrefixOpExpr",
        declarations: "fn form_prefix(form_x: Formtype) -> Formtype { form_x }\nop - __ { impl form_prefix; };",
        source: "- form_value",
        root_tag: "operator",
    },
    ExprForm {
        id: "infix-operator",
        grammar: "OperatorTail",
        declarations: "fn form_infix(form_x: Formtype, form_y: Formtype) -> Formtype { form_x }\nop _ + _ { impl form_infix; };",
        source: "form_left + form_right",
        root_tag: "operator",
    },
    ExprForm {
        id: "postfix-operator",
        grammar: "OperatorTail",
        declarations: "fn form_postfix(form_x: Formtype) -> Formtype { form_x }\nop _ ? { impl form_postfix; };",
        source: "form_value ?",
        root_tag: "operator",
    },
    ExprForm {
        id: "ternary-operator",
        grammar: "OperatorTail",
        declarations: "fn form_select(form_x: Formtype, form_y: Formtype, form_z: Formtype) -> Formtype { form_x }\nop _ ? _ : _ { impl form_select; };",
        source: "form_condition ? form_then : form_else",
        root_tag: "operator",
    },
    ExprForm {
        id: "matched-operator",
        grammar: "OperatorTail",
        declarations: "fn form_index(form_x: Formtype, form_y: Formtype) -> Formtype { form_x }\nop _ <| _ |> { impl form_index; };",
        source: "form_record <| form_index |>",
        root_tag: "operator",
    },
    ExprForm {
        id: "variadic-operator",
        grammar: "VariadicLiteral",
        declarations: "varop [% %] { foldr form_push form_empty; };",
        source: "[% form_left, form_right %]",
        root_tag: "variadic-operator",
    },
];

const EXPR_CONTEXTS: &[ExprContext] = &[
    ExprContext {
        id: "block-final",
        grammar: "BlockBody",
        declarations: "",
        item: "fn subject() -> . { __EXPR_HOLE__ }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "grouping",
        grammar: "ExprAtom",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "expression-statement",
        grammar: "ExprStmt",
        declarations: "",
        item: "fn subject() -> . { __EXPR_HOLE__; context_final }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "simple-let-rhs",
        grammar: "LetStmt",
        declarations: "",
        item: "fn subject() -> . { let context_bound = __EXPR_HOLE__; context_bound }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "typed-let-rhs",
        grammar: "PatternLet",
        declarations: "",
        item: "fn subject() -> . { let .(context_bound: Contexttype) = __EXPR_HOLE__; context_bound }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "pattern-let-rhs",
        grammar: "PatternLet",
        declarations: "",
        item: "fn subject() -> . { let .(context_left: Contexttype, context_right: Contexttype) = __EXPR_HOLE__; context_left }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "as-pattern-let-rhs",
        grammar: "PatternLet",
        declarations: "",
        item: "fn subject() -> . { let .(context_pair: (context_left: Contexttype, context_right: Contexttype)) = __EXPR_HOLE__; context_left }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "existential-let-rhs",
        grammar: "ExistentialLet",
        declarations: "",
        item: "fn subject() -> . { let .(<Contexttype> context_bound) = __EXPR_HOLE__; context_bound }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "existential-pattern-let-rhs",
        grammar: "ExistentialLet",
        declarations: "",
        item: "fn subject() -> . { let .(<Contexttype> (context_left: Contexttype, context_right: Contexttype)) = __EXPR_HOLE__; context_left }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "existential-discard-component-let-rhs",
        grammar: "ExistentialLet",
        declarations: "",
        item: "fn subject() -> . { let .(<Contexttype> (_: Contexttype, context_bound: Contexttype)) = __EXPR_HOLE__; context_bound }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "row-let-rhs",
        grammar: "RowLet",
        declarations: "",
        item: "fn subject() -> . { let .({context_field as context_bound}) = __EXPR_HOLE__; context_bound }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "lambda-body",
        grammar: "FnExpr",
        declarations: "",
        item: "fn subject() -> . { .() { __EXPR_HOLE__ } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "callee",
        grammar: "ExprPostfix",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__)(context_arg) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "call-argument",
        grammar: "CallArg",
        declarations: "",
        item: "fn subject() -> . { context_callee(__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "bang-call-argument",
        grammar: "CallArgList",
        declarations: "",
        item: "fn subject() -> . { context_elab!(__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "recursive-call-argument",
        grammar: "CallArgList",
        declarations: "",
        item: "fn subject() -> . { rec context_loop(__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "tuple-element",
        grammar: "TupleLit",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__, context_other) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "variadic-element",
        grammar: "VariadicLiteral",
        declarations: "varop [! !] { foldr context_push context_empty; };",
        item: "fn subject() -> . { [! __EXPR_HOLE__ !] }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "label-payload",
        grammar: "LabelValueLabel",
        declarations: "",
        item: "fn subject() -> . { {context_field = __EXPR_HOLE__} }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "field-update-payload",
        grammar: "FieldUpdateLabel",
        declarations: "",
        item: "fn subject() -> . { context_record.!{context_field = __EXPR_HOLE__} }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "if-condition",
        grammar: "BlockElabCall",
        declarations: "",
        item: "fn subject() -> . { if!(__EXPR_HOLE__) { context_then } else { context_else } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "if-then",
        grammar: "BlockElabCall",
        declarations: "",
        item: "fn subject() -> . { if! context_condition { __EXPR_HOLE__ } else { context_else } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "if-else",
        grammar: "BlockElabCall",
        declarations: "",
        item: "fn subject() -> . { if! context_condition { context_then } else { __EXPR_HOLE__ } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "plain-do-final",
        grammar: "BlockElabCall",
        declarations: "",
        item: "fn subject() -> . { scope! { __EXPR_HOLE__ } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "monadic-receiver",
        grammar: "BlockElabCall",
        declarations: "",
        item: "fn subject() -> . { do!(__EXPR_HOLE__) { context_final } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "monadic-bind-rhs",
        grammar: "NeutralBind",
        declarations: "",
        item: "fn subject() -> . { do! context_bind { let context_bound <- __EXPR_HOLE__; context_bound } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "monadic-pure-rhs",
        grammar: "LetStmt",
        declarations: "",
        item: "fn subject() -> . { do! context_bind { let context_bound = __EXPR_HOLE__; context_bound } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "monadic-sequence",
        grammar: "NeutralBlock",
        declarations: "",
        item: "fn subject() -> . { do! context_bind { __EXPR_HOLE__; context_final } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "monadic-final",
        grammar: "NeutralBlock",
        declarations: "",
        item: "fn subject() -> . { do! context_bind { __EXPR_HOLE__ } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "placeholder-body",
        grammar: "PlaceholderLambda",
        declarations: "",
        item: "fn subject() -> . { .x. { let context_bound = __EXPR_HOLE__; x1 } }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "field-access-receiver",
        grammar: "FieldAccess",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__).?{context_field} }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "field-update-receiver",
        grammar: "FieldUpdate",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__).!{context_field = context_value} }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "ufcs-receiver-first",
        grammar: "DotSpliceSuffix",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__).>context_callee }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "ufcs-receiver-last",
        grammar: "DotSpliceSuffix",
        declarations: "",
        item: "fn subject() -> . { (__EXPR_HOLE__).>>context_callee }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "ufcs-argument-last",
        grammar: "LeftCallSplice",
        declarations: "",
        item: "fn subject() -> . { context_callee.<(__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "ufcs-argument-first",
        grammar: "LeftCallSplice",
        declarations: "",
        item: "fn subject() -> . { context_callee.<<(__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "prefix-operand",
        grammar: "PrefixOpExpr",
        declarations: "fn context_prefix(context_x: Contexttype) -> Contexttype { context_x }\nop ~ __ { impl context_prefix; };",
        item: "fn subject() -> . { ~ (__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "infix-left-operand",
        grammar: "OperatorTail",
        declarations: "fn context_infix(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\nop _ %% _ { impl context_infix; };",
        item: "fn subject() -> . { (__EXPR_HOLE__) %% context_right }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "infix-right-operand",
        grammar: "OperatorTail",
        declarations: "fn context_infix(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\nop _ %% _ { impl context_infix; };",
        item: "fn subject() -> . { context_left %% (__EXPR_HOLE__) }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "matched-operator-operand",
        grammar: "OperatorTail",
        declarations: "fn context_index(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\nop _ @< _ > { impl context_index; };",
        item: "fn subject() -> . { context_record @< (__EXPR_HOLE__) > }",
        subject: Subject::Function,
    },
    ExprContext {
        id: "equiv-arm",
        grammar: "Equiv",
        declarations: "",
        item: "equiv subject() { __EXPR_HOLE__; context_other }",
        subject: Subject::EquivFirst,
    },
];

fn matrix_source(form: ExprForm, context: ExprContext, expression: &str) -> String {
    assert!(
        context.item.contains(EXPR_HOLE),
        "context {} must contain the expression hole",
        context.id
    );
    format!(
        "module matrix;\n{}\n{}\n{}\n",
        form.declarations,
        context.declarations,
        context.item.replace(EXPR_HOLE, expression)
    )
}

fn parse_eager_and_lazy(source: &str, cell: &str) -> Module {
    let eager = parse(source)
        .unwrap_or_else(|error| panic!("eager parse failed for {cell}: {error:?}\n{source}"));
    let lazy = parse_lazy(source)
        .unwrap_or_else(|error| panic!("lazy header parse failed for {cell}: {error:?}\n{source}"));
    let forced = lazy.force_all().unwrap_or_else(|error| {
        panic!("forcing lazy bodies failed for {cell}: {error:?}\n{source}")
    });
    assert_eq!(forced, eager, "eager/lazy topology differs for {cell}");
    eager
}

// Erase locations, trivia, parser-fresh identities and canonical layout choices
// while retaining expression ownership and every neutral item.
fn normalize_topology(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.len() == 2 && map.contains_key("start") && map.contains_key("end") {
                map.insert("start".to_owned(), serde_json::Value::from(0));
                map.insert("end".to_owned(), serde_json::Value::from(0));
                return;
            }
            for (key, child) in map {
                match key.as_str() {
                    "ext" | "access_ext" | "match_id" | "temp_name" | "id" | "prefix_explicit"
                    | "elided" | "semicolon" => {
                        *child = serde_json::Value::Null;
                    }
                    "leading_trivia" | "trailing_trivia" | "leading" | "trailing"
                    | "separators" => {
                        *child = serde_json::Value::Array(Vec::new());
                    }
                    _ => normalize_topology(child),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                normalize_topology(child);
            }
        }
        _ => {}
    }
}

fn topology<T: serde::Serialize>(value: &T) -> serde_json::Value {
    let mut value = serde_json::to_value(value).expect("surface AST serializes");
    normalize_topology(&mut value);
    value
}

fn replace_subtree(
    value: &mut serde_json::Value,
    needle: &serde_json::Value,
    replacement: &serde_json::Value,
) -> usize {
    if value == needle {
        *value = replacement.clone();
        return 1;
    }
    match value {
        serde_json::Value::Object(map) => map
            .values_mut()
            .map(|child| replace_subtree(child, needle, replacement))
            .sum(),
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|child| replace_subtree(child, needle, replacement))
            .sum(),
        _ => 0,
    }
}

fn assert_pretty_topology(module: &Module, cell: &str) {
    let before = topology(module);
    let rendered = pretty_module(module);
    let reparsed = parse_eager_and_lazy(&rendered, &format!("{cell} pretty output"));
    assert_eq!(
        topology(&reparsed),
        before,
        "pretty/reparse topology differs for {cell}"
    );
    assert_eq!(
        pretty_module(&reparsed),
        rendered,
        "pretty output is not idempotent for {cell}"
    );
}

#[test]
fn expression_forms_compose_across_grammar_contexts() {
    for &form in EXPR_FORMS {
        let base_context = EXPR_CONTEXTS[0];
        let base_cell = format!("{} x {}", form.id, base_context.id);
        let base =
            parse_eager_and_lazy(&matrix_source(form, base_context, form.source), &base_cell);
        let base_expr = subject_expr(&base, base_context.subject);
        assert_eq!(
            expr_tag(base_expr),
            form.root_tag,
            "form {} parsed as an unexpected expression variant",
            form.id
        );
        let needle = topology(base_expr);

        for &context in EXPR_CONTEXTS {
            let cell = format!("{} x {}", form.id, context.id);
            let scaffold = parse_eager_and_lazy(
                &matrix_source(form, context, "matrix_hole"),
                &format!("{cell} scaffold"),
            );
            let marker_module = parse_eager_and_lazy(
                "module matrix; fn subject() -> . { matrix_hole }",
                "matrix marker",
            );
            let marker = topology(subject_expr(&marker_module, Subject::Function));
            let mut expected = topology(subject_expr(&scaffold, context.subject));
            assert_eq!(
                replace_subtree(&mut expected, &marker, &needle),
                1,
                "{cell} scaffold must expose exactly one expression slot"
            );

            let parsed = parse_eager_and_lazy(&matrix_source(form, context, form.source), &cell);
            assert_eq!(
                topology(subject_expr(&parsed, context.subject)),
                expected,
                "{cell} did not preserve the form in the selected grammar slot"
            );
            assert_pretty_topology(&parsed, &cell);
        }
    }
}

#[derive(Clone, Copy)]
struct TypeForm {
    id: &'static str,
    grammar: &'static str,
    source: &'static str,
    root_tag: &'static str,
}

#[derive(Clone, Copy)]
struct TypeContext {
    id: &'static str,
    grammar: &'static str,
    item: &'static str,
}

const TYPE_FORMS: &[TypeForm] = &[
    TypeForm {
        id: "unit",
        grammar: "TypeAtom",
        source: ".",
        root_tag: "unit",
    },
    TypeForm {
        id: "bottom",
        grammar: "TypeAtom",
        source: "!",
        root_tag: "bottom",
    },
    TypeForm {
        id: "path",
        grammar: "TypePath",
        source: "Formtype",
        root_tag: "path",
    },
    TypeForm {
        id: "qualified-path",
        grammar: "TypePath",
        source: "formmodule.Formtype",
        root_tag: "path",
    },
    TypeForm {
        id: "application",
        grammar: "TypeArgList",
        source: "Formbox(Formtype)",
        root_tag: "path",
    },
    TypeForm {
        id: "grouping",
        grammar: "TypeAtom",
        source: "((Formtype))",
        root_tag: "path",
    },
    TypeForm {
        id: "product",
        grammar: "TypeChainTail",
        source: "Formleft & Formright",
        root_tag: "product",
    },
    TypeForm {
        id: "sum",
        grammar: "TypeChainTail",
        source: "Formleft | Formright",
        root_tag: "sum",
    },
    TypeForm {
        id: "function",
        grammar: "TypeArrow",
        source: "Forminput -> Formoutput",
        root_tag: "function",
    },
    TypeForm {
        id: "product-function",
        grammar: "TypeArrowParam",
        source: "(Formleft & Formright) -> Formoutput",
        root_tag: "function",
    },
    TypeForm {
        id: "forall",
        grammar: "TypeArrow",
        source: "[Formtype] Formtype",
        root_tag: "forall",
    },
    TypeForm {
        id: "higher-kinded-forall",
        grammar: "TypeBinderGroup",
        source: "[*Formctor] Formctor(Formtype)",
        root_tag: "forall",
    },
    TypeForm {
        id: "inference-hole",
        grammar: "TypeAtom",
        source: "_",
        root_tag: "inference-hole",
    },
];

const TYPE_CONTEXTS: &[TypeContext] = &[
    TypeContext {
        id: "type-alias",
        grammar: "TypeAliasBody",
        item: "type Subjecttype = __TYPE_HOLE__;",
    },
    TypeContext {
        id: "newtype-payload",
        grammar: "NewtypeAfterVis",
        item: "newtype Subjecttype : __TYPE_HOLE__ { constructor make_subject; projector un_subject; };",
    },
    TypeContext {
        id: "type-argument",
        grammar: "TypeArgList",
        item: "type Subjecttype = Contextbox(__TYPE_HOLE__);",
    },
    TypeContext {
        id: "function-parameter",
        grammar: "SignatureParam",
        item: "fn subject(context_value: __TYPE_HOLE__) -> . { () }",
    },
    TypeContext {
        id: "function-return",
        grammar: "FnDef",
        item: "fn subject() -> __TYPE_HOLE__ { context_value }",
    },
    TypeContext {
        id: "lambda-parameter",
        grammar: "FnSignatureParam",
        item: "fn subject() -> . { .(context_value: __TYPE_HOLE__) { context_value } }",
    },
    TypeContext {
        id: "lambda-return",
        grammar: "FnExpr",
        item: "fn subject() -> . { .() -> __TYPE_HOLE__ { context_value } }",
    },
    TypeContext {
        id: "literal-annotation",
        grammar: "LiteralCall",
        item: "fn subject() -> . { 23(__TYPE_HOLE__) }",
    },
    TypeContext {
        id: "checked-let",
        grammar: "ParamPatternElem",
        item: "fn subject() -> . { let .(context_bound: __TYPE_HOLE__) = context_value; context_bound }",
    },
    TypeContext {
        id: "parameter-pattern-element",
        grammar: "ParamPatternElem",
        item: "fn subject((context_value: __TYPE_HOLE__)) -> . { () }",
    },
    TypeContext {
        id: "monadic-bind",
        grammar: "ParamPatternElem",
        item: "fn subject() -> . { do! context_bind { let .(context_bound: __TYPE_HOLE__) <- context_action; context_bound } }",
    },
    TypeContext {
        id: "monadic-pure-let",
        grammar: "ParamPatternElem",
        item: "fn subject() -> . { do! context_bind { let .(context_bound: __TYPE_HOLE__) = context_value; context_bound } }",
    },
    TypeContext {
        id: "elaborator-call-type",
        grammar: "ElaboratorItem",
        item: "elab subject : __TYPE_HOLE__ { impl context_value; };",
    },
];

fn type_matrix_source(context: TypeContext, ty: &str) -> String {
    assert!(
        context.item.contains(TYPE_HOLE),
        "context {} must contain the type hole",
        context.id
    );
    format!("module matrix;\n{}\n", context.item.replace(TYPE_HOLE, ty))
}

#[test]
fn type_forms_compose_across_grammar_contexts() {
    let marker = topology(
        &parse_type_fragment("Matrixtype").expect("parse the type-position scaffold marker"),
    );

    for &form in TYPE_FORMS {
        let parsed_type = parse_type_fragment(form.source)
            .unwrap_or_else(|error| panic!("type form {} failed to parse: {error:?}", form.id));
        assert_eq!(
            type_tag(&parsed_type),
            form.root_tag,
            "type form {} parsed as an unexpected type variant",
            form.id
        );
        let replacement = topology(&parsed_type);

        for &context in TYPE_CONTEXTS {
            let cell = format!("{} x {}", form.id, context.id);
            let scaffold = parse_eager_and_lazy(
                &type_matrix_source(context, "Matrixtype"),
                &format!("{cell} scaffold"),
            );
            let mut expected = topology(&scaffold);
            assert_eq!(
                replace_subtree(&mut expected, &marker, &replacement),
                1,
                "{cell} scaffold must expose exactly one type slot"
            );

            let parsed = parse_eager_and_lazy(&type_matrix_source(context, form.source), &cell);
            assert_eq!(
                topology(&parsed),
                expected,
                "{cell} did not preserve the type in the selected grammar slot"
            );
            assert_pretty_topology(&parsed, &cell);
        }
    }
}

#[test]
fn unordered_elaborator_entries_preserve_path_only_boundaries() {
    for implementation_head in ["impl", "impl(fills)"] {
        let source = |implementation: &str, captures: &str, implementation_first: bool| {
            let implementation = format!("{implementation_head} {implementation};");
            let captures = format!("captures {captures};");
            let body = if implementation_first {
                format!("{implementation} {captures}")
            } else {
                format!("{captures} {implementation}")
            };
            format!("module matrix; elab subject: . {{ {body} }};")
        };
        let canonical = parse_eager_and_lazy(
            &source("context_impl", "context_capture", false),
            implementation_head,
        );
        for implementation_first in [false, true] {
            let cell =
                format!("{implementation_head}, implementation_first={implementation_first}");
            let parsed = parse_eager_and_lazy(
                &source("context_impl", "context_capture", implementation_first),
                &cell,
            );
            assert_eq!(topology(&parsed), topology(&canonical), "{cell}");
            assert_pretty_topology(&parsed, &cell);

            for (implementation, captures) in [
                ("context_impl(value)", "context_capture"),
                (".(value) { value }", "context_capture"),
                ("(context_impl)", "context_capture"),
                ("context_impl", "context_capture(value)"),
                ("context_impl", ".(value) { value }"),
            ] {
                let invalid = source(implementation, captures, implementation_first);
                assert!(parse(&invalid).is_err(), "{cell}: accepted {invalid}");
                assert!(parse_lazy(&invalid).is_err(), "{cell}: deferred {invalid}");
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ArgClass {
    Value(&'static str),
    Type(&'static str),
}

fn subject_call_arg(module: &Module) -> &CallArg {
    let Expr::Call { args, .. } = subject_expr(module, Subject::Function) else {
        panic!("call-argument fixture must have a call body")
    };
    let [arg] = args.as_slice() else {
        panic!("call-argument fixture must have exactly one argument")
    };
    arg
}

#[test]
fn raw_call_argument_ambiguity_is_classified_by_topology() {
    for (id, declarations, source, expected) in [
        ("lowercase-path", "", "form_value", ArgClass::Value("path")),
        ("uppercase-path", "", "Formtype", ArgClass::Value("path")),
        (
            "parametric-uppercase-call",
            "",
            "Formbox(Formtype)",
            ArgClass::Value("call"),
        ),
        ("unit-value", "", "()", ArgClass::Value("unit")),
        (
            "tuple-value",
            "",
            "(form_left, form_right)",
            ArgClass::Value("tuple"),
        ),
        ("bottom-type", "", "!", ArgClass::Type("bottom")),
        (
            "product-type",
            "",
            "(Formleft & Formright)",
            ArgClass::Type("product"),
        ),
        (
            "sum-type",
            "",
            "(Formleft | Formright)",
            ArgClass::Type("sum"),
        ),
        (
            "function-type",
            "",
            "Forminput -> Formoutput",
            ArgClass::Type("function"),
        ),
        (
            "forall-type",
            "",
            "[Formtype] Formtype",
            ArgClass::Type("forall"),
        ),
        (
            "prefix-operator-value",
            "fn form_not(form_x: Formtype) -> Formtype { form_x }\nop ! __ { impl form_not; };",
            "! form_value",
            ArgClass::Value("operator"),
        ),
        (
            "member-call-value",
            "",
            "Formbox.make_formbox(form_value)",
            ArgClass::Value("call"),
        ),
    ] {
        let source = format!(
            "module matrix;\n{declarations}\nfn subject() -> . {{ context_callee({source}) }}\n"
        );
        let parsed = parse_eager_and_lazy(&source, id);
        match (subject_call_arg(&parsed), expected) {
            (CallArg::Value(value), ArgClass::Value(tag)) => assert_eq!(
                expr_tag(value),
                tag,
                "{id} had the wrong value topology: {value:?}"
            ),
            (CallArg::Type(ty), ArgClass::Type(tag)) => assert_eq!(
                type_tag(ty),
                tag,
                "{id} had the wrong type topology: {ty:?}"
            ),
            (actual, _) => panic!("{id} had the wrong raw argument class: {actual:?}"),
        }
        assert_pretty_topology(&parsed, id);
    }
}

#[cfg(feature = "surface")]
#[test]
fn typed_parametric_call_argument_is_baked_into_its_resolved_slot() {
    use crate::pass::full::FullPipeline;
    use crate::pass::resolve::Package;
    use crate::pipeline::Pipeline;
    use std::path::{Path, PathBuf};

    let source = "module matrix;\n\
        newtype Formbox[A] : A { constructor make_formbox; projector un_formbox; };\n\
        fn identity[A](value: A) -> A { value }\n\
        fn subject[A](value: Formbox(A)) -> Formbox(A) { identity(Formbox(A), value) }";
    let parsed = parse(source).expect("parse typed ambiguity fixture");
    let raw = subject_expr(&parsed, Subject::Function);
    let Expr::Call { args, .. } = raw else {
        panic!("typed ambiguity fixture must start as an ordinary call")
    };
    assert!(
        matches!(
            args.as_slice(),
            [CallArg::Value(Expr::Call { .. }), CallArg::Value(_)]
        ),
        "the parametric type candidate must remain a value-shaped parser node: {args:?}"
    );

    let (lowered, _) =
        FullPipeline::lower_package(vec![(PathBuf::from("matrix.kio"), parsed)], None)
            .expect("lower typed ambiguity fixture");
    let package =
        Package::build(Path::new(""), lowered, None).expect("assemble typed ambiguity fixture");
    let prime = FullPipeline::typecheck(&package).expect("typecheck typed ambiguity fixture");
    let module = &prime.module("matrix").expect("typed matrix module").module;
    let subject = module.items.iter().find_map(|item| match item {
        Item::FnDef(def) if def.name == "subject" => Some(&def.body),
        _ => None,
    });
    let Expr::Call { args, .. } = subject.expect("typed subject function") else {
        panic!("typed subject must remain an ordinary call")
    };
    assert!(
        matches!(
            args.as_slice(),
            [
                CallArg::Type(Type::Path { segments, args, .. }),
                CallArg::Value(_)
            ] if segments.last().is_some_and(|segment| segment.as_str() == "Formbox")
                && args.len() == 1
        ),
        "typing must bake the candidate into the resolved type slot: {args:?}"
    );
}

fn assert_deferred_body_rejected(source: &str, id: &str) {
    let eager = match parse(source) {
        Ok(module) => panic!("{id} unexpectedly eager-parsed as {module:?}"),
        Err(error) => error,
    };
    let lazy = parse_lazy(source)
        .unwrap_or_else(|error| panic!("{id} must reach deferred-body parsing: {error:?}"));
    let forced = match lazy.force_all() {
        Ok(module) => panic!("{id} unexpectedly lazy-parsed as {module:?}"),
        Err(error) => error,
    };
    assert_eq!(
        std::mem::discriminant(&forced),
        std::mem::discriminant(&eager),
        "{id} eager/lazy rejection categories differ: eager={eager:?}, lazy={forced:?}"
    );
}

#[test]
fn unparenthesized_and_tight_context_boundaries_keep_their_topology() {
    for (id, declarations, body, expected_root) in [
        (
            "call-result-ufcs-receiver",
            "",
            "form_callee(form_arg).>context_next",
            "ufcs:.>:context_next:false",
        ),
        (
            "tight-left-splice-argument",
            "",
            "context_callee.<form_value",
            "ufcs:.<:context_callee:false",
        ),
        (
            "prefix-call-operand",
            "fn context_prefix(context_x: Contexttype) -> Contexttype { context_x }\nop ~ __ { impl context_prefix; };",
            "~ form_callee(form_arg)",
            "operator",
        ),
        (
            "infix-call-left-operand",
            "fn context_infix(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\nop _ %% _ { impl context_infix; };",
            "form_callee(form_arg) %% context_right",
            "operator",
        ),
        (
            "infix-call-right-operand",
            "fn context_infix(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\nop _ %% _ { impl context_infix; };",
            "context_left %% form_callee(form_arg)",
            "operator",
        ),
        (
            "conditional-call-condition",
            "",
            "if! form_callee(form_arg) { context_then } else { context_else }",
            "block-call",
        ),
        (
            "monadic-path-receiver",
            "",
            "do! form_bind { context_final }",
            "block-call",
        ),
    ] {
        let source = format!("module matrix;\n{declarations}\nfn subject() -> . {{ {body} }}");
        let parsed = parse_eager_and_lazy(&source, id);
        assert_eq!(
            expr_root_signature(subject_expr(&parsed, Subject::Function)),
            expected_root,
            "{id} parsed with the wrong root topology",
        );
        assert_pretty_topology(&parsed, id);
    }
}

#[test]
fn precedence_and_scope_boundaries_accept_only_explicit_composition() {
    let operators = "fn context_add(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\n\
        fn context_mul(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\n\
        op _ + _ { impl context_add; };\n\
        op _ %% _ { impl context_mul; };";
    for (id, body) in [
        ("mixed-operators", "context_a + context_b %% context_c"),
        (
            "non-associative-operator",
            "context_a + context_b + context_c",
        ),
    ] {
        assert_deferred_body_rejected(
            &format!("module matrix;\n{operators}\nfn subject() -> . {{ {body} }}"),
            id,
        );
    }
    for (id, body) in [
        ("grouped-mixed-left", "(context_a + context_b) %% context_c"),
        (
            "grouped-mixed-right",
            "context_a + (context_b %% context_c)",
        ),
    ] {
        let source = format!("module matrix;\n{operators}\nfn subject() -> . {{ {body} }}");
        let parsed = parse_eager_and_lazy(&source, id);
        assert_pretty_topology(&parsed, id);
    }

    assert_deferred_body_rejected(
        "module matrix; fn subject() -> . { do! {context_field = context_value} { context_final } }",
        "ungrouped-braced-monadic-receiver",
    );
    let grouped = parse_eager_and_lazy(
        "module matrix; fn subject() -> . { do!({context_field = context_value}) { context_final } }",
        "grouped-braced-monadic-receiver",
    );
    assert!(matches!(
        subject_expr(&grouped, Subject::Function),
        Expr::BlockCall { .. }
    ));
    assert_pretty_topology(&grouped, "grouped-braced-monadic-receiver");

    assert_deferred_body_rejected(
        "module matrix; fn subject() -> . { #1 }",
        "placeholder-outside-scope",
    );
    let placeholder = parse_eager_and_lazy(
        "module matrix; fn subject() -> . { .x. { x1 } }",
        "placeholder-inside-scope",
    );
    assert_pretty_topology(&placeholder, "placeholder-inside-scope");

    assert_deferred_body_rejected(
        "module matrix; fn subject() -> . { let context_bound <- context_value; context_bound }",
        "monadic-bind-outside-do",
    );
}

#[test]
fn tight_splice_boundary_is_distinct_from_a_grouped_rhs() {
    let tight = parse_eager_and_lazy(
        "module matrix; fn subject() -> . { context_callee.<context_value.>context_next }",
        "tight splice",
    );
    let grouped = parse_eager_and_lazy(
        "module matrix; fn subject() -> . { context_callee.<(context_value.>context_next) }",
        "grouped splice",
    );
    let tight = subject_expr(&tight, Subject::Function);
    let grouped = subject_expr(&grouped, Subject::Function);
    assert_ne!(
        topology(tight),
        topology(grouped),
        "tight and grouped splice topology collapsed"
    );
    let Expr::Ufcs {
        receiver: tight_receiver,
        callee_segments: tight_callee,
        flavor: tight_flavor,
        ..
    } = tight
    else {
        panic!("tight splice must parse with the right splice outermost")
    };
    assert_eq!(tight_flavor.token(), ".>");
    assert_eq!(
        tight_callee.last().map(crate::ast::PathSegment::as_str),
        Some("context_next")
    );
    assert_eq!(
        expr_root_signature(tight_receiver),
        "ufcs:.<:context_callee:false"
    );

    let Expr::Ufcs {
        receiver: grouped_receiver,
        callee_segments: grouped_callee,
        flavor: grouped_flavor,
        ..
    } = grouped
    else {
        panic!("grouped splice must parse with the left splice outermost")
    };
    assert_eq!(grouped_flavor.token(), ".<");
    assert_eq!(
        grouped_callee.last().map(crate::ast::PathSegment::as_str),
        Some("context_callee")
    );
    assert_eq!(
        expr_root_signature(grouped_receiver),
        "ufcs:.>:context_next:false"
    );
}

#[test]
fn matched_operator_and_structural_forall_follow_their_syntax() {
    let no_operator = parse_eager_and_lazy(
        "module matrix; fn subject() -> . { context_callee([Formtype] Formtype) }",
        "structural forall call argument",
    );
    assert!(matches!(
        subject_call_arg(&no_operator),
        CallArg::Type(Type::Forall { .. })
    ));

    let with_operator = parse_eager_and_lazy(
        "module matrix;\n\
         fn context_bracket(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\n\
         op <| _ |> _ { impl context_bracket; };\n\
         fn subject() -> . { context_callee(<| form_type |> form_value) }",
        "matched operator call argument",
    );
    assert!(matches!(
        subject_call_arg(&with_operator),
        CallArg::Value(Expr::OpChain { .. })
    ));
    assert_pretty_topology(&with_operator, "matched operator call argument");

    let forall_with_operator = parse_eager_and_lazy(
        "module matrix;\n\
         fn context_bracket(context_x: Contexttype, context_y: Contexttype) -> Contexttype { context_x }\n\
         op <| _ |> _ { impl context_bracket; };\n\
         fn subject() -> . { context_callee([Formtype] Formtype) }",
        "structural forall beside a matched operator",
    );
    assert!(matches!(
        subject_call_arg(&forall_with_operator),
        CallArg::Type(Type::Forall { .. })
    ));
    assert_pretty_topology(
        &forall_with_operator,
        "structural forall beside a matched operator",
    );

    let forbidden = "module matrix; op [ _ ] _ { impl context_bracket; };";
    assert!(
        parse(forbidden).is_err(),
        "fixed bracket operators are rejected"
    );
    assert!(
        parse_lazy(forbidden)
            .and_then(|module| module.force_all())
            .is_err(),
        "lazy parsing also rejects fixed bracket operators"
    );
}

fn parse_eager_and_lazy_with_grammar(source: &str, _provider: &Module, cell: &str) -> Module {
    let eager = parse(source).unwrap_or_else(|error| {
        panic!("consumer-declared eager parse failed for {cell}: {error:?}\n{source}")
    });
    let lazy = parse_lazy(source).unwrap_or_else(|error| {
        panic!("consumer-declared lazy header parse failed for {cell}: {error:?}\n{source}")
    });
    let forced = lazy.force_all().unwrap_or_else(|error| {
        panic!("consumer-declared lazy force failed for {cell}: {error:?}\n{source}")
    });
    assert_eq!(
        forced, eager,
        "consumer eager/lazy topology differs for {cell}"
    );

    #[cfg(any(feature = "surface", feature = "cli"))]
    crate::pass::surface_registry::validate_operator_context(
        &eager,
        &[(std::path::PathBuf::from("syntax.kio"), _provider.clone())],
    )
    .expect("selected provider matches the consumer grammar");
    let rendered = pretty_module(&eager);
    let reparsed = parse(&rendered).unwrap_or_else(|error| {
        panic!(
            "consumer-declared pretty output failed to reparse for {cell}: {error:?}\n{rendered}"
        )
    });
    let lazy_reparsed = parse_lazy(&rendered).unwrap_or_else(|error| {
        panic!(
            "consumer-declared pretty output failed lazy header parsing for {cell}: {error:?}\n{rendered}"
        )
    });
    let forced_reparsed = lazy_reparsed.force_all().unwrap_or_else(|error| {
        panic!(
            "consumer-declared pretty output failed lazy forcing for {cell}: {error:?}\n{rendered}"
        )
    });
    assert_eq!(
        forced_reparsed, reparsed,
        "consumer-declared pretty output eager/lazy topology differs for {cell}"
    );
    assert_eq!(
        topology(&reparsed),
        topology(&eager),
        "consumer-declared pretty/reparse topology differs for {cell}"
    );
    assert_eq!(
        pretty_module(&reparsed),
        rendered,
        "consumer-declared pretty output is not idempotent for {cell}"
    );
    eager
}

#[test]
fn unrelated_provider_declarations_do_not_change_selected_operator_grammar() {
    let base_provider_module = parse(
        "module syntax;\n\
         pub fn syntax_add(syntax_left: Syntaxtype, syntax_right: Syntaxtype) -> Syntaxtype { syntax_left }\n\
         pub op _ + _ { impl syntax_add; };",
    )
    .expect("parse base operator provider");
    let expanded_provider_module = parse(
        "module syntax;\n\
         pub fn syntax_add(syntax_left: Syntaxtype, syntax_right: Syntaxtype) -> Syntaxtype { syntax_left }\n\
         pub op _ + _ { impl syntax_add; };\n\
         pub fn syntax_noise(syntax_value: Syntaxtype) -> Syntaxtype { syntax_value }\n\
         pub fn syntax_other(syntax_left: Syntaxtype, syntax_right: Syntaxtype) -> Syntaxtype { syntax_left }\n\
         pub op _ %% _ { impl syntax_other; };",
    )
    .expect("parse expanded operator provider");
    let consumer = "module consumer;\n\
        import syntax(op _ + _);\n\
        fn subject() -> . { consumer_left + consumer_right }";
    let base = parse_eager_and_lazy_with_grammar(consumer, &base_provider_module, "base provider");
    let expanded =
        parse_eager_and_lazy_with_grammar(consumer, &expanded_provider_module, "expanded provider");
    assert_eq!(
        topology(subject_expr(&base, Subject::Function)),
        topology(subject_expr(&expanded, Subject::Function)),
        "an unselected provider declaration changed the consumer expression"
    );

    let expanded_consumer = "module consumer;\n\
        import syntax(op _ + _);\n\
        fn consumer_unrelated(consumer_value: Consumertype) -> Consumertype { consumer_value }\n\
        fn subject() -> . { consumer_left + consumer_right }";
    let with_unrelated_consumer_item = parse_eager_and_lazy_with_grammar(
        expanded_consumer,
        &base_provider_module,
        "consumer with unrelated item",
    );
    assert_eq!(
        topology(subject_expr(&base, Subject::Function)),
        topology(subject_expr(
            &with_unrelated_consumer_item,
            Subject::Function
        )),
        "an unrelated consumer declaration changed the existing expression"
    );
}

#[derive(Debug)]
struct GrammarDefinition {
    name: String,
    operator: String,
    rhs: String,
}

const MONITORED_GRAMMAR: &[&str] = &[
    "TypeAlias",
    "TypeAliasBody",
    "NewtypeAfterVis",
    "FnDef",
    "TypeBinderGroup",
    "SignatureParam",
    "FnSignatureParam",
    "Type",
    "TypeArrow",
    "TypeArrowParam",
    "TypeChain",
    "TypeChainTail",
    "TypeAtom",
    "TypePath",
    "TypeArgList",
    "Block",
    "BlockBody",
    "Stmt",
    "LetStmt",
    "ExprStmt",
    "Expr",
    "ExprPostfix",
    "ExprSuffix",
    "CallArg",
    "ExprAtom",
    "LiteralCall",
    "BoolLit",
    "FnExpr",
    "ElaboratorItem",
    "ElaboratorBody",
    "ElaboratorEntry",
    "ElaboratorImpl",
    "Equiv",
    "FixedOpToken",
    "VaropHead",
    "VaropOpen",
    "VaropClose",
    "VariadicLiteral",
    "TupleLit",
    "LabelValue",
    "LabelValueLabel",
    "FieldAccess",
    "FieldUpdate",
    "FieldUpdateLabel",
    "BlockElabCall",
    "NeutralBlock",
    "NeutralBinding",
    "NeutralBind",
    "NeutralBinder",
    "BangCall",
    "RecCall",
    "CallArgList",
    "NonemptyCallArgList",
    "DotPlaceholderIntro",
    "PlaceholderLambda",
    "PrefixOpExpr",
    "DotSpliceSuffix",
    "LeftCallSplice",
    "LeftSpliceCallee",
    "LeftSplicePathCallee",
    "TightCallArg",
    "OperatorTail",
    "PatternLet",
    "ParamPatternElem",
    "ExistentialLet",
    "ExistentialLetBinder",
    "RowLet",
    "TrailingBlockDecl",
    "BlockExposure",
    "BlockPrefix",
    "BlockValueArgs",
    "BarePrefixExpr",
    "LabelledBlock",
    "FinalElidedBlock",
];

const DIRECT_EXPR_OWNERS: &[&str] = &[
    "BarePrefixExpr",
    "BlockBody",
    "BlockValueArgs",
    "CallArg",
    "Equiv",
    "ExistentialLet",
    "ExprAtom",
    "ExprStmt",
    "FieldUpdateLabel",
    "LabelValueLabel",
    "LetStmt",
    "NeutralBind",
    "NeutralBlock",
    "OperatorTail",
    "PatternLet",
    "PrefixOpExpr",
    "RowLet",
    "TupleLit",
    "VariadicLiteral",
];

const DIRECT_TYPE_OWNERS: &[&str] = &[
    "CallArg",
    "ElaboratorItem",
    "FnDef",
    "FnExpr",
    "FnSignatureParam",
    "HostFnBody",
    "LabelPayload",
    "LiteralCall",
    "NewtypeAfterVis",
    "ParamPatternElem",
    "RecFnDef",
    "SigExportFn",
    "SignatureParam",
    "TypeAliasBody",
    "TypeArgList",
    "TypeAtom",
];

// Direct `Type` owners outside TYPE_CONTEXTS need an explicit reason. This
// list is deliberately exact: adding a grammar owner cannot silently escape
// either the matrix or one of these separately exercised boundaries.
const NON_MATRIX_TYPE_OWNERS: &[(&str, &str)] = &[
    (
        "CallArg",
        "raw expression/type ambiguity has its own topology table",
    ),
    (
        "HostFnBody",
        "shares SignatureParam and return-type parsing with function declarations",
    ),
    (
        "LabelPayload",
        "exact bare `_` is a label-reuse marker rather than an inference type",
    ),
    (
        "RecFnDef",
        "shares SignatureParam and return-type parsing with FnDef",
    ),
    (
        "SigExportFn",
        "signature-file export functions share HostFnBody's signature representation",
    ),
    (
        "TypeAtom",
        "recursive grouping and chain precedence are exercised by the type-form axis",
    ),
];

const EXPECTED_EXPRESSION_GRAMMAR: &str = r#"TypeAlias ::= TypeAliasBody ';'
TypeAliasBody ::= Vis? 'type' IDENT TypeParamList? '=' Type
NewtypeAfterVis ::= 'newtype' IDENT TypeParamList? ExistsBinder* ':' Type NewtypeBody
FnDef ::= FnDeclModifiers 'fn' IDENT Signature ('->' Type)? Block
TypeBinderGroup ::= '[' TypeBinder (',' TypeBinder)* ']'
SignatureParam ::= IDENT ':' Type
SignatureParam += ParamPatternTuple | IDENT ':' ParamPatternTuple
FnSignatureParam ::= IDENT (':' Type)?
FnSignatureParam += ParamPatternTuple | IDENT ':' ParamPatternTuple
Type ::= TypeArrow
TypeArrow ::= TypeBinderGroup+ TypeArrow | TypeArrowParam '->' TypeArrow | TypeChain
TypeArrowParam ::= TypeAtom
TypeChain ::= TypeAtom TypeChainTail?
TypeChainTail ::= ('&' TypeAtom)+ | ('|' TypeAtom)+
TypeAtom ::= '!' | '.' | '(' Type ')' | '(' Type ('|' Type)+ ')' | '(' Type ('&' Type)+ ')' | TypePath
TypeAtom += '_'
TypePath ::= TypeName TypeArgList? | ModulePath '.' TypeName TypeArgList?
TypeArgList ::= '(' Type (',' Type)* ')'
Block ::= '{' BlockBody '}'
BlockBody ::= ';'* (Stmt ';'*)* Expr ';'*
Stmt ::= LetStmt | ExprStmt
Stmt += PatternLet | ExistentialLet | RowLet
LetStmt ::= 'let' LetBinder '=' Expr ';'
ExprStmt ::= Expr ';'
Expr ::= ExprPostfix
ExprPostfix ::= ExprAtom ExprSuffix*
ExprPostfix += LeftCallSplice ExprSuffix*
ExprSuffix ::= '(' (CallArg (',' CallArg)*)? ')'
ExprSuffix += FieldAccess | FieldUpdate
ExprSuffix += DotSpliceSuffix | OperatorTail
CallArg ::= Expr | Type
ExprAtom ::= LiteralCall | '(' ')' | '(' Expr ')' | ValuePath | FnExpr
ExprAtom += TupleLit | LabelValue | BangCall | BlockElabCall | PlaceholderLambda | RecCall
ExprAtom += PrefixOpExpr | VariadicLiteral
LiteralCall ::= (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit) '(' Type ')'
LiteralCall += (INT_LIT | FLOAT_LIT | STR_LIT+ | BoolLit)
BoolLit ::= '.' ('t' | 'f')
FnExpr ::= '.' FnSignature ('->' Type)? Block
ElaboratorItem ::= Vis? 'elab' IDENT ':' Type ElaboratorBody
ElaboratorBody ::= '{' SemiEntries<ElaboratorEntry> '}'
ElaboratorEntry ::= ElaboratorCaptures | ElaboratorImpl | TrailingBlockDecl
ElaboratorImpl ::= 'impl' ValuePath | 'impl' '(' 'fills' ')' ValuePath
Equiv ::= 'equiv' IDENT Signature? '{' ';'* Expr (';'+ Expr)+ ';'* '}'
FixedOpToken ::= OpToken
VaropHead ::= VaropOpen VaropClose
VaropOpen ::= OpToken
VaropClose ::= OpToken
VariadicLiteral ::= VaropOpen (Expr (',' Expr)*)? VaropClose
TupleLit ::= '(' Expr ',' Expr (',' Expr)* ','? ')'
LabelValue ::= '{' (LabelValueLabel (',' LabelValueLabel)* ','?)? '}'
LabelValueLabel ::= LabelPath ('=' Expr?)?
FieldAccess ::= '.?' '{' (FieldAccessLabel (',' FieldAccessLabel)* ','?)? '}'
FieldUpdate ::= '.!' '{' (FieldUpdateLabel (',' FieldUpdateLabel)* ','?)? '}'
FieldUpdateLabel ::= LabelPath ('=' Expr?)?
BlockElabCall ::= IDENT '!' BlockPrefix? NeutralBlock LabelledBlock* FinalElidedBlock?
NeutralBlock ::= '{' ';'* ((NeutralBinding | Expr ';') ';'*)* Expr? '}'
NeutralBinding ::= LetStmt | PatternLet | ExistentialLet | RowLet | NeutralBind
NeutralBind ::= 'let' NeutralBinder '<-' Expr ';'
NeutralBinder ::= LetBinder | '.' ParamPatternTuple
BangCall ::= IDENT '!' CallArgList
RecCall ::= 'rec' RecCallAnnotationList? IDENT CallArgList
CallArgList ::= '(' (CallArg (',' CallArg)*)? ','? ')'
NonemptyCallArgList ::= '(' CallArg (',' CallArg)* ','? ')'
DotPlaceholderIntro ::= '.' IDENT '.'
PlaceholderLambda ::= DotPlaceholderIntro Block
PrefixOpExpr ::= FixedOpToken+ Expr
DotSpliceSuffix ::= ('.>' | '.>>') DotSpliceCallee NonemptyCallArgList?
LeftCallSplice ::= LeftSpliceCallee ('.<' | '.<<') TightCallArg
LeftSpliceCallee ::= LeftSplicePathCallee | IDENT '!' NonemptyCallArgList?
LeftSplicePathCallee ::= IDENT ('.' IDENT)* NonemptyCallArgList? | '(' LeftSplicePathCallee ')'
TightCallArg ::= ExprAtom CallArgList*
OperatorTail ::= (FixedOpToken Expr)+
PatternLet ::= 'let' '.' ParamPatternTuple '=' Expr ';'
ParamPatternElem ::= IDENT (':' Type)? | '_' (':' Type)? | ParamPatternTuple | IDENT ':' ParamPatternTuple
ExistentialLet ::= 'let' '.' '(' ExistsBinder+ ExistentialLetBinder ')' '=' Expr ';'
ExistentialLetBinder ::= IDENT | ParamPatternTuple
RowLet ::= 'let' '.' '(' '{' RowLetEntry (',' RowLetEntry)* ','? '}' ')' '=' Expr ';'
TrailingBlockDecl ::= 'trailing' BlockExposure IDENT?
BlockExposure ::= 'product' | 'thunk' | 'sequence'
BlockPrefix ::= BlockValueArgs | BarePrefixExpr
BlockValueArgs ::= '(' (Expr (',' Expr)*)? ','? ')'
BarePrefixExpr ::= Expr
LabelledBlock ::= IDENT NeutralBlock
FinalElidedBlock ::= IDENT BlockElabCall"#;

fn strip_ebnf_comments(line: &str, comment_depth: &mut usize) -> String {
    let chars = line.chars().collect::<Vec<_>>();
    let mut clean = String::new();
    let mut index = 0;
    while index < chars.len() {
        if index + 1 < chars.len() && chars[index] == '(' && chars[index + 1] == '*' {
            *comment_depth += 1;
            index += 2;
        } else if index + 1 < chars.len()
            && chars[index] == '*'
            && chars[index + 1] == ')'
            && *comment_depth > 0
        {
            *comment_depth -= 1;
            index += 2;
        } else {
            if *comment_depth == 0 {
                clean.push(chars[index]);
            }
            index += 1;
        }
    }
    clean
}

fn normalized_grammar_piece(piece: &str) -> String {
    piece.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn grammar_definition_start(line: &str) -> Option<(&str, &str, &str)> {
    for operator in ["::=", "+="] {
        if let Some(index) = line.find(operator) {
            let name = line[..index].trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                return Some((name, operator, &line[index + operator.len()..]));
            }
        }
    }
    None
}

// This deliberately reads only fenced production definitions; it is a bounded
// inventory guard, not a general Markdown or EBNF parser.
fn grammar_definitions() -> Vec<GrammarDefinition> {
    fn finish(definitions: &mut Vec<GrammarDefinition>, current: &mut Option<GrammarDefinition>) {
        if let Some(definition) = current.take() {
            definitions.push(definition);
        }
    }

    let mut definitions = Vec::new();
    let mut current = None;
    let mut in_ebnf = false;
    let mut comment_depth = 0;
    for line in include_str!("../../../../specs/grammar.md").lines() {
        let trimmed = line.trim();
        if !in_ebnf && trimmed == "```ebnf" {
            in_ebnf = true;
            continue;
        }
        if in_ebnf && trimmed == "```" {
            finish(&mut definitions, &mut current);
            in_ebnf = false;
            assert_eq!(comment_depth, 0, "unterminated EBNF comment");
            continue;
        }
        if !in_ebnf {
            continue;
        }

        let clean = strip_ebnf_comments(line, &mut comment_depth);
        let piece = normalized_grammar_piece(&clean);
        if piece.is_empty() {
            continue;
        }
        if let Some((name, operator, rhs)) = grammar_definition_start(&piece) {
            finish(&mut definitions, &mut current);
            current = Some(GrammarDefinition {
                name: name.to_owned(),
                operator: operator.to_owned(),
                rhs: normalized_grammar_piece(rhs),
            });
        } else if let Some(definition) = current.as_mut() {
            if !definition.rhs.is_empty() {
                definition.rhs.push(' ');
            }
            definition.rhs.push_str(&piece);
        }
    }
    assert!(!in_ebnf, "unterminated EBNF fence");
    definitions
}

fn grammar_rhs_mentions(rhs: &str, name: &str) -> bool {
    rhs.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word == name)
}

#[test]
fn expression_grammar_inventory_is_bounded_and_fail_closed() {
    use std::collections::{BTreeMap, BTreeSet};

    let definitions = grammar_definitions();
    let known_names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut row_ids = BTreeSet::new();
    for (axis, id, grammar) in EXPR_FORMS
        .iter()
        .map(|row| ("expression form", row.id, row.grammar))
        .chain(
            EXPR_CONTEXTS
                .iter()
                .map(|row| ("expression context", row.id, row.grammar)),
        )
        .chain(
            TYPE_FORMS
                .iter()
                .map(|row| ("type form", row.id, row.grammar)),
        )
        .chain(
            TYPE_CONTEXTS
                .iter()
                .map(|row| ("type context", row.id, row.grammar)),
        )
    {
        assert!(row_ids.insert((axis, id)), "duplicate {axis} row `{id}`");
        assert!(
            MONITORED_GRAMMAR.contains(&grammar),
            "{axis} `{id}` names unmonitored grammar production `{grammar}`"
        );
        assert!(
            known_names.contains(grammar),
            "{axis} `{id}` names missing grammar production `{grammar}`"
        );
    }

    let mut direct_expr_owners = definitions
        .iter()
        .filter(|definition| grammar_rhs_mentions(&definition.rhs, "Expr"))
        .map(|definition| definition.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    direct_expr_owners.sort_unstable();
    assert_eq!(
        direct_expr_owners, DIRECT_EXPR_OWNERS,
        "the set of grammar contexts that directly contain `Expr` changed; classify the new boundary before updating this inventory"
    );

    let mut direct_type_owners = definitions
        .iter()
        .filter(|definition| grammar_rhs_mentions(&definition.rhs, "Type"))
        .map(|definition| definition.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    direct_type_owners.sort_unstable();
    assert_eq!(
        direct_type_owners, DIRECT_TYPE_OWNERS,
        "the set of grammar contexts that directly contain `Type` changed; classify the new boundary before updating this inventory"
    );

    let matrix_type_owners = TYPE_CONTEXTS
        .iter()
        .map(|context| context.grammar)
        .collect::<BTreeSet<_>>();
    let non_matrix_type_owners = NON_MATRIX_TYPE_OWNERS
        .iter()
        .map(|(owner, reason)| {
            assert!(
                !reason.is_empty(),
                "non-matrix type owner `{owner}` needs a reason"
            );
            *owner
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        matrix_type_owners
            .union(&non_matrix_type_owners)
            .copied()
            .collect::<BTreeSet<_>>(),
        direct_type_owners.iter().copied().collect::<BTreeSet<_>>(),
        "every direct `Type` owner must be either a matrix context or an explicit non-matrix disposition"
    );
    assert!(
        matrix_type_owners.is_disjoint(&non_matrix_type_owners),
        "a direct `Type` owner cannot be both matrix-covered and dispositioned away"
    );

    let monitored = MONITORED_GRAMMAR.iter().copied().collect::<BTreeSet<_>>();
    let mut by_name = BTreeMap::<&str, Vec<&GrammarDefinition>>::new();
    for definition in &definitions {
        if monitored.contains(definition.name.as_str()) {
            by_name
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
    }
    assert_eq!(
        by_name.keys().copied().collect::<BTreeSet<_>>(),
        monitored,
        "a monitored expression-grammar production disappeared"
    );
    let snapshot = MONITORED_GRAMMAR
        .iter()
        .flat_map(|name| {
            by_name[name].iter().map(|definition| {
                format!(
                    "{} {} {}",
                    definition.name, definition.operator, definition.rhs
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        snapshot,
        EXPECTED_EXPRESSION_GRAMMAR.trim(),
        "the bounded expression/type grammar changed; update the declarative axes or boundary controls before accepting the new snapshot\n\nactual snapshot:\n{snapshot}"
    );
}

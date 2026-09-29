//! Statement-oriented Go conversion over prepared boundary facades.
//!
//! This module consumes only contextual [`super::facade`] cursors and opaque
//! live execution capabilities. Public type spellings, structural fields, sum
//! cases, constructors, and nominal carriers stay owned by the facade. A
//! declaration head's ABI partition remains inside its live capability, while
//! a nested function's partition comes from its effective live view.
//!
//! Callable declaration binders are consumed before a live cursor is minted.
//! A nested `Forall` view is itself proof of the paired invocation action, so
//! neither path accepts a detached action or reconstructs one from a type.
//!
//! Every conversion writes its work into the caller's statement block and
//! returns one fresh local. This keeps source evaluation single-shot and
//! preserves left-to-right conversion order without rebuilding expression
//! trees from immediately-invoked closures.

use std::fmt;

use crate::backends::boundary_facade::{CallableSourceParamAdapter, CallableValueStageLayout};
use crate::backends::skin::FfiDir;

use super::facade::{
    GoFacadeError, GoFunctionUse, GoLiveFacadeUseRef, GoLiveFacadeUseView, GoLiveTypeHeadStage,
    GoLiveValueHeadStage, GoLiveValueSourceGroup, GoLiveValueSourceGroupView, GoProductUse,
    GoSumUse,
};

const ERASED_TYPE: &str = "any";
const UNIT_TYPE: &str = "Unit";

/// Statement operations the facade converter needs from a Go function body.
///
/// Implementations retain indentation and lexical ownership. `line` must
/// indent every line in a multiline fragment, while `open` writes its header
/// plus the opening brace and enters the block.
pub(crate) trait GoStatementSink {
    fn line(&mut self, source: &str);
    fn open(&mut self, header: &str);
    fn indent(&mut self);
    fn dedent(&mut self);
    fn close(&mut self);
}

/// Capture-proof fresh Go value names for one generated function.
///
/// The implementation must return a distinct valid identifier on every call,
/// from the backend's generated-only class outside the image of legal Kio
/// binders. `class` is diagnostic grouping only and cannot be a semantic name.
pub(crate) trait GoFreshNames {
    fn fresh(&mut self, class: &'static str) -> String;
    fn host_expression(&self) -> &str;
}

/// Failure to render one prepared live boundary conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GoFacadeSkinError {
    message: String,
}

impl GoFacadeSkinError {
    fn invariant(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GoFacadeSkinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GoFacadeSkinError {}

impl From<GoFacadeError> for GoFacadeSkinError {
    fn from(error: GoFacadeError) -> Self {
        Self::invariant(error.to_string())
    }
}

/// Convert one live contextual value between the typed facade and erased body.
///
/// `In` maps a typed host value to `any`; `Out` maps an erased body value to
/// the cursor's exact boundary type. The returned identifier owns the result,
/// and `expression` is evaluated exactly once by the emitted statements.
pub(crate) fn convert<S, N>(
    cursor: &GoLiveFacadeUseRef<'_, '_>,
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let boundary_type = cursor.boundary_type()?;
    let target_type = match direction {
        FfiDir::In => ERASED_TYPE,
        FfiDir::Out => boundary_type.as_str(),
    };
    let converted = convert_unmaterialized(cursor, expression, direction, statements, names)?;
    Ok(materialize(
        statements,
        names,
        "facade_value",
        target_type,
        &converted,
    ))
}

/// Invoke one erased declaration-owned type stage.
///
/// A method value and a later erased closure use the same route: first capture
/// the callee as `any`, then assert and invoke exactly one nullary stage.
pub(crate) fn invoke_type_stage<S, N>(
    stage: &GoLiveTypeHeadStage<'_>,
    expression: &str,
    statements: &mut S,
    names: &mut N,
) -> String
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    stage.require_invoke_nullary();
    let callee = materialize(statements, names, "type_callee", ERASED_TYPE, expression);
    materialize(
        statements,
        names,
        "type_result",
        ERASED_TYPE,
        &format!("({callee}).(func() any)()"),
    )
}

/// Apply one declaration-owned value stage to typed host arguments.
///
/// The callee is evaluated before the arguments. Each argument then converts
/// `In` from left to right, and the capability's bound source groups map those
/// facade slots to the erased body's exact source-parameter ABI.
pub(crate) fn invoke_value_stage<S, N>(
    stage: &GoLiveValueHeadStage<'_, '_>,
    expression: &str,
    arguments: &[String],
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    if arguments.len() != stage.slots().len() {
        return Err(GoFacadeSkinError::invariant(
            "Go facade value stage argument count disagrees with its paired slots",
        ));
    }
    let source_groups = stage.source_groups()?;
    let callee = materialize(statements, names, "value_callee", ERASED_TYPE, expression);
    let body_arguments =
        convert_live_value_arguments(&source_groups, arguments, statements, names)?;
    let body_type = erased_function_type(source_groups.len());
    Ok(materialize(
        statements,
        names,
        "value_result",
        ERASED_TYPE,
        &format!("({callee}).({body_type})({})", body_arguments.join(", ")),
    ))
}

fn convert_live_value_arguments<S, N>(
    source_groups: &[GoLiveValueSourceGroup<'_, '_, '_>],
    arguments: &[String],
    statements: &mut S,
    names: &mut N,
) -> Result<Vec<String>, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let mut arguments = arguments.iter();
    let mut body_arguments = Vec::with_capacity(source_groups.len());
    for source_group in source_groups {
        let body_argument = match source_group.view() {
            GoLiveValueSourceGroupView::UnitValue => format!("{UNIT_TYPE}{{}}"),
            GoLiveValueSourceGroupView::Identity(slot) => {
                let argument = arguments.next().ok_or_else(|| {
                    GoFacadeSkinError::invariant(
                        "Go facade value stage exhausted its paired arguments",
                    )
                })?;
                convert(slot, argument, FfiDir::In, statements, names)?
            }
            GoLiveValueSourceGroupView::RightNest(slots) => {
                let mut converted = Vec::with_capacity(slots.len());
                for slot in slots {
                    let argument = arguments.next().ok_or_else(|| {
                        GoFacadeSkinError::invariant(
                            "Go facade value stage exhausted its paired arguments",
                        )
                    })?;
                    converted.push(convert(slot, argument, FfiDir::In, statements, names)?);
                }
                right_nest(&converted)?
            }
        };
        body_arguments.push(body_argument);
    }
    if arguments.next().is_some() {
        return Err(GoFacadeSkinError::invariant(
            "Go facade value stage left a paired argument unconsumed",
        ));
    }
    Ok(body_arguments)
}

fn convert_nested_function_arguments<S, N>(
    slots: &[GoLiveFacadeUseRef<'_, '_>],
    layout: &CallableValueStageLayout,
    arguments: &[String],
    statements: &mut S,
    names: &mut N,
) -> Result<Vec<String>, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    validate_layout(layout, slots.len())?;
    if arguments.len() != slots.len() {
        return Err(GoFacadeSkinError::invariant(
            "Go facade value stage argument count disagrees with its prepared slots",
        ));
    }

    let mut converted_slots = Vec::with_capacity(slots.len());
    for (slot, argument) in slots.iter().zip(arguments) {
        converted_slots.push(convert(slot, argument, FfiDir::In, statements, names)?);
    }
    group_facade_slots(layout, &converted_slots)
}

fn convert_unmaterialized<S, N>(
    cursor: &GoLiveFacadeUseRef<'_, '_>,
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    match cursor.view()? {
        GoLiveFacadeUseView::Unit => Ok(match direction {
            FfiDir::In => expression.to_owned(),
            FfiDir::Out => format!("({expression}).({UNIT_TYPE})"),
        }),
        GoLiveFacadeUseView::Bottom | GoLiveFacadeUseView::Erased { .. } => {
            Ok(expression.to_owned())
        }
        GoLiveFacadeUseView::ExactHost {
            boundary_type,
            role_adapter,
        } => Ok(match (direction, role_adapter) {
            (FfiDir::In, Some(adapter)) => format!(
                "{}.{}({expression})",
                names.host_expression(),
                adapter.convert_in()
            ),
            (FfiDir::Out, Some(adapter)) => format!(
                "{}.{}(({expression}).({}))",
                names.host_expression(),
                adapter.convert_out(),
                adapter.internal_type()
            ),
            (FfiDir::In, None) => expression.to_owned(),
            (FfiDir::Out, None) => format!("({expression}).({boundary_type})"),
        }),
        GoLiveFacadeUseView::HostCarrier {
            boundary_type,
            carrier,
        } => Ok(match direction {
            FfiDir::In => carrier.render_in(expression),
            FfiDir::Out => carrier.render_out(&boundary_type, expression),
        }),
        GoLiveFacadeUseView::Carrier { carrier, .. } => Ok(match direction {
            FfiDir::In => carrier.render_in(expression),
            FfiDir::Out => carrier.render_out(expression),
        }),
        GoLiveFacadeUseView::Transparent { payload, .. } => {
            convert(&payload, expression, direction, statements, names)
        }
        GoLiveFacadeUseView::Product { semantic, fields } => {
            convert_product(&semantic, &fields, expression, direction, statements, names)
        }
        GoLiveFacadeUseView::Sum { semantic, arms } => {
            convert_sum(&semantic, &arms, expression, direction, statements, names)
        }
        GoLiveFacadeUseView::Function {
            semantic,
            layout,
            slots,
            result,
        } => convert_function(
            &semantic, &slots, &result, layout, expression, direction, statements, names,
        ),
        GoLiveFacadeUseView::Forall { result, .. } => {
            convert_forall(&result, expression, direction, statements, names)
        }
    }
}

fn convert_product<S, N>(
    product: &GoProductUse<'_, '_>,
    fields: &[GoLiveFacadeUseRef<'_, '_>],
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    if product.fields().len() != fields.len() || fields.len() < 2 {
        return Err(GoFacadeSkinError::invariant(
            "Go facade product metadata has an invalid field inventory",
        ));
    }
    let source_type = match direction {
        FfiDir::In => product.boundary_type().as_str(),
        FfiDir::Out => ERASED_TYPE,
    };
    let source = materialize(statements, names, "product_source", source_type, expression);

    match direction {
        FfiDir::Out => {
            let slots = materialize(
                statements,
                names,
                "product_slots",
                "[]any",
                &format!("kioProductSlots({source}, {})", fields.len()),
            );
            let mut assignments = Vec::with_capacity(fields.len());
            for (index, (field, payload)) in product.fields().iter().zip(fields).enumerate() {
                let value = convert(
                    payload,
                    &format!("{slots}[{index}]"),
                    direction,
                    statements,
                    names,
                )?;
                assignments.push(format!("{}: {value}", field.name()));
            }
            Ok(format!(
                "{}{{{}}}",
                product.boundary_type(),
                assignments.join(", ")
            ))
        }
        FfiDir::In => {
            let mut values = Vec::with_capacity(fields.len());
            for (field, payload) in product.fields().iter().zip(fields) {
                values.push(convert(
                    payload,
                    &format!("{source}.{}", field.name()),
                    direction,
                    statements,
                    names,
                )?);
            }
            right_nest(&values)
        }
    }
}

fn convert_sum<S, N>(
    sum: &GoSumUse<'_, '_>,
    arms: &[GoLiveFacadeUseRef<'_, '_>],
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    if sum.arms().len() != arms.len() || arms.len() < 2 {
        return Err(GoFacadeSkinError::invariant(
            "Go facade sum metadata has an invalid arm inventory",
        ));
    }
    let source_type = match direction {
        FfiDir::In => sum.boundary_type().as_str(),
        FfiDir::Out => ERASED_TYPE,
    };
    let source = materialize(statements, names, "sum_source", source_type, expression);
    let result_type = match direction {
        FfiDir::In => ERASED_TYPE,
        FfiDir::Out => sum.boundary_type().as_str(),
    };
    let result = declare(statements, names, "sum_result", result_type);

    match direction {
        FfiDir::Out => {
            let variant = names.fresh("sum_variant");
            let payload = names.fresh("sum_payload");
            statements.line(&format!(
                "{variant}, {payload} := kioSumPayload({source}, {})",
                arms.len()
            ));
            statements.open(&format!("switch {variant}"));
            for (index, (arm, payload_cursor)) in sum.arms().iter().zip(arms).enumerate() {
                statements.line(&format!("case {index}:"));
                statements.indent();
                let converted = convert(payload_cursor, &payload, direction, statements, names)?;
                statements.line(&format!(
                    "{result} = {}",
                    arm.render_constructor(sum.row_alias(), &converted)
                ));
                statements.dedent();
            }
            statements.line("default:");
            statements.indent();
            statements.line("panic(\"kio internal: invalid facade sum variant\")");
            statements.dedent();
            statements.close();
        }
        FfiDir::In => {
            let case_value = materialize(
                statements,
                names,
                "sum_case",
                "kioCaseMarker",
                &format!("{source}.Case()"),
            );
            let selected = names.fresh("sum_selected");
            statements.open(&format!("switch {selected} := {case_value}.(type)"));
            for (index, (arm, payload_cursor)) in sum.arms().iter().zip(arms).enumerate() {
                statements.line(&format!("case {}:", arm.case_type(sum.row_alias())));
                statements.indent();
                let selected_payload = materialize(
                    statements,
                    names,
                    "sum_selected_payload",
                    arm.payload_type().as_str(),
                    &format!("{selected}.{}()", arm.value_accessor()),
                );
                let converted = convert(
                    payload_cursor,
                    &selected_payload,
                    direction,
                    statements,
                    names,
                )?;
                statements.line(&format!(
                    "{result} = kioSumInject({converted}, {index}, {})",
                    arms.len()
                ));
                statements.dedent();
            }
            statements.line("default:");
            statements.indent();
            statements.line("panic(\"kio internal: foreign facade sum case\")");
            statements.dedent();
            statements.close();
        }
    }

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn convert_function<S, N>(
    function: &GoFunctionUse<'_, '_>,
    slots: &[GoLiveFacadeUseRef<'_, '_>],
    result: &GoLiveFacadeUseRef<'_, '_>,
    layout: &CallableValueStageLayout,
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    if function.slots().len() != slots.len() {
        return Err(GoFacadeSkinError::invariant(
            "Go facade function metadata has misaligned semantic and live slots",
        ));
    }
    validate_layout(layout, slots.len())?;
    match direction {
        FfiDir::Out => function_out(slots, result, layout, expression, statements, names),
        FfiDir::In => function_in(
            function, slots, result, layout, expression, statements, names,
        ),
    }
}

fn function_out<S, N>(
    slots: &[GoLiveFacadeUseRef<'_, '_>],
    result: &GoLiveFacadeUseRef<'_, '_>,
    layout: &CallableValueStageLayout,
    expression: &str,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let body_type = erased_function_type(layout.body_abi_arity());
    let inner = materialize(
        statements,
        names,
        "function_inner",
        &body_type,
        &format!("({expression}).({body_type})"),
    );

    let mut parameters = Vec::with_capacity(slots.len());
    let mut arguments = Vec::with_capacity(slots.len());
    for slot in slots {
        let name = names.fresh("function_param");
        parameters.push(format!("{name} {}", slot.boundary_type()?));
        arguments.push(name);
    }
    let return_type = result.boundary_type()?;
    let mut body = GoSourceBlock::with_indent(1);
    let body_arguments =
        convert_nested_function_arguments(slots, layout, &arguments, &mut body, names)?;
    let call = materialize(
        &mut body,
        names,
        "function_call",
        ERASED_TYPE,
        &format!("{inner}({})", body_arguments.join(", ")),
    );
    let returned = convert(result, &call, FfiDir::Out, &mut body, names)?;
    body.line(&format!("return {returned}"));
    Ok(render_function_literal(
        &parameters,
        return_type.as_str(),
        body.finish(),
    ))
}

fn function_in<S, N>(
    function: &GoFunctionUse<'_, '_>,
    slots: &[GoLiveFacadeUseRef<'_, '_>],
    result: &GoLiveFacadeUseRef<'_, '_>,
    layout: &CallableValueStageLayout,
    expression: &str,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let inner = materialize(
        statements,
        names,
        "function_inner",
        function.boundary_type().as_str(),
        expression,
    );
    let parameters = (0..layout.body_abi_arity())
        .map(|_| names.fresh("function_param"))
        .collect::<Vec<_>>();
    let declarations = parameters
        .iter()
        .map(|name| format!("{name} any"))
        .collect::<Vec<_>>();
    let mut body = GoSourceBlock::with_indent(1);
    let facade_arguments = expand_body_arguments(layout, slots, &parameters, &mut body, names)?;
    let call = materialize(
        &mut body,
        names,
        "function_call",
        result.boundary_type()?.as_str(),
        &format!("{inner}({})", facade_arguments.join(", ")),
    );
    let returned = convert(result, &call, FfiDir::In, &mut body, names)?;
    body.line(&format!("return {returned}"));
    Ok(render_function_literal(
        &declarations,
        ERASED_TYPE,
        body.finish(),
    ))
}

fn convert_forall<S, N>(
    result: &GoLiveFacadeUseRef<'_, '_>,
    expression: &str,
    direction: FfiDir,
    statements: &mut S,
    names: &mut N,
) -> Result<String, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    match direction {
        FfiDir::Out => {
            let callee = materialize(statements, names, "forall_callee", ERASED_TYPE, expression);
            let applied = materialize(
                statements,
                names,
                "forall_result",
                ERASED_TYPE,
                &format!("({callee}).(func() any)()"),
            );
            convert(result, &applied, direction, statements, names)
        }
        FfiDir::In => {
            let captured = convert(result, expression, direction, statements, names)?;
            Ok(format!("func() any {{ return {captured} }}"))
        }
    }
}

fn validate_layout(
    layout: &CallableValueStageLayout,
    facade_slot_count: usize,
) -> Result<(), GoFacadeSkinError> {
    if layout.facade_slot_count() != facade_slot_count
        || layout.source_param_count() != layout.body_abi_arity()
        || layout.source_params().len() != layout.body_abi_arity()
    {
        return Err(GoFacadeSkinError::invariant(
            "Go facade function layout disagrees with its prepared slot inventory",
        ));
    }
    Ok(())
}

fn group_facade_slots(
    layout: &CallableValueStageLayout,
    slots: &[String],
) -> Result<Vec<String>, GoFacadeSkinError> {
    let mut grouped = Vec::with_capacity(layout.body_abi_arity());
    for source in layout.source_params() {
        let range = source.facade_slots();
        let values = slots.get(range.clone()).ok_or_else(|| {
            GoFacadeSkinError::invariant(
                "Go facade source parameter range is outside its prepared slot inventory",
            )
        })?;
        let value = match source.adapter() {
            CallableSourceParamAdapter::UnitValue if values.is_empty() => {
                format!("{UNIT_TYPE}{{}}")
            }
            CallableSourceParamAdapter::Identity if values.len() == 1 => values[0].clone(),
            CallableSourceParamAdapter::RightNest if values.len() > 1 => right_nest(values)?,
            _ => {
                return Err(GoFacadeSkinError::invariant(
                    "Go facade source adapter disagrees with its prepared slot range",
                ));
            }
        };
        grouped.push(value);
    }
    Ok(grouped)
}

fn expand_body_arguments<S, N>(
    layout: &CallableValueStageLayout,
    slots: &[GoLiveFacadeUseRef<'_, '_>],
    body_arguments: &[String],
    statements: &mut S,
    names: &mut N,
) -> Result<Vec<String>, GoFacadeSkinError>
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    if body_arguments.len() != layout.body_abi_arity() {
        return Err(GoFacadeSkinError::invariant(
            "Go erased function parameter count disagrees with its prepared ABI",
        ));
    }
    let mut facade_arguments = vec![None; slots.len()];
    for ((source, argument), source_index) in layout
        .source_params()
        .iter()
        .zip(body_arguments)
        .zip(0usize..)
    {
        let range = source.facade_slots();
        let range_slots = slots.get(range.clone()).ok_or_else(|| {
            GoFacadeSkinError::invariant(
                "Go facade source parameter range is outside its prepared slot inventory",
            )
        })?;
        match source.adapter() {
            CallableSourceParamAdapter::UnitValue if range_slots.is_empty() => {}
            CallableSourceParamAdapter::Identity if range_slots.len() == 1 => {
                facade_arguments[range.start] = Some(convert(
                    &range_slots[0],
                    argument,
                    FfiDir::Out,
                    statements,
                    names,
                )?);
            }
            CallableSourceParamAdapter::RightNest if range_slots.len() > 1 => {
                let expanded = materialize(
                    statements,
                    names,
                    "function_slots",
                    "[]any",
                    &format!("kioProductSlots({argument}, {})", range_slots.len()),
                );
                for (offset, slot) in range_slots.iter().enumerate() {
                    facade_arguments[range.start + offset] = Some(convert(
                        slot,
                        &format!("{expanded}[{offset}]"),
                        FfiDir::Out,
                        statements,
                        names,
                    )?);
                }
            }
            _ => {
                return Err(GoFacadeSkinError::invariant(format!(
                    "Go facade source adapter {source_index} disagrees with its prepared slot range"
                )));
            }
        }
    }
    facade_arguments
        .into_iter()
        .map(|argument| {
            argument.ok_or_else(|| {
                GoFacadeSkinError::invariant(
                    "Go facade function layout left one public slot unassigned",
                )
            })
        })
        .collect()
}

fn right_nest(values: &[String]) -> Result<String, GoFacadeSkinError> {
    let Some((last, prefix)) = values.split_last() else {
        return Err(GoFacadeSkinError::invariant(
            "an empty facade slot range cannot form an erased product",
        ));
    };
    let mut nested = last.clone();
    for value in prefix.iter().rev() {
        nested = format!("[]any{{{value}, {nested}}}");
    }
    Ok(nested)
}

fn erased_function_type(arity: usize) -> String {
    format!("func({}) any", vec![ERASED_TYPE; arity].join(", "))
}

fn materialize<S, N>(
    statements: &mut S,
    names: &mut N,
    class: &'static str,
    ty: &str,
    expression: &str,
) -> String
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let name = names.fresh(class);
    statements.line(&format!("var {name} {ty}"));
    statements.line(&format!("{name} = {expression}"));
    name
}

fn declare<S, N>(statements: &mut S, names: &mut N, class: &'static str, ty: &str) -> String
where
    S: GoStatementSink,
    N: GoFreshNames,
{
    let name = names.fresh(class);
    statements.line(&format!("var {name} {ty}"));
    name
}

fn render_function_literal(parameters: &[String], ret: &str, body: String) -> String {
    format!("func({}) {ret} {{\n{body}}}", parameters.join(", "))
}

#[derive(Default)]
struct GoSourceBlock {
    source: String,
    indent: usize,
}

impl GoSourceBlock {
    fn with_indent(indent: usize) -> Self {
        Self {
            source: String::new(),
            indent,
        }
    }

    fn finish(self) -> String {
        self.source
    }
}

impl GoStatementSink for GoSourceBlock {
    fn line(&mut self, source: &str) {
        for line in source.lines() {
            self.source.push_str(&"\t".repeat(self.indent));
            self.source.push_str(line);
            self.source.push('\n');
        }
    }

    fn open(&mut self, header: &str) {
        self.line(&format!("{header} {{"));
        self.indent += 1;
    }

    fn indent(&mut self) {
        self.indent += 1;
    }

    fn dedent(&mut self) {
        self.indent = self
            .indent
            .checked_sub(1)
            .expect("Go facade block indentation underflow");
    }

    fn close(&mut self) {
        self.dedent();
        self.line("}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_nesting_preserves_slot_order() {
        assert_eq!(right_nest(&["a".to_owned()]).unwrap(), "a");
        assert_eq!(
            right_nest(&["a".to_owned(), "b".to_owned(), "c".to_owned()]).unwrap(),
            "[]any{a, []any{b, c}}"
        );
    }

    #[test]
    fn erased_function_type_distinguishes_zero_from_one_parameter() {
        assert_eq!(erased_function_type(0), "func() any");
        assert_eq!(erased_function_type(1), "func(any) any");
    }

    #[test]
    fn source_block_nests_switch_cases_without_iifes() {
        let mut block = GoSourceBlock::with_indent(1);
        block.open("switch tag");
        block.line("case 0:");
        block.indent();
        block.line("return value");
        block.dedent();
        block.close();
        assert_eq!(
            block.finish(),
            "\tswitch tag {\n\t\tcase 0:\n\t\t\treturn value\n\t}\n"
        );
    }
}

/// Causal converter tests prepare real surface packages so every cursor and
/// live stage capability comes from the production facade transaction. The
/// production converter above never receives a package or an unpaired plan.
#[cfg(all(test, feature = "surface"))]
mod prepared_tests {
    use super::super::facade::{GoFacadeCatalog, GoLiveCallableEntry};
    use super::*;
    use crate::ast::Routed;
    use crate::backends::boundary_facade::{
        BoundaryFacadeSiteId, BoundaryFacadeSiteOwner, PreparedBoundaryCallableSites,
    };
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::{parse, parse_package_file, parse_signature_file};
    use crate::pass::resolve::{Package, PackageFileEntry};
    use crate::pass::structural_recovery;
    use crate::pass::typecheck_full::check_package;
    use crate::pipeline::Pipeline;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    const TEXT_HOST: &str = "KioHost_V1_M1_C3_apiN4_Text";

    const LIVE_SOURCE: &str = "module api; \
         host type I32 role(i32); \
         host type Str role(str); \
         pub newtype Id[A] : A { \
             pub constructor make_id; \
             pub projector read_id; \
         }; \
         host fn abi0() -> I32; \
         host fn unitful(value: .) -> I32; \
         host fn regroup(first: I32, rest: Str & I32) -> I32; \
         host fn product() -> (I32 & Str & I32); \
         host fn sum() -> (I32 | Str); \
         host fn nested_function() -> Id((I32 & Str & I32) -> I32); \
         host fn nested_forall() -> Id([A] A -> A); \
         host fn staged[A](value: I32) -> I32;";

    #[derive(Default)]
    struct TestNames {
        next: usize,
    }

    impl GoFreshNames for TestNames {
        fn fresh(&mut self, class: &'static str) -> String {
            let next = self.next;
            self.next += 1;
            format!("kioFacadeTest_{class}_{next}")
        }

        fn host_expression(&self) -> &str {
            "host"
        }
    }

    fn package(source: &str) -> Package<Routed> {
        let parsed = vec![(
            PathBuf::from("api.kio"),
            parse(source).expect("parse facade-skin test module"),
        )];
        let package_file = parse_package_file(
            "package pkg; \
             build { target go { out \"out/go/\"; } } \
             bridge { api; }",
            None,
        )
        .expect("parse facade-skin test package file");
        let (modules, package_file) = FullPipeline::lower_package(parsed, Some(package_file))
            .expect("lower facade-skin test package");
        let package_file = package_file.map(|package_file| PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file,
        });
        let package = Package::build(Path::new(""), modules, package_file)
            .expect("build facade-skin test package");
        package
            .resolve_imports()
            .expect("resolve facade-skin test uses");
        package
            .check_in_body_resolution()
            .expect("check facade-skin test body resolution");
        let prime = check_package(&package).expect("typecheck facade-skin test package");
        crate::pass::recover_to_low::lower(&structural_recovery::recover_package(&prime))
    }

    fn host_site(name: &str) -> BoundaryFacadeSiteId {
        BoundaryFacadeSiteId::new(
            vec!["api".to_owned()],
            BoundaryFacadeSiteOwner::HostFunction {
                name: name.to_owned(),
            },
        )
        .expect("valid facade-skin host site")
    }

    fn live_entry<'site, 'source>(
        catalog: &'site GoFacadeCatalog<'source>,
        name: &str,
    ) -> GoLiveCallableEntry<'site, 'source> {
        catalog
            .site(&host_site(name))
            .expect("prepared Go facade-skin host site")
            .live()
            .expect("live Go facade-skin host site")
            .callable_entry()
            .expect("paired Go facade-skin callable entry")
    }

    fn only_value_head<'entry, 'site, 'source>(
        entry: &'entry GoLiveCallableEntry<'site, 'source>,
    ) -> &'entry GoLiveValueHeadStage<'site, 'source> {
        assert_eq!(entry.stages().len(), 1);
        entry.stages()[0]
            .value_stage()
            .expect("one live value head")
    }

    fn render_conversion(
        cursor: &GoLiveFacadeUseRef<'_, '_>,
        expression: &str,
        direction: FfiDir,
    ) -> String {
        let mut statements = GoSourceBlock::default();
        let mut names = TestNames::default();
        let value = convert(cursor, expression, direction, &mut statements, &mut names)
            .expect("render prepared facade conversion");
        statements.line(&format!("_ = {value}"));
        statements.finish()
    }

    fn render_value_invocation(
        stage: &GoLiveValueHeadStage<'_, '_>,
        expression: &str,
        arguments: &[String],
    ) -> String {
        let mut statements = GoSourceBlock::default();
        let mut names = TestNames::default();
        let value = invoke_value_stage(stage, expression, arguments, &mut statements, &mut names)
            .expect("render prepared value-head invocation");
        statements.line(&format!("_ = {value}"));
        statements.finish()
    }

    fn assert_once(source: &str, needle: &str) {
        assert_eq!(
            source.matches(needle).count(),
            1,
            "expected one `{needle}` in:\n{source}",
        );
    }

    fn assert_in_order(source: &str, needles: &[String]) {
        let mut rest = source;
        for needle in needles {
            let offset = rest
                .find(needle)
                .unwrap_or_else(|| panic!("missing ordered `{needle}` in:\n{source}"));
            rest = &rest[offset + needle.len()..];
        }
    }

    #[test]
    fn declaration_value_heads_keep_canonical_unit_nullary() {
        let package = package(LIVE_SOURCE);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare facade-skin declaration stages");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize facade-skin declaration stages");

        let abi0_entry = live_entry(&catalog, "abi0");
        let abi0 = only_value_head(&abi0_entry);
        assert!(abi0.slots().is_empty());
        let abi0_source = render_value_invocation(abi0, "nextAbi0()", &[]);
        assert_once(&abi0_source, "nextAbi0()");
        assert!(abi0_source.contains(".(func() any)()"));

        let unit_entry = live_entry(&catalog, "unitful");
        let unit = only_value_head(&unit_entry);
        assert!(unit.slots().is_empty());
        let unit_source = render_value_invocation(unit, "nextUnit()", &[]);
        assert_once(&unit_source, "nextUnit()");
        assert!(unit_source.contains(".(func() any)()"));

        let regroup_entry = live_entry(&catalog, "regroup");
        let regroup = only_value_head(&regroup_entry);
        let arguments = [
            "nextLeft()".to_owned(),
            "nextMiddle()".to_owned(),
            "nextRight()".to_owned(),
        ];
        let regroup_source = render_value_invocation(regroup, "nextCallee()", &arguments);
        assert_in_order(
            &regroup_source,
            &[
                "nextCallee()".to_owned(),
                "nextLeft()".to_owned(),
                "nextMiddle()".to_owned(),
                "nextRight()".to_owned(),
            ],
        );
        for expression in ["nextCallee()", "nextLeft()", "nextMiddle()", "nextRight()"] {
            assert_once(&regroup_source, expression);
        }
        assert_eq!(regroup_source.matches("[]any{").count(), 1);
        assert!(regroup_source.contains(".(func(any, any) any)("));
    }

    #[test]
    fn substituted_unit_function_keeps_one_public_slot() {
        let package = package(
            "module api; \
             host type Text role(str); \
             pub type Callback[A] = A -> Text; \
             host fn substituted() -> Callback(.); \
             host fn direct() -> . -> Text;",
        );
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare substituted Unit callback sites");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize substituted Unit callback facades");

        let substituted = live_entry(&catalog, "substituted");
        let (substituted_slots, substituted_layout) = match substituted
            .returned()
            .view()
            .expect("classify substituted Unit callback")
        {
            GoLiveFacadeUseView::Function { slots, layout, .. } => (slots, layout),
            _ => panic!("substituted callback must remain a Function"),
        };
        assert_eq!(substituted_slots.len(), 1);
        assert_eq!(
            substituted_slots[0]
                .boundary_type()
                .expect("substituted Unit slot type")
                .as_str(),
            "Unit"
        );
        assert_eq!(substituted_layout.body_abi_arity(), 1);
        let [source] = substituted_layout.source_params() else {
            panic!("substituted callback must retain one source parameter")
        };
        assert_eq!(source.facade_slots(), 0..1);
        assert_eq!(source.adapter(), CallableSourceParamAdapter::Identity);
        assert_eq!(
            substituted
                .returned()
                .boundary_type()
                .expect("substituted callback boundary type")
                .as_str(),
            format!("func(Unit) {TEXT_HOST}")
        );
        let substituted_out =
            render_conversion(substituted.returned(), "nextSubstituted()", FfiDir::Out);
        assert!(substituted_out.contains(&format!(" Unit) {TEXT_HOST}")));

        let direct = live_entry(&catalog, "direct");
        let (direct_slots, direct_layout) = match direct
            .returned()
            .view()
            .expect("classify direct Unit callback")
        {
            GoLiveFacadeUseView::Function { slots, layout, .. } => (slots, layout),
            _ => panic!("direct callback must remain a Function"),
        };
        assert!(direct_slots.is_empty());
        assert_eq!(direct_layout.body_abi_arity(), 0);
        assert!(direct_layout.source_params().is_empty());
        assert_eq!(
            direct
                .returned()
                .boundary_type()
                .expect("direct callback boundary type")
                .as_str(),
            format!("func() {TEXT_HOST}")
        );
        let direct_out = render_conversion(direct.returned(), "nextDirect()", FfiDir::Out);
        assert!(direct_out.contains(&format!("func() {TEXT_HOST}")));
    }

    #[test]
    fn product_and_sum_convert_once_in_public_order() {
        let package = package(LIVE_SOURCE);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare facade-skin structures");
        let catalog = GoFacadeCatalog::prepare(&prepared).expect("realize facade-skin structures");

        let product_entry = live_entry(&catalog, "product");
        let product = match product_entry.returned().view().unwrap() {
            GoLiveFacadeUseView::Product { semantic, .. } => semantic,
            _ => panic!("product return must retain its live Product view"),
        };
        let product_out =
            render_conversion(product_entry.returned(), "nextProductOut()", FfiDir::Out);
        assert_once(&product_out, "nextProductOut()");
        assert!(product_out.contains("kioProductSlots("));
        assert_in_order(
            &product_out,
            &["[0]".to_owned(), "[1]".to_owned(), "[2]".to_owned()],
        );
        let product_in = render_conversion(product_entry.returned(), "nextProductIn()", FfiDir::In);
        assert_once(&product_in, "nextProductIn()");
        assert_eq!(product_in.matches("[]any{").count(), 2);
        let field_accesses = product
            .fields()
            .iter()
            .map(|field| format!(".{}", field.name()))
            .collect::<Vec<_>>();
        assert_in_order(&product_in, &field_accesses);

        let sum_entry = live_entry(&catalog, "sum");
        let sum = match sum_entry.returned().view().unwrap() {
            GoLiveFacadeUseView::Sum { semantic, .. } => semantic,
            _ => panic!("sum return must retain its live Sum view"),
        };
        let sum_out = render_conversion(sum_entry.returned(), "nextSumOut()", FfiDir::Out);
        assert_once(&sum_out, "nextSumOut()");
        assert!(sum_out.contains("kioSumPayload("));
        let constructors = sum
            .arms()
            .iter()
            .map(|arm| arm.constructor().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_in_order(&sum_out, &constructors);

        let sum_in = render_conversion(sum_entry.returned(), "nextSumIn()", FfiDir::In);
        assert_once(&sum_in, "nextSumIn()");
        assert_once(&sum_in, ".Case()");
        assert_eq!(sum_in.matches("kioSumInject(").count(), sum.arms().len());
        let case_types = sum
            .arms()
            .iter()
            .map(|arm| arm.case_type(sum.row_alias()).into_string())
            .collect::<Vec<_>>();
        assert_in_order(&sum_in, &case_types);
        let mut value_calls = BTreeMap::new();
        for arm in sum.arms() {
            *value_calls
                .entry(format!(".{}()", arm.value_accessor()))
                .or_insert(0usize) += 1;
        }
        for (value_call, expected) in value_calls {
            assert_eq!(sum_in.matches(&value_call).count(), expected);
        }
    }

    #[test]
    fn substituted_function_and_forall_keep_live_execution_and_timing() {
        let package = package(LIVE_SOURCE);
        let prepared = PreparedBoundaryCallableSites::collect_live(&package)
            .expect("prepare substituted facade-skin values");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize substituted facade-skin values");

        let function_entry = live_entry(&catalog, "nested_function");
        let function_out =
            render_conversion(function_entry.returned(), "nextFunctionOut()", FfiDir::Out);
        assert_once(&function_out, "nextFunctionOut()");
        assert_eq!(function_out.matches(".(func(any) any)").count(), 1);
        assert_eq!(function_out.matches("[]any{").count(), 2);
        let function_in =
            render_conversion(function_entry.returned(), "nextFunctionIn()", FfiDir::In);
        assert_once(&function_in, "nextFunctionIn()");
        assert_eq!(function_in.matches("kioProductSlots(").count(), 1);
        assert_in_order(
            &function_in,
            &["[0]".to_owned(), "[1]".to_owned(), "[2]".to_owned()],
        );
        assert!(function_in.contains("func("));

        let forall_entry = live_entry(&catalog, "nested_forall");
        let forall_out = render_conversion(forall_entry.returned(), "nextForallOut()", FfiDir::Out);
        assert_once(&forall_out, "nextForallOut()");
        assert!(forall_out.contains(".(func() any)()"));
        let forall_in = render_conversion(forall_entry.returned(), "nextForallIn()", FfiDir::In);
        assert_once(&forall_in, "nextForallIn()");
        assert!(forall_in.contains("func() any { return"));
        assert_in_order(
            &forall_in,
            &[
                "nextForallIn()".to_owned(),
                "func() any { return".to_owned(),
            ],
        );

        let staged_entry = live_entry(&catalog, "staged");
        let type_head = staged_entry.stages()[0]
            .type_stage()
            .expect("staged callable starts with a live type head");
        let mut statements = GoSourceBlock::default();
        let mut names = TestNames::default();
        let value = invoke_type_stage(type_head, "nextTypeStage()", &mut statements, &mut names);
        statements.line(&format!("_ = {value}"));
        let type_source = statements.finish();
        assert_once(&type_source, "nextTypeStage()");
        assert!(type_source.contains(".(func() any)()"));
    }

    #[test]
    fn retained_sites_cannot_supply_live_converter_capabilities() {
        let package = package("module api;");
        let signature = parse_signature_file(
            r#"signature pkg v(2);
v(1) {
  nonbreaking {
    add {
      module api {
        host fn old(value: .) -> .;
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
            None,
        )
        .expect("parse retained facade-skin signature");
        let replayed =
            crate::sig::replay(&signature).expect("replay retained facade-skin signature");
        let prepared = PreparedBoundaryCallableSites::collect(&package, Some(&replayed))
            .expect("prepare retained facade-skin site");
        let catalog =
            GoFacadeCatalog::prepare(&prepared).expect("realize retained facade-skin site");

        assert_eq!(catalog.live_sites().count(), 0);
        let retained = catalog
            .site(&host_site("old"))
            .expect("retained Go facade-skin host site");
        assert!(retained.semantic_root().is_ok());
        assert!(retained.live().is_none());
    }
}

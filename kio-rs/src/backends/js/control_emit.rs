use super::*;
use crate::backends::control_plan::{
    AtomKind, ChildRole, Completion, ControlEntry, Exit, Operand, Operation, Slot,
};

fn slot_name(slot: Slot) -> String {
    match slot {
        Slot::Binding(id) => format!("b{}", id.index()),
        Slot::Temporary(index) => format!("t{index}"),
    }
}

fn slot_value(slot: Slot) -> String {
    format!("__kio_{}", slot_name(slot))
}

fn snapshot(slots: &[Slot]) -> String {
    format!(
        "{{{}}}",
        slots
            .iter()
            .map(|slot| format!("{}: {}", slot_name(*slot), slot_value(*slot)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(super) fn lower_body(
    body: TailContext<'_>,
    top: &TopLevel,
    locals: &mut JsLocalScope<'_>,
    out: &mut String,
) -> Result<bool, EmitError> {
    let entry_id = locals.plan.entry_for_body(body.expr()).id;
    let control = std::rc::Rc::clone(&locals.control);
    let entry = control.entry(entry_id);
    if !entry.has_pending_work() {
        lower_expr_in_context(body, top, locals, out)?;
        return Ok(body.has_tail_transfer());
    }
    if locals.emitted_control_entries.insert(entry_id) {
        let program = render_program(entry, top, locals)?;
        locals.control_programs.push(program);
    }
    let initial = entry.blocks[entry.start]
        .live
        .iter()
        .map(|slot| {
            let Slot::Binding(binding) = slot else {
                unreachable!("a control entry starts with lexical inputs only")
            };
            let name = locals.plan.binding(*binding).name;
            locals.require_value(name);
            format!("{}: {}", slot_name(*slot), mangle_js_ident(name))
        })
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "__kioControl{}({}, {{{initial}}}, undefined, __kioTailCursor)",
        entry_id.index(),
        entry.start
    ));
    Ok(true)
}

fn jump(entry: &ControlEntry<'_>, block: usize, argument: Option<&str>, out: &mut String) {
    out.push_str(&format!(
        "__kioSaved = {}; __kioBlock = {block}; ",
        snapshot(&entry.blocks[block].live)
    ));
    if let Some(argument) = argument {
        out.push_str(&format!("__kioInput = {argument}; "));
    }
}

fn render_program(
    entry: &ControlEntry<'_>,
    top: &TopLevel,
    locals: &mut JsLocalScope<'_>,
) -> Result<String, EmitError> {
    let mut out = format!(
        "function __kioControl{}(__kioBlock, __kioSaved, __kioInput, __kioTailCursor) {{ for (;;) {{ switch (__kioBlock) {{ ",
        entry.entry.index()
    );
    for (index, block) in entry.blocks.iter().enumerate() {
        out.push_str(&format!("case {index}: {{ "));
        if !block.live.is_empty() {
            out.push_str(&format!("const {} = __kioSaved; ", snapshot(&block.live)));
        }
        if let Some(input) = block.input {
            out.push_str(&format!("const {} = __kioInput; ", slot_value(input)));
        }
        out.push_str("__kioSaved = undefined; __kioInput = undefined; ");
        for operation in &block.operations {
            match operation {
                Operation::Bind { binding, input, .. } => out.push_str(&format!(
                    "const {} = {}; ",
                    slot_value(Slot::Binding(*binding)),
                    slot_value(*input)
                )),
                Operation::Atom {
                    output,
                    source,
                    kind,
                    operands,
                } => {
                    let value = render_atom(source, *kind, operands, top, locals)?;
                    out.push_str(&format!("const {} = {value}; ", slot_value(*output)));
                }
            }
        }
        match &block.exit {
            Exit::Return(slot) => out.push_str(&format!("return {}; ", slot_value(*slot))),
            Exit::Jump { block, argument } => {
                jump(entry, *block, Some(&slot_value(*argument)), &mut out);
                out.push_str("continue; ");
            }
            Exit::Branch {
                condition,
                then_block,
                else_block,
                ..
            } => {
                out.push_str(&format!("if ({}) {{ ", slot_value(*condition)));
                jump(entry, *then_block, None, &mut out);
                out.push_str("} else { ");
                jump(entry, *else_block, None, &mut out);
                out.push_str("} continue; ");
            }
            Exit::Match {
                scrutinee, arms, ..
            } => {
                out.push_str(&format!(
                    "const __kioSum = __kioSumPayload({}, {}); switch (__kioSum[0]) {{ ",
                    slot_value(*scrutinee),
                    arms.len()
                ));
                for (variant, target) in arms.iter().enumerate() {
                    out.push_str(&format!("case {variant}: "));
                    jump(entry, *target, Some("__kioSum[1]"), &mut out);
                    out.push_str("break; ");
                }
                out.push_str("default: throw new Error('sum payload out of bounds'); } continue; ");
            }
            Exit::Invoke(invoke) => {
                let callee = slot_value(invoke.callee);
                let domain = canonical_js_domain(
                    &invoke
                        .arguments
                        .iter()
                        .copied()
                        .map(slot_value)
                        .collect::<Vec<_>>(),
                );
                let helper = match (invoke.stage, invoke.completion) {
                    (TailStage::Type, Completion::Return) => "__kioTailAdvance",
                    (TailStage::Value, Completion::Return) => "__kioTailCall",
                    (TailStage::Type, Completion::Resume(_)) => "__kioAdvanceWithResume",
                    (TailStage::Value, Completion::Resume(_)) => "__kioCallWithResume",
                };
                out.push_str(&format!("return {helper}(__kioTailCursor, {callee}"));
                if invoke.stage == TailStage::Value {
                    out.push_str(&format!(", {domain}"));
                }
                if let Completion::Resume(id) = invoke.completion {
                    let resume = &entry.resumes[id];
                    out.push_str(&format!(
                        ", __kioResume(__kioControl{}, {}, {})",
                        entry.entry.index(),
                        resume.block,
                        snapshot(&resume.live)
                    ));
                }
                out.push_str("); ");
            }
        }
        out.push_str("} ");
    }
    out.push_str("default: throw new Error('invalid checked control block'); } } } ");
    Ok(out)
}

fn operand(operands: &[Operand], role: ChildRole) -> String {
    let mut found = operands.iter().filter(|operand| operand.role == role);
    let value = found
        .next()
        .expect("a child-free operation has its checked operand");
    assert!(
        found.next().is_none(),
        "a child-free operation has one slot per child"
    );
    slot_value(value.slot)
}

fn render_atom(
    source: &Expr,
    kind: AtomKind,
    operands: &[Operand],
    top: &TopLevel,
    locals: &mut JsLocalScope<'_>,
) -> Result<String, EmitError> {
    let mut out = String::new();
    match (kind, source) {
        (AtomKind::Leaf, Expr::Unit { .. }) => out.push_str("null"),
        (AtomKind::Leaf, Expr::StrLit { value, .. }) => {
            out.push('"');
            push_js_escaped(value, &mut out);
            out.push('"');
        }
        (
            AtomKind::Leaf,
            Expr::IntLit {
                digits, annotation, ..
            },
        ) => {
            out.push_str(digits);
            if literal_annotation_is_wide_int(annotation, top) {
                out.push('n');
            }
        }
        (AtomKind::Leaf, Expr::FloatLit { digits, .. }) => out.push_str(digits),
        (AtomKind::Leaf, Expr::BoolLit { value, .. }) => {
            out.push_str(if *value { "true" } else { "false" })
        }
        (
            AtomKind::Leaf,
            Expr::LowHostFnValueRef {
                name,
                module_path,
                sig,
                ..
            },
        ) => out = emit_host_fn_value_ref(name, module_path, sig, top)?,
        (AtomKind::Leaf, Expr::LowModuleFnValueRef { mangled, .. })
        | (AtomKind::NamedCallable, Expr::LowModuleCall { mangled, .. }) => {
            emit_module_fn_value_ref(mangled, top, &mut out)
        }
        (AtomKind::NamedCallable, Expr::LowQualifiedModuleCall { alias, mangled, .. }) => {
            let member = mangled
                .strip_prefix(&format!("{alias}."))
                .unwrap_or(mangled);
            let namespace = top
                .qualified_imports
                .get(alias.as_str())
                .cloned()
                .unwrap_or_else(|| {
                    assert!(
                        top.package
                            .is_some_and(|package| package.module(alias).is_some()),
                        "a qualified callable has its resolved module"
                    );
                    module_namespace_from_key(alias)
                });
            out = format!("{namespace}.{}", mangle_js_ident(member));
        }
        (AtomKind::Closure(entry), Expr::FnExpr { .. }) => {
            let captures = locals.plan.entry(entry).captures().to_vec();
            let names = captures
                .iter()
                .map(|binding| locals.plan.binding(*binding).name.to_owned())
                .collect::<Vec<_>>();
            let previous = std::mem::replace(&mut locals.values, names.clone().into());
            let rendered =
                lower_expr_in_context(TailContext::operand(source), top, locals, &mut out);
            locals.values = previous;
            rendered?;
            if !names.is_empty() {
                out = format!(
                    "(({}) => {out})({})",
                    names
                        .iter()
                        .map(|name| mangle_js_ident(name))
                        .collect::<Vec<_>>()
                        .join(", "),
                    captures
                        .into_iter()
                        .map(|binding| slot_value(Slot::Binding(binding)))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        (AtomKind::Tuple, Expr::EnrichedTuple { items, .. }) => {
            lower_enriched_product_from_rendered(
                &(0..items.len())
                    .map(|index| operand(operands, ChildRole::Item(index)))
                    .collect::<Vec<_>>(),
                &mut out,
            )
        }
        (AtomKind::Project, Expr::EnrichedProject { index, arity, .. }) => {
            out = format!(
                "__kioProductSlot({}, {index}, {arity})",
                operand(operands, ChildRole::Target)
            )
        }
        (
            AtomKind::Field,
            Expr::EnrichedFieldGet {
                index,
                arity,
                target_ty,
                ..
            },
        ) => {
            let selected = format!(
                "__kioProductSlot({}, {index}, {arity})",
                operand(operands, ChildRole::Target)
            );
            let frame = field_frame(target_ty, *index, *arity, top, &locals.type_params);
            out = js_nominal_operation(top, &frame, &selected, false);
        }
        (
            AtomKind::Inject,
            Expr::EnrichedInject {
                variant, variants, ..
            },
        ) => {
            out = format!(
                "__kioSumInject({}, {variant}, {variants})",
                operand(operands, ChildRole::Payload)
            )
        }
        (
            AtomKind::Nominal,
            Expr::LowNewtypeCtor {
                newtype,
                member,
                type_args,
                ..
            },
        )
        | (
            AtomKind::Nominal,
            Expr::LowNewtypeProj {
                newtype,
                member,
                type_args,
                ..
            },
        ) => {
            let namespace = top
                .cross_module_newtypes
                .get(newtype.as_str())
                .map(|ns| format!("{ns}."))
                .unwrap_or_default();
            let role = if matches!(source, Expr::LowNewtypeProj { .. }) {
                ChildRole::Target
            } else {
                ChildRole::Payload
            };
            out = format!(
                "{}({})",
                advance_erased_type_stages(
                    format!("{namespace}{newtype}.{member}"),
                    type_args.len()
                ),
                operand(operands, role)
            );
        }
        (
            AtomKind::Nominal,
            Expr::LowQualifiedNewtypeMember {
                module_path,
                newtype,
                member,
                type_args,
                ..
            },
        ) => {
            out = format!(
                "{}({})",
                advance_erased_type_stages(
                    format!(
                        "{}.{newtype}.{member}",
                        module_namespace_from_key(module_path)
                    ),
                    type_args.len()
                ),
                operand(operands, ChildRole::Payload)
            )
        }
        (
            AtomKind::HostCall,
            Expr::LowHostCall {
                name,
                module_path,
                sig,
                type_args,
                args,
                ..
            },
        ) => {
            let callee = advance_erased_type_stages(
                emit_host_fn_value_ref(name, module_path, sig, top)?,
                type_args.len(),
            );
            out = if sig.has_type_params() && !sig.has_value_group() {
                callee
            } else {
                let domain = canonical_js_domain(
                    &(0..args.len())
                        .map(|index| operand(operands, ChildRole::Argument(index)))
                        .collect::<Vec<_>>(),
                );
                format!("({callee})({domain})")
            };
        }
        (
            AtomKind::CpsPayload,
            Expr::LowCpsProjectorApply {
                newtype,
                module_path,
                continuation_ty,
                ..
            },
        ) => {
            let declaration = cps_projector_newtype(newtype, module_path, top)?;
            assert_eq!(
                cps_continuation_runtime_shape(continuation_ty).0,
                declaration.existential_params.len(),
                "a CPS continuation consumes its exact existential binders"
            );
            let name = QualifiedTypeName::new(
                module_path.split('/').map(str::to_owned).collect(),
                newtype,
            )
            .expect("a CPS projector has an exact owner");
            let frame = crate::backends::serialized_runtime_ir::exact_nominal_identity_frame(&name);
            out = js_nominal_operation(top, &frame, &operand(operands, ChildRole::Receiver), false);
        }
        (AtomKind::Absurd, Expr::LowAbsurdCall { .. }) => {
            out = format!(
                "((__bottom__) => {{ throw new Error('Kio: __absurd__ on bottom value'); }})({})",
                operand(operands, ChildRole::Absurd)
            )
        }
        _ => unreachable!("a checked child-free operation agrees with its routed representation"),
    }
    Ok(out)
}

pub(super) fn field_frame(
    target_ty: &Type,
    index: usize,
    arity: usize,
    top: &TopLevel,
    type_params: &[crate::ast::TypeParam],
) -> String {
    let nominal = if let Some(package) = top.package {
        let values = HashMap::new();
        let types = crate::backends::reconstruct::RoutedBodyTypes {
            package,
            module: top.module,
            type_params,
            values: &values,
        };
        let canonical = types.canonical(target_ty);
        let slot =
            crate::backends::reconstruct::nth_product_slot_ty_for_arity(&canonical, index, arity)
                .expect("a field projection fits its checked product arity");
        types
            .nominal(slot)
            .expect("a field projection selects a nominal slot")
    } else {
        let slot =
            crate::backends::reconstruct::nth_product_slot_ty_for_arity(target_ty, index, arity)
                .expect("a field projection fits its checked product arity");
        let Type::Path { segments, .. } = slot else {
            unreachable!("a field projection selects a nominal slot")
        };
        let (name, module) = segments.split_last().expect("a nominal path is nonempty");
        let module = if module.is_empty() {
            top.module_key.split('/').map(str::to_owned).collect()
        } else {
            module.iter().map(|part| part.name.clone()).collect()
        };
        QualifiedTypeName::new(module, &name.name).expect("a field has an exact nominal identity")
    };
    crate::backends::serialized_runtime_ir::exact_nominal_identity_frame(&nominal)
}

//! Compiled operations and explicit resume environments for Swift bodies.

use super::*;
use crate::backends::closure_plan::BindingDefinition;
use crate::backends::control_plan::{AtomKind, ChildRole, ControlEntry, ProducerKind, Slot};
use crate::backends::control_plan::{Completion, Exit, Invoke, Operation};
use std::collections::HashSet;

enum SwiftControlExit {
    Return(String),
    Jump(usize, String),
    Branch(String, usize, usize),
    Match(String, Vec<usize>),
    Invoke(String, Completion),
}

struct SwiftControlNames {
    helpers: Vec<String>,
    temporaries: Vec<String>,
    owner: bool,
}

impl BodyEmitter<'_, '_> {
    fn control_slot_name(&self, names: &SwiftControlNames, slot: Slot) -> String {
        match slot {
            Slot::Binding(binding) => self.binding_name(binding),
            Slot::Temporary(index) => names.temporaries[index].clone(),
        }
    }

    fn control_block_call(
        &self,
        graph: &ControlEntry<'_>,
        names: &SwiftControlNames,
        block: usize,
        input: Option<&str>,
        cursor: &str,
    ) -> String {
        assert_eq!(graph.blocks[block].input.is_some(), input.is_some());
        let mut arguments = graph.blocks[block]
            .live
            .iter()
            .map(|slot| self.control_slot_name(names, *slot))
            .collect::<Vec<_>>();
        arguments.extend(input.map(str::to_owned));
        arguments.extend(
            self.control_witnesses
                .borrow()
                .captures(graph.entry, block)
                .iter()
                .map(|binding| self.witness_names[binding].clone()),
        );
        arguments.push(cursor.to_owned());
        let owner = if names.owner { "self" } else { "Self" };
        format!("{owner}.{}({})", names.helpers[block], arguments.join(", "))
    }

    pub(super) fn emit_control(
        &mut self,
        entry: EntryId,
        cursor: &str,
    ) -> Result<String, EmitError> {
        let control = Rc::clone(&self.control);
        let graph = control.entry(entry);
        graph.validate(&self.executable);
        let names = SwiftControlNames {
            helpers: (0..graph.blocks.len())
                .map(|_| {
                    let suffix = self.fresh_name("block");
                    format!("{}{suffix}", self.helper_prefix)
                })
                .collect(),
            temporaries: (0..graph.temporaries.len())
                .map(|_| self.fresh_name("value"))
                .collect(),
            owner: self.executable.entry(entry).requires_owner,
        };
        let saved_locals = self.locals.clone();
        let saved_types = self.local_types.clone();
        let saved_context = self.control_context;
        let mut rendered = Vec::new();
        for (index, block) in graph.blocks.iter().enumerate() {
            self.control_context = Some((entry, index));
            let mut available = block
                .live
                .iter()
                .copied()
                .chain(block.input)
                .collect::<HashSet<_>>();
            self.locals = available
                .iter()
                .filter_map(|slot| match slot {
                    Slot::Binding(binding) => {
                        Some(self.executable.binding(*binding).name.to_owned())
                    }
                    Slot::Temporary(_) => None,
                })
                .collect();
            self.local_types.clear();
            for slot in &available {
                if let Slot::Binding(binding) = slot {
                    let ty = self.control_binding_type(*binding)?;
                    self.local_types
                        .insert(self.executable.binding(*binding).name.to_owned(), ty);
                }
            }
            let mut body = String::new();
            for operation in &block.operations {
                match operation {
                    Operation::Bind { binding, input, .. } => {
                        assert!(
                            available.contains(input),
                            "a Swift control binding reads a planned slot"
                        );
                        let ty = self.control_slot_type(graph, *input)?;
                        self.binding_types.insert(*binding, ty.clone());
                        let source_name = self.executable.binding(*binding).name.to_owned();
                        self.locals.push(source_name.clone());
                        self.local_types.insert(source_name, ty);
                        let name = self.binding_name(*binding);
                        let input = self.control_slot_name(&names, *input);
                        body.push_str(&format!("let {name}: Any = {input}; _ = {name}; "));
                        available.insert(Slot::Binding(*binding));
                    }
                    Operation::Atom {
                        output,
                        source,
                        kind,
                        operands,
                    } => {
                        if let AtomKind::Closure(child) = kind {
                            assert!(
                                self.executable
                                    .entry(*child)
                                    .captures()
                                    .iter()
                                    .all(|binding| available.contains(&Slot::Binding(*binding))),
                                "a Swift closure construction captures only planned live slots"
                            );
                        }
                        let operands = operands
                            .iter()
                            .map(|operand| {
                                assert!(
                                    available.contains(&operand.slot),
                                    "a Swift operation reads a planned live slot"
                                );
                                (operand.role, self.control_slot_name(&names, operand.slot))
                            })
                            .collect::<Vec<_>>();
                        let value = self.control_atom(source, *kind, &operands)?;
                        let name = self.control_slot_name(&names, *output);
                        body.push_str(&format!("let {name}: Any = {value}; _ = {name}; "));
                        available.insert(*output);
                    }
                }
            }
            let read = |slot: Slot| {
                assert!(
                    available.contains(&slot),
                    "a Swift control exit reads a planned live slot"
                );
                self.control_slot_name(&names, slot)
            };
            let exit = match &block.exit {
                Exit::Return(slot) => SwiftControlExit::Return(read(*slot)),
                Exit::Jump { block, argument } => SwiftControlExit::Jump(*block, read(*argument)),
                Exit::Branch {
                    condition,
                    then_block,
                    else_block,
                    ..
                } => {
                    let value = read(*condition);
                    let ty = self.control_slot_type(graph, *condition)?;
                    assert_eq!(self.atom_role_of(&ty), Some(Role::Bool));
                    let exact = self.exact_host_type_name(&ty)?;
                    SwiftControlExit::Branch(
                        format!("({value} as! {exact}) == (true as {exact})"),
                        *then_block,
                        *else_block,
                    )
                }
                Exit::Match {
                    scrutinee, arms, ..
                } => SwiftControlExit::Match(read(*scrutinee), arms.clone()),
                Exit::Invoke(invoke) => {
                    read(invoke.callee);
                    for slot in invoke.arguments.iter().chain(invoke.receiver.iter()) {
                        read(*slot);
                    }
                    if let Completion::Resume(resume) = invoke.completion {
                        assert!(
                            graph.resumes[resume]
                                .live
                                .iter()
                                .all(|slot| available.contains(slot)),
                            "a Swift resume captures exactly available planned slots"
                        );
                    }
                    SwiftControlExit::Invoke(
                        self.control_invocation(graph, &names, invoke)?,
                        invoke.completion,
                    )
                }
            };
            rendered.push((body, exit));
        }
        self.control_context = saved_context;
        self.locals = saved_locals;
        self.local_types = saved_types;
        self.control_witnesses.borrow_mut().propagate(graph);
        for (index, (mut body, exit)) in rendered.into_iter().enumerate() {
            let block = &graph.blocks[index];
            let mut parameters = block
                .live
                .iter()
                .copied()
                .chain(block.input)
                .map(|slot| format!("_ {}: Any", self.control_slot_name(&names, slot)))
                .collect::<Vec<_>>();
            parameters.extend(
                self.control_witnesses
                    .borrow()
                    .captures(entry, index)
                    .iter()
                    .map(|binding| {
                        format!("_ {}: any __KioTypeWitness", self.witness_names[binding])
                    }),
            );
            parameters.push("_ __kioCursor: __KioCursor".to_owned());
            let call =
                |block, input| self.control_block_call(graph, &names, block, input, "__kioCursor");
            match exit {
                SwiftControlExit::Return(value) => body.push_str(&format!("return {value}")),
                SwiftControlExit::Jump(block, argument) => {
                    body.push_str(&format!("return {}", call(block, Some(&argument))))
                }
                SwiftControlExit::Branch(condition, then_block, else_block) => {
                    body.push_str(&format!(
                        "if {condition} {{ return {} }} else {{ return {} }}",
                        call(then_block, None),
                        call(else_block, None)
                    ))
                }
                SwiftControlExit::Match(scrutinee, arms) => {
                    body.push_str(&format!("let (__kioVariant, __kioPayload) = kioSumPayload({scrutinee}, {}); switch __kioVariant {{ ", arms.len()));
                    for (variant, block) in arms.into_iter().enumerate() {
                        body.push_str(&format!(
                            "case {variant}: return {}; ",
                            call(block, Some("__kioPayload"))
                        ));
                    }
                    body.push_str(
                        "default: fatalError(\"kio erased body: invalid sum variant\") }",
                    );
                }
                SwiftControlExit::Invoke(invocation, completion) => {
                    if let Completion::Resume(resume) = completion {
                        let resume = &graph.resumes[resume];
                        let mut captures = resume
                            .live
                            .iter()
                            .map(|slot| self.control_slot_name(&names, *slot))
                            .collect::<Vec<_>>();
                        captures.extend(
                            self.control_witnesses
                                .borrow()
                                .captures(entry, resume.block)
                                .iter()
                                .map(|binding| self.witness_names[binding].clone()),
                        );
                        if names.owner {
                            captures.push("self".to_owned());
                        }
                        let capture = if captures.is_empty() {
                            String::new()
                        } else {
                            format!("[{}] ", captures.join(", "))
                        };
                        let call = self.control_block_call(
                            graph,
                            &names,
                            resume.block,
                            Some("__kioResult"),
                            "__kioResumeCursor",
                        );
                        body.push_str(&format!("__kioCursor.pushResume {{ {capture}__kioResult, __kioResumeCursor in {call} }}; "));
                    }
                    body.push_str(&format!("return {invocation}"));
                }
            }
            let static_ = if names.owner { "" } else { "static " };
            self.helpers.push_str(&format!(
                "\tprivate {static_}func {}({}) -> Any {{ {body} }}\n",
                names.helpers[index],
                parameters.join(", ")
            ));
        }
        Ok(self.control_block_call(graph, &names, graph.start, None, cursor))
    }

    fn control_invocation(
        &mut self,
        graph: &ControlEntry<'_>,
        names: &SwiftControlNames,
        invoke: &Invoke<'_>,
    ) -> Result<String, EmitError> {
        let callee = self.control_slot_name(names, invoke.callee);
        let receiver = invoke
            .receiver
            .map(|slot| self.control_slot_name(names, slot));
        if invoke.stage == TailStage::Type {
            let index = invoke
                .type_index
                .expect("a type edge selects a checked binder");
            let witness = match receiver {
                Some(receiver) => format!("{}.witnesses[{index}]", Self::control_packet(&receiver)),
                None => self.witness(&Self::control_type_arguments(invoke.source)[index])?,
            };
            return Ok(format!(
                "__kioAdvanceTail({callee}, {witness}, __kioCursor)"
            ));
        }
        let values = invoke
            .arguments
            .iter()
            .map(|slot| self.control_slot_name(names, *slot))
            .collect::<Vec<_>>();
        let (witness, argument) = if let Some(receiver) = receiver {
            let Expr::LowCpsProjectorApply {
                continuation_ty, ..
            } = invoke.source
            else {
                unreachable!("an existential invocation retains its continuation type")
            };
            let plan = self.value_plan(continuation_ty)?;
            self.record_witness(&plan, plan.root());
            let spine = callable_spine(&plan, plan.root());
            let mut scope = plan
                .ambient_binders()
                .iter()
                .copied()
                .zip(self.type_binders.iter().map(|(_, name)| name.clone()))
                .collect::<super::super::value::Scope>();
            for (index, stage) in spine.type_stages.iter().enumerate() {
                scope.insert(
                    stage.binder.expect("a continuation stage binds a witness"),
                    format!("{}.witnesses[{index}]", Self::control_packet(&receiver)),
                );
            }
            let types = super::super::value::Types {
                plan: &plan,
                shapes: self.facade_shapes,
                retained_nominals: None,
                origin: BoundaryFacadeSupportOrigin::Live,
            };
            let argument = values
                .first()
                .cloned()
                .unwrap_or_else(|| "KioUnit()".to_owned());
            (types.witness(spine.function, &scope, "H")?, argument)
        } else {
            let ty = self.control_slot_type(graph, invoke.callee)?;
            let Type::Function { param, .. } = &ty else {
                return Err(EmitError::internal(
                    "a Swift value edge has its exact function witness",
                ));
            };
            (
                self.witness(&ty)?,
                self.product_from_values(param, &values)?,
            )
        };
        Ok(format!(
            "__kioChecked({witness}, as: (any __KioFunctionWitness).self).invokeTail({callee}, {argument}, __kioCursor)"
        ))
    }

    fn control_binding_type(&mut self, binding: BindingId) -> Result<Type<Routed>, EmitError> {
        if let Some(ty) = self.binding_types.get(&binding) {
            return Ok(ty.clone());
        }
        let ty = match self.executable.binding_definition(binding) {
            Some(BindingDefinition::Local { value, .. }) => self.control_source_type(value)?,
            Some(BindingDefinition::Arm { source, index }) => {
                let Expr::EnrichedMatch {
                    scrutinee_ty, arms, ..
                } = source
                else {
                    unreachable!("a match binder retains its checked sum")
                };
                Type::right_spine_sum_slot_for_arity(scrutinee_ty, index, arms.len())
                    .expect("a match binder has its selected payload type")
                    .clone()
            }
            None => self
                .executable
                .binding(binding)
                .ty
                .cloned()
                .ok_or_else(|| {
                    EmitError::internal("a Swift parameter retains its selected stage type")
                })?,
        };
        self.binding_types.insert(binding, ty.clone());
        Ok(ty)
    }

    fn control_source_type(&mut self, source: &Expr<Routed>) -> Result<Type<Routed>, EmitError> {
        let mut lexical = HashMap::new();
        for binding in self.executable.scope_bindings(source) {
            let ty = self.control_binding_type(binding)?;
            lexical.insert(self.executable.binding(binding).name.to_owned(), ty);
        }
        let saved = std::mem::replace(&mut self.local_types, lexical);
        let ty = self.expr_type(source);
        self.local_types = saved;
        ty.ok_or_else(|| EmitError::internal("a Swift control producer retains its checked type"))
    }

    fn control_call_scheme(&mut self, source: &Expr<Routed>) -> Result<Type<Routed>, EmitError> {
        match source {
            Expr::LowModuleCall { .. } | Expr::LowQualifiedModuleCall { .. } => {
                let (module, name) = self.control_module_target(source)?;
                crate::backends::reconstruct::module_function_scheme(
                    self.shapes.package(),
                    &module,
                    &name,
                    source.span(),
                )
                .ok_or_else(|| {
                    EmitError::internal("a Swift named callable retains its resolved scheme")
                })
            }
            Expr::LowClosureCall { .. } => {
                self.control_binding_type(self.executable.reference_binding(source))
            }
            Expr::LowIndirectCall { callee, .. } | Expr::LowTypeApplication { callee, .. } => {
                self.control_source_type(callee)
            }
            Expr::LowCpsProjectorApply {
                continuation_ty, ..
            } => Ok(continuation_ty.clone()),
            _ => unreachable!("a control invocation retains its checked callable source"),
        }
    }

    fn control_type_arguments(source: &Expr<Routed>) -> &[Type<Routed>] {
        match source {
            Expr::LowModuleCall { type_args, .. }
            | Expr::LowQualifiedModuleCall { type_args, .. }
            | Expr::LowClosureCall { type_args, .. }
            | Expr::LowIndirectCall { type_args, .. } => type_args,
            Expr::LowTypeApplication { type_arg, .. } => std::slice::from_ref(type_arg),
            _ => unreachable!("an ordinary type edge retains its selected type argument"),
        }
    }

    fn control_slot_type(
        &mut self,
        graph: &ControlEntry<'_>,
        slot: Slot,
    ) -> Result<Type<Routed>, EmitError> {
        let Slot::Temporary(index) = slot else {
            let Slot::Binding(binding) = slot else {
                unreachable!()
            };
            return self.control_binding_type(binding);
        };
        let producer = &graph.temporaries[index];
        match producer.kind {
            ProducerKind::Atom(AtomKind::NamedCallable) => {
                self.control_call_scheme(producer.source)
            }
            ProducerKind::TypeStage(index) => {
                assert!(
                    !matches!(producer.source, Expr::LowCpsProjectorApply { .. }),
                    "CPS stage values use their exact packet-selected continuation witness"
                );
                let scheme = self.control_call_scheme(producer.source)?;
                let arguments = Self::control_type_arguments(producer.source);
                crate::backends::reconstruct::instantiate_type_stages(&scheme, &arguments[..=index])
                    .ok_or_else(|| {
                        EmitError::internal("a Swift type-stage result follows its selected binder")
                    })
            }
            ProducerKind::Atom(AtomKind::CpsPayload) => unreachable!(
                "an existential payload is consumed only by its packet-selected continuation witness"
            ),
            ProducerKind::Atom(_) | ProducerKind::ValueStage | ProducerKind::Join => {
                self.control_source_type(producer.source)
            }
        }
    }

    fn control_atom(
        &mut self,
        source: &Expr<Routed>,
        kind: AtomKind,
        operands: &[(ChildRole, String)],
    ) -> Result<String, EmitError> {
        let operand = |role| {
            operands
                .iter()
                .find(|(selected, _)| *selected == role)
                .expect("a shared representation operation supplies its checked operand")
                .1
                .clone()
        };
        match kind {
            AtomKind::Leaf => {
                assert!(matches!(
                    source,
                    Expr::Unit { .. }
                        | Expr::StrLit { .. }
                        | Expr::IntLit { .. }
                        | Expr::FloatLit { .. }
                        | Expr::BoolLit { .. }
                        | Expr::LowHostFnValueRef { .. }
                        | Expr::LowModuleFnValueRef { .. }
                ));
                self.emit_context(TailContext::operand(source), None)
            }
            AtomKind::Closure(entry) => {
                let Expr::FnExpr { body, .. } = source else {
                    unreachable!("a closure operation retains its function expression")
                };
                assert_eq!(self.executable.entry_for_body(body).id, entry);
                let ty = self.control_source_type(source)?;
                let captures = self.executable.entry(entry).captures().to_vec();
                let mut local_types = HashMap::new();
                let mut locals = Vec::new();
                for binding in captures {
                    let ty = self.control_binding_type(binding)?;
                    let name = self.executable.binding(binding).name.to_owned();
                    local_types.insert(name.clone(), ty);
                    locals.push(name);
                }
                let saved_types = std::mem::replace(&mut self.local_types, local_types);
                let saved_locals = std::mem::replace(&mut self.locals, locals);
                let value = self.callable_value(&ty, body);
                self.local_types = saved_types;
                self.locals = saved_locals;
                value
            }
            AtomKind::NamedCallable => {
                let (module, name) = self.control_module_target(source)?;
                Ok(format!("self.{}()", module_fn_method_name(&module, &name)))
            }
            AtomKind::Tuple => {
                let Expr::EnrichedTuple {
                    items, synth_ty, ..
                } = source
                else {
                    unreachable!("a product operation retains its checked product")
                };
                let values = (0..items.len())
                    .map(|index| operand(ChildRole::Item(index)))
                    .collect::<Vec<_>>();
                self.product_from_values(synth_ty, &values)
            }
            AtomKind::Project | AtomKind::Field => {
                let (index, arity) = match source {
                    Expr::EnrichedProject { index, arity, .. }
                    | Expr::EnrichedFieldGet { index, arity, .. } => (*index, *arity),
                    _ => unreachable!("a projection operation retains its checked slot"),
                };
                let target = operand(ChildRole::Target);
                let value = format!("kioProductSlot({target}, {index}, {arity})");
                Ok(if kind == AtomKind::Field {
                    format!("__kioChecked({value}, as: (any __KioNominalValue).self).__kioValue")
                } else {
                    value
                })
            }
            AtomKind::Inject => {
                let Expr::EnrichedInject {
                    variant,
                    variants,
                    synth_ty,
                    ..
                } = source
                else {
                    unreachable!("an injection operation retains its checked sum")
                };
                let payload = operand(ChildRole::Payload);
                let witnesses = self.structural_witnesses(synth_ty, *variants, true)?;
                Ok(format!(
                    "__kioInject({payload}, {variant}, [{}])",
                    witnesses.join(", ")
                ))
            }
            AtomKind::Nominal => {
                let role = if matches!(source, Expr::LowNewtypeProj { .. }) {
                    ChildRole::Target
                } else {
                    ChildRole::Payload
                };
                self.emit_newtype_member_value(source, operand(role))
            }
            AtomKind::HostCall => {
                let Expr::LowHostCall {
                    name,
                    module_path,
                    type_args,
                    args,
                    ..
                } = source
                else {
                    unreachable!("a host operation retains its resolved host site")
                };
                let site_id = host_function_site_id(module_path, name);
                let site = prepared_site(self.prepared, &site_id)?;
                let render = PreparedSwiftRender {
                    site,
                    plan: site.plan().facade(),
                    shapes: self.facade_shapes,
                };
                let witnesses = type_args
                    .iter()
                    .map(|ty| self.witness(ty))
                    .collect::<Result<Vec<_>, _>>()?;
                let arguments = (0..args.len())
                    .map(|index| operand(ChildRole::Argument(index)))
                    .collect::<Vec<_>>();
                self.opened_host_call(render, name, module_path, &witnesses, &arguments)
            }
            AtomKind::Absurd => {
                let value = operand(ChildRole::Absurd);
                Ok(format!(
                    "{{ () -> Any in _ = {value}; fatalError(\"kio: __absurd__ reached\") }}()"
                ))
            }
            AtomKind::CpsPayload => {
                let receiver = operand(ChildRole::Receiver);
                Ok(format!("{}.payload", Self::control_packet(&receiver)))
            }
        }
    }

    fn control_packet(receiver: &str) -> String {
        format!(
            "__kioChecked(__kioChecked({receiver}, as: (any __KioNominalValue).self).__kioValue, as: __KioExistential.self)"
        )
    }

    fn control_module_target(&self, source: &Expr<Routed>) -> Result<(String, String), EmitError> {
        match source {
            Expr::LowModuleCall { mangled, .. } => Ok((
                self.selective_imports
                    .get(mangled)
                    .map_or(self.module_key, String::as_str)
                    .to_owned(),
                mangled.clone(),
            )),
            Expr::LowQualifiedModuleCall { mangled, .. } => {
                let (alias, name) = mangled.split_once('.').ok_or_else(|| {
                    EmitError::internal("a qualified Swift call has an import prefix")
                })?;
                let module = self.qualified_imports.get(alias).ok_or_else(|| {
                    EmitError::internal("a qualified Swift call has a resolved owner")
                })?;
                Ok((module.clone(), name.to_owned()))
            }
            _ => unreachable!("a named callable operation names a resolved module function"),
        }
    }
}

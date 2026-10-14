//! Native blocks and one-shot resume frames for the shared pending-work plan.

use super::*;
use crate::backends::closure_plan::{BindingDefinition, BindingId, EntryId};
use crate::backends::control_plan::{
    AtomKind, BlockId, ChildRole, Completion, ControlEntry, Exit, Invoke, Operand, Operation,
    ProducerKind, Slot,
};
use crate::backends::tail_plan::TailStage;

fn slot_name(slot: Slot) -> String {
    match slot {
        Slot::Binding(id) => format!("__kio_binding{}", id.index()),
        Slot::Temporary(id) => format!("__kio_temporary{id}"),
    }
}

fn slot_value(slot: Slot) -> String {
    format!("{}.clone()", slot_name(slot))
}

fn named_source<'a>(graph: &ControlEntry<'a>, slot: Slot) -> Option<&'a Expr<Routed>> {
    let Slot::Temporary(index) = slot else {
        return None;
    };
    let producer = &graph.temporaries[index];
    (producer.kind == ProducerKind::Atom(AtomKind::NamedCallable)).then_some(producer.source)
}

impl BodyEmitter<'_> {
    pub(super) fn emit_entry_body(
        &mut self,
        entry: &ExecutableEntry<'_>,
    ) -> Result<String, EmitError> {
        let graph = self.control.entry(entry.id);
        graph.validate(self.plan);
        if graph.has_pending_work() {
            Ok(self.emit_control_entry(entry))
        } else {
            self.emit_result(entry.body())
        }
    }

    fn control_method(&self, entry: EntryId, block: BlockId) -> String {
        // The length makes the root method identity unambiguous even when it
        // contains a suffix resembling an entry/block index.
        format!(
            "__kio_control_{}_{}_e{}_b{block}",
            self.control_name.len(),
            self.control_name,
            entry.index()
        )
    }

    fn control_resume_method(&self, graph: &ControlEntry<'_>, resume: usize) -> String {
        format!(
            "{}_resume{resume}",
            self.control_method(graph.entry, graph.resumes[resume].block)
        )
    }

    fn stored_live(&self, graph: &ControlEntry<'_>, live: &[Slot]) -> Vec<Slot> {
        // A resolved named callee is represented by the block's retained owner
        // and its static method entry. It has no additional value payload.
        live.iter()
            .copied()
            .filter(|slot| named_source(graph, *slot).is_none())
            .collect()
    }

    pub(super) fn emit_control_entry(&self, entry: &ExecutableEntry<'_>) -> String {
        let graph = self.control.entry(entry.id);
        let available = entry_value_scope(self.plan, entry);
        let arguments = graph.blocks[graph.start]
            .live
            .iter()
            .map(|slot| {
                let Slot::Binding(binding) = slot else {
                    unreachable!("an executable entry starts with lexical inputs only")
                };
                let name = self.plan.binding(*binding).name;
                assert!(
                    available.contains(name),
                    "a checked Rust local reference must belong to the planned entry scope: {name}"
                );
                format!("{}.clone()", escape_rust_keyword(name))
            })
            .collect::<Vec<_>>();
        format!(
            "{}.{}({})",
            self.self_ref_str(),
            self.control_method(entry.id, graph.start),
            arguments.join(", ")
        )
    }

    fn control_jump(
        &self,
        graph: &ControlEntry<'_>,
        block: BlockId,
        argument: Option<&str>,
    ) -> String {
        let target = &graph.blocks[block];
        assert_eq!(
            target.input.is_some(),
            argument.is_some(),
            "a compiled block receives its planned result input"
        );
        let arguments = self
            .stored_live(graph, &target.live)
            .into_iter()
            .map(slot_value)
            .chain(argument.map(str::to_owned))
            .collect::<Vec<_>>();
        format!(
            "self.{}({})",
            self.control_method(graph.entry, block),
            arguments.join(", ")
        )
    }

    pub(super) fn emit_control_methods(&mut self, out: &mut String) -> Result<(), EmitError> {
        let plan = self.plan;
        let control = self.control;
        let mut types = std::collections::HashMap::new();
        for entry in plan.entries() {
            let graph = control.entry(entry.id);
            graph.validate(plan);
            if !graph.has_pending_work() {
                continue;
            }
            self.tparams = entry
                .type_scope
                .iter()
                .map(|id| plan.type_binding(*id).param.name.clone())
                .collect();
            for (index, block) in graph.blocks.iter().enumerate() {
                self.self_ref = None;
                let inputs = self
                    .stored_live(graph, &block.live)
                    .into_iter()
                    .chain(block.input)
                    .collect::<Vec<_>>();
                self.active_values = inputs
                    .iter()
                    .filter_map(|slot| match slot {
                        Slot::Binding(binding) => Some(plan.binding(*binding).name.to_owned()),
                        Slot::Temporary(_) => None,
                    })
                    .collect();
                let parameters = inputs
                    .iter()
                    .map(|slot| {
                        format!(", {}: ::std::rc::Rc<dyn ::std::any::Any>", slot_name(*slot))
                    })
                    .collect::<String>();
                out.push_str(&format!("    fn {}(self: &::std::rc::Rc<Self>{parameters}) -> {}::__kio_runtime::Step {{\n", self.control_method(entry.id, index), self.crate_root()));
                for operation in &block.operations {
                    match operation {
                        Operation::Bind { binding, input, .. } => {
                            out.push_str(&format!(
                                "        let {} = {};\n",
                                slot_name(Slot::Binding(*binding)),
                                slot_value(*input)
                            ));
                            self.active_values
                                .insert(plan.binding(*binding).name.to_owned());
                        }
                        Operation::Atom {
                            kind: AtomKind::NamedCallable,
                            ..
                        } => {}
                        Operation::Atom {
                            output,
                            source,
                            kind,
                            operands,
                        } => {
                            let value =
                                self.emit_control_atom(source, *kind, operands, &mut types)?;
                            out.push_str(&format!(
                                "        let {}: ::std::rc::Rc<dyn ::std::any::Any> = {value};\n",
                                slot_name(*output)
                            ));
                        }
                    }
                }
                let exit = self.emit_control_exit(graph, &block.exit, &mut types)?;
                out.push_str(&format!("        {exit}\n    }}\n"));
            }
            for (id, resume) in graph.resumes.iter().enumerate() {
                let live = self.stored_live(graph, &resume.live);
                let name = self.control_resume_method(graph, id);
                out.push_str(&format!(
                    "    fn {name}(__kio_owner: ::std::rc::Rc<dyn ::std::any::Any>, __kio_values: Vec<::std::rc::Rc<dyn ::std::any::Any>>, __kio_result: ::std::rc::Rc<dyn ::std::any::Any>) -> {}::__kio_runtime::Step {{\n",
                    self.crate_root()
                ));
                out.push_str("        let __kio_owner = __kio_owner.downcast::<Self>().expect(\"resume retains its package owner\");\n        let mut __kio_values = __kio_values.into_iter();\n");
                for slot in &live {
                    out.push_str(&format!(
                        "        let {} = __kio_values.next().expect(\"resume retains its planned live slots\");\n",
                        slot_name(*slot)
                    ));
                }
                out.push_str("        assert!(__kio_values.next().is_none(), \"resume consumes its exact live slots\");\n        drop(__kio_values);\n");
                let arguments = live
                    .into_iter()
                    .map(slot_name)
                    .chain(std::iter::once("__kio_result".to_owned()))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "        __kio_owner.{}({arguments})\n    }}\n",
                    self.control_method(graph.entry, resume.block)
                ));
            }
        }
        Ok(())
    }

    fn control_scope_types(
        &mut self,
        source: &Expr<Routed>,
        cache: &mut std::collections::HashMap<BindingId, Type<Routed>>,
    ) -> std::collections::HashMap<String, Type<Routed>> {
        let plan = self.plan;
        let mut scope = std::collections::HashMap::new();
        for binding in plan.scope_bindings(source) {
            let ty = if let Some(ty) = cache.get(&binding) {
                Some(ty.clone())
            } else if let Some(ty) = plan.binding(binding).ty {
                Some(ty.clone())
            } else {
                match plan.binding_definition(binding) {
                    Some(BindingDefinition::Local { value, .. }) => {
                        let inputs = self.control_scope_types(value, cache);
                        let previous = std::mem::replace(&mut self.local_types, inputs);
                        let ty = self.expr_type(value);
                        self.local_types = previous;
                        ty
                    }
                    Some(BindingDefinition::Arm { source, index }) => {
                        let Expr::EnrichedMatch {
                            scrutinee_ty, arms, ..
                        } = source
                        else {
                            unreachable!("a checked arm retains its sum type")
                        };
                        Type::right_spine_sum_slot_for_arity(scrutinee_ty, index, arms.len())
                            .cloned()
                    }
                    None => None,
                }
            };
            if let Some(ty) = ty {
                cache.insert(binding, ty.clone());
                scope.insert(plan.binding(binding).name.to_owned(), ty);
            }
        }
        scope
    }

    fn emit_control_atom(
        &mut self,
        source: &Expr<Routed>,
        kind: AtomKind,
        operands: &[Operand],
        types: &mut std::collections::HashMap<BindingId, Type<Routed>>,
    ) -> Result<String, EmitError> {
        let operand = |role| {
            let selected = operands
                .iter()
                .filter(|operand| operand.role == role)
                .collect::<Vec<_>>();
            let [selected] = selected.as_slice() else {
                unreachable!("a compiled operation selects exactly its planned operand")
            };
            slot_value(selected.slot)
        };
        match (kind, source) {
            (AtomKind::Leaf, Expr::Unit { .. }) => Ok(self.box_any("()")),
            (
                AtomKind::Leaf,
                Expr::StrLit {
                    value, annotation, ..
                },
            ) => {
                assert_eq!(self.literal_host_role(annotation)?, Role::Str);
                let value = self.exact_literal_value(annotation, &rust_string_lit(value))?;
                Ok(self.box_any(&value))
            }
            (
                AtomKind::Leaf,
                Expr::IntLit {
                    digits, annotation, ..
                }
                | Expr::FloatLit {
                    digits, annotation, ..
                },
            ) => {
                let role = self.literal_host_role(annotation)?;
                let value = self.exact_literal_value(
                    annotation,
                    &format!("{digits}_{}", role_literal_primitive_type(role)),
                )?;
                Ok(self.box_any(&value))
            }
            (
                AtomKind::Leaf,
                Expr::BoolLit {
                    value, annotation, ..
                },
            ) => {
                assert_eq!(self.literal_host_role(annotation)?, Role::Bool);
                let value = self.exact_literal_value(annotation, &value.to_string())?;
                Ok(self.box_any(&value))
            }
            (
                AtomKind::Leaf,
                Expr::LowHostFnValueRef {
                    name,
                    module_path,
                    sig,
                    ret_ty,
                    ..
                },
            ) => self.emit_low_host_fn_value_ref(name, module_path, sig, ret_ty),
            (AtomKind::Leaf, Expr::LowModuleFnValueRef { mangled, sig, .. }) => {
                self.emit_low_module_fn_value_ref(mangled, sig)
            }
            (AtomKind::Closure(id), Expr::FnExpr { sig, body, .. }) => {
                let entry = self.plan.entry_for_body(body);
                assert_eq!(
                    id, entry.id,
                    "a compiled closure retains its exact lexical entry"
                );
                let aliases = entry
                    .captures()
                    .iter()
                    .map(|binding| {
                        let name = self.plan.binding(*binding).name;
                        self.require_local(name);
                        format!(
                            "let {} = {}; ",
                            escape_rust_keyword(name),
                            slot_value(Slot::Binding(*binding))
                        )
                    })
                    .collect::<String>();
                let scope = self.control_scope_types(source, types);
                let previous = std::mem::replace(&mut self.local_types, scope);
                let value = self.emit_fn_expr(sig, body);
                self.local_types = previous;
                let value = value?;
                Ok(format!("{{ {aliases}{value} }}"))
            }
            (AtomKind::Tuple, Expr::EnrichedTuple { items, .. }) => {
                let values = (0..items.len())
                    .map(|index| operand(ChildRole::Item(index)))
                    .collect::<Vec<_>>();
                Ok(pack_erased_domain(self.crate_root(), &values))
            }
            (AtomKind::Project, Expr::EnrichedProject { index, arity, .. })
            | (AtomKind::Field, Expr::EnrichedFieldGet { index, arity, .. }) => {
                Ok(product_slot_access(
                    self.crate_root(),
                    &operand(ChildRole::Target),
                    *index,
                    *arity,
                ))
            }
            (
                AtomKind::Inject,
                Expr::EnrichedInject {
                    variant, variants, ..
                },
            ) => Ok(format!(
                "{}::__kio_runtime::sum_inject({}, {variant}, {variants})",
                self.crate_root(),
                operand(ChildRole::Payload)
            )),
            (
                AtomKind::Nominal,
                Expr::LowNewtypeCtor { .. } | Expr::LowQualifiedNewtypeMember { .. },
            ) => Ok(operand(ChildRole::Payload)),
            (AtomKind::Nominal, Expr::LowNewtypeProj { .. }) => Ok(operand(ChildRole::Target)),
            (AtomKind::CpsPayload, Expr::LowCpsProjectorApply { .. }) => {
                Ok(operand(ChildRole::Receiver))
            }
            (
                AtomKind::HostCall,
                Expr::LowHostCall {
                    name,
                    module_path,
                    type_args,
                    args,
                    sig,
                    ret_ty,
                    ..
                },
            ) => {
                let values = (0..args.len())
                    .map(|index| operand(ChildRole::Argument(index)))
                    .collect::<Vec<_>>();
                self.emit_host_call_values(name, module_path, type_args, &values, sig, ret_ty)
            }
            (AtomKind::Absurd, Expr::LowAbsurdCall { .. }) => Ok(format!(
                "{{ let _ = {}; unreachable!() }}",
                operand(ChildRole::Absurd)
            )),
            _ => unreachable!(
                "a compiled operation has the shared child-free classification of its checked source"
            ),
        }
    }

    fn emit_control_exit(
        &mut self,
        graph: &ControlEntry<'_>,
        exit: &Exit<'_>,
        types: &mut std::collections::HashMap<BindingId, Type<Routed>>,
    ) -> Result<String, EmitError> {
        Ok(match exit {
            Exit::Return(value) => done_value(self.crate_root(), &slot_value(*value)),
            Exit::Jump { block, argument } => {
                self.control_jump(graph, *block, Some(&slot_value(*argument)))
            }
            Exit::Branch {
                source,
                condition,
                then_block,
                else_block,
            } => {
                let Expr::EnrichedConditional { cond, .. } = source else {
                    unreachable!("a branch retains its checked condition")
                };
                self.local_types = self.control_scope_types(cond, types);
                let condition = self.emit_bool_value(cond, &slot_value(*condition))?;
                format!(
                    "if {condition} {{ {} }} else {{ {} }}",
                    self.control_jump(graph, *then_block, None),
                    self.control_jump(graph, *else_block, None)
                )
            }
            Exit::Match {
                source,
                scrutinee,
                arms,
            } => {
                let Expr::EnrichedMatch { arms: checked, .. } = source else {
                    unreachable!("a match retains its checked alternatives")
                };
                assert_eq!(checked.len(), arms.len());
                let branches = arms
                    .iter()
                    .enumerate()
                    .map(|(index, block)| {
                        format!(
                            "{index} => {}",
                            self.control_jump(graph, *block, Some("__kio_payload.clone()"))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{{ let (__kio_variant, __kio_payload) = {}::__kio_runtime::sum_payload({}, {}); match __kio_variant {{ {branches}, _ => unreachable!(\"kio erased body: invalid sum variant\") }} }}",
                    self.crate_root(),
                    slot_value(*scrutinee),
                    arms.len()
                )
            }
            Exit::Invoke(invoke) => self.emit_control_invoke(graph, invoke)?,
        })
    }

    fn emit_control_invoke(
        &mut self,
        graph: &ControlEntry<'_>,
        invoke: &Invoke<'_>,
    ) -> Result<String, EmitError> {
        let root = self.crate_root();
        let arguments = invoke
            .arguments
            .iter()
            .copied()
            .map(slot_value)
            .collect::<Vec<_>>();
        let call = if let Some(source) = named_source(graph, invoke.callee) {
            let (method, sig) = self.control_named_method(source)?;
            let stage = signature_stages(sig)[0];
            assert_eq!(
                stage.is_value(),
                invoke.stage == TailStage::Value,
                "a direct named edge selects its first checked stage"
            );
            assert_eq!(
                stage.param_names().len(),
                arguments.len(),
                "a direct named edge supplies its complete private stage ABI"
            );
            self.method_invocation(&method, &arguments)
        } else {
            let domain = match invoke.stage {
                TailStage::Type => "None".to_owned(),
                TailStage::Value => format!("Some({})", pack_erased_domain(root, &arguments)),
            };
            format!(
                "{root}::__kio_runtime::Invocation::Apply {{ callee: {}, domain: {domain} }}",
                slot_value(invoke.callee)
            )
        };
        Ok(match invoke.completion {
            Completion::Return => format!("{root}::__kio_runtime::Step::Invoke({call})"),
            Completion::Resume(id) => {
                let resume = &graph.resumes[id];
                let live = self.stored_live(graph, &resume.live);
                let values = live
                    .into_iter()
                    .map(slot_value)
                    .collect::<Vec<_>>()
                    .join(", ");
                let method = self.control_resume_method(graph, id);
                format!(
                    "{{ let __kio_call = {call}; {root}::__kio_runtime::Step::Pending {{ call: __kio_call, resume: {root}::__kio_runtime::Resume {{ owner: self.clone(), values: vec![{values}], invoke: Self::{method} }} }} }}"
                )
            }
        })
    }

    fn control_named_method<'a>(
        &self,
        source: &'a Expr<Routed>,
    ) -> Result<(String, &'a crate::ast::Signature<Routed>), EmitError> {
        match source {
            Expr::LowModuleCall { mangled, sig, .. } => Ok((
                self.selective_imports
                    .get(mangled)
                    .cloned()
                    .unwrap_or_else(|| mangled.to_owned()),
                sig,
            )),
            Expr::LowQualifiedModuleCall { mangled, sig, .. } => {
                let (alias, member) = mangled
                    .split_once('.')
                    .expect("a checked qualified call names its resolved member");
                let module = self
                    .qualified_imports
                    .get(alias)
                    .map(String::as_str)
                    .or_else(|| self.package.module(alias).map(|_| alias))
                    .ok_or_else(|| {
                        EmitError::unsupported(
                            "Rust emitter: a checked qualified callee lost its module",
                        )
                    })?;
                Ok((mangle_module_fn(module, member), sig))
            }
            _ => unreachable!("a named callable producer is a resolved module call"),
        }
    }
}

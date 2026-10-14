//! Pending work over checked routed bodies. Operations contain no executing
//! children: application stages and their result destinations are graph edges.

use std::collections::{BTreeSet, HashMap};

use crate::ast::{Expr, Routed};

use super::boundary_facade::FacadeBinderId;
use super::closure_plan::{
    BindingId, EntryId, ExecutableBodyPlan, TypeBindingId, WitnessCaptures, WitnessContext,
    cps_continuation_runtime_shape,
};
use super::tail_plan::{TailContext, TailStage};
use super::value_plan::{ValuePlan, ValueUseId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Slot {
    Binding(BindingId),
    Temporary(usize),
}

pub(crate) type BlockId = usize;
pub(crate) type ResumeId = usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChildRole {
    Argument(usize),
    Item(usize),
    Payload,
    Target,
    Receiver,
    Absurd,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Operand {
    pub(crate) role: ChildRole,
    pub(crate) slot: Slot,
}

/// This whitelist is the boundary between representation work and execution.
/// A named callable reference constructs its existing staged value; it does not
/// call its declaration. A CPS payload unwraps its already evaluated receiver.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum AtomKind {
    Leaf,
    Closure(EntryId),
    Tuple,
    Project,
    Inject,
    Field,
    Nominal,
    HostCall,
    Absurd,
    NamedCallable,
    CpsPayload,
}

pub(crate) enum Operation<'a> {
    Bind {
        binding: BindingId,
        input: Slot,
        source: &'a Expr<Routed>,
    },
    Atom {
        output: Slot,
        source: &'a Expr<Routed>,
        kind: AtomKind,
        operands: Vec<Operand>,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ProducerKind {
    Atom(AtomKind),
    TypeStage(usize),
    ValueStage,
    Join,
}

/// The checked source and selected elimination retain the result's type and
/// witness provenance. Consumers reuse their existing canonical type plans.
pub(crate) struct Producer<'a> {
    pub(crate) source: &'a Expr<Routed>,
    pub(crate) kind: ProducerKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Completion {
    Return,
    Resume(ResumeId),
}

pub(crate) struct Invoke<'a> {
    pub(crate) source: &'a Expr<Routed>,
    pub(crate) callee: Slot,
    pub(crate) stage: TailStage,
    /// Index in the source's explicit leading type arguments, or in its exact
    /// CPS continuation type. Standalone LowTypeApplication uses index zero.
    pub(crate) type_index: Option<usize>,
    /// CPS existential witnesses belong to this exact evaluated packet.
    pub(crate) receiver: Option<Slot>,
    pub(crate) arguments: Vec<Slot>,
    pub(crate) completion: Completion,
}

pub(crate) enum Exit<'a> {
    Return(Slot),
    Jump {
        block: BlockId,
        argument: Slot,
    },
    Branch {
        source: &'a Expr<Routed>,
        condition: Slot,
        then_block: BlockId,
        else_block: BlockId,
    },
    Match {
        source: &'a Expr<Routed>,
        scrutinee: Slot,
        arms: Vec<BlockId>,
    },
    Invoke(Invoke<'a>),
}

pub(crate) struct Block<'a> {
    source: &'a Expr<Routed>,
    pub(crate) input: Option<Slot>,
    pub(crate) operations: Vec<Operation<'a>>,
    pub(crate) exit: Exit<'a>,
    /// Values needed on entry, excluding the input supplied by the edge.
    pub(crate) live: Vec<Slot>,
}

pub(crate) struct Resume {
    pub(crate) block: BlockId,
    pub(crate) result: Slot,
    pub(crate) live: Vec<Slot>,
}

pub(crate) struct ControlEntry<'a> {
    pub(crate) entry: EntryId,
    pub(crate) start: BlockId,
    pub(crate) blocks: Vec<Block<'a>>,
    pub(crate) resumes: Vec<Resume>,
    pub(crate) temporaries: Vec<Producer<'a>>,
    results: HashMap<usize, Slot>,
}

pub(crate) struct ControlPlan<'a> {
    entries: HashMap<EntryId, ControlEntry<'a>>,
}

/// Native witness requests are collected at their actual block of execution.
/// Propagation follows the same graph as term liveness, without inspecting
/// source annotations or guessing dependencies from generated text.
#[derive(Default)]
pub(crate) struct ControlWitnessCaptures {
    blocks: HashMap<(EntryId, BlockId), Vec<TypeBindingId>>,
}

impl ControlWitnessCaptures {
    pub(crate) fn require(
        &mut self,
        bodies: &ExecutableBodyPlan<'_>,
        entry: EntryId,
        block: BlockId,
        bindings: &[TypeBindingId],
    ) {
        let captures = self.blocks.entry((entry, block)).or_default();
        for binding in bindings {
            assert!(
                bodies.entry(entry).type_scope.contains(binding),
                "a control witness belongs to its entry's exact lexical type scope"
            );
            if !captures.contains(binding) {
                captures.push(*binding);
            }
        }
    }
    pub(crate) fn record(
        &mut self,
        bodies: &ExecutableBodyPlan<'_>,
        entry: EntryId,
        block: BlockId,
        plan: &ValuePlan,
        use_id: ValueUseId,
        scope: &[(FacadeBinderId, TypeBindingId)],
    ) {
        let context = WitnessContext::Execute {
            entry,
            stage: bodies.entry(entry).stages.len() - 1,
        };
        let captures = self.blocks.entry((entry, block)).or_default();
        for binding in WitnessCaptures::dependencies(bodies, context, plan, use_id, scope) {
            if !captures.contains(&binding) {
                captures.push(binding);
            }
        }
    }

    pub(crate) fn propagate(&mut self, graph: &ControlEntry<'_>) {
        for block in graph.successor_order() {
            let mut captures = self
                .blocks
                .remove(&(graph.entry, block))
                .unwrap_or_default();
            for successor in graph.successors(block) {
                for binding in self.captures(graph.entry, successor) {
                    if !captures.contains(binding) {
                        captures.push(*binding);
                    }
                }
            }
            self.blocks.insert((graph.entry, block), captures);
        }
    }

    pub(crate) fn captures(&self, entry: EntryId, block: BlockId) -> &[TypeBindingId] {
        self.blocks.get(&(entry, block)).map_or(&[], Vec::as_slice)
    }
}

impl<'a> ControlPlan<'a> {
    pub(crate) fn new(bodies: &ExecutableBodyPlan<'a>) -> Self {
        Self {
            entries: bodies
                .entries()
                .map(|entry| {
                    let graph = Builder::build(bodies, entry.id);
                    (entry.id, graph)
                })
                .collect(),
        }
    }

    pub(crate) fn entry(&self, entry: EntryId) -> &ControlEntry<'a> {
        &self.entries[&entry]
    }

    #[cfg(test)]
    pub(crate) fn entry_mut(&mut self, entry: EntryId) -> &mut ControlEntry<'a> {
        self.entries
            .get_mut(&entry)
            .expect("a mutable test entry belongs to its plan")
    }
}

struct DraftBlock<'a> {
    source: &'a Expr<Routed>,
    input: Option<Slot>,
    operations: Vec<Operation<'a>>,
    exit: Option<Exit<'a>>,
}

struct Builder<'a, 'p> {
    bodies: &'p ExecutableBodyPlan<'a>,
    blocks: Vec<DraftBlock<'a>>,
    resumes: Vec<Resume>,
    temporaries: Vec<Producer<'a>>,
    results: HashMap<usize, Slot>,
}

impl<'a, 'p> Builder<'a, 'p> {
    fn build(bodies: &'p ExecutableBodyPlan<'a>, entry: EntryId) -> ControlEntry<'a> {
        let mut builder = Self {
            bodies,
            blocks: Vec::new(),
            resumes: Vec::new(),
            temporaries: Vec::new(),
            results: HashMap::new(),
        };
        let start = builder.block(bodies.entry(entry).body().expr(), None);
        assert!(
            builder
                .lower(bodies.entry(entry).body().expr(), start, true)
                .is_none()
        );
        let mut graph = ControlEntry {
            entry,
            start,
            blocks: builder
                .blocks
                .into_iter()
                .map(|block| Block {
                    source: block.source,
                    input: block.input,
                    operations: block.operations,
                    exit: block.exit.expect("every checked control path has an exit"),
                    live: Vec::new(),
                })
                .collect(),
            resumes: builder.resumes,
            temporaries: builder.temporaries,
            results: builder.results,
        };
        let live = graph.liveness(bodies);
        for (block, live) in graph.blocks.iter_mut().zip(live) {
            block.live = live;
        }
        for resume in &mut graph.resumes {
            resume.live = graph.blocks[resume.block].live.clone();
        }
        graph.validate(bodies);
        graph
    }

    fn block(&mut self, source: &'a Expr<Routed>, input: Option<Slot>) -> BlockId {
        let id = self.blocks.len();
        self.blocks.push(DraftBlock {
            source,
            input,
            operations: Vec::new(),
            exit: None,
        });
        id
    }

    fn temporary(&mut self, source: &'a Expr<Routed>, kind: ProducerKind) -> Slot {
        let slot = Slot::Temporary(self.temporaries.len());
        self.temporaries.push(Producer { source, kind });
        slot
    }

    fn exit(&mut self, block: BlockId, exit: Exit<'a>) {
        assert!(
            self.blocks[block].exit.replace(exit).is_none(),
            "a control block has one exit"
        );
    }

    fn finish(&mut self, block: BlockId, slot: Slot, tail: bool) -> Option<(BlockId, Slot)> {
        if tail {
            self.exit(block, Exit::Return(slot));
            None
        } else {
            Some((block, slot))
        }
    }

    fn atom(
        &mut self,
        block: BlockId,
        source: &'a Expr<Routed>,
        kind: AtomKind,
        operands: Vec<Operand>,
    ) -> Slot {
        let output = self.temporary(source, ProducerKind::Atom(kind));
        self.blocks[block].operations.push(Operation::Atom {
            output,
            source,
            kind,
            operands,
        });
        output
    }

    fn invoke(
        &mut self,
        block: BlockId,
        mut invoke: Invoke<'a>,
        tail: bool,
    ) -> Option<(BlockId, Slot)> {
        if tail {
            invoke.completion = Completion::Return;
            self.exit(block, Exit::Invoke(invoke));
            None
        } else {
            let kind = match invoke.stage {
                TailStage::Type => ProducerKind::TypeStage(
                    invoke.type_index.expect("a type edge selects one binder"),
                ),
                TailStage::Value => ProducerKind::ValueStage,
            };
            let result = self.temporary(invoke.source, kind);
            let next = self.block(invoke.source, Some(result));
            let resume = self.resumes.len();
            self.resumes.push(Resume {
                block: next,
                result,
                live: Vec::new(),
            });
            invoke.completion = Completion::Resume(resume);
            self.exit(block, Exit::Invoke(invoke));
            Some((next, result))
        }
    }

    fn type_stages(
        &mut self,
        source: &'a Expr<Routed>,
        mut block: BlockId,
        mut callee: Slot,
        count: usize,
        receiver: Option<Slot>,
        tail: bool,
    ) -> Option<(BlockId, Slot)> {
        for index in 0..count {
            let next = self.invoke(
                block,
                Invoke {
                    source,
                    callee,
                    stage: TailStage::Type,
                    type_index: Some(index),
                    receiver,
                    arguments: Vec::new(),
                    completion: Completion::Return,
                },
                tail && index + 1 == count,
            );
            match next {
                Some((next, value)) => {
                    block = next;
                    callee = value;
                }
                None => return None,
            }
        }
        self.finish(block, callee, tail)
    }

    fn arguments(&mut self, mut block: BlockId, args: &'a [Expr<Routed>]) -> (BlockId, Vec<Slot>) {
        let mut slots = Vec::new();
        for arg in args {
            let (next, slot) = self
                .lower(arg, block, false)
                .expect("an operand has a result");
            block = next;
            slots.push(slot);
        }
        (block, slots)
    }

    fn call(
        &mut self,
        source: &'a Expr<Routed>,
        block: BlockId,
        callee: Slot,
        tail: bool,
    ) -> Option<(BlockId, Slot)> {
        let (type_args, args, only_types) = match source {
            Expr::LowModuleCall {
                type_args,
                args,
                sig,
                ..
            }
            | Expr::LowQualifiedModuleCall {
                type_args,
                args,
                sig,
                ..
            } => (
                type_args,
                args,
                sig.has_type_params() && !sig.has_value_group(),
            ),
            Expr::LowClosureCall {
                type_args, args, ..
            }
            | Expr::LowIndirectCall {
                type_args, args, ..
            } => (type_args, args, false),
            _ => unreachable!("a routed call retains its explicit application stages"),
        };
        let type_count = type_args.len();
        if only_types {
            return self.type_stages(source, block, callee, type_count, None, tail);
        }
        let (block, callee) = self
            .type_stages(source, block, callee, type_count, None, false)
            .expect("leading type stages return the value callee");
        let (block, arguments) = self.arguments(block, args);
        self.invoke(
            block,
            Invoke {
                source,
                callee,
                stage: TailStage::Value,
                type_index: None,
                receiver: None,
                arguments,
                completion: Completion::Return,
            },
            tail,
        )
    }

    fn lower(
        &mut self,
        source: &'a Expr<Routed>,
        block: BlockId,
        tail: bool,
    ) -> Option<(BlockId, Slot)> {
        let result = match source {
            Expr::LowBoundRef { .. } => self.finish(
                block,
                Slot::Binding(self.bodies.reference_binding(source)),
                tail,
            ),
            Expr::Let { value, body, .. } => {
                let (block, input) = self
                    .lower(value, block, false)
                    .expect("a let initializer completes before binding");
                self.blocks[block].operations.push(Operation::Bind {
                    binding: self.bodies.local_binding(source),
                    input,
                    source,
                });
                self.lower(body, block, tail)
            }
            Expr::Seq { value, body, .. } => {
                let (block, _) = self
                    .lower(value, block, false)
                    .expect("a sequence prefix completes first");
                self.lower(body, block, tail)
            }
            Expr::EnrichedConditional {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                let (block, condition) = self
                    .lower(cond, block, false)
                    .expect("a condition completes before selection");
                let join = (!tail).then(|| {
                    let result = self.temporary(source, ProducerKind::Join);
                    (self.block(source, Some(result)), result)
                });
                let then_block = self.block(then_branch, None);
                let else_block = self.block(else_branch, None);
                self.exit(
                    block,
                    Exit::Branch {
                        source,
                        condition,
                        then_block,
                        else_block,
                    },
                );
                for (start, branch) in [
                    (then_block, then_branch.as_ref()),
                    (else_block, else_branch.as_ref()),
                ] {
                    if let Some((end, argument)) = self.lower(branch, start, tail) {
                        self.exit(
                            end,
                            Exit::Jump {
                                block: join.expect("a non-tail branch has a join").0,
                                argument,
                            },
                        );
                    }
                }
                join
            }
            Expr::EnrichedMatch {
                scrutinee, arms, ..
            } => {
                let (block, scrutinee) = self
                    .lower(scrutinee, block, false)
                    .expect("a scrutinee completes before selection");
                let join = (!tail).then(|| {
                    let result = self.temporary(source, ProducerKind::Join);
                    (self.block(source, Some(result)), result)
                });
                let targets = arms
                    .iter()
                    .enumerate()
                    .map(|(index, _)| {
                        self.block(
                            &arms[index].body,
                            Some(Slot::Binding(self.bodies.arm_binding(source, index))),
                        )
                    })
                    .collect::<Vec<_>>();
                self.exit(
                    block,
                    Exit::Match {
                        source,
                        scrutinee,
                        arms: targets.clone(),
                    },
                );
                for (start, arm) in targets.into_iter().zip(arms) {
                    if let Some((end, argument)) = self.lower(&arm.body, start, tail) {
                        self.exit(
                            end,
                            Exit::Jump {
                                block: join.expect("a non-tail match has a join").0,
                                argument,
                            },
                        );
                    }
                }
                join
            }
            Expr::LowModuleCall { .. } | Expr::LowQualifiedModuleCall { .. } => {
                let callee = self.atom(block, source, AtomKind::NamedCallable, Vec::new());
                self.call(source, block, callee, tail)
            }
            Expr::LowClosureCall { .. } => self.call(
                source,
                block,
                Slot::Binding(self.bodies.reference_binding(source)),
                tail,
            ),
            Expr::LowIndirectCall { callee, .. } => {
                let (block, callee) = self
                    .lower(callee, block, false)
                    .expect("a computed callee completes first");
                self.call(source, block, callee, tail)
            }
            Expr::LowTypeApplication { callee, .. } => {
                let (block, callee) = self
                    .lower(callee, block, false)
                    .expect("a computed type callee completes first");
                self.type_stages(source, block, callee, 1, None, tail)
            }
            Expr::LowCpsProjectorApply {
                receiver,
                continuation,
                continuation_ty,
                ..
            } => {
                let (block, receiver) = self
                    .lower(receiver, block, false)
                    .expect("a CPS receiver completes first");
                let payload = self.atom(
                    block,
                    source,
                    AtomKind::CpsPayload,
                    vec![Operand {
                        role: ChildRole::Receiver,
                        slot: receiver,
                    }],
                );
                let (block, callee) = self
                    .lower(continuation, block, false)
                    .expect("a CPS continuation is evaluated after projection");
                let (count, arity) = cps_continuation_runtime_shape(continuation_ty);
                let (block, callee) = self
                    .type_stages(source, block, callee, count, Some(receiver), false)
                    .expect("CPS type stages return its continuation");
                self.invoke(
                    block,
                    Invoke {
                        source,
                        callee,
                        stage: TailStage::Value,
                        type_index: None,
                        receiver: Some(receiver),
                        arguments: if arity == 0 {
                            Vec::new()
                        } else {
                            vec![payload]
                        },
                        completion: Completion::Return,
                    },
                    tail,
                )
            }
            Expr::FnExpr { body, .. } => {
                let slot = self.atom(
                    block,
                    source,
                    AtomKind::Closure(self.bodies.entry_for_body(body).id),
                    Vec::new(),
                );
                self.finish(block, slot, tail)
            }
            Expr::EnrichedTuple { items, .. } => {
                let (block, slots) = self.arguments(block, items);
                let operands = slots
                    .into_iter()
                    .enumerate()
                    .map(|(index, slot)| Operand {
                        role: ChildRole::Item(index),
                        slot,
                    })
                    .collect();
                let slot = self.atom(block, source, AtomKind::Tuple, operands);
                self.finish(block, slot, tail)
            }
            Expr::LowHostCall { args, .. } => {
                let (block, slots) = self.arguments(block, args);
                let operands = slots
                    .into_iter()
                    .enumerate()
                    .map(|(index, slot)| Operand {
                        role: ChildRole::Argument(index),
                        slot,
                    })
                    .collect();
                let slot = self.atom(block, source, AtomKind::HostCall, operands);
                self.finish(block, slot, tail)
            }
            Expr::EnrichedProject { target, .. }
            | Expr::EnrichedFieldGet { target, .. }
            | Expr::LowNewtypeProj { target, .. } => {
                let (block, slot) = self
                    .lower(target, block, false)
                    .expect("a representation operand completes first");
                let kind = match source {
                    Expr::EnrichedProject { .. } => AtomKind::Project,
                    Expr::EnrichedFieldGet { .. } => AtomKind::Field,
                    _ => AtomKind::Nominal,
                };
                let slot = self.atom(
                    block,
                    source,
                    kind,
                    vec![Operand {
                        role: ChildRole::Target,
                        slot,
                    }],
                );
                self.finish(block, slot, tail)
            }
            Expr::EnrichedInject { payload, .. }
            | Expr::LowNewtypeCtor { payload, .. }
            | Expr::LowQualifiedNewtypeMember { payload, .. } => {
                let (block, slot) = self
                    .lower(payload, block, false)
                    .expect("a representation payload completes first");
                let kind = if matches!(source, Expr::EnrichedInject { .. }) {
                    AtomKind::Inject
                } else {
                    AtomKind::Nominal
                };
                let slot = self.atom(
                    block,
                    source,
                    kind,
                    vec![Operand {
                        role: ChildRole::Payload,
                        slot,
                    }],
                );
                self.finish(block, slot, tail)
            }
            Expr::LowAbsurdCall { value_arg, .. } => {
                let (block, slot) = self
                    .lower(value_arg, block, false)
                    .expect("the empty value operand completes first");
                let slot = self.atom(
                    block,
                    source,
                    AtomKind::Absurd,
                    vec![Operand {
                        role: ChildRole::Absurd,
                        slot,
                    }],
                );
                self.finish(block, slot, tail)
            }
            Expr::Unit { .. }
            | Expr::StrLit { .. }
            | Expr::IntLit { .. }
            | Expr::FloatLit { .. }
            | Expr::BoolLit { .. }
            | Expr::LowHostFnValueRef { .. }
            | Expr::LowModuleFnValueRef { .. } => {
                let slot = self.atom(block, source, AtomKind::Leaf, Vec::new());
                self.finish(block, slot, tail)
            }
            Expr::BlockCall { ext, .. } => match *ext {},
            Expr::Path { ext, .. } | Expr::Call { ext, .. } => match *ext {},
            Expr::Tuple { ext, .. } => match *ext {},
            Expr::FnPlaceholder { ext, .. } => match *ext {},
            Expr::LabelValue { ext, .. } => match *ext {},
            Expr::RowLet { ext, .. } => match *ext {},
            Expr::Elaborator { ext, .. } | Expr::UserElaborator { ext, .. } => match *ext {},
            Expr::RecOrder { ext, .. } | Expr::RecQuote { ext, .. } => match *ext {},
            Expr::Ufcs { ext, .. } => match *ext {},
            Expr::OpChain { ext, .. } => match *ext {},
            Expr::RecCall { ext, .. } => match *ext {},
        };
        if let Some((_, slot)) = result {
            self.results
                .insert(source as *const Expr<Routed> as usize, slot);
        }
        result
    }
}

impl ControlEntry<'_> {
    pub(crate) fn source_result(&self, source: &Expr<Routed>) -> Slot {
        self.results[&(source as *const Expr<Routed> as usize)]
    }
    pub(crate) fn has_pending_work(&self) -> bool {
        !self.resumes.is_empty()
    }

    fn successors(&self, block: BlockId) -> Vec<BlockId> {
        match &self.blocks[block].exit {
            Exit::Return(_) => Vec::new(),
            Exit::Jump { block, .. } => vec![*block],
            Exit::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            Exit::Match { arms, .. } => arms.clone(),
            Exit::Invoke(invoke) => match invoke.completion {
                Completion::Return => Vec::new(),
                Completion::Resume(id) => vec![self.resumes[id].block],
            },
        }
    }

    /// Successor-first order for this acyclic source graph. Recursion occurs
    /// through Invoke, never through an intra-entry backedge.
    fn successor_order(&self) -> Vec<BlockId> {
        let mut order = Vec::new();
        let mut color = vec![0u8; self.blocks.len()];
        let mut pending = vec![(self.start, false)];
        while let Some((block, finish)) = pending.pop() {
            if finish {
                color[block] = 2;
                order.push(block);
                continue;
            }
            if color[block] == 2 {
                continue;
            }
            assert_eq!(
                color[block], 0,
                "an entry control graph has no execution backedge"
            );
            color[block] = 1;
            pending.push((block, true));
            for successor in self.successors(block).into_iter().rev() {
                pending.push((successor, false));
            }
        }
        assert_eq!(
            order.len(),
            self.blocks.len(),
            "every planned control block is reachable"
        );
        order
    }

    fn liveness(&self, bodies: &ExecutableBodyPlan<'_>) -> Vec<Vec<Slot>> {
        let mut live = vec![BTreeSet::new(); self.blocks.len()];
        for index in self.successor_order() {
            let block = &self.blocks[index];
            let mut required = BTreeSet::new();
            match &block.exit {
                Exit::Return(slot) => {
                    required.insert(*slot);
                }
                Exit::Jump { block, argument } => {
                    required.extend(&live[*block]);
                    required.insert(*argument);
                }
                Exit::Branch {
                    condition,
                    then_block,
                    else_block,
                    ..
                } => {
                    required.extend(&live[*then_block]);
                    required.extend(&live[*else_block]);
                    required.insert(*condition);
                }
                Exit::Match {
                    scrutinee, arms, ..
                } => {
                    for arm in arms {
                        required.extend(&live[*arm]);
                    }
                    required.insert(*scrutinee);
                }
                Exit::Invoke(invoke) => {
                    if let Completion::Resume(resume) = invoke.completion {
                        required.extend(&live[self.resumes[resume].block]);
                    }
                    required.insert(invoke.callee);
                    required.extend(&invoke.arguments);
                    required.extend(invoke.receiver);
                }
            }
            for operation in block.operations.iter().rev() {
                match operation {
                    Operation::Bind { binding, input, .. } => {
                        required.remove(&Slot::Binding(*binding));
                        required.insert(*input);
                    }
                    Operation::Atom {
                        output,
                        kind,
                        operands,
                        ..
                    } => {
                        required.remove(output);
                        required.extend(operands.iter().map(|operand| operand.slot));
                        if let AtomKind::Closure(entry) = kind {
                            required.extend(
                                bodies
                                    .entry(*entry)
                                    .captures()
                                    .iter()
                                    .copied()
                                    .map(Slot::Binding),
                            );
                        }
                    }
                }
            }
            if let Some(input) = block.input {
                required.remove(&input);
            }
            live[index] = required;
        }
        live.into_iter()
            .map(|slots| slots.into_iter().collect())
            .collect()
    }

    pub(crate) fn validate(&self, bodies: &ExecutableBodyPlan<'_>) {
        let computed = self.liveness(bodies);
        let mut producers = HashMap::new();
        for (index, producer) in self.temporaries.iter().enumerate() {
            assert!(
                producers
                    .insert(
                        (
                            producer.source as *const Expr<Routed> as usize,
                            producer.kind
                        ),
                        Slot::Temporary(index)
                    )
                    .is_none(),
                "a checked occurrence/stage has one result producer"
            );
        }
        let produced = |source: &Expr<Routed>, kind| {
            producers[&(source as *const Expr<Routed> as usize, kind)]
        };
        for (index, block) in self.blocks.iter().enumerate() {
            assert_eq!(
                block.live, computed[index],
                "a control block retains its exact live inputs"
            );
            let mut available = block
                .live
                .iter()
                .copied()
                .chain(block.input)
                .collect::<BTreeSet<_>>();
            for operation in &block.operations {
                match operation {
                    Operation::Bind {
                        binding,
                        input,
                        source,
                    } => {
                        let Expr::Let { value, .. } = source else {
                            unreachable!("a control binding belongs to a checked let")
                        };
                        assert_eq!(
                            *binding,
                            bodies.local_binding(source),
                            "a control binding preserves lexical identity"
                        );
                        assert_eq!(
                            *input,
                            self.source_result(value),
                            "a control binding receives its initializer result"
                        );
                        assert!(
                            available.contains(input),
                            "a binding initializer result is available"
                        );
                        assert!(
                            available.insert(Slot::Binding(*binding)),
                            "a local control binding is defined once"
                        );
                    }
                    Operation::Atom {
                        output,
                        source,
                        kind,
                        operands,
                    } => {
                        let children: Vec<(ChildRole, &Expr<Routed>)> = match (kind, *source) {
                            (
                                AtomKind::Leaf,
                                Expr::Unit { .. }
                                | Expr::StrLit { .. }
                                | Expr::IntLit { .. }
                                | Expr::FloatLit { .. }
                                | Expr::BoolLit { .. }
                                | Expr::LowHostFnValueRef { .. }
                                | Expr::LowModuleFnValueRef { .. },
                            ) => Vec::new(),
                            (AtomKind::Closure(id), Expr::FnExpr { body, .. }) => {
                                assert_eq!(
                                    *id,
                                    bodies.entry_for_body(body).id,
                                    "a closure operation selects its own checked entry"
                                );
                                assert!(
                                    bodies
                                        .entry(*id)
                                        .captures()
                                        .iter()
                                        .all(|id| available.contains(&Slot::Binding(*id))),
                                    "closure construction has every planned capture"
                                );
                                Vec::new()
                            }
                            (AtomKind::Tuple, Expr::EnrichedTuple { items, .. }) => items
                                .iter()
                                .enumerate()
                                .map(|(i, item)| (ChildRole::Item(i), item))
                                .collect(),
                            (AtomKind::Project, Expr::EnrichedProject { target, .. })
                            | (AtomKind::Field, Expr::EnrichedFieldGet { target, .. })
                            | (AtomKind::Nominal, Expr::LowNewtypeProj { target, .. }) => {
                                vec![(ChildRole::Target, target)]
                            }
                            (AtomKind::Inject, Expr::EnrichedInject { payload, .. })
                            | (
                                AtomKind::Nominal,
                                Expr::LowNewtypeCtor { payload, .. }
                                | Expr::LowQualifiedNewtypeMember { payload, .. },
                            ) => vec![(ChildRole::Payload, payload)],
                            (AtomKind::HostCall, Expr::LowHostCall { args, .. }) => args
                                .iter()
                                .enumerate()
                                .map(|(i, arg)| (ChildRole::Argument(i), arg))
                                .collect(),
                            (AtomKind::Absurd, Expr::LowAbsurdCall { value_arg, .. }) => {
                                vec![(ChildRole::Absurd, value_arg)]
                            }
                            (
                                AtomKind::NamedCallable,
                                Expr::LowModuleCall { .. } | Expr::LowQualifiedModuleCall { .. },
                            ) => Vec::new(),
                            (AtomKind::CpsPayload, Expr::LowCpsProjectorApply { receiver, .. }) => {
                                vec![(ChildRole::Receiver, receiver)]
                            }
                            _ => unreachable!(
                                "a control atom belongs to the child-free representation whitelist"
                            ),
                        };
                        assert_eq!(
                            operands.len(),
                            children.len(),
                            "a control atom consumes every checked child exactly once"
                        );
                        for (operand, (role, child)) in operands.iter().zip(children) {
                            assert_eq!(
                                operand.role, role,
                                "a control atom preserves ordered child roles"
                            );
                            assert_eq!(
                                operand.slot,
                                self.source_result(child),
                                "a control atom consumes the exact child result"
                            );
                            assert!(
                                available.contains(&operand.slot),
                                "a control atom's operand is available"
                            );
                        }
                        assert_eq!(
                            *output,
                            produced(source, ProducerKind::Atom(*kind)),
                            "an atom defines its own checked result"
                        );
                        assert!(
                            available.insert(*output),
                            "a control temporary is defined once"
                        );
                    }
                }
            }
            for successor in self.successors(index) {
                assert!(
                    self.blocks[successor]
                        .live
                        .iter()
                        .all(|slot| available.contains(slot)),
                    "a control edge supplies every live successor input"
                );
            }
            match &block.exit {
                Exit::Return(slot) => assert!(
                    available.contains(slot),
                    "a returned control value is available"
                ),
                Exit::Jump { block, argument } => {
                    assert!(
                        available.contains(argument),
                        "a joined control value is available"
                    );
                    let Some(Slot::Temporary(input)) = self.blocks[*block].input else {
                        unreachable!("a join receives a checked expression result")
                    };
                    assert_eq!(
                        self.temporaries[input].kind,
                        ProducerKind::Join,
                        "a jump targets a checked expression join"
                    );
                    assert!(
                        std::ptr::eq(self.temporaries[input].source, self.blocks[*block].source),
                        "a join input belongs to its exact checked expression"
                    );
                }
                Exit::Branch {
                    source,
                    condition,
                    then_block,
                    else_block,
                } => {
                    let Expr::EnrichedConditional {
                        cond,
                        then_branch,
                        else_branch,
                        ..
                    } = source
                    else {
                        unreachable!("a branch belongs to a checked conditional")
                    };
                    assert_eq!(
                        *condition,
                        self.source_result(cond),
                        "a branch tests its exact condition"
                    );
                    assert!(
                        available.contains(condition),
                        "a branch condition is available"
                    );
                    assert!(
                        self.blocks[*then_block].input.is_none()
                            && self.blocks[*else_block].input.is_none(),
                        "conditional branches bind no payload"
                    );
                    assert!(
                        std::ptr::eq(self.blocks[*then_block].source, then_branch.as_ref())
                            && std::ptr::eq(self.blocks[*else_block].source, else_branch.as_ref()),
                        "a conditional preserves its exact selected branch destinations"
                    );
                }
                Exit::Match {
                    source,
                    scrutinee,
                    arms,
                } => {
                    let Expr::EnrichedMatch {
                        scrutinee: source_scrutinee,
                        arms: source_arms,
                        ..
                    } = source
                    else {
                        unreachable!("a match belongs to a checked sum dispatch")
                    };
                    assert_eq!(
                        *scrutinee,
                        self.source_result(source_scrutinee),
                        "a match tests its exact scrutinee"
                    );
                    assert!(
                        available.contains(scrutinee),
                        "a match scrutinee is available"
                    );
                    assert_eq!(
                        arms.len(),
                        source_arms.len(),
                        "a match preserves every checked arm"
                    );
                    for (index, arm) in arms.iter().enumerate() {
                        assert!(
                            std::ptr::eq(self.blocks[*arm].source, &source_arms[index].body),
                            "a match preserves its exact arm destination"
                        );
                        assert_eq!(
                            self.blocks[*arm].input,
                            Some(Slot::Binding(bodies.arm_binding(source, index))),
                            "a match arm binds its exact lexical payload"
                        );
                    }
                }
                Exit::Invoke(invoke) => {
                    assert!(
                        available.contains(&invoke.callee)
                            && invoke
                                .arguments
                                .iter()
                                .chain(invoke.receiver.iter())
                                .all(|slot| available.contains(slot)),
                        "an invoke consumes available callee and argument values"
                    );
                }
            }
            if let Exit::Invoke(invoke) = &block.exit {
                assert_eq!(
                    invoke.type_index.is_some(),
                    invoke.stage == TailStage::Type,
                    "a control invocation identifies its exact selected stage"
                );
                let (initial, count, arguments, receiver, only_types) = match invoke.source {
                    Expr::LowModuleCall {
                        type_args,
                        args,
                        sig,
                        ..
                    }
                    | Expr::LowQualifiedModuleCall {
                        type_args,
                        args,
                        sig,
                        ..
                    } => (
                        produced(invoke.source, ProducerKind::Atom(AtomKind::NamedCallable)),
                        type_args.len(),
                        args.iter()
                            .map(|arg| self.source_result(arg))
                            .collect::<Vec<_>>(),
                        None,
                        sig.has_type_params() && !sig.has_value_group(),
                    ),
                    Expr::LowClosureCall {
                        type_args, args, ..
                    } => (
                        Slot::Binding(bodies.reference_binding(invoke.source)),
                        type_args.len(),
                        args.iter().map(|arg| self.source_result(arg)).collect(),
                        None,
                        false,
                    ),
                    Expr::LowIndirectCall {
                        callee,
                        type_args,
                        args,
                        ..
                    } => (
                        self.source_result(callee),
                        type_args.len(),
                        args.iter().map(|arg| self.source_result(arg)).collect(),
                        None,
                        false,
                    ),
                    Expr::LowTypeApplication { callee, .. } => {
                        (self.source_result(callee), 1, Vec::new(), None, true)
                    }
                    Expr::LowCpsProjectorApply {
                        receiver,
                        continuation,
                        continuation_ty,
                        ..
                    } => {
                        let (count, arity) = cps_continuation_runtime_shape(continuation_ty);
                        (
                            self.source_result(continuation),
                            count,
                            if arity == 0 {
                                Vec::new()
                            } else {
                                vec![produced(
                                    invoke.source,
                                    ProducerKind::Atom(AtomKind::CpsPayload),
                                )]
                            },
                            Some(self.source_result(receiver)),
                            false,
                        )
                    }
                    _ => unreachable!("an invoke is an executing checked application"),
                };
                assert_eq!(
                    invoke.receiver, receiver,
                    "a CPS invocation retains its exact evaluated packet"
                );
                let selected = match invoke.stage {
                    TailStage::Type => {
                        let index = invoke.type_index.unwrap();
                        assert!(
                            index < count,
                            "a type invocation selects an existing binder"
                        );
                        assert!(
                            invoke.arguments.is_empty(),
                            "a type invocation has no value operands"
                        );
                        index
                    }
                    TailStage::Value => {
                        assert!(
                            !only_types,
                            "a type-only elimination never invokes its callable result"
                        );
                        assert_eq!(
                            invoke.arguments, arguments,
                            "a value invocation preserves exact argument result order"
                        );
                        count
                    }
                };
                assert_eq!(
                    invoke.callee,
                    if selected == 0 {
                        initial
                    } else {
                        produced(invoke.source, ProducerKind::TypeStage(selected - 1))
                    },
                    "an invocation consumes its exact preceding callable stage"
                );
                if let Completion::Resume(id) = invoke.completion {
                    let resume = &self.resumes[id];
                    assert!(
                        std::ptr::eq(self.blocks[resume.block].source, invoke.source),
                        "an invoke resumes its own checked continuation block"
                    );
                    assert_eq!(
                        self.blocks[resume.block].input,
                        Some(resume.result),
                        "an invoke result reaches its own resume input"
                    );
                    let Slot::Temporary(result) = resume.result else {
                        unreachable!("an invoke produces a fresh temporary")
                    };
                    let producer = &self.temporaries[result];
                    assert!(
                        std::ptr::eq(producer.source, invoke.source),
                        "an invoke result retains its exact checked source"
                    );
                    assert_eq!(
                        producer.kind,
                        match invoke.stage {
                            TailStage::Type => ProducerKind::TypeStage(invoke.type_index.unwrap()),
                            TailStage::Value => ProducerKind::ValueStage,
                        },
                        "an invoke result retains its selected result type"
                    );
                    assert_eq!(
                        resume.live, computed[resume.block],
                        "a resume saves exactly its live inputs"
                    );
                    assert!(
                        !resume.live.contains(&resume.result),
                        "an unfinished invocation result is not captured"
                    );
                } else {
                    assert!(
                        TailContext::body(invoke.source).tail_stage().is_some(),
                        "a tail graph edge is an executable checked application"
                    );
                }
            }
        }
        let entry = bodies.entry(self.entry);
        assert!(
            std::ptr::eq(self.blocks[self.start].source, entry.body().expr()),
            "a control entry starts at its own checked body"
        );
        let final_stage = entry
            .stages
            .last()
            .expect("an executable entry has a stage");
        let available = final_stage
            .captures
            .iter()
            .chain(&final_stage.bindings)
            .copied()
            .map(Slot::Binding)
            .collect::<BTreeSet<_>>();
        assert!(
            computed[self.start]
                .iter()
                .all(|slot| available.contains(slot)),
            "a control entry reads only its planned lexical inputs"
        );
    }
}

#[cfg(test)]
mod tests;

use super::*;

#[cfg(test)]
mod type_depth_tests;
use std::num::NonZeroUsize;

mod branch_callback;
mod callback;
mod comparison;
mod reflection;

#[derive(Clone)]
enum Code<'code> {
    Borrowed(&'code EvalExpr),
    Owned(EvalRef<EvalExpr>),
}

impl<'code> Code<'code> {
    fn expr(&self) -> &EvalExpr {
        match self {
            Self::Borrowed(expr) => expr,
            Self::Owned(expr) => expr,
        }
    }

    fn child(&self, select: impl FnOnce(&EvalExpr) -> &EvalRef<EvalExpr>) -> Self {
        match self {
            Self::Borrowed(expr) => Self::Borrowed(select(expr)),
            Self::Owned(expr) => Self::Owned(select(expr).clone()),
        }
    }

    fn function(&self) -> Function<'code> {
        fn select(expr: &EvalExpr) -> &EvalRef<EvalFunction> {
            match expr {
                EvalExpr::CallFn { function, .. }
                | EvalExpr::CallFnExpr { function, .. }
                | EvalExpr::FnRef { function, .. }
                | EvalExpr::FnExpr { function, .. } => function,
                _ => unreachable!("direct call owns its function"),
            }
        }
        match self {
            Self::Borrowed(expr) => Function::Borrowed(select(expr)),
            Self::Owned(expr) => Function::Owned(select(expr).clone()),
        }
    }

    fn branch(&self, left: bool) -> Self {
        self.child(|expr| match expr {
            EvalExpr::CoreIf {
                then_thunk,
                else_thunk,
                ..
            } => {
                if left {
                    then_thunk
                } else {
                    else_thunk
                }
            }
            EvalExpr::CoreEither {
                left: on_left,
                right,
                ..
            } => {
                if left {
                    on_left
                } else {
                    right
                }
            }
            EvalExpr::CoreEitherThunk {
                left_thunk,
                right_thunk,
                ..
            } => {
                if left {
                    left_thunk
                } else {
                    right_thunk
                }
            }
            _ => unreachable!("branch expression retains its handlers"),
        })
    }
}

enum Function<'code> {
    Borrowed(&'code EvalRef<EvalFunction>),
    Owned(EvalRef<EvalFunction>),
}

impl<'code> Function<'code> {
    fn get(&self) -> &EvalRef<EvalFunction> {
        match self {
            Self::Borrowed(function) => function,
            Self::Owned(function) => function,
        }
    }

    fn body(&self) -> Code<'code> {
        match self {
            Self::Borrowed(function) => Code::Borrowed(&function.body),
            Self::Owned(function) => Code::Owned(function.body.clone()),
        }
    }
}

/// Argument and residual ranges share one operation-owned buffer. Promotion keeps
/// its allocation until the operation finishes, not until the current call returns.
struct Values {
    inline: [Value; 3],
    heap: Vec<Value>,
    len: usize,
}

impl Values {
    fn new() -> Self {
        Self {
            inline: [Value::Unit, Value::Unit, Value::Unit],
            heap: Vec::new(),
            len: 0,
        }
    }

    fn as_slice(&self) -> &[Value] {
        if self.heap.capacity() == 0 {
            &self.inline[..self.len]
        } else {
            &self.heap
        }
    }

    fn as_mut_slice(&mut self) -> &mut [Value] {
        if self.heap.capacity() == 0 {
            &mut self.inline[..self.len]
        } else {
            &mut self.heap
        }
    }

    fn push(&mut self, value: Value) -> bool {
        let mut grows = false;
        if self.heap.capacity() == 0 {
            if self.len < 3 {
                self.inline[self.len] = value;
                self.len += 1;
                return false;
            }
            self.heap.reserve(8);
            for slot in &mut self.inline {
                self.heap.push(std::mem::replace(slot, Value::Unit));
            }
            grows = true;
        }
        grows |= self.heap.len() == self.heap.capacity();
        self.heap.push(value);
        self.len += 1;
        grows
    }

    fn pop(&mut self) -> Option<Value> {
        self.len = self.len.checked_sub(1)?;
        if self.heap.capacity() == 0 {
            Some(std::mem::replace(&mut self.inline[self.len], Value::Unit))
        } else {
            self.heap.pop()
        }
    }

    fn take(&mut self, index: usize) -> Value {
        std::mem::replace(&mut self.as_mut_slice()[index], Value::Unit)
    }

    fn truncate(&mut self, len: usize) {
        while self.len > len {
            drop(self.pop());
        }
    }

    fn take_args(&mut self, start: usize) -> EvalArgs {
        let len = self.len - start;
        let args = match len {
            0 => EvalArgs::Empty,
            1 => EvalArgs::One([self.take(start)]),
            2 => EvalArgs::Two([self.take(start), self.take(start + 1)]),
            3 => EvalArgs::Three([self.take(start), self.take(start + 1), self.take(start + 2)]),
            _ => EvalArgs::Many((start..self.len).map(|i| self.take(i)).collect()),
        };
        self.truncate(start);
        args
    }
}

impl Drop for Values {
    fn drop(&mut self) {
        self.truncate(0);
    }
}

/// Shallow evaluation uses the inline region; deeper work spills without a depth limit.
struct Stack<T, const N: usize> {
    inline: [Option<T>; N],
    spill: Vec<T>,
    len: usize,
}

#[cfg(test)]
#[derive(Default, Debug)]
pub(super) struct StorageMetrics {
    pub frame_pushes: usize,
    pub frame_peak: usize,
    pub frame_growths: usize,
    pub value_pushes: usize,
    pub value_peak: usize,
    pub value_growths: usize,
    pub activation_pushes: usize,
    pub activation_peak: usize,
    pub record_pushes: usize,
    pub record_peak: usize,
    pub record_growths: usize,
    active_calls: usize,
    pub local_promotions: usize,
    pub local_copies: usize,
}

trait StorageObserver {
    #[inline]
    fn machine_started(&mut self) {}
    #[inline]
    fn frame_pushed(&mut self, _len: usize, _grows: bool) {}
    #[inline]
    fn value_pushed(&mut self, _len: usize, _grows: bool) {}
    #[inline]
    fn activation_pushed(&mut self) {}
    #[inline]
    fn activation_returned(&mut self) {}
    #[inline]
    fn record_pushed(&mut self, _len: usize, _grows: bool) {}
    #[inline]
    fn locals_promoted(&mut self, _copies: usize) {}
}

impl StorageObserver for () {}

#[cfg(test)]
impl StorageObserver for &mut StorageMetrics {
    fn machine_started(&mut self) {
        self.active_calls = 0;
    }

    fn frame_pushed(&mut self, len: usize, grows: bool) {
        self.frame_pushes += 1;
        self.frame_peak = self.frame_peak.max(len);
        self.frame_growths += usize::from(grows);
    }

    fn value_pushed(&mut self, len: usize, grows: bool) {
        self.value_pushes += 1;
        self.value_peak = self.value_peak.max(len);
        self.value_growths += usize::from(grows);
    }

    fn activation_pushed(&mut self) {
        self.activation_pushes += 1;
        self.active_calls += 1;
        self.activation_peak = self.activation_peak.max(self.active_calls);
    }

    fn activation_returned(&mut self) {
        self.active_calls -= 1;
    }

    fn record_pushed(&mut self, len: usize, grows: bool) {
        self.record_pushes += 1;
        self.record_peak = self.record_peak.max(len);
        self.record_growths += usize::from(grows);
    }

    fn locals_promoted(&mut self, copies: usize) {
        self.local_promotions += 1;
        self.local_copies += copies;
    }
}

impl<T, const N: usize> Stack<T, N> {
    fn new() -> Self {
        Self {
            inline: std::array::from_fn(|_| None),
            spill: Vec::new(),
            len: 0,
        }
    }

    fn push(&mut self, value: T) -> bool {
        let grows = self.len >= N && self.spill.len() == self.spill.capacity();
        if self.len < N {
            self.inline[self.len] = Some(value);
        } else {
            self.spill.push(value);
        }
        self.len += 1;
        grows
    }

    fn pop(&mut self) -> Option<T> {
        self.len = self.len.checked_sub(1)?;
        if self.len < N {
            self.inline[self.len].take()
        } else {
            self.spill.pop()
        }
    }

    fn discard_top(&mut self) {
        self.len = self.len.checked_sub(1).expect("live stack entry");
        if self.len < N {
            self.inline[self.len] = None;
        } else {
            self.spill.truncate(self.spill.len() - 1);
        }
    }

    fn get(&self, index: usize) -> &T {
        if index < N {
            self.inline[index]
                .as_ref()
                .expect("live inline stack entry")
        } else {
            &self.spill[index - N]
        }
    }

    fn get_mut(&mut self, index: usize) -> &mut T {
        if index < N {
            self.inline[index]
                .as_mut()
                .expect("live inline stack entry")
        } else {
            &mut self.spill[index - N]
        }
    }
}

impl<T, const N: usize> Drop for Stack<T, N> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

#[derive(Clone, Copy)]
enum Backing {
    RootCaptured,
    RootLocals,
    Arguments(usize),
    Locals(usize),
    Captures(usize),
}

/// Ranges refer directly to the ultimate owner, never another range.
#[derive(Clone, Copy)]
struct CaptureRange {
    backing: Backing,
    start: usize,
    len: usize,
}

enum Captures {
    View(CaptureRange),
    Owned(Vec<Value>),
    Shared(EvalRef<Vec<Value>>),
}

impl Captures {
    fn empty() -> Self {
        Self::View(CaptureRange {
            backing: Backing::RootCaptured,
            start: 0,
            len: 0,
        })
    }
}

type Owner = Option<NonZeroUsize>;

struct CallRecord<'code> {
    function: Function<'code>,
    captures: Captures,
    args: usize,
    end: usize,
    base: usize,
    locals: Option<Vec<Value>>,
    memo: bool,
    started: Option<Instant>,
    generic_started: Option<Instant>,
    caller: Owner,
}

enum Frame<'code> {
    ComparisonApply,
    ReflectedBranch,
    BinderCallback,
    Proofed(Code<'code>),
    #[cfg(feature = "surface")]
    ProofRetry {
        builtin: ComptimeBuiltin,
        base: usize,
    },
    FoldStep,
    DirectResidual {
        builtin: ComptimeBuiltin,
        base: usize,
        right: Option<Code<'code>>,
    },
    DirectArguments {
        code: Code<'code>,
        base: usize,
        next: usize,
    },
    LetValue(Code<'code>),
    LetBody,
    SeqValue(Code<'code>),
    SeqBody,
    Callee {
        call: Code<'code>,
        base: usize,
    },
    Arguments {
        call: Code<'code>,
        base: usize,
        next: usize,
    },
    Unary(Code<'code>),
    Condition(Code<'code>),
    ResidualLeft {
        right: Code<'code>,
        base: usize,
        name: &'static str,
    },
    ResidualRight {
        base: usize,
        name: &'static str,
    },
    Scrutinee(Code<'code>),
    EitherLeft {
        code: Code<'code>,
        base: usize,
    },
    EitherRight {
        base: usize,
    },
    HandlerReturn {
        base: usize,
        args: usize,
    },
    BranchLeft,
    BranchRight,
    Measure {
        kind: Measure,
        started: Instant,
    },
    StructuralSite,
    CallBody {
        excess: usize,
    },
    CallReturn,
}

struct Pending<'code> {
    frame: Frame<'code>,
    values: usize,
}

enum Control<'code> {
    Compare,
    Eval(Code<'code>),
    Apply { base: usize, args: usize },
    Task,
    Return,
}

#[derive(Clone)]
enum Handler<'code> {
    Expr(Code<'code>),
    Value(Value),
}

enum Task<'code> {
    Handler {
        handler: Handler<'code>,
        args: EvalArgs,
    },
    Either {
        scrutinee: Value,
        left: Handler<'code>,
        right: Handler<'code>,
        thunks: bool,
    },
}

struct Branch<'code> {
    scrutinee: EvalRef<Value>,
    left_payload: String,
    right_payload: String,
    next: Option<Task<'code>>,
    left: Option<Value>,
}

enum Measure {
    Atom,
    Generic,
    Reflection(ComptimeBuiltin),
}

struct Machine<'code, 'slots, 'values, 'ctx, 'artifact, Observer: StorageObserver> {
    root: &'slots mut EvalSlots<'values>,
    ctx: &'ctx EvalCtx<'artifact>,
    frames: Stack<Pending<'code>, 4>,
    values: Values,
    records: Stack<CallRecord<'code>, 1>,
    memos: Vec<ExactCallMemoKey>,
    tasks: Vec<Task<'code>>,
    branches: Vec<Branch<'code>>,
    folds: Vec<reflection::Fold<'code>>,
    callbacks: Vec<callback::Callback<'code>>,
    reflected_branches: Vec<branch_callback::Callback<'code>>,
    comparisons: Vec<comparison::Engine>,
    sites: Vec<StructuralRecurSiteGuard<'ctx, 'artifact>>,
    current: Owner,
    observer: Observer,
}

struct PendingCleanup<'a, 'code, 'slots, 'values, 'ctx, 'artifact, Observer: StorageObserver> {
    machine: &'a mut Machine<'code, 'slots, 'values, 'ctx, 'artifact, Observer>,
}

impl<Observer: StorageObserver> PendingCleanup<'_, '_, '_, '_, '_, '_, Observer> {
    fn drain(&mut self) {
        while self.machine.cleanup_step() {}
    }
}

impl<Observer: StorageObserver> Drop for PendingCleanup<'_, '_, '_, '_, '_, '_, Observer> {
    fn drop(&mut self) {
        self.drain();
    }
}

impl<Observer: StorageObserver> Drop for Machine<'_, '_, '_, '_, '_, Observer> {
    fn drop(&mut self) {
        PendingCleanup { machine: self }.drain();
    }
}

#[inline]
fn is_terminal(expr: &EvalExpr) -> bool {
    matches!(
        expr,
        EvalExpr::Unit
            | EvalExpr::Bool(_)
            | EvalExpr::IntAtom(..)
            | EvalExpr::FloatAtom(..)
            | EvalExpr::StrAtom(_)
            | EvalExpr::DiagnosticText(_)
            | EvalExpr::Primitive(_)
            | EvalExpr::StructuralRecurPrimitive(_)
            | EvalExpr::NewtypeMember(_)
            | EvalExpr::Const(_)
            | EvalExpr::Local(_)
            | EvalExpr::Atom(_)
    )
}

#[inline(always)]
fn terminal(expr: &EvalExpr, local: impl FnOnce(usize) -> Option<Value>) -> Value {
    match expr {
        EvalExpr::Unit => Value::Unit,
        EvalExpr::Bool(value) => Value::Bool(*value),
        EvalExpr::IntAtom(digits, annotation) => Value::IntAtom(digits.clone(), annotation.clone()),
        EvalExpr::FloatAtom(digits, annotation) => {
            Value::FloatAtom(digits.clone(), annotation.clone())
        }
        EvalExpr::StrAtom(value) => Value::StrAtom(value.clone()),
        EvalExpr::DiagnosticText(value) => Value::DiagnosticText(value.clone()),
        EvalExpr::Primitive(builtin) => Value::Primitive(*builtin),
        EvalExpr::StructuralRecurPrimitive(primitive) => Value::Data(primitive.clone()),
        EvalExpr::NewtypeMember(member) => Value::NewtypeMember(member.clone()),
        EvalExpr::Const(value) => value.clone(),
        EvalExpr::Local(slot) => {
            local(*slot).unwrap_or_else(|| Value::Atom(format!("<bad-local-{slot}>")))
        }
        EvalExpr::Atom(name) => Value::Atom(name.clone()),
        _ => unreachable!("terminal classification precedes reduction"),
    }
}

#[inline]
pub(super) fn eval(expr: &EvalExpr, slots: &mut EvalSlots<'_>, ctx: &EvalCtx<'_>) -> Value {
    if is_terminal(expr) {
        let value = terminal(expr, |slot| slots.get(slot).cloned());
        if let Some(metrics) = ctx.metrics.as_deref() {
            metrics.record_expr_visit();
        }
        return value;
    }
    eval_pending(expr, slots, ctx)
}

#[inline(never)]
fn eval_pending(expr: &EvalExpr, slots: &mut EvalSlots<'_>, ctx: &EvalCtx<'_>) -> Value {
    Machine::new(slots, ctx, ()).run(Control::Eval(Code::Borrowed(expr)))
}

pub(super) fn compare(left: &Value, right: &Value, ctx: &EvalCtx<'_>) -> bool {
    let mut slots = EvalSlots::root(Vec::new());
    let mut machine = Machine::new(&mut slots, ctx, ());
    machine
        .comparisons
        .push(comparison::Engine::equal(left.clone(), right.clone()));
    let Value::Bool(equal) = machine.run(Control::Compare) else {
        unreachable!("equality returns a boolean")
    };
    equal
}

pub(super) fn apply(callee: Value, args: EvalArgs, ctx: &EvalCtx<'_>) -> Value {
    let mut slots = EvalSlots::root(Vec::new());
    let mut machine = Machine::new(&mut slots, ctx, ());
    machine.push_value(callee);
    machine.push_args(args);
    machine.run(Control::Apply { base: 0, args: 1 })
}

#[cfg(test)]
pub(super) fn eval_handler(
    expr: &EvalExpr,
    args: EvalArgs,
    slots: &mut EvalSlots<'_>,
    ctx: &EvalCtx<'_>,
) -> Value {
    let mut machine = Machine::new(slots, ctx, ());
    let control = machine.task(Task::Handler {
        handler: Handler::Expr(Code::Borrowed(expr)),
        args,
    });
    machine.run(control)
}

#[cfg(test)]
pub(super) fn eval_with_storage(
    expr: &EvalExpr,
    slots: &mut EvalSlots<'_>,
    ctx: &EvalCtx<'_>,
    storage: &mut StorageMetrics,
) -> Value {
    Machine::new(slots, ctx, storage).run(Control::Eval(Code::Borrowed(expr)))
}

impl<'code, 'slots, 'values, 'ctx, 'artifact, Observer: StorageObserver>
    Machine<'code, 'slots, 'values, 'ctx, 'artifact, Observer>
{
    fn new(
        root: &'slots mut EvalSlots<'values>,
        ctx: &'ctx EvalCtx<'artifact>,
        observer: Observer,
    ) -> Self {
        Self {
            root,
            ctx,
            frames: Stack::new(),
            values: Values::new(),
            records: Stack::new(),
            memos: Vec::new(),
            folds: Vec::new(),
            callbacks: Vec::new(),
            reflected_branches: Vec::new(),
            comparisons: Vec::new(),
            tasks: Vec::new(),
            branches: Vec::new(),
            sites: Vec::new(),
            current: None,
            observer,
        }
    }

    fn push_frame(&mut self, frame: Frame<'code>) {
        let grows = self.frames.push(Pending {
            frame,
            values: self.values.len,
        });
        self.observer.frame_pushed(self.frames.len, grows);
    }

    fn push_value(&mut self, value: Value) {
        let grows = self.values.push(value);
        self.observer.value_pushed(self.values.len, grows);
    }

    fn cleanup_step(&mut self) -> bool {
        let Some(index) = self.frames.len.checked_sub(1) else {
            return false;
        };
        if self.values.len > self.frames.get(index).values {
            drop(self.values.pop());
            return true;
        }
        match &self.frames.get(index).frame {
            #[cfg(feature = "surface")]
            Frame::ProofRetry { base, .. } if self.values.len > *base => {
                drop(self.values.pop());
                return true;
            }
            Frame::Arguments { base, .. }
            | Frame::DirectArguments { base, .. }
            | Frame::DirectResidual { base, .. }
            | Frame::Callee { base, .. }
            | Frame::ResidualLeft { base, .. }
            | Frame::ResidualRight { base, .. }
            | Frame::EitherLeft { base, .. }
            | Frame::EitherRight { base }
            | Frame::HandlerReturn { base, .. }
                if self.values.len > *base =>
            {
                drop(self.values.pop());
                return true;
            }
            _ => {}
        }
        match self.frames.pop().expect("live continuation").frame {
            Frame::LetBody => self.pop_local(),
            Frame::SeqBody => {
                drop(self.values.pop());
            }
            Frame::BranchLeft | Frame::BranchRight => {
                self.branches.pop();
            }
            Frame::StructuralSite => {
                self.sites.pop();
            }
            Frame::FoldStep => {
                self.folds.pop();
            }
            Frame::BinderCallback => {
                self.callbacks.pop();
            }
            Frame::ReflectedBranch => {
                self.reflected_branches.pop();
            }
            Frame::ComparisonApply => {
                self.comparisons.pop();
            }
            Frame::CallReturn => {
                let record = self
                    .records
                    .get_mut(self.current.expect("call owner").get() - 1);
                record.locals = None;
                self.values.truncate(record.base);
                if record.memo {
                    self.memos.pop();
                }
                self.current = record.caller;
                self.records.discard_top();
            }
            _ => {}
        }
        true
    }

    fn backing(&self, backing: Backing) -> &[Value] {
        match backing {
            Backing::RootCaptured => self.root.captured,
            Backing::RootLocals => self.root.locals_slice(),
            Backing::Arguments(index) => {
                let record = self.records.get(index);
                &self.values.as_slice()[record.args..record.end]
            }
            Backing::Locals(index) => self
                .records
                .get(index)
                .locals
                .as_deref()
                .expect("owned activation local backing"),
            Backing::Captures(index) => match &self.records.get(index).captures {
                Captures::Owned(values) => values,
                Captures::Shared(values) => values,
                Captures::View(_) => {
                    unreachable!("capture views are normalized to their ultimate backing")
                }
            },
        }
    }

    fn capture_slice<'s>(&'s self, captures: &'s Captures) -> &'s [Value] {
        match captures {
            Captures::View(range) => {
                &self.backing(range.backing)[range.start..range.start + range.len]
            }
            Captures::Owned(values) => values,
            Captures::Shared(values) => values,
        }
    }

    fn current_captures(&self) -> &[Value] {
        if let Some(owner) = self.current {
            self.capture_slice(&self.records.get(owner.get() - 1).captures)
        } else {
            self.root.captured
        }
    }

    fn current_locals(&self) -> &[Value] {
        if let Some(owner) = self.current {
            let activation = self.records.get(owner.get() - 1);
            activation
                .locals
                .as_deref()
                .unwrap_or_else(|| &self.values.as_slice()[activation.args..activation.end])
        } else {
            self.root.locals_slice()
        }
    }

    fn local(&self, slot: usize) -> Option<&Value> {
        let captured = self.current_captures();
        if slot < captured.len() {
            captured.get(slot)
        } else {
            self.current_locals().get(slot - captured.len())
        }
    }

    fn push_local(&mut self, value: Value) {
        if let Some(owner) = self.current {
            let activation = self.records.get_mut(owner.get() - 1);
            if activation.locals.is_none() {
                self.observer
                    .locals_promoted(activation.end - activation.args);
            }
            activation
                .locals
                .get_or_insert_with(|| {
                    self.values.as_slice()[activation.args..activation.end].to_vec()
                })
                .push(value);
        } else {
            self.root.push(value);
        }
    }

    fn pop_local(&mut self) {
        if let Some(owner) = self.current {
            self.records
                .get_mut(owner.get() - 1)
                .locals
                .as_mut()
                .expect("let body owns its local extent")
                .pop();
        } else {
            self.root.pop();
        }
    }

    fn capture_range(&self, slots: &[usize]) -> Option<CaptureRange> {
        let Some((&first, rest)) = slots.split_first() else {
            return Some(CaptureRange {
                backing: Backing::RootCaptured,
                start: 0,
                len: 0,
            });
        };
        if rest
            .iter()
            .enumerate()
            .any(|(i, slot)| first.checked_add(i + 1) != Some(*slot))
        {
            return None;
        }
        let len = slots.len();
        let end = first.checked_add(len)?;
        let captured_len = self.current_captures().len();
        let current = self.current.map(|owner| owner.get() - 1);
        if end <= captured_len {
            let mut range = match current {
                None => CaptureRange {
                    backing: Backing::RootCaptured,
                    start: 0,
                    len: captured_len,
                },
                Some(index) => match &self.records.get(index).captures {
                    Captures::View(range) => *range,
                    Captures::Owned(_) | Captures::Shared(_) => CaptureRange {
                        backing: Backing::Captures(index),
                        start: 0,
                        len: captured_len,
                    },
                },
            };
            range.start += first;
            range.len = len;
            return Some(range);
        }
        if first < captured_len || end - captured_len > self.current_locals().len() {
            return None;
        }
        let backing = match current {
            None => Backing::RootLocals,
            Some(index) if self.records.get(index).locals.is_some() => Backing::Locals(index),
            Some(index) => Backing::Arguments(index),
        };
        Some(CaptureRange {
            backing,
            start: first - captured_len,
            len,
        })
    }

    fn captures(&self, slots: &[usize]) -> Captures {
        if let Some(range) = self.capture_range(slots) {
            return Captures::View(range);
        }
        let values: Vec<_> = slots
            .iter()
            .filter_map(|slot| self.local(*slot).cloned())
            .collect();
        if let Some(metrics) = self.ctx.metrics.as_deref() {
            metrics.record_capture_vec(values.len());
        }
        Captures::Owned(values)
    }

    fn closure(&self, expr: &EvalExpr) -> Value {
        let ctx = self.ctx;
        match expr {
            EvalExpr::FnRef {
                cache_key,
                function,
                share_closure,
            } => {
                if !*share_closure {
                    if let Some(metrics) = ctx.metrics.as_deref() {
                        metrics.record_closure_build();
                    }
                    return Value::EvalClosure {
                        function: function.clone(),
                        captured: Vec::new().into(),
                    };
                }
                let mut cache = ctx
                    .fn_closure_cache
                    .inner
                    .lock()
                    .expect("eval fn closure cache lock poisoned");
                if let Some(cached) = cache.get(cache_key).cloned() {
                    return cached;
                }
                if let Some(metrics) = ctx.metrics.as_deref() {
                    metrics.record_closure_build();
                }
                let value = Value::EvalClosure {
                    function: function.clone(),
                    captured: Vec::new().into(),
                };
                cache.insert(cache_key.clone(), value.clone());
                value
            }
            EvalExpr::FnExpr {
                function,
                captured_slots,
                empty_closure_cache,
            } => {
                if captured_slots.is_empty() {
                    if let Some(cached) = empty_closure_cache.get().cloned() {
                        return cached;
                    }
                    if let Some(metrics) = ctx.metrics.as_deref() {
                        metrics.record_closure_build();
                    }
                    let value = Value::EvalClosure {
                        function: function.clone(),
                        captured: Vec::new().into(),
                    };
                    if empty_closure_cache.set(value.clone()).is_ok() {
                        return value;
                    }
                    return empty_closure_cache.get().cloned().unwrap_or(value);
                }
                if let Some(metrics) = ctx.metrics.as_deref() {
                    metrics.record_closure_build();
                }
                let captured: Vec<Value> = captured_slots
                    .iter()
                    .filter_map(|slot| self.local(*slot).cloned())
                    .collect();
                if let Some(metrics) = ctx.metrics.as_deref() {
                    metrics.record_capture_vec(captured.len());
                }
                Value::EvalClosure {
                    function: function.clone(),
                    captured: captured.into(),
                }
            }
            _ => unreachable!("closure construction retains a function expression"),
        }
    }

    fn call_args(call: &EvalExpr) -> &[EvalRef<EvalExpr>] {
        match call {
            EvalExpr::Call { args, .. }
            | EvalExpr::CallAtom { args, .. }
            | EvalExpr::CallPrimitive { args, .. }
            | EvalExpr::CallFn { args, .. }
            | EvalExpr::CallFnExpr { args, .. }
            | EvalExpr::CoreCtor { args, .. } => args,
            _ => unreachable!("argument continuation retains an application"),
        }
    }

    fn start_arguments(
        &mut self,
        call: Code<'code>,
        base: usize,
        result: &mut Value,
    ) -> Control<'code> {
        if Self::call_args(call.expr()).is_empty() {
            self.complete_arguments(call, base, result)
        } else {
            let first = call.child(|expr| &Self::call_args(expr)[0]);
            self.push_frame(Frame::Arguments {
                call,
                base,
                next: 1,
            });
            Control::Eval(first)
        }
    }

    fn complete_arguments(
        &mut self,
        call: Code<'code>,
        base: usize,
        result: &mut Value,
    ) -> Control<'code> {
        match call.expr() {
            EvalExpr::CallFn { .. } | EvalExpr::CallFnExpr { .. } => {
                let captures = match call.expr() {
                    EvalExpr::CallFnExpr { captured_slots, .. } => self.captures(captured_slots),
                    _ => Captures::empty(),
                };
                self.enter_function(call.function(), captures, base, base, true, result)
            }
            EvalExpr::Call { .. } => Control::Apply {
                base,
                args: base + 1,
            },
            EvalExpr::CallAtom { name, .. } => {
                let args = self.take_arg_vec(base);
                self.measure(Measure::Atom);
                self.atom(AtomCallee::Borrowed(name), args, result)
            }
            EvalExpr::CallPrimitive { builtin, .. } => {
                let args = self.take_arg_vec(base);
                self.measure(Measure::Atom);
                self.primitive(*builtin, args, result)
            }
            EvalExpr::CoreCtor { name, .. } => {
                *result = data_value(*name, self.take_arg_vec(base));
                Control::Return
            }
            _ => unreachable!("argument continuation retains an application"),
        }
    }

    fn take_arg_vec(&mut self, base: usize) -> Vec<Value> {
        let args = self.values.take_args(base).into_vec();
        if let Some(metrics) = self.ctx.metrics.as_deref() {
            metrics.record_arg_vec(args.len());
        }
        args
    }

    fn enter_function(
        &mut self,
        function: Function<'code>,
        captures: Captures,
        base: usize,
        args: usize,
        record_apply: bool,
        result: &mut Value,
    ) -> Control<'code> {
        let definition = function.get();
        let exact = self.values.len - args == definition.abi_arity()
            && abi_slots_saturate_groups(definition);
        let memo = if exact {
            match lookup_exact_call_memo(
                definition,
                self.capture_slice(&captures),
                &self.values.as_slice()[args..],
                self.ctx,
            ) {
                Ok(memo) => memo,
                Err(value) => {
                    self.values.truncate(base);
                    *result = value;
                    return Control::Return;
                }
            }
        } else {
            None
        };
        let started = (record_apply && self.ctx.metrics.is_some()).then(Instant::now);
        let (locals, excess, generic_started) = if exact {
            (
                (!definition.params_are_abi_identity()).then(|| {
                    bind_eval_function_abi_slots(definition, &self.values.as_slice()[args..])
                }),
                self.values.len,
                None,
            )
        } else {
            if self.values.len == args
                && definition
                    .value_groups
                    .first()
                    .is_some_and(|group| group.abi_arity == 0)
            {
                self.push_value(Value::Unit);
            }
            let values = &self.values.as_slice()[args..];
            let group_count = definition.value_groups.len();
            if let Some((locals, consumed)) =
                bind_eval_function_group_args(definition, 0, 0, values, group_count)
            {
                (
                    Some(locals),
                    args + consumed,
                    self.ctx.metrics.as_deref().map(|_| Instant::now()),
                )
            } else {
                let mut suspended = None;
                for prefix in (1..group_count).rev() {
                    if let Some((locals, consumed)) =
                        bind_eval_function_group_args(definition, 0, 0, values, prefix)
                        && consumed == values.len()
                    {
                        suspended = Some(suspend_eval_function_suffix(
                            definition,
                            self.capture_slice(&captures),
                            locals,
                            prefix,
                            self.ctx,
                        ));
                        break;
                    }
                }
                *result = match suspended {
                    Some(value) => value,
                    None => {
                        if let Some(metrics) = self.ctx.metrics.as_deref() {
                            metrics.record_closure_build();
                            metrics.record_capture_vec(self.capture_slice(&captures).len());
                        }
                        let callee = Value::EvalClosure {
                            function: definition.clone(),
                            captured: self.capture_slice(&captures).to_vec().into(),
                        };
                        Value::Stuck(
                            EvalRef::new(callee),
                            self.values.take_args(args).into_vec().into(),
                        )
                    }
                };
                self.values.truncate(base);
                if let (Some(metrics), Some(start)) = (self.ctx.metrics.as_deref(), started) {
                    if let Some(label) = &definition.label {
                        metrics.record_function_apply(label, start.elapsed());
                    } else {
                        metrics.record_apply_fn(start.elapsed());
                    }
                }
                return Control::Return;
            }
        };
        let body = function.body();
        let has_memo = memo.is_some();
        if let Some(memo) = memo {
            self.memos.push(memo);
        }
        let owner = NonZeroUsize::new(self.records.len + 1).expect("record index fits storage");
        let end = self.values.len;
        let grows = self.records.push(CallRecord {
            function,
            captures,
            args,
            end,
            base,
            locals,
            memo: has_memo,
            started,
            generic_started,
            caller: self.current,
        });
        self.current = Some(owner);
        self.push_frame(Frame::CallReturn);
        if excess < end {
            self.push_frame(Frame::CallBody { excess });
        }
        self.observer.record_pushed(self.records.len, grows);
        self.observer.activation_pushed();
        Control::Eval(body)
    }

    fn finish_call(&mut self, value: &Value) {
        let index = self.current.expect("call return owns an activation").get() - 1;
        let activation = self.records.get_mut(index);
        if let (Some(metrics), Some(start)) = (
            self.ctx.metrics.as_deref(),
            activation.generic_started.take(),
        ) {
            metrics.record_closure_apply(start.elapsed());
        }
        activation.locals = None;
        self.values.truncate(activation.base);
        let elapsed = activation.started.take().map(|start| start.elapsed());
        let memo = activation
            .memo
            .then(|| self.memos.pop().expect("memo owner"));
        insert_exact_call_memo(self.ctx, memo, value.clone());
        if let (Some(metrics), Some(elapsed)) = (self.ctx.metrics.as_deref(), elapsed) {
            if let Some(label) = &activation.function.get().label {
                metrics.record_function_apply(label, elapsed);
            } else {
                metrics.record_apply_fn(elapsed);
            }
        }
        self.current = activation.caller;
        self.records.discard_top();
        self.observer.activation_returned();
    }

    fn apply(&mut self, base: usize, args: usize, result: &mut Value) -> Control<'code> {
        let callee = self.values.take(base);
        if let Value::EvalClosure { function, captured } = callee {
            let values = &self.values.as_slice()[args..];
            if values.len() == function.abi_arity()
                || (function.abi_arity() == 0 && matches!(values, [Value::Unit]))
                || can_bind_eval_function_args(&function, values)
            {
                if function.abi_arity() == 0 && matches!(values, [Value::Unit]) {
                    self.values.truncate(args);
                }
                return self.enter_function(
                    Function::Owned(function),
                    Captures::Shared(captured),
                    base,
                    args,
                    true,
                    result,
                );
            }
            if values.is_empty()
                && function
                    .value_groups
                    .first()
                    .is_none_or(|group| group.abi_arity != 0)
            {
                self.measure(Measure::Generic);
                self.values.truncate(base);
                *result = Value::EvalClosure { function, captured };
                return Control::Return;
            }
            self.measure(Measure::Generic);
            return self.enter_function(
                Function::Owned(function),
                Captures::Shared(captured),
                base,
                args,
                false,
                result,
            );
        }
        let args = self.values.take_args(args);
        self.values.truncate(base);
        self.measure(Measure::Generic);
        if let Value::Atom(name) = callee {
            return self.atom(AtomCallee::Owned(name), args.into_vec(), result);
        }
        if let Value::Primitive(builtin) = callee {
            return self.primitive(builtin, args.into_vec(), result);
        }
        if let Value::Stuck(callee, prefix) = &callee
            && args.len() != 0
            && !prefix.is_empty()
            && is_reflection_proof(&prefix[0])
            && let Value::Primitive(builtin) = callee.as_ref()
        {
            let mut combined = prefix.as_ref().clone();
            combined.extend(args.into_vec());
            return self.primitive(*builtin, combined, result);
        }
        if let Value::Data(primitive) = &callee
            && let Some(StructuralRecurDataIdentity::Primitive { site, stage }) =
                primitive.structural_recur_identity()
        {
            let args = args.into_vec();
            let (primitive, args) = if *stage == StructuralRecurPrimitiveStage::AwaitingProof {
                let Some((_, runtime)) = args.split_first() else {
                    *result = callee;
                    return Control::Return;
                };
                (
                    structural_recur_primitive_data(
                        site.clone(),
                        StructuralRecurPrimitiveStage::AwaitingRuntime,
                    ),
                    runtime,
                )
            } else {
                (primitive.clone(), args.as_slice())
            };
            if let Some([fuel, input, step]) = structural_recur_runtime_operands(args) {
                self.measure(Measure::Reflection(ComptimeBuiltin::StructuralRecur));
                return self.structural_at_site(site, fuel, input, step, result);
            }
            *result = if args.is_empty() {
                Value::Data(primitive)
            } else {
                Value::Stuck(EvalRef::new(Value::Data(primitive)), args.to_vec().into())
            };
            return Control::Return;
        }
        if let Value::StructuralRecur {
            root_measure,
            current_measure,
            step,
        } = callee
        {
            let (fuel, input) = match args.into_product_pair_or_vec() {
                Ok(pair) => pair,
                Err(args) => {
                    *result = structural_recur_fault(
                        Value::Stuck(
                            EvalRef::new(Value::StructuralRecur {
                                root_measure,
                                current_measure,
                                step,
                            }),
                            args.into(),
                        ),
                        self.ctx,
                    );
                    return Control::Return;
                }
            };
            let measure = fuel_measure(&fuel, self.ctx);
            if let Some(next) = measure
                && next < current_measure
                && next < root_measure
            {
                let recur = Value::StructuralRecur {
                    root_measure,
                    current_measure: next,
                    step: step.clone(),
                };
                return self.task(Task::Handler {
                    handler: Handler::Value(step.unwrap_or_clone()),
                    args: EvalArgs::Three([recur, fuel, input]),
                });
            }
            *result = structural_recur_fault(
                Value::Stuck(
                    EvalRef::new(Value::StructuralRecur {
                        root_measure,
                        current_measure,
                        step,
                    }),
                    vec![fuel, input].into(),
                ),
                self.ctx,
            );
            return Control::Return;
        }
        *result = match callee {
            Value::NewtypeMember(member)
                if args.len() > 0
                    || matches!(
                        member.kind,
                        NewtypeMemberKind::Constructor {
                            payload_abi_arity: 0
                        }
                    ) =>
            {
                apply_newtype_member(member, args.into_vec())
            }
            other if args.len() == 0 => other,
            other => Value::Stuck(EvalRef::new(other), args.into_vec().into()),
        };
        Control::Return
    }

    fn primitive(
        &mut self,
        builtin: ComptimeBuiltin,
        args: Vec<Value>,
        result: &mut Value,
    ) -> Control<'code> {
        self.reflect(builtin, args, true, result)
    }

    fn reflect(
        &mut self,
        builtin: ComptimeBuiltin,
        args: Vec<Value>,
        record_reflection: bool,
        result: &mut Value,
    ) -> Control<'code> {
        #[cfg(feature = "surface")]
        let args = {
            if args.iter().any(|arg| matches!(arg, Value::Projected(_)))
                && let ProjectedBuiltinClass::Helper(class) = projected_builtin_class(builtin)
            {
                let Some(proof) = args.first().and_then(marked_comptime_proof_from_value) else {
                    *result = Value::Stuck(EvalRef::new(Value::Primitive(builtin)), args.into());
                    return Control::Return;
                };
                if class
                    == crate::pass::typecheck_core::apply::fills::ProjectedHelperClass::GoalFreeOnly
                {
                    let Some(closed) = close_authenticated_projected_args(&args, proof, self.ctx)
                    else {
                        *result =
                            Value::Stuck(EvalRef::new(Value::Primitive(builtin)), args.into());
                        return Control::Return;
                    };
                    closed
                } else {
                    args
                }
            } else {
                args
            }
        };
        if matches!(
            builtin,
            ComptimeBuiltin::TypeArityFold | ComptimeBuiltin::TypeNameParamAritiesFold
        ) {
            let started = direct_reflection_start(self.ctx);
            let runtime = reflection_runtime_args(builtin, &args);
            let items = match (builtin, runtime) {
                (ComptimeBuiltin::TypeArityFold, [arity, _, _]) => {
                    as_refl_type_arity(arity).map(reflection::FoldItems::Count)
                }
                (
                    ComptimeBuiltin::TypeNameParamAritiesFold,
                    [Value::ReflTypeName { param_arities, .. }, _, _],
                ) => Some(reflection::FoldItems::Values(
                    param_arities
                        .iter()
                        .map(|arity| Value::ReflTypeArity(*arity))
                        .collect::<Vec<_>>()
                        .into_iter(),
                )),
                _ => None,
            };
            if let Some(items) = items {
                let [_, init, step] = runtime else {
                    unreachable!("validated arity fold")
                };
                let init = init.clone();
                let step = Handler::Value(step.clone());
                if record_reflection && let Some(started) = started {
                    self.push_frame(Frame::Measure {
                        kind: Measure::Reflection(builtin),
                        started,
                    });
                }
                return self.fold_items(items, init, step, result);
            }
            *result = if args.is_empty() && record_reflection {
                Value::Primitive(builtin)
            } else {
                Value::Stuck(EvalRef::new(Value::Primitive(builtin)), args.into())
            };
            return Control::Return;
        }
        if matches!(
            builtin,
            ComptimeBuiltin::IntrinsicEither | ComptimeBuiltin::IntrinsicIfThenElse
        ) {
            let source = callback::Source {
                builtin,
                args,
                code: None,
                started: direct_reflection_start(self.ctx),
                record: record_reflection,
            };
            return self.reflected_branch(source, result);
        }
        if matches!(
            builtin,
            ComptimeBuiltin::TermLet
                | ComptimeBuiltin::TermFn
                | ComptimeBuiltin::TermTypeFn
                | ComptimeBuiltin::TypeForall
        ) {
            let source = callback::Source {
                builtin,
                args,
                code: None,
                started: direct_reflection_start(self.ctx),
                record: record_reflection,
            };
            return self.binder_callback(source, result);
        }
        if matches!(
            builtin,
            ComptimeBuiltin::TypeProductSpineFold
                | ComptimeBuiltin::TypeSumSpineFold
                | ComptimeBuiltin::TypeArgsFold
                | ComptimeBuiltin::TypeFunctionParamsFold
        ) {
            let started = direct_reflection_start(self.ctx);
            if let Some(items) = reflection::primitive_type_fold_items(builtin, &args, self.ctx) {
                let [_, init, step] = reflection_runtime_args(builtin, &args) else {
                    unreachable!("validated fold operands")
                };
                let init = init.clone();
                let step = Handler::Value(step.clone());
                if record_reflection && let Some(started) = started {
                    self.push_frame(Frame::Measure {
                        kind: Measure::Reflection(builtin),
                        started,
                    });
                }
                return self.fold(items, init, step, result);
            }
            *result = if args.is_empty() {
                Value::Primitive(builtin)
            } else {
                Value::Stuck(EvalRef::new(Value::Primitive(builtin)), args.into())
            };
            return Control::Return;
        }
        if builtin == ComptimeBuiltin::StructuralRecur
            && let Some([fuel, input, step]) = public_structural_recur_runtime_operands(&args)
        {
            self.measure(Measure::Reflection(builtin));
            return self.structural_at_site(
                &self.ctx.public_structural_recur_site,
                fuel,
                input,
                step,
                result,
            );
        }
        let started = direct_reflection_start(self.ctx);
        if let Some(value) = reflect_leaf(builtin, &args, self.ctx) {
            if record_reflection
                && let Some(started) = started
                && let Some(metrics) = self.ctx.metrics.as_deref()
            {
                metrics.record_reflection_name(builtin.public_name(), started.elapsed());
            }
            *result = value;
        } else {
            *result = if args.is_empty() && record_reflection {
                Value::Primitive(builtin)
            } else {
                Value::Stuck(EvalRef::new(Value::Primitive(builtin)), args.into())
            };
        }
        Control::Return
    }

    fn structural_at_site(
        &mut self,
        site: &Arc<StructuralRecurSite>,
        fuel: Value,
        input: Value,
        step: Value,
        result: &mut Value,
    ) -> Control<'code> {
        match self.ctx.enter_structural_recur_site(site, &fuel) {
            Ok((measure, guard)) => {
                self.sites.push(guard);
                self.push_frame(Frame::StructuralSite);
                let recur = Value::StructuralRecur {
                    root_measure: measure,
                    current_measure: measure,
                    step: EvalRef::new(step.clone()),
                };
                self.task(Task::Handler {
                    handler: Handler::Value(step),
                    args: EvalArgs::Three([recur, fuel, input]),
                })
            }
            Err(StructuralRecurSiteEntryError::UnmeasurableInitialFuel) => {
                *result =
                    structural_recur_fault(structural_recur_initial_rejected_value(), self.ctx);
                Control::Return
            }
            Err(StructuralRecurSiteEntryError::NonDecreasingReentry {
                root_measure,
                current_measure,
                next_measure,
            }) => {
                *result = structural_recur_fault(
                    structural_recur_site_rejected_value(
                        root_measure,
                        current_measure,
                        next_measure,
                    ),
                    self.ctx,
                );
                Control::Return
            }
        }
    }

    fn measure(&mut self, kind: Measure) {
        if self.ctx.metrics.is_some() {
            self.push_frame(Frame::Measure {
                kind,
                started: Instant::now(),
            });
        }
    }

    fn atom(
        &mut self,
        atom: AtomCallee<'_>,
        mut args: Vec<Value>,
        result: &mut Value,
    ) -> Control<'code> {
        let name = atom.as_str();
        if let Some(arity) = intrinsic_elim_value_arity(name)
            && args.len() > arity
        {
            args = args.split_off(args.len() - arity);
        }
        if name == "__if_then_else__"
            && let [Value::Bool(left), _, _] = args.as_slice()
        {
            let left = *left;
            let right = args.pop().expect("false thunk");
            let on_left = args.pop().expect("true thunk");
            return self.task(Task::Handler {
                handler: Handler::Value(if left { on_left } else { right }),
                args: EvalArgs::Empty,
            });
        }
        if name == "__either__" && args.len() == 3 {
            let right = args.pop().expect("right handler");
            let left = args.pop().expect("left handler");
            let scrutinee = args.pop().expect("scrutinee");
            return self.task(Task::Either {
                scrutinee,
                left: Handler::Value(left),
                right: Handler::Value(right),
                thunks: false,
            });
        }
        *result = apply_atom_leaf(atom, args);
        Control::Return
    }

    fn push_args(&mut self, args: EvalArgs) {
        match args {
            EvalArgs::Empty => {}
            EvalArgs::One([one]) => self.push_value(one),
            EvalArgs::Two([one, two]) => {
                self.push_value(one);
                self.push_value(two);
            }
            EvalArgs::Three([one, two, three]) => {
                self.push_value(one);
                self.push_value(two);
                self.push_value(three);
            }
            EvalArgs::Many(values) => {
                for value in values {
                    self.push_value(value);
                }
            }
        }
    }

    fn task(&mut self, task: Task<'code>) -> Control<'code> {
        self.tasks.push(task);
        Control::Task
    }

    fn binder_callback(
        &mut self,
        source: callback::Source<'code>,
        result: &mut Value,
    ) -> Control<'code> {
        if let Some((finish, args)) = source.prepare(self.ctx) {
            let handler = source.handler(0);
            self.callbacks.push(callback::Callback { source, finish });
            self.push_frame(Frame::BinderCallback);
            self.task(Task::Handler { handler, args })
        } else {
            self.finish_callback(source, None, result)
        }
    }

    fn reflected_branch(
        &mut self,
        source: callback::Source<'code>,
        result: &mut Value,
    ) -> Control<'code> {
        if let Some((finish, args)) = branch_callback::prepare(&source, self.ctx) {
            let handler = source.handler(0);
            self.reflected_branches
                .push(branch_callback::Callback { source, finish });
            self.push_frame(Frame::ReflectedBranch);
            self.task(Task::Handler { handler, args })
        } else {
            self.finish_callback(source, None, result)
        }
    }

    fn finish_callback(
        &mut self,
        source: callback::Source<'code>,
        value: Option<Value>,
        result: &mut Value,
    ) -> Control<'code> {
        if let Some(value) = value {
            if source.record {
                record_direct_reflection_success(
                    source.builtin.public_name(),
                    self.ctx,
                    source.started,
                );
            }
            *result = value;
            return Control::Return;
        }
        if source.code.is_some() {
            let Handler::Expr(body) = source.handler(0) else {
                unreachable!("compiled callback")
            };
            let right = if matches!(
                source.builtin,
                ComptimeBuiltin::IntrinsicEither | ComptimeBuiltin::IntrinsicIfThenElse
            ) {
                let Handler::Expr(right) = source.handler(1) else {
                    unreachable!("compiled right callback")
                };
                Some(right)
            } else {
                None
            };
            let base = self.values.len;
            self.push_args(EvalArgs::Many(source.args));
            self.push_frame(Frame::DirectResidual {
                builtin: source.builtin,
                base,
                right,
            });
            Control::Eval(body)
        } else {
            *result = if source.args.is_empty() && source.record {
                Value::Primitive(source.builtin)
            } else {
                Value::Stuck(
                    EvalRef::new(Value::Primitive(source.builtin)),
                    source.args.into(),
                )
            };
            Control::Return
        }
    }

    fn fold(
        &mut self,
        items: Vec<Value>,
        init: Value,
        step: Handler<'code>,
        result: &mut Value,
    ) -> Control<'code> {
        self.fold_items(
            reflection::FoldItems::Values(items.into_iter()),
            init,
            step,
            result,
        )
    }

    fn fold_items(
        &mut self,
        items: reflection::FoldItems,
        init: Value,
        step: Handler<'code>,
        result: &mut Value,
    ) -> Control<'code> {
        let mut fold = reflection::Fold { step, items };
        if let Some(first) = fold.items.next() {
            let handler = fold.step.clone();
            self.folds.push(fold);
            self.push_frame(Frame::FoldStep);
            let args = match first {
                Some(item) => EvalArgs::Two([init, item]),
                None => EvalArgs::One([init]),
            };
            self.task(Task::Handler { handler, args })
        } else {
            *result = init;
            Control::Return
        }
    }

    fn direct(
        &mut self,
        code: Code<'code>,
        base: usize,
        next: usize,
        result: &mut Value,
    ) -> Control<'code> {
        if reflection::child(code.expr(), next).is_some() {
            let child = code.child(|expr| reflection::child(expr, next).expect("direct operand"));
            self.push_frame(Frame::DirectArguments {
                code,
                base,
                next: next + 1,
            });
            Control::Eval(child)
        } else {
            let args = self.values.take_args(base);
            let binder = match code.expr() {
                EvalExpr::ReflTermLet { .. } => Some(ComptimeBuiltin::TermLet),
                EvalExpr::ReflTermFn { .. } => Some(ComptimeBuiltin::TermFn),
                EvalExpr::ReflTermTypeFn { .. } => Some(ComptimeBuiltin::TermTypeFn),
                _ => None,
            };
            if let Some(builtin) = binder {
                let source = callback::Source {
                    builtin,
                    args: args.into_vec(),
                    code: Some(code),
                    started: direct_reflection_start(self.ctx),
                    record: true,
                };
                return self.binder_callback(source, result);
            }
            let branch = match code.expr() {
                EvalExpr::ReflIntrinsicEither { .. } => Some(ComptimeBuiltin::IntrinsicEither),
                EvalExpr::ReflIntrinsicIfThenElse { .. } => {
                    Some(ComptimeBuiltin::IntrinsicIfThenElse)
                }
                _ => None,
            };
            if let Some(builtin) = branch {
                let source = callback::Source {
                    builtin,
                    args: args.into_vec(),
                    code: Some(code),
                    started: direct_reflection_start(self.ctx),
                    record: true,
                };
                return self.reflected_branch(source, result);
            }
            if let EvalExpr::ReflTypeFold { name, .. } = code.expr() {
                let builtin =
                    ComptimeBuiltin::from_public_name(name).expect("compiled fold builtin");
                let EvalArgs::Two([typ, init]) = args else {
                    unreachable!("compiled fold header")
                };
                let started = direct_reflection_start(self.ctx);
                if let Some(items) = reflection::type_fold_items(builtin, &typ, self.ctx) {
                    let step = Handler::Expr(code.child(|expr| {
                        let EvalExpr::ReflTypeFold { step, .. } = expr else {
                            unreachable!()
                        };
                        step
                    }));
                    if let Some(started) = started {
                        self.push_frame(Frame::Measure {
                            kind: Measure::Reflection(builtin),
                            started,
                        });
                    }
                    return self.fold(items, init, step, result);
                }
                self.push_value(typ);
                self.push_value(init);
                self.push_frame(Frame::DirectResidual {
                    builtin,
                    base,
                    right: None,
                });
                Control::Eval(code.child(|expr| {
                    let EvalExpr::ReflTypeFold { step, .. } = expr else {
                        unreachable!()
                    };
                    step
                }))
            } else {
                *result = reflection::reduce_leaf(code.expr(), args, self.ctx);
                Control::Return
            }
        }
    }

    fn handler(
        &mut self,
        handler: Handler<'code>,
        args: EvalArgs,
        result: &mut Value,
    ) -> Control<'code> {
        let base = self.values.len;
        match handler {
            Handler::Value(value) => {
                self.push_value(value);
                self.push_args(args);
                Control::Apply {
                    base,
                    args: base + 1,
                }
            }
            Handler::Expr(code) => {
                let slots = match code.expr() {
                    EvalExpr::FnRef { .. } => Some(&[][..]),
                    EvalExpr::FnExpr { captured_slots, .. } => Some(captured_slots.as_ref()),
                    _ => None,
                };
                if let Some(slots) = slots {
                    let function = code.function();
                    let definition = function.get();
                    if definition.params_are_abi_identity() && args.len() == definition.abi_arity()
                    {
                        let locals = EvalSlots::closure_borrowed(
                            self.current_captures(),
                            self.current_locals(),
                        );
                        if let Some(value) = try_apply_simple_compiled_handler(
                            definition,
                            slots,
                            args.as_slice(),
                            &locals,
                        ) {
                            *result = value;
                            return Control::Return;
                        }
                    }
                    let captures = self.captures(slots);
                    self.push_args(args);
                    return self.enter_function(function, captures, base, base, true, result);
                }
                self.push_value(Value::Unit);
                self.push_args(args);
                self.push_frame(Frame::HandlerReturn {
                    base,
                    args: base + 1,
                });
                Control::Eval(code)
            }
        }
    }

    fn either(
        &mut self,
        scrutinee: Value,
        left: Handler<'code>,
        right: Handler<'code>,
        thunks: bool,
        result: &mut Value,
    ) -> Control<'code> {
        if let Value::Data(data) = &scrutinee
            && matches!(data.ctor(), "__left__" | "__right__")
            && (thunks || !data.args().is_empty())
        {
            let args = if thunks {
                EvalArgs::Empty
            } else {
                EvalArgs::One([data.args().last().expect("injection payload").clone()])
            };
            let handler = if data.ctor() == "__left__" {
                left
            } else {
                right
            };
            return self.task(Task::Handler { handler, args });
        }
        match scrutinee {
            Value::CaseSplit {
                scrutinee,
                left_payload,
                left: inner_left,
                right_payload,
                right: inner_right,
            } => {
                self.branches.push(Branch {
                    scrutinee,
                    left_payload,
                    right_payload,
                    next: Some(Task::Either {
                        scrutinee: inner_right.unwrap_or_clone(),
                        left: left.clone(),
                        right: right.clone(),
                        thunks,
                    }),
                    left: None,
                });
                self.push_frame(Frame::BranchLeft);
                self.task(Task::Either {
                    scrutinee: inner_left.unwrap_or_clone(),
                    left,
                    right,
                    thunks,
                })
            }
            scrutinee @ (Value::Atom(_) | Value::Stuck(..)) => {
                let left_payload = fresh_case_split_payload("l");
                let right_payload = fresh_case_split_payload("r");
                let left_args = if thunks {
                    EvalArgs::Empty
                } else {
                    EvalArgs::One([Value::Atom(left_payload.clone())])
                };
                let right_args = if thunks {
                    EvalArgs::Empty
                } else {
                    EvalArgs::One([Value::Atom(right_payload.clone())])
                };
                self.branches.push(Branch {
                    scrutinee: EvalRef::new(scrutinee),
                    left_payload,
                    right_payload,
                    next: Some(Task::Handler {
                        handler: right,
                        args: right_args,
                    }),
                    left: None,
                });
                self.push_frame(Frame::BranchLeft);
                self.task(Task::Handler {
                    handler: left,
                    args: left_args,
                })
            }
            scrutinee => {
                match (left, right) {
                    (Handler::Value(left), Handler::Value(right)) => {
                        *result = Value::Stuck(
                            EvalRef::new(Value::Atom("__either__".into())),
                            vec![scrutinee, left, right].into(),
                        );
                        Control::Return
                    }
                    (Handler::Expr(left), Handler::Expr(right)) => {
                        let base = self.values.len;
                        self.push_value(scrutinee);
                        // Residual handlers are evaluated as expressions, not forced.
                        self.push_frame(Frame::ResidualLeft {
                            right,
                            base,
                            name: "__either__",
                        });
                        Control::Eval(left)
                    }
                    _ => {
                        unreachable!("a branch pair shares its expression or value representation")
                    }
                }
            }
        }
    }

    fn run(mut self, mut control: Control<'code>) -> Value {
        self.observer.machine_started();
        let mut result = Value::Unit;
        loop {
            control = match control {
                Control::Compare => {
                    match self
                        .comparisons
                        .last_mut()
                        .expect("comparison owner")
                        .advance(self.ctx)
                    {
                        comparison::Outcome::Apply(callee, args) => {
                            self.push_frame(Frame::ComparisonApply);
                            self.task(Task::Handler {
                                handler: Handler::Value(callee),
                                args: args.into_eval(),
                            })
                        }
                        comparison::Outcome::Done(value) => {
                            self.comparisons.pop();
                            result = value;
                            Control::Return
                        }
                    }
                }
                Control::Task => match self.tasks.pop().expect("scheduled operation") {
                    Task::Handler { handler, args } => self.handler(handler, args, &mut result),
                    Task::Either {
                        scrutinee,
                        left,
                        right,
                        thunks,
                    } => self.either(scrutinee, left, right, thunks, &mut result),
                },
                Control::Apply { base, args } => self.apply(base, args, &mut result),
                Control::Eval(code) => {
                    let expr = code.expr();
                    if reflection::is_direct(expr) {
                        if let Some(metrics) = self.ctx.metrics.as_deref() {
                            metrics.record_expr_visit();
                        }
                        let base = self.values.len;
                        control = self.direct(code, base, 0, &mut result);
                        continue;
                    }
                    if is_terminal(expr) {
                        result = terminal(expr, |slot| self.local(slot).cloned());
                        if let Some(metrics) = self.ctx.metrics.as_deref() {
                            metrics.record_expr_visit();
                        }
                        Control::Return
                    } else {
                        let handled = matches!(
                            expr,
                            EvalExpr::ProofedReflection { .. }
                                | EvalExpr::Let { .. }
                                | EvalExpr::Seq { .. }
                                | EvalExpr::Call { .. }
                                | EvalExpr::CallAtom { .. }
                                | EvalExpr::CallPrimitive { .. }
                                | EvalExpr::CallFn { .. }
                                | EvalExpr::CallFnExpr { .. }
                                | EvalExpr::CoreCtor { .. }
                                | EvalExpr::NewtypeCtor { .. }
                                | EvalExpr::NewtypeProj { .. }
                                | EvalExpr::CoreProj { .. }
                                | EvalExpr::FnRef { .. }
                                | EvalExpr::FnExpr { .. }
                                | EvalExpr::CoreIf { .. }
                                | EvalExpr::CoreEither { .. }
                                | EvalExpr::CoreEitherThunk { .. }
                        );
                        if handled && let Some(metrics) = self.ctx.metrics.as_deref() {
                            metrics.record_expr_visit();
                        }
                        match expr {
                            EvalExpr::ProofedReflection { .. } => {
                                let direct = code.child(|expr| {
                                    let EvalExpr::ProofedReflection { direct, .. } = expr else {
                                        unreachable!()
                                    };
                                    direct
                                });
                                self.push_frame(Frame::Proofed(code));
                                Control::Eval(direct)
                            }
                            EvalExpr::FnRef { .. } | EvalExpr::FnExpr { .. } => {
                                result = self.closure(expr);
                                Control::Return
                            }
                            EvalExpr::Let { .. } => {
                                let value = code.child(|expr| {
                                    let EvalExpr::Let { value, .. } = expr else {
                                        unreachable!()
                                    };
                                    value
                                });
                                let body = code.child(|expr| {
                                    let EvalExpr::Let { body, .. } = expr else {
                                        unreachable!()
                                    };
                                    body
                                });
                                self.push_frame(Frame::LetValue(body));
                                Control::Eval(value)
                            }
                            EvalExpr::Seq { .. } => {
                                let value = code.child(|expr| {
                                    let EvalExpr::Seq { value, .. } = expr else {
                                        unreachable!()
                                    };
                                    value
                                });
                                let body = code.child(|expr| {
                                    let EvalExpr::Seq { body, .. } = expr else {
                                        unreachable!()
                                    };
                                    body
                                });
                                self.push_frame(Frame::SeqValue(body));
                                Control::Eval(value)
                            }
                            EvalExpr::Call { .. } => {
                                let callee = code.child(|expr| {
                                    let EvalExpr::Call { callee, .. } = expr else {
                                        unreachable!()
                                    };
                                    callee
                                });
                                self.push_frame(Frame::Callee {
                                    call: code,
                                    base: self.values.len,
                                });
                                Control::Eval(callee)
                            }
                            EvalExpr::CallFn { .. }
                            | EvalExpr::CallFnExpr { .. }
                            | EvalExpr::CallAtom { .. }
                            | EvalExpr::CallPrimitive { .. }
                            | EvalExpr::CoreCtor { .. } => {
                                let base = self.values.len;
                                self.start_arguments(code, base, &mut result)
                            }
                            EvalExpr::NewtypeCtor { .. }
                            | EvalExpr::NewtypeProj { .. }
                            | EvalExpr::CoreProj { .. } => {
                                let child = code.child(|expr| match expr {
                                    EvalExpr::NewtypeCtor { value, .. }
                                    | EvalExpr::NewtypeProj { value, .. } => value,
                                    EvalExpr::CoreProj { pair, .. } => pair,
                                    _ => unreachable!(),
                                });
                                self.push_frame(Frame::Unary(code));
                                Control::Eval(child)
                            }
                            EvalExpr::CoreIf { .. } => {
                                let condition = code.child(|expr| {
                                    let EvalExpr::CoreIf { condition, .. } = expr else {
                                        unreachable!()
                                    };
                                    condition
                                });
                                self.push_frame(Frame::Condition(code));
                                Control::Eval(condition)
                            }
                            EvalExpr::CoreEither { .. } | EvalExpr::CoreEitherThunk { .. } => {
                                let scrutinee = code.child(|expr| match expr {
                                    EvalExpr::CoreEither { scrutinee, .. }
                                    | EvalExpr::CoreEitherThunk { scrutinee, .. } => scrutinee,
                                    _ => unreachable!(),
                                });
                                self.push_frame(Frame::Scrutinee(code));
                                Control::Eval(scrutinee)
                            }
                            _ => unreachable!(
                                "leaf expressions are reduced before operation dispatch"
                            ),
                        }
                    }
                }
                Control::Return => match self.frames.pop().map(|pending| pending.frame) {
                    Some(Frame::ComparisonApply) => {
                        self.comparisons
                            .last_mut()
                            .expect("comparison owner")
                            .resume(std::mem::replace(&mut result, Value::Unit));
                        Control::Compare
                    }
                    Some(Frame::ReflectedBranch) => {
                        let callback = self
                            .reflected_branches
                            .pop()
                            .expect("reflected branch owner");
                        let body = std::mem::replace(&mut result, Value::Unit);
                        match callback.finish.resume(&callback.source, body, self.ctx) {
                            branch_callback::Step::Next(finish, payload) => {
                                let args = match payload {
                                    Some(value) => EvalArgs::One([value]),
                                    None => EvalArgs::Empty,
                                };
                                let handler = callback.source.handler(1);
                                self.reflected_branches.push(branch_callback::Callback {
                                    source: callback.source,
                                    finish,
                                });
                                self.push_frame(Frame::ReflectedBranch);
                                self.task(Task::Handler { handler, args })
                            }
                            branch_callback::Step::Done(value) => {
                                self.finish_callback(callback.source, value, &mut result)
                            }
                        }
                    }
                    Some(Frame::BinderCallback) => {
                        let callback = self.callbacks.pop().expect("binder callback owner");
                        let body = std::mem::replace(&mut result, Value::Unit);
                        let value = callback.finish.finish(&callback.source, body, self.ctx);
                        self.finish_callback(callback.source, value, &mut result)
                    }
                    Some(Frame::Proofed(code)) => {
                        #[cfg(feature = "surface")]
                        {
                            let EvalExpr::ProofedReflection { builtin, .. } = code.expr() else {
                                unreachable!("proofed reflection")
                            };
                            let retry = matches!(&result, Value::Stuck(callee, args)
                                if matches!(callee.as_ref(), Value::Primitive(stuck) if stuck == builtin)
                                && (*builtin == ComptimeBuiltin::TermTypeFn || args.iter().any(|arg| matches!(arg, Value::Projected(_)))));
                            if retry {
                                let Value::Stuck(_, args) =
                                    std::mem::replace(&mut result, Value::Unit)
                                else {
                                    unreachable!("retry residual")
                                };
                                let base = self.values.len;
                                self.push_value(Value::Unit);
                                self.push_args(EvalArgs::Many(args.unwrap_or_clone()));
                                self.push_frame(Frame::ProofRetry {
                                    builtin: *builtin,
                                    base,
                                });
                                Control::Eval(code.child(|expr| {
                                    let EvalExpr::ProofedReflection { proof, .. } = expr else {
                                        unreachable!()
                                    };
                                    proof
                                }))
                            } else {
                                Control::Return
                            }
                        }
                        #[cfg(not(feature = "surface"))]
                        {
                            let _ = code;
                            Control::Return
                        }
                    }
                    #[cfg(feature = "surface")]
                    Some(Frame::ProofRetry { builtin, base }) => {
                        self.values.as_mut_slice()[base] =
                            std::mem::replace(&mut result, Value::Unit);
                        let args = self.values.take_args(base).into_vec();
                        self.reflect(builtin, args, false, &mut result)
                    }
                    None => return result,
                    Some(Frame::DirectArguments { code, base, next }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        self.direct(code, base, next, &mut result)
                    }
                    Some(Frame::DirectResidual {
                        builtin,
                        base,
                        right,
                    }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        if let Some(right) = right {
                            self.push_frame(Frame::DirectResidual {
                                builtin,
                                base,
                                right: None,
                            });
                            Control::Eval(right)
                        } else {
                            result = Value::Stuck(
                                EvalRef::new(Value::Primitive(builtin)),
                                self.values.take_args(base).into_vec().into(),
                            );
                            Control::Return
                        }
                    }
                    Some(Frame::FoldStep) => {
                        let mut fold = self.folds.pop().expect("fold callback owner");
                        if let Some(item) = fold.items.next() {
                            let handler = fold.step.clone();
                            self.folds.push(fold);
                            self.push_frame(Frame::FoldStep);
                            let acc = std::mem::replace(&mut result, Value::Unit);
                            let args = match item {
                                Some(item) => EvalArgs::Two([acc, item]),
                                None => EvalArgs::One([acc]),
                            };
                            self.task(Task::Handler { handler, args })
                        } else {
                            Control::Return
                        }
                    }
                    Some(Frame::LetValue(body)) => {
                        self.push_local(std::mem::replace(&mut result, Value::Unit));
                        self.push_frame(Frame::LetBody);
                        Control::Eval(body)
                    }
                    Some(Frame::LetBody) => {
                        self.pop_local();
                        Control::Return
                    }
                    Some(Frame::SeqValue(body)) => {
                        if !matches!(result, Value::Unit) {
                            self.push_value(std::mem::replace(&mut result, Value::Unit));
                            self.push_frame(Frame::SeqBody);
                        }
                        Control::Eval(body)
                    }
                    Some(Frame::SeqBody) => {
                        result = Value::Seq {
                            value: EvalRef::new(self.values.pop().expect("sequence owns a value")),
                            body: EvalRef::new(result),
                        };
                        Control::Return
                    }
                    Some(Frame::Callee { call, base }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        self.start_arguments(call, base, &mut result)
                    }
                    Some(Frame::Arguments { call, base, next }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        if next < Self::call_args(call.expr()).len() {
                            let arg = call.child(|expr| &Self::call_args(expr)[next]);
                            self.push_frame(Frame::Arguments {
                                call,
                                base,
                                next: next + 1,
                            });
                            Control::Eval(arg)
                        } else {
                            self.complete_arguments(call, base, &mut result)
                        }
                    }
                    Some(Frame::Unary(code)) => {
                        result = match code.expr() {
                            EvalExpr::NewtypeCtor { member, .. } => {
                                newtype_data_value(member, vec![result])
                            }
                            EvalExpr::NewtypeProj { member, .. } => {
                                if let Value::Data(data) = &result
                                    && data.has_newtype_constructor(&member.constructor)
                                    && let Some(payload) = data.args().last()
                                {
                                    payload.clone()
                                } else {
                                    Value::Stuck(
                                        EvalRef::new(Value::NewtypeMember(member.clone())),
                                        vec![result].into(),
                                    )
                                }
                            }
                            EvalExpr::CoreProj { name, .. } => project_product_value(name, &result),
                            _ => unreachable!("unary continuation retains its operation"),
                        };
                        Control::Return
                    }
                    Some(Frame::Condition(code)) => {
                        if let Value::Bool(left) = result {
                            self.task(Task::Handler {
                                handler: Handler::Expr(code.branch(left)),
                                args: EvalArgs::Empty,
                            })
                        } else {
                            let base = self.values.len;
                            self.push_value(std::mem::replace(&mut result, Value::Unit));
                            self.push_frame(Frame::ResidualLeft {
                                right: code.branch(false),
                                base,
                                name: "__if_then_else__",
                            });
                            Control::Eval(code.branch(true))
                        }
                    }
                    Some(Frame::ResidualLeft { right, base, name }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        self.push_frame(Frame::ResidualRight { base, name });
                        Control::Eval(right)
                    }
                    Some(Frame::ResidualRight { base, name }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        result = Value::Stuck(
                            EvalRef::new(Value::Atom(name.into())),
                            self.values.take_args(base).into_vec().into(),
                        );
                        Control::Return
                    }
                    Some(Frame::Scrutinee(code)) => {
                        let thunks = matches!(code.expr(), EvalExpr::CoreEitherThunk { .. });
                        let known = matches!(&result, Value::Data(data) if matches!(data.ctor(), "__left__" | "__right__") && !data.args().is_empty());
                        if thunks || known {
                            let scrutinee = std::mem::replace(&mut result, Value::Unit);
                            self.task(Task::Either {
                                scrutinee,
                                left: Handler::Expr(code.branch(true)),
                                right: Handler::Expr(code.branch(false)),
                                thunks,
                            })
                        } else {
                            let base = self.values.len;
                            self.push_value(std::mem::replace(&mut result, Value::Unit));
                            let left = code.branch(true);
                            self.push_frame(Frame::EitherLeft { code, base });
                            Control::Eval(left)
                        }
                    }
                    Some(Frame::EitherLeft { code, base }) => {
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        self.push_frame(Frame::EitherRight { base });
                        Control::Eval(code.branch(false))
                    }
                    Some(Frame::EitherRight { base }) => {
                        let EvalArgs::Two([scrutinee, left]) = self.values.take_args(base) else {
                            unreachable!("scrutinee and left handler")
                        };
                        let right = std::mem::replace(&mut result, Value::Unit);
                        self.task(Task::Either {
                            scrutinee,
                            left: Handler::Value(left),
                            right: Handler::Value(right),
                            thunks: false,
                        })
                    }
                    Some(Frame::HandlerReturn { base, args }) => {
                        self.values.as_mut_slice()[base] =
                            std::mem::replace(&mut result, Value::Unit);
                        Control::Apply { base, args }
                    }
                    Some(Frame::BranchLeft) => {
                        let branch = self.branches.last_mut().expect("left branch owner");
                        branch.left = Some(std::mem::replace(&mut result, Value::Unit));
                        let next = branch.next.take().expect("right branch continuation");
                        self.push_frame(Frame::BranchRight);
                        self.task(next)
                    }
                    Some(Frame::BranchRight) => {
                        let branch = self.branches.pop().expect("right branch owner");
                        result = Value::CaseSplit {
                            scrutinee: branch.scrutinee,
                            left_payload: branch.left_payload,
                            left: EvalRef::new(branch.left.expect("completed left branch")),
                            right_payload: branch.right_payload,
                            right: EvalRef::new(result),
                        };
                        Control::Return
                    }
                    Some(Frame::Measure { kind, started }) => {
                        let metrics = self.ctx.metrics.as_deref().expect("measurement is enabled");
                        match kind {
                            Measure::Atom => metrics.record_apply_atom(started.elapsed()),
                            Measure::Generic => metrics.record_apply_generic(started.elapsed()),
                            Measure::Reflection(builtin) => metrics
                                .record_reflection_name(builtin.public_name(), started.elapsed()),
                        }
                        Control::Return
                    }
                    Some(Frame::StructuralSite) => {
                        self.sites.pop();
                        Control::Return
                    }
                    Some(Frame::CallBody { excess }) => {
                        let base = self.values.len;
                        self.push_value(std::mem::replace(&mut result, Value::Unit));
                        let end = self
                            .records
                            .get(self.current.expect("call body owner").get() - 1)
                            .end;
                        for index in excess..end {
                            let arg = self.values.as_slice()[index].clone();
                            self.push_value(arg);
                        }
                        Control::Apply {
                            base,
                            args: base + 1,
                        }
                    }
                    Some(Frame::CallReturn) => {
                        self.finish_call(&result);
                        Control::Return
                    }
                },
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_eval_function;
    use super::super::{DataValue, EvalMetrics, EvalParamGroup, apply};
    use super::*;
    use std::sync::{Arc, Mutex};

    fn tracked(name: &str) -> (Value, super::super::ownership::EvalWeak<DataValue>) {
        let data = EvalRef::new(DataValue::new(name, Vec::new()));
        let weak = data.downgrade();
        (Value::Data(data), weak)
    }

    #[test]
    fn indexed_values_keep_ranges_contiguous_and_release_in_reverse_order() {
        for depth in [0, 1, 2, 3, 4, 65] {
            let mut values = Values::new();
            let mut weak = Vec::new();
            for _ in 0..depth {
                let (value, owner) = tracked("argument");
                values.push(value);
                weak.push(owner);
            }
            assert_eq!(values.as_slice().len(), depth);
            let capacity = values.heap.capacity();
            for index in (0..depth).rev() {
                drop(values.pop());
                assert!(weak[..index].iter().all(|owner| owner.strong_count() == 1));
                assert!(weak[index..].iter().all(|owner| owner.strong_count() == 0));
            }
            values.push(Value::Bool(true));
            assert!(matches!(values.as_slice(), [Value::Bool(true)]));
            assert_eq!(values.heap.capacity(), capacity);
        }
    }

    fn generic_chain(depth: usize) -> EvalExpr {
        let template = test_eval_function(&["arg"], EvalExpr::Local(0));
        let mut function = template.clone();
        for index in 0..depth {
            function = EvalRef::new(EvalFunction {
                cache_key: eval_function_cache_key_for_suffix(&template.cache_key, index),
                body: EvalRef::new(EvalExpr::Call {
                    callee: EvalRef::new(EvalExpr::Const(Value::EvalClosure {
                        function,
                        captured: Vec::new().into(),
                    })),
                    args: vec![EvalRef::new(EvalExpr::Local(0))],
                }),
                simple_body: false,
                memo_candidate: false,
                ..template.as_ref().clone()
            });
        }
        EvalExpr::Call {
            callee: EvalRef::new(EvalExpr::Const(Value::EvalClosure {
                function,
                captured: Vec::new().into(),
            })),
            args: vec![EvalRef::new(EvalExpr::Bool(true))],
        }
    }

    #[test]
    fn generic_calls_and_owned_code_share_the_execution_stack() {
        let expr = generic_chain(20_000);
        let mut storage = StorageMetrics::default();
        let value = eval_with_storage(
            &expr,
            &mut EvalSlots::root(Vec::new()),
            &EvalCtx::new(),
            &mut storage,
        );
        assert!(matches!(value, Value::Bool(true)));
        assert_eq!(storage.activation_peak, 20_001);
        assert_eq!(storage.record_peak, 20_001);
        assert_eq!(storage.local_promotions, 0);
    }

    struct DeepCallFault(usize);

    fn callback_comparison_chain(depth: usize) -> Value {
        let outer_template = test_eval_function(&["probe"], EvalExpr::Unit);
        let step_template = test_eval_function(&["acc", "typ"], EvalExpr::Unit);
        let typ = EvalRef::new(EvalExpr::Const(refl_type_value(Type::Bottom {
            meta: zero_meta(),
        })));
        let mut value = Value::Bool(true);
        for index in 0..depth {
            let step = EvalRef::new(EvalFunction {
                cache_key: eval_function_cache_key_for_suffix(&step_template.cache_key, index),
                body: EvalRef::new(EvalExpr::Let {
                    value: EvalRef::new(EvalExpr::Unit),
                    body: EvalRef::new(EvalExpr::CoreCtor {
                        name: "__left__",
                        args: vec![EvalRef::new(EvalExpr::Const(value))],
                    }),
                }),
                simple_body: false,
                memo_candidate: false,
                ..step_template.as_ref().clone()
            });
            let function = EvalRef::new(EvalFunction {
                cache_key: eval_function_cache_key_for_suffix(&outer_template.cache_key, index),
                body: EvalRef::new(EvalExpr::ReflTypeFold {
                    name: "__type_product_spine_fold__",
                    typ: typ.clone(),
                    init: EvalRef::new(EvalExpr::Unit),
                    step: EvalRef::new(EvalExpr::FnRef {
                        cache_key: step.cache_key.clone(),
                        function: step,
                        share_closure: false,
                    }),
                }),
                simple_body: false,
                memo_candidate: false,
                ..outer_template.as_ref().clone()
            });
            value = Value::EvalClosure {
                function,
                captured: Vec::new().into(),
            };
        }
        value
    }

    #[test]
    fn mixed_reflection_callbacks_and_comparison_probes_use_the_default_stack() {
        let depth = 5_000;
        let value = callback_comparison_chain(depth);
        let metrics = Arc::new(EvalMetrics::default());
        let ctx = EvalCtx::new().with_metrics(metrics.clone());
        assert!(nf_eq(&value, &value, &ctx));
        assert_eq!(metrics.snapshot().apply_fn_calls, depth * 4);
    }

    #[test]
    fn comparison_callback_unwind_releases_names_and_reuses_context() {
        let value = callback_comparison_chain(2_000);
        let ctx = EvalCtx::new();
        let mut root = EvalSlots::root(Vec::new());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut machine = Machine::new(&mut root, &ctx, DeepCallFault(3_000));
                machine
                    .comparisons
                    .push(comparison::Engine::equal(value.clone(), value.clone()));
                machine.run(Control::Compare)
            }))
            .is_err()
        );
        assert!(nf_eq(&Value::Bool(true), &Value::Bool(true), &ctx));
        assert!(root.locals_slice().is_empty());
    }

    #[test]
    fn reflection_created_checked_terms_compare_on_the_default_stack() {
        let depth = 10_000;
        let body = EvalExpr::FnExpr {
            function: test_eval_function(&["binder"], EvalExpr::Local(0)),
            captured_slots: vec![0].into(),
            empty_closure_cache: Arc::new(OnceLock::new()),
        };
        let step = test_eval_function(
            &["acc"],
            EvalExpr::ReflTermLet {
                value_type: EvalRef::new(EvalExpr::ReflTypeUnit),
                value: EvalRef::new(EvalExpr::ReflTermUnit),
                body: EvalRef::new(body),
            },
        );
        let expr = EvalExpr::CallPrimitive {
            builtin: ComptimeBuiltin::TypeArityFold,
            args: vec![
                EvalRef::new(EvalExpr::Const(Value::ReflTypeArity(depth))),
                EvalRef::new(EvalExpr::ReflTermUnit),
                EvalRef::new(EvalExpr::FnRef {
                    cache_key: step.cache_key.clone(),
                    function: step,
                    share_closure: false,
                }),
            ],
        };
        let ctx = EvalCtx::new();
        let value = eval(&expr, &mut EvalSlots::root(Vec::new()), &ctx);
        assert!(matches!(value, Value::CheckedTerm(_)));
        eprintln!("constructed {depth} reflected lets before comparison");
        assert!(nf_eq(&value, &value, &ctx));
        let Value::CheckedTerm(term) = &value else {
            unreachable!()
        };
        eprintln!("compared {depth} reflected lets before public result decoding");
        assert_eq!(term.checked_error(), None);
        let canonical = term.canonicalize_generated_names();
        assert_eq!(canonical.checked_error(), None);
        assert!(canonical.serialization_type_binders().is_empty());
        eprintln!("decoded {depth} reflected lets before template replay");
        let mut replayed = canonical.instantiate_template_pre_prime(
            &[],
            zero_span(),
            &mut |expr, _| crate::ast::convert_expr::<EvalPhase, PrePrime>(expr),
            &mut crate::ast::convert_type::<EvalPhase, PrePrime>,
        );
        eprintln!("replayed {depth} reflected lets before inspecting the result");
        let mut actual = 0;
        while let Expr::Let { value, body, .. } = replayed {
            assert!(matches!(*value, Expr::Unit { .. }));
            actual += 1;
            replayed = *body;
        }
        assert_eq!(actual, depth);
        assert!(matches!(replayed, Expr::Unit { .. }));
    }

    impl StorageObserver for DeepCallFault {
        fn activation_pushed(&mut self) {
            self.0 -= 1;
            assert_ne!(self.0, 0, "selected deep active call boundary");
        }
    }

    #[test]
    fn deep_active_call_unwind_restores_root_slots_and_reuses_context() {
        let expr = EvalExpr::Let {
            value: EvalRef::new(EvalExpr::Bool(false)),
            body: EvalRef::new(generic_chain(10_000)),
        };
        let mut root = EvalSlots::root(vec![Value::Bool(true)]);
        let ctx = EvalCtx::new();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Machine::new(&mut root, &ctx, DeepCallFault(9_000))
                    .run(Control::Eval(Code::Borrowed(&expr)))
            }))
            .is_err()
        );
        assert!(matches!(root.locals_slice(), [Value::Bool(true)]));
        assert!(matches!(
            eval(&EvalExpr::Local(0), &mut root, &ctx),
            Value::Bool(true)
        ));
    }

    #[test]
    fn pending_cleanup_preserves_mixed_owners_and_resumes() {
        for unwind in [false, true] {
            let mut root = EvalSlots::root(Vec::new());
            let ctx = EvalCtx::new();
            let mut machine = Machine::new(&mut root, &ctx, ());
            let mut weak = Vec::new();
            for _ in 0..9 {
                let (value, owner) = tracked("residual");
                machine.push_value(value);
                machine.push_frame(Frame::SeqBody);
                weak.push(owner);
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut cleanup = PendingCleanup {
                    machine: &mut machine,
                };
                for index in (0..weak.len()).rev() {
                    assert!(cleanup.machine.cleanup_step());
                    assert!(weak[..index].iter().all(|owner| owner.strong_count() == 1));
                    assert!(weak[index..].iter().all(|owner| owner.strong_count() == 0));
                    if unwind {
                        panic!("selected committed cleanup boundary");
                    }
                }
                cleanup.drain();
            }));
            assert_eq!(result.is_err(), unwind);
            assert!(weak.iter().all(|owner| owner.strong_count() == 0));
            assert_eq!(machine.values.len, 0);
            assert_eq!(machine.frames.len, 0);
        }
    }

    struct ArgumentFault<'a> {
        owners: &'a [super::super::ownership::EvalWeak<DataValue>],
        fired: &'a mut bool,
    }

    impl StorageObserver for ArgumentFault<'_> {
        fn frame_pushed(&mut self, len: usize, _grows: bool) {
            if len == 4 {
                assert!(self.owners.iter().all(|owner| owner.strong_count() == 2));
                *self.fired = true;
                panic!("selected pending argument boundary");
            }
        }
    }

    #[test]
    fn pending_argument_fault_releases_real_evaluator_owners() {
        let mut owners = Vec::new();
        let locals = (0..3)
            .map(|_| {
                let (value, owner) = tracked("root");
                owners.push(owner);
                value
            })
            .collect();
        let mut slots = EvalSlots::root(locals);
        let expr = EvalExpr::Seq {
            value: EvalRef::new(EvalExpr::Local(0)),
            body: EvalRef::new(EvalExpr::CallFn {
                function: test_eval_function(&["first", "second"], EvalExpr::Local(0)),
                args: vec![
                    EvalRef::new(EvalExpr::Local(1)),
                    EvalRef::new(EvalExpr::Seq {
                        value: EvalRef::new(EvalExpr::Local(2)),
                        body: EvalRef::new(EvalExpr::Let {
                            value: EvalRef::new(EvalExpr::Unit),
                            body: EvalRef::new(EvalExpr::Unit),
                        }),
                    }),
                ],
            }),
        };
        let metrics = Arc::new(EvalMetrics::default());
        let ctx = EvalCtx::new().with_metrics(metrics.clone());
        let mut fired = false;
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Machine::new(
                    &mut slots,
                    &ctx,
                    ArgumentFault {
                        owners: &owners,
                        fired: &mut fired,
                    },
                )
                .run(Control::Eval(Code::Borrowed(&expr)))
            }))
            .is_err()
        );
        assert!(fired);
        assert_eq!(metrics.snapshot().apply_fn_calls, 0);
        assert_eq!(slots.locals_slice().len(), 3);
        assert!(owners.iter().all(|owner| owner.strong_count() == 1));
        drop(slots);
        assert!(owners.iter().all(|owner| owner.strong_count() == 0));
    }

    #[test]
    fn lazy_records_follow_live_prefixes_and_preserve_spilled_captures() {
        let identity = test_eval_function(&["arg"], EvalExpr::Local(0));
        for depth in [128, 1024] {
            let mut retained = vec![EvalRef::new(EvalExpr::Bool(true))];
            for _ in 0..depth {
                retained.push(EvalRef::new(EvalExpr::CallFn {
                    function: identity.clone(),
                    args: vec![retained.last().expect("initial value").clone()],
                }));
            }
            let mut storage = StorageMetrics::default();
            let value = eval_with_storage(
                retained.last().expect("outer call"),
                &mut EvalSlots::root(Vec::new()),
                &EvalCtx::new(),
                &mut storage,
            );
            assert!(matches!(value, Value::Bool(true)));
            assert_eq!(storage.frame_peak, depth, "{storage:?}");
            assert_eq!(storage.record_pushes, depth, "{storage:?}");
            assert_eq!(storage.record_peak, 1, "{storage:?}");
            assert_eq!(storage.record_growths, 0, "{storage:?}");
            assert_eq!(storage.activation_pushes, depth, "{storage:?}");
            assert_eq!(storage.activation_peak, 1, "{storage:?}");
            assert_eq!(storage.value_pushes, depth, "{storage:?}");
            while retained.pop().is_some() {}
        }

        for promoted in [false, true] {
            let nested = EvalExpr::CallFnExpr {
                function: test_eval_function(&[], EvalExpr::Local(0)),
                captured_slots: vec![usize::from(promoted)].into(),
                args: Vec::new(),
            };
            let body = EvalExpr::CallFn {
                function: test_eval_function(&["first", "second"], EvalExpr::Local(1)),
                args: vec![EvalRef::new(EvalExpr::Local(0)), EvalRef::new(nested)],
            };
            let body = if promoted {
                EvalExpr::Let {
                    value: EvalRef::new(EvalExpr::Bool(false)),
                    body: EvalRef::new(body),
                }
            } else {
                body
            };
            let expr = EvalExpr::CallFn {
                function: test_eval_function(&["arg"], body),
                args: vec![EvalRef::new(EvalExpr::Bool(true))],
            };
            let metrics = Arc::new(EvalMetrics::default());
            let ctx = EvalCtx::new().with_metrics(metrics.clone());
            let mut storage = StorageMetrics::default();
            let value =
                eval_with_storage(&expr, &mut EvalSlots::root(Vec::new()), &ctx, &mut storage);
            assert!(matches!(value, Value::Bool(actual) if actual != promoted));
            assert_eq!(storage.record_pushes, 3, "{storage:?}");
            assert_eq!(storage.record_peak, 2, "{storage:?}");
            assert!(storage.record_growths > 0, "{storage:?}");
            assert_eq!(storage.activation_pushes, 3, "{storage:?}");
            assert_eq!(storage.activation_peak, 2, "{storage:?}");
            assert_eq!(
                storage.local_promotions,
                usize::from(promoted),
                "{storage:?}"
            );
            assert_eq!(metrics.snapshot().capture_vec_builds, 0);
            assert_eq!(metrics.snapshot().apply_fn_calls, 3);
        }

        let names = ["first", "second", "third", "fourth", "fifth"];
        let mut body = EvalExpr::Local(4);
        for index in (0..4).rev() {
            body = EvalExpr::Seq {
                value: EvalRef::new(EvalExpr::Local(index)),
                body: EvalRef::new(body),
            };
        }
        let mut args: Vec<_> = names[..4]
            .iter()
            .map(|name| EvalRef::new(EvalExpr::Atom((*name).into())))
            .collect();
        args.push(EvalRef::new(EvalExpr::CallFnExpr {
            function: test_eval_function(&[], EvalExpr::Local(0)),
            captured_slots: vec![0].into(),
            args: Vec::new(),
        }));
        let expr = EvalExpr::CallFn {
            function: test_eval_function(&names, body),
            args,
        };
        let metrics = Arc::new(EvalMetrics::default());
        let ctx = EvalCtx::new().with_metrics(metrics.clone());
        let mut storage = StorageMetrics::default();
        let value = eval_with_storage(
            &expr,
            &mut EvalSlots::root(vec![Value::Atom("fifth".into())]),
            &ctx,
            &mut storage,
        );
        let mut residual = &value;
        for name in &names[..4] {
            let Value::Seq { value, body } = residual else {
                panic!("each earlier argument remains ordered");
            };
            assert!(matches!(value.as_ref(), Value::Atom(actual) if actual.as_str() == *name));
            residual = body;
        }
        assert!(matches!(residual, Value::Atom(actual) if actual == "fifth"));
        assert_eq!(metrics.snapshot().arg_vec_builds, 0);
        assert_eq!(metrics.snapshot().arg_vec_values, 0);
        assert_eq!(metrics.snapshot().capture_vec_builds, 0);
        assert_eq!(metrics.snapshot().apply_fn_calls, 2);
        assert_eq!(storage.record_pushes, 2, "{storage:?}");
        assert_eq!(storage.record_peak, 1, "{storage:?}");
        assert_eq!(storage.record_growths, 0, "{storage:?}");
        assert_eq!(storage.activation_peak, 1, "{storage:?}");
        assert_eq!(storage.value_pushes, 9, "{storage:?}");
    }

    #[test]
    fn memo_and_nonexact_calls_preserve_preparation_boundaries() {
        for arity in [0, 1] {
            let params = if arity == 0 { &[][..] } else { &["arg"][..] };
            let function = test_eval_function(
                params,
                EvalExpr::ReflTypeEqual {
                    left: EvalRef::new(EvalExpr::ReflTypeUnit),
                    right: EvalRef::new(EvalExpr::ReflTypeUnit),
                },
            );
            assert!(function.memo_candidate);
            let expr = EvalExpr::CallFn {
                function,
                args: vec![EvalRef::new(EvalExpr::Bool(false)); arity],
            };
            let metrics = Arc::new(EvalMetrics::default());
            let ctx = EvalCtx::new().with_metrics(metrics.clone());
            for hit in [false, true] {
                let mut storage = StorageMetrics::default();
                let value =
                    eval_with_storage(&expr, &mut EvalSlots::root(Vec::new()), &ctx, &mut storage);
                assert!(matches!(value, Value::Bool(true)));
                assert_eq!(storage.record_pushes, usize::from(!hit), "{storage:?}");
                assert_eq!(storage.activation_pushes, usize::from(!hit), "{storage:?}");
                assert_eq!(storage.activation_peak, usize::from(!hit), "{storage:?}");
                assert_eq!(metrics.snapshot().apply_fn_calls, 1);
                assert_eq!(metrics.snapshot().exact_call_memo_hits, usize::from(hit));
            }
        }

        for zero_group in [false, true] {
            let (captured, weak) = tracked("captured");
            let mut slots = EvalSlots::root(vec![captured]);
            let params = if zero_group {
                &["last"][..]
            } else {
                &["first", "last"][..]
            };
            let mut function = test_eval_function(params, EvalExpr::Local(params.len()));
            if zero_group {
                EvalRef::get_mut(&mut function)
                    .expect("unique function")
                    .value_groups = vec![
                    EvalParamGroup {
                        param_start: 0,
                        param_count: 0,
                        abi_arity: 0,
                    },
                    EvalParamGroup {
                        param_start: 0,
                        param_count: 1,
                        abi_arity: 1,
                    },
                ]
                .into();
            } else {
                EvalRef::get_mut(&mut function)
                    .expect("unique function")
                    .value_groups = vec![
                    EvalParamGroup {
                        param_start: 0,
                        param_count: 1,
                        abi_arity: 1,
                    },
                    EvalParamGroup {
                        param_start: 1,
                        param_count: 1,
                        abi_arity: 1,
                    },
                ]
                .into();
            }
            let expr = EvalExpr::CallFnExpr {
                function,
                captured_slots: vec![0].into(),
                args: vec![EvalRef::new(EvalExpr::Bool(true))],
            };
            let metrics = Arc::new(EvalMetrics::default());
            let ctx = EvalCtx::new().with_metrics(metrics.clone());
            let mut storage = StorageMetrics::default();
            let partial = eval_with_storage(&expr, &mut slots, &ctx, &mut storage);
            assert!(matches!(partial, Value::EvalClosure { .. }));
            assert_eq!(storage.record_pushes, 0, "{storage:?}");
            assert_eq!(storage.activation_pushes, 0, "{storage:?}");
            assert_eq!(metrics.snapshot().apply_fn_calls, 1);
            assert_eq!(weak.strong_count(), 2);
            let value = apply(partial, vec![Value::Bool(false)], &ctx);
            assert!(matches!(value, Value::Bool(false)));
            assert_eq!(weak.strong_count(), 1);
            assert_eq!(metrics.snapshot().apply_fn_calls, 2);
        }
    }

    #[test]
    fn memo_lookup_fault_releases_prepared_arguments_and_captures() {
        let mut owners = Vec::new();
        let locals = (0..3)
            .map(|_| {
                let (value, owner) = tracked("root");
                owners.push(owner);
                value
            })
            .collect();
        let mut slots = EvalSlots::root(locals);
        let function = test_eval_function(
            &["arg"],
            EvalExpr::ReflTypeEqual {
                left: EvalRef::new(EvalExpr::ReflTypeUnit),
                right: EvalRef::new(EvalExpr::ReflTypeUnit),
            },
        );
        assert!(function.memo_candidate);
        let expr = EvalExpr::Seq {
            value: EvalRef::new(EvalExpr::Local(0)),
            body: EvalRef::new(EvalExpr::CallFnExpr {
                function,
                captured_slots: vec![2, 0].into(),
                args: vec![EvalRef::new(EvalExpr::Local(1))],
            }),
        };
        let metrics = Arc::new(EvalMetrics::default());
        let ctx = EvalCtx::new().with_metrics(metrics.clone());
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = ctx
                    .active_structural_recur_sites
                    .lock()
                    .expect("unpoisoned site state");
                panic!("selected memo preparation fault");
            }))
            .is_err()
        );
        let mut storage = StorageMetrics::default();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                eval_with_storage(&expr, &mut slots, &ctx, &mut storage)
            }))
            .is_err()
        );
        assert_eq!(storage.record_pushes, 0, "{storage:?}");
        assert_eq!(storage.activation_pushes, 0, "{storage:?}");
        assert_eq!(metrics.snapshot().capture_vec_builds, 1);
        assert_eq!(metrics.snapshot().capture_vec_values, 2);
        assert_eq!(metrics.snapshot().apply_fn_calls, 0);
        assert!(owners.iter().all(|owner| owner.strong_count() == 1));
        drop(slots);
        assert!(owners.iter().all(|owner| owner.strong_count() == 0));
    }

    struct DropProbe {
        events: Arc<Mutex<Vec<usize>>>,
        id: usize,
        panics: bool,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.events.lock().expect("drop events").push(self.id);
            assert!(!self.panics, "selected drop panic");
        }
    }

    #[test]
    fn discard_top_keeps_inline_and_spilled_ownership_after_panic() {
        for depth in [1, 2] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut stack = Stack::<_, 1>::new();
            for id in 0..depth {
                stack.push(DropProbe {
                    events: events.clone(),
                    id,
                    panics: id + 1 == depth,
                });
            }
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    stack.discard_top();
                }))
                .is_err()
            );
            assert_eq!(stack.len, depth - 1);
            drop(stack);
            assert_eq!(
                *events.lock().expect("drop events"),
                (0..depth).rev().collect::<Vec<_>>()
            );
        }
    }
}

//! Structural fuel bounds and operation-local totality faults.

use super::{
    EvalCtx, EvalPhase, StructuralRecurDataIdentity, Type, Value, eval_refl_type_free_bare_heads,
    project_product_value,
};
use std::collections::HashSet;
use std::time::Instant;

#[derive(Debug)]
pub(super) struct StructuralRecurOperationState {
    pub(super) depth: usize,
    pub(super) active_helpers: usize,
    pub(super) first_fault: Option<Value>,
}

impl<'m> EvalCtx<'m> {
    pub(super) fn has_active_structural_recur_helper(&self) -> bool {
        let thread_id = std::thread::current().id();
        self.structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned")
            .get(&thread_id)
            .is_some_and(|state| state.active_helpers != 0)
    }

    pub(super) fn enter_structural_recur_operation(&self) -> StructuralRecurOperationGuard<'_, 'm> {
        let thread_id = std::thread::current().id();
        let mut operations = self
            .structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned");
        let state = operations
            .entry(thread_id)
            .or_insert(StructuralRecurOperationState {
                depth: 0,
                active_helpers: 0,
                first_fault: None,
            });
        let root = state.depth == 0;
        state.depth += 1;
        StructuralRecurOperationGuard {
            ctx: self,
            thread_id,
            root,
            active: true,
        }
    }

    pub(super) fn record_structural_recur_fault(&self, fault: &Value) {
        let thread_id = std::thread::current().id();
        let mut operations = self
            .structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned");
        let Some(state) = operations.get_mut(&thread_id) else {
            return;
        };
        if state.first_fault.is_none() {
            state.first_fault = Some(fault.clone());
        }
    }

    pub(super) fn structural_recur_operation_has_fault(&self) -> bool {
        let thread_id = std::thread::current().id();
        self.structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned")
            .get(&thread_id)
            .is_some_and(|state| state.first_fault.is_some())
    }

    fn leave_structural_recur_operation(
        &self,
        thread_id: std::thread::ThreadId,
        root: bool,
    ) -> Option<Value> {
        let mut operations = self
            .structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned");
        let state = operations
            .get_mut(&thread_id)
            .expect("active __structural_recur__ operation frame");
        state.depth = state
            .depth
            .checked_sub(1)
            .expect("__structural_recur__ operation depth is positive");
        if root {
            debug_assert_eq!(state.depth, 0);
            debug_assert_eq!(state.active_helpers, 0);
            return operations
                .remove(&thread_id)
                .and_then(|state| state.first_fault);
        }
        debug_assert!(state.depth > 0);
        None
    }

    pub(super) fn enter_structural_recur_helper(
        &self,
        fuel: &Value,
    ) -> Option<(usize, StructuralRecurHelperGuard<'_, 'm>)> {
        let measure = initial_fuel_measure(fuel, self)?;
        let thread_id = std::thread::current().id();
        let mut operations = self
            .structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned");
        let state = operations
            .get_mut(&thread_id)
            .expect("structural recursion runs within an evaluator operation");
        // This count preserves the conservative cache scope; callback descent
        // depends only on the immutable bounds carried by each callback.
        state.active_helpers = state
            .active_helpers
            .checked_add(1)
            .expect("active structural recursion helper count overflow");
        Some((
            measure,
            StructuralRecurHelperGuard {
                ctx: self,
                thread_id,
            },
        ))
    }
}

pub(super) struct StructuralRecurOperationGuard<'ctx, 'm> {
    ctx: &'ctx EvalCtx<'m>,
    thread_id: std::thread::ThreadId,
    root: bool,
    active: bool,
}

impl StructuralRecurOperationGuard<'_, '_> {
    pub(super) fn finish(mut self, value: Value) -> Value {
        let fault = self
            .ctx
            .leave_structural_recur_operation(self.thread_id, self.root);
        self.active = false;
        if self.root {
            fault.unwrap_or(value)
        } else {
            value
        }
    }

    pub(super) fn finish_checked<T>(mut self, value: T) -> Result<T, Value> {
        let fault = self
            .ctx
            .leave_structural_recur_operation(self.thread_id, self.root);
        self.active = false;
        match fault {
            Some(fault) => Err(fault),
            None => Ok(value),
        }
    }
}

impl Drop for StructuralRecurOperationGuard<'_, '_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self
                .ctx
                .leave_structural_recur_operation(self.thread_id, self.root);
        }
    }
}

pub(super) struct StructuralRecurHelperGuard<'ctx, 'm> {
    ctx: &'ctx EvalCtx<'m>,
    thread_id: std::thread::ThreadId,
}

impl Drop for StructuralRecurHelperGuard<'_, '_> {
    fn drop(&mut self) {
        let mut operations = self
            .ctx
            .structural_recur_operations
            .lock()
            .expect("__structural_recur__ operation map poisoned");
        let state = operations
            .get_mut(&self.thread_id)
            .expect("active structural recursion operation exists");
        state.active_helpers = state
            .active_helpers
            .checked_sub(1)
            .expect("active structural recursion helper count is positive");
    }
}

#[derive(Clone, Copy)]
enum FuelMeasureMode {
    Initial,
    Recursive,
}

pub(super) fn fuel_measure(value: &Value, ctx: &EvalCtx<'_>) -> Option<usize> {
    fuel_measure_with_mode(value, ctx, FuelMeasureMode::Recursive)
}

pub(super) fn descending_fuel_measure(
    fuel: &Value,
    current_measure: usize,
    root_measure: usize,
    ctx: &EvalCtx<'_>,
) -> Option<usize> {
    fuel_measure(fuel, ctx).filter(|next| *next < current_measure && *next < root_measure)
}

pub(super) fn initial_fuel_measure(value: &Value, ctx: &EvalCtx<'_>) -> Option<usize> {
    fuel_measure_with_mode(value, ctx, FuelMeasureMode::Initial)
}

fn fuel_measure_with_mode(
    value: &Value,
    ctx: &EvalCtx<'_>,
    mode: FuelMeasureMode,
) -> Option<usize> {
    let mut total = 0usize;
    let mut pending = vec![(value, false)];
    while let Some((value, nested_in_data)) = pending.pop() {
        let measure = match value {
            Value::Unit
            | Value::Bool(_)
            | Value::IntAtom(_, _)
            | Value::FloatAtom(_, _)
            | Value::StrAtom(_)
            | Value::CheckedTerm(_)
            | Value::DiagnosticText(_)
            | Value::ReflTypeVar { .. }
            | Value::ReflTypeName { .. } => 1,
            Value::ReflType(reflected) => refl_type_measure(reflected.as_type(), ctx)?,
            Value::ReflTypeArity(arity) => arity.checked_add(1)?,
            Value::Data(data) => {
                if data.structural_recur_identity().is_some() {
                    return None;
                }
                #[cfg(feature = "surface")]
                if data.is_fills_capability() {
                    return None;
                }
                pending.extend(data.args().iter().rev().map(|arg| (arg, true)));
                1
            }
            Value::EvalClosure { .. }
            | Value::StructuralRecur { .. }
            | Value::Primitive(_)
            | Value::NewtypeMember(_)
            | Value::Atom(_)
            | Value::CaseSplit { .. }
            | Value::Seq { .. }
            | Value::Stuck(_, _) => return None,
            #[cfg(feature = "surface")]
            Value::Projected(projected) => {
                if nested_in_data {
                    projected.initial_structural_measure()?
                } else {
                    match mode {
                        FuelMeasureMode::Initial => projected.initial_structural_measure()?,
                        FuelMeasureMode::Recursive => projected.structural_measure()?,
                    }
                }
            }
        };
        total = total.checked_add(measure)?;
    }
    Some(total)
}

fn refl_type_measure(typ: &Type<EvalPhase>, ctx: &EvalCtx<'_>) -> Option<usize> {
    let start = ctx.metrics.as_deref().map(|_| Instant::now());
    let binders = eval_refl_type_free_bare_heads(typ);
    let out = crate::pass::typecheck_core::measure_materialized_aliases_in_scope(
        typ,
        ctx.root_module,
        ctx.package,
        Some(&binders),
        true,
    );
    if let (Some(metrics), Some(start)) = (ctx.metrics.as_deref(), start) {
        metrics.record_type_unfold(start.elapsed());
    }
    out
}

pub(super) fn structural_recur_fault(value: Value, ctx: &EvalCtx<'_>) -> Value {
    ctx.record_structural_recur_fault(&value);
    value
}

pub fn structural_recur_stuck_message(value: &Value, ctx: &EvalCtx<'_>) -> Option<String> {
    // Totality failures are represented internally as fault values, so a deep
    // but finite evaluation may wrap one in many ordinary value constructors.
    // Scan iteratively:
    // an arbitrary recursion cap could silently reclassify that failure, while
    // pointer deduplication keeps shared residual subgraphs proportional to the
    // represented values and edges.
    let mut pending = vec![value];
    let mut visited = HashSet::<*const Value>::new();
    while let Some(value) = pending.pop() {
        if !visited.insert(value as *const Value) {
            continue;
        }
        match value {
            Value::Stuck(callee, args) => {
                if let Value::StructuralRecur {
                    root_measure,
                    current_measure,
                    ..
                } = callee.as_ref()
                {
                    return Some(structural_recur_recursive_stuck_message(
                        *root_measure,
                        *current_measure,
                        args,
                        ctx,
                    ));
                }
                pending.extend(args.iter().rev());
                pending.push(callee.as_ref());
            }
            Value::Data(data) => match data.structural_recur_identity() {
                Some(StructuralRecurDataIdentity::Primitive { .. }) => {}
                Some(StructuralRecurDataIdentity::InitialRejected) => {
                    return Some(
                        "`__structural_recur__` got stuck because its initial fuel has no structural compile-time measure"
                            .to_string(),
                    );
                }
                None => pending.extend(data.args().iter().rev()),
            },
            Value::CaseSplit {
                scrutinee,
                left,
                right,
                ..
            } => {
                pending.push(right.as_ref());
                pending.push(left.as_ref());
                pending.push(scrutinee.as_ref());
            }
            Value::Seq { value, body } => {
                pending.push(body.as_ref());
                pending.push(value.as_ref());
            }
            _ => {}
        }
    }
    None
}

fn structural_recur_recursive_stuck_message(
    root_measure: usize,
    current_measure: usize,
    args: &[Value],
    ctx: &EvalCtx<'_>,
) -> String {
    if !matches!(args.len(), 1 | 2) {
        let next = match args.first() {
            Some(fuel) => fuel_measure(fuel, ctx)
                .map_or_else(|| "unmeasurable".to_string(), |measure| measure.to_string()),
            None => "missing".to_string(),
        };
        return format!(
            "`__structural_recur__` got stuck because a recursive call did not provide one product packet or two flattened arguments (root={root_measure}, current={current_measure}, next={next})"
        );
    }
    let next_fuel = if let [packet] = args {
        project_product_value("__fst__", packet)
    } else {
        args[0].clone()
    };
    let Some(next_measure) = fuel_measure(&next_fuel, ctx) else {
        return format!(
            "`__structural_recur__` got stuck because a recursive call used fuel with no structural compile-time measure (root={root_measure}, current={current_measure}, next=unmeasurable)"
        );
    };
    if next_measure < current_measure && next_measure >= root_measure {
        return format!(
            "`__structural_recur__` got stuck because a recursive call did not stay below the root fuel measure (root={root_measure}, current={current_measure}, next={next_measure})"
        );
    }
    format!(
        "`__structural_recur__` got stuck because a recursive call did not descend from the current fuel measure (root={root_measure}, current={current_measure}, next={next_measure})"
    )
}

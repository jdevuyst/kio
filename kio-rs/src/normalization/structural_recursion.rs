//! Structural fuel bounds and operation-local compile-time faults.

use super::{
    EvalCtx, StructuralRecurDataIdentity, Value, eval_refl_type_free_bare_heads,
    project_product_value,
};
use std::collections::HashSet;
use std::time::Instant;

#[derive(Debug)]
pub(super) struct EvalOperationState {
    pub(super) depth: usize,
    pub(super) active_helpers: usize,
    pub(super) first_fault: Option<Value>,
}

impl<'m> EvalCtx<'m> {
    pub(super) fn has_active_structural_recur_helper(&self) -> bool {
        let thread_id = std::thread::current().id();
        self.operations
            .lock()
            .expect("evaluator operation map poisoned")
            .get(&thread_id)
            .is_some_and(|state| state.active_helpers != 0)
    }

    pub(super) fn enter_eval_operation(&self) -> EvalOperationGuard<'_, 'm> {
        let thread_id = std::thread::current().id();
        let mut operations = self
            .operations
            .lock()
            .expect("evaluator operation map poisoned");
        let state = operations.entry(thread_id).or_insert(EvalOperationState {
            depth: 0,
            active_helpers: 0,
            first_fault: None,
        });
        let root = state.depth == 0;
        state.depth += 1;
        EvalOperationGuard {
            ctx: self,
            thread_id,
            root,
            active: true,
        }
    }

    pub(super) fn record_operation_fault(&self, fault: &Value) {
        let thread_id = std::thread::current().id();
        let mut operations = self
            .operations
            .lock()
            .expect("evaluator operation map poisoned");
        let Some(state) = operations.get_mut(&thread_id) else {
            return;
        };
        if state.first_fault.is_none() {
            state.first_fault = Some(fault.clone());
        }
    }

    pub(super) fn operation_has_fault(&self) -> bool {
        let thread_id = std::thread::current().id();
        self.operations
            .lock()
            .expect("evaluator operation map poisoned")
            .get(&thread_id)
            .is_some_and(|state| state.first_fault.is_some())
    }

    fn leave_eval_operation(&self, thread_id: std::thread::ThreadId, root: bool) -> Option<Value> {
        let mut operations = self
            .operations
            .lock()
            .expect("evaluator operation map poisoned");
        let state = operations
            .get_mut(&thread_id)
            .expect("active evaluator operation frame");
        state.depth = state
            .depth
            .checked_sub(1)
            .expect("evaluator operation depth is positive");
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
            .operations
            .lock()
            .expect("evaluator operation map poisoned");
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

pub(super) struct EvalOperationGuard<'ctx, 'm> {
    ctx: &'ctx EvalCtx<'m>,
    thread_id: std::thread::ThreadId,
    root: bool,
    active: bool,
}

impl EvalOperationGuard<'_, '_> {
    pub(super) fn finish(mut self, value: Value) -> Value {
        let fault = self.ctx.leave_eval_operation(self.thread_id, self.root);
        self.active = false;
        if self.root {
            fault.unwrap_or(value)
        } else {
            value
        }
    }

    pub(super) fn finish_checked<T>(mut self, value: T) -> Result<T, Value> {
        let fault = self.ctx.leave_eval_operation(self.thread_id, self.root);
        self.active = false;
        match fault {
            Some(fault) => Err(fault),
            None => Ok(value),
        }
    }
}

impl Drop for EvalOperationGuard<'_, '_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.ctx.leave_eval_operation(self.thread_id, self.root);
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
            .operations
            .lock()
            .expect("evaluator operation map poisoned");
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
            | Value::ReflTypeVar(_)
            | Value::ReflTypeName { .. } => 1,
            Value::ReflType(_) if nested_in_data => 1,
            Value::ReflType(reflected) => refl_type_measure(reflected, ctx, mode)?,
            Value::ReflTypeArity(arity) => arity.checked_add(1)?,
            Value::Data(data) => {
                if data.structural_recur_identity().is_some() {
                    return None;
                }
                if data.incomplete_observation().is_some() {
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
            | Value::Stuck(_, _)
            | Value::StuckType(_, _) => return None,
            #[cfg(feature = "surface")]
            Value::Projected(projected) => {
                if nested_in_data {
                    1
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

fn refl_type_measure(
    reflected: &super::ReflectedType,
    ctx: &EvalCtx<'_>,
    mode: FuelMeasureMode,
) -> Option<usize> {
    let typ = reflected.as_type();
    let neutral_heads = reflected
        .free_bindings()
        .into_iter()
        .filter(|(_, variable)| {
            matches!(
                variable.meaning,
                super::reflection_bindings::VariableMeaning::Neutral(_)
            )
        })
        .map(|(name, _)| name.to_owned())
        .collect();
    let start = ctx.metrics.as_deref().map(|_| Instant::now());
    let binders = eval_refl_type_free_bare_heads(typ);
    let out = crate::pass::typecheck_core::measure_materialized_aliases_in_scope(
        typ,
        ctx.root_module,
        ctx.package,
        Some(&binders),
        true,
        &neutral_heads,
        matches!(mode, FuelMeasureMode::Initial),
    );
    if let (Some(metrics), Some(start)) = (ctx.metrics.as_deref(), start) {
        metrics.record_type_unfold(start.elapsed());
    }
    out
}

pub(super) fn structural_recur_fault(value: Value, ctx: &EvalCtx<'_>) -> Value {
    ctx.record_operation_fault(&value);
    value
}

#[derive(Debug, Clone)]
pub(crate) enum ComptimeFault {
    Totality(String),
    IncompleteObservation(crate::comptime::ComptimeBuiltin),
}

impl ComptimeFault {
    pub fn message(&self) -> String {
        match self {
            Self::Totality(message) => message.clone(),
            Self::IncompleteObservation(builtin) => format!(
                "`{}` inspected unresolved type information; supply a complete type before this observation",
                builtin.public_name(),
            ),
        }
    }

    pub fn into_error(self, span: crate::span::Span) -> crate::error::Error {
        match self {
            Self::Totality(message) => crate::error::Error::totality(span, message),
            Self::IncompleteObservation(_) => {
                crate::error::Error::macro_error(span, self.message())
            }
        }
    }
}

/// Reports structural-recursion totality failures in an evaluated value.
pub fn structural_recur_stuck_message(value: &Value, ctx: &EvalCtx<'_>) -> Option<String> {
    match comptime_fault(value, ctx) {
        Some(ComptimeFault::Totality(message)) => Some(message),
        _ => None,
    }
}

pub(crate) fn comptime_fault_message(value: &Value, ctx: &EvalCtx<'_>) -> Option<String> {
    comptime_fault(value, ctx).map(|fault| fault.message())
}

pub(crate) fn comptime_fault(value: &Value, ctx: &EvalCtx<'_>) -> Option<ComptimeFault> {
    // Evaluated failures are represented internally as fault values, so a deep
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
            Value::StuckType(callee, _) => pending.push(callee.as_ref()),
            Value::Stuck(callee, args) => {
                if let Value::StructuralRecur {
                    root_measure,
                    current_measure,
                    ..
                } = callee.as_ref()
                {
                    return Some(ComptimeFault::Totality(
                        structural_recur_recursive_stuck_message(
                            *root_measure,
                            *current_measure,
                            args,
                            ctx,
                        ),
                    ));
                }
                pending.extend(args.iter().rev());
                pending.push(callee.as_ref());
            }
            Value::Data(data) if data.incomplete_observation().is_some() => {
                return Some(ComptimeFault::IncompleteObservation(
                    data.incomplete_observation()
                        .expect("matched observation fault"),
                ));
            }
            Value::Data(data) => match data.structural_recur_identity() {
                Some(StructuralRecurDataIdentity::Primitive { .. }) => {}
                Some(StructuralRecurDataIdentity::InitialRejected) => {
                    return Some(ComptimeFault::Totality(
                        "`__structural_recur__` got stuck because its initial fuel has no structural compile-time measure"
                            .to_string(),
                    ));
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

#[cfg(test)]
mod neutral_fuel_tests {
    use super::*;
    use crate::normalization::*;

    #[test]
    fn supplied_neutral_type_has_only_a_fresh_entry_lower_bound() {
        let ctx = EvalCtx::new();
        let neutral = reflection_bindings::ReflectedVariable::neutral("A".into(), 0);
        let value = Value::ReflType(neutral.as_type());
        assert_eq!(initial_fuel_measure(&value, &ctx), Some(1));
        assert_eq!(fuel_measure(&value, &ctx), None);
        assert_eq!(descending_fuel_measure(&value, 10, 10, &ctx), None);
        assert_eq!(initial_fuel_measure(&Value::Atom("A".into()), &ctx), None);

        let unit = Value::ReflType(ReflectedType::new_trusted(Type::Unit { meta: zero_meta() }));
        let pair = reflection_binary_type_value(ComptimeBuiltin::TypeProduct, &value, &unit)
            .expect("type product");
        assert_eq!(initial_fuel_measure(&pair, &ctx), Some(3));
        assert_eq!(fuel_measure(&pair, &ctx), None);
        assert_eq!(fuel_measure(&value_pair(value, Value::Unit), &ctx), Some(3));
        assert_eq!(fuel_measure(&unit, &ctx), Some(1));
    }

    #[test]
    fn neutral_head_discards_arguments_only_for_the_initial_bound() {
        let ctx = EvalCtx::new();
        let head = Value::ReflType(
            reflection_bindings::ReflectedVariable::neutral("F".into(), 1).as_type(),
        );
        let argument = Value::ReflType(ReflectedType::new_trusted(Type::Product {
            left: Box::new(Type::Unit { meta: zero_meta() }),
            right: Box::new(Type::Unit { meta: zero_meta() }),
            meta: zero_meta(),
        }));
        let applied = reflection_type_apply_value(&head, &argument).expect("type application");
        assert_eq!(initial_fuel_measure(&applied, &ctx), Some(1));
        assert_eq!(fuel_measure(&applied, &ctx), None);

        let bound = ReflectedType::new_trusted(Type::Forall {
            param: TypeParam {
                name: "F".into(),
                presentation: Default::default(),
                kind: None,
                span: zero_span(),
            },
            body: Box::new(type_path("F")),
            meta: zero_meta(),
        });
        let neutral = reflection_bindings::ReflectedVariable::neutral("F".into(), 0);
        let shadowed = Value::ReflType(bound.with_lexical_bindings([("F".into(), neutral)]));
        assert_eq!(initial_fuel_measure(&shadowed, &ctx), Some(2));
        assert_eq!(fuel_measure(&shadowed, &ctx), Some(2));
    }
}

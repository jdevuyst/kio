//! Optional normalization counters, timing snapshots, and hotspot aggregation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
pub struct EvalMetrics {
    root_eval_ns: AtomicU64,
    root_eval_calls: AtomicUsize,
    expr_visits: AtomicUsize,
    apply_ns: AtomicU64,
    apply_calls: AtomicUsize,
    apply_generic_ns: AtomicU64,
    apply_generic_calls: AtomicUsize,
    apply_atom_ns: AtomicU64,
    apply_atom_calls: AtomicUsize,
    apply_fn_ns: AtomicU64,
    apply_fn_calls: AtomicUsize,
    closure_apply_ns: AtomicU64,
    closure_apply_calls: AtomicUsize,
    reflection_ns: AtomicU64,
    reflection_calls: AtomicUsize,
    reflection_type_view_ns: AtomicU64,
    reflection_type_view_calls: AtomicUsize,
    reflection_type_equal_ns: AtomicU64,
    reflection_type_equal_calls: AtomicUsize,
    reflection_type_fold_ns: AtomicU64,
    reflection_type_fold_calls: AtomicUsize,
    reflection_term_ns: AtomicU64,
    reflection_term_calls: AtomicUsize,
    reflection_term_let_ns: AtomicU64,
    reflection_term_let_calls: AtomicUsize,
    reflection_term_fn_ns: AtomicU64,
    reflection_term_fn_calls: AtomicUsize,
    reflection_term_call_ns: AtomicU64,
    reflection_term_call_calls: AtomicUsize,
    reflection_term_type_ns: AtomicU64,
    reflection_term_type_calls: AtomicUsize,
    reflection_term_unit_ns: AtomicU64,
    reflection_term_unit_calls: AtomicUsize,
    reflection_intrinsic_pair_ns: AtomicU64,
    reflection_intrinsic_pair_calls: AtomicUsize,
    reflection_intrinsic_projection_ns: AtomicU64,
    reflection_intrinsic_projection_calls: AtomicUsize,
    reflection_intrinsic_injection_ns: AtomicU64,
    reflection_intrinsic_injection_calls: AtomicUsize,
    reflection_intrinsic_either_ns: AtomicU64,
    reflection_intrinsic_either_calls: AtomicUsize,
    reflection_intrinsic_other_ns: AtomicU64,
    reflection_intrinsic_other_calls: AtomicUsize,
    reflection_structural_recur_ns: AtomicU64,
    reflection_structural_recur_calls: AtomicUsize,
    reflection_other_ns: AtomicU64,
    reflection_other_calls: AtomicUsize,
    type_unfold_ns: AtomicU64,
    type_unfold_calls: AtomicUsize,
    type_equiv_ns: AtomicU64,
    type_equiv_calls: AtomicUsize,
    nf_eq_ns: AtomicU64,
    nf_eq_calls: AtomicUsize,
    eta_contract_ns: AtomicU64,
    eta_contract_calls: AtomicUsize,
    env_clones: AtomicUsize,
    closure_builds: AtomicUsize,
    arg_vec_builds: AtomicUsize,
    arg_vec_values: AtomicUsize,
    capture_vec_builds: AtomicUsize,
    capture_vec_values: AtomicUsize,
    exact_call_memo_hits: AtomicUsize,
    exact_call_memo_misses: AtomicUsize,
    exact_call_memo_key_skips: AtomicUsize,
    exact_call_memo_stores: AtomicUsize,
    function_apply: Mutex<HashMap<String, EvalFunctionMetric>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EvalFunctionMetric {
    duration: Duration,
    calls: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalFunctionHotspot {
    pub label: String,
    pub duration: Duration,
    pub calls: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvalMetricsSnapshot {
    pub root_eval: Duration,
    pub root_eval_calls: usize,
    pub expr_visits: usize,
    pub apply: Duration,
    pub apply_calls: usize,
    pub apply_generic: Duration,
    pub apply_generic_calls: usize,
    pub apply_atom: Duration,
    pub apply_atom_calls: usize,
    pub apply_fn: Duration,
    pub apply_fn_calls: usize,
    pub closure_apply: Duration,
    pub closure_apply_calls: usize,
    pub reflection: Duration,
    pub reflection_calls: usize,
    pub reflection_type_view: Duration,
    pub reflection_type_view_calls: usize,
    pub reflection_type_equal: Duration,
    pub reflection_type_equal_calls: usize,
    pub reflection_type_fold: Duration,
    pub reflection_type_fold_calls: usize,
    pub reflection_term: Duration,
    pub reflection_term_calls: usize,
    pub reflection_term_let: Duration,
    pub reflection_term_let_calls: usize,
    pub reflection_term_fn: Duration,
    pub reflection_term_fn_calls: usize,
    pub reflection_term_call: Duration,
    pub reflection_term_call_calls: usize,
    pub reflection_term_type: Duration,
    pub reflection_term_type_calls: usize,
    pub reflection_term_unit: Duration,
    pub reflection_term_unit_calls: usize,
    pub reflection_intrinsic_pair: Duration,
    pub reflection_intrinsic_pair_calls: usize,
    pub reflection_intrinsic_projection: Duration,
    pub reflection_intrinsic_projection_calls: usize,
    pub reflection_intrinsic_injection: Duration,
    pub reflection_intrinsic_injection_calls: usize,
    pub reflection_intrinsic_either: Duration,
    pub reflection_intrinsic_either_calls: usize,
    pub reflection_intrinsic_other: Duration,
    pub reflection_intrinsic_other_calls: usize,
    pub reflection_structural_recur: Duration,
    pub reflection_structural_recur_calls: usize,
    pub reflection_other: Duration,
    pub reflection_other_calls: usize,
    pub type_unfold: Duration,
    pub type_unfold_calls: usize,
    pub type_equiv: Duration,
    pub type_equiv_calls: usize,
    pub nf_eq: Duration,
    pub nf_eq_calls: usize,
    pub eta_contract: Duration,
    pub eta_contract_calls: usize,
    pub env_clones: usize,
    pub closure_builds: usize,
    pub arg_vec_builds: usize,
    pub arg_vec_values: usize,
    pub capture_vec_builds: usize,
    pub capture_vec_values: usize,
    pub exact_call_memo_hits: usize,
    pub exact_call_memo_misses: usize,
    pub exact_call_memo_key_skips: usize,
    pub exact_call_memo_stores: usize,
    pub function_hotspots: Vec<EvalFunctionHotspot>,
}

impl EvalMetrics {
    pub fn collect() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn enabled() -> Option<Arc<Self>> {
        crate::timing::eval_enabled().then(Self::collect)
    }

    pub fn snapshot(&self) -> EvalMetricsSnapshot {
        EvalMetricsSnapshot {
            root_eval: duration_from_nanos(self.root_eval_ns.load(Ordering::Relaxed)),
            root_eval_calls: self.root_eval_calls.load(Ordering::Relaxed),
            expr_visits: self.expr_visits.load(Ordering::Relaxed),
            apply: duration_from_nanos(self.apply_ns.load(Ordering::Relaxed)),
            apply_calls: self.apply_calls.load(Ordering::Relaxed),
            apply_generic: duration_from_nanos(self.apply_generic_ns.load(Ordering::Relaxed)),
            apply_generic_calls: self.apply_generic_calls.load(Ordering::Relaxed),
            apply_atom: duration_from_nanos(self.apply_atom_ns.load(Ordering::Relaxed)),
            apply_atom_calls: self.apply_atom_calls.load(Ordering::Relaxed),
            apply_fn: duration_from_nanos(self.apply_fn_ns.load(Ordering::Relaxed)),
            apply_fn_calls: self.apply_fn_calls.load(Ordering::Relaxed),
            closure_apply: duration_from_nanos(self.closure_apply_ns.load(Ordering::Relaxed)),
            closure_apply_calls: self.closure_apply_calls.load(Ordering::Relaxed),
            reflection: duration_from_nanos(self.reflection_ns.load(Ordering::Relaxed)),
            reflection_calls: self.reflection_calls.load(Ordering::Relaxed),
            reflection_type_view: duration_from_nanos(
                self.reflection_type_view_ns.load(Ordering::Relaxed),
            ),
            reflection_type_view_calls: self.reflection_type_view_calls.load(Ordering::Relaxed),
            reflection_type_equal: duration_from_nanos(
                self.reflection_type_equal_ns.load(Ordering::Relaxed),
            ),
            reflection_type_equal_calls: self.reflection_type_equal_calls.load(Ordering::Relaxed),
            reflection_type_fold: duration_from_nanos(
                self.reflection_type_fold_ns.load(Ordering::Relaxed),
            ),
            reflection_type_fold_calls: self.reflection_type_fold_calls.load(Ordering::Relaxed),
            reflection_term: duration_from_nanos(self.reflection_term_ns.load(Ordering::Relaxed)),
            reflection_term_calls: self.reflection_term_calls.load(Ordering::Relaxed),
            reflection_term_let: duration_from_nanos(
                self.reflection_term_let_ns.load(Ordering::Relaxed),
            ),
            reflection_term_let_calls: self.reflection_term_let_calls.load(Ordering::Relaxed),
            reflection_term_fn: duration_from_nanos(
                self.reflection_term_fn_ns.load(Ordering::Relaxed),
            ),
            reflection_term_fn_calls: self.reflection_term_fn_calls.load(Ordering::Relaxed),
            reflection_term_call: duration_from_nanos(
                self.reflection_term_call_ns.load(Ordering::Relaxed),
            ),
            reflection_term_call_calls: self.reflection_term_call_calls.load(Ordering::Relaxed),
            reflection_term_type: duration_from_nanos(
                self.reflection_term_type_ns.load(Ordering::Relaxed),
            ),
            reflection_term_type_calls: self.reflection_term_type_calls.load(Ordering::Relaxed),
            reflection_term_unit: duration_from_nanos(
                self.reflection_term_unit_ns.load(Ordering::Relaxed),
            ),
            reflection_term_unit_calls: self.reflection_term_unit_calls.load(Ordering::Relaxed),
            reflection_intrinsic_pair: duration_from_nanos(
                self.reflection_intrinsic_pair_ns.load(Ordering::Relaxed),
            ),
            reflection_intrinsic_pair_calls: self
                .reflection_intrinsic_pair_calls
                .load(Ordering::Relaxed),
            reflection_intrinsic_projection: duration_from_nanos(
                self.reflection_intrinsic_projection_ns
                    .load(Ordering::Relaxed),
            ),
            reflection_intrinsic_projection_calls: self
                .reflection_intrinsic_projection_calls
                .load(Ordering::Relaxed),
            reflection_intrinsic_injection: duration_from_nanos(
                self.reflection_intrinsic_injection_ns
                    .load(Ordering::Relaxed),
            ),
            reflection_intrinsic_injection_calls: self
                .reflection_intrinsic_injection_calls
                .load(Ordering::Relaxed),
            reflection_intrinsic_either: duration_from_nanos(
                self.reflection_intrinsic_either_ns.load(Ordering::Relaxed),
            ),
            reflection_intrinsic_either_calls: self
                .reflection_intrinsic_either_calls
                .load(Ordering::Relaxed),
            reflection_intrinsic_other: duration_from_nanos(
                self.reflection_intrinsic_other_ns.load(Ordering::Relaxed),
            ),
            reflection_intrinsic_other_calls: self
                .reflection_intrinsic_other_calls
                .load(Ordering::Relaxed),
            reflection_structural_recur: duration_from_nanos(
                self.reflection_structural_recur_ns.load(Ordering::Relaxed),
            ),
            reflection_structural_recur_calls: self
                .reflection_structural_recur_calls
                .load(Ordering::Relaxed),
            reflection_other: duration_from_nanos(self.reflection_other_ns.load(Ordering::Relaxed)),
            reflection_other_calls: self.reflection_other_calls.load(Ordering::Relaxed),
            type_unfold: duration_from_nanos(self.type_unfold_ns.load(Ordering::Relaxed)),
            type_unfold_calls: self.type_unfold_calls.load(Ordering::Relaxed),
            type_equiv: duration_from_nanos(self.type_equiv_ns.load(Ordering::Relaxed)),
            type_equiv_calls: self.type_equiv_calls.load(Ordering::Relaxed),
            nf_eq: duration_from_nanos(self.nf_eq_ns.load(Ordering::Relaxed)),
            nf_eq_calls: self.nf_eq_calls.load(Ordering::Relaxed),
            eta_contract: duration_from_nanos(self.eta_contract_ns.load(Ordering::Relaxed)),
            eta_contract_calls: self.eta_contract_calls.load(Ordering::Relaxed),
            env_clones: self.env_clones.load(Ordering::Relaxed),
            closure_builds: self.closure_builds.load(Ordering::Relaxed),
            arg_vec_builds: self.arg_vec_builds.load(Ordering::Relaxed),
            arg_vec_values: self.arg_vec_values.load(Ordering::Relaxed),
            capture_vec_builds: self.capture_vec_builds.load(Ordering::Relaxed),
            capture_vec_values: self.capture_vec_values.load(Ordering::Relaxed),
            exact_call_memo_hits: self.exact_call_memo_hits.load(Ordering::Relaxed),
            exact_call_memo_misses: self.exact_call_memo_misses.load(Ordering::Relaxed),
            exact_call_memo_key_skips: self.exact_call_memo_key_skips.load(Ordering::Relaxed),
            exact_call_memo_stores: self.exact_call_memo_stores.load(Ordering::Relaxed),
            function_hotspots: self.function_hotspots(),
        }
    }

    pub fn add_snapshot(&self, snapshot: EvalMetricsSnapshot) {
        add_duration(&self.root_eval_ns, snapshot.root_eval);
        self.root_eval_calls
            .fetch_add(snapshot.root_eval_calls, Ordering::Relaxed);
        self.expr_visits
            .fetch_add(snapshot.expr_visits, Ordering::Relaxed);
        add_duration(&self.apply_ns, snapshot.apply);
        self.apply_calls
            .fetch_add(snapshot.apply_calls, Ordering::Relaxed);
        add_duration(&self.apply_generic_ns, snapshot.apply_generic);
        self.apply_generic_calls
            .fetch_add(snapshot.apply_generic_calls, Ordering::Relaxed);
        add_duration(&self.apply_atom_ns, snapshot.apply_atom);
        self.apply_atom_calls
            .fetch_add(snapshot.apply_atom_calls, Ordering::Relaxed);
        add_duration(&self.apply_fn_ns, snapshot.apply_fn);
        self.apply_fn_calls
            .fetch_add(snapshot.apply_fn_calls, Ordering::Relaxed);
        add_duration(&self.closure_apply_ns, snapshot.closure_apply);
        self.closure_apply_calls
            .fetch_add(snapshot.closure_apply_calls, Ordering::Relaxed);
        add_duration(&self.reflection_ns, snapshot.reflection);
        self.reflection_calls
            .fetch_add(snapshot.reflection_calls, Ordering::Relaxed);
        add_duration(&self.reflection_type_view_ns, snapshot.reflection_type_view);
        self.reflection_type_view_calls
            .fetch_add(snapshot.reflection_type_view_calls, Ordering::Relaxed);
        add_duration(
            &self.reflection_type_equal_ns,
            snapshot.reflection_type_equal,
        );
        self.reflection_type_equal_calls
            .fetch_add(snapshot.reflection_type_equal_calls, Ordering::Relaxed);
        add_duration(&self.reflection_type_fold_ns, snapshot.reflection_type_fold);
        self.reflection_type_fold_calls
            .fetch_add(snapshot.reflection_type_fold_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_ns, snapshot.reflection_term);
        self.reflection_term_calls
            .fetch_add(snapshot.reflection_term_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_let_ns, snapshot.reflection_term_let);
        self.reflection_term_let_calls
            .fetch_add(snapshot.reflection_term_let_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_fn_ns, snapshot.reflection_term_fn);
        self.reflection_term_fn_calls
            .fetch_add(snapshot.reflection_term_fn_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_call_ns, snapshot.reflection_term_call);
        self.reflection_term_call_calls
            .fetch_add(snapshot.reflection_term_call_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_type_ns, snapshot.reflection_term_type);
        self.reflection_term_type_calls
            .fetch_add(snapshot.reflection_term_type_calls, Ordering::Relaxed);
        add_duration(&self.reflection_term_unit_ns, snapshot.reflection_term_unit);
        self.reflection_term_unit_calls
            .fetch_add(snapshot.reflection_term_unit_calls, Ordering::Relaxed);
        add_duration(
            &self.reflection_intrinsic_pair_ns,
            snapshot.reflection_intrinsic_pair,
        );
        self.reflection_intrinsic_pair_calls
            .fetch_add(snapshot.reflection_intrinsic_pair_calls, Ordering::Relaxed);
        add_duration(
            &self.reflection_intrinsic_projection_ns,
            snapshot.reflection_intrinsic_projection,
        );
        self.reflection_intrinsic_projection_calls.fetch_add(
            snapshot.reflection_intrinsic_projection_calls,
            Ordering::Relaxed,
        );
        add_duration(
            &self.reflection_intrinsic_injection_ns,
            snapshot.reflection_intrinsic_injection,
        );
        self.reflection_intrinsic_injection_calls.fetch_add(
            snapshot.reflection_intrinsic_injection_calls,
            Ordering::Relaxed,
        );
        add_duration(
            &self.reflection_intrinsic_either_ns,
            snapshot.reflection_intrinsic_either,
        );
        self.reflection_intrinsic_either_calls.fetch_add(
            snapshot.reflection_intrinsic_either_calls,
            Ordering::Relaxed,
        );
        add_duration(
            &self.reflection_intrinsic_other_ns,
            snapshot.reflection_intrinsic_other,
        );
        self.reflection_intrinsic_other_calls
            .fetch_add(snapshot.reflection_intrinsic_other_calls, Ordering::Relaxed);
        add_duration(
            &self.reflection_structural_recur_ns,
            snapshot.reflection_structural_recur,
        );
        self.reflection_structural_recur_calls.fetch_add(
            snapshot.reflection_structural_recur_calls,
            Ordering::Relaxed,
        );
        add_duration(&self.reflection_other_ns, snapshot.reflection_other);
        self.reflection_other_calls
            .fetch_add(snapshot.reflection_other_calls, Ordering::Relaxed);
        add_duration(&self.type_unfold_ns, snapshot.type_unfold);
        self.type_unfold_calls
            .fetch_add(snapshot.type_unfold_calls, Ordering::Relaxed);
        add_duration(&self.type_equiv_ns, snapshot.type_equiv);
        self.type_equiv_calls
            .fetch_add(snapshot.type_equiv_calls, Ordering::Relaxed);
        add_duration(&self.nf_eq_ns, snapshot.nf_eq);
        self.nf_eq_calls
            .fetch_add(snapshot.nf_eq_calls, Ordering::Relaxed);
        add_duration(&self.eta_contract_ns, snapshot.eta_contract);
        self.eta_contract_calls
            .fetch_add(snapshot.eta_contract_calls, Ordering::Relaxed);
        self.env_clones
            .fetch_add(snapshot.env_clones, Ordering::Relaxed);
        self.closure_builds
            .fetch_add(snapshot.closure_builds, Ordering::Relaxed);
        self.arg_vec_builds
            .fetch_add(snapshot.arg_vec_builds, Ordering::Relaxed);
        self.arg_vec_values
            .fetch_add(snapshot.arg_vec_values, Ordering::Relaxed);
        self.capture_vec_builds
            .fetch_add(snapshot.capture_vec_builds, Ordering::Relaxed);
        self.capture_vec_values
            .fetch_add(snapshot.capture_vec_values, Ordering::Relaxed);
        self.exact_call_memo_hits
            .fetch_add(snapshot.exact_call_memo_hits, Ordering::Relaxed);
        self.exact_call_memo_misses
            .fetch_add(snapshot.exact_call_memo_misses, Ordering::Relaxed);
        self.exact_call_memo_key_skips
            .fetch_add(snapshot.exact_call_memo_key_skips, Ordering::Relaxed);
        self.exact_call_memo_stores
            .fetch_add(snapshot.exact_call_memo_stores, Ordering::Relaxed);
        self.add_function_hotspots(snapshot.function_hotspots);
    }

    pub(super) fn record_root_eval(&self, duration: Duration) {
        add_duration(&self.root_eval_ns, duration);
        self.root_eval_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_expr_visit(&self) {
        self.expr_visits.fetch_add(1, Ordering::Relaxed);
    }

    fn record_apply(&self, duration: Duration) {
        add_duration(&self.apply_ns, duration);
        self.apply_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_apply_generic(&self, duration: Duration) {
        self.record_apply(duration);
        add_duration(&self.apply_generic_ns, duration);
        self.apply_generic_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_apply_atom(&self, duration: Duration) {
        self.record_apply(duration);
        add_duration(&self.apply_atom_ns, duration);
        self.apply_atom_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_apply_fn(&self, duration: Duration) {
        self.record_apply(duration);
        add_duration(&self.apply_fn_ns, duration);
        self.apply_fn_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_function_apply(&self, label: &str, duration: Duration) {
        self.record_apply_fn(duration);
        let mut function_apply = self
            .function_apply
            .lock()
            .expect("eval function metric lock poisoned");
        let entry = function_apply.entry(label.to_string()).or_default();
        entry.duration = saturating_duration_add(entry.duration, duration);
        entry.calls = entry.calls.saturating_add(1);
    }

    fn add_function_hotspots(&self, hotspots: Vec<EvalFunctionHotspot>) {
        if hotspots.is_empty() {
            return;
        }
        let mut function_apply = self
            .function_apply
            .lock()
            .expect("eval function metric lock poisoned");
        for hotspot in hotspots {
            let entry = function_apply.entry(hotspot.label).or_default();
            entry.duration = saturating_duration_add(entry.duration, hotspot.duration);
            entry.calls = entry.calls.saturating_add(hotspot.calls);
        }
    }

    fn function_hotspots(&self) -> Vec<EvalFunctionHotspot> {
        let function_apply = self
            .function_apply
            .lock()
            .expect("eval function metric lock poisoned");
        let mut hotspots: Vec<_> = function_apply
            .iter()
            .map(|(label, metric)| EvalFunctionHotspot {
                label: label.clone(),
                duration: metric.duration,
                calls: metric.calls,
            })
            .collect();
        hotspots.sort_by(|a, b| {
            b.duration
                .cmp(&a.duration)
                .then_with(|| b.calls.cmp(&a.calls))
                .then_with(|| a.label.cmp(&b.label))
        });
        hotspots
    }

    pub(super) fn record_closure_apply(&self, duration: Duration) {
        add_duration(&self.closure_apply_ns, duration);
        self.closure_apply_calls.fetch_add(1, Ordering::Relaxed);
    }

    fn record_reflection(&self, duration: Duration) {
        add_duration(&self.reflection_ns, duration);
        self.reflection_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_reflection_kind(&self, kind: ReflectionMetricKind, duration: Duration) {
        self.record_reflection(duration);
        match kind {
            ReflectionMetricKind::TypeView => {
                add_duration(&self.reflection_type_view_ns, duration);
                self.reflection_type_view_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            ReflectionMetricKind::TypeEqual => {
                add_duration(&self.reflection_type_equal_ns, duration);
                self.reflection_type_equal_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            ReflectionMetricKind::TypeFold => {
                add_duration(&self.reflection_type_fold_ns, duration);
                self.reflection_type_fold_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            ReflectionMetricKind::Term => {
                add_duration(&self.reflection_term_ns, duration);
                self.reflection_term_calls.fetch_add(1, Ordering::Relaxed);
            }
            ReflectionMetricKind::StructuralRecur => {
                add_duration(&self.reflection_structural_recur_ns, duration);
                self.reflection_structural_recur_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            ReflectionMetricKind::Other => {
                add_duration(&self.reflection_other_ns, duration);
                self.reflection_other_calls.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn record_reflection_name(&self, name: &str, duration: Duration) {
        self.record_reflection_kind(reflection_metric_kind(name), duration);
        match reflection_term_metric_kind(name) {
            Some(ReflectionTermMetricKind::Let) => {
                add_duration(&self.reflection_term_let_ns, duration);
                self.reflection_term_let_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::Fn) => {
                add_duration(&self.reflection_term_fn_ns, duration);
                self.reflection_term_fn_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::Call) => {
                add_duration(&self.reflection_term_call_ns, duration);
                self.reflection_term_call_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::Type) => {
                add_duration(&self.reflection_term_type_ns, duration);
                self.reflection_term_type_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::Unit) => {
                add_duration(&self.reflection_term_unit_ns, duration);
                self.reflection_term_unit_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::IntrinsicPair) => {
                add_duration(&self.reflection_intrinsic_pair_ns, duration);
                self.reflection_intrinsic_pair_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::IntrinsicProjection) => {
                add_duration(&self.reflection_intrinsic_projection_ns, duration);
                self.reflection_intrinsic_projection_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::IntrinsicInjection) => {
                add_duration(&self.reflection_intrinsic_injection_ns, duration);
                self.reflection_intrinsic_injection_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::IntrinsicEither) => {
                add_duration(&self.reflection_intrinsic_either_ns, duration);
                self.reflection_intrinsic_either_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            Some(ReflectionTermMetricKind::IntrinsicOther) => {
                add_duration(&self.reflection_intrinsic_other_ns, duration);
                self.reflection_intrinsic_other_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            None => {}
        }
    }

    pub(super) fn record_type_unfold(&self, duration: Duration) {
        add_duration(&self.type_unfold_ns, duration);
        self.type_unfold_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_type_equiv(&self, duration: Duration) {
        add_duration(&self.type_equiv_ns, duration);
        self.type_equiv_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_nf_eq(&self, duration: Duration) {
        add_duration(&self.nf_eq_ns, duration);
        self.nf_eq_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_eta_contract(&self, duration: Duration) {
        add_duration(&self.eta_contract_ns, duration);
        self.eta_contract_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_closure_build(&self) {
        self.closure_builds.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_arg_vec(&self, len: usize) {
        self.arg_vec_builds.fetch_add(1, Ordering::Relaxed);
        self.arg_vec_values.fetch_add(len, Ordering::Relaxed);
    }

    pub(super) fn record_capture_vec(&self, len: usize) {
        self.capture_vec_builds.fetch_add(1, Ordering::Relaxed);
        self.capture_vec_values.fetch_add(len, Ordering::Relaxed);
    }

    pub(super) fn record_exact_call_memo_hit(&self) {
        self.exact_call_memo_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_exact_call_memo_miss(&self) {
        self.exact_call_memo_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_exact_call_memo_key_skip(&self) {
        self.exact_call_memo_key_skips
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_exact_call_memo_store(&self) {
        self.exact_call_memo_stores.fetch_add(1, Ordering::Relaxed);
    }
}

impl EvalMetricsSnapshot {
    pub fn add_assign(&mut self, other: Self) {
        self.root_eval = saturating_duration_add(self.root_eval, other.root_eval);
        self.root_eval_calls = self.root_eval_calls.saturating_add(other.root_eval_calls);
        self.expr_visits = self.expr_visits.saturating_add(other.expr_visits);
        self.apply = saturating_duration_add(self.apply, other.apply);
        self.apply_calls = self.apply_calls.saturating_add(other.apply_calls);
        self.apply_generic = saturating_duration_add(self.apply_generic, other.apply_generic);
        self.apply_generic_calls = self
            .apply_generic_calls
            .saturating_add(other.apply_generic_calls);
        self.apply_atom = saturating_duration_add(self.apply_atom, other.apply_atom);
        self.apply_atom_calls = self.apply_atom_calls.saturating_add(other.apply_atom_calls);
        self.apply_fn = saturating_duration_add(self.apply_fn, other.apply_fn);
        self.apply_fn_calls = self.apply_fn_calls.saturating_add(other.apply_fn_calls);
        self.closure_apply = saturating_duration_add(self.closure_apply, other.closure_apply);
        self.closure_apply_calls = self
            .closure_apply_calls
            .saturating_add(other.closure_apply_calls);
        self.reflection = saturating_duration_add(self.reflection, other.reflection);
        self.reflection_calls = self.reflection_calls.saturating_add(other.reflection_calls);
        self.reflection_type_view =
            saturating_duration_add(self.reflection_type_view, other.reflection_type_view);
        self.reflection_type_view_calls = self
            .reflection_type_view_calls
            .saturating_add(other.reflection_type_view_calls);
        self.reflection_type_equal =
            saturating_duration_add(self.reflection_type_equal, other.reflection_type_equal);
        self.reflection_type_equal_calls = self
            .reflection_type_equal_calls
            .saturating_add(other.reflection_type_equal_calls);
        self.reflection_type_fold =
            saturating_duration_add(self.reflection_type_fold, other.reflection_type_fold);
        self.reflection_type_fold_calls = self
            .reflection_type_fold_calls
            .saturating_add(other.reflection_type_fold_calls);
        self.reflection_term = saturating_duration_add(self.reflection_term, other.reflection_term);
        self.reflection_term_calls = self
            .reflection_term_calls
            .saturating_add(other.reflection_term_calls);
        self.reflection_term_let =
            saturating_duration_add(self.reflection_term_let, other.reflection_term_let);
        self.reflection_term_let_calls = self
            .reflection_term_let_calls
            .saturating_add(other.reflection_term_let_calls);
        self.reflection_term_fn =
            saturating_duration_add(self.reflection_term_fn, other.reflection_term_fn);
        self.reflection_term_fn_calls = self
            .reflection_term_fn_calls
            .saturating_add(other.reflection_term_fn_calls);
        self.reflection_term_call =
            saturating_duration_add(self.reflection_term_call, other.reflection_term_call);
        self.reflection_term_call_calls = self
            .reflection_term_call_calls
            .saturating_add(other.reflection_term_call_calls);
        self.reflection_term_type =
            saturating_duration_add(self.reflection_term_type, other.reflection_term_type);
        self.reflection_term_type_calls = self
            .reflection_term_type_calls
            .saturating_add(other.reflection_term_type_calls);
        self.reflection_term_unit =
            saturating_duration_add(self.reflection_term_unit, other.reflection_term_unit);
        self.reflection_term_unit_calls = self
            .reflection_term_unit_calls
            .saturating_add(other.reflection_term_unit_calls);
        self.reflection_intrinsic_pair = saturating_duration_add(
            self.reflection_intrinsic_pair,
            other.reflection_intrinsic_pair,
        );
        self.reflection_intrinsic_pair_calls = self
            .reflection_intrinsic_pair_calls
            .saturating_add(other.reflection_intrinsic_pair_calls);
        self.reflection_intrinsic_projection = saturating_duration_add(
            self.reflection_intrinsic_projection,
            other.reflection_intrinsic_projection,
        );
        self.reflection_intrinsic_projection_calls = self
            .reflection_intrinsic_projection_calls
            .saturating_add(other.reflection_intrinsic_projection_calls);
        self.reflection_intrinsic_injection = saturating_duration_add(
            self.reflection_intrinsic_injection,
            other.reflection_intrinsic_injection,
        );
        self.reflection_intrinsic_injection_calls = self
            .reflection_intrinsic_injection_calls
            .saturating_add(other.reflection_intrinsic_injection_calls);
        self.reflection_intrinsic_either = saturating_duration_add(
            self.reflection_intrinsic_either,
            other.reflection_intrinsic_either,
        );
        self.reflection_intrinsic_either_calls = self
            .reflection_intrinsic_either_calls
            .saturating_add(other.reflection_intrinsic_either_calls);
        self.reflection_intrinsic_other = saturating_duration_add(
            self.reflection_intrinsic_other,
            other.reflection_intrinsic_other,
        );
        self.reflection_intrinsic_other_calls = self
            .reflection_intrinsic_other_calls
            .saturating_add(other.reflection_intrinsic_other_calls);
        self.reflection_structural_recur = saturating_duration_add(
            self.reflection_structural_recur,
            other.reflection_structural_recur,
        );
        self.reflection_structural_recur_calls = self
            .reflection_structural_recur_calls
            .saturating_add(other.reflection_structural_recur_calls);
        self.reflection_other =
            saturating_duration_add(self.reflection_other, other.reflection_other);
        self.reflection_other_calls = self
            .reflection_other_calls
            .saturating_add(other.reflection_other_calls);
        self.type_unfold = saturating_duration_add(self.type_unfold, other.type_unfold);
        self.type_unfold_calls = self
            .type_unfold_calls
            .saturating_add(other.type_unfold_calls);
        self.type_equiv = saturating_duration_add(self.type_equiv, other.type_equiv);
        self.type_equiv_calls = self.type_equiv_calls.saturating_add(other.type_equiv_calls);
        self.nf_eq = saturating_duration_add(self.nf_eq, other.nf_eq);
        self.nf_eq_calls = self.nf_eq_calls.saturating_add(other.nf_eq_calls);
        self.eta_contract = saturating_duration_add(self.eta_contract, other.eta_contract);
        self.eta_contract_calls = self
            .eta_contract_calls
            .saturating_add(other.eta_contract_calls);
        self.env_clones = self.env_clones.saturating_add(other.env_clones);
        self.closure_builds = self.closure_builds.saturating_add(other.closure_builds);
        self.arg_vec_builds = self.arg_vec_builds.saturating_add(other.arg_vec_builds);
        self.arg_vec_values = self.arg_vec_values.saturating_add(other.arg_vec_values);
        self.capture_vec_builds = self
            .capture_vec_builds
            .saturating_add(other.capture_vec_builds);
        self.capture_vec_values = self
            .capture_vec_values
            .saturating_add(other.capture_vec_values);
        self.exact_call_memo_hits = self
            .exact_call_memo_hits
            .saturating_add(other.exact_call_memo_hits);
        self.exact_call_memo_misses = self
            .exact_call_memo_misses
            .saturating_add(other.exact_call_memo_misses);
        self.exact_call_memo_key_skips = self
            .exact_call_memo_key_skips
            .saturating_add(other.exact_call_memo_key_skips);
        self.exact_call_memo_stores = self
            .exact_call_memo_stores
            .saturating_add(other.exact_call_memo_stores);
        add_hotspots(&mut self.function_hotspots, other.function_hotspots);
    }
}

fn add_hotspots(target: &mut Vec<EvalFunctionHotspot>, other: Vec<EvalFunctionHotspot>) {
    if other.is_empty() {
        return;
    }
    let mut by_label: HashMap<String, EvalFunctionHotspot> = target
        .drain(..)
        .map(|hotspot| (hotspot.label.clone(), hotspot))
        .collect();
    for hotspot in other {
        let entry = by_label
            .entry(hotspot.label.clone())
            .or_insert(EvalFunctionHotspot {
                label: hotspot.label,
                duration: Duration::ZERO,
                calls: 0,
            });
        entry.duration = saturating_duration_add(entry.duration, hotspot.duration);
        entry.calls = entry.calls.saturating_add(hotspot.calls);
    }
    *target = by_label.into_values().collect();
    target.sort_by(|a, b| {
        b.duration
            .cmp(&a.duration)
            .then_with(|| b.calls.cmp(&a.calls))
            .then_with(|| a.label.cmp(&b.label))
    });
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReflectionMetricKind {
    TypeView,
    TypeEqual,
    TypeFold,
    Term,
    StructuralRecur,
    Other,
}

#[derive(Debug, Clone, Copy)]
enum ReflectionTermMetricKind {
    Let,
    Fn,
    Call,
    Type,
    Unit,
    IntrinsicPair,
    IntrinsicProjection,
    IntrinsicInjection,
    IntrinsicEither,
    IntrinsicOther,
}

fn reflection_metric_kind(name: &str) -> ReflectionMetricKind {
    match name {
        "__type_view__" => ReflectionMetricKind::TypeView,
        "__type_equal__"
        | "__type_name_equal__"
        | "__type_var_equal__"
        | "__type_arity_equal__" => ReflectionMetricKind::TypeEqual,
        "__type_product_spine_fold__"
        | "__type_sum_spine_fold__"
        | "__type_args_fold__"
        | "__type_function_params_fold__"
        | "__type_arity_fold__"
        | "__type_name_param_arities_fold__"
        | "__type_instantiate__" => ReflectionMetricKind::TypeFold,
        "__structural_recur__" => ReflectionMetricKind::StructuralRecur,
        _ if name.starts_with("__term_") || name.starts_with("__intrinsic_") => {
            ReflectionMetricKind::Term
        }
        _ => ReflectionMetricKind::Other,
    }
}

fn reflection_term_metric_kind(name: &str) -> Option<ReflectionTermMetricKind> {
    Some(match name {
        "__term_let__" => ReflectionTermMetricKind::Let,
        "__term_fn__" => ReflectionTermMetricKind::Fn,
        "__term_call__" => ReflectionTermMetricKind::Call,
        "__term_type_fn__" | "__term_type_app__" => ReflectionTermMetricKind::Type,
        "__term_unit__" => ReflectionTermMetricKind::Unit,
        "__intrinsic_pair__" => ReflectionTermMetricKind::IntrinsicPair,
        "__intrinsic_fst__" | "__intrinsic_snd__" => ReflectionTermMetricKind::IntrinsicProjection,
        "__intrinsic_left__" | "__intrinsic_right__" => {
            ReflectionTermMetricKind::IntrinsicInjection
        }
        "__intrinsic_either__" => ReflectionTermMetricKind::IntrinsicEither,
        "__intrinsic_absurd__" | "__intrinsic_if_then_else__" => {
            ReflectionTermMetricKind::IntrinsicOther
        }
        _ => return None,
    })
}

fn add_duration(slot: &AtomicU64, duration: Duration) {
    slot.fetch_add(duration_nanos(duration), Ordering::Relaxed);
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn duration_from_nanos(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}

fn saturating_duration_add(left: Duration, right: Duration) -> Duration {
    left.checked_add(right).unwrap_or(Duration::MAX)
}

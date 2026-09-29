use std::collections::HashMap;
#[cfg(feature = "surface")]
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
#[cfg(feature = "surface")]
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crate::ast::Phase;

use super::InternedType;
#[cfg(feature = "surface")]
use super::TypeMemoKey;

#[cfg(feature = "surface")]
use crate::ast::Type;
#[cfg(feature = "surface")]
use crate::ast::UncheckedPrime;

pub struct MemoCtx<P>
where
    P: Phase,
{
    polymorphic_instantiations: Mutex<HashMap<PolymorphicInstantiationKey<P>, InternedType<P>>>,
    polymorphic_stats: MemoStats,
    user_elaborator_timings: UserElaboratorTimings,
    #[cfg(feature = "surface")]
    user_elaborator: SingleflightMemo<UserElaboratorMemoKey, UserElaboratorTemplateResult>,
    #[cfg(feature = "surface")]
    user_elaborator_stats: MemoStats,
    #[cfg(feature = "surface")]
    user_elaborator_prepared: SingleflightMemo<UserElaboratorPreparedKey, PreparedUserElaborator>,
    #[cfg(feature = "surface")]
    user_elaborator_prepared_stats: MemoStats,
    #[cfg(feature = "surface")]
    user_elaborator_prepared_caches: Mutex<HashMap<String, PreparedUserElaboratorCaches>>,
    #[cfg(feature = "surface")]
    user_elaborator_eval_function_cache: crate::normalization::SharedEvalFunctionCache,
    #[cfg(all(feature = "surface", test))]
    user_elaborator_template_executions: AtomicUsize,
    #[cfg(all(feature = "surface", test))]
    user_elaborator_template_execution_sources: Mutex<HashMap<(String, String), usize>>,
}

impl<P> Default for MemoCtx<P>
where
    P: Phase,
{
    fn default() -> Self {
        Self {
            polymorphic_instantiations: Mutex::new(HashMap::new()),
            polymorphic_stats: MemoStats::default(),
            user_elaborator_timings: UserElaboratorTimings::default(),
            #[cfg(feature = "surface")]
            user_elaborator: SingleflightMemo::default(),
            #[cfg(feature = "surface")]
            user_elaborator_stats: MemoStats::default(),
            #[cfg(feature = "surface")]
            user_elaborator_prepared: SingleflightMemo::default(),
            #[cfg(feature = "surface")]
            user_elaborator_prepared_stats: MemoStats::default(),
            #[cfg(feature = "surface")]
            user_elaborator_prepared_caches: Mutex::new(HashMap::new()),
            #[cfg(feature = "surface")]
            user_elaborator_eval_function_cache:
                crate::normalization::SharedEvalFunctionCache::default(),
            #[cfg(all(feature = "surface", test))]
            user_elaborator_template_executions: AtomicUsize::new(0),
            #[cfg(all(feature = "surface", test))]
            user_elaborator_template_execution_sources: Mutex::new(HashMap::new()),
        }
    }
}

impl<P> Drop for MemoCtx<P>
where
    P: Phase,
{
    fn drop(&mut self) {
        if !memo_log_enabled() {
            return;
        }
        #[cfg(feature = "surface")]
        self.user_elaborator_stats.log("user-elaborator");
        #[cfg(feature = "surface")]
        self.user_elaborator_prepared_stats
            .log("user-elaborator-prepared");
        self.polymorphic_stats.log("polymorphic-instantiation");
    }
}

impl<P> MemoCtx<P>
where
    P: Phase,
{
    pub fn snapshot(&self) -> MemoSnapshot {
        let user_elaborator_timings = self.user_elaborator_timings.snapshot();
        #[cfg(feature = "surface")]
        let user_elaborator_timings = {
            let mut user_elaborator_timings = user_elaborator_timings;
            let prepared = self.user_elaborator_prepared_stats.snapshot();
            user_elaborator_timings.prepared_hits = prepared.hits;
            user_elaborator_timings.prepared_misses = prepared.misses;
            user_elaborator_timings.prepared_first_writes = prepared.first_writes;
            user_elaborator_timings
        };
        MemoSnapshot {
            polymorphic_instantiations: self.polymorphic_stats.snapshot(),
            user_elaborator_timings,
            #[cfg(feature = "surface")]
            user_elaborator: self.user_elaborator_stats.snapshot(),
            #[cfg(feature = "surface")]
            user_elaborator_prepared: self.user_elaborator_prepared_stats.snapshot(),
        }
    }

    pub fn record_user_elaborator_template_eval(&self, duration: Duration) {
        self.user_elaborator_timings.template_eval.add(duration);
    }

    #[cfg(all(feature = "surface", test))]
    pub(crate) fn record_user_elaborator_template_execution(&self, module: &str, name: &str) {
        self.user_elaborator_template_executions
            .fetch_add(1, Ordering::Relaxed);
        *self
            .user_elaborator_template_execution_sources
            .lock()
            .expect("template execution observation lock poisoned")
            .entry((module.to_owned(), name.to_owned()))
            .or_default() += 1;
    }

    #[cfg(all(feature = "surface", test))]
    pub(crate) fn user_elaborator_template_executions_for(
        &self,
        module: &str,
        name: &str,
    ) -> usize {
        self.user_elaborator_template_execution_sources
            .lock()
            .expect("template execution observation lock poisoned")
            .get(&(module.to_owned(), name.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    #[cfg(all(feature = "surface", test))]
    pub(crate) fn user_elaborator_template_executions(&self) -> usize {
        self.user_elaborator_template_executions
            .load(Ordering::Relaxed)
    }

    pub fn record_user_elaborator_prepared_eval(&self, duration: Duration) {
        self.user_elaborator_timings.prepared_eval.add(duration);
    }

    #[cfg(feature = "surface")]
    pub fn record_user_elaborator_eval_metrics(
        &self,
        snapshot: crate::normalization::EvalMetricsSnapshot,
    ) {
        self.user_elaborator_timings.eval.add_snapshot(snapshot);
    }

    pub fn record_user_elaborator_template_replay(&self, duration: Duration) {
        self.user_elaborator_timings.template_replay.add(duration);
    }

    pub fn record_user_elaborator_template_memo_total(&self, duration: Duration) {
        self.user_elaborator_timings
            .template_memo_total
            .add(duration);
    }

    pub fn record_user_elaborator_template_batch(&self, obligations: usize, unique: usize) {
        self.user_elaborator_timings
            .template_batch_obligations
            .fetch_add(obligations, Ordering::Relaxed);
        self.user_elaborator_timings
            .template_batch_unique
            .fetch_add(unique, Ordering::Relaxed);
    }

    pub fn record_user_elaborator_template_batch_key(&self, duration: Duration) {
        self.user_elaborator_timings
            .template_batch_key
            .add(duration);
    }

    pub fn record_user_elaborator_template_batch_compute(&self, duration: Duration) {
        self.user_elaborator_timings
            .template_batch_compute
            .add(duration);
    }

    pub fn record_user_elaborator_template_batch_replay_loop(&self, duration: Duration) {
        self.user_elaborator_timings
            .template_batch_replay_loop
            .add(duration);
    }

    pub fn polymorphic_instantiation<F>(
        &self,
        key: PolymorphicInstantiationKey<P>,
        mut compute: F,
    ) -> InternedType<P>
    where
        P: Clone,
        F: FnMut() -> InternedType<P>,
    {
        if let Some((same_representation, cached)) = {
            let map = self
                .polymorphic_instantiations
                .lock()
                .expect("polymorphic memo lock poisoned");
            map.get_key_value(&key).map(|(stored_key, cached)| {
                (stored_key.representation_is_exact(&key), cached.clone())
            })
        } {
            if !same_representation {
                self.polymorphic_stats.miss();
                let fresh = compute();
                assert!(
                    cached == fresh,
                    "polymorphic instantiation memo returned a different semantic type"
                );
                return fresh;
            }
            self.polymorphic_stats.hit();
            if memo_verify_enabled() {
                let fresh = compute();
                assert!(
                    cached == fresh,
                    "polymorphic instantiation memo returned a different semantic type"
                );
            }
            return cached;
        }

        self.polymorphic_stats.miss();
        let fresh = compute();
        let mut map = self
            .polymorphic_instantiations
            .lock()
            .expect("polymorphic memo lock poisoned");
        if let Some((stored_key, existing)) = map.get_key_value(&key) {
            return if stored_key.representation_is_exact(&key) {
                existing.clone()
            } else {
                fresh
            };
        }
        map.insert(key, fresh.clone());
        self.polymorphic_stats.first_write();
        fresh
    }

    #[cfg(feature = "surface")]
    pub fn user_elaborator_template_or_compute<E, F, V>(
        &self,
        key: UserElaboratorMemoKey,
        compute: F,
        verify: V,
    ) -> Result<UserElaboratorTemplateResult, E>
    where
        F: FnOnce() -> Result<UserElaboratorTemplateResult, E>,
        V: Fn(&UserElaboratorTemplateResult),
    {
        self.user_elaborator.get_or_try_compute(
            key,
            &self.user_elaborator_stats,
            compute,
            "user elaborator memo lock poisoned",
            verify,
        )
    }

    #[cfg(feature = "surface")]
    pub fn user_elaborator_prepared_or_try_compute<E, F>(
        &self,
        key: UserElaboratorPreparedKey,
        compute: F,
    ) -> Result<PreparedUserElaborator, E>
    where
        F: FnOnce() -> Result<PreparedUserElaborator, E>,
    {
        self.user_elaborator_prepared.get_or_try_compute(
            key,
            &self.user_elaborator_prepared_stats,
            compute,
            "user elaborator prepared memo lock poisoned",
            |_| {},
        )
    }

    #[cfg(feature = "surface")]
    pub fn user_elaborator_prepared_caches_for_module(
        &self,
        module_path: &str,
    ) -> PreparedUserElaboratorCaches {
        let mut caches = self
            .user_elaborator_prepared_caches
            .lock()
            .expect("user elaborator prepared cache-bundle lock poisoned");
        let entry = caches.entry(module_path.to_owned()).or_default();
        entry.eval_function_cache = self.user_elaborator_eval_function_cache.clone();
        entry.clone()
    }
}

#[derive(Clone)]
pub struct PolymorphicInstantiationKey<P>
where
    P: Phase,
{
    scheme: InternedType<P>,
    type_args: Vec<InternedType<P>>,
}

impl<P> PartialEq for PolymorphicInstantiationKey<P>
where
    P: Phase,
{
    fn eq(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.type_args == other.type_args
    }
}

impl<P> Eq for PolymorphicInstantiationKey<P> where P: Phase {}

impl<P> Hash for PolymorphicInstantiationKey<P>
where
    P: Phase,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.scheme.hash(state);
        self.type_args.hash(state);
    }
}

impl<P> PolymorphicInstantiationKey<P>
where
    P: Phase + Clone,
{
    pub fn new(scheme: InternedType<P>, type_args: Vec<InternedType<P>>) -> Self {
        Self::try_new(scheme, type_args).expect(
            "an open type-inference goal cannot enter the package-wide polymorphic-instantiation memo; close and zonk its owning domain first",
        )
    }

    pub fn try_new(scheme: InternedType<P>, type_args: Vec<InternedType<P>>) -> Option<Self> {
        if super::type_contains_goal(scheme.as_type())
            || type_args
                .iter()
                .any(|arg| super::type_contains_goal(arg.as_type()))
        {
            return None;
        }
        Some(Self { scheme, type_args })
    }

    fn representation_is_exact(&self, other: &Self) -> bool {
        self.scheme.representation_is_exact(&other.scheme)
            && self.type_args.len() == other.type_args.len()
            && self
                .type_args
                .iter()
                .zip(&other.type_args)
                .all(|(left, right)| left.representation_is_exact(right))
    }
}

#[cfg(feature = "surface")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UserElaboratorMemoKey {
    module_path: String,
    elaborator_name: String,
    inputs: Vec<UserElaboratorMemoInputKey>,
}

#[cfg(feature = "surface")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum UserElaboratorMemoInputKey {
    ComptimeProof,
    Type(TypeMemoKey),
    OptionalTypeKnown(TypeMemoKey),
    OptionalTypeInfer,
    Term(TypeMemoKey),
    ResidualShape(TypeMemoKey),
    CapturedTerm(TypeMemoKey),
}

#[cfg(feature = "surface")]
impl UserElaboratorMemoKey {
    pub(crate) fn new(
        module_path: String,
        elaborator_name: String,
        inputs: Vec<UserElaboratorMemoInputKey>,
    ) -> Self {
        Self {
            module_path,
            elaborator_name,
            inputs,
        }
    }
}

#[cfg(feature = "surface")]
#[derive(Clone, Debug)]
pub struct UserElaboratorTemplate {
    pub result_ty: Type<UncheckedPrime>,
    pub checked: crate::normalization::CheckedTerm,
}

#[cfg(feature = "surface")]
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UserElaboratorTemplateGeneratedImport {
    pub module_path: String,
    pub alias: String,
}

#[cfg(feature = "surface")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct UserElaboratorArtifactKey {
    elaborator_module_path: String,
    elaborator_name: String,
    capture_imports: Vec<UserElaboratorTemplateGeneratedImport>,
}

#[cfg(feature = "surface")]
impl UserElaboratorArtifactKey {
    pub(crate) fn new(
        elaborator_module_path: String,
        elaborator_name: String,
        capture_imports: Vec<UserElaboratorTemplateGeneratedImport>,
    ) -> Self {
        Self {
            elaborator_module_path,
            elaborator_name,
            capture_imports,
        }
    }
}

#[cfg(feature = "surface")]
pub(crate) struct UserElaboratorEvalArtifactPair {
    pub(crate) artifact: std::sync::Arc<crate::normalization::EvalArtifact>,
    pub(crate) implementation: crate::ast::Expr<UncheckedPrime>,
}

#[cfg(feature = "surface")]
#[derive(Default)]
pub(crate) struct UserElaboratorArtifactMemo {
    entries:
        SingleflightMemo<UserElaboratorArtifactKey, std::sync::Arc<UserElaboratorEvalArtifactPair>>,
    stats: MemoStats,
}

#[cfg(feature = "surface")]
impl UserElaboratorArtifactMemo {
    pub(crate) fn get_or_try_compute<E, F, V>(
        &self,
        key: UserElaboratorArtifactKey,
        compute: F,
        verify: V,
    ) -> Result<std::sync::Arc<UserElaboratorEvalArtifactPair>, E>
    where
        F: FnOnce() -> Result<UserElaboratorEvalArtifactPair, E>,
        V: Fn(&std::sync::Arc<UserElaboratorEvalArtifactPair>),
    {
        self.entries.get_or_try_compute(
            key,
            &self.stats,
            || compute().map(std::sync::Arc::new),
            "user elaborator artifact memo lock poisoned",
            verify,
        )
    }

    #[cfg(any(feature = "cli", test))]
    pub(crate) fn snapshot(&self) -> CacheSnapshot {
        self.stats.snapshot()
    }
}

#[cfg(feature = "surface")]
pub type UserElaboratorTemplateResult = Result<UserElaboratorTemplate, String>;

#[cfg(feature = "surface")]
#[derive(Clone, Default)]
pub struct PreparedUserElaboratorCaches {
    pub(crate) fn_closure_cache: crate::normalization::SharedFnClosureCache,
    pub(crate) eval_function_cache: crate::normalization::SharedEvalFunctionCache,
    pub(crate) type_view_cache: crate::normalization::SharedTypeViewCache,
    pub(crate) exact_call_memo_cache: crate::normalization::SharedExactCallMemoCache,
}

#[cfg(feature = "surface")]
#[derive(Clone)]
pub struct PreparedUserElaborator {
    pub(crate) value: crate::normalization::Value,
    pub(crate) fn_closure_cache: crate::normalization::SharedFnClosureCache,
    pub(crate) eval_function_cache: crate::normalization::SharedEvalFunctionCache,
    pub(crate) type_view_cache: crate::normalization::SharedTypeViewCache,
    pub(crate) exact_call_memo_cache: crate::normalization::SharedExactCallMemoCache,
}

#[cfg(feature = "surface")]
impl PreparedUserElaborator {
    pub(crate) fn new(
        value: crate::normalization::Value,
        fn_closure_cache: crate::normalization::SharedFnClosureCache,
        eval_function_cache: crate::normalization::SharedEvalFunctionCache,
        type_view_cache: crate::normalization::SharedTypeViewCache,
        exact_call_memo_cache: crate::normalization::SharedExactCallMemoCache,
    ) -> Self {
        Self {
            value,
            fn_closure_cache,
            eval_function_cache,
            type_view_cache,
            exact_call_memo_cache,
        }
    }
}

#[cfg(feature = "surface")]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserElaboratorPreparedPurpose {
    Template,
}

#[cfg(feature = "surface")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UserElaboratorPreparedKey {
    module_path: String,
    elaborator_name: String,
    purpose: UserElaboratorPreparedPurpose,
}

#[cfg(feature = "surface")]
impl UserElaboratorPreparedKey {
    pub(crate) fn new(
        module_path: String,
        elaborator_name: String,
        purpose: UserElaboratorPreparedPurpose,
    ) -> Self {
        Self {
            module_path,
            elaborator_name,
            purpose,
        }
    }
}

pub struct MemoSnapshot {
    pub polymorphic_instantiations: CacheSnapshot,
    pub user_elaborator_timings: UserElaboratorTimingSnapshot,
    #[cfg(feature = "surface")]
    pub user_elaborator: CacheSnapshot,
    #[cfg(feature = "surface")]
    pub user_elaborator_prepared: CacheSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSnapshot {
    pub hits: usize,
    pub misses: usize,
    pub first_writes: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserElaboratorTimingSnapshot {
    pub prepared_eval: Duration,
    pub prepared_hits: usize,
    pub prepared_misses: usize,
    pub prepared_first_writes: usize,
    pub artifact_hits: usize,
    pub artifact_misses: usize,
    pub artifact_first_writes: usize,
    pub template_eval: Duration,
    pub template_replay: Duration,
    pub template_memo_total: Duration,
    pub template_batch_obligations: usize,
    pub template_batch_unique: usize,
    pub template_batch_key: Duration,
    pub template_batch_compute: Duration,
    pub template_batch_replay_loop: Duration,
    #[cfg(feature = "surface")]
    pub eval: crate::normalization::EvalMetricsSnapshot,
}

#[cfg(feature = "surface")]
struct SingleflightMemo<K, V> {
    state: Mutex<SingleflightMemoState<K, V>>,
    ready: Condvar,
}

#[cfg(feature = "surface")]
struct SingleflightMemoState<K, V> {
    values: HashMap<K, V>,
    inflight: HashSet<K>,
}

#[cfg(feature = "surface")]
impl<K, V> Default for SingleflightMemoState<K, V> {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
            inflight: HashSet::new(),
        }
    }
}

#[cfg(feature = "surface")]
impl<K, V> Default for SingleflightMemo<K, V> {
    fn default() -> Self {
        Self {
            state: Mutex::new(SingleflightMemoState::default()),
            ready: Condvar::new(),
        }
    }
}

#[cfg(feature = "surface")]
struct SingleflightGuard<'a, K: Eq + Hash, V> {
    memo: &'a SingleflightMemo<K, V>,
    key: Option<K>,
}

#[derive(Default)]
struct MemoStats {
    hits: AtomicUsize,
    misses: AtomicUsize,
    first_writes: AtomicUsize,
}

#[derive(Default)]
struct UserElaboratorTimings {
    prepared_eval: AtomicDuration,
    template_eval: AtomicDuration,
    template_replay: AtomicDuration,
    template_memo_total: AtomicDuration,
    template_batch_obligations: AtomicUsize,
    template_batch_unique: AtomicUsize,
    template_batch_key: AtomicDuration,
    template_batch_compute: AtomicDuration,
    template_batch_replay_loop: AtomicDuration,
    #[cfg(feature = "surface")]
    eval: crate::normalization::EvalMetrics,
}

#[derive(Default)]
struct AtomicDuration {
    nanos: AtomicU64,
}

impl MemoStats {
    fn hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    fn miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    fn first_write(&self) {
        self.first_writes.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> CacheSnapshot {
        CacheSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            first_writes: self.first_writes.load(Ordering::Relaxed),
        }
    }

    fn log(&self, name: &str) {
        let snapshot = self.snapshot();
        eprintln!(
            "memo {name}: hits={} misses={} first-writes={}",
            snapshot.hits, snapshot.misses, snapshot.first_writes
        );
    }
}

#[cfg(feature = "surface")]
impl<K, V> SingleflightMemo<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    fn get_or_try_compute<E, F, Verify>(
        &self,
        key: K,
        stats: &MemoStats,
        compute: F,
        lock_message: &str,
        verify: Verify,
    ) -> Result<V, E>
    where
        F: FnOnce() -> Result<V, E>,
        Verify: Fn(&V),
    {
        let mut compute = Some(compute);
        loop {
            let mut state = self.state.lock().expect(lock_message);
            if let Some(cached) = state.values.get(&key).cloned() {
                stats.hit();
                drop(state);
                if memo_verify_enabled() {
                    verify(&cached);
                }
                return Ok(cached);
            }
            if state.inflight.insert(key.clone()) {
                stats.miss();
                break;
            }
            drop(self.ready.wait(state).expect(lock_message));
        }

        let mut guard = SingleflightGuard {
            memo: self,
            key: Some(key.clone()),
        };
        let computed = compute.take().expect("singleflight compute used once")();
        let mut state = self.state.lock().expect(lock_message);
        let result = match computed {
            Ok(value) => {
                let value = match state.values.entry(key.clone()) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.get().clone(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(value.clone());
                        stats.first_write();
                        value
                    }
                };
                Ok(value)
            }
            Err(error) => Err(error),
        };
        let removed = state.inflight.remove(&key);
        debug_assert!(removed);
        drop(state);
        self.ready.notify_all();
        guard.key = None;
        result
    }
}

#[cfg(feature = "surface")]
impl<K, V> Drop for SingleflightGuard<'_, K, V>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        if let Ok(mut state) = self.memo.state.lock() {
            state.inflight.remove(&key);
        }
        self.memo.ready.notify_all();
    }
}

impl UserElaboratorTimings {
    fn snapshot(&self) -> UserElaboratorTimingSnapshot {
        UserElaboratorTimingSnapshot {
            prepared_eval: self.prepared_eval.snapshot(),
            prepared_hits: 0,
            prepared_misses: 0,
            prepared_first_writes: 0,
            artifact_hits: 0,
            artifact_misses: 0,
            artifact_first_writes: 0,
            template_eval: self.template_eval.snapshot(),
            template_replay: self.template_replay.snapshot(),
            template_memo_total: self.template_memo_total.snapshot(),
            template_batch_obligations: self.template_batch_obligations.load(Ordering::Relaxed),
            template_batch_unique: self.template_batch_unique.load(Ordering::Relaxed),
            template_batch_key: self.template_batch_key.snapshot(),
            template_batch_compute: self.template_batch_compute.snapshot(),
            template_batch_replay_loop: self.template_batch_replay_loop.snapshot(),
            #[cfg(feature = "surface")]
            eval: self.eval.snapshot(),
        }
    }
}

impl AtomicDuration {
    fn add(&self, duration: Duration) {
        self.nanos
            .fetch_add(duration_nanos_u64(duration), Ordering::Relaxed);
    }

    fn snapshot(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

fn duration_nanos_u64(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
thread_local! {
    // Test-only stand-in for the `KIO_DEBUG_MEMO_VERIFY` variable
    // (installed via `tests::MemoVerifyVar`): the crate forbids
    // `unsafe_code` and edition-2024 `std::env::set_var` is unsafe, so
    // a test cannot set the real variable. It carries the variable's
    // *value* rather than a parsed bool so `memo_verify_enabled`'s
    // parse stays on the tested path.
    static MEMO_VERIFY_VAR_OVERRIDE: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

pub fn memo_verify_enabled() -> bool {
    #[cfg(test)]
    let value = MEMO_VERIFY_VAR_OVERRIDE
        .with(std::cell::Cell::get)
        .map(str::to_owned);
    #[cfg(not(test))]
    let value = std::env::var("KIO_DEBUG_MEMO_VERIFY").ok();
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

fn memo_log_enabled() -> bool {
    std::env::var("KIO_DEBUG_MEMO")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "surface")]
    use crate::ast::{Expr, UncheckedPrime};
    use crate::ast::{Lowered, Meta, Type};
    #[cfg(feature = "surface")]
    use crate::ast::{TypeGoalDomain, TypeGoalOwner, TypeGoalRef, TypeGoalSlot};
    #[cfg(feature = "surface")]
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_core::TypeInterner;
    use crate::span::Span;
    #[cfg(feature = "surface")]
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(feature = "surface")]
    use std::sync::{Arc, Barrier};
    #[cfg(feature = "surface")]
    use std::time::Duration;

    fn unit() -> Type<Lowered> {
        Type::Unit {
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    fn bottom() -> Type<Lowered> {
        Type::Bottom {
            meta: Meta::new(Span::new(0, 0)),
        }
    }

    #[cfg(feature = "surface")]
    fn open_goal() -> Type<Lowered> {
        Type::Goal {
            goal: TypeGoalRef::new(
                TypeGoalOwner::new(
                    TypeGoalDomain::new(crate::ast::TypeGoalStoreIdentity::process(7), 0),
                    0,
                ),
                TypeGoalSlot::from_index(0),
            ),
            args: Vec::new(),
            meta: Meta::new(Span::new(0, 0)),
            ext: (),
        }
    }

    #[cfg(feature = "surface")]
    #[test]
    fn polymorphic_instantiation_key_rejects_open_goals_in_scheme_and_arguments() {
        let interner = TypeInterner::<Lowered>::default();
        let unit = interner.intern(&unit());
        let goal = interner.intern(&open_goal());

        assert!(PolymorphicInstantiationKey::try_new(goal.clone(), vec![unit.clone()]).is_none());
        assert!(PolymorphicInstantiationKey::try_new(unit.clone(), vec![goal.clone()]).is_none());
        assert!(
            std::panic::catch_unwind(|| { PolymorphicInstantiationKey::new(goal, vec![unit]) })
                .is_err()
        );
    }

    /// Installs a thread-local stand-in value for
    /// `KIO_DEBUG_MEMO_VERIFY` for the duration of a test, restoring
    /// the prior override on drop so nested guards and panicking tests
    /// cannot leak the flag into later assertions on this thread.
    struct MemoVerifyVar {
        prior: Option<&'static str>,
    }

    impl MemoVerifyVar {
        fn set(value: &'static str) -> Self {
            let prior = MEMO_VERIFY_VAR_OVERRIDE.with(|cell| cell.replace(Some(value)));
            Self { prior }
        }
    }

    impl Drop for MemoVerifyVar {
        fn drop(&mut self) {
            MEMO_VERIFY_VAR_OVERRIDE.with(|cell| cell.set(self.prior));
        }
    }

    #[cfg(feature = "surface")]
    fn user_elaborator_key() -> UserElaboratorMemoKey {
        let ty = unit();
        let ty_key = TypeMemoKey::from_type(&ty);
        UserElaboratorMemoKey::new(
            "elaborators".to_owned(),
            "id".to_owned(),
            vec![
                UserElaboratorMemoInputKey::Type(ty_key.clone()),
                UserElaboratorMemoInputKey::OptionalTypeKnown(ty_key),
            ],
        )
    }

    #[cfg(feature = "surface")]
    fn user_elaborator_prepared_key() -> UserElaboratorPreparedKey {
        UserElaboratorPreparedKey::new(
            "elaborators".to_owned(),
            "id".to_owned(),
            UserElaboratorPreparedPurpose::Template,
        )
    }

    #[cfg(feature = "surface")]
    fn empty_user_elaborator_artifact() -> UserElaboratorEvalArtifactPair {
        UserElaboratorEvalArtifactPair {
            artifact: std::sync::Arc::new(
                crate::normalization::EvalArtifact::from_validated_parts(
                    Package::from_parts(std::collections::BTreeMap::new(), None),
                    crate::normalization::EvalPrimitiveEnv::default(),
                ),
            ),
            implementation: Expr::<UncheckedPrime>::Unit {
                occurrence: Default::default(),
                meta: Meta::new(Span::new(0, 0)),
            },
        }
    }

    #[test]
    fn polymorphic_instantiation_records_miss_write_then_hit() {
        let interner = TypeInterner::<Lowered>::default();
        let memo = MemoCtx::<Lowered>::default();
        let ty = interner.intern(&unit());
        let key = PolymorphicInstantiationKey::new(ty.clone(), Vec::new());

        let first = memo.polymorphic_instantiation(key.clone(), || ty.clone());
        let second = memo.polymorphic_instantiation(key, || ty.clone());

        assert_eq!(first, second);
        let stats = memo.snapshot().polymorphic_instantiations;
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.first_writes, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn polymorphic_instantiation_presentation_miss_retains_the_callers_exact_view() {
        let interner = TypeInterner::<Lowered>::default();
        let memo = MemoCtx::<Lowered>::default();
        let first_scheme = Type::Unit {
            meta: Meta::new(Span::new(1, 2)),
        };
        let second_scheme = Type::Unit {
            meta: Meta::new(Span::new(10, 11)),
        };
        let first_key =
            PolymorphicInstantiationKey::new(interner.intern(&first_scheme), Vec::new());
        let second_key =
            PolymorphicInstantiationKey::new(interner.intern(&second_scheme), Vec::new());
        let first_view = Type::Unit {
            meta: Meta::new(Span::new(3, 4)),
        };
        let second_view = Type::Unit {
            meta: Meta::new(Span::new(30, 31)),
        };
        let computes = std::cell::Cell::new(0usize);

        let first = memo.polymorphic_instantiation(first_key, || {
            computes.set(computes.get() + 1);
            interner.intern(&first_view)
        });
        let second = memo.polymorphic_instantiation(second_key, || {
            computes.set(computes.get() + 1);
            interner.intern(&second_view)
        });

        assert!(
            first.ptr_eq(&second),
            "the semantic memo node remains shared"
        );
        assert_eq!(first.as_type().span(), Span::new(3, 4));
        assert_eq!(second.as_type().span(), Span::new(30, 31));
        assert_eq!(computes.get(), 2);
        let stats = memo.snapshot().polymorphic_instantiations;
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.first_writes, 1);
        assert_eq!(stats.hits, 0);
    }

    #[test]
    fn memo_verify_gate_parses_injected_variable_shapes() {
        {
            let _var = MemoVerifyVar::set("1");
            assert!(memo_verify_enabled());
        }
        {
            let _var = MemoVerifyVar::set("yes");
            assert!(memo_verify_enabled());
        }
        {
            let _var = MemoVerifyVar::set("0");
            assert!(!memo_verify_enabled());
        }
        {
            let _var = MemoVerifyVar::set("");
            assert!(!memo_verify_enabled());
        }
        assert!(
            !memo_verify_enabled(),
            "dropping a guard should restore the prior (absent) override"
        );
    }

    #[test]
    fn memo_verify_on_recomputes_memo_hits() {
        let _var = MemoVerifyVar::set("1");
        let interner = TypeInterner::<Lowered>::default();
        let memo = MemoCtx::<Lowered>::default();
        let ty = interner.intern(&unit());
        let key = PolymorphicInstantiationKey::new(ty.clone(), Vec::new());
        let computes = std::cell::Cell::new(0usize);

        let first = memo.polymorphic_instantiation(key.clone(), || {
            computes.set(computes.get() + 1);
            ty.clone()
        });
        assert_eq!(computes.get(), 1);
        let second = memo.polymorphic_instantiation(key, || {
            computes.set(computes.get() + 1);
            ty.clone()
        });

        assert_eq!(first, second);
        assert_eq!(
            computes.get(),
            2,
            "the verify path should recompute on the memo hit"
        );
        let stats = memo.snapshot().polymorphic_instantiations;
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn memo_verify_off_skips_hit_recompute() {
        let _var = MemoVerifyVar::set("0");
        let interner = TypeInterner::<Lowered>::default();
        let memo = MemoCtx::<Lowered>::default();
        let ty = interner.intern(&unit());
        let key = PolymorphicInstantiationKey::new(ty.clone(), Vec::new());
        let computes = std::cell::Cell::new(0usize);

        let first = memo.polymorphic_instantiation(key.clone(), || {
            computes.set(computes.get() + 1);
            ty.clone()
        });
        let second = memo.polymorphic_instantiation(key, || {
            computes.set(computes.get() + 1);
            ty.clone()
        });

        assert_eq!(first, second);
        assert_eq!(
            computes.get(),
            1,
            "with verify off, the memo hit should not recompute"
        );
    }

    #[test]
    #[should_panic(expected = "polymorphic instantiation memo returned a different semantic type")]
    fn memo_verify_panics_on_recompute_mismatch() {
        let _var = MemoVerifyVar::set("1");
        let interner = TypeInterner::<Lowered>::default();
        let memo = MemoCtx::<Lowered>::default();
        let unit_ty = interner.intern(&unit());
        let bottom_ty = interner.intern(&bottom());
        let key = PolymorphicInstantiationKey::new(unit_ty.clone(), Vec::new());

        let _ = memo.polymorphic_instantiation(key.clone(), || unit_ty.clone());
        let _ = memo.polymorphic_instantiation(key, || bottom_ty.clone());
    }

    #[cfg(feature = "surface")]
    #[test]
    fn artifact_memo_verify_on_checks_hits() {
        let _var = MemoVerifyVar::set("1");
        let memo = UserElaboratorArtifactMemo::default();
        let key =
            UserElaboratorArtifactKey::new("elaborators".to_owned(), "id".to_owned(), Vec::new());
        let verifies = std::cell::Cell::new(0usize);

        memo.get_or_try_compute(
            key.clone(),
            || Ok::<_, ()>(empty_user_elaborator_artifact()),
            |_| verifies.set(verifies.get() + 1),
        )
        .expect("cold artifact memo entry");
        memo.get_or_try_compute(
            key,
            || Ok::<_, ()>(empty_user_elaborator_artifact()),
            |_| verifies.set(verifies.get() + 1),
        )
        .expect("warm artifact memo entry");

        assert_eq!(verifies.get(), 1, "the artifact hit must be verified");
    }

    #[cfg(feature = "surface")]
    #[test]
    fn user_elaborator_template_singleflight_shares_parallel_cold_miss() {
        let memo = Arc::new(MemoCtx::<Lowered>::default());
        let key = user_elaborator_key();
        let compute_calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let memo = Arc::clone(&memo);
                let key = key.clone();
                let compute_calls = Arc::clone(&compute_calls);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    memo.user_elaborator_template_or_compute(
                        key,
                        || {
                            compute_calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            Ok::<UserElaboratorTemplateResult, ()>(Err("cached".to_owned()))
                        },
                        |_| {},
                    )
                    .expect("singleflight compute should succeed")
                })
            })
            .collect();

        for handle in handles {
            match handle.join().expect("worker should not panic") {
                Err(message) => assert_eq!(message, "cached"),
                Ok(_) => panic!("test value should be a cached user-elaborator diagnostic"),
            }
        }
        assert_eq!(compute_calls.load(Ordering::SeqCst), 1);
        let stats = memo.snapshot().user_elaborator;
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.first_writes, 1);
        assert_eq!(stats.hits, 7);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn user_elaborator_prepared_singleflight_shares_parallel_cold_miss() {
        let memo = Arc::new(MemoCtx::<Lowered>::default());
        let key = user_elaborator_prepared_key();
        let compute_calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let memo = Arc::clone(&memo);
                let key = key.clone();
                let compute_calls = Arc::clone(&compute_calls);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    memo.user_elaborator_prepared_or_try_compute(key, || {
                        compute_calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok::<_, ()>(PreparedUserElaborator::new(
                            crate::normalization::Value::Unit,
                            crate::normalization::SharedFnClosureCache::default(),
                            crate::normalization::SharedEvalFunctionCache::default(),
                            crate::normalization::SharedTypeViewCache::default(),
                            crate::normalization::SharedExactCallMemoCache::default(),
                        ))
                    })
                    .expect("prepared singleflight compute should succeed")
                })
            })
            .collect();

        for handle in handles {
            assert!(matches!(
                handle.join().expect("worker should not panic").value,
                crate::normalization::Value::Unit
            ));
        }
        assert_eq!(compute_calls.load(Ordering::SeqCst), 1);
        let snapshot = memo.snapshot();
        assert_eq!(snapshot.user_elaborator_prepared.misses, 1);
        assert_eq!(snapshot.user_elaborator_prepared.first_writes, 1);
        assert_eq!(snapshot.user_elaborator_prepared.hits, 7);
        assert_eq!(snapshot.user_elaborator_timings.prepared_misses, 1);
        assert_eq!(snapshot.user_elaborator_timings.prepared_first_writes, 1);
        assert_eq!(snapshot.user_elaborator_timings.prepared_hits, 7);
    }

    #[cfg(feature = "surface")]
    #[test]
    fn user_elaborator_prepared_caches_share_compiled_functions_package_wide() {
        let memo = MemoCtx::<Lowered>::default();

        let first = memo.user_elaborator_prepared_caches_for_module("elaborators");
        let second = memo.user_elaborator_prepared_caches_for_module("elaborators");
        let other = memo.user_elaborator_prepared_caches_for_module("other_elaborators");

        assert!(
            first
                .eval_function_cache
                .ptr_eq(&second.eval_function_cache)
        );
        assert!(first.eval_function_cache.ptr_eq(&other.eval_function_cache));
        assert!(
            first
                .exact_call_memo_cache
                .ptr_eq(&second.exact_call_memo_cache)
        );
        assert!(
            !first
                .exact_call_memo_cache
                .ptr_eq(&other.exact_call_memo_cache)
        );
        let cache_count = memo
            .user_elaborator_prepared_caches
            .lock()
            .expect("user elaborator prepared cache-bundle lock poisoned")
            .len();
        assert_eq!(cache_count, 2);
    }
}

//! Finite, call-owned inference domains.
//!
//! Open [`Type::Goal`] nodes are meaningful only together with one
//! [`GoalStore`].  A speculative equation accumulates in [`GoalDelta`];
//! [`GoalStore::prepare_owner_with_publication`] validates and normalizes a
//! complete owner before [`GoalStore::commit_prepared_owner`] publishes it.
//! This keeps a failed equation or failed owner close from publishing a
//! partial solution.
//!
//! This module is the storage and first-order unification substrate.  The
//! application planner decides when owners are opened and which equations are
//! offered; the store does not schedule expression checking.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ast::{
    Kind, Lowered, Meta, PathSegment, Type, TypeGoalDomain, TypeGoalOwner, TypeGoalRef,
    TypeGoalSlot, TypeGoalStoreIdentity, TypeParam, UncheckedPrime,
};
use crate::error::Error;
use crate::span::Span;

use super::aliases::{
    AliasBinderLookup, has_unfoldable_alias_frontier_for_comparison,
    unfold_alias_frontier_for_comparison, unfold_alias_frontier_for_comparison_with_binder_lookup,
};
#[cfg(test)]
use super::canonicalize_for_comparison;
use super::persistent_exact::PersistentExactNameMap;
use super::types::{
    NominalHeadBinding, collect_free_type_vars, fresh_type_var_with, resolve_nominal_head_kind,
    type_contains_goal,
};
use super::{InternedType, RetainedTypeBinder, TypeCtx, append_type_args, subst_type};

#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct PublicationValidationWork {
    full: usize,
    goal_free_retained: usize,
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_VALIDATION_WORK: std::cell::Cell<PublicationValidationWork> =
        const { std::cell::Cell::new(PublicationValidationWork {
            full: 0,
            goal_free_retained: 0,
        }) };
}

#[cfg(test)]
fn record_full_publication_validation() {
    PUBLICATION_VALIDATION_WORK.with(|work| {
        let mut work = work.get();
        work.full += 1;
        PUBLICATION_VALIDATION_WORK.set(work);
    });
}

#[cfg(test)]
fn record_goal_free_retained_publication() {
    PUBLICATION_VALIDATION_WORK.with(|work| {
        let mut work = work.get();
        work.goal_free_retained += 1;
        PUBLICATION_VALIDATION_WORK.set(work);
    });
}

#[cfg(test)]
pub(crate) fn reset_publication_validation_work() {
    PUBLICATION_VALIDATION_WORK.with(|work| work.set(PublicationValidationWork::default()));
}

#[cfg(test)]
pub(crate) fn publication_validation_work() -> (usize, usize) {
    PUBLICATION_VALIDATION_WORK.with(|work| {
        let work = work.get();
        (work.full, work.goal_free_retained)
    })
}

/// Proof identity for one rigid type binder.
///
/// Ambient binders use a monotonic identity allocated by their `TypeCtx`.
/// Alpha binders exist only while two `Forall` bodies are compared.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RigidIdentity {
    Ambient(super::env::TypeBinderId),
    Alpha(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum GoalContextCapability {
    Production(super::env::TypeContextId),
    #[cfg(test)]
    Test(u64),
}

impl fmt::Debug for GoalContextCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoalContextCapability(<opaque>)")
    }
}

impl GoalContextCapability {
    pub(crate) const fn isolated_specialization() -> Self {
        Self::Production(super::env::TypeContextId::isolated_specialization())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RigidBinding {
    kind: Kind,
    identity: RigidIdentity,
}

/// Rigid binders whose proof identities are in scope for one open type.
///
/// The store nonce makes a scope a capability of this particular transient
/// inference store. The nonce and all alpha identities disappear when every
/// goal closes; neither is serialized or otherwise observable.
#[derive(Clone)]
pub(crate) struct RigidScope {
    store_nonce: TypeGoalStoreIdentity,
    context: GoalContextCapability,
    module_path: String,
    bindings: RigidBindings,
}

type RigidBindings = PersistentExactNameMap<RigidBinding>;

#[derive(Clone)]
struct ProjectedScopeContribution {
    scope: RigidScope,
    contains_goal: bool,
}

impl ProjectedScopeContribution {
    fn merged(&self, other: &Self) -> Option<Self> {
        let contains_goal = self.contains_goal || other.contains_goal;
        let scope = if other.contains_goal && !self.contains_goal {
            other.scope.merged(&self.scope).ok()?
        } else {
            self.scope.merged(&other.scope).ok()?
        };
        Some(Self {
            scope,
            contains_goal,
        })
    }
}

#[cfg(test)]
thread_local! {
    static RIGID_SCOPE_NAME_SNAPSHOTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RIGID_SCOPE_NAMES_COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROJECTED_PUBLIC_FORALL_BODY_REBUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROJECTED_SCOPE_TYPE_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static GOAL_STORE_WORK: std::cell::Cell<GoalStoreWorkState> = const {
        std::cell::Cell::new(GoalStoreWorkState {
            observed: GoalStoreWork {
                owners_opened: 0,
                owners_committed: 0,
                owners_discarded: 0,
                goals_reserved: 0,
                equation_attempts: 0,
                max_live_domains: 0,
                max_live_owners: 0,
                max_live_goals: 0,
                max_owner_depth: 0,
            },
            live_domains: 0,
            live_owners: 0,
            live_goals: 0,
            enabled: false,
        })
    };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GoalStoreWork {
    pub(crate) owners_opened: usize,
    pub(crate) owners_committed: usize,
    pub(crate) owners_discarded: usize,
    pub(crate) goals_reserved: usize,
    pub(crate) equation_attempts: usize,
    pub(crate) max_live_domains: usize,
    pub(crate) max_live_owners: usize,
    pub(crate) max_live_goals: usize,
    pub(crate) max_owner_depth: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct GoalStoreWorkState {
    observed: GoalStoreWork,
    live_domains: usize,
    live_owners: usize,
    live_goals: usize,
    enabled: bool,
}

#[cfg(test)]
pub(crate) fn reset_goal_store_work() {
    GOAL_STORE_WORK.with(|work| {
        work.set(GoalStoreWorkState {
            observed: GoalStoreWork::default(),
            live_domains: 0,
            live_owners: 0,
            live_goals: 0,
            enabled: true,
        });
    });
}

#[cfg(test)]
fn goal_store_work() -> GoalStoreWork {
    GOAL_STORE_WORK.with(|work| work.get().observed)
}

#[cfg(test)]
pub(crate) fn finish_goal_store_work() -> GoalStoreWork {
    let observed = goal_store_work();
    GOAL_STORE_WORK.with(|work| {
        let mut current = work.get();
        current.enabled = false;
        work.set(current);
    });
    observed
}

#[cfg(test)]
fn record_goal_store_work(update: impl FnOnce(&mut GoalStoreWorkState)) {
    GOAL_STORE_WORK.with(|work| {
        let mut current = work.get();
        if !current.enabled {
            return;
        }
        update(&mut current);
        current.observed.max_live_domains =
            current.observed.max_live_domains.max(current.live_domains);
        current.observed.max_live_owners =
            current.observed.max_live_owners.max(current.live_owners);
        current.observed.max_live_goals = current.observed.max_live_goals.max(current.live_goals);
        work.set(current);
    });
}

#[cfg(test)]
fn record_owner_open(root: bool, depth: usize) {
    record_goal_store_work(|work| {
        work.observed.owners_opened += 1;
        work.live_domains += usize::from(root);
        work.live_owners += 1;
        work.observed.max_owner_depth = work.observed.max_owner_depth.max(depth);
    });
}

#[cfg(test)]
fn record_owner_close(root: bool, goals: usize) {
    record_goal_store_work(|work| {
        work.observed.owners_committed += 1;
        work.live_domains -= usize::from(root);
        work.live_owners -= 1;
        work.live_goals -= goals;
    });
}

#[cfg(test)]
fn record_owner_discard(goals: usize) {
    record_goal_store_work(|work| {
        work.observed.owners_discarded += 1;
        work.live_owners -= 1;
        work.live_goals -= goals;
    });
}

#[cfg(test)]
fn record_goal_reserved() {
    record_goal_store_work(|work| {
        work.observed.goals_reserved += 1;
        work.live_goals += 1;
    });
}

#[cfg(test)]
fn record_equation_attempt() {
    record_goal_store_work(|work| work.observed.equation_attempts += 1);
}

#[cfg(test)]
fn reset_rigid_scope_name_snapshots() {
    RIGID_SCOPE_NAME_SNAPSHOTS.with(|count| count.set(0));
    RIGID_SCOPE_NAMES_COPIED.with(|count| count.set(0));
}

#[cfg(test)]
fn rigid_scope_name_snapshots() -> usize {
    RIGID_SCOPE_NAME_SNAPSHOTS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn rigid_scope_names_copied() -> usize {
    RIGID_SCOPE_NAMES_COPIED.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_projected_public_forall_body_rebuilds() {
    PROJECTED_PUBLIC_FORALL_BODY_REBUILDS.with(|count| count.set(0));
}

#[cfg(test)]
fn projected_public_forall_body_rebuilds() -> usize {
    PROJECTED_PUBLIC_FORALL_BODY_REBUILDS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_projected_scope_type_walks() {
    PROJECTED_SCOPE_TYPE_WALKS.with(|count| count.set(0));
}

#[cfg(test)]
fn projected_scope_type_walks() -> usize {
    PROJECTED_SCOPE_TYPE_WALKS.with(std::cell::Cell::get)
}

impl fmt::Debug for RigidScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RigidScope")
            .field("module_path", &self.module_path)
            .field("binding_count", &self.bindings.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for RigidScope {
    fn eq(&self, other: &Self) -> bool {
        self.store_nonce == other.store_nonce
            && self.context == other.context
            && self.module_path == other.module_path
            && self
                .bindings
                .visible_eq(&other.bindings, |left, right| left == right)
    }
}

impl Eq for RigidScope {}

impl RigidScope {
    fn empty(
        store_nonce: TypeGoalStoreIdentity,
        context: GoalContextCapability,
        module_path: String,
    ) -> Self {
        Self {
            store_nonce,
            context,
            module_path,
            bindings: RigidBindings::default(),
        }
    }

    fn from_type_ctx(store_nonce: TypeGoalStoreIdentity, tcx: &TypeCtx<'_, '_, Lowered>) -> Self {
        let mut scope = Self::empty(
            store_nonce,
            GoalContextCapability::Production(tcx.context_id()),
            tcx.env.module_path.clone(),
        );
        for (name, kind, binder_id) in tcx.in_scope_type_param_bindings() {
            scope.insert_ambient(name.to_owned(), kind.clone(), binder_id);
        }
        scope
    }

    fn for_isolated_specialization(&self) -> Self {
        Self {
            store_nonce: TypeGoalStoreIdentity::ISOLATED,
            context: GoalContextCapability::isolated_specialization(),
            module_path: self.module_path.clone(),
            bindings: self.bindings.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::empty(
            TypeGoalStoreIdentity::UNSTAMPED_TEST,
            GoalContextCapability::Test(1),
            "<test>".to_owned(),
        )
    }

    #[cfg(test)]
    fn with_test_context(mut self, context: u64, module_path: impl Into<String>) -> Self {
        self.context = GoalContextCapability::Test(context);
        self.module_path = module_path.into();
        self
    }

    #[cfg(test)]
    fn from_bindings(bindings: impl IntoIterator<Item = (String, Kind, u64)>) -> Self {
        let mut scope = Self::new();
        for (name, kind, binder_id) in bindings {
            scope.insert_ambient(name, kind, super::env::TypeBinderId::for_test(binder_id));
        }
        scope
    }

    #[cfg(test)]
    fn with_binding(mut self, name: impl Into<String>, kind: Kind, binder_id: u64) -> Self {
        self.insert_ambient(name, kind, super::env::TypeBinderId::for_test(binder_id));
        self
    }

    #[cfg(test)]
    fn stamped_for_test(mut self, store_nonce: TypeGoalStoreIdentity) -> Self {
        assert!(
            self.store_nonce == TypeGoalStoreIdentity::UNSTAMPED_TEST,
            "test rigid scope was already attached to a goal store"
        );
        self.store_nonce = store_nonce;
        self
    }

    fn with_lexical_forall(mut self, param: &TypeParam) -> Self {
        self.bindings = self.bindings.push(
            param.name.clone(),
            RigidBinding {
                kind: param.effective_kind(),
                identity: RigidIdentity::Alpha(param.name.clone()),
            },
            |left, right| left == right,
        );
        self
    }

    fn without_lexical_foralls(mut self, params: &[TypeParam]) -> Self {
        for param in params.iter().rev() {
            let removed = self
                .bindings
                .remove(param.name.as_str())
                .expect("specialization pattern scope omitted a leading forall binder");
            assert_eq!(
                removed,
                RigidBinding {
                    kind: param.effective_kind(),
                    identity: RigidIdentity::Alpha(param.name.clone()),
                },
                "specialization pattern scope carried the wrong leading forall proof"
            );
        }
        self
    }

    fn with_alpha(mut self, name: String, kind: Kind) -> Self {
        self.insert(
            name.clone(),
            RigidBinding {
                kind,
                identity: RigidIdentity::Alpha(name),
            },
        );
        self
    }

    fn insert_ambient(
        &mut self,
        name: impl Into<String>,
        kind: Kind,
        binder_id: super::env::TypeBinderId,
    ) {
        self.insert(
            name,
            RigidBinding {
                kind,
                identity: RigidIdentity::Ambient(binder_id),
            },
        );
    }

    /// Derive one exact lexical prefix without rescanning the mutable
    /// `TypeCtx`. The persistent exact-name map shares every unchanged trie
    /// branch with the preceding prefix, including the shadow chain for a
    /// repeated spelling.
    pub(crate) fn with_retained_binders(
        mut self,
        binders: &[super::env::RetainedTypeBinder<'_>],
    ) -> Self {
        for binder in binders {
            self.insert_ambient(
                binder.param().name.clone(),
                binder.param().effective_kind(),
                binder.id(),
            );
        }
        self
    }

    /// Keep only the exact rigid proofs actually referenced by a goal-free
    /// type. This is the sparse lexical evidence for a logical lambda slice;
    /// it carries no owner or fill authority.
    fn projected_for_goal_free_type(&self, ty: &Type<Lowered>) -> Self {
        debug_assert!(!super::type_contains_goal(ty));
        self.projected_contribution_for_type(ty, &mut ProjectedTypeCloseScratch::default())
            .scope
    }

    /// Retain the full lexical authority needed by embedded goals, or only
    /// the exact rigid proofs used by a goal-free projected occurrence.
    fn projected_contribution_for_type(
        &self,
        ty: &Type<Lowered>,
        scratch: &mut ProjectedTypeCloseScratch,
    ) -> ProjectedScopeContribution {
        #[cfg(test)]
        PROJECTED_SCOPE_TYPE_WALKS.with(|count| count.set(count.get() + 1));
        scratch.free_rigids.clear();
        scratch.bound_rigids.clear();
        let mut contains_goal = false;
        let mut bindings = RigidBindings::default();
        for_each_type_event(ty, true, &mut |event| match event {
            TypeWalkEvent::Goal(_) => contains_goal = true,
            TypeWalkEvent::SingleSegmentPath(name)
                if scratch.bound_rigids.get(name).copied().unwrap_or_default() == 0
                    && scratch.free_rigids.insert(name.to_owned()) =>
            {
                if let Some(binding) = self.binding(name) {
                    bindings = std::mem::take(&mut bindings).push(
                        name.to_owned(),
                        binding.clone(),
                        |left, right| left == right,
                    );
                }
            }
            TypeWalkEvent::EnterBinder(name) => {
                *scratch.bound_rigids.entry(name.to_owned()).or_default() += 1;
            }
            TypeWalkEvent::ExitBinder(name) => {
                let count = scratch
                    .bound_rigids
                    .get_mut(name)
                    .expect("projected scope traversal exited a binder it did not enter");
                *count -= 1;
            }
            TypeWalkEvent::SingleSegmentPath(_) => {}
        });
        debug_assert!(scratch.bound_rigids.values().all(|count| *count == 0));
        if contains_goal {
            return ProjectedScopeContribution {
                scope: self.clone(),
                contains_goal,
            };
        }
        ProjectedScopeContribution {
            scope: Self {
                store_nonce: self.store_nonce,
                context: self.context,
                module_path: self.module_path.clone(),
                bindings,
            },
            contains_goal,
        }
    }

    /// Check that every free single-segment use keeps its exact meaning in a
    /// composed projected scope. Comparing `Option` is intentional: an
    /// unbound nominal head must not become a same-spelled rigid variable.
    fn projected_free_uses_match(
        &self,
        ty: &Type<Lowered>,
        composed: &Self,
        additional: Option<&Self>,
        scratch: &mut ProjectedTypeCloseScratch,
    ) -> bool {
        #[cfg(test)]
        PROJECTED_SCOPE_TYPE_WALKS.with(|count| count.set(count.get() + 1));
        let mut matches = true;
        for_each_free_type_var(
            ty,
            true,
            &mut scratch.free_rigids,
            &mut scratch.bound_rigids,
            &mut |name| {
                matches &= self.binding(name) == composed.binding(name)
                    && additional
                        .map(|scope| self.binding(name) == scope.binding(name))
                        .unwrap_or(true);
            },
        );
        matches
    }

    fn insert(&mut self, name: impl Into<String>, binding: RigidBinding) {
        let name = name.into();
        self.bindings = self
            .bindings
            .push(name, binding, |left, right| left == right);
    }

    pub(crate) fn kind(&self, name: &str) -> Option<Kind> {
        self.binding(name).map(|binding| binding.kind.clone())
    }

    fn binding(&self, name: &str) -> Option<&RigidBinding> {
        self.bindings.get(name)
    }

    fn extends(&self, parent: &Self) -> bool {
        self.store_nonce == parent.store_nonce
            && self.context == parent.context
            && self.module_path == parent.module_path
            && self
                .bindings
                .extends(&parent.bindings, |left, right| left == right)
    }

    fn merged(&self, other: &Self) -> Result<Self, ()> {
        if self.store_nonce != other.store_nonce
            || self.context != other.context
            || self.module_path != other.module_path
        {
            return Err(());
        }
        Ok(Self {
            store_nonce: self.store_nonce,
            context: self.context,
            module_path: self.module_path.clone(),
            bindings: self
                .bindings
                .merged(&other.bindings, |left, right| left == right)?,
        })
    }

    fn shares_history_with(&self, other: &Self) -> bool {
        self.store_nonce == other.store_nonce
            && self.context == other.context
            && self.module_path == other.module_path
            && self.bindings.shares_root_with(&other.bindings)
    }
}

impl AliasBinderLookup for RigidScope {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.binding(name).is_some()
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        self.bindings.for_each(|name, _| visit(name));
    }
}

struct CombinedRigidScopes<'a> {
    first: &'a RigidScope,
    second: &'a RigidScope,
}

impl AliasBinderLookup for CombinedRigidScopes<'_> {
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.first.binding(name).is_some() || self.second.binding(name).is_some()
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        self.first.for_each_alias_binder(visit);
        self.second.bindings.for_each(|name, _| {
            if self.first.binding(name).is_none() {
                visit(name);
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EquationSide {
    Found,
    Expected,
}

/// A type paired with the rigid binders that give its bare variables meaning.
#[derive(Clone, Debug)]
pub(crate) struct ScopedType {
    ty: InternedType<Lowered>,
    scope: RigidScope,
}

/// Immutable, authority-free input for reflecting one already goal-free
/// projected type through the evaluator's explicit package artifact.
pub(crate) struct ProjectedReflectionCandidate {
    pub(crate) ty: Type<UncheckedPrime>,
    pub(crate) identity_is_canonical: bool,
    pub(crate) module_path: String,
    pub(crate) binder_kinds: HashMap<String, Kind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopedTypeEdge {
    ProductLeft,
    ProductRight,
    SumLeft,
    SumRight,
    FunctionParam,
    FunctionReturn,
    ForallBody,
    PathArgument(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScopedTypeBinaryKind {
    Product,
    Sum,
    Function { abi_arity: usize },
}

/// Hashable identity for one open type within its transient inference
/// frontier. This key may contain call-local goal and binder capabilities and
/// must not enter persistent or cross-frontier memo state.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TransientScopedTypeKey {
    ty: super::TransientTypeKey,
    identity_canonical: bool,
    store_nonce: TypeGoalStoreIdentity,
    context: GoalContextCapability,
    module_path: String,
    bindings: Vec<(String, RigidBinding)>,
}

/// One by-value expected equation operand. The optional written view cannot
/// enter a reusable [`ScopedType`]: only the equation boundary may consume it
/// for a mismatch diagnostic before solving against `solver`.
pub(crate) struct ExpectedEquationOperand {
    solver: ScopedType,
    written: Option<InternedType<Lowered>>,
}

struct RawScopedType {
    ty: Type<Lowered>,
    scope: RigidScope,
    identity_canonical: bool,
}

#[derive(Clone, Copy)]
struct ScopedTypeView<'a> {
    ty: &'a Type<Lowered>,
    scope: &'a RigidScope,
    identity_canonical: bool,
    requirement_source: Option<&'a std::sync::Arc<super::intern::RequirementSource>>,
}

/// The canonical whole equation that caused a recursive solver descent.
///
/// Structural descent may alpha-rename binders or isolate a leaf before it
/// discovers a mismatch. Retaining borrowed views of the original equation
/// lets that failure report the user-facing types without cloning solver
/// trees on the successful path.
#[derive(Clone, Copy)]
struct EquationProvenance<'a> {
    found: ScopedTypeView<'a>,
    expected: ScopedTypeView<'a>,
}

impl<'a> ScopedTypeView<'a> {
    fn from_scoped(scoped: &'a ScopedType) -> Self {
        Self {
            ty: scoped.ty.as_type(),
            scope: &scoped.scope,
            identity_canonical: scoped.ty.identity_is_canonical(),
            requirement_source: scoped.ty.requirement_source(),
        }
    }

    fn child(self, ty: &'a Type<Lowered>) -> Self {
        Self { ty, ..self }
    }

    fn materialize(self) -> ScopedType {
        ScopedType::new(
            InternedType::fresh_with_identity(self.ty.clone(), self.identity_canonical)
                .with_requirement_source(self.requirement_source.cloned()),
            self.scope.clone(),
        )
    }
}

impl EquationProvenance<'_> {
    fn mismatch(
        self,
        store: &GoalStore,
        delta: &mut GoalDelta,
        ctx: &impl GoalTypeContext,
        span: Span,
    ) -> Error {
        delta.mark_relation_incompatible();
        let found = store.materialize_diagnostic_view(self.found, delta);
        let expected = store.materialize_diagnostic_view(self.expected, delta);
        ctx.mismatch(&found, &expected, span)
    }
}

impl RawScopedType {
    fn into_scoped(self) -> ScopedType {
        ScopedType::from_parts(self.ty, self.scope, self.identity_canonical)
    }
}

impl ScopedType {
    fn new(ty: InternedType<Lowered>, scope: RigidScope) -> Self {
        Self { ty, scope }
    }

    fn fresh(ty: Type<Lowered>, scope: RigidScope) -> Self {
        Self::new(InternedType::fresh(ty), scope)
    }

    pub(crate) fn ty(&self) -> &InternedType<Lowered> {
        &self.ty
    }

    pub(crate) fn canonicalized_for_comparison(&self, tcx: &TypeCtx<'_, '_, Lowered>) -> Self {
        let (ty, identity_canonical) =
            super::aliases::canonicalize_deep_for_comparison_with_binder_lookup(
                self.ty.as_type(),
                &tcx.env.alias_ctx(),
                &self.scope,
                self.ty.identity_is_canonical(),
            );
        Self::new(
            InternedType::fresh_with_identity(ty, identity_canonical),
            self.scope.clone(),
        )
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        self.ty == other.ty && self.scope == other.scope
    }

    pub(crate) fn projected_scope_compatible(&self, other: &Self) -> bool {
        self.projected_scope_with(other).is_some()
    }

    fn projected_scope_for<'a>(
        first: &'a Self,
        operands: impl Clone + Iterator<Item = (&'a Self, bool)>,
    ) -> Option<RigidScope> {
        let mut shares_history = true;
        let mut all_contribute = true;
        for (operand, contributes) in operands.clone() {
            shares_history &= first.scope.shares_history_with(&operand.scope);
            all_contribute &= contributes;
        }
        if shares_history {
            return Some(first.scope.clone());
        }
        let mut authenticated_scope = first.scope.clone();
        for (operand, _) in operands.clone() {
            authenticated_scope = authenticated_scope.merged(&operand.scope).ok()?;
        }

        let mut scratch = ProjectedTypeCloseScratch::default();
        let mut authenticated = first
            .scope
            .projected_contribution_for_type(first.ty.as_type(), &mut scratch);
        let mut output = (!all_contribute).then(|| authenticated.clone());
        for (operand, contributes) in operands.clone() {
            let next = operand
                .scope
                .projected_contribution_for_type(operand.ty.as_type(), &mut scratch);
            authenticated = authenticated.merged(&next)?;
            if contributes && !all_contribute {
                output = Some(match output {
                    Some(output) => output.merged(&next)?,
                    None => next,
                });
            }
        }
        let authenticated = authenticated.scope;
        let output = output.map(|output| output.scope);
        let result = output.as_ref().unwrap_or(&authenticated);
        if !first.scope.projected_free_uses_match(
            first.ty.as_type(),
            &authenticated,
            (!all_contribute).then_some(result),
            &mut scratch,
        ) {
            return None;
        }
        for (operand, contributes) in operands {
            if !operand.scope.projected_free_uses_match(
                operand.ty.as_type(),
                &authenticated,
                (!all_contribute && contributes).then_some(result),
                &mut scratch,
            ) {
                return None;
            }
        }
        Some(output.unwrap_or(authenticated))
    }

    /// Authenticate the original full scopes, then derive the smallest scope
    /// that preserves the meaning of both projected occurrences. Goal-bearing
    /// types keep their complete owner authority; goal-free types contribute
    /// only proofs for rigid names they actually use.
    fn projected_scope_with(&self, other: &Self) -> Option<RigidScope> {
        Self::projected_scope_for(self, std::iter::once((other, true)))
    }

    #[cfg(test)]
    pub(crate) fn transient_key(&self) -> TransientScopedTypeKey {
        let mut bindings = Vec::with_capacity(self.scope.bindings.len());
        self.scope
            .bindings
            .for_each(|name, binding| bindings.push((name.to_owned(), binding.clone())));
        bindings.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        TransientScopedTypeKey {
            ty: super::TransientTypeKey::from_type(self.ty.as_type()),
            identity_canonical: self.ty.identity_is_canonical(),
            store_nonce: self.scope.store_nonce,
            context: self.scope.context,
            module_path: self.scope.module_path.clone(),
            bindings,
        }
    }

    pub(crate) fn projected_reflection_candidate(&self) -> Option<ProjectedReflectionCandidate> {
        if type_contains_goal(self.ty.as_type()) || super::type_contains_infer(self.ty.as_type()) {
            return None;
        }
        let mut binder_kinds = HashMap::new();
        self.scope.bindings.for_each(|name, binding| {
            binder_kinds.insert(name.to_owned(), binding.kind.clone());
        });
        Some(ProjectedReflectionCandidate {
            ty: crate::ast::convert_type::<Lowered, UncheckedPrime>(self.ty.as_type()),
            identity_is_canonical: self.ty.identity_is_canonical(),
            module_path: self.scope.module_path.clone(),
            binder_kinds,
        })
    }

    pub(crate) fn projected_structural_child(&self, edge: ScopedTypeEdge) -> Option<Self> {
        let (child, scope) = match (edge, self.ty.as_type()) {
            (ScopedTypeEdge::ProductLeft, Type::Product { left, .. }) => {
                (left.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::ProductRight, Type::Product { right, .. }) => {
                (right.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::SumLeft, Type::Sum { left, .. }) => {
                (left.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::SumRight, Type::Sum { right, .. }) => {
                (right.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::FunctionParam, Type::Function { param, .. }) => {
                (param.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::FunctionReturn, Type::Function { ret, .. }) => {
                (ret.as_ref(), self.scope.clone())
            }
            (ScopedTypeEdge::ForallBody, Type::Forall { param, body, .. }) => {
                (body.as_ref(), self.scope.clone().with_lexical_forall(param))
            }
            (ScopedTypeEdge::PathArgument(index), Type::Path { args, .. }) => {
                (args.get(index)?, self.scope.clone())
            }
            _ => return None,
        };
        Some(Self::new(
            InternedType::fresh_with_identity(child.clone(), self.ty.identity_is_canonical()),
            scope,
        ))
    }

    pub(crate) fn projected_binary_peer(
        &self,
        other: &Self,
        kind: ScopedTypeBinaryKind,
        span: Span,
    ) -> Option<Self> {
        let scope = self.projected_scope_with(other)?;
        let meta = Meta::new(span);
        let ty = match kind {
            ScopedTypeBinaryKind::Product => Type::Product {
                left: Box::new(self.ty.clone_type()),
                right: Box::new(other.ty.clone_type()),
                meta,
            },
            ScopedTypeBinaryKind::Sum => Type::Sum {
                left: Box::new(self.ty.clone_type()),
                right: Box::new(other.ty.clone_type()),
                meta,
            },
            ScopedTypeBinaryKind::Function { abi_arity } => Type::Function {
                param: Box::new(self.ty.clone_type()),
                ret: Box::new(other.ty.clone_type()),
                meta,
                abi_arity,
                caps: (),
            },
        };
        Some(Self::new(
            InternedType::fresh_with_identity(
                ty,
                self.ty.identity_is_canonical() && other.ty.identity_is_canonical(),
            ),
            scope,
        ))
    }

    pub(crate) fn projected_path_apply(&self, argument: &Self) -> Option<Self> {
        let scope = self.projected_scope_with(argument)?;
        let mut ty = self.ty.clone_type();
        let Type::Path { args, .. } = &mut ty else {
            return None;
        };
        args.push(argument.ty.clone_type());
        Some(Self::new(
            InternedType::fresh_with_identity(
                ty,
                self.ty.identity_is_canonical() && argument.ty.identity_is_canonical(),
            ),
            scope,
        ))
    }

    pub(crate) fn projected_forall_abstract(&self, param: TypeParam, span: Span) -> Option<Self> {
        if self.scope.binding(&param.name).is_some() {
            return None;
        }
        Some(Self::new(
            InternedType::fresh_with_identity(
                Type::Forall {
                    param,
                    body: Box::new(self.ty.clone_type()),
                    meta: Meta::new(span),
                },
                self.ty.identity_is_canonical(),
            ),
            self.scope.clone(),
        ))
    }

    pub(crate) fn projected_closed_peer(&self, ty: InternedType<Lowered>) -> Self {
        assert!(
            ty.identity_is_canonical(),
            "a projected closed peer must carry canonical nominal identity"
        );
        assert!(
            !type_contains_goal(ty.as_type()),
            "a projected closed peer must be goal-free"
        );
        Self::new(ty, self.scope.clone())
    }

    pub(crate) fn projected_rigid_path_arity(&self) -> Option<usize> {
        let Type::Path { segments, .. } = self.ty.as_type() else {
            return None;
        };
        let [name] = segments.as_slice() else {
            return None;
        };
        self.scope.kind(name.as_str()).map(|kind| kind.arity())
    }

    pub(crate) fn projected_instantiate_forall(&self, argument: &Self) -> Option<Self> {
        if !argument.ty.identity_is_canonical() {
            return None;
        }
        let Type::Forall { param, body, .. } = self.ty.as_type() else {
            return None;
        };
        let argument_is_used = self.scope.shares_history_with(&argument.scope) || {
            let mut free = HashSet::new();
            collect_free_type_vars(body, &mut free);
            free.contains(&param.name)
        };
        let scope = Self::projected_scope_for(self, std::iter::once((argument, argument_is_used)))?;
        if goal_refs(argument.ty.as_type())
            .iter()
            .any(|goal| !goal.domain().belongs_to(scope.store_nonce))
        {
            return None;
        }
        let ty = subst_type(
            body,
            &HashMap::from([(param.name.clone(), argument.ty.clone_type())]),
        );
        Some(Self::new(
            InternedType::fresh_with_identity(ty, self.ty.identity_is_canonical()),
            scope,
        ))
    }

    /// Apply one written public-call forall prefix with its exact use-site scopes.
    pub(crate) fn projected_public_forall_prefix_application(
        &self,
        arguments: &[Self],
    ) -> Option<Self> {
        if arguments.is_empty() {
            return Some(self.clone());
        }
        let mut body = self.ty.as_type();
        let mut shares_history = true;
        let mut substitutions = HashMap::with_capacity(arguments.len());
        let mut effective_arguments = HashMap::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            let Type::Forall {
                param, body: next, ..
            } = body
            else {
                return None;
            };
            shares_history &= self.scope.shares_history_with(&argument.scope);
            if goal_refs(argument.ty.as_type())
                .iter()
                .any(|goal| !goal.domain().belongs_to(self.scope.store_nonce))
            {
                return None;
            }
            substitutions.insert(param.name.clone(), argument.ty.clone_type());
            effective_arguments.insert(
                param.name.clone(),
                (index, argument.ty.identity_is_canonical()),
            );
            body = next;
        }
        let mut free = HashSet::new();
        collect_free_type_vars(body, &mut free);
        let mut identity_is_canonical = self.ty.identity_is_canonical();
        let mut effective = (!shares_history).then(|| vec![false; arguments.len()]);
        for (name, (index, canonical)) in &effective_arguments {
            if free.contains(name) {
                identity_is_canonical &= *canonical;
                if let Some(effective) = &mut effective {
                    effective[*index] = true;
                }
            }
        }
        let scope = if let Some(effective) = effective {
            Self::projected_scope_for(
                self,
                arguments
                    .iter()
                    .zip(&effective)
                    .map(|(argument, effective)| (argument, *effective)),
            )?
        } else {
            self.scope.clone()
        };
        #[cfg(test)]
        PROJECTED_PUBLIC_FORALL_BODY_REBUILDS.with(|count| count.set(count.get() + 1));
        let ty = subst_type(body, &substitutions);
        Some(Self::new(
            InternedType::fresh_with_identity(ty, identity_is_canonical),
            scope,
        ))
    }

    #[cfg(test)]
    pub(crate) fn projected_for_test(ty: Type<Lowered>) -> Self {
        Self::new(InternedType::fresh_canonical(ty), RigidScope::new())
    }

    #[cfg(test)]
    pub(crate) fn projected_noncanonical_for_test(
        ty: Type<Lowered>,
        module_path: impl Into<String>,
        bindings: Vec<(String, Kind, u64)>,
    ) -> Self {
        Self::new(
            InternedType::fresh(ty),
            RigidScope::from_bindings(bindings).with_test_context(1, module_path),
        )
    }

    #[cfg(test)]
    pub(crate) fn projected_canonical_peer_for_test(&self, ty: Type<Lowered>) -> Self {
        Self::new(InternedType::fresh_canonical(ty), self.scope.clone())
    }

    pub(crate) fn into_interned_type(self) -> InternedType<Lowered> {
        self.ty
    }

    /// Strengthen an occurrence to a descendant lexical scope only when every
    /// free rigid keeps the same proof identity there. Closed subtrees and
    /// exact nominals can therefore cross intermediate owner scopes without
    /// laundering a same-spelled shadowed binder.
    fn strengthen_scope_if_compatible(mut self, scope: &RigidScope) -> Self {
        let mut free = HashSet::new();
        collect_free_type_vars(self.ty.as_type(), &mut free);
        if free
            .iter()
            .all(|name| scope.binding(name) == self.scope.binding(name))
        {
            self.scope = scope.clone();
        }
        self
    }

    fn from_parts(ty: Type<Lowered>, scope: RigidScope, identity_canonical: bool) -> Self {
        Self {
            ty: InternedType::fresh_with_identity(ty, identity_canonical),
            scope,
        }
    }

    #[cfg(test)]
    fn stamped_for_test(mut self, store_nonce: TypeGoalStoreIdentity) -> Self {
        if self.scope.store_nonce == TypeGoalStoreIdentity::UNSTAMPED_TEST {
            self.scope = self.scope.stamped_for_test(store_nonce);
        }
        self
    }
}

impl ExpectedEquationOperand {
    pub(crate) fn solver(solver: ScopedType) -> Self {
        Self {
            solver,
            written: None,
        }
    }

    pub(crate) fn with_written(solver: ScopedType, written: InternedType<Lowered>) -> Self {
        Self {
            solver,
            written: Some(written),
        }
    }

    pub(crate) fn solver_type(&self) -> &ScopedType {
        &self.solver
    }

    #[cfg(test)]
    fn stamped_for_test(mut self, store_nonce: TypeGoalStoreIdentity) -> Self {
        self.solver = self.solver.stamped_for_test(store_nonce);
        self
    }

    fn into_parts(self) -> (ScopedType, Option<ScopedType>) {
        let Self { solver, written } = self;
        let diagnostic = written.map(|ty| ScopedType::new(ty, solver.scope.clone()));
        (solver, diagnostic)
    }
}

impl From<ScopedType> for ExpectedEquationOperand {
    fn from(solver: ScopedType) -> Self {
        Self::solver(solver)
    }
}

/// Read-only close preparation that cannot feed a publication sink.
///
/// Only [`GoalStore`] can compose or consume this typestate. A projected header
/// export may expose its type without publishing the unfinished source value.
#[derive(Clone, Debug)]
pub(crate) struct PreparedCloseType {
    delta: GoalDeltaAuthority,
    owner: TypeGoalOwner,
    value: ScopedType,
}

/// A completed derivation blocked only by a goal in its retained ancestor chain.
/// This witnesses suspension, never a solved type or publication permission.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompletionWait {
    delta: GoalDeltaAuthority,
    origin: TypeGoalOwner,
    deepest: TypeGoalRef,
}

/// One canonical structural shell whose children were prepared under the
/// same exact owner delta. Keeping construction inside `GoalStore` prevents a
/// caller from rebuilding a mixed-provenance type around retained binders.
pub(crate) enum PreparedTypeShell {
    Function {
        abi_arity: usize,
        span: Span,
    },
    Product {
        span: Span,
    },
    Sum {
        span: Span,
    },
    Path {
        segments: Vec<PathSegment>,
        span: Span,
    },
    Goal {
        goal: TypeGoalRef,
        span: Span,
    },
}

/// Opaque process-unique identity of one speculative delta. Token zero is
/// reserved for the sole delta of a confined isolated specialization store.
#[derive(Clone, Copy, PartialEq, Eq)]
struct GoalDeltaToken(u64);

const _: () = assert!(std::mem::size_of::<GoalDeltaToken>() == std::mem::size_of::<u64>());

impl fmt::Debug for GoalDeltaToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoalDeltaToken(<opaque>)")
    }
}

/// Copyable authority proving identity with one exact speculative delta.
///
/// The token itself remains private to the goal store. Publication planning
/// may retain and compare this wrapper, but cannot mint one independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GoalDeltaAuthority(GoalDeltaToken);

/// One parent-scoped structural header whose descendant goal heads have been
/// reserved but not yet related to their source goals.
///
/// Reservation and installation are deliberately separate. A projected
/// source tree reserves every sibling's parent goals before any child delta
/// receives an equation, so no partial header can become semantic input while
/// the rest of the tree is still allocating its authority.
pub(crate) struct ReservedParentHeaderExport {
    child_delta: GoalDeltaAuthority,
    child: ScopedType,
    parent: ScopedType,
}

#[cfg(all(target_pointer_width = "64", not(test)))]
const _: () = assert!(std::mem::size_of::<ReservedParentHeaderExport>() == 184);
#[cfg(all(target_pointer_width = "64", test))]
const _: () = assert!(std::mem::size_of::<ReservedParentHeaderExport>() == 200);

impl ReservedParentHeaderExport {
    /// The exact child-side operand of this reserved header relation.
    pub(crate) fn child_header(&self) -> &ScopedType {
        &self.child
    }

    /// Borrow the parent-scoped view without installing this export.
    ///
    /// A nested tree uses this view to reserve the next ancestor's goal heads;
    /// only after every level has reserved may it consume the artifacts and
    /// install their child-local equations from the innermost level outward.
    pub(crate) fn parent_header(&self) -> &ScopedType {
        &self.parent
    }

    /// Test whether this artifact belongs to one exact retained child edge.
    /// The opaque process-unique delta token is comparison-only; a match does
    /// not grant installation authority or expose the token to the caller.
    pub(crate) fn matches_child_delta(&self, child_delta: &GoalDelta) -> bool {
        self.child_delta == child_delta.authority()
    }
}

#[derive(Default)]
pub(crate) struct ProjectedTypeCloseScratch {
    free_rigids: HashSet<String>,
    bound_rigids: HashMap<String, u32>,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<ProjectedTypeCloseScratch>() == 96);

/// Exact, goal-free type evidence produced by the projected recipe closer.
///
/// The inner scoped type stays private so a caller cannot relabel an arbitrary
/// goal-free-looking value as close evidence. Recipe construction may borrow
/// the closed type for reflection, while coherence validation consumes only
/// this authenticated result and never closes the occurrence a second time.
pub(crate) struct ClosedProjectedType(ScopedType);

const _: () =
    assert!(std::mem::size_of::<ClosedProjectedType>() == std::mem::size_of::<ScopedType>());

impl ClosedProjectedType {
    pub(crate) fn ty(&self) -> &InternedType<Lowered> {
        self.0.ty()
    }

    pub(crate) fn structural_child(&self, edge: ScopedTypeEdge) -> Option<Self> {
        self.0.projected_structural_child(edge).map(Self)
    }
}

/// Planner-level reason for opening one owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoalOwnerKind {
    Application,
    /// Short-lived, zero-local-goal child that atomically seeds an open
    /// application's ancestor goals from written arguments, its contextual
    /// result, and independently completed fresh premises.
    ApplicationGather,
    RetainedValue,
    LambdaFrontier,
    NestedApplication,
    /// One forward equation for the retained placeholders in a single source
    /// annotation. The owner is root-local and closes before the annotated
    /// value or binder becomes visible.
    Annotation,
}

/// Outcome of an equation that may be kept only when its owner can close.
/// `Applied` means the equation cannot publish an ancestor solution that
/// retains a descendant goal; it makes no claim about closing another owner
/// or its publication transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseReadyEquationConstraint {
    Applied,
    Deferred,
}

/// User-facing position represented by one goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoalRole {
    TypeArgument,
    LambdaParameter,
    Annotation,
}

/// Whether a goal may be solved by a polymorphic value type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoalSolutionPolicy {
    Monotype,
    PolytypeAllowed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoalCloseRequirement {
    Required,
    ActivateOnUse,
}

/// Opaque owner-local slot for a lexical layer that may remain unapplied.
/// Only [`GoalStore::activate_reserved_goal_at`] can turn it into a usable
/// inference-goal occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReservedGoalRef(TypeGoalRef);

impl GoalSolutionPolicy {
    fn meet(self, other: Self) -> Self {
        if self == Self::Monotype || other == Self::Monotype {
            Self::Monotype
        } else {
            Self::PolytypeAllowed
        }
    }
}

/// Stable diagnostic provenance for an inference goal.
#[derive(Clone, Debug)]
pub(crate) struct GoalOrigin {
    span: Span,
    role: GoalRole,
    source_name: Option<String>,
    unresolved_diagnostic: UnresolvedGoalDiagnostic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnresolvedGoalDiagnostic {
    CannotInfer,
    SchemeInMonomorphicPosition,
}

impl GoalOrigin {
    pub(crate) fn named(span: Span, role: GoalRole, source_name: impl Into<String>) -> Self {
        Self {
            span,
            role,
            source_name: Some(source_name.into()),
            unresolved_diagnostic: UnresolvedGoalDiagnostic::CannotInfer,
        }
    }

    pub(crate) fn contextual_returned_forall(span: Span, source_name: impl Into<String>) -> Self {
        Self {
            span,
            role: GoalRole::TypeArgument,
            source_name: Some(source_name.into()),
            unresolved_diagnostic: UnresolvedGoalDiagnostic::SchemeInMonomorphicPosition,
        }
    }

    pub(crate) fn annotation(span: Span) -> Self {
        Self {
            span,
            role: GoalRole::Annotation,
            source_name: None,
            unresolved_diagnostic: UnresolvedGoalDiagnostic::CannotInfer,
        }
    }

    fn unresolved_error(&self) -> Error {
        if self.unresolved_diagnostic == UnresolvedGoalDiagnostic::SchemeInMonomorphicPosition {
            return super::types::scheme_in_mono_position(self.span);
        }
        let position = match self.role {
            GoalRole::TypeArgument => "type argument",
            GoalRole::LambdaParameter => "lambda parameter",
            GoalRole::Annotation => "type annotation",
        };
        let name = self
            .source_name
            .as_deref()
            .map(|name| format!(" `{name}`"))
            .unwrap_or_default();
        Error::type_(
            self.span,
            format!("cannot infer {position}{name} from the available type information"),
        )
    }
}

#[derive(Debug)]
struct GoalState {
    kind: Kind,
    solution_policy: GoalSolutionPolicy,
    close_requirement: GoalCloseRequirement,
    origin: GoalOrigin,
    solution: Option<ScopedType>,
}

#[derive(Debug)]
struct OwnerState {
    parent: Option<TypeGoalOwner>,
    kind: GoalOwnerKind,
    scope: RigidScope,
    goals: Vec<GoalState>,
    open_children: usize,
    lifecycle: OwnerLifecycle,
    producer_view: Option<ProducerViewId>,
    #[cfg(test)]
    depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerLifecycle {
    Open,
    Closed,
}

#[derive(Debug, Default)]
struct DomainState {
    owners: Vec<OwnerState>,
}

#[derive(Clone, Debug)]
struct GoalBinding {
    value: ScopedType,
    span: Span,
}

#[derive(Clone, Copy, Debug)]
struct GoalPolicyMeet {
    policy: GoalSolutionPolicy,
    span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProducerViewId(NonZeroU32);

#[derive(Debug)]
struct ProducerView {
    boundary: TypeGoalOwner,
    parent: Option<ProducerViewId>,
    accepted: bool,
    context_writes: BTreeMap<TypeGoalRef, GoalBinding>,
    context_policies: BTreeMap<TypeGoalRef, GoalPolicyMeet>,
    writes: BTreeMap<TypeGoalRef, GoalBinding>,
    policy_meets: BTreeMap<TypeGoalRef, GoalPolicyMeet>,
}

pub(crate) struct ProducerObligations {
    owner: TypeGoalOwner,
    writes: BTreeMap<TypeGoalRef, GoalBinding>,
    policy_meets: BTreeMap<TypeGoalRef, GoalPolicyMeet>,
}

#[derive(Clone, Copy, Debug, Default)]
enum GoalReadView {
    #[default]
    Owner,
    EnclosingProducer,
    Trial(ProducerViewId),
}

#[derive(Debug, Default)]
struct EquationUndo {
    writes: BTreeMap<TypeGoalRef, Option<GoalBinding>>,
    policy_meets: BTreeMap<TypeGoalRef, Option<GoalPolicyMeet>>,
    failure: Option<RelationFailureKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelationFailureKind {
    Incompatible,
}

/// Speculative writes owned by one inference frontier.
#[derive(Debug)]
pub(crate) struct GoalDelta {
    token: GoalDeltaToken,
    owner: TypeGoalOwner,
    owner_goal_count: usize,
    writes: BTreeMap<TypeGoalRef, GoalBinding>,
    policy_meets: BTreeMap<TypeGoalRef, GoalPolicyMeet>,
    equation_undo: Option<EquationUndo>,
    read_view: GoalReadView,
}

impl GoalDelta {
    pub(crate) fn owner(&self) -> TypeGoalOwner {
        self.owner
    }

    pub(crate) fn authority(&self) -> GoalDeltaAuthority {
        GoalDeltaAuthority(self.token)
    }

    pub(crate) fn is_empty(&self) -> bool {
        assert!(
            self.equation_undo.is_none(),
            "planner inspected a goal delta during an open equation"
        );
        self.writes.is_empty() && self.policy_meets.is_empty()
    }

    fn begin_equation(&mut self) {
        assert!(
            self.equation_undo
                .replace(EquationUndo::default())
                .is_none(),
            "goal solver started a nested equation transaction"
        );
    }

    fn write_solution(&mut self, goal: TypeGoalRef, binding: GoalBinding) {
        let previous = self.writes.get(&goal).cloned();
        if let Some(undo) = &mut self.equation_undo {
            undo.writes.entry(goal).or_insert(previous);
        }
        self.writes.insert(goal, binding);
    }

    fn write_policy_meet(&mut self, goal: TypeGoalRef, meet: GoalPolicyMeet) {
        let previous = self.policy_meets.get(&goal).copied();
        if let Some(undo) = &mut self.equation_undo {
            undo.policy_meets.entry(goal).or_insert(previous);
        }
        self.policy_meets.insert(goal, meet);
    }

    fn mark_relation_incompatible(&mut self) {
        let undo = self
            .equation_undo
            .as_mut()
            .expect("goal relation failed outside an equation transaction");
        undo.failure = Some(RelationFailureKind::Incompatible);
    }

    fn relation_failure(&self) -> Option<RelationFailureKind> {
        self.equation_undo
            .as_ref()
            .expect("goal relation was inspected outside an equation transaction")
            .failure
    }

    fn finish_equation(&mut self, success: bool) {
        let undo = self
            .equation_undo
            .take()
            .expect("goal solver finished an equation without an active transaction");
        if success {
            return;
        }
        for (goal, previous) in undo.writes {
            match previous {
                Some(binding) => {
                    self.writes.insert(goal, binding);
                }
                None => {
                    self.writes.remove(&goal);
                }
            }
        }
        for (goal, previous) in undo.policy_meets {
            match previous {
                Some(meet) => {
                    self.policy_meets.insert(goal, meet);
                }
                None => {
                    self.policy_meets.remove(&goal);
                }
            }
        }
    }
}

/// Normalized outputs returned only after an owner closes atomically.
#[derive(Debug)]
pub(crate) struct ClosedGoalOutputs {
    owner: TypeGoalOwner,
    outputs: Vec<ClosedGoalOutput>,
}

/// Normalized publication-only outputs returned by the same atomic owner close
/// as ordinary semantic outputs. These values preserve their exact lexical
/// scope and cannot enter ordinary inference equations.
#[derive(Debug)]
pub(crate) struct ClosedPublicationOutputs {
    owner: TypeGoalOwner,
    outputs: Vec<ClosedPublicationOutput>,
}

/// Fully validated owner close whose GoalStore writes and lifecycle transition
/// have not yet become visible. Callers may inspect the semantic outputs while
/// preparing other infallible side effects, then consume the private commit
/// capability exactly once.
#[derive(Debug)]
pub(crate) struct PreparedGoalClose {
    commit: PreparedGoalCommit,
    outputs: ClosedGoalOutputs,
    publication_outputs: ClosedPublicationOutputs,
}

#[derive(Debug)]
pub(crate) struct PreparedGoalCommit {
    store_nonce: TypeGoalStoreIdentity,
    store_revision: u64,
    delta: GoalDelta,
    parent: Option<TypeGoalOwner>,
}

impl PreparedGoalClose {
    pub(crate) fn into_parts(
        self,
    ) -> (
        PreparedGoalCommit,
        ClosedGoalOutputs,
        ClosedPublicationOutputs,
    ) {
        (self.commit, self.outputs, self.publication_outputs)
    }
}

#[derive(Debug)]
pub(crate) enum ClosedGoalOutput {
    Retained(RetainedGoalOutput),
    RetainedFunctionScheme(RetainedFunctionSchemeOutput),
    GoalFree(GoalFreeOutput),
    FunctionScheme(GoalFreeFunctionSchemeOutput),
}

#[derive(Debug)]
pub(crate) enum ClosedPublicationOutput {
    Retained(RetainedPublicationOutput),
    GoalFree(GoalFreePublicationOutput),
}

#[derive(Debug)]
pub(crate) struct RetainedGoalOutput {
    destination: TypeGoalOwner,
    value: ScopedType,
}

/// A child-retained whole function scheme whose lexical binders remain in
/// scope while ancestor goals are still open.
#[derive(Debug)]
pub(crate) struct RetainedFunctionSchemeOutput {
    destination: TypeGoalOwner,
    value: ScopedType,
}

impl RetainedGoalOutput {
    pub(crate) fn destination(&self) -> TypeGoalOwner {
        self.destination
    }

    pub(crate) fn into_scoped_type(self) -> ScopedType {
        self.value
    }
}

impl RetainedFunctionSchemeOutput {
    pub(crate) fn destination(&self) -> TypeGoalOwner {
        self.destination
    }

    pub(crate) fn into_scoped_type(self) -> ScopedType {
        self.value
    }
}

#[derive(Debug)]
pub(crate) struct RetainedPublicationOutput {
    destination: TypeGoalOwner,
    state: RetainedPublicationState,
}

impl RetainedPublicationOutput {
    pub(crate) fn destination(&self) -> TypeGoalOwner {
        self.destination
    }
}

#[derive(Debug)]
enum RetainedPublicationState {
    ValidationRequired(ScopedType),
    GoalFreeValidated(GoalFreeValidatedPublication),
}

#[derive(Debug)]
struct GoalFreeValidatedPublication {
    value: ScopedType,
    root_kind: PublicationRootValidationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationRootValidationKind {
    GoalFree,
    FunctionScheme,
}

impl RetainedPublicationState {
    fn scoped_type(&self) -> &ScopedType {
        match self {
            Self::ValidationRequired(value) => value,
            Self::GoalFreeValidated(value) => &value.value,
        }
    }

    fn prepare_for_owner_scope(self, scope: &RigidScope) -> Self {
        match self {
            Self::ValidationRequired(value) => {
                Self::ValidationRequired(value.strengthen_scope_if_compatible(scope))
            }
            Self::GoalFreeValidated(value) => {
                assert!(
                    value.value.scope.extends(scope),
                    "validated publication output lost its source owner scope"
                );
                Self::GoalFreeValidated(value)
            }
        }
    }

    #[cfg(test)]
    fn stamped_for_test(self, store_nonce: TypeGoalStoreIdentity) -> Self {
        match self {
            Self::ValidationRequired(value) => {
                Self::ValidationRequired(value.stamped_for_test(store_nonce))
            }
            Self::GoalFreeValidated(value) => {
                Self::GoalFreeValidated(GoalFreeValidatedPublication {
                    value: value.value.stamped_for_test(store_nonce),
                    root_kind: value.root_kind,
                })
            }
        }
    }
}

#[derive(Debug)]
enum PublicationGoalInputKind {
    Pending(ScopedType),
    Retained(RetainedPublicationOutput),
}

/// Opaque publication input accepted only by
/// [`GoalStore::prepare_owner_with_publication`]. A retained child output
/// cannot be extracted into an ordinary inference value.
#[derive(Debug)]
pub(crate) struct PublicationGoalInput(PublicationGoalInputKind);

impl PublicationGoalInput {
    pub(crate) fn pending(value: ScopedType) -> Self {
        Self(PublicationGoalInputKind::Pending(value))
    }

    pub(crate) fn retained(value: RetainedPublicationOutput) -> Self {
        Self(PublicationGoalInputKind::Retained(value))
    }

    fn store_nonce(&self) -> TypeGoalStoreIdentity {
        match &self.0 {
            PublicationGoalInputKind::Pending(value) => value.scope.store_nonce,
            PublicationGoalInputKind::Retained(output) => {
                output.state.scoped_type().scope.store_nonce
            }
        }
    }

    fn into_state_for_owner(
        self,
        owner: TypeGoalOwner,
        scope: &RigidScope,
    ) -> RetainedPublicationState {
        match self.0 {
            PublicationGoalInputKind::Pending(value) => {
                RetainedPublicationState::ValidationRequired(
                    value.strengthen_scope_if_compatible(scope),
                )
            }
            PublicationGoalInputKind::Retained(output) => {
                assert_eq!(
                    output.destination, owner,
                    "retained publication output was replayed through a different inference owner"
                );
                output.state.prepare_for_owner_scope(scope)
            }
        }
    }

    #[cfg(test)]
    fn stamped_for_test(self, store_nonce: TypeGoalStoreIdentity) -> Self {
        match self.0 {
            PublicationGoalInputKind::Pending(value) => {
                Self::pending(value.stamped_for_test(store_nonce))
            }
            PublicationGoalInputKind::Retained(mut output) => {
                output.state = output.state.stamped_for_test(store_nonce);
                Self::retained(output)
            }
        }
    }
}

impl PreparedCloseType {
    pub(crate) fn into_goal_free_position_publication(
        self,
    ) -> (GoalDeltaAuthority, TypeGoalOwner, PublicationGoalInput) {
        (
            self.delta,
            self.owner,
            PublicationGoalInput::pending(self.value),
        )
    }
}

#[derive(Debug)]
pub(crate) struct GoalFreeOutput {
    destination: TypeGoalOwner,
    value: ScopedType,
}

#[derive(Debug)]
pub(crate) struct GoalFreePublicationOutput {
    destination: TypeGoalOwner,
    value: ScopedType,
}

impl GoalFreePublicationOutput {
    pub(crate) fn destination(&self) -> TypeGoalOwner {
        self.destination
    }

    pub(crate) fn into_interned_type(self) -> InternedType<Lowered> {
        self.value.into_interned_type()
    }
}

#[derive(Debug)]
pub(crate) struct GoalFreeFunctionSchemeOutput {
    value: ScopedType,
}

impl GoalFreeFunctionSchemeOutput {
    pub(crate) fn into_scoped_type(self) -> ScopedType {
        self.value
    }
}

impl GoalFreeOutput {
    pub(crate) fn destination(&self) -> TypeGoalOwner {
        self.destination
    }

    pub(crate) fn into_scoped_type(self) -> ScopedType {
        self.value
    }
}

impl ClosedGoalOutputs {
    pub(crate) fn owner(&self) -> TypeGoalOwner {
        self.owner
    }

    pub(crate) fn into_outputs(self) -> Vec<ClosedGoalOutput> {
        self.outputs
    }
}

impl ClosedPublicationOutputs {
    pub(crate) fn owner(&self) -> TypeGoalOwner {
        self.owner
    }

    pub(crate) fn into_outputs(self) -> Vec<ClosedPublicationOutput> {
        self.outputs
    }
}

/// Which unresolved goals may remain after zonking an owner result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GoalEscape {
    /// Carry a nested result into this still-open owner. Goals owned by the
    /// destination or its ancestors and rigid binders in its scope may remain.
    ToOwner(TypeGoalOwner),
    /// Carry a prepared whole function scheme into this still-open owner.
    /// This mode is selected only from an exact-delta
    /// [`PreparedCloseType`] proof; ordinary retained values remain governed
    /// by the destination goal's explicit solution policy.
    ToOwnerFunctionScheme(TypeGoalOwner),
    /// Produce a goal-free result at this owner's rigid scope.
    ClosedAt(TypeGoalOwner),
    /// Publish a goal-free whole function scheme while preserving its scheme
    /// classification at the publication boundary.
    ClosedFunctionSchemeAt(TypeGoalOwner),
}

/// Publication-only output policy. Unlike [`GoalEscape`], this preserves the
/// exact lexical scope carried by the input while allowing remaining goals to
/// flow only into the named ancestor. The resulting opaque output can be
/// consumed only by the Lowered publication boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationGoalEscape {
    ToOwner(TypeGoalOwner),
    ToOwnerFunctionScheme(TypeGoalOwner),
    ClosedAt(TypeGoalOwner),
    ClosedFunctionSchemeAt(TypeGoalOwner),
}

impl PublicationRootValidationKind {
    fn from_escape(escape: PublicationGoalEscape) -> Self {
        match escape {
            PublicationGoalEscape::ToOwner(_) | PublicationGoalEscape::ClosedAt(_) => {
                Self::GoalFree
            }
            PublicationGoalEscape::ToOwnerFunctionScheme(_)
            | PublicationGoalEscape::ClosedFunctionSchemeAt(_) => Self::FunctionScheme,
        }
    }
}

/// Environment-dependent operations kept outside the deterministic store.
///
/// Production uses the ordinary [`TypeCtx`] implementation below.  Focused
/// store tests use a tiny declared-kind table; the unification algorithm is
/// otherwise identical.
pub(crate) trait GoalTypeContext {
    fn capability(&self) -> GoalContextCapability;

    fn canonicalize(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        scope: &RigidScope,
    ) -> (Type<Lowered>, bool);

    /// Canonicalize a transparent alias at one structural path frontier.
    ///
    /// `None` means that the borrowed path is already the frontier the
    /// structural walk should compare. Implementations must make that decision
    /// from the head without cloning or traversing its type arguments.
    fn canonicalize_alias_frontier(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        scope: &RigidScope,
    ) -> Option<(Type<Lowered>, bool)>;

    fn nominal_head_kind(
        &self,
        segments: &[PathSegment],
        supplied_args: usize,
        span: Span,
        scope: &RigidScope,
        identity_canonical: bool,
    ) -> Result<Kind, Error>;

    /// Goal-free alias/scheme fast path over the shared virtual cursor.
    /// `Deferred` means the frontier selected an open goal and this store must
    /// resume with its goal-aware adapter instead.
    fn complete_scheme_frontier(
        &self,
        _ty: &Type<Lowered>,
        _identity_canonical: bool,
        _scope: &RigidScope,
    ) -> Option<super::aliases::AliasCompleteSchemeAnalysis> {
        None
    }

    fn mismatch(&self, found: &ScopedType, expected: &ScopedType, span: Span) -> Error;
}

/// Immutable environment for one stack-confined specialization relation.
/// Every operand's structural carrier is deeply alias-canonicalized while the
/// producing `TypeCtx` is still available, so evaluator-time solving needs
/// only exact nominal head kinds and performs no alias lookup.
#[derive(Debug)]
pub(crate) struct IsolatedSpecializationContext {
    nominal_head_kinds: HashMap<Vec<String>, Kind>,
    opaque_goal_kinds: HashMap<TypeGoalRef, Kind>,
}

pub(crate) struct IsolatedSpecializationContextBuilder {
    source_capability: GoalContextCapability,
    nominal_head_kinds: HashMap<Vec<String>, Kind>,
    opaque_goal_kinds: HashMap<TypeGoalRef, Kind>,
}

impl IsolatedSpecializationContextBuilder {
    pub(crate) fn new(tcx: &TypeCtx<'_, '_, Lowered>) -> Self {
        Self {
            source_capability: GoalContextCapability::Production(tcx.context_id()),
            nominal_head_kinds: HashMap::new(),
            opaque_goal_kinds: HashMap::new(),
        }
    }

    /// Retain only immutable kind evidence for open carrier goals while the
    /// live store is available. Evaluator-time specialization treats these
    /// goals as opaque rigid subtrees; it never receives store authority.
    pub(crate) fn record_carrier_goals(
        &mut self,
        input: &ScopedType,
        store: &GoalStore,
    ) -> Result<(), Error> {
        assert_eq!(
            input.scope.context, self.source_capability,
            "specialization goal evidence came from another type-checking context"
        );
        assert_eq!(
            input.scope.store_nonce, store.nonce,
            "specialization goal evidence came from another inference store"
        );
        for goal in goal_refs(input.ty.as_type()) {
            let kind = store.goal_state(goal, input.ty.span())?.kind.clone();
            match self.opaque_goal_kinds.entry(goal) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(kind);
                }
                std::collections::hash_map::Entry::Occupied(entry) => {
                    assert_eq!(
                        entry.get(),
                        &kind,
                        "one projected goal acquired conflicting kind evidence"
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn canonicalize_carrier(
        &mut self,
        input: &ScopedType,
        tcx: &TypeCtx<'_, '_, Lowered>,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            input.scope.context, self.source_capability,
            "specialization input came from another type-checking context"
        );
        assert_eq!(
            GoalContextCapability::Production(tcx.context_id()),
            self.source_capability,
            "specialization context builder was used with another TypeCtx"
        );
        let alias_ctx = tcx.env.alias_ctx();
        let (ty, identity_canonical) =
            super::aliases::canonicalize_deep_for_comparison_with_binder_lookup(
                input.ty.as_type(),
                &alias_ctx,
                &input.scope,
                input.ty.identity_is_canonical(),
            );
        assert!(
            identity_canonical,
            "whole-type specialization canonicalization did not close its identity frontier"
        );
        self.record_nominal_head_kinds(&ty, &input.scope, tcx)?;
        Ok(ScopedType::new(
            InternedType::fresh_canonical(ty),
            input.scope.clone(),
        ))
    }

    pub(crate) fn finish(self) -> Arc<IsolatedSpecializationContext> {
        Arc::new(IsolatedSpecializationContext {
            nominal_head_kinds: self.nominal_head_kinds,
            opaque_goal_kinds: self.opaque_goal_kinds,
        })
    }

    fn record_nominal_head_kinds(
        &mut self,
        ty: &Type<Lowered>,
        scope: &RigidScope,
        tcx: &TypeCtx<'_, '_, Lowered>,
    ) -> Result<(), Error> {
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } => {}
            Type::Function { param, ret, .. } => {
                self.record_nominal_head_kinds(param, scope, tcx)?;
                self.record_nominal_head_kinds(ret, scope, tcx)?;
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.record_nominal_head_kinds(left, scope, tcx)?;
                self.record_nominal_head_kinds(right, scope, tcx)?;
            }
            Type::Forall { param, body, .. } => {
                self.record_nominal_head_kinds(
                    body,
                    &scope.clone().with_lexical_forall(param),
                    tcx,
                )?;
            }
            Type::Path {
                segments,
                args,
                meta,
            } => {
                for arg in args {
                    self.record_nominal_head_kinds(arg, scope, tcx)?;
                }
                if segments.len() == 1 && scope.binding(segments[0].as_str()).is_some() {
                    return Ok(());
                }
                let kind = tcx.nominal_head_kind(segments, args.len(), meta.span, scope, true)?;
                let key = segments
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect::<Vec<_>>();
                match self.nominal_head_kinds.entry(key) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(kind);
                    }
                    std::collections::hash_map::Entry::Occupied(entry) => {
                        assert_eq!(
                            entry.get(),
                            &kind,
                            "one canonical nominal head acquired conflicting kinds"
                        );
                    }
                }
            }
            Type::Infer { meta, .. } => {
                return Err(Error::type_(
                    meta.span,
                    "specialization requires an infer-free inspected type",
                ));
            }
            Type::Goal { args, .. } => {
                for arg in args {
                    self.record_nominal_head_kinds(arg, scope, tcx)?;
                }
            }
            Type::LabelSugar { ext, .. } => match *ext {},
        }
        Ok(())
    }
}

impl IsolatedSpecializationContext {
    pub(crate) fn from_canonical_nominal_head_kinds(
        nominal_head_kinds: HashMap<Vec<String>, Kind>,
    ) -> Arc<Self> {
        Arc::new(Self {
            nominal_head_kinds,
            opaque_goal_kinds: HashMap::new(),
        })
    }

    fn opaque_goal_head_kind(
        &self,
        goal: TypeGoalRef,
        supplied_args: usize,
        span: Span,
    ) -> Result<Kind, Error> {
        let base = self.opaque_goal_kinds.get(&goal).ok_or_else(|| {
            Error::type_(
                span,
                "specialization context omitted immutable kind evidence for an open type",
            )
        })?;
        base.arity().checked_sub(supplied_args).ok_or_else(|| {
            Error::type_(span, "specialization received an over-applied open type")
        })?;
        Ok(base.clone())
    }

    fn validate_input(&self, input: &ScopedType) -> Result<(), Error> {
        if !input.ty.identity_is_canonical() {
            return Err(Error::type_(
                input.ty.span(),
                "specialization received a non-canonical inspected type",
            ));
        }
        self.validate_type(input.ty.as_type(), &input.scope, false)
    }

    fn validate_solver_pattern(&self, input: &ScopedType) -> Result<(), Error> {
        if !input.ty.identity_is_canonical() {
            return Err(Error::type_(
                input.ty.span(),
                "specialization received a non-canonical pattern",
            ));
        }
        self.validate_type(input.ty.as_type(), &input.scope, true)
    }

    fn validate_type(
        &self,
        ty: &Type<Lowered>,
        scope: &RigidScope,
        allow_local_goals: bool,
    ) -> Result<(), Error> {
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } => Ok(()),
            Type::Function { param, ret, .. } => {
                self.validate_type(param, scope, allow_local_goals)?;
                self.validate_type(ret, scope, allow_local_goals)
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.validate_type(left, scope, allow_local_goals)?;
                self.validate_type(right, scope, allow_local_goals)
            }
            Type::Forall { param, body, .. } => self.validate_type(
                body,
                &scope.clone().with_lexical_forall(param),
                allow_local_goals,
            ),
            Type::Path {
                segments,
                args,
                meta,
            } => {
                for arg in args {
                    self.validate_type(arg, scope, allow_local_goals)?;
                }
                if segments.len() == 1 && scope.binding(segments[0].as_str()).is_some() {
                    return Ok(());
                }
                let key = segments
                    .iter()
                    .map(|segment| segment.name.clone())
                    .collect::<Vec<_>>();
                self.nominal_head_kinds
                    .get(&key)
                    .map(|_| ())
                    .ok_or_else(|| {
                        Error::type_(
                            meta.span,
                            "specialization context omitted a canonical nominal head kind",
                        )
                    })
            }
            Type::Infer { meta, .. } => Err(Error::type_(
                meta.span,
                "specialization requires an infer-free inspected type",
            )),
            Type::Goal { args, meta, .. } if allow_local_goals => {
                for arg in args {
                    self.validate_type(arg, scope, true)?;
                }
                Ok(())
            }
            Type::Goal { meta, .. } => Err(Error::type_(
                meta.span,
                "specialization requires a goal-free inspected type",
            )),
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }
}

impl GoalTypeContext for IsolatedSpecializationContext {
    fn capability(&self) -> GoalContextCapability {
        GoalContextCapability::isolated_specialization()
    }

    fn canonicalize(
        &self,
        ty: &Type<Lowered>,
        _identity_canonical: bool,
        _scope: &RigidScope,
    ) -> (Type<Lowered>, bool) {
        // Entry validation already proved that every source subtree is
        // canonical. Solver-created goal shells may conservatively clear the
        // interned flag even though they cannot introduce an alias frontier.
        (ty.clone(), true)
    }

    fn canonicalize_alias_frontier(
        &self,
        _ty: &Type<Lowered>,
        _identity_canonical: bool,
        _scope: &RigidScope,
    ) -> Option<(Type<Lowered>, bool)> {
        None
    }

    fn nominal_head_kind(
        &self,
        segments: &[PathSegment],
        _supplied_args: usize,
        span: Span,
        scope: &RigidScope,
        _identity_canonical: bool,
    ) -> Result<Kind, Error> {
        if segments.len() == 1
            && let Some(kind) = scope.kind(segments[0].as_str())
        {
            return Ok(kind);
        }
        let key = segments
            .iter()
            .map(|segment| segment.name.clone())
            .collect::<Vec<_>>();
        self.nominal_head_kinds.get(&key).cloned().ok_or_else(|| {
            Error::type_(
                span,
                "specialization context omitted a canonical nominal head kind",
            )
        })
    }

    fn mismatch(&self, found: &ScopedType, expected: &ScopedType, span: Span) -> Error {
        Error::type_(
            span,
            format!(
                "type mismatch: found `{}`, expected `{}`",
                super::display_type(found.ty.as_type()),
                super::display_type(expected.ty.as_type())
            ),
        )
    }
}

/// Production adapter for the one goal store owned by an alpha-normalized
/// item-checking frontier. Every `RigidScope` supplied to that store is a
/// snapshot of this context's active normalized binders; the store nonce is
/// capability-only and all goals close before the Lowered-to-Prime boundary.
impl GoalTypeContext for TypeCtx<'_, '_, Lowered> {
    fn capability(&self) -> GoalContextCapability {
        GoalContextCapability::Production(self.context_id())
    }

    fn canonicalize(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        scope: &RigidScope,
    ) -> (Type<Lowered>, bool) {
        let alias_ctx = self.env.alias_ctx();
        super::aliases::canonicalize_for_comparison_with_binder_lookup(
            ty,
            &alias_ctx,
            scope,
            identity_canonical,
        )
    }

    fn canonicalize_alias_frontier(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        scope: &RigidScope,
    ) -> Option<(Type<Lowered>, bool)> {
        if let Type::Path { segments, .. } = ty
            && let [name] = segments.as_slice()
            && scope.binding(name.as_str()).is_some()
        {
            return None;
        }
        let alias_ctx = self.env.alias_ctx();
        if !has_unfoldable_alias_frontier_for_comparison(ty, &alias_ctx, identity_canonical) {
            return None;
        }
        if scope.bindings.is_empty() {
            return unfold_alias_frontier_for_comparison(ty, &alias_ctx, identity_canonical);
        }
        unfold_alias_frontier_for_comparison_with_binder_lookup(
            ty,
            &alias_ctx,
            scope,
            identity_canonical,
        )
    }

    fn nominal_head_kind(
        &self,
        segments: &[PathSegment],
        supplied_args: usize,
        span: Span,
        scope: &RigidScope,
        identity_canonical: bool,
    ) -> Result<Kind, Error> {
        resolve_nominal_head_kind(
            segments,
            supplied_args,
            span,
            self,
            NominalHeadBinding::RigidScope(
                (segments.len() == 1)
                    .then(|| scope.kind(segments[0].as_str()))
                    .flatten(),
            ),
            identity_canonical,
        )
    }

    fn complete_scheme_frontier(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        scope: &RigidScope,
    ) -> Option<super::aliases::AliasCompleteSchemeAnalysis> {
        let alias_ctx = self.env.alias_ctx();
        Some(
            super::aliases::analyze_complete_function_scheme_in_scope_with_binder_lookup(
                ty,
                &alias_ctx,
                scope,
                identity_canonical,
            ),
        )
    }

    fn mismatch(&self, found: &ScopedType, expected: &ScopedType, span: Span) -> Error {
        let binders = CombinedRigidScopes {
            first: &found.scope,
            second: &expected.scope,
        };
        let alias_ctx = self.env.alias_ctx();
        super::aliases::type_mismatch_error_state_with_binder_lookup(
            found.ty.as_type(),
            expected.ty.as_type(),
            span,
            &alias_ctx,
            found.ty.identity_is_canonical(),
            expected.ty.identity_is_canonical(),
            &binders,
            expected.ty.requirement_source().map(AsRef::as_ref),
        )
    }
}

/// Process-unique capability identity for a transient store. Allocation order
/// is intentionally not deterministic; the value is never observable.
static NEXT_GOAL_STORE_NONCE: AtomicU64 = AtomicU64::new(1);
static NEXT_GOAL_DELTA_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(crate) struct GoalStore {
    nonce: TypeGoalStoreIdentity,
    revision: u64,
    context: Option<GoalContextCapability>,
    domains: Vec<DomainState>,
    producer_views: Vec<ProducerView>,
}

impl Default for GoalStore {
    fn default() -> Self {
        let nonce = NEXT_GOAL_STORE_NONCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("process exhausted unique type-inference goal-store identities");
        Self {
            nonce: TypeGoalStoreIdentity::process(nonce),
            revision: 0,
            context: None,
            domains: Vec::new(),
            producer_views: Vec::new(),
        }
    }
}

impl GoalStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn new_isolated() -> Self {
        Self {
            nonce: TypeGoalStoreIdentity::ISOLATED,
            revision: 0,
            context: None,
            domains: Vec::new(),
            producer_views: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn planning_state_fingerprint(&self) -> (u64, Vec<Vec<(usize, usize)>>) {
        (
            self.revision,
            self.domains
                .iter()
                .map(|domain| {
                    domain
                        .owners
                        .iter()
                        .map(|owner| (owner.goals.len(), owner.open_children))
                        .collect()
                })
                .collect(),
        )
    }

    fn next_revision(&self) -> u64 {
        self.revision
            .checked_add(1)
            .expect("type-inference goal-store revision overflowed")
    }

    pub(crate) fn scope_from_type_ctx(&self, tcx: &TypeCtx<'_, '_, Lowered>) -> RigidScope {
        RigidScope::from_type_ctx(self.nonce, tcx)
    }

    /// Snapshot an already-validated owner prefix. Callers may derive narrow
    /// lexical children from this persistent value without rebuilding or
    /// revalidating the complete ambient binder map.
    pub(crate) fn owner_scope_snapshot(
        &self,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<RigidScope, Error> {
        Ok(self.owner_state(owner, span)?.scope.clone())
    }

    /// Open a lexical child by extending its validated parent with exactly
    /// the newly introduced retained binders. Unlike the general owner
    /// constructor, this path does not rescan `TypeCtx` or run a subset check
    /// over an already-related persistent prefix.
    pub(crate) fn begin_lexical_owner(
        &mut self,
        parent: TypeGoalOwner,
        binders: &[super::env::RetainedTypeBinder<'_>],
        kind: GoalOwnerKind,
        span: Span,
    ) -> Result<TypeGoalOwner, Error> {
        assert!(
            matches!(
                kind,
                GoalOwnerKind::NestedApplication
                    | GoalOwnerKind::LambdaFrontier
                    | GoalOwnerKind::RetainedValue
            ),
            "a lexical owner used a non-retained owner kind"
        );
        let scope = self
            .owner_state(parent, span)?
            .scope
            .clone()
            .with_retained_binders(binders);
        assert_eq!(
            self.owner_state(parent, span)?.lifecycle,
            OwnerLifecycle::Open,
            "planner tried to open a lexical child below a closed owner"
        );
        #[cfg(test)]
        let depth = self.owner_state(parent, span)?.depth + 1;
        let next_revision = self.next_revision();
        let producer_view = self.owner_state(parent, span)?.producer_view;
        let domain = self
            .domains
            .get_mut(parent.domain().index() as usize)
            .expect("validated owner domain disappeared");
        let owner_index =
            u32::try_from(domain.owners.len()).expect("type-inference owner count exceeded u32");
        let owner = TypeGoalOwner::new(parent.domain(), owner_index);
        domain.owners[parent.index() as usize].open_children += 1;
        domain.owners.push(OwnerState {
            parent: Some(parent),
            kind,
            scope,
            goals: Vec::new(),
            open_children: 0,
            lifecycle: OwnerLifecycle::Open,
            producer_view,
            #[cfg(test)]
            depth,
        });
        self.revision = next_revision;
        #[cfg(test)]
        record_owner_open(false, depth);
        Ok(owner)
    }

    /// Open an outer domain or a nested owner in an existing domain.
    pub(crate) fn begin_owner(
        &mut self,
        parent: Option<TypeGoalOwner>,
        scope: RigidScope,
        kind: GoalOwnerKind,
        span: Span,
    ) -> Result<TypeGoalOwner, Error> {
        #[cfg(test)]
        let scope = if scope.store_nonce == TypeGoalStoreIdentity::UNSTAMPED_TEST {
            scope.stamped_for_test(self.nonce)
        } else {
            scope
        };
        assert!(
            scope.store_nonce == self.nonce,
            "planner passed rigid-scope evidence to the wrong goal store"
        );
        match self.context {
            Some(context) => assert_eq!(
                scope.context, context,
                "planner mixed package-analysis contexts in one goal store"
            ),
            None => self.context = Some(scope.context),
        }
        match parent {
            None => {
                let domain_index = u32::try_from(self.domains.len())
                    .expect("type-inference domain count exceeded u32");
                let domain = TypeGoalDomain::new(self.nonce, domain_index);
                let next_revision = self.next_revision();
                self.domains.push(DomainState {
                    owners: vec![OwnerState {
                        parent: None,
                        kind,
                        scope,
                        goals: Vec::new(),
                        open_children: 0,
                        lifecycle: OwnerLifecycle::Open,
                        producer_view: None,
                        #[cfg(test)]
                        depth: 1,
                    }],
                });
                self.revision = next_revision;
                #[cfg(test)]
                record_owner_open(true, 1);
                Ok(TypeGoalOwner::new(domain, 0))
            }
            Some(parent) => {
                let parent_scope = &self.owner_state(parent, span)?.scope;
                assert!(
                    scope.extends(parent_scope),
                    "nested inference scope does not preserve every parent rigid binder"
                );
                assert_eq!(
                    self.owner_state(parent, span)?.lifecycle,
                    OwnerLifecycle::Open,
                    "planner tried to open a nested inference owner below a closed owner"
                );
                #[cfg(test)]
                let depth = self.owner_state(parent, span)?.depth + 1;
                let next_revision = self.next_revision();
                let producer_view = self.owner_state(parent, span)?.producer_view;
                let domain = self
                    .domains
                    .get_mut(parent.domain().index() as usize)
                    .expect("validated owner domain disappeared");
                let owner_index = u32::try_from(domain.owners.len())
                    .expect("type-inference owner count exceeded u32");
                let owner = TypeGoalOwner::new(parent.domain(), owner_index);
                domain.owners[parent.index() as usize].open_children += 1;
                domain.owners.push(OwnerState {
                    parent: Some(parent),
                    kind,
                    scope,
                    goals: Vec::new(),
                    open_children: 0,
                    lifecycle: OwnerLifecycle::Open,
                    producer_view,
                    #[cfg(test)]
                    depth,
                });
                self.revision = next_revision;
                #[cfg(test)]
                record_owner_open(false, depth);
                Ok(owner)
            }
        }
    }

    /// Discard the most recently planned nested owner before its delta opens.
    ///
    /// A fallible structural route may allocate goals while deciding whether
    /// it applies.  Declining that route must remove the whole still-private
    /// owner rather than leave unreachable goals in the live domain.  The
    /// last-owner requirement proves that no later owner can refer to it; the
    /// unopened leaf requirements prove that no delta or child can carry its
    /// authority.
    pub(crate) fn discard_unopened_leaf_owner(&mut self, owner: TypeGoalOwner, span: Span) {
        assert!(
            owner.domain().belongs_to(self.nonce),
            "planner tried to discard an inference owner from another store"
        );
        let next_revision = self.next_revision();
        let domain = self
            .domains
            .get_mut(owner.domain().index() as usize)
            .expect("planner tried to discard an owner from a missing domain");
        assert_eq!(
            owner.index() as usize + 1,
            domain.owners.len(),
            "planner tried to discard an inference owner after allocating a successor"
        );
        let discarded = domain
            .owners
            .last()
            .expect("planner tried to discard an owner from an empty domain");
        assert_eq!(
            discarded.lifecycle,
            OwnerLifecycle::Open,
            "planner tried to discard a closed inference owner"
        );
        assert_eq!(
            discarded.open_children, 0,
            "planner tried to discard an inference owner with open children"
        );
        let parent = discarded
            .parent
            .expect("only a provisional nested owner may be discarded");
        #[cfg(test)]
        let discarded_goals = discarded.goals.len();
        domain.owners.pop();
        let parent = domain
            .owners
            .get_mut(parent.index() as usize)
            .expect("discarded inference owner's parent disappeared");
        assert!(
            parent.open_children > 0,
            "discarded inference owner was not counted by its parent at {span:?}"
        );
        parent.open_children -= 1;
        self.revision = next_revision;
        #[cfg(test)]
        record_owner_discard(discarded_goals);
    }

    pub(crate) fn alloc_goal(
        &mut self,
        owner: TypeGoalOwner,
        kind: Kind,
        solution_policy: GoalSolutionPolicy,
        origin: GoalOrigin,
    ) -> Result<TypeGoalRef, Error> {
        self.alloc_goal_with_requirement(
            owner,
            kind,
            solution_policy,
            GoalCloseRequirement::Required,
            origin,
        )
    }

    fn alloc_goal_with_requirement(
        &mut self,
        owner: TypeGoalOwner,
        kind: Kind,
        solution_policy: GoalSolutionPolicy,
        close_requirement: GoalCloseRequirement,
        origin: GoalOrigin,
    ) -> Result<TypeGoalRef, Error> {
        let next_revision = self.next_revision();
        let state = self.owner_state_mut(owner, origin.span)?;
        assert_eq!(
            state.lifecycle,
            OwnerLifecycle::Open,
            "planner tried to allocate an inference goal after its owner closed"
        );
        let slot_index =
            u32::try_from(state.goals.len()).expect("type-inference goal count exceeded u32");
        let goal = TypeGoalRef::new(owner, TypeGoalSlot::from_index(slot_index));
        state.goals.push(GoalState {
            kind,
            solution_policy,
            close_requirement,
            origin,
            solution: None,
        });
        self.revision = next_revision;
        #[cfg(test)]
        record_goal_reserved();
        Ok(goal)
    }

    /// Reserve a finite owner-local inference slot whose lexical layer may
    /// remain unapplied. The dormant slot is ignored at owner close, and the
    /// returned capability cannot be placed in a type until activation for
    /// this exact open owner, before the slot's first semantic use.
    pub(crate) fn reserve_goal(
        &mut self,
        owner: TypeGoalOwner,
        kind: Kind,
        solution_policy: GoalSolutionPolicy,
        origin: GoalOrigin,
    ) -> Result<ReservedGoalRef, Error> {
        self.alloc_goal_with_requirement(
            owner,
            kind,
            solution_policy,
            GoalCloseRequirement::ActivateOnUse,
            origin,
        )
        .map(ReservedGoalRef)
    }

    /// Append one inference goal to the exact owner covered by a live delta.
    /// Retained public-plan, projected-header, and recursive-order function-
    /// shell staging use this only after completing their read-only structural
    /// preflight. Completed public-plan or header writes may precede it, but no
    /// equation transaction, fill adoption, or retained-child/body advancement
    /// may overlap or follow before all reservations for that structural plan
    /// have been made.
    pub(crate) fn reserve_goal_in_live_delta(
        &mut self,
        delta: &mut GoalDelta,
        kind: Kind,
        solution_policy: GoalSolutionPolicy,
        origin: GoalOrigin,
    ) -> Result<TypeGoalRef, Error> {
        assert!(
            delta.equation_undo.is_none(),
            "retained planning reserved a live-delta inference goal during an active equation"
        );
        self.require_delta_owner(delta, origin.span)?;
        let goal = self.alloc_goal_with_requirement(
            delta.owner,
            kind,
            solution_policy,
            GoalCloseRequirement::Required,
            origin,
        )?;
        delta.owner_goal_count = delta
            .owner_goal_count
            .checked_add(1)
            .expect("type-inference goal count exceeded usize");
        Ok(goal)
    }

    /// Export a prepared lexical-binder abstraction through its exact live
    /// child delta, retaining the ordinary direct-parent export checks.
    pub(crate) fn reserve_prepared_parent_header_export(
        &mut self,
        parent_delta: &mut GoalDelta,
        child_delta: &GoalDelta,
        header: PreparedCloseType,
        span: Span,
    ) -> Result<ReservedParentHeaderExport, Error> {
        assert_eq!(
            header.delta,
            child_delta.authority(),
            "a prepared projected header crossed its speculative delta"
        );
        assert_eq!(
            header.owner, child_delta.owner,
            "a prepared projected header crossed its retained owner"
        );
        self.reserve_parent_header_export(parent_delta, child_delta, header.value, span)
    }

    /// Reserve a direct parent's counterparts for every child-local goal head
    /// in one progressed structural header, without adding an equation.
    ///
    /// Repeated applications of one child goal share one parent goal; only the
    /// head changes, so each occurrence keeps its full argument tree. Goals
    /// already owned by the parent or one of its ancestors remain unchanged.
    /// The caller must reserve every sibling export before installing any of
    /// them with [`GoalStore::install_parent_header_export`].
    pub(crate) fn reserve_parent_header_export(
        &mut self,
        parent_delta: &mut GoalDelta,
        child_delta: &GoalDelta,
        child_header: ScopedType,
        span: Span,
    ) -> Result<ReservedParentHeaderExport, Error> {
        self.require_delta_owner(parent_delta, span)?;
        self.require_delta_owner(child_delta, span)?;
        assert!(
            parent_delta.equation_undo.is_none() && child_delta.equation_undo.is_none(),
            "projected header export overlapped an open goal equation"
        );
        let parent_owner = parent_delta.owner;
        let child_owner = child_delta.owner;
        assert_eq!(
            self.owner_state(child_owner, span)?.parent,
            Some(parent_owner),
            "projected header export skipped an inference-owner boundary"
        );
        self.require_scoped_goal_chain(child_owner, &child_header, span)?;

        // Keep the exact source vocabulary. Canonicalization belongs to the
        // isolated evaluator view and to close-time comparison, not to this
        // occurrence-local header.
        let child = self.zonk_with_delta(&child_header, child_delta)?;
        self.require_scoped_goal_chain(child_owner, &child, span)?;
        let parent_scope = self.owner_state(parent_owner, span)?.scope.clone();
        let mut free = HashSet::new();
        collect_free_type_vars(child.ty.as_type(), &mut free);
        if free
            .iter()
            .any(|name| child.scope.binding(name) != parent_scope.binding(name))
        {
            return Err(Error::type_(
                span,
                "a projected header would let a nested rigid type variable escape its scope",
            ));
        }

        // Gather every fallible fact before allocating the first parent goal.
        // Once allocation begins, each reservation is infallible for this
        // already-validated live parent delta.
        let mut parent_ty = child.ty.clone_type();
        let mut proxies = Vec::new();
        for goal in goal_heads_in_structural_order(&parent_ty) {
            self.require_goal_usable_in_delta(child_delta, goal, span)?;
            if goal.owner() == child_owner {
                let state = self.goal_state(goal, span)?;
                proxies.push((
                    goal,
                    state.kind.clone(),
                    self.effective_solution_policy(goal, Some(child_delta), span)?,
                    state.origin.clone(),
                ));
            } else {
                assert!(
                    self.is_ancestor_or_same(goal.owner(), parent_owner, span)?,
                    "projected header retained a peer or descendant inference goal"
                );
            }
        }

        let mut replacements = HashMap::with_capacity(proxies.len());
        for (child_goal, kind, solution_policy, origin) in proxies {
            let parent_goal =
                self.reserve_goal_in_live_delta(parent_delta, kind, solution_policy, origin)?;
            replacements.insert(child_goal, parent_goal);
        }
        replace_goal_heads(&mut parent_ty, &replacements);
        let parent =
            ScopedType::from_parts(parent_ty, parent_scope, child.ty.identity_is_canonical());
        self.require_scoped_goal_chain(parent_owner, &parent, span)?;
        Ok(ReservedParentHeaderExport {
            child_delta: child_delta.authority(),
            child,
            parent,
        })
    }

    /// Install one already-reserved header relation in the exact child delta
    /// that can legally name both the source goals and their parent proxies.
    /// The parent-scoped header becomes observable only after that equation is
    /// accepted.
    pub(crate) fn install_parent_header_export(
        &self,
        child_delta: &mut GoalDelta,
        reserved: ReservedParentHeaderExport,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            child_delta.authority(),
            reserved.child_delta,
            "projected header export was installed through another child delta"
        );
        self.require_delta_owner(child_delta, span)?;
        let child_owner = child_delta.owner;
        let parent_owner = self
            .owner_state(child_owner, span)?
            .parent
            .expect("projected header export child lost its direct parent");
        let parent_at_child =
            self.use_retained_type_at(parent_owner, child_owner, reserved.parent.clone(), span)?;
        assert!(
            reserved.child.projected_scope_compatible(&parent_at_child),
            "projected header export lost child rigid evidence used by its type"
        );
        self.constrain(child_delta, reserved.child, parent_at_child, span, ctx)?;
        Ok(reserved.parent)
    }

    /// Activate one reserved lexical slot for the remainder of its open-owner
    /// lifecycle and construct its first scoped occurrence. The diagnostic
    /// span replaces the provisional reservation span once the dynamic packet
    /// boundary is known; the occurrence span remains the lexical binder
    /// position carried by the returned type. Activation is one-shot and
    /// monotonic for the remainder of the open owner's lifecycle: pending work
    /// retains the activated slot, and close treats it like an ordinary required
    /// goal.
    pub(crate) fn activate_reserved_goal_at(
        &mut self,
        owner: TypeGoalOwner,
        reserved: ReservedGoalRef,
        use_site_owner: TypeGoalOwner,
        diagnostic_span: Span,
        occurrence_span: Span,
    ) -> Result<(TypeGoalRef, ScopedType), Error> {
        let goal = reserved.0;
        assert_eq!(
            goal.owner(),
            owner,
            "planner activated a reserved inference goal through another owner"
        );
        assert!(
            self.is_ancestor_or_same(goal.owner(), use_site_owner, occurrence_span)?,
            "planner activated a reserved inference goal at a peer, ancestor, or foreign owner"
        );
        let next_revision = self.next_revision();
        let state = self.owner_state_mut(owner, occurrence_span)?;
        assert_eq!(
            state.lifecycle,
            OwnerLifecycle::Open,
            "planner activated a reserved inference goal after its owner closed"
        );
        let goal_state = state
            .goals
            .get_mut(goal.slot().index() as usize)
            .expect("reserved inference goal disappeared before activation");
        assert_eq!(
            goal_state.close_requirement,
            GoalCloseRequirement::ActivateOnUse,
            "planner activated a reserved inference goal twice or activated an ordinary goal"
        );
        assert!(
            goal_state.solution.is_none(),
            "a dormant reserved inference goal had a solution before activation"
        );
        goal_state.origin.span = diagnostic_span;
        goal_state.close_requirement = GoalCloseRequirement::Required;
        self.revision = next_revision;
        let scope = self
            .owner_state(use_site_owner, occurrence_span)?
            .scope
            .clone();
        Ok((
            goal,
            ScopedType::fresh(
                Type::Goal {
                    goal,
                    args: Vec::new(),
                    meta: Meta::new(occurrence_span),
                    ext: (),
                },
                scope,
            ),
        ))
    }

    fn scoped_goal(
        &self,
        goal: TypeGoalRef,
        args: Vec<Type<Lowered>>,
        span: Span,
    ) -> Result<ScopedType, Error> {
        self.goal_state(goal, span)?;
        let scope = self.owner_state(goal.owner(), span)?.scope.clone();
        Ok(ScopedType::fresh(
            Type::Goal {
                goal,
                args,
                meta: Meta::new(span),
                ext: (),
            },
            scope,
        ))
    }

    /// Construct a goal occurrence with the lexical evidence at its actual
    /// use site. An ancestor goal used below a nested owner must retain the
    /// child's additional rigid binders for its arguments.
    pub(crate) fn scoped_goal_at(
        &self,
        goal: TypeGoalRef,
        args: Vec<Type<Lowered>>,
        use_site_owner: TypeGoalOwner,
        span: Span,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            self.goal_state(goal, span)?.close_requirement,
            GoalCloseRequirement::Required,
            "planner materialized a reserved inference goal without activating it"
        );
        assert!(
            self.is_ancestor_or_same(goal.owner(), use_site_owner, span)?,
            "planner used an inference goal at a peer, ancestor, or foreign owner"
        );
        let scope = self.owner_state(use_site_owner, span)?.scope.clone();
        Ok(ScopedType::fresh(
            Type::Goal {
                goal,
                args,
                meta: Meta::new(span),
                ext: (),
            },
            scope,
        ))
    }

    /// Attach one unsplit planner type to the exact lexical scope retained by
    /// `owner`. The input is either one raw source-context tree or one wholly
    /// canonical tree; callers must compose previously scoped values through
    /// the store's opaque composition APIs rather than rebuilding a
    /// mixed-provenance `Type`.
    pub(crate) fn scoped_type(
        &self,
        owner: TypeGoalOwner,
        ty: InternedType<Lowered>,
        span: Span,
    ) -> Result<ScopedType, Error> {
        assert!(
            !contains_goal_beneath_forall(ty.as_type(), false),
            "only retained lexical-binder abstraction may place an inference goal beneath a `Forall`"
        );
        let scope = self.owner_state(owner, span)?.scope.clone();
        Ok(ScopedType::new(ty, scope))
    }

    /// Attach a reconstructed type beneath fresh lexical `forall` binders
    /// while preserving the live owner's exact goal and rigid authority.
    pub(crate) fn scoped_type_with_lexical_foralls(
        &self,
        owner: TypeGoalOwner,
        ty: InternedType<Lowered>,
        params: &[TypeParam],
        span: Span,
    ) -> Result<ScopedType, Error> {
        let mut scope = self.owner_state(owner, span)?.scope.clone();
        for param in params {
            scope = scope.with_lexical_forall(param);
        }
        for goal in goal_refs(ty.as_type()) {
            self.require_goal_in_owner_chain(owner, goal, span)?;
            let goal_scope = &self.owner_state(goal.owner(), span)?.scope;
            assert!(
                scope.extends(goal_scope),
                "reconstructed type omitted or conflicted with an embedded goal's rigid evidence"
            );
        }
        Ok(ScopedType::new(ty, scope))
    }

    /// Attach a type to the exact logical lexical prefix at which it was
    /// derived, even when an owner-free slice is physically carried by a
    /// later retained owner. Goal-free values retain only their referenced
    /// rigid proofs; goal-bearing values retain the full prefix required to
    /// validate every ancestor goal occurrence.
    pub(crate) fn scoped_type_at_lexical_prefix(
        &self,
        scope: &RigidScope,
        ty: InternedType<Lowered>,
    ) -> ScopedType {
        assert_eq!(
            scope.store_nonce, self.nonce,
            "planner passed a lexical prefix from another goal store"
        );
        if let Some(context) = self.context {
            assert_eq!(
                scope.context, context,
                "planner mixed package-analysis contexts in one lexical prefix"
            );
        }
        let scope = if super::type_contains_goal(ty.as_type()) {
            scope.clone()
        } else {
            scope.projected_for_goal_free_type(ty.as_type())
        };
        ScopedType::new(ty, scope)
    }

    /// Reattach a structural-`forall` reconstruction to the exact logical
    /// prefix under which its canonical tree was built. The temporary
    /// retained owner may be a later physical carrier, but every embedded
    /// goal must already be authorized by this narrower prefix.
    pub(crate) fn rebase_reconstructed_type_to_lexical_prefix(
        &self,
        mut value: ScopedType,
        scope: &RigidScope,
        span: Span,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            scope.store_nonce, self.nonce,
            "planner passed a reconstructed lexical prefix from another goal store"
        );
        for goal in goal_refs(value.ty.as_type()) {
            self.require_goal_in_owner_chain(goal.owner(), goal, span)?;
            let goal_scope = &self.owner_state(goal.owner(), span)?.scope;
            assert!(
                scope.extends(goal_scope),
                "reconstructed inference-goal occurrence omitted or conflicted with its logical prefix"
            );
        }
        value.scope = scope.clone();
        Ok(value)
    }

    /// Consume an already-scoped retained value at its declared destination
    /// or one of that destination's nested inference owners. When every free
    /// rigid keeps the same proof at the use site, strengthen the occurrence
    /// to that exact descendant scope so publications created there can close
    /// through each direct parent. A shadowed rigid keeps its original scope.
    pub(crate) fn use_retained_type_at(
        &self,
        destination: TypeGoalOwner,
        use_site: TypeGoalOwner,
        value: ScopedType,
        span: Span,
    ) -> Result<ScopedType, Error> {
        assert!(
            self.is_ancestor_or_same(destination, use_site, span)?,
            "retained ordinary type consumed outside its destination owner chain"
        );
        self.require_scoped_goal_chain(use_site, &value, span)?;
        let use_scope = &self.owner_state(use_site, span)?.scope;
        Ok(value.strengthen_scope_if_compatible(use_scope))
    }

    pub(crate) fn begin_delta(&self, owner: TypeGoalOwner, span: Span) -> Result<GoalDelta, Error> {
        let state = self.owner_state(owner, span)?;
        assert_eq!(
            state.lifecycle,
            OwnerLifecycle::Open,
            "planner tried to begin an inference delta for a closed owner"
        );
        let token = if self.nonce.is_process() {
            NEXT_GOAL_DELTA_TOKEN
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .map(GoalDeltaToken)
                .expect("process exhausted unique speculative goal-delta identities")
        } else if self.nonce == TypeGoalStoreIdentity::ISOLATED {
            GoalDeltaToken(0)
        } else {
            unreachable!("a goal store cannot retain an unstamped test identity")
        };
        Ok(GoalDelta {
            token,
            owner,
            owner_goal_count: state.goals.len(),
            writes: BTreeMap::new(),
            policy_meets: BTreeMap::new(),
            equation_undo: None,
            read_view: GoalReadView::Owner,
        })
    }

    fn producer_view(&self, id: ProducerViewId) -> &ProducerView {
        &self.producer_views[id.0.get() as usize - 1]
    }

    fn producer_view_mut(&mut self, id: ProducerViewId) -> &mut ProducerView {
        &mut self.producer_views[id.0.get() as usize - 1]
    }

    fn push_producer_view(&mut self, view: ProducerView) -> ProducerViewId {
        let index =
            u32::try_from(self.producer_views.len() + 1).expect("producer view count exceeded u32");
        self.producer_views.push(view);
        ProducerViewId(NonZeroU32::new(index).expect("producer view index is nonzero"))
    }

    /// Isolate the ancestor proposals of one retained producer before entering
    /// its body. Reserved lexical children inherit the same exact boundary.
    pub(crate) fn isolate_producer(
        &mut self,
        boundary: TypeGoalOwner,
        context: &GoalDelta,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.require_delta_owner(context, span)?;
        self.owner_state(boundary, span)?;
        let mut context_writes = BTreeMap::new();
        for (&goal, binding) in &context.writes {
            if !self.is_ancestor_or_same(goal.owner(), boundary, binding.span)? {
                continue;
            }
            let value = self.zonk_with_delta(&binding.value, context)?;
            if goal_refs(value.ty.as_type()).into_iter().any(|remaining| {
                !self
                    .is_ancestor_or_same(remaining.owner(), boundary, binding.span)
                    .expect("accepted context goal belongs to this store")
            }) {
                continue;
            }
            context_writes.insert(
                goal,
                GoalBinding {
                    value,
                    span: binding.span,
                },
            );
        }
        let context_policies = context
            .policy_meets
            .iter()
            .filter(|(goal, meet)| {
                self.is_ancestor_or_same(goal.owner(), boundary, meet.span)
                    .expect("accepted context policy belongs to this store")
            })
            .map(|(&goal, &meet)| (goal, meet))
            .collect::<BTreeMap<_, _>>();
        let inherited = self.owner_state(boundary, span)?.producer_view;
        let id = match inherited {
            Some(id) if self.producer_view(id).boundary == boundary => id,
            _ => {
                let id = self.push_producer_view(ProducerView {
                    boundary,
                    parent: inherited,
                    accepted: false,
                    context_writes: BTreeMap::new(),
                    context_policies: BTreeMap::new(),
                    writes: BTreeMap::new(),
                    policy_meets: BTreeMap::new(),
                });
                let descendants = self.domains[boundary.domain().index() as usize]
                    .owners
                    .iter()
                    .enumerate()
                    .skip(boundary.index() as usize)
                    .filter(|(_, state)| state.lifecycle == OwnerLifecycle::Open)
                    .map(|(index, _)| TypeGoalOwner::new(boundary.domain(), index as u32))
                    .filter(|&owner| {
                        self.is_ancestor_or_same(boundary, owner, span)
                            .expect("reserved producer owner chain is valid")
                    })
                    .collect::<Vec<_>>();
                for owner in descendants {
                    let state = &mut self.domains[owner.domain().index() as usize].owners
                        [owner.index() as usize];
                    assert_eq!(state.producer_view, inherited);
                    state.producer_view = Some(id);
                }
                self.revision = self.next_revision();
                id
            }
        };
        // Reconcile the live accepted delta before changing the read context.
        // These equations only inspect the existing producer; no source is run.
        let mut trial = self.begin_delta(boundary, span)?;
        trial.read_view = GoalReadView::EnclosingProducer;
        for (&goal, meet) in &self.producer_view(id).policy_meets {
            let policy = self
                .effective_solution_policy(goal, Some(&trial), meet.span)?
                .meet(meet.policy);
            trial.write_policy_meet(
                goal,
                GoalPolicyMeet {
                    policy,
                    span: meet.span,
                },
            );
        }
        for (&goal, binding) in &self.producer_view(id).writes {
            self.bind_goal(
                &mut trial,
                goal,
                binding.value.clone(),
                None,
                binding.span,
                ctx,
            )?;
        }
        for (&goal, meet) in &context_policies {
            self.require_goal_usable_in_delta(&trial, goal, meet.span)?;
            let policy = self
                .effective_solution_policy(goal, Some(&trial), meet.span)?
                .meet(meet.policy);
            trial.write_policy_meet(
                goal,
                GoalPolicyMeet {
                    policy,
                    span: meet.span,
                },
            );
        }
        for (&goal, binding) in &context_writes {
            self.bind_goal(
                &mut trial,
                goal,
                binding.value.clone(),
                None,
                binding.span,
                ctx,
            )?;
        }
        for (&goal, meet) in &trial.policy_meets {
            let open = self.scoped_goal(goal, Vec::new(), meet.span)?;
            let value = self.zonk_with_delta(&open, &trial)?;
            self.require_solution_policy(goal, &value, Some(&trial), meet.span)?;
        }
        let view = self.producer_view_mut(id);
        view.writes.extend(trial.writes);
        view.policy_meets.extend(trial.policy_meets);
        view.context_writes.extend(context_writes);
        for (&goal, meet) in &context_policies {
            view.context_policies
                .entry(goal)
                .and_modify(|previous| previous.policy = previous.policy.meet(meet.policy))
                .or_insert(*meet);
        }
        self.revision = self.next_revision();
        Ok(())
    }

    fn delta_producer_view(&self, delta: &GoalDelta) -> Option<ProducerViewId> {
        match delta.read_view {
            GoalReadView::Trial(id) => Some(id),
            GoalReadView::Owner | GoalReadView::EnclosingProducer => {
                self.domains[delta.owner.domain().index() as usize].owners
                    [delta.owner.index() as usize]
                    .producer_view
            }
        }
    }

    fn view_binding(&self, goal: TypeGoalRef, delta: &GoalDelta) -> Option<&GoalBinding> {
        if self.producer_views.is_empty() {
            return None;
        }
        let mut view = self.delta_producer_view(delta);
        let mut skip_private = matches!(delta.read_view, GoalReadView::EnclosingProducer);
        while let Some(id) = view {
            let current = self.producer_view(id);
            if let Some(binding) = current.context_writes.get(&goal) {
                return Some(binding);
            }
            if !skip_private && let Some(binding) = current.writes.get(&goal) {
                return Some(binding);
            }
            skip_private = false;
            view = current.parent;
        }
        None
    }

    fn write_producer_view(
        &self,
        owner: TypeGoalOwner,
        goal: TypeGoalRef,
    ) -> Option<ProducerViewId> {
        let mut id = self
            .owner_state(owner, Span::new(0, 0))
            .expect("live owner")
            .producer_view?;
        if self.producer_view(id).boundary == owner {
            assert!(
                self.producer_view(id).accepted,
                "producer closed before wave acceptance"
            );
            id = self.producer_view(id).parent?;
        }
        self.is_strict_ancestor(
            goal.owner(),
            self.producer_view(id).boundary,
            Span::new(0, 0),
        )
        .expect("validated producer target ancestry")
        .then_some(id)
    }

    pub(crate) fn producer_obligations(
        &self,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
    ) -> Result<ProducerObligations, Error> {
        let prepared =
            self.prepare_owner_with_publication_snapshot(delta, Vec::new(), Vec::new(), ctx)?;
        let mut obligations = ProducerObligations {
            owner: delta.owner,
            writes: prepared.commit.delta.writes,
            policy_meets: prepared.commit.delta.policy_meets,
        };
        self.retain_wave_context_policies(delta, &mut obligations)?;
        Ok(obligations)
    }

    fn retain_wave_context_policies(
        &self,
        delta: &GoalDelta,
        obligations: &mut ProducerObligations,
    ) -> Result<(), Error> {
        let goals = obligations
            .writes
            .keys()
            .chain(obligations.policy_meets.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        for goal in goals {
            let span = self.goal_state(goal, Span::new(0, 0))?.origin.span;
            let policy = self.effective_solution_policy(goal, Some(delta), span)?;
            obligations
                .policy_meets
                .entry(goal)
                .and_modify(|meet| meet.policy = meet.policy.meet(policy))
                .or_insert(GoalPolicyMeet { policy, span });
        }
        Ok(())
    }

    fn normalized_wave_binding(
        &self,
        goal: TypeGoalRef,
        binding: &GoalBinding,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
    ) -> Result<Option<GoalBinding>, Error> {
        let mut value = self.zonk_with_delta(&binding.value, delta)?;
        for remaining in goal_refs(value.ty.as_type()) {
            if !self.is_ancestor_or_same(remaining.owner(), goal.owner(), binding.span)? {
                return Ok(None);
            }
        }
        let scope = &self.owner_state(goal.owner(), binding.span)?.scope;
        let mut free = HashSet::new();
        collect_free_type_vars(value.ty.as_type(), &mut free);
        if free.iter().any(|name| {
            value.scope.binding(name).is_some() && scope.binding(name) != value.scope.binding(name)
        }) {
            return Err(Error::type_(
                binding.span,
                "an inferred type would let a nested rigid type variable escape its scope",
            ));
        }
        value.scope = scope.clone();
        self.require_solution_policy(goal, &value, Some(delta), binding.span)?;
        assert_eq!(
            self.kind_of(&value, delta, ctx, &mut BTreeSet::new())?,
            self.goal_state(goal, binding.span)?.kind,
            "validated producer binding changed kind during normalization"
        );
        Ok(Some(GoalBinding {
            value,
            span: binding.span,
        }))
    }

    /// Validate one finite completed-producer wave under the original owner of
    /// every obligation. The temporary read view combines equations, not owner
    /// authority; no source action can run while it exists.
    pub(crate) fn accept_producer_wave(
        &mut self,
        host: &mut GoalDelta,
        mut wrappers: Vec<&mut GoalDelta>,
        producers: Vec<ProducerObligations>,
        equations: Vec<(ScopedType, ScopedType, Span)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        let span = equations.first().map_or(Span::new(0, 0), |entry| entry.2);
        self.require_delta_owner(host, span)?;
        let mut owners = BTreeSet::from([host.owner]);
        for wrapper in &wrappers {
            self.require_delta_owner(wrapper, span)?;
            assert!(
                owners.insert(wrapper.owner),
                "a producer wave borrowed one delta twice"
            );
        }
        let mut wrapper_contexts = Vec::with_capacity(wrappers.len());
        for delta in &wrappers {
            let mut context = ProducerObligations {
                owner: delta.owner,
                writes: BTreeMap::new(),
                policy_meets: delta.policy_meets.clone(),
            };
            for (&goal, binding) in &delta.writes {
                let binding = self
                    .normalized_wave_binding(goal, binding, delta, ctx)?
                    .expect("retained pair header proxies must point only to ancestor goals");
                context.writes.insert(goal, binding);
            }
            self.retain_wave_context_policies(delta, &mut context)?;
            wrapper_contexts.push(context);
        }
        let host_parent_view = self.owner_state(host.owner, span)?.producer_view;
        let trial_id = self.push_producer_view(ProducerView {
            boundary: host.owner,
            parent: None,
            accepted: false,
            context_writes: BTreeMap::new(),
            context_policies: BTreeMap::new(),
            writes: BTreeMap::new(),
            policy_meets: BTreeMap::new(),
        });
        let result = (|| {
            // Tighten every policy before testing any candidate solution.
            for producer in wrapper_contexts.iter().chain(&producers) {
                let mut delta = self.begin_delta(producer.owner, span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                for (&goal, meet) in &producer.policy_meets {
                    self.require_goal_usable_in_delta(&delta, goal, meet.span)?;
                    let policy = self
                        .effective_solution_policy(goal, Some(&delta), meet.span)?
                        .meet(meet.policy);
                    self.producer_view_mut(trial_id).policy_meets.insert(
                        goal,
                        GoalPolicyMeet {
                            policy,
                            span: meet.span,
                        },
                    );
                }
            }
            for producer in wrapper_contexts.iter().chain(&producers) {
                let mut delta = self.begin_delta(producer.owner, span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                for (&goal, binding) in &producer.writes {
                    self.bind_goal(
                        &mut delta,
                        goal,
                        binding.value.clone(),
                        None,
                        binding.span,
                        ctx,
                    )?;
                }
                for (&goal, meet) in &producer.policy_meets {
                    let open = self.scoped_goal(goal, Vec::new(), meet.span)?;
                    let value = self.zonk_with_delta(&open, &delta)?;
                    self.require_solution_policy(goal, &value, Some(&delta), meet.span)?;
                }
                let view = self.producer_view_mut(trial_id);
                view.writes.extend(delta.writes);
                view.policy_meets.extend(delta.policy_meets);
            }
            let mut shared = BTreeMap::new();
            for (&goal, binding) in &self.producer_view(trial_id).writes {
                let mut delta = self.begin_delta(goal.owner(), binding.span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                let binding = self
                    .normalized_wave_binding(goal, binding, &delta, ctx)?
                    .expect("completed producer obligations must be target-transport-ready");
                shared.insert(goal, binding);
            }
            self.producer_view_mut(trial_id).writes = shared;

            // Only the actual host may reconcile its unfinished local structure
            // and inherited view. Original producer equations never read them.
            let mut host_reconciled = self.begin_delta(host.owner, span)?;
            host_reconciled.writes = host.writes.clone();
            host_reconciled.policy_meets = host.policy_meets.clone();
            assert!(matches!(host_reconciled.read_view, GoalReadView::Owner));
            for (&goal, meet) in &self.producer_view(trial_id).policy_meets {
                if self.is_ancestor_or_same(goal.owner(), host.owner, meet.span)? {
                    let policy = self
                        .effective_solution_policy(goal, Some(&host_reconciled), meet.span)?
                        .meet(meet.policy);
                    host_reconciled.write_policy_meet(
                        goal,
                        GoalPolicyMeet {
                            policy,
                            span: meet.span,
                        },
                    );
                }
            }
            for (&goal, binding) in &self.producer_view(trial_id).writes {
                if self.is_ancestor_or_same(goal.owner(), host.owner, binding.span)? {
                    self.bind_goal(
                        &mut host_reconciled,
                        goal,
                        binding.value.clone(),
                        None,
                        binding.span,
                        ctx,
                    )?;
                }
            }
            self.constrain_equations_atomically(&mut host_reconciled, equations, ctx)?;
            let mut validation_goals = self
                .producer_view(trial_id)
                .writes
                .keys()
                .chain(self.producer_view(trial_id).policy_meets.keys())
                .chain(host.writes.keys())
                .chain(host.policy_meets.keys())
                .chain(host_reconciled.writes.keys())
                .chain(host_reconciled.policy_meets.keys())
                .copied()
                .collect::<BTreeSet<_>>();
            for &goal in &validation_goals {
                if !self.is_ancestor_or_same(goal.owner(), host.owner, span)? {
                    continue;
                }
                let policy = self.effective_solution_policy(goal, Some(&host_reconciled), span)?;
                self.producer_view_mut(trial_id)
                    .policy_meets
                    .entry(goal)
                    .and_modify(|meet| meet.policy = meet.policy.meet(policy))
                    .or_insert(GoalPolicyMeet { policy, span });
                if let Some(value) = self.binding(goal, Some(&host_reconciled)) {
                    let binding = GoalBinding { value, span };
                    if let Some(binding) =
                        self.normalized_wave_binding(goal, &binding, &host_reconciled, ctx)?
                    {
                        self.producer_view_mut(trial_id)
                            .writes
                            .insert(goal, binding);
                    }
                }
            }
            let target = |goal: TypeGoalRef| -> Result<Option<usize>, Error> {
                if self.is_ancestor_or_same(goal.owner(), host.owner, span)? {
                    return Ok(Some(0));
                }
                if let Some(index) = wrappers
                    .iter()
                    .position(|delta| delta.owner == goal.owner())
                {
                    return Ok(Some(index + 1));
                }
                assert!(
                    producers
                        .iter()
                        .any(|producer| producer.owner == goal.owner()),
                    "a projected producer ancestor target was neither on the host chain nor an owned pair wrapper"
                );
                Ok(None)
            };
            let mut writes = (0..=wrappers.len())
                .map(|_| BTreeMap::new())
                .collect::<Vec<_>>();
            let mut policies = (0..=wrappers.len())
                .map(|_| BTreeMap::new())
                .collect::<Vec<_>>();
            writes[0] = host_reconciled.writes.clone();
            policies[0] = host_reconciled.policy_meets.clone();
            for (&goal, binding) in &self.producer_view(trial_id).writes {
                let mut delta = self.begin_delta(goal.owner(), binding.span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                let mut value = self.zonk_with_delta(&binding.value, &delta)?;
                for remaining in goal_refs(value.ty.as_type()) {
                    assert!(
                        self.is_ancestor_or_same(remaining.owner(), goal.owner(), binding.span)?,
                        "a producer wave retained a descendant goal in an ancestor solution"
                    );
                }
                let scope = &self.owner_state(goal.owner(), binding.span)?.scope;
                let mut free = HashSet::new();
                collect_free_type_vars(value.ty.as_type(), &mut free);
                if free.iter().any(|name| {
                    value.scope.binding(name).is_some()
                        && scope.binding(name) != value.scope.binding(name)
                }) {
                    return Err(Error::type_(
                        binding.span,
                        "an inferred type would let a nested rigid type variable escape its scope",
                    ));
                }
                value.scope = scope.clone();
                self.require_solution_policy(goal, &value, Some(&delta), binding.span)?;
                assert_eq!(
                    self.kind_of(&value, &delta, ctx, &mut BTreeSet::new())?,
                    self.goal_state(goal, binding.span)?.kind,
                    "jointly validated producer binding changed kind during normalization"
                );
                if let Some(target) = target(goal)? {
                    writes[target].insert(
                        goal,
                        GoalBinding {
                            value,
                            span: binding.span,
                        },
                    );
                }
            }
            for (&goal, &meet) in &self.producer_view(trial_id).policy_meets {
                if let Some(target) = target(goal)? {
                    policies[target]
                        .entry(goal)
                        .and_modify(|previous| previous.policy = previous.policy.meet(meet.policy))
                        .or_insert(meet);
                }
            }
            validation_goals.extend(self.producer_view(trial_id).writes.keys().copied());
            validation_goals.extend(self.producer_view(trial_id).policy_meets.keys().copied());
            // From here the trial is read-only: it checks policy aliases and
            // builds chain-filtered snapshots, never submits another equation.
            {
                let view = self.producer_view_mut(trial_id);
                view.context_writes = host_reconciled.writes;
                view.context_policies = host_reconciled.policy_meets;
                view.parent = host_parent_view;
            }
            for goal in validation_goals {
                let mut delta = self.begin_delta(goal.owner(), span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                let open = self.scoped_goal(goal, Vec::new(), span)?;
                let value = self.zonk_with_delta(&open, &delta)?;
                self.require_solution_policy(goal, &value, Some(&delta), span)?;
            }
            let mut context_goals = host.writes.keys().copied().collect::<BTreeSet<_>>();
            let mut policy_goals = host.policy_meets.keys().copied().collect::<BTreeSet<_>>();
            for wrapper in &wrappers {
                context_goals.extend(wrapper.writes.keys().copied());
                policy_goals.extend(wrapper.policy_meets.keys().copied());
            }
            context_goals.extend(writes.iter().flat_map(|map| map.keys().copied()));
            policy_goals.extend(policies.iter().flat_map(|map| map.keys().copied()));
            let mut snapshots = Vec::new();
            for producer in &producers {
                let Some(id) = self
                    .owner_state(producer.owner, span)?
                    .producer_view
                    .filter(|&id| self.producer_view(id).boundary == producer.owner)
                else {
                    continue;
                };
                let mut delta = self.begin_delta(producer.owner, span)?;
                delta.read_view = GoalReadView::Trial(trial_id);
                let mut accepted_writes = BTreeMap::new();
                let mut accepted_policies = BTreeMap::new();
                for &goal in &context_goals {
                    if !self.is_ancestor_or_same(goal.owner(), producer.owner, span)? {
                        continue;
                    }
                    let Some(value) = self.binding(goal, Some(&delta)) else {
                        continue;
                    };
                    let value = self.zonk_with_delta(&value, &delta)?;
                    if goal_refs(value.ty.as_type()).into_iter().any(|remaining| {
                        !self
                            .is_ancestor_or_same(remaining.owner(), producer.owner, span)
                            .expect("wave context belongs to this owner domain")
                    }) {
                        continue;
                    }
                    accepted_writes.insert(goal, GoalBinding { value, span });
                }
                for &goal in &policy_goals {
                    if self.is_ancestor_or_same(goal.owner(), producer.owner, span)? {
                        accepted_policies.insert(
                            goal,
                            GoalPolicyMeet {
                                policy: self.effective_solution_policy(goal, Some(&delta), span)?,
                                span,
                            },
                        );
                    }
                }
                snapshots.push((id, accepted_writes, accepted_policies));
            }
            Ok((writes, policies, snapshots))
        })();
        self.producer_views
            .pop()
            .expect("wave trial view disappeared");
        let (writes, policies, snapshots) = result?;
        let revision = self.next_revision();
        // Mutable destination borrows have remained held throughout preflight.
        // No source callback or fallible planning work follows the first write.
        for ((delta, writes), policies) in std::iter::once(host)
            .chain(wrappers.iter_mut().map(|delta| &mut **delta))
            .zip(writes)
            .zip(policies)
        {
            for (goal, binding) in writes {
                delta.write_solution(goal, binding);
            }
            for (goal, meet) in policies {
                delta.write_policy_meet(goal, meet);
            }
        }
        for (id, writes, policies) in snapshots {
            let view = self.producer_view_mut(id);
            view.context_writes.extend(writes);
            for (goal, meet) in policies {
                view.context_policies
                    .entry(goal)
                    .and_modify(|previous| previous.policy = previous.policy.meet(meet.policy))
                    .or_insert(meet);
            }
            view.accepted = true;
        }
        self.revision = revision;
        Ok(())
    }

    pub(crate) fn owner_kind(
        &self,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<GoalOwnerKind, Error> {
        Ok(self.owner_state(owner, span)?.kind)
    }

    #[cfg(test)]
    pub(crate) fn owner_goal_count_for_test(&self, owner: TypeGoalOwner, span: Span) -> usize {
        self.owner_state(owner, span)
            .expect("test inspected an unknown goal owner")
            .goals
            .len()
    }

    pub(crate) fn owner_parent(
        &self,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<Option<TypeGoalOwner>, Error> {
        Ok(self.owner_state(owner, span)?.parent)
    }

    pub(crate) fn owner_module_path(
        &self,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<&str, Error> {
        Ok(&self.owner_state(owner, span)?.scope.module_path)
    }

    /// Resolve one prospective close component without committing the delta.
    ///
    /// This is the planner's bridge for rebuilding a lexical signature at its
    /// exact source binder positions. The prepare/commit transition remains
    /// the sole atomic publication operation and repeats zonking before
    /// publication.
    pub(crate) fn zonk_for_close(
        &self,
        delta: &GoalDelta,
        output: ScopedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedCloseType, Error> {
        self.require_context(ctx);
        assert!(
            output.scope.store_nonce == self.nonce,
            "close preparation carried rigid evidence from another goal store"
        );
        self.require_delta_owner(delta, output.ty.span())?;
        self.require_scoped_goal_chain(delta.owner, &output, output.ty.span())?;
        let output = self.canonical_scoped(output, ctx);
        let zonked = self.zonk_with_delta(&output, delta)?;
        assert!(
            zonked.ty.identity_is_canonical(),
            "close preparation failed to establish identity-exact type provenance"
        );
        for remaining in goal_refs(zonked.ty.as_type()) {
            if remaining.owner() == delta.owner {
                return Err(self
                    .goal_state(remaining, zonked.ty.span())?
                    .origin
                    .unresolved_error());
            }
            assert!(
                self.is_strict_ancestor(remaining.owner(), delta.owner, zonked.ty.span())?,
                "close preparation retained a non-ancestor inference goal"
            );
        }
        Ok(PreparedCloseType {
            delta: delta.authority(),
            owner: delta.owner,
            value: zonked,
        })
    }

    /// Inspect already prepared closure evidence without repeating zonking.
    pub(crate) fn prepared_completion_wait(
        &self,
        prepared: &PreparedCloseType,
    ) -> Result<Option<CompletionWait>, Error> {
        let span = prepared.value.ty.span();
        let mut deepest: Option<TypeGoalRef> = None;
        for goal in goal_refs(prepared.value.ty.as_type()) {
            assert!(
                self.is_strict_ancestor(goal.owner(), prepared.owner, span)?,
                "prepared completion retained a local or unrelated goal"
            );
            if deepest.is_none_or(|old| {
                self.is_strict_ancestor(old.owner(), goal.owner(), span)
                    .expect("prepared goals retain their validated owner chain")
            }) {
                deepest = Some(goal);
            }
        }
        Ok(deepest.map(|deepest| CompletionWait {
            delta: prepared.delta,
            origin: prepared.owner,
            deepest,
        }))
    }

    pub(crate) fn validate_completion_wait_origin(
        &self,
        wait: CompletionWait,
        delta: &GoalDelta,
        span: Span,
    ) -> Result<(), Error> {
        self.require_delta_owner(delta, span)?;
        assert_eq!(wait.origin, delta.owner);
        assert_eq!(wait.delta, delta.authority());
        assert!(self.is_strict_ancestor(wait.deepest.owner(), wait.origin, span)?);
        Ok(())
    }

    pub(crate) fn completion_wait_outside_boundary(
        &self,
        wait: CompletionWait,
        boundary: TypeGoalOwner,
        span: Span,
    ) -> Result<bool, Error> {
        assert!(
            self.is_ancestor_or_same(boundary, wait.origin, span)?,
            "completion wait crossed an unrelated source boundary"
        );
        self.is_strict_ancestor(wait.deepest.owner(), boundary, span)
    }

    pub(crate) fn completion_wait_changed(
        &self,
        wait: CompletionWait,
        delta: &GoalDelta,
        span: Span,
    ) -> Result<bool, Error> {
        self.validate_completion_wait_origin(wait, delta, span)?;
        Ok(self.binding(wait.deepest, Some(delta)).is_some())
    }

    pub(crate) fn completion_wait_error(&self, wait: CompletionWait, span: Span) -> Error {
        self.goal_state(wait.deepest, span)
            .expect("completion wait lost its retained inference domain")
            .origin
            .unresolved_error()
    }

    /// Resolve an owner-scoped type through the currently committed ancestor
    /// solutions and this exact speculative delta, returning `None` while any
    /// inference goal remains.  This is a readiness probe for the fixed
    /// ExpectedOnly pass; unlike owner close it neither publishes nor treats
    /// an unresolved ancestor goal as an error.
    pub(crate) fn try_zonk_goal_free(
        &self,
        delta: &GoalDelta,
        value: ScopedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<Option<ScopedType>, Error> {
        let zonked = self.zonk_goal_free_probe(delta, value, ctx)?;
        if type_contains_goal(zonked.ty.as_type()) {
            return Ok(None);
        }
        Ok(Some(zonked))
    }

    /// Inspect the structural progress visible through one exact speculative
    /// delta without requiring every nested goal to have closed.
    ///
    /// This is a readiness probe only: it neither commits the delta nor turns
    /// an unresolved goal into a synthesized type.
    pub(crate) fn zonk_for_progress(
        &self,
        delta: &GoalDelta,
        value: ScopedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        self.zonk_goal_free_probe(delta, value, ctx)
    }

    /// Resolve one type through an exact speculative delta and reject every
    /// remaining goal, including a goal owned by an open ancestor. This is the
    /// explicit layer boundary used when later application inputs must not
    /// retroactively solve an earlier layer.
    pub(crate) fn require_goal_free_after_delta(
        &self,
        delta: &GoalDelta,
        value: ScopedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        let zonked = self.zonk_goal_free_probe(delta, value, ctx)?;
        if let Some(remaining) = goal_refs(zonked.ty.as_type()).into_iter().next() {
            return Err(self
                .goal_state(remaining, zonked.ty.span())?
                .origin
                .unresolved_error());
        }
        Ok(zonked)
    }

    /// Close one projected recipe type under the exact generated type-binder
    /// path maintained by the recipe walker.
    pub(crate) fn close_projected_type_at_lexical_path(
        &self,
        delta: &GoalDelta,
        mut value: ScopedType,
        binders: &[TypeParam],
        scratch: &mut ProjectedTypeCloseScratch,
        ctx: &impl GoalTypeContext,
    ) -> Result<ClosedProjectedType, Error> {
        let span = value.ty.span();
        let root = &self.owner_state(delta.owner, span)?.scope;
        assert_eq!(
            value.scope.store_nonce, root.store_nonce,
            "projected recipe type carried rigid evidence from another goal store"
        );
        assert_eq!(
            (value.scope.context, value.scope.module_path.as_str()),
            (root.context, root.module_path.as_str()),
            "projected recipe type crossed its package or consumer module"
        );
        let mut expected = root.clone();
        for param in binders {
            if expected.binding(param.name.as_str()).is_some() {
                return Err(Error::type_(
                    param.span,
                    "a projected recipe type-binder path would capture an existing rigid",
                ));
            }
            expected = expected.with_lexical_forall(param);
        }

        let mut augmented = value.scope.clone();
        for param in binders {
            let required = expected
                .binding(&param.name)
                .expect("the explicit projected binder was added to its exact path");
            match augmented.binding(&param.name) {
                Some(supplied) if supplied != required => {
                    return Err(Error::type_(
                        span,
                        "a projected recipe type retained a rigid outside its type-binder path",
                    ));
                }
                Some(_) => {}
                None => augmented.insert(param.name.clone(), required.clone()),
            }
        }
        value.scope = augmented;
        let mut closed = self.require_goal_free_after_delta(delta, value, ctx)?;

        let mut valid = true;
        let mut bindings = RigidBindings::default();
        for_each_free_type_var(
            closed.ty.as_type(),
            false,
            &mut scratch.free_rigids,
            &mut scratch.bound_rigids,
            &mut |name| {
                let required = expected.binding(name);
                valid &= closed.scope.binding(name) == required
                    && (required.is_some()
                        || ctx
                            .nominal_head_kind(
                                &[PathSegment::synth(name, span)],
                                0,
                                span,
                                &expected,
                                true,
                            )
                            .is_ok());
                if let Some(binding) = required {
                    bindings = std::mem::take(&mut bindings).push(
                        name.to_owned(),
                        binding.clone(),
                        |left, right| left == right,
                    );
                }
            },
        );
        if !valid {
            return Err(Error::type_(
                span,
                "a projected recipe type closed outside its type-binder path",
            ));
        }
        closed.scope = RigidScope {
            store_nonce: expected.store_nonce,
            context: expected.context,
            module_path: expected.module_path,
            bindings,
        };
        Ok(ClosedProjectedType(closed))
    }

    /// Validate one projected coherence edge whose two scoped occurrences
    /// are already goal-free at this exact owner. This retains rigid proof
    /// identity and nominal qualification while adding no solver writes.
    pub(crate) fn validate_goal_free_projected_type_relation(
        &self,
        delta: &mut GoalDelta,
        actual: &ScopedType,
        expected: &ScopedType,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.require_context(ctx);
        self.require_delta_owner(delta, span)?;
        let owner_scope = &self.owner_state(delta.owner, span)?.scope;
        for value in [actual, expected] {
            assert_eq!(
                value.scope.store_nonce, self.nonce,
                "goal-free projected relation carried rigid evidence from another store"
            );
            assert_eq!(
                (value.scope.context, value.scope.module_path.as_str()),
                (owner_scope.context, owner_scope.module_path.as_str()),
                "goal-free projected relation crossed its package or consumer module"
            );
            if super::type_contains_infer(value.ty.as_type()) {
                return Err(Error::type_(
                    span,
                    "an unresolved source placeholder reached a projected type relation",
                ));
            }
            self.require_escape(value, GoalEscape::ClosedAt(delta.owner))?;
        }

        self.validate_projected_type_relation_without_writes(delta, actual, expected, span, ctx)
    }

    /// Validate one coherence edge whose two occurrences were already closed
    /// under the recipe walker's current lexical path.
    pub(crate) fn validate_closed_projected_type_relation(
        &self,
        delta: &mut GoalDelta,
        actual: &ClosedProjectedType,
        expected: &ClosedProjectedType,
        span: Span,
        diagnostic: &'static str,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.validate_projected_type_relation_without_writes(
            delta,
            &actual.0,
            &expected.0,
            span,
            ctx,
        )
        .map_err(|_| Error::elaborator(span, diagnostic))
    }

    fn validate_projected_type_relation_without_writes(
        &self,
        delta: &mut GoalDelta,
        actual: &ScopedType,
        expected: &ScopedType,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        let writes = delta.writes.len();
        let policy_meets = delta.policy_meets.len();
        let result = self.constrain(delta, actual.clone(), expected.clone(), span, ctx);
        assert_eq!(delta.writes.len(), writes);
        assert_eq!(delta.policy_meets.len(), policy_meets);
        result
    }

    /// Instantiate one already-closed projected forall only after checking
    /// the argument kind through the exact live store and evaluation context.
    pub(crate) fn instantiate_closed_projected_forall(
        &self,
        delta: &GoalDelta,
        scheme: &ClosedProjectedType,
        argument: &ClosedProjectedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<Option<ClosedProjectedType>, Error> {
        let span = scheme.0.ty.span();
        let writes = delta.writes.len();
        let policy_meets = delta.policy_meets.len();
        let result = (|| {
            self.require_context(ctx);
            self.require_delta_owner(delta, span)?;
            let owner_scope = &self.owner_state(delta.owner, span)?.scope;
            for evidence in [&scheme.0, &argument.0] {
                assert_eq!(
                    evidence.scope.store_nonce, self.nonce,
                    "closed projected forall evidence came from another goal store"
                );
                assert_eq!(
                    (evidence.scope.context, evidence.scope.module_path.as_str()),
                    (owner_scope.context, owner_scope.module_path.as_str()),
                    "closed projected forall evidence crossed its package or consumer module"
                );
            }
            let Type::Forall { param, .. } = scheme.0.ty.as_type() else {
                return Ok(None);
            };
            let argument_kind = self.kind_of(&argument.0, delta, ctx, &mut BTreeSet::new())?;
            if argument_kind != param.effective_kind() {
                return Ok(None);
            }
            Ok(scheme
                .0
                .projected_public_forall_prefix_application(std::slice::from_ref(&argument.0))
                .map(ClosedProjectedType))
        })();
        assert_eq!(delta.writes.len(), writes);
        assert_eq!(delta.policy_meets.len(), policy_meets);
        result
    }

    fn zonk_goal_free_probe(
        &self,
        delta: &GoalDelta,
        value: ScopedType,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        self.require_context(ctx);
        assert_eq!(
            value.scope.store_nonce, self.nonce,
            "goal-free readiness probe carried rigid evidence from another goal store"
        );
        self.require_delta_owner(delta, value.ty.span())?;
        self.require_scoped_goal_chain(delta.owner, &value, value.ty.span())?;
        let value = self.canonical_scoped(value, ctx);
        let zonked = self.zonk_with_delta(&value, delta)?;
        assert!(
            zonked.ty.identity_is_canonical(),
            "goal-free readiness probe lost canonical nominal identity"
        );
        Ok(zonked)
    }

    /// Rebind one child-retained whole function scheme to the exact
    /// speculative delta that will close its destination owner.
    pub(crate) fn prepare_retained_function_scheme(
        &self,
        delta: &GoalDelta,
        retained: RetainedFunctionSchemeOutput,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedCloseType, Error> {
        assert_eq!(
            retained.destination, delta.owner,
            "retained function scheme was prepared through a different destination owner"
        );
        let prepared = self.zonk_for_close(delta, retained.value, ctx)?;
        assert!(
            is_function_scheme(prepared.value.ty.as_type()),
            "retained whole-function-scheme proof carried a non-function type"
        );
        Ok(prepared)
    }

    /// Abstract one explicit lexical type binder at the planner's current
    /// right-fold position.
    ///
    /// The proof must be the exact opaque identity allocated for this source
    /// binder and retained across planner retries. This operation never
    /// generalizes a type or invents a `Forall`.
    pub(crate) fn abstract_lexical_forall(
        &self,
        body: PreparedCloseType,
        binder: &RetainedTypeBinder<'_>,
    ) -> PreparedCloseType {
        let delta = body.delta;
        let owner = body.owner;
        let mut body = body.value;
        assert!(
            body.scope.store_nonce == self.nonce,
            "forall abstraction carried rigid evidence from another goal store"
        );
        for goal in goal_refs(body.ty.as_type()) {
            assert!(
                self.is_strict_ancestor(goal.owner(), owner, body.ty.span())
                    .expect("prepared lexical abstraction used an invalid goal owner"),
                "planner tried to abstract a lexical binder around a non-ancestor inference goal"
            );
        }
        let param = binder.param();
        let kind = param.effective_kind();
        let binding = body
            .scope
            .bindings
            .remove(param.name.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "planner tried to abstract lexical binder `{}` outside its retained scope",
                    param.name
                )
            });
        assert_eq!(
            binding,
            RigidBinding {
                kind,
                identity: RigidIdentity::Ambient(binder.id()),
            },
            "planner tried to abstract a same-spelled binder with the wrong opaque identity"
        );
        let identity_canonical = body.ty.identity_is_canonical();
        assert!(
            identity_canonical,
            "forall abstraction received a non-canonical prepared component"
        );
        PreparedCloseType {
            delta,
            owner,
            value: ScopedType::from_parts(
                Type::Forall {
                    param: param.clone(),
                    body: Box::new(body.ty.clone_type()),
                    meta: Meta::new(param.span),
                },
                body.scope,
                identity_canonical,
            ),
        }
    }

    /// Right-fold one resolved source value group and add its function layer.
    /// The same prepared values determine both the domain and ABI arity, so an
    /// inferred Unit parameter cannot disagree with an explicitly Unit one.
    pub(crate) fn value_group_function_for_close(
        &self,
        values: Vec<PreparedCloseType>,
        ret: PreparedCloseType,
        span: Span,
    ) -> Result<PreparedCloseType, Error> {
        let abi_arity = match values.as_slice() {
            [] => 0,
            [value] if matches!(value.value.ty.as_type(), Type::Unit { .. }) => 0,
            _ => values.len(),
        };
        let mut values = values.into_iter().rev();
        let param = match values.next() {
            Some(last) => values.try_fold(last, |right, left| {
                self.compose_for_close(PreparedTypeShell::Product { span }, vec![left, right])
            })?,
            None => PreparedCloseType {
                delta: ret.delta,
                owner: ret.owner,
                value: ScopedType::from_parts(
                    Type::Unit {
                        meta: Meta::new(span),
                    },
                    ret.value.scope.clone(),
                    true,
                ),
            },
        };
        self.function_for_close(param, ret, abi_arity, span)
    }

    /// Add one value-parameter layer while right-folding a prepared signature.
    pub(crate) fn function_for_close(
        &self,
        param: PreparedCloseType,
        ret: PreparedCloseType,
        abi_arity: usize,
        span: Span,
    ) -> Result<PreparedCloseType, Error> {
        self.compose_for_close(
            PreparedTypeShell::Function { abi_arity, span },
            vec![param, ret],
        )
    }

    /// Rebuild one canonical structural node from children prepared under the
    /// same exact delta and compatible retained-binder scopes. This is the
    /// only route for placing a lexical `Forall` below another type node.
    pub(crate) fn compose_for_close(
        &self,
        shell: PreparedTypeShell,
        children: Vec<PreparedCloseType>,
    ) -> Result<PreparedCloseType, Error> {
        let (expected_children, span) = match &shell {
            PreparedTypeShell::Function { span, .. }
            | PreparedTypeShell::Product { span }
            | PreparedTypeShell::Sum { span } => (2, *span),
            PreparedTypeShell::Path { span, .. } | PreparedTypeShell::Goal { span, .. } => {
                (children.len(), *span)
            }
        };
        assert_eq!(
            children.len(),
            expected_children,
            "prepared structural shell received the wrong child count"
        );
        let first = children
            .first()
            .expect("a prepared structural shell has at least one child");
        let delta = first.delta;
        let owner = first.owner;
        for child in &children {
            assert_eq!(
                child.delta, delta,
                "planner composed close components prepared from different speculative deltas"
            );
            assert_eq!(
                child.owner, owner,
                "planner composed close components prepared for different inference owners"
            );
            assert_eq!(
                child.value.scope.store_nonce, self.nonce,
                "prepared structural composition crossed goal-store capabilities"
            );
            assert!(
                child.value.ty.identity_is_canonical(),
                "prepared structural composition received a non-canonical component"
            );
        }

        let mut scope = first.value.scope.clone();
        for child in &children[1..] {
            scope = scope.merged(&child.value.scope).map_err(|()| {
                Error::type_(
                    span,
                    "prepared structural node combined incompatible rigid binder scopes",
                )
            })?;
        }
        let mut child_types = children
            .into_iter()
            .map(|child| child.value.ty.clone_type());
        let ty = match shell {
            PreparedTypeShell::Function { abi_arity, span } => Type::Function {
                param: Box::new(child_types.next().expect("function parameter child")),
                ret: Box::new(child_types.next().expect("function result child")),
                meta: Meta::new(span),
                abi_arity,
                caps: (),
            },
            PreparedTypeShell::Product { span } => Type::Product {
                left: Box::new(child_types.next().expect("product left child")),
                right: Box::new(child_types.next().expect("product right child")),
                meta: Meta::new(span),
            },
            PreparedTypeShell::Sum { span } => Type::Sum {
                left: Box::new(child_types.next().expect("sum left child")),
                right: Box::new(child_types.next().expect("sum right child")),
                meta: Meta::new(span),
            },
            PreparedTypeShell::Path { segments, span } => Type::Path {
                segments,
                args: child_types.collect(),
                meta: Meta::new(span),
            },
            PreparedTypeShell::Goal { goal, span } => Type::Goal {
                goal,
                args: child_types.collect(),
                meta: Meta::new(span),
                ext: (),
            },
        };
        Ok(PreparedCloseType {
            delta,
            owner,
            value: ScopedType::from_parts(ty, scope, true),
        })
    }

    /// Validate prepared semantic outputs and opaque publication outputs in one
    /// transaction without committing either class.
    pub(crate) fn prepare_prepared_owner_with_publication(
        &self,
        delta: GoalDelta,
        outputs: Vec<(PreparedCloseType, GoalEscape)>,
        publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedGoalClose, Error> {
        for (prepared, _) in &outputs {
            assert_eq!(
                prepared.delta,
                delta.authority(),
                "planner tried to close a type prepared from a different speculative delta"
            );
            assert_eq!(
                prepared.owner, delta.owner,
                "planner tried to close a type prepared for a different inference owner"
            );
        }
        self.prepare_owner_with_publication(
            delta,
            outputs
                .into_iter()
                .map(|(prepared, escape)| (prepared.value, escape))
                .collect(),
            publication_outputs,
            ctx,
        )
    }

    fn owner_state(&self, owner: TypeGoalOwner, span: Span) -> Result<&OwnerState, Error> {
        assert!(
            owner.domain().belongs_to(self.nonce),
            "planner passed an inference-owner capability to the wrong goal store"
        );
        Ok(self
            .domains
            .get(owner.domain().index() as usize)
            .and_then(|domain| domain.owners.get(owner.index() as usize))
            .unwrap_or_else(|| {
                panic!("planner used an invalid inference-owner handle at source span {span:?}")
            }))
    }

    fn require_context(&self, ctx: &impl GoalTypeContext) {
        assert_eq!(
            self.context,
            Some(ctx.capability()),
            "goal inference was evaluated through a different package-analysis context"
        );
    }

    fn owner_state_mut(
        &mut self,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<&mut OwnerState, Error> {
        assert!(
            owner.domain().belongs_to(self.nonce),
            "planner passed a mutable inference-owner capability to the wrong goal store"
        );
        Ok(self
            .domains
            .get_mut(owner.domain().index() as usize)
            .and_then(|domain| domain.owners.get_mut(owner.index() as usize))
            .unwrap_or_else(|| {
                panic!(
                    "planner used an invalid mutable inference-owner handle at source span {span:?}"
                )
            }))
    }

    fn goal_state(&self, goal: TypeGoalRef, span: Span) -> Result<&GoalState, Error> {
        Ok(self
            .owner_state(goal.owner(), span)?
            .goals
            .get(goal.slot().index() as usize)
            .unwrap_or_else(|| {
                panic!("planner used an invalid inference-goal slot at source span {span:?}")
            }))
    }

    fn is_ancestor_or_same(
        &self,
        ancestor: TypeGoalOwner,
        mut owner: TypeGoalOwner,
        span: Span,
    ) -> Result<bool, Error> {
        if ancestor.domain() != owner.domain() {
            return Ok(false);
        }
        loop {
            if ancestor == owner {
                return Ok(true);
            }
            let Some(parent) = self.owner_state(owner, span)?.parent else {
                return Ok(false);
            };
            owner = parent;
        }
    }

    fn is_strict_ancestor(
        &self,
        ancestor: TypeGoalOwner,
        owner: TypeGoalOwner,
        span: Span,
    ) -> Result<bool, Error> {
        Ok(ancestor != owner && self.is_ancestor_or_same(ancestor, owner, span)?)
    }

    fn require_goal_in_owner_chain(
        &self,
        owner: TypeGoalOwner,
        goal: TypeGoalRef,
        span: Span,
    ) -> Result<(), Error> {
        self.goal_state(goal, span)?;
        assert!(
            self.is_ancestor_or_same(goal.owner(), owner, span)?,
            "planner passed a peer, descendant, or foreign inference goal into an owner"
        );
        Ok(())
    }

    fn require_goal_usable_in_delta(
        &self,
        delta: &GoalDelta,
        goal: TypeGoalRef,
        span: Span,
    ) -> Result<(), Error> {
        self.require_goal_in_owner_chain(delta.owner, goal, span)?;
        assert_eq!(
            self.goal_state(goal, span)?.close_requirement,
            GoalCloseRequirement::Required,
            "an inactive reserved inference goal entered solver state"
        );
        Ok(())
    }

    fn require_type_goals_usable_in_delta(
        &self,
        delta: &GoalDelta,
        ty: &ScopedType,
        span: Span,
    ) -> Result<(), Error> {
        for goal in goal_refs(ty.ty.as_type()) {
            self.require_goal_usable_in_delta(delta, goal, span)?;
        }
        Ok(())
    }

    fn require_scoped_goal_chain(
        &self,
        owner: TypeGoalOwner,
        scoped: &ScopedType,
        span: Span,
    ) -> Result<(), Error> {
        for goal in goal_refs(scoped.ty.as_type()) {
            self.require_goal_in_owner_chain(owner, goal, span)?;
            let goal_scope = &self.owner_state(goal.owner(), span)?.scope;
            assert!(
                scoped.scope.extends(goal_scope),
                "inference-goal occurrence omitted or conflicted with its owner's rigid evidence"
            );
        }
        Ok(())
    }

    /// Add one equation atomically. The equation records only the map entries
    /// it changes; failure restores those entries in `O(changes)` time rather
    /// than cloning the accumulated delta for every constraint.
    pub(crate) fn constrain(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ScopedType,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.constrain_equation(
            delta,
            found,
            ExpectedEquationOperand::solver(expected),
            span,
            ctx,
        )
    }

    /// Add one finite relation group as a single solver transaction. A
    /// mismatch in any member restores every solution and policy meet written
    /// by the group while preserving the delta prefix that preceded it.
    pub(crate) fn constrain_equations_atomically<I>(
        &self,
        delta: &mut GoalDelta,
        equations: I,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error>
    where
        I: IntoIterator<Item = (ScopedType, ScopedType, Span)>,
    {
        let mut equations = equations.into_iter().peekable();
        let Some((_, _, span)) = equations.peek() else {
            return Ok(());
        };
        self.require_context(ctx);
        self.require_delta_owner(delta, *span)?;
        delta.begin_equation();
        let result = equations.try_for_each(|(found, expected, span)| {
            self.constrain_in_open_equation(
                delta,
                found,
                ExpectedEquationOperand::solver(expected),
                span,
                ctx,
            )
        });
        delta.finish_equation(result.is_ok());
        result
    }

    /// Add one equation whose expected operand may retain an exact written
    /// view solely for this mismatch diagnostic. The by-value expected
    /// operand cannot escape into solver, retry, progress, or publication
    /// state.
    pub(crate) fn constrain_equation(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ExpectedEquationOperand,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.constrain_with_close_readiness(delta, found, expected, span, ctx, false)
            .map(|applied| {
                assert!(
                    applied,
                    "an ordinary goal equation was unexpectedly treated as deferred"
                );
            })
    }

    fn constrain_isolated_specialization_equation(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ScopedType,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<bool, Error> {
        self.require_context(ctx);
        self.require_delta_owner(delta, span)?;
        delta.begin_equation();
        let result = self.constrain_in_open_equation(
            delta,
            found,
            ExpectedEquationOperand::solver(expected),
            span,
            ctx,
        );
        let failure = result.as_ref().err().and_then(|_| delta.relation_failure());
        delta.finish_equation(result.is_ok());
        match (result, failure) {
            (Ok(()), None) => Ok(true),
            (Err(_), Some(RelationFailureKind::Incompatible)) => Ok(false),
            (Err(error), None) => Err(error),
            (Ok(()), Some(_)) => {
                unreachable!("a successful goal relation retained a failure classification")
            }
        }
    }

    /// Add one equation only when the owning delta can close without an
    /// ancestor solution retaining one of this owner's still-open goals.
    ///
    /// A retained application can know its result equation before its value
    /// children have solved all result goals.  Binding an ancestor goal to
    /// that unfinished structure would be valid transient solver work, but it
    /// cannot cross the child-owner boundary.  This operation performs the
    /// normal equation transaction and rolls it back when that exact close
    /// condition is not ready yet.  The caller may retry the same equation
    /// after its retained children close; no partial write or policy meet is
    /// left behind.
    pub(crate) fn try_constrain_gather_equation<E>(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: E,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<CloseReadyEquationConstraint, Error>
    where
        E: Into<ExpectedEquationOperand>,
    {
        self.try_constrain_gather_equations(delta, [(found, expected.into(), span)], ctx)
    }

    /// Add the final equality of one already-prepared retained leaf only when
    /// that leaf can close without publishing a descendant goal through an
    /// ancestor solution. A deferred attempt rolls back exactly this
    /// equation, leaving the leaf's existing delta reusable after its parent
    /// domain makes causal progress.
    pub(crate) fn try_constrain_retained_leaf_equation<E>(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: E,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<CloseReadyEquationConstraint, Error>
    where
        E: Into<ExpectedEquationOperand>,
    {
        let owner = self.owner_state(delta.owner, span)?;
        assert_eq!(
            owner.kind,
            GoalOwnerKind::RetainedValue,
            "a prepared-leaf close-readiness equation used another owner kind"
        );
        // An annotation can reserve goals before the leaf's delta begins;
        // closing must account for those goals even when synthesis adds none.
        self.constrain_with_close_readiness(delta, found, expected.into(), span, ctx, true)
            .map(|close_ready| {
                if close_ready {
                    CloseReadyEquationConstraint::Applied
                } else {
                    CloseReadyEquationConstraint::Deferred
                }
            })
    }

    /// Atomically add a candidate application-layer equation bundle.
    ///
    /// Prefix selection may need several independently synthesized source
    /// values to match one folded function domain.  All equations belong to
    /// one candidate boundary: a mismatch or unsafe close rolls every write
    /// and policy meet back together, while each equation retains its own
    /// source span for diagnostics.
    pub(crate) fn try_constrain_gather_equations<I, E>(
        &self,
        delta: &mut GoalDelta,
        equations: I,
        ctx: &impl GoalTypeContext,
    ) -> Result<CloseReadyEquationConstraint, Error>
    where
        I: IntoIterator<Item = (ScopedType, E, Span)>,
        E: Into<ExpectedEquationOperand>,
    {
        let equations = equations
            .into_iter()
            .map(|(found, expected, span)| (found, expected.into(), span))
            .collect::<Vec<_>>();
        let span = equations
            .first()
            .map_or(Span::new(0, 0), |(_, _, span)| *span);
        let owner = self.owner_state(delta.owner, span)?;
        assert_eq!(
            owner.kind,
            GoalOwnerKind::ApplicationGather,
            "close-readiness equations are admitted only in application gathers"
        );
        assert!(
            owner.goals.is_empty(),
            "an application gather must not own inference goals"
        );
        self.require_context(ctx);
        self.require_delta_owner(delta, span)?;
        delta.begin_equation();
        let result = equations
            .into_iter()
            .try_for_each(|(found, expected, span)| {
                self.constrain_in_open_equation(delta, found, expected, span, ctx)
            });
        let close_readiness = if result.is_ok() {
            self.delta_writes_are_close_ready(delta)
        } else {
            Ok(true)
        };
        let equation_succeeded = result.is_ok() && matches!(close_readiness, Ok(true));
        delta.finish_equation(equation_succeeded);
        result?;
        close_readiness.map(|close_ready| {
            if close_ready {
                CloseReadyEquationConstraint::Applied
            } else {
                CloseReadyEquationConstraint::Deferred
            }
        })
    }

    fn constrain_with_close_readiness(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ExpectedEquationOperand,
        span: Span,
        ctx: &impl GoalTypeContext,
        require_close_ready: bool,
    ) -> Result<bool, Error> {
        self.require_context(ctx);
        self.require_delta_owner(delta, span)?;
        delta.begin_equation();
        let result = self.constrain_in_open_equation(delta, found, expected, span, ctx);
        let close_readiness = if result.is_ok() && require_close_ready {
            self.delta_writes_are_close_ready(delta)
        } else {
            Ok(true)
        };
        let equation_succeeded = result.is_ok() && matches!(close_readiness, Ok(true));
        delta.finish_equation(equation_succeeded);
        result?;
        close_readiness
    }

    fn constrain_in_open_equation(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ExpectedEquationOperand,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        #[cfg(test)]
        record_equation_attempt();
        #[cfg(test)]
        let found = found.stamped_for_test(self.nonce);
        #[cfg(test)]
        let expected = expected.stamped_for_test(self.nonce);
        let (expected, expected_diagnostic) = expected.into_parts();
        assert!(
            found.scope.store_nonce == self.nonce,
            "goal equation carried found-side rigid evidence from another store"
        );
        assert!(
            expected.scope.store_nonce == self.nonce,
            "goal equation carried expected-side rigid evidence from another store"
        );
        self.require_scoped_goal_chain(delta.owner, &found, span)?;
        self.require_scoped_goal_chain(delta.owner, &expected, span)?;
        self.require_type_goals_usable_in_delta(delta, &found, span)?;
        self.require_type_goals_usable_in_delta(delta, &expected, span)?;
        if let Some(diagnostic) = &expected_diagnostic {
            for goal in diagnostic_goal_refs(diagnostic.ty.as_type()) {
                self.require_goal_in_owner_chain(delta.owner, goal, span)?;
                let goal_scope = &self.owner_state(goal.owner(), span)?.scope;
                assert!(
                    diagnostic.scope.extends(goal_scope),
                    "diagnostic inference-goal occurrence omitted or conflicted with its owner's rigid evidence"
                );
                self.require_goal_usable_in_delta(delta, goal, span)?;
            }
        }
        // A raw equation side is one source-context tree. Canonicalize the
        // complete skeleton before following any goal solution so an
        // identity-exact solution is never inserted beneath a
        // source-spelling parent and stripped of its provenance bit.
        let found = self.canonical_scoped(found, ctx);
        let expected = self.canonical_scoped(expected, ctx);
        if type_contains_goal(found.ty.as_type()) || type_contains_goal(expected.ty.as_type()) {
            let found_kind = self.kind_of(&found, delta, ctx, &mut BTreeSet::new())?;
            let expected_kind = self.kind_of(&expected, delta, ctx, &mut BTreeSet::new())?;
            if found_kind != expected_kind {
                delta.mark_relation_incompatible();
                return Err(Error::type_(
                    span,
                    format!(
                        "inference equation relates kind `{found_kind}` to kind `{expected_kind}`"
                    ),
                ));
            }
        }
        let equation = EquationProvenance {
            found: ScopedTypeView::from_scoped(&found),
            expected: ScopedTypeView::from_scoped(
                expected_diagnostic.as_ref().unwrap_or(&expected),
            ),
        };
        self.constrain_views(
            delta,
            ScopedTypeView::from_scoped(&found),
            ScopedTypeView::from_scoped(&expected),
            equation,
            span,
            ctx,
        )
    }

    fn delta_writes_are_close_ready(&self, delta: &GoalDelta) -> Result<bool, Error> {
        for (goal, binding) in &delta.writes {
            let value = self.zonk_with_delta(&binding.value, delta)?;
            for remaining in goal_refs(value.ty.as_type()) {
                self.goal_state(remaining, binding.span)?;
                if !self.is_ancestor_or_same(remaining.owner(), goal.owner(), binding.span)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn delta_bindings_transport_ready_for_test(&self, delta: &GoalDelta) -> bool {
        self.delta_writes_are_close_ready(delta)
            .expect("test inspected a live delta from this goal store")
    }

    /// Validate and normalize one owner and its opaque publication metadata
    /// without making any mutation visible.
    pub(crate) fn prepare_owner_with_publication(
        &self,
        delta: GoalDelta,
        outputs: Vec<(ScopedType, GoalEscape)>,
        publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedGoalClose, Error> {
        self.prepare_owner_with_publication_snapshot(&delta, outputs, publication_outputs, ctx)
    }

    /// Validate the goal-free semantic output that one still-open retained
    /// premise will eventually close into its exact direct parent. This is an
    /// inert preview: the live delta, owner lifecycle, and committed bindings
    /// remain unchanged so the ordinary driver can perform the physical close
    /// when it unwinds to that parent.
    pub(crate) fn preview_goal_free_close_output(
        &self,
        delta: &GoalDelta,
        output: ScopedType,
        destination: TypeGoalOwner,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        let span = output.ty.span();
        assert_eq!(
            self.owner_state(delta.owner, span)?.parent,
            Some(destination),
            "goal-free close preview targeted a non-parent owner"
        );
        let prepared = self.prepare_owner_with_publication_snapshot(
            delta,
            vec![(output, GoalEscape::ToOwner(destination))],
            Vec::new(),
            ctx,
        )?;
        let (_commit, outputs, publication_outputs) = prepared.into_parts();
        assert!(
            publication_outputs.outputs.is_empty(),
            "a semantic close preview produced publication outputs"
        );
        let mut outputs = outputs.into_outputs();
        assert_eq!(
            outputs.len(),
            1,
            "a single close preview returned another semantic output count"
        );
        let ClosedGoalOutput::Retained(output) = outputs
            .pop()
            .expect("a single close preview returned no semantic output")
        else {
            unreachable!("a goal-free retained preview changed semantic output class")
        };
        assert_eq!(
            output.destination(),
            destination,
            "a close preview retained into another destination"
        );
        let output = output.into_scoped_type();
        if let Some(remaining) = goal_refs(output.ty.as_type()).into_iter().next() {
            return Err(self
                .goal_state(remaining, output.ty.span())?
                .origin
                .unresolved_error());
        }
        self.rebase_goal_free_type_to_owner(output, destination, span, ctx)
    }

    /// Reattach one already-zonked goal-free type to the exact lexical owner
    /// that will consume it. Canonicalization runs while the source scope is
    /// still attached, so source-spelled nominal identities are resolved
    /// before exact free-rigid proof compatibility is checked against the
    /// destination scope.
    pub(crate) fn rebase_goal_free_type_to_owner(
        &self,
        value: ScopedType,
        destination: TypeGoalOwner,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<ScopedType, Error> {
        assert_eq!(
            value.scope.store_nonce, self.nonce,
            "goal-free type rebase carried rigid evidence from another goal store"
        );
        self.require_context(ctx);
        let mut value = self.canonical_scoped(value, ctx);
        assert!(
            value.ty.identity_is_canonical(),
            "goal-free type rebase did not close its nominal identity frontier"
        );
        assert!(
            !super::type_contains_infer(value.ty.as_type()),
            "goal-free type rebase received an unresolved source placeholder"
        );
        if let Some(remaining) = goal_refs(value.ty.as_type()).into_iter().next() {
            return Err(self.goal_state(remaining, span)?.origin.unresolved_error());
        }
        self.require_escape(&value, GoalEscape::ToOwner(destination))?;
        value.scope = self.owner_state(destination, span)?.scope.clone();
        Ok(value)
    }

    fn prepare_owner_with_publication_snapshot(
        &self,
        delta: &GoalDelta,
        outputs: Vec<(ScopedType, GoalEscape)>,
        publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
        ctx: &impl GoalTypeContext,
    ) -> Result<PreparedGoalClose, Error> {
        self.require_context(ctx);
        #[cfg(test)]
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(output, escape)| (output.stamped_for_test(self.nonce), escape))
            .collect();
        #[cfg(test)]
        let publication_outputs: Vec<_> = publication_outputs
            .into_iter()
            .map(|(output, escape)| (output.stamped_for_test(self.nonce), escape))
            .collect();
        for (output, _) in &outputs {
            assert!(
                output.scope.store_nonce == self.nonce,
                "goal close carried rigid evidence from another store"
            );
        }
        for (output, _) in &publication_outputs {
            assert!(
                output.store_nonce() == self.nonce,
                "publication close carried rigid evidence from another store"
            );
        }
        self.require_delta_owner(delta, Span::new(0, 0))?;
        assert_eq!(
            self.owner_state(delta.owner, Span::new(0, 0))?
                .open_children,
            0,
            "planner tried to close an inference owner before its nested owners"
        );
        let goal_states = self
            .owner_state(delta.owner, Span::new(0, 0))?
            .goals
            .iter()
            .map(|goal| (goal.origin.clone(), goal.close_requirement))
            .collect::<Vec<_>>();
        let originals = delta.writes.clone();
        let mut original_policy_meets = BTreeMap::new();
        let producer = self
            .owner_state(delta.owner, Span::new(0, 0))?
            .producer_view
            .filter(|&id| self.producer_view(id).boundary == delta.owner);
        if let Some(id) = producer {
            original_policy_meets.extend(self.producer_view(id).policy_meets.clone());
        }
        for (&goal, meet) in &delta.policy_meets {
            original_policy_meets
                .entry(goal)
                .and_modify(|previous| previous.policy = previous.policy.meet(meet.policy))
                .or_insert(*meet);
        }
        let mut replay = GoalDelta {
            token: delta.token,
            owner: delta.owner,
            owner_goal_count: delta.owner_goal_count,
            writes: BTreeMap::new(),
            policy_meets: BTreeMap::new(),
            equation_undo: None,
            read_view: if producer.is_some() {
                GoalReadView::EnclosingProducer
            } else {
                delta.read_view
            },
        };

        for (goal, meet) in original_policy_meets {
            self.require_goal_usable_in_delta(&replay, goal, meet.span)?;
            let policy = self
                .effective_solution_policy(goal, Some(&replay), meet.span)?
                .meet(meet.policy);
            replay.policy_meets.insert(
                goal,
                GoalPolicyMeet {
                    policy,
                    span: meet.span,
                },
            );
            if let Some(solution) = self.binding(goal, Some(&replay)) {
                let solution = self.zonk_with_delta(&solution, &replay)?;
                self.require_solution_policy(goal, &solution, Some(&replay), meet.span)?;
            }
        }

        // Replaying from an empty delta proves that no stale intermediate
        // mutation was required for validation. BTree order is only a stable
        // traversal order; semantic orientation is decided by ancestry.
        for (&goal, binding) in producer
            .into_iter()
            .flat_map(|id| &self.producer_view(id).writes)
            .chain(&originals)
        {
            self.bind_goal(
                &mut replay,
                goal,
                binding.value.clone(),
                None,
                binding.span,
                ctx,
            )?;
        }

        for slot in 0..goal_states.len() {
            let slot = u32::try_from(slot).expect("type-inference goal count exceeded u32");
            let goal = TypeGoalRef::new(delta.owner, TypeGoalSlot::from_index(slot));
            let (origin, close_requirement) = &goal_states[slot as usize];
            if *close_requirement == GoalCloseRequirement::ActivateOnUse {
                assert!(
                    self.goal_state(goal, origin.span)?.solution.is_none(),
                    "an inactive reserved inference goal already had a committed solution"
                );
                continue;
            }
            let open = self.scoped_goal(goal, Vec::new(), origin.span)?;
            let zonked = self.zonk_with_delta(&open, &replay)?;
            self.require_solution_policy(goal, &zonked, Some(&replay), origin.span)?;
            let refs = goal_refs(zonked.ty.as_type());
            if refs
                .iter()
                .any(|remaining| remaining.owner() == delta.owner)
            {
                return Err(origin.unresolved_error());
            }
            for remaining in refs {
                assert!(
                    self.is_strict_ancestor(remaining.owner(), delta.owner, zonked.ty.span(),)?,
                    "goal-store close retained a non-ancestor goal"
                );
            }
        }

        // A nested owner's delta may temporarily express an ancestor result in
        // terms of child-owned goals. Resolve every such dependency before any
        // ancestor write is committed, then narrow its proof scope back to the
        // target owner's scope. This makes equation order irrelevant without
        // ever publishing a descendant capability through an ancestor.
        let mut normalized_writes = BTreeMap::new();
        for (goal, binding) in &replay.writes {
            let mut value = self.zonk_with_delta(&binding.value, &replay)?;
            for remaining in goal_refs(value.ty.as_type()) {
                assert!(
                    self.is_ancestor_or_same(remaining.owner(), goal.owner(), binding.span)?,
                    "goal-store close could not eliminate a descendant goal from an ancestor solution"
                );
            }
            let target_scope = &self.owner_state(goal.owner(), binding.span)?.scope;
            let mut free = HashSet::new();
            collect_free_type_vars(value.ty.as_type(), &mut free);
            if free.iter().any(|name| {
                value.scope.binding(name).is_some()
                    && target_scope.binding(name) != value.scope.binding(name)
            }) {
                return Err(Error::type_(
                    binding.span,
                    "an inferred type would let a nested rigid type variable escape its scope",
                ));
            }
            value.scope = target_scope.clone();
            self.require_solution_policy(*goal, &value, Some(&replay), binding.span)?;
            let expected_kind = self.goal_state(*goal, binding.span)?.kind.clone();
            let actual_kind = self.kind_of(&value, &replay, ctx, &mut BTreeSet::new())?;
            if actual_kind != expected_kind {
                return Err(Error::type_(
                    binding.span,
                    format!(
                        "inferred type has kind `{actual_kind}`, but this position requires kind `{expected_kind}`"
                    ),
                ));
            }
            normalized_writes.insert(
                *goal,
                GoalBinding {
                    value,
                    span: binding.span,
                },
            );
        }
        replay.writes = normalized_writes;

        // Recheck every published target after following the complete replay.
        // A monotype goal may have been unioned with a permissive goal before
        // that representative was solved, so checking only the immediate
        // candidate in `bind_goal` is not sufficient.
        let mut published_goals = replay.writes.keys().copied().collect::<BTreeSet<_>>();
        published_goals.extend(replay.policy_meets.keys().copied());
        for goal in published_goals {
            let origin = self.goal_state(goal, Span::new(0, 0))?.origin.clone();
            let open = self.scoped_goal(goal, Vec::new(), origin.span)?;
            let zonked = self.zonk_with_delta(&open, &replay)?;
            self.require_solution_policy(goal, &zonked, Some(&replay), origin.span)?;
        }

        let mut zonked_outputs = Vec::with_capacity(outputs.len());
        for (output, escape) in outputs {
            self.require_scoped_goal_chain(delta.owner, &output, output.ty.span())?;
            self.require_type_goals_usable_in_delta(&replay, &output, output.ty.span())?;
            let destination = match escape {
                GoalEscape::ToOwner(destination)
                | GoalEscape::ToOwnerFunctionScheme(destination) => {
                    assert!(
                        self.is_strict_ancestor(destination, delta.owner, output.ty.span(),)?,
                        "planner targeted a retained inference output at a non-ancestor owner"
                    );
                    destination
                }
                GoalEscape::ClosedAt(destination)
                | GoalEscape::ClosedFunctionSchemeAt(destination) => destination,
            };
            self.owner_state(destination, output.ty.span())?;
            let output = self.canonical_scoped(output, ctx);
            let zonked = self.zonk_with_delta(&output, &replay)?;
            let function_scheme = matches!(
                escape,
                GoalEscape::ToOwnerFunctionScheme(_) | GoalEscape::ClosedFunctionSchemeAt(_)
            );
            if function_scheme {
                assert!(
                    is_function_scheme(zonked.ty.as_type()),
                    "planner used the whole-function-scheme escape for a non-function type"
                );
            }
            self.kind_of(&zonked, &replay, ctx, &mut BTreeSet::new())?;
            self.require_escape(&zonked, escape)?;
            zonked_outputs.push(match escape {
                GoalEscape::ToOwner(destination) => {
                    ClosedGoalOutput::Retained(RetainedGoalOutput {
                        destination,
                        value: zonked,
                    })
                }
                GoalEscape::ToOwnerFunctionScheme(destination) => {
                    ClosedGoalOutput::RetainedFunctionScheme(RetainedFunctionSchemeOutput {
                        destination,
                        value: zonked,
                    })
                }
                GoalEscape::ClosedAt(destination) => {
                    assert!(
                        goal_refs(zonked.ty.as_type()).is_empty(),
                        "goal-free close output retained an inference goal"
                    );
                    ClosedGoalOutput::GoalFree(GoalFreeOutput {
                        destination,
                        value: zonked,
                    })
                }
                GoalEscape::ClosedFunctionSchemeAt(_destination) => {
                    assert!(
                        goal_refs(zonked.ty.as_type()).is_empty(),
                        "goal-free function-scheme output retained an inference goal"
                    );
                    ClosedGoalOutput::FunctionScheme(GoalFreeFunctionSchemeOutput { value: zonked })
                }
            });
        }

        let mut zonked_publication_outputs = Vec::with_capacity(publication_outputs.len());
        for (output, escape) in publication_outputs {
            let owner_scope = &self.owner_state(delta.owner, Span::new(0, 0))?.scope;
            let output = output.into_state_for_owner(delta.owner, owner_scope);
            let closed = match output {
                RetainedPublicationState::ValidationRequired(output) => {
                    #[cfg(test)]
                    record_full_publication_validation();
                    self.require_scoped_goal_chain(delta.owner, &output, output.ty.span())?;
                    self.require_type_goals_usable_in_delta(&replay, &output, output.ty.span())?;
                    self.publication_destination(delta.owner, output.ty.span(), escape)?;
                    let output = self.canonical_scoped(output, ctx);
                    let zonked = self.zonk_with_delta(&output, &replay)?;
                    let root_kind = PublicationRootValidationKind::from_escape(escape);
                    if root_kind == PublicationRootValidationKind::FunctionScheme {
                        assert!(
                            is_function_scheme(zonked.ty.as_type()),
                            "planner used the whole-function-scheme publication escape for a non-function type"
                        );
                    }
                    self.kind_of(&zonked, &replay, ctx, &mut BTreeSet::new())?;
                    let goal_free = self.require_publication_escape(&zonked, escape)?;
                    match escape {
                        PublicationGoalEscape::ToOwner(destination)
                        | PublicationGoalEscape::ToOwnerFunctionScheme(destination) => {
                            let state = if goal_free && zonked.ty.identity_is_canonical() {
                                RetainedPublicationState::GoalFreeValidated(
                                    GoalFreeValidatedPublication {
                                        value: zonked,
                                        root_kind,
                                    },
                                )
                            } else {
                                RetainedPublicationState::ValidationRequired(zonked)
                            };
                            ClosedPublicationOutput::Retained(RetainedPublicationOutput {
                                destination,
                                state,
                            })
                        }
                        PublicationGoalEscape::ClosedAt(destination)
                        | PublicationGoalEscape::ClosedFunctionSchemeAt(destination) => {
                            assert!(
                                goal_free,
                                "goal-free publication output retained an inference goal"
                            );
                            ClosedPublicationOutput::GoalFree(GoalFreePublicationOutput {
                                destination,
                                value: zonked,
                            })
                        }
                    }
                }
                RetainedPublicationState::GoalFreeValidated(validated) => {
                    let output = validated.value;
                    let destination =
                        self.publication_destination(delta.owner, output.ty.span(), escape)?;
                    assert_eq!(
                        validated.root_kind,
                        PublicationRootValidationKind::from_escape(escape),
                        "validated publication output changed root kind"
                    );
                    assert!(
                        output.ty.identity_is_canonical(),
                        "validated publication output lost its canonical identity proof"
                    );
                    let destination_scope = &self.owner_state(destination, output.ty.span())?.scope;
                    assert!(
                        output.scope.extends(destination_scope),
                        "validated publication output did not preserve its destination scope"
                    );
                    #[cfg(test)]
                    record_goal_free_retained_publication();
                    match escape {
                        PublicationGoalEscape::ToOwner(destination)
                        | PublicationGoalEscape::ToOwnerFunctionScheme(destination) => {
                            ClosedPublicationOutput::Retained(RetainedPublicationOutput {
                                destination,
                                state: RetainedPublicationState::GoalFreeValidated(
                                    GoalFreeValidatedPublication {
                                        value: output,
                                        root_kind: validated.root_kind,
                                    },
                                ),
                            })
                        }
                        PublicationGoalEscape::ClosedAt(destination)
                        | PublicationGoalEscape::ClosedFunctionSchemeAt(destination) => {
                            ClosedPublicationOutput::GoalFree(GoalFreePublicationOutput {
                                destination,
                                value: output,
                            })
                        }
                    }
                }
            };
            zonked_publication_outputs.push(closed);
        }

        // Preflight the exact mutable cells before installing any write.
        self.require_delta_owner(delta, Span::new(0, 0))?;
        let parent = self.owner_state(delta.owner, Span::new(0, 0))?.parent;
        if let Some(parent) = parent {
            assert!(
                self.owner_state(parent, Span::new(0, 0))?.open_children > 0,
                "open inference child was not counted by its parent"
            );
        }
        for goal in replay.writes.keys() {
            assert!(
                goal.domain().belongs_to(self.nonce),
                "goal-store commit received a goal capability from another store"
            );
            let state = self
                .domains
                .get(goal.domain().index() as usize)
                .and_then(|domain| domain.owners.get(goal.owner().index() as usize))
                .and_then(|owner| owner.goals.get(goal.slot().index() as usize))
                .expect("validated inference goal disappeared before commit");
            assert!(
                state.solution.is_none(),
                "validated inference delta was committed over an existing solution"
            );
        }
        for goal in replay.policy_meets.keys() {
            self.goal_state(*goal, Span::new(0, 0))?;
        }
        Ok(PreparedGoalClose {
            commit: PreparedGoalCommit {
                store_nonce: self.nonce,
                store_revision: self.revision,
                delta: replay,
                parent,
            },
            outputs: ClosedGoalOutputs {
                owner: delta.owner,
                outputs: zonked_outputs,
            },
            publication_outputs: ClosedPublicationOutputs {
                owner: delta.owner,
                outputs: zonked_publication_outputs,
            },
        })
    }

    /// Revalidate the exact store snapshot named by a prepared close.
    ///
    /// Preparation is inert, so another owner/goal mutation may occur before
    /// the capability is consumed. Every check happens before the first write.
    pub(crate) fn validate_prepared_owner(&self, commit: &PreparedGoalCommit) {
        assert_eq!(
            commit.store_nonce, self.nonce,
            "prepared inference close belongs to another goal store"
        );
        assert_eq!(
            commit.store_revision, self.revision,
            "prepared inference close became stale before commit"
        );
        self.require_delta_owner(&commit.delta, Span::new(0, 0))
            .expect("prepared inference close no longer names a live owner");
        let current_parent = self
            .owner_state(commit.delta.owner, Span::new(0, 0))
            .expect("prepared inference owner disappeared before commit")
            .parent;
        assert_eq!(
            current_parent, commit.parent,
            "prepared inference owner's parent changed before commit"
        );
        if let Some(parent) = commit.parent {
            assert!(
                self.owner_state(parent, Span::new(0, 0))
                    .expect("prepared parent inference owner disappeared before commit")
                    .open_children
                    > 0,
                "prepared inference child was no longer open at commit"
            );
        }
        for goal in commit.delta.writes.keys() {
            self.write_producer_view(commit.delta.owner, *goal);
            let state = self
                .goal_state(*goal, Span::new(0, 0))
                .expect("prepared inference goal disappeared before commit");
            assert!(
                state.solution.is_none(),
                "prepared inference close would overwrite a committed solution"
            );
        }
        for goal in commit.delta.policy_meets.keys() {
            self.write_producer_view(commit.delta.owner, *goal);
            self.goal_state(*goal, Span::new(0, 0))
                .expect("prepared inference policy target disappeared before commit");
        }
    }

    /// Install a previously validated owner close. Every lookup and mutable
    /// cell is revalidated before the first write, after which this method
    /// contains no user-code callback or fallible planning operation.
    pub(crate) fn commit_prepared_owner(&mut self, commit: PreparedGoalCommit) {
        self.validate_prepared_owner(&commit);
        #[cfg(test)]
        let committed_goals = self
            .owner_state(commit.delta.owner, Span::new(0, 0))
            .expect("preflighted inference owner disappeared before work accounting")
            .goals
            .len();
        #[cfg(test)]
        let committed_root = commit.parent.is_none();
        let next_revision = self.next_revision();
        let PreparedGoalCommit {
            store_nonce: _,
            store_revision: _,
            delta,
            parent,
        } = commit;
        for (goal, binding) in delta.writes {
            if let Some(id) = self.write_producer_view(delta.owner, goal) {
                self.producer_view_mut(id).writes.insert(goal, binding);
                continue;
            }
            self.domains
                .get_mut(goal.domain().index() as usize)
                .and_then(|domain| domain.owners.get_mut(goal.owner().index() as usize))
                .and_then(|owner| owner.goals.get_mut(goal.slot().index() as usize))
                .expect("preflighted inference goal disappeared before commit")
                .solution = Some(binding.value);
        }
        for (goal, meet) in delta.policy_meets {
            if let Some(id) = self.write_producer_view(delta.owner, goal) {
                self.producer_view_mut(id)
                    .policy_meets
                    .entry(goal)
                    .and_modify(|previous| previous.policy = previous.policy.meet(meet.policy))
                    .or_insert(meet);
                continue;
            }
            let state = self
                .domains
                .get_mut(goal.domain().index() as usize)
                .and_then(|domain| domain.owners.get_mut(goal.owner().index() as usize))
                .and_then(|owner| owner.goals.get_mut(goal.slot().index() as usize))
                .expect("preflighted inference goal disappeared before policy commit");
            state.solution_policy = state.solution_policy.meet(meet.policy);
        }
        self.domains
            .get_mut(delta.owner.domain().index() as usize)
            .and_then(|domain| domain.owners.get_mut(delta.owner.index() as usize))
            .expect("preflighted inference owner disappeared before close")
            .lifecycle = OwnerLifecycle::Closed;
        if let Some(parent) = parent {
            let parent = self
                .domains
                .get_mut(parent.domain().index() as usize)
                .and_then(|domain| domain.owners.get_mut(parent.index() as usize))
                .expect("preflighted parent inference owner disappeared before close");
            parent.open_children -= 1;
        }
        if let Some(id) = self
            .owner_state(delta.owner, Span::new(0, 0))
            .expect("closed producer owner disappeared")
            .producer_view
            .filter(|&id| self.producer_view(id).boundary == delta.owner)
        {
            let view = self.producer_view_mut(id);
            view.writes.clear();
            view.policy_meets.clear();
            view.context_writes.clear();
            view.context_policies.clear();
        }
        self.revision = next_revision;
        #[cfg(test)]
        record_owner_close(committed_root, committed_goals);
    }

    fn require_delta_owner(&self, delta: &GoalDelta, span: Span) -> Result<(), Error> {
        let owner = self.owner_state(delta.owner, span)?;
        assert_eq!(
            owner.lifecycle,
            OwnerLifecycle::Open,
            "planner reused an inference delta after its owner closed"
        );
        assert_eq!(
            owner.goals.len(),
            delta.owner_goal_count,
            "planner used a stale inference delta after allocating another goal"
        );
        Ok(())
    }

    /// Rebuild one equation side for diagnostics only. Currently available
    /// solved progress (including writes from the live equation journal) is
    /// zonked first; source binder names replace the remaining open inferred
    /// type-argument goals. The solver never consumes this tree, so successful
    /// equations retain only the borrowed views in [`EquationProvenance`].
    fn materialize_diagnostic_view(
        &self,
        scoped: ScopedTypeView<'_>,
        delta: &GoalDelta,
    ) -> ScopedType {
        use crate::pass::visit_mut::{TypecheckVisitMut, walk_type};

        struct DiagnosticGoalNames<'a> {
            store: &'a GoalStore,
        }

        impl TypecheckVisitMut<Lowered> for DiagnosticGoalNames<'_> {
            fn visit_type(&mut self, ty: &mut Type<Lowered>) {
                walk_type(self, ty);
                let source = match ty {
                    Type::Goal { goal, meta, .. } => {
                        let origin = &self
                            .store
                            .goal_state(*goal, meta.span)
                            .expect("diagnostic equation referenced an unknown inference goal")
                            .origin;
                        if origin.role == GoalRole::TypeArgument {
                            origin
                                .source_name
                                .as_ref()
                                .map(|name| (name.clone(), meta.span))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((name, span)) = source {
                    let Type::Goal { args, .. } = ty else {
                        unreachable!("diagnostic goal replacement lost its goal node")
                    };
                    *ty = Type::synth_path(vec![name], std::mem::take(args), span);
                }
            }
        }

        let zonked = self
            .zonk_raw(
                scoped.ty,
                scoped.scope,
                scoped.identity_canonical,
                Some(delta),
                &mut BTreeSet::new(),
                true,
            )
            .expect("diagnostic equation could not zonk a validated solver state");
        let mut ty = zonked.ty;
        DiagnosticGoalNames { store: self }.visit_type(&mut ty);
        ScopedType::new(
            InternedType::fresh_with_identity(ty, zonked.identity_canonical)
                .with_requirement_source(scoped.requirement_source.cloned()),
            zonked.scope,
        )
    }

    fn constrain_inner(
        &self,
        delta: &mut GoalDelta,
        found: ScopedType,
        expected: ScopedType,
        equation: EquationProvenance<'_>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        let found = self.canonical_scoped(found, ctx);
        let expected = self.canonical_scoped(expected, ctx);
        self.constrain_views(
            delta,
            ScopedTypeView::from_scoped(&found),
            ScopedTypeView::from_scoped(&expected),
            equation,
            span,
            ctx,
        )
    }

    fn constrain_views(
        &self,
        delta: &mut GoalDelta,
        found: ScopedTypeView<'_>,
        expected: ScopedTypeView<'_>,
        equation: EquationProvenance<'_>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        if let Type::Goal { goal, .. } = found.ty
            && self.binding(*goal, Some(delta)).is_some()
        {
            let resolved = self
                .zonk_raw(
                    found.ty,
                    found.scope,
                    found.identity_canonical,
                    Some(delta),
                    &mut BTreeSet::new(),
                    false,
                )?
                .into_scoped();
            return self.constrain_views(
                delta,
                ScopedTypeView::from_scoped(&resolved),
                expected,
                equation,
                span,
                ctx,
            );
        }
        if let Type::Goal { goal, .. } = expected.ty
            && self.binding(*goal, Some(delta)).is_some()
        {
            let resolved = self
                .zonk_raw(
                    expected.ty,
                    expected.scope,
                    expected.identity_canonical,
                    Some(delta),
                    &mut BTreeSet::new(),
                    false,
                )?
                .into_scoped();
            return self.constrain_views(
                delta,
                found,
                ScopedTypeView::from_scoped(&resolved),
                equation,
                span,
                ctx,
            );
        }

        // A canonical composite can still contain a canonical transparent
        // alias head.  The identity bit says that the head names its exact
        // declaration; it does not mean that the alias has been unfolded.
        // Probe each path frontier as the structural walk reaches it so
        // aliases nested inside products, functions, and nominal arguments
        // compare by their bodies. A non-alias path keeps this borrowed view;
        // only an alias that actually unfolds allocates an owned expansion.
        let found_path = matches!(found.ty, Type::Path { .. })
            .then(|| {
                ctx.canonicalize_alias_frontier(found.ty, found.identity_canonical, found.scope)
            })
            .flatten();
        let expected_path = matches!(expected.ty, Type::Path { .. })
            .then(|| {
                ctx.canonicalize_alias_frontier(
                    expected.ty,
                    expected.identity_canonical,
                    expected.scope,
                )
            })
            .flatten();
        let found = found_path
            .as_ref()
            .map(|(ty, identity_canonical)| ScopedTypeView {
                ty,
                scope: found.scope,
                identity_canonical: *identity_canonical,
                requirement_source: found.requirement_source,
            })
            .unwrap_or(found);
        let expected = expected_path
            .as_ref()
            .map(|(ty, identity_canonical)| ScopedTypeView {
                ty,
                scope: expected.scope,
                identity_canonical: *identity_canonical,
                requirement_source: expected.requirement_source,
            })
            .unwrap_or(expected);

        match (found.ty, expected.ty) {
            (
                Type::Goal {
                    goal: left,
                    args: left_args,
                    ..
                },
                Type::Goal {
                    goal: right,
                    args: right_args,
                    ..
                },
            ) if left == right => {
                if left_args.len() != right_args.len() {
                    return Err(equation.mismatch(self, delta, ctx, span));
                }
                for (left, right) in left_args.iter().zip(right_args) {
                    self.constrain_views(
                        delta,
                        found.child(left),
                        expected.child(right),
                        equation,
                        span,
                        ctx,
                    )?;
                }
                Ok(())
            }
            (
                Type::Goal {
                    goal: left,
                    args: left_args,
                    ..
                },
                Type::Goal {
                    goal: right,
                    args: right_args,
                    ..
                },
            ) if left_args.is_empty() && right_args.is_empty() => {
                self.meet_goal_policies(delta, *left, *right, span)?;
                let (target, value_goal) =
                    self.orient_goal_pair(*left, *right, delta.owner, span)?;
                let value_identity_canonical = if value_goal == *left {
                    found.identity_canonical
                } else {
                    expected.identity_canonical
                };
                let value_scope = self.owner_state(value_goal.owner(), span)?.scope.clone();
                self.bind_goal(
                    delta,
                    target,
                    ScopedType::from_parts(
                        Type::Goal {
                            goal: value_goal,
                            args: Vec::new(),
                            meta: Meta::new(span),
                            ext: (),
                        },
                        value_scope,
                        value_identity_canonical,
                    ),
                    Some(equation),
                    span,
                    ctx,
                )
            }
            (Type::Goal { args, .. }, _) if args.is_empty() => {
                let Type::Goal { goal, .. } = found.ty else {
                    unreachable!()
                };
                self.bind_goal(
                    delta,
                    *goal,
                    expected.materialize(),
                    Some(equation),
                    span,
                    ctx,
                )
            }
            (_, Type::Goal { args, .. }) if args.is_empty() => {
                let Type::Goal { goal, .. } = expected.ty else {
                    unreachable!()
                };
                self.bind_goal(delta, *goal, found.materialize(), Some(equation), span, ctx)
            }
            (
                Type::Goal {
                    args: found_args, ..
                },
                Type::Goal {
                    args: expected_args,
                    ..
                },
            ) if !found_args.is_empty() && !expected_args.is_empty() => {
                if found_args.len() <= expected_args.len() {
                    self.constrain_applied_goal(
                        delta,
                        found.materialize(),
                        expected.materialize(),
                        EquationSide::Found,
                        equation,
                        span,
                        ctx,
                    )
                } else {
                    self.constrain_applied_goal(
                        delta,
                        expected.materialize(),
                        found.materialize(),
                        EquationSide::Expected,
                        equation,
                        span,
                        ctx,
                    )
                }
            }
            (Type::Goal { args, .. }, _) if !args.is_empty() => self.constrain_applied_goal(
                delta,
                found.materialize(),
                expected.materialize(),
                EquationSide::Found,
                equation,
                span,
                ctx,
            ),
            (_, Type::Goal { args, .. }) if !args.is_empty() => self.constrain_applied_goal(
                delta,
                expected.materialize(),
                found.materialize(),
                EquationSide::Expected,
                equation,
                span,
                ctx,
            ),
            (
                Type::Function {
                    param: found_param,
                    ret: found_ret,
                    ..
                },
                Type::Function {
                    param: expected_param,
                    ret: expected_ret,
                    ..
                },
            ) => {
                self.constrain_views(
                    delta,
                    found.child(found_param),
                    expected.child(expected_param),
                    equation,
                    span,
                    ctx,
                )?;
                self.constrain_views(
                    delta,
                    found.child(found_ret),
                    expected.child(expected_ret),
                    equation,
                    span,
                    ctx,
                )
            }
            (
                Type::Product {
                    left: found_left,
                    right: found_right,
                    ..
                },
                Type::Product {
                    left: expected_left,
                    right: expected_right,
                    ..
                },
            )
            | (
                Type::Sum {
                    left: found_left,
                    right: found_right,
                    ..
                },
                Type::Sum {
                    left: expected_left,
                    right: expected_right,
                    ..
                },
            ) => {
                self.constrain_views(
                    delta,
                    found.child(found_left),
                    expected.child(expected_left),
                    equation,
                    span,
                    ctx,
                )?;
                self.constrain_views(
                    delta,
                    found.child(found_right),
                    expected.child(expected_right),
                    equation,
                    span,
                    ctx,
                )
            }
            (
                Type::Path {
                    segments: found_head,
                    args: found_args,
                    ..
                },
                Type::Path {
                    segments: expected_head,
                    args: expected_args,
                    ..
                },
            ) if found_args.len() == expected_args.len()
                && self.path_heads_equal(
                    found_head,
                    found.scope,
                    expected_head,
                    expected.scope,
                ) =>
            {
                for (found_arg, expected_arg) in found_args.iter().zip(expected_args) {
                    self.constrain_views(
                        delta,
                        found.child(found_arg),
                        expected.child(expected_arg),
                        equation,
                        span,
                        ctx,
                    )?;
                }
                Ok(())
            }
            (
                Type::Forall {
                    param: found_param,
                    body: found_body,
                    ..
                },
                Type::Forall {
                    param: expected_param,
                    body: expected_body,
                    ..
                },
            ) if found_param.effective_kind() == expected_param.effective_kind() => self
                .constrain_foralls(
                    delta,
                    &found.materialize(),
                    found_param,
                    found_body,
                    &expected.materialize(),
                    expected_param,
                    expected_body,
                    equation,
                    span,
                    ctx,
                ),
            (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => {
                Ok(())
            }
            (Type::Infer { .. }, _) | (_, Type::Infer { .. }) => unreachable!(
                "a source `_` placeholder reached the goal solver; planning must allocate a Type::Goal first"
            ),
            (Type::LabelSugar { ext, .. }, _) | (_, Type::LabelSugar { ext, .. }) => match *ext {},
            _ => Err(equation.mismatch(self, delta, ctx, span)),
        }
    }

    fn constrain_children(
        &self,
        delta: &mut GoalDelta,
        found: ScopedTypeView<'_>,
        expected: ScopedTypeView<'_>,
        equation: EquationProvenance<'_>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.constrain_views(delta, found, expected, equation, span, ctx)
    }

    #[allow(clippy::too_many_arguments)] // one structural equation plus its goal-policy context
    fn constrain_applied_goal(
        &self,
        delta: &mut GoalDelta,
        applied: ScopedType,
        other: ScopedType,
        applied_side: EquationSide,
        equation: EquationProvenance<'_>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        let Type::Goal {
            goal,
            args: applied_args,
            ..
        } = applied.ty.as_type()
        else {
            unreachable!("constrain_applied_goal requires an applied goal")
        };
        let (other_head, other_args) = match other.ty.as_type() {
            Type::Path { segments, args, .. } if args.len() >= applied_args.len() => {
                let split = args.len() - applied_args.len();
                (
                    Type::synth_path_segments(segments.clone(), args[..split].to_vec(), span),
                    &args[split..],
                )
            }
            Type::Goal {
                goal: other_goal,
                args,
                ..
            } if args.len() >= applied_args.len() => {
                let split = args.len() - applied_args.len();
                (
                    Type::Goal {
                        goal: *other_goal,
                        args: args[..split].to_vec(),
                        meta: Meta::new(span),
                        ext: (),
                    },
                    &args[split..],
                )
            }
            _ => return Err(equation.mismatch(self, delta, ctx, span)),
        };

        let goal_scope = self.owner_state(goal.owner(), span)?.scope.clone();
        let goal_head = ScopedType::from_parts(
            Type::Goal {
                goal: *goal,
                args: Vec::new(),
                meta: Meta::new(span),
                ext: (),
            },
            goal_scope,
            applied.ty.identity_is_canonical(),
        );
        let other_head = ScopedType::from_parts(
            other_head,
            other.scope.clone(),
            other.ty.identity_is_canonical(),
        );
        let (found_head, expected_head) = match applied_side {
            EquationSide::Found => (goal_head, other_head),
            EquationSide::Expected => (other_head, goal_head),
        };
        self.constrain_inner(delta, found_head, expected_head, equation, span, ctx)?;
        for (left, right) in applied_args.iter().zip(other_args) {
            match applied_side {
                EquationSide::Found => self.constrain_children(
                    delta,
                    ScopedTypeView::from_scoped(&applied).child(left),
                    ScopedTypeView::from_scoped(&other).child(right),
                    equation,
                    span,
                    ctx,
                )?,
                EquationSide::Expected => self.constrain_children(
                    delta,
                    ScopedTypeView::from_scoped(&other).child(right),
                    ScopedTypeView::from_scoped(&applied).child(left),
                    equation,
                    span,
                    ctx,
                )?,
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn constrain_foralls(
        &self,
        delta: &mut GoalDelta,
        found: &ScopedType,
        found_param: &TypeParam,
        found_body: &Type<Lowered>,
        expected: &ScopedType,
        expected_param: &TypeParam,
        expected_body: &Type<Lowered>,
        equation: EquationProvenance<'_>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        let kind = found_param.effective_kind();
        let mut taken = HashSet::new();
        collect_free_type_vars(found_body, &mut taken);
        collect_free_type_vars(expected_body, &mut taken);
        taken.insert(found_param.name.clone());
        taken.insert(expected_param.name.clone());
        let shared = fresh_type_var_with("__alpha__", |candidate| {
            taken.contains(candidate)
                || found.scope.binding(candidate).is_some()
                || expected.scope.binding(candidate).is_some()
        });
        let found_rename = HashMap::from([(
            found_param.name.clone(),
            Type::synth_path(vec![shared.clone()], Vec::new(), found_param.span),
        )]);
        let expected_rename = HashMap::from([(
            expected_param.name.clone(),
            Type::synth_path(vec![shared.clone()], Vec::new(), expected_param.span),
        )]);
        let found_body = subst_type(found_body, &found_rename);
        let expected_body = subst_type(expected_body, &expected_rename);
        let found_scope = found.scope.clone().with_alpha(shared.clone(), kind.clone());
        let expected_scope = expected.scope.clone().with_alpha(shared, kind);
        self.constrain_inner(
            delta,
            ScopedType::from_parts(found_body, found_scope, found.ty.identity_is_canonical()),
            ScopedType::from_parts(
                expected_body,
                expected_scope,
                expected.ty.identity_is_canonical(),
            ),
            equation,
            span,
            ctx,
        )
    }

    fn canonical_scoped(&self, ty: ScopedType, ctx: &impl GoalTypeContext) -> ScopedType {
        let (canonical, identity_canonical) =
            ctx.canonicalize(ty.ty.as_type(), ty.ty.identity_is_canonical(), &ty.scope);
        ScopedType::new(
            InternedType::fresh_with_identity(canonical, identity_canonical)
                .preserving_requirement_from(&ty.ty),
            ty.scope,
        )
    }

    fn path_heads_equal(
        &self,
        found: &[PathSegment],
        found_scope: &RigidScope,
        expected: &[PathSegment],
        expected_scope: &RigidScope,
    ) -> bool {
        if found != expected {
            return false;
        }
        let ([found], [expected]) = (found, expected) else {
            return true;
        };
        match (
            found_scope.binding(found.as_str()),
            expected_scope.binding(expected.as_str()),
        ) {
            (Some(found), Some(expected)) => found == expected,
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        }
    }

    fn bind_goal(
        &self,
        delta: &mut GoalDelta,
        goal: TypeGoalRef,
        candidate: ScopedType,
        equation: Option<EquationProvenance<'_>>,
        span: Span,
        ctx: &impl GoalTypeContext,
    ) -> Result<(), Error> {
        self.require_goal_usable_in_delta(delta, goal, span)?;
        assert!(
            self.is_ancestor_or_same(goal.owner(), delta.owner, span)?,
            "goal solver tried to write a peer, descendant, or foreign inference goal"
        );
        let candidate = self.canonical_scoped(self.zonk_with_delta(&candidate, delta)?, ctx);
        if let Some(existing) = self.binding(goal, Some(delta)) {
            if let Some(equation) = equation {
                return self.constrain_inner(delta, existing, candidate, equation, span, ctx);
            }
            let found = self.canonical_scoped(existing, ctx);
            let equation = EquationProvenance {
                found: ScopedTypeView::from_scoped(&found),
                expected: ScopedTypeView::from_scoped(&candidate),
            };
            delta.begin_equation();
            let result = self.constrain_views(
                delta,
                equation.found,
                equation.expected,
                equation,
                span,
                ctx,
            );
            delta.finish_equation(result.is_ok());
            return result;
        }
        self.require_solution_policy(goal, &candidate, Some(delta), span)?;
        if self.occurs(goal, &candidate, delta, &mut BTreeSet::new())? {
            return Err(Error::type_(
                span,
                "cannot construct an infinite type while solving this inference goal",
            ));
        }

        let target_scope = &self.owner_state(goal.owner(), span)?.scope;
        let mut free = HashSet::new();
        collect_free_type_vars(candidate.ty.as_type(), &mut free);
        if free.iter().any(|name| {
            candidate.scope.binding(name).is_some()
                && target_scope.binding(name) != candidate.scope.binding(name)
        }) {
            return Err(match equation {
                Some(equation) => equation.mismatch(self, delta, ctx, span),
                None => Error::type_(
                    span,
                    "an inferred type would let a nested rigid type variable escape its scope",
                ),
            });
        }

        let refs = goal_refs(candidate.ty.as_type());
        for referenced in &refs {
            self.require_goal_usable_in_delta(delta, *referenced, span)?;
            assert!(
                self.is_ancestor_or_same(referenced.owner(), delta.owner, span)?,
                "goal solver constructed a candidate containing a peer, descendant, or foreign goal"
            );
        }
        let expected_kind = self.goal_state(goal, span)?.kind.clone();
        let actual_kind = self.kind_of(&candidate, delta, ctx, &mut BTreeSet::new())?;
        if actual_kind != expected_kind {
            if equation.is_some() {
                delta.mark_relation_incompatible();
            }
            return Err(Error::type_(
                span,
                format!(
                    "inferred type has kind `{actual_kind}`, but this position requires kind `{expected_kind}`"
                ),
            ));
        }

        delta.write_solution(
            goal,
            GoalBinding {
                value: candidate,
                span,
            },
        );
        Ok(())
    }

    fn require_solution_policy(
        &self,
        goal: TypeGoalRef,
        candidate: &ScopedType,
        delta: Option<&GoalDelta>,
        span: Span,
    ) -> Result<(), Error> {
        if matches!(candidate.ty.as_type(), Type::Forall { .. })
            && self.effective_solution_policy(goal, delta, span)? == GoalSolutionPolicy::Monotype
        {
            Err(Error::type_(
                span,
                "this inference position is predicative and cannot be a polymorphic value type",
            ))
        } else {
            Ok(())
        }
    }

    fn effective_solution_policy(
        &self,
        goal: TypeGoalRef,
        delta: Option<&GoalDelta>,
        span: Span,
    ) -> Result<GoalSolutionPolicy, Error> {
        let mut declared = self.goal_state(goal, span)?.solution_policy;
        if let Some(delta) = delta.filter(|_| !self.producer_views.is_empty()) {
            let mut view = self.delta_producer_view(delta);
            let mut skip_private = matches!(delta.read_view, GoalReadView::EnclosingProducer);
            while let Some(id) = view {
                let current = self.producer_view(id);
                if let Some(meet) = current.context_policies.get(&goal) {
                    declared = declared.meet(meet.policy);
                }
                if !skip_private && let Some(meet) = current.policy_meets.get(&goal) {
                    declared = declared.meet(meet.policy);
                }
                skip_private = false;
                view = current.parent;
            }
        }
        Ok(delta
            .and_then(|delta| delta.policy_meets.get(&goal).map(|meet| meet.policy))
            .map_or(declared, |constraint| declared.meet(constraint)))
    }

    fn meet_goal_policies(
        &self,
        delta: &mut GoalDelta,
        left: TypeGoalRef,
        right: TypeGoalRef,
        span: Span,
    ) -> Result<(), Error> {
        self.require_goal_usable_in_delta(delta, left, span)?;
        self.require_goal_usable_in_delta(delta, right, span)?;
        let policy = self
            .effective_solution_policy(left, Some(delta), span)?
            .meet(self.effective_solution_policy(right, Some(delta), span)?);
        let meet = GoalPolicyMeet { policy, span };
        delta.write_policy_meet(left, meet);
        delta.write_policy_meet(right, meet);

        for goal in [left, right] {
            let open = self.scoped_goal(goal, Vec::new(), span)?;
            let zonked = self.zonk_with_delta(&open, delta)?;
            self.require_solution_policy(goal, &zonked, Some(delta), span)?;
        }
        Ok(())
    }

    fn orient_goal_pair(
        &self,
        left: TypeGoalRef,
        right: TypeGoalRef,
        current: TypeGoalOwner,
        span: Span,
    ) -> Result<(TypeGoalRef, TypeGoalRef), Error> {
        self.goal_state(left, span)?;
        self.goal_state(right, span)?;
        assert!(
            self.is_ancestor_or_same(left.owner(), current, span)?
                && self.is_ancestor_or_same(right.owner(), current, span)?,
            "goal-pair orientation received a peer, descendant, or foreign goal"
        );
        if left.owner() == right.owner() {
            // Slot order chooses only a stable union representative.  It does
            // not select a type, diagnostic, or semantic winner.
            return Ok(if left.slot() > right.slot() {
                (left, right)
            } else {
                (right, left)
            });
        }
        if self.is_strict_ancestor(left.owner(), right.owner(), span)? {
            return Ok((right, left));
        }
        if self.is_strict_ancestor(right.owner(), left.owner(), span)? {
            return Ok((left, right));
        }
        unreachable!("owner-chain validation admitted unrelated inference goals")
    }

    fn binding(&self, goal: TypeGoalRef, delta: Option<&GoalDelta>) -> Option<ScopedType> {
        assert!(
            goal.domain().belongs_to(self.nonce),
            "goal solver tried to read a goal capability from another store"
        );
        delta
            .and_then(|delta| delta.writes.get(&goal))
            .map(|binding| binding.value.clone())
            .or_else(|| {
                delta
                    .and_then(|delta| self.view_binding(goal, delta))
                    .map(|binding| binding.value.clone())
            })
            .or_else(|| {
                self.domains
                    .get(goal.domain().index() as usize)?
                    .owners
                    .get(goal.owner().index() as usize)?
                    .goals
                    .get(goal.slot().index() as usize)?
                    .solution
                    .clone()
            })
    }

    fn occurs(
        &self,
        needle: TypeGoalRef,
        candidate: &ScopedType,
        delta: &GoalDelta,
        visiting: &mut BTreeSet<TypeGoalRef>,
    ) -> Result<bool, Error> {
        for goal in goal_refs(candidate.ty.as_type()) {
            if goal == needle {
                return Ok(true);
            }
            if !visiting.insert(goal) {
                continue;
            }
            if let Some(solution) = self.binding(goal, Some(delta))
                && self.occurs(needle, &solution, delta, visiting)?
            {
                return Ok(true);
            }
            visiting.remove(&goal);
        }
        Ok(false)
    }

    fn kind_of(
        &self,
        scoped: &ScopedType,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        visiting: &mut BTreeSet<TypeGoalRef>,
    ) -> Result<Kind, Error> {
        let mut proofs = super::kind_scheme::CompleteSchemeProofs::default();
        self.kind_of_view(
            ScopedTypeView::from_scoped(scoped),
            delta,
            ctx,
            visiting,
            &mut proofs,
            true,
            super::kind_scheme::KindDemand::Tree,
        )
    }

    fn complete_function_scheme_summary_view(
        &self,
        scoped: ScopedTypeView<'_>,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        visiting: &mut BTreeSet<TypeGoalRef>,
        proofs: &mut super::kind_scheme::CompleteSchemeProofs,
        cacheable: bool,
    ) -> Result<super::kind_scheme::CompleteScheme, Error> {
        struct SchemeView<'a> {
            ty: &'a Type<Lowered>,
            scope: RigidScope,
            identity_canonical: bool,
        }

        let mut deferred = None;
        let mut traversed = Vec::new();
        let direct = super::kind_scheme::classify_complete_scheme(
            SchemeView {
                ty: scoped.ty,
                scope: scoped.scope.clone(),
                identity_canonical: scoped.identity_canonical,
            },
            |view| {
                #[cfg(test)]
                super::kind_scheme::record_goal_frontier_step();
                if cacheable && proofs.contains(view.ty) {
                    return Ok(super::kind_scheme::SchemeFrontier::Complete);
                }
                Ok(match view.ty {
                    Type::Forall { param, body, .. } => {
                        if cacheable {
                            traversed.push(view.ty as *const Type<Lowered> as usize);
                        }
                        super::kind_scheme::SchemeFrontier::Continue(SchemeView {
                            ty: body,
                            scope: view.scope.with_lexical_forall(param),
                            identity_canonical: view.identity_canonical,
                        })
                    }
                    Type::Function { .. } => super::kind_scheme::SchemeFrontier::Complete,
                    Type::Path { .. } | Type::Goal { .. } => {
                        deferred = Some(view);
                        super::kind_scheme::SchemeFrontier::Deferred
                    }
                    Type::Unit { .. }
                    | Type::Bottom { .. }
                    | Type::Product { .. }
                    | Type::Sum { .. } => super::kind_scheme::SchemeFrontier::Incomplete,
                    Type::Infer { .. } => {
                        unreachable!("a source `_` placeholder reached the goal-aware kind checker")
                    }
                    Type::LabelSugar { ext, .. } => match *ext {},
                })
            },
        )?;
        if direct != super::kind_scheme::CompleteScheme::Deferred {
            if cacheable && direct.is_complete() {
                proofs.extend_raw(traversed);
            }
            return Ok(direct);
        }

        let deferred = deferred.expect("a deferred scheme frontier retained its exact view");
        let summary = match deferred.ty {
            Type::Path { .. } => {
                if let Some(analysis) = ctx.complete_scheme_frontier(
                    deferred.ty,
                    deferred.identity_canonical,
                    &deferred.scope,
                ) {
                    let summary = analysis.summary;
                    if cacheable {
                        proofs.extend_raw(analysis.proved_bare_spines);
                    }
                    if summary != super::kind_scheme::CompleteScheme::Deferred {
                        if cacheable && summary.is_complete() {
                            proofs.extend_raw(traversed);
                        }
                        return Ok(summary);
                    }
                }
                let Some((expanded, identity_canonical)) = ctx.canonicalize_alias_frontier(
                    deferred.ty,
                    deferred.identity_canonical,
                    &deferred.scope,
                ) else {
                    return Ok(super::kind_scheme::CompleteScheme::Incomplete);
                };
                self.complete_function_scheme_summary_view(
                    ScopedTypeView {
                        ty: &expanded,
                        scope: &deferred.scope,
                        identity_canonical,
                        requirement_source: None,
                    },
                    delta,
                    ctx,
                    visiting,
                    proofs,
                    false,
                )?
            }
            Type::Goal {
                goal, args, meta, ..
            } => {
                if !visiting.insert(*goal) {
                    return Err(Error::type_(
                        meta.span,
                        "cyclic inference-goal solution encountered while checking kinds",
                    ));
                }
                let summary = if let Some(solution) = self.binding(*goal, Some(delta)) {
                    let resolved = self.append_goal_args(
                        solution,
                        args,
                        deferred.identity_canonical,
                        &deferred.scope,
                        meta.span,
                    )?;
                    self.complete_function_scheme_summary_view(
                        ScopedTypeView::from_scoped(&resolved),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        false,
                    )?
                } else {
                    super::kind_scheme::CompleteScheme::Incomplete
                };
                visiting.remove(goal);
                summary
            }
            _ => unreachable!("only a path or goal can defer a scheme frontier"),
        };
        if cacheable && summary.is_complete() {
            proofs.extend_raw(traversed);
        }
        Ok(summary)
    }

    pub(crate) fn classify_complete_function_scheme(
        &self,
        scoped: &ScopedType,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
    ) -> Result<super::kind_scheme::CompleteScheme, Error> {
        #[cfg(test)]
        super::kind_scheme::record_open_view_consumer();
        self.complete_function_scheme_summary_view(
            ScopedTypeView::from_scoped(scoped),
            delta,
            ctx,
            &mut BTreeSet::new(),
            &mut super::kind_scheme::CompleteSchemeProofs::default(),
            true,
        )
    }

    pub(crate) fn classify_complete_function_scheme_at_owner(
        &self,
        ty: &Type<Lowered>,
        identity_canonical: bool,
        owner: TypeGoalOwner,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        proofs: &mut super::kind_scheme::CompleteSchemeProofs,
    ) -> Result<super::kind_scheme::CompleteScheme, Error> {
        #[cfg(test)]
        super::kind_scheme::record_open_view_consumer();
        let scope = &self.owner_state(owner, ty.span())?.scope;
        self.complete_function_scheme_summary_view(
            ScopedTypeView {
                ty,
                scope,
                identity_canonical,
                requirement_source: None,
            },
            delta,
            ctx,
            &mut BTreeSet::new(),
            proofs,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)] // one shared traversal threads exact goal/scope/proof state
    fn kind_of_view(
        &self,
        scoped: ScopedTypeView<'_>,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        visiting: &mut BTreeSet<TypeGoalRef>,
        proofs: &mut super::kind_scheme::CompleteSchemeProofs,
        cacheable: bool,
        demand: super::kind_scheme::KindDemand,
    ) -> Result<Kind, Error> {
        match scoped.ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let identity_canonical = scoped.identity_canonical;
                let head = ctx.nominal_head_kind(
                    segments,
                    args.len(),
                    meta.span,
                    scoped.scope,
                    identity_canonical,
                )?;
                self.apply_kind(
                    head,
                    args,
                    scoped.scope,
                    delta,
                    ctx,
                    visiting,
                    proofs,
                    cacheable,
                    meta.span,
                    identity_canonical,
                    demand,
                )
            }
            Type::Goal {
                goal, args, meta, ..
            } => {
                if !visiting.insert(*goal) {
                    return Err(Error::type_(
                        meta.span,
                        "cyclic inference-goal solution encountered while checking kinds",
                    ));
                }
                let result = if let Some(solution) = self.binding(*goal, Some(delta)) {
                    let resolved = self.append_goal_args(
                        solution,
                        args,
                        scoped.identity_canonical,
                        scoped.scope,
                        meta.span,
                    )?;
                    self.kind_of_view(
                        ScopedTypeView::from_scoped(&resolved),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        false,
                        demand,
                    )
                } else {
                    let head = self.goal_state(*goal, meta.span)?.kind.clone();
                    self.apply_kind(
                        head,
                        args,
                        scoped.scope,
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        cacheable,
                        meta.span,
                        scoped.identity_canonical,
                        demand,
                    )
                };
                visiting.remove(goal);
                result
            }
            Type::Function {
                param, ret, meta, ..
            } => {
                if demand.validates_tree() {
                    self.require_star(
                        scoped.child(param),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        cacheable,
                        meta.span,
                        demand,
                    )?;
                    let ret_kind = self.kind_of_view(
                        scoped.child(ret),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        cacheable,
                        demand,
                    )?;
                    if ret_kind != Kind::Star {
                        return Err(Error::type_(
                            meta.span,
                            format!("value type requires kind `*`, found kind `{ret_kind}`"),
                        ));
                    }
                }
                Ok(Kind::Star)
            }
            Type::Product { left, right, meta } | Type::Sum { left, right, meta } => {
                if demand.validates_tree() {
                    self.require_star(
                        scoped.child(left),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        cacheable,
                        meta.span,
                        demand,
                    )?;
                    self.require_star(
                        scoped.child(right),
                        delta,
                        ctx,
                        visiting,
                        proofs,
                        cacheable,
                        meta.span,
                        demand,
                    )?;
                }
                Ok(Kind::Star)
            }
            Type::Unit { .. } | Type::Bottom { .. } => Ok(Kind::Star),
            Type::Forall { param, body, meta } => {
                if param.effective_kind() != Kind::Star && {
                    #[cfg(test)]
                    super::kind_scheme::record_goal_kind_consumer();
                    !self
                        .complete_function_scheme_summary_view(
                            scoped, delta, ctx, visiting, proofs, cacheable,
                        )?
                        .is_complete()
                } {
                    return Err(Error::type_(
                        meta.span,
                        "a higher-kinded `Forall` binder must bind a complete function scheme",
                    ));
                }
                let nested = scoped.scope.clone().with_lexical_forall(param);
                self.kind_of_view(
                    ScopedTypeView {
                        ty: body,
                        scope: &nested,
                        identity_canonical: scoped.identity_canonical,
                        requirement_source: scoped.requirement_source,
                    },
                    delta,
                    ctx,
                    visiting,
                    proofs,
                    cacheable,
                    demand,
                )
            }
            Type::Infer { .. } => {
                unreachable!("a source `_` placeholder reached the goal-aware kind checker")
            }
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_kind(
        &self,
        mut remaining: Kind,
        args: &[Type<Lowered>],
        scope: &RigidScope,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        visiting: &mut BTreeSet<TypeGoalRef>,
        proofs: &mut super::kind_scheme::CompleteSchemeProofs,
        cacheable: bool,
        span: Span,
        identity_canonical: bool,
        demand: super::kind_scheme::KindDemand,
    ) -> Result<Kind, Error> {
        for arg in args {
            let (domain, codomain) = match remaining {
                Kind::Arrow(domain, codomain) => (*domain, *codomain),
                Kind::Star => {
                    return Err(Error::type_(
                        span,
                        "a kind-`*` type cannot be applied to another type",
                    ));
                }
            };
            let arg_kind = self.kind_of_view(
                ScopedTypeView {
                    ty: arg,
                    scope,
                    identity_canonical,
                    requirement_source: None,
                },
                delta,
                ctx,
                visiting,
                proofs,
                cacheable,
                demand,
            )?;
            if arg_kind != domain {
                return Err(Error::type_(
                    arg.span(),
                    format!(
                        "type application argument has kind `{arg_kind}`, but kind `{domain}` is required"
                    ),
                ));
            }
            remaining = codomain;
        }
        Ok(remaining)
    }

    #[allow(clippy::too_many_arguments)] // preserves the shared traversal state without rebuilding it
    fn require_star(
        &self,
        scoped: ScopedTypeView<'_>,
        delta: &GoalDelta,
        ctx: &impl GoalTypeContext,
        visiting: &mut BTreeSet<TypeGoalRef>,
        proofs: &mut super::kind_scheme::CompleteSchemeProofs,
        cacheable: bool,
        span: Span,
        demand: super::kind_scheme::KindDemand,
    ) -> Result<(), Error> {
        let kind = self.kind_of_view(scoped, delta, ctx, visiting, proofs, cacheable, demand)?;
        if kind == Kind::Star {
            Ok(())
        } else {
            Err(Error::type_(
                span,
                format!("value type requires kind `*`, found kind `{kind}`"),
            ))
        }
    }

    fn zonk_with_delta(&self, ty: &ScopedType, delta: &GoalDelta) -> Result<ScopedType, Error> {
        self.zonk_type(ty, Some(delta), &mut BTreeSet::new())
    }

    fn zonk_type(
        &self,
        scoped: &ScopedType,
        delta: Option<&GoalDelta>,
        visiting: &mut BTreeSet<TypeGoalRef>,
    ) -> Result<ScopedType, Error> {
        self.zonk_raw(
            scoped.ty.as_type(),
            &scoped.scope,
            scoped.ty.identity_is_canonical(),
            delta,
            visiting,
            false,
        )
        .map(|raw| {
            let mut result = raw.into_scoped();
            if !matches!(scoped.ty.as_type(), Type::Goal { .. }) {
                result.ty = result.ty.preserving_requirement_from(&scoped.ty);
            }
            result
        })
    }

    fn zonk_raw(
        &self,
        ty: &Type<Lowered>,
        scope: &RigidScope,
        identity: bool,
        delta: Option<&GoalDelta>,
        visiting: &mut BTreeSet<TypeGoalRef>,
        allow_source_infer: bool,
    ) -> Result<RawScopedType, Error> {
        let rebuilt = match ty {
            Type::Goal {
                goal, args, meta, ..
            } => {
                let mut zonked_args = Vec::with_capacity(args.len());
                let mut args_identity = true;
                let mut combined_scope = scope.clone();
                for arg in args {
                    let arg =
                        self.zonk_raw(arg, scope, identity, delta, visiting, allow_source_infer)?;
                    args_identity &= arg.identity_canonical;
                    combined_scope = combined_scope.merged(&arg.scope).map_err(|()| {
                        Error::type_(
                            meta.span,
                            "inference combined incompatible rigid binder scopes",
                        )
                    })?;
                    zonked_args.push(arg.ty);
                }
                if let Some(solution) = self.binding(*goal, delta) {
                    if !visiting.insert(*goal) {
                        return Err(Error::type_(
                            meta.span,
                            "cyclic inference-goal solution encountered while zonking",
                        ));
                    }
                    let solution = self.zonk_raw(
                        solution.ty.as_type(),
                        &solution.scope,
                        solution.ty.identity_is_canonical(),
                        delta,
                        visiting,
                        allow_source_infer,
                    )?;
                    visiting.remove(goal);
                    self.append_goal_args_raw(
                        solution,
                        &zonked_args,
                        args_identity,
                        &combined_scope,
                        meta.span,
                    )?
                } else {
                    RawScopedType {
                        ty: Type::Goal {
                            goal: *goal,
                            args: zonked_args,
                            meta: meta.clone(),
                            ext: (),
                        },
                        scope: combined_scope,
                        identity_canonical: identity && args_identity,
                    }
                }
            }
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => {
                let param =
                    self.zonk_raw(param, scope, identity, delta, visiting, allow_source_infer)?;
                let ret =
                    self.zonk_raw(ret, scope, identity, delta, visiting, allow_source_infer)?;
                let scope = param.scope.merged(&ret.scope).map_err(|()| {
                    Error::type_(
                        meta.span,
                        "inference combined incompatible rigid binder scopes",
                    )
                })?;
                RawScopedType {
                    ty: Type::Function {
                        param: Box::new(param.ty),
                        ret: Box::new(ret.ty),
                        meta: meta.clone(),
                        abi_arity: *abi_arity,
                        caps: *caps,
                    },
                    scope,
                    identity_canonical: identity
                        && param.identity_canonical
                        && ret.identity_canonical,
                }
            }
            Type::Product { left, right, meta } | Type::Sum { left, right, meta } => {
                let left =
                    self.zonk_raw(left, scope, identity, delta, visiting, allow_source_infer)?;
                let right =
                    self.zonk_raw(right, scope, identity, delta, visiting, allow_source_infer)?;
                let scope = left.scope.merged(&right.scope).map_err(|()| {
                    Error::type_(
                        meta.span,
                        "inference combined incompatible rigid binder scopes",
                    )
                })?;
                let ty = if matches!(ty, Type::Product { .. }) {
                    Type::Product {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: meta.clone(),
                    }
                } else {
                    Type::Sum {
                        left: Box::new(left.ty),
                        right: Box::new(right.ty),
                        meta: meta.clone(),
                    }
                };
                RawScopedType {
                    ty,
                    scope,
                    identity_canonical: identity
                        && left.identity_canonical
                        && right.identity_canonical,
                }
            }
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let mut rebuilt_args = Vec::with_capacity(args.len());
                let mut combined_scope = scope.clone();
                let mut args_identity = true;
                for arg in args {
                    let arg =
                        self.zonk_raw(arg, scope, identity, delta, visiting, allow_source_infer)?;
                    args_identity &= arg.identity_canonical;
                    combined_scope = combined_scope.merged(&arg.scope).map_err(|()| {
                        Error::type_(
                            meta.span,
                            "inference combined incompatible rigid binder scopes",
                        )
                    })?;
                    rebuilt_args.push(arg.ty);
                }
                RawScopedType {
                    ty: Type::synth_path_segments(segments.clone(), rebuilt_args, meta.span),
                    scope: combined_scope,
                    identity_canonical: identity && args_identity,
                }
            }
            Type::Forall { param, body, meta } => {
                let nested_scope = scope.clone().with_lexical_forall(param);
                let body = self.zonk_raw(
                    body,
                    &nested_scope,
                    identity,
                    delta,
                    visiting,
                    allow_source_infer,
                )?;
                RawScopedType {
                    ty: Type::Forall {
                        param: param.clone(),
                        body: Box::new(body.ty),
                        meta: meta.clone(),
                    },
                    scope: scope.clone(),
                    identity_canonical: identity && body.identity_canonical,
                }
            }
            Type::Unit { .. } | Type::Bottom { .. } => RawScopedType {
                ty: ty.clone(),
                scope: scope.clone(),
                identity_canonical: identity,
            },
            Type::Infer { .. } if allow_source_infer => RawScopedType {
                ty: ty.clone(),
                scope: scope.clone(),
                identity_canonical: identity,
            },
            Type::Infer { .. } => unreachable!("a source `_` placeholder reached goal zonking"),
            Type::LabelSugar { ext, .. } => match *ext {},
        };
        Ok(rebuilt)
    }

    fn append_goal_args_raw(
        &self,
        solution: RawScopedType,
        args: &[Type<Lowered>],
        args_identity_canonical: bool,
        use_scope: &RigidScope,
        span: Span,
    ) -> Result<RawScopedType, Error> {
        let scope = solution.scope.merged(use_scope).map_err(|()| {
            Error::type_(span, "inference combined incompatible rigid binder scopes")
        })?;
        let ty = append_type_args(solution.ty, args.to_vec(), span).ok_or_else(|| {
            Error::type_(
                span,
                "a higher-kinded inference goal resolved to a non-applicable type",
            )
        })?;
        Ok(RawScopedType {
            ty,
            scope,
            identity_canonical: solution.identity_canonical && args_identity_canonical,
        })
    }

    fn append_goal_args(
        &self,
        solution: ScopedType,
        args: &[Type<Lowered>],
        args_identity_canonical: bool,
        use_scope: &RigidScope,
        span: Span,
    ) -> Result<ScopedType, Error> {
        self.append_goal_args_raw(
            RawScopedType {
                ty: solution.ty.clone_type(),
                scope: solution.scope,
                identity_canonical: solution.ty.identity_is_canonical(),
            },
            args,
            args_identity_canonical,
            use_scope,
            span,
        )
        .map(RawScopedType::into_scoped)
    }

    fn require_escape(&self, ty: &ScopedType, escape: GoalEscape) -> Result<(), Error> {
        let destination = match escape {
            GoalEscape::ToOwner(destination)
            | GoalEscape::ToOwnerFunctionScheme(destination)
            | GoalEscape::ClosedAt(destination)
            | GoalEscape::ClosedFunctionSchemeAt(destination) => destination,
        };
        for goal in goal_refs(ty.ty.as_type()) {
            match escape {
                GoalEscape::ClosedAt(_) | GoalEscape::ClosedFunctionSchemeAt(_) => {
                    return Err(Error::type_(
                        ty.ty.span(),
                        "an unresolved inference goal reached a closed type boundary",
                    ));
                }
                GoalEscape::ToOwner(owner) | GoalEscape::ToOwnerFunctionScheme(owner)
                    if !self.is_ancestor_or_same(goal.owner(), owner, ty.ty.span())? =>
                {
                    panic!("goal-store output retained a goal outside its destination owner chain");
                }
                GoalEscape::ToOwner(_) | GoalEscape::ToOwnerFunctionScheme(_) => {}
            }
        }
        let destination_scope = &self.owner_state(destination, ty.ty.span())?.scope;
        let mut free = HashSet::new();
        collect_free_type_vars(ty.ty.as_type(), &mut free);
        if free
            .iter()
            .any(|name| destination_scope.binding(name) != ty.scope.binding(name))
        {
            return Err(Error::type_(
                ty.ty.span(),
                "an inference result retained a rigid type variable outside its destination scope",
            ));
        }
        Ok(())
    }

    fn publication_destination(
        &self,
        owner: TypeGoalOwner,
        span: Span,
        escape: PublicationGoalEscape,
    ) -> Result<TypeGoalOwner, Error> {
        let destination = match escape {
            PublicationGoalEscape::ToOwner(destination)
            | PublicationGoalEscape::ToOwnerFunctionScheme(destination) => {
                assert_eq!(
                    self.owner_state(owner, span)?.parent,
                    Some(destination),
                    "publication output must retain into its direct inference parent"
                );
                destination
            }
            PublicationGoalEscape::ClosedAt(destination)
            | PublicationGoalEscape::ClosedFunctionSchemeAt(destination) => {
                assert_eq!(
                    destination, owner,
                    "closed publication output targeted a different inference owner"
                );
                assert!(
                    self.owner_state(owner, span)?.parent.is_none(),
                    "closed publication output requires a root inference owner"
                );
                destination
            }
        };
        self.owner_state(destination, span)?;
        Ok(destination)
    }

    fn require_publication_escape(
        &self,
        ty: &ScopedType,
        escape: PublicationGoalEscape,
    ) -> Result<bool, Error> {
        let destination = match escape {
            PublicationGoalEscape::ToOwner(destination)
            | PublicationGoalEscape::ToOwnerFunctionScheme(destination)
            | PublicationGoalEscape::ClosedAt(destination)
            | PublicationGoalEscape::ClosedFunctionSchemeAt(destination) => destination,
        };
        let goals = goal_refs(ty.ty.as_type());
        let goal_free = goals.is_empty();
        for goal in goals {
            match escape {
                PublicationGoalEscape::ClosedAt(_)
                | PublicationGoalEscape::ClosedFunctionSchemeAt(_) => {
                    return Err(Error::type_(
                        ty.ty.span(),
                        "an unresolved inference goal reached a closed publication boundary",
                    ));
                }
                PublicationGoalEscape::ToOwner(owner)
                | PublicationGoalEscape::ToOwnerFunctionScheme(owner)
                    if !self.is_ancestor_or_same(goal.owner(), owner, ty.ty.span())? =>
                {
                    panic!(
                        "publication output retained a goal outside its destination owner chain"
                    );
                }
                PublicationGoalEscape::ToOwner(_)
                | PublicationGoalEscape::ToOwnerFunctionScheme(_) => {}
            }
        }
        let destination_scope = &self.owner_state(destination, ty.ty.span())?.scope;
        assert!(
            ty.scope.extends(destination_scope),
            "publication output did not preserve a lexical scope extending its destination"
        );
        Ok(goal_free)
    }
}

/// Structured outcome of one stack-confined specialization equation.
#[derive(Debug)]
pub(crate) enum IsolatedSpecialization<T> {
    Matched(Vec<T>),
    NoMatch,
}

/// Solve one binder-local specialization through the ordinary goal relation.
/// The closures keep binder-proof substitution and returned-value construction
/// in the marked carrier that owns those proofs; no local goal capability can
/// cross this boundary.
#[allow(clippy::too_many_arguments)] // one equation carries its binder scopes, constructors, and isolated goal context
pub(crate) fn solve_isolated_specialization<T>(
    params: &[TypeParam],
    target: &InternedType<Lowered>,
    pattern_scope: &RigidScope,
    target_scope: &RigidScope,
    instantiate_pattern: impl FnOnce(&[InternedType<Lowered>]) -> InternedType<Lowered>,
    mut finalize_output: impl FnMut(ScopedType) -> Result<T, Error>,
    span: Span,
    ctx: &IsolatedSpecializationContext,
) -> Result<IsolatedSpecialization<T>, Error> {
    let pattern_base_scope = pattern_scope.clone().without_lexical_foralls(params);
    let source_scope = pattern_base_scope.merged(target_scope).map_err(|()| {
        Error::type_(
            span,
            "specialization operands carry incompatible rigid binder scopes",
        )
    })?;
    let isolated_scope = source_scope.for_isolated_specialization();
    ctx.validate_input(&ScopedType::new(
        target.clone(),
        target_scope.for_isolated_specialization(),
    ))?;
    let target = ScopedType::new(target.clone(), isolated_scope.clone());

    let mut store = GoalStore::new_isolated();
    let owner = store.begin_owner(
        None,
        isolated_scope.clone(),
        GoalOwnerKind::Application,
        span,
    )?;
    let mut goals = Vec::with_capacity(params.len());
    let mut replacements = Vec::with_capacity(params.len());
    for param in params {
        let goal = store.alloc_goal(
            owner,
            param.effective_kind(),
            GoalSolutionPolicy::PolytypeAllowed,
            GoalOrigin::named(span, GoalRole::TypeArgument, param.name.clone()),
        )?;
        let occurrence = store.scoped_goal_at(goal, Vec::new(), owner, span)?;
        replacements.push(occurrence.ty().clone());
        goals.push(goal);
    }

    let instantiated = instantiate_pattern(&replacements);
    let found = ScopedType::new(instantiated, isolated_scope.clone());
    ctx.validate_solver_pattern(&found)?;
    let mut delta = store.begin_delta(owner, span)?;
    if !store.constrain_isolated_specialization_equation(&mut delta, found, target, span, ctx)? {
        return Ok(IsolatedSpecialization::NoMatch);
    }
    let outputs = goals
        .into_iter()
        .map(|goal| {
            Ok((
                store.scoped_goal_at(goal, Vec::new(), owner, span)?,
                GoalEscape::ClosedAt(owner),
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let prepared = store.prepare_owner_with_publication(delta, outputs, Vec::new(), ctx)?;
    let (commit, outputs, publication_outputs) = prepared.into_parts();
    assert!(
        publication_outputs.into_outputs().is_empty(),
        "isolated specialization unexpectedly produced publication state"
    );
    store.commit_prepared_owner(commit);
    outputs
        .into_outputs()
        .into_iter()
        .map(|output| match output {
            ClosedGoalOutput::GoalFree(output) => {
                let mut output = output.into_scoped_type();
                assert!(
                    !type_contains_goal(output.ty().as_type()),
                    "isolated specialization leaked a local inference goal"
                );
                output.scope = source_scope.projected_for_goal_free_type(output.ty().as_type());
                finalize_output(output)
            }
            ClosedGoalOutput::Retained(_)
            | ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                unreachable!("isolated binder output was not an ordinary goal-free type")
            }
        })
        .collect::<Result<Vec<_>, Error>>()
        .map(IsolatedSpecialization::Matched)
}

pub(crate) fn solve_scoped_specialization_by_name(
    params: &[TypeParam],
    pattern: &ScopedType,
    target: &ScopedType,
    span: Span,
    ctx: &IsolatedSpecializationContext,
) -> Result<IsolatedSpecialization<ScopedType>, Error> {
    if type_contains_goal(pattern.ty().as_type()) || type_contains_goal(target.ty().as_type()) {
        return solve_scoped_specialization_with_opaque_goals(params, pattern, target, span, ctx);
    }
    solve_isolated_specialization(
        params,
        target.ty(),
        &pattern.scope,
        &target.scope,
        |replacements| {
            let substitutions = params
                .iter()
                .zip(replacements)
                .map(|(param, replacement)| (param.name.clone(), replacement.clone_type()))
                .collect::<HashMap<_, _>>();
            InternedType::fresh_canonical(subst_type(pattern.ty().as_type(), &substitutions))
        },
        Ok,
        span,
        ctx,
    )
}

#[derive(Debug)]
struct OpaqueSpecializationGoal {
    name: String,
    kind: Kind,
    exact: Type<Lowered>,
}

fn solve_scoped_specialization_with_opaque_goals(
    params: &[TypeParam],
    pattern: &ScopedType,
    target: &ScopedType,
    span: Span,
    ctx: &IsolatedSpecializationContext,
) -> Result<IsolatedSpecialization<ScopedType>, Error> {
    fn collect_single_path_names(ty: &Type<Lowered>, names: &mut HashSet<String>) {
        match ty {
            Type::Path { segments, args, .. } => {
                if let [name] = segments.as_slice() {
                    names.insert(name.name.clone());
                }
                for arg in args {
                    collect_single_path_names(arg, names);
                }
            }
            Type::Goal { args, .. } => {
                for arg in args {
                    collect_single_path_names(arg, names);
                }
            }
            Type::Function { param, ret, .. } => {
                collect_single_path_names(param, names);
                collect_single_path_names(ret, names);
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                collect_single_path_names(left, names);
                collect_single_path_names(right, names);
            }
            Type::Forall { param, body, .. } => {
                names.insert(param.name.clone());
                collect_single_path_names(body, names);
            }
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    fn replace_goals(
        ty: &Type<Lowered>,
        ctx: &IsolatedSpecializationContext,
        by_goal: &mut HashMap<TypeGoalRef, usize>,
        placeholders: &mut Vec<OpaqueSpecializationGoal>,
        forbidden_names: &mut HashSet<String>,
    ) -> Result<Type<Lowered>, Error> {
        if let Type::Goal {
            goal,
            args,
            meta,
            ext,
        } = ty
        {
            let kind = ctx.opaque_goal_head_kind(*goal, args.len(), meta.span)?;
            let index = if let Some(index) = by_goal.get(goal) {
                *index
            } else {
                let mut ordinal = placeholders.len();
                let name = loop {
                    let candidate = format!("\0isolated-specialization-open-{ordinal}");
                    if forbidden_names.insert(candidate.clone()) {
                        break candidate;
                    }
                    ordinal = ordinal
                        .checked_add(1)
                        .expect("specialization placeholder count exceeded usize");
                };
                let index = placeholders.len();
                placeholders.push(OpaqueSpecializationGoal {
                    name,
                    kind,
                    exact: Type::Goal {
                        goal: *goal,
                        args: Vec::new(),
                        meta: meta.clone(),
                        ext: *ext,
                    },
                });
                by_goal.insert(*goal, index);
                index
            };
            let args = args
                .iter()
                .map(|arg| replace_goals(arg, ctx, by_goal, placeholders, forbidden_names))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Type::synth_path(
                vec![placeholders[index].name.clone()],
                args,
                meta.span,
            ));
        }

        let mut replaced = ty.clone();
        match &mut replaced {
            Type::Path { args, .. } => {
                for arg in args {
                    *arg = replace_goals(arg, ctx, by_goal, placeholders, forbidden_names)?;
                }
            }
            Type::Function { param, ret, .. } => {
                **param = replace_goals(param, ctx, by_goal, placeholders, forbidden_names)?;
                **ret = replace_goals(ret, ctx, by_goal, placeholders, forbidden_names)?;
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                **left = replace_goals(left, ctx, by_goal, placeholders, forbidden_names)?;
                **right = replace_goals(right, ctx, by_goal, placeholders, forbidden_names)?;
            }
            Type::Forall { body, .. } => {
                **body = replace_goals(body, ctx, by_goal, placeholders, forbidden_names)?;
            }
            Type::Goal { .. } => unreachable!("goal replacement returned at the root"),
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
            Type::LabelSugar { ext, .. } => match *ext {},
        }
        Ok(replaced)
    }

    let pattern_base_scope = pattern.scope.clone().without_lexical_foralls(params);
    let exact_source_scope = pattern_base_scope.merged(&target.scope).map_err(|()| {
        Error::type_(
            span,
            "specialization operands carry incompatible rigid binder scopes",
        )
    })?;
    let mut forbidden_names = HashSet::new();
    collect_single_path_names(pattern.ty().as_type(), &mut forbidden_names);
    collect_single_path_names(target.ty().as_type(), &mut forbidden_names);
    exact_source_scope.bindings.for_each(|name, _| {
        forbidden_names.insert(name.to_owned());
    });
    let mut by_goal = HashMap::new();
    let mut placeholders = Vec::new();
    let opaque_pattern = replace_goals(
        pattern.ty().as_type(),
        ctx,
        &mut by_goal,
        &mut placeholders,
        &mut forbidden_names,
    )?;
    let opaque_target = replace_goals(
        target.ty().as_type(),
        ctx,
        &mut by_goal,
        &mut placeholders,
        &mut forbidden_names,
    )?;

    let mut opaque_source_scope = exact_source_scope.clone();
    for placeholder in &placeholders {
        opaque_source_scope =
            opaque_source_scope.with_alpha(placeholder.name.clone(), placeholder.kind.clone());
    }
    let mut opaque_pattern_scope = opaque_source_scope.clone();
    for param in params {
        opaque_pattern_scope = opaque_pattern_scope.with_lexical_forall(param);
    }
    let opaque_pattern = InternedType::fresh_canonical(opaque_pattern);
    let opaque_target = InternedType::fresh_canonical(opaque_target);
    let rehydration = placeholders
        .into_iter()
        .map(|placeholder| (placeholder.name, placeholder.exact))
        .collect::<HashMap<_, _>>();

    solve_isolated_specialization(
        params,
        &opaque_target,
        &opaque_pattern_scope,
        &opaque_source_scope,
        |replacements| {
            let substitutions = params
                .iter()
                .zip(replacements)
                .map(|(param, replacement)| (param.name.clone(), replacement.clone_type()))
                .collect::<HashMap<_, _>>();
            InternedType::fresh_canonical(subst_type(opaque_pattern.as_type(), &substitutions))
        },
        |output| {
            let exact = subst_type(output.ty().as_type(), &rehydration);
            Ok(ScopedType::new(
                InternedType::fresh_canonical(exact),
                exact_source_scope.clone(),
            ))
        },
        span,
        ctx,
    )
}

pub(crate) fn solve_canonical_specialization_by_name(
    params: &[TypeParam],
    pattern: &InternedType<Lowered>,
    target: &InternedType<Lowered>,
    span: Span,
    ctx: &IsolatedSpecializationContext,
) -> Result<IsolatedSpecialization<InternedType<Lowered>>, Error> {
    let mut pattern_scope = RigidScope::empty(
        TypeGoalStoreIdentity::ISOLATED,
        GoalContextCapability::isolated_specialization(),
        "<isolated-specialization>".to_owned(),
    );
    for param in params {
        pattern_scope = pattern_scope.with_lexical_forall(param);
    }
    let target_scope = RigidScope::empty(
        TypeGoalStoreIdentity::ISOLATED,
        GoalContextCapability::isolated_specialization(),
        "<isolated-specialization>".to_owned(),
    );
    solve_isolated_specialization(
        params,
        target,
        &pattern_scope,
        &target_scope,
        |replacements| {
            let substitutions = params
                .iter()
                .zip(replacements)
                .map(|(param, replacement)| (param.name.clone(), replacement.clone_type()))
                .collect::<HashMap<_, _>>();
            InternedType::fresh_canonical(subst_type(pattern.as_type(), &substitutions))
        },
        |output| Ok(output.into_interned_type()),
        span,
        ctx,
    )
}

fn goal_refs(ty: &Type<Lowered>) -> BTreeSet<TypeGoalRef> {
    let mut refs = BTreeSet::new();
    for_each_type_event(ty, false, &mut |event| {
        if let TypeWalkEvent::Goal(goal) = event {
            refs.insert(goal);
        }
    });
    refs
}

fn goal_heads_in_structural_order(ty: &Type<Lowered>) -> Vec<TypeGoalRef> {
    let mut seen = HashSet::new();
    let mut heads = Vec::new();
    for_each_type_event(ty, false, &mut |event| {
        if let TypeWalkEvent::Goal(goal) = event
            && seen.insert(goal)
        {
            heads.push(goal);
        }
    });
    heads
}

fn replace_goal_heads(ty: &mut Type<Lowered>, replacements: &HashMap<TypeGoalRef, TypeGoalRef>) {
    use crate::pass::visit_mut::{TypecheckVisitMut, walk_type};

    struct Replace<'a>(&'a HashMap<TypeGoalRef, TypeGoalRef>);

    impl TypecheckVisitMut<Lowered> for Replace<'_> {
        fn visit_type(&mut self, ty: &mut Type<Lowered>) {
            if let Type::Goal { goal, .. } = ty
                && let Some(replacement) = self.0.get(goal)
            {
                *goal = *replacement;
            }
            walk_type(self, ty);
        }
    }

    Replace(replacements).visit_type(ty);
}

fn diagnostic_goal_refs(ty: &Type<Lowered>) -> BTreeSet<TypeGoalRef> {
    let mut refs = BTreeSet::new();
    for_each_type_event(ty, true, &mut |event| {
        if let TypeWalkEvent::Goal(goal) = event {
            refs.insert(goal);
        }
    });
    refs
}

fn is_function_scheme(ty: &Type<Lowered>) -> bool {
    match ty {
        Type::Forall { body, .. } => is_function_scheme(body),
        Type::Function { .. } => true,
        _ => false,
    }
}

pub(crate) fn contains_goal_beneath_forall(ty: &Type<Lowered>, beneath_forall: bool) -> bool {
    match ty {
        Type::Goal { args, .. } => {
            beneath_forall
                || args
                    .iter()
                    .any(|arg| contains_goal_beneath_forall(arg, beneath_forall))
        }
        Type::Forall { body, .. } => contains_goal_beneath_forall(body, true),
        Type::Function { param, ret, .. } => {
            contains_goal_beneath_forall(param, beneath_forall)
                || contains_goal_beneath_forall(ret, beneath_forall)
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            contains_goal_beneath_forall(left, beneath_forall)
                || contains_goal_beneath_forall(right, beneath_forall)
        }
        Type::Path { args, .. } => args
            .iter()
            .any(|arg| contains_goal_beneath_forall(arg, beneath_forall)),
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => false,
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

enum TypeWalkEvent<'a> {
    Goal(TypeGoalRef),
    SingleSegmentPath(&'a str),
    EnterBinder(&'a str),
    ExitBinder(&'a str),
}

fn for_each_type_event<'a>(
    ty: &'a Type<Lowered>,
    allow_source_infer: bool,
    visit: &mut impl FnMut(TypeWalkEvent<'a>),
) {
    match ty {
        Type::Goal { goal, args, .. } => {
            visit(TypeWalkEvent::Goal(*goal));
            for arg in args {
                for_each_type_event(arg, allow_source_infer, visit);
            }
        }
        Type::Function { param, ret, .. } => {
            for_each_type_event(param, allow_source_infer, visit);
            for_each_type_event(ret, allow_source_infer, visit);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            for_each_type_event(left, allow_source_infer, visit);
            for_each_type_event(right, allow_source_infer, visit);
        }
        Type::Path { segments, args, .. } => {
            if let [segment] = segments.as_slice() {
                visit(TypeWalkEvent::SingleSegmentPath(&segment.name));
            }
            for arg in args {
                for_each_type_event(arg, allow_source_infer, visit);
            }
        }
        Type::Forall { param, body, .. } => {
            visit(TypeWalkEvent::EnterBinder(&param.name));
            for_each_type_event(body, allow_source_infer, visit);
            visit(TypeWalkEvent::ExitBinder(&param.name));
        }
        Type::Unit { .. } | Type::Bottom { .. } => {}
        Type::Infer { .. } if allow_source_infer => {}
        Type::Infer { .. } => {
            unreachable!("a source `_` placeholder reached inference-goal traversal")
        }
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

fn for_each_free_type_var(
    ty: &Type<Lowered>,
    allow_source_infer: bool,
    seen: &mut HashSet<String>,
    bound: &mut HashMap<String, u32>,
    visit: &mut impl FnMut(&str),
) {
    seen.clear();
    bound.clear();
    for_each_type_event(ty, allow_source_infer, &mut |event| match event {
        TypeWalkEvent::SingleSegmentPath(name)
            if bound.get(name).copied().unwrap_or_default() == 0
                && seen.insert(name.to_owned()) =>
        {
            visit(name);
        }
        TypeWalkEvent::EnterBinder(name) => {
            if let Some(count) = bound.get_mut(name) {
                *count += 1;
            } else {
                bound.insert(name.to_owned(), 1);
            }
        }
        TypeWalkEvent::ExitBinder(name) => {
            let count = bound
                .get_mut(name)
                .expect("type traversal exited a binder it did not enter");
            *count -= 1;
        }
        TypeWalkEvent::Goal(_) | TypeWalkEvent::SingleSegmentPath(_) => {}
    });
    debug_assert!(bound.values().all(|count| *count == 0));
}

#[cfg(test)]
mod tests {
    use super::super::aliases::{
        deep_canonicalization_calls_for_test, reset_deep_canonicalization_calls_for_test,
        type_equiv_state,
    };
    use super::*;
    use crate::ast::{Expr, Item, SignatureParam};
    use crate::pass::full::FullPipeline;
    use crate::pass::parser::parse;
    use crate::pass::resolve::Package;
    use crate::pass::typecheck_core::{PendingRecOrderState, display_type};
    use crate::pass::typecheck_full::publication::PublicationBuilder;
    use crate::pass::typecheck_full::{Elaborations, RecordedElaboration};
    use crate::pipeline::Pipeline;
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

    trait GoalOutputTestExt {
        fn ty(&self) -> &InternedType<Lowered>;
    }

    impl GoalOutputTestExt for RetainedGoalOutput {
        fn ty(&self) -> &InternedType<Lowered> {
            self.value.ty()
        }
    }

    impl GoalOutputTestExt for GoalFreeFunctionSchemeOutput {
        fn ty(&self) -> &InternedType<Lowered> {
            self.value.ty()
        }
    }

    impl GoalOutputTestExt for GoalFreeOutput {
        fn ty(&self) -> &InternedType<Lowered> {
            self.value.ty()
        }
    }

    trait ClosedGoalOutputsTestExt {
        fn outputs(&self) -> &[ClosedGoalOutput];
    }

    impl ClosedGoalOutputsTestExt for ClosedGoalOutputs {
        fn outputs(&self) -> &[ClosedGoalOutput] {
            &self.outputs
        }
    }

    trait GoalStoreTestExt {
        fn owner_scope(&self, owner: TypeGoalOwner, span: Span) -> Result<RigidScope, Error>;

        fn close_prepared_owner(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(PreparedCloseType, GoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<ClosedGoalOutputs, Error>;

        fn close_owner(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(ScopedType, GoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<ClosedGoalOutputs, Error>;

        fn close_owner_with_publication(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(ScopedType, GoalEscape)>,
            publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<(ClosedGoalOutputs, ClosedPublicationOutputs), Error>;
    }

    impl GoalStoreTestExt for GoalStore {
        fn owner_scope(&self, owner: TypeGoalOwner, span: Span) -> Result<RigidScope, Error> {
            Ok(self.owner_state(owner, span)?.scope.clone())
        }

        fn close_prepared_owner(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(PreparedCloseType, GoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<ClosedGoalOutputs, Error> {
            let prepared =
                self.prepare_prepared_owner_with_publication(delta, outputs, Vec::new(), ctx)?;
            let (commit, outputs, publication_outputs) = prepared.into_parts();
            assert!(
                publication_outputs.outputs.is_empty(),
                "ordinary prepared close unexpectedly returned publication outputs"
            );
            self.commit_prepared_owner(commit);
            Ok(outputs)
        }

        fn close_owner(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(ScopedType, GoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<ClosedGoalOutputs, Error> {
            let prepared = self.prepare_owner_with_publication(delta, outputs, Vec::new(), ctx)?;
            let (commit, outputs, publication_outputs) = prepared.into_parts();
            assert!(
                publication_outputs.outputs.is_empty(),
                "ordinary goal close unexpectedly returned publication outputs"
            );
            self.commit_prepared_owner(commit);
            Ok(outputs)
        }

        fn close_owner_with_publication(
            &mut self,
            delta: GoalDelta,
            outputs: Vec<(ScopedType, GoalEscape)>,
            publication_outputs: Vec<(PublicationGoalInput, PublicationGoalEscape)>,
            ctx: &impl GoalTypeContext,
        ) -> Result<(ClosedGoalOutputs, ClosedPublicationOutputs), Error> {
            let prepared =
                self.prepare_owner_with_publication(delta, outputs, publication_outputs, ctx)?;
            let (commit, outputs, publication_outputs) = prepared.into_parts();
            self.commit_prepared_owner(commit);
            Ok((outputs, publication_outputs))
        }
    }

    struct TestContext {
        capability: u64,
        kinds: BTreeMap<Vec<String>, Kind>,
        canonicalize_identity: Cell<bool>,
        canonicalize_calls: Cell<usize>,
        alias_frontier_probes: Cell<usize>,
        display_mismatch_types: bool,
    }

    impl Default for TestContext {
        fn default() -> Self {
            Self {
                capability: 1,
                kinds: BTreeMap::new(),
                canonicalize_identity: Cell::new(true),
                canonicalize_calls: Cell::new(0),
                alias_frontier_probes: Cell::new(0),
                display_mismatch_types: false,
            }
        }
    }

    impl TestContext {
        fn with_capability(mut self, capability: u64) -> Self {
            self.capability = capability;
            self
        }

        fn with_kind(mut self, name: &[&str], kind: Kind) -> Self {
            self.kinds
                .insert(name.iter().map(|part| (*part).to_owned()).collect(), kind);
            self
        }

        fn with_display_mismatch_types(mut self) -> Self {
            self.display_mismatch_types = true;
            self
        }

        fn set_canonicalize_identity(&self, identity_canonical: bool) {
            self.canonicalize_identity.set(identity_canonical);
        }
    }

    impl GoalTypeContext for TestContext {
        fn capability(&self) -> GoalContextCapability {
            GoalContextCapability::Test(self.capability)
        }

        fn canonicalize(
            &self,
            ty: &Type<Lowered>,
            _identity_canonical: bool,
            _scope: &RigidScope,
        ) -> (Type<Lowered>, bool) {
            self.canonicalize_calls
                .set(self.canonicalize_calls.get() + 1);
            (ty.clone(), self.canonicalize_identity.get())
        }

        fn canonicalize_alias_frontier(
            &self,
            _ty: &Type<Lowered>,
            _identity_canonical: bool,
            _scope: &RigidScope,
        ) -> Option<(Type<Lowered>, bool)> {
            self.alias_frontier_probes
                .set(self.alias_frontier_probes.get() + 1);
            None
        }

        fn nominal_head_kind(
            &self,
            segments: &[PathSegment],
            _supplied_args: usize,
            span: Span,
            scope: &RigidScope,
            _identity_canonical: bool,
        ) -> Result<Kind, Error> {
            if segments.len() == 1
                && let Some(kind) = scope.kind(segments[0].as_str())
            {
                return Ok(kind);
            }
            let key = segments
                .iter()
                .map(|segment| segment.name.clone())
                .collect::<Vec<_>>();
            self.kinds.get(&key).cloned().ok_or_else(|| {
                Error::type_(
                    span,
                    format!("test type `{}` has no declared kind", key.join(".")),
                )
            })
        }

        fn mismatch(&self, found: &ScopedType, expected: &ScopedType, span: Span) -> Error {
            if self.display_mismatch_types {
                return Error::type_(
                    span,
                    format!(
                        "type mismatch: expected `{}`, found `{}`",
                        display_type(expected.ty.as_type()),
                        display_type(found.ty.as_type()),
                    ),
                );
            }
            Error::type_(span, "type mismatch")
        }
    }

    fn sp() -> Span {
        Span::new(0, 1)
    }

    fn path(names: &[&str], args: Vec<Type<Lowered>>) -> Type<Lowered> {
        Type::synth_path(
            names.iter().map(|name| (*name).to_owned()).collect(),
            args,
            sp(),
        )
    }

    fn unit() -> Type<Lowered> {
        Type::Unit {
            meta: Meta::new(sp()),
        }
    }

    fn product(left: Type<Lowered>, right: Type<Lowered>) -> Type<Lowered> {
        Type::Product {
            left: Box::new(left),
            right: Box::new(right),
            meta: Meta::new(sp()),
        }
    }

    fn function(param: Type<Lowered>, ret: Type<Lowered>) -> Type<Lowered> {
        Type::Function {
            param: Box::new(param),
            ret: Box::new(ret),
            meta: Meta::new(sp()),
            abi_arity: 1,
            caps: (),
        }
    }

    fn nested_nominal(depth: usize) -> Type<Lowered> {
        (0..depth).fold(unit(), |arg, _| path(&["m", "Box"], vec![arg]))
    }

    fn forall(name: &str, body: Type<Lowered>) -> Type<Lowered> {
        Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span: sp(),
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(sp()),
        }
    }

    fn ctx() -> TestContext {
        TestContext::default()
            .with_kind(&["m", "I32"], Kind::Star)
            .with_kind(&["m", "String"], Kind::Star)
            .with_kind(&["m", "Bool"], Kind::Star)
            .with_kind(&["m", "Box"], Kind::arrow_chain(1))
            .with_kind(&["m", "Pair"], Kind::arrow_chain(2))
    }

    fn owner(store: &mut GoalStore, scope: RigidScope) -> TypeGoalOwner {
        store
            .begin_owner(None, scope, GoalOwnerKind::Application, sp())
            .unwrap()
    }

    fn goal(store: &mut GoalStore, owner: TypeGoalOwner, kind: Kind) -> TypeGoalRef {
        store
            .alloc_goal(
                owner,
                kind,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "T"),
            )
            .unwrap()
    }

    fn goal_type(goal: TypeGoalRef) -> Type<Lowered> {
        Type::Goal {
            goal,
            args: Vec::new(),
            meta: Meta::new(sp()),
            ext: (),
        }
    }

    fn scoped_canonical(store: &GoalStore, owner: TypeGoalOwner, ty: Type<Lowered>) -> ScopedType {
        store
            .scoped_type(owner, InternedType::fresh_canonical(ty), sp())
            .unwrap()
    }

    fn isolated_ctx() -> Arc<IsolatedSpecializationContext> {
        IsolatedSpecializationContext::from_canonical_nominal_head_kinds(HashMap::from([
            (vec!["m".to_owned(), "I32".to_owned()], Kind::Star),
            (vec!["m".to_owned(), "String".to_owned()], Kind::Star),
        ]))
    }

    #[test]
    fn producer_views_validate_full_ancestor_obligations_atomically() {
        for reverse in [false, true] {
            let context = ctx();
            let mut store = GoalStore::new();
            assert_eq!(store.producer_views.capacity(), 0);
            let host = owner(&mut store, RigidScope::new());
            let shared = goal(&mut store, host, Kind::Star);
            let mut host_delta = store.begin_delta(host, sp()).unwrap();
            let mut producers = Vec::new();
            for name in if reverse {
                ["String", "I32"]
            } else {
                ["I32", "String"]
            } {
                let producer = store
                    .begin_owner(
                        Some(host),
                        RigidScope::new(),
                        GoalOwnerKind::RetainedValue,
                        sp(),
                    )
                    .unwrap();
                store
                    .isolate_producer(producer, &host_delta, sp(), &context)
                    .unwrap();
                let child = store
                    .begin_owner(
                        Some(producer),
                        RigidScope::new(),
                        GoalOwnerKind::RetainedValue,
                        sp(),
                    )
                    .unwrap();
                let mut delta = store.begin_delta(child, sp()).unwrap();
                store
                    .constrain(
                        &mut delta,
                        scoped_canonical(&store, child, goal_type(shared)),
                        scoped_canonical(&store, child, path(&["m", name], Vec::new())),
                        sp(),
                        &context,
                    )
                    .unwrap();
                store.close_owner(delta, Vec::new(), &context).unwrap();
                assert!(
                    store.binding(shared, Some(&host_delta)).is_none(),
                    "a descendant proposal escaped its producer"
                );
                let delta = store.begin_delta(producer, sp()).unwrap();
                producers.push(store.producer_obligations(&delta, &context).unwrap());
            }
            let revision = store.revision;
            let error = store
                .accept_producer_wave(&mut host_delta, Vec::new(), producers, Vec::new(), &context)
                .expect_err("conflicting non-output obligations must reject the wave");
            assert!(error.diagnostic().message.contains("type mismatch"));
            assert_eq!(store.revision, revision);
            assert!(host_delta.is_empty());
            assert!(store.binding(shared, Some(&host_delta)).is_none());
            assert_eq!(
                store.producer_views.len(),
                2,
                "a failed trial leaked its read view"
            );
            assert!(
                store
                    .producer_views
                    .iter()
                    .all(|view| !view.accepted && view.writes.len() == 1)
            );
        }
        eprintln!(
            "producer layouts: GoalDelta={} OwnerState={} GoalStore={} ProducerView={} GoalReadView={}",
            std::mem::size_of::<GoalDelta>(),
            std::mem::size_of::<OwnerState>(),
            std::mem::size_of::<GoalStore>(),
            std::mem::size_of::<ProducerView>(),
            std::mem::size_of::<GoalReadView>()
        );
    }

    #[test]
    fn producer_views_refresh_live_context_and_keep_wrapper_owner_transport() {
        let context = ctx();
        let mut store = GoalStore::new();
        let host = owner(&mut store, RigidScope::new());
        let outside = goal(&mut store, host, Kind::Star);
        let mut host_delta = store.begin_delta(host, sp()).unwrap();
        let wrapper = store
            .begin_owner(
                Some(host),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let local = goal(&mut store, wrapper, Kind::Star);
        let mut wrapper_delta = store.begin_delta(wrapper, sp()).unwrap();
        let earlier = store
            .begin_owner(
                Some(wrapper),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        store
            .isolate_producer(earlier, &wrapper_delta, sp(), &context)
            .unwrap();
        let earlier_delta = store.begin_delta(earlier, sp()).unwrap();
        let producer = store
            .begin_owner(
                Some(wrapper),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        store
            .isolate_producer(producer, &host_delta, sp(), &context)
            .unwrap();
        let producer_delta = store.begin_delta(producer, sp()).unwrap();
        store
            .constrain(
                &mut host_delta,
                scoped_canonical(&store, host, goal_type(outside)),
                scoped_canonical(&store, host, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .unwrap();
        store
            .isolate_producer(producer, &host_delta, sp(), &context)
            .unwrap();
        assert!(store.binding(outside, Some(&producer_delta)).is_some());
        assert!(
            store.goal_state(outside, sp()).unwrap().solution.is_none(),
            "refresh must not commit the host delta"
        );
        let child = store
            .begin_owner(
                Some(producer),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        store
            .constrain(
                &mut child_delta,
                scoped_canonical(&store, child, goal_type(local)),
                scoped_canonical(&store, child, path(&["m", "String"], Vec::new())),
                sp(),
                &context,
            )
            .unwrap();
        store
            .close_owner(child_delta, Vec::new(), &context)
            .unwrap();
        assert!(store.binding(local, Some(&wrapper_delta)).is_none());
        let obligations = store
            .producer_obligations(&producer_delta, &context)
            .unwrap();
        store
            .accept_producer_wave(
                &mut host_delta,
                vec![&mut wrapper_delta],
                vec![obligations],
                Vec::new(),
                &context,
            )
            .unwrap();
        assert!(
            !host_delta.writes.contains_key(&local),
            "the host cannot own a wrapper-local equation"
        );
        assert_eq!(
            store.owner_state(producer, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert!(store.goal_state(local, sp()).unwrap().solution.is_none());
        store
            .isolate_producer(earlier, &wrapper_delta, sp(), &context)
            .unwrap();
        assert!(
            store.binding(local, Some(&earlier_delta)).is_some(),
            "the earlier pending producer must see the accepted later wrapper fact before either physically closes"
        );
        let earlier_obligations = store
            .producer_obligations(&earlier_delta, &context)
            .unwrap();
        store
            .accept_producer_wave(
                &mut host_delta,
                vec![&mut wrapper_delta],
                vec![earlier_obligations],
                Vec::new(),
                &context,
            )
            .unwrap();
        store
            .close_owner(earlier_delta, Vec::new(), &context)
            .unwrap();
        store
            .close_owner(producer_delta, Vec::new(), &context)
            .unwrap();
        assert!(store.binding(local, Some(&wrapper_delta)).is_some());
        assert!(store.goal_state(outside, sp()).unwrap().solution.is_none());
        assert!(
            store
                .producer_views
                .iter()
                .all(|view| view.writes.is_empty()
                    && view.context_writes.is_empty()
                    && view.policy_meets.is_empty()
                    && view.context_policies.is_empty())
        );
        store
            .close_owner(wrapper_delta, Vec::new(), &context)
            .unwrap();
        store.close_owner(host_delta, Vec::new(), &context).unwrap();
    }

    #[test]
    fn producer_views_validate_policy_meets_in_the_same_wave() {
        let context = ctx();
        let mut store = GoalStore::new();
        let host = owner(&mut store, RigidScope::new());
        let shared = store
            .alloc_goal(
                host,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "T"),
            )
            .unwrap();
        let mut host_delta = store.begin_delta(host, sp()).unwrap();
        let mut producers = Vec::new();
        for proposes_scheme in [true, false] {
            let producer = store
                .begin_owner(
                    Some(host),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            store
                .isolate_producer(producer, &host_delta, sp(), &context)
                .unwrap();
            let child = store
                .begin_owner(
                    Some(producer),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            let mut delta = store.begin_delta(child, sp()).unwrap();
            if proposes_scheme {
                store
                    .constrain(
                        &mut delta,
                        scoped_canonical(&store, child, goal_type(shared)),
                        scoped_canonical(
                            &store,
                            child,
                            forall(
                                "A",
                                function(path(&["A"], Vec::new()), path(&["A"], Vec::new())),
                            ),
                        ),
                        sp(),
                        &context,
                    )
                    .unwrap();
            } else {
                delta.write_policy_meet(
                    shared,
                    GoalPolicyMeet {
                        policy: GoalSolutionPolicy::Monotype,
                        span: sp(),
                    },
                );
            }
            store.close_owner(delta, Vec::new(), &context).unwrap();
            let delta = store.begin_delta(producer, sp()).unwrap();
            producers.push(store.producer_obligations(&delta, &context).unwrap());
        }
        assert!(
            store
                .accept_producer_wave(&mut host_delta, Vec::new(), producers, Vec::new(), &context)
                .is_err()
        );
        assert!(host_delta.is_empty());
        assert_eq!(
            store
                .effective_solution_policy(shared, Some(&host_delta), sp())
                .unwrap(),
            GoalSolutionPolicy::PolytypeAllowed
        );
    }

    #[test]
    fn producer_views_refresh_preserves_equations_derived_from_private_aliases() {
        let context = ctx();
        let mut store = GoalStore::new();
        let host = owner(&mut store, RigidScope::new());
        let inner = goal(&mut store, host, Kind::Star);
        let outer = goal(&mut store, host, Kind::Star);
        let mut host_delta = store.begin_delta(host, sp()).unwrap();
        let producer = store
            .begin_owner(
                Some(host),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        store
            .isolate_producer(producer, &host_delta, sp(), &context)
            .unwrap();
        let child = store
            .begin_owner(
                Some(producer),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        store
            .constrain(
                &mut child_delta,
                scoped_canonical(&store, child, goal_type(outer)),
                scoped_canonical(&store, child, goal_type(inner)),
                sp(),
                &context,
            )
            .unwrap();
        store
            .close_owner(child_delta, Vec::new(), &context)
            .unwrap();
        store
            .constrain(
                &mut host_delta,
                scoped_canonical(&store, host, goal_type(outer)),
                scoped_canonical(&store, host, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .unwrap();
        store
            .isolate_producer(producer, &host_delta, sp(), &context)
            .unwrap();
        let producer_delta = store.begin_delta(producer, sp()).unwrap();
        let value = store
            .try_zonk_goal_free(
                &producer_delta,
                scoped_canonical(&store, producer, goal_type(inner)),
                &context,
            )
            .unwrap()
            .expect("accepted context must propagate through the private alias before resumption");
        assert_eq!(display_type(value.ty.as_type()), "m.I32");
        assert!(
            store.binding(inner, Some(&host_delta)).is_none(),
            "producer-derived context remains private"
        );
    }

    #[test]
    fn producer_views_preflight_live_wrapper_bindings_and_policies_before_any_write() {
        for policy_conflict in [false, true] {
            let context = ctx();
            let mut store = GoalStore::new();
            let host = owner(&mut store, RigidScope::new());
            let shared = goal(&mut store, host, Kind::Star);
            let mut host_delta = store.begin_delta(host, sp()).unwrap();
            let wrapper = store
                .begin_owner(
                    Some(host),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            let local = store
                .alloc_goal(
                    wrapper,
                    Kind::Star,
                    GoalSolutionPolicy::PolytypeAllowed,
                    GoalOrigin::named(sp(), GoalRole::TypeArgument, "T"),
                )
                .unwrap();
            let mut wrapper_delta = store.begin_delta(wrapper, sp()).unwrap();
            let producer = store
                .begin_owner(
                    Some(wrapper),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            store
                .isolate_producer(producer, &wrapper_delta, sp(), &context)
                .unwrap();
            let child = store
                .begin_owner(
                    Some(producer),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            let mut child_delta = store.begin_delta(child, sp()).unwrap();
            let candidate = if policy_conflict {
                forall(
                    "A",
                    function(path(&["A"], Vec::new()), path(&["A"], Vec::new())),
                )
            } else {
                path(&["m", "String"], Vec::new())
            };
            store
                .constrain(
                    &mut child_delta,
                    scoped_canonical(&store, child, goal_type(local)),
                    scoped_canonical(&store, child, candidate),
                    sp(),
                    &context,
                )
                .unwrap();
            store
                .constrain(
                    &mut child_delta,
                    scoped_canonical(&store, child, goal_type(shared)),
                    scoped_canonical(&store, child, unit()),
                    sp(),
                    &context,
                )
                .unwrap();
            store
                .close_owner(child_delta, Vec::new(), &context)
                .unwrap();
            let producer_delta = store.begin_delta(producer, sp()).unwrap();
            let obligations = store
                .producer_obligations(&producer_delta, &context)
                .unwrap();
            if policy_conflict {
                wrapper_delta.write_policy_meet(
                    local,
                    GoalPolicyMeet {
                        policy: GoalSolutionPolicy::Monotype,
                        span: sp(),
                    },
                );
            } else {
                store
                    .constrain(
                        &mut wrapper_delta,
                        scoped_canonical(&store, wrapper, goal_type(local)),
                        scoped_canonical(&store, wrapper, path(&["m", "I32"], Vec::new())),
                        sp(),
                        &context,
                    )
                    .unwrap();
            }
            let revision = store.revision;
            let wrapper_before = format!("{wrapper_delta:?}");
            assert!(
                store
                    .accept_producer_wave(
                        &mut host_delta,
                        vec![&mut wrapper_delta],
                        vec![obligations],
                        Vec::new(),
                        &context
                    )
                    .is_err()
            );
            assert_eq!(store.revision, revision);
            assert!(host_delta.is_empty());
            assert_eq!(format!("{wrapper_delta:?}"), wrapper_before);
            assert!(store.goal_state(shared, sp()).unwrap().solution.is_none());
            assert!(store.goal_state(local, sp()).unwrap().solution.is_none());
            assert!(store.producer_views.iter().all(|view| !view.accepted));
        }
    }

    #[test]
    fn producer_views_reconcile_host_local_equations_without_foreign_owner_reads() {
        for inherited in [false, true] {
            for related in [false, true] {
                let context = ctx();
                let mut store = GoalStore::new();
                let outer = owner(&mut store, RigidScope::new());
                let shared = goal(&mut store, outer, Kind::Star);
                let independent = goal(&mut store, outer, Kind::Star);
                let outer_delta = store.begin_delta(outer, sp()).unwrap();
                let host = store
                    .begin_owner(
                        Some(outer),
                        RigidScope::new(),
                        GoalOwnerKind::RetainedValue,
                        sp(),
                    )
                    .unwrap();
                let local = goal(&mut store, host, Kind::Star);
                let mut host_delta = store.begin_delta(host, sp()).unwrap();
                store
                    .constrain(
                        &mut host_delta,
                        scoped_canonical(&store, host, goal_type(shared)),
                        scoped_canonical(&store, host, function(unit(), goal_type(local))),
                        sp(),
                        &context,
                    )
                    .unwrap();
                assert!(!store.delta_writes_are_close_ready(&host_delta).unwrap());
                let original = format!("{:?}", host_delta.writes.get(&shared));
                if inherited {
                    store
                        .isolate_producer(host, &host_delta, sp(), &context)
                        .unwrap();
                    host_delta = store.begin_delta(host, sp()).unwrap();
                }
                let producer = store
                    .begin_owner(
                        Some(outer),
                        RigidScope::new(),
                        GoalOwnerKind::RetainedValue,
                        sp(),
                    )
                    .unwrap();
                store
                    .isolate_producer(producer, &outer_delta, sp(), &context)
                    .unwrap();
                let mut producer_delta = store.begin_delta(producer, sp()).unwrap();
                store
                    .constrain(
                        &mut producer_delta,
                        scoped_canonical(
                            &store,
                            producer,
                            goal_type(if related { shared } else { independent }),
                        ),
                        scoped_canonical(
                            &store,
                            producer,
                            if related {
                                function(unit(), unit())
                            } else {
                                unit()
                            },
                        ),
                        sp(),
                        &context,
                    )
                    .unwrap();
                let obligations = store
                    .producer_obligations(&producer_delta, &context)
                    .unwrap();
                let views = store.producer_views.len();
                store
                    .accept_producer_wave(
                        &mut host_delta,
                        Vec::new(),
                        vec![obligations],
                        Vec::new(),
                        &context,
                    )
                    .unwrap();
                assert_eq!(store.producer_views.len(), views);
                assert_eq!(store.binding(local, Some(&host_delta)).is_some(), related);
                assert!(store.goal_state(local, sp()).unwrap().solution.is_none());
                assert!(outer_delta.is_empty());
                if !related && !inherited {
                    assert_eq!(format!("{:?}", host_delta.writes.get(&shared)), original);
                    assert!(!store.delta_writes_are_close_ready(&host_delta).unwrap());
                }
                if related {
                    let resolved = store
                        .try_zonk_goal_free(
                            &producer_delta,
                            scoped_canonical(&store, producer, goal_type(shared)),
                            &context,
                        )
                        .unwrap()
                        .expect("accepted host consequence must not retain the foreign local hole");
                    assert_eq!(display_type(resolved.ty.as_type()), ". -> .");
                }
            }
        }
    }

    #[test]
    fn producer_views_recheck_alias_policy_against_raw_host_context_atomically() {
        for inherited in [false, true] {
            let context = ctx();
            let mut store = GoalStore::new();
            let outer = owner(&mut store, RigidScope::new());
            let monotype = goal(&mut store, outer, Kind::Star);
            let shared = store
                .alloc_goal(
                    outer,
                    Kind::Star,
                    GoalSolutionPolicy::PolytypeAllowed,
                    GoalOrigin::named(sp(), GoalRole::TypeArgument, "H"),
                )
                .unwrap();
            let outer_delta = store.begin_delta(outer, sp()).unwrap();
            let host = store
                .begin_owner(
                    Some(outer),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            let mut host_delta = store.begin_delta(host, sp()).unwrap();
            store
                .constrain(
                    &mut host_delta,
                    scoped_canonical(&store, host, goal_type(shared)),
                    scoped_canonical(
                        &store,
                        host,
                        forall(
                            "T",
                            function(path(&["T"], Vec::new()), path(&["T"], Vec::new())),
                        ),
                    ),
                    sp(),
                    &context,
                )
                .unwrap();
            if inherited {
                store
                    .isolate_producer(host, &host_delta, sp(), &context)
                    .unwrap();
                host_delta = store.begin_delta(host, sp()).unwrap();
            }
            let producer = store
                .begin_owner(
                    Some(outer),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            store
                .isolate_producer(producer, &outer_delta, sp(), &context)
                .unwrap();
            let mut producer_delta = store.begin_delta(producer, sp()).unwrap();
            store
                .constrain(
                    &mut producer_delta,
                    scoped_canonical(&store, producer, goal_type(monotype)),
                    scoped_canonical(&store, producer, goal_type(shared)),
                    sp(),
                    &context,
                )
                .unwrap();
            let obligations = store
                .producer_obligations(&producer_delta, &context)
                .unwrap();
            let revision = store.revision;
            let host_before = format!("{host_delta:?}");
            let views_before = format!("{:?}", store.producer_views);
            assert!(
                store
                    .accept_producer_wave(
                        &mut host_delta,
                        Vec::new(),
                        vec![obligations],
                        Vec::new(),
                        &context
                    )
                    .is_err()
            );
            assert_eq!(store.revision, revision);
            assert_eq!(format!("{host_delta:?}"), host_before);
            assert_eq!(format!("{:?}", store.producer_views), views_before);
            assert!(outer_delta.is_empty());
        }
    }

    #[test]
    fn isolated_specialization_returns_binder_ordered_goal_free_arguments() {
        let params = [
            TypeParam {
                name: "A".to_owned(),
                span: sp(),
                kind: None,
            },
            TypeParam {
                name: "B".to_owned(),
                span: sp(),
                kind: None,
            },
        ];
        let pattern = InternedType::fresh_canonical(product(
            path(&["A"], Vec::new()),
            path(&["B"], Vec::new()),
        ));
        let target = InternedType::fresh_canonical(product(
            path(&["m", "I32"], Vec::new()),
            path(&["m", "String"], Vec::new()),
        ));
        let IsolatedSpecialization::Matched(arguments) = solve_canonical_specialization_by_name(
            &params,
            &pattern,
            &target,
            sp(),
            &isolated_ctx(),
        )
        .expect("compatible isolated relation should close every binder") else {
            panic!("compatible isolated relation was reported as a miss")
        };
        assert_eq!(
            arguments
                .iter()
                .map(|argument| display_type(argument.as_type()))
                .collect::<Vec<_>>(),
            ["m.I32", "m.String"]
        );
        assert!(
            arguments
                .iter()
                .all(|argument| !type_contains_goal(argument.as_type()))
        );
    }

    #[test]
    fn isolated_specialization_classifies_structural_mismatch_without_text() {
        let params = [TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        }];
        let pattern = InternedType::fresh_canonical(product(
            path(&["A"], Vec::new()),
            path(&["A"], Vec::new()),
        ));
        let target = InternedType::fresh_canonical(Type::Sum {
            left: Box::new(path(&["m", "I32"], Vec::new())),
            right: Box::new(path(&["m", "I32"], Vec::new())),
            meta: Meta::new(sp()),
        });
        let outcome = solve_canonical_specialization_by_name(
            &params,
            &pattern,
            &target,
            sp(),
            &isolated_ctx(),
        )
        .expect("ordinary structural incompatibility is not malformed");
        assert!(matches!(outcome, IsolatedSpecialization::NoMatch));
    }

    #[test]
    fn isolated_specialization_accepts_a_binder_free_exact_equation() {
        let ty = InternedType::fresh_canonical(unit());
        let outcome = solve_canonical_specialization_by_name(&[], &ty, &ty, sp(), &isolated_ctx())
            .expect("a binder-free exact equation is a valid specialization");
        let IsolatedSpecialization::Matched(arguments) = outcome else {
            panic!("a binder-free exact equation was reported as a miss")
        };
        assert!(arguments.is_empty());
    }

    #[test]
    fn isolated_specialization_reports_an_underdetermined_binder() {
        let params = [
            TypeParam {
                name: "A".to_owned(),
                span: sp(),
                kind: None,
            },
            TypeParam {
                name: "Unused".to_owned(),
                span: sp(),
                kind: None,
            },
        ];
        let error = solve_canonical_specialization_by_name(
            &params,
            &InternedType::fresh_canonical(path(&["A"], Vec::new())),
            &InternedType::fresh_canonical(unit()),
            sp(),
            &isolated_ctx(),
        )
        .expect_err("unused flexible binder must remain underdetermined");
        assert!(error.diagnostic().message.contains("Unused"));
    }

    #[test]
    fn scoped_specialization_preserves_target_rigid_proof_through_instantiation() {
        let param = TypeParam {
            name: "T".to_owned(),
            span: sp(),
            kind: None,
        };
        let outer_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 7)]);
        let pattern = ScopedType::new(
            InternedType::fresh_canonical(path(&["T"], Vec::new())),
            outer_scope.clone().with_lexical_forall(&param),
        );
        let target = ScopedType::new(
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
            outer_scope.clone(),
        );
        let IsolatedSpecialization::Matched(mut arguments) = solve_scoped_specialization_by_name(
            std::slice::from_ref(&param),
            &pattern,
            &target,
            sp(),
            &isolated_ctx(),
        )
        .expect("the scoped rigid target should specialize") else {
            panic!("the scoped rigid target was reported as a miss")
        };
        let argument = arguments.pop().expect("one forall produced one argument");
        assert_eq!(
            argument.scope.binding("A"),
            outer_scope.binding("A"),
            "the inferred argument must retain the target's exact rigid proof"
        );
        assert!(
            argument.scope.binding("T").is_none(),
            "the flexible specialization binder must not escape in output evidence"
        );

        let scheme = ScopedType::new(
            InternedType::fresh_canonical(forall(
                "T",
                function(path(&["T"], Vec::new()), path(&["A"], Vec::new())),
            )),
            outer_scope.clone(),
        );
        let instantiated = scheme
            .projected_instantiate_forall(&argument)
            .expect("a solved argument instantiates its corresponding forall");
        assert_eq!(display_type(instantiated.ty().as_type()), "A -> A");
        assert_eq!(instantiated.scope, outer_scope);
    }

    #[test]
    fn scoped_specialization_rehydrates_an_open_target_without_store_authority() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let open_goal = goal(&mut store, owner, Kind::Star);
        let target = scoped_canonical(&store, owner, goal_type(open_goal));
        let param = TypeParam {
            name: "T".to_owned(),
            span: sp(),
            kind: None,
        };
        let pattern = ScopedType::new(
            InternedType::fresh_canonical(path(&["T"], Vec::new())),
            target.scope.clone().with_lexical_forall(&param),
        );
        let context = IsolatedSpecializationContext {
            nominal_head_kinds: HashMap::new(),
            opaque_goal_kinds: HashMap::from([(open_goal, Kind::Star)]),
        };

        let IsolatedSpecialization::Matched(mut arguments) = solve_scoped_specialization_by_name(
            std::slice::from_ref(&param),
            &pattern,
            &target,
            sp(),
            &context,
        )
        .expect("an open target is rigid evidence, not isolated-store authority") else {
            panic!("an open target was reported as a structural miss")
        };
        let argument = arguments.pop().expect("one forall produced one argument");
        assert_eq!(
            argument.transient_key(),
            target.transient_key(),
            "the matched argument must recover the exact foreign goal and rigid scope"
        );

        let scheme = ScopedType::new(
            InternedType::fresh_canonical(forall("T", path(&["T"], Vec::new()))),
            target.scope.clone(),
        );
        let instantiated = scheme
            .projected_instantiate_forall(&argument)
            .expect("an exact same-store open argument must instantiate its forall");
        assert!(matches!(
            instantiated.ty().as_type(),
            Type::Goal { goal, .. } if *goal == open_goal
        ));
    }

    #[test]
    fn scoped_specialization_rehydrates_one_open_head_across_distinct_arguments() {
        let a = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };
        let b = TypeParam {
            name: "B".to_owned(),
            span: sp(),
            kind: None,
        };
        let mut store = GoalStore::new();
        let owner = owner(
            &mut store,
            RigidScope::new()
                .with_lexical_forall(&a)
                .with_lexical_forall(&b),
        );
        let open_goal = goal(&mut store, owner, Kind::arrow_chain(1));
        let outer_scope = store.owner_scope(owner, sp()).unwrap();
        let flexible = TypeParam {
            name: "F".to_owned(),
            span: sp(),
            kind: Some(Kind::arrow_chain(1)),
        };
        let pattern = ScopedType::new(
            InternedType::fresh_canonical(product(
                path(&["F"], vec![path(&["A"], Vec::new())]),
                path(&["F"], vec![path(&["B"], Vec::new())]),
            )),
            outer_scope.clone().with_lexical_forall(&flexible),
        );
        let target = ScopedType::new(
            InternedType::fresh_canonical(product(
                Type::Goal {
                    goal: open_goal,
                    args: vec![path(&["A"], Vec::new())],
                    meta: Meta::new(sp()),
                    ext: (),
                },
                Type::Goal {
                    goal: open_goal,
                    args: vec![path(&["B"], Vec::new())],
                    meta: Meta::new(sp()),
                    ext: (),
                },
            )),
            outer_scope.clone(),
        );
        let context = IsolatedSpecializationContext {
            nominal_head_kinds: HashMap::new(),
            opaque_goal_kinds: HashMap::from([(open_goal, Kind::arrow_chain(1))]),
        };

        let IsolatedSpecialization::Matched(mut arguments) = solve_scoped_specialization_by_name(
            std::slice::from_ref(&flexible),
            &pattern,
            &target,
            sp(),
            &context,
        )
        .expect("one open head may be observed at two exact rigid arguments") else {
            panic!("two applications of one open head were treated as unrelated constants")
        };
        let argument = arguments.pop().expect("one forall produced one argument");
        let expected = ScopedType::new(
            InternedType::fresh_canonical(goal_type(open_goal)),
            outer_scope,
        );
        assert_eq!(
            argument.transient_key(),
            expected.transient_key(),
            "the inferred higher-kinded argument must rehydrate the exact foreign goal head"
        );
    }

    #[test]
    fn scoped_specialization_does_not_hoist_an_applied_goal_out_of_forall() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let open_goal = goal(&mut store, owner, Kind::arrow_chain(1));
        let outer_scope = store.owner_scope(owner, sp()).unwrap();
        let flexible = TypeParam {
            name: "T".to_owned(),
            span: sp(),
            kind: None,
        };
        let pattern = ScopedType::new(
            InternedType::fresh_canonical(forall("A", path(&["T"], Vec::new()))),
            outer_scope.clone().with_lexical_forall(&flexible),
        );
        let target = ScopedType::new(
            InternedType::fresh_canonical(forall(
                "A",
                Type::Goal {
                    goal: open_goal,
                    args: vec![path(&["A"], Vec::new())],
                    meta: Meta::new(sp()),
                    ext: (),
                },
            )),
            outer_scope,
        );
        let context = IsolatedSpecializationContext {
            nominal_head_kinds: HashMap::new(),
            opaque_goal_kinds: HashMap::from([(open_goal, Kind::arrow_chain(1))]),
        };

        let outcome = solve_scoped_specialization_by_name(
            std::slice::from_ref(&flexible),
            &pattern,
            &target,
            sp(),
            &context,
        );
        assert!(
            !matches!(outcome, Ok(IsolatedSpecialization::Matched(_))),
            "an inferred argument cannot retain the target forall's local rigid"
        );
    }

    #[test]
    fn projected_public_forall_prefix_retains_noncanonical_rigid_identity() {
        let scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 7)]);
        let argument =
            ScopedType::new(InternedType::fresh(path(&["A"], Vec::new())), scope.clone());
        let scheme = ScopedType::new(
            InternedType::fresh_canonical(forall("T", path(&["T"], Vec::new()))),
            scope.clone(),
        );
        let applied = scheme
            .projected_public_forall_prefix_application(std::slice::from_ref(&argument))
            .expect("an exact ambient rigid may instantiate a retained public forall");
        assert_eq!(applied.scope.binding("A"), scope.binding("A"));
        assert!(matches!(
            applied.ty().as_type(),
            Type::Path { segments, .. } if segments[0].as_str() == "A"
        ));
        assert!(
            !applied.ty().identity_is_canonical(),
            "substituting an exact rigid must not mark it canonical"
        );

        let unused_scheme = ScopedType::new(
            InternedType::fresh_canonical(forall("T", unit())),
            scope.clone(),
        );
        let unused = unused_scheme
            .projected_public_forall_prefix_application(std::slice::from_ref(&argument))
            .expect("an unused exact argument still has compatible scope");
        assert!(
            unused.ty().identity_is_canonical(),
            "an unused noncanonical argument must not taint a canonical result"
        );

        let capture_scheme = ScopedType::new(
            InternedType::fresh_canonical(forall(
                "T",
                forall(
                    "A",
                    product(path(&["T"], Vec::new()), path(&["A"], Vec::new())),
                ),
            )),
            scope.clone(),
        );
        let captured = capture_scheme
            .projected_public_forall_prefix_application(&[
                argument.clone(),
                ScopedType::new(InternedType::fresh_canonical(unit()), scope.clone()),
            ])
            .expect("a use-site rigid is not captured by a later same-spelled forall");
        let Type::Product { left, right, .. } = captured.ty().as_type() else {
            panic!("the captured prefix lost its result product")
        };
        assert!(
            matches!(left.as_ref(), Type::Path { segments, .. } if segments[0].as_str() == "A")
        );
        assert!(matches!(right.as_ref(), Type::Unit { .. }));
        assert_eq!(captured.scope.binding("A"), scope.binding("A"));

        let shadowed = ScopedType::new(
            InternedType::fresh(path(&["A"], Vec::new())),
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 8)]),
        );
        assert!(
            scheme
                .projected_public_forall_prefix_application(std::slice::from_ref(&shadowed))
                .is_none(),
            "a same-spelled rigid from an incompatible scope must fail closed"
        );
    }

    #[test]
    fn projected_public_forall_prefix_rebuilds_a_sixty_four_binder_tail_once() {
        const BINDERS: usize = 64;
        let scope = RigidScope::new();
        let mut ty = unit();
        for index in (0..BINDERS).rev() {
            ty = product(path(&[&format!("T{index}")], Vec::new()), ty);
        }
        for index in (0..BINDERS).rev() {
            ty = forall(&format!("T{index}"), ty);
        }
        let scheme = ScopedType::new(InternedType::fresh_canonical(ty), scope.clone());
        let arguments = (0..BINDERS)
            .map(|_| ScopedType::new(InternedType::fresh_canonical(unit()), scope.clone()))
            .collect::<Vec<_>>();

        reset_projected_public_forall_body_rebuilds();
        reset_projected_scope_type_walks();
        let applied = scheme
            .projected_public_forall_prefix_application(&arguments)
            .expect("the complete written forall prefix applies");
        assert_eq!(projected_public_forall_body_rebuilds(), 1);
        assert_eq!(
            projected_scope_type_walks(),
            0,
            "one exact persistent scope history must bypass every contribution/proof walk"
        );

        let mut cursor = applied.ty().as_type();
        for _ in 0..BINDERS {
            let Type::Product { left, right, .. } = cursor else {
                panic!("the written prefix lost a substituted product component")
            };
            assert!(matches!(left.as_ref(), Type::Unit { .. }));
            cursor = right;
        }
        assert!(matches!(cursor, Type::Unit { .. }));

        let scheme_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let argument_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        assert!(!scheme_scope.shares_history_with(&argument_scope));
        assert_eq!(scheme_scope, argument_scope);
        let distinct_scheme = ScopedType::new(scheme.ty().clone(), scheme_scope);
        let distinct_arguments = (0..BINDERS)
            .map(|_| {
                ScopedType::new(
                    InternedType::fresh_canonical(unit()),
                    argument_scope.clone(),
                )
            })
            .collect::<Vec<_>>();
        reset_projected_scope_type_walks();
        distinct_scheme
            .projected_public_forall_prefix_application(&distinct_arguments)
            .expect("visible-equal scopes with distinct histories use the checked slow path");
        assert_eq!(
            projected_scope_type_walks(),
            2 * (BINDERS + 1),
            "the checked comparator walks each scheme/argument exactly once for contribution and once for free-use proof"
        );
    }

    #[test]
    fn reconstructed_lexical_foralls_preserve_embedded_goal_authority() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let open_goal = goal(&mut store, owner, Kind::Star);
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };
        let reconstructed =
            InternedType::fresh_canonical(function(path(&["A"], Vec::new()), goal_type(open_goal)));
        let scoped = store
            .scoped_type_with_lexical_foralls(
                owner,
                reconstructed.clone(),
                std::slice::from_ref(&param),
                sp(),
            )
            .expect("the owner-authorized goal may appear beneath a fresh lexical proof");
        assert_eq!(scoped.ty(), &reconstructed);
        assert_eq!(scoped.scope.kind("A"), Some(Kind::Star));
        assert!(matches!(
            scoped.ty().as_type(),
            Type::Function { ret, .. }
                if matches!(ret.as_ref(), Type::Goal { goal, .. } if *goal == open_goal)
        ));
    }

    #[test]
    fn transient_scoped_key_retains_exact_shadow_identity() {
        let left = ScopedType::new(
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]),
        );
        let right = ScopedType::new(
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 2)]),
        );
        assert_ne!(left.transient_key(), right.transient_key());
        assert_eq!(left.transient_key(), left.clone().transient_key());
    }

    #[test]
    fn projected_structural_child_preserves_and_extends_exact_scope() {
        let outer_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 7)]);
        let product = ScopedType::new(
            InternedType::fresh_canonical(product(
                path(&["A"], Vec::new()),
                path(&["m", "I32"], Vec::new()),
            )),
            outer_scope.clone(),
        );
        let left = product
            .projected_structural_child(ScopedTypeEdge::ProductLeft)
            .expect("a product exposes its left structural child");
        assert_eq!(left.scope, outer_scope);
        assert_eq!(left.projected_rigid_path_arity(), Some(0));
        assert_eq!(
            product
                .projected_structural_child(ScopedTypeEdge::ProductRight)
                .unwrap()
                .projected_rigid_path_arity(),
            None,
            "a canonical nominal path must not be reported as a rigid variable"
        );
        assert!(
            product
                .projected_structural_child(ScopedTypeEdge::FunctionParam)
                .is_none(),
            "a structural edge must fail closed on the wrong parent shape"
        );
        let sum = ScopedType::new(
            InternedType::fresh_canonical(Type::Sum {
                left: Box::new(path(&["A"], Vec::new())),
                right: Box::new(unit()),
                meta: Meta::new(sp()),
            }),
            outer_scope.clone(),
        );
        assert_eq!(
            sum.projected_structural_child(ScopedTypeEdge::SumLeft)
                .unwrap()
                .scope,
            outer_scope
        );
        assert!(matches!(
            sum.projected_structural_child(ScopedTypeEdge::SumRight)
                .unwrap()
                .ty()
                .as_type(),
            Type::Unit { .. }
        ));

        let applied = ScopedType::new(
            InternedType::fresh_canonical(path(
                &["m", "F"],
                vec![path(&["A"], Vec::new()), unit()],
            )),
            outer_scope.clone(),
        );
        assert_eq!(
            applied
                .projected_structural_child(ScopedTypeEdge::PathArgument(0))
                .unwrap()
                .scope,
            outer_scope
        );
        assert!(matches!(
            applied
                .projected_structural_child(ScopedTypeEdge::PathArgument(1))
                .unwrap()
                .ty()
                .as_type(),
            Type::Unit { .. }
        ));
        assert!(
            applied
                .projected_structural_child(ScopedTypeEdge::PathArgument(2))
                .is_none(),
            "a path-argument edge must fail closed outside the argument list"
        );

        let unit_peer = ScopedType::new(InternedType::fresh_canonical(unit()), outer_scope.clone());
        let rebuilt_sum = left
            .projected_binary_peer(&unit_peer, ScopedTypeBinaryKind::Sum, sp())
            .expect("compatible exact scopes can form a projected sum peer");
        assert_eq!(rebuilt_sum.scope, outer_scope);
        assert!(matches!(rebuilt_sum.ty().as_type(), Type::Sum { .. }));
        let rebuilt_function = left
            .projected_binary_peer(
                &unit_peer,
                ScopedTypeBinaryKind::Function { abi_arity: 1 },
                sp(),
            )
            .expect("compatible exact scopes can form a projected function peer");
        assert!(matches!(
            rebuilt_function.ty().as_type(),
            Type::Function { abi_arity: 1, .. }
        ));
        let noncanonical = ScopedType::new(InternedType::fresh(unit()), outer_scope.clone());
        let rebuilt_product = left
            .projected_binary_peer(&noncanonical, ScopedTypeBinaryKind::Product, sp())
            .expect("a noncanonical operand can retain its exact identity frontier");
        assert!(
            !rebuilt_product.ty().identity_is_canonical(),
            "a projected constructor must not launder a noncanonical operand"
        );
        let shadowed = ScopedType::new(
            InternedType::fresh_canonical(unit()),
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 8)]),
        );
        assert!(
            left.projected_binary_peer(&shadowed, ScopedTypeBinaryKind::Product, sp())
                .is_none(),
            "incompatible shadow identities must fail closed"
        );

        let head = ScopedType::new(
            InternedType::fresh_canonical(path(&["m", "F"], Vec::new())),
            outer_scope.clone(),
        );
        let rebuilt_application = head
            .projected_path_apply(&left)
            .expect("compatible exact scopes can form a projected type application");
        let Type::Path { args, .. } = rebuilt_application.ty().as_type() else {
            panic!("a projected path application must remain a path")
        };
        assert_eq!(args.len(), 1);
        assert_eq!(rebuilt_application.scope, outer_scope);
        assert!(head.projected_path_apply(&shadowed).is_none());
        assert!(unit_peer.projected_path_apply(&left).is_none());

        let abstracted = left
            .projected_forall_abstract(
                TypeParam {
                    name: "B".to_owned(),
                    span: sp(),
                    kind: None,
                },
                sp(),
            )
            .expect("a fresh generated binder can abstract an exact projected body");
        assert_eq!(abstracted.scope, outer_scope);
        let abstracted_body = abstracted
            .projected_structural_child(ScopedTypeEdge::ForallBody)
            .expect("a projected abstraction must expose its lexical body proof");
        assert_eq!(abstracted_body.scope.binding("A"), outer_scope.binding("A"));
        assert!(abstracted_body.scope.binding("B").is_some());
        assert!(
            left.projected_forall_abstract(
                TypeParam {
                    name: "A".to_owned(),
                    span: sp(),
                    kind: None,
                },
                sp(),
            )
            .is_none(),
            "a generated binder must not capture an existing exact rigid"
        );

        let forall = ScopedType::new(
            InternedType::fresh_canonical(forall("A", path(&["A"], Vec::new()))),
            outer_scope.clone(),
        );
        let body = forall
            .projected_structural_child(ScopedTypeEdge::ForallBody)
            .expect("a forall exposes its body with the lexical binder proof");
        assert_ne!(body.scope.binding("A"), outer_scope.binding("A"));
        assert_eq!(
            body.scope.binding("A").map(|binding| &binding.identity),
            Some(&RigidIdentity::Alpha("A".to_owned()))
        );
    }

    #[test]
    fn projected_composition_preserves_used_rigids_across_an_irrelevant_shadow() {
        let outer_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let shadow_scope = outer_scope.clone().with_binding("A", Kind::Star, 2);
        let outer_a = ScopedType::new(
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
            outer_scope.clone(),
        );
        let shadow_unit =
            ScopedType::new(InternedType::fresh_canonical(unit()), shadow_scope.clone());
        let shadow_a = ScopedType::new(
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
            shadow_scope,
        );

        let pair = outer_a
            .projected_binary_peer(&shadow_unit, ScopedTypeBinaryKind::Product, sp())
            .expect("an irrelevant descendant shadow must not poison a projected pair");
        assert_eq!(pair.scope.binding("A"), outer_scope.binding("A"));
        assert!(
            outer_a
                .projected_binary_peer(&shadow_a, ScopedTypeBinaryKind::Product, sp())
                .is_none(),
            "two actual uses of different same-spelled rigids must fail closed"
        );

        let unbound_a = ScopedType::new(
            InternedType::fresh(path(&["A"], Vec::new())),
            RigidScope::new(),
        );
        let bound_unit =
            ScopedType::new(InternedType::fresh_canonical(unit()), outer_scope.clone());
        let nominal_pair = unbound_a
            .projected_binary_peer(&bound_unit, ScopedTypeBinaryKind::Product, sp())
            .expect("an irrelevant rigid proof must not capture an unbound nominal head");
        assert_eq!(nominal_pair.scope.binding("A"), None);
        assert!(
            unbound_a
                .projected_binary_peer(&outer_a, ScopedTypeBinaryKind::Product, sp())
                .is_none(),
            "an unbound head and an actual same-spelled rigid use cannot share one scope"
        );

        let head = ScopedType::new(
            InternedType::fresh_canonical(path(&["m", "F"], vec![path(&["A"], Vec::new())])),
            outer_scope.clone(),
        );
        let applied = head
            .projected_path_apply(&shadow_unit)
            .expect("an irrelevant descendant shadow must not poison a path application");
        assert_eq!(applied.scope.binding("A"), outer_scope.binding("A"));
        assert!(head.projected_path_apply(&shadow_a).is_none());

        let scheme = ScopedType::new(
            InternedType::fresh_canonical(forall(
                "T",
                product(path(&["A"], Vec::new()), path(&["T"], Vec::new())),
            )),
            outer_scope.clone(),
        );
        let instantiated = scheme
            .projected_instantiate_forall(&shadow_unit)
            .expect("a binder-independent argument scope must not shadow a surviving rigid");
        assert_eq!(instantiated.scope.binding("A"), outer_scope.binding("A"));
        assert!(scheme.projected_instantiate_forall(&shadow_a).is_none());

        let prefixed = scheme
            .projected_public_forall_prefix_application(std::slice::from_ref(&shadow_unit))
            .expect("the batched prefix must derive authority from effective free uses");
        assert_eq!(prefixed.scope.binding("A"), outer_scope.binding("A"));
        assert!(
            scheme
                .projected_public_forall_prefix_application(std::slice::from_ref(&shadow_a))
                .is_none()
        );

        let unused_scheme = ScopedType::new(
            InternedType::fresh_canonical(forall("T", path(&["A"], Vec::new()))),
            outer_scope.clone(),
        );
        let singly_unused = unused_scheme
            .projected_instantiate_forall(&shadow_unit)
            .expect("an unused descendant Unit contributes no rigid proof");
        assert!(matches!(
            singly_unused.ty().as_type(),
            Type::Path { segments, .. } if segments[0].as_str() == "A"
        ));
        assert_eq!(singly_unused.scope.binding("A"), outer_scope.binding("A"));
        let unused = unused_scheme
            .projected_public_forall_prefix_application(std::slice::from_ref(&shadow_unit))
            .expect("an authenticated binder-independent unused argument contributes no proof");
        assert_eq!(unused.scope.binding("A"), outer_scope.binding("A"));

        let sibling_unit = ScopedType::new(
            InternedType::fresh_canonical(unit()),
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 3)]),
        );
        assert!(
            unused_scheme
                .projected_instantiate_forall(&sibling_unit)
                .is_none(),
            "an unused argument must still pass the original full-scope authentication"
        );
        assert!(
            unused_scheme
                .projected_public_forall_prefix_application(std::slice::from_ref(&sibling_unit))
                .is_none(),
            "a batched unused argument must not bypass full-scope authentication"
        );
        assert!(
            unused_scheme
                .projected_instantiate_forall(&shadow_a)
                .is_none(),
            "an unused descendant rigid argument must still preserve its exact free use"
        );
        assert!(
            unused_scheme
                .projected_public_forall_prefix_application(std::slice::from_ref(&shadow_a))
                .is_none(),
            "a batched unused descendant rigid argument must still fail closed"
        );

        let repeated_scheme = ScopedType::new(
            InternedType::fresh_canonical(forall("T", forall("T", path(&["T"], Vec::new())))),
            outer_scope.clone(),
        );
        assert!(
            repeated_scheme
                .projected_public_forall_prefix_application(&[shadow_a.clone(), outer_a.clone(),])
                .is_none(),
            "an overwritten argument still participates in exact-scope authentication"
        );
        let repeated = repeated_scheme
            .projected_public_forall_prefix_application(&[shadow_unit, outer_a.clone()])
            .expect("only the innermost repeated binder substitution is effective");
        assert!(matches!(
            repeated.ty().as_type(),
            Type::Path { segments, .. } if segments[0].as_str() == "A"
        ));
        assert_eq!(repeated.scope.binding("A"), outer_scope.binding("A"));
    }

    #[test]
    fn projected_composition_retains_goal_owner_scope_through_a_shadow_chain() {
        let mut store = GoalStore::new();
        let parent = owner(
            &mut store,
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]),
        );
        let parent_scope = store.owner_scope(parent, sp()).unwrap().clone();
        let child_scope = parent_scope.clone().with_binding("A", Kind::Star, 2);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let parent_goal = goal(&mut store, parent, Kind::Star);
        let child_goal = goal(&mut store, child, Kind::Star);
        let parent_value = scoped_canonical(&store, parent, goal_type(parent_goal));
        let child_value = scoped_canonical(&store, child, goal_type(child_goal));

        let combined = parent_value
            .projected_binary_peer(&child_value, ScopedTypeBinaryKind::Product, sp())
            .expect("ancestor and descendant goal authority scopes may compose");
        store
            .require_scoped_goal_chain(child, &combined, sp())
            .expect("the combined occurrence must retain both goal owner scopes");

        let child_scope = store.owner_scope(child, sp()).unwrap().clone();
        let sparse_child_rigid = store.scoped_type_at_lexical_prefix(
            &child_scope,
            InternedType::fresh_canonical(path(&["A"], Vec::new())),
        );
        assert!(
            sparse_child_rigid
                .scope
                .bindings
                .frame("A")
                .unwrap()
                .shadowed()
                .is_none(),
            "the goal-free child occurrence is intentionally sparse"
        );
        let sparse_first = sparse_child_rigid
            .projected_binary_peer(&combined, ScopedTypeBinaryKind::Product, sp())
            .expect("sparse evidence must compose with same-visible full goal authority");
        let retained = sparse_first.scope.bindings.frame("A").unwrap();
        assert_eq!(retained.value(), child_scope.binding("A").unwrap());
        assert_eq!(
            retained.shadowed().map(|frame| frame.value()),
            Some(parent_scope.binding("A").unwrap()),
            "the goal-bearing contribution must retain its complete shadow chain"
        );
        store
            .require_scoped_goal_chain(child, &sparse_first, sp())
            .expect("sparse-left composition must preserve embedded goal authority");

        let parent_rigid = scoped_canonical(&store, parent, path(&["A"], Vec::new()));
        assert!(
            parent_rigid
                .projected_binary_peer(&child_value, ScopedTypeBinaryKind::Product, sp())
                .is_none(),
            "a descendant goal scope must not capture a parent rigid occurrence"
        );
    }

    #[test]
    fn projected_reflection_candidate_closes_only_a_goal_free_exact_child() {
        let mut store = GoalStore::new();
        let scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 7)])
            .with_test_context(1, "consumer");
        let owner = owner(&mut store, scope);
        let open_return = goal(&mut store, owner, Kind::Star);
        let function = store
            .scoped_type(
                owner,
                InternedType::fresh(function(path(&["A"], Vec::new()), goal_type(open_return))),
                sp(),
            )
            .expect("scoped projected function");

        assert!(
            function.projected_reflection_candidate().is_none(),
            "an open parent must not become reflected evidence"
        );
        let param = function
            .projected_structural_child(ScopedTypeEdge::FunctionParam)
            .expect("function parameter child");
        let candidate = param
            .projected_reflection_candidate()
            .expect("the exact goal-free parameter may become a reflection candidate");
        assert!(!candidate.identity_is_canonical);
        assert_eq!(candidate.module_path, "consumer");
        assert_eq!(candidate.binder_kinds.get("A"), Some(&Kind::Star));
        assert!(matches!(
            candidate.ty,
            Type::Path { ref segments, .. } if segments.len() == 1 && segments[0].as_str() == "A"
        ));
        assert!(
            function
                .projected_structural_child(ScopedTypeEdge::FunctionReturn)
                .expect("function return child")
                .projected_reflection_candidate()
                .is_none(),
            "an open sibling must remain unreflectable"
        );

        let shadowed = ScopedType::new(
            InternedType::fresh(path(&["A"], Vec::new())),
            RigidScope::from_bindings([
                ("A".to_owned(), Kind::Star, 10),
                ("A".to_owned(), Kind::arrow_chain(1), 11),
            ])
            .with_test_context(1, "consumer"),
        );
        let shadowed_candidate = shadowed
            .projected_reflection_candidate()
            .expect("a goal-free exact shadow remains reflectable");
        assert_eq!(shadowed_candidate.binder_kinds.len(), 1);
        assert_eq!(
            shadowed_candidate.binder_kinds.get("A"),
            Some(&Kind::arrow_chain(1)),
            "reflection must retain only the exact visible shadow binding"
        );
    }

    #[test]
    fn failed_equation_rolls_back_goal_policy_meets_and_writes() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let left = goal(&mut store, owner, Kind::Star);
        let right = store
            .alloc_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();

        // Establish that the successful prefix really exercises both journal
        // channels. The failing product equation below repeats this exact
        // prefix before reaching its mismatching second member.
        let mut prefix_delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut prefix_delta,
                scoped_canonical(&store, owner, goal_type(left)),
                scoped_canonical(&store, owner, goal_type(right)),
                sp(),
                &context,
            )
            .unwrap();
        assert!(
            !prefix_delta.writes.is_empty(),
            "the rollback witness did not exercise a goal write"
        );
        assert!(
            !prefix_delta.policy_meets.is_empty(),
            "the rollback witness did not exercise a policy meet"
        );

        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let found = scoped_canonical(
            &store,
            owner,
            product(goal_type(left), path(&["m", "String"], Vec::new())),
        );
        let expected = scoped_canonical(
            &store,
            owner,
            product(goal_type(right), path(&["m", "Bool"], Vec::new())),
        );
        let error = store
            .constrain(&mut delta, found, expected, sp(), &context)
            .expect_err("the second product member must reject the equation");

        assert!(error.diag().1.contains("type mismatch"));
        assert!(
            delta.writes.is_empty(),
            "the failed equation leaked a write"
        );
        assert!(
            delta.policy_meets.is_empty(),
            "the failed equation leaked a policy meet"
        );
    }

    #[test]
    fn cross_owner_goal_equations_always_bind_descendant_to_ancestor() {
        for reverse in [false, true] {
            let context = ctx();
            let mut store = GoalStore::new();
            let ancestor_owner = owner(&mut store, RigidScope::new());
            let ancestor_goal = goal(&mut store, ancestor_owner, Kind::Star);
            let descendant_owner = store
                .begin_owner(
                    Some(ancestor_owner),
                    RigidScope::new(),
                    GoalOwnerKind::RetainedValue,
                    sp(),
                )
                .unwrap();
            let descendant_goal = goal(&mut store, descendant_owner, Kind::Star);
            let mut delta = store.begin_delta(descendant_owner, sp()).unwrap();
            let ancestor = scoped_canonical(&store, descendant_owner, goal_type(ancestor_goal));
            let descendant = scoped_canonical(&store, descendant_owner, goal_type(descendant_goal));
            let (found, expected) = if reverse {
                (ancestor, descendant)
            } else {
                (descendant, ancestor)
            };

            store
                .constrain(&mut delta, found, expected, sp(), &context)
                .expect("cross-owner goal equality should be orientable");
            assert!(
                !delta.writes.contains_key(&ancestor_goal),
                "an equation orientation must never bind an ancestor to a descendant"
            );
            let binding = delta
                .writes
                .get(&descendant_goal)
                .expect("the descendant goal should be the union representative");
            assert!(matches!(
                binding.value.ty().as_type(),
                Type::Goal { goal, .. } if *goal == ancestor_goal
            ));
        }
    }

    #[test]
    fn retained_leaf_annotation_goal_solves_before_close() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let leaf = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let annotation = goal(&mut store, leaf, Kind::Star);
        let mut delta = store.begin_delta(leaf, sp()).unwrap();
        let expected = scoped_canonical(&store, leaf, goal_type(annotation));
        assert_eq!(
            store
                .try_constrain_retained_leaf_equation(
                    &mut delta,
                    scoped_canonical(&store, leaf, path(&["m", "I32"], Vec::new())),
                    expected.clone(),
                    sp(),
                    &context,
                )
                .unwrap(),
            CloseReadyEquationConstraint::Applied,
        );
        let mut outputs = store
            .close_owner(
                delta,
                vec![(expected, GoalEscape::ClosedAt(root))],
                &context,
            )
            .expect("the annotation hole must close with its leaf")
            .into_outputs();
        let closed = into_goal_free(outputs.remove(0));
        assert!(matches!(closed.ty().as_type(), Type::Path { segments, .. }
            if segments.last().is_some_and(|segment| segment.as_str() == "I32")));
    }

    #[test]
    fn retained_leaf_annotation_mismatch_preserves_prior_equation() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let leaf = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let prior = goal(&mut store, leaf, Kind::Star);
        let left = goal(&mut store, leaf, Kind::Star);
        let right = store
            .alloc_goal(
                leaf,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "annotation"),
            )
            .unwrap();
        let mut prefix = store.begin_delta(leaf, sp()).unwrap();
        store
            .try_constrain_retained_leaf_equation(
                &mut prefix,
                scoped_canonical(&store, leaf, goal_type(left)),
                scoped_canonical(&store, leaf, goal_type(right)),
                sp(),
                &context,
            )
            .unwrap();
        assert!(!prefix.writes.is_empty() && !prefix.policy_meets.is_empty());

        let mut delta = store.begin_delta(leaf, sp()).unwrap();
        store
            .try_constrain_retained_leaf_equation(
                &mut delta,
                scoped_canonical(&store, leaf, goal_type(prior)),
                scoped_canonical(&store, leaf, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .unwrap();
        let error = store
            .try_constrain_retained_leaf_equation(
                &mut delta,
                scoped_canonical(
                    &store,
                    leaf,
                    product(goal_type(left), path(&["m", "String"], Vec::new())),
                ),
                scoped_canonical(
                    &store,
                    leaf,
                    product(goal_type(right), path(&["m", "Bool"], Vec::new())),
                ),
                sp(),
                &context,
            )
            .expect_err("the second product member must reject without publishing its prefix");
        assert!(error.diag().1.contains("type mismatch"));
        assert_eq!(delta.writes.len(), 1);
        assert!(delta.writes.contains_key(&prior));
        assert!(delta.policy_meets.is_empty());
    }

    #[test]
    fn retained_leaf_annotation_cannot_escape_in_ancestor_composite() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let ancestor = goal(&mut store, root, Kind::Star);
        let leaf = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let annotation = goal(&mut store, leaf, Kind::Star);
        let mut delta = store.begin_delta(leaf, sp()).unwrap();
        let found = scoped_canonical(
            &store,
            leaf,
            path(&["m", "Box"], vec![goal_type(annotation)]),
        );
        let expected = scoped_canonical(&store, leaf, goal_type(ancestor));
        assert_eq!(
            store
                .try_constrain_retained_leaf_equation(
                    &mut delta,
                    found.clone(),
                    expected.clone(),
                    sp(),
                    &context,
                )
                .unwrap(),
            CloseReadyEquationConstraint::Deferred,
        );
        assert!(delta.writes.is_empty() && delta.policy_meets.is_empty());
        store
            .try_constrain_retained_leaf_equation(
                &mut delta,
                scoped_canonical(&store, leaf, goal_type(annotation)),
                scoped_canonical(&store, leaf, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .unwrap();
        assert_eq!(
            store
                .try_constrain_retained_leaf_equation(&mut delta, found, expected, sp(), &context,)
                .unwrap(),
            CloseReadyEquationConstraint::Applied,
        );
        store
            .close_owner(delta, Vec::new(), &context)
            .expect("close the resolved leaf");
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let output = scoped_canonical(&store, root, goal_type(ancestor));
        let mut outputs = store
            .close_owner(
                root_delta,
                vec![(output, GoalEscape::ClosedAt(root))],
                &context,
            )
            .expect("the ancestor receives only the solved composite")
            .into_outputs();
        let closed = into_goal_free(outputs.remove(0));
        assert!(matches!(closed.ty().as_type(), Type::Path { args, .. }
            if matches!(args.as_slice(), [Type::Path { segments, .. }]
                if segments.last().is_some_and(|segment| segment.as_str() == "I32"))));
    }

    #[test]
    fn retained_leaf_equation_retries_after_sibling_gather_solves_descendant() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let ancestor = goal(&mut store, root, Kind::Star);
        let application = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let descendant = goal(&mut store, application, Kind::Star);
        let leaf = store
            .begin_owner(
                Some(application),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut leaf_delta = store.begin_delta(leaf, sp()).unwrap();
        let found = scoped_canonical(
            &store,
            leaf,
            path(&["m", "Box"], vec![goal_type(descendant)]),
        );
        let expected = scoped_canonical(&store, leaf, goal_type(ancestor));

        assert_eq!(
            store
                .try_constrain_retained_leaf_equation(
                    &mut leaf_delta,
                    found.clone(),
                    expected.clone(),
                    sp(),
                    &context,
                )
                .expect("the unsafe retained-leaf equation should defer"),
            CloseReadyEquationConstraint::Deferred
        );
        assert!(
            leaf_delta.writes.is_empty() && leaf_delta.policy_meets.is_empty(),
            "retained-leaf deferral must roll back its equation"
        );
        let unresolved = store
            .require_goal_free_after_delta(&leaf_delta, found.clone(), &context)
            .expect_err("a final retry without causal progress must remain an input error");
        assert!(
            unresolved.diag().1.contains("cannot infer type argument"),
            "unexpected unresolved retained-leaf diagnostic: {}",
            unresolved.diag().1
        );

        let gather = store
            .begin_owner(
                Some(application),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();
        let mut gather_delta = store.begin_delta(gather, sp()).unwrap();
        assert_eq!(
            store
                .try_constrain_gather_equation(
                    &mut gather_delta,
                    scoped_canonical(&store, gather, goal_type(descendant)),
                    scoped_canonical(&store, gather, path(&["m", "I32"], Vec::new())),
                    sp(),
                    &context,
                )
                .expect("the sibling gather should solve the descendant"),
            CloseReadyEquationConstraint::Applied
        );
        store
            .close_owner(gather_delta, Vec::new(), &context)
            .expect("close the sibling gather");

        assert_eq!(
            store
                .try_constrain_retained_leaf_equation(
                    &mut leaf_delta,
                    found,
                    expected,
                    sp(),
                    &context,
                )
                .expect("the solved descendant should release the retained leaf"),
            CloseReadyEquationConstraint::Applied
        );
        store
            .close_owner(leaf_delta, Vec::new(), &context)
            .expect("close the retained leaf");
        let application_delta = store.begin_delta(application, sp()).unwrap();
        store
            .close_owner(application_delta, Vec::new(), &context)
            .expect("close the nested application");
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let output = scoped_canonical(&store, root, goal_type(ancestor));
        let mut outputs = store
            .close_owner(
                root_delta,
                vec![(output, GoalEscape::ClosedAt(root))],
                &context,
            )
            .expect("close the root application")
            .into_outputs();
        let closed = into_goal_free(outputs.remove(0));
        assert!(matches!(
            closed.ty().as_type(),
            Type::Path { segments, args, .. }
                if segments.last().is_some_and(|segment| segment.as_str() == "Box")
                    && matches!(args.as_slice(), [Type::Path { segments, .. }]
                        if segments.last().is_some_and(|segment| segment.as_str() == "I32"))
        ));
    }

    #[test]
    fn atomic_relation_group_solves_common_endpoints_in_either_physical_order() {
        for reverse in [false, true] {
            let context = ctx();
            let mut store = GoalStore::new();
            let root = owner(&mut store, RigidScope::new());
            let left = goal(&mut store, root, Kind::Star);
            let right = goal(&mut store, root, Kind::Star);
            let mut equations = vec![
                (
                    scoped_canonical(&store, root, goal_type(right)),
                    scoped_canonical(&store, root, goal_type(left)),
                    sp(),
                ),
                (
                    scoped_canonical(&store, root, unit()),
                    scoped_canonical(&store, root, goal_type(right)),
                    sp(),
                ),
            ];
            if reverse {
                equations.reverse();
            }
            let mut delta = store.begin_delta(root, sp()).unwrap();
            store
                .constrain_equations_atomically(&mut delta, equations, &context)
                .expect("a compatible relation group must not depend on physical order");
            for endpoint in [left, right] {
                let closed = store
                    .require_goal_free_after_delta(
                        &delta,
                        scoped_canonical(&store, root, goal_type(endpoint)),
                        &context,
                    )
                    .expect("the common concrete endpoint must close the relation group");
                assert!(matches!(closed.ty().as_type(), Type::Unit { .. }));
            }
        }
    }

    #[test]
    fn atomic_relation_contradiction_restores_the_complete_delta_prefix() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let left = goal(&mut store, root, Kind::Star);
        let right = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let prefix = goal(&mut store, root, Kind::Star);
        let reusable = goal(&mut store, root, Kind::Star);
        let mut delta = store.begin_delta(root, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, root, unit()),
                scoped_canonical(&store, root, goal_type(prefix)),
                sp(),
                &context,
            )
            .expect("seed a delta prefix outside the relation group");
        let prefix_write_keys = delta.writes.keys().copied().collect::<Vec<_>>();
        let prefix_meet_keys = delta.policy_meets.keys().copied().collect::<Vec<_>>();

        let error = store
            .constrain_equations_atomically(
                &mut delta,
                [
                    (
                        scoped_canonical(&store, root, goal_type(left)),
                        scoped_canonical(&store, root, goal_type(right)),
                        sp(),
                    ),
                    (
                        scoped_canonical(&store, root, unit()),
                        scoped_canonical(&store, root, goal_type(left)),
                        sp(),
                    ),
                    (
                        scoped_canonical(&store, root, path(&["m", "String"], Vec::new())),
                        scoped_canonical(&store, root, goal_type(right)),
                        sp(),
                    ),
                ],
                &context,
            )
            .expect_err("incompatible concrete endpoints must reject the relation group");
        assert!(error.diag().1.contains("type mismatch"));
        assert_eq!(
            delta.writes.keys().copied().collect::<Vec<_>>(),
            prefix_write_keys
        );
        assert_eq!(
            delta.policy_meets.keys().copied().collect::<Vec<_>>(),
            prefix_meet_keys
        );
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, root, unit()),
                scoped_canonical(&store, root, goal_type(reusable)),
                sp(),
                &context,
            )
            .expect("a rolled-back relation group must leave the delta reusable");
    }

    #[test]
    fn atomic_relation_with_two_open_endpoints_remains_underdetermined() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let left = goal(&mut store, root, Kind::Star);
        let right = goal(&mut store, root, Kind::Star);
        let mut delta = store.begin_delta(root, sp()).unwrap();
        store
            .constrain_equations_atomically(
                &mut delta,
                [(
                    scoped_canonical(&store, root, goal_type(right)),
                    scoped_canonical(&store, root, goal_type(left)),
                    sp(),
                )],
                &context,
            )
            .expect("two open endpoints form a valid relation");
        store
            .require_goal_free_after_delta(
                &delta,
                scoped_canonical(&store, root, goal_type(left)),
                &context,
            )
            .expect_err("a relation alone must not invent a concrete endpoint");
    }

    #[test]
    fn gather_batch_mismatch_rolls_back_every_equation() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let left = goal(&mut store, root, Kind::Star);
        let right = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let prefix_left = goal(&mut store, root, Kind::Star);
        let prefix_right = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let reusable = goal(&mut store, root, Kind::Star);
        let gather = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();

        let mut causality = store.begin_delta(gather, sp()).unwrap();
        assert_eq!(
            store
                .try_constrain_gather_equation(
                    &mut causality,
                    scoped_canonical(&store, gather, goal_type(left)),
                    scoped_canonical(&store, gather, goal_type(right)),
                    sp(),
                    &context,
                )
                .expect("the first batch equation is independently valid"),
            CloseReadyEquationConstraint::Applied
        );
        assert!(
            !causality.writes.is_empty() && !causality.policy_meets.is_empty(),
            "the first equation must exercise both rollback journals"
        );
        drop(causality);

        let mut delta = store.begin_delta(gather, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(prefix_left)),
                scoped_canonical(&store, gather, goal_type(prefix_right)),
                sp(),
                &context,
            )
            .expect("seed an unrelated write and policy-meet prefix");
        let prefix_write_keys = delta.writes.keys().copied().collect::<Vec<_>>();
        let prefix_meet_keys = delta.policy_meets.keys().copied().collect::<Vec<_>>();
        let error = store
            .try_constrain_gather_equations(
                &mut delta,
                [
                    (
                        scoped_canonical(&store, gather, goal_type(left)),
                        scoped_canonical(&store, gather, goal_type(right)),
                        sp(),
                    ),
                    (
                        scoped_canonical(&store, gather, path(&["m", "String"], Vec::new())),
                        scoped_canonical(&store, gather, path(&["m", "Bool"], Vec::new())),
                        sp(),
                    ),
                ],
                &context,
            )
            .expect_err("a later mismatch must reject the whole candidate bundle");
        assert!(error.diag().1.contains("type mismatch"));
        assert_eq!(
            delta.writes.keys().copied().collect::<Vec<_>>(),
            prefix_write_keys,
            "the failed batch changed the pre-existing write prefix"
        );
        assert_eq!(
            delta.policy_meets.keys().copied().collect::<Vec<_>>(),
            prefix_meet_keys,
            "the failed batch changed the pre-existing policy prefix"
        );
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(reusable)),
                scoped_canonical(&store, gather, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .expect("the same delta remains usable after batch rollback");
        assert!(delta.writes.contains_key(&reusable));
    }

    #[test]
    fn deferred_gather_equation_preserves_prefix_and_delta_reuse() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let ancestor = goal(&mut store, root, Kind::Star);
        let parent = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let descendant = goal(&mut store, parent, Kind::Star);
        let batch_left = goal(&mut store, parent, Kind::Star);
        let batch_right = store
            .alloc_goal(
                parent,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let gather = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(gather, sp()).unwrap();
        let outcome = store
            .try_constrain_gather_equations(
                &mut delta,
                [
                    (
                        scoped_canonical(&store, gather, goal_type(batch_left)),
                        scoped_canonical(&store, gather, goal_type(batch_right)),
                        sp(),
                    ),
                    (
                        scoped_canonical(&store, gather, goal_type(ancestor)),
                        scoped_canonical(&store, gather, product(goal_type(descendant), unit())),
                        sp(),
                    ),
                ],
                &context,
            )
            .expect("unsafe ancestor-to-descendant retention should defer, not fail");
        assert_eq!(outcome, CloseReadyEquationConstraint::Deferred);
        assert!(
            delta.writes.is_empty() && delta.policy_meets.is_empty(),
            "deferral must roll back the preceding batch equation's write and policy meet"
        );

        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(descendant)),
                scoped_canonical(&store, gather, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .expect("the same delta remains usable after deferral");
        assert_eq!(
            store
                .try_constrain_gather_equations(
                    &mut delta,
                    [
                        (
                            scoped_canonical(&store, gather, goal_type(batch_left)),
                            scoped_canonical(&store, gather, goal_type(batch_right)),
                            sp(),
                        ),
                        (
                            scoped_canonical(&store, gather, goal_type(ancestor)),
                            scoped_canonical(
                                &store,
                                gather,
                                product(goal_type(descendant), unit()),
                            ),
                            sp(),
                        ),
                    ],
                    &context,
                )
                .expect("the resolved descendant makes the equation close-safe"),
            CloseReadyEquationConstraint::Applied
        );
        assert!(
            delta.writes.contains_key(&ancestor) && !delta.policy_meets.is_empty(),
            "the applied retry must retain the ancestor write and the batch policy meet"
        );
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(batch_left)),
                scoped_canonical(&store, gather, path(&["m", "I32"], Vec::new())),
                sp(),
                &context,
            )
            .expect("finish the policy-meet witness before closing its owner");

        store
            .close_owner(delta, Vec::new(), &context)
            .expect("close the applied gather");
        let parent_delta = store.begin_delta(parent, sp()).unwrap();
        store
            .close_owner(parent_delta, Vec::new(), &context)
            .expect("close the descendant owner");
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let ancestor_output = scoped_canonical(&store, root, goal_type(ancestor));
        let mut outputs = store
            .close_owner(
                root_delta,
                vec![(ancestor_output, GoalEscape::ClosedAt(root))],
                &context,
            )
            .expect("close the applied ancestor")
            .into_outputs();
        let closed = into_goal_free(outputs.remove(0));
        assert!(matches!(
            closed.ty().as_type(),
            Type::Product { left, right, .. }
                if matches!(left.as_ref(), Type::Path { segments, .. }
                    if segments.last().is_some_and(|segment| segment.as_str() == "I32"))
                    && matches!(right.as_ref(), Type::Unit { .. })
        ));
    }

    #[test]
    fn close_readiness_error_rolls_back_and_leaves_the_delta_reusable() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let cycle_left = goal(&mut store, root, Kind::Star);
        let cycle_right = goal(&mut store, root, Kind::Star);
        let equation_left = goal(&mut store, root, Kind::Star);
        let equation_right = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let gather = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(gather, sp()).unwrap();

        // Readiness is a fallible validation pass over every speculative
        // write. Seed a malformed pre-existing cycle directly so the test can
        // force that pass to fail after the new equation has written both its
        // solution and policy meet. Production equations cannot create this
        // seed because the occurs check rejects it first.
        delta.write_solution(
            cycle_left,
            GoalBinding {
                value: scoped_canonical(&store, gather, goal_type(cycle_right)),
                span: sp(),
            },
        );
        delta.write_solution(
            cycle_right,
            GoalBinding {
                value: scoped_canonical(&store, gather, goal_type(cycle_left)),
                span: sp(),
            },
        );

        let error = store
            .try_constrain_gather_equation(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(equation_left)),
                scoped_canonical(&store, gather, goal_type(equation_right)),
                sp(),
                &context,
            )
            .expect_err("the seeded cycle must make close-readiness traversal fail");
        assert!(error.diag().1.contains("cyclic inference-goal solution"));
        assert_eq!(delta.writes.len(), 2, "the failed equation leaked a write");
        assert!(
            delta.writes.contains_key(&cycle_left) && delta.writes.contains_key(&cycle_right),
            "readiness rollback changed the pre-existing delta prefix"
        );
        assert!(
            delta.policy_meets.is_empty(),
            "the readiness error leaked a policy meet"
        );

        // Removing the deliberately malformed prefix leaves the exact delta
        // empty and proves the equation transaction was closed: the same
        // delta accepts another ordinary equation immediately.
        delta.writes.clear();
        assert!(delta.is_empty());
        store
            .constrain(
                &mut delta,
                scoped_canonical(&store, gather, goal_type(equation_left)),
                scoped_canonical(&store, gather, goal_type(equation_right)),
                sp(),
                &context,
            )
            .expect("a readiness error must leave its delta reusable");
        assert!(!delta.is_empty());
    }

    #[test]
    fn retained_type_use_is_confined_to_its_destination_owner_chain() {
        let mut store = GoalStore::new();
        let ancestor = owner(&mut store, RigidScope::new());
        let destination = store
            .begin_owner(
                Some(ancestor),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let direct_child = store
            .begin_owner(
                Some(destination),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let nested_child = store
            .begin_owner(
                Some(direct_child),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let sibling = store
            .begin_owner(
                Some(ancestor),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let foreign = owner(&mut store, RigidScope::new());
        let retained = store
            .scoped_type(destination, InternedType::fresh_canonical(unit()), sp())
            .unwrap();

        for use_site in [destination, direct_child, nested_child] {
            let used = store
                .use_retained_type_at(destination, use_site, retained.clone(), sp())
                .expect("a retained type must remain usable down its destination owner chain");
            assert!(matches!(used.ty().as_type(), Type::Unit { .. }));
        }
        for use_site in [ancestor, sibling, foreign] {
            assert_panics_with(
                || {
                    let _ =
                        store.use_retained_type_at(destination, use_site, retained.clone(), sp());
                },
                "retained ordinary type consumed outside its destination owner chain",
            );
        }
    }

    #[test]
    fn goal_free_close_preview_is_inert_and_matches_the_later_physical_close() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let local = goal(&mut store, child, Kind::Star);
        let mut delta = store.begin_delta(child, sp()).unwrap();
        let open = store
            .scoped_goal_at(local, Vec::new(), child, sp())
            .unwrap();
        let concrete = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut delta, open.clone(), concrete, sp(), &context)
            .unwrap();

        let revision = store.revision;
        let writes = delta.writes.keys().copied().collect::<Vec<_>>();
        let preview = store
            .preview_goal_free_close_output(&delta, open.clone(), parent, &context)
            .expect("a completed retained child has one inert goal-free close preview");
        assert_path(&preview, &["m", "I32"], 0);
        assert_eq!(preview.scope, store.owner_scope(parent, sp()).unwrap());
        assert_eq!(store.revision, revision, "close preview mutated the store");
        assert_eq!(
            delta.writes.keys().copied().collect::<Vec<_>>(),
            writes,
            "close preview mutated the live delta"
        );
        assert!(
            store.binding(local, None).is_none(),
            "close preview committed a producer-local goal"
        );
        assert_eq!(
            store.owner_state(child, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open,
            "close preview consumed the retained producer owner"
        );

        let closed = store
            .close_owner(delta, vec![(open, GoalEscape::ToOwner(parent))], &context)
            .expect("the previewed child must still close normally");
        let ClosedGoalOutput::Retained(closed) =
            closed.into_outputs().pop().expect("one close output")
        else {
            panic!("the later physical close changed semantic output class")
        };
        assert_path(&closed.into_scoped_type(), &["m", "I32"], 0);
    }

    #[test]
    fn close_preview_rejects_an_unresolved_ancestor_and_a_non_parent_destination() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let unresolved = goal(&mut store, parent, Kind::Star);
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let delta = store.begin_delta(child, sp()).unwrap();
        let open = store
            .scoped_goal_at(unresolved, Vec::new(), child, sp())
            .unwrap();
        let error = store
            .preview_goal_free_close_output(&delta, open, parent, &context)
            .expect_err("a goal-free close preview must reject an unresolved ancestor");
        assert!(error.diag().1.contains("cannot infer"));

        let non_parent = owner(&mut store, RigidScope::new());
        let closed = store
            .scoped_type(child, InternedType::fresh_canonical(unit()), sp())
            .unwrap();
        assert_panics_with(
            || {
                let _ = store.preview_goal_free_close_output(&delta, closed, non_parent, &context);
            },
            "goal-free close preview targeted a non-parent owner",
        );
    }

    #[test]
    fn close_preview_rejects_a_child_local_rigid() {
        let context = ctx();
        let parent_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let child_scope = parent_scope.clone().with_binding("Child", Kind::Star, 11);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, parent_scope);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let delta = store.begin_delta(child, sp()).unwrap();
        let local = scoped(path(&["Child"], Vec::new()), child_scope);
        let error = store
            .preview_goal_free_close_output(&delta, local, parent, &context)
            .expect_err("a close preview must not launder a child-local rigid");
        assert!(error.diag().1.contains("outside its destination scope"));
    }

    #[test]
    fn goal_free_rebase_preserves_exact_rigid_proofs_and_rejects_shadowing() {
        let context = ctx();
        let parent_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let action_scope = parent_scope.clone().with_binding("Action", Kind::Star, 11);
        let shadow_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 12)]);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, parent_scope);
        let action = store
            .begin_owner(
                Some(parent),
                action_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let shadow = owner(&mut store, shadow_scope);

        let compatible = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(action, sp()).unwrap(),
        );
        let rebased = store
            .rebase_goal_free_type_to_owner(compatible, parent, sp(), &context)
            .expect("a goal-free type keeps the same rigid proof at its destination");
        assert_path(&rebased, &["A"], 0);
        assert!(
            rebased.ty().identity_is_canonical(),
            "rebase must close a noncanonical input's nominal identity frontier"
        );
        assert_eq!(rebased.scope, store.owner_scope(parent, sp()).unwrap());

        let shadowed = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(shadow, sp()).unwrap(),
        );
        let error = store
            .rebase_goal_free_type_to_owner(shadowed, parent, sp(), &context)
            .expect_err("same-spelled shadow binders must not be recaptured on rebase");
        assert!(error.diag().1.contains("outside its destination scope"));
    }

    #[test]
    fn parent_header_export_freezes_an_open_function_result_without_reserve_writes() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                store.owner_scope(parent, sp()).unwrap(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let result = goal(&mut store, child, Kind::Star);
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        let header = scoped_canonical(&store, child, function(unit(), goal_type(result)));

        let reserved = store
            .reserve_parent_header_export(&mut parent_delta, &child_delta, header, sp())
            .expect("a direct child header can reserve one parent result goal");
        assert!(parent_delta.writes.is_empty());
        assert!(parent_delta.policy_meets.is_empty());
        assert!(child_delta.writes.is_empty());
        assert!(child_delta.policy_meets.is_empty());

        let exported = store
            .install_parent_header_export(&mut child_delta, reserved, sp(), &context)
            .expect("the exact child delta can install its reserved header relation");
        let Type::Function { ret, .. } = exported.ty().as_type() else {
            panic!("an exported function header changed structural shape")
        };
        let Type::Goal { goal: proxy, .. } = ret.as_ref() else {
            panic!("an open function result was not exported through a parent goal")
        };
        assert_eq!(proxy.owner(), parent);
        assert_ne!(*proxy, result);
        assert!(child_delta.writes.contains_key(&result));
        assert!(parent_delta.writes.is_empty());

        let parent_unit = scoped_canonical(&store, parent, unit());
        store
            .constrain(
                &mut parent_delta,
                exported
                    .projected_structural_child(ScopedTypeEdge::FunctionReturn)
                    .unwrap(),
                parent_unit,
                sp(),
                &context,
            )
            .expect("parent adoption can solve the exported result independently");
        let closed = store
            .require_goal_free_after_delta(&parent_delta, exported, &context)
            .expect("the parent header closes after its own proxy is solved");
        assert_eq!(display_type(closed.ty().as_type()), ". -> .");
    }

    #[test]
    fn parent_header_export_installs_a_bound_forall_with_retained_child_evidence() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let parent_scope = store.owner_scope(parent, sp()).unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                parent_scope,
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };
        let child_scope = store
            .owner_scope(child, sp())
            .unwrap()
            .with_lexical_forall(&param);
        let header = scoped(
            forall("A", function(path(&["A"], Vec::new()), unit())),
            child_scope,
        );
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();

        let reserved = store
            .reserve_parent_header_export(&mut parent_delta, &child_delta, header, sp())
            .expect("a bound forall may retain its child lexical evidence");
        let parent_at_child = store
            .use_retained_type_at(parent, child, reserved.parent.clone(), sp())
            .unwrap();
        assert_ne!(
            reserved.child.scope, parent_at_child.scope,
            "the causal requires a harmless bound-only scope difference"
        );
        assert!(reserved.child.projected_scope_compatible(&parent_at_child));

        let exported = store
            .install_parent_header_export(&mut child_delta, reserved, sp(), &context)
            .expect("use-aware scope validation admits the bound forall");
        assert!(matches!(exported.ty().as_type(), Type::Forall { .. }));
        assert!(parent_delta.writes.is_empty());
        assert!(child_delta.writes.is_empty());
    }

    #[test]
    fn parent_header_export_shares_one_higher_kinded_head_across_arguments() {
        let context = ctx();
        let scope = RigidScope::from_bindings([
            ("A".to_owned(), Kind::Star, 10),
            ("B".to_owned(), Kind::Star, 11),
        ]);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, scope.clone());
        let child = store
            .begin_owner(Some(parent), scope, GoalOwnerKind::RetainedValue, sp())
            .unwrap();
        let head = store
            .alloc_goal(
                child,
                Kind::arrow_chain(1),
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "F"),
            )
            .unwrap();
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        let applied = |argument| Type::Goal {
            goal: head,
            args: vec![argument],
            meta: Meta::new(sp()),
            ext: (),
        };
        let header = scoped(
            product(
                applied(path(&["A"], Vec::new())),
                applied(path(&["B"], Vec::new())),
            ),
            store.owner_scope(child, sp()).unwrap(),
        );

        let reserved = store
            .reserve_parent_header_export(&mut parent_delta, &child_delta, header, sp())
            .expect("an applied child goal can reserve its parent head");
        assert!(parent_delta.writes.is_empty());
        assert!(child_delta.writes.is_empty());
        let exported = store
            .install_parent_header_export(&mut child_delta, reserved, sp(), &context)
            .expect("one head relation covers every retained application");

        let Type::Product { left, right, .. } = exported.ty().as_type() else {
            panic!("the higher-kinded header changed product shape")
        };
        let Type::Goal {
            goal: left_head,
            args: left_args,
            ..
        } = left.as_ref()
        else {
            panic!("the left higher-kinded application lost its goal head")
        };
        let Type::Goal {
            goal: right_head,
            args: right_args,
            ..
        } = right.as_ref()
        else {
            panic!("the right higher-kinded application lost its goal head")
        };
        assert_eq!(left_head, right_head);
        assert_eq!(left_head.owner(), parent);
        assert_eq!(left_args.len(), 1);
        assert_eq!(right_args.len(), 1);
        assert!(matches!(&left_args[0], Type::Path { segments, .. }
            if segments[0].as_str() == "A"));
        assert!(matches!(&right_args[0], Type::Path { segments, .. }
            if segments[0].as_str() == "B"));
        let proxy = store.goal_state(*left_head, sp()).unwrap();
        assert_eq!(proxy.kind, Kind::arrow_chain(1));
        assert_eq!(proxy.solution_policy, GoalSolutionPolicy::PolytypeAllowed);
        assert_eq!(proxy.origin.source_name.as_deref(), Some("F"));
        assert!(child_delta.writes.contains_key(&head));
    }

    #[test]
    fn sibling_header_exports_reserve_all_proxies_before_nested_construction() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let scope = store.owner_scope(parent, sp()).unwrap();
        let left_child = store
            .begin_owner(
                Some(parent),
                scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let right_child = store
            .begin_owner(Some(parent), scope, GoalOwnerKind::RetainedValue, sp())
            .unwrap();
        let left_goal = goal(&mut store, left_child, Kind::Star);
        let right_goal = goal(&mut store, right_child, Kind::Star);
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let mut left_delta = store.begin_delta(left_child, sp()).unwrap();
        let mut right_delta = store.begin_delta(right_child, sp()).unwrap();
        let left_header = scoped_canonical(
            &store,
            left_child,
            Type::Sum {
                left: Box::new(unit()),
                right: Box::new(goal_type(left_goal)),
                meta: Meta::new(sp()),
            },
        );
        let right_header =
            scoped_canonical(&store, right_child, product(goal_type(right_goal), unit()));

        let left_reserved = store
            .reserve_parent_header_export(&mut parent_delta, &left_delta, left_header, sp())
            .unwrap();
        let right_reserved = store
            .reserve_parent_header_export(&mut parent_delta, &right_delta, right_header, sp())
            .unwrap();
        assert!(left_reserved.matches_child_delta(&left_delta));
        assert!(!left_reserved.matches_child_delta(&right_delta));
        assert!(right_reserved.matches_child_delta(&right_delta));
        assert!(!right_reserved.matches_child_delta(&left_delta));
        assert!(parent_delta.writes.is_empty());
        assert!(left_delta.writes.is_empty());
        assert!(right_delta.writes.is_empty());
        assert_eq!(store.owner_state(parent, sp()).unwrap().goals.len(), 2);

        let left = store
            .install_parent_header_export(&mut left_delta, left_reserved, sp(), &context)
            .unwrap();
        let right = store
            .install_parent_header_export(&mut right_delta, right_reserved, sp(), &context)
            .unwrap();
        let proxies = goal_refs(left.ty().as_type())
            .into_iter()
            .chain(goal_refs(right.ty().as_type()))
            .collect::<Vec<_>>();
        assert_eq!(proxies.len(), 2);
        assert_ne!(proxies[0], proxies[1]);
        assert!(proxies.iter().all(|goal| goal.owner() == parent));
        let tail = right
            .projected_binary_peer(
                &scoped_canonical(&store, parent, unit()),
                ScopedTypeBinaryKind::Product,
                sp(),
            )
            .expect("one parent-scoped sibling composes with a closed tail");
        let nested = left
            .projected_binary_peer(&tail, ScopedTypeBinaryKind::Product, sp())
            .expect("independently exported siblings compose only after installation");
        assert!(matches!(nested.ty().as_type(), Type::Product { .. }));
    }

    #[test]
    fn later_invalid_sibling_leaves_the_reserved_batch_uninstalled() {
        let parent_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let invalid_scope = parent_scope.clone().with_binding("A", Kind::Star, 11);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, parent_scope.clone());
        let valid_child = store
            .begin_owner(
                Some(parent),
                parent_scope,
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let invalid_child = store
            .begin_owner(
                Some(parent),
                invalid_scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let valid_goal = goal(&mut store, valid_child, Kind::Star);
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let valid_delta = store.begin_delta(valid_child, sp()).unwrap();
        let invalid_delta = store.begin_delta(invalid_child, sp()).unwrap();
        let valid = store
            .reserve_parent_header_export(
                &mut parent_delta,
                &valid_delta,
                scoped_canonical(&store, valid_child, goal_type(valid_goal)),
                sp(),
            )
            .expect("the first sibling reserves its parent goal");
        let error = match store.reserve_parent_header_export(
            &mut parent_delta,
            &invalid_delta,
            scoped(path(&["A"], Vec::new()), invalid_scope),
            sp(),
        ) {
            Ok(_) => panic!("a shadowed later sibling reserved an invalid header"),
            Err(error) => error,
        };
        assert!(error.diag().1.contains("nested rigid type variable"));
        assert!(parent_delta.writes.is_empty());
        assert!(valid_delta.writes.is_empty());
        assert!(invalid_delta.writes.is_empty());
        assert_eq!(store.owner_state(parent, sp()).unwrap().goals.len(), 1);
        assert_eq!(
            goal_refs(valid.parent_header().ty().as_type()).len(),
            1,
            "the valid reservation stays inert until the caller abandons the failed batch"
        );
        // Dropping the speculative store, deltas, and uninstalled artifact is
        // the caller's existing `?`-error path; no child equation or
        // publication can survive that root-level abandonment.
    }

    #[test]
    fn nested_header_export_reserves_every_level_before_inner_installation() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let scope = store.owner_scope(parent, sp()).unwrap();
        let middle = store
            .begin_owner(
                Some(parent),
                scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(Some(middle), scope, GoalOwnerKind::RetainedValue, sp())
            .unwrap();
        let result = goal(&mut store, child, Kind::Star);
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let mut middle_delta = store.begin_delta(middle, sp()).unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        let child_header = scoped_canonical(&store, child, function(unit(), goal_type(result)));

        let child_to_middle = store
            .reserve_parent_header_export(&mut middle_delta, &child_delta, child_header, sp())
            .expect("the inner header reserves a middle-owned goal");
        let middle_header = child_to_middle.parent_header().clone();
        let middle_key = middle_header.transient_key();
        assert!(parent_delta.writes.is_empty());
        assert!(middle_delta.writes.is_empty());
        assert!(child_delta.writes.is_empty());
        let middle_to_parent = store
            .reserve_parent_header_export(&mut parent_delta, &middle_delta, middle_header, sp())
            .expect("the borrowed middle header reserves its parent-owned goal");
        assert!(parent_delta.writes.is_empty());
        assert!(middle_delta.writes.is_empty());
        assert!(child_delta.writes.is_empty());

        let installed_middle = store
            .install_parent_header_export(&mut child_delta, child_to_middle, sp(), &context)
            .expect("the inner relation installs only after both levels reserve");
        assert_eq!(
            installed_middle.transient_key(),
            middle_key,
            "the nested proof must retain the same structural middle header"
        );
        let installed_parent = store
            .install_parent_header_export(&mut middle_delta, middle_to_parent, sp(), &context)
            .expect("the outer relation installs after the inner child-local relation");
        assert!(child_delta.writes.contains_key(&result));
        assert_eq!(middle_delta.writes.len(), 1);
        assert!(parent_delta.writes.is_empty());

        let parent_result = installed_parent
            .projected_structural_child(ScopedTypeEdge::FunctionReturn)
            .unwrap();
        store
            .constrain(
                &mut parent_delta,
                parent_result,
                scoped_canonical(&store, parent, unit()),
                sp(),
                &context,
            )
            .unwrap();
        let closed = store
            .require_goal_free_after_delta(&parent_delta, installed_parent, &context)
            .unwrap();
        assert_eq!(display_type(closed.ty().as_type()), ". -> .");
    }

    #[test]
    fn parent_header_export_rejects_free_nominal_capture_before_reservation() {
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new().with_lexical_forall(&param));
        let parent_scope = store.owner_scope(parent, sp()).unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                parent_scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let sparse_child_scope = parent_scope
            .clone()
            .without_lexical_foralls(std::slice::from_ref(&param));
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let before = store.owner_state(parent, sp()).unwrap().goals.len();

        let error = match store.reserve_parent_header_export(
            &mut parent_delta,
            &child_delta,
            scoped(path(&["A"], Vec::new()), sparse_child_scope),
            sp(),
        ) {
            Ok(_) => panic!("an unbound child nominal was captured by a parent rigid"),
            Err(error) => error,
        };
        assert!(error.diag().1.contains("nested rigid type variable"));
        assert_eq!(store.owner_state(parent, sp()).unwrap().goals.len(), before);
        assert!(parent_delta.writes.is_empty());
        assert!(child_delta.writes.is_empty());
    }

    #[test]
    fn parent_header_export_rejects_foreign_store_and_shadowed_rigid_before_reservation() {
        let parent_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let child_scope = parent_scope.clone().with_binding("A", Kind::Star, 11);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, parent_scope);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope.clone(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let shadowed = scoped(path(&["A"], Vec::new()), child_scope);
        let before = store.owner_state(parent, sp()).unwrap().goals.len();
        let error = match store.reserve_parent_header_export(
            &mut parent_delta,
            &child_delta,
            shadowed,
            sp(),
        ) {
            Ok(_) => panic!("a same-spelled child rigid was recaptured by its parent"),
            Err(error) => error,
        };
        assert!(error.diag().1.contains("nested rigid type variable"));
        assert_eq!(store.owner_state(parent, sp()).unwrap().goals.len(), before);
        assert!(parent_delta.writes.is_empty());

        let mut foreign = GoalStore::new();
        let foreign_parent = owner(
            &mut foreign,
            RigidScope::new().with_test_context(1, "<test>"),
        );
        let foreign_child = foreign
            .begin_owner(
                Some(foreign_parent),
                foreign.owner_scope(foreign_parent, sp()).unwrap(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let foreign_goal = goal(&mut foreign, foreign_child, Kind::Star);
        let foreign_delta = foreign.begin_delta(foreign_child, sp()).unwrap();
        let foreign_header = scoped_canonical(&foreign, foreign_child, goal_type(foreign_goal));
        assert_panics_with(
            || {
                let _ = store.reserve_parent_header_export(
                    &mut parent_delta,
                    &foreign_delta,
                    foreign_header,
                    sp(),
                );
            },
            "inference-owner capability to the wrong goal store",
        );
        assert_eq!(store.owner_state(parent, sp()).unwrap().goals.len(), before);
        assert!(parent_delta.writes.is_empty());
    }

    #[test]
    fn projected_single_type_close_preserves_single_segment_nominal_heads() {
        let context = ctx()
            .with_kind(&["Packet"], Kind::Star)
            .with_kind(&["Container"], Kind::arrow_chain(1));
        let mut store = GoalStore::new();
        let nominal_owner = owner(&mut store, RigidScope::new());
        let nominal_delta = store.begin_delta(nominal_owner, sp()).unwrap();
        let mut scratch = ProjectedTypeCloseScratch::default();
        for ty in [
            path(&["Packet"], Vec::new()),
            path(&["Container"], vec![path(&["Packet"], Vec::new())]),
        ] {
            let value = scoped_canonical(&store, nominal_owner, ty.clone());
            let closed = store
                .close_projected_type_at_lexical_path(
                    &nominal_delta,
                    value,
                    &[],
                    &mut scratch,
                    &context,
                )
                .expect("a declared nominal head needs no lexical rigid binding");
            assert_eq!(closed.ty().as_type(), &ty);
            assert!(closed.0.scope.bindings.is_empty());
        }
        let rigid_owner = owner(
            &mut store,
            RigidScope::from_bindings([("Packet".to_owned(), Kind::Star, 41)]),
        );
        let rigid_delta = store.begin_delta(rigid_owner, sp()).unwrap();
        for (delta, source_owner) in [(&nominal_delta, rigid_owner), (&rigid_delta, nominal_owner)]
        {
            let value = scoped_canonical(&store, source_owner, path(&["Packet"], Vec::new()));
            assert!(
                store
                    .close_projected_type_at_lexical_path(
                        delta,
                        value,
                        &[],
                        &mut scratch,
                        &context,
                    )
                    .is_err(),
                "a declared nominal cannot replace a same-spelled rigid or supply its missing proof"
            );
        }
        assert!(nominal_delta.is_empty());
        assert!(rigid_delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_supplies_an_explicit_type_fn_binder() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let peer = scoped_canonical(&store, owner, unit());
        let body =
            peer.projected_closed_peer(InternedType::fresh_canonical(path(&["A"], Vec::new())));
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: Some(Kind::Star),
        };
        let mut scratch = ProjectedTypeCloseScratch::default();

        let closed = store
            .close_projected_type_at_lexical_path(&delta, body, &[param], &mut scratch, &context)
            .expect("a ReflTypeVar-backed type-function body keeps its explicit binder");
        assert_eq!(closed.0.projected_rigid_path_arity(), Some(0));
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_supplies_interleaved_higher_kinded_binders() {
        let context = ctx();
        let mut store = GoalStore::new();
        let destination = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(destination, sp()).unwrap();
        let peer = scoped_canonical(&store, destination, unit());
        let body = peer.projected_closed_peer(InternedType::fresh_canonical(path(
            &["F"],
            vec![path(&["A"], Vec::new())],
        )));
        let binders = [
            TypeParam {
                name: "F".to_owned(),
                span: sp(),
                kind: Some(Kind::arrow_chain(1)),
            },
            TypeParam {
                name: "A".to_owned(),
                span: sp(),
                kind: Some(Kind::Star),
            },
        ];
        let mut scratch = ProjectedTypeCloseScratch::default();

        let closed = store
            .close_projected_type_at_lexical_path(&delta, body, &binders, &mut scratch, &context)
            .expect("an applied HKT body keeps both explicit type-function binders");
        assert_eq!(closed.0.scope.kind("F"), Some(Kind::arrow_chain(1)));
        assert_eq!(closed.0.scope.kind("A"), Some(Kind::Star));
        assert!(
            matches!(closed.ty().as_type(), Type::Path { segments, args, .. }
            if segments[0].as_str() == "F"
                && matches!(&args[0], Type::Path { segments, .. }
                    if segments[0].as_str() == "A"))
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_slices_closed_structural_children() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let bottom = Type::Bottom {
            meta: Meta::new(sp()),
        };
        let ty = function(
            product(unit(), bottom.clone()),
            Type::Sum {
                left: Box::new(unit()),
                right: Box::new(bottom),
                meta: Meta::new(sp()),
            },
        );
        let value = scoped_canonical(&store, owner, ty);
        let mut scratch = ProjectedTypeCloseScratch::default();

        let closed = store
            .close_projected_type_at_lexical_path(&delta, value, &[], &mut scratch, &context)
            .expect("the structural parent closes once");
        let param = closed
            .structural_child(ScopedTypeEdge::FunctionParam)
            .expect("the closed function keeps its parameter");
        let result = closed
            .structural_child(ScopedTypeEdge::FunctionReturn)
            .expect("the closed function keeps its result");
        assert!(matches!(
            param
                .structural_child(ScopedTypeEdge::ProductLeft)
                .expect("the closed product keeps its left child")
                .ty()
                .as_type(),
            Type::Unit { .. }
        ));
        assert!(matches!(
            param
                .structural_child(ScopedTypeEdge::ProductRight)
                .expect("the closed product keeps its right child")
                .ty()
                .as_type(),
            Type::Bottom { .. }
        ));
        assert!(matches!(
            result
                .structural_child(ScopedTypeEdge::SumLeft)
                .expect("the closed sum keeps its left child")
                .ty()
                .as_type(),
            Type::Unit { .. }
        ));
        assert!(matches!(
            result
                .structural_child(ScopedTypeEdge::SumRight)
                .expect("the closed sum keeps its right child")
                .ty()
                .as_type(),
            Type::Bottom { .. }
        ));
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_instantiates_closed_forall_evidence() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let scheme = scoped_canonical(&store, owner, forall("A", path(&["A"], Vec::new())));
        let argument = scoped_canonical(&store, owner, unit());
        let mut scratch = ProjectedTypeCloseScratch::default();

        let scheme = store
            .close_projected_type_at_lexical_path(&delta, scheme, &[], &mut scratch, &context)
            .expect("the forall scheme closes");
        let argument = store
            .close_projected_type_at_lexical_path(&delta, argument, &[], &mut scratch, &context)
            .expect("the forall argument closes");
        let result = store
            .instantiate_closed_projected_forall(&delta, &scheme, &argument, &context)
            .expect("kind checking closed forall evidence succeeds")
            .expect("closed forall evidence applies without reopening inference");
        assert!(matches!(result.ty().as_type(), Type::Unit { .. }));
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_rejects_a_wrong_kind_forall_argument() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let scheme = scoped_canonical(
            &store,
            owner,
            Type::Forall {
                param: TypeParam {
                    name: "F".to_owned(),
                    span: sp(),
                    kind: Some(Kind::arrow_chain(1)),
                },
                body: Box::new(unit()),
                meta: Meta::new(sp()),
            },
        );
        let argument = scoped_canonical(&store, owner, unit());
        let mut scratch = ProjectedTypeCloseScratch::default();

        let scheme = store
            .close_projected_type_at_lexical_path(&delta, scheme, &[], &mut scratch, &context)
            .expect("the higher-kinded forall scheme closes");
        let argument = store
            .close_projected_type_at_lexical_path(&delta, argument, &[], &mut scratch, &context)
            .expect("the monotype argument closes");
        assert!(
            store
                .instantiate_closed_projected_forall(&delta, &scheme, &argument, &context)
                .expect("kind checking closed forall evidence succeeds")
                .is_none(),
            "closed evidence must not apply a monotype to a higher-kinded binder"
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_canonicalizes_a_qualified_nominal_with_arguments() {
        let context = ctx();
        let scope = RigidScope::from_bindings([
            ("A".to_owned(), Kind::Star, 10),
            ("Unused".to_owned(), Kind::Star, 11),
        ]);
        let mut store = GoalStore::new();
        let owner = owner(&mut store, scope);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let ty = path(&["m", "Box"], vec![path(&["A"], Vec::new())]);
        let actual = store
            .scoped_type(owner, InternedType::fresh(ty.clone()), sp())
            .unwrap();
        let expected = scoped_canonical(&store, owner, ty);
        let mut scratch = ProjectedTypeCloseScratch::default();

        let actual = store
            .close_projected_type_at_lexical_path(&delta, actual, &[], &mut scratch, &context)
            .expect("a noncanonical qualified nominal closes");
        let expected = store
            .close_projected_type_at_lexical_path(&delta, expected, &[], &mut scratch, &context)
            .expect("the canonical qualified nominal closes");
        store
            .validate_closed_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                "qualified projected nominal did not match",
                &context,
            )
            .expect("closed qualified nominals compare exactly");
        assert!(actual.ty().identity_is_canonical());
        assert!(expected.ty().identity_is_canonical());
        assert_eq!(actual.ty().as_type(), expected.ty().as_type());
        assert_eq!(actual.0.scope.binding("A"), expected.0.scope.binding("A"));
        assert!(actual.0.scope.binding("Unused").is_none());
        assert!(expected.0.scope.binding("Unused").is_none());
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_preserves_a_real_transparent_alias_application() {
        let module = parse(
            "module main;
             host type Actual;
             host type Box[A];
             type Alias[A] = Box(A);",
        )
        .expect("parse projected close alias witness");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower projected close alias witness");
        let package =
            Package::build(Path::new(""), modules, None).expect("build projected close package");
        package
            .resolve_imports()
            .expect("resolve projected close uses");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("projected close module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let destination = owner(&mut store, scope.clone());
        let mut delta = store.begin_delta(destination, sp()).unwrap();
        let argument = path(&["Actual"], Vec::new());
        let alias = scoped(path(&["Alias"], vec![argument.clone()]), scope.clone());
        let nominal = scoped(path(&["Box"], vec![argument]), scope);
        let mut scratch = ProjectedTypeCloseScratch::default();

        let alias = store
            .close_projected_type_at_lexical_path(&delta, alias, &[], &mut scratch, &tcx)
            .expect("the transparent alias application closes");
        let nominal = store
            .close_projected_type_at_lexical_path(&delta, nominal, &[], &mut scratch, &tcx)
            .expect("the qualified nominal application closes");
        store
            .validate_closed_projected_type_relation(
                &mut delta,
                &alias,
                &nominal,
                sp(),
                "projected alias application did not retain its nominal identity",
                &tcx,
            )
            .expect("the alias and nominal close to one exact identity");
        assert!(alias.ty().identity_is_canonical());
        assert_eq!(alias.ty().as_type(), nominal.ty().as_type());
        assert!(
            matches!(alias.ty().as_type(), Type::Path { segments, args, .. }
                if segments.last().is_some_and(|segment| segment.as_str() == "Box")
                    && matches!(&args[0], Type::Path { segments, .. }
                        if segments.last().is_some_and(|segment| segment.as_str() == "Actual")))
        );
        assert_eq!(alias.0.scope.module_path, "main");
        assert!(delta.is_empty());

        let mut collision_store = GoalStore::new();
        let missing_scope = collision_store.scope_from_type_ctx(&tcx);
        let mut expected_scope = missing_scope.clone();
        expected_scope.insert_ambient(
            "Actual",
            Kind::Star,
            crate::pass::typecheck_core::env::TypeBinderId::for_test(91),
        );
        let collision_owner = owner(&mut collision_store, expected_scope);
        let collision_delta = collision_store.begin_delta(collision_owner, sp()).unwrap();
        let missing_proof = ScopedType::new(
            InternedType::fresh_canonical(path(&["Actual"], Vec::new())),
            missing_scope,
        );
        assert!(
            collision_store
                .close_projected_type_at_lexical_path(
                    &collision_delta,
                    missing_proof,
                    &[],
                    &mut scratch,
                    &tcx,
                )
                .is_err(),
            "a canonical bare rigid cannot become the same-spelled nominal when its ambient proof is missing"
        );
    }

    #[test]
    fn projected_single_type_close_observes_adopted_goals_without_new_writes() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let goal = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal_at(goal, Vec::new(), owner, sp()).unwrap();
        let unit = scoped_canonical(&store, owner, unit());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(&mut delta, open.clone(), unit.clone(), sp(), &context)
            .expect("adoption solves the projected result goal");
        let writes = delta.writes.len();
        let mut scratch = ProjectedTypeCloseScratch::default();

        let actual = store
            .close_projected_type_at_lexical_path(&delta, open, &[], &mut scratch, &context)
            .expect("close observes the adopted goal solution");
        let expected = store
            .close_projected_type_at_lexical_path(&delta, unit, &[], &mut scratch, &context)
            .expect("the expected type closes");
        store
            .validate_closed_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                "adopted projected relation did not close",
                &context,
            )
            .expect("closed adopted types compare exactly");
        assert!(matches!(actual.ty().as_type(), Type::Unit { .. }));
        assert!(matches!(expected.ty().as_type(), Type::Unit { .. }));
        assert_eq!(delta.writes.len(), writes);
    }

    #[test]
    fn projected_single_type_close_rejects_capture_missing_ambient_and_shadow_proofs() {
        let context = ctx();
        let mut store = GoalStore::new();
        let destination = owner(
            &mut store,
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]),
        );
        let missing = owner(&mut store, RigidScope::new());
        let shadow = owner(
            &mut store,
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 11)]),
        );
        let delta = store.begin_delta(destination, sp()).unwrap();
        let mut scratch = ProjectedTypeCloseScratch::default();
        let at = |owner| {
            scoped(
                path(&["A"], Vec::new()),
                store.owner_scope(owner, sp()).unwrap(),
            )
        };
        for value in [at(missing), at(shadow)] {
            assert!(
                store
                    .close_projected_type_at_lexical_path(
                        &delta,
                        value,
                        &[],
                        &mut scratch,
                        &context,
                    )
                    .is_err()
            );
        }
        assert!(
            store
                .close_projected_type_at_lexical_path(
                    &delta,
                    scoped(
                        path(&["Unknown"], Vec::new()),
                        store.owner_scope(destination, sp()).unwrap(),
                    ),
                    &[],
                    &mut scratch,
                    &context,
                )
                .is_err(),
            "a free rigid absent from both the root and explicit path must fail closed"
        );
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: Some(Kind::Star),
        };
        assert!(
            store
                .close_projected_type_at_lexical_path(
                    &delta,
                    at(destination),
                    &[param],
                    &mut scratch,
                    &context,
                )
                .is_err()
        );
        let duplicate = [
            TypeParam {
                name: "B".to_owned(),
                span: sp(),
                kind: Some(Kind::Star),
            },
            TypeParam {
                name: "B".to_owned(),
                span: sp(),
                kind: Some(Kind::Star),
            },
        ];
        assert!(
            store
                .close_projected_type_at_lexical_path(
                    &delta,
                    scoped_canonical(&store, destination, unit()),
                    &duplicate,
                    &mut scratch,
                    &context,
                )
                .is_err(),
            "a duplicate explicit path name must fail before it can capture"
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_rejects_same_store_wrong_module_or_context() {
        let context = ctx();
        let mut store = GoalStore::new();
        let destination = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(destination, sp()).unwrap();
        let root_scope = store.owner_scope(destination, sp()).unwrap();
        let mut scratch = ProjectedTypeCloseScratch::default();

        for value in [
            scoped(
                unit(),
                root_scope.clone().with_test_context(1, "other-module"),
            ),
            scoped(unit(), root_scope.clone().with_test_context(2, "<test>")),
        ] {
            assert_panics_with(
                || {
                    let _ = store.close_projected_type_at_lexical_path(
                        &delta,
                        value,
                        &[],
                        &mut scratch,
                        &context,
                    );
                },
                "projected recipe type crossed its package or consumer module",
            );
        }
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_rejects_foreign_store_evidence() {
        let context = ctx();
        let mut store = GoalStore::new();
        let destination = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(destination, sp()).unwrap();
        let mut foreign = GoalStore::new();
        let foreign_owner = owner(
            &mut foreign,
            RigidScope::new().with_test_context(1, "<test>"),
        );
        let value = scoped_canonical(&foreign, foreign_owner, unit());
        let mut scratch = ProjectedTypeCloseScratch::default();

        assert_panics_with(
            || {
                let _ = store.close_projected_type_at_lexical_path(
                    &delta,
                    value,
                    &[],
                    &mut scratch,
                    &context,
                );
            },
            "projected recipe type carried rigid evidence from another goal store",
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn projected_single_type_close_reports_structured_relation_mismatch() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let actual = scoped_canonical(&store, owner, unit());
        let expected = scoped_canonical(
            &store,
            owner,
            Type::Bottom {
                meta: Meta::new(sp()),
            },
        );
        let mut scratch = ProjectedTypeCloseScratch::default();

        let actual = store
            .close_projected_type_at_lexical_path(&delta, actual, &[], &mut scratch, &context)
            .expect("the actual mismatch operand closes");
        let expected = store
            .close_projected_type_at_lexical_path(&delta, expected, &[], &mut scratch, &context)
            .expect("the expected mismatch operand closes");
        let error = store
            .validate_closed_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                "projected recipe result type mismatch",
                &context,
            )
            .unwrap_err();
        assert_eq!(
            error.diag(),
            (sp(), "projected recipe result type mismatch")
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn goal_free_projected_relation_rejects_same_spelled_peer_rigids_without_writes() {
        let context = ctx();
        let destination_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let peer_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 2)]);
        let mut store = GoalStore::new();
        let destination = owner(&mut store, destination_scope);
        let peer = owner(&mut store, peer_scope);
        let mut delta = store.begin_delta(destination, sp()).unwrap();
        let actual = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(peer, sp()).unwrap(),
        );
        let expected = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(destination, sp()).unwrap(),
        );
        let local = HashMap::new();
        let cross_module = HashMap::new();
        let aliases = super::super::aliases::AliasCtx {
            local: &local,
            cross_module: &cross_module,
            type_interner: None,
            source_module: None,
            package: None,
            binder_locals: None,
        };

        assert!(
            type_equiv_state(
                actual.ty().as_type(),
                expected.ty().as_type(),
                &aliases,
                actual.ty().identity_is_canonical(),
                expected.ty().identity_is_canonical(),
            ),
            "the former structural-only comparison must expose this proof-identity causal"
        );
        let error = store
            .validate_goal_free_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                &context,
            )
            .expect_err("same-spelled peer rigid proofs must not compare equal");
        assert!(
            error.diag().1.contains("outside its destination scope"),
            "the scoped relation must preserve the exact rigid escape diagnostic"
        );
        assert!(delta.is_empty());

        let actual = scoped_canonical(&store, destination, unit());
        let expected = scoped_canonical(
            &store,
            destination,
            Type::Bottom {
                meta: Meta::new(sp()),
            },
        );
        let error = store
            .validate_goal_free_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                &context,
            )
            .expect_err("a goal-free shape mismatch must remain structured");
        assert_eq!(error.diag(), (sp(), "type mismatch"));
        assert!(delta.is_empty());
    }

    #[test]
    fn goal_free_projected_relation_accepts_an_exact_retained_parent_proof() {
        let context = ctx();
        let parent_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let child_scope = parent_scope.clone().with_binding("Child", Kind::Star, 2);
        let mut store = GoalStore::new();
        let parent = owner(&mut store, parent_scope);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope,
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let actual = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(child, sp()).unwrap(),
        );
        let expected = scoped(
            path(&["A"], Vec::new()),
            store.owner_scope(parent, sp()).unwrap(),
        );
        let mut delta = store.begin_delta(parent, sp()).unwrap();

        store
            .validate_goal_free_projected_type_relation(
                &mut delta,
                &actual,
                &expected,
                sp(),
                &context,
            )
            .expect("an unused child binding must not obscure the exact parent rigid proof");
        assert!(delta.is_empty());
    }

    #[test]
    fn explicit_goal_free_barrier_rejects_then_accepts_an_ancestor_goal() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let goal = goal(&mut store, root, Kind::Star);
        let child = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(child, sp()).unwrap();
        let occurrence = store.scoped_goal_at(goal, Vec::new(), child, sp()).unwrap();

        store
            .zonk_for_close(&delta, occurrence.clone(), &context)
            .expect("an ordinary child close may retain an ancestor goal");
        let error = store
            .require_goal_free_after_delta(&delta, occurrence.clone(), &context)
            .expect_err("an explicit layer boundary must reject an unresolved ancestor goal");
        assert!(error.diag().1.contains("cannot infer"));

        let concrete = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut delta, occurrence.clone(), concrete, sp(), &context)
            .unwrap();
        let resolved = store
            .require_goal_free_after_delta(&delta, occurrence, &context)
            .expect("the same boundary accepts the goal after this delta solves it");
        assert!(matches!(
            resolved.ty.as_type(),
            Type::Path { segments, .. }
                if segments.iter().map(PathSegment::as_str).eq(["m", "I32"])
        ));
    }

    #[test]
    fn completion_wait_uses_origin_delta_and_refreshes_the_deepest_blocker() {
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let external = goal(&mut store, root, Kind::Star);
        let boundary = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let local = goal(&mut store, boundary, Kind::Star);
        let origin = store
            .begin_owner(
                Some(boundary),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut origin_delta = store.begin_delta(origin, sp()).unwrap();
        let value = scoped_canonical(
            &store,
            origin,
            product(goal_type(external), goal_type(local)),
        );
        let prepared = store
            .zonk_for_close(&origin_delta, value.clone(), &context)
            .unwrap();
        let wait = store.prepared_completion_wait(&prepared).unwrap().unwrap();
        assert_eq!(
            wait.deepest, local,
            "the full prepared type selects its deepest blocker"
        );
        assert!(
            !store
                .completion_wait_outside_boundary(wait, boundary, sp())
                .unwrap()
        );
        assert!(
            !store
                .completion_wait_changed(wait, &origin_delta, sp())
                .unwrap()
        );

        let mut tentative_parent = store.begin_delta(boundary, sp()).unwrap();
        store
            .constrain(
                &mut tentative_parent,
                scoped_canonical(&store, boundary, goal_type(local)),
                scoped_canonical(&store, boundary, unit()),
                sp(),
                &context,
            )
            .unwrap();
        assert!(
            !store
                .completion_wait_changed(wait, &origin_delta, sp())
                .unwrap(),
            "parent tentative equations are not visible in the origin delta"
        );
        drop(tentative_parent);

        let gather = store
            .begin_owner(
                Some(boundary),
                RigidScope::new(),
                GoalOwnerKind::ApplicationGather,
                sp(),
            )
            .unwrap();
        let mut gather_delta = store.begin_delta(gather, sp()).unwrap();
        store
            .constrain(
                &mut gather_delta,
                scoped_canonical(&store, gather, goal_type(local)),
                scoped_canonical(&store, gather, unit()),
                sp(),
                &context,
            )
            .unwrap();
        store
            .close_owner(gather_delta, Vec::new(), &context)
            .unwrap();
        assert!(
            store
                .completion_wait_changed(wait, &origin_delta, sp())
                .unwrap()
        );
        let refreshed = store
            .zonk_for_close(&origin_delta, value.clone(), &context)
            .unwrap();
        let refreshed = store.prepared_completion_wait(&refreshed).unwrap().unwrap();
        assert_eq!(refreshed.deepest, external);
        assert!(
            store
                .completion_wait_outside_boundary(refreshed, boundary, sp())
                .unwrap()
        );
        assert!(
            !store
                .completion_wait_changed(refreshed, &origin_delta, sp())
                .unwrap(),
            "a consumed dependency change does not authorize repeated preparation"
        );

        let unrelated_delta = store.begin_delta(origin, sp()).unwrap();
        assert_panics(|| {
            let _ = store.validate_completion_wait_origin(refreshed, &unrelated_delta, sp());
        });
        store
            .constrain(
                &mut origin_delta,
                scoped_canonical(&store, origin, goal_type(external)),
                scoped_canonical(&store, origin, unit()),
                sp(),
                &context,
            )
            .unwrap();
        assert!(
            store
                .completion_wait_changed(refreshed, &origin_delta, sp())
                .unwrap(),
            "the exact origin's own equations are visible"
        );
        let closed = store
            .zonk_for_close(&origin_delta, value, &context)
            .unwrap();
        assert!(store.prepared_completion_wait(&closed).unwrap().is_none());
    }

    #[test]
    fn cross_root_close_rejects_shadow_binder_without_writes() {
        let context = ctx();
        let mut store = GoalStore::new();
        let source = owner(
            &mut store,
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]),
        );
        let destination = owner(
            &mut store,
            RigidScope::from_bindings([("A".to_owned(), Kind::Star, 2)]),
        );
        let value = store
            .scoped_type(
                source,
                InternedType::fresh_canonical(path(&["A"], Vec::new())),
                sp(),
            )
            .unwrap();
        let source_delta = store.begin_delta(source, sp()).unwrap();
        let revision = store.revision;
        let error = store
            .close_owner(
                source_delta,
                vec![(value, GoalEscape::ClosedAt(destination))],
                &context,
            )
            .unwrap_err();

        assert!(error.diag().1.contains("outside its destination scope"));
        assert_eq!(
            store.revision, revision,
            "failed close must install no write"
        );
        assert_eq!(
            store.owner_state(source, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(
            store.owner_state(destination, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
    }

    fn scoped(ty: Type<Lowered>, scope: RigidScope) -> ScopedType {
        ScopedType::fresh(ty, scope)
    }

    fn into_goal_free(output: ClosedGoalOutput) -> ScopedType {
        match output {
            ClosedGoalOutput::GoalFree(output) => output.into_scoped_type(),
            ClosedGoalOutput::Retained(_)
            | ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                panic!("expected a value-position goal-free close output")
            }
        }
    }

    fn goal_free(output: &ClosedGoalOutput) -> &GoalFreeOutput {
        match output {
            ClosedGoalOutput::GoalFree(output) => output,
            ClosedGoalOutput::Retained(_)
            | ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                panic!("expected a value-position goal-free close output")
            }
        }
    }

    fn retained(output: &ClosedGoalOutput) -> &RetainedGoalOutput {
        match output {
            ClosedGoalOutput::Retained(output) => output,
            ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::GoalFree(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                panic!("expected a retained close output")
            }
        }
    }

    #[test]
    fn prepared_publication_is_inert_until_atomic_commit() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let goal = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal_at(goal, Vec::new(), owner, sp()).unwrap();
        let concrete = scoped(path(&["m", "I32"], Vec::new()), RigidScope::new());
        let mut goal_delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(&mut goal_delta, open.clone(), concrete, sp(), &context)
            .unwrap();
        let mut builder = PublicationBuilder::new(&store, &goal_delta);
        builder.record_type_resolution(publication_site, 0, open);
        builder.mark_value_at_type_slot(publication_site, 0);
        let prepared = builder
            .finish()
            .prepare_root(&store, goal_delta, Vec::new(), &context)
            .unwrap();
        assert_eq!(publication_validation_work(), (1, 0));

        let mut elaborations = Elaborations::new();
        assert!(
            elaborations
                .type_resolution_for("<test>", publication_site.id, 0)
                .is_none()
        );
        assert!(!elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open,
            "preparation must not close the goal owner"
        );

        let semantic_outputs = prepared.commit_without_rec_order(&mut store, &mut elaborations);
        assert!(semantic_outputs.is_empty());
        let resolved = elaborations
            .type_resolution_for("<test>", publication_site.id, 0)
            .expect("closed publication committed its resolved type");
        let Type::Path { segments, .. } = &resolved.resolved else {
            panic!("resolved publication type should remain a nominal path")
        };
        assert_eq!(segments.last().unwrap().as_str(), "I32");
        assert!(elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
    }

    #[test]
    fn prepared_publication_cannot_commit_into_another_goal_store() {
        let context = ctx();
        let mut first_store = GoalStore::new();
        let first_owner = owner(&mut first_store, RigidScope::new());
        let first_delta = first_store.begin_delta(first_owner, sp()).unwrap();
        let prepared = PublicationBuilder::new(&first_store, &first_delta)
            .finish()
            .prepare_root(&first_store, first_delta, Vec::new(), &context)
            .unwrap();

        let mut second_store = GoalStore::new();
        let second_owner = owner(&mut second_store, RigidScope::new());
        assert_eq!(first_owner.domain().index(), second_owner.domain().index());
        assert_eq!(first_owner.index(), second_owner.index());
        assert_ne!(first_owner.domain(), second_owner.domain());

        let mut elaborations = Elaborations::new();
        let rejected = catch_unwind(AssertUnwindSafe(|| {
            prepared.commit_without_rec_order(&mut second_store, &mut elaborations);
        }))
        .is_err();

        assert_eq!(
            first_store
                .owner_state(first_owner, sp())
                .unwrap()
                .lifecycle,
            OwnerLifecycle::Open,
            "the source store must remain open until its own prepared close commits"
        );
        assert_eq!(
            second_store
                .owner_state(second_owner, sp())
                .unwrap()
                .lifecycle,
            OwnerLifecycle::Open,
            "rejecting a foreign prepared close must not mutate the receiving store"
        );
        assert!(
            rejected,
            "a prepared close is an exact capability for its source goal store"
        );
    }

    #[test]
    fn stale_prepared_publication_cannot_overwrite_an_intervening_close() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store
            .scoped_goal_at(inferred, Vec::new(), owner, sp())
            .unwrap();

        let mut stale_delta = store.begin_delta(owner, sp()).unwrap();
        let stale_solution = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(
                &mut stale_delta,
                open.clone(),
                stale_solution,
                sp(),
                &context,
            )
            .unwrap();
        let stale = PublicationBuilder::new(&store, &stale_delta)
            .finish()
            .prepare_root(&store, stale_delta, Vec::new(), &context)
            .unwrap();

        let mut winning_delta = store.begin_delta(owner, sp()).unwrap();
        let winning_solution = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "Bool"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut winning_delta, open, winning_solution, sp(), &context)
            .unwrap();
        let winning = PublicationBuilder::new(&store, &winning_delta)
            .finish()
            .prepare_root(&store, winning_delta, Vec::new(), &context)
            .unwrap();

        let mut elaborations = Elaborations::new();
        winning.commit_without_rec_order(&mut store, &mut elaborations);
        assert_path(
            &store
                .binding(inferred, None)
                .expect("the intervening close installed its solution"),
            &["m", "Bool"],
            0,
        );

        let rejected = catch_unwind(AssertUnwindSafe(|| {
            stale.commit_without_rec_order(&mut store, &mut elaborations);
        }))
        .is_err();

        assert_path(
            &store
                .binding(inferred, None)
                .expect("rejecting the stale close retained the winning solution"),
            &["m", "Bool"],
            0,
        );
        assert!(
            rejected,
            "a prepared close must become stale after an intervening close"
        );
    }

    #[test]
    fn prepared_root_becomes_stale_when_another_root_closes() {
        let context = ctx();
        let mut store = GoalStore::new();
        let outer = owner(&mut store, RigidScope::new());
        let independent = owner(&mut store, RigidScope::new());

        let outer_delta = store.begin_delta(outer, sp()).unwrap();
        let stale_outer = PublicationBuilder::new(&store, &outer_delta)
            .finish()
            .prepare_root(&store, outer_delta, Vec::new(), &context)
            .unwrap();

        let independent_delta = store.begin_delta(independent, sp()).unwrap();
        let closed_independent = PublicationBuilder::new(&store, &independent_delta)
            .finish()
            .prepare_root(&store, independent_delta, Vec::new(), &context)
            .unwrap();
        let mut elaborations = Elaborations::new();
        closed_independent.commit_without_rec_order(&mut store, &mut elaborations);

        let rejected = catch_unwind(AssertUnwindSafe(|| {
            stale_outer.commit_without_rec_order(&mut store, &mut elaborations);
        }))
        .is_err();

        assert!(
            rejected,
            "another root close must invalidate an already-prepared outer close"
        );
        assert_eq!(
            store.owner_state(outer, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(
            store.owner_state(independent, sp()).unwrap().lifecycle,
            OwnerLifecycle::Closed
        );
    }

    #[test]
    fn prepared_root_with_reused_publication_becomes_stale_before_commit() {
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let independent = owner(&mut store, RigidScope::new());
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let published_span = Span::new(20, 21);
        let published = store
            .scoped_type(child, InternedType::fresh_canonical(unit()), published_span)
            .unwrap();
        let mut root_builder = PublicationBuilder::new(&store, &root_delta);
        let child_slot = root_builder.reserve_child(&store, child);
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_position_type(published_span, published, HashSet::new());
        let (child_publication, outputs) = child_builder
            .finish()
            .close_child(&mut store, child_delta, root, Vec::new(), &context)
            .unwrap();
        assert!(outputs.is_empty());
        root_builder.fill_child(child_slot, child_publication);
        let stale_root = root_builder
            .finish()
            .prepare_root(&store, root_delta, Vec::new(), &context)
            .unwrap();
        assert_eq!(publication_validation_work(), (1, 1));

        let independent_delta = store.begin_delta(independent, sp()).unwrap();
        let closed_independent = PublicationBuilder::new(&store, &independent_delta)
            .finish()
            .prepare_root(&store, independent_delta, Vec::new(), &context)
            .unwrap();
        let mut elaborations = Elaborations::new();
        closed_independent.commit_without_rec_order(&mut store, &mut elaborations);
        assert_panics_with(
            || {
                stale_root.commit_without_rec_order(&mut store, &mut elaborations);
            },
            "prepared inference close became stale before commit",
        );

        assert_eq!(
            store.owner_state(root, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(
            store.owner_state(independent, sp()).unwrap().lifecycle,
            OwnerLifecycle::Closed
        );
        assert!(
            elaborations
                .position_index()
                .type_at("<test>", published_span)
                .is_none()
        );
        assert_eq!(publication_validation_work(), (1, 1));
    }

    #[test]
    fn prepared_rec_order_commit_rejects_another_type_context_before_closing_goals() {
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(sp()),
        };

        let mut first_elaborations = Elaborations::new();
        let mut first_tcx = TypeCtx::new(&env, &mut first_elaborations);
        first_tcx.push_pending_rec_order("pending".to_owned(), &source, sp());
        let rec_order =
            first_tcx.prepare_rec_order_updates(vec![(&source, PendingRecOrderState::TypeOnly)]);

        let mut second_elaborations = Elaborations::new();
        let mut second_tcx = TypeCtx::new(&env, &mut second_elaborations);
        let second_pending = second_tcx.push_pending_rec_order("pending".to_owned(), &source, sp());
        assert!(
            !second_tcx.has_context_id(),
            "the foreign target begins without a goal-context identity"
        );

        let mut store = GoalStore::new();
        let owner = store
            .begin_owner(
                None,
                store.scope_from_type_ctx(&first_tcx),
                GoalOwnerKind::Application,
                sp(),
            )
            .expect("open root owner");
        let delta = store.begin_delta(owner, sp()).expect("open root delta");
        let prepared = PublicationBuilder::new(&store, &delta)
            .finish()
            .prepare_root(&store, delta, Vec::new(), &first_tcx)
            .expect("prepare root publication");

        let rejected = catch_unwind(AssertUnwindSafe(|| {
            let _ = prepared.commit_with_rec_order(&mut store, &mut second_tcx, rec_order);
        }))
        .is_err();

        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open,
            "a recursive-order plan from another TypeCtx must be rejected before the goal owner closes"
        );
        assert!(matches!(
            second_tcx.pending_rec_order_state(second_pending),
            PendingRecOrderState::Pending
        ));
        assert!(
            !second_tcx.has_context_id(),
            "rejecting a foreign recursive-order capability must not mint identity in its target"
        );
        assert!(
            rejected,
            "a prepared recursive-order update is an exact capability for its source TypeCtx"
        );
    }

    #[test]
    fn stale_prepared_rec_order_commit_rejects_before_closing_goals() {
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let source = Expr::Unit {
            occurrence: Default::default(),
            meta: Meta::new(sp()),
        };

        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let mark = tcx.save();
        tcx.push_pending_rec_order("pending".to_owned(), &source, sp());
        let rec_order =
            tcx.prepare_rec_order_updates(vec![(&source, PendingRecOrderState::TypeOnly)]);

        let mut store = GoalStore::new();
        let owner = store
            .begin_owner(
                None,
                store.scope_from_type_ctx(&tcx),
                GoalOwnerKind::Application,
                sp(),
            )
            .expect("open root owner");
        let delta = store.begin_delta(owner, sp()).expect("open root delta");
        let prepared = PublicationBuilder::new(&store, &delta)
            .finish()
            .prepare_root(&store, delta, Vec::new(), &tcx)
            .expect("prepare root publication");

        tcx.restore(mark);
        let rejected = catch_unwind(AssertUnwindSafe(|| {
            let _ = prepared.commit_with_rec_order(&mut store, &mut tcx, rec_order);
        }))
        .is_err();

        assert!(
            rejected,
            "restoring the TypeCtx must make its prepared recursive-order update stale"
        );
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open,
            "stale recursive-order rejection must precede the goal-store mutation"
        );
    }

    #[test]
    fn nested_publication_fills_reserved_source_position() {
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let first_child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let second_child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();
        let first_child_delta = store.begin_delta(first_child, sp()).unwrap();
        let second_child_delta = store.begin_delta(second_child, sp()).unwrap();

        let parent_i32 = store
            .scoped_type(
                parent,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let parent_bool = store
            .scoped_type(
                parent,
                InternedType::fresh_canonical(path(&["m", "Bool"], Vec::new())),
                sp(),
            )
            .unwrap();
        let parent_pair = store
            .scoped_type(
                parent,
                InternedType::fresh_canonical(path(
                    &["m", "Pair"],
                    vec![
                        path(&["m", "I32"], Vec::new()),
                        path(&["m", "String"], Vec::new()),
                    ],
                )),
                sp(),
            )
            .unwrap();
        let first_child_string = store
            .scoped_type(
                first_child,
                InternedType::fresh_canonical(path(&["m", "String"], Vec::new())),
                sp(),
            )
            .unwrap();
        let second_child_box = store
            .scoped_type(
                second_child,
                InternedType::fresh_canonical(path(
                    &["m", "Box"],
                    vec![path(&["m", "I32"], Vec::new())],
                )),
                sp(),
            )
            .unwrap();

        let mut parent_builder = PublicationBuilder::new(&store, &parent_delta);
        let site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        parent_builder.record_inlay_type_args(site, sp(), 0, vec![parent_i32], Default::default());
        let first_child_slot = parent_builder.reserve_child(&store, first_child);
        parent_builder.record_inlay_type_args(site, sp(), 2, vec![parent_bool], Default::default());
        let second_child_slot = parent_builder.reserve_child(&store, second_child);
        parent_builder.record_inlay_type_args(site, sp(), 4, vec![parent_pair], Default::default());

        let mut second_child_builder = PublicationBuilder::new(&store, &second_child_delta);
        second_child_builder.record_inlay_type_args(
            site,
            sp(),
            3,
            vec![second_child_box],
            Default::default(),
        );
        let (second_child_publication, second_child_semantic_outputs) = second_child_builder
            .finish()
            .close_child(&mut store, second_child_delta, parent, Vec::new(), &context)
            .unwrap();
        assert!(second_child_semantic_outputs.is_empty());
        parent_builder.fill_child(second_child_slot, second_child_publication);

        let mut first_child_builder = PublicationBuilder::new(&store, &first_child_delta);
        first_child_builder.record_inlay_type_args(
            site,
            sp(),
            1,
            vec![first_child_string],
            Default::default(),
        );
        let (first_child_publication, first_child_semantic_outputs) = first_child_builder
            .finish()
            .close_child(&mut store, first_child_delta, parent, Vec::new(), &context)
            .unwrap();
        assert!(first_child_semantic_outputs.is_empty());
        parent_builder.fill_child(first_child_slot, first_child_publication);

        let mut elaborations = Elaborations::new();
        let semantic_outputs = parent_builder
            .finish()
            .close_root(
                &mut store,
                parent_delta,
                Vec::new(),
                &context,
                &mut elaborations,
            )
            .unwrap();
        assert!(semantic_outputs.is_empty());
        let types = elaborations
            .position_index()
            .inlay_type_args_iter()
            .filter(|((module_path, span), _)| module_path == "<test>" && *span == sp())
            .flat_map(|(_, types)| types.iter())
            .collect::<Vec<_>>();
        let names = types
            .iter()
            .map(|ty| {
                let Type::Path { segments, .. } = ty else {
                    panic!("inlay publication should remain a nominal path")
                };
                segments.last().unwrap().as_str()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["I32", "String", "Bool", "Box", "Pair"]);
    }

    #[test]
    fn publication_preserves_child_rigids_while_parent_goals_close() {
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let parent_goal = goal(&mut store, parent, Kind::Star);
        let child_scope =
            store
                .owner_scope(parent, sp())
                .unwrap()
                .with_binding("A", Kind::Star, 41);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let bare_span = Span::new(40, 41);
        let mixed_span = Span::new(50, 51);
        let bare = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["A"], Vec::new())),
                bare_span,
            )
            .unwrap();
        let parent_goal_in_child = store
            .scoped_goal_at(parent_goal, Vec::new(), child, mixed_span)
            .unwrap();
        let mixed = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(product(
                    path(&["A"], Vec::new()),
                    parent_goal_in_child.ty().clone_type(),
                )),
                mixed_span,
            )
            .unwrap();
        let structural_node_id = crate::ast::NodeId(43);
        let structural_replacement = Expr::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::new(Vec::new()),
            ret_ty: Some(mixed.ty().clone_type()),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(sp()),
            }),
            meta: Meta::new(sp()),
            caps: (),
        };

        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let mut parent_builder = PublicationBuilder::new(&store, &parent_delta);
        let child_slot = parent_builder.reserve_child(&store, child);
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_position_type(bare_span, bare, HashSet::from(["A".to_owned()]));
        child_builder.record_position_type(mixed_span, mixed, HashSet::from(["A".to_owned()]));
        child_builder.record_lowered_elaboration(
            &store,
            structural_node_id,
            structural_replacement,
        );
        let (child_publication, outputs) = child_builder
            .finish()
            .close_child(&mut store, child_delta, parent, Vec::new(), &context)
            .unwrap();
        assert!(outputs.is_empty());
        parent_builder.fill_child(child_slot, child_publication);

        let open_parent = store
            .scoped_goal_at(parent_goal, Vec::new(), parent, sp())
            .unwrap();
        let concrete = store
            .scoped_type(
                parent,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut parent_delta, open_parent, concrete, sp(), &context)
            .unwrap();
        let mut elaborations = Elaborations::new();
        let outputs = parent_builder
            .finish()
            .close_root(
                &mut store,
                parent_delta,
                Vec::new(),
                &context,
                &mut elaborations,
            )
            .unwrap();
        assert!(outputs.is_empty());
        assert_eq!(publication_validation_work(), (5, 1));

        assert!(matches!(
            elaborations.position_index().type_at("<test>", bare_span),
            Some(Type::Path { segments, .. }) if segments[0].as_str() == "A"
        ));
        assert!(matches!(
            elaborations.position_index().type_at("<test>", mixed_span),
            Some(Type::Product { left, right, .. })
                if matches!(
                    left.as_ref(),
                    Type::Path { segments, .. } if segments[0].as_str() == "A"
                ) && matches!(
                    right.as_ref(),
                    Type::Path { segments, .. }
                        if segments.last().unwrap().as_str() == "I32"
                )
        ));
        assert_eq!(
            elaborations
                .position_index()
                .type_binders_at("<test>", mixed_span),
            Some(HashSet::from(["A".to_owned()]))
        );
        let structural_original = Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                tail_continuation: None,
                name: "same".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::ExpectedFromBody,
                ),
                annotation: None,
                value: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                body: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                runtime_ty: Type::Unit {
                    meta: Meta::new(sp()),
                },
            }),
            meta: Meta::new(sp()),
            ext: structural_node_id,
        };
        let Some(RecordedElaboration::Lowered(Expr::FnExpr {
            ret_ty: Some(Type::Product { left, right, .. }),
            ..
        })) = elaborations.elaboration_for("<test>", &structural_original)
        else {
            panic!("structural publication must preserve its lexical binder and close its goal")
        };
        assert!(
            matches!(left.as_ref(), Type::Path { segments, .. } if segments[0].as_str() == "A")
        );
        assert!(matches!(right.as_ref(), Type::Path { segments, .. }
                if segments.last().unwrap().as_str() == "I32"));
    }

    #[test]
    fn rec_order_publication_pairs_replacement_with_its_exact_type() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let replacement_goal = goal(&mut store, owner, Kind::Star);
        let replacement_open = store
            .scoped_goal_at(replacement_goal, Vec::new(), owner, sp())
            .unwrap();
        let exact = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let replacement = Expr::Let {
            occurrence: Default::default(),
            name: "same".to_owned(),
            name_span: sp(),
            ty: None,
            pattern: (),
            value: Box::new(Expr::FnExpr {
                occurrence: Default::default(),
                sig: crate::ast::Signature::new(Vec::new()),
                ret_ty: Some(replacement_open.ty().clone_type()),
                body: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                meta: Meta::new(sp()),
                caps: (),
            }),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(sp()),
            }),
            meta: Meta::new(sp()),
        };
        let node_id = crate::ast::NodeId(41);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &delta);
        builder.record_rec_order_binding(&store, node_id, replacement, exact);
        let concrete = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "Bool"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut delta, replacement_open, concrete, sp(), &context)
            .unwrap();
        let mut elaborations = Elaborations::new();
        let outputs = builder
            .finish()
            .close_root(&mut store, delta, Vec::new(), &context, &mut elaborations)
            .unwrap();
        assert!(outputs.is_empty());

        let original = Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                tail_continuation: None,
                name: "same".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::ExpectedFromBody,
                ),
                annotation: None,
                value: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                body: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                runtime_ty: Type::Unit {
                    meta: Meta::new(sp()),
                },
            }),
            meta: Meta::new(sp()),
            ext: node_id,
        };
        assert!(
            elaborations.elaboration_for("<test>", &original).is_none(),
            "compiler-private recursive-order data must not widen the public elaboration API"
        );
        let Some((replacement, binding_type)) =
            elaborations.rec_order_runtime_for("<test>", &original)
        else {
            panic!("recursive-order publication must commit one paired artifact")
        };
        assert!(matches!(
            replacement,
            Expr::Let {
                name,
                ty: None,
                value,
                ..
            } if name == "same"
                && matches!(
                    value.as_ref(),
                    Expr::FnExpr {
                        ret_ty: Some(Type::Path { segments, .. }),
                        ..
                    } if segments.last().unwrap().as_str() == "Bool"
                )
        ));
        assert!(binding_type.identity_canonical);
        let Type::Path { segments, .. } = &binding_type.resolved else {
            panic!("recursive-order exact type should remain a nominal path")
        };
        assert_eq!(segments.last().unwrap().as_str(), "I32");
    }

    #[test]
    fn publication_plan_rejects_a_second_live_delta_for_the_same_owner_before_mutation() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store
            .scoped_goal_at(inferred, Vec::new(), owner, sp())
            .unwrap();

        let mut first_delta = store.begin_delta(owner, sp()).unwrap();
        let first_type = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut first_delta, open.clone(), first_type, sp(), &context)
            .unwrap();

        let mut builder = PublicationBuilder::new(&store, &first_delta);
        builder.mark_value_at_type_slot(publication_site, 0);

        let mut second_delta = store.begin_delta(owner, sp()).unwrap();
        let second_type = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "Bool"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut second_delta, open.clone(), second_type, sp(), &context)
            .unwrap();

        assert_panics_with(
            || {
                let mut elaborations = Elaborations::new();
                let _ = builder.finish().close_root(
                    &mut store,
                    second_delta,
                    Vec::new(),
                    &context,
                    &mut elaborations,
                );
            },
            "publication plan and inference delta belong to different speculative deltas",
        );

        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert!(
            store.binding(inferred, None).is_none(),
            "rejected publication must not install the second delta's solution"
        );
        let first_value = store
            .zonk_with_delta(&open, &first_delta)
            .expect("the first speculative delta remains live");
        assert_path(&first_value, &["m", "I32"], 0);
        let elaborations = Elaborations::new();
        assert!(!elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
    }

    #[test]
    fn publication_retains_mixed_goal_free_and_hkt_slots_from_child_to_root() {
        reset_publication_validation_work();
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);

        let mut store = GoalStore::new();
        let root_scope = store.scope_from_type_ctx(&tcx);
        let root = store
            .begin_owner(None, root_scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let parent = store
            .begin_owner(
                Some(root),
                root_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();

        let mark = tcx.save();
        let param = TypeParam {
            name: "F".to_owned(),
            span: sp(),
            kind: Some(Kind::arrow_chain(1)),
        };
        let binder = tcx.push_retained_type_param(&param);
        let child_scope = store.scope_from_type_ctx(&tcx);
        let child = store
            .begin_owner(
                Some(parent),
                child_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let source_body = store
            .scoped_type(
                child,
                InternedType::fresh(function(
                    path(&["F"], vec![unit()]),
                    path(&["F"], vec![unit()]),
                )),
                sp(),
            )
            .unwrap();
        let source_body = store
            .zonk_for_close(&child_delta, source_body, &tcx)
            .unwrap();
        let source_scheme = store.abstract_lexical_forall(source_body, &binder);
        let goal_free_span = Span::new(10, 11);
        let goal_free = store
            .scoped_type(child, InternedType::fresh(unit()), goal_free_span)
            .unwrap();

        let mut root_builder = PublicationBuilder::new(&store, &root_delta);
        let parent_slot = root_builder.reserve_child(&store, parent);
        let mut parent_builder = PublicationBuilder::new(&store, &parent_delta);
        let child_slot = parent_builder.reserve_child(&store, child);
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_scoped_function_scheme_position_type(
            sp(),
            source_scheme.value.clone(),
            HashSet::from(["F".to_owned()]),
        );
        child_builder.record_position_type(goal_free_span, goal_free, HashSet::new());

        let (child_publication, mut child_outputs) = child_builder
            .finish()
            .close_prepared_child(
                &mut store,
                child_delta,
                parent,
                vec![(source_scheme, GoalEscape::ToOwnerFunctionScheme(parent))],
                &tcx,
            )
            .unwrap();
        let child_scheme = match child_outputs.remove(0) {
            ClosedGoalOutput::RetainedFunctionScheme(output) => output,
            _ => panic!("child must retain one prepared whole function scheme"),
        };
        assert!(child_outputs.is_empty());
        parent_builder.fill_child(child_slot, child_publication);
        tcx.restore(mark);

        let parent_scheme = store
            .prepare_retained_function_scheme(&parent_delta, child_scheme, &tcx)
            .unwrap();
        let (parent_publication, mut parent_outputs) = parent_builder
            .finish()
            .close_prepared_child(
                &mut store,
                parent_delta,
                root,
                vec![(parent_scheme, GoalEscape::ToOwnerFunctionScheme(root))],
                &tcx,
            )
            .unwrap();
        let parent_scheme = match parent_outputs.remove(0) {
            ClosedGoalOutput::RetainedFunctionScheme(output) => output,
            _ => panic!("parent must retain the prepared whole function scheme"),
        };
        assert!(parent_outputs.is_empty());
        root_builder.fill_child(parent_slot, parent_publication);

        let root_scheme = store
            .prepare_retained_function_scheme(&root_delta, parent_scheme, &tcx)
            .unwrap();
        let prepared = root_builder
            .finish()
            .prepare_root(
                &store,
                root_delta,
                vec![(root_scheme.value, GoalEscape::ClosedFunctionSchemeAt(root))],
                &tcx,
            )
            .unwrap();
        assert_eq!(publication_validation_work(), (2, 4));
        let root_outputs = prepared.commit_without_rec_order(&mut store, tcx.elaborations);
        assert_eq!(root_outputs.len(), 1);
        assert!(matches!(
            function_scheme(&root_outputs[0]).ty().as_type(),
            Type::Forall { .. }
        ));
        drop(tcx);

        assert!(matches!(
            elaborations
                .position_index()
                .type_at("main", goal_free_span),
            Some(Type::Unit { .. })
        ));

        let Some(Type::Forall { param, body, .. }) =
            elaborations.position_index().type_at("main", sp())
        else {
            panic!("the source `[*F]` function scheme must survive root publication")
        };
        assert_eq!(param.name, "F");
        assert_eq!(param.effective_kind(), Kind::arrow_chain(1));
        let Type::Function { param, ret, .. } = body.as_ref() else {
            panic!("the retained whole scheme must remain a function")
        };
        assert!(matches!(
            param.as_ref(),
            Type::Path { segments, args, .. }
                if segments[0].as_str() == "F" && matches!(args.as_slice(), [Type::Unit { .. }])
        ));
        assert!(matches!(
            ret.as_ref(),
            Type::Path { segments, args, .. }
                if segments[0].as_str() == "F" && matches!(args.as_slice(), [Type::Unit { .. }])
        ));
    }

    #[test]
    fn publication_reuses_a_canonicalized_function_scheme_alias_at_root() {
        reset_publication_validation_work();
        let module = parse("module main; type Scheme = [A] A -> A;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);

        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let root = store
            .begin_owner(None, scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let parent = store
            .begin_owner(
                Some(root),
                scope.clone(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(Some(parent), scope, GoalOwnerKind::NestedApplication, sp())
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let alias_span = Span::new(30, 31);
        let alias = store
            .scoped_type(
                child,
                InternedType::fresh(path(&["Scheme"], Vec::new())),
                alias_span,
            )
            .unwrap();

        let mut root_builder = PublicationBuilder::new(&store, &root_delta);
        let parent_slot = root_builder.reserve_child(&store, parent);
        let mut parent_builder = PublicationBuilder::new(&store, &parent_delta);
        let child_slot = parent_builder.reserve_child(&store, child);
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_scoped_function_scheme_position_type(
            alias_span,
            alias,
            HashSet::new(),
        );
        let (child_publication, outputs) = child_builder
            .finish()
            .close_child(&mut store, child_delta, parent, Vec::new(), &tcx)
            .unwrap();
        assert!(outputs.is_empty());
        parent_builder.fill_child(child_slot, child_publication);
        let (parent_publication, outputs) = parent_builder
            .finish()
            .close_child(&mut store, parent_delta, root, Vec::new(), &tcx)
            .unwrap();
        assert!(outputs.is_empty());
        root_builder.fill_child(parent_slot, parent_publication);
        let prepared = root_builder
            .finish()
            .prepare_root(&store, root_delta, Vec::new(), &tcx)
            .unwrap();
        assert_eq!(publication_validation_work(), (1, 2));
        let outputs = prepared.commit_without_rec_order(&mut store, tcx.elaborations);
        assert!(outputs.is_empty());
        drop(tcx);

        assert!(matches!(
            elaborations
                .position_index()
                .type_at("main", alias_span),
            Some(Type::Forall { body, .. }) if matches!(body.as_ref(), Type::Function { .. })
        ));
    }

    #[test]
    fn publication_retains_hkt_binder_after_value_layer_through_parent_to_root() {
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);

        let mut store = GoalStore::new();
        let root_scope = store.scope_from_type_ctx(&tcx);
        let root = store
            .begin_owner(None, root_scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let parent = store
            .begin_owner(
                Some(root),
                root_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();

        let mark = tcx.save();
        let param = TypeParam {
            name: "F".to_owned(),
            span: sp(),
            kind: Some(Kind::arrow_chain(1)),
        };
        let binder = tcx.push_retained_type_param(&param);
        let child = store
            .begin_owner(
                Some(parent),
                store.scope_from_type_ctx(&tcx),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();

        let inner = store
            .scoped_type(
                child,
                InternedType::fresh(function(
                    path(&["F"], vec![unit()]),
                    path(&["F"], vec![unit()]),
                )),
                sp(),
            )
            .unwrap();
        let inner = store.zonk_for_close(&child_delta, inner, &tcx).unwrap();
        let quantified = store.abstract_lexical_forall(inner, &binder);
        let leading = store
            .scoped_type(child, InternedType::fresh(unit()), sp())
            .unwrap();
        let leading = store.zonk_for_close(&child_delta, leading, &tcx).unwrap();
        let source_scheme = store
            .function_for_close(leading, quantified, 1, sp())
            .expect("a value layer may precede an explicit HKT binder");

        let mut child_outputs = store
            .close_prepared_owner(
                child_delta,
                vec![(source_scheme, GoalEscape::ToOwnerFunctionScheme(parent))],
                &tcx,
            )
            .expect("retain the mixed-position HKT scheme into its parent")
            .into_outputs();
        let child_scheme = match child_outputs.remove(0) {
            ClosedGoalOutput::RetainedFunctionScheme(output) => output,
            _ => panic!("the child must retain the complete mixed-position function scheme"),
        };
        assert!(child_outputs.is_empty());
        tcx.restore(mark);

        let parent_scheme = store
            .prepare_retained_function_scheme(&parent_delta, child_scheme, &tcx)
            .unwrap();
        let mut parent_outputs = store
            .close_prepared_owner(
                parent_delta,
                vec![(parent_scheme, GoalEscape::ToOwnerFunctionScheme(root))],
                &tcx,
            )
            .expect("retain the mixed-position HKT scheme into the root")
            .into_outputs();
        let parent_scheme = match parent_outputs.remove(0) {
            ClosedGoalOutput::RetainedFunctionScheme(output) => output,
            _ => panic!("the parent must retain the complete mixed-position function scheme"),
        };
        assert!(parent_outputs.is_empty());

        let root_scheme = store
            .prepare_retained_function_scheme(&root_delta, parent_scheme, &tcx)
            .unwrap();
        let mut root_outputs = store
            .close_prepared_owner(
                root_delta,
                vec![(root_scheme, GoalEscape::ClosedFunctionSchemeAt(root))],
                &tcx,
            )
            .expect("close the complete mixed-position HKT scheme at the root")
            .into_outputs();
        let root_scheme = root_outputs.remove(0);
        assert!(root_outputs.is_empty());

        let Type::Function { param, ret, .. } = function_scheme(&root_scheme).ty().as_type() else {
            panic!("the leading value layer must remain outside the explicit HKT binder")
        };
        assert!(matches!(param.as_ref(), Type::Unit { .. }));
        let Type::Forall {
            param: hkt, body, ..
        } = ret.as_ref()
        else {
            panic!("the explicit HKT binder must remain after the leading value layer")
        };
        assert_eq!(hkt.name, "F");
        assert_eq!(hkt.effective_kind(), Kind::arrow_chain(1));
        assert!(matches!(body.as_ref(), Type::Function { .. }));
    }

    #[test]
    fn publication_rejects_a_non_function_marked_as_a_retained_whole_scheme_before_mutation() {
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let non_function = store
            .scoped_type(child, InternedType::fresh_canonical(unit()), sp())
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &child_delta);
        builder.record_scoped_function_scheme_position_type(sp(), non_function, HashSet::new());

        assert_panics_with(
            || {
                let _ = builder.finish().close_child(
                    &mut store,
                    child_delta,
                    parent,
                    Vec::new(),
                    &ctx(),
                );
            },
            "planner used the whole-function-scheme publication escape for a non-function type",
        );

        assert_eq!(
            store.owner_state(child, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(store.owner_state(parent, sp()).unwrap().open_children, 1);
        let elaborations = Elaborations::new();
        assert!(
            elaborations
                .position_index()
                .type_at("<test>", sp())
                .is_none()
        );
    }

    #[test]
    fn publication_child_reservation_cannot_skip_its_direct_parent() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let parent = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();

        let root_delta = store.begin_delta(root, sp()).unwrap();
        let mut root_builder = PublicationBuilder::new(&store, &root_delta);
        assert_panics_with(
            || {
                root_builder.reserve_child(&store, child);
            },
            "publication child reservation must name the direct inference parent",
        );
    }

    #[test]
    fn publication_child_close_cannot_skip_its_direct_parent() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let parent = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.mark_value_at_type_slot(publication_site, 0);
        assert_panics_with(
            || {
                let _ = child_builder.finish().close_child(
                    &mut store,
                    child_delta,
                    root,
                    Vec::new(),
                    &context,
                );
            },
            "publication child close must retain into the direct inference parent",
        );
    }

    #[test]
    fn publication_child_cannot_mint_a_root_close() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.mark_value_at_type_slot(publication_site, 0);
        let mut elaborations = Elaborations::new();

        assert_panics_with(
            || {
                let _ = child_builder.finish().close_root(
                    &mut store,
                    child_delta,
                    Vec::new(),
                    &context,
                    &mut elaborations,
                );
            },
            "publication root close requires a root inference owner",
        );
        assert_eq!(
            store.owner_state(child, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(store.owner_state(parent, sp()).unwrap().open_children, 1);
        let elaborations = Elaborations::new();
        assert!(!elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
    }

    #[test]
    fn publication_root_rejects_a_different_owner_delta_before_mutation() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let publication_owner = owner(&mut store, RigidScope::new());
        let delta_owner = owner(&mut store, RigidScope::new());
        let publication_delta = store.begin_delta(publication_owner, sp()).unwrap();
        let wrong_delta = store.begin_delta(delta_owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &publication_delta);
        builder.mark_value_at_type_slot(publication_site, 0);
        let mut elaborations = Elaborations::new();

        assert_panics_with(
            || {
                let _ = builder.finish().close_root(
                    &mut store,
                    wrong_delta,
                    Vec::new(),
                    &context,
                    &mut elaborations,
                );
            },
            "publication plan and inference delta belong to different owners",
        );
        assert_eq!(
            store
                .owner_state(publication_owner, sp())
                .unwrap()
                .lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(
            store.owner_state(delta_owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        let elaborations = Elaborations::new();
        assert!(!elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
    }

    #[test]
    fn publication_child_rejects_a_sibling_delta_before_mutation() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let publication_owner = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let delta_owner = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let publication_delta = store.begin_delta(publication_owner, sp()).unwrap();
        let wrong_delta = store.begin_delta(delta_owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &publication_delta);
        builder.mark_value_at_type_slot(publication_site, 0);

        assert_panics_with(
            || {
                let _ = builder.finish().close_child(
                    &mut store,
                    wrong_delta,
                    parent,
                    Vec::new(),
                    &context,
                );
            },
            "publication plan and inference delta belong to different owners",
        );
        assert_eq!(
            store
                .owner_state(publication_owner, sp())
                .unwrap()
                .lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(
            store.owner_state(delta_owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(store.owner_state(parent, sp()).unwrap().open_children, 2);
        let elaborations = Elaborations::new();
        assert!(!elaborations.is_value_at_type_slot("<test>", publication_site.id, 0));
    }

    #[test]
    fn publication_module_key_is_derived_from_owner_authority() {
        let publication_site = crate::ast::ExpressionSite {
            id: crate::ast::ExpressionOccurrence::fresh().key(),
            span: sp(),
        };
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(
            &mut store,
            RigidScope::new().with_test_context(1, "authoritative/module"),
        );
        let delta = store.begin_delta(owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &delta);
        builder.mark_value_at_type_slot(publication_site, 0);
        let mut elaborations = Elaborations::new();
        let outputs = builder
            .finish()
            .close_root(&mut store, delta, Vec::new(), &context, &mut elaborations)
            .unwrap();
        assert!(outputs.is_empty());

        assert!(elaborations.is_value_at_type_slot("authoritative/module", publication_site.id, 0));
        assert!(!elaborations.is_value_at_type_slot("wrong/module", publication_site.id, 0));
    }

    #[test]
    fn publication_failure_leaves_semantic_state_and_elaborations_untouched() {
        reset_publication_validation_work();
        let context = ctx().with_kind(&["m", "Unary"], Kind::arrow_chain(1));
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(owner),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let semantic_goal = goal(&mut store, owner, Kind::Star);
        let semantic_open = store
            .scoped_goal_at(semantic_goal, Vec::new(), owner, sp())
            .unwrap();
        let concrete = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let malformed = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(product(path(&["m", "Unary"], Vec::new()), unit())),
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        store
            .constrain(&mut delta, semantic_open, concrete, sp(), &context)
            .unwrap();
        let mut builder = PublicationBuilder::new(&store, &delta);
        let child_slot = builder.reserve_child(&store, child);
        let valid_span = Span::new(40, 41);
        let valid = store
            .scoped_type(child, InternedType::fresh_canonical(unit()), valid_span)
            .unwrap();
        let mut child_builder = PublicationBuilder::new(&store, &child_delta);
        child_builder.record_position_type(valid_span, valid, HashSet::new());
        let (child_publication, outputs) = child_builder
            .finish()
            .close_child(&mut store, child_delta, owner, Vec::new(), &context)
            .unwrap();
        assert!(outputs.is_empty());
        builder.fill_child(child_slot, child_publication);
        builder.record_position_type(sp(), malformed, HashSet::new());
        let mut elaborations = Elaborations::new();

        let error = builder
            .finish()
            .close_root(&mut store, delta, Vec::new(), &context, &mut elaborations)
            .expect_err("malformed root publication should fail");
        let (diagnostic_span, diagnostic_message) = error.diag();
        assert_eq!(diagnostic_span, sp());
        assert!(diagnostic_message.contains("kind"));
        assert_eq!(publication_validation_work(), (2, 1));
        assert!(store.binding(semantic_goal, None).is_none());
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert!(
            elaborations
                .position_index()
                .type_at("<test>", sp())
                .is_none()
        );
        assert!(
            elaborations
                .position_index()
                .type_at("<test>", valid_span)
                .is_none()
        );
    }

    #[test]
    fn retained_publication_cannot_be_replayed_through_another_owner() {
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let sibling = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_value = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let (_, publication) = store
            .close_owner_with_publication(
                child_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::pending(child_value),
                    PublicationGoalEscape::ToOwner(parent),
                )],
                &context,
            )
            .unwrap();
        let mut outputs = publication.into_outputs();
        let ClosedPublicationOutput::Retained(retained) = outputs
            .pop()
            .expect("child close should retain one publication output")
        else {
            panic!("child publication unexpectedly closed at its child owner")
        };
        assert!(outputs.is_empty());
        let sibling_delta = store.begin_delta(sibling, sp()).unwrap();

        assert_panics_with(
            || {
                let _ = store.close_owner_with_publication(
                    sibling_delta,
                    Vec::new(),
                    vec![(
                        PublicationGoalInput::retained(retained),
                        PublicationGoalEscape::ToOwner(parent),
                    )],
                    &context,
                );
            },
            "retained publication output was replayed through a different inference owner",
        );
        assert_eq!(
            store.owner_state(sibling, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(store.owner_state(parent, sp()).unwrap().open_children, 1);
        assert_eq!(publication_validation_work(), (1, 0));
    }

    #[test]
    fn noncanonical_retained_publication_validates_until_canonical_proof_is_minted() {
        reset_publication_validation_work();
        let context = ctx();
        context.set_canonicalize_identity(false);
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let canonicalizer = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let parent = store
            .begin_owner(
                Some(canonicalizer),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let canonicalizer_delta = store.begin_delta(canonicalizer, sp()).unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let value = store
            .scoped_type(child, InternedType::fresh(unit()), sp())
            .unwrap();
        let (_, child_outputs) = store
            .close_owner_with_publication(
                child_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::pending(value),
                    PublicationGoalEscape::ToOwner(parent),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(child_output) = child_outputs
            .into_outputs()
            .pop()
            .expect("child close should retain the noncanonical publication")
        else {
            panic!("child publication unexpectedly closed")
        };
        let (_, parent_outputs) = store
            .close_owner_with_publication(
                parent_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::retained(child_output),
                    PublicationGoalEscape::ToOwner(canonicalizer),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(parent_output) = parent_outputs
            .into_outputs()
            .pop()
            .expect("parent close should retain the noncanonical publication")
        else {
            panic!("parent publication unexpectedly closed")
        };
        assert_eq!(publication_validation_work(), (2, 0));

        context.set_canonicalize_identity(true);
        let (_, canonicalizer_outputs) = store
            .close_owner_with_publication(
                canonicalizer_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::retained(parent_output),
                    PublicationGoalEscape::ToOwner(root),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(canonical_output) = canonicalizer_outputs
            .into_outputs()
            .pop()
            .expect("canonicalizing close should retain the publication")
        else {
            panic!("canonicalizing publication unexpectedly closed")
        };
        assert_eq!(publication_validation_work(), (3, 0));
        let (_, root_outputs) = store
            .close_owner_with_publication(
                root_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::retained(canonical_output),
                    PublicationGoalEscape::ClosedAt(root),
                )],
                &context,
            )
            .unwrap();
        assert!(matches!(
            root_outputs.into_outputs().as_slice(),
            [ClosedPublicationOutput::GoalFree(_)]
        ));
        assert_eq!(publication_validation_work(), (3, 1));
    }

    #[test]
    fn goal_free_retained_publication_rejects_an_opposite_root_kind_before_reuse() {
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let scheme = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(function(unit(), unit())),
                sp(),
            )
            .unwrap();
        let (_, child_outputs) = store
            .close_owner_with_publication(
                child_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::pending(scheme),
                    PublicationGoalEscape::ToOwnerFunctionScheme(root),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(child_output) = child_outputs
            .into_outputs()
            .pop()
            .expect("child close should retain the function scheme")
        else {
            panic!("child function-scheme publication unexpectedly closed")
        };
        assert_eq!(publication_validation_work(), (1, 0));

        assert_panics_with(
            || {
                let _ = store.close_owner_with_publication(
                    root_delta,
                    Vec::new(),
                    vec![(
                        PublicationGoalInput::retained(child_output),
                        PublicationGoalEscape::ClosedAt(root),
                    )],
                    &context,
                );
            },
            "validated publication output changed root kind",
        );
        assert_eq!(
            store.owner_state(root, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(publication_validation_work(), (1, 0));
    }

    #[test]
    fn goal_bearing_retained_publication_validates_until_solution_then_reuses_proof() {
        reset_publication_validation_work();
        let context = ctx();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, root, Kind::Star);
        let parent = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let mut parent_delta = store.begin_delta(parent, sp()).unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let occurrence = store
            .scoped_goal_at(inferred, Vec::new(), child, sp())
            .unwrap();
        let (_, child_outputs) = store
            .close_owner_with_publication(
                child_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::pending(occurrence),
                    PublicationGoalEscape::ToOwner(parent),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(child_output) = child_outputs
            .into_outputs()
            .pop()
            .expect("child close should retain the root goal")
        else {
            panic!("child publication unexpectedly became goal-free")
        };
        let open_parent = store
            .scoped_goal_at(inferred, Vec::new(), parent, sp())
            .unwrap();
        let concrete = store
            .scoped_type(
                parent,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut parent_delta, open_parent, concrete, sp(), &context)
            .unwrap();
        let (_, parent_outputs) = store
            .close_owner_with_publication(
                parent_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::retained(child_output),
                    PublicationGoalEscape::ToOwner(root),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(parent_output) = parent_outputs
            .into_outputs()
            .pop()
            .expect("parent close should retain the solved publication")
        else {
            panic!("parent publication unexpectedly closed before the root")
        };
        let (_, root_outputs) = store
            .close_owner_with_publication(
                root_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::retained(parent_output),
                    PublicationGoalEscape::ClosedAt(root),
                )],
                &context,
            )
            .unwrap();
        assert!(matches!(
            root_outputs.into_outputs().as_slice(),
            [ClosedPublicationOutput::GoalFree(_)]
        ));
        assert_eq!(publication_validation_work(), (2, 1));
    }

    #[test]
    fn goal_free_retained_publication_rejects_another_store_before_reuse() {
        reset_publication_validation_work();
        let context = ctx();
        let mut source_store = GoalStore::new();
        let parent = owner(&mut source_store, RigidScope::new());
        let child = source_store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_value = source_store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let child_delta = source_store.begin_delta(child, sp()).unwrap();
        let (_, publication) = source_store
            .close_owner_with_publication(
                child_delta,
                Vec::new(),
                vec![(
                    PublicationGoalInput::pending(child_value),
                    PublicationGoalEscape::ToOwner(parent),
                )],
                &context,
            )
            .unwrap();
        let ClosedPublicationOutput::Retained(retained) = publication
            .into_outputs()
            .pop()
            .expect("child close should retain one publication output")
        else {
            panic!("child publication unexpectedly closed at its child owner")
        };
        assert_eq!(publication_validation_work(), (1, 0));

        let mut target_store = GoalStore::new();
        let target = owner(&mut target_store, RigidScope::new());
        let target_delta = target_store.begin_delta(target, sp()).unwrap();
        assert_panics_with(
            || {
                let _ = target_store.close_owner_with_publication(
                    target_delta,
                    Vec::new(),
                    vec![(
                        PublicationGoalInput::retained(retained),
                        PublicationGoalEscape::ClosedAt(target),
                    )],
                    &context,
                );
            },
            "publication close carried rigid evidence from another store",
        );
        assert_eq!(
            target_store.owner_state(target, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        assert_eq!(publication_validation_work(), (1, 0));
    }

    #[test]
    fn generic_structural_publication_rejects_an_unproved_source_binder() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store
            .scoped_goal_at(inferred, Vec::new(), owner, sp())
            .unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &delta);
        let replacement = Expr::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::new(vec![
                SignatureParam::Type(TypeParam {
                    name: "A".to_owned(),
                    span: sp(),
                    kind: None,
                }),
                SignatureParam::Value(crate::ast::Param {
                    name: "value".to_owned(),
                    ty: Some(open.ty().clone_type()),
                    pattern: (),
                    meta: Meta::new(sp()),
                }),
            ]),
            ret_ty: Some(open.ty().clone_type()),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(sp()),
            }),
            meta: Meta::new(sp()),
            caps: (),
        };

        assert_panics_with(
            || builder.record_lowered_elaboration(&store, crate::ast::NodeId(44), replacement),
            "structural publication captured a goal beneath an unproved source binder",
        );
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
        let elaborations = Elaborations::new();
        assert!(
            elaborations
                .position_index()
                .type_at("<test>", sp())
                .is_none()
        );
    }

    #[test]
    fn lowered_elaboration_publication_materializes_closed_goal_types() {
        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let param_goal = goal(&mut store, owner, Kind::Star);
        let return_goal = goal(&mut store, owner, Kind::Star);
        let open_param = store
            .scoped_goal_at(param_goal, Vec::new(), owner, sp())
            .unwrap();
        let open_return = store
            .scoped_goal_at(return_goal, Vec::new(), owner, sp())
            .unwrap();
        let replacement = Expr::<Lowered>::FnExpr {
            occurrence: Default::default(),
            sig: crate::ast::Signature::new(vec![SignatureParam::Value(crate::ast::Param {
                name: "value".to_owned(),
                ty: Some(open_param.ty().clone_type()),
                pattern: (),
                meta: Meta::new(sp()),
            })]),
            ret_ty: Some(open_return.ty().clone_type()),
            body: Box::new(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(sp()),
            }),
            meta: Meta::new(sp()),
            caps: (),
        };
        let node_id = crate::ast::NodeId(42);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let mut builder = PublicationBuilder::new(&store, &delta);
        builder.record_lowered_elaboration(&store, node_id, replacement);

        let param_type = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        let return_type = store
            .scoped_type(
                owner,
                InternedType::fresh_canonical(path(&["m", "Bool"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut delta, open_param, param_type, sp(), &context)
            .unwrap();
        store
            .constrain(&mut delta, open_return, return_type, sp(), &context)
            .unwrap();
        let mut elaborations = Elaborations::new();
        let outputs = builder
            .finish()
            .close_root(&mut store, delta, Vec::new(), &context, &mut elaborations)
            .unwrap();
        assert!(outputs.is_empty());

        let original = Expr::RecOrder {
            occurrence: Default::default(),
            plan: Box::new(crate::ast::RecOrderPlan {
                tail_continuation: None,
                name: "same".to_owned(),
                disposition: crate::ast::RecOrderDisposition::Ordered(
                    crate::ast::RecOrderTypeFlow::ExpectedFromBody,
                ),
                annotation: None,
                value: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                body: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(sp()),
                }),
                runtime_ty: Type::Unit {
                    meta: Meta::new(sp()),
                },
            }),
            meta: Meta::new(sp()),
            ext: node_id,
        };
        let Some(RecordedElaboration::Lowered(Expr::FnExpr { sig, ret_ty, .. })) =
            elaborations.elaboration_for("<test>", &original)
        else {
            panic!("structural publication must commit its closed replacement")
        };
        let [SignatureParam::Value(param)] = sig.params.as_slice() else {
            panic!("structural publication changed its value parameter")
        };
        assert!(matches!(&param.ty, Some(Type::Path { segments, .. })
                if segments.last().unwrap().as_str() == "I32"));
        assert!(matches!(ret_ty, Some(Type::Path { segments, .. })
                if segments.last().unwrap().as_str() == "Bool"));
    }

    fn function_scheme(output: &ClosedGoalOutput) -> &GoalFreeFunctionSchemeOutput {
        match output {
            ClosedGoalOutput::FunctionScheme(output) => output,
            ClosedGoalOutput::Retained(_)
            | ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::GoalFree(_) => {
                panic!("expected a whole-function-scheme close output")
            }
        }
    }

    fn assert_panics(action: impl FnOnce()) {
        assert!(catch_unwind(AssertUnwindSafe(action)).is_err());
    }

    fn assert_panics_with(action: impl FnOnce(), expected: &str) {
        let payload = catch_unwind(AssertUnwindSafe(action))
            .expect_err("expected the internal contract violation to panic");
        let message = if let Some(message) = payload.downcast_ref::<String>() {
            message.as_str()
        } else if let Some(message) = payload.downcast_ref::<&str>() {
            message
        } else {
            panic!("panic payload was not a string")
        };
        assert!(
            message.contains(expected),
            "panic message `{message}` did not contain `{expected}`"
        );
    }

    fn assert_path(ty: &ScopedType, expected: &[&str], expected_args: usize) {
        let Type::Path { segments, args, .. } = ty.ty.as_type() else {
            panic!("expected a path, got {:?}", ty.ty.as_type())
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(args.len(), expected_args);
    }

    fn normalized_shadow_names() -> (String, String) {
        let module = parse(
            "module main;
             fn outer[A](seed: A) -> . {
                 let inner = .[A](value: A) { value };
                 ()
             }",
        )
        .expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let normalized = crate::pass::alpha_normalize::normalize_package(&package);
        let module = &normalized
            .package()
            .module("main")
            .expect("main module")
            .module;
        let [Item::FnDef(outer)] = module.items.as_slice() else {
            panic!("expected outer function");
        };
        let SignatureParam::Type(outer_param) = &outer.sig.params[0] else {
            panic!("expected outer type binder");
        };
        let Expr::Let { value, .. } = &outer.body else {
            panic!("expected let body");
        };
        let Expr::FnExpr { sig, .. } = value.as_ref() else {
            panic!("expected inner function");
        };
        let SignatureParam::Type(inner_param) = &sig.params[0] else {
            panic!("expected inner type binder");
        };
        (outer_param.name.clone(), inner_param.name.clone())
    }

    #[test]
    fn independently_allocated_domains_do_not_alias_local_indices() {
        let mut store = GoalStore::new();
        let first_owner = owner(&mut store, RigidScope::new());
        let first = goal(&mut store, first_owner, Kind::Star);
        let second_owner = owner(&mut store, RigidScope::new());
        let second = goal(&mut store, second_owner, Kind::Star);

        assert_eq!(first.owner().index(), second.owner().index());
        assert_eq!(first.slot().index(), second.slot().index());
        assert_ne!(first.domain(), second.domain());
        assert_ne!(first, second);
    }

    #[test]
    fn identical_local_indices_from_two_stores_are_distinct_capabilities() {
        let mut first_store = GoalStore::new();
        let first_owner = owner(&mut first_store, RigidScope::new());
        let first = goal(&mut first_store, first_owner, Kind::Star);
        let first_ty = first_store.scoped_goal(first, Vec::new(), sp()).unwrap();

        let mut second_store = GoalStore::new();
        let second_owner = owner(&mut second_store, RigidScope::new());
        let second = goal(&mut second_store, second_owner, Kind::Star);
        assert_eq!(first.domain().index(), second.domain().index());
        assert_eq!(first.owner().index(), second.owner().index());
        assert_eq!(first.slot().index(), second.slot().index());
        assert_ne!(first, second);

        let mut second_delta = second_store.begin_delta(second_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = second_store.constrain(
                &mut second_delta,
                first_ty,
                second_store.scoped_goal(second, Vec::new(), sp()).unwrap(),
                sp(),
                &ctx(),
            );
        });
    }

    #[test]
    fn goal_free_scopes_cannot_cross_goal_store_capabilities() {
        let mut first_store = GoalStore::new();
        let first_owner = owner(&mut first_store, RigidScope::new());
        let foreign_for_constraint = first_store
            .scoped_type(first_owner, InternedType::fresh(unit()), sp())
            .unwrap();
        let foreign_for_close = foreign_for_constraint.clone();

        let mut second_store = GoalStore::new();
        let second_owner = owner(&mut second_store, RigidScope::new());
        let local = second_store
            .scoped_type(second_owner, InternedType::fresh(unit()), sp())
            .unwrap();
        let mut delta = second_store.begin_delta(second_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = second_store.constrain(&mut delta, foreign_for_constraint, local, sp(), &ctx());
        });

        let close_delta = second_store.begin_delta(second_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = second_store.close_owner(
                close_delta,
                vec![(foreign_for_close, GoalEscape::ClosedAt(second_owner))],
                &ctx(),
            );
        });
    }

    #[test]
    fn one_store_cannot_mix_package_analysis_contexts_or_evaluators() {
        let mut store = GoalStore::new();
        let first_owner = owner(&mut store, RigidScope::new().with_test_context(1, "main"));

        assert_panics(|| {
            let _ = store.begin_owner(
                None,
                RigidScope::new().with_test_context(2, "main"),
                GoalOwnerKind::Application,
                sp(),
            );
        });

        let inferred = goal(&mut store, first_owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(first_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = store.constrain(
                &mut delta,
                open,
                scoped(
                    path(&["m", "I32"], Vec::new()),
                    RigidScope::new().with_test_context(1, "main"),
                ),
                sp(),
                &ctx().with_capability(2),
            );
        });
    }

    #[test]
    fn unused_store_allocation_does_not_change_closed_results_or_diagnostics() {
        fn solve(allocate_unused: bool) -> (Type<Lowered>, String) {
            if allocate_unused {
                let _unused = GoalStore::new();
            }
            let mut store = GoalStore::new();
            let solved_owner = owner(&mut store, RigidScope::new());
            let inferred = goal(&mut store, solved_owner, Kind::Star);
            let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
            let mut delta = store.begin_delta(solved_owner, sp()).unwrap();
            store
                .constrain(
                    &mut delta,
                    open.clone(),
                    scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .unwrap();
            let output = store
                .close_owner(
                    delta,
                    vec![(open, GoalEscape::ClosedAt(solved_owner))],
                    &ctx(),
                )
                .unwrap()
                .into_outputs()
                .remove(0);
            let result = into_goal_free(output).ty.clone_type();

            let mut unresolved_store = GoalStore::new();
            let unresolved_owner = owner(&mut unresolved_store, RigidScope::new());
            let _unresolved = goal(&mut unresolved_store, unresolved_owner, Kind::Star);
            let unresolved_delta = unresolved_store
                .begin_delta(unresolved_owner, sp())
                .unwrap();
            let unresolved_error = unresolved_store
                .close_owner(unresolved_delta, Vec::new(), &ctx())
                .unwrap_err();
            let diagnostic = unresolved_error.diag().1.to_owned();
            (result, diagnostic)
        }

        assert_eq!(solve(false), solve(true));
    }

    #[test]
    fn inactive_reserved_goal_does_not_block_owner_close() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let _reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();

        store
            .close_owner(delta, Vec::new(), &ctx())
            .expect("an unopened lexical layer leaves its reserved slot inactive");
    }

    #[test]
    fn reserved_goal_activation_is_exact_owner_one_shot_and_required_at_close() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let delta = store.begin_delta(root, sp()).unwrap();
        let (_goal, _open) = store
            .activate_reserved_goal_at(root, reserved, root, sp(), sp())
            .unwrap();

        assert_panics_with(
            || {
                let _ = store.activate_reserved_goal_at(root, reserved, root, sp(), sp());
            },
            "planner activated a reserved inference goal twice or activated an ordinary goal",
        );
        let error = store.close_owner(delta, Vec::new(), &ctx()).unwrap_err();
        assert_eq!(
            error.diag().1,
            "cannot infer type argument `Later` from the available type information"
        );
    }

    #[test]
    fn reserved_goal_rejects_wrong_owner_without_consuming_activation() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let child = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        assert_panics_with(
            || {
                let _ = store.activate_reserved_goal_at(child, reserved, child, sp(), sp());
            },
            "planner activated a reserved inference goal through another owner",
        );
        store
            .activate_reserved_goal_at(root, reserved, root, sp(), sp())
            .expect("a rejected foreign-owner attempt must leave the slot dormant");
    }

    #[test]
    fn reserved_goal_rejects_peer_and_foreign_use_sites_without_consuming_activation() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let reserved_owner = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let peer = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let foreign = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                reserved_owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();

        for use_site in [peer, foreign] {
            assert_panics_with(
                || {
                    let _ = store.activate_reserved_goal_at(
                        reserved_owner,
                        reserved,
                        use_site,
                        sp(),
                        sp(),
                    );
                },
                "planner activated a reserved inference goal at a peer, ancestor, or foreign owner",
            );
        }
        store
            .activate_reserved_goal_at(reserved_owner, reserved, reserved_owner, sp(), sp())
            .expect("rejected use sites must leave the slot dormant");
    }

    #[test]
    fn reserved_goal_activation_rejects_ancestors_and_adopts_descendant_scope() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let reserved_owner = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let reserved = store
            .reserve_goal(
                reserved_owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();

        assert_panics_with(
            || {
                let _ = store.activate_reserved_goal_at(reserved_owner, reserved, root, sp(), sp());
            },
            "planner activated a reserved inference goal at a peer, ancestor, or foreign owner",
        );

        let descendant_scope = RigidScope::new().with_binding("Child", Kind::Star, 42);
        let descendant = store
            .begin_owner(
                Some(reserved_owner),
                descendant_scope,
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let (_, occurrence) = store
            .activate_reserved_goal_at(reserved_owner, reserved, descendant, sp(), sp())
            .expect("a rejected ancestor use must leave the slot dormant");

        assert_eq!(
            occurrence.scope,
            store.owner_state(descendant, sp()).unwrap().scope,
            "the activated occurrence must retain its descendant use-site scope"
        );
    }

    #[test]
    fn activation_uses_distinct_diagnostic_and_occurrence_spans() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let reservation_span = Span::new(10, 11);
        let diagnostic_span = Span::new(20, 21);
        let occurrence_span = Span::new(30, 31);
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(reservation_span, GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();
        let (_goal, occurrence) = store
            .activate_reserved_goal_at(owner, reserved, owner, diagnostic_span, occurrence_span)
            .unwrap();

        assert_eq!(occurrence.ty.span(), occurrence_span);
        let error = store.close_owner(delta, Vec::new(), &ctx()).unwrap_err();
        assert_eq!(
            error.diag(),
            (
                diagnostic_span,
                "cannot infer type argument `Later` from the available type information",
            )
        );
    }

    #[test]
    fn activation_invalidates_a_prepared_close_before_any_write() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let ordinary = goal(&mut store, owner, Kind::Star);
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let open = store
            .scoped_goal_at(ordinary, Vec::new(), owner, sp())
            .unwrap();
        let concrete = scoped_canonical(&store, owner, path(&["m", "I32"], Vec::new()));
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(&mut delta, open, concrete, sp(), &ctx())
            .unwrap();
        let prepared = store
            .prepare_owner_with_publication(delta, Vec::new(), Vec::new(), &ctx())
            .unwrap();
        let (commit, _outputs, _publication) = prepared.into_parts();
        assert!(store.binding(ordinary, None).is_none());

        store
            .activate_reserved_goal_at(owner, reserved, owner, sp(), sp())
            .unwrap();
        assert_panics_with(
            || store.commit_prepared_owner(commit),
            "prepared inference close became stale before commit",
        );
        assert!(store.binding(ordinary, None).is_none());
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
    }

    #[test]
    fn inactive_reserved_goal_rejects_forged_write_during_replay() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let concrete = scoped_canonical(&store, owner, path(&["m", "I32"], Vec::new()));
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        delta.write_solution(
            reserved.0,
            GoalBinding {
                value: concrete,
                span: sp(),
            },
        );

        assert_panics_with(
            || {
                let _ = store.close_owner(delta, Vec::new(), &ctx());
            },
            "an inactive reserved inference goal entered solver state",
        );
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
    }

    #[test]
    fn inactive_reserved_goal_rejects_forged_policy_meet_during_replay() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        delta.write_policy_meet(
            reserved.0,
            GoalPolicyMeet {
                policy: GoalSolutionPolicy::Monotype,
                span: sp(),
            },
        );

        assert_panics_with(
            || {
                let _ = store.close_owner(delta, Vec::new(), &ctx());
            },
            "an inactive reserved inference goal entered solver state",
        );
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
    }

    #[test]
    fn activated_reserved_goal_can_be_solved_and_returned_from_a_descendant() {
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let root_delta = store.begin_delta(root, sp()).unwrap();
        let (goal, root_open) = store
            .activate_reserved_goal_at(root, reserved, root, sp(), sp())
            .unwrap();
        let child = store
            .begin_owner(
                Some(root),
                RigidScope::new(),
                GoalOwnerKind::RetainedValue,
                sp(),
            )
            .unwrap();
        let mut child_delta = store.begin_delta(child, sp()).unwrap();
        let open = store.scoped_goal_at(goal, Vec::new(), child, sp()).unwrap();
        let concrete = store
            .scoped_type(
                child,
                InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
                sp(),
            )
            .unwrap();
        store
            .constrain(&mut child_delta, open.clone(), concrete, sp(), &ctx())
            .unwrap();

        let retained = match store
            .close_owner(child_delta, vec![(open, GoalEscape::ToOwner(root))], &ctx())
            .unwrap()
            .into_outputs()
            .remove(0)
        {
            ClosedGoalOutput::Retained(output) => output.into_scoped_type(),
            ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::GoalFree(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                panic!("descendant output should retain into the activated goal owner")
            }
        };
        assert_path(&retained, &["m", "I32"], 0);

        let output = store
            .close_owner(
                root_delta,
                vec![(root_open, GoalEscape::ClosedAt(root))],
                &ctx(),
            )
            .unwrap()
            .into_outputs()
            .remove(0);
        assert_path(&into_goal_free(output), &["m", "I32"], 0);
    }

    #[test]
    fn inactive_reserved_goal_cannot_materialize_or_escape() {
        let mut materialize_store = GoalStore::new();
        let materialize_owner = owner(&mut materialize_store, RigidScope::new());
        let reserved = materialize_store
            .reserve_goal(
                materialize_owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        assert_panics(|| {
            let _ =
                materialize_store.scoped_goal_at(reserved.0, Vec::new(), materialize_owner, sp());
        });

        let mut output_store = GoalStore::new();
        let output_owner = owner(&mut output_store, RigidScope::new());
        let reserved = output_store
            .reserve_goal(
                output_owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let output = output_store
            .scoped_goal(reserved.0, Vec::new(), sp())
            .unwrap();
        let delta = output_store.begin_delta(output_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = output_store.close_owner(
                delta,
                vec![(output, GoalEscape::ClosedAt(output_owner))],
                &ctx(),
            );
        });
    }

    #[test]
    fn inactive_reserved_goal_cannot_escape_through_publication_input() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let output = store.scoped_goal(reserved.0, Vec::new(), sp()).unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();

        assert_panics_with(
            || {
                let _ = store.close_owner_with_publication(
                    delta,
                    Vec::new(),
                    vec![(
                        PublicationGoalInput::pending(output),
                        PublicationGoalEscape::ClosedAt(owner),
                    )],
                    &ctx(),
                );
            },
            "an inactive reserved inference goal entered solver state",
        );
        assert_eq!(
            store.owner_state(owner, sp()).unwrap().lifecycle,
            OwnerLifecycle::Open
        );
    }

    #[test]
    fn activation_persists_monotonically_after_a_failed_close() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let diagnostic_span = Span::new(20, 21);
        let reserved = store
            .reserve_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Later"),
            )
            .unwrap();
        let first_delta = store.begin_delta(owner, sp()).unwrap();
        store
            .activate_reserved_goal_at(owner, reserved, owner, diagnostic_span, sp())
            .unwrap();

        let first = store
            .close_owner(first_delta, Vec::new(), &ctx())
            .unwrap_err();
        assert_eq!(
            store
                .goal_state(reserved.0, sp())
                .unwrap()
                .close_requirement,
            GoalCloseRequirement::Required
        );
        let second_delta = store.begin_delta(owner, sp()).unwrap();
        let second = store
            .close_owner(second_delta, Vec::new(), &ctx())
            .unwrap_err();
        assert_eq!(first.diag(), second.diag());
        assert_eq!(
            second.diag(),
            (
                diagnostic_span,
                "cannot infer type argument `Later` from the available type information",
            )
        );
    }

    #[test]
    fn unresolved_goal_diagnostics_preserve_each_source_role_name_and_span() {
        let cases = [
            (
                GoalRole::TypeArgument,
                "T",
                Span::new(10, 12),
                "cannot infer type argument `T` from the available type information",
            ),
            (
                GoalRole::LambdaParameter,
                "value",
                Span::new(20, 25),
                "cannot infer lambda parameter `value` from the available type information",
            ),
        ];

        for (role, name, span, message) in cases {
            let mut store = GoalStore::new();
            let owner = owner(&mut store, RigidScope::new());
            store
                .alloc_goal(
                    owner,
                    Kind::Star,
                    GoalSolutionPolicy::Monotype,
                    GoalOrigin::named(span, role, name),
                )
                .unwrap();
            let delta = store.begin_delta(owner, span).unwrap();
            let error = store.close_owner(delta, Vec::new(), &ctx()).unwrap_err();
            assert_eq!(error.diag(), (span, message));
        }
    }

    #[test]
    fn rank_n_diagnostic_zonks_solved_goal_and_names_open_bare_and_applied_goals() {
        let context = ctx().with_display_mismatch_types();
        let mut store = GoalStore::new();
        let root = owner(&mut store, RigidScope::new());
        let solved = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Solved"),
            )
            .unwrap();
        let escaping = store
            .alloc_goal(
                root,
                Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "Result"),
            )
            .unwrap();
        let constructor = store
            .alloc_goal(
                root,
                Kind::arrow_chain(1),
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "F"),
            )
            .unwrap();
        let scope = store.owner_state(root, sp()).unwrap().scope.clone();
        let solved_occurrence = store.scoped_goal(solved, Vec::new(), sp()).unwrap();
        let escaping_occurrence = store.scoped_goal(escaping, Vec::new(), sp()).unwrap();
        let constructor_occurrence = store.scoped_goal(constructor, vec![unit()], sp()).unwrap();
        let mut delta = store.begin_delta(root, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                solved_occurrence.clone(),
                scoped(unit(), scope.clone()),
                sp(),
                &context,
            )
            .unwrap();

        let found = product(
            solved_occurrence.ty().clone_type(),
            product(
                constructor_occurrence.ty().clone_type(),
                forall(
                    "T",
                    function(path(&["T"], Vec::new()), path(&["T"], Vec::new())),
                ),
            ),
        );
        let expected = product(
            unit(),
            product(
                constructor_occurrence.ty().clone_type(),
                forall(
                    "B",
                    function(
                        path(&["B"], Vec::new()),
                        escaping_occurrence.ty().clone_type(),
                    ),
                ),
            ),
        );
        let error = store
            .constrain(
                &mut delta,
                scoped(found, scope.clone()),
                scoped(expected, scope),
                sp(),
                &context,
            )
            .unwrap_err();

        assert_eq!(
            error.diag().1,
            "type mismatch: expected `(. & (F(.) & ([B] B -> Result)))`, found `(. & (F(.) & ([T] T -> T)))`"
        );
    }

    #[test]
    fn diagnostic_carrier_never_becomes_solver_authority() {
        let context = ctx().with_display_mismatch_types();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let found = scoped_canonical(&store, owner, path(&["m", "I32"], Vec::new()));
        let expected = scoped_canonical(&store, owner, path(&["m", "String"], Vec::new()));
        let expected = ExpectedEquationOperand::with_written(
            expected,
            InternedType::fresh_canonical(path(&["m", "I32"], Vec::new())),
        );

        let error = store
            .constrain_equation(&mut delta, found, expected, sp(), &context)
            .expect_err("the distinct solver types must still mismatch");

        assert_eq!(
            error.diag().1,
            "type mismatch: expected `m.I32`, found `m.I32`",
            "only diagnostic rendering may observe the written expected type"
        );
        assert!(delta.writes.is_empty());
    }

    #[test]
    fn rec_order_style_expected_reuse_does_not_retain_equation_diagnostic() {
        let context = ctx().with_display_mismatch_types();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let runtime = scoped_canonical(&store, owner, path(&["m", "I32"], Vec::new()));
        let expected_solver = scoped_canonical(&store, owner, path(&["m", "I32"], Vec::new()));
        let expected_equation = ExpectedEquationOperand::with_written(
            expected_solver.clone(),
            InternedType::fresh_canonical(path(&["m", "Written"], Vec::new())),
        );

        // `finish_rec_order` retains this clean solver value while consuming
        // the separate equation operand.
        store
            .constrain_equation(&mut delta, runtime, expected_equation, sp(), &context)
            .expect("the recursive runtime type matches its solver expectation");
        let later_expected = scoped_canonical(&store, owner, path(&["m", "String"], Vec::new()));
        let error = store
            .constrain(&mut delta, expected_solver, later_expected, sp(), &context)
            .expect_err("the completed recursive value is not String");

        assert_eq!(
            error.diag().1,
            "type mismatch: expected `m.String`, found `m.I32`",
            "equation-only spelling must not survive into a later equation"
        );
    }

    #[test]
    fn failed_structural_equation_leaves_delta_unchanged() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let found = product(
            store
                .scoped_goal(inferred, Vec::new(), sp())
                .unwrap()
                .ty
                .clone_type(),
            path(&["m", "I32"], Vec::new()),
        );
        let expected = product(
            path(&["m", "I32"], Vec::new()),
            path(&["m", "String"], Vec::new()),
        );

        assert!(
            store
                .constrain(
                    &mut delta,
                    scoped(found, RigidScope::new()),
                    scoped(expected, RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert!(delta.writes.is_empty());
        let still_open = store
            .zonk_with_delta(
                &store.scoped_goal(inferred, Vec::new(), sp()).unwrap(),
                &delta,
            )
            .unwrap();
        assert!(matches!(still_open.ty().as_type(), Type::Goal { .. }));
    }

    #[test]
    fn canonical_composite_identity_survives_constraint_descent_and_zonking() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let canonical = ScopedType::from_parts(
            product(open.ty.clone_type(), path(&["m", "I32"], Vec::new())),
            open.scope.clone(),
            true,
        );
        let expected = ScopedType::from_parts(
            product(
                path(&["m", "String"], Vec::new()),
                path(&["m", "I32"], Vec::new()),
            ),
            RigidScope::new(),
            true,
        );
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        store
            .constrain(&mut delta, canonical.clone(), expected, sp(), &ctx())
            .unwrap();
        let closed = store
            .close_owner(
                delta,
                vec![(canonical, GoalEscape::ClosedAt(owner))],
                &ctx(),
            )
            .unwrap();
        assert!(goal_free(&closed.outputs()[0]).ty().identity_is_canonical());
    }

    fn alias_frontier_package() -> crate::pass::resolve::Package<Lowered> {
        let parsed = vec![(
            PathBuf::from("main.kio"),
            parse(
                "module main;
                 host type Actual;
                 host type Either[E][A];
                 type Alias = Actual;
                 type Id[A] = A;
                 type Unary[E] = Either(E);
                 type Twice[*F][A] = F(F(A));
                 type Pair = Actual & .;",
            )
            .expect("parse"),
        )];
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        package
    }

    #[test]
    fn nested_transparent_alias_unfolds_at_structural_path_frontier() {
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = owner(&mut store, scope.clone());
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        store
            .constrain(
                &mut delta,
                scoped(
                    product(path(&["Actual"], Vec::new()), unit()),
                    scope.clone(),
                ),
                scoped(product(path(&["Alias"], Vec::new()), unit()), scope),
                sp(),
                &tcx,
            )
            .expect("a transparent alias nested in a product must compare by its body");
    }

    #[test]
    fn finite_repeated_transparent_aliases_constrain_to_the_terminal_type() {
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = owner(&mut store, scope.clone());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let terminal = path(&["Actual"], Vec::new());
        let once = path(&["Id"], vec![terminal.clone()]);
        let twice = path(&["Id"], vec![once]);

        store
            .constrain(
                &mut delta,
                scoped(twice, scope.clone()),
                scoped(terminal, scope),
                sp(),
                &tcx,
            )
            .expect("finite repeated transparent aliases must reach the terminal type");
    }

    #[test]
    fn repeated_formal_uses_of_one_higher_kinded_alias_view_constrain() {
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = owner(&mut store, scope.clone());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let actual = path(&["Actual"], Vec::new());
        let unary = path(&["Unary"], vec![actual.clone()]);
        let source = path(&["Twice"], vec![unary, actual.clone()]);
        let inner = path(&["Either"], vec![actual.clone(), actual.clone()]);
        let expected = path(&["Either"], vec![actual, inner]);

        store
            .constrain(
                &mut delta,
                scoped(source, scope.clone()),
                scoped(expected, scope),
                sp(),
                &tcx,
            )
            .expect("each formal use of one saturated alias view must expand independently");
    }

    #[test]
    fn nested_transparent_alias_can_expand_to_structure_at_path_frontier() {
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = owner(&mut store, scope.clone());
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        store
            .constrain(
                &mut delta,
                scoped(
                    function(unit(), product(path(&["Actual"], Vec::new()), unit())),
                    scope.clone(),
                ),
                scoped(function(unit(), path(&["Pair"], Vec::new())), scope),
                sp(),
                &tcx,
            )
            .expect("a nested alias path must expand to its structural body");
    }

    #[test]
    fn projected_carrier_deeply_unfolds_cross_module_aliases_with_exact_rigid_scope() {
        let parsed = [
            (
                "owner.kio",
                "module owner;
                 pub type Pair[A, B] = A & B;
                 pub type Swap[A, B] = Pair(B, A);",
            ),
            (
                "consumer.kio",
                "module consumer; import owner as owner; type Rigid = .;",
            ),
        ]
        .into_iter()
        .map(|(path, source)| (PathBuf::from(path), parse(source).expect("parse")))
        .collect::<Vec<_>>();
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("consumer").expect("consumer module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("consumer environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let store = GoalStore::new();
        let scope = store
            .scope_from_type_ctx(&tcx)
            .with_binding("Rigid", Kind::Star, 77);
        let input = ScopedType::from_parts(
            path(
                &["owner", "Swap"],
                vec![path(&["Rigid"], Vec::new()), unit()],
            ),
            scope,
            false,
        );
        let mut builder = IsolatedSpecializationContextBuilder::new(&tcx);

        reset_deep_canonicalization_calls_for_test();
        let canonical = builder
            .canonicalize_carrier(&input, &tcx)
            .expect("deep projected carrier canonicalization");

        assert_eq!(deep_canonicalization_calls_for_test(), 1);
        let Type::Product { left, right, .. } = canonical.ty().as_type() else {
            panic!("nested transparent aliases did not expose their product carrier")
        };
        assert!(matches!(left.as_ref(), Type::Unit { .. }));
        assert!(
            matches!(right.as_ref(), Type::Path { segments, args, .. }
                if segments.iter().map(|segment| segment.as_str()).eq(["Rigid"])
                    && args.is_empty()),
            "the consumer's same-spelled alias captured an exact rigid binder: {right:?}"
        );
        assert!(canonical.ty().identity_is_canonical());
    }

    #[test]
    fn nested_nominal_spine_uses_only_root_canonicalization() {
        const DEPTH: usize = 64;

        let context = ctx();
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        store
            .constrain(
                &mut delta,
                scoped(nested_nominal(DEPTH), RigidScope::new()),
                scoped(nested_nominal(DEPTH), RigidScope::new()),
                sp(),
                &context,
            )
            .expect("the same nested nominal spine must constrain");

        assert_eq!(context.canonicalize_calls.get(), 2);
        assert_eq!(context.alias_frontier_probes.get(), DEPTH * 2);
    }

    #[test]
    fn canonical_reexport_children_are_not_reinterpreted_in_the_consumer_module() {
        let parsed = [
            (
                "base.kio",
                "module base;
                 pub host type Value;
                 pub newtype Box[A] : A {
                     pub constructor box;
                     pub projector unbox;
                 };",
            ),
            (
                "wrong.kio",
                "module wrong;
                 pub host type Value;
                 pub newtype Box[A, B] : A {
                     pub constructor box;
                     pub projector unbox;
                 };",
            ),
            (
                "owner.kio",
                "module owner;
                 import base as base;
                 pub type Pair = base.Box(base.Value) & base.Value;",
            ),
            (
                "consumer.kio",
                "module consumer; import owner as owner; import wrong as base; import wrong as wrong;",
            ),
        ]
        .into_iter()
        .map(|(path, source)| (PathBuf::from(path), parse(source).expect("parse")))
        .collect::<Vec<_>>();
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("consumer").expect("consumer module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("consumer environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let (owner_pair, identity_canonical) = canonicalize_for_comparison(
            &path(&["owner", "Pair"], Vec::new()),
            &tcx.env.alias_ctx(),
            false,
        );
        assert!(identity_canonical);
        let Type::Product {
            left: owner_left, ..
        } = owner_pair
        else {
            panic!("owner.Pair should expand to its canonical product body")
        };
        let canonical_child = *owner_left;

        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = store
            .begin_owner(None, scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                open.clone(),
                ScopedType::from_parts(canonical_child.clone(), scope.clone(), true),
                sp(),
                &tcx,
            )
            .unwrap();

        let mixed_skeleton =
            ScopedType::from_parts(product(open.ty.clone_type(), unit()), scope.clone(), false);
        let exact_expected = ScopedType::from_parts(
            product(canonical_child.clone(), unit()),
            scope.clone(),
            true,
        );
        store
            .constrain(
                &mut delta,
                mixed_skeleton,
                exact_expected,
                sp(),
                &tcx,
            )
            .expect(
                "kind checking a solved goal inside a raw composite must retain the provider identity",
            );
        let wrong_box = path(
            &["wrong", "Box"],
            vec![
                path(&["wrong", "Value"], Vec::new()),
                path(&["wrong", "Value"], Vec::new()),
            ],
        );
        assert!(
            store
                .constrain(
                    &mut delta,
                    ScopedType::from_parts(canonical_child.clone(), scope.clone(), true),
                    ScopedType::from_parts(wrong_box, scope, true),
                    sp(),
                    &tcx,
                )
                .is_err(),
            "the consumer's `base` alias must not capture canonical `base.Box`"
        );
        assert_eq!(delta.writes.len(), 1);

        let mut close_store = GoalStore::new();
        let close_scope = close_store.scope_from_type_ctx(&tcx);
        let close_owner = close_store
            .begin_owner(None, close_scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let inferred = goal(&mut close_store, close_owner, Kind::Star);
        let open = close_store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let mut close_delta = close_store.begin_delta(close_owner, sp()).unwrap();
        close_store
            .constrain(
                &mut close_delta,
                open.clone(),
                ScopedType::from_parts(canonical_child, close_scope.clone(), true),
                sp(),
                &tcx,
            )
            .unwrap();
        let mixed =
            ScopedType::from_parts(product(open.ty().clone_type(), unit()), close_scope, false);
        let closed = close_store
            .close_owner(
                close_delta,
                vec![(mixed, GoalEscape::ClosedAt(close_owner))],
                &tcx,
            )
            .unwrap();
        let published = goal_free(&closed.outputs()[0]).ty();
        assert!(published.identity_is_canonical());
        let consumer_spelling = product(
            path(&["base", "Box"], vec![path(&["base", "Value"], Vec::new())]),
            unit(),
        );
        assert!(
            !type_equiv_state(
                published.as_type(),
                &consumer_spelling,
                &tcx.env.alias_ctx(),
                published.identity_is_canonical(),
                false,
            ),
            "closing a mixed goal/composite must not let the consumer recapture a provider-canonical child"
        );
    }

    #[test]
    fn many_independent_equations_accumulate_without_copying_the_existing_delta() {
        const EQUATIONS: usize = 4_096;

        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let goals = (0..EQUATIONS)
            .map(|_| goal(&mut store, owner, Kind::Star))
            .collect::<Vec<_>>();
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        for inferred in goals {
            store
                .constrain(
                    &mut delta,
                    store.scoped_goal(inferred, Vec::new(), sp()).unwrap(),
                    scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .unwrap();
        }

        assert_eq!(delta.writes.len(), EQUATIONS);
        store.close_owner(delta, Vec::new(), &ctx()).unwrap();
    }

    #[test]
    fn deep_spine_constraints_do_not_reintern_each_remaining_subtree() {
        fn product_spine(depth: usize, leaf: Type<Lowered>) -> Type<Lowered> {
            (0..depth).fold(leaf, |tail, _| product(unit(), tail))
        }

        for depth in [64, 128] {
            let mut store = GoalStore::new();
            let owner = owner(&mut store, RigidScope::new());
            let inferred = goal(&mut store, owner, Kind::Star);
            let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
            let found = scoped(
                product_spine(depth, open.ty().clone_type()),
                RigidScope::new(),
            );
            let expected = scoped(
                product_spine(depth, path(&["m", "I32"], Vec::new())),
                RigidScope::new(),
            );
            let mut delta = store.begin_delta(owner, sp()).unwrap();

            store
                .constrain(&mut delta, found.clone(), expected, sp(), &ctx())
                .unwrap();
            let closed = store
                .close_owner(delta, vec![(found, GoalEscape::ClosedAt(owner))], &ctx())
                .unwrap();
            assert!(!type_contains_goal(
                goal_free(&closed.outputs()[0]).ty().as_type()
            ));
        }
    }

    #[test]
    fn hkt_head_solution_reapplies_ordinary_arguments_once() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let arg = path(&["m", "I32"], Vec::new());
        let applied = store.scoped_goal(head, vec![arg.clone()], sp()).unwrap();
        let concrete = scoped(path(&["m", "Box"], vec![arg.clone()]), RigidScope::new());

        store
            .constrain(&mut delta, applied.clone(), concrete, sp(), &ctx())
            .unwrap();
        let closed = store
            .close_owner(
                delta,
                vec![(applied.clone(), GoalEscape::ClosedAt(owner))],
                &ctx(),
            )
            .unwrap();
        let zonked = goal_free(&closed.outputs()[0]);
        assert_path(&zonked.value, &["m", "Box"], 1);
        let Type::Path { args, .. } = zonked.ty().as_type() else {
            unreachable!()
        };
        assert_eq!(args[0], arg);
    }

    #[test]
    fn partially_applied_hkt_solution_keeps_existing_and_use_site_args() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let first = path(&["m", "I32"], Vec::new());
        let second = path(&["m", "String"], Vec::new());
        let open_head = store.scoped_goal(head, Vec::new(), sp()).unwrap();
        store
            .constrain(
                &mut delta,
                open_head,
                scoped(path(&["m", "Pair"], vec![first.clone()]), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let applied = store.scoped_goal(head, vec![second.clone()], sp()).unwrap();
        let closed = store
            .close_owner(delta, vec![(applied, GoalEscape::ClosedAt(owner))], &ctx())
            .unwrap();
        let zonked = goal_free(&closed.outputs()[0]);

        assert_path(&zonked.value, &["m", "Pair"], 2);
        let Type::Path { args, .. } = zonked.ty().as_type() else {
            unreachable!()
        };
        assert_eq!(args, &[first, second]);
    }

    #[test]
    fn applied_goal_infers_a_partially_applied_concrete_constructor() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let first = path(&["m", "I32"], Vec::new());
        let second = path(&["m", "String"], Vec::new());
        let applied = store.scoped_goal(head, vec![second.clone()], sp()).unwrap();
        let concrete = scoped(
            path(&["m", "Pair"], vec![first.clone(), second.clone()]),
            RigidScope::new(),
        );
        let mut delta = store.begin_delta(owner, sp()).unwrap();

        store
            .constrain(&mut delta, applied.clone(), concrete, sp(), &ctx())
            .unwrap();
        let closed = store
            .close_owner(delta, vec![(applied, GoalEscape::ClosedAt(owner))], &ctx())
            .unwrap();
        let Type::Path { segments, args, .. } = goal_free(&closed.outputs()[0]).ty().as_type()
        else {
            panic!("expected a concrete partial-constructor solution")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["m", "Pair"]
        );
        assert_eq!(args, &[first, second]);
    }

    #[test]
    fn applied_goal_infers_a_partially_applied_goal_constructor_in_either_order() {
        fn solve(reverse: bool) -> Type<Lowered> {
            let mut store = GoalStore::new();
            let owner = owner(&mut store, RigidScope::new());
            let unary = goal(&mut store, owner, Kind::arrow_chain(1));
            let binary = goal(&mut store, owner, Kind::arrow_chain(2));
            let first = path(&["m", "I32"], Vec::new());
            let second = path(&["m", "String"], Vec::new());
            let applied_unary = store
                .scoped_goal(unary, vec![second.clone()], sp())
                .unwrap();
            let applied_binary = store
                .scoped_goal(binary, vec![first, second], sp())
                .unwrap();
            let mut delta = store.begin_delta(owner, sp()).unwrap();

            let (found, expected) = if reverse {
                (applied_binary, applied_unary.clone())
            } else {
                (applied_unary.clone(), applied_binary)
            };
            store
                .constrain(&mut delta, found, expected, sp(), &ctx())
                .unwrap();
            store
                .constrain(
                    &mut delta,
                    store.scoped_goal(binary, Vec::new(), sp()).unwrap(),
                    scoped(path(&["m", "Pair"], Vec::new()), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .unwrap();
            into_goal_free(
                store
                    .close_owner(
                        delta,
                        vec![(applied_unary, GoalEscape::ClosedAt(owner))],
                        &ctx(),
                    )
                    .unwrap()
                    .into_outputs()
                    .remove(0),
            )
            .ty
            .clone_type()
        }

        let forward = solve(false);
        let reverse = solve(true);
        assert_eq!(forward, reverse);
        let Type::Path { segments, args, .. } = forward else {
            panic!("expected a goal-headed partial-constructor solution")
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["m", "Pair"]
        );
        assert_eq!(
            args,
            [
                path(&["m", "I32"], Vec::new()),
                path(&["m", "String"], Vec::new())
            ]
        );
    }

    #[test]
    fn solved_hkt_does_not_drop_noncanonical_use_site_identity() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let open_head = store.scoped_goal(head, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                open_head,
                ScopedType::from_parts(path(&["m", "Box"], Vec::new()), RigidScope::new(), true),
                sp(),
                &ctx(),
            )
            .unwrap();
        let applied = ScopedType::from_parts(
            Type::Goal {
                goal: head,
                args: vec![path(&["alias", "Imported"], Vec::new())],
                meta: Meta::new(sp()),
                ext: (),
            },
            RigidScope::new(),
            false,
        );
        let zonked = store
            .zonk_with_delta(&applied.stamped_for_test(store.nonce), &delta)
            .unwrap();

        assert!(!zonked.ty.identity_is_canonical());
    }

    #[test]
    fn applied_goal_requires_exact_exposed_arity() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let applied = store
            .scoped_goal(head, vec![path(&["m", "I32"], Vec::new())], sp())
            .unwrap();
        let pair = scoped(
            path(
                &["m", "Pair"],
                vec![
                    path(&["m", "I32"], Vec::new()),
                    path(&["m", "String"], Vec::new()),
                ],
            ),
            RigidScope::new(),
        );
        assert!(
            store
                .constrain(&mut delta, applied, pair, sp(), &ctx())
                .is_err()
        );
        assert!(delta.writes.is_empty());
    }

    #[test]
    fn applied_goal_argument_kinds_are_checked_without_requested_outputs() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let open_head = store.scoped_goal(head, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                open_head,
                scoped(path(&["m", "Box"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let before = delta.writes.clone();
        let invalid_argument = path(&["m", "Box"], Vec::new());
        let applied = store
            .scoped_goal(head, vec![invalid_argument.clone()], sp())
            .unwrap();
        let concrete = scoped(
            path(&["m", "Box"], vec![invalid_argument]),
            RigidScope::new(),
        );

        assert!(
            store
                .constrain(&mut delta, applied, concrete, sp(), &ctx())
                .is_err()
        );
        assert_eq!(
            delta.writes.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        store
            .close_owner(delta, Vec::new(), &ctx())
            .expect("the valid head equation closes without relying on output validation");
    }

    #[test]
    fn a_solved_applied_goal_self_equation_still_checks_argument_kinds() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let open_head = store.scoped_goal(head, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                open_head,
                scoped(path(&["m", "Box"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let before = delta.writes.clone();
        let invalid = store
            .scoped_goal(head, vec![path(&["m", "Box"], Vec::new())], sp())
            .unwrap();

        assert!(
            store
                .constrain(&mut delta, invalid.clone(), invalid, sp(), &ctx())
                .is_err()
        );
        assert_eq!(
            delta.writes.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        store
            .close_owner(delta, Vec::new(), &ctx())
            .expect("the valid head solution closes with no requested outputs");
    }

    #[test]
    fn close_accepts_a_saturated_alias_with_the_production_kind_context() {
        let module = parse("module main; type Alias[A] = A;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = store
            .begin_owner(None, scope.clone(), GoalOwnerKind::Application, sp())
            .unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();
        let output = scoped(path(&["Alias"], vec![unit()]), scope);

        store
            .close_owner(delta, vec![(output, GoalEscape::ClosedAt(owner))], &tcx)
            .expect("a fully applied transparent alias is a valid closed output");
    }

    #[test]
    fn canonical_goal_frontier_keeps_same_spelled_rigid_binder_opaque() {
        let module = parse("module main; type A = .; type Id[T] = T;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio'");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 41)]);

        assert!(
            GoalTypeContext::canonicalize_alias_frontier(
                &tcx,
                &path(&["A"], Vec::new()),
                true,
                &scope,
            )
            .is_none(),
            "canonical identity must not discard the exact rigid scope before alias probing"
        );
        let (expanded, _) = GoalTypeContext::canonicalize_alias_frontier(
            &tcx,
            &path(&["Id"], vec![path(&["A"], Vec::new())]),
            true,
            &scope,
        )
        .expect("the saturated alias frontier expands");
        assert_eq!(
            expanded,
            path(&["A"], Vec::new()),
            "a head-only miss optimization must still protect rigid arguments when an alias fires"
        );
    }

    #[test]
    fn malformed_recursive_alias_cutoff_is_not_reopened_by_goal_store_frontier() {
        let parsed = vec![(
            PathBuf::from("main.kio"),
            parse("module main; type A = A & .;").expect("parse"),
        )];
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let scope = RigidScope::from_bindings([("Unused".to_owned(), Kind::Star, 1)]);

        assert!(
            GoalTypeContext::canonicalize_alias_frontier(
                &tcx,
                &path(&["A"], Vec::new()),
                false,
                &scope,
            )
            .is_none(),
            "a nonempty exact rigid scope must not reopen a malformed cutoff"
        );
    }

    #[test]
    fn alias_frontier_probes_do_not_snapshot_the_entire_rigid_scope() {
        const BINDERS: usize = 256;
        const PROBES: usize = 256;

        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let scope = RigidScope::from_bindings((0..BINDERS).map(|index| {
            (
                format!("B{index}"),
                Kind::Star,
                u64::try_from(index + 1).expect("test binder identity fits in u64"),
            )
        }));
        let nominal = path(&["Actual"], Vec::new());

        reset_rigid_scope_name_snapshots();
        for _ in 0..PROBES {
            assert!(
                GoalTypeContext::canonicalize_alias_frontier(&tcx, &nominal, true, &scope)
                    .is_none()
            );
        }
        assert_eq!(
            rigid_scope_name_snapshots(),
            0,
            "one head probe must use direct rigid membership rather than clone every binder name"
        );
    }

    #[test]
    fn alias_frontier_hits_use_exact_rigid_membership_without_scope_snapshots() {
        const BINDERS: usize = 256;
        const HITS: usize = 256;

        let parsed = vec![(
            PathBuf::from("main.kio"),
            parse(
                "module main;
                 host type Actual;
                 type A = Actual;
                 type Id[T] = T;",
            )
            .expect("parse"),
        )];
        let (modules, _) = FullPipeline::lower_package(parsed, None).expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        package.resolve_imports().expect("resolve imports");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let scope = RigidScope::from_bindings(
            std::iter::once(("A".to_owned(), Kind::Star, 1)).chain((1..BINDERS).map(|index| {
                (
                    format!("B{index}"),
                    Kind::Star,
                    u64::try_from(index + 1).expect("test binder identity fits in u64"),
                )
            })),
        );
        let rigid = path(&["A"], Vec::new());
        let alias = path(&["Id"], vec![rigid.clone()]);

        reset_rigid_scope_name_snapshots();
        for _ in 0..HITS {
            let (expanded, _) =
                GoalTypeContext::canonicalize_alias_frontier(&tcx, &alias, true, &scope)
                    .expect("the saturated alias frontier expands");
            assert_eq!(
                expanded, rigid,
                "the exact rigid argument must not become its same-spelled module alias"
            );
        }
        assert_eq!(
            rigid_scope_name_snapshots(),
            0,
            "alias hits must query exact rigid membership instead of snapshotting the scope"
        );
        assert_eq!(
            rigid_scope_names_copied(),
            0,
            "alias-hit work must not grow with every unrelated rigid binder"
        );
    }

    #[test]
    fn canonicalization_and_mismatch_borrow_exact_rigid_scopes() {
        const BINDERS: usize = 256;
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let scope = RigidScope::from_bindings((0..BINDERS).map(|index| {
            (
                format!("B{index}"),
                Kind::Star,
                u64::try_from(index + 1).expect("test binder identity fits in u64"),
            )
        }));

        reset_rigid_scope_name_snapshots();
        let (canonical, _) = GoalTypeContext::canonicalize(
            &tcx,
            &path(&["Id"], vec![path(&["B0"], Vec::new())]),
            false,
            &scope,
        );
        assert_eq!(canonical, path(&["B0"], Vec::new()));
        let error = GoalTypeContext::mismatch(
            &tcx,
            &scoped(path(&["B0"], Vec::new()), scope.clone()),
            &scoped(unit(), scope),
            sp(),
        );
        assert!(error.diagnostic().message.contains("type mismatch"));
        assert_eq!(
            rigid_scope_name_snapshots(),
            0,
            "canonicalization and diagnostics must borrow exact binder views"
        );
        assert_eq!(rigid_scope_names_copied(), 0);
    }

    #[test]
    fn rigid_scope_shadowing_preserves_exact_parent_and_restoration() {
        let outer = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let inner = outer.clone().with_binding("A", Kind::arrow_chain(1), 2);

        assert_eq!(
            outer.binding("A").map(|binding| &binding.identity),
            Some(&RigidIdentity::Ambient(
                super::super::env::TypeBinderId::for_test(1)
            ))
        );
        assert_eq!(
            inner.binding("A").map(|binding| &binding.identity),
            Some(&RigidIdentity::Ambient(
                super::super::env::TypeBinderId::for_test(2)
            ))
        );
        assert!(inner.extends(&outer));
        assert!(!outer.extends(&inner));

        let visible = inner
            .bindings
            .frame("A")
            .expect("the inner binding is visible");
        assert_eq!(
            visible.shadowed().map(|frame| &frame.value().identity),
            Some(&RigidIdentity::Ambient(
                super::super::env::TypeBinderId::for_test(1)
            ))
        );
        assert_eq!(
            outer.binding("A").map(|binding| &binding.identity),
            Some(&RigidIdentity::Ambient(
                super::super::env::TypeBinderId::for_test(1)
            )),
            "retaining the parent root restores the exact outer proof"
        );
    }

    #[test]
    fn rigid_scope_merge_reuses_ancestry_and_rejects_sibling_conflicts() {
        let parent = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 1)]);
        let left = parent.clone().with_binding("B", Kind::Star, 2);
        let right = parent.clone().with_binding("C", Kind::Star, 3);
        let merged = left.merged(&right).expect("compatible sibling scopes");

        assert!(merged.extends(&left));
        assert!(merged.extends(&right));
        assert_eq!(merged.binding("A"), parent.binding("A"));
        assert_eq!(merged.binding("B"), left.binding("B"));
        assert_eq!(merged.binding("C"), right.binding("C"));

        let shadow_left = parent.clone().with_binding("A", Kind::Star, 4);
        let shadow_right = parent.with_binding("A", Kind::Star, 5);
        assert!(shadow_left.merged(&shadow_right).is_err());
    }

    #[test]
    fn scope_from_type_ctx_keeps_the_innermost_same_spelled_exact_binder() {
        let package = alias_frontier_package();
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("main environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        tcx.push_type_param_kinded("A", Kind::Star, Span::new(1, 2));
        tcx.push_type_param_kinded("A", Kind::arrow_chain(1), Span::new(3, 4));
        let store = GoalStore::new();

        let scope = store.scope_from_type_ctx(&tcx);
        assert_eq!(scope.kind("A"), Some(Kind::arrow_chain(1)));
        let visible = scope
            .bindings
            .frame("A")
            .expect("the innermost exact binder is visible");
        assert_eq!(visible.value().kind, Kind::arrow_chain(1));
        assert_eq!(
            visible.shadowed().map(|frame| frame.value().kind.clone()),
            Some(Kind::Star)
        );
    }

    #[test]
    fn canonical_same_source_kind_is_terminal_over_a_package_path_collision() {
        let source =
            parse("module aux; host type Scalar; host type Box[T];").expect("parse source module");
        let (mut source_modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("aux.kio"), source)], None)
                .expect("lower source module");
        let (_, source_module) = source_modules.pop().expect("one lowered source module");

        let indexed = parse("module aux; host type Scalar; host type Box[A][B];")
            .expect("parse indexed module");
        let (indexed_modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("aux.kio"), indexed)], None)
                .expect("lower indexed module");
        let package = Package::build(Path::new(""), indexed_modules, None)
            .expect("build conflicting package shell");
        let env = super::super::ModuleEnv::build(&source_module, None, None, Some(&package))
            .expect("source module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = GoalStore::new();
        let scope = store.scope_from_type_ctx(&tcx);
        let owner = store
            .begin_owner(None, scope.clone(), GoalOwnerKind::Application, sp())
            .expect("begin owner");
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store
            .scoped_goal(inferred, Vec::new(), sp())
            .expect("open kind-star goal");
        let mut delta = store.begin_delta(owner, sp()).expect("begin delta");
        let output = ScopedType::from_parts(
            path(&["aux", "Box"], vec![path(&["aux", "Scalar"], Vec::new())]),
            scope,
            true,
        );
        store
            .constrain(&mut delta, open.clone(), output, sp(), &tcx)
            .expect("the exact source Box has kind `* -> *`, not the indexed decoy's kind");

        let closed = store
            .close_owner(delta, vec![(open, GoalEscape::ClosedAt(owner))], &tcx)
            .expect("the source module supplies both canonical nominal kinds");
        assert!(goal_free(&closed.outputs()[0]).ty().identity_is_canonical());
    }

    #[test]
    fn retained_binder_proofs_restore_exact_identity_and_abstract_at_source_positions() {
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let mark = tcx.save();
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };

        let first_binder = tcx.push_retained_type_param(&param);
        let mut store = GoalStore::new();
        let first_scope = store.scope_from_type_ctx(&tcx);
        tcx.restore(mark);

        let same_fields_distinct_param = param.clone();
        let second_binder = tcx.push_retained_type_param(&same_fields_distinct_param);
        assert_ne!(first_binder.id(), second_binder.id());
        assert!(
            !std::ptr::eq(first_binder.param(), second_binder.param()),
            "identical parameter fields do not make two source declarations the same binder"
        );
        let second_scope = store.scope_from_type_ctx(&tcx);
        assert_ne!(
            first_scope.binding("A"),
            second_scope.binding("A"),
            "same spelling and span must not confer the same binder authority"
        );
        tcx.restore(mark);

        tcx.push_retained_type_binder(&first_binder);
        let restored_scope = store.scope_from_type_ctx(&tcx);
        assert_eq!(first_scope.binding("A"), restored_scope.binding("A"));

        let owner = store
            .begin_owner(None, restored_scope, GoalOwnerKind::LambdaFrontier, sp())
            .unwrap();
        let delta = store.begin_delta(owner, sp()).unwrap();
        let inner = store
            .scoped_type(
                owner,
                InternedType::fresh(function(unit(), path(&["A"], Vec::new()))),
                sp(),
            )
            .unwrap();
        let prepared = store.zonk_for_close(&delta, inner, &tcx).unwrap();

        assert_panics(|| {
            let _ = store.abstract_lexical_forall(prepared.clone(), &second_binder);
        });

        let leading = store.abstract_lexical_forall(prepared, &first_binder);
        let plain = store
            .scoped_type(owner, InternedType::fresh(unit()), sp())
            .unwrap();
        let prepared_plain = store.zonk_for_close(&delta, plain, &tcx).unwrap();
        let mid_signature = store
            .function_for_close(prepared_plain.clone(), leading.clone(), 1, sp())
            .unwrap();
        let closed = store
            .close_prepared_owner(
                delta,
                vec![
                    (leading, GoalEscape::ClosedAt(owner)),
                    (mid_signature, GoalEscape::ClosedAt(owner)),
                    (prepared_plain, GoalEscape::ClosedAt(owner)),
                ],
                &tcx,
            )
            .unwrap();

        assert!(matches!(
            goal_free(&closed.outputs()[0]).ty().as_type(),
            Type::Forall { .. }
        ));
        let Type::Function { ret, .. } = goal_free(&closed.outputs()[1]).ty().as_type() else {
            panic!("expected the leading value parameter")
        };
        assert!(
            matches!(ret.as_ref(), Type::Forall { .. }),
            "the explicit binder must remain at its mid-signature source position"
        );
        assert!(
            matches!(
                goal_free(&closed.outputs()[2]).ty().as_type(),
                Type::Unit { .. }
            ),
            "closing an owner without an explicit binder must not synthesize a forall"
        );
    }

    #[test]
    fn lexical_forall_may_retain_an_ancestor_goal_until_the_parent_closes() {
        let module = parse("module main;").expect("parse");
        let (modules, _) =
            FullPipeline::lower_package(vec![(PathBuf::from("main.kio"), module)], None)
                .expect("lower Kio");
        let package =
            Package::build(Path::new(""), modules, None).expect("build resolved package shell");
        let module = &package.module("main").expect("main module").module;
        let env = super::super::ModuleEnv::build(module, None, None, Some(&package))
            .expect("module environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);

        let mut store = GoalStore::new();
        let outer_scope = store.scope_from_type_ctx(&tcx);
        let outer = store
            .begin_owner(None, outer_scope, GoalOwnerKind::Application, sp())
            .unwrap();
        let ancestor = store
            .alloc_goal(
                outer,
                Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "A"),
            )
            .unwrap();

        let mark = tcx.save();
        let param = TypeParam {
            name: "A".to_owned(),
            span: sp(),
            kind: None,
        };
        let binder = tcx.push_retained_type_param(&param);
        let child_scope = store.scope_from_type_ctx(&tcx);
        let child = store
            .begin_owner(
                Some(outer),
                child_scope,
                GoalOwnerKind::LambdaFrontier,
                sp(),
            )
            .unwrap();
        let child_delta = store.begin_delta(child, sp()).unwrap();
        let parameter = store
            .scoped_type(child, InternedType::fresh(path(&["A"], Vec::new())), sp())
            .unwrap();
        let result = store
            .scoped_goal_at(ancestor, Vec::new(), child, sp())
            .unwrap();
        let parameter = store.zonk_for_close(&child_delta, parameter, &tcx).unwrap();
        let result = store.zonk_for_close(&child_delta, result, &tcx).unwrap();
        let function = store
            .function_for_close(parameter, result, 1, sp())
            .unwrap();
        let scheme = store.abstract_lexical_forall(function, &binder);
        let child_closed = store
            .close_prepared_owner(
                child_delta,
                vec![(scheme, GoalEscape::ToOwner(outer))],
                &tcx,
            )
            .unwrap();
        tcx.restore(mark);

        let retained = match child_closed.into_outputs().remove(0) {
            ClosedGoalOutput::Retained(output) => output.into_scoped_type(),
            ClosedGoalOutput::RetainedFunctionScheme(_)
            | ClosedGoalOutput::GoalFree(_)
            | ClosedGoalOutput::FunctionScheme(_) => {
                panic!("child output should retain the ancestor result goal")
            }
        };
        assert!(matches!(retained.ty().as_type(), Type::Forall { .. }));

        let mut outer_delta = store.begin_delta(outer, sp()).unwrap();
        let open = store.scoped_goal(ancestor, Vec::new(), sp()).unwrap();
        let unit = store
            .scoped_type(outer, InternedType::fresh(unit()), sp())
            .unwrap();
        store
            .constrain(&mut outer_delta, open, unit, sp(), &tcx)
            .unwrap();
        let outer_closed = store
            .close_owner(
                outer_delta,
                vec![(retained, GoalEscape::ClosedFunctionSchemeAt(outer))],
                &tcx,
            )
            .unwrap();
        let scheme = function_scheme(&outer_closed.outputs()[0]).ty();
        let Type::Forall { body, .. } = scheme.as_type() else {
            panic!("the explicit source binder must survive parent closure")
        };
        let Type::Function { ret, .. } = body.as_ref() else {
            panic!("the retained scheme must contain its function layer")
        };
        assert!(matches!(ret.as_ref(), Type::Unit { .. }));
        assert!(
            goal_refs(scheme.as_type()).is_empty(),
            "the parent must eliminate its result goal before publication"
        );
    }

    #[test]
    fn close_preparation_cannot_be_reused_with_another_delta() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();

        let mut first = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut first,
                open.clone(),
                scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let prepared_first = store.zonk_for_close(&first, open.clone(), &ctx()).unwrap();

        let mut second = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut second,
                open.clone(),
                scoped(path(&["m", "String"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        assert_panics(|| {
            let _ = store.close_prepared_owner(
                second,
                vec![(prepared_first, GoalEscape::ClosedAt(owner))],
                &ctx(),
            );
        });

        let mut final_delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut final_delta,
                open.clone(),
                scoped(path(&["m", "String"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let prepared_final = store.zonk_for_close(&final_delta, open, &ctx()).unwrap();
        let closed = store
            .close_prepared_owner(
                final_delta,
                vec![(prepared_final, GoalEscape::ClosedAt(owner))],
                &ctx(),
            )
            .unwrap();
        assert_path(&goal_free(&closed.outputs()[0]).value, &["m", "String"], 0);
    }

    #[test]
    fn direct_and_indirect_occurs_checks_are_atomic() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let first = goal(&mut store, owner, Kind::Star);
        let second = goal(&mut store, owner, Kind::Star);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let first_ty = store.scoped_goal(first, Vec::new(), sp()).unwrap();
        let second_ty = store.scoped_goal(second, Vec::new(), sp()).unwrap();

        let direct = scoped(
            path(&["m", "Box"], vec![first_ty.ty.clone_type()]),
            RigidScope::new(),
        );
        assert!(
            store
                .constrain(&mut delta, first_ty.clone(), direct, sp(), &ctx())
                .is_err()
        );
        assert!(delta.writes.is_empty());

        store
            .constrain(
                &mut delta,
                first_ty.clone(),
                second_ty.clone(),
                sp(),
                &ctx(),
            )
            .unwrap();
        let before = store.zonk_with_delta(&first_ty, &delta).unwrap();
        let indirect = scoped(
            path(&["m", "Box"], vec![second_ty.ty.clone_type()]),
            RigidScope::new(),
        );
        assert!(
            store
                .constrain(&mut delta, first_ty.clone(), indirect, sp(), &ctx())
                .is_err()
        );
        let after = store.zonk_with_delta(&first_ty, &delta).unwrap();
        assert_eq!(after.ty.as_type(), before.ty.as_type());
        assert_eq!(after.scope, before.scope);
    }

    #[test]
    fn child_may_export_only_closed_ancestor_scoped_assignments() {
        let (outer_name, child_name) = normalized_shadow_names();
        assert_ne!(outer_name, child_name);
        let outer_scope = RigidScope::from_bindings([(outer_name.clone(), Kind::Star, 10)]);
        let child_scope = outer_scope
            .clone()
            .with_binding(child_name.clone(), Kind::Star, 12);
        let mut store = GoalStore::new();
        let outer = owner(&mut store, outer_scope.clone());
        let outer_goal = goal(&mut store, outer, Kind::Star);
        let child = store
            .begin_owner(
                Some(outer),
                child_scope.clone(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(child, sp()).unwrap();
        let target = store.scoped_goal(outer_goal, Vec::new(), sp()).unwrap();

        assert!(
            store
                .constrain(
                    &mut delta,
                    target.clone(),
                    scoped(
                        path(&[child_name.as_str()], Vec::new()),
                        child_scope.clone(),
                    ),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert!(delta.writes.is_empty());
        store
            .constrain(
                &mut delta,
                target,
                scoped(path(&[outer_name.as_str()], Vec::new()), child_scope),
                sp(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(delta.writes.len(), 1);
    }

    #[test]
    fn ancestor_hkt_use_keeps_child_rigid_evidence_and_cannot_leak_it() {
        let outer_scope = RigidScope::new();
        let child_scope = outer_scope.clone().with_binding("B", Kind::Star, 12);
        let mut store = GoalStore::new();
        let outer = owner(&mut store, outer_scope);
        let constructor = goal(&mut store, outer, Kind::arrow_chain(1));
        let child = store
            .begin_owner(
                Some(outer),
                child_scope,
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_use = store
            .scoped_goal_at(constructor, vec![path(&["B"], Vec::new())], child, sp())
            .unwrap();
        let concrete = store
            .scoped_type(
                child,
                InternedType::fresh(path(&["m", "Box"], vec![path(&["B"], Vec::new())])),
                sp(),
            )
            .unwrap();
        let mut delta = store.begin_delta(child, sp()).unwrap();

        store
            .constrain(&mut delta, child_use.clone(), concrete, sp(), &ctx())
            .expect("the child binder must be available while checking the HKT argument");
        let error = store
            .close_owner(delta, vec![(child_use, GoalEscape::ToOwner(outer))], &ctx())
            .expect_err("a child-local binder must not escape into the ancestor owner");
        assert!(
            error.diag().1.contains("outside its destination scope"),
            "the use-site binder must survive through kind checking to the escape check"
        );
    }

    #[test]
    fn nested_composite_goal_dependencies_are_equation_order_independent_and_atomic() {
        fn solve(child_first: bool) -> Type<Lowered> {
            let mut store = GoalStore::new();
            let outer = owner(&mut store, RigidScope::new());
            let result = goal(&mut store, outer, Kind::Star);
            let child = store
                .begin_owner(
                    Some(outer),
                    RigidScope::new(),
                    GoalOwnerKind::NestedApplication,
                    sp(),
                )
                .unwrap();
            let component = goal(&mut store, child, Kind::Star);
            let result_ty = store.scoped_goal(result, Vec::new(), sp()).unwrap();
            let component_ty = store
                .scoped_goal_at(component, Vec::new(), child, sp())
                .unwrap();
            let composite = scoped(
                product(component_ty.ty().clone_type(), unit()),
                store.owner_scope(child, sp()).unwrap(),
            );
            let concrete = scoped(path(&["m", "I32"], Vec::new()), RigidScope::new());
            let mut delta = store.begin_delta(child, sp()).unwrap();

            let add_composite = |delta: &mut GoalDelta| {
                store
                    .constrain(delta, result_ty.clone(), composite.clone(), sp(), &ctx())
                    .unwrap();
            };
            let solve_component = |delta: &mut GoalDelta| {
                store
                    .constrain(delta, component_ty.clone(), concrete.clone(), sp(), &ctx())
                    .unwrap();
            };
            if child_first {
                solve_component(&mut delta);
                add_composite(&mut delta);
            } else {
                add_composite(&mut delta);
                solve_component(&mut delta);
            }
            store.close_owner(delta, Vec::new(), &ctx()).unwrap();
            let probe = store.begin_delta(outer, sp()).unwrap();
            store
                .zonk_with_delta(&result_ty, &probe)
                .unwrap()
                .ty()
                .clone_type()
        }

        assert_eq!(solve(false), solve(true));

        let mut store = GoalStore::new();
        let outer = owner(&mut store, RigidScope::new());
        let result = goal(&mut store, outer, Kind::Star);
        let child = store
            .begin_owner(
                Some(outer),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let component = goal(&mut store, child, Kind::Star);
        let result_ty = store.scoped_goal(result, Vec::new(), sp()).unwrap();
        let component_ty = store
            .scoped_goal_at(component, Vec::new(), child, sp())
            .unwrap();
        let mut delta = store.begin_delta(child, sp()).unwrap();
        store
            .constrain(
                &mut delta,
                result_ty.clone(),
                scoped(
                    product(component_ty.ty().clone_type(), unit()),
                    store.owner_scope(child, sp()).unwrap(),
                ),
                sp(),
                &ctx(),
            )
            .unwrap();
        let writes_before = delta.writes.keys().copied().collect::<Vec<_>>();
        assert!(
            store
                .constrain(
                    &mut delta,
                    scoped(
                        product(
                            component_ty.ty().clone_type(),
                            path(&["m", "I32"], Vec::new()),
                        ),
                        store.owner_scope(child, sp()).unwrap(),
                    ),
                    scoped(
                        product(
                            path(&["m", "String"], Vec::new()),
                            path(&["m", "String"], Vec::new()),
                        ),
                        RigidScope::new(),
                    ),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert_eq!(
            delta.writes.keys().copied().collect::<Vec<_>>(),
            writes_before
        );
        assert!(
            store.close_owner(delta, Vec::new(), &ctx()).is_err(),
            "the unresolved child component must prevent every ancestor write from committing"
        );
        let outer_probe = store.begin_delta(outer, sp()).unwrap();
        assert!(matches!(
            store
                .zonk_with_delta(&result_ty, &outer_probe)
                .unwrap()
                .ty()
                .as_type(),
            Type::Goal { .. }
        ));
    }

    #[test]
    fn close_rejects_child_local_rigids_but_accepts_destination_rigids() {
        let outer_scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let child_scope = outer_scope.clone().with_binding("A_n2", Kind::Star, 12);
        let mut store = GoalStore::new();
        let outer = owner(&mut store, outer_scope.clone());
        let child = store
            .begin_owner(
                Some(outer),
                child_scope.clone(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let invalid = scoped(path(&["A_n2"], Vec::new()), child_scope.clone());
        let invalid_delta = store.begin_delta(child, sp()).unwrap();
        assert!(
            store
                .close_owner(
                    invalid_delta,
                    vec![(invalid, GoalEscape::ToOwner(outer))],
                    &ctx(),
                )
                .is_err()
        );

        let valid = scoped(path(&["A"], Vec::new()), child_scope);
        let valid_delta = store.begin_delta(child, sp()).unwrap();
        let closed = store
            .close_owner(
                valid_delta,
                vec![(valid, GoalEscape::ToOwner(outer))],
                &ctx(),
            )
            .unwrap();
        assert!(matches!(
            retained(&closed.outputs()[0]).ty().as_type(),
            Type::Path { segments, .. } if segments[0].as_str() == "A"
        ));
    }

    #[test]
    fn closed_output_accepts_a_rigid_from_its_own_destination_scope() {
        let scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let mut store = GoalStore::new();
        let owner = owner(&mut store, scope.clone());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let output = scoped(path(&["A"], Vec::new()), scope);

        store
            .close_owner(delta, vec![(output, GoalEscape::ClosedAt(owner))], &ctx())
            .unwrap();
    }

    #[test]
    fn cross_domain_and_peer_goals_are_rejected() {
        let mut store = GoalStore::new();
        let first_owner = owner(&mut store, RigidScope::new());
        let first = goal(&mut store, first_owner, Kind::Star);
        let second_owner = owner(&mut store, RigidScope::new());
        let second = goal(&mut store, second_owner, Kind::Star);
        let mut delta = store.begin_delta(first_owner, sp()).unwrap();
        assert_panics(|| {
            let _ = store.constrain(
                &mut delta,
                store.scoped_goal(first, Vec::new(), sp()).unwrap(),
                store.scoped_goal(second, Vec::new(), sp()).unwrap(),
                sp(),
                &ctx(),
            );
        });
        assert!(delta.writes.is_empty());
    }

    #[test]
    fn solved_foreign_goal_cannot_cross_between_domains_as_a_capability() {
        let mut store = GoalStore::new();
        let source = owner(&mut store, RigidScope::new());
        let foreign = goal(&mut store, source, Kind::Star);
        let foreign_ty = store.scoped_goal(foreign, Vec::new(), sp()).unwrap();
        let mut source_delta = store.begin_delta(source, sp()).unwrap();
        store
            .constrain(
                &mut source_delta,
                foreign_ty.clone(),
                scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        store
            .close_owner(
                source_delta,
                vec![(foreign_ty.clone(), GoalEscape::ClosedAt(source))],
                &ctx(),
            )
            .unwrap();

        let destination = owner(&mut store, RigidScope::new());
        let mut destination_delta = store.begin_delta(destination, sp()).unwrap();
        assert_panics(|| {
            let _ = store.constrain(
                &mut destination_delta,
                foreign_ty,
                scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            );
        });
        assert!(destination_delta.writes.is_empty());
    }

    #[test]
    fn unresolved_foreign_self_equation_cannot_cross_domains() {
        let mut store = GoalStore::new();
        let source = owner(&mut store, RigidScope::new());
        let foreign = goal(&mut store, source, Kind::Star);
        let foreign_ty = store.scoped_goal(foreign, Vec::new(), sp()).unwrap();
        let destination = owner(&mut store, RigidScope::new());
        let mut destination_delta = store.begin_delta(destination, sp()).unwrap();

        assert_panics(|| {
            let _ = store.constrain(
                &mut destination_delta,
                foreign_ty.clone(),
                foreign_ty,
                sp(),
                &ctx(),
            );
        });
    }

    #[test]
    fn goal_free_closed_output_can_cross_domains_when_rigid_scopes_agree() {
        let scope = RigidScope::from_bindings([("A".to_owned(), Kind::Star, 10)]);
        let mut store = GoalStore::new();
        let source = owner(&mut store, scope.clone());
        let destination = owner(&mut store, scope.clone());
        let delta = store.begin_delta(source, sp()).unwrap();
        let closed = store
            .close_owner(
                delta,
                vec![(
                    scoped(path(&["A"], Vec::new()), scope),
                    GoalEscape::ClosedAt(destination),
                )],
                &ctx(),
            )
            .unwrap();

        assert!(matches!(
            goal_free(&closed.outputs()[0]).ty().as_type(),
            Type::Path { segments, .. } if segments[0].as_str() == "A"
        ));
    }

    #[test]
    fn child_closure_can_retain_only_an_ancestor_goal() {
        let mut store = GoalStore::new();
        let outer = owner(&mut store, RigidScope::new());
        let ancestor = goal(&mut store, outer, Kind::Star);
        let child = store
            .begin_owner(
                Some(outer),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let child_goal = goal(&mut store, child, Kind::Star);
        let mut delta = store.begin_delta(child, sp()).unwrap();
        let child_ty = store.scoped_goal(child_goal, Vec::new(), sp()).unwrap();
        store
            .constrain(
                &mut delta,
                child_ty.clone(),
                store.scoped_goal(ancestor, Vec::new(), sp()).unwrap(),
                sp(),
                &ctx(),
            )
            .unwrap();

        let closed = store
            .close_owner(delta, vec![(child_ty, GoalEscape::ToOwner(outer))], &ctx())
            .unwrap();
        let retained = retained(&closed.outputs()[0]);
        assert!(matches!(retained.ty().as_type(), Type::Goal { goal, .. } if *goal == ancestor));
    }

    #[test]
    fn failed_close_does_not_publish_blocked_child_state() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let unresolved = goal(&mut store, owner, Kind::Star);
        let delta = store.begin_delta(owner, sp()).unwrap();
        assert!(store.close_owner(delta, Vec::new(), &ctx()).is_err());
        let probe = store.begin_delta(owner, sp()).unwrap();
        let open = store
            .zonk_with_delta(
                &store.scoped_goal(unresolved, Vec::new(), sp()).unwrap(),
                &probe,
            )
            .unwrap();
        assert!(matches!(open.ty().as_type(), Type::Goal { .. }));
    }

    #[test]
    fn successful_close_seals_the_owner() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let closed = store.close_owner(delta, Vec::new(), &ctx()).unwrap();

        assert_eq!(closed.owner(), owner);
        assert_panics(|| {
            let _ = store.alloc_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "T"),
            );
        });
        assert_panics(|| {
            let _ = store.begin_delta(owner, sp());
        });
    }

    #[test]
    fn parent_closes_only_after_all_nested_owners_close() {
        let mut store = GoalStore::new();
        let parent = owner(&mut store, RigidScope::new());
        let child = store
            .begin_owner(
                Some(parent),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let premature = store.begin_delta(parent, sp()).unwrap();
        assert_panics(|| {
            let _ = store.close_owner(premature, Vec::new(), &ctx());
        });
        assert!(store.begin_delta(parent, sp()).is_ok());

        let child_delta = store.begin_delta(child, sp()).unwrap();
        store.close_owner(child_delta, Vec::new(), &ctx()).unwrap();
        let parent_delta = store.begin_delta(parent, sp()).unwrap();
        store.close_owner(parent_delta, Vec::new(), &ctx()).unwrap();
        assert_panics(|| {
            let _ = store.begin_delta(child, sp());
        });
        assert_panics(|| {
            let _ = store.begin_delta(parent, sp());
        });
    }

    #[test]
    fn allocating_after_delta_creation_makes_that_delta_stale() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let _first = goal(&mut store, owner, Kind::Star);
        let stale = store.begin_delta(owner, sp()).unwrap();
        let _second = goal(&mut store, owner, Kind::Star);

        assert_panics(|| {
            let _ = store.close_owner(stale, Vec::new(), &ctx());
        });
        assert!(store.begin_delta(owner, sp()).is_ok());
    }

    #[test]
    fn failed_output_validation_does_not_publish_or_close_the_owner() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let head = goal(&mut store, owner, Kind::arrow_chain(1));
        let open_head = store.scoped_goal(head, Vec::new(), sp()).unwrap();
        let mut invalid_delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut invalid_delta,
                open_head.clone(),
                scoped(path(&["m", "Box"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let invalid_output = store
            .scoped_goal(head, vec![path(&["m", "Box"], Vec::new())], sp())
            .unwrap();

        assert!(
            store
                .close_owner(
                    invalid_delta,
                    vec![(invalid_output, GoalEscape::ClosedAt(owner))],
                    &ctx(),
                )
                .is_err()
        );
        let probe = store.begin_delta(owner, sp()).unwrap();
        let still_open = store.zonk_with_delta(&open_head, &probe).unwrap();
        assert!(matches!(still_open.ty().as_type(), Type::Goal { .. }));

        let mut corrected = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(
                &mut corrected,
                open_head,
                scoped(path(&["m", "Box"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        let valid_output = store
            .scoped_goal(head, vec![path(&["m", "I32"], Vec::new())], sp())
            .unwrap();
        store
            .close_owner(
                corrected,
                vec![(valid_output, GoalEscape::ClosedAt(owner))],
                &ctx(),
            )
            .expect("a failed close leaves the owner available for a corrected transaction");
    }

    #[test]
    fn conflicting_child_close_fails_without_changing_committed_ancestor() {
        let mut store = GoalStore::new();
        let outer = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, outer, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let first_child = store
            .begin_owner(
                Some(outer),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let second_child = store
            .begin_owner(
                Some(outer),
                RigidScope::new(),
                GoalOwnerKind::NestedApplication,
                sp(),
            )
            .unwrap();
        let mut first = store.begin_delta(first_child, sp()).unwrap();
        let mut conflicting = store.begin_delta(second_child, sp()).unwrap();
        store
            .constrain(
                &mut first,
                open.clone(),
                scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        store
            .constrain(
                &mut conflicting,
                open.clone(),
                scoped(path(&["m", "String"], Vec::new()), RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        store.close_owner(first, Vec::new(), &ctx()).unwrap();

        assert!(store.close_owner(conflicting, Vec::new(), &ctx()).is_err());
        assert!(store.begin_delta(second_child, sp()).is_ok());
        let outer_probe = store.begin_delta(outer, sp()).unwrap();
        let committed = store.zonk_with_delta(&open, &outer_probe).unwrap();
        assert_path(&committed, &["m", "I32"], 0);
    }

    #[test]
    fn monotype_policy_rejects_a_whole_scheme_but_preserves_nested_forall() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let poly = forall("A", path(&["A"], Vec::new()));
        assert!(
            store
                .constrain(
                    &mut delta,
                    open.clone(),
                    scoped(poly.clone(), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert!(delta.writes.is_empty());

        let nested = function(unit(), poly);
        store
            .constrain(
                &mut delta,
                open,
                scoped(nested, RigidScope::new()),
                sp(),
                &ctx(),
            )
            .unwrap();
        assert_eq!(delta.writes.len(), 1);
    }

    #[test]
    fn a_polytype_capable_type_argument_goal_accepts_a_whole_forall_type() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = store
            .alloc_goal(
                owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::TypeArgument, "T"),
            )
            .unwrap();
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let poly = scoped(
            forall(
                "A",
                function(path(&["A"], Vec::new()), path(&["A"], Vec::new())),
            ),
            RigidScope::new(),
        );
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        store
            .constrain(&mut delta, open.clone(), poly, sp(), &ctx())
            .unwrap();
        let closed = store
            .close_owner(delta, vec![(open, GoalEscape::ClosedAt(owner))], &ctx())
            .unwrap();

        assert!(matches!(
            goal_free(&closed.outputs()[0]).ty().as_type(),
            Type::Forall { .. }
        ));
    }

    #[test]
    fn goal_aware_complete_scheme_frontier_visits_a_deep_spine_once() {
        const DEPTH: usize = 96;
        let mut ty = function(unit(), unit());
        for index in (0..DEPTH).rev() {
            ty = forall(&format!("A{index}"), ty);
        }

        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let scoped = store
            .scoped_type(owner, InternedType::fresh(ty), sp())
            .unwrap();

        super::super::kind_scheme::reset_complete_scheme_consumer_work();
        assert!(
            store
                .classify_complete_function_scheme(&scoped, &delta, &ctx())
                .expect("classify the goal-aware complete scheme")
                .is_complete()
        );
        assert_eq!(
            super::super::kind_scheme::complete_scheme_consumer_work(),
            super::super::kind_scheme::CompleteSchemeConsumerWork {
                open_view: 1,
                goal_frontier_steps: DEPTH + 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn goal_kind_consumes_the_singular_complete_scheme_summary() {
        let f = path(&["F"], vec![unit()]);
        let ty = Type::Forall {
            param: TypeParam {
                name: "F".to_owned(),
                span: sp(),
                kind: Some(Kind::arrow_chain(1)),
            },
            body: Box::new(function(f.clone(), f)),
            meta: Meta::new(sp()),
        };
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let delta = store.begin_delta(owner, sp()).unwrap();
        let scoped = store
            .scoped_type(owner, InternedType::fresh(ty), sp())
            .unwrap();

        super::super::kind_scheme::reset_complete_scheme_consumer_work();
        assert_eq!(
            store
                .kind_of(&scoped, &delta, &ctx(), &mut BTreeSet::new())
                .expect("the complete higher-kinded scheme has kind `*`"),
            Kind::Star
        );
        assert_eq!(
            super::super::kind_scheme::complete_scheme_consumer_work(),
            super::super::kind_scheme::CompleteSchemeConsumerWork {
                goal_kind: 1,
                goal_frontier_steps: 2,
                ..Default::default()
            }
        );
    }

    #[test]
    fn complete_higher_kinded_function_scheme_is_a_structural_value_type() {
        let hkt_scheme = Type::Forall {
            param: TypeParam {
                name: "F".to_owned(),
                span: sp(),
                kind: Some(Kind::arrow_chain(1)),
            },
            body: Box::new(function(
                path(&["F"], vec![unit()]),
                path(&["F"], vec![unit()]),
            )),
            meta: Meta::new(sp()),
        };
        let mut scheme_store = GoalStore::new();
        let scheme_owner = owner(&mut scheme_store, RigidScope::new());
        let scheme_delta = scheme_store.begin_delta(scheme_owner, sp()).unwrap();
        let scheme = scheme_store
            .scoped_type(scheme_owner, InternedType::fresh(hkt_scheme.clone()), sp())
            .unwrap();
        let closed = scheme_store
            .close_owner(
                scheme_delta,
                vec![(scheme, GoalEscape::ClosedFunctionSchemeAt(scheme_owner))],
                &ctx(),
            )
            .expect("a complete HKT function scheme admits ordinary scheme publication");
        let published = function_scheme(&closed.outputs()[0]);
        assert!(matches!(published.ty().as_type(), Type::Forall { .. }));
        let published = published.ty().clone_type();

        let mut goal_store = GoalStore::new();
        let goal_owner = owner(&mut goal_store, RigidScope::new());
        let inferred = goal_store
            .alloc_goal(
                goal_owner,
                Kind::Star,
                GoalSolutionPolicy::PolytypeAllowed,
                GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
            )
            .unwrap();
        let open = goal_store.scoped_goal(inferred, Vec::new(), sp()).unwrap();
        let mut goal_delta = goal_store.begin_delta(goal_owner, sp()).unwrap();
        goal_store
            .constrain(
                &mut goal_delta,
                open.clone(),
                scoped(hkt_scheme, RigidScope::new()),
                sp(),
                &ctx(),
            )
            .expect("a polytype-capable goal accepts a complete HKT function scheme");
        let goal_closed = goal_store
            .close_owner(
                goal_delta,
                vec![(open, GoalEscape::ClosedAt(goal_owner))],
                &ctx(),
            )
            .expect("the structurally complete HKT goal must close");
        assert!(matches!(
            goal_free(&goal_closed.outputs()[0]).ty().as_type(),
            Type::Forall { .. }
        ));

        let mut value_store = GoalStore::new();
        let value_owner = owner(&mut value_store, RigidScope::new());
        let value_delta = value_store.begin_delta(value_owner, sp()).unwrap();
        let embedded = value_store
            .scoped_type(
                value_owner,
                InternedType::fresh(function(unit(), published)),
                sp(),
            )
            .unwrap();
        let value_closed = value_store
            .close_owner(
                value_delta,
                vec![(embedded, GoalEscape::ClosedAt(value_owner))],
                &ctx(),
            )
            .expect("a complete HKT scheme remains valid inside a function type");
        assert!(matches!(
            goal_free(&value_closed.outputs()[0]).ty().as_type(),
            Type::Function { .. }
        ));

        let mut mislabeled_store = GoalStore::new();
        let mislabeled_owner = owner(&mut mislabeled_store, RigidScope::new());
        let mislabeled_delta = mislabeled_store
            .begin_delta(mislabeled_owner, sp())
            .unwrap();
        let non_function = mislabeled_store
            .scoped_type(mislabeled_owner, InternedType::fresh(unit()), sp())
            .unwrap();
        assert_panics(|| {
            let _ = mislabeled_store.close_owner(
                mislabeled_delta,
                vec![(
                    non_function,
                    GoalEscape::ClosedFunctionSchemeAt(mislabeled_owner),
                )],
                &ctx(),
            );
        });
    }

    #[test]
    fn goal_unions_cannot_bypass_monotype_policy_in_either_allocation_order() {
        fn rejected(monotype_first: bool) -> bool {
            let mut store = GoalStore::new();
            let owner = owner(&mut store, RigidScope::new());
            let allocate = |store: &mut GoalStore, policy| {
                store
                    .alloc_goal(
                        owner,
                        Kind::Star,
                        policy,
                        GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
                    )
                    .unwrap()
            };
            let (first_policy, second_policy) = if monotype_first {
                (
                    GoalSolutionPolicy::Monotype,
                    GoalSolutionPolicy::PolytypeAllowed,
                )
            } else {
                (
                    GoalSolutionPolicy::PolytypeAllowed,
                    GoalSolutionPolicy::Monotype,
                )
            };
            let first = allocate(&mut store, first_policy);
            let second = allocate(&mut store, second_policy);
            let (monotype, permissive) = if monotype_first {
                (first, second)
            } else {
                (second, first)
            };
            run(store, owner, monotype, permissive)
        }

        fn run(
            store: GoalStore,
            owner: TypeGoalOwner,
            monotype: TypeGoalRef,
            permissive: TypeGoalRef,
        ) -> bool {
            let mut store = store;
            let monotype = store.scoped_goal(monotype, Vec::new(), sp()).unwrap();
            let permissive = store.scoped_goal(permissive, Vec::new(), sp()).unwrap();
            let mut delta = store.begin_delta(owner, sp()).unwrap();
            store
                .constrain(&mut delta, monotype, permissive.clone(), sp(), &ctx())
                .unwrap();
            let poly = scoped(
                forall(
                    "A",
                    function(path(&["A"], Vec::new()), path(&["A"], Vec::new())),
                ),
                RigidScope::new(),
            );
            if store
                .constrain(&mut delta, permissive, poly, sp(), &ctx())
                .is_err()
            {
                return true;
            }
            store.close_owner(delta, Vec::new(), &ctx()).is_err()
        }

        assert!(rejected(true));
        assert!(rejected(false));
    }

    #[test]
    fn child_monotype_policy_restricts_an_aliased_ancestor_after_child_close() {
        fn run(reverse_operands: bool) {
            let mut store = GoalStore::new();
            let outer = owner(&mut store, RigidScope::new());
            let permissive = store
                .alloc_goal(
                    outer,
                    Kind::Star,
                    GoalSolutionPolicy::PolytypeAllowed,
                    GoalOrigin::named(sp(), GoalRole::LambdaParameter, "value"),
                )
                .unwrap();
            let child = store
                .begin_owner(
                    Some(outer),
                    RigidScope::new(),
                    GoalOwnerKind::NestedApplication,
                    sp(),
                )
                .unwrap();
            let monotype = goal(&mut store, child, Kind::Star);
            let permissive_ty = store.scoped_goal(permissive, Vec::new(), sp()).unwrap();
            let monotype_ty = store.scoped_goal(monotype, Vec::new(), sp()).unwrap();
            let (left, right) = if reverse_operands {
                (permissive_ty.clone(), monotype_ty.clone())
            } else {
                (monotype_ty.clone(), permissive_ty.clone())
            };
            let mut child_delta = store.begin_delta(child, sp()).unwrap();
            store
                .constrain(&mut child_delta, left, right, sp(), &ctx())
                .unwrap();
            store
                .close_owner(
                    child_delta,
                    vec![(monotype_ty, GoalEscape::ToOwner(outer))],
                    &ctx(),
                )
                .unwrap();

            let mut outer_delta = store.begin_delta(outer, sp()).unwrap();
            let poly = scoped(
                forall(
                    "A",
                    function(path(&["A"], Vec::new()), path(&["A"], Vec::new())),
                ),
                RigidScope::new(),
            );
            assert!(
                store
                    .constrain(&mut outer_delta, permissive_ty, poly, sp(), &ctx(),)
                    .is_err()
            );
            assert!(outer_delta.writes.is_empty());
        }

        run(false);
        run(true);
    }

    #[test]
    fn kind_mismatch_rejects_a_bare_higher_kinded_solution() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        assert!(
            store
                .constrain(
                    &mut delta,
                    store.scoped_goal(inferred, Vec::new(), sp()).unwrap(),
                    scoped(path(&["m", "Box"], Vec::new()), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert!(delta.writes.is_empty());
    }

    #[test]
    fn forall_equations_are_alpha_normalized_without_goal_ids() {
        let store = GoalStore::new();
        let mut store = store;
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let scope = RigidScope::from_bindings((0..256).map(|index| {
            (
                format!("T{index}"),
                Kind::Star,
                u64::try_from(index + 1).expect("test binder identity fits in u64"),
            )
        }));
        reset_rigid_scope_name_snapshots();
        store
            .constrain(
                &mut delta,
                scoped(forall("A", path(&["A"], Vec::new())), scope.clone()),
                scoped(forall("B", path(&["B"], Vec::new())), scope),
                sp(),
                &ctx(),
            )
            .unwrap();
        assert!(delta.writes.is_empty());
        assert_eq!(rigid_scope_name_snapshots(), 0);
        assert_eq!(rigid_scope_names_copied(), 0);
    }

    #[test]
    fn nested_forall_comparison_distinguishes_free_and_bound_same_spelling() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let mut delta = store.begin_delta(owner, sp()).unwrap();
        let left = forall("A", forall("B", path(&["B"], Vec::new())));
        let adversarial = forall("B", forall("C", path(&["B"], Vec::new())));

        assert!(
            store
                .constrain(
                    &mut delta,
                    scoped(left.clone(), RigidScope::new()),
                    scoped(adversarial, RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .is_err()
        );
        assert!(delta.writes.is_empty());
        store
            .constrain(
                &mut delta,
                scoped(left, RigidScope::new()),
                scoped(
                    forall("X", forall("Y", path(&["Y"], Vec::new()))),
                    RigidScope::new(),
                ),
                sp(),
                &ctx(),
            )
            .expect("ordinary nested alpha-equivalent schemes must compare equal");
    }

    #[test]
    fn planner_cannot_place_an_open_goal_under_a_structural_forall() {
        let mut store = GoalStore::new();
        let owner = owner(&mut store, RigidScope::new());
        let inferred = goal(&mut store, owner, Kind::Star);
        let open = store.scoped_goal(inferred, Vec::new(), sp()).unwrap();

        assert_panics(|| {
            let _ = store.scoped_type(
                owner,
                InternedType::fresh(forall("A", open.ty().clone_type())),
                sp(),
            );
        });
    }

    #[test]
    fn allocation_order_changes_representatives_not_meaning() {
        fn solve(reverse: bool) -> Type<Lowered> {
            let mut store = GoalStore::new();
            let owner = owner(&mut store, RigidScope::new());
            let first = goal(&mut store, owner, Kind::Star);
            let second = goal(&mut store, owner, Kind::Star);
            let first_ty = store.scoped_goal(first, Vec::new(), sp()).unwrap();
            let second_ty = store.scoped_goal(second, Vec::new(), sp()).unwrap();
            let mut delta = store.begin_delta(owner, sp()).unwrap();
            let (left, right) = if reverse {
                (second_ty.clone(), first_ty.clone())
            } else {
                (first_ty.clone(), second_ty.clone())
            };
            store
                .constrain(&mut delta, left, right, sp(), &ctx())
                .unwrap();
            store
                .constrain(
                    &mut delta,
                    if reverse {
                        second_ty.clone()
                    } else {
                        first_ty.clone()
                    },
                    scoped(path(&["m", "I32"], Vec::new()), RigidScope::new()),
                    sp(),
                    &ctx(),
                )
                .unwrap();
            let output = store
                .close_owner(delta, vec![(first_ty, GoalEscape::ClosedAt(owner))], &ctx())
                .unwrap()
                .into_outputs()
                .remove(0);
            into_goal_free(output).ty.clone_type()
        }

        assert_eq!(solve(false), solve(true));
    }
}

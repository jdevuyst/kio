//! Conservative authentication for source refactorings. Hover keeps its
//! last-write-wins view; an inconsistent observation cannot authorize an edit.

use super::{PositionIndex, ResolvedBinder, Type, TypeBinderScope};
use crate::span::Span;
use std::collections::HashSet;

type Position = (String, Span);

#[derive(Debug, Clone)]
pub(super) struct RefactoringFacts {
    complete: bool,
    conflicts: HashSet<Position>,
    public_callees: HashSet<Position>,
}

impl Default for RefactoringFacts {
    fn default() -> Self {
        Self {
            complete: true,
            conflicts: HashSet::new(),
            public_callees: HashSet::new(),
        }
    }
}

impl PositionIndex {
    pub(crate) fn for_refactoring() -> Self {
        Self {
            refactoring: Some(Box::default()),
            ..Self::default()
        }
    }

    pub(super) fn observe_type(
        &mut self,
        key: &Position,
        ty: &Type,
        canonical: bool,
        binders: Option<&TypeBinderScope>,
    ) {
        let Some(facts) = &mut self.refactoring else {
            return;
        };
        if self.types.get(key).is_some_and(|previous| {
            previous != ty
                || self.canonical_types.contains(key) != canonical
                || self.type_binders.get(key) != binders
        }) {
            facts.conflicts.insert(key.clone());
        }
    }

    pub(super) fn observe_binder(&mut self, key: &Position, binder: &ResolvedBinder) {
        let Some(facts) = &mut self.refactoring else {
            return;
        };
        if self
            .binders
            .get(key)
            .is_some_and(|previous| !same_binder(previous, binder))
        {
            facts.conflicts.insert(key.clone());
        }
    }

    pub(super) fn invalidate_refactoring_position(&mut self, key: &Position) {
        if let Some(facts) = &mut self.refactoring {
            facts.conflicts.insert(key.clone());
        }
    }

    pub(super) fn remap_refactoring_position(&mut self, old: &Position, new: &Position) {
        let Some(facts) = &mut self.refactoring else {
            return;
        };
        if old == new {
            return;
        }
        if facts.conflicts.contains(old) {
            facts.conflicts.insert(new.clone());
        }
        if facts.public_callees.remove(old) {
            facts.public_callees.insert(new.clone());
        }
        facts.conflicts.insert(old.clone());
    }

    pub(super) fn merge_refactoring_coverage(&mut self, incoming: Option<Box<RefactoringFacts>>) {
        let Some(facts) = &mut self.refactoring else {
            return;
        };
        if let Some(incoming) = incoming {
            facts.complete &= incoming.complete;
            facts.conflicts.extend(incoming.conflicts);
            facts.public_callees.extend(incoming.public_callees);
        } else {
            // Even an empty unobserved shard may have removed earlier facts.
            // Only construction opts in; merging never upgrades coverage.
            facts.complete = false;
        }
    }

    /// Called only when the original direct-callee resolution's staged
    /// position type commits. Ordinary expression/hover types do not qualify.
    pub(super) fn record_public_callee(&mut self, module: &str, span: Span) {
        if let Some(facts) = &mut self.refactoring {
            facts.public_callees.insert((module.to_owned(), span));
        }
    }

    #[cfg(test)]
    pub(crate) fn refactoring_storage_counts(&self) -> Option<(usize, usize, usize, usize, usize)> {
        self.refactoring.as_ref().map(|facts| {
            (
                facts.conflicts.len(),
                facts.conflicts.capacity(),
                facts.public_callees.len(),
                facts.public_callees.capacity(),
                facts
                    .conflicts
                    .iter()
                    .chain(&facts.public_callees)
                    .map(|(module, _)| module.capacity())
                    .sum(),
            )
        })
    }

    pub(crate) fn authenticated_callee(
        &self,
        module: &str,
        path: Span,
        terminal: Span,
        call: Span,
    ) -> Option<(&Type, &TypeBinderScope, &ResolvedBinder)> {
        let facts = self.refactoring.as_ref()?;
        let key = (module.to_owned(), path);
        let terminal_key = (module.to_owned(), terminal);
        if !facts.complete
            || !facts.public_callees.contains(&key)
            || facts.conflicts.contains(&key)
            || facts.conflicts.contains(&terminal_key)
            || !self.canonical_types.contains(&key)
            || self
                .value_at_type_slots
                .iter()
                .any(|(owner, span, _)| owner == module && *span == call)
        {
            return None;
        }
        let ty = self.types.get(&key)?;
        let scope = self.type_binders.get(&key)?;
        if scope.0.values().any(Option::is_none) {
            return None;
        }
        let binder = self.binders.get(&terminal_key)?;
        match binder {
            ResolvedBinder::Local {
                decl_span: Some(_), ..
            }
            | ResolvedBinder::Fn { .. }
            | ResolvedBinder::HostEnvFn { .. }
            | ResolvedBinder::Intrinsic { .. }
            | ResolvedBinder::NewtypeMember { .. } => Some((ty, scope, binder)),
            _ => None,
        }
    }
}

fn same_binder(left: &ResolvedBinder, right: &ResolvedBinder) -> bool {
    match (left, right) {
        (
            ResolvedBinder::Local {
                decl_span: Some(a), ..
            },
            ResolvedBinder::Local {
                decl_span: Some(b), ..
            },
        )
        | (
            ResolvedBinder::TypeParam {
                decl_span: Some(a), ..
            },
            ResolvedBinder::TypeParam {
                decl_span: Some(b), ..
            },
        ) => a == b,
        (ResolvedBinder::Local { .. } | ResolvedBinder::TypeParam { .. }, _)
        | (_, ResolvedBinder::Local { .. } | ResolvedBinder::TypeParam { .. }) => false,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Meta;
    use crate::pass::typecheck_core::InternedType;
    use std::collections::HashMap;

    const SITE: Span = Span { start: 10, end: 11 };
    const CALL: Span = Span { start: 10, end: 30 };

    fn unit() -> Type {
        Type::Unit {
            meta: Meta::new(SITE),
        }
    }

    fn binder() -> ResolvedBinder {
        ResolvedBinder::Local {
            name: "f".into(),
            decl_span: Some(Span::new(2, 3)),
        }
    }

    fn record(index: &mut PositionIndex) {
        index.record_type_with_identity(
            "x",
            SITE,
            InternedType::fresh_with_identity(unit(), true),
            TypeBinderScope::default(),
        );
        index.record_binder("x", SITE, binder());
        index.record_public_callee("x", SITE);
    }

    fn clean() -> PositionIndex {
        let mut index = PositionIndex::for_refactoring();
        record(&mut index);
        assert!(index.authenticated_callee("x", SITE, SITE, CALL).is_some());
        index
    }

    fn accepts(index: &PositionIndex) -> bool {
        index.authenticated_callee("x", SITE, SITE, CALL).is_some()
    }

    #[test]
    fn ufcs_authentication_requires_first_observation_in_every_shard() {
        let mut disabled = PositionIndex::new();
        record(&mut disabled);
        disabled.merge(clean());
        assert!(disabled.refactoring.is_none());
        assert!(!accepts(&disabled));
        for mut unobserved in [PositionIndex::new(), disabled] {
            // Coverage cannot be inferred from an empty type map, including
            // a shard whose recorded positions have already been removed.
            unobserved.remove_local_declaration_metadata(std::iter::once(("x", SITE)));
            let mut index = clean();
            index.merge(unobserved);
            index.merge(clean());
            assert!(!accepts(&index));
        }
        let mut observed = clean();
        observed.merge(PositionIndex::for_refactoring());
        observed.merge(clean());
        assert!(accepts(&observed));
    }

    #[test]
    fn ufcs_authentication_public_type_and_complete_origins_are_required() {
        let mut hover_only = PositionIndex::for_refactoring();
        hover_only.record_type_with_identity(
            "x",
            SITE,
            InternedType::fresh_with_identity(unit(), true),
            TypeBinderScope::default(),
        );
        hover_only.record_binder("x", SITE, binder());
        assert!(!accepts(&hover_only));
        hover_only.record_public_callee("x", SITE);
        assert!(accepts(&hover_only));

        let mut no_origin = PositionIndex::for_refactoring();
        no_origin.record_binder(
            "x",
            SITE,
            ResolvedBinder::Local {
                name: "f".into(),
                decl_span: None,
            },
        );
        record(&mut no_origin);
        assert!(!accepts(&no_origin));
        let mut no_type_origin = PositionIndex::for_refactoring();
        no_type_origin.record_type_with_identity(
            "x",
            SITE,
            InternedType::fresh_with_identity(unit(), true),
            TypeBinderScope(HashMap::from([("A".into(), None)])),
        );
        no_type_origin.record_binder("x", SITE, binder());
        no_type_origin.record_public_callee("x", SITE);
        assert!(!accepts(&no_type_origin));
    }

    #[test]
    fn ufcs_authentication_conflicts_are_sticky_across_writes_and_merges() {
        for dimension in 0..4 {
            let mut index = clean();
            if dimension == 2 {
                index = PositionIndex::for_refactoring();
                index.record_type_with_identity(
                    "x",
                    SITE,
                    InternedType::fresh_with_identity(unit(), true),
                    TypeBinderScope(HashMap::from([("A".into(), Some(Span::new(1, 2)))])),
                );
                index.record_binder("x", SITE, binder());
                index.record_public_callee("x", SITE);
                assert!(accepts(&index));
            }
            let mut changed = PositionIndex::for_refactoring();
            match dimension {
                0 => changed.record_type_with_identity(
                    "x",
                    SITE,
                    InternedType::fresh_with_identity(
                        Type::Bottom {
                            meta: Meta::new(SITE),
                        },
                        true,
                    ),
                    TypeBinderScope::default(),
                ),
                1 => changed.record_type_with_identity(
                    "x",
                    SITE,
                    InternedType::fresh_with_identity(unit(), false),
                    TypeBinderScope::default(),
                ),
                2 => changed.record_type_with_identity(
                    "x",
                    SITE,
                    InternedType::fresh_with_identity(unit(), true),
                    TypeBinderScope(HashMap::from([("A".into(), Some(Span::new(5, 6)))])),
                ),
                3 => changed.record_binder(
                    "x",
                    SITE,
                    ResolvedBinder::Local {
                        name: "f".into(),
                        decl_span: Some(Span::new(4, 5)),
                    },
                ),
                _ => unreachable!(),
            }
            index.merge(changed);
            assert!(!accepts(&index), "isolated dimension {dimension}");
            record(&mut index);
            assert!(!accepts(&index), "dimension {dimension}");
            let mut final_index = PositionIndex::for_refactoring();
            final_index.merge(index);
            final_index.merge(clean());
            assert!(!accepts(&final_index));
            assert_eq!(
                final_index.type_at("x", SITE),
                Some(&unit()),
                "hover retains the last type"
            );
        }
        let mut names_only = clean();
        names_only.record_binder(
            "x",
            SITE,
            ResolvedBinder::Local {
                name: "presented_f".into(),
                decl_span: Some(Span::new(2, 3)),
            },
        );
        assert!(
            accepts(&names_only),
            "presentation spelling is not lexical identity"
        );
    }

    #[test]
    fn ufcs_authentication_removal_remap_and_filter_keep_tombstones() {
        let mut removed = clean();
        removed.remove_local_declaration_metadata(std::iter::once(("x", SITE)));
        record(&mut removed);
        assert!(!accepts(&removed));

        let mut filtered = clean();
        filtered.record_binder("x", Span::new(40, 41), binder());
        let mut calls = 0;
        assert!(!filtered.try_retain_binders(|_, _, _| {
            calls += 1;
            (calls == 1).then_some(false)
        }));
        assert!(
            accepts(&filtered),
            "cancelled filtering publishes no removals"
        );
        assert!(filtered.try_retain_binders(|_, _, _| Some(false)));
        record(&mut filtered);
        assert!(!accepts(&filtered));

        let new = Span::new(50, 51);
        for conflict in [false, true] {
            let mut index = clean();
            index.record_binder_declaration("x", SITE, binder());
            if conflict {
                index.record_type("x", SITE, unit());
                record(&mut index);
            }
            index.remap_local_declaration_spans(&HashMap::from([(("x".into(), SITE), new)]));
            assert_eq!(
                index.authenticated_callee("x", new, new, CALL).is_some(),
                !conflict
            );
            record(&mut index);
            assert!(!accepts(&index), "old source position remains invalidated");
        }
        let mut collision = clean();
        collision.record_binder_declaration("x", SITE, binder());
        collision.record_binder(
            "x",
            new,
            ResolvedBinder::Local {
                name: "other".into(),
                decl_span: Some(Span::new(8, 9)),
            },
        );
        collision.remap_local_declaration_spans(&HashMap::from([(("x".into(), SITE), new)]));
        assert!(
            collision
                .authenticated_callee("x", new, new, CALL)
                .is_none()
        );
    }

    #[test]
    fn ufcs_authentication_any_positive_role_observation_vetoes_the_call() {
        let mut index = clean();
        let mut role = PositionIndex::for_refactoring();
        role.record_value_at_type_slot("x", CALL, 99);
        index.merge(role);
        index.merge(clean());
        assert!(!accepts(&index));
        assert!(
            index
                .authenticated_callee("x", SITE, SITE, Span::new(31, 40))
                .is_some()
        );
    }

    #[test]
    fn ufcs_authentication_disabled_observer_and_bounded_storage_cost() {
        for enabled in [false, true] {
            let mut index = if enabled {
                PositionIndex::for_refactoring()
            } else {
                PositionIndex::new()
            };
            let start = std::time::Instant::now();
            for i in 0..512 {
                let span = Span::new(i * 2, i * 2 + 1);
                for canonical in [true, false, true] {
                    index.record_type_with_identity(
                        "x",
                        span,
                        InternedType::fresh_with_identity(unit(), canonical),
                        TypeBinderScope::default(),
                    );
                    index.record_binder("x", span, binder());
                    index.record_public_callee("x", span);
                }
            }
            let elapsed = start.elapsed();
            if let Some(facts) = &index.refactoring {
                assert_eq!(facts.conflicts.len(), 512);
                assert_eq!(facts.public_callees.len(), 512);
                eprintln!(
                    "ufcs observer: enabled={enabled} elapsed={elapsed:?} conflicts={}/{} public={}/{} string_bytes={}",
                    facts.conflicts.len(),
                    facts.conflicts.capacity(),
                    facts.public_callees.len(),
                    facts.public_callees.capacity(),
                    facts
                        .conflicts
                        .iter()
                        .chain(&facts.public_callees)
                        .map(|(module, _)| module.capacity())
                        .sum::<usize>()
                );
            } else {
                assert!(!enabled);
                eprintln!("ufcs observer: enabled=false elapsed={elapsed:?} retained_markers=0");
            }
        }
        eprintln!(
            "ufcs layout: index={} inactive_pointer={} active_state={}",
            std::mem::size_of::<PositionIndex>(),
            std::mem::size_of::<Option<Box<RefactoringFacts>>>(),
            std::mem::size_of::<RefactoringFacts>()
        );
    }
}

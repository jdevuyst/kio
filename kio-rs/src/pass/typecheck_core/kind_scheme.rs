//! Shared demand/result algebra for kind and complete-function-scheme walks.
//!
//! Source types and open GoalStore views keep their own authority-bearing
//! adapters, but both drive this one frontier machine. The machine is
//! deliberately storage-free: adapters retain exact lexical/nominal context
//! and decide how a `Continue` view is represented.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompleteScheme {
    Complete,
    Incomplete,
    /// The scheme frontier terminates at one written source placeholder.
    /// Annotation planning continues the ordinary kind walk so its exact
    /// source-occurrence transport can issue the authorized binder-local
    /// placeholder diagnostic; other consumers retain the established
    /// incomplete-scheme error.
    SourcePlaceholder,
    /// The frontier reached an open type goal. A goal-aware adapter must
    /// resolve or classify that capability before deciding completeness.
    Deferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KindDemand {
    Head,
    Tree,
}

impl KindDemand {
    pub(crate) fn validates_tree(self) -> bool {
        matches!(self, Self::Tree)
    }
}

impl CompleteScheme {
    pub(crate) fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Exact structural spines already proved complete during one authoritative
/// kind/scheme walk. Source and goal-aware adapters share this one root-local
/// proof set so a nested consumer never rescans every suffix of a deep
/// `forall` chain.
#[derive(Default)]
pub(crate) struct CompleteSchemeProofs {
    bare_spines: HashSet<usize>,
}

impl CompleteSchemeProofs {
    pub(crate) fn contains<P: crate::ast::Phase>(&self, ty: &crate::ast::Type<P>) -> bool {
        self.bare_spines
            .contains(&(ty as *const crate::ast::Type<P> as usize))
    }

    pub(crate) fn extend_raw(&mut self, proofs: impl IntoIterator<Item = usize>) {
        self.bare_spines.extend(proofs);
    }
}

pub(crate) enum SchemeFrontier<V> {
    Continue(V),
    Complete,
    Incomplete,
    SourcePlaceholder,
    Deferred,
}

/// Candidate-only firing evidence for the consumers of the singular
/// complete-scheme summary. Production keeps no counters or persisted state.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompleteSchemeConsumerWork {
    pub(crate) source_kind: usize,
    pub(crate) goal_kind: usize,
    pub(crate) open_view: usize,
    pub(crate) retained_frontier: usize,
    pub(crate) synthesized_result: usize,
    pub(crate) goal_frontier_steps: usize,
}

#[cfg(test)]
thread_local! {
    static COMPLETE_SCHEME_CONSUMER_WORK: std::cell::Cell<CompleteSchemeConsumerWork> =
        const { std::cell::Cell::new(CompleteSchemeConsumerWork {
            source_kind: 0,
            goal_kind: 0,
            open_view: 0,
            retained_frontier: 0,
            synthesized_result: 0,
            goal_frontier_steps: 0,
        }) };
}

#[cfg(test)]
fn update_complete_scheme_consumer_work(update: impl FnOnce(&mut CompleteSchemeConsumerWork)) {
    COMPLETE_SCHEME_CONSUMER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(test)]
pub(crate) fn reset_complete_scheme_consumer_work() {
    COMPLETE_SCHEME_CONSUMER_WORK.with(|work| work.set(CompleteSchemeConsumerWork::default()));
}

#[cfg(test)]
pub(crate) fn complete_scheme_consumer_work() -> CompleteSchemeConsumerWork {
    COMPLETE_SCHEME_CONSUMER_WORK.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn record_source_kind_consumer() {
    update_complete_scheme_consumer_work(|work| work.source_kind += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_goal_kind_consumer() {
    update_complete_scheme_consumer_work(|work| work.goal_kind += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_open_view_consumer() {
    update_complete_scheme_consumer_work(|work| work.open_view += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_retained_frontier_consumer() {
    update_complete_scheme_consumer_work(|work| work.retained_frontier += 1);
}

#[cfg(test)]
pub(crate) fn record_synthesized_result_consumer() {
    update_complete_scheme_consumer_work(|work| work.synthesized_result += 1);
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn record_goal_frontier_step() {
    update_complete_scheme_consumer_work(|work| work.goal_frontier_steps += 1);
}

/// Follow only a possible `forall* -> function` frontier.
///
/// The adapter must not validate ordinary children here. This keeps the
/// established precedence where an incomplete outer higher-kinded `forall`
/// masks a descendant head/kind error, while a complete function frontier
/// allows the ordinary kind walk to expose that later error.
pub(crate) fn classify_complete_scheme<V, E>(
    mut view: V,
    mut step: impl FnMut(V) -> Result<SchemeFrontier<V>, E>,
) -> Result<CompleteScheme, E> {
    loop {
        match step(view)? {
            SchemeFrontier::Continue(next) => view = next,
            SchemeFrontier::Complete => return Ok(CompleteScheme::Complete),
            SchemeFrontier::Incomplete => return Ok(CompleteScheme::Incomplete),
            SchemeFrontier::SourcePlaceholder => {
                return Ok(CompleteScheme::SourcePlaceholder);
            }
            SchemeFrontier::Deferred => return Ok(CompleteScheme::Deferred),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_frontier_machine_short_circuits_without_visiting_children() {
        let mut visits = Vec::new();
        let summary = classify_complete_scheme(0_u8, |node| {
            visits.push(node);
            Ok::<_, std::convert::Infallible>(match node {
                0 | 1 => SchemeFrontier::Continue(node + 1),
                2 => SchemeFrontier::Complete,
                _ => unreachable!(),
            })
        })
        .expect("an infallible frontier");
        assert_eq!(summary, CompleteScheme::Complete);
        assert_eq!(visits, vec![0, 1, 2]);
    }
}

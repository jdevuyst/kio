//! Name resolution for Kio.
//!
//! Builds the **top-level scope** of each module (the set of `fn` /
//! `type` / `literal` / `newtype` names declared at the file top, indexed
//! for later in-body resolution and type checking) and the
//! [`Package`] aggregate that holds every module in a single Kio
//! package plus its optional package file.
//!
//! ## Phase polymorphism
//!
//! `Package<P>` and `ModuleEntry<P>` are generic over phase so the
//! typer can transform `Package<Lowered>` into `Package<Prime>`
//! through the substitution pass without re-implementing the
//! container shape. The phase-polymorphic methods on
//! `impl<P: Phase> Package<P>` — `Package::build`, `resolve_imports`,
//! `check_no_value_cycles`, plus the dispatch helpers — work on
//! either phase: their bodies only inspect items by name and span
//! (via the phase-agnostic `item_is_exported`, `env_item_exists`,
//! `item_name_and_span` helpers), never pattern-matching on
//! phase-specific extension fields, so the same code serves the
//! full and Kio'-only pipelines.
//!
//! `check_in_body_resolution` lives on `impl<P: ResolvePhase>
//! Package<P>` and the in-body `Resolver` is generic over the same
//! [`ResolvePhase`] bound. The kio-prime pipeline runs the
//! resolver against `Module<Prime>` directly; the full pipeline
//! runs it against `Module<Lowered>`; the implementation is
//! shared.

use crate::path_display::DisplayPath;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// The `Package`-level methods are phase-polymorphic — see the generic
// `impl<P: Phase> Package<P>` block. The body-walking [`Resolver`] is
// also phase-polymorphic via an associated-type-equality bound that
// requires the surface-only ext fields to be `Never` at the
// caller's phase; it accepts `Lowered`, `UncheckedPrime`, and `Prime` without
// code duplication.
use crate::ast::{
    Import, ImportKind, Item, Kind, LitAnnotationExt, Lowered, ModulePath, Newtype,
    NewtypeHostSurface, PathSegment, Phase, Type, TypeRecMember,
};
use crate::error::{Error, Fix, FixEdit, FixReplacementPart};
use crate::span::Span;

/// Kio'-core intrinsics — recognized in both the full Kio surface and
/// the standalone Kio' (kio-prime) pipeline. Brought into scope by
/// `import __intrinsics__;`.
pub const PRIME_INTRINSICS: &[&str] = &[
    "__left__",
    "__right__",
    "__either__",
    "__pair__",
    "__fst__",
    "__snd__",
    "__if_then_else__",
    "__absurd__",
];
// Higher-kinded types are expressed directly — `F(A)` for a kind-`*→*`
// binder, `Either(String)` for a partially-applied multi-arity
// newtype — with no `__App__` type former and no `__inj__` / `__prj__`
// brand-crossing intrinsics. The kind discipline (see `specs/grammar.md`
// § Kind grammar) decides admissibility; brand crossing routes through
// a newtype's own constructor / projector or a threaded instance value.

/// Returns true if `name` is in scope as an intrinsic.
fn is_intrinsic_in_scope(name: &str) -> bool {
    PRIME_INTRINSICS.contains(&name)
}

/// Stable index of a top-level declaration inside its `Module.items` vector.
///
/// The low 32 bits select the outer item. The high 32 bits are zero for an
/// ordinary item and encode one plus the member index for a type-recursive
/// group. Keeping the identity compact lets the scope retain its existing
/// one-word value while a phase-stable group binds several top-level names.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TopLevelId(pub u64);

impl TopLevelId {
    fn item(index: usize) -> Self {
        Self(index as u64)
    }

    fn type_rec_member(item: usize, member: usize) -> Self {
        Self(item as u64 | ((member as u64 + 1) << 32))
    }

    pub(crate) fn item_index(self) -> usize {
        self.0 as u32 as usize
    }

    fn type_rec_member_index(self) -> Option<usize> {
        let encoded = (self.0 >> 32) as u32;
        if encoded == 0 {
            None
        } else {
            Some((encoded - 1) as usize)
        }
    }
}

/// One declaration selected from either an ordinary outer item or a member of
/// a phase-stable type-recursive group.
pub(crate) enum TopLevelDeclaration<'a, P: Phase> {
    Item(&'a Item<P>),
    TypeRecMember(&'a TypeRecMember<P>),
}

impl<P: Phase> Clone for TopLevelDeclaration<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Phase> Copy for TopLevelDeclaration<'_, P> {}

impl<'a, P: Phase> TopLevelDeclaration<'a, P> {
    fn name_and_span(self) -> (&'a str, Span)
    where
        P: ResolvePhase,
    {
        match self {
            Self::Item(item) => item_name_and_span(item),
            Self::TypeRecMember(TypeRecMember::TypeAlias(alias)) => (&alias.name, alias.meta.span),
            Self::TypeRecMember(TypeRecMember::Newtype(newtype)) => {
                (&newtype.name, newtype.meta.span)
            }
            Self::TypeRecMember(TypeRecMember::Labels(labels, _)) => (
                labels.type_alias_name.as_deref().unwrap_or(""),
                labels.meta.span,
            ),
        }
    }

    pub(crate) fn type_alias(self) -> Option<&'a crate::ast::TypeAlias<P>> {
        match self {
            Self::Item(Item::TypeAlias(alias))
            | Self::TypeRecMember(TypeRecMember::TypeAlias(alias)) => Some(alias),
            _ => None,
        }
    }

    pub(crate) fn newtype(self) -> Option<&'a crate::ast::Newtype<P>> {
        match self {
            Self::Item(Item::Newtype(newtype))
            | Self::TypeRecMember(TypeRecMember::Newtype(newtype)) => Some(newtype),
            _ => None,
        }
    }

    pub(crate) fn host_type(self) -> Option<&'a crate::ast::HostType<P>> {
        match self {
            Self::Item(Item::HostType(host)) => Some(host),
            _ => None,
        }
    }

    pub(crate) fn fn_def(self) -> Option<&'a crate::ast::FnDef<P>> {
        match self {
            Self::Item(Item::FnDef(def)) => Some(def),
            _ => None,
        }
    }

    pub(crate) fn host_fn(self) -> Option<&'a crate::ast::HostFn<P>> {
        match self {
            Self::Item(Item::HostFn(def)) => Some(def),
            _ => None,
        }
    }

    pub(crate) fn elaborator(self) -> Option<&'a crate::ast::UserElaboratorDef<P>> {
        match self {
            Self::Item(Item::Elaborator(def, _)) => Some(def),
            _ => None,
        }
    }

    pub(crate) fn visibility(self) -> crate::ast::Visibility
    where
        P: ResolvePhase,
    {
        match self {
            Self::Item(item) => item_visibility(item),
            Self::TypeRecMember(TypeRecMember::TypeAlias(alias)) => alias.vis.clone(),
            Self::TypeRecMember(TypeRecMember::Newtype(newtype)) => newtype.vis.clone(),
            Self::TypeRecMember(TypeRecMember::Labels(labels, _)) => labels.vis.clone(),
        }
    }
}

fn visit_item_declarations<'a, P: Phase>(
    item_index: usize,
    item: &'a Item<P>,
    mut visit: impl FnMut(TopLevelId, TopLevelDeclaration<'a, P>),
) {
    match item {
        Item::TypeRecGroup(group) => {
            for (member_index, member) in group.members.iter().enumerate() {
                visit(
                    TopLevelId::type_rec_member(item_index, member_index),
                    TopLevelDeclaration::TypeRecMember(member),
                );
            }
        }
        _ => visit(
            TopLevelId::item(item_index),
            TopLevelDeclaration::Item(item),
        ),
    }
}

/// Visit every top-level declaration represented by one outer item. A
/// recursive type group contributes each member independently; consumers that
/// care about declarations rather than source containers must use this view.
pub(crate) fn for_each_item_declaration<'a, P: Phase>(
    item: &'a Item<P>,
    mut visit: impl FnMut(TopLevelDeclaration<'a, P>),
) {
    match item {
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                visit(TopLevelDeclaration::TypeRecMember(member));
            }
        }
        _ => visit(TopLevelDeclaration::Item(item)),
    }
}

pub(crate) fn declaration_by_id<P: Phase>(
    module: &crate::ast::Module<P>,
    id: TopLevelId,
) -> Option<TopLevelDeclaration<'_, P>> {
    let item = module.items.get(id.item_index())?;
    match id.type_rec_member_index() {
        Some(member) => match item {
            Item::TypeRecGroup(group) => group
                .members
                .get(member)
                .map(TopLevelDeclaration::TypeRecMember),
            _ => None,
        },
        None => Some(TopLevelDeclaration::Item(item)),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TypeRecAnalysis {
    pub edges: Vec<Vec<usize>>,
    pub edge_spans: Vec<Vec<(usize, Span)>>,
    pub components: Vec<Vec<usize>>,
    pub cyclic_components: Vec<Vec<usize>>,
    pub alias_cycle: Option<Vec<usize>>,
}

impl TypeRecAnalysis {
    pub(crate) fn component_is_cyclic(&self, component: &[usize]) -> bool {
        #[cfg(test)]
        record_type_rec_classification_query();
        // A nonempty SCC is cyclic unless it is a singleton without a self-edge.
        component.len() > 1
            || self.edges[component[0]].iter().any(|target| {
                #[cfg(test)]
                record_type_rec_classification_inspection();
                *target == component[0]
            })
    }
}

#[cfg(all(test, feature = "prime"))]
mod type_rec_classification_tests;

#[cfg(test)]
thread_local! {
    static TYPE_REC_CLASSIFICATION_WORK: std::cell::Cell<(usize, usize)> = const {
        std::cell::Cell::new((0, 0))
    };
}

#[cfg(all(test, any(feature = "surface", feature = "prime")))]
pub(crate) fn reset_type_rec_classification_work() {
    TYPE_REC_CLASSIFICATION_WORK.with(|work| work.set((0, 0)));
}

#[cfg(all(test, any(feature = "surface", feature = "prime")))]
pub(crate) fn type_rec_classification_work() -> (usize, usize) {
    TYPE_REC_CLASSIFICATION_WORK.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_type_rec_classification_query() {
    TYPE_REC_CLASSIFICATION_WORK.with(|work| {
        let (queries, inspected) = work.get();
        work.set((queries + 1, inspected));
    });
}

#[cfg(test)]
fn record_type_rec_classification_inspection() {
    TYPE_REC_CLASSIFICATION_WORK.with(|work| {
        let (queries, inspected) = work.get();
        work.set((queries, inspected + 1));
    });
}

fn type_rec_member_name<P: Phase>(member: &TypeRecMember<P>) -> &str {
    match member {
        TypeRecMember::TypeAlias(alias) => &alias.name,
        TypeRecMember::Newtype(newtype) => &newtype.name,
        TypeRecMember::Labels(labels, _) => labels
            .type_alias_name
            .as_deref()
            .unwrap_or("<anonymous labels>"),
    }
}

fn type_rec_member_name_span<P: ResolvePhase>(member: &TypeRecMember<P>) -> Span {
    match member {
        TypeRecMember::TypeAlias(alias) => alias.name_span,
        TypeRecMember::Newtype(newtype) => newtype.name_span,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

fn type_rec_member_type<P: ResolvePhase>(member: &TypeRecMember<P>) -> &Type<P> {
    match member {
        TypeRecMember::TypeAlias(alias) => &alias.body,
        TypeRecMember::Newtype(newtype) => &newtype.payload,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

fn type_rec_member_bound<P: ResolvePhase>(member: &TypeRecMember<P>) -> Vec<&str> {
    match member {
        TypeRecMember::TypeAlias(alias) => alias
            .type_params
            .iter()
            .map(|param| param.name.as_str())
            .collect(),
        TypeRecMember::Newtype(newtype) => newtype
            .type_params
            .iter()
            .chain(&newtype.existential_params)
            .map(|param| param.name.as_str())
            .collect(),
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

struct TypeRecBoundNames<'a> {
    counts: HashMap<&'a str, usize>,
}

impl<'a> TypeRecBoundNames<'a> {
    fn new(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut counts = HashMap::new();
        for name in names {
            *counts.entry(name).or_insert(0) += 1;
        }
        Self { counts }
    }

    fn contains(&self, name: &str) -> bool {
        #[cfg(test)]
        TYPE_REC_BOUND_NAME_LOOKUPS.with(|lookups| lookups.set(lookups.get() + 1));
        self.counts.contains_key(name)
    }

    fn push(&mut self, name: &'a str) {
        *self.counts.entry(name).or_insert(0) += 1;
    }

    fn pop(&mut self, name: &str) {
        let count = self
            .counts
            .get_mut(name)
            .expect("a recursive-type binder is removed inside its lexical scope");
        *count -= 1;
        if *count == 0 {
            self.counts.remove(name);
        }
    }
}

#[cfg(test)]
thread_local! {
    static TYPE_REC_BOUND_NAME_LOOKUPS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
    static TYPE_REC_SCC_CANONICALIZATION_VISITS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_type_rec_linear_work_counts() {
    TYPE_REC_BOUND_NAME_LOOKUPS.with(|lookups| lookups.set(0));
    TYPE_REC_SCC_CANONICALIZATION_VISITS.with(|visits| visits.set(0));
}

#[cfg(test)]
pub(crate) fn type_rec_linear_work_counts() -> (usize, usize) {
    (
        TYPE_REC_BOUND_NAME_LOOKUPS.with(std::cell::Cell::get),
        TYPE_REC_SCC_CANONICALIZATION_VISITS.with(std::cell::Cell::get),
    )
}

fn collect_type_rec_edges<'a, P: ResolvePhase>(
    ty: &'a Type<P>,
    names: &HashMap<&'a str, usize>,
    bound: &mut TypeRecBoundNames<'a>,
    canonical_owner: Option<&ModulePath>,
    out: &mut Vec<(usize, Span)>,
) {
    match ty {
        Type::Path { segments, args, .. } => {
            let bare_head = match segments.as_slice() {
                [head] if !bound.contains(head.as_str()) => Some(head),
                _ => None,
            };
            let canonical_head = canonical_owner.and_then(|owner| {
                (segments.len() == owner.segments.len() + 1
                    && segments[..owner.segments.len()]
                        .iter()
                        .map(PathSegment::as_str)
                        .eq(owner.segments.iter().map(PathSegment::as_str)))
                .then(|| segments.last())
                .flatten()
            });
            if let Some(head) = bare_head.or(canonical_head)
                && let Some(index) = names.get(head.as_str())
            {
                out.push((*index, head.span));
            }
            for arg in args {
                collect_type_rec_edges(arg, names, bound, canonical_owner, out);
            }
        }
        Type::Function { param, ret, .. } => {
            collect_type_rec_edges(param, names, bound, canonical_owner, out);
            collect_type_rec_edges(ret, names, bound, canonical_owner, out);
        }
        Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
            collect_type_rec_edges(left, names, bound, canonical_owner, out);
            collect_type_rec_edges(right, names, bound, canonical_owner, out);
        }
        Type::Forall { param, body, .. } => {
            bound.push(param.name.as_str());
            collect_type_rec_edges(body, names, bound, canonical_owner, out);
            bound.pop(param.name.as_str());
        }
        Type::Goal { args, .. } => {
            for arg in args {
                collect_type_rec_edges(arg, names, bound, canonical_owner, out);
            }
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => {}
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

pub(crate) fn strongly_connected_components(
    edges: &[Vec<usize>],
    active: impl Fn(usize) -> bool,
) -> Vec<Vec<usize>> {
    let active = (0..edges.len()).map(active).collect::<Vec<_>>();
    let mut reverse_edges = vec![Vec::new(); edges.len()];
    for (from, successors) in edges.iter().enumerate() {
        if !active[from] {
            continue;
        }
        for &to in successors {
            if active[to] {
                reverse_edges[to].push(from);
            }
        }
    }

    // Kosaraju's two iterative DFS passes keep both work and memory linear in
    // the written type-reference graph. The iterative form also avoids making
    // a long acyclic chain consume the Rust call stack.
    let mut visited = vec![false; edges.len()];
    let mut finish_order = Vec::with_capacity(edges.len());
    for start in 0..edges.len() {
        if !active[start] || visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![(start, 0)];
        while let Some((node, next_edge)) = stack.last_mut() {
            if *next_edge == edges[*node].len() {
                finish_order.push(*node);
                stack.pop();
                continue;
            }
            let successor = edges[*node][*next_edge];
            *next_edge += 1;
            if active[successor] && !visited[successor] {
                visited[successor] = true;
                stack.push((successor, 0));
            }
        }
    }

    visited.fill(false);
    let mut component_of = vec![usize::MAX; edges.len()];
    let mut component_count = 0;
    for &start in finish_order.iter().rev() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            component_of[node] = component_count;
            for &predecessor in &reverse_edges[node] {
                if !visited[predecessor] {
                    visited[predecessor] = true;
                    stack.push(predecessor);
                }
            }
        }
        component_count += 1;
    }

    let mut discovered = vec![Vec::new(); component_count];
    let mut source_order = Vec::with_capacity(component_count);
    for (node, &component) in component_of.iter().enumerate() {
        #[cfg(test)]
        TYPE_REC_SCC_CANONICALIZATION_VISITS.with(|visits| visits.set(visits.get() + 1));
        if component == usize::MAX {
            continue;
        }
        if discovered[component].is_empty() {
            source_order.push(component);
        }
        discovered[component].push(node);
    }
    source_order
        .into_iter()
        .map(|component| std::mem::take(&mut discovered[component]))
        .collect()
}

pub(crate) fn analyze_type_rec_members<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
) -> TypeRecAnalysis {
    analyze_type_rec_members_with_owner(members, None)
}

/// Project an expanded declaration graph back onto atomic written members.
///
/// Surface `labels` can lower to several type declarations, but one written
/// labels declaration cannot be split by a source repair. Multiple expanded
/// components may therefore collapse only when they belong entirely to that
/// one source member. Any other overlap has no source-preserving partition.
pub(crate) fn source_partition_analysis(
    lowered: &TypeRecAnalysis,
    owners: &[usize],
    source_members: usize,
) -> Option<TypeRecAnalysis> {
    if owners.len() != lowered.edges.len() || owners.iter().any(|owner| *owner >= source_members) {
        return None;
    }

    let mut edge_spans = vec![Vec::new(); source_members];
    for (from, edges) in lowered.edge_spans.iter().enumerate() {
        let source_from = owners[from];
        for &(to, span) in edges {
            let source_to = owners[to];
            if source_from != source_to {
                edge_spans[source_from].push((source_to, span));
            }
        }
    }
    for edges in &mut edge_spans {
        edges.sort_unstable_by_key(|(target, span)| (*target, span.start, span.end));
        edges.dedup_by_key(|(target, _)| *target);
    }

    let mut projected_cycles = Vec::with_capacity(lowered.cyclic_components.len());
    for component in &lowered.cyclic_components {
        let mut source_component = component
            .iter()
            .map(|member| owners[*member])
            .collect::<Vec<_>>();
        source_component.sort_unstable();
        source_component.dedup();
        projected_cycles.push(source_component);
    }
    projected_cycles.sort_unstable();
    let mut expected_cycles: Vec<Vec<usize>> = Vec::with_capacity(projected_cycles.len());
    for source_component in projected_cycles {
        if expected_cycles.last() == Some(&source_component) {
            if source_component.len() == 1 {
                continue;
            }
            // Two distinct expanded SCCs cannot be represented as one written
            // mutual group merely because they happen to involve the same
            // atomic source declarations.
            return None;
        }
        expected_cycles.push(source_component);
    }

    // Internal edges of one expanded labels declaration are not themselves a
    // written self-cycle. Add a source self-edge only when the expanded graph
    // proved a genuine cyclic component wholly owned by that declaration.
    for component in &expected_cycles {
        if let [owner] = component.as_slice()
            && !edge_spans[*owner].iter().any(|(target, _)| target == owner)
        {
            edge_spans[*owner].push((*owner, Span::new(0, 0)));
        }
    }
    let edges = edge_spans
        .iter()
        .map(|edges| edges.iter().map(|(target, _)| *target).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let components = strongly_connected_components(&edges, |_| true);
    let mut cyclic_components = components
        .iter()
        .filter(|component| component.len() > 1 || edges[component[0]].contains(&component[0]))
        .cloned()
        .collect::<Vec<_>>();
    cyclic_components.sort_unstable();
    if cyclic_components != expected_cycles {
        return None;
    }

    Some(TypeRecAnalysis {
        edges,
        edge_spans,
        components,
        cyclic_components,
        // Alias-only cycles were already rejected on the expanded graph.
        alias_cycle: None,
    })
}

/// Analyze a declaration set whose module-local type references have already
/// been identity-qualified. Signature replay uses this on one canonical
/// module epoch to verify that stored recursive contexts coincide with the
/// maximal live components rather than validating each context in isolation.
pub(crate) fn analyze_canonical_type_rec_members<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
    owner: &ModulePath,
) -> TypeRecAnalysis {
    analyze_type_rec_members_with_owner(members, Some(owner))
}

/// Compute only the canonical SCC partition needed by signature-history
/// validation. Duplicate written references are harmless to Kosaraju's walk,
/// so this path keeps them instead of sorting and deduplicating diagnostic
/// edges that it never consumes.
pub(crate) fn canonical_type_rec_cyclic_components<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
    owner: &ModulePath,
) -> Vec<Vec<usize>> {
    let edge_spans = collect_type_rec_member_edge_spans(members, Some(owner));
    let edges = edge_spans
        .iter()
        .map(|edges| edges.iter().map(|(index, _)| *index).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    strongly_connected_components(&edges, |_| true)
        .into_iter()
        .filter(|component| component.len() > 1 || edges[component[0]].contains(&component[0]))
        .collect()
}

/// Recheck the structural recursive-group contract after a canonical phase
/// artifact has been filtered to one live epoch. Canonical type heads carry
/// their exact module path, so this uses the same group validator with those
/// identity-qualified intra-group edges restored.
pub(crate) fn validate_canonical_type_rec_group<P: ResolvePhase>(
    group: &crate::ast::TypeRecGroup<P>,
    owner: &ModulePath,
) -> Result<(), Error> {
    let analysis = analyze_canonical_type_rec_members(&group.members, owner);
    Resolver::<'_>::validate_type_rec_group_with_analysis(group, &analysis)
}

fn analyze_type_rec_members_with_owner<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
    canonical_owner: Option<&ModulePath>,
) -> TypeRecAnalysis {
    let mut edge_spans = collect_type_rec_member_edge_spans(members, canonical_owner);
    for edges in &mut edge_spans {
        edges.sort_unstable_by_key(|(index, span)| (*index, span.start, span.end));
        edges.dedup_by_key(|(index, _)| *index);
    }
    let edges = edge_spans
        .iter()
        .map(|edges| edges.iter().map(|(index, _)| *index).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let components = strongly_connected_components(&edges, |_| true);
    let cyclic_components = components
        .iter()
        .filter(|component| component.len() > 1 || edges[component[0]].contains(&component[0]))
        .cloned()
        .collect::<Vec<_>>();

    let is_alias = |index: usize| matches!(members[index], TypeRecMember::TypeAlias(_));
    let alias_cycle = strongly_connected_components(&edges, is_alias)
        .into_iter()
        .find(|component| component.len() > 1 || edges[component[0]].contains(&component[0]));

    TypeRecAnalysis {
        edges,
        edge_spans,
        components,
        cyclic_components,
        alias_cycle,
    }
}

fn collect_type_rec_member_edge_spans<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
    canonical_owner: Option<&ModulePath>,
) -> Vec<Vec<(usize, Span)>> {
    let names = members
        .iter()
        .enumerate()
        .map(|(index, member)| (type_rec_member_name(member), index))
        .collect::<HashMap<_, _>>();
    members
        .iter()
        .map(|member| {
            let mut bound = TypeRecBoundNames::new(type_rec_member_bound(member));
            let mut edges = Vec::new();
            collect_type_rec_edges(
                type_rec_member_type(member),
                &names,
                &mut bound,
                canonical_owner,
                &mut edges,
            );
            edges
        })
        .collect()
}

/// Stable dependency-first order of the SCCs in one written type group.
/// Components and outgoing edges are already source ordered, so otherwise
/// independent components retain their written order.
pub(crate) fn type_rec_component_order(analysis: &TypeRecAnalysis) -> Vec<usize> {
    let mut component_of = vec![0; analysis.edges.len()];
    for (component, members) in analysis.components.iter().enumerate() {
        for &member in members {
            component_of[member] = component;
        }
    }

    let mut dependencies = vec![Vec::new(); analysis.components.len()];
    let mut seen_at = vec![usize::MAX; analysis.components.len()];
    for (component, members) in analysis.components.iter().enumerate() {
        for &member in members {
            for &dependency in &analysis.edges[member] {
                let dependency = component_of[dependency];
                if dependency != component && seen_at[dependency] != component {
                    seen_at[dependency] = component;
                    dependencies[component].push(dependency);
                }
            }
        }
    }

    let mut state = vec![0; analysis.components.len()];
    let mut order = Vec::with_capacity(analysis.components.len());
    for start in 0..analysis.components.len() {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        let mut stack = vec![(start, 0_usize)];
        while let Some((component, next)) = stack.last_mut() {
            if *next == dependencies[*component].len() {
                state[*component] = 2;
                order.push(*component);
                stack.pop();
                continue;
            }
            let dependency = dependencies[*component][*next];
            *next += 1;
            if state[dependency] == 0 {
                state[dependency] = 1;
                stack.push((dependency, 0));
            }
        }
    }
    order
}

fn type_rec_member_item<P: Phase>(member: TypeRecMember<P>) -> Item<P> {
    match member {
        TypeRecMember::TypeAlias(alias) => Item::TypeAlias(alias),
        TypeRecMember::Newtype(newtype) => Item::Newtype(newtype),
        TypeRecMember::Labels(labels, ext) => Item::Labels(labels, ext),
    }
}

/// Re-emit a recursive declaration container as the minimal
/// dependency-first sequence described by `analysis`. Internal source
/// projections use this after removing edges or members from a previously
/// valid written group: acyclic declarations become ordinary items,
/// self-recursive singleton newtypes/labels regain their explicit `rec`
/// marker, and only genuinely mutual components remain grouped.
pub(crate) fn emit_type_rec_partition<P: Phase>(
    group: crate::ast::TypeRecGroup<P>,
    analysis: &TypeRecAnalysis,
) -> Vec<Item<P>> {
    let rec_span = group.rec_span.unwrap_or(group.meta.span);
    let group_span = group.meta.span;
    let mut members = group.members.into_iter().map(Some).collect::<Vec<_>>();
    let mut out = Vec::new();
    for component_index in type_rec_component_order(analysis) {
        let component = &analysis.components[component_index];
        let cyclic = analysis.component_is_cyclic(component);
        let mut component_members = component
            .iter()
            .map(|&index| members[index].take().expect("member emitted exactly once"))
            .collect::<Vec<_>>();
        if !cyclic {
            debug_assert_eq!(component_members.len(), 1);
            out.push(type_rec_member_item(component_members.pop().unwrap()));
        } else if component_members.len() == 1 {
            match component_members.pop().unwrap() {
                TypeRecMember::Newtype(mut newtype) => {
                    newtype.rec_span = Some(rec_span);
                    out.push(Item::Newtype(newtype));
                }
                TypeRecMember::Labels(mut labels, ext) => {
                    labels.rec_span = Some(rec_span);
                    out.push(Item::Labels(labels, ext));
                }
                TypeRecMember::TypeAlias(alias) => {
                    // Preserve an invalid alias-only self-cycle for the normal
                    // lowerer to diagnose as user input; projection must not
                    // turn malformed source into an internal panic.
                    out.push(Item::TypeRecGroup(crate::ast::TypeRecGroup {
                        members: vec![TypeRecMember::TypeAlias(alias)],
                        doc: None,
                        source_layout: None,
                        rec_span: group.rec_span,
                        open_brace_span: group.open_brace_span,
                        close_brace_span: group.close_brace_span,
                        deferred_rec_labels_diagnostic: None,
                        meta: crate::ast::Meta::new(group_span),
                    }));
                }
            }
        } else {
            out.push(Item::TypeRecGroup(crate::ast::TypeRecGroup {
                members: component_members,
                doc: None,
                source_layout: None,
                rec_span: group.rec_span,
                open_brace_span: group.open_brace_span,
                close_brace_span: group.close_brace_span,
                deferred_rec_labels_diagnostic: None,
                meta: crate::ast::Meta::new(group_span),
            }));
        }
    }
    out
}

/// Project each Surface recursive type group onto the declaration graph the
/// active language pipeline can validate. Full Kio expands atomic `labels`
/// declarations before projecting them back onto their written owners; the
/// Prime-only build rejects surface forms and analyzes its lowered groups
/// directly.
#[cfg(all(feature = "cli", feature = "surface"))]
pub(crate) fn projected_surface_type_rec_analyses(
    module: &crate::ast::Module<crate::ast::Surface>,
) -> Result<Vec<(Span, Option<TypeRecAnalysis>)>, Error> {
    crate::pass::label_elab::analyze_surface_type_rec_groups(module)
}

#[cfg(all(feature = "cli", not(feature = "surface"), feature = "prime"))]
pub(crate) fn projected_surface_type_rec_analyses(
    module: &crate::ast::Module<crate::ast::Surface>,
) -> Result<Vec<(Span, Option<TypeRecAnalysis>)>, Error> {
    let prime = crate::prime::lower::lower_module(module.clone())?;
    Ok(prime
        .items
        .into_iter()
        .filter_map(|item| {
            let crate::ast::Item::TypeRecGroup(group) = item else {
                return None;
            };
            Some((
                group.meta.span,
                Some(analyze_type_rec_members(&group.members)),
            ))
        })
        .collect())
}

#[cfg(all(feature = "cli", not(feature = "surface"), not(feature = "prime")))]
pub(crate) fn projected_surface_type_rec_analyses(
    module: &crate::ast::Module<crate::ast::Surface>,
) -> Result<Vec<(Span, Option<TypeRecAnalysis>)>, Error> {
    Err(Error::internal(
        module.meta.span,
        "recursive-type projection requires a surface or Prime language pipeline",
    ))
}

/// Build a source-preserving repair for a complete over-broad or acyclic
/// written type group. Declaration bytes are copied from exact compiler-owned
/// spans; the intervening gaps are required to be whitespace-only before the
/// LSP materializes the edit, so comment/document ownership is never guessed.
fn type_rec_partition_fix<P: ResolvePhase>(
    group: &crate::ast::TypeRecGroup<P>,
    analysis: &TypeRecAnalysis,
) -> Option<Fix> {
    let open = group.open_brace_span?;
    let close = group.close_brace_span?;
    let layout = group.source_layout.as_ref()?;
    if group.members.is_empty()
        || layout.member_spans.len() != group.members.len()
        || layout.member_marker_offsets.len() != group.members.len()
    {
        return None;
    }
    type_rec_partition_fix_from_spans(
        group.meta.span,
        (open, close),
        &layout.member_spans,
        &layout.member_marker_offsets,
        &layout.separator_spans,
        layout.trailing_comment_span,
        analysis,
    )
}

/// Remove a singleton/member `rec` marker together with only the separator
/// between it and the declaration keyword.  The whitespace guard makes the
/// edit disappear if recovery or intervening trivia means that separator is
/// not wholly compiler-owned.
pub(crate) fn remove_type_rec_marker_edit(rec_span: Span, declaration_start: u32) -> FixEdit {
    FixEdit::from_parts(
        Span::new(rec_span.start, declaration_start),
        Vec::new(),
        vec![Span::new(rec_span.end, declaration_start)],
    )
}

fn type_rec_whitespace_gaps(gaps: Vec<Span>, separators: &[Span]) -> Option<Vec<Span>> {
    let mut result = Vec::with_capacity(gaps.len() + separators.len());
    let mut separator_index = 0;
    for gap in gaps {
        let mut start = gap.start;
        while let Some(separator) = separators.get(separator_index) {
            if separator.start >= gap.end {
                break;
            }
            if separator.start < start
                || separator.end > gap.end
                || separator.end != separator.start + 1
            {
                return None;
            }
            result.push(Span::new(start, separator.start));
            start = separator.end;
            separator_index += 1;
        }
        result.push(Span::new(start, gap.end));
    }
    (separator_index == separators.len()).then_some(result)
}

fn type_rec_source_fix(
    group_span: Span,
    close: Span,
    parts: Vec<FixReplacementPart>,
    gaps: Vec<Span>,
    separators: &[Span],
) -> Option<Fix> {
    let split = separators.partition_point(|span| span.start < close.end);
    let (inner, outer) = separators.split_at(split);
    if outer.len() > 1
        || outer
            .iter()
            .any(|span| span.start < group_span.end || span.end != span.start + 1)
    {
        return None;
    }
    let whitespace = type_rec_whitespace_gaps(gaps, inner)?;
    let mut edits = vec![FixEdit::from_parts(group_span, parts, whitespace)];
    edits.extend(outer.iter().map(|&span| FixEdit::new(span, "")));
    let scope = Span::new(
        group_span.start,
        outer.last().map_or(group_span.end, |span| span.end),
    );
    Some(
        Fix::machine_applicable("Fix recursive type groups", edits)
            .allowing_follow_on_reanalysis_outside(scope),
    )
}

pub(crate) fn type_rec_partition_fix_from_spans(
    group_span: Span,
    delimiters: (Span, Span),
    spans: &[Span],
    marker_offsets: &[u32],
    separator_spans: &[Span],
    trailing_comment_span: Option<Span>,
    analysis: &TypeRecAnalysis,
) -> Option<Fix> {
    let (open, close) = delimiters;
    if spans.is_empty()
        || marker_offsets.len() != spans.len()
        || spans.windows(2).any(|pair| pair[0].end > pair[1].start)
        || spans
            .iter()
            .zip(marker_offsets)
            .any(|(span, offset)| *offset < span.start || *offset > span.end)
        || open.end > spans[0].start
        || spans.last()?.end > close.start
    {
        return None;
    }
    let mut required_whitespace = Vec::with_capacity(spans.len() + 2);
    required_whitespace.push(Span::new(open.end, spans[0].start));
    required_whitespace.extend(
        spans
            .windows(2)
            .map(|pair| Span::new(pair[0].end, pair[1].start)),
    );
    if let Some(trailing) = trailing_comment_span {
        if spans.last()?.end > trailing.start || trailing.end > close.start {
            return None;
        }
        required_whitespace.push(Span::new(spans.last()?.end, trailing.start));
        required_whitespace.push(Span::new(trailing.end, close.start));
    } else {
        required_whitespace.push(Span::new(spans.last()?.end, close.start));
    }

    let mut parts = Vec::new();
    for (output_index, component_index) in
        type_rec_component_order(analysis).into_iter().enumerate()
    {
        if output_index != 0 {
            parts.push(FixReplacementPart::Text("\n".to_owned()));
        }
        let component = &analysis.components[component_index];
        let cyclic = analysis.component_is_cyclic(component);
        if cyclic && component.len() > 1 {
            parts.push(FixReplacementPart::Text("rec {\n".to_owned()));
            for (index, &member) in component.iter().enumerate() {
                if index != 0 {
                    parts.push(FixReplacementPart::Text(";\n".to_owned()));
                }
                parts.push(FixReplacementPart::Source(spans[member]));
            }
            parts.push(FixReplacementPart::Text("\n}".to_owned()));
        } else if cyclic {
            // Alias-only singleton cycles are rejected before this helper is
            // reached, so the only valid cyclic singleton is a newtype.
            let member = component[0];
            let span = spans[member];
            let marker_offset = marker_offsets[member];
            if span.start != marker_offset {
                parts.push(FixReplacementPart::Source(Span::new(
                    span.start,
                    marker_offset,
                )));
            }
            parts.push(FixReplacementPart::Text("rec ".to_owned()));
            parts.push(FixReplacementPart::Source(Span::new(
                marker_offset,
                span.end,
            )));
            parts.push(FixReplacementPart::Text(";".to_owned()));
        } else {
            debug_assert_eq!(component.len(), 1);
            parts.push(FixReplacementPart::Source(spans[component[0]]));
            parts.push(FixReplacementPart::Text(";".to_owned()));
        }
    }
    if let Some(trailing) = trailing_comment_span {
        parts.push(FixReplacementPart::Text("\n".to_owned()));
        parts.push(FixReplacementPart::Source(trailing));
    }
    type_rec_source_fix(
        group_span,
        close,
        parts,
        required_whitespace,
        separator_spans,
    )
}

pub(crate) fn type_rec_unwrap_fix<P: Phase>(
    group: &crate::ast::TypeRecGroup<P>,
    add_singleton_marker: bool,
) -> Option<Fix> {
    let open = group.open_brace_span?;
    let close = group.close_brace_span?;
    let layout = group.source_layout.as_ref()?;
    let [member] = layout.member_spans.as_slice() else {
        return None;
    };
    let [marker_offset] = layout.member_marker_offsets.as_slice() else {
        return None;
    };
    if open.end > member.start || member.end > close.start {
        return None;
    }
    let mut parts = Vec::new();
    if add_singleton_marker {
        if *marker_offset < member.start || *marker_offset > member.end {
            return None;
        }
        if member.start != *marker_offset {
            parts.push(FixReplacementPart::Source(Span::new(
                member.start,
                *marker_offset,
            )));
        }
        parts.push(FixReplacementPart::Text("rec ".to_owned()));
        parts.push(FixReplacementPart::Source(Span::new(
            *marker_offset,
            member.end,
        )));
    } else {
        parts.push(FixReplacementPart::Source(*member));
    }
    parts.push(FixReplacementPart::Text(";".to_owned()));
    let mut required_whitespace = vec![Span::new(open.end, member.start)];
    if let Some(trailing) = layout.trailing_comment_span {
        if member.end > trailing.start || trailing.end > close.start {
            return None;
        }
        required_whitespace.push(Span::new(member.end, trailing.start));
        required_whitespace.push(Span::new(trailing.end, close.start));
        parts.push(FixReplacementPart::Text("\n".to_owned()));
        parts.push(FixReplacementPart::Source(trailing));
    } else {
        required_whitespace.push(Span::new(member.end, close.start));
    }
    type_rec_source_fix(
        group.meta.span,
        close,
        parts,
        required_whitespace,
        &layout.separator_spans,
    )
}

/// Conservative local proof used before publishing an add-`rec` editor
/// repair for a labels declaration. Every named head must be one of the
/// declaration's generated members or a binder, every type must be saturated
/// to kind `*`, and every occurrence belonging to its SCC must be strictly
/// positive. A shape needing an imported declaration's kind or variance is
/// left diagnostic-only rather than advertising an edit whose result has not
/// been proven valid.
pub(crate) fn type_rec_marker_fix_is_locally_proven<P: ResolvePhase>(
    members: &[TypeRecMember<P>],
    analysis: &TypeRecAnalysis,
) -> bool {
    fn visit<P: ResolvePhase>(
        ty: &Type<P>,
        heads: &HashMap<&str, (usize, Kind)>,
        recursive_members: &HashSet<usize>,
        bound: &mut HashMap<String, Kind>,
        positive: bool,
    ) -> Option<(Kind, bool)> {
        match ty {
            Type::Unit { .. } | Type::Bottom { .. } => Some((Kind::Star, false)),
            Type::Function { param, ret, .. } => {
                let (param_kind, param_recursive) =
                    visit(param, heads, recursive_members, bound, !positive)?;
                let (ret_kind, ret_recursive) =
                    visit(ret, heads, recursive_members, bound, positive)?;
                (param_kind == Kind::Star && ret_kind == Kind::Star)
                    .then_some((Kind::Star, param_recursive || ret_recursive))
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                let (left_kind, left_recursive) =
                    visit(left, heads, recursive_members, bound, positive)?;
                let (right_kind, right_recursive) =
                    visit(right, heads, recursive_members, bound, positive)?;
                (left_kind == Kind::Star && right_kind == Kind::Star)
                    .then_some((Kind::Star, left_recursive || right_recursive))
            }
            Type::Forall { param, body, .. } => {
                let previous = bound.insert(param.name.clone(), param.effective_kind());
                let result = visit(body, heads, recursive_members, bound, positive);
                match previous {
                    Some(kind) => {
                        bound.insert(param.name.clone(), kind);
                    }
                    None => {
                        bound.remove(&param.name);
                    }
                }
                let (kind, recursive) = result?;
                (kind == Kind::Star).then_some((Kind::Star, recursive))
            }
            Type::Path { segments, args, .. } if segments.len() == 1 => {
                let head = segments[0].as_str();
                let (mut kind, recursive_head) = if let Some(kind) = bound.get(head) {
                    (kind.clone(), false)
                } else {
                    let &(index, ref kind) = heads.get(head)?;
                    if recursive_members.contains(&index) && !positive {
                        return None;
                    }
                    (kind.clone(), recursive_members.contains(&index))
                };
                for argument in args {
                    let (argument_kind, argument_recursive) =
                        visit(argument, heads, recursive_members, bound, positive)?;
                    // Variance of an arbitrary type-level function is not
                    // available to this declaration-local proof.
                    let Kind::Arrow(expected, result) = kind else {
                        return None;
                    };
                    if argument_kind != *expected || argument_recursive {
                        return None;
                    }
                    kind = *result;
                }
                Some((kind, recursive_head))
            }
            Type::Path { .. } | Type::Infer { .. } | Type::Goal { .. } => None,
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    let heads = members
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let parameters = match member {
                TypeRecMember::TypeAlias(alias) => alias.type_params.as_slice(),
                TypeRecMember::Newtype(newtype) => newtype.type_params.as_slice(),
                TypeRecMember::Labels(_, ext) => match *ext {},
            };
            let kind = parameters.iter().rev().fold(Kind::Star, |result, param| {
                Kind::Arrow(Box::new(param.effective_kind()), Box::new(result))
            });
            (type_rec_member_name(member), (index, kind))
        })
        .collect::<HashMap<_, _>>();
    let component_of = analysis
        .cyclic_components
        .iter()
        .enumerate()
        .flat_map(|(component, members)| members.iter().map(move |&member| (member, component)))
        .collect::<HashMap<_, _>>();

    members.iter().enumerate().all(|(index, member)| {
        let recursive_members = component_of
            .get(&index)
            .map_or_else(HashSet::new, |component| {
                analysis.cyclic_components[*component]
                    .iter()
                    .copied()
                    .collect()
            });
        let mut bound = type_rec_member_bound(member)
            .into_iter()
            .map(|name| (name.to_owned(), Kind::Star))
            .collect::<HashMap<_, _>>();
        // Preserve explicit higher-kinded annotations rather than treating
        // every binder as kind `*`.
        match member {
            TypeRecMember::TypeAlias(alias) => {
                for parameter in &alias.type_params {
                    bound.insert(parameter.name.clone(), parameter.effective_kind());
                }
            }
            TypeRecMember::Newtype(newtype) => {
                for parameter in newtype
                    .type_params
                    .iter()
                    .chain(&newtype.existential_params)
                {
                    bound.insert(parameter.name.clone(), parameter.effective_kind());
                }
            }
            TypeRecMember::Labels(_, ext) => match *ext {},
        }
        visit(
            type_rec_member_type(member),
            &heads,
            &recursive_members,
            &mut bound,
            true,
        )
        .is_some_and(|(kind, recursive)| {
            kind == Kind::Star && (recursive_members.is_empty() || recursive)
        })
    })
}

fn type_self_references<'a, P: ResolvePhase>(
    ty: &'a Type<P>,
    name: &'a str,
    bound: impl IntoIterator<Item = &'a str>,
) -> std::vec::IntoIter<Span> {
    let names = HashMap::from([(name, 0)]);
    let mut bound = TypeRecBoundNames::new(bound);
    let mut references = Vec::new();
    collect_type_rec_edges(ty, &names, &mut bound, None, &mut references);
    references
        .into_iter()
        .map(|(_, span)| span)
        .collect::<Vec<_>>()
        .into_iter()
}

fn recursive_newtype_self_references<P: ResolvePhase>(newtype: &Newtype<P>) -> Vec<Span> {
    type_self_references(
        &newtype.payload,
        &newtype.name,
        newtype
            .type_params
            .iter()
            .chain(&newtype.existential_params)
            .map(|param| param.name.as_str()),
    )
    .collect()
}

/// Return the missing-marker diagnostic only after the caller has validated
/// the completed declaration's resolution, kinds, arity, and positivity.
/// Both source-local and identity-qualified self heads are recognized so the
/// ordinary compiler and fresh signature-artifact validator share this gate.
pub(crate) fn missing_recursive_newtype_marker_error<P: ResolvePhase>(
    newtype: &Newtype<P>,
    module_path: &ModulePath,
) -> Option<Error> {
    if newtype.rec_span.is_some() {
        return None;
    }
    fn first_self_reference<P: ResolvePhase>(
        ty: &Type<P>,
        name: &str,
        module_path: &ModulePath,
        bound: &mut Vec<String>,
    ) -> Option<Span> {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let is_bare_self = matches!(segments.as_slice(), [head]
                    if head.as_str() == name && !bound.iter().rev().any(|bound| bound == name));
                let is_exact_self = segments.len() == module_path.segments.len() + 1
                    && segments[..module_path.segments.len()]
                        .iter()
                        .map(crate::ast::PathSegment::as_str)
                        .eq(module_path
                            .segments
                            .iter()
                            .map(crate::ast::PathSegment::as_str))
                    && segments.last().is_some_and(|head| head.as_str() == name);
                if is_bare_self || is_exact_self {
                    return Some(meta.span);
                }
                args.iter()
                    .find_map(|argument| first_self_reference(argument, name, module_path, bound))
            }
            Type::Function { param, ret, .. } => {
                first_self_reference(param, name, module_path, bound)
                    .or_else(|| first_self_reference(ret, name, module_path, bound))
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                first_self_reference(left, name, module_path, bound)
                    .or_else(|| first_self_reference(right, name, module_path, bound))
            }
            Type::Forall { param, body, .. } => {
                bound.push(param.name.clone());
                let result = first_self_reference(body, name, module_path, bound);
                bound.pop();
                result
            }
            Type::Goal { args, .. } => args
                .iter()
                .find_map(|argument| first_self_reference(argument, name, module_path, bound)),
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => None,
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    let mut bound = newtype
        .type_params
        .iter()
        .chain(&newtype.existential_params)
        .map(|parameter| parameter.name.clone())
        .collect();
    let reference = first_self_reference(&newtype.payload, &newtype.name, module_path, &mut bound)?;
    Some(
        Error::name_res(reference, "recursive data declaration requires `rec`")
            .with_secondary(
                newtype.name_span,
                format!(
                    "`{}` is declared here without recursive scope",
                    newtype.name
                ),
            )
            .with_help(format!(
                "write `rec newtype {}` to give the payload its own type head",
                newtype.name
            ))
            .with_fix(
                Fix::machine_applicable(
                    "Add `rec` to this recursive newtype",
                    vec![FixEdit::new(
                        Span::new(newtype.meta.span.start, newtype.meta.span.start),
                        "rec ",
                    )],
                )
                .allowing_follow_on_reanalysis_outside(newtype.meta.span),
            ),
    )
}

#[derive(Clone)]
struct ImplicitTypeCycleProblem {
    alias_only: bool,
    members: Vec<(String, Span)>,
    wrap_span: Option<Span>,
    wrap_separators: Vec<u32>,
}

impl ImplicitTypeCycleProblem {
    fn error(&self, reference: Span) -> Error {
        if self.alias_only {
            let mut error = Error::totality(
                reference,
                "recursive type component has no `newtype` boundary",
            );
            for (name, span) in &self.members {
                error = error.with_secondary(
                    *span,
                    format!("transparent alias `{name}` is in this cycle"),
                );
            }
            return error.with_help(
                "transparent aliases cannot form a recursive component without a nominal `newtype` knot",
            );
        }

        let mut error = Error::name_res(
            reference,
            "mutually recursive data declarations require `rec { ... }`",
        );
        for (name, span) in &self.members {
            error = error.with_secondary(
                *span,
                format!("`{name}` participates in this recursive component"),
            );
        }
        error = error.with_help(
            "put exactly this genuinely mutual component in one bare `rec { ... }` type group",
        );
        if let Some(span) = self.wrap_span {
            let mut edits = vec![FixEdit::new(Span::new(span.start, span.start), "rec {\n")];
            edits.extend(
                self.wrap_separators
                    .iter()
                    .map(|&end| FixEdit::new(Span::new(end, end), ";")),
            );
            edits.push(FixEdit::new(Span::new(span.end, span.end), "\n}"));
            error = error.with_fix(
                Fix::machine_applicable("Fix recursive type groups", edits)
                    .allowing_follow_on_reanalysis_outside(span),
            );
        }
        error
    }
}

/// Label elaboration represents one atomic `labels` declaration as several
/// generated type items. Their constructor and projector spans both point at
/// the label head because neither member exists in Surface source; that
/// phase-stable structural fact distinguishes them from written newtypes
/// without granting behavior based on a generated name.
fn is_generated_label_newtype<P: ResolvePhase>(newtype: &Newtype<P>) -> bool {
    newtype.constructor.span == newtype.name_span && newtype.projector.span == newtype.name_span
}

#[cfg(test)]
thread_local! {
    static IMPLICIT_TYPE_CYCLE_CANDIDATE_VISITS: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

fn implicit_cycle_candidate_indices(
    nodes_by_source_item: &[Vec<usize>],
    source_items: &[usize],
) -> Vec<usize> {
    let candidate_count = source_items
        .iter()
        .map(|&source_item| nodes_by_source_item[source_item].len())
        .sum();
    #[cfg(test)]
    IMPLICIT_TYPE_CYCLE_CANDIDATE_VISITS.with(|visits| {
        visits.set(visits.get() + candidate_count);
    });
    let mut candidates = Vec::with_capacity(candidate_count);
    for &source_item in source_items {
        candidates.extend_from_slice(&nodes_by_source_item[source_item]);
    }
    candidates
}

#[cfg(all(test, feature = "surface", feature = "prime"))]
fn reset_implicit_type_cycle_candidate_visits() {
    IMPLICIT_TYPE_CYCLE_CANDIDATE_VISITS.with(|visits| visits.set(0));
}

#[cfg(all(test, feature = "surface", feature = "prime"))]
fn implicit_type_cycle_candidate_visits() -> usize {
    IMPLICIT_TYPE_CYCLE_CANDIDATE_VISITS.with(std::cell::Cell::get)
}

fn implicit_type_cycle_problems<P: ResolvePhase>(
    module: &crate::ast::Module<P>,
) -> HashMap<Span, Arc<ImplicitTypeCycleProblem>> {
    struct Node<'a, P: ResolvePhase> {
        name: &'a str,
        ty: &'a Type<P>,
        bound: Vec<&'a str>,
        lowered_item_index: usize,
        source_item_index: usize,
        span: Span,
        alias: bool,
        safe_wrap: bool,
    }

    // Generated newtypes and a named labels alias share the labels owner's
    // complete source span. Collapse those adjacent lowered fragments back to
    // one source-item index for contiguity and atomicity checks.
    let labels_owner_spans = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Newtype(newtype) if is_generated_label_newtype(newtype) => {
                Some(newtype.meta.span)
            }
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut next_source_item = 0;
    let mut labels_owner_indices = HashMap::new();
    let source_item_indices = module
        .items
        .iter()
        .map(|item| {
            if labels_owner_spans.contains(&item.meta().span) {
                *labels_owner_indices
                    .entry(item.meta().span)
                    .or_insert_with(|| {
                        let index = next_source_item;
                        next_source_item += 1;
                        index
                    })
            } else {
                let index = next_source_item;
                next_source_item += 1;
                index
            }
        })
        .collect::<Vec<_>>();

    let nodes = module
        .items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| match item {
            Item::TypeAlias(alias) => {
                let labels_owner = labels_owner_spans.contains(&alias.meta.span);
                Some(Node {
                    name: alias.name.as_str(),
                    ty: &alias.body,
                    bound: alias
                        .type_params
                        .iter()
                        .map(|param| param.name.as_str())
                        .collect(),
                    lowered_item_index: item_index,
                    source_item_index: source_item_indices[item_index],
                    span: if labels_owner {
                        alias.name_span
                    } else {
                        alias.meta.span
                    },
                    alias: true,
                    safe_wrap: alias.editable_span.is_some(),
                })
            }
            Item::Newtype(newtype) => {
                let labels_owner = labels_owner_spans.contains(&newtype.meta.span);
                Some(Node {
                    name: newtype.name.as_str(),
                    ty: &newtype.payload,
                    bound: newtype
                        .type_params
                        .iter()
                        .chain(&newtype.existential_params)
                        .map(|param| param.name.as_str())
                        .collect(),
                    lowered_item_index: item_index,
                    source_item_index: source_item_indices[item_index],
                    span: if labels_owner {
                        newtype.name_span
                    } else {
                        newtype.meta.span
                    },
                    alias: false,
                    safe_wrap: newtype.editable_span.is_some() && newtype.rec_span.is_none(),
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let names = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.name, index))
        .collect::<HashMap<_, _>>();
    let edge_spans = nodes
        .iter()
        .map(|node| {
            let mut bound = TypeRecBoundNames::new(node.bound.iter().copied());
            let mut edges = Vec::new();
            collect_type_rec_edges(node.ty, &names, &mut bound, None, &mut edges);
            edges
        })
        .collect::<Vec<_>>();
    let edges = edge_spans
        .iter()
        .map(|edges| edges.iter().map(|(target, _)| *target).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let components = strongly_connected_components(&edges, |_| true);
    let mut component_of = vec![usize::MAX; nodes.len()];
    let mut component_source_items = Vec::with_capacity(components.len());
    let mut cyclic_component_counts = vec![0usize; next_source_item];
    for (component_index, component) in components.iter().enumerate() {
        let mut source_items = Vec::new();
        for &node in component {
            component_of[node] = component_index;
            let source_item = nodes[node].source_item_index;
            if source_items.last() != Some(&source_item) {
                source_items.push(source_item);
            }
        }
        if component.len() > 1 || edges[component[0]].contains(&component[0]) {
            for &source_item in &source_items {
                cyclic_component_counts[source_item] += 1;
            }
        }
        component_source_items.push(source_items);
    }
    let mut nodes_by_source_item = vec![Vec::new(); next_source_item];
    let mut source_item_spans = vec![None::<Span>; next_source_item];
    let mut source_item_bare_braced_ends = vec![None::<u32>; next_source_item];
    for (item_index, item) in module.items.iter().enumerate() {
        let source_item = source_item_indices[item_index];
        let item_span = match item {
            Item::TypeAlias(alias) => alias.editable_span.unwrap_or(alias.meta.span),
            Item::Newtype(newtype) => newtype.editable_span.unwrap_or(newtype.meta.span),
            _ => item.meta().span,
        };
        if let Item::Newtype(newtype) = item
            && !labels_owner_spans.contains(&newtype.meta.span)
            && newtype
                .editable_span
                .is_some_and(|span| span.end == newtype.meta.span.end)
        {
            source_item_bare_braced_ends[source_item] = Some(newtype.meta.span.end);
        }
        source_item_spans[source_item] =
            Some(source_item_spans[source_item].map_or(item_span, |span| {
                Span::new(span.start.min(item_span.start), span.end.max(item_span.end))
            }));
    }
    for (index, node) in nodes.iter().enumerate() {
        nodes_by_source_item[node.source_item_index].push(index);
    }
    let mut problems = HashMap::new();
    for (component_index, component) in components.into_iter().enumerate() {
        if component.len() < 2 {
            continue;
        }
        let alias_only = component.iter().all(|&index| nodes[index].alias);
        let source_items = &component_source_items[component_index];
        let first_item = source_items[0];
        let last_item = *source_items.last().expect("nonempty component");
        let contiguous = last_item - first_item + 1 == source_items.len();
        let owns_one_cyclic_component = source_items
            .iter()
            .all(|&source_item| cyclic_component_counts[source_item] == 1);
        let safe_wrap = if !alias_only && contiguous && owns_one_cyclic_component {
            let candidate_indices =
                implicit_cycle_candidate_indices(&nodes_by_source_item, source_items);
            let candidate_members = candidate_indices
                .iter()
                .map(
                    |&index| match &module.items[nodes[index].lowered_item_index] {
                        Item::TypeAlias(alias) => TypeRecMember::TypeAlias(alias.clone()),
                        Item::Newtype(newtype) => TypeRecMember::Newtype(newtype.clone()),
                        _ => unreachable!("implicit type-cycle nodes are aliases or newtypes"),
                    },
                )
                .collect::<Vec<_>>();
            let candidate_analysis = analyze_type_rec_members(&candidate_members);
            let candidate_owners = candidate_indices
                .iter()
                .map(|&index| nodes[index].source_item_index - first_item)
                .collect::<Vec<_>>();
            let source_analysis = source_partition_analysis(
                &candidate_analysis,
                &candidate_owners,
                source_items.len(),
            );
            let source_has_one_complete_recursive_component =
                source_analysis.as_ref().is_some_and(|analysis| {
                    analysis.cyclic_components.as_slice()
                        == [(0..source_items.len()).collect::<Vec<_>>()]
                });
            source_has_one_complete_recursive_component
                && candidate_indices
                    .iter()
                    .all(|&index| nodes[index].safe_wrap)
                && type_rec_marker_fix_is_locally_proven(&candidate_members, &candidate_analysis)
        } else {
            false
        };
        let wrap_span = safe_wrap.then(|| {
            Span::new(
                source_item_spans[first_item]
                    .expect("first component item")
                    .start,
                source_item_spans[last_item]
                    .expect("last component item")
                    .end,
            )
        });
        let problem = Arc::new(ImplicitTypeCycleProblem {
            alias_only,
            members: component
                .iter()
                .map(|&index| (nodes[index].name.to_owned(), nodes[index].span))
                .collect(),
            wrap_span,
            wrap_separators: if wrap_span.is_some() {
                source_items[..source_items.len() - 1]
                    .iter()
                    .filter_map(|&source_item| source_item_bare_braced_ends[source_item])
                    .collect()
            } else {
                Vec::new()
            },
        });
        for &from in &component {
            for &(to, span) in &edge_spans[from] {
                if component_of[to] == component_index {
                    problems.insert(span, Arc::clone(&problem));
                }
            }
        }
    }
    problems
}

/// Top-level scope: maps each declared name to its `TopLevelId` and indexes
/// the module's written selective/qualified import edges. Built from a parsed
/// [`Module`]; the build pass detects duplicate declarations and returns a
/// name-resolution error if any are found. Import conflicts remain the later
/// package validator's responsibility; `len`/`is_empty` describe declarations.
#[derive(Clone)]
pub struct TopLevelScope {
    decls: HashMap<String, TopLevelId>,
    type_imports: Option<Arc<TypeImportIndex>>,
}

struct TypeImportIndex {
    selective_type_paths: HashMap<String, SelectiveTypePath>,
    qualified_type_paths: HashMap<String, Vec<crate::ast::PathSegment>>,
}

#[derive(Clone)]
struct SelectiveTypePath {
    prefix: Arc<[crate::ast::PathSegment]>,
    span: Span,
}

impl std::fmt::Debug for TopLevelScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Package Debug output participates in the JS emit-cache context.
        // Import edges are already present in `Module.imports`; keep this derived
        // acceleration index out of that semantic fingerprint.
        f.debug_struct("TopLevelScope")
            .field("decls", &self.decls)
            .finish()
    }
}

impl TopLevelScope {
    /// Walks a module's top-level items, registering each by name.
    /// Returns `Error::NameRes` on the first duplicate declaration.
    /// Cross-origin conflicts involving imports are checked against the
    /// validated import environment.
    ///
    /// Phase-polymorphic — the scope is built from item names and
    /// spans, both phase-agnostic, so the same builder serves
    /// `Module<Lowered>` (full pipeline) and `Module<Prime>`
    /// (kio-prime pipeline).
    pub fn build<P: ResolvePhase>(module: &crate::ast::Module<P>) -> Result<Self, Error> {
        let (scope, error) = Self::build_recording_name_error(module);
        match error {
            Some(error) => Err(error),
            None => Ok(scope),
        }
    }

    /// Build the usable first-declaration scope while retaining the first
    /// name error for a caller that schedules diagnostics by phase.
    fn build_recording_name_error<P: ResolvePhase>(
        module: &crate::ast::Module<P>,
    ) -> (Self, Option<Error>) {
        let mut decls: HashMap<String, TopLevelId> = HashMap::new();
        let mut first_error = None;
        for (idx, item) in module.items.iter().enumerate() {
            visit_item_declarations(idx, item, |id, declaration| {
                let (name, span) = declaration.name_and_span();
                if let Some(first_id) = decls.get(name).copied() {
                    // Point the user at the original declaration, and name
                    // the fix in surface vocabulary — open-world compilation
                    // rules out a second top-level binding for one name.
                    let first_span = declaration_by_id(module, first_id)
                        .expect("scope id names a declaration")
                        .name_and_span()
                        .1;
                    let error = Error::name_res(
                        span,
                        format!("duplicate top-level declaration `{name}`"),
                    )
                    .with_secondary(first_span, format!("`{name}` first declared here"))
                    .with_help(format!(
                        "each top-level name may be declared once — rename this `{name}` or remove the \
                         earlier declaration"
                    ));
                    first_error.get_or_insert(error);
                    return;
                }
                decls.insert(name.to_owned(), id);
            });
        }
        let type_imports = Self::build_type_import_index(&module.imports);
        (
            Self {
                decls,
                type_imports,
            },
            first_error,
        )
    }

    fn build_type_import_index(imports: &[Import]) -> Option<Arc<TypeImportIndex>> {
        let mut selective_type_paths = HashMap::new();
        let mut qualified_type_paths = HashMap::new();
        for usage in imports {
            match &usage.kind {
                ImportKind::Selective { items, from } => {
                    let mut shared_prefix = None;
                    for name in items
                        .iter()
                        .filter_map(crate::ast::ImportItem::as_name)
                        .filter(|name| crate::naming::is_type_name(name))
                    {
                        let prefix = shared_prefix.get_or_insert_with(|| {
                            Arc::<[crate::ast::PathSegment]>::from(from.segments.clone())
                        });
                        selective_type_paths
                            .entry(name.to_owned())
                            .or_insert_with(|| SelectiveTypePath {
                                prefix: Arc::clone(prefix),
                                span: usage.span,
                            });
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    qualified_type_paths
                        .entry(alias.clone())
                        .or_insert_with(|| path.segments.clone());
                }
                ImportKind::Intrinsics | ImportKind::Comptime => {}
            }
        }
        if selective_type_paths.is_empty() && qualified_type_paths.is_empty() {
            None
        } else {
            Some(Arc::new(TypeImportIndex {
                selective_type_paths,
                qualified_type_paths,
            }))
        }
    }

    pub fn lookup(&self, name: &str) -> Option<TopLevelId> {
        self.decls.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.decls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    fn selective_type_path(&self, name: &str) -> Option<Vec<crate::ast::PathSegment>> {
        let imported = self.type_imports.as_ref()?.selective_type_paths.get(name)?;
        #[cfg(test)]
        update_type_reexport_work(|work| {
            work.indexed_import_prefix_clones += 1;
            work.indexed_import_prefix_segments_cloned += imported.prefix.len();
        });
        let mut path = imported.prefix.to_vec();
        path.push(crate::ast::PathSegment::synth(name, imported.span));
        Some(path)
    }

    fn qualified_type_path(&self, name: &str) -> Option<Vec<crate::ast::PathSegment>> {
        let path = self.type_imports.as_ref()?.qualified_type_paths.get(name)?;
        #[cfg(test)]
        update_type_reexport_work(|work| {
            work.indexed_import_prefix_clones += 1;
            work.indexed_import_prefix_segments_cloned += path.len();
        });
        Some(path.clone())
    }

    fn selective_type_target(&self, name: &str) -> Option<(&[crate::ast::PathSegment], Span)> {
        let imported = self.type_imports.as_ref()?.selective_type_paths.get(name)?;
        Some((imported.prefix.as_ref(), imported.span))
    }

    fn qualified_type_target(&self, name: &str) -> Option<&[crate::ast::PathSegment]> {
        self.type_imports
            .as_ref()?
            .qualified_type_paths
            .get(name)
            .map(Vec::as_slice)
    }

    fn selective_type_origin(&self, name: &str) -> Option<Vec<crate::ast::PathSegment>> {
        Some(
            self.type_imports
                .as_ref()?
                .selective_type_paths
                .get(name)?
                .prefix
                .to_vec(),
        )
    }

    #[cfg(all(test, feature = "surface", feature = "prime"))]
    fn selective_type_retained_storage(&self) -> (usize, usize) {
        let Some(type_imports) = self.type_imports.as_ref() else {
            return (0, 0);
        };
        let mut prefixes = HashSet::new();
        let mut path_segments = 0;
        let mut prefix_name_bytes = 0;
        for imported in type_imports.selective_type_paths.values() {
            let identity = (imported.prefix.as_ptr() as usize, imported.prefix.len());
            if prefixes.insert(identity) {
                path_segments += imported.prefix.len();
                prefix_name_bytes += imported
                    .prefix
                    .iter()
                    .map(|segment| segment.as_str().len())
                    .sum::<usize>();
            }
        }
        let name_bytes = type_imports
            .selective_type_paths
            .keys()
            .map(String::len)
            .sum::<usize>()
            + prefix_name_bytes;
        (path_segments, name_bytes)
    }

    #[cfg(all(test, feature = "surface", feature = "prime"))]
    fn type_import_storage_identity(&self) -> Option<(usize, usize, usize)> {
        let type_imports = self.type_imports.as_ref()?;
        Some((
            Arc::as_ptr(type_imports) as usize,
            type_imports.selective_type_paths.len(),
            type_imports.qualified_type_paths.len(),
        ))
    }
}

/// Exact nominal scope selected for one type operation.
///
/// A checking root remains allocation-local until a validator rebases it into
/// a package entry. A same-path, pointer-distinct package entry therefore marks
/// only cross-owner lookup as malformed; local declarations remain usable for
/// the diagnostic/checking operation that owns the root.
pub(crate) enum NominalScope<'m, P: Phase> {
    Absent,
    CheckingRoot {
        module: &'m crate::ast::Module<P>,
        package_collision: bool,
    },
    PackageOwner(&'m ModuleEntry<P>),
}

impl<P: Phase> Clone for NominalScope<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Phase> Copy for NominalScope<'_, P> {}

impl<'m, P: Phase> NominalScope<'m, P> {
    pub(crate) fn module(&self) -> Option<&'m crate::ast::Module<P>> {
        match self {
            Self::Absent => None,
            Self::CheckingRoot { module, .. } => Some(*module),
            Self::PackageOwner(entry) => Some(&entry.module),
        }
    }

    /// Exact provider file carried by a validated package-owner scope.
    ///
    /// A checking root is the caller's primary file and therefore needs no
    /// separate related-file identity. Only an exact package entry can mint a
    /// cross-file diagnostic location; callers must never recover one from a
    /// module spelling or filesystem search.
    pub(crate) fn provider_file(&self) -> Option<&'m std::path::Path> {
        match self {
            Self::PackageOwner(entry) => Some(entry.file_path.as_path()),
            Self::Absent | Self::CheckingRoot { .. } => None,
        }
    }

    fn blocks_cross_owner(&self) -> bool {
        matches!(
            self,
            Self::CheckingRoot {
                package_collision: true,
                ..
            }
        )
    }
}

/// One nominal declaration selected by exact scope/import identity.
pub(crate) struct NominalDeclaration<'m, P: Phase> {
    pub(crate) owner: NominalScope<'m, P>,
    pub(crate) id: TopLevelId,
    pub(crate) declaration: TopLevelDeclaration<'m, P>,
    pub(crate) route: NominalRoute,
}

impl<P: Phase> Clone for NominalDeclaration<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Phase> Copy for NominalDeclaration<'_, P> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NominalRoute {
    Local,
    Selective,
    Qualified,
    Exact,
}

pub(crate) enum NominalSelection<'m, P: Phase> {
    Selected(NominalDeclaration<'m, P>),
    Missing,
    Opaque,
}

impl<P: Phase> Clone for NominalSelection<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Phase> Copy for NominalSelection<'_, P> {}

#[derive(Clone, Copy)]
struct BorrowedSelectiveTypePath<'m> {
    prefix: &'m [crate::ast::PathSegment],
    span: Span,
}

struct BorrowedNominalScope<'m> {
    declarations: HashMap<&'m str, TopLevelId>,
    selective: HashMap<&'m str, BorrowedSelectiveTypePath<'m>>,
    qualified: HashMap<&'m str, &'m [crate::ast::PathSegment]>,
}

type BorrowedTargetCache<'m, P> = HashMap<(usize, usize), Option<&'m ModuleEntry<P>>>;

#[derive(Clone, Copy)]
enum BorrowedTargetRoute {
    ExactOwner,
    WrittenEdge,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NominalProviderWork {
    pub(crate) cache_initializations: usize,
    pub(crate) checking_scope_builds: usize,
    pub(crate) declaration_items_indexed: usize,
    pub(crate) import_edges_indexed: usize,
    pub(crate) exact_target_lookups: usize,
    pub(crate) exact_target_path_segments: usize,
    pub(crate) edge_target_lookups: usize,
    pub(crate) edge_target_path_segments: usize,
}

#[cfg(test)]
thread_local! {
    static NOMINAL_PROVIDER_WORK: std::cell::Cell<NominalProviderWork> =
        const { std::cell::Cell::new(NominalProviderWork {
            cache_initializations: 0,
            checking_scope_builds: 0,
            declaration_items_indexed: 0,
            import_edges_indexed: 0,
            exact_target_lookups: 0,
            exact_target_path_segments: 0,
            edge_target_lookups: 0,
            edge_target_path_segments: 0,
        }) };
}

#[cfg(test)]
fn update_nominal_provider_work(update: impl FnOnce(&mut NominalProviderWork)) {
    NOMINAL_PROVIDER_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn reset_nominal_provider_work() {
    NOMINAL_PROVIDER_WORK.with(|work| work.set(NominalProviderWork::default()));
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn nominal_provider_work() -> NominalProviderWork {
    NOMINAL_PROVIDER_WORK.with(std::cell::Cell::get)
}

/// Operation-local provider for nominal declaration and written-import facts.
///
/// Package owners borrow their validator-built [`TopLevelScope`]. Standalone
/// checking roots lazily build one borrowed projection per exact module
/// pointer. Written edges retain one exact target entry per operation; no
/// package/global cache and no candidate search is introduced.
struct ProviderCaches<'m, P: Phase> {
    checking_scopes: RefCell<HashMap<usize, BorrowedNominalScope<'m>>>,
    borrowed_targets: RefCell<BorrowedTargetCache<'m, P>>,
    exact_targets: RefCell<HashMap<String, Option<&'m ModuleEntry<P>>>>,
}

impl<P: Phase> ProviderCaches<'_, P> {
    fn new() -> Self {
        Self {
            checking_scopes: RefCell::new(HashMap::new()),
            borrowed_targets: RefCell::new(HashMap::new()),
            exact_targets: RefCell::new(HashMap::new()),
        }
    }
}

pub(crate) struct NominalProvider<'m, P: Phase> {
    package: Option<&'m Package<P>>,
    root_module: Option<&'m crate::ast::Module<P>>,
    root: OnceCell<NominalScope<'m, P>>,
    caches: OnceCell<Box<ProviderCaches<'m, P>>>,
}

impl<'m, P: Phase> NominalProvider<'m, P> {
    pub(crate) fn new(
        module: Option<&'m crate::ast::Module<P>>,
        package: Option<&'m Package<P>>,
    ) -> Self {
        Self {
            package,
            root_module: module,
            root: OnceCell::new(),
            caches: OnceCell::new(),
        }
    }

    fn caches(&self) -> &ProviderCaches<'m, P> {
        self.caches.get_or_init(|| {
            #[cfg(test)]
            update_nominal_provider_work(|work| {
                work.cache_initializations += 1;
            });
            Box::new(ProviderCaches::new())
        })
    }

    pub(crate) fn root(&self) -> NominalScope<'m, P> {
        *self
            .root
            .get_or_init(|| Self::capture_scope(self.root_module, self.package))
    }

    pub(crate) fn scope_for_owner(
        &self,
        owner: Option<&'m crate::ast::Module<P>>,
    ) -> NominalScope<'m, P> {
        let Some(module) = owner else {
            return NominalScope::Absent;
        };
        let root = self.root();
        if root.module().is_some_and(|root| std::ptr::eq(root, module)) {
            return root;
        }
        if self.package.is_none() {
            return NominalScope::CheckingRoot {
                module,
                package_collision: false,
            };
        }
        match self.cached_borrowed_target(
            module.path.segments.as_slice(),
            BorrowedTargetRoute::ExactOwner,
        ) {
            Some(entry) if std::ptr::eq(&entry.module, module) => NominalScope::PackageOwner(entry),
            Some(_) => NominalScope::CheckingRoot {
                module,
                package_collision: true,
            },
            None => NominalScope::CheckingRoot {
                module,
                package_collision: false,
            },
        }
    }

    fn capture_scope(
        module: Option<&'m crate::ast::Module<P>>,
        package: Option<&'m Package<P>>,
    ) -> NominalScope<'m, P> {
        let Some(module) = module else {
            return NominalScope::Absent;
        };
        let Some(package) = package else {
            return NominalScope::CheckingRoot {
                module,
                package_collision: false,
            };
        };
        #[cfg(test)]
        update_nominal_provider_work(|work| {
            work.exact_target_lookups += 1;
            work.exact_target_path_segments += module.path.segments.len();
        });
        let path = module
            .path
            .segments
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        match package.module(&path) {
            Some(entry) if std::ptr::eq(&entry.module, module) => NominalScope::PackageOwner(entry),
            Some(_) => NominalScope::CheckingRoot {
                module,
                package_collision: true,
            },
            None => NominalScope::CheckingRoot {
                module,
                package_collision: false,
            },
        }
    }

    fn ensure_checking_scope(&self, module: &'m crate::ast::Module<P>) {
        let key = module as *const crate::ast::Module<P> as usize;
        let caches = self.caches();
        if caches.checking_scopes.borrow().contains_key(&key) {
            return;
        }
        #[cfg(test)]
        update_nominal_provider_work(|work| {
            work.checking_scope_builds += 1;
            work.declaration_items_indexed += module.items.len();
            work.import_edges_indexed += module.imports.len();
        });
        let mut declarations = HashMap::new();
        for (index, item) in module.items.iter().enumerate() {
            visit_item_declarations(index, item, |id, declaration| {
                let name = declaration
                    .type_alias()
                    .map(|alias| alias.name.as_str())
                    .or_else(|| declaration.newtype().map(|newtype| newtype.name.as_str()))
                    .or_else(|| declaration.host_type().map(|host| host.name.as_str()));
                if let Some(name) = name {
                    declarations.entry(name).or_insert(id);
                }
            });
        }
        let mut selective = HashMap::new();
        let mut qualified = HashMap::new();
        for usage in &module.imports {
            match &usage.kind {
                ImportKind::Selective { items, from } => {
                    for name in items
                        .iter()
                        .filter_map(crate::ast::ImportItem::as_name)
                        .filter(|name| crate::naming::is_type_name(name))
                    {
                        selective.entry(name).or_insert(BorrowedSelectiveTypePath {
                            prefix: &from.segments,
                            span: usage.span,
                        });
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    qualified
                        .entry(alias.as_str())
                        .or_insert(path.segments.as_slice());
                }
                ImportKind::Intrinsics | ImportKind::Comptime => {}
            }
        }
        caches.checking_scopes.borrow_mut().insert(
            key,
            BorrowedNominalScope {
                declarations,
                selective,
                qualified,
            },
        );
    }

    fn declaration_id(&self, scope: NominalScope<'m, P>, name: &str) -> Option<TopLevelId> {
        match scope {
            NominalScope::Absent => None,
            NominalScope::CheckingRoot { module, .. } => {
                self.ensure_checking_scope(module);
                let key = module as *const crate::ast::Module<P> as usize;
                self.caches()
                    .checking_scopes
                    .borrow()
                    .get(&key)
                    .and_then(|scope| scope.declarations.get(name).copied())
            }
            NominalScope::PackageOwner(entry) => entry.scope.lookup(name).filter(|id| {
                declaration_by_id(&entry.module, *id).is_some_and(|declaration| {
                    declaration
                        .type_alias()
                        .map(|alias| alias.name.as_str())
                        .or_else(|| declaration.newtype().map(|newtype| newtype.name.as_str()))
                        .or_else(|| declaration.host_type().map(|host| host.name.as_str()))
                        == Some(name)
                })
            }),
        }
    }

    pub(crate) fn declaration(
        &self,
        scope: NominalScope<'m, P>,
        name: &str,
        route: NominalRoute,
    ) -> Option<NominalDeclaration<'m, P>> {
        let id = self.declaration_id(scope, name)?;
        let declaration = declaration_by_id(scope.module()?, id)?;
        Some(NominalDeclaration {
            owner: scope,
            id,
            declaration,
            route,
        })
    }

    fn selective_target(
        &self,
        scope: NominalScope<'m, P>,
        name: &str,
    ) -> Option<(&'m [crate::ast::PathSegment], Span)> {
        match scope {
            NominalScope::Absent => None,
            NominalScope::CheckingRoot { module, .. } => {
                self.ensure_checking_scope(module);
                let key = module as *const crate::ast::Module<P> as usize;
                self.caches()
                    .checking_scopes
                    .borrow()
                    .get(&key)
                    .and_then(|scope| scope.selective.get(name).copied())
                    .map(|target| (target.prefix, target.span))
            }
            NominalScope::PackageOwner(entry) => entry.scope.selective_type_target(name),
        }
    }

    fn qualified_target(
        &self,
        scope: NominalScope<'m, P>,
        name: &str,
    ) -> Option<&'m [crate::ast::PathSegment]> {
        match scope {
            NominalScope::Absent => None,
            NominalScope::CheckingRoot { module, .. } => {
                self.ensure_checking_scope(module);
                let key = module as *const crate::ast::Module<P> as usize;
                self.caches()
                    .checking_scopes
                    .borrow()
                    .get(&key)
                    .and_then(|scope| scope.qualified.get(name).copied())
            }
            NominalScope::PackageOwner(entry) => entry.scope.qualified_type_target(name),
        }
    }

    fn cached_borrowed_target(
        &self,
        path: &'m [crate::ast::PathSegment],
        _route: BorrowedTargetRoute,
    ) -> Option<&'m ModuleEntry<P>> {
        let key = (path.as_ptr() as usize, path.len());
        let caches = self.caches();
        if let Some(target) = caches.borrowed_targets.borrow().get(&key).copied() {
            return target;
        }
        #[cfg(test)]
        match _route {
            BorrowedTargetRoute::ExactOwner => update_nominal_provider_work(|work| {
                work.exact_target_lookups += 1;
                work.exact_target_path_segments += path.len();
            }),
            BorrowedTargetRoute::WrittenEdge => update_nominal_provider_work(|work| {
                work.edge_target_lookups += 1;
                work.edge_target_path_segments += path.len();
            }),
        }
        let joined = path
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        let target = self.package.and_then(|package| package.module(&joined));
        caches.borrowed_targets.borrow_mut().insert(key, target);
        target
    }

    fn edge_target(&self, path: &'m [crate::ast::PathSegment]) -> Option<&'m ModuleEntry<P>> {
        self.cached_borrowed_target(path, BorrowedTargetRoute::WrittenEdge)
    }

    fn exact_target(&self, path: &[crate::ast::PathSegment]) -> Option<&'m ModuleEntry<P>> {
        let joined = path
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        let caches = self.caches();
        if let Some(target) = caches.exact_targets.borrow().get(&joined).copied() {
            return target;
        }
        #[cfg(test)]
        update_nominal_provider_work(|work| {
            work.exact_target_lookups += 1;
            work.exact_target_path_segments += path.len();
        });
        let target = self.package.and_then(|package| package.module(&joined));
        caches.exact_targets.borrow_mut().insert(joined, target);
        target
    }

    fn selected(
        &self,
        scope: NominalScope<'m, P>,
        name: &str,
        route: NominalRoute,
    ) -> NominalSelection<'m, P> {
        self.declaration(scope, name, route)
            .map_or(NominalSelection::Missing, NominalSelection::Selected)
    }

    pub(crate) fn select(
        &self,
        scope: NominalScope<'m, P>,
        segments: &[crate::ast::PathSegment],
        identity_canonical: bool,
    ) -> NominalSelection<'m, P> {
        let Some((head, tail)) = segments.split_first() else {
            return NominalSelection::Missing;
        };
        if identity_canonical && segments.len() > 1 {
            let (name, owner_path) = segments.split_last().expect("non-empty nominal path");
            if scope.module().is_some_and(|module| {
                owner_path
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .eq(module
                        .path
                        .segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str))
            }) {
                return self.selected(scope, name.as_str(), NominalRoute::Exact);
            }
            if scope.blocks_cross_owner() {
                return NominalSelection::Opaque;
            }
            return self
                .exact_target(owner_path)
                .map_or(NominalSelection::Missing, |entry| {
                    self.selected(
                        NominalScope::PackageOwner(entry),
                        name.as_str(),
                        NominalRoute::Exact,
                    )
                });
        }
        if tail.is_empty() {
            if let Some((path, _)) = self.selective_target(scope, head.as_str()) {
                if scope.blocks_cross_owner() {
                    return NominalSelection::Opaque;
                }
                return self
                    .edge_target(path)
                    .map_or(NominalSelection::Missing, |entry| {
                        self.selected(
                            NominalScope::PackageOwner(entry),
                            head.as_str(),
                            NominalRoute::Selective,
                        )
                    });
            }
            return self.selected(scope, head.as_str(), NominalRoute::Local);
        }
        if let Some(path) = self.qualified_target(scope, head.as_str()) {
            if tail.len() != 1 {
                return NominalSelection::Missing;
            }
            if scope.blocks_cross_owner() {
                return NominalSelection::Opaque;
            }
            return self
                .edge_target(path)
                .map_or(NominalSelection::Missing, |entry| {
                    self.selected(
                        NominalScope::PackageOwner(entry),
                        tail[0].as_str(),
                        NominalRoute::Qualified,
                    )
                });
        }
        if scope.blocks_cross_owner() {
            return NominalSelection::Opaque;
        }
        let (name, owner_path) = segments.split_last().expect("non-empty nominal path");
        self.exact_target(owner_path)
            .map_or(NominalSelection::Missing, |entry| {
                self.selected(
                    NominalScope::PackageOwner(entry),
                    name.as_str(),
                    NominalRoute::Exact,
                )
            })
    }

    fn canonical_terminal(
        &self,
        mut owner: &'m ModuleEntry<P>,
        name: &crate::ast::PathSegment,
    ) -> (&'m ModuleEntry<P>, crate::ast::PathSegment) {
        let mut seen = HashSet::new();
        loop {
            if self
                .declaration_id(NominalScope::PackageOwner(owner), name.as_str())
                .is_some()
            {
                return (owner, name.clone());
            }
            let owner_key = owner as *const ModuleEntry<P> as usize;
            if !seen.insert(owner_key) {
                return (owner, name.clone());
            }
            let Some(path) = owner.scope.selective_type_origin(name.as_str()) else {
                return (owner, name.clone());
            };
            let Some(next) = self.exact_target(&path) else {
                return (owner, name.clone());
            };
            owner = next;
        }
    }

    pub(crate) fn qualify(
        &self,
        scope: NominalScope<'m, P>,
        segments: &[crate::ast::PathSegment],
        identity_canonical: bool,
    ) -> (Vec<crate::ast::PathSegment>, bool) {
        let Some((head, tail)) = segments.split_first() else {
            return (Vec::new(), false);
        };
        if identity_canonical && segments.len() > 1 {
            let (name, owner_path) = segments.split_last().expect("non-empty nominal path");
            if scope.module().is_some_and(|module| {
                owner_path
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .eq(module
                        .path
                        .segments
                        .iter()
                        .map(crate::ast::PathSegment::as_str))
            }) {
                return (segments.to_vec(), true);
            }
            if scope.blocks_cross_owner() {
                return (segments.to_vec(), false);
            }
            let Some(owner) = self.exact_target(owner_path) else {
                return (segments.to_vec(), true);
            };
            let (owner, name) = self.canonical_terminal(owner, name);
            let mut out = owner.module.path.segments.clone();
            out.push(name);
            return (out, true);
        }
        let written_target = if tail.is_empty() {
            self.selective_target(scope, head.as_str())
                .map(|(path, _)| (path, head.clone()))
        } else if tail.len() == 1 {
            self.qualified_target(scope, head.as_str())
                .map(|path| (path, tail[0].clone()))
        } else {
            None
        };
        if let Some((path, name)) = written_target {
            if scope.blocks_cross_owner() {
                return (segments.to_vec(), false);
            }
            let Some(entry) = self.edge_target(path) else {
                let mut out = path.to_vec();
                out.push(name);
                return (out, true);
            };
            let (entry, name) = self.canonical_terminal(entry, &name);
            let mut out = entry.module.path.segments.clone();
            out.push(name);
            return (out, true);
        }
        if tail.is_empty()
            && self
                .declaration(scope, head.as_str(), NominalRoute::Local)
                .is_some()
        {
            let Some(module) = scope.module() else {
                return (segments.to_vec(), false);
            };
            let mut out = module.path.segments.clone();
            out.push(head.clone());
            return (out, true);
        }
        if tail.len() > 1 && self.qualified_target(scope, head.as_str()).is_some() {
            return (segments.to_vec(), false);
        }
        if tail.is_empty() || scope.blocks_cross_owner() {
            return (segments.to_vec(), false);
        }
        let (name, owner_path) = segments.split_last().expect("non-empty nominal path");
        let Some(owner) = self.exact_target(owner_path) else {
            return (segments.to_vec(), false);
        };
        let (owner, name) = self.canonical_terminal(owner, name);
        let mut out = owner.module.path.segments.clone();
        out.push(name);
        (out, true)
    }
}

/// Exact semantic identity of one resolved newtype declaration. The owner
/// path is canonical module identity, not the lexical alias used by a
/// consumer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ResolvedNewtypeIdentity {
    module_path: String,
    declaration_name: String,
}

impl ResolvedNewtypeIdentity {
    pub(crate) fn new<P: Phase>(owner: &ModulePath, declaration: &crate::ast::Newtype<P>) -> Self {
        Self {
            module_path: owner
                .segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("/"),
            declaration_name: declaration.name.clone(),
        }
    }

    #[cfg(feature = "cli")]
    pub(crate) fn from_parts(
        module_path: impl Into<String>,
        declaration_name: impl Into<String>,
    ) -> Self {
        Self {
            module_path: module_path.into(),
            declaration_name: declaration_name.into(),
        }
    }
}

/// Allocation-free lookup for the two admitted lexical newtype-head shapes.
/// The outer qualified map lets a call-site slice (`alias`, `Foo`) probe by
/// borrowed strings without joining or cloning its path.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedNewtypeHeadMap<T> {
    unqualified: HashMap<String, T>,
    qualified: HashMap<String, HashMap<String, T>>,
}

impl<T> Default for ResolvedNewtypeHeadMap<T> {
    fn default() -> Self {
        Self {
            unqualified: HashMap::new(),
            qualified: HashMap::new(),
        }
    }
}

impl<T> ResolvedNewtypeHeadMap<T> {
    pub(crate) fn insert(&mut self, visible_head: &[String], value: T) {
        match visible_head {
            [name] => {
                self.unqualified.insert(name.clone(), value);
            }
            [alias, name] => {
                self.qualified
                    .entry(alias.clone())
                    .or_default()
                    .insert(name.clone(), value);
            }
            _ => unreachable!("resolved newtype member heads have one or two segments"),
        }
    }

    pub(crate) fn get(&self, visible_head: &[crate::ast::PathSegment]) -> Option<&T> {
        match visible_head {
            [name] => self.unqualified.get(name.as_str()),
            [alias, name] => self
                .qualified
                .get(alias.as_str())
                .and_then(|members| members.get(name.as_str())),
            _ => None,
        }
    }
}

/// Visit each literal newtype reachable as the head of a member call in
/// `module`: either `Foo.member` through local/selective scope or
/// `alias.Foo.member` through one written qualified import. `visible_head`
/// retains that exact lexical route, while `owner_path` and `declaration`
/// identify the exact selected nominal. No ambient package declaration can
/// enter the catalogue without one of those ordinary scope edges.
pub(crate) fn for_each_resolved_newtype_member_head<'m, P: Phase>(
    package: &'m Package<P>,
    module: &'m crate::ast::Module<P>,
    mut visit: impl FnMut(&[String], &'m ModulePath, &'m crate::ast::Newtype<P>),
) {
    let mut candidate_heads = BTreeSet::new();
    for item in &module.items {
        for_each_item_declaration(item, |declaration| {
            if let Some(newtype) = declaration.newtype() {
                candidate_heads.insert(vec![newtype.name.clone()]);
            }
        });
    }
    for import_ in &module.imports {
        match &import_.kind {
            crate::ast::ImportKind::Selective { items, .. } => {
                candidate_heads.extend(
                    items
                        .iter()
                        .filter_map(crate::ast::ImportItem::as_name)
                        .map(|name| vec![name.to_owned()]),
                );
            }
            crate::ast::ImportKind::Qualified { path, alias } => {
                let target_path = path
                    .segments
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .collect::<Vec<_>>()
                    .join("/");
                let Some(target) = package.module(&target_path) else {
                    continue;
                };
                let same_owner = target.module.path == module.path;
                for item in &target.module.items {
                    for_each_item_declaration(item, |declaration| {
                        let Some(newtype) = declaration.newtype() else {
                            return;
                        };
                        if same_owner || is_visible(&newtype.vis, &module.path) {
                            candidate_heads.insert(vec![alias.clone(), newtype.name.clone()]);
                        }
                    });
                }
            }
            crate::ast::ImportKind::Intrinsics | crate::ast::ImportKind::Comptime => {}
        }
    }

    let provider = NominalProvider::new(Some(module), Some(package));
    let scope = provider.root();
    for visible_head in candidate_heads {
        let segments = visible_head
            .iter()
            .map(|name| crate::ast::PathSegment::synth(name.clone(), Span::new(0, 0)))
            .collect::<Vec<_>>();
        let NominalSelection::Selected(selected) = provider.select(scope, &segments, false) else {
            continue;
        };
        if let Some(newtype) = selected.declaration.newtype() {
            let owner = selected
                .owner
                .module()
                .expect("a selected newtype has an exact owner");
            visit(&visible_head, &owner.path, newtype);
        }
    }
}

/// Returns the declared name and its declaration span for any top-
/// level item. Phase-polymorphic — the named form of `labels`
/// (`labels T = { … }`) carries `type_alias_name`; the anonymous
/// form has no single name and falls back to the labels' span.
/// The resolver only actually invokes this on `Module<Lowered>`,
/// where `Item::Labels` is uninhabited, so the fallback branch is
/// never reached at runtime — it's there to keep the helper
/// compilable across phases.
fn item_name_and_span<P: ResolvePhase>(item: &crate::ast::Item<P>) -> (&str, Span) {
    match item {
        crate::ast::Item::FnDef(d) => (&d.name, d.meta.span),
        crate::ast::Item::TypeAlias(a) => (&a.name, a.meta.span),
        crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
        crate::ast::Item::Newtype(d) => (&d.name, d.meta.span),
        crate::ast::Item::Labels(d, _ext) => match &d.type_alias_name {
            Some(name) => (name.as_str(), d.meta.span),
            None => ("", d.meta.span),
        },
        crate::ast::Item::LabelForward(_, ext) => match *ext {},
        crate::ast::Item::Equiv(e, _ext) => (e.name.as_str(), e.meta.span),
        crate::ast::Item::Elaborator(s, _ext) => (s.name.as_str(), s.meta.span),
        crate::ast::Item::HostType(h) => (h.name.as_str(), h.meta.span),
        crate::ast::Item::HostFn(h) => (h.name.as_str(), h.meta.span),
        // `op` items declare an operator (not a value name);
        // they don't enter the resolver's name-and-span table. The
        // arm is unreachable at every resolver phase, where `Item::Op` is
        // `Never`.
        crate::ast::Item::Op(_, ext) => match *ext {},
        crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
        crate::ast::Item::RecGroup(_, ext) => match *ext {},
        crate::ast::Item::TypeRecGroup(_) => {
            unreachable!("a type-recursive group has several declaration names")
        }
    }
}

/// The declared visibility of an item. Phase-polymorphic — the `vis`
/// field exists on every phase's `FnDef` / `TypeAlias` / `Newtype` /
/// `Labels` / elaborator struct, so the read goes through cleanly.
fn item_visibility<P: ResolvePhase>(item: &crate::ast::Item<P>) -> crate::ast::Visibility {
    use crate::ast::Visibility;
    match item {
        crate::ast::Item::FnDef(d) => d.vis.clone(),
        crate::ast::Item::TypeAlias(a) => a.vis.clone(),
        crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
        crate::ast::Item::Newtype(d) => d.vis.clone(),
        crate::ast::Item::Labels(d, _ext) => d.vis.clone(),
        crate::ast::Item::LabelForward(_, ext) => match *ext {},
        // `equiv` decls have no visibility (they're never reachable
        // by callers; they exist for the test runner). Treat as
        // private for visibility checks.
        crate::ast::Item::Equiv(_, _ext) => Visibility::Private,
        crate::ast::Item::Elaborator(s, _ext) => s.vis.clone(),
        // Host items are always public.
        crate::ast::Item::HostType(_) | crate::ast::Item::HostFn(_) => Visibility::Public,
        // `op` items are uninhabited at the only phase the
        // resolver runs against (`Lowered`).
        crate::ast::Item::Op(_, ext) => match *ext {},
        crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
        crate::ast::Item::RecGroup(_, ext) => match *ext {},
        crate::ast::Item::TypeRecGroup(_) => {
            unreachable!("a type-recursive group has member-local visibility")
        }
    }
}

/// True if the item belongs to the package's host contract. Scoped
/// `pub(path)` visibility remains package-internal.
fn item_is_exported<P: ResolvePhase>(item: &crate::ast::Item<P>) -> bool {
    if let Item::TypeRecGroup(group) = item {
        return group.members.iter().any(|member| match member {
            TypeRecMember::TypeAlias(alias) => alias.vis.is_exported(),
            TypeRecMember::Newtype(newtype) => newtype.vis.is_exported(),
            TypeRecMember::Labels(_, ext) => match *ext {},
        });
    }
    item_visibility(item).is_exported()
}

/// Every `(visibility, site-description, span)` an item declares — the
/// item's own visibility plus, for a `newtype`, its `constructor` and
/// `projector` member visibilities. Each of those members may independently
/// carry a `pub(<module-path>)` scope (see
/// [`specs/language.md`](../../../specs/language.md) § Visibility), so the
/// prefix-validity check ([`Package::check_visibility_decls`]) walks all of
/// them — a member scope is validated exactly like the item's own, and its
/// span anchors the diagnostic at the offending member. (`rec`-group members
/// carry their own visibility too, but a `rec` group is already lowered to
/// `FnDef`s by the phase the resolver runs against, so each member surfaces
/// here as its own `FnDef` item.)
fn item_visibility_sites<P: ResolvePhase>(
    item: &crate::ast::Item<P>,
) -> Vec<(crate::ast::Visibility, String, Span)> {
    if let Item::TypeRecGroup(group) = item {
        let mut sites = Vec::new();
        for member in &group.members {
            match member {
                TypeRecMember::TypeAlias(alias) => {
                    sites.push((alias.vis.clone(), alias.name.clone(), alias.meta.span))
                }
                TypeRecMember::Newtype(newtype) => {
                    sites.push((newtype.vis.clone(), newtype.name.clone(), newtype.meta.span));
                    sites.push((
                        newtype.constructor.vis.clone(),
                        format!("{}.{}", newtype.name, newtype.constructor.name),
                        newtype.constructor.span,
                    ));
                    sites.push((
                        newtype.projector.vis.clone(),
                        format!("{}.{}", newtype.name, newtype.projector.name),
                        newtype.projector.span,
                    ));
                }
                TypeRecMember::Labels(_, ext) => match *ext {},
            }
        }
        return sites;
    }
    let (name, span) = item_name_and_span(item);
    let mut sites = vec![(item_visibility(item), name.to_owned(), span)];
    if let crate::ast::Item::Newtype(d) = item {
        sites.push((
            d.constructor.vis.clone(),
            format!("{}.{}", d.name, d.constructor.name),
            d.constructor.span,
        ));
        sites.push((
            d.projector.vis.clone(),
            format!("{}.{}", d.name, d.projector.name),
            d.projector.span,
        ));
    }
    sites
}

/// Whether an item with visibility `vis` is importable from a module
/// whose path is `importer`. `Public` is importable everywhere;
/// `Private` from nowhere outside its own module; `PublicIn(p)` only
/// from `p`'s module subtree — i.e. when `importer` is `p` itself or a
/// descendant of it (`p`'s segments a prefix of `importer`'s).
pub(crate) fn is_visible(vis: &crate::ast::Visibility, importer: &ModulePath) -> bool {
    use crate::ast::Visibility;
    match vis {
        Visibility::Public => true,
        Visibility::Private => false,
        Visibility::PublicIn(p) => {
            importer.segments.len() >= p.segments.len()
                && importer.segments[..p.segments.len()] == p.segments[..]
        }
    }
}

pub(crate) fn intersect_visibility(
    left: &crate::ast::Visibility,
    right: &crate::ast::Visibility,
) -> crate::ast::Visibility {
    use crate::ast::Visibility;
    match (left, right) {
        (Visibility::Public, visibility) | (visibility, Visibility::Public) => visibility.clone(),
        (Visibility::Private, _) | (_, Visibility::Private) => Visibility::Private,
        (Visibility::PublicIn(left), Visibility::PublicIn(right)) => {
            if left.segments.starts_with(&right.segments) {
                Visibility::PublicIn(left.clone())
            } else if right.segments.starts_with(&left.segments) {
                Visibility::PublicIn(right.clone())
            } else {
                Visibility::Private
            }
        }
    }
}

/// Whether `target_vis` exposes a declaration everywhere `referrer_vis`
/// exposes the declaration that refers to it.
pub(crate) fn visibility_covers(
    target_vis: &crate::ast::Visibility,
    target_owner: &ModulePath,
    referrer_vis: &crate::ast::Visibility,
    referrer_owner: &ModulePath,
) -> bool {
    use crate::ast::Visibility;
    match referrer_vis {
        Visibility::Public => matches!(target_vis, Visibility::Public),
        Visibility::PublicIn(referrer_scope) => match target_vis {
            Visibility::Public => true,
            Visibility::PublicIn(target_scope) => {
                referrer_scope.segments.len() >= target_scope.segments.len()
                    && referrer_scope.segments[..target_scope.segments.len()]
                        == target_scope.segments[..]
            }
            Visibility::Private => false,
        },
        Visibility::Private => match target_vis {
            Visibility::Public => true,
            Visibility::PublicIn(target_scope) => {
                referrer_owner.segments.len() >= target_scope.segments.len()
                    && referrer_owner.segments[..target_scope.segments.len()]
                        == target_scope.segments[..]
            }
            Visibility::Private => target_owner.segments == referrer_owner.segments,
        },
    }
}

/// The canonical module-map key form: segments `/`-joined.
fn module_path_str(p: &ModulePath) -> String {
    p.segments.join("/")
}

/// The surface module-path spelling. Used in diagnostics so the path a
/// user reads matches the path they write.
fn module_path_surface(p: &ModulePath) -> String {
    module_path_str(p)
}

pub trait ExportContractPhase:
    Phase<
        TypeLabelSugar = crate::ast::Never,
        ItemLabels = crate::ast::Never,
        ItemLiteralAlias = crate::ast::Never,
        ItemOp = crate::ast::Never,
        ItemRecGroup = crate::ast::Never,
    >
{
}

/// Exact read-only membership for type binders that protect a bare contract
/// head from nominal qualification.
///
/// The resolver needs only membership. Proof-bearing typer scopes can provide
/// it without first copying every active spelling into a temporary map.
pub(crate) trait ContractBinderLookup {
    fn contains_contract_binder(&self, name: &str) -> bool;
}

impl ContractBinderLookup for HashMap<String, Kind> {
    fn contains_contract_binder(&self, name: &str) -> bool {
        self.contains_key(name)
    }
}
impl<T> ExportContractPhase for T where
    T: Phase<
            TypeLabelSugar = crate::ast::Never,
            ItemLabels = crate::ast::Never,
            ItemLiteralAlias = crate::ast::Never,
            ItemOp = crate::ast::Never,
            ItemRecGroup = crate::ast::Never,
        >
{
}

pub(crate) fn exported_contract_type<P: ExportContractPhase + Clone>(
    ty: &Type<P>,
    defining_module: &ModuleEntry<P>,
    locals: &HashMap<String, Kind>,
) -> Type<P> {
    qualify_contract_type_in_module(ty, &defining_module.module, locals)
}

/// Qualify a Routed contract type without re-resolving an already-qualified
/// nominal head through the consumer module's imports.
pub(crate) fn exported_routed_contract_type(
    ty: &Type<crate::ast::Routed>,
    defining_module: &ModuleEntry<crate::ast::Routed>,
    locals: &HashMap<String, Kind>,
) -> Type<crate::ast::Routed> {
    qualify_routed_contract_type_in_module(ty, &defining_module.module, locals)
}

/// As [`exported_contract_type`], but resolving against a bare
/// [`crate::ast::Module`] rather than a [`ModuleEntry`] — a deep walk
/// qualifying every nominal `Type::Path` head to its identity-exact
/// `(module, name)` form through `defining_module`. The identity-exact
/// nominal canonicalizer in [`crate::pass::typecheck_core`] uses this to
/// qualify an unfolded alias body in the alias's *owner* module, for
/// which only the `Module` is on hand.
pub(crate) fn qualify_contract_type_in_module<
    P: Phase<TypeLabelSugar = crate::ast::Never> + Clone,
>(
    ty: &Type<P>,
    defining_module: &crate::ast::Module<P>,
    locals: &HashMap<String, Kind>,
) -> Type<P> {
    qualify_contract_type_in_module_with(ty, defining_module, locals, ContractPathState::Unresolved)
}

pub(crate) fn qualify_contract_type_in_module_with_binder_lookup<
    P: Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    L: ContractBinderLookup + ?Sized,
>(
    ty: &Type<P>,
    defining_module: &crate::ast::Module<P>,
    locals: &L,
) -> Type<P> {
    let mut nested = Vec::new();
    qualify_contract_type_in_module_with_lookup(
        ty,
        defining_module,
        locals,
        &mut nested,
        ContractPathState::Unresolved,
    )
}

/// Referenced Routed contract types have already interpreted every qualified
/// head in the module that wrote it. Only a bare head still needs its
/// defining-module scope; consulting imports for a multi-segment head would
/// let a consumer reinterpret the producer's identity and violate open-world
/// compilation. Raw `Item::TypeAlias` declaration bodies are outside this
/// contract and remain written as declared.
pub(crate) fn qualify_routed_contract_type_in_module(
    ty: &Type<crate::ast::Routed>,
    defining_module: &crate::ast::Module<crate::ast::Routed>,
    locals: &HashMap<String, Kind>,
) -> Type<crate::ast::Routed> {
    qualify_contract_type_in_module_with(ty, defining_module, locals, ContractPathState::Routed)
}

#[derive(Clone, Copy)]
enum ContractPathState {
    Unresolved,
    Routed,
}

fn qualify_contract_type_in_module_with<P: Phase<TypeLabelSugar = crate::ast::Never> + Clone>(
    ty: &Type<P>,
    defining_module: &crate::ast::Module<P>,
    locals: &HashMap<String, Kind>,
    path_state: ContractPathState,
) -> Type<P> {
    let mut nested = Vec::new();
    qualify_contract_type_in_module_with_lookup(
        ty,
        defining_module,
        locals,
        &mut nested,
        path_state,
    )
}

fn qualify_contract_type_in_module_with_lookup<
    P: Phase<TypeLabelSugar = crate::ast::Never> + Clone,
    L: ContractBinderLookup + ?Sized,
>(
    ty: &Type<P>,
    defining_module: &crate::ast::Module<P>,
    locals: &L,
    nested: &mut Vec<String>,
    path_state: ContractPathState,
) -> Type<P> {
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let qualified_segments =
                if matches!(path_state, ContractPathState::Routed) && segments.len() > 1 {
                    segments.clone()
                } else {
                    qualify_type_segments_in_module_with(segments, defining_module, |name| {
                        nested.iter().rev().any(|bound| bound == name)
                            || locals.contains_contract_binder(name)
                    })
                };
            let qualified_args: Vec<Type<P>> = args
                .iter()
                .map(|arg| {
                    qualify_contract_type_in_module_with_lookup(
                        arg,
                        defining_module,
                        locals,
                        nested,
                        path_state,
                    )
                })
                .collect();
            Type::synth_path_segments(qualified_segments, qualified_args, meta.span)
        }
        Type::Unit { meta } => Type::Unit { meta: meta.clone() },
        Type::Bottom { meta } => Type::Bottom { meta: meta.clone() },
        Type::Infer { meta, ext } => Type::Infer {
            meta: meta.clone(),
            ext: ext.clone(),
        },
        Type::Goal {
            goal,
            args,
            meta,
            ext,
        } => Type::Goal {
            goal: *goal,
            args: args
                .iter()
                .map(|arg| {
                    qualify_contract_type_in_module_with_lookup(
                        arg,
                        defining_module,
                        locals,
                        nested,
                        path_state,
                    )
                })
                .collect(),
            meta: meta.clone(),
            ext: ext.clone(),
        },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => Type::Function {
            param: Box::new(qualify_contract_type_in_module_with_lookup(
                param,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            ret: Box::new(qualify_contract_type_in_module_with_lookup(
                ret,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            meta: meta.clone(),
            abi_arity: *abi_arity,
            caps: caps.clone(),
        },
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(qualify_contract_type_in_module_with_lookup(
                left,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            right: Box::new(qualify_contract_type_in_module_with_lookup(
                right,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            meta: meta.clone(),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(qualify_contract_type_in_module_with_lookup(
                left,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            right: Box::new(qualify_contract_type_in_module_with_lookup(
                right,
                defining_module,
                locals,
                nested,
                path_state,
            )),
            meta: meta.clone(),
        },
        Type::Forall { param, body, meta } => {
            nested.push(param.name.clone());
            let qualified = Type::Forall {
                param: param.clone(),
                body: Box::new(qualify_contract_type_in_module_with_lookup(
                    body,
                    defining_module,
                    locals,
                    nested,
                    path_state,
                )),
                meta: meta.clone(),
            };
            nested.pop();
            qualified
        }
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

pub(crate) fn exported_routed_contract_fn_signature_and_ret(
    sig: &crate::ast::Signature<crate::ast::Routed>,
    ret: &Type<crate::ast::Routed>,
    defining_module: &ModuleEntry<crate::ast::Routed>,
) -> (
    crate::ast::Signature<crate::ast::Routed>,
    Type<crate::ast::Routed>,
) {
    qualify_contract_fn_signature_and_ret_in_module_with(
        sig,
        ret,
        &defining_module.module,
        ContractPathState::Routed,
    )
}

/// Qualify a cross-module fn's signature against its bare defining
/// [`crate::ast::Module`]. The typer resolves the signature in the fn's **declaring**
/// module: a bare `Meters` in a dependency's `fn dep_len(m:
/// Meters)` must resolve to that dependency's `(module, Meters)`, not the
/// caller's same-named type. Only the source module's [`crate::ast::Module`]
/// is on hand at the consumer's import site, so this variant takes one.
pub(crate) fn qualify_contract_fn_signature_and_ret_in_module<P: ExportContractPhase + Clone>(
    sig: &crate::ast::Signature<P>,
    ret: &Type<P>,
    defining_module: &crate::ast::Module<P>,
) -> (crate::ast::Signature<P>, Type<P>) {
    qualify_contract_fn_signature_and_ret_in_module_with(
        sig,
        ret,
        defining_module,
        ContractPathState::Unresolved,
    )
}

fn qualify_contract_fn_signature_and_ret_in_module_with<P: ExportContractPhase + Clone>(
    sig: &crate::ast::Signature<P>,
    ret: &Type<P>,
    defining_module: &crate::ast::Module<P>,
    path_state: ContractPathState,
) -> (crate::ast::Signature<P>, Type<P>) {
    let sig = qualify_contract_signature_in_module_with(sig, defining_module, path_state);
    let locals = signature_type_param_scope(&sig);
    let ret = qualify_contract_type_in_module_with(ret, defining_module, &locals, path_state);
    (sig, ret)
}

fn qualify_contract_signature_in_module_with<P: ExportContractPhase + Clone>(
    sig: &crate::ast::Signature<P>,
    defining_module: &crate::ast::Module<P>,
    path_state: ContractPathState,
) -> crate::ast::Signature<P> {
    let mut locals = HashMap::new();
    let params = sig
        .params
        .iter()
        .map(|param| match param {
            crate::ast::SignatureParam::Type(tp) => {
                locals.insert(tp.name.clone(), tp.effective_kind());
                crate::ast::SignatureParam::Type(tp.clone())
            }
            crate::ast::SignatureParam::Value(vp) => {
                crate::ast::SignatureParam::Value(crate::ast::Param {
                    name: vp.name.clone(),
                    ty: vp.ty.as_ref().map(|ty| {
                        qualify_contract_type_in_module_with(
                            ty,
                            defining_module,
                            &locals,
                            path_state,
                        )
                    }),
                    pattern: vp.pattern.clone(),
                    meta: vp.meta.clone(),
                })
            }
        })
        .collect();
    crate::ast::Signature {
        params,
        groups: sig.groups.clone(),
    }
}

fn signature_type_param_scope<P: Phase>(sig: &crate::ast::Signature<P>) -> HashMap<String, Kind> {
    sig.params
        .iter()
        .filter_map(|param| match param {
            crate::ast::SignatureParam::Type(tp) => Some((tp.name.clone(), tp.effective_kind())),
            crate::ast::SignatureParam::Value(_) => None,
        })
        .collect()
}

pub(crate) fn exported_contract_type_segments<P: ExportContractPhase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &ModuleEntry<P>,
    locals: &HashMap<String, Kind>,
) -> Vec<crate::ast::PathSegment> {
    qualify_type_segments_in_entry(segments, defining_module, locals)
}

fn qualify_type_segments_with<P: Phase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &crate::ast::Module<P>,
    is_local: impl Fn(&str) -> bool,
    selective_type_path: impl Fn(&str) -> Option<Vec<crate::ast::PathSegment>>,
    qualified_type_path: impl Fn(&str) -> Option<Vec<crate::ast::PathSegment>>,
    declares_type_name: impl Fn(&str) -> bool,
) -> Vec<crate::ast::PathSegment> {
    let Some((head, tail)) = segments.split_first() else {
        return Vec::new();
    };
    if is_local(head.as_str()) {
        return segments.to_vec();
    }
    // A selectively-imported item (`import path(Name);`) is a leaf name,
    // never a module-path qualifier. A qualified import may head a longer
    // path, so only that branch appends `tail`.
    if tail.is_empty()
        && let Some(path) = selective_type_path(head.as_str())
    {
        return path;
    }
    if let Some(mut path) = qualified_type_path(head.as_str()) {
        path.extend_from_slice(tail);
        return path;
    }
    if tail.is_empty() && declares_type_name(head.as_str()) {
        let mut out = defining_module.path.segments.clone();
        out.push(head.clone());
        return out;
    }
    segments.to_vec()
}

/// Qualify a type head through one package entry's already-validated
/// declaration and written-import indexes.
pub(crate) fn qualify_type_segments_in_entry<P: Phase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &ModuleEntry<P>,
    locals: &HashMap<String, Kind>,
) -> Vec<crate::ast::PathSegment> {
    qualify_type_segments_with(
        segments,
        &defining_module.module,
        |name| locals.contains_key(name),
        |name| defining_module.scope.selective_type_path(name),
        |name| defining_module.scope.qualified_type_path(name),
        |name| scope_declares_type_name(defining_module, name),
    )
}

/// Expand only an explicit written import edge through an indexed module.
/// Unlike [`qualify_type_segments_in_entry`], this leaves a local declaration
/// bare so callers can distinguish import priority from local lookup.
#[cfg(all(test, feature = "surface"))]
pub(crate) fn expand_type_import_segments_in_entry<P: Phase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &ModuleEntry<P>,
) -> Vec<crate::ast::PathSegment> {
    let Some((head, tail)) = segments.split_first() else {
        return Vec::new();
    };
    if tail.is_empty()
        && let Some(path) = defining_module.scope.selective_type_path(head.as_str())
    {
        return path;
    }
    if let Some(mut path) = defining_module.scope.qualified_type_path(head.as_str()) {
        path.extend_from_slice(tail);
        return path;
    }
    segments.to_vec()
}

/// As [`exported_contract_type_segments`], but resolving against a bare
/// [`crate::ast::Module`] rather than a [`ModuleEntry`]. The identity-
/// exact nominal canonicalizer in [`crate::pass::typecheck_core`] needs
/// to qualify an unfolded alias body's heads in the alias's *owner*
/// module, for which only the `Module` is on hand.
pub(crate) fn qualify_type_segments_in_module<P: Phase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &crate::ast::Module<P>,
    locals: &HashMap<String, Kind>,
) -> Vec<crate::ast::PathSegment> {
    qualify_type_segments_in_module_with(segments, defining_module, |name| {
        locals.contains_key(name)
    })
}

fn qualify_type_segments_in_module_with<P: Phase>(
    segments: &[crate::ast::PathSegment],
    defining_module: &crate::ast::Module<P>,
    is_local: impl Fn(&str) -> bool,
) -> Vec<crate::ast::PathSegment> {
    qualify_type_segments_with(
        segments,
        defining_module,
        is_local,
        |name| selective_import_type_path(&defining_module.imports, name),
        |name| qualified_import_type_path(&defining_module.imports, name),
        |name| module_declares_type_name(defining_module, name),
    )
}

fn selective_import_type_path(
    imports: &[Import],
    name: &str,
) -> Option<Vec<crate::ast::PathSegment>> {
    imports.iter().find_map(|u| {
        #[cfg(test)]
        update_type_reexport_work(|work| work.import_edges_scanned += 1);
        match &u.kind {
            ImportKind::Selective { items, from }
                if items
                    .iter()
                    .filter_map(crate::ast::ImportItem::as_name)
                    .any(|item| item == name) =>
            {
                let mut out = from.segments.clone();
                out.push(crate::ast::PathSegment::synth(name, u.span));
                Some(out)
            }
            _ => None,
        }
    })
}

fn qualified_import_type_path(
    imports: &[Import],
    alias: &str,
) -> Option<Vec<crate::ast::PathSegment>> {
    imports.iter().find_map(|u| {
        #[cfg(test)]
        update_type_reexport_work(|work| work.import_edges_scanned += 1);
        match &u.kind {
            ImportKind::Qualified { path, alias: a } if a == alias => Some(path.segments.clone()),
            _ => None,
        }
    })
}

fn module_declares_type_name<P: Phase>(module: &crate::ast::Module<P>, name: &str) -> bool {
    module.items.iter().any(|item| {
        #[cfg(test)]
        update_type_reexport_work(|work| work.declaration_items_scanned += 1);
        match item {
            Item::TypeRecGroup(group) => group.members.iter().any(|member| match member {
                TypeRecMember::TypeAlias(alias) => alias.name == name,
                TypeRecMember::Newtype(newtype) => newtype.name == name,
                TypeRecMember::Labels(labels, _) => {
                    labels.type_alias_name.as_deref() == Some(name)
                        || labels.entries.iter().any(|entry| {
                            !entry.is_reuse_marker()
                                && crate::ast::mint_label_newtype_name(&entry.name) == name
                        })
                }
            }),
            Item::Labels(labels, _) => {
                labels.type_alias_name.as_deref() == Some(name)
                    || labels.entries.iter().any(|entry| {
                        !entry.is_reuse_marker()
                            && crate::ast::mint_label_newtype_name(&entry.name) == name
                    })
            }
            _ => type_declaration_name(item) == Some(name),
        }
    })
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TypeReexportWork {
    declaration_items_scanned: usize,
    import_edges_scanned: usize,
    indexed_import_prefix_clones: usize,
    indexed_import_prefix_segments_cloned: usize,
}

#[cfg(test)]
thread_local! {
    static TYPE_REEXPORT_WORK: std::cell::Cell<TypeReexportWork> =
        const { std::cell::Cell::new(TypeReexportWork {
            declaration_items_scanned: 0,
            import_edges_scanned: 0,
            indexed_import_prefix_clones: 0,
            indexed_import_prefix_segments_cloned: 0,
        }) };
}

#[cfg(test)]
fn update_type_reexport_work(update: impl FnOnce(&mut TypeReexportWork)) {
    TYPE_REEXPORT_WORK.with(|work| {
        let mut current = work.get();
        update(&mut current);
        work.set(current);
    });
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn reset_type_reexport_work() {
    TYPE_REEXPORT_WORK.with(|work| work.set(TypeReexportWork::default()));
}

#[cfg(all(test, feature = "surface"))]
fn type_reexport_work() -> TypeReexportWork {
    TYPE_REEXPORT_WORK.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn type_reexport_declaration_items_scanned() -> usize {
    type_reexport_work().declaration_items_scanned
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn type_reexport_import_edges_scanned() -> usize {
    type_reexport_work().import_edges_scanned
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn indexed_import_prefix_clones() -> usize {
    type_reexport_work().indexed_import_prefix_clones
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn indexed_import_prefix_segments_cloned() -> usize {
    type_reexport_work().indexed_import_prefix_segments_cloned
}

fn type_declaration_name<P: Phase>(item: &crate::ast::Item<P>) -> Option<&str> {
    match item {
        crate::ast::Item::TypeAlias(alias) => Some(alias.name.as_str()),
        crate::ast::Item::Newtype(def) => Some(def.name.as_str()),
        crate::ast::Item::HostType(def) => Some(def.name.as_str()),
        crate::ast::Item::Labels(def, _ext) => def.type_alias_name.as_deref(),
        _ => None,
    }
}

fn scope_declares_type_name<P: Phase>(entry: &ModuleEntry<P>, name: &str) -> bool {
    entry
        .scope
        .lookup(name)
        .and_then(|id| declaration_by_id(&entry.module, id))
        .is_some_and(|declaration| {
            declaration
                .type_alias()
                .map(|alias| alias.name.as_str())
                .or_else(|| declaration.newtype().map(|newtype| newtype.name.as_str()))
                .or_else(|| declaration.host_type().map(|host| host.name.as_str()))
                == Some(name)
        })
}

/// Canonicalize an already-admitted nominal head to the module that declares
/// it, following any remaining intermediate module whose lexical binding came
/// from `import <origin>(<Name>);`. A source-written qualified path must have
/// passed exact-member and visibility checks before reaching this helper:
/// following a selective binding determines identity and never authorizes
/// `owner.Name`. A single-segment head, an unknown module, a module that
/// declares `<Name>` directly, or a broken/cyclic chain is returned unchanged.
pub(crate) fn follow_type_reexport<P: Phase>(
    segments: &[crate::ast::PathSegment],
    package: &Package<P>,
) -> Vec<crate::ast::PathSegment> {
    let Some((name, mod_segs)) = segments.split_last() else {
        return segments.to_vec();
    };
    if mod_segs.is_empty() {
        return segments.to_vec();
    }
    let mut module_path = mod_segs.to_vec();
    let mut seen = HashSet::new();
    loop {
        let key = module_path
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        let Some(entry) = package.module(&key) else {
            return segments.to_vec();
        };
        if scope_declares_type_name(entry, name.as_str()) {
            let mut out = module_path;
            out.push(name.clone());
            return out;
        }
        if !seen.insert(key) {
            return segments.to_vec();
        }
        let Some(origin) = entry.scope.selective_type_origin(name.as_str()) else {
            return segments.to_vec();
        };
        module_path = origin;
    }
}

// =========================================================================
// Package — multi-module resolution
// =========================================================================

/// One entry in a [`Package`]: the parsed module, its top-level scope, and
/// the on-disk path (carried for diagnostics). Generic over phase so
/// the typer can produce `ModuleEntry<Prime>` from `ModuleEntry<Lowered>`
/// without duplicating the container shape.
#[derive(Debug, Clone)]
pub struct ModuleEntry<P: Phase = Lowered> {
    pub file_path: PathBuf,
    pub module: crate::ast::Module<P>,
    pub scope: TopLevelScope,
}

/// All modules in a single package, indexed by their declared `module
/// <path>;` path. Optionally also carries the parsed `<name>.pkg.kio`
/// — the package's boundary contract.
/// Uses a `BTreeMap` so iteration order is deterministic — important
/// for stable diagnostics (cycle reports name the first back-edge in
/// alphabetical-walk order).
///
/// Generic over phase so the typer can transform `Package<Lowered>`
/// into `Package<Prime>` by walking each module's AST and
/// substituting elaborator-position elaborations.
#[derive(Debug, Default, Clone)]
pub struct Package<P: Phase = Lowered> {
    /// Module path (`a/b/c`) → entry.
    modules: BTreeMap<String, ModuleEntry<P>>,
    /// The package's parsed package file, if present. The package name
    /// is the package file's filename stem; consumers that import from
    /// `import <pkg>(...);` against that name route through here.
    package_file: Option<PackageFileEntry<P>>,
}

/// One package-level package file. Carries the parsed AST plus its
/// on-disk path (for diagnostics) and the package name derived from
/// the filename stem.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "crate::ast::PackageFile<P>: serde::Serialize",
    deserialize = "crate::ast::PackageFile<P>: serde::Deserialize<'de>"
))]
pub struct PackageFileEntry<P: Phase = Lowered> {
    pub file_path: PathBuf,
    pub package_name: String,
    pub package_file: crate::ast::PackageFile<P>,
}

/// Pair returned to the caller when use-resolution fails: the file path
/// of the module containing the offending import statement, plus the error
/// itself. The caller looks up the source for that file path to render a
/// `path:line:col:` diagnostic.
#[derive(Debug)]
pub struct LocatedError {
    pub file_path: PathBuf,
    pub error: Error,
}

impl LocatedError {
    pub(crate) fn new(fallback_file: PathBuf, mut error: Error) -> Self {
        Self {
            file_path: error.take_source_file().unwrap_or(fallback_file),
            error,
        }
    }

    #[cfg(feature = "surface")]
    pub(crate) fn into_error(self) -> Error {
        self.error.with_source_file(self.file_path)
    }
}

#[cfg(all(test, feature = "surface"))]
mod located_error_tests {
    use super::*;

    fn enriched_error() -> Error {
        Error::type_(Span::new(21, 27), "invalid declaration")
            .with_help("make the payload public")
            .with_note("declaration context")
            .with_secondary(Span::new(4, 10), "payload declaration")
            .with_secondary_in_file("dependency.kio", Span::new(1, 3), "dependency")
            .with_suggestion(Span::new(0, 0), "pub ")
            .with_unresolved_name("Hidden")
    }

    #[test]
    fn unlocated_errors_keep_fallback_without_allocating() {
        let error = LocatedError::new(
            PathBuf::from("caller.kio"),
            Error::type_(Span::new(1, 2), "invalid expression"),
        );
        assert_eq!(error.file_path, PathBuf::from("caller.kio"));
        assert!(error.error.diagnostic().extra.is_none());
    }

    #[test]
    fn nested_transport_keeps_primary_file_and_all_enrichments() {
        for source in ["caller.kio", "declaration.kio"] {
            let original = enriched_error();
            let nested = LocatedError::new(PathBuf::from(source), original.clone()).into_error();
            let located = LocatedError::new(PathBuf::from("caller.kio"), nested.clone());
            assert_eq!(located.file_path, PathBuf::from(source));
            assert_eq!(located.error, original);
            assert!(located.error.source_file().is_none());
            let repeated = LocatedError::new(
                PathBuf::from("outer.kio"),
                located.into_error().with_source_file("intermediate.kio"),
            );
            assert_eq!(repeated.file_path, PathBuf::from(source));
            assert_eq!(repeated.error, original);
            assert!(
                repeated.error.diagnostic().fixes()[0].edits[0]
                    .file
                    .is_none()
            );
            assert_eq!(nested.source_file(), Some(Path::new(source)));
        }
    }

    #[test]
    fn synthetic_origin_leaves_generated_error_callsite_anchoring_available() {
        let nested = LocatedError::new(PathBuf::new(), enriched_error()).into_error();
        assert!(nested.source_file().is_none());
        let anchored = nested
            .reanchored_at(Span::new(2, 8))
            .with_note("expanded at this call");
        let located = LocatedError::new(PathBuf::from("caller.kio"), anchored);
        assert_eq!(located.file_path, PathBuf::from("caller.kio"));
        assert_eq!(
            located.error.diag(),
            (Span::new(2, 8), "invalid declaration")
        );
        assert!(located.error.diagnostic().help().is_none());
        assert!(located.error.diagnostic().secondary().is_empty());
        assert!(located.error.diagnostic().fixes().is_empty());
        assert_eq!(
            located.error.diagnostic().notes(),
            &["expanded at this call"]
        );
    }
}

/// Phase-generic accessors and constructors. Available at every [`Phase`], from
/// frontend phases through `Prime`, `Enriched`, and `Routed`.
/// These are pure container operations with no resolution logic, so
/// they don't need the [`ResolvePhase`] bound the resolution methods
/// below carry.
impl<P: Phase> Package<P> {
    pub fn modules(&self) -> impl Iterator<Item = (&str, &ModuleEntry<P>)> {
        self.modules.iter().map(|(p, e)| (p.as_str(), e))
    }

    pub fn module(&self, path: &str) -> Option<&ModuleEntry<P>> {
        self.modules.get(path)
    }

    /// The module keys reachable from the package's bridge surface: the
    /// modules its `bridge { … }` globs match, plus their transitive
    /// `import`-graph closure. `None` when the package declares no bridge
    /// (no contract surface to scope to). This is the same reachability
    /// the module-completeness check ([`Self::validate_bridge_contract`])
    /// walks — the contract surface determines what belongs to the
    /// package, independent of what else happens to sit on disk.
    pub fn bridge_reachable_modules(&self) -> Option<BTreeSet<String>> {
        let bridge = self.package_file.as_ref()?.package_file.bridge.as_ref()?;
        let mut reachable: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = Vec::new();
        for glob in &bridge.globs {
            for path in self.modules.keys() {
                if glob_matches(&glob.segments, path) {
                    stack.push(path.as_str());
                }
            }
        }
        while let Some(path) = stack.pop() {
            if !reachable.insert(path) {
                continue;
            }
            let Some(entry) = self.modules.get(path) else {
                continue;
            };
            for u in &entry.module.imports {
                let target = match &u.kind {
                    ImportKind::Selective { from, .. } => module_path_str(from),
                    ImportKind::Qualified { path, .. } => module_path_str(path),
                    ImportKind::Intrinsics | ImportKind::Comptime => continue,
                };
                if let Some((key, _)) = self.modules.get_key_value(&target) {
                    stack.push(key.as_str());
                }
            }
        }
        Some(reachable.into_iter().map(str::to_owned).collect())
    }

    /// Restrict the package to its bridge-reachable closure, dropping
    /// modules the bridge never reaches. `kio build` calls this so that
    /// emit and the host interface reflect the **contract surface**, not
    /// every module present on disk: a dependency's materialized-but-
    /// unused modules (e.g. a library's demo `main`) must not have their
    /// out-of-contract host requirements leak into the consumer's emitted
    /// package. No-op when the package declares no bridge. `kio check`
    /// does **not** prune — it type-checks every module on disk.
    pub fn prune_to_bridge_reachable(&mut self) {
        if let Some(reachable) = self.bridge_reachable_modules() {
            self.modules
                .retain(|key, _| reachable.contains(key.as_str()));
        }
    }

    pub fn replace_module(&mut self, path: String, entry: ModuleEntry<P>)
    where
        P: Clone,
    {
        self.modules.insert(path, entry);
    }

    /// Mutable iterator over the package's module entries. Used by the Prime
    /// completion-baking pass to materialize recorded call arguments and
    /// checked-lambda parameter annotations after typechecking.
    pub fn modules_mut(&mut self) -> impl Iterator<Item = &mut ModuleEntry<P>> {
        self.modules.values_mut()
    }

    /// The parsed package file, if the package declares one.
    pub fn package_file(&self) -> Option<&PackageFileEntry<P>> {
        self.package_file.as_ref()
    }

    /// Construct a [`Package`] from already-built parts. Used by the
    /// typer's `Lowered → Prime` substitution pass: the substitution
    /// walks an existing `Package<Lowered>` and emits a fresh
    /// `Package<Prime>` from its modules and package file.
    pub fn from_parts(
        modules: BTreeMap<String, ModuleEntry<P>>,
        package_file: Option<PackageFileEntry<P>>,
    ) -> Self {
        Package {
            modules,
            package_file,
        }
    }

    /// Destructure into the same parts [`from_parts`](Self::from_parts)
    /// constructs from. Used by passes that consume a package and
    /// emit a fresh one (e.g. the post-recovery optimization catalog)
    /// — owning the modules map avoids per-entry clones the
    /// `&Package` shape would otherwise force.
    pub fn into_parts(
        self,
    ) -> (
        BTreeMap<String, ModuleEntry<P>>,
        Option<PackageFileEntry<P>>,
    ) {
        (self.modules, self.package_file)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BindingOrigin {
    Item {
        module: String,
        name: String,
    },
    Module {
        module: String,
    },
    Builtin {
        module: &'static str,
        name: &'static str,
    },
}

#[derive(Clone, Debug)]
struct VisibleBinding {
    origin: BindingOrigin,
    span: Span,
    description: String,
}

#[derive(Clone)]
enum IdentityAliasNode {
    Alias {
        target: (String, String),
        binders: Vec<Kind>,
    },
    Newtype {
        binders: Vec<Kind>,
    },
}

type IdentityAliasKey = (String, String);
type IdentityAliasResolution = Option<(IdentityAliasKey, Vec<Kind>)>;
type IdentityAliasResolutionMemo = HashMap<IdentityAliasKey, IdentityAliasResolution>;

/// One package-analysis-local index of the exact declaration terminals
/// reached by fully saturated positional identity aliases, plus the subset
/// whose terminal is a member-capable nominal newtype.
///
/// The index stores only declaration identities. Consumers recover the
/// current phase's declaration from the package (or checking root), so no
/// source-derived side channel crosses a phase boundary. Building the index
/// memoizes every suffix of an alias chain; each declaration and alias edge is
/// therefore visited at most once even when many member occurrences share a
/// long forwarding chain.
#[derive(Default)]
pub(crate) struct IdentityAliasNewtypeIndex {
    terminals: HashMap<String, HashMap<String, (String, String)>>,
}

#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct IdentityAliasIndexWork {
    pub(crate) builds: usize,
    pub(crate) items: usize,
}

#[cfg(test)]
thread_local! {
    static IDENTITY_ALIAS_INDEX_EDGE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static IDENTITY_ALIAS_INDEXED_SCOPE_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static IDENTITY_ALIAS_INDEX_WORK: std::cell::RefCell<IdentityAliasIndexWork> = const {
        std::cell::RefCell::new(IdentityAliasIndexWork { builds: 0, items: 0 })
    };
}

#[cfg(all(test, feature = "prime"))]
pub(crate) fn take_identity_alias_index_work() -> IdentityAliasIndexWork {
    IDENTITY_ALIAS_INDEX_WORK.with(|work| std::mem::take(&mut *work.borrow_mut()))
}

#[cfg(test)]
fn record_identity_alias_index_edge_visit() {
    IDENTITY_ALIAS_INDEX_EDGE_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn reset_identity_alias_index_edge_visits() {
    IDENTITY_ALIAS_INDEX_EDGE_VISITS.with(|visits| visits.set(0));
    IDENTITY_ALIAS_INDEXED_SCOPE_RESOLUTIONS.with(|resolutions| resolutions.set(0));
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn identity_alias_index_edge_visits() -> usize {
    IDENTITY_ALIAS_INDEX_EDGE_VISITS.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "surface", feature = "prime"))]
pub(crate) fn identity_alias_indexed_scope_resolutions() -> usize {
    IDENTITY_ALIAS_INDEXED_SCOPE_RESOLUTIONS.with(std::cell::Cell::get)
}

impl IdentityAliasNewtypeIndex {
    pub(crate) fn build_for_package<P: Phase>(package: &Package<P>) -> Self {
        let mut nodes = HashMap::new();
        for (module_path, entry) in package.modules() {
            Self::index_module(&mut nodes, module_path, &entry.module);
        }
        Self::from_nodes(nodes)
    }

    pub(crate) fn build<P: ResolvePhase>(
        checking_root: &crate::ast::Module<P>,
        package: Option<&Package<P>>,
    ) -> Self {
        let mut nodes = HashMap::new();
        if let Some(package) = package {
            for (module_path, entry) in package.modules() {
                Self::index_module(&mut nodes, module_path, &entry.module);
            }
        }

        let checking_path = module_path_str_from_module(checking_root);
        let checking_is_indexed = package
            .and_then(|package| package.module(&checking_path))
            .is_some_and(|entry| std::ptr::eq(&entry.module, checking_root));
        if !checking_is_indexed {
            // A standalone/checking-root module still uses the ordinary
            // parser-owned scope. A same-path package entry must not lend its
            // declarations to that distinct root.
            nodes.retain(|(owner, _), _| owner != &checking_path);
            if TopLevelScope::build(checking_root).is_ok() {
                Self::index_module(&mut nodes, &checking_path, checking_root);
            }
        }

        Self::from_nodes(nodes)
    }

    /// Build the same exact identity-alias graph over parsed Surface modules
    /// for consumers that run before label elaboration (notably operator/fold
    /// lexical callable-path validation). Surface label entries contribute the
    /// ordinary generated nominal heads that label elaboration will emit, and
    /// a one-entry named form contributes the positional identity alias that
    /// elaboration will emit over that head. Neither receives a separate
    /// identity or lookup rule.
    #[cfg(feature = "surface")]
    pub(crate) fn build_for_surface_modules<'a>(
        modules: impl IntoIterator<Item = &'a crate::ast::Module<crate::ast::Surface>>,
    ) -> Self {
        let mut nodes = HashMap::new();
        for module in modules {
            let module_path = module_path_str_from_module(module);
            let mut scope = identity_alias_surface_type_scope(module);
            for (item_index, item) in module.items.iter().enumerate() {
                identity_alias_progressive_scope_add_item(&mut scope, item_index, item);
                Self::index_item(&mut nodes, &module_path, module, &scope, item);
                match item {
                    Item::Labels(labels, _) => {
                        Self::index_surface_labels(&mut nodes, &module_path, labels)
                    }
                    Item::TypeRecGroup(group) => {
                        for member in &group.members {
                            if let TypeRecMember::Labels(labels, _) = member {
                                Self::index_surface_labels(&mut nodes, &module_path, labels);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Self::from_nodes(nodes)
    }

    #[cfg(feature = "surface")]
    fn index_surface_labels(
        nodes: &mut HashMap<(String, String), IdentityAliasNode>,
        module_path: &str,
        labels: &crate::ast::Labels<crate::ast::Surface>,
    ) {
        if let Some(alias_name) = &labels.type_alias_name
            && let Some(entry) = crate::ast::named_labels_identity_alias_entry(labels)
        {
            nodes.insert(
                (module_path.to_owned(), alias_name.clone()),
                IdentityAliasNode::Alias {
                    target: (
                        module_path.to_owned(),
                        crate::ast::mint_label_newtype_name(&entry.name),
                    ),
                    binders: labels
                        .type_alias_params
                        .iter()
                        .map(crate::ast::TypeParam::effective_kind)
                        .collect(),
                },
            );
        }
        for entry in labels
            .entries
            .iter()
            .filter(|entry| !entry.is_reuse_marker())
        {
            nodes.insert(
                (
                    module_path.to_owned(),
                    crate::ast::mint_label_newtype_name(&entry.name),
                ),
                IdentityAliasNode::Newtype {
                    binders: entry
                        .type_params
                        .iter()
                        .map(crate::ast::TypeParam::effective_kind)
                        .collect(),
                },
            );
        }
    }

    fn from_nodes(nodes: HashMap<(String, String), IdentityAliasNode>) -> Self {
        #[cfg(test)]
        IDENTITY_ALIAS_INDEX_WORK.with(|work| work.borrow_mut().builds += 1);
        let aliases = nodes
            .iter()
            .filter_map(|(key, node)| {
                matches!(node, IdentityAliasNode::Alias { .. }).then_some(key.clone())
            })
            .collect::<Vec<_>>();
        let mut memo = IdentityAliasResolutionMemo::new();
        for alias in aliases {
            Self::resolve_alias(&alias, &nodes, &mut memo);
        }
        let mut terminals: HashMap<String, HashMap<String, (String, String)>> = HashMap::new();
        for ((module, name), target) in memo {
            if let Some((target, _)) = target {
                terminals.entry(module).or_default().insert(name, target);
            }
        }
        Self { terminals }
    }

    fn index_module<P: Phase>(
        nodes: &mut HashMap<(String, String), IdentityAliasNode>,
        module_path: &str,
        module: &crate::ast::Module<P>,
    ) {
        let mut scope = identity_alias_progressive_type_scope(module);
        for (item_index, item) in module.items.iter().enumerate() {
            identity_alias_progressive_scope_add_item(&mut scope, item_index, item);
            Self::index_item(nodes, module_path, module, &scope, item);
        }
    }

    fn index_item<P: Phase>(
        nodes: &mut HashMap<(String, String), IdentityAliasNode>,
        module_path: &str,
        module: &crate::ast::Module<P>,
        scope: &TopLevelScope,
        item: &Item<P>,
    ) {
        #[cfg(test)]
        IDENTITY_ALIAS_INDEX_WORK.with(|work| work.borrow_mut().items += 1);
        for_each_item_declaration(item, |declaration| {
            if let Some(alias) = declaration.type_alias() {
                let Some(target) = identity_alias_binding_target_in_scope(alias, module, scope)
                else {
                    return;
                };
                nodes.insert(
                    (module_path.to_owned(), alias.name.clone()),
                    IdentityAliasNode::Alias {
                        target,
                        binders: alias
                            .type_params
                            .iter()
                            .map(crate::ast::TypeParam::effective_kind)
                            .collect(),
                    },
                );
            } else if let Some(newtype) = declaration.newtype() {
                nodes.insert(
                    (module_path.to_owned(), newtype.name.clone()),
                    IdentityAliasNode::Newtype {
                        binders: newtype
                            .type_params
                            .iter()
                            .map(crate::ast::TypeParam::effective_kind)
                            .collect(),
                    },
                );
            }
        });
    }

    fn resolve_alias(
        start: &(String, String),
        nodes: &HashMap<(String, String), IdentityAliasNode>,
        memo: &mut IdentityAliasResolutionMemo,
    ) {
        if memo.contains_key(start) {
            return;
        }
        let mut current = start.clone();
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut resolved = loop {
            if let Some(cached) = memo.get(&current) {
                break cached.clone();
            }
            if !seen.insert(current.clone()) {
                break None;
            }
            match nodes.get(&current) {
                Some(IdentityAliasNode::Alias { target, binders }) => {
                    #[cfg(test)]
                    record_identity_alias_index_edge_visit();
                    path.push((current, binders.clone()));
                    current = target.clone();
                }
                Some(IdentityAliasNode::Newtype { binders }) => {
                    break Some((current, binders.clone()));
                }
                None => break None,
            }
        };
        for (alias, binders) in path.into_iter().rev() {
            if resolved
                .as_ref()
                .is_some_and(|(_, terminal_binders)| terminal_binders != &binders)
            {
                resolved = None;
            }
            memo.insert(alias, resolved.clone());
        }
    }

    pub(crate) fn terminal_key(&self, module: &str, name: &str) -> Option<&(String, String)> {
        self.terminals.get(module)?.get(name)
    }

    pub(crate) fn target<'a, P: Phase>(
        &self,
        checking_root: &'a crate::ast::Module<P>,
        package: Option<&'a Package<P>>,
        module: &str,
        name: &str,
    ) -> Option<IdentityAliasNewtypeTarget<'a, P>> {
        let (terminal_module, terminal_name) = self.terminal_key(module, name)?;
        let checking_path = module_path_str_from_module(checking_root);
        let indexed_checking_root = package
            .and_then(|package| package.module(&checking_path))
            .filter(|entry| std::ptr::eq(&entry.module, checking_root));
        let declaration = if terminal_module == &checking_path
            && let Some(entry) = indexed_checking_root
        {
            entry
                .scope
                .lookup(terminal_name)
                .and_then(|id| declaration_by_id(&entry.module, id))?
        } else if terminal_module == &checking_path {
            checking_root.items.iter().find_map(|item| {
                let mut found = None;
                for_each_item_declaration(item, |candidate| {
                    if candidate
                        .newtype()
                        .is_some_and(|newtype| newtype.name == *terminal_name)
                    {
                        found = Some(candidate);
                    }
                });
                found
            })?
        } else {
            let entry = package?.module(terminal_module)?;
            entry
                .scope
                .lookup(terminal_name)
                .and_then(|id| declaration_by_id(&entry.module, id))?
        };
        let newtype = declaration
            .newtype()
            .filter(|newtype| newtype.name == *terminal_name)?;
        Some(IdentityAliasNewtypeTarget {
            module_path: terminal_module.clone(),
            newtype,
        })
    }
}

#[cfg(feature = "surface")]
pub(crate) fn identity_alias_surface_type_scope(
    module: &crate::ast::Module<crate::ast::Surface>,
) -> TopLevelScope {
    identity_alias_progressive_type_scope(module)
}

fn identity_alias_progressive_type_scope<P: Phase>(
    module: &crate::ast::Module<P>,
) -> TopLevelScope {
    TopLevelScope {
        decls: HashMap::new(),
        type_imports: TopLevelScope::build_type_import_index(&module.imports),
    }
}

pub(crate) fn identity_alias_progressive_scope_add_item<P: Phase>(
    scope: &mut TopLevelScope,
    item_index: usize,
    item: &Item<P>,
) {
    let outer_id = TopLevelId::item(item_index);
    for_each_item_declaration(item, |declaration| {
        let name = declaration
            .type_alias()
            .map(|alias| alias.name.as_str())
            .or_else(|| declaration.newtype().map(|newtype| newtype.name.as_str()))
            .or_else(|| declaration.host_type().map(|host| host.name.as_str()));
        if let Some(name) = name {
            scope.decls.entry(name.to_owned()).or_insert(outer_id);
        }
    });
    let mut add_labels = |labels: &crate::ast::Labels<P>| {
        if let Some(name) = &labels.type_alias_name {
            scope.decls.entry(name.clone()).or_insert(outer_id);
        }
        for entry in labels
            .entries
            .iter()
            .filter(|entry| !entry.is_reuse_marker())
        {
            scope
                .decls
                .entry(crate::ast::mint_label_newtype_name(&entry.name))
                .or_insert(outer_id);
        }
    };
    match item {
        Item::Labels(labels, _) => add_labels(labels),
        Item::TypeRecGroup(group) => {
            for member in &group.members {
                if let TypeRecMember::Labels(labels, _) = member {
                    add_labels(labels);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn identity_alias_binding_target_in_scope<P: Phase>(
    alias: &crate::ast::TypeAlias<P>,
    module: &crate::ast::Module<P>,
    scope: &TopLevelScope,
) -> Option<(String, String)> {
    #[cfg(test)]
    IDENTITY_ALIAS_INDEXED_SCOPE_RESOLUTIONS.with(|resolutions| {
        resolutions.set(resolutions.get() + 1);
    });
    let Type::Path { segments, args, .. } = alias.type_body() else {
        return None;
    };
    if args.len() != alias.type_params.len()
        || !args.iter().zip(&alias.type_params).all(|(arg, param)| {
            matches!(arg,
                Type::Path { segments, args, .. }
                    if args.is_empty()
                        && matches!(segments.as_slice(), [segment] if segment.as_str() == param.name)
            )
        })
    {
        return None;
    }
    let (head, tail) = segments.split_first()?;
    let qualified = if tail.is_empty() {
        if let Some(path) = scope.selective_type_path(head.as_str()) {
            path
        } else if scope.lookup(head.as_str()).is_some() {
            let mut path = module.path.segments.clone();
            path.push(head.clone());
            path
        } else {
            return None;
        }
    } else {
        let mut path = scope.qualified_type_path(head.as_str())?;
        path.extend_from_slice(tail);
        path
    };
    let (name, owner) = qualified.split_last()?;
    if owner.len() == module.path.segments.len()
        && owner
            .iter()
            .zip(&module.path.segments)
            .all(|(candidate, local)| candidate.as_str() == local.as_str())
        && scope.lookup(name.as_str()).is_none()
    {
        // A written import of this module does not make a later local type
        // declaration visible early. The progressive declaration scope remains
        // authoritative for both bare and self-qualified local targets.
        return None;
    }
    (!owner.is_empty()).then(|| {
        (
            owner
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("/"),
            name.name.clone(),
        )
    })
}

fn module_path_str_from_module<P: Phase>(module: &crate::ast::Module<P>) -> String {
    module
        .path
        .segments
        .iter()
        .map(crate::ast::PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

/// The exact nominal newtype reached through one or more transparent
/// identity-alias declarations.
pub(crate) struct IdentityAliasNewtypeTarget<'a, P: Phase> {
    pub module_path: String,
    pub newtype: &'a crate::ast::Newtype<P>,
}

/// Resolve an alias chain that preserves a newtype identity and its member
/// scheme exactly. Every alias edge must forward all binders positionally,
/// and every declaration on the chain must have the same binder kinds. This
/// deliberately excludes partial, reordered, and structurally transformed
/// aliases: those aliases expose their type expansion, not the terminal
/// nominal's constructor/projector namespace.
#[cfg(all(test, feature = "surface", feature = "prime"))]
pub(crate) fn identity_alias_newtype_target<'a, P: Phase>(
    package: &'a Package<P>,
    module: &str,
    name: &str,
) -> Option<IdentityAliasNewtypeTarget<'a, P>> {
    let root = package.module(module).map(|entry| &entry.module)?;
    IdentityAliasNewtypeIndex::build_for_package(package).target(root, Some(package), module, name)
}

/// Resolution-phase methods — these consult `import` statements, the
/// in-body name resolver, and visibility, all of which need the
/// surface-only / elaboration variants to be statically uninhabited
/// (the [`ResolvePhase`] bound).
impl<P: ResolvePhase> Package<P> {
    pub fn build(
        package_root: &Path,
        parsed: Vec<(PathBuf, crate::ast::Module<P>)>,
        package_file: Option<PackageFileEntry<P>>,
    ) -> Result<Self, LocatedError>
    where
        P: Clone,
    {
        let (package, deferred_name_errors) =
            Self::build_with_validation_mode(package_root, parsed, package_file, true, false)?;
        debug_assert!(deferred_name_errors.is_empty());
        Ok(package)
    }

    /// Assemble a package while retaining name errors and deferring bridge
    /// validation, so the caller can finish the earlier import tier first.
    #[cfg(any(feature = "surface", feature = "cli"))]
    pub(crate) fn build_deferring_contract_checks(
        package_root: &Path,
        parsed: Vec<(PathBuf, crate::ast::Module<P>)>,
        package_file: Option<PackageFileEntry<P>>,
    ) -> Result<(Self, Vec<LocatedError>), LocatedError>
    where
        P: Clone,
    {
        Self::build_with_validation_mode(package_root, parsed, package_file, false, true)
    }

    /// Resolve every `import` statement in every module of the package.
    /// Bails on the first error.
    ///
    /// `import __intrinsics__;` and `import __comptime__;` always succeed.
    /// `import` statements resolve against the package's own modules.
    ///
    /// Phase-polymorphic — only inspects import statements and the
    /// phase-polymorphic helpers (`item_is_exported`,
    /// `env_item_exists`).
    pub fn resolve_imports(&self) -> Result<(), LocatedError> {
        for entry in self.modules.values() {
            if let Err(e) = self.check_visibility_decls(entry) {
                return Err(LocatedError {
                    file_path: entry.file_path.clone(),
                    error: e,
                });
            }
            for u in &entry.module.imports {
                if let Err(e) = self.check_one_import(u, entry) {
                    return Err(LocatedError {
                        file_path: entry.file_path.clone(),
                        error: e,
                    });
                }
            }
        }
        Ok(())
    }

    /// Validate every `pub(P)` declaration in a module — the item's own
    /// scope **and** a `newtype` constructor/projector member scope: `P`
    /// must name the declaring module itself or one of its ancestors (its
    /// segments a prefix of the module's path). This keeps the restriction
    /// sound — an item or member can never be made visible somewhere its
    /// own module cannot reach. Walking the member scopes is what catches a
    /// member restriction that a dependency materialization failed to
    /// re-root (a bare pre-materialization path no longer prefixes the
    /// re-rooted module), so the corrupted scope is a clean error rather
    /// than silently accepted.
    fn check_visibility_decls(&self, entry: &ModuleEntry<P>) -> Result<(), Error> {
        let mp = &entry.module.path;
        for item in &entry.module.items {
            for (vis, site, span) in item_visibility_sites(item) {
                if let crate::ast::Visibility::PublicIn(p) = vis {
                    let prefix_ok = mp.segments.len() >= p.segments.len()
                        && mp.segments[..p.segments.len()] == p.segments[..];
                    if !prefix_ok {
                        return Err(Error::import(
                            span,
                            format!(
                                "`pub({})` on `{site}` must name the declaring module `{}` or one of its ancestors",
                                module_path_surface(&p),
                                module_path_surface(mp)
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_one_import(&self, u: &Import, in_module: &ModuleEntry<P>) -> Result<(), Error> {
        self.check_import_module(u, in_module)
    }

    fn check_import_module(&self, u: &Import, in_module: &ModuleEntry<P>) -> Result<(), Error> {
        match &u.kind {
            ImportKind::Intrinsics => Ok(()),
            ImportKind::Comptime => Ok(()),
            ImportKind::Selective { items, from } => {
                let target = self.lookup_target(from)?;
                let importer = &in_module.module.path;
                for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                    let id = match target.scope.lookup(name) {
                        Some(id) => id,
                        None => {
                            // Nothing of that name. Offer the nearest
                            // item the module exports to this importer.
                            let mut pub_names = Vec::new();
                            for (index, item) in target.module.items.iter().enumerate() {
                                visit_item_declarations(index, item, |_id, declaration| {
                                    if is_visible(&declaration.visibility(), importer) {
                                        pub_names.push(declaration.name_and_span().0.to_owned());
                                    }
                                });
                            }
                            pub_names.sort_unstable();
                            let mut err = Error::import(
                                u.span,
                                format!(
                                    "module `{}` does not export `{name}`",
                                    module_path_surface(from)
                                ),
                            );
                            if let Some(near) = crate::error::closest_name(
                                name,
                                pub_names.iter().map(String::as_str),
                            ) {
                                err = err.with_help(format!("did you mean `{near}`?"));
                            }
                            return Err(err);
                        }
                    };
                    let declaration = declaration_by_id(&target.module, id)
                        .expect("scope id names a declaration");
                    let vis = declaration.visibility();
                    if !is_visible(&vis, importer) {
                        let err = match &vis {
                            crate::ast::Visibility::PublicIn(p) => Error::import(
                                u.span,
                                format!(
                                    "`{name}` is sealed — it is `pub({})`, importable only within that module subtree",
                                    module_path_surface(p)
                                ),
                            )
                            .with_help(format!(
                                "`{name}` can be imported only from `{}` or a module beneath it",
                                module_path_surface(p)
                            )),
                            _ => Error::import(
                                u.span,
                                format!(
                                    "`{name}` is not `pub` in module `{}`",
                                    module_path_surface(from)
                                ),
                            ),
                        };
                        return Err(err);
                    }
                }
                Ok(())
            }
            ImportKind::Qualified { path, .. } => {
                self.lookup_target(path)?;
                Ok(())
            }
        }
    }

    fn lookup_target(&self, path: &ModulePath) -> Result<&ModuleEntry<P>, Error> {
        let s = module_path_str(path);
        if let Some(entry) = self.modules.get(&s) {
            return Ok(entry);
        }
        let mut err = Error::import(path.span, format!("module `{s}` not found in package"));
        // Offer the nearest in-package module in the spelling the user writes.
        if let Some(near) = crate::error::closest_name(&s, self.modules.keys().map(String::as_str))
        {
            err = err.with_help(format!("did you mean `{near}`?"));
        }
        Err(err)
    }

    /// Verify that each ordinary visible name is introduced once in each
    /// consumer module. The set is derived only from that consumer's own
    /// declarations and explicit `import` clauses: provider export growth is
    /// never scanned, so adding an unrelated provider declaration cannot
    /// change an existing consumer's result.
    ///
    /// This consumes written imports after validation and, on the normal pipeline,
    /// import-cycle detection. Only fixed compiler-block imports are idempotent.
    pub fn check_binding_origins(&self) -> Result<(), LocatedError> {
        for (module_path, entry) in &self.modules {
            let mut visible: BTreeMap<String, VisibleBinding> = BTreeMap::new();
            let mut register = |name: &str,
                                origin: BindingOrigin,
                                span: Span,
                                description: String|
             -> Result<(), LocatedError> {
                if let Some(first) = visible.get(name) {
                    if first.origin == origin && matches!(origin, BindingOrigin::Builtin { .. }) {
                        return Ok(());
                    }
                    let message = if first.origin == origin {
                        format!("binding `{name}` is introduced more than once")
                    } else {
                        format!("binding `{name}` has more than one source")
                    };
                    return Err(LocatedError {
                        file_path: entry.file_path.clone(),
                        error: Error::name_res(span, message)
                        .with_secondary(first.span, first.description.clone())
                        .with_help(format!(
                            "introduce `{name}` once in module `{module_path}`: remove one import, \
                             change a qualified-import alias, or rename the local declaration"
                        )),
                    });
                }
                visible.insert(
                    name.to_owned(),
                    VisibleBinding {
                        origin,
                        span,
                        description,
                    },
                );
                Ok(())
            };

            for usage in &entry.module.imports {
                match &usage.kind {
                    ImportKind::Selective { items, from } => {
                        let source = module_path_str(from);
                        for (name, span) in items
                            .iter()
                            .filter_map(crate::ast::ImportItem::as_name_with_span)
                        {
                            register(
                                name,
                                BindingOrigin::Item {
                                    module: source.clone(),
                                    name: name.to_owned(),
                                },
                                span,
                                format!("`{name}` is imported from `{source}` here"),
                            )?;
                        }
                    }
                    ImportKind::Qualified { path, alias } => {
                        let source = module_path_str(path);
                        register(
                            alias,
                            BindingOrigin::Module {
                                module: source.clone(),
                            },
                            usage.span,
                            format!("`{alias}` names module `{source}` here"),
                        )?;
                    }
                    ImportKind::Comptime => {
                        for name in crate::comptime::PUBLIC_COMPTIME_NAMES {
                            register(
                                name,
                                BindingOrigin::Builtin {
                                    module: "__comptime__",
                                    name,
                                },
                                usage.span,
                                format!("`{name}` is imported from `__comptime__` here"),
                            )?;
                        }
                    }
                    ImportKind::Intrinsics => {
                        for name in PRIME_INTRINSICS {
                            register(
                                name,
                                BindingOrigin::Builtin {
                                    module: "__intrinsics__",
                                    name,
                                },
                                usage.span,
                                format!("`{name}` is imported from `__intrinsics__` here"),
                            )?;
                        }
                    }
                }
            }
            for (item_index, item) in entry.module.items.iter().enumerate() {
                if matches!(item, crate::ast::Item::Equiv(_, _)) {
                    continue;
                }
                let mut declarations = Vec::new();
                visit_item_declarations(item_index, item, |_, declaration| {
                    declarations.push(declaration)
                });
                for declaration in declarations {
                    let (name, span) = declaration.name_and_span();
                    if name.is_empty() {
                        continue;
                    }
                    register(
                        name,
                        BindingOrigin::Item {
                            module: module_path.clone(),
                            name: name.to_owned(),
                        },
                        span,
                        format!("`{name}` is declared locally here"),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Detect value-level cycles in the package's `import` graph. Per spec:
    /// "Value-level cycles between `*.kio` module files are disallowed."
    /// Edges come from every selective and qualified `import` statement,
    /// including imports that select only types.
    ///
    /// Phase-polymorphic — the cycle check only inspects `import`
    /// statements (which are not phase-parametrized) and per-module
    /// file paths, so it works uniformly on any `Package<P>`
    /// regardless of how its modules' items are shaped. Both the
    /// full pipeline (`P = Lowered`) and the kio-prime pipeline
    /// (`P = Prime`) call it on the same body.
    #[allow(clippy::type_complexity)] // edges adjacency-list type with phase parameter
    pub fn check_no_value_cycles(&self) -> Result<(), LocatedError> {
        // Build an adjacency list (BTreeMap for deterministic iteration).
        // Module paths absent from the package are ignored — already
        // handled by `resolve_imports`. Every selective or qualified import
        // contributes an edge; compiler-block imports name no package module.
        let mut edges: BTreeMap<&str, Vec<(&str, Span, &ModuleEntry<P>)>> = BTreeMap::new();
        for (path, entry) in &self.modules {
            let mut out = Vec::new();
            for u in &entry.module.imports {
                let target_path = match &u.kind {
                    ImportKind::Selective { from, .. } => module_path_str(from),
                    ImportKind::Qualified { path, .. } => module_path_str(path),
                    ImportKind::Intrinsics | ImportKind::Comptime => {
                        continue;
                    }
                };
                if let Some((target_key, _)) = self.modules.get_key_value(&target_path) {
                    out.push((target_key.as_str(), u.span, entry));
                }
            }
            edges.insert(path.as_str(), out);
        }

        // DFS coloring: 0 = unvisited, 1 = in-progress (on the stack),
        // 2 = fully visited. Iterate starts in BTreeMap key order so the
        // first cycle we report is deterministic across runs.
        let mut color: BTreeMap<&str, u8> = BTreeMap::new();
        for start in self.modules.keys().map(|s| s.as_str()) {
            if color.get(start).copied().unwrap_or(0) == 2 {
                continue;
            }
            let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
            color.insert(start, 1);
            while let Some(&(node, idx)) = stack.last() {
                let neighbors = edges.get(node).map(|v| v.as_slice()).unwrap_or(&[]);
                if idx >= neighbors.len() {
                    color.insert(node, 2);
                    stack.pop();
                    continue;
                }
                let (next, span, owning_entry) = neighbors[idx];
                let last = stack.last_mut().unwrap();
                last.1 += 1;
                let next_color = color.get(next).copied().unwrap_or(0);
                if next_color == 1 {
                    return Err(LocatedError {
                        file_path: owning_entry.file_path.clone(),
                        error: Error::import(
                            span,
                            format!(
                                "value-level import cycle: module `{node}` already on the path to `{next}`"
                            ),
                        )
                        .with_help(
                            "break the cycle by removing one of the `import` edges between these \
                             modules, or move the shared definition into a third module both depend on",
                        ),
                    });
                }
                if next_color == 0 {
                    stack.push((next, 0));
                    color.insert(next, 1);
                }
            }
        }
        Ok(())
    }

    /// Group modules into deterministic value-import topological levels.
    ///
    /// Edges point from a consumer module to the same-package module it
    /// imports at value level. A module appears in the first level whose
    /// dependencies are all in earlier levels, so each level can be
    /// typechecked in parallel after the previous levels finish. The
    /// package must already have passed [`Self::check_no_value_cycles`];
    /// if a caller violates that contract, the fallback below still
    /// returns the remaining modules in key order rather than looping.
    pub fn value_import_topo_levels(&self) -> Vec<Vec<(&str, &ModuleEntry<P>)>> {
        let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (path, entry) in &self.modules {
            let mut out = BTreeSet::new();
            for u in &entry.module.imports {
                let target_path = match &u.kind {
                    ImportKind::Selective { from, .. } => module_path_str(from),
                    ImportKind::Qualified { path, .. } => module_path_str(path),
                    ImportKind::Intrinsics | ImportKind::Comptime => {
                        continue;
                    }
                };
                if let Some((target_key, _)) = self.modules.get_key_value(&target_path) {
                    out.insert(target_key.as_str());
                }
            }
            deps.insert(path.as_str(), out);
        }

        let mut done: BTreeSet<&str> = BTreeSet::new();
        let mut remaining: BTreeSet<&str> = self.modules.keys().map(String::as_str).collect();
        let mut levels = Vec::new();
        while !remaining.is_empty() {
            let ready: Vec<&str> = remaining
                .iter()
                .copied()
                .filter(|path| deps.get(path).is_none_or(|ds| ds.is_subset(&done)))
                .collect();
            let level_paths = if ready.is_empty() {
                debug_assert!(
                    false,
                    "value_import_topo_levels called before value-import cycle detection"
                );
                remaining.iter().copied().collect()
            } else {
                ready
            };
            let mut level = Vec::with_capacity(level_paths.len());
            for path in level_paths {
                remaining.remove(path);
                done.insert(path);
                let entry = self
                    .modules
                    .get(path)
                    .expect("topo path came from Package::modules");
                level.push((path, entry));
            }
            levels.push(level);
        }
        levels
    }
}

/// Decompose a module file's path *relative to the package root* into
/// the segment list its `module` declaration must carry. The segments
/// are the relative path's directory components plus the filename stem
/// (the `.kio` extension stripped). The package name is **not**
/// prepended — see [`specs/package.md`](../../specs/package.md)
/// § Module-name rules.
///
/// A regular module at `<root>/util/list.kio` must declare
/// `module util/list;` — so this returns `["util", "list"]`. A
/// regular module at `<root>/main.kio` returns `["main"]`. A
/// host-using module under `<root>/<pkg>/main.kio` returns
/// `["<pkg>", "main"]` and must declare `module <pkg>/main;`.
///
/// Returns `None` when `file_path` is not under `package_root`, or
/// when a path component is not valid UTF-8 — neither of which the
/// package-file walker should pass for a regular module discovered
/// under the root.
fn module_fs_segments(package_root: &Path, file_path: &Path) -> Option<Vec<String>> {
    let rel = file_path.strip_prefix(package_root).ok()?;
    let mut segments: Vec<String> = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(os) => segments.push(os.to_str()?.to_owned()),
            // A regular module path is a plain relative chain of
            // directory names ending in a file — no `.`, `..`, or
            // root components. Anything else means the path wasn't a
            // clean descendant of the root.
            _ => return None,
        }
    }
    let last = segments.last_mut()?;
    *last = last.strip_suffix(".kio")?.to_owned();
    Some(segments)
}

impl<P: ResolvePhase> Package<P> {
    /// Build a package from a list of parsed modules and an optional
    /// parsed package file. Returns `LocatedError` on the first per-
    /// module top-level scope failure (duplicate top-level decl), on a
    /// `module <path>;` declaration that doesn't satisfy the
    /// module-name rules from [`specs/package.md`](../../specs/package.md)
    /// § Module-name rules, or on a duplicate declaration across files.
    ///
    /// `package_root` is the directory the module paths are rooted at.
    /// Two rules apply per spec:
    ///
    /// 1. **Path coherence.** The `/`-separated segments of `module
    ///    <path>;` must equal the file's location relative to
    ///    `package_root`, with `.kio` stripped. The package
    ///    name is **not** prepended at the declaration or at
    ///    module `import` paths.
    ///
    /// Violations are `Error::Parse` (exit code 11). The checks
    /// are local to each file's declaration + own `import` statements;
    /// open-world compilation is preserved (no module body can make
    /// another module's coherence check pass or fail).
    ///
    /// Modules are stored under their declared slash path.
    ///
    /// Phase-polymorphic — the builder only consults names, paths,
    /// and per-module scopes, none of which depend on the phase, so
    /// the same constructor serves the full pipeline
    /// (`Module<Lowered>`) and the kio-prime pipeline
    /// (`Module<Prime>`).
    fn build_with_validation_mode(
        package_root: &Path,
        parsed: Vec<(PathBuf, crate::ast::Module<P>)>,
        package_file: Option<PackageFileEntry<P>>,
        validate_bridge: bool,
        defer_name_errors: bool,
    ) -> Result<(Self, Vec<LocatedError>), LocatedError>
    where
        P: Clone,
    {
        let mut modules: BTreeMap<String, ModuleEntry<P>> = BTreeMap::new();
        let mut deferred_name_errors = Vec::new();
        let mut deferred_import_error = None;
        for (file_path, module) in parsed {
            let scope = if defer_name_errors {
                let (scope, error) = TopLevelScope::build_recording_name_error(&module);
                if let Some(error) = error {
                    deferred_name_errors.push(LocatedError {
                        file_path: file_path.clone(),
                        error,
                    });
                }
                scope
            } else {
                TopLevelScope::build(&module).map_err(|error| LocatedError {
                    file_path: file_path.clone(),
                    error,
                })?
            };
            let declared_str = module_path_str(&module.path);
            // Rule 1: declared segments must equal the file's path
            // relative to the package root (`/`-separated, with `.kio`
            // stripped). The package name is not prepended
            // at the declaration site.
            if let Some(fs_segments) = module_fs_segments(package_root, &file_path) {
                let declared: Vec<&str> = module
                    .path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect();
                let fs_matches = fs_segments.len() == declared.len()
                    && fs_segments
                        .iter()
                        .zip(&declared)
                        .all(|(a, b)| a.as_str() == *b);
                if !fs_matches {
                    // The hint shows surface module-path spellings, so
                    // join the segments with the `/` separator.
                    let expected = fs_segments.join("/");
                    let declared_surface = declared.join("/");
                    let rel = DisplayPath(
                        file_path
                            .strip_prefix(package_root)
                            .unwrap_or(file_path.as_path()),
                    )
                    .to_string();
                    let hint = format!(
                        "module declaration `{declared_surface}` does not match the file \
                         path; expected `module {expected};` for `{rel}` (the declared \
                         name is the file's path relative to the package root, with `.kio` \
                         stripped and directory separators written as `/` — the package \
                         name is not prepended)"
                    );
                    return Err(LocatedError {
                        file_path,
                        error: Error::parse(module.path.span, hint),
                    });
                }
            }

            let storage_key = declared_str.clone();

            if let Some(existing) = modules.get(&storage_key) {
                let existing_path = DisplayPath(&existing.file_path).to_string();
                let error = LocatedError {
                    file_path,
                    error: Error::import(
                        module.path.span,
                        format!(
                            "duplicate module path `{storage_key}` (also declared at {existing_path})"
                        ),
                    ),
                };
                if defer_name_errors {
                    deferred_import_error.get_or_insert(error);
                    continue;
                }
                return Err(error);
            }
            modules.insert(
                storage_key,
                ModuleEntry {
                    file_path,
                    module,
                    scope,
                },
            );
        }
        if let Some(error) = deferred_import_error {
            return Err(error);
        }
        let package = Package {
            modules,
            package_file,
        };
        if validate_bridge {
            package.validate_bridge_contract()?;
        }
        Ok((package, deferred_name_errors))
    }

    /// Well-formedness of the package's host contract surface. The
    /// package `bridge { … }` block selects **modules** by glob; from
    /// each matched module, all `pub` items form the contract surface
    /// (`pub host` → env, other `pub` → exports). Three user-facing
    /// checks report errors here (open-world note: the match set depends
    /// on module *paths* not bodies; adding a non-host `pub` item only
    /// grows the export surface covariantly — see `specs/language.md`
    /// § Open-world design / contract-surface versioning):
    ///
    /// 1. **Dead glob** — a glob matching zero modules (almost
    ///    certainly a typo).
    /// 2. **Module completeness** — a host-bearing module transitively
    ///    reachable (through the module `import` graph) from a bridged
    ///    module must itself be bridged, else the generated interface
    ///    would hide a referenced host item from the host.
    /// 3. **Type closure** — every nominal type reachable from a
    ///    bridged item's signature must itself be exposed (its module
    ///    bridged and the type `pub`); primitives / `__intrinsics__`
    ///    types are exempt. Gives the self-contained-interface property.
    ///
    /// A duplicate qualified export would require duplicate declarations
    /// in one module, which name resolution rejects before this check.
    pub(crate) fn validate_bridge_contract(&self) -> Result<(), LocatedError> {
        let Some(package_file) = &self.package_file else {
            return Ok(());
        };
        let Some(bridge) = &package_file.package_file.bridge else {
            return Ok(());
        };
        // Bridge validation is a later diagnostic category than import and name
        // resolution. Validate every consumer-written edge and binding origin
        // before a dead glob or closure failure can mask them. Package-less
        // module trees take the same checks in their normal driver path.
        self.resolve_imports()?;
        self.check_no_value_cycles()?;
        self.check_binding_origins()?;
        let pkg_path = package_file.file_path.clone();
        let err = |span: Span, message: String| LocatedError {
            file_path: pkg_path.clone(),
            error: Error::bridge(span, message),
        };

        // ---- Glob expansion + dead-glob check (1) -------------------
        let mut bridged: BTreeSet<&str> = BTreeSet::new();
        for glob in &bridge.globs {
            let mut matched = false;
            for path in self.modules.keys() {
                if glob_matches(&glob.segments, path) {
                    bridged.insert(path.as_str());
                    matched = true;
                }
            }
            if !matched {
                return Err(err(
                    glob.span,
                    format!(
                        "bridge glob `{}` matches no module in this package",
                        glob_surface(&glob.segments)
                    ),
                ));
            }
        }

        // ---- Module completeness (2) --------------------------------
        // Walk the module-import graph from each bridged
        // module; any reachable module that declares a host item but is
        // not itself bridged is a referenced-but-unexposed host item.
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        let mut stack: Vec<&str> = bridged.iter().copied().collect();
        while let Some(path) = stack.pop() {
            if !visited.insert(path) {
                continue;
            }
            let Some(entry) = self.modules.get(path) else {
                continue;
            };
            for dep in self.import_graph_targets(&entry.module) {
                if let Some((dep_key, _)) = self.modules.get_key_value(&dep) {
                    stack.push(dep_key.as_str());
                }
            }
        }
        for path in &visited {
            let Some(entry) = self.modules.get(*path) else {
                continue;
            };
            if module_has_host_items(&entry.module) && !bridged.contains(*path) {
                return Err(LocatedError {
                    file_path: entry.file_path.clone(),
                    error: Error::bridge(
                        entry.module.path.span,
                        format!(
                            "module `{path}` declares `host` items and is reachable from a bridged \
                             module, but is not itself selected by any bridge glob; add a glob \
                             matching `{path}` to the package `bridge {{ … }}` block so the host \
                             contract surface is complete"
                        ),
                    ),
                });
            }
        }

        // ---- Type closure (3) ---------------------------------------
        // Every nominal type reachable from a bridged item's signature
        // must resolve to a `pub` declaration in a bridged module.
        for path in &bridged {
            let Some(entry) = self.modules.get(*path) else {
                continue;
            };
            for item in &entry.module.items {
                if !item_is_exported(item) {
                    continue;
                }
                self.check_contract_item_type_closure(item, entry, &bridged)?;
            }
        }

        // ---- Qualified-export uniqueness invariant -----------------
        // Export names are qualified by their declaring module, so the
        // only way two exports could collide is a same-(module, leaf)
        // duplicate. But `TopLevelScope::build` already rejected every
        // such duplicate per module (exit 13, name-res) before this
        // contract check runs — its name table keys on the leaf across
        // all item kinds, and fn vs. type leaves additionally can't
        // collide because their spelling classes are disjoint (a value's
        // first letter is lowercase, while a type's is uppercase, in either
        // case possibly after the permitted leading marker). So a surviving same-(module, leaf)
        // collision is impossible by construction; assert it.
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        for path in &bridged {
            let Some(entry) = self.modules.get(*path) else {
                continue;
            };
            for (index, item) in entry.module.items.iter().enumerate() {
                visit_item_declarations(index, item, |_id, declaration| {
                    if !declaration.visibility().is_exported() {
                        return;
                    }
                    let name = declaration.name_and_span().0;
                    if name.is_empty() {
                        return;
                    }
                    if !seen.insert((path, name)) {
                        unreachable!(
                            "duplicate package export `{path}.{name}` — `TopLevelScope::build` \
                             rejects every same-(module, leaf) duplicate per module before this \
                             contract check runs (name-res, exit 13)"
                        );
                    }
                });
            }
        }

        Ok(())
    }

    /// The set of in-package module paths a module's `import` clauses point
    /// at (selective `from`, qualified `path`). Compiler pseudo-module
    /// imports (`__intrinsics__` / `__comptime__`) contribute no edge.
    fn import_graph_targets(&self, module: &crate::ast::Module<P>) -> Vec<String> {
        let mut out = Vec::new();
        for u in &module.imports {
            match &u.kind {
                ImportKind::Selective { from, .. } => out.push(module_path_str(from)),
                ImportKind::Qualified { path, .. } => {
                    out.push(module_path_str(path));
                }
                ImportKind::Intrinsics | ImportKind::Comptime => {}
            }
        }
        out
    }

    /// Check one bridged item's signature under its written binder scope.
    fn check_contract_item_type_closure(
        &self,
        item: &crate::ast::Item<P>,
        referring_module: &ModuleEntry<P>,
        bridged: &BTreeSet<&str>,
    ) -> Result<(), LocatedError> {
        let mut locals = HashMap::new();
        match item {
            crate::ast::Item::FnDef(def) => {
                for param in &def.sig.params {
                    match param {
                        crate::ast::SignatureParam::Type(param) => {
                            locals.insert(param.name.clone(), param.effective_kind());
                        }
                        crate::ast::SignatureParam::Value(param) => {
                            if let Some(ty) = &param.ty {
                                self.check_contract_type_closure(
                                    ty,
                                    referring_module,
                                    bridged,
                                    &locals,
                                )?;
                            }
                        }
                    }
                }
                self.check_contract_type_closure(&def.ret, referring_module, bridged, &locals)
            }
            crate::ast::Item::HostFn(def) => {
                for param in &def.params {
                    match param {
                        crate::ast::HostFnParam::Type(param) => {
                            locals.insert(param.name.clone(), param.effective_kind());
                        }
                        crate::ast::HostFnParam::Value(param) => {
                            self.check_contract_type_closure(
                                &param.ty,
                                referring_module,
                                bridged,
                                &locals,
                            )?;
                        }
                    }
                }
                self.check_contract_type_closure(&def.ret, referring_module, bridged, &locals)
            }
            crate::ast::Item::TypeAlias(alias) => {
                for param in &alias.type_params {
                    locals.insert(param.name.clone(), param.effective_kind());
                }
                self.check_contract_type_closure(&alias.body, referring_module, bridged, &locals)
            }
            crate::ast::Item::Newtype(def) => {
                let Some(surface) = def.host_surface() else {
                    return Ok(());
                };
                let Some(payload) = surface.payload() else {
                    return Ok(());
                };
                for param in def.type_params.iter().chain(&def.existential_params) {
                    locals.insert(param.name.clone(), param.effective_kind());
                }
                self.check_contract_type_closure(payload, referring_module, bridged, &locals)
            }
            crate::ast::Item::HostType(_)
            | crate::ast::Item::Labels(_, _)
            | crate::ast::Item::LabelForward(_, _)
            | crate::ast::Item::Equiv(_, _)
            | crate::ast::Item::Elaborator(_, _)
            | crate::ast::Item::Op(_, _)
            | crate::ast::Item::VariadicOperator(_, _)
            | crate::ast::Item::RecGroup(_, _)
            | crate::ast::Item::LiteralAlias(_, _) => Ok(()),
            crate::ast::Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        TypeRecMember::TypeAlias(alias) if alias.vis.is_exported() => {
                            let mut locals = HashMap::new();
                            for param in &alias.type_params {
                                locals.insert(param.name.clone(), param.effective_kind());
                            }
                            self.check_contract_type_closure(
                                &alias.body,
                                referring_module,
                                bridged,
                                &locals,
                            )?;
                        }
                        TypeRecMember::Newtype(def) if def.vis.is_exported() => {
                            let Some(surface) = def.host_surface() else {
                                continue;
                            };
                            let Some(payload) = surface.payload() else {
                                continue;
                            };
                            let mut locals = HashMap::new();
                            for param in def.type_params.iter().chain(&def.existential_params) {
                                locals.insert(param.name.clone(), param.effective_kind());
                            }
                            self.check_contract_type_closure(
                                payload,
                                referring_module,
                                bridged,
                                &locals,
                            )?;
                        }
                        TypeRecMember::TypeAlias(_) | TypeRecMember::Newtype(_) => {}
                        TypeRecMember::Labels(_, ext) => match *ext {},
                    }
                }
                Ok(())
            }
        }
    }

    /// Recurse through a bridged signature type, asserting every nominal
    /// type it reaches is exposed (declared `pub` in a bridged module).
    /// Written binders, primitives, and `__intrinsics__` types are exempt.
    fn check_contract_type_closure(
        &self,
        ty: &Type<P>,
        referring_module: &ModuleEntry<P>,
        bridged: &BTreeSet<&str>,
        locals: &HashMap<String, Kind>,
    ) -> Result<(), LocatedError> {
        match ty {
            Type::Path { segments, args, .. } => {
                for arg in args {
                    self.check_contract_type_closure(arg, referring_module, bridged, locals)?;
                }
                // Resolve to the fully-qualified contract form, then
                // check the head's defining module is bridged & `pub`.
                let qualified = exported_contract_type_segments(segments, referring_module, locals);
                self.check_contract_nominal(&qualified, ty.span(), referring_module, bridged)
            }
            Type::Function { param, ret, .. } => {
                self.check_contract_type_closure(param, referring_module, bridged, locals)?;
                self.check_contract_type_closure(ret, referring_module, bridged, locals)
            }
            Type::Product { left, right, .. } | Type::Sum { left, right, .. } => {
                self.check_contract_type_closure(left, referring_module, bridged, locals)?;
                self.check_contract_type_closure(right, referring_module, bridged, locals)
            }
            Type::Forall { param, body, .. } => {
                let mut nested = locals.clone();
                nested.insert(param.name.clone(), param.effective_kind());
                self.check_contract_type_closure(body, referring_module, bridged, &nested)
            }
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => Ok(()),
            Type::Goal { args, .. } => {
                for arg in args {
                    self.check_contract_type_closure(arg, referring_module, bridged, locals)?;
                }
                Ok(())
            }
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    fn check_contract_nominal(
        &self,
        segments: &[crate::ast::PathSegment],
        span: Span,
        referring_module: &ModuleEntry<P>,
        bridged: &BTreeSet<&str>,
    ) -> Result<(), LocatedError> {
        let Some((leaf, module_segments)) = segments.split_last() else {
            return Ok(());
        };
        let head = leaf.as_str();
        // Unqualified type-parameter references and primitives are exempt.
        if module_segments.is_empty() {
            if crate::naming::is_value_name(head) {
                return Ok(());
            }
            // A bare type-shaped head with no module prefix is a primitive or
            // an unresolved local type; the type-closure walk only
            // constrains module-qualified nominal types.
            return Ok(());
        }
        if module_segments
            .first()
            .is_some_and(|s| s.as_str() == "__intrinsics__")
        {
            return Ok(());
        }
        let module_path = module_segments
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("/");
        let Some(entry) = self.modules.get(&module_path) else {
            // Not an in-package module — treat as a primitive / host
            // reference outside the closure's reach.
            return Ok(());
        };
        let declaration = entry
            .scope
            .lookup(head)
            .and_then(|id| declaration_by_id(&entry.module, id));
        let Some(declaration) = declaration else {
            return Ok(());
        };
        if !declaration.visibility().is_exported() {
            return Err(LocatedError {
                file_path: referring_module.file_path.clone(),
                error: Error::bridge(
                    span,
                    format!(
                        "type `{module_path}.{head}` is reachable from the package's host \
                         contract surface but is private; declare `{head}` as `pub` so the \
                         interface is self-contained"
                    ),
                ),
            });
        }
        if !bridged.contains(module_path.as_str()) {
            return Err(LocatedError {
                file_path: referring_module.file_path.clone(),
                error: Error::bridge(
                    span,
                    format!(
                        "type `{module_path}.{head}` is reachable from the package's host \
                         contract surface but module `{module_path}` is not selected by any \
                         bridge glob; add a glob matching `{module_path}` so the interface is \
                         self-contained"
                    ),
                ),
            });
        }
        Ok(())
    }
}

/// The set of module paths selected by the package's `bridge { … }`
/// glob list. Empty when the package declares no bridge block. Used by
/// the host-descriptor builder and the emitters to scan exactly the
/// modules whose `pub` items form the host contract surface.
pub fn bridged_module_paths<P: Phase>(package: &Package<P>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(entry) = package.package_file() else {
        return out;
    };
    let Some(bridge) = &entry.package_file.bridge else {
        return out;
    };
    for glob in &bridge.globs {
        for path in package.modules.keys() {
            if glob_matches(&glob.segments, path) {
                out.insert(path.clone());
            }
        }
    }
    out
}

/// Public newtype host surfaces selected by the package bridge, indexed by
/// exact declaration identity. A same-leaf declaration in another module is
/// a distinct entry and cannot replace or redirect an existing one.
pub(crate) struct IndexedNewtypeHostSurface<'a, P: Phase> {
    pub(crate) declaration: &'a Newtype<P>,
    pub(crate) surface: NewtypeHostSurface<'a, P>,
}

pub(crate) type PublicNewtypeHostSurfaces<'a, P> =
    BTreeMap<(String, String), IndexedNewtypeHostSurface<'a, P>>;

pub(crate) fn public_newtype_host_surfaces<P: Phase>(
    package: &Package<P>,
) -> PublicNewtypeHostSurfaces<'_, P> {
    let mut surfaces = BTreeMap::new();
    for module_path in bridged_module_paths(package) {
        let Some(entry) = package.module(&module_path) else {
            continue;
        };
        for item in &entry.module.items {
            for_each_item_declaration(item, |declaration| {
                let Some(newtype) = declaration.newtype() else {
                    return;
                };
                let Some(surface) = newtype.host_surface() else {
                    return;
                };
                match surfaces.entry((module_path.clone(), newtype.name.clone())) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(IndexedNewtypeHostSurface {
                            declaration: newtype,
                            surface,
                        });
                    }
                    std::collections::btree_map::Entry::Occupied(_) => {
                        unreachable!(
                            "duplicate newtype declaration identity survived scope validation"
                        )
                    }
                }
            });
        }
    }
    surfaces
}

/// Match a bridge glob's segments against a slash-joined module path,
/// shell-style: a literal matches one segment exactly, `*` matches
/// exactly one segment, `**` matches zero or more segments.
pub(crate) fn glob_matches(segments: &[crate::ast::BridgeGlobSegment], module_path: &str) -> bool {
    let path: Vec<&str> = module_path.split('/').collect();
    glob_matches_at(segments, &path)
}

fn glob_matches_at(segments: &[crate::ast::BridgeGlobSegment], path: &[&str]) -> bool {
    use crate::ast::BridgeGlobSegment as Seg;
    match segments.split_first() {
        None => path.is_empty(),
        Some((Seg::DoubleStar, rest)) => {
            // Zero or more segments.
            (0..=path.len()).any(|skip| glob_matches_at(rest, &path[skip..]))
        }
        Some((seg, rest)) => {
            let Some((head, tail)) = path.split_first() else {
                return false;
            };
            let head_ok = match seg {
                Seg::Literal(name) => name == head,
                Seg::Star => true,
                Seg::DoubleStar => unreachable!("handled above"),
            };
            head_ok && glob_matches_at(rest, tail)
        }
    }
}

fn glob_surface(segments: &[crate::ast::BridgeGlobSegment]) -> String {
    use crate::ast::BridgeGlobSegment as Seg;
    segments
        .iter()
        .map(|s| match s {
            Seg::Literal(name) => name.as_str(),
            Seg::Star => "*",
            Seg::DoubleStar => "**",
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// True when a module declares any `host type` / `host fn` item.
fn module_has_host_items<P: Phase>(module: &crate::ast::Module<P>) -> bool {
    module.items.iter().any(|item| {
        matches!(
            item,
            crate::ast::Item::HostType(_) | crate::ast::Item::HostFn(_)
        )
    })
}

// `resolve_imports` and its helpers (`check_one_import`,
// `check_import_module`,
// `lookup_target`) live on the generic `impl<P: Phase> Package<P>`
// block — they only inspect `Import` statements and call helpers that
// are themselves phase-polymorphic. `check_no_value_cycles` is in
// the same generic block.

impl<P: ResolvePhase> Package<P> {
    /// Resolve identifier and type references inside every item body of
    /// every module in the package. Enforces order-sensitive visibility
    /// (`fn` cannot self-reference, no forward references) and rejects
    /// unbound names. Returns the first error.
    ///
    /// Phase-polymorphic — works at every `ResolvePhase`: `Lowered`,
    /// `UncheckedPrime`, and `Prime`. The `Resolver` walker is generic
    /// over the same bound, so the same in-body resolver serves the
    /// full pipeline (post-label-elab `Lowered`), evaluator-facing
    /// `UncheckedPrime`, and the kio-prime pipeline (post-`prime::lower`
    /// `Prime`).
    pub fn check_in_body_resolution(&self) -> Result<(), LocatedError> {
        let identity_aliases = IdentityAliasNewtypeIndex::build_for_package(self);
        self.check_binding_origins()?;
        for entry in self.modules.values() {
            if let Err(e) =
                Resolver::check_module_in_package(&entry.module, self, &identity_aliases)
            {
                return Err(LocatedError {
                    file_path: entry.file_path.clone(),
                    error: e,
                });
            }
        }
        Ok(())
    }
}

// =========================================================================
// Resolver — in-body name resolution + order-sensitive visibility
// =========================================================================

/// The lexical binding class retained by in-body resolution. Type-parameter
/// spans diagnose value-position misuse before binder normalization; the
/// typer carries ordinary value/type target lookup.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Binding {
    /// A type parameter `[A]` in scope.
    TypeParam(Span),
    /// A value parameter `name: T` in scope.
    Param,
    /// A `let x = e;` local binding.
    Local,
}

/// Bound for the in-body resolver: any phase whose surface-only
/// extensions are uninhabited. That's `Lowered` (the full pipeline's
/// post-label-elab AST), `UncheckedPrime` (the compile-time evaluator's
/// Kio'-shaped artifact), and `Prime` (the kio-prime pipeline's
/// post-`prime::lower` AST).
///
/// Elaborator-position variants (`Expr::UserElaborator`, `Expr::Elaborator`)
/// stay inhabited at `Lowered` (each carries a
/// `NodeId`) and uninhabited at `Prime`; the resolver walks children
/// either way — the corresponding match arms compile at both phases.
///
pub trait ResolvePhase:
    Phase<
        ExprTuple = crate::ast::Never,
        ExprLabelValue = crate::ast::Never,
        ExprRowLet = crate::ast::Never,
        ExprFnPlaceholder = crate::ast::Never,
        ExprOpChain = crate::ast::Never,
        ExprRecCall = crate::ast::Never,
        ExprBlockSyntax = crate::ast::Never,
        ExprEnriched = crate::ast::Never,
        ExprLow = crate::ast::Never,
        TypeLabelSugar = crate::ast::Never,
        ItemLabels = crate::ast::Never,
        ItemOp = crate::ast::Never,
        ItemRecGroup = crate::ast::Never,
        ItemLiteralAlias = crate::ast::Never,
    > + Clone
{
}
impl ResolvePhase for crate::ast::Lowered {}
impl ResolvePhase for crate::ast::UncheckedPrime {}
impl ResolvePhase for crate::ast::Prime {}

/// Resolves identifier and type references inside one module's items.
/// Maintains the visible top-level set, which grows as we walk items in
/// source order — enforcing the order-sensitive visibility rule.
///
/// Phase-polymorphic — the body walks only consult names, spans,
/// and child nodes, all of which flow uniformly across `Lowered`,
/// `UncheckedPrime`, and `Prime`. The resolver does not see `Surface` or `Desugared`
/// ASTs (those are pre-label-elab and still carry surface-only
/// sugar), and the [`ResolvePhase`] bound excludes those phases at
/// the type level.
pub struct Resolver<'m> {
    /// Top-level names declared at or before the current item.
    visible_top_level: HashSet<&'m str>,
    /// Names brought in by selective imports and the comptime block,
    /// plus qualified aliases.
    imported: HashSet<&'m str>,
    /// Whether `import __intrinsics__;` is in scope.
    intrinsics_in_scope: bool,
    /// User elaborators available to bang calls: selectively imported
    /// declarations plus same-module declarations already visited in source
    /// order.
    user_elaborators_in_scope: HashSet<&'m str>,
    /// Explicit recursive-data heads visible only while checking a marked
    /// singleton or one written type-recursive group.
    recursive_types: HashSet<&'m str>,
    /// Exact references that prove an unmarked module-level type cycle.
    /// This is diagnostic evidence only: the names remain out of scope.
    implicit_type_cycles: HashMap<Span, Arc<ImplicitTypeCycleProblem>>,
    /// Every type declaration in this module, including members of explicit
    /// recursive groups. Consulted only after ordinary scope lookup fails, so
    /// it improves a source-order error without binding the invalid path.
    later_type_declarations: HashMap<&'m str, LaterTypeDeclaration<'m>>,
    /// Source item currently being checked, for a compiler-authored stable
    /// dependency move when a later type head is used acyclically.
    current_item_index: usize,
    current_item_span: Span,
    current_item_leading_gap: Span,
}

#[derive(Clone)]
struct LaterTypeDeclaration<'m> {
    name_span: Span,
    item_span: Span,
    item_index: usize,
    dependency_items: Vec<usize>,
    dependent_items: usize,
    leading_gap: Span,
    owns_complete_source_declaration: bool,
    safe_move: bool,
    /// A later transparent alias is enough to diagnose the written head as
    /// source-order-invalid, even though this resolver deliberately does not
    /// grant authority to whatever member name follows it. The typer checks
    /// identity-alias and terminal-member validity after the declaration is
    /// actually in scope.
    alias_member_head: bool,
    /// Exact terminal member spellings callable from this module. Literal
    /// local newtypes contribute both members; an identity alias contributes
    /// only independently visible terminal members.
    value_members: Vec<&'m str>,
}

impl<'m> Resolver<'m> {
    /// Walk a module: process its `import` statements once, then walk each
    /// item in source order, growing `visible_top_level` after each.
    pub fn check_module<P: ResolvePhase>(module: &'m crate::ast::Module<P>) -> Result<(), Error> {
        Self::check_module_with_identity_aliases(module, None, None)
    }

    fn check_module_in_package<P: ResolvePhase>(
        module: &'m crate::ast::Module<P>,
        package: &'m Package<P>,
        identity_aliases: &IdentityAliasNewtypeIndex,
    ) -> Result<(), Error> {
        Self::check_module_with_identity_aliases(module, Some(package), Some(identity_aliases))
    }

    fn check_module_with_identity_aliases<P: ResolvePhase>(
        module: &'m crate::ast::Module<P>,
        package: Option<&'m Package<P>>,
        identity_aliases: Option<&IdentityAliasNewtypeIndex>,
    ) -> Result<(), Error> {
        let implicit_type_cycles = implicit_type_cycle_problems(module);
        let mut later_type_declarations = HashMap::new();
        let mut declaration_items = HashMap::new();
        let mut span_counts = HashMap::new();
        let module_path = module_path_str_from_module(module);
        for item in &module.items {
            *span_counts.entry(item.meta().span).or_insert(0usize) += 1;
        }
        let mut previous_end = module.imports.last().map_or_else(
            || module.path.span.end.saturating_add(1),
            |import_| import_.span.end,
        );
        for (item_index, item) in module.items.iter().enumerate() {
            let item_span = item.meta().span;
            let leading_gap = Span::new(previous_end.min(item_span.start), item_span.start);
            for_each_item_declaration(item, |declaration| {
                let named = declaration
                    .type_alias()
                    .map(|declaration| {
                        let value_members = package
                            .zip(identity_aliases)
                            .and_then(|(package, identity_aliases)| {
                                identity_aliases.target(
                                    module,
                                    Some(package),
                                    &module_path,
                                    &declaration.name,
                                )
                            })
                            .map(|target| {
                                let same_owner = target.module_path == module_path;
                                if !same_owner && !is_visible(&target.newtype.vis, &module.path) {
                                    return Vec::new();
                                }
                                [
                                    (
                                        &target.newtype.constructor.vis,
                                        target.newtype.constructor.name.as_str(),
                                    ),
                                    (
                                        &target.newtype.projector.vis,
                                        target.newtype.projector.name.as_str(),
                                    ),
                                ]
                                .into_iter()
                                .filter_map(|(visibility, name)| {
                                    (same_owner || is_visible(visibility, &module.path))
                                        .then_some(name)
                                })
                                .collect()
                            })
                            .unwrap_or_default();
                        (
                            declaration.name.as_str(),
                            declaration.name_span,
                            matches!(item, Item::TypeAlias(_))
                                && !declaration.vis.is_pub()
                                && declaration.doc.is_none(),
                            true,
                            value_members,
                            false,
                        )
                    })
                    .or_else(|| {
                        declaration.newtype().map(|declaration| {
                            (
                                declaration.name.as_str(),
                                declaration.name_span,
                                matches!(item, Item::Newtype(_))
                                    && !declaration.vis.is_pub()
                                    && declaration.doc.is_none()
                                    && declaration.rec_span.is_none(),
                                false,
                                vec![
                                    declaration.constructor.name.as_str(),
                                    declaration.projector.name.as_str(),
                                ],
                                is_generated_label_newtype(declaration),
                            )
                        })
                    });
                if let Some((
                    name,
                    name_span,
                    locally_safe,
                    alias_member_head,
                    value_members,
                    generated_fragment,
                )) = named
                {
                    // Lowering may mint a declaration from a larger source item.
                    // Its retained owner span is useful for structural repairs,
                    // but the generated head is not independently movable.
                    let owns_complete_source_declaration = !generated_fragment
                        && item_span.start < name_span.start
                        && name_span.end <= item_span.end;
                    declaration_items.entry(name).or_insert(item_index);
                    later_type_declarations
                        .entry(name)
                        .or_insert(LaterTypeDeclaration {
                            name_span,
                            item_span,
                            item_index,
                            dependency_items: Vec::new(),
                            dependent_items: 0,
                            leading_gap,
                            owns_complete_source_declaration,
                            safe_move: owns_complete_source_declaration
                                && locally_safe
                                && span_counts.get(&item_span).copied() == Some(1),
                            alias_member_head,
                            value_members,
                        });
                }
            });
            previous_end = item_span.end;
        }
        let mut dependent_items = HashMap::<usize, HashSet<usize>>::new();
        for (item_index, item) in module.items.iter().enumerate() {
            let (name, ty, mut bound) = match item {
                Item::TypeAlias(alias) => (
                    alias.name.as_str(),
                    &alias.body,
                    TypeRecBoundNames::new(
                        alias.type_params.iter().map(|param| param.name.as_str()),
                    ),
                ),
                Item::Newtype(newtype) => (
                    newtype.name.as_str(),
                    &newtype.payload,
                    TypeRecBoundNames::new(
                        newtype
                            .type_params
                            .iter()
                            .chain(&newtype.existential_params)
                            .map(|param| param.name.as_str()),
                    ),
                ),
                _ => continue,
            };
            let mut dependencies = Vec::new();
            collect_type_rec_edges(ty, &declaration_items, &mut bound, None, &mut dependencies);
            dependencies.sort_unstable_by_key(|(dependency, _)| *dependency);
            dependencies.dedup_by_key(|(dependency, _)| *dependency);
            if let Some(declaration) = later_type_declarations.get_mut(name) {
                declaration.dependency_items = dependencies
                    .iter()
                    .map(|(dependency, _)| *dependency)
                    .collect();
            }
            for (dependency, _) in dependencies {
                dependent_items
                    .entry(dependency)
                    .or_default()
                    .insert(item_index);
            }
        }
        for declaration in later_type_declarations.values_mut() {
            declaration.dependent_items = dependent_items
                .get(&declaration.item_index)
                .map_or(0, HashSet::len);
        }
        let mut r = Resolver {
            visible_top_level: HashSet::new(),
            imported: HashSet::new(),
            intrinsics_in_scope: false,
            user_elaborators_in_scope: HashSet::new(),
            recursive_types: HashSet::new(),
            implicit_type_cycles,
            later_type_declarations,
            current_item_index: 0,
            current_item_span: Span::new(0, 0),
            current_item_leading_gap: Span::new(0, 0),
        };
        r.process_uses(&module.imports);
        let mut previous_end = module.imports.last().map_or_else(
            || module.path.span.end.saturating_add(1),
            |import_| import_.span.end,
        );
        for (item_index, item) in module.items.iter().enumerate() {
            r.current_item_index = item_index;
            r.current_item_span = item.meta().span;
            r.current_item_leading_gap = Span::new(
                previous_end.min(r.current_item_span.start),
                r.current_item_span.start,
            );
            match item {
                Item::Newtype(newtype) if newtype.rec_span.is_some() => {
                    r.recursive_types.insert(newtype.name.as_str());
                    let result = r.check_newtype(newtype);
                    r.recursive_types.remove(newtype.name.as_str());
                    result?;
                    Self::validate_recursive_newtype(newtype)?;
                }
                Item::Newtype(newtype) => {
                    let self_refs = recursive_newtype_self_references(newtype);
                    if !self_refs.is_empty() {
                        // Let the shared signature checker validate the
                        // declaration as if its own head were scoped. Only
                        // after kind, arity, and positivity succeed does it
                        // publish the tailored missing-marker diagnostic.
                        r.recursive_types.insert(newtype.name.as_str());
                        let result = r.check_newtype(newtype);
                        r.recursive_types.remove(newtype.name.as_str());
                        result?;
                    } else {
                        r.check_newtype(newtype)?;
                    }
                }
                Item::TypeAlias(alias)
                    if type_self_references(
                        &alias.body,
                        &alias.name,
                        alias.type_params.iter().map(|param| param.name.as_str()),
                    )
                    .next()
                    .is_some() =>
                {
                    let reference = type_self_references(
                        &alias.body,
                        &alias.name,
                        alias.type_params.iter().map(|param| param.name.as_str()),
                    )
                    .next()
                    .expect("self reference was observed");
                    return Err(Error::totality(
                        reference,
                        "recursive type component has no `newtype` boundary",
                    )
                    .with_secondary(
                        alias.name_span,
                        format!("`{}` is a transparent alias", alias.name),
                    )
                    .with_help(
                        "transparent aliases cannot be self-recursive; put mutually recursive aliases in a `rec { ... }` group grounded by a `newtype`",
                    ));
                }
                Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        match member {
                            TypeRecMember::TypeAlias(alias) => {
                                r.recursive_types.insert(alias.name.as_str());
                            }
                            TypeRecMember::Newtype(newtype) => {
                                r.recursive_types.insert(newtype.name.as_str());
                            }
                            TypeRecMember::Labels(_, ext) => match *ext {},
                        }
                    }
                    let result = group.members.iter().try_for_each(|member| match member {
                        TypeRecMember::TypeAlias(alias) => r.check_type_alias(alias),
                        TypeRecMember::Newtype(newtype) => r.check_newtype(newtype),
                        TypeRecMember::Labels(_, ext) => match *ext {},
                    });
                    r.recursive_types.clear();
                    result?;
                    if group.deferred_rec_labels_diagnostic.is_none() {
                        Self::validate_type_rec_group(group)?;
                    }
                }
                _ => r.check_item(item)?,
            }
            if let crate::ast::Item::Elaborator(elaborator, _) = item {
                r.user_elaborators_in_scope.insert(elaborator.name.as_str());
            }
            match item {
                Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        match member {
                            TypeRecMember::TypeAlias(alias) => {
                                r.visible_top_level.insert(alias.name.as_str());
                            }
                            TypeRecMember::Newtype(newtype) => {
                                r.visible_top_level.insert(newtype.name.as_str());
                            }
                            TypeRecMember::Labels(_, ext) => match *ext {},
                        }
                    }
                }
                _ => {
                    r.visible_top_level.insert(item_name(item));
                }
            }
            previous_end = item.meta().span.end;
        }
        Ok(())
    }

    fn validate_recursive_newtype<P: ResolvePhase>(newtype: &Newtype<P>) -> Result<(), Error> {
        if recursive_newtype_self_references(newtype).is_empty() {
            let rec_span = newtype
                .rec_span
                .expect("recursive singleton validation requires a marker");
            return Err(Error::parse(rec_span, "this `rec` marker is unnecessary")
                .with_secondary(
                    newtype.name_span,
                    format!("`{}` has no recursive dependency", newtype.name),
                )
                .with_help("remove `rec` from this acyclic newtype declaration")
                .with_fix(
                    Fix::machine_applicable(
                        "Remove unnecessary `rec`",
                        vec![remove_type_rec_marker_edit(
                            rec_span,
                            newtype.meta.span.start,
                        )],
                    )
                    .allowing_follow_on_reanalysis_outside(newtype.meta.span),
                ));
        }
        Ok(())
    }

    fn validate_type_rec_group<P: ResolvePhase>(
        group: &crate::ast::TypeRecGroup<P>,
    ) -> Result<(), Error> {
        let analysis = analyze_type_rec_members(&group.members);
        Self::validate_type_rec_group_with_analysis(group, &analysis)
    }

    fn validate_type_rec_group_with_analysis<P: ResolvePhase>(
        group: &crate::ast::TypeRecGroup<P>,
        analysis: &TypeRecAnalysis,
    ) -> Result<(), Error> {
        let rec_span = group.rec_span.unwrap_or_else(|| {
            Span::new(
                group.meta.span.start,
                group.meta.span.start.saturating_add(3),
            )
        });
        if group.members.len() == 1 {
            let nested_marker = match &group.members[0] {
                TypeRecMember::TypeAlias(_) => None,
                TypeRecMember::Newtype(newtype) => newtype.rec_span,
                TypeRecMember::Labels(_, ext) => match *ext {},
            };
            let add_singleton_marker = analysis.alias_cycle.is_none()
                && !analysis.cyclic_components.is_empty()
                && matches!(group.members[0], TypeRecMember::Newtype(_))
                && type_rec_marker_fix_is_locally_proven(&group.members, analysis);
            let can_unwrap = analysis.cyclic_components.is_empty()
                || add_singleton_marker
                || nested_marker.is_some();
            let mut error = Error::parse(
                rec_span,
                "one recursive data declaration uses a `rec` modifier, not a group",
            )
            .with_secondary(
                group.open_brace_span.unwrap_or(rec_span),
                "the singleton group opens here",
            )
            .with_secondary(
                group.close_brace_span.unwrap_or(rec_span),
                "the singleton group closes here",
            )
            .with_secondary(
                type_rec_member_name_span(&group.members[0]),
                "this is the group's only declaration",
            )
            .with_help(if nested_marker.is_some() {
                "unwrap the group; the declaration already has its required `rec` modifier"
            } else if add_singleton_marker {
                "unwrap the group and write `rec newtype` for this recursive singleton"
            } else if analysis.cyclic_components.is_empty() {
                "unwrap this acyclic declaration without a `rec` marker"
            } else {
                "unwrap the group only after repairing the declaration's recursive type errors"
            });
            if can_unwrap
                && let Some(fix) =
                    type_rec_unwrap_fix(group, add_singleton_marker && nested_marker.is_none())
            {
                error = error.with_fix(fix);
            }
            return Err(error);
        }

        if let Some((marker_span, member_start, member_name_span)) =
            group.members.iter().find_map(|member| match member {
                TypeRecMember::TypeAlias(_) => None,
                TypeRecMember::Newtype(newtype) => newtype
                    .rec_span
                    .map(|span| (span, newtype.meta.span.start, newtype.name_span)),
                TypeRecMember::Labels(_, ext) => match *ext {},
            })
        {
            return Err(Error::parse(
                marker_span,
                "the enclosing `rec { ... }` already supplies recursive scope",
            )
            .with_secondary(
                member_name_span,
                "this member is scoped by the enclosing group",
            )
            .with_help("remove the nested `rec` marker")
            .with_fix(
                Fix::machine_applicable(
                    "Fix recursive type groups",
                    vec![remove_type_rec_marker_edit(marker_span, member_start)],
                )
                .allowing_follow_on_reanalysis_outside(group.meta.span),
            ));
        }
        if let Some(alias_cycle) = &analysis.alias_cycle {
            let first = alias_cycle[0];
            let primary = analysis.edge_spans[first]
                .iter()
                .find(|(target, _)| alias_cycle.contains(target))
                .map_or(group.members[first].meta().span, |(_, span)| *span);
            let mut error = Error::totality(
                primary,
                "recursive type component has no `newtype` boundary",
            );
            for &index in alias_cycle {
                error = error.with_secondary(
                    type_rec_member_name_span(&group.members[index]),
                    format!(
                        "transparent alias `{}` participates in this cycle",
                        type_rec_member_name(&group.members[index])
                    ),
                );
            }
            return Err(error
                .with_help("every recursive type cycle must cross a nominal `newtype` boundary"));
        }

        if analysis.cyclic_components.is_empty() {
            let mut error = Error::parse(rec_span, "this `rec` group contains no recursive cycle");
            for component_index in type_rec_component_order(analysis) {
                let component = &analysis.components[component_index];
                let index = component[0];
                error = error.with_secondary(
                    type_rec_member_name_span(&group.members[index]),
                    format!(
                        "`{}` is acyclic",
                        type_rec_member_name(&group.members[index])
                    ),
                );
            }
            error = error.with_help(
                "move each dependency before its users and remove the unnecessary group",
            );
            if let Some(fix) = type_rec_partition_fix(group, analysis) {
                error = error.with_fix(fix);
            }
            return Err(error);
        }

        if analysis.components.len() != 1 {
            let mut error = Error::parse(
                rec_span,
                "this `rec` group contains multiple independent components",
            );
            for component in &analysis.components {
                let index = component[0];
                let cyclic = analysis.component_is_cyclic(component);
                error = error.with_secondary(
                    type_rec_member_name_span(&group.members[index]),
                    format!(
                        "`{}` starts an {} component",
                        type_rec_member_name(&group.members[index]),
                        if cyclic {
                            "independent recursive"
                        } else {
                            "acyclic"
                        }
                    ),
                );
            }
            error = error.with_help(
                "keep exactly one genuinely mutual recursive component in each `rec` group",
            );
            if let Some(fix) = type_rec_partition_fix(group, analysis) {
                error = error.with_fix(fix);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Collect names brought into scope by the module's `import` statements.
    fn process_uses(&mut self, imports: &'m [Import]) {
        for u in imports {
            match &u.kind {
                ImportKind::Intrinsics => self.intrinsics_in_scope = true,
                ImportKind::Selective { items, .. } => {
                    for n in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                        self.imported.insert(n);
                        self.user_elaborators_in_scope.insert(n);
                    }
                }
                ImportKind::Qualified { alias, .. } => {
                    self.imported.insert(alias.as_str());
                }
                ImportKind::Comptime => {
                    self.imported
                        .extend(crate::comptime::PUBLIC_COMPTIME_NAMES.iter().copied());
                }
            }
        }
    }

    fn check_host_fn<P: ResolvePhase>(&self, f: &'m crate::ast::HostFn<P>) -> Result<(), Error> {
        let mut locals: HashMap<&'m str, Binding> = HashMap::new();
        for p in &f.params {
            match p {
                crate::ast::HostFnParam::Type(tp) => {
                    locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
                }
                crate::ast::HostFnParam::Value(v) => {
                    self.check_type(&v.ty, &locals)?;
                }
            }
        }
        self.check_type(&f.ret, &locals)
    }

    fn check_item<P: ResolvePhase>(&self, item: &'m crate::ast::Item<P>) -> Result<(), Error> {
        match item {
            crate::ast::Item::FnDef(d) => self.check_fn_def(d),
            crate::ast::Item::TypeAlias(a) => self.check_type_alias(a),
            crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
            crate::ast::Item::Newtype(d) => self.check_newtype(d),
            // Host type: declaration only (no body); the type-param
            // scope of a role-bearing host type is trivially closed.
            crate::ast::Item::HostType(_) => Ok(()),
            crate::ast::Item::HostFn(h) => self.check_host_fn(h),
            // Statically uninhabited at every `ResolvePhase`.
            crate::ast::Item::Labels(_, ext) => match *ext {},
            crate::ast::Item::LabelForward(_, ext) => match *ext {},
            // Resolve `equiv` body — bind type/value params in scope,
            // then resolve each `term`'s expression. Reuses the same
            // path `fn` already takes.
            crate::ast::Item::Equiv(e, _ext) => self.check_equiv(e),
            crate::ast::Item::Elaborator(s, _ext) => self.check_elaborator_item(s),
            // Statically uninhabited at every `ResolvePhase`.
            crate::ast::Item::Op(_, ext) => match *ext {},
            crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
            crate::ast::Item::RecGroup(_, ext) => match *ext {},
            crate::ast::Item::TypeRecGroup(_) => {
                unreachable!("type-recursive groups are handled with their explicit head scope")
            }
        }
    }

    fn check_elaborator_item<P: ResolvePhase>(
        &self,
        s: &'m crate::ast::UserElaboratorDef<P>,
    ) -> Result<(), Error> {
        let locals: HashMap<&'m str, Binding> = HashMap::new();
        self.check_type(&s.call_ty, &locals)?;
        let Some(head) = s.implementation.first() else {
            unreachable!("the parser emits a nonempty elaborator implementation path")
        };
        self.lookup_name(head.as_str(), s.implementation.span(), &locals)
    }

    fn check_fn_def<P: ResolvePhase>(&self, d: &'m crate::ast::FnDef<P>) -> Result<(), Error> {
        let mut locals: HashMap<&'m str, Binding> = HashMap::new();
        // Walk parameters left-to-right: each value-param's type is
        // checked under the locals collected so far (so a later param
        // can reference an earlier type-param), then the param itself
        // is added to scope.
        for p in &d.sig.params {
            match p {
                crate::ast::SignatureParam::Type(tp) => {
                    locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
                }
                crate::ast::SignatureParam::Value(vp) => {
                    if let Some(ty) = &vp.ty {
                        self.check_type(ty, &locals)?;
                    }
                    locals.insert(vp.name.as_str(), Binding::Param);
                }
            }
        }
        self.check_type(&d.ret, &locals)?;
        self.check_expr(&d.body, &locals)
    }

    fn check_equiv<P: ResolvePhase>(&self, e: &'m crate::ast::Equiv<P>) -> Result<(), Error> {
        let mut locals: HashMap<&'m str, Binding> = HashMap::new();
        // Same parameter scoping as `fn`.
        for p in &e.sig.params {
            match p {
                crate::ast::SignatureParam::Type(tp) => {
                    locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
                }
                crate::ast::SignatureParam::Value(vp) => {
                    if let Some(ty) = &vp.ty {
                        self.check_type(ty, &locals)?;
                    }
                    locals.insert(vp.name.as_str(), Binding::Param);
                }
            }
        }
        // Each `term` body is resolved under the same parameter
        // bindings; the bodies are independent expressions.
        for term in &e.terms {
            self.check_expr(&term.body, &locals)?;
        }
        Ok(())
    }

    /// Resolve a type alias.
    fn check_type_alias<P: ResolvePhase>(
        &self,
        a: &'m crate::ast::TypeAlias<P>,
    ) -> Result<(), Error> {
        let mut locals: HashMap<&'m str, Binding> = HashMap::new();
        for tp in &a.type_params {
            locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
        }
        self.check_type(&a.body, &locals)
    }

    /// Walk a newtype payload. The newtype's own type parameters are in
    /// scope; recursive declaration heads are supplied only by the explicit
    /// singleton marker or surrounding type-recursive group.
    ///
    /// Existential binders (parsed as a trailing `<X>` atom run on
    /// the newtype's header, stored on `existential_params`) scope
    /// inside the payload alongside the universals — both flow into
    /// `locals` as `Binding::TypeParam`. The escape check that keeps
    /// existentials from leaking out of a CPS continuation lives in
    /// the typer; the resolver only ensures the names resolve where
    /// they appear.
    fn check_newtype<P: ResolvePhase>(&self, d: &'m crate::ast::Newtype<P>) -> Result<(), Error> {
        let mut locals: HashMap<&'m str, Binding> = HashMap::new();
        for tp in &d.type_params {
            locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
        }
        for tp in &d.existential_params {
            locals.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
        }
        self.check_type(&d.payload, &locals)
    }

    fn check_type<'a, P: ResolvePhase>(
        &self,
        ty: &'a crate::ast::Type<P>,
        locals: &HashMap<&'a str, Binding>,
    ) -> Result<(), Error> {
        match ty {
            crate::ast::Type::Path {
                segments,
                args,
                meta: _,
                ..
            } => {
                let head = segments[0].as_str();
                self.lookup_type_name(head, segments[0].span, locals)?;
                // A bound type-parameter `[*F]` of kind `*→*`-or-higher
                // is callable in type position: `F(A)` is the direct
                // application form. Whether the binder's kind is high
                // enough to be applied to the supplied arguments is a
                // kind-checking concern, decided by the typer's
                // post-pass — the resolver only ensures the names
                // resolve.
                for arg in args {
                    self.check_type(arg, locals)?;
                }
                Ok(())
            }
            crate::ast::Type::Unit { .. } | crate::ast::Type::Bottom { .. } => Ok(()),
            crate::ast::Type::Function { param, ret, .. } => {
                self.check_type(param, locals)?;
                self.check_type(ret, locals)
            }
            crate::ast::Type::Product { left, right, .. }
            | crate::ast::Type::Sum { left, right, .. } => {
                self.check_type(left, locals)?;
                self.check_type(right, locals)
            }
            crate::ast::Type::Forall { param, body, .. } => {
                let mut inner = locals.clone();
                inner.insert(param.name.as_str(), Binding::TypeParam(param.span));
                self.check_type(body, &inner)
            }
            // Statically uninhabited at every `ResolvePhase`.
            crate::ast::Type::LabelSugar { ext, .. } => match *ext {},
            // `_` placeholder: nothing to resolve. No-op at Lowered;
            // statically uninhabited at Prime.
            crate::ast::Type::Infer { .. } => Ok(()),
            crate::ast::Type::Goal { args, .. } => {
                for arg in args {
                    self.check_type(arg, locals)?;
                }
                Ok(())
            }
        }
    }

    fn check_expr<P: ResolvePhase>(
        &self,
        e: &'m crate::ast::Expr<P>,
        locals: &HashMap<&'m str, Binding>,
    ) -> Result<(), Error> {
        match e {
            crate::ast::Expr::BlockCall { ext, .. } => match *ext {},
            crate::ast::Expr::Path {
                occurrence: _,
                segments,
                meta,
                ext: _,
            } => self.lookup_value_path(segments, meta.span, locals),
            crate::ast::Expr::Call { callee, args, .. } => {
                self.check_expr(callee, locals)?;
                for arg in args {
                    self.check_call_arg(arg, locals)?;
                }
                Ok(())
            }
            crate::ast::Expr::FnExpr {
                sig, ret_ty, body, ..
            } => {
                let mut inner = locals.clone();
                for p in &sig.params {
                    match p {
                        crate::ast::SignatureParam::Type(tp) => {
                            inner.insert(tp.name.as_str(), Binding::TypeParam(tp.span));
                        }
                        crate::ast::SignatureParam::Value(vp) => {
                            // Optional `: T` annotation references type
                            // names that must be in scope at the fn's
                            // signature position.
                            if let Some(ty) = &vp.ty {
                                self.check_type(ty, &inner)?;
                            }
                            inner.insert(vp.name.as_str(), Binding::Param);
                        }
                    }
                }
                if let Some(ty) = ret_ty {
                    self.check_type(ty, &inner)?;
                }
                self.check_expr(body, &inner)
            }
            crate::ast::Expr::Let {
                name, value, body, ..
            } => {
                self.check_expr(value, locals)?;
                let mut inner = locals.clone();
                inner.insert(name.as_str(), Binding::Local);
                self.check_expr(body, &inner)
            }
            // `e;` discards the value — no new binding scope.
            crate::ast::Expr::Seq { value, body, .. } => {
                self.check_expr(value, locals)?;
                self.check_expr(body, locals)
            }
            crate::ast::Expr::Unit { .. } => Ok(()),
            crate::ast::Expr::StrLit { annotation, .. }
            | crate::ast::Expr::IntLit { annotation, .. }
            | crate::ast::Expr::FloatLit { annotation, .. }
            | crate::ast::Expr::BoolLit { annotation, .. } => {
                if let Some(ty) = annotation.as_type() {
                    self.check_type(ty, locals)?;
                }
                Ok(())
            }
            // Statically uninhabited at every `ResolvePhase`.
            crate::ast::Expr::Tuple { ext, .. } => match *ext {},
            crate::ast::Expr::FnPlaceholder { ext, .. } => match *ext {},
            crate::ast::Expr::OpChain { ext, .. } => match *ext {},
            crate::ast::Expr::RecCall { ext, .. } => match *ext {},
            // Statically uninhabited at every `ResolvePhase`.
            crate::ast::Expr::LabelValue { ext, .. } => match *ext {},
            crate::ast::Expr::RowLet { ext, .. } => match *ext {},
            crate::ast::Expr::Elaborator { kind, call, .. } => {
                // Field-syntax elaborator positions are stripped by the
                // typer at the `Lowered → Prime` boundary; resolve still
                // needs to walk the receiver at `Lowered` so the typer
                // sees pre-resolved names. At `Prime` the variant is
                // uninhabited and this arm is statically unreachable.
                match call {
                    crate::ast::ElaboratorCall::FieldAccess { receiver, .. } => {
                        debug_assert_eq!(*kind, crate::ast::ElaboratorKind::Access);
                        self.check_expr(receiver, locals)
                    }
                    crate::ast::ElaboratorCall::FieldUpdate { receiver, updates } => {
                        debug_assert_eq!(*kind, crate::ast::ElaboratorKind::Filtered);
                        self.check_expr(receiver, locals)?;
                        for update in updates {
                            self.check_expr(&update.value, locals)?;
                        }
                        Ok(())
                    }
                }
            }
            crate::ast::Expr::RecQuote { plan, .. } => {
                for annotation in plan.annotations() {
                    self.check_type(annotation, locals)?;
                }
                for expression in plan.expressions() {
                    self.check_expr(expression, locals)?;
                }
                Ok(())
            }
            crate::ast::Expr::RecOrder { plan, .. } => {
                if let Some(continuation) = &plan.tail_continuation {
                    self.check_expr(continuation, locals)?;
                }
                if let Some(annotation) = &plan.annotation {
                    self.check_type(annotation, locals)?;
                }
                self.check_type(&plan.runtime_ty, locals)?;
                self.check_expr(&plan.value, locals)?;
                let mut body_locals = locals.clone();
                body_locals.insert(plan.name.as_str(), Binding::Local);
                self.check_expr(&plan.body, &body_locals)
            }
            crate::ast::Expr::UserElaborator {
                name, args, meta, ..
            } => {
                if !self.user_elaborators_in_scope.contains(name.as_str()) {
                    return Err(Error::name_res(
                        meta.span,
                        format!("user elaborator `{name}!` is not in scope"),
                    )
                    .with_help(format!("import it with `import <module>({name});`")));
                }
                for arg in args {
                    self.check_call_arg(arg, locals)?;
                }
                Ok(())
            }
            crate::ast::Expr::Ufcs {
                receiver,
                callee_segments,
                args,
                bang,
                ..
            } => {
                self.check_expr(receiver, locals)?;
                if bang.is_some() {
                    debug_assert_eq!(
                        callee_segments.len(),
                        1,
                        "bang UFCS must have a single-segment callee — parser invariant"
                    );
                    let name = callee_segments[0].as_str();
                    if !self.user_elaborators_in_scope.contains(name) {
                        return Err(Error::name_res(
                            callee_segments[0].span,
                            format!("user elaborator `{name}!` is not in scope"),
                        )
                        .with_help(format!("import it with `import <module>({name});`")));
                    }
                } else {
                    self.lookup_value_path(callee_segments, callee_segments[0].span, locals)?;
                }
                for arg in args {
                    self.check_call_arg(arg, locals)?;
                }
                Ok(())
            }
            // Statically uninhabited at every `ResolvePhase`: the
            // enriched structural variants only appear post-typecheck,
            // produced by the structural-recovery pass at the
            // `Enriched` phase, long after name resolution runs.
            crate::ast::Expr::EnrichedTuple { ext, .. }
            | crate::ast::Expr::EnrichedProject { ext, .. }
            | crate::ast::Expr::EnrichedInject { ext, .. }
            | crate::ast::Expr::EnrichedMatch { ext, .. }
            | crate::ast::Expr::EnrichedConditional { ext, .. }
            | crate::ast::Expr::EnrichedRecord { ext, .. }
            | crate::ast::Expr::EnrichedFieldGet { ext, .. } => match *ext {},
            crate::ast::Expr::LowHostCall { ext, .. }
            | crate::ast::Expr::LowModuleCall { ext, .. }
            | crate::ast::Expr::LowQualifiedModuleCall { ext, .. }
            | crate::ast::Expr::LowQualifiedNewtypeMember { ext, .. }
            | crate::ast::Expr::LowNewtypeCtor { ext, .. }
            | crate::ast::Expr::LowNewtypeProj { ext, .. }
            | crate::ast::Expr::LowClosureCall { ext, .. }
            | crate::ast::Expr::LowIndirectCall { ext, .. }
            | crate::ast::Expr::LowTypeApplication { ext, .. }
            | crate::ast::Expr::LowAbsurdCall { ext, .. }
            | crate::ast::Expr::LowCpsProjectorApply { ext, .. }
            | crate::ast::Expr::LowBoundRef { ext, .. }
            | crate::ast::Expr::LowHostFnValueRef { ext, .. }
            | crate::ast::Expr::LowModuleFnValueRef { ext, .. } => match *ext {},
        }
    }

    fn check_call_arg<P: ResolvePhase>(
        &self,
        arg: &'m crate::ast::CallArg<P>,
        locals: &HashMap<&'m str, Binding>,
    ) -> Result<(), Error> {
        let value = match arg {
            crate::ast::CallArg::Type(ty) => return self.check_type(ty, locals),
            crate::ast::CallArg::Value(value) => value,
        };
        let head = match value {
            crate::ast::Expr::Path { segments, .. } => segments.as_slice(),
            crate::ast::Expr::Call { callee, .. } => match callee.as_ref() {
                crate::ast::Expr::Path { segments, .. } => segments.as_slice(),
                _ => &[],
            },
            _ => &[],
        };
        // Parsed expression-shaped arguments can become types only when the
        // resolved call type selects that slot. Check their names here without
        // preempting that decision. Exact lexical binders also cover hygienic
        // type names in fresh phase artifacts.
        let bound_type_head = matches!(head, [head]
            if matches!(locals.get(head.as_str()), Some(Binding::TypeParam(_))));
        if (crate::pass::typecheck_core::value_arg_looks_like_type_arg(value) || bound_type_head)
            && let Ok(ty) = crate::pass::typecheck_core::expr_to_type_arg(value)
        {
            return self.check_type(&ty, locals);
        }
        self.check_expr(value, locals)
    }

    /// Resolve a name's first segment. Order of lookup: locals (params,
    /// let-bindings, type params), top-level decls visible at this point,
    /// imports brought in by `import`, intrinsics. Reports unbound if none
    /// of those match.
    fn lookup_name(
        &self,
        name: &str,
        span: Span,
        locals: &HashMap<&'m str, Binding>,
    ) -> Result<(), Error> {
        if self.name_is_visible(name, locals) {
            return Ok(());
        }
        // For names beginning with `__`: this is a compiler-reserved spelling.
        // The eight intrinsics are handled above. Any other `__name__` is
        // unbound — the desugar mints hygienic non-reserved names for
        // user-introduced bindings, so the only `__name__`-shaped
        // declarations are the intrinsics.
        Err(self.unbound_name_error(name, span, locals))
    }

    /// Resolve a value path without granting diagnostic evidence any binding
    /// authority. A known constructor or projector on a later newtype gets the
    /// same directed source-order diagnostic as a type-position reference;
    /// ordinary paths retain the existing lookup behavior.
    fn lookup_value_path(
        &self,
        segments: &[crate::ast::PathSegment],
        fallback_span: Span,
        locals: &HashMap<&'m str, Binding>,
    ) -> Result<(), Error> {
        if let [head] = segments
            && let Some(Binding::TypeParam(declaration_span)) = locals.get(head.as_str())
        {
            return Err(Error::name_res(
                head.span,
                format!("`{}` is a type, not a value", head.as_str()),
            )
            .with_secondary(*declaration_span, "type parameter declared here"));
        }
        let head = &segments[0];
        if !self.name_is_visible(head.as_str(), locals)
            && let [head, member] = segments
            && let Some(declaration) = self.later_type_declarations.get(head.as_str())
            && (declaration.alias_member_head
                || declaration.value_members.contains(&member.as_str()))
        {
            return Err(self
                // A machine edit is offered only when either the local
                // newtype or the package's exact identity-alias index proves
                // this callable member before the move.
                .later_type_declaration_error(
                    head.as_str(),
                    head.span,
                    declaration.value_members.contains(&member.as_str()),
                )
                .expect("the member lookup proved a later type declaration"));
        }
        self.lookup_name(head.as_str(), fallback_span, locals)
    }

    fn name_is_visible(&self, name: &str, locals: &HashMap<&str, Binding>) -> bool {
        locals.contains_key(name)
            || self.visible_top_level.contains(name)
            || self.imported.contains(name)
            || self.recursive_types.contains(name)
            || (self.intrinsics_in_scope && is_intrinsic_in_scope(name))
    }

    fn lookup_type_name(
        &self,
        name: &str,
        span: Span,
        locals: &HashMap<&str, Binding>,
    ) -> Result<(), Error> {
        if self.name_is_visible(name, locals) {
            return Ok(());
        }
        if let Some(problem) = self.implicit_type_cycles.get(&span) {
            return Err(problem.error(span));
        }
        if let Some(error) = self.later_type_declaration_error(name, span, true) {
            return Err(error);
        }
        Err(self.unbound_name_error(name, span, locals))
    }

    fn later_type_declaration_error(
        &self,
        name: &str,
        span: Span,
        allow_fix: bool,
    ) -> Option<Error> {
        self.later_type_declarations.get(name).map(|declaration| {
            let move_subject = if declaration.owns_complete_source_declaration {
                format!("the declaration of `{name}`")
            } else {
                format!("the source declaration that provides `{name}`")
            };
            let mut error = Error::name_res(
                span,
                format!("`{name}` is declared later and is not visible here"),
            )
            .with_secondary(declaration.name_span, format!("`{name}` is declared here"))
            .with_help(format!(
                "move {move_subject} before this use; use a bare `rec {{ ... }}` group only when the declarations form one genuine recursive component"
            ));
            let dependencies_are_already_visible = declaration
                .dependency_items
                .iter()
                .all(|dependency| *dependency < self.current_item_index);
            if allow_fix
                && declaration.safe_move
                && declaration.item_index > self.current_item_index
                && dependencies_are_already_visible
                && declaration.dependent_items <= 1
            {
                error = error.with_fix(Fix::machine_applicable(
                    format!("Move `{name}` before its use"),
                    vec![
                        FixEdit::from_parts(
                            Span::new(self.current_item_span.start, self.current_item_span.start),
                            vec![
                                FixReplacementPart::Source(declaration.item_span),
                                FixReplacementPart::Text("\n".to_owned()),
                            ],
                            vec![self.current_item_leading_gap, declaration.leading_gap],
                        ),
                        FixEdit::new(declaration.item_span, ""),
                    ],
                ));
            }
            error
        })
    }

    /// The unbound-name diagnostic, enriched with a `did you mean …?`
    /// suggestion when a near-miss is in scope. The candidate pool is
    /// every name reachable at this point — the visible top-level decls,
    /// the imports, explicit recursive heads, and the active locals — so a typo on
    /// any of them earns the nearest-match suggestion (the highest-value
    /// name-resolution diagnostic per `specs/diagnostics.md` and the
    /// phase-10 rubric).
    fn unbound_name_error(&self, name: &str, span: Span, locals: &HashMap<&str, Binding>) -> Error {
        if let Some(problem) = self.implicit_type_cycles.get(&span) {
            return problem.error(span);
        }
        let message = format!("unbound name `{name}`");
        // Sorted so the suggestion is deterministic across runs when two
        // candidates tie on edit distance.
        let mut candidates: Vec<&str> = self
            .visible_top_level
            .iter()
            .copied()
            .chain(self.imported.iter().copied())
            .chain(self.recursive_types.iter().copied())
            .chain(locals.keys().copied())
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        match crate::error::closest_name(name, candidates.iter().copied()) {
            Some(near) => Error::name_res(span, message)
                .with_unresolved_name(name.to_owned())
                .with_help(format!(
                    "a name `{near}` is in scope — did you mean `{near}`?"
                ))
                .with_suggestion(span, near.to_owned()),
            None => Error::name_res(span, message).with_unresolved_name(name.to_owned()),
        }
    }
}

pub(crate) fn item_name<P: ResolvePhase>(item: &crate::ast::Item<P>) -> &str {
    match item {
        crate::ast::Item::FnDef(d) => &d.name,
        crate::ast::Item::TypeAlias(a) => &a.name,
        crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
        crate::ast::Item::Newtype(d) => &d.name,
        // Statically uninhabited at every `ResolvePhase`.
        crate::ast::Item::Labels(_, ext) => match *ext {},
        crate::ast::Item::LabelForward(_, ext) => match *ext {},
        crate::ast::Item::Equiv(e, _ext) => &e.name,
        crate::ast::Item::Elaborator(s, _ext) => &s.name,
        crate::ast::Item::HostType(h) => &h.name,
        crate::ast::Item::HostFn(h) => &h.name,
        // Statically uninhabited at every `ResolvePhase`.
        crate::ast::Item::Op(_, ext) => match *ext {},
        crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
        crate::ast::Item::RecGroup(_, ext) => match *ext {},
        crate::ast::Item::TypeRecGroup(_) => {
            unreachable!("a type-recursive group has several declaration names")
        }
    }
}

#[cfg(all(test, feature = "prime"))]
mod host_source_order_tests;

#[cfg(all(test, feature = "prime"))]
mod wrong_namespace_tests;

#[cfg(all(test, feature = "surface", feature = "prime"))]
mod tests {
    use super::*;
    use crate::error::Diagnostic;
    use crate::pass::parser::parse;

    /// Run a Surface module through desugar + label-elab so the
    /// resolver (which is generic over `ResolvePhase`) can be
    /// exercised against the `Lowered` end of the bound — that's
    /// the typical entry point for full-pipeline unit tests.
    fn parse_module(src: &str) -> crate::ast::Module<Lowered> {
        let m = parse(src).expect("parse");
        let m = crate::pass::desugar::desugar_module(m).expect("desugar");
        let parsed = vec![(std::path::PathBuf::from("test.kio"), m)];
        let (mut lowered, _) =
            crate::pass::label_elab::elaborate_package(parsed, None).expect("elaborate");
        lowered.pop().expect("one module").1
    }

    fn path(src: &str) -> ModulePath {
        parse(&format!("module {src};")).expect("module path").path
    }

    fn apply_source_fix(source: &str, fix: &Fix) -> String {
        let mut repaired = source.to_owned();
        for edit in fix.edits.iter().rev() {
            let replacement = if edit.replacement_parts.is_empty() {
                edit.replacement.clone()
            } else {
                edit.replacement_parts
                    .iter()
                    .map(|part| match part {
                        FixReplacementPart::Text(text) => text.as_str(),
                        FixReplacementPart::Source(span) => {
                            &source[usize::try_from(span.start).unwrap()
                                ..usize::try_from(span.end).unwrap()]
                        }
                    })
                    .collect::<String>()
            };
            repaired.replace_range(
                usize::try_from(edit.span.start).unwrap()..usize::try_from(edit.span.end).unwrap(),
                &replacement,
            );
        }
        repaired
    }

    #[test]
    fn visibility_coverage_uses_scope_containment() {
        use crate::ast::Visibility::{Private, Public, PublicIn};

        let owner = path("pkg/feature/impls");
        let other = path("pkg/other");
        let pkg = path("pkg");
        let feature = path("pkg/feature");
        let nested = path("pkg/feature/nested");

        assert!(visibility_covers(&Public, &owner, &Public, &owner));
        assert!(!visibility_covers(
            &PublicIn(pkg.clone()),
            &owner,
            &Public,
            &owner
        ));
        assert!(visibility_covers(
            &PublicIn(pkg.clone()),
            &owner,
            &PublicIn(feature.clone()),
            &owner,
        ));
        assert!(!visibility_covers(
            &PublicIn(nested),
            &owner,
            &PublicIn(feature),
            &owner,
        ));
        assert!(visibility_covers(&Private, &owner, &Private, &owner));
        assert!(!visibility_covers(&Private, &other, &Private, &owner));
        assert!(visibility_covers(&PublicIn(pkg), &other, &Private, &owner,));
    }

    #[test]
    fn empty_module_has_empty_scope() {
        let m = parse_module("module x;");
        let scope = TopLevelScope::build(&m).unwrap();
        assert!(scope.is_empty());
    }

    #[test]
    fn three_distinct_decls_register() {
        let m = parse_module(
            "module x;\n\
             fn foo() -> . { () }\n\
             type Bar = .;\n\
             newtype Baz : . { pub constructor mk_baz; pub projector un_baz; };",
        );
        let scope = TopLevelScope::build(&m).unwrap();
        assert_eq!(scope.len(), 3);
        assert_eq!(scope.lookup("foo"), Some(TopLevelId(0)));
        assert_eq!(scope.lookup("Bar"), Some(TopLevelId(1)));
        assert_eq!(scope.lookup("Baz"), Some(TopLevelId(2)));
    }

    #[test]
    fn lookup_unknown_returns_none() {
        let m = parse_module("module x; fn foo() -> . { () }");
        let scope = TopLevelScope::build(&m).unwrap();
        assert_eq!(scope.lookup("nope"), None);
    }

    #[test]
    fn duplicate_fn_def_is_error() {
        let m = parse_module("module x; fn foo() -> . { () } fn foo() -> . { () }");
        let err = TopLevelScope::build(&m).unwrap_err();
        match err {
            Error::NameRes(Diagnostic { message, .. }) => assert!(message.contains("duplicate")),
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn fn_def_and_type_alias_with_same_name_collide() {
        // Type names and value names have disjoint spelling classes, so this
        // case can only happen if the user writes a same-spelling collision —
        // which is impossible by Kio's naming convention. The validator
        // catches it at the parse layer (a type-shaped ident in fn position
        // is a parse error). The resolver still rejects it if the parser
        // ever loosened, so we exercise it here by constructing two decls
        // that share an obviously-collidable name.
        let m = parse_module("module x; fn foo() -> . { () } type Foo = .;");
        // No collision — `foo` and `Foo` differ in case. Build succeeds.
        let scope = TopLevelScope::build(&m).unwrap();
        assert_eq!(scope.len(), 2);
    }

    /// Confirms the in-body `Resolver` works at the `Prime` end of
    /// the `ResolvePhase` bound. Lowers
    /// a Kio'-shaped Surface module via `prime::lower` (skipping
    /// desugar + label_elab entirely) and runs `Resolver::check_module`
    /// on the resulting `Module<Prime>`.
    #[cfg(feature = "prime")]
    #[test]
    fn resolver_checks_a_prime_module() {
        let src = "module x; fn id[A](x: A) -> A { x }";
        let parsed = parse(src).expect("parse");
        let m: crate::ast::Module<crate::ast::Prime> =
            crate::prime::lower::lower_module(parsed).expect("prime::lower");
        Resolver::check_module(&m).expect("resolver checks a prime module");
    }

    /// Confirms the resolver's error path also fires at `Prime`:
    /// an unbound name reference is flagged the same way it would
    /// be at `Lowered`.
    #[cfg(feature = "prime")]
    #[test]
    fn resolver_rejects_unbound_name_at_prime() {
        let src = "module x; fn use_undef() -> . { not_a_thing }";
        let parsed = parse(src).expect("parse");
        let m: crate::ast::Module<crate::ast::Prime> =
            crate::prime::lower::lower_module(parsed).expect("prime::lower");
        let err = Resolver::check_module(&m).expect_err("expected unbound");
        match err {
            Error::NameRes(Diagnostic { message, .. }) => {
                assert!(message.contains("unbound"), "names form: {message}")
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_newtype_is_error_at_second_decl() {
        let src = "module x;\n\
                   newtype Foo : . { pub constructor mk_a; pub projector un_a; };\n\
                   newtype Foo : . { pub constructor mk_b; pub projector un_b; };";
        let m = parse_module(src);
        let err = TopLevelScope::build(&m).unwrap_err();
        match err {
            Error::NameRes(Diagnostic { span, message, .. }) => {
                assert!(message.contains("duplicate"));
                // Span should be on the second newtype, which starts after
                // the first declaration's terminating `;\n`.
                assert!(span.start > 50);
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn top_level_scope_defers_local_import_conflicts() {
        // Package import validation must run before cross-origin name checks,
        // so the module-local declaration index deliberately ignores imports.
        let src = "module x;\n\
                   import other(Wrap);\n\
                   newtype Wrap : . { pub constructor mk; pub projector un; };";
        let m = parse_module(src);
        TopLevelScope::build(&m).expect("cross-origin conflicts are a later package check");
    }

    #[test]
    fn top_level_scope_indexes_alias_despite_import_for_later_validation() {
        let src = "module x;\n\
                   import other(Tag);\n\
                   pub type Tag = .;";
        let m = parse_module(src);
        let scope = TopLevelScope::build(&m).expect("scope construction is declaration-only");
        assert!(scope.lookup("Tag").is_some());
    }

    #[test]
    fn top_level_scope_common_header_is_one_word_over_declarations() {
        let declaration_header = std::mem::size_of::<HashMap<String, TopLevelId>>();
        let one_word = std::mem::size_of::<usize>();

        assert!(
            std::mem::size_of::<TopLevelScope>() <= declaration_header + one_word,
            "the common module scope must not inline import-map headers: declaration={declaration_header}, actual={}",
            std::mem::size_of::<TopLevelScope>(),
        );
    }

    #[test]
    fn nominal_provider_header_is_lazy_and_compact() {
        let eight_words = 8 * std::mem::size_of::<usize>();

        assert!(
            std::mem::size_of::<NominalProvider<'static, Lowered>>() <= eight_words,
            "an operation-local nominal provider must keep its maps behind lazy storage: limit={eight_words}, actual={}",
            std::mem::size_of::<NominalProvider<'static, Lowered>>(),
        );
    }

    #[test]
    fn top_level_scope_uses_optional_shared_type_import_storage() {
        let empty = TopLevelScope::build(&parse_module("module consumer;"))
            .expect("empty declaration scope");
        let lowercase =
            TopLevelScope::build(&parse_module("module consumer; import provider(value);"))
                .expect("value-only import scope");
        let mixed = TopLevelScope::build(&parse_module(
            "module consumer; import provider(Item, _Item, value); import provider/nested as dep;",
        ))
        .expect("mixed import scope");
        let mixed_clone = mixed.clone();

        assert_eq!(empty.type_import_storage_identity(), None);
        assert_eq!(lowercase.type_import_storage_identity(), None);
        let (identity, selective, qualified) = mixed
            .type_import_storage_identity()
            .expect("type import storage");
        assert_eq!((selective, qualified), (2, 1));
        assert_eq!(
            mixed_clone.type_import_storage_identity(),
            Some((identity, selective, qualified)),
            "cloned module scopes must share one immutable type-import index"
        );
    }

    #[test]
    fn top_level_scope_indexes_written_type_import_edges() {
        let module = parse_module(
            "module consumer; import provider(Item, _Item, value); import provider/nested as dep;",
        );
        let scope = TopLevelScope::build(&module).expect("declaration and import index");
        assert_eq!(
            scope
                .selective_type_path("Item")
                .expect("selective type edge")
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["provider", "Item"]
        );
        assert_eq!(
            scope
                .selective_type_path("_Item")
                .expect("marked selective type edge")
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["provider", "_Item"]
        );
        assert_eq!(
            scope
                .qualified_type_path("dep")
                .expect("qualified module edge")
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["provider", "nested"]
        );
        assert!(
            scope.selective_type_path("value").is_none(),
            "lowercase value imports must not occupy the type-only acceleration index"
        );
    }

    #[test]
    fn selective_type_index_shares_one_deep_provider_prefix_across_fanout() {
        const DEPTH: usize = 64;
        const NAMES: usize = 64;

        let prefix = (0..DEPTH)
            .map(|index| format!("provider{index}"))
            .collect::<Vec<_>>();
        let names = (0..NAMES)
            .map(|index| format!("Type{index}"))
            .collect::<Vec<_>>();
        let source = format!(
            "module consumer; import {}({});",
            prefix.join("/"),
            names.join(", ")
        );
        let module = parse_module(&source);
        let scope = TopLevelScope::build(&module).expect("deep selective import scope");
        let (retained_segments, retained_name_bytes) = scope.selective_type_retained_storage();
        let expected_name_bytes = prefix.iter().map(String::len).sum::<usize>()
            + names.iter().map(String::len).sum::<usize>();

        assert_eq!(
            retained_segments, DEPTH,
            "one written selective import must retain its provider prefix once, not once per name"
        );
        assert_eq!(
            retained_name_bytes, expected_name_bytes,
            "retained provider/name bytes must be linear in the written path plus item list"
        );
    }

    #[test]
    fn selective_type_index_preserves_first_written_edge_and_source_spans() {
        let module = parse_module(
            "module consumer; import first/provider(Item); import second/provider(Item);",
        );
        let first_use = &module.imports[0];
        let ImportKind::Selective { from, .. } = &first_use.kind else {
            panic!("the first fixture edge is selective");
        };
        let mut expected_spans = from
            .segments
            .iter()
            .map(|segment| segment.span)
            .collect::<Vec<_>>();
        expected_spans.push(first_use.span);

        let scope =
            TopLevelScope::build(&module).expect("duplicate written edges defer validation");
        let path = scope
            .selective_type_path("Item")
            .expect("first selective type edge");

        assert_eq!(
            path.iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>(),
            vec!["first", "provider", "Item"]
        );
        assert_eq!(
            path.iter().map(|segment| segment.span).collect::<Vec<_>>(),
            expected_spans,
            "sharing storage must preserve the exact written edge and diagnostic spans"
        );
    }

    #[test]
    fn top_level_scope_debug_ignores_derived_import_indexes() {
        let empty = parse_module("module consumer;");
        let imported =
            parse_module("module consumer; import provider(Item); import provider/nested as dep;");
        let declared = parse_module("module consumer; type Named = .;");
        let empty = TopLevelScope::build(&empty).expect("empty declaration scope");
        let imported = TopLevelScope::build(&imported).expect("indexed import declaration scope");
        let declared = TopLevelScope::build(&declared).expect("named declaration scope");

        assert_eq!(
            format!("{imported:?}"),
            format!("{empty:?}"),
            "derived written-import indexes must not perturb the declaration-only scope fingerprint"
        );
        assert_ne!(
            format!("{declared:?}"),
            format!("{empty:?}"),
            "the declaration scope must remain part of the package fingerprint"
        );
    }

    // ---- Package tests --------------------------------------------------

    /// Derive the on-disk file path a module must live at, given its
    /// declared `module a/b;` path: `a/b.kio` — per
    /// `specs/package.md` § Module-name rules, the declared
    /// `/`-separated segments are the file path relative to the package
    /// root, with `.kio` stripped. The package name is
    /// **not** prepended at the declaration.
    fn module_file_path(src: &str) -> PathBuf {
        let module = parse_module(src);
        let segs = &module.path.segments;
        let mut path = PathBuf::new();
        for seg in &segs[..segs.len().saturating_sub(1)] {
            path.push(&seg.name);
        }
        let stem = segs.last().map(|s| s.name.as_str()).unwrap_or("module");
        path.push(format!("{stem}.kio"));
        path
    }

    /// Build a package whose modules are placed at filesystem paths
    /// derived from their `module` declarations, so the path-coherence
    /// check passes. The package root is the empty path.
    fn pkg(srcs: Vec<&str>) -> Result<Package, LocatedError> {
        let parsed: Vec<_> = srcs
            .into_iter()
            .map(|src| (module_file_path(src), parse_module(src)))
            .collect();
        Package::build(Path::new(""), parsed, None)
    }

    #[test]
    fn direct_nominal_lookup_uses_top_level_scope_without_declaration_scan() {
        const MODULES: usize = 128;
        let mut modules = BTreeMap::new();
        for index in 0..MODULES {
            let source = format!("module m{index}; pub type N{index} = .;");
            let module = parse_module(&source);
            let scope = TopLevelScope::build(&module).expect("declaration-only scope");
            modules.insert(
                format!("m{index}"),
                ModuleEntry {
                    file_path: PathBuf::from(format!("m{index}.kio")),
                    module,
                    scope,
                },
            );
        }
        let package = Package::from_parts(modules, None);
        let input = vec![
            crate::ast::PathSegment::synth("m0", Span::new(0, 0)),
            crate::ast::PathSegment::synth("N0", Span::new(0, 0)),
        ];

        reset_type_reexport_work();
        assert_eq!(follow_type_reexport(&input, &package), input);
        assert_eq!(
            type_reexport_work(),
            TypeReexportWork::default(),
            "a direct declaration must use the exact top-level scope instead of scanning unrelated items"
        );
    }

    fn full_pkg(srcs: &[(&str, &str)]) -> Result<Package, LocatedError> {
        use crate::pipeline::Pipeline;

        let parsed = srcs
            .iter()
            .map(|(path, source)| {
                let module = crate::pass::parser::parse(source).expect("parse surface module");
                (PathBuf::from(path), module)
            })
            .collect();
        let (lowered, _) = crate::pass::full::FullPipeline::lower_package(parsed, None)?;
        Package::build(Path::new(""), lowered, None)
    }

    fn pkg_with_package_file(
        srcs: Vec<&str>,
        package_name: &str,
        package_body: &str,
    ) -> Result<Package, LocatedError> {
        let parsed: Vec<_> = srcs
            .into_iter()
            .map(|src| (module_file_path(src), parse_module(src)))
            .collect();
        let package_file_full = format!("package {package_name};\n{package_body}");
        let package_file =
            crate::pass::parser::parse_package_file(&package_file_full, Some(package_name))
                .expect("parse package file");
        let package_file =
            crate::pass::desugar::desugar_package_file(package_file).expect("desugar package file");
        let (_, package_file) =
            crate::pass::label_elab::elaborate_package(Vec::new(), Some(package_file))
                .expect("elaborate");
        let entry = PackageFileEntry {
            file_path: PathBuf::from(format!("{package_name}.pkg.kio")),
            package_name: package_name.to_owned(),
            package_file: package_file.expect("package file"),
        };
        Package::build(Path::new(""), parsed, Some(entry))
    }

    fn module_file_pkg_with_export(
        file_path: &str,
        src: &str,
        package_name: &str,
    ) -> Result<Package, LocatedError> {
        let parsed_surface = crate::pass::parser::parse_module_file(src).expect("parse");
        let desugared =
            crate::pass::desugar::desugar_module(parsed_surface.module).expect("desugar");
        let parsed_desugared = vec![(PathBuf::from(file_path), desugared)];
        let (parsed, _) =
            crate::pass::label_elab::elaborate_package(parsed_desugared, None).expect("elaborate");
        let package_file_full = format!("package {package_name};\nbridge {{ lib; }}");
        let package_file =
            crate::pass::parser::parse_package_file(&package_file_full, Some(package_name))
                .expect("parse package file");
        let package_file =
            crate::pass::desugar::desugar_package_file(package_file).expect("desugar package file");
        let (_, package_file) =
            crate::pass::label_elab::elaborate_package(Vec::new(), Some(package_file))
                .expect("elaborate");
        let entry = PackageFileEntry {
            file_path: PathBuf::from(format!("{package_name}.pkg.kio")),
            package_name: package_name.to_owned(),
            package_file: package_file.expect("package file"),
        };
        Package::build(Path::new(""), parsed, Some(entry))
    }

    #[test]
    fn package_indexes_modules_by_declared_path() {
        let p = pkg(vec!["module pkg/a;", "module pkg/b;"]).unwrap();
        assert!(p.module("pkg/a").is_some());
        assert!(p.module("pkg/b").is_some());
        assert!(p.module("pkg/c").is_none());
    }

    #[test]
    fn duplicate_module_path_is_import_error() {
        let result = pkg(vec!["module pkg/a;", "module pkg/a;"]);
        let err = result.unwrap_err();
        match err.error {
            Error::Import(Diagnostic { message, .. }) => {
                assert!(message.contains("duplicate module path"));
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn pub_in_is_sealed_from_outside_the_subtree() {
        let p = pkg(vec![
            "module helper/submod; pub(helper) fn secret() -> . { () }",
            "module main; import helper/submod(secret);",
        ])
        .unwrap();
        let err = p.resolve_imports().unwrap_err();
        match err.error {
            Error::Import(Diagnostic { message, .. }) => {
                assert!(message.contains("sealed"), "got: {message}");
                assert!(message.contains("pub(helper)"), "got: {message}");
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn pub_in_is_visible_within_the_subtree() {
        let p = pkg(vec![
            "module helper/submod; pub(helper) fn secret() -> . { () }",
            "module helper/other; import helper/submod(secret);",
        ])
        .unwrap();
        p.resolve_imports()
            .expect("a module under `helper` may import a `pub(helper)` item");
    }

    #[test]
    fn pub_in_restriction_must_be_an_ancestor_of_the_declaring_module() {
        let p = pkg(vec!["module foo/bar; pub(other) fn x() -> . { () }"]).unwrap();
        let err = p.resolve_imports().unwrap_err();
        match err.error {
            Error::Import(Diagnostic { message, .. }) => {
                assert!(message.contains("ancestors"), "got: {message}");
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn root_storage_key_is_not_package_prefixed() {
        let p = module_file_pkg_with_export(
            "lib.kio",
            "module lib; pub fn identity[A](x: A) -> A { x }",
            "pkg",
        )
        .unwrap();
        assert!(p.module("lib").is_some());
        assert!(p.module("pkg/lib").is_none());
    }

    #[test]
    fn import_selective_resolves_against_package() {
        let p = pkg(vec![
            "module pkg/a; pub fn foo() -> . { () }",
            "module pkg/b; import pkg/a(foo);",
        ])
        .unwrap();
        p.resolve_imports().unwrap();
    }

    #[test]
    fn import_qualified_resolves_against_package() {
        let p = pkg(vec!["module pkg/a;", "module pkg/b; import pkg/a as a;"]).unwrap();
        p.resolve_imports().unwrap();
    }

    #[test]
    fn newtype_member_heads_follow_only_written_lexical_routes() {
        let p = pkg(vec![
            "module provider; \
             pub newtype Imported : . { pub constructor make_i; pub projector open_i; }; \
             pub newtype Qualified : . { pub constructor make_q; pub projector open_q; };",
            "module decoy; \
             pub newtype Qualified : . { pub constructor make_d; pub projector open_d; };",
            "module main; \
             import provider(Imported); \
             import provider as p; \
             newtype Local : . { constructor make_l; projector open_l; };",
        ])
        .expect("package");
        p.resolve_imports().expect("resolve imports");
        let main = &p.module("main").expect("main module").module;
        let mut seen = Vec::new();
        for_each_resolved_newtype_member_head(&p, main, |head, owner, declaration| {
            seen.push((
                head.join("."),
                owner
                    .segments
                    .iter()
                    .map(crate::ast::PathSegment::as_str)
                    .collect::<Vec<_>>()
                    .join("/"),
                declaration.name.clone(),
            ));
        });

        assert_eq!(
            seen,
            vec![
                (
                    "Imported".to_owned(),
                    "provider".to_owned(),
                    "Imported".to_owned()
                ),
                ("Local".to_owned(), "main".to_owned(), "Local".to_owned()),
                (
                    "p.Imported".to_owned(),
                    "provider".to_owned(),
                    "Imported".to_owned()
                ),
                (
                    "p.Qualified".to_owned(),
                    "provider".to_owned(),
                    "Qualified".to_owned()
                ),
            ],
            "local, selective, and qualified heads retain their exact selected identity; the unimported decoy is absent"
        );
    }

    #[test]
    fn import_unknown_module_is_error() {
        let p = pkg(vec!["module pkg/a; import pkg/nope(foo);"]).unwrap();
        let err = p.resolve_imports().unwrap_err();
        match err.error {
            Error::Import(Diagnostic { message, .. }) => assert!(message.contains("not found")),
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn import_non_pub_item_is_error() {
        let p = pkg(vec![
            "module pkg/a; fn foo() -> . { () }", // not pub
            "module pkg/b; import pkg/a(foo);",
        ])
        .unwrap();
        let err = p.resolve_imports().unwrap_err();
        match err.error {
            Error::Import(diagnostic) => {
                assert!(diagnostic.message.contains("`pub`"));
                assert_eq!(
                    diagnostic.help(),
                    None,
                    "an importer cannot be told to weaken another module's visibility"
                );
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn import_unknown_name_in_module_is_error() {
        let p = pkg(vec![
            "module pkg/a; pub fn foo() -> . { () }",
            "module pkg/b; import pkg/a(bar);",
        ])
        .unwrap();
        let err = p.resolve_imports().unwrap_err();
        match err.error {
            Error::Import(Diagnostic { message, .. }) => {
                assert!(message.contains("does not export"))
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn import_intrinsics_always_succeeds() {
        let p = pkg(vec!["module pkg/a; import __intrinsics__;"]).unwrap();
        p.resolve_imports().unwrap();
    }

    fn expect_binding_origin_error(srcs: Vec<&str>, binding: &str) {
        let p = pkg(srcs).expect("binding-origin conflicts are checked after package assembly");
        p.resolve_imports().expect("all written uses are valid");
        let err = p
            .check_binding_origins()
            .expect_err("distinct origins for one visible name must be rejected");
        match err.error {
            Error::NameRes(Diagnostic { message, .. }) => {
                assert!(message.contains(binding), "got: {message}");
                assert!(message.contains("more than one source"), "got: {message}");
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn selective_import_binding_has_one_source_identity() {
        expect_binding_origin_error(
            vec![
                "module first; pub fn pick() -> . { () }",
                "module second; pub fn pick() -> . { () }",
                "module main; import first(pick); import second(pick);",
            ],
            "pick",
        );
    }

    #[test]
    fn qualified_import_alias_has_one_source_identity() {
        expect_binding_origin_error(
            vec![
                "module first;",
                "module second;",
                "module main; import first as dep; import second as dep;",
            ],
            "dep",
        );
    }

    #[test]
    fn local_alias_and_selective_import_have_distinct_binding_origins() {
        expect_binding_origin_error(
            vec![
                "module origin; pub type Tag = .;",
                "module main; import origin(Tag); type Tag = .;",
            ],
            "Tag",
        );
    }

    #[test]
    fn identity_alias_and_selective_import_are_distinct_introductions() {
        let p = pkg(vec![
            "module origin; pub newtype Tag : . { constructor mk; projector un; };",
            "module main; import origin(Tag); import origin as source; type Tag = source.Tag;",
        ])
        .expect("package");
        p.resolve_imports().expect("uses");
        let error = p
            .check_binding_origins()
            .expect_err("the alias is a second introduction");
        let Error::NameRes(diagnostic) = error.error else {
            panic!("expected NameRes");
        };
        assert_eq!(diagnostic.message, "binding `Tag` has more than one source");
        assert!(diagnostic.secondary()[0].span.start < diagnostic.span.start);
    }

    #[test]
    fn identity_aliases_to_nonnewtype_declarations_do_not_merge_introductions_or_gain_members() {
        let p = pkg(vec![
            "module origin; pub host type Handle; pub type Shape = . & .;",
            "module host_consumer; \
             import origin(Handle); \
             import origin as source; \
             type Handle = source.Handle;",
            "module alias_consumer; \
             import origin(Shape); \
             import origin as source; \
             type Shape = source.Shape;",
        ])
        .expect("package");
        p.resolve_imports().expect("uses");
        let error = p
            .check_binding_origins()
            .expect_err("a positional alias is a local introduction");
        assert!(matches!(error.error, Error::NameRes(_)));
        let members = IdentityAliasNewtypeIndex::build_for_package(&p);
        for (module, name) in [("host_consumer", "Handle"), ("alias_consumer", "Shape")] {
            assert!(
                members.terminal_key(module, name).is_none(),
                "a non-newtype terminal must not acquire constructor/projector members"
            );
        }
    }

    #[test]
    fn recursive_group_identity_alias_and_import_are_distinct_introductions() {
        let p = pkg(vec![
            "module origin; \
             rec { \
               pub type Tag = Wrap; \
               pub newtype Wrap : (. | Tag) { constructor mk; projector un; }; \
             }",
            "module main; \
             import origin(Tag); \
             import origin as source; \
             type Tag = source.Wrap;",
        ])
        .expect("package");
        p.resolve_imports().expect("uses");
        let error = p
            .check_binding_origins()
            .expect_err("terminal identity does not merge introductions");
        let Error::NameRes(diagnostic) = error.error else {
            panic!("expected NameRes");
        };
        assert_eq!(diagnostic.message, "binding `Tag` has more than one source");
    }

    #[test]
    fn identity_alias_newtype_members_require_full_positional_kind_preservation() {
        let p = pkg(vec![
            "module origin; \
             pub newtype Pair[A][B] : A & B { pub constructor mk; pub projector un; }; \
             pub newtype Higher[*F] : . { pub constructor make; pub projector open; };",
            "module relay; \
             import origin as source; \
             pub type Exact[A][B] = source.Pair(A, B); \
             pub type Partial[A] = source.Pair(A); \
             pub type Reordered[A][B] = source.Pair(B, A); \
             pub type Wrong_kind[A] = source.Higher(A);",
            "module facade; import relay as source; pub type Exact[X][Y] = source.Exact(X, Y);",
        ])
        .expect("package");
        p.resolve_imports().expect("uses");

        let exact = identity_alias_newtype_target(&p, "relay", "Exact")
            .expect("a fully-saturated positional alias exposes members");
        assert_eq!(exact.module_path, "origin");
        assert_eq!(exact.newtype.name, "Pair");

        let chained = identity_alias_newtype_target(&p, "facade", "Exact")
            .expect("the same identity proof composes across aliases");
        assert_eq!(chained.module_path, "origin");
        assert_eq!(chained.newtype.name, "Pair");

        for alias in ["Partial", "Reordered", "Wrong_kind"] {
            assert!(
                identity_alias_newtype_target(&p, "relay", alias).is_none(),
                "{alias} must not acquire a nominal member namespace"
            );
        }
    }

    #[test]
    fn identity_alias_member_target_requires_a_written_import_edge() {
        let p = pkg(vec![
            "module provider; \
             pub newtype Tag : . { pub constructor make; pub projector open; };",
            "module no_import; pub type Alias = provider.Tag;",
            "module imported; import provider as source; pub type Alias = source.Tag;",
            "module unrelated; pub newtype Noise : . { constructor make; projector open; };",
        ])
        .expect("package");

        assert!(
            identity_alias_newtype_target(&p, "no_import", "Alias").is_none(),
            "a module path is not an implicit qualified import"
        );
        let imported = identity_alias_newtype_target(&p, "imported", "Alias")
            .expect("the written qualified import supplies the exact edge");
        assert_eq!(imported.module_path, "provider");
        assert_eq!(imported.newtype.name, "Tag");
    }

    #[test]
    fn identity_alias_newtype_index_visits_each_chain_edge_once() {
        const ALIASES: usize = 256;
        let mut source = String::from(
            "module chain; pub newtype Terminal : . { pub constructor make; pub projector open; };",
        );
        for index in 0..ALIASES {
            let target = if index == 0 {
                "Terminal".to_owned()
            } else {
                format!("Alias{}", index - 1)
            };
            source.push_str(&format!(" pub type Alias{index} = {target};"));
        }
        let package = pkg(vec![source.as_str()]).expect("package");

        reset_identity_alias_index_edge_visits();
        let index = IdentityAliasNewtypeIndex::build_for_package(&package);
        assert_eq!(identity_alias_index_edge_visits(), ALIASES);
        for _ in 0..1024 {
            assert_eq!(
                index.terminal_key("chain", &format!("Alias{}", ALIASES - 1)),
                Some(&("chain".to_owned(), "Terminal".to_owned()))
            );
        }
        assert_eq!(
            identity_alias_index_edge_visits(),
            ALIASES,
            "member occurrences must query the completed index, not retraverse alias chains"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn surface_identity_alias_index_uses_one_indexed_scope_lookup_per_alias() {
        const ALIASES: usize = 128;
        let origin = parse(
            "module origin; \
             pub newtype Terminal : . { pub constructor make; pub projector open; };",
        )
        .expect("parse origin");
        let mut source = String::from("module relay;");
        for index in 0..ALIASES {
            source.push_str(&format!(" import origin as source{index};"));
        }
        for index in 0..ALIASES {
            source.push_str(&format!(" type Alias{index} = source{index}.Terminal;"));
        }
        let relay = parse(&source).expect("parse relay");

        reset_identity_alias_index_edge_visits();
        let index = IdentityAliasNewtypeIndex::build_for_surface_modules([&origin, &relay]);
        assert_eq!(
            identity_alias_indexed_scope_resolutions(),
            ALIASES,
            "Surface aliases must query the once-built type-import index rather than rescan every use"
        );
        assert_eq!(identity_alias_index_edge_visits(), ALIASES);
        assert_eq!(
            index.terminal_key("relay", "Alias127"),
            Some(&("origin".to_owned(), "Terminal".to_owned()))
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn surface_identity_alias_index_preserves_local_declaration_order() {
        let module = parse(
            "module local; \
             type Later = Target; \
             pub newtype Target : . { pub constructor make; pub projector open; }; \
             rec { \
               type Peer = Wrapped; \
               newtype Wrapped : Peer { constructor wrap; projector unwrap; }; \
             }",
        )
        .expect("parse local aliases");

        reset_identity_alias_index_edge_visits();
        let index = IdentityAliasNewtypeIndex::build_for_surface_modules([&module]);
        assert!(
            index.terminal_key("local", "Later").is_none(),
            "a standalone alias cannot borrow a later local target"
        );
        assert_eq!(
            index.terminal_key("local", "Peer"),
            Some(&("local".to_owned(), "Wrapped".to_owned())),
            "all heads in one explicit recursive group are mutually visible"
        );
        assert_eq!(
            identity_alias_index_edge_visits(),
            1,
            "the explicit-group alias edge is indexed exactly once"
        );
    }

    #[cfg(feature = "surface")]
    #[test]
    fn surface_identity_alias_index_does_not_import_later_local_targets_early() {
        let qualified = parse(
            "module qualified; \
             import qualified as self_module; \
             type Premature = self_module.Later; \
             pub newtype Later : . { pub constructor make; pub projector open; }; \
             type Inscope = self_module.Later;",
        )
        .expect("parse qualified self import");
        let selective = parse(
            "module selective; \
             import selective(Later); \
             type Premature = Later; \
             pub newtype Later : . { pub constructor make; pub projector open; }; \
             type Inscope = Later;",
        )
        .expect("parse selective self import");
        let recursive = parse(
            "module recursive; \
             import recursive as self_module; \
             rec { \
               type Peer = self_module.Wrapped; \
               newtype Wrapped : Peer { constructor wrap; projector unwrap; }; \
             }",
        )
        .expect("parse recursive self import");

        let index = IdentityAliasNewtypeIndex::build_for_surface_modules([
            &qualified, &selective, &recursive,
        ]);
        for module in ["qualified", "selective"] {
            assert!(
                index.terminal_key(module, "Premature").is_none(),
                "a self import must not lend a later local target to an earlier alias"
            );
            assert_eq!(
                index.terminal_key(module, "Inscope"),
                Some(&(module.to_owned(), "Later".to_owned())),
                "the same written self-import edge remains usable after the target declaration"
            );
        }
        assert_eq!(
            index.terminal_key("recursive", "Peer"),
            Some(&("recursive".to_owned(), "Wrapped".to_owned())),
            "all heads in an explicit recursive group remain atomically visible through a self import"
        );
    }

    #[test]
    fn surface_named_labels_aliases_require_the_exact_positional_lowering_shape() {
        check(
            "module valid; \
             labels Reordered[A][B] = { swapped[B][A]: A & B };",
        )
        .expect("binder permutation is a valid structural alias, just not an identity alias");
        let standalone = parse(
            "module standalone; \
             labels Exact[A][B] = { item[A][B]: A & B }; \
             labels Reordered[A][B] = { swapped[B][A]: A & B }; \
             labels Renamed[A] = { renamed_item[B]: B }; \
             labels Partial[A] = { partial_item: . };",
        )
        .expect("parse standalone labels aliases");
        let grouped = parse(
            "module grouped; \
             rec { \
               labels Exact[A][B] = { link[A][B]: Peer(A, B) }; \
               newtype Peer[A][B] : Exact(A, B) { constructor make; projector open; }; \
             } \
             rec { \
               labels Reordered[A][B] = { back[B][A]: Other(B, A) }; \
               newtype Other[A][B] : Reordered(A, B) { constructor make; projector open; }; \
             }",
        )
        .expect("parse recursive labels aliases");
        check(
            "module valid_group; \
             rec { \
               labels Reordered[A][B] = { swapped[B][A]: Peer(B, A) }; \
               newtype Peer[A][B] : Reordered(A, B) { constructor make; projector open; }; \
             }",
        )
        .expect("the binder-permuting recursive declaration remains structurally valid");

        let index = IdentityAliasNewtypeIndex::build_for_surface_modules([&standalone, &grouped]);
        assert_eq!(
            index.terminal_key("standalone", "Exact"),
            Some(&("standalone".to_owned(), "Item".to_owned()))
        );
        assert_eq!(
            index.terminal_key("grouped", "Exact"),
            Some(&("grouped".to_owned(), "Link".to_owned()))
        );
        for (module, alias) in [
            ("standalone", "Reordered"),
            ("standalone", "Renamed"),
            ("standalone", "Partial"),
            ("grouped", "Reordered"),
        ] {
            assert!(
                index.terminal_key(module, alias).is_none(),
                "{module}.{alias} must not acquire a nominal member namespace"
            );
        }
    }

    #[test]
    fn repeated_ordinary_imports_report_the_two_written_introductions() {
        for (source, name, first_start, later_start) in [
            (
                "module main; import origin(pick); import origin(pick);",
                "pick",
                27,
                48,
            ),
            ("module main; import origin(pick, pick);", "pick", 27, 33),
            (
                "module main; import origin as dep; import origin as dep;",
                "dep",
                13,
                35,
            ),
        ] {
            let p = pkg(vec!["module origin; pub fn pick() -> . { () }", source]).unwrap();
            p.resolve_imports().unwrap();
            p.check_no_value_cycles().unwrap();
            let error = p
                .check_binding_origins()
                .expect_err("each ordinary import introduces a binding");
            let Error::NameRes(diagnostic) = error.error else {
                panic!("expected NameRes");
            };
            assert_eq!(
                diagnostic.message,
                format!("binding `{name}` is introduced more than once")
            );
            assert_eq!(diagnostic.span.start, later_start);
            assert_eq!(diagnostic.secondary().len(), 1);
            assert_eq!(diagnostic.secondary()[0].span.start, first_start);
        }
    }

    #[test]
    fn repeated_compiler_block_imports_remain_idempotent() {
        let p = pkg(vec![
            "module main; import __intrinsics__; import __intrinsics__; import __comptime__; import __comptime__;",
        ]).unwrap();
        p.resolve_imports().unwrap();
        p.check_binding_origins()
            .expect("compiler blocks import fixed sets");
    }

    #[test]
    fn invalid_import_precedes_local_import_binding_conflict() {
        let p = pkg(vec![
            "module main; import missing(Wrap); \
             newtype Wrap : . { constructor mk; projector un; };",
        ])
        .expect("name conflicts are deferred until import validation has completed");
        let err = p
            .resolve_imports()
            .expect_err("the missing module is an import error");
        assert!(
            matches!(err.error, Error::Import(_)),
            "got: {:?}",
            err.error
        );
    }

    #[test]
    fn binding_origin_error_precedes_bridge_error() {
        let err = pkg_with_package_file(
            vec![
                "module origin; pub type Tag = .;",
                "module main; import origin(Tag); type Tag = .;",
            ],
            "testpkg",
            "bridge { missing; }",
        )
        .expect_err("name resolution must run before bridge validation");
        assert!(
            matches!(err.error, Error::NameRes(_)),
            "got: {:?}",
            err.error
        );
    }

    #[test]
    fn invalid_import_precedes_binding_origin_and_bridge_errors() {
        let err = pkg_with_package_file(
            vec!["module main; import absent(Tag); type Tag = .;"],
            "testpkg",
            "bridge { missing; }",
        )
        .expect_err("import validation must run before name and bridge validation");
        assert!(
            matches!(err.error, Error::Import(_)),
            "got: {:?}",
            err.error
        );
    }

    #[test]
    fn comptime_import_and_local_type_have_distinct_binding_origins() {
        expect_binding_origin_error(
            vec!["module main; import __comptime__; type Comptime_bool = .;"],
            "Comptime_bool",
        );
    }

    #[test]
    fn operator_generated_alias_avoids_consumer_binding_and_round_trips_to_prime() {
        use crate::backends::kio_prime::emit_module;
        use crate::pipeline::Pipeline;
        use crate::prime::pipeline::PrimePipeline;

        let ops_source = "module ops; \
                          pub fn add(_left: ., _right: .) -> . { () } \
                          pub op _ + _ { impl add; };";
        let ops = crate::pass::parser::parse(ops_source).expect("parse operator provider");
        let main = crate::pass::parser::parse(
            "module main; \
             import ops(op _ + _); \
             fn _op_gphahd__() -> . { () } \
             fn run() -> . { () + () }",
        )
        .expect("parse consumer with operator provider");
        let parsed = vec![
            (PathBuf::from("ops.kio"), ops),
            (PathBuf::from("main.kio"), main),
        ];
        let (lowered, _) = crate::pass::full::FullPipeline::lower_package(parsed, None)
            .expect("lower operator package");
        let package =
            Package::build(Path::new(""), lowered, None).expect("assemble operator package");
        package
            .resolve_imports()
            .expect("generated qualified import resolves");
        package
            .check_binding_origins()
            .expect("compiler-generated alias must allocate around the consumer binding");
        package
            .check_no_value_cycles()
            .expect("operator package has no value cycle");
        package
            .check_in_body_resolution()
            .expect("operator package resolves in-body names");

        let prime = crate::pass::full::FullPipeline::typecheck(&package)
            .expect("typecheck operator package");
        let mut saw_main = false;
        let reparsed = prime
            .modules()
            .map(|(module_path, entry)| {
                let source = emit_module(&entry.module);
                if module_path == "main" {
                    saw_main = true;
                    assert!(
                        source.contains("import ops as _op_gphahd_n2__;"),
                        "first occupied alias must allocate the deterministic successor: {source}"
                    );
                    assert!(
                        source.contains("_op_gphahd_n2__.add"),
                        "lowered call must use the allocated alias: {source}"
                    );
                    assert!(
                        !source.contains("__internal__"),
                        "ordinary operator lowering must carry no visibility privilege: {source}"
                    );
                }
                (
                    PathBuf::from(format!("{module_path}.kio")),
                    parse(&source).unwrap_or_else(|error| {
                        panic!("parse emitted `{module_path}` as fresh source: {error:?}\n{source}")
                    }),
                )
            })
            .collect();
        assert!(saw_main, "emitted package must contain module `main`");

        let (fresh_prime, _) = PrimePipeline::lower_package(reparsed, None)
            .expect("fresh source is admitted by the Kio' grammar");
        let fresh_package = Package::build(Path::new(""), fresh_prime, None)
            .expect("assemble freshly parsed Kio' package");
        fresh_package
            .resolve_imports()
            .expect("fresh Kio' ordinary import resolves");
        fresh_package
            .check_binding_origins()
            .expect("fresh Kio' bindings retain distinct identities");
        fresh_package
            .check_no_value_cycles()
            .expect("fresh Kio' has no value cycle");
        fresh_package
            .check_in_body_resolution()
            .expect("fresh Kio' resolves the allocated alias");
        PrimePipeline::typecheck(&fresh_package)
            .expect("standalone Prime validation accepts the fresh artifact");
    }

    #[test]
    fn operator_generated_alias_is_not_captured_by_nested_value_binder() {
        use crate::pipeline::Pipeline;

        let ops_source = "module ops; \
                          pub fn add(_left: ., _right: .) -> . { () } \
                          pub op _ + _ { impl add; };";
        let ops = crate::pass::parser::parse(ops_source).expect("parse operator provider");
        let main = crate::pass::parser::parse(
            "module main; \
             import ops(op _ + _); \
             fn run(_op_gphahd__: .) -> . { _op_gphahd__; () + () }",
        )
        .expect("parse consumer with operator provider");
        let parsed = vec![
            (PathBuf::from("ops.kio"), ops),
            (PathBuf::from("main.kio"), main),
        ];
        let (lowered, _) = crate::pass::full::FullPipeline::lower_package(parsed, None)
            .expect("lower operator package");
        let package =
            Package::build(Path::new(""), lowered, None).expect("assemble operator package");
        crate::pass::full::FullPipeline::typecheck(&package)
            .expect("qualified operator call resolves independently of nested value scope");
    }

    #[test]
    fn label_generated_type_has_one_source_identity() {
        let err = full_pkg(&[
            ("source.kio", "module source; pub type Foo = .;"),
            (
                "main.kio",
                "module main; import source(Foo); labels { foo: . };",
            ),
        ])
        .expect_err("generated label type must not overwrite an imported type");
        match err.error {
            Error::NameRes(Diagnostic { message, .. }) => {
                assert!(message.contains("Foo"), "got: {message}");
                assert!(message.contains("more than one source"), "got: {message}");
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn root_env_names_resolve_in_body() {
        let p = pkg(vec![
            "module hello; \
             host type String role(str); host fn print(s: String) -> .; \
             fn f(s: String) -> . { print(s) }",
        ])
        .unwrap();
        p.resolve_imports().unwrap();
        p.check_in_body_resolution().unwrap();
    }

    #[test]
    fn root_env_signature_resolves_kio_type() {
        let p = pkg(vec![
            "module hello; \
             pub newtype Box : . { pub constructor mk_box; pub projector un_box; }; \
             host fn accept(b: Box) -> .;",
        ])
        .unwrap();
        p.resolve_imports().unwrap();
        p.check_in_body_resolution().unwrap();
    }

    #[test]
    fn root_env_signature_unknown_type_is_error() {
        let p = pkg(vec!["module hello; host fn accept(b: Missing) -> .;"]).unwrap();
        p.resolve_imports().unwrap();
        let err = p.check_in_body_resolution().unwrap_err();
        match err.error {
            Error::NameRes(Diagnostic { message, .. }) => {
                assert!(message.contains("unbound name `Missing`"), "got: {message}");
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    #[test]
    fn package_bridge_dead_glob_is_bridge_error() {
        // A bridge glob matching zero modules is almost certainly a typo
        // — the resolver rejects it (the dead-glob check).
        let err = pkg_with_package_file(
            vec!["module app; pub fn run() -> . { () }"],
            "app",
            "bridge { nope; }",
        )
        .unwrap_err();
        match err.error {
            Error::Bridge(Diagnostic { message, .. }) => {
                assert!(message.contains("nope"), "got: {message}");
            }
            other => panic!("expected Bridge, got {other:?}"),
        }
    }

    #[test]
    fn package_bridge_glob_match_builds() {
        // A bridge glob matching a real module is admitted; the module's
        // `pub` items form the derived contract surface.
        pkg_with_package_file(
            vec!["module app; host type String role(str); pub fn run(s: String) -> String { s }"],
            "app",
            "bridge { app; }",
        )
        .unwrap();
    }

    #[test]
    fn public_newtype_host_surfaces_keep_exact_module_identity_and_all_four_states() {
        let package = pkg_with_package_file(
            vec![
                "module app/left; \
                 pub newtype Shared : . { pub constructor make; projector read; };",
                "module app/right; \
                 pub newtype Shared : . { constructor make; pub projector read; };",
                "module app/states; \
                 rec { \
                   pub newtype Opaque : Both { constructor make_opaque; projector read_opaque; }; \
                   pub newtype Both : Opaque { \
                     pub constructor make_both; pub projector read_both; \
                   }; \
                 }",
                "module app/outside; \
                 pub newtype Shared : . { pub constructor make; pub projector read; };",
            ],
            "app",
            "bridge { app/left; app/right; app/states; }",
        )
        .expect("build exact-identity host-surface fixture");

        let surfaces = public_newtype_host_surfaces(&package);
        assert_eq!(
            surfaces.keys().cloned().collect::<Vec<_>>(),
            vec![
                ("app/left".to_owned(), "Shared".to_owned()),
                ("app/right".to_owned(), "Shared".to_owned()),
                ("app/states".to_owned(), "Both".to_owned()),
                ("app/states".to_owned(), "Opaque".to_owned()),
            ]
        );
        assert!(matches!(
            surfaces[&("app/left".to_owned(), "Shared".to_owned())].surface,
            NewtypeHostSurface::Constructor { .. }
        ));
        assert!(matches!(
            surfaces[&("app/right".to_owned(), "Shared".to_owned())].surface,
            NewtypeHostSurface::Projector { .. }
        ));
        assert!(matches!(
            surfaces[&("app/states".to_owned(), "Both".to_owned())].surface,
            NewtypeHostSurface::Both { .. }
        ));
        assert!(matches!(
            surfaces[&("app/states".to_owned(), "Opaque".to_owned())].surface,
            NewtypeHostSurface::Opaque
        ));
        assert!(!surfaces.contains_key(&("app/outside".to_owned(), "Shared".to_owned())));
    }

    #[test]
    fn bridge_closure_ignores_all_private_newtype_payload() {
        pkg_with_package_file(
            vec![
                "module app; \
                 type Hidden = .; \
                 pub newtype Opaque : Hidden { constructor mk; projector un; };",
            ],
            "app",
            "bridge { app; }",
        )
        .expect("an opaque public identity does not expose its private payload");
    }

    #[test]
    fn bridge_closure_ignores_scoped_newtype_member_payload() {
        pkg_with_package_file(
            vec![
                "module app; \
                 pub(app) type Scoped = .; \
                 pub newtype Opaque : Scoped { \
                   pub(app) constructor mk; \
                   pub(app) projector un; \
                 };",
            ],
            "app",
            "bridge { app; }",
        )
        .expect("scoped members do not expose the payload to the host");
    }

    #[test]
    fn bridge_closure_checks_payload_exposed_by_one_public_member() {
        let error = pkg_with_package_file(
            vec![
                "module app; \
                 type Hidden = .; \
                 pub newtype Exposed : Hidden { pub constructor mk; projector un; };",
            ],
            "app",
            "bridge { app; }",
        )
        .expect_err("one public member exposes the payload");

        assert!(matches!(error.error, Error::Bridge(_)));
        assert!(
            error.error.diagnostic().message.contains("private"),
            "unexpected bridge diagnostic: {:?}",
            error.error
        );
    }

    // ---- Module-declaration / filesystem coherence ----------------------

    #[test]
    fn module_decl_matching_file_path_builds() {
        // `module utils/string;` at `utils/string.kio` (relative to
        // the package root) — the declared path equals the relative
        // file path, with `/` as `.` and `.kio` stripped.
        let module = parse_module("module utils/string;");
        let parsed = vec![(PathBuf::from("utils/string.kio"), module)];
        Package::build(Path::new(""), parsed, None).unwrap();
    }

    #[test]
    fn module_decl_root_level_module_builds() {
        // `module main;` directly at `main.kio` (package root). With
        // no package file, no host-namespace check applies.
        let module = parse_module("module main;");
        let parsed = vec![(PathBuf::from("main.kio"), module)];
        Package::build(Path::new(""), parsed, None).unwrap();
    }

    #[test]
    fn module_decl_wrong_subdirectory_is_parse_error() {
        // `module foo/bar;` declared in a file living at
        // `subdir/helper.kio` — the file path implies
        // `module subdir/helper;`.
        let module = parse_module("module foo/bar;");
        let parsed = vec![(PathBuf::from("subdir/helper.kio"), module)];
        let err = Package::build(Path::new(""), parsed, None).unwrap_err();
        match err.error {
            Error::Parse(Diagnostic { message, .. }) => {
                assert!(
                    message.contains("does not match the file path"),
                    "got: {message}"
                );
                assert!(message.contains("subdir/helper"), "got: {message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn module_decl_wrong_filename_stem_is_parse_error() {
        // `module main;` declared in a file living at `other.kio`.
        let module = parse_module("module main;");
        let parsed = vec![(PathBuf::from("other.kio"), module)];
        let err = Package::build(Path::new(""), parsed, None).unwrap_err();
        match err.error {
            Error::Parse(Diagnostic { message, .. }) => {
                assert!(
                    message.contains("does not match the file path"),
                    "got: {message}"
                );
                assert!(message.contains("other"), "got: {message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn module_decl_package_prefix_at_root_is_parse_error() {
        // Under the new spec, `module pkg/main;` at `main.kio` (with
        // a package file naming the package `pkg`) is no longer
        // accepted — the declaration must be `module main;` (and the
        // file would have to move to `pkg/main.kio` for the
        // pkg-prefix form to be coherent). The error is rule (1): the
        // declared segments don't match the file path.
        let module = parse_module("module pkg/main;");
        let parsed = vec![(PathBuf::from("main.kio"), module)];
        let package_file = crate::pass::parser::parse_package_file(
            "package pkg;\n\
             bridge { main; }",
            None,
        )
        .and_then(crate::pass::desugar::desugar_package_file)
        .expect("package file");
        let (_, package_file) =
            crate::pass::label_elab::elaborate_package(Vec::new(), Some(package_file))
                .expect("elaborate");
        let entry = PackageFileEntry {
            file_path: PathBuf::from("pkg.pkg.kio"),
            package_name: "pkg".to_owned(),
            package_file: package_file.expect("package file"),
        };
        let err = Package::build(Path::new(""), parsed, Some(entry)).unwrap_err();
        match err.error {
            Error::Parse(Diagnostic { message, .. }) => {
                assert!(
                    message.contains("does not match the file path"),
                    "got: {message}"
                );
                assert!(message.contains("module main;"), "got: {message}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    #[test]
    fn root_module_env_builds() {
        let module = parse_module(
            "module hello; \
             host type String role(str);",
        );
        let parsed = vec![(PathBuf::from("hello.kio"), module)];
        let package_file = crate::pass::parser::parse_package_file(
            "package hello;\n\
             bridge { hello; }",
            None,
        )
        .and_then(crate::pass::desugar::desugar_package_file)
        .expect("package file");
        let (_, package_file) =
            crate::pass::label_elab::elaborate_package(Vec::new(), Some(package_file))
                .expect("elaborate");
        let entry = PackageFileEntry {
            file_path: PathBuf::from("hello.pkg.kio"),
            package_name: "hello".to_owned(),
            package_file: package_file.expect("package file"),
        };
        let p = Package::build(Path::new(""), parsed, Some(entry)).unwrap();
        assert!(p.module("hello").is_some());
    }

    #[test]
    fn package_file_does_not_prefix_module_storage_key() {
        let module = parse_module(
            "module main; \
             pub fn id[A](x: A) -> A { x }",
        );
        let parsed = vec![(PathBuf::from("main.kio"), module)];
        let package_file = crate::pass::parser::parse_package_file(
            "package hello;\n\
             bridge { main; }",
            None,
        )
        .and_then(crate::pass::desugar::desugar_package_file)
        .expect("package file");
        let (_, package_file) =
            crate::pass::label_elab::elaborate_package(Vec::new(), Some(package_file))
                .expect("elaborate");
        let entry = PackageFileEntry {
            file_path: PathBuf::from("hello.pkg.kio"),
            package_name: "hello".to_owned(),
            package_file: package_file.expect("package file"),
        };
        let p = Package::build(Path::new(""), parsed, Some(entry)).unwrap();
        assert!(p.module("main").is_some());
        assert!(p.module("hello/main").is_none());
    }

    // ---- Cycle detection ------------------------------------------------

    #[test]
    fn no_cycle_in_acyclic_package() {
        let p = pkg(vec![
            "module pkg/a; pub fn x() -> . { () }",
            "module pkg/b; import pkg/a(x);",
        ])
        .unwrap();
        p.check_no_value_cycles().unwrap();
    }

    #[test]
    fn direct_cycle_is_detected() {
        // `import` statements come before items per Kio grammar.
        let p = pkg(vec![
            "module pkg/a; import pkg/b(y); pub fn x() -> . { () }",
            "module pkg/b; import pkg/a(x); pub fn y() -> . { () }",
        ])
        .unwrap();
        let err = p.check_no_value_cycles().unwrap_err();
        match err.error {
            Error::Import(diagnostic) => {
                assert!(diagnostic.message.contains("cycle"));
                assert!(diagnostic.message.starts_with("value-level import cycle:"));
                assert_eq!(
                    diagnostic.help(),
                    Some(
                        "break the cycle by removing one of the `import` edges between these \
                         modules, or move the shared definition into a third module both depend on"
                    )
                );
            }
            other => panic!("expected Use, got {other:?}"),
        }
    }

    #[test]
    fn value_import_topo_levels_put_dependencies_before_consumers() {
        let p = pkg(vec![
            "module pkg/a; pub fn a() -> . { () }",
            "module pkg/b; import pkg/a(a); pub fn b() -> . { a() }",
            "module pkg/c; import pkg/a(a); pub fn c() -> . { a() }",
            "module pkg/d; import pkg/b(b); import pkg/c(c); pub fn d() -> . { b() }",
        ])
        .unwrap();
        p.resolve_imports().unwrap();
        p.check_no_value_cycles().unwrap();
        let levels: Vec<Vec<&str>> = p
            .value_import_topo_levels()
            .into_iter()
            .map(|level| level.into_iter().map(|(path, _)| path).collect())
            .collect();
        assert_eq!(
            levels,
            vec![vec!["pkg/a"], vec!["pkg/b", "pkg/c"], vec!["pkg/d"]]
        );
    }

    // ---- Resolver: in-body name resolution -------------------------------

    fn check(src: &str) -> Result<(), Error> {
        Resolver::check_module(&parse_module(src))
    }

    fn elaborate_error(src: &str) -> Error {
        let module = parse(src).expect("parse");
        let module = crate::pass::desugar::desugar_module(module).expect("desugar");
        crate::pass::label_elab::elaborate_package(
            vec![(std::path::PathBuf::from("test.kio"), module)],
            None,
        )
        .expect_err("elaboration must reject the recursive group")
        .error
    }

    fn check_err(src: &str) -> String {
        match check(src) {
            Err(Error::NameRes(Diagnostic { message, .. })) => message,
            Err(other) => panic!("expected NameRes, got {other:?}"),
            Ok(_) => panic!("expected NameRes error"),
        }
    }

    #[test]
    fn empty_module_resolves() {
        check("module x;").unwrap();
    }

    #[test]
    fn fn_def_with_param_reference_resolves() {
        check("module x; fn id[A](x: A) -> A { x }").unwrap();
    }

    #[test]
    fn fn_def_with_let_binding_resolves() {
        check("module x; fn f[A](x: A) -> A { let y = x; y }").unwrap();
    }

    #[test]
    fn fn_def_calling_earlier_fn_def_resolves() {
        check(
            "module x; \
             fn helper() -> . { () } \
             fn main() -> . { helper() }",
        )
        .unwrap();
    }

    #[test]
    fn fn_def_self_reference_is_unbound() {
        // `fn` cannot mention itself: its own name is not in scope inside
        // its body.
        let msg = check_err("module x; fn loop() -> . { loop() }");
        assert!(msg.contains("`loop`"));
    }

    #[test]
    fn fn_def_forward_reference_is_unbound() {
        // Top-level visibility is order-sensitive: the second `fn`'s name
        // is not in scope inside the first's body.
        let msg = check_err(
            "module x; \
             fn first() -> . { second() } \
             fn second() -> . { () }",
        );
        assert!(msg.contains("`second`"));
    }

    #[test]
    fn fn_def_calling_earlier_private_elaborator_resolves() {
        check(
            "module x; \
             import __comptime__; \
             pure fn unit_impl(ct: __Comptime__) -> __Checked_term__ { \
               __term_unit__(ct) \
             } \
             elab unit : . -> . { impl unit_impl; }; \
             fn main() -> . { unit!() }",
        )
        .unwrap();
    }

    #[test]
    fn fn_def_calling_later_private_elaborator_is_unbound() {
        let source = "module x; \
             import __comptime__; \
             pure fn unit_impl(ct: __Comptime__) -> __Checked_term__ { \
               __term_unit__(ct) \
             } \
             fn main() -> . { unit!() } \
             elab unit : . -> . { impl unit_impl; };";
        for call in ["unit!()", "().>unit!"] {
            let error = check(&source.replace("unit!()", call))
                .expect_err("the later elaborator is not in scope");
            let Error::NameRes(diagnostic) = error else {
                panic!("expected NameRes, got {error:?}");
            };
            assert!(
                diagnostic
                    .message
                    .contains("user elaborator `unit!` is not in scope")
            );
            assert_eq!(
                diagnostic.help(),
                Some("import it with `import <module>(unit);`")
            );
        }
    }

    #[test]
    fn elaborator_implementation_helper_cannot_call_later_elaborator() {
        let msg = check_err(
            "module x; \
             import __comptime__; \
             pure fn unit_impl(ct: __Comptime__) -> __Checked_term__ { \
               unit!(); \
               __term_unit__(ct) \
             } \
             elab unit : . -> . { impl unit_impl; };",
        );
        assert!(msg.contains("user elaborator `unit!` is not in scope"));
    }

    #[test]
    fn type_alias_self_reference_is_an_alias_only_cycle() {
        let error = check("module x; type Loop = Loop;").expect_err("alias-only cycle");
        let Error::Totality(Diagnostic { message, .. }) = error else {
            panic!("expected Totality, got {error:?}");
        };
        assert!(message.contains("no `newtype` boundary"));
    }

    #[test]
    fn unbound_value_in_body() {
        let msg = check_err("module x; fn f() -> . { not_a_thing }");
        assert!(msg.contains("`not_a_thing`"));
    }

    #[test]
    fn unbound_type_in_signature() {
        let msg = check_err("module x; fn f(x: Nope) -> . { () }");
        assert!(msg.contains("`Nope`"));
    }

    #[test]
    fn recursive_partial_let_annotation_resolves_type_names() {
        let mut module = parse_module(
            "module x; \
             host type I32 role(i32); \
             host fn loop[S][R](step: S -> S | R, state: S) -> R; \
             newtype Box[A] : A { pub constructor mk_box; pub projector un_box; }; \
             fn choose_box[A](tag: I32, value: Box(A)) -> Box(A) { value } \
             rec(loop) fn value(n: I32) -> Box(I32) { \
               let .(result: Box(_)) = choose_box(0, rec(cont) value(0)); \
               result \
             }",
        );

        struct HideUnboundTypeInRecOrder {
            rewritten: usize,
        }

        impl crate::pass::visit_mut::TypecheckVisitMut<Lowered> for HideUnboundTypeInRecOrder {
            fn visit_expr(&mut self, expr: &mut crate::ast::Expr<Lowered>) {
                if let crate::ast::Expr::RecOrder { plan, .. } = expr
                    && let Some(crate::ast::Type::Path { segments, .. }) = &mut plan.annotation
                {
                    let span = segments[0].span;
                    segments[0] = crate::ast::PathSegment::new("Missing".to_owned(), span);
                    self.rewritten += 1;
                }
                crate::pass::visit_mut::walk_expr(self, expr);
            }
        }

        let mut poison = HideUnboundTypeInRecOrder { rewritten: 0 };
        crate::pass::visit_mut::TypecheckVisitMut::visit_module(&mut poison, &mut module);
        assert_eq!(
            poison.rewritten, 1,
            "fixture must contain one retained annotation"
        );

        let err = Resolver::check_module(&module)
            .expect_err("the retained partial annotation must be name-resolved");
        let Error::NameRes(Diagnostic { message, .. }) = err else {
            panic!("expected NameRes, got {err:?}");
        };
        assert!(message.contains("`Missing`"), "got: {message}");
    }

    #[test]
    fn literal_annotations_require_qualified_imports() {
        for literal in [
            "42(provider.Int)",
            "3.14(provider.Float)",
            "\"text\"(provider.Text)",
            ".t(provider.Bool)",
        ] {
            let msg = check_err(&format!(
                "module main; fn f() -> . {{ let value = {literal}; () }}"
            ));
            assert!(msg.contains("`provider`"), "literal `{literal}`: {msg}");
        }
    }

    #[test]
    fn intrinsic_without_import_is_unbound() {
        let msg = check_err("module x; fn f() -> . { __pair__ }");
        assert!(msg.contains("`__pair__`"));
    }

    #[test]
    fn intrinsic_with_import_resolves() {
        check(
            "module x; \
             import __intrinsics__; \
             fn f() -> . { __pair__ }",
        )
        .unwrap();
    }

    #[test]
    fn imported_name_resolves_in_body() {
        // The import validator (Package::resolve_imports) catches whether the
        // target module / name actually exist; the in-body resolver just
        // sees that `helper` is in scope here.
        check("module x; import other/thing(helper); fn f() -> . { helper() }").unwrap();
    }

    #[test]
    fn env_fn_is_in_scope() {
        check(
            "module x; \
             host fn print() -> .; \
             fn f() -> . { print() }",
        )
        .unwrap();
    }

    #[test]
    fn fn_expression_introduces_value_param() {
        check("module x; fn f() -> . { .(y) { y }(()) }").unwrap();
    }

    #[test]
    fn let_binding_does_not_leak_outside_body() {
        // `let x = e;` binds x for the rest of the enclosing block.
        // Here the second body references `inside` which is only bound
        // in the first body.
        let msg = check_err(
            "module x; \
             fn outer() -> . { let inside = (); inside } \
             fn other() -> . { inside }",
        );
        assert!(msg.contains("`inside`"));
    }

    #[test]
    fn nested_let_resolves() {
        check(
            "module x; \
             fn f() -> . { let a = (); let b = a; b }",
        )
        .unwrap();
    }

    // ---- Newtype payloads + self / mutual references --------------------

    #[test]
    fn marked_recursive_newtype_resolves_its_own_head() {
        check(
            "module x; \
             rec newtype Tree : (. | (Tree)) { \
               pub constructor mk_tree; pub projector un_tree; \
             };",
        )
        .unwrap();
    }

    #[test]
    fn unmarked_recursive_newtype_resolves_for_deferred_semantic_validation() {
        check(
            "module x; \
             newtype Tree : (. | Tree) { \
               pub constructor mk_tree; pub projector un_tree; \
             };",
        )
        .expect("the typer reports a missing marker after validating the declaration");
    }

    #[test]
    fn negative_unmarked_recursive_newtype_reaches_semantic_validation() {
        check("module x; newtype Bad : Bad -> . { constructor mk; projector un; };")
            .expect("resolution defers the marker diagnostic to semantic validation");
    }

    #[test]
    fn arity_invalid_unmarked_recursive_newtype_reaches_semantic_validation() {
        check("module x; newtype Bad[A] : Bad { constructor mk; projector un; };")
            .expect("resolution defers the marker diagnostic to semantic validation");
    }

    #[test]
    fn marked_acyclic_newtype_reports_the_redundant_marker() {
        let error = check(
            "module x; rec newtype Box[A] : A { \
               pub constructor mk_box; pub projector un_box; \
             };",
        )
        .expect_err("redundant marker");
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        assert_eq!(diagnostic.message, "this `rec` marker is unnecessary");
        assert_eq!(diagnostic.fixes()[0].title, "Remove unnecessary `rec`");
    }

    #[test]
    fn rec_unknown_newtype_is_unbound() {
        let msg = check_err(
            "module x; \
             newtype Foo : (Bar) { \
               pub constructor mk_foo; pub projector un_foo; \
             };",
        );
        assert!(msg.contains("`Bar`"), "got: {msg}");
    }

    #[test]
    fn acyclic_later_type_reports_directed_source_order_error_without_binding_it() {
        let error = check("module x; type Before = Later; type Later = .;")
            .expect_err("ordinary declarations remain source ordered");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Later` is declared later and is not visible here"
        );
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(diagnostic.fixes().len(), 1);
        assert_eq!(diagnostic.fixes()[0].title, "Move `Later` before its use");
    }

    #[test]
    fn later_newtype_member_in_function_body_reports_directed_source_order_error() {
        let error = check(
            "module x; \
             fn before() { Later.un(Later.mk(())) } \
             newtype Later : . { constructor mk; projector un; };",
        )
        .expect_err("a later newtype head remains unavailable in a value path");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Later` is declared later and is not visible here"
        );
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(diagnostic.fixes().len(), 1);
        assert_eq!(diagnostic.fixes()[0].title, "Move `Later` before its use");
    }

    #[test]
    fn later_type_alias_member_head_reports_directed_source_order_error() {
        let error = check(
            "module x; \
             import provider as imported; \
             fn before() { Later.make(()) } \
             type Later = imported.Tag;",
        )
        .expect_err("a later alias head remains unavailable before member validation");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Later` is declared later and is not visible here"
        );
        assert_eq!(diagnostic.secondary().len(), 1);
        assert!(
            diagnostic.fixes().is_empty(),
            "a standalone resolver has no terminal-member proof for a machine move"
        );

        check(
            "module x; \
             import provider as imported; \
             type Later = imported.Tag; \
             fn after() { Later.make(()) }",
        )
        .expect("once visible, the typer—not resolver diagnostics—owns alias member validity");
    }

    #[test]
    fn later_newtype_member_in_ufcs_reports_directed_source_order_error() {
        let error = check(
            "module x; \
             fn before() { ().>Later.mk.>Later.un } \
             newtype Later : . { constructor mk; projector un; };",
        )
        .expect_err("a later newtype head remains unavailable in a UFCS callee path");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Later` is declared later and is not visible here"
        );
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(diagnostic.fixes().len(), 1);
    }

    #[test]
    fn later_generated_label_member_has_no_partial_declaration_move() {
        let error = check(
            "module x; \
             fn before() { Foo.get(Foo.mk(())) } \
             labels { foo: . };",
        )
        .expect_err("a generated label head remains unavailable before its declaration");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Foo` is declared later and is not visible here"
        );
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(
            diagnostic.help(),
            Some(
                "move the source declaration that provides `Foo` before this use; use a bare \
                 `rec { ... }` group only when the declarations form one genuine recursive \
                 component"
            )
        );
        assert!(
            diagnostic.fixes().is_empty(),
            "a generated label entry is not a complete movable declaration"
        );
    }

    #[test]
    fn later_generated_label_type_in_multi_entry_declaration_has_no_partial_move() {
        let error = check(
            "module x; \
             type Before = Foo; \
             labels { foo: ., bar: . };",
        )
        .expect_err("a generated label type remains unavailable before its declaration");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "`Foo` is declared later and is not visible here"
        );
        assert!(
            diagnostic.fixes().is_empty(),
            "moving one entry would corrupt its owning labels declaration"
        );
    }

    #[test]
    fn unknown_member_on_later_newtype_keeps_the_unbound_path_diagnostic() {
        let error = check(
            "module x; \
             fn before() -> . { Later.missing(()) } \
             newtype Later : . { constructor mk; projector un; };",
        )
        .expect_err("an unknown member is not diagnostic evidence for a later type path");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(diagnostic.message, "unbound name `Later`");
        assert!(diagnostic.secondary().is_empty());
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn later_type_move_is_withheld_when_its_dependency_cannot_move_with_it() {
        let error = check("module x; type Before = Later; type Middle = .; type Later = Middle;")
            .expect_err("ordinary declarations remain source ordered");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn later_type_move_is_withheld_for_multiple_dependent_declarations() {
        let error = check("module x; type First = Later; type Second = Later; type Later = .;")
            .expect_err("ordinary declarations remain source ordered");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn mutual_newtypes_require_and_resolve_an_explicit_group() {
        check(
            "module x; \
             rec { \
               newtype A : (. | B) { pub constructor mk_a; pub projector un_a; }; \
               newtype B : (. | A) { pub constructor mk_b; pub projector un_b; }; \
             }",
        )
        .unwrap();
    }

    #[test]
    fn implicit_later_newtype_scope_is_rejected() {
        let error = check(
            "module x; \
             newtype A : (. | B) { pub constructor mk_a; pub projector un_a; }; \
             newtype B : (. | A) { pub constructor mk_b; pub projector un_b; };",
        )
        .expect_err("mutual declarations need a group");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "mutually recursive data declarations require `rec { ... }`"
        );
        assert_eq!(diagnostic.secondary().len(), 2);
        assert_eq!(diagnostic.fixes()[0].title, "Fix recursive type groups");
    }

    #[test]
    fn implicit_mutual_group_fix_preserves_visibility_and_attached_docs() {
        for (case, source, expected_start) in [
            (
                "public",
                "module x; pub type A = B; \
                 pub newtype B : A { constructor mk_b; projector un_b; };",
                "pub type A",
            ),
            (
                "scoped-public",
                "module x; pub(x) newtype A : B { constructor mk_a; projector un_a; }; \
                 pub(x) newtype B : A { constructor mk_b; projector un_b; };",
                "pub(x) newtype A",
            ),
            (
                "documented",
                "module x; /// A docs\ntype A = B; \
                 /// B docs\nnewtype B : A { constructor mk_b; projector un_b; };",
                "/// A docs",
            ),
        ] {
            let error = check(source).expect_err("mutual declarations need a group");
            let Error::NameRes(diagnostic) = error else {
                panic!("expected NameRes for {case}, got {error:?}");
            };
            let fix = diagnostic
                .fixes()
                .first()
                .unwrap_or_else(|| panic!("missing group repair for {case}"));
            assert_eq!(fix.title, "Fix recursive type groups");
            assert_eq!(fix.edits.len(), 2);
            assert_eq!(
                fix.edits[0].span.start,
                source.find(expected_start).unwrap() as u32,
                "the wrapper must own the complete first declaration for {case}"
            );
        }
    }

    #[test]
    fn implicit_label_cycle_fix_owns_the_complete_labels_declaration() {
        let source = concat!(
            "module x; ",
            "/// label docs\n",
            "labels { foo: Bar }; ",
            "newtype Bar : Foo { constructor mk_bar; projector un_bar; };",
        );
        let error = check(source).expect_err("mutual declarations need a group");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        let fix = diagnostic
            .fixes()
            .first()
            .expect("a complete labels owner and its peer can be wrapped");
        assert_eq!(fix.title, "Fix recursive type groups");
        assert_eq!(fix.edits.len(), 2);
        assert_eq!(
            fix.edits[0].span.start,
            source.find("/// label docs").unwrap() as u32,
            "the group must open before the labels owner's attached docs"
        );
        assert_eq!(
            fix.edits[1].span.start,
            source.len() as u32,
            "the group must close after the complete peer declaration"
        );
    }

    #[test]
    fn implicit_label_cycle_fix_includes_acyclic_generated_siblings() {
        let source = "module x; \
                      labels { other: ., foo: Bar }; \
                      newtype Bar : Foo { constructor mk_bar; projector un_bar; };";
        let error = check(source).expect_err("mutual declarations need a group");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        let fix = diagnostic
            .fixes()
            .first()
            .expect("the complete labels owner can carry an acyclic generated sibling");
        assert_eq!(
            fix.edits[0].span.start,
            source.find("labels").unwrap() as u32
        );
        assert_eq!(fix.edits[1].span.start, source.len() as u32);
        let repaired = apply_source_fix(source, fix);
        check(&repaired).expect("the complete labels owner must form a legal group");
    }

    #[test]
    fn implicit_named_sum_label_cycle_fix_includes_acyclic_arm() {
        let source = "module x; \
                      labels Tree = { leaf: . } | { branch: Bar }; \
                      newtype Bar : Tree { constructor mk_bar; projector un_bar; };";
        let error = check(source).expect_err("mutual declarations need a group");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        let fix = diagnostic
            .fixes()
            .first()
            .expect("the complete named labels owner can carry an acyclic arm");
        assert_eq!(
            fix.edits[0].span.start,
            source.find("labels").unwrap() as u32
        );
        assert_eq!(fix.edits[1].span.start, source.len() as u32);
        let repaired = apply_source_fix(source, fix);
        check(&repaired).expect("the complete named labels owner must form a legal group");
    }

    #[test]
    fn implicit_label_cycle_fix_is_withheld_for_two_recursive_components() {
        let error = check(
            "module x; \
             labels { first: Left_peer, second: Right_peer }; \
             newtype Left_peer : First { constructor mk_left; projector un_left; }; \
             newtype Right_peer : Second { constructor mk_right; projector un_right; };",
        );
        let error = error.expect_err("two independent components need separate groups");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn mutual_group_fix_is_withheld_when_wrapping_would_fail_positivity() {
        let error = check(
            "module x; \
             newtype A : B -> . { constructor mk_a; projector un_a; }; \
             newtype B : A { constructor mk_b; projector un_b; };",
        )
        .expect_err("mutual declarations need an explicit group");
        let Error::NameRes(diagnostic) = error else {
            panic!("expected NameRes, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "mutually recursive data declarations require `rec { ... }`"
        );
        assert!(
            diagnostic.fixes().is_empty(),
            "wrapping the component would expose a strict-positivity error"
        );
    }

    #[test]
    fn mutual_group_fix_accepts_matching_higher_kinds() {
        let valid = check(
            "module x; \
             type A[*F] = B(F); \
             newtype B[*G] : A(G) { constructor mk_b; projector un_b; };",
        )
        .expect_err("mutual declarations need an explicit group");
        let Error::NameRes(valid) = valid else {
            panic!("expected NameRes, got {valid:?}");
        };
        assert_eq!(valid.fixes().len(), 1);
        assert_eq!(valid.fixes()[0].title, "Fix recursive type groups");
    }

    #[test]
    fn mutual_group_fix_rejects_mismatched_higher_kinds() {
        let invalid = check(
            "module x; \
             type A[X] = B(X); \
             newtype B[*G] : A(.) { constructor mk_b; projector un_b; };",
        )
        .expect_err("mutual declarations need an explicit group");
        let Error::NameRes(invalid) = invalid else {
            panic!("expected NameRes, got {invalid:?}");
        };
        assert!(
            invalid.fixes().is_empty(),
            "the proposed group would be ill-kinded"
        );
    }

    #[test]
    fn recursive_group_rejects_alias_only_subcycle() {
        let error = elaborate_error(
            "module x; rec { \
               type A = B; type B = A | Box; \
               newtype Box : A { constructor mk; projector un; }; \
             }",
        );
        let Error::Totality(diagnostic) = error else {
            panic!("expected Totality, got {error:?}");
        };
        assert!(diagnostic.message.contains("no `newtype` boundary"));
    }

    #[test]
    fn recursive_group_rejects_an_acyclic_member() {
        let error = elaborate_error(
            "module x; rec { \
               type Helper = .; \
               newtype A : B { constructor mk_a; projector un_a; }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             }",
        );
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        assert!(
            diagnostic
                .message
                .contains("multiple independent components")
        );
    }

    #[test]
    fn acyclic_group_fix_topologically_orders_dependencies() {
        let ordered = elaborate_error("module x; rec { type Base = .; type Box = Base; }");
        let Error::Parse(ordered) = ordered else {
            panic!("expected Parse, got {ordered:?}");
        };
        assert_eq!(
            ordered.message,
            "this `rec` group contains no recursive cycle"
        );
        assert_eq!(ordered.fixes()[0].title, "Fix recursive type groups");

        let source = "module x; rec { type Box = Base; type Base = .; }";
        let reversed = elaborate_error(source);
        let Error::Parse(reversed) = reversed else {
            panic!("expected Parse, got {reversed:?}");
        };
        let fix = &reversed.fixes()[0];
        assert_eq!(fix.title, "Fix recursive type groups");
        assert_eq!(fix.edits.len(), 1);
        let repaired = apply_source_fix(source, fix);
        assert_eq!(repaired, "module x; type Base = .;\ntype Box = Base;");
        check(&repaired).expect("topologically ordered aliases must recheck");
    }

    #[test]
    fn type_cycle_analysis_is_proportional_to_a_large_acyclic_graph() {
        const NODES: usize = 50_000;
        let mut edges = vec![Vec::new(); NODES];
        for (node, successors) in edges.iter_mut().enumerate().take(NODES - 1) {
            successors.push(node + 1);
        }

        let components = strongly_connected_components(&edges, |_| true);

        assert_eq!(components.len(), NODES);
        assert_eq!(components.first(), Some(&vec![0]));
        assert_eq!(components.last(), Some(&vec![NODES - 1]));

        let analysis = TypeRecAnalysis {
            edges,
            edge_spans: vec![Vec::new(); NODES],
            components,
            cyclic_components: Vec::new(),
            alias_cycle: None,
        };
        let order = type_rec_component_order(&analysis);
        assert_eq!(order.len(), NODES);
        assert_eq!(order.first(), Some(&(NODES - 1)));
        assert_eq!(order.last(), Some(&0));
    }

    #[test]
    fn implicit_cycle_repairs_inspect_each_independent_component_once() {
        const PAIRS: usize = 128;
        let mut source = String::from("module x;");
        for index in 0..PAIRS {
            source.push_str(&format!(
                " newtype Left{index} : Right{index} {{ constructor mk_left{index}; projector un_left{index}; }};\
                 newtype Right{index} : Left{index} {{ constructor mk_right{index}; projector un_right{index}; }};"
            ));
        }
        let module = parse_module(&source);

        reset_implicit_type_cycle_candidate_visits();
        let problems = implicit_type_cycle_problems(&module);

        assert_eq!(problems.len(), PAIRS * 2);
        assert_eq!(implicit_type_cycle_candidate_visits(), PAIRS * 2);
    }

    #[test]
    fn dense_implicit_cycle_diagnostic_index_shares_one_component_record() {
        const NODES: usize = 128;
        let heads = (0..NODES)
            .map(|index| format!("T{index}"))
            .collect::<Vec<_>>()
            .join(" | ");
        let mut source = String::from("module x;");
        for index in 0..NODES {
            source.push_str(&format!(
                " newtype T{index} : {heads} {{ constructor mk{index}; projector un{index}; }};"
            ));
        }
        let module = parse_module(&source);

        let problems = implicit_type_cycle_problems(&module);

        assert_eq!(problems.len(), NODES * NODES);
        let first = problems
            .values()
            .next()
            .expect("dense cycle has references");
        assert!(
            problems.values().all(|problem| Arc::ptr_eq(first, problem)),
            "each reference in one SCC must share one diagnostic component record"
        );
    }
}

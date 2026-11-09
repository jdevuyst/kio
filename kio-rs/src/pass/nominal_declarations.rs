//! Shared nominal declaration construction and recursive-scope normalization.
//!
//! Callers convert authored types into their checking phase before entering
//! this module. Recursive validation uses the shared resolver analysis and
//! retains the distinction between one compact declaration and a written group
//! only long enough to validate its source scope and produce diagnostics.

use crate::ast::{
    DeferredRecLabelsDiagnostic, Item, LabelEntry, Labels, LabelsArm, Meta, Newtype, Phase, Type,
    TypeMember, TypeRecGroup, TypeRecMember, convert_type_member,
};
use crate::error::{Error, Fix};
use crate::pass::resolve::ResolvePhase;
use crate::span::Span;

pub(crate) fn label_source_owner_span<P: crate::ast::Phase>(labels: &Labels<P>) -> Span {
    // One owner span keeps the expansion atomic during cycle analysis;
    // editable_span separately retains the original edit eligibility.
    labels.editable_span.unwrap_or_else(|| {
        labels.doc.as_ref().map_or(labels.meta.span, |doc| {
            Span::new(doc.span.start, labels.meta.span.end)
        })
    })
}

/// Construct the ordinary nominal declaration for an explicit label entry.
/// The caller supplies its selected name and phase-appropriate payload.
pub(crate) fn label_nominal_declaration<P: crate::ast::Phase, Q: crate::ast::Phase>(
    owner: &Labels<P>,
    entry: &LabelEntry<P>,
    name: String,
    payload: Type<Q>,
) -> Newtype<Q> {
    Newtype {
        vis: owner.vis.clone(),
        rec_span: None,
        name,
        name_span: entry.name_span,
        type_params: entry.type_params.clone(),
        existential_params: entry.existential_params.clone(),
        payload,
        constructor: entry
            .members
            .as_ref()
            .map(|members| convert_type_member(&members.constructor))
            .unwrap_or_else(|| TypeMember {
                vis: owner.vis.clone(),
                name: "mk".to_owned(),
                span: entry.name_span,
                leading_trivia: Q::LeadingTrivia::default(),
            }),
        projector: entry
            .members
            .as_ref()
            .map(|members| convert_type_member(&members.projector))
            .unwrap_or_else(|| TypeMember {
                vis: owner.vis.clone(),
                name: "get".to_owned(),
                span: entry.name_span,
                leading_trivia: Q::LeadingTrivia::default(),
            }),
        meta: Meta::new(label_source_owner_span(owner)),
        editable_span: owner.editable_span,
        // A sole anonymous entry is its owner's only documented declaration.
        doc: if owner.type_alias_name.is_none() && owner.entries.len() == 1 {
            owner.doc.clone()
        } else {
            None
        },
    }
}

/// Build the named owner's written product/sum with explicit nominal paths.
/// Universal arguments retain the entry's own written names and spans;
/// existential binders remain solely inside the nominal declaration.
pub(crate) fn named_label_alias_body<P, Q>(
    arms: &[LabelsArm<P>],
    span: Span,
    mut nominal_path: impl FnMut(&LabelEntry<P>) -> Vec<String>,
) -> Type<Q>
where
    P: crate::ast::Phase,
    Q: crate::ast::Phase + Clone,
{
    let arm_types = arms
        .iter()
        .map(|arm| {
            let parts = arm
                .entries
                .iter()
                .map(|entry| {
                    let args = entry
                        .type_params
                        .iter()
                        .map(|param| {
                            Type::synth_path(vec![param.name.clone()], Vec::new(), param.span)
                        })
                        .collect();
                    Type::synth_path(nominal_path(entry), args, entry.meta.span)
                })
                .collect::<Vec<Type<Q>>>();
            crate::pass::typecheck_core::right_fold_product(&parts, arm.meta.span)
        })
        .collect::<Vec<_>>();
    crate::pass::typecheck_core::right_fold_sum(&arm_types, span)
}

/// Expand only the algebraic declaration RHS; ordinary type leaves retain
/// their exact source structure and binders.
pub(crate) fn named_nominal_alias_body<P: crate::ast::Phase + Clone>(
    owner: &Labels<P>,
    mut nominal_path: impl FnMut(&LabelEntry<P>) -> Vec<String>,
) -> Type<P> {
    fn expand<P: crate::ast::Phase + Clone>(
        rhs: &crate::ast::NominalRhs<P>,
        arms: &[LabelsArm<P>],
        nominal_path: &mut impl FnMut(&LabelEntry<P>) -> Vec<String>,
    ) -> Type<P> {
        use crate::ast::NominalRhs;
        match rhs {
            NominalRhs::Group(index) => {
                let arm = &arms[*index];
                named_label_alias_body(std::slice::from_ref(arm), arm.meta.span, nominal_path)
            }
            NominalRhs::Type(ty) => ty.clone(),
            NominalRhs::Product(left, right, meta) => Type::Product {
                left: Box::new(expand(left, arms, nominal_path)),
                right: Box::new(expand(right, arms, nominal_path)),
                meta: Meta::new(meta.span),
            },
            NominalRhs::Sum(left, right, meta) => Type::Sum {
                left: Box::new(expand(left, arms, nominal_path)),
                right: Box::new(expand(right, arms, nominal_path)),
                meta: Meta::new(meta.span),
            },
            NominalRhs::Grouped(inner, _) => expand(inner, arms, nominal_path),
        }
    }
    let arms = owner
        .type_alias_arms
        .as_deref()
        .expect("named nominal declaration carries compact groups");
    if let Some(rhs) = &owner.type_alias_rhs {
        expand(rhs, arms, &mut nominal_path)
    } else {
        named_label_alias_body(arms, owner.meta.span, nominal_path)
    }
}

pub(crate) fn ordinary_type_member_name<Q: ResolvePhase>(member: &TypeRecMember<Q>) -> &str {
    match member {
        TypeRecMember::TypeAlias(alias) => &alias.name,
        TypeRecMember::Newtype(newtype) => &newtype.name,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

fn ordinary_type_member_name_span<Q: ResolvePhase>(member: &TypeRecMember<Q>) -> Span {
    match member {
        TypeRecMember::TypeAlias(alias) => alias.name_span,
        TypeRecMember::Newtype(newtype) => newtype.name_span,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

#[cfg(feature = "surface")]
fn source_type_member_name_span<P: Phase>(member: &TypeRecMember<P>) -> Span {
    match member {
        TypeRecMember::TypeAlias(alias) => alias.name_span,
        TypeRecMember::Newtype(newtype) => newtype.name_span,
        TypeRecMember::Labels(labels, _) => labels.type_alias_span.unwrap_or_else(|| {
            Span::new(
                labels.meta.span.start,
                labels.meta.span.start.saturating_add(4),
            )
        }),
    }
}

fn ordinary_type_member_item<Q: ResolvePhase>(member: TypeRecMember<Q>) -> Item<Q> {
    match member {
        TypeRecMember::TypeAlias(alias) => Item::TypeAlias(alias),
        TypeRecMember::Newtype(newtype) => Item::Newtype(newtype),
        TypeRecMember::Labels(_, ext) => match ext {},
    }
}

pub(crate) fn alias_cycle_error<Q: ResolvePhase>(
    members: &[TypeRecMember<Q>],
    analysis: &crate::pass::resolve::TypeRecAnalysis,
) -> Option<Error> {
    let cycle = analysis.alias_cycle.as_ref()?;
    let first = cycle[0];
    let primary = analysis.edge_spans[first]
        .iter()
        .find(|(target, _)| cycle.contains(target))
        .map_or(members[first].meta().span, |(_, span)| *span);
    let mut error = Error::totality(
        primary,
        "recursive type component has no `newtype` boundary",
    );
    for &index in cycle {
        error = error.with_secondary(
            ordinary_type_member_name_span(&members[index]),
            format!(
                "transparent alias `{}` participates in this cycle",
                ordinary_type_member_name(&members[index])
            ),
        );
    }
    Some(error.with_help("every recursive type cycle must cross a nominal `newtype` boundary"))
}

fn emit_recursive_partition<Q: ResolvePhase>(
    members: Vec<TypeRecMember<Q>>,
    analysis: &crate::pass::resolve::TypeRecAnalysis,
    rec_span: Span,
    group_span: Span,
) -> Vec<Item<Q>> {
    let mut members = members.into_iter().map(Some).collect::<Vec<_>>();
    let mut out = Vec::new();
    for component_index in crate::pass::resolve::type_rec_component_order(analysis) {
        let component = &analysis.components[component_index];
        let cyclic = analysis.component_is_cyclic(component);
        let mut component_members = component
            .iter()
            .map(|&index| members[index].take().expect("member emitted exactly once"))
            .collect::<Vec<_>>();
        if !cyclic {
            debug_assert_eq!(component_members.len(), 1);
            out.push(ordinary_type_member_item(component_members.pop().unwrap()));
        } else if component_members.len() == 1 {
            let member = component_members.pop().unwrap();
            match member {
                TypeRecMember::Newtype(mut newtype) => {
                    newtype.rec_span = Some(rec_span);
                    out.push(Item::Newtype(newtype));
                }
                TypeRecMember::TypeAlias(_) => {
                    unreachable!("alias-only singleton cycles are rejected before emission")
                }
                TypeRecMember::Labels(_, ext) => match ext {},
            }
        } else {
            out.push(Item::TypeRecGroup(TypeRecGroup {
                members: component_members,
                doc: None,
                source_layout: None,
                rec_span: None,
                open_brace_span: None,
                close_brace_span: None,
                deferred_rec_labels_diagnostic: None,
                meta: Meta::new(group_span),
            }));
        }
    }
    out
}

/// Validate the supplied members under one compact declaration's recursive
/// scope, then emit their minimal ordinary declarations. Contract callers
/// supply members after applying their payload-visibility projection.
pub(crate) fn normalize_compact_declaration<P: Phase, Q: ResolvePhase>(
    d: &Labels<P>,
    members: Vec<TypeRecMember<Q>>,
) -> Result<Vec<Item<Q>>, Error> {
    let rec_span = d.rec_span;
    let span = d.meta.span;
    let head_span = d
        .type_alias_span
        .unwrap_or_else(|| Span::new(span.start, span.start.saturating_add(4)));
    let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
    let Some(rec_span) = rec_span else {
        if let Some(component) = analysis.cyclic_components.first() {
            let primary = component
                .iter()
                .find_map(|&from| {
                    analysis.edge_spans[from]
                        .iter()
                        .find(|(target, _)| component.contains(target))
                        .map(|(_, span)| *span)
                })
                .unwrap_or(span);
            // Keep the repaired declaration's entire atomic generated
            // scope together until the shared signature checker has
            // validated every member. It then publishes the tailored
            // marker diagnostic; any kind, arity, grounding, or
            // positivity defect wins first with its ordinary error.
            return Ok(vec![Item::TypeRecGroup(TypeRecGroup {
                members,
                doc: None,
                source_layout: None,
                rec_span: None,
                open_brace_span: None,
                close_brace_span: None,
                deferred_rec_labels_diagnostic: Some(DeferredRecLabelsDiagnostic {
                    reference_span: primary,
                    declaration_span: span,
                    head_span,
                }),
                meta: Meta::new(span),
            })]);
        }
        return Ok(members.into_iter().map(ordinary_type_member_item).collect());
    };
    if let Some(error) = alias_cycle_error(&members, &analysis) {
        return Err(error);
    }
    if analysis.cyclic_components.is_empty() {
        return Err(Error::parse(rec_span, "this `rec` marker is unnecessary")
            .with_secondary(
                head_span,
                "this nominal declaration has no recursive component",
            )
            .with_help("remove `rec` from this acyclic nominal declaration")
            .with_fix(
                Fix::machine_applicable(
                    "Remove unnecessary `rec`",
                    vec![crate::pass::resolve::remove_type_rec_marker_edit(
                        rec_span, span.start,
                    )],
                )
                .allowing_follow_on_reanalysis_outside(span),
            ));
    }
    for component in &analysis.cyclic_components {
        if !component
            .iter()
            .any(|&index| matches!(members[index], TypeRecMember::Newtype(_)))
        {
            return Err(Error::totality(
                members[component[0]].meta().span,
                "recursive type component has no `newtype` boundary",
            ));
        }
    }
    Ok(emit_recursive_partition(members, &analysis, rec_span, span))
}

/// Validate a written recursive group against its expanded ordinary members.
/// `owners` maps each expanded member to its written group member; that local
/// mapping supplies source diagnostics and is consumed before emitted items.
#[cfg(feature = "surface")]
pub(crate) fn normalize_recursive_group<P: Phase, Q: ResolvePhase>(
    group: &TypeRecGroup<P>,
    members: Vec<TypeRecMember<Q>>,
    owners: Vec<usize>,
) -> Result<Vec<Item<Q>>, Error> {
    let group_span = group.meta.span;
    let rec_span = group
        .rec_span
        .unwrap_or_else(|| Span::new(group_span.start, group_span.start.saturating_add(3)));
    let group_delimiters = (
        group.rec_span,
        group.open_brace_span,
        group.close_brace_span,
    );
    let source_member_spans = group
        .source_layout
        .as_ref()
        .map(|layout| layout.member_spans.clone())
        .unwrap_or_default();
    let source_member_marker_offsets = group
        .source_layout
        .as_ref()
        .map(|layout| layout.member_marker_offsets.clone())
        .unwrap_or_default();
    let source_separator_spans = group
        .source_layout
        .as_ref()
        .map(|layout| layout.separator_spans.clone())
        .unwrap_or_default();
    let trailing_comment_span = group
        .source_layout
        .as_ref()
        .and_then(|layout| layout.trailing_comment_span);
    let source_member_name_spans = group
        .members
        .iter()
        .map(source_type_member_name_span)
        .collect::<Vec<_>>();
    let source_member_names = group
        .members
        .iter()
        .map(|member| match member {
            TypeRecMember::TypeAlias(alias) => alias.name.clone(),
            TypeRecMember::Newtype(newtype) => newtype.name.clone(),
            TypeRecMember::Labels(labels, _) => labels
                .type_alias_name
                .clone()
                .unwrap_or_else(|| "<anonymous labels>".to_owned()),
        })
        .collect::<Vec<_>>();
    let source_partition_is_safe = source_member_spans.len() == group.members.len()
        && source_member_marker_offsets.len() == group.members.len();

    #[derive(Clone, Copy)]
    enum SingletonKind {
        Alias,
        Newtype,
        Labels,
    }

    let singleton = (group.members.len() == 1).then(|| {
        let member = &group.members[0];
        let kind = match member {
            TypeRecMember::TypeAlias(_) => SingletonKind::Alias,
            TypeRecMember::Newtype(_) => SingletonKind::Newtype,
            TypeRecMember::Labels(_, _) => SingletonKind::Labels,
        };
        (kind, source_type_member_name_span(member))
    });
    let nested_marker = group.members.iter().find_map(|member| match member {
        TypeRecMember::TypeAlias(_) => None,
        TypeRecMember::Newtype(newtype) => newtype
            .rec_span
            .map(|span| (span, newtype.meta.span.start, newtype.name_span)),
        TypeRecMember::Labels(labels, _) => labels.rec_span.map(|span| {
            (
                span,
                labels.meta.span.start,
                labels.type_alias_span.unwrap_or_else(|| {
                    Span::new(
                        labels.meta.span.start,
                        labels.meta.span.start.saturating_add(4),
                    )
                }),
            )
        }),
    });

    let surface_members = group.members.len();
    let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
    let source_analysis =
        crate::pass::resolve::source_partition_analysis(&analysis, &owners, surface_members);
    if let Some((kind, member_name_span)) = singleton {
        let add_singleton_marker = !matches!(kind, SingletonKind::Alias)
            && !analysis.cyclic_components.is_empty()
            && analysis.cyclic_components.iter().all(|component| {
                component
                    .iter()
                    .any(|&index| matches!(members[index], TypeRecMember::Newtype(_)))
            })
            && crate::pass::resolve::type_rec_marker_fix_is_locally_proven(&members, &analysis);
        let can_unwrap = analysis.cyclic_components.is_empty()
            || add_singleton_marker
            || nested_marker.is_some();
        let mut error = Error::parse(
            rec_span,
            "one recursive data declaration uses a `rec` modifier, not a group",
        )
        .with_secondary(
            group_delimiters.1.unwrap_or(rec_span),
            "the singleton group opens here",
        )
        .with_secondary(
            group_delimiters.2.unwrap_or(rec_span),
            "the singleton group closes here",
        )
        .with_secondary(member_name_span, "this is the group's only declaration")
        .with_help(if nested_marker.is_some() {
            "unwrap the group; the declaration already has its required `rec` modifier"
        } else if add_singleton_marker {
            match kind {
                SingletonKind::Newtype => {
                    "unwrap the group and write `rec type` for this recursive singleton"
                }
                SingletonKind::Labels => {
                    "unwrap the group and write `rec type` for this recursive singleton"
                }
                SingletonKind::Alias => unreachable!("aliases have no singleton marker"),
            }
        } else if analysis.cyclic_components.is_empty() {
            "unwrap this acyclic declaration without a `rec` marker"
        } else {
            "unwrap the group only after repairing the declaration's recursive type errors"
        });
        if can_unwrap
            && let Some(fix) = crate::pass::resolve::type_rec_unwrap_fix(
                group,
                add_singleton_marker && nested_marker.is_none(),
            )
        {
            error = error.with_fix(fix);
        }
        return Err(error);
    }
    if let Some((marker_span, member_start, member_name_span)) = nested_marker {
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
                vec![crate::pass::resolve::remove_type_rec_marker_edit(
                    marker_span,
                    member_start,
                )],
            )
            .allowing_follow_on_reanalysis_outside(group_span),
        ));
    }
    if let Some(error) = alias_cycle_error(&members, &analysis) {
        return Err(error);
    }
    if analysis.cyclic_components.is_empty() {
        let mut error = Error::parse(rec_span, "this `rec` group contains no recursive cycle");
        let order = source_analysis
            .as_ref()
            .map(crate::pass::resolve::type_rec_component_order)
            .unwrap_or_else(|| crate::pass::resolve::type_rec_component_order(&analysis));
        for component_index in order {
            let component = source_analysis
                .as_ref()
                .map_or(&analysis.components[component_index], |source| {
                    &source.components[component_index]
                });
            let index = component[0];
            error = error.with_secondary(
                source_analysis.as_ref().map_or_else(
                    || ordinary_type_member_name_span(&members[index]),
                    |_| source_member_name_spans[index],
                ),
                format!(
                    "`{}` is acyclic",
                    if source_analysis.is_some() {
                        &source_member_names[index]
                    } else {
                        ordinary_type_member_name(&members[index])
                    }
                ),
            );
        }
        error = error
            .with_help("move each dependency before its users and remove the unnecessary group");
        if source_partition_is_safe
            && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
            && let Some(source_analysis) = source_analysis.as_ref()
            && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                group_span,
                (open, close),
                &source_member_spans,
                &source_member_marker_offsets,
                &source_separator_spans,
                trailing_comment_span,
                source_analysis,
            )
        {
            error = error.with_fix(fix);
        }
        return Err(error);
    }
    if analysis.cyclic_components.len() != 1 {
        let mut error = Error::parse(
            rec_span,
            "this `rec` group contains multiple independent components",
        )
        .with_help("split each genuinely mutual component into its own `rec` group");
        for component in &analysis.cyclic_components {
            let lowered_index = component[0];
            let source_index = owners[lowered_index];
            let lowered_name = ordinary_type_member_name(&members[lowered_index]);
            let source_name = &source_member_names[source_index];
            let label = if lowered_name == source_name {
                format!("`{source_name}` starts an independent recursive component")
            } else {
                format!(
                    "generated declaration `{lowered_name}` starts an independent recursive component of `{source_name}`"
                )
            };
            error = error.with_secondary(source_member_name_spans[source_index], label);
        }
        if source_partition_is_safe
            && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
            && let Some(source_analysis) = source_analysis.as_ref()
            && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                group_span,
                (open, close),
                &source_member_spans,
                &source_member_marker_offsets,
                &source_separator_spans,
                trailing_comment_span,
                source_analysis,
            )
        {
            error = error.with_fix(fix);
        }
        return Err(error);
    }
    let recursive_component = &analysis.cyclic_components[0];
    let mut participating = vec![false; surface_members];
    for &index in recursive_component {
        participating[owners[index]] = true;
    }
    if let Some(unrelated) = participating.iter().position(|participates| !participates) {
        let mut error = Error::parse(
            rec_span,
            "this `rec` group contains multiple independent components",
        )
        .with_secondary(
            source_member_name_spans[unrelated],
            format!(
                "`{}` does not participate in the mutual component",
                source_member_names[unrelated]
            ),
        )
        .with_help("move the unrelated declaration outside the recursive group");
        if source_partition_is_safe
            && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
            && let Some(source_analysis) = source_analysis.as_ref()
            && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                group_span,
                (open, close),
                &source_member_spans,
                &source_member_marker_offsets,
                &source_separator_spans,
                trailing_comment_span,
                source_analysis,
            )
        {
            error = error.with_fix(fix);
        }
        return Err(error);
    }
    Ok(emit_recursive_partition(
        members, &analysis, rec_span, group_span,
    ))
}

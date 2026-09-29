use super::*;
use crate::pass::parser::{CursorSlot, ScopeSyntax, ToolingProbe, ToolingSyntax};

#[cfg(feature = "lsp")]
pub(crate) fn tooling_module<'a>(
    probe: &'a ToolingProbe<'_>,
) -> Option<std::borrow::Cow<'a, Module>> {
    if let Some(ToolingSyntax::Module(module)) = &probe.syntax {
        return Some(std::borrow::Cow::Borrowed(module));
    }
    Some(std::borrow::Cow::Owned(Module {
        path: probe.facts.module_path.clone()?,
        imports: probe.facts.prefix_imports.clone(),
        items: probe.facts.prefix_items.clone(),
        meta: crate::ast::Meta::new(Span::new(0, probe.source.len() as u32)),
        doc: None,
    }))
}

#[cfg(feature = "lsp")]
pub(crate) fn tooling_path_allowed(probe: &ToolingProbe<'_>, outer: Option<&Module>) -> bool {
    let Some(cursor) = &probe.facts.cursor else {
        return false;
    };
    let Some(head) = cursor.path.as_ref().and_then(|path| path.prefix.first()) else {
        return false;
    };
    let ns = if crate::naming::is_type_reference_name(head.as_str()) {
        Namespace::Type
    } else {
        Namespace::Value
    };
    let mut visible = namespace_candidates(probe, cursor.atom.prefix.end, ns);
    if let Some(outer) = outer {
        visible.extend(candidates(outer, u32::MAX, ns));
    }
    visible
        .into_iter()
        .find(|candidate| candidate.label == head.as_str())
        .is_some_and(|candidate| {
            !matches!(candidate.origin, CandidateOrigin::Local(_))
                && matches!(candidate.kind, CandidateKind::Type | CandidateKind::Module)
        })
}

/// Consumer-declared grammar controls expression syntax independently of the
/// implementation that the explicitly selected provider resolves.
#[cfg(feature = "lsp")]
pub(crate) fn expression_operators(
    module: &Module,
    offset: u32,
) -> Vec<crate::ast::OperatorGrammar> {
    let mut grammars = Vec::new();
    for import in &module.imports {
        if import.span.end > offset {
            continue;
        }
        if let ImportKind::Selective { items, .. } = &import.kind {
            grammars.extend(items.iter().filter_map(|item| {
                if let crate::ast::ImportItem::OperatorPattern { grammar, .. } = item {
                    Some(grammar.clone())
                } else {
                    None
                }
            }));
        }
    }
    for item in &module.items {
        if item.span().end > offset {
            continue;
        }
        match item {
            Item::Op(op, _) => {
                let crate::ast::OpBody::Normal { pattern, .. } = &op.body;
                grammars.push(crate::ast::OperatorGrammar::fixed(pattern));
            }
            Item::VariadicOperator(op, _) => {
                grammars.push(crate::ast::OperatorGrammar::variadic(&op.open, &op.spec))
            }
            _ => {}
        }
    }
    grammars
}

#[cfg(feature = "lsp")]
pub(crate) fn tooling_operators(
    probe: &ToolingProbe<'_>,
    grammars: &[crate::ast::OperatorGrammar],
) -> Vec<(String, String)> {
    let Some(cursor) = &probe.facts.cursor else {
        return Vec::new();
    };
    if probe.facts.suppression.is_some() || cursor.path.is_some() {
        return Vec::new();
    }
    let expr_start = match cursor.slot {
        CursorSlot::Value | CursorSlot::Argument => true,
        CursorSlot::OperatorContinuation => false,
        _ => return Vec::new(),
    };
    let mut result = Vec::new();
    for grammar in grammars {
        let key = grammar.dispatch_key();
        if key.expr_start != expr_start {
            continue;
        }
        if let Some(prefix) = &cursor.operator_prefix
            && (prefix.len() > key.leading_run.len()
                || prefix.iter().enumerate().any(|(index, part)| {
                    if index + 1 == prefix.len() {
                        !key.leading_run[index].starts_with(part)
                    } else {
                        key.leading_run[index] != *part
                    }
                }))
        {
            continue;
        }
        result.push((key.leading_run.join(" "), grammar.render()));
    }
    result.sort_unstable();
    result.dedup();
    result
}

/// Grammar choices narrowed by the structural owner and shared build contract.
pub(crate) fn tooling_keywords(probe: &ToolingProbe<'_>) -> Vec<&'static str> {
    let Some(cursor) = &probe.facts.cursor else {
        return Vec::new();
    };
    if probe.facts.suppression.is_some() {
        return Vec::new();
    }
    let mut words = cursor.keywords.clone();
    if matches!(cursor.slot, CursorSlot::Value | CursorSlot::Argument) {
        if recursive_owner(probe).is_none() {
            words.retain(|word| *word != "rec");
        }
    } else if cursor.slot == CursorSlot::RecursiveAnnotation {
        let Some((_, polymorphic)) = recursive_owner(probe) else {
            return Vec::new();
        };
        if !polymorphic {
            words.retain(|word| *word != "poly");
        }
    }
    if cursor.slot == CursorSlot::TargetId {
        words.extend(
            crate::build_target::BuildTarget::ALL
                .iter()
                .map(|target| target.id())
                .filter(|id| !cursor.target_ids.iter().any(|written| written == id)),
        );
    } else if cursor.slot == CursorSlot::TargetField
        && let Some(context) = &cursor.target
        && let Some(target) = crate::build_target::BuildTarget::from_id(&context.id)
    {
        words.extend(
            target
                .keys()
                .iter()
                .copied()
                .filter(|key| !context.fields.iter().any(|written| written == key)),
        );
    }
    words.sort_unstable();
    words.dedup();
    words
}

fn recursive_owner<'a>(
    probe: &'a ToolingProbe<'_>,
) -> Option<(&'a [crate::ast::PathSegment], bool)> {
    let offset = probe.facts.cursor.as_ref()?.atom.prefix.end;
    if probe
        .facts
        .regions
        .iter()
        .any(|region| contains(region.interior, offset) && region.lambda)
    {
        return None;
    }
    probe.facts.scope_prefix.iter().rev().find_map(|prefix| {
        if contains(prefix.interval, offset)
            && let ScopeSyntax::RecursiveMembers { names, polymorphic } = &prefix.syntax
        {
            Some((names.as_slice(), *polymorphic))
        } else {
            None
        }
    })
}

/// Lexical candidates projected from the current parser-owned cursor and scopes.
pub(crate) fn tooling_candidates(probe: &ToolingProbe<'_>) -> Vec<Candidate> {
    let Some(cursor) = probe.facts.cursor.as_ref() else {
        return Vec::new();
    };
    if probe.facts.suppression.is_some() || cursor.path.is_some() {
        return Vec::new();
    }
    let offset = cursor.atom.prefix.end;
    match cursor.slot {
        CursorSlot::Value => namespace_candidates(probe, offset, Namespace::Value),
        CursorSlot::Type => namespace_candidates(probe, offset, Namespace::Type),
        CursorSlot::Argument => {
            let mut result = namespace_candidates(probe, offset, Namespace::Value);
            result.extend(namespace_candidates(probe, offset, Namespace::Type));
            result
        }
        CursorSlot::RecursiveCallee => {
            let mut result = Vec::new();
            if let Some((members, _)) = recursive_owner(probe) {
                for member in members {
                    declaration(
                        &mut result,
                        &member.name,
                        member.span,
                        CandidateKind::Function,
                    );
                }
            }
            deduplicate(result)
        }
        CursorSlot::Grammar
        | CursorSlot::ModulePath
        | CursorSlot::ImportProvider
        | CursorSlot::RecursiveAnnotation
        | CursorSlot::BuildField
        | CursorSlot::NewtypeMember
        | CursorSlot::ImportSelection
        | CursorSlot::TargetId
        | CursorSlot::TargetField
        | CursorSlot::OperatorContinuation
        | CursorSlot::BlockLabel => Vec::new(),
    }
}

pub(super) fn namespace_candidates(
    probe: &ToolingProbe<'_>,
    offset: u32,
    ns: Namespace,
) -> Vec<Candidate> {
    if probe.facts.regions.is_empty()
        && let Some(syntax) = &probe.syntax
    {
        return match syntax {
            ToolingSyntax::Module(module) => candidates(module, offset, ns),
            ToolingSyntax::Expression(expr) => {
                let mut out = Vec::new();
                expr_scope(expr, offset, ns, &mut out);
                deduplicate(out)
            }
            ToolingSyntax::Declarations { imports, items } => {
                let mut out = Vec::new();
                preceding_items_scope(items, offset, None, ns, &mut out);
                imports_scope(imports, offset, ns, &mut out);
                deduplicate(out)
            }
            _ => Vec::new(),
        };
    }
    let mut out = Vec::new();
    let mut seen_placeholder = false;
    for prefix in probe.facts.scope_prefix.iter().rev() {
        if !contains(prefix.interval, offset) {
            continue;
        }
        match &prefix.syntax {
            ScopeSyntax::Placeholder { stem, slot_count } if ns == Namespace::Value => {
                if !seen_placeholder {
                    // An unfinished owner has no arity yet; offer its first slot.
                    placeholder_scope(stem, (*slot_count).max(1), &mut out);
                    seen_placeholder = true;
                }
            }
            ScopeSyntax::Signature(sig) => signature_scope(sig, true, offset, ns, &mut out),
            ScopeSyntax::TypeParameters(params) if ns == Namespace::Type => {
                type_params(params, offset, &mut out)
            }
            ScopeSyntax::RecursiveTypes(members) if ns == Namespace::Type => {
                for (name, _) in members {
                    declaration(&mut out, &name.name, name.span, CandidateKind::Type);
                }
            }
            ScopeSyntax::TypeDeclarations(names) if ns == Namespace::Type => {
                for name in names {
                    declaration(&mut out, &name.name, name.span, CandidateKind::Type);
                }
            }
            ScopeSyntax::Binding {
                name,
                pattern,
                type_params: params,
            } => {
                if ns == Namespace::Type {
                    type_params(params, offset, &mut out);
                } else if let Some((name, span)) = name {
                    binding_scope(name, *span, pattern.as_ref(), &mut out);
                } else if let Some(pattern) = pattern {
                    pattern_scope(pattern, offset, ns, true, &mut out);
                }
            }
            ScopeSyntax::RowLet(entries) if ns == Namespace::Value => {
                for entry in entries.iter().rev() {
                    local(
                        &mut out,
                        &entry.local,
                        entry.local_span,
                        CandidateKind::Variable,
                    );
                }
            }
            ScopeSyntax::TypeParameters(_)
            | ScopeSyntax::RecursiveMembers { .. }
            | ScopeSyntax::RecursiveTypes(_)
            | ScopeSyntax::TypeDeclarations(_)
            | ScopeSyntax::RowLet(_)
            | ScopeSyntax::Placeholder { .. } => {}
        }
    }
    let active_item = |items: &[Item]| {
        items.iter().position(|item| {
            contains(item.span(), offset)
                || probe.facts.regions.iter().any(|region| {
                    contains(region.interior, offset) && contains(item.span(), region.open.start)
                })
        })
    };
    if let Some(ToolingSyntax::Module(module)) = &probe.syntax {
        let active = active_item(&module.items);
        preceding_items_scope(&module.items, offset, active, ns, &mut out);
        imports_scope(&module.imports, offset, ns, &mut out);
    } else {
        preceding_items_scope(
            &probe.facts.prefix_items,
            offset,
            active_item(&probe.facts.prefix_items),
            ns,
            &mut out,
        );
        imports_scope(&probe.facts.prefix_imports, offset, ns, &mut out);
    }
    deduplicate(out)
}

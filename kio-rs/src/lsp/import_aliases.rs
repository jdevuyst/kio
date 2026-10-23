//! Source-local identities for explicit selective aliases. Semantic binders
//! continue to name provider declarations; refactorings use these local facts.

use std::collections::HashMap;

use crate::ast::{ImportItem, ImportKind, Module, Surface};
use crate::scope_walk::{CandidateOrigin, SourceBindingIndex};
use crate::span::Span;

#[derive(Debug)]
pub(crate) struct AliasBinding {
    pub name: String,
    pub declaration: Span,
    pub occurrences: Vec<Span>,
    provider: String,
    original: String,
}

#[derive(Debug, Default)]
pub(crate) struct AliasIndex {
    modules: HashMap<String, Vec<AliasBinding>>,
    occurrences: HashMap<String, HashMap<Span, usize>>,
    source_selectors: HashMap<String, HashMap<Span, usize>>,
    definitions: HashMap<(String, String), Span>,
}

impl AliasIndex {
    pub(crate) fn record_definitions(&mut self, module: &Module<Surface>, source: &str) {
        let Ok(references) = crate::tokens::source_references(source, module) else {
            return;
        };
        let owner = module.path.segments.join("/");
        for reference in references.into_iter().filter(|reference| {
            reference.declaration
                && matches!(
                    reference.kind,
                    crate::tokens::SourceNameKind::Value | crate::tokens::SourceNameKind::Macro
                )
        }) {
            self.definitions
                .insert((owner.clone(), reference.path), reference.span);
        }
    }

    pub(crate) fn record_module(&mut self, module: &Module<Surface>, source: &str) {
        let mut aliases = Vec::new();
        let mut source_selectors = HashMap::new();
        for import in &module.imports {
            let ImportKind::Selective { from, items } = &import.kind else {
                continue;
            };
            for item in items {
                let ImportItem::Name {
                    name,
                    span,
                    alias: Some(alias),
                    ..
                } = item
                else {
                    continue;
                };
                source_selectors.insert(*span, aliases.len());
                aliases.push(AliasBinding {
                    name: alias.local.clone(),
                    declaration: alias.span,
                    occurrences: vec![alias.span],
                    provider: from.segments.join("/"),
                    original: name.clone(),
                });
            }
        }
        if aliases.is_empty() {
            return;
        }
        let by_declaration: HashMap<_, _> = aliases
            .iter()
            .enumerate()
            .map(|(index, alias)| (alias.declaration, index))
            .collect();
        let Ok(references) = crate::tokens::source_dependencies(source, module) else {
            return;
        };
        let scope = SourceBindingIndex::new(module);
        for reference in references {
            let Some(CandidateOrigin::Import {
                provider,
                source,
                span,
            }) = scope.origin(&reference)
            else {
                continue;
            };
            if let Some(alias) = by_declaration.get(&span).map(|index| &mut aliases[*index])
                && alias.provider == provider
                && alias.original == source
            {
                alias.occurrences.push(reference.span);
            }
        }
        for alias in &mut aliases {
            alias
                .occurrences
                .sort_unstable_by_key(|span| (span.start, span.end));
            alias.occurrences.dedup();
        }
        let module = module.path.segments.join("/");
        self.source_selectors
            .insert(module.clone(), source_selectors);
        self.occurrences.insert(
            module.clone(),
            aliases
                .iter()
                .enumerate()
                .flat_map(|(index, alias)| alias.occurrences.iter().map(move |span| (*span, index)))
                .collect(),
        );
        self.modules.insert(module, aliases);
    }

    pub(crate) fn at(&self, module: &str, offset: u32) -> Option<(&AliasBinding, Span)> {
        self.modules.get(module)?.iter().find_map(|alias| {
            crate::lsp::util::smallest_containing_span(
                offset,
                alias.occurrences.iter().map(|span| (*span, alias)),
            )
            .map(|(span, alias)| (alias, span))
        })
    }

    pub(crate) fn contains(&self, module: &str, span: Span) -> bool {
        self.occurrences
            .get(module)
            .is_some_and(|occurrences| occurrences.contains_key(&span))
    }

    pub(crate) fn source_at(&self, module: &str, offset: u32) -> Option<(&AliasBinding, Span)> {
        let aliases = self.modules.get(module)?;
        crate::lsp::util::smallest_containing_span(
            offset,
            self.source_selectors
                .get(module)?
                .iter()
                .map(|(span, index)| (*span, &aliases[*index])),
        )
        .map(|(span, alias)| (alias, span))
    }

    pub(crate) fn authenticates_binder(
        &self,
        module: &str,
        span: Span,
        written: &str,
        binder: &crate::pass::typecheck_full::ResolvedBinder,
    ) -> bool {
        use crate::pass::typecheck_full::ResolvedBinder;

        let Some(index) = self
            .occurrences
            .get(module)
            .and_then(|occurrences| occurrences.get(&span))
        else {
            return false;
        };
        let alias = &self.modules[module][*index];
        let (provider, original) = match binder {
            ResolvedBinder::Fn { module_path, name }
            | ResolvedBinder::HostEnvFn { module_path, name }
            | ResolvedBinder::TypeAlias { module_path, name }
            | ResolvedBinder::HostType { module_path, name }
            | ResolvedBinder::Newtype { module_path, name } => (module_path, name),
            _ => return false,
        };
        written == alias.name && *provider == alias.provider && *original == alias.original
    }

    pub(crate) fn is_aliased_source_selector(&self, module: &str, span: Span) -> bool {
        self.source_selectors
            .get(module)
            .is_some_and(|selectors| selectors.contains_key(&span))
    }

    pub(crate) fn definition<'a>(&self, alias: &'a AliasBinding) -> Option<(&'a str, Span)> {
        let span = self
            .definitions
            .get(&(alias.provider.clone(), alias.original.clone()))?;
        Some((&alias.provider, *span))
    }
}

use std::collections::{BTreeMap, HashMap};

use crate::ast::{Item, Labels, Module, Surface, TypeRecMember};
use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelBinding {
    pub name: String,
    pub declaration_span: Span,
    pub declaration_entry_span: Span,
    pub occurrences: Vec<Span>,
    pub cursor_span: Span,
    pub cursor_is_reuse: bool,
}

#[derive(Debug, Clone)]
struct BindingState {
    declaration_span: Span,
    declaration_entry_span: Span,
    occurrences: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    name: String,
    span: Span,
    is_reuse: bool,
}

/// Module-local lookup table for explicit label declarations and their valid
/// reuse markers. The table is derived from a cached surface parse; requests
/// only perform a span lookup and never parse source text themselves.
#[derive(Debug, Clone, Default)]
pub struct LabelReuseIndex {
    bindings: HashMap<String, BindingState>,
    occurrences: BTreeMap<u32, Occurrence>,
    label_imports: BTreeMap<u32, (String, Span)>,
    protected_labels: BTreeMap<u32, Span>,
}

/// Live document text paired with the label index derived from that exact
/// version. Keeping them together prevents handlers from combining current
/// positions with a stale analysis index.
#[derive(Debug, Clone, Copy)]
pub struct LabelReuseSnapshot<'a> {
    pub source: &'a str,
    pub index: Option<&'a LabelReuseIndex>,
}

pub fn snapshot_for<'a>(
    path: &std::path::Path,
    analysis: &'a crate::cmd::check::LspAnalysis,
    overlay: Option<LabelReuseSnapshot<'a>>,
) -> Option<LabelReuseSnapshot<'a>> {
    overlay.or_else(|| {
        analysis.sources.get(path).map(|source| LabelReuseSnapshot {
            source,
            index: analysis.label_reuse_indexes.get(path).map(AsRef::as_ref),
        })
    })
}

/// Whether a typed position may be consulted when label syntax may have changed
/// under the cursor. Typed spans and identities belong to `analysis.sources`;
/// after an overlay edit, a label in either the analyzed or live source makes a
/// still-overlapping typed span stale. Surface-local declaration/reuse handling
/// may continue from the overlay's own index, but typed navigation waits for
/// fresh analysis.
pub fn typed_label_positions_are_current(
    path: &std::path::Path,
    byte_offset: u32,
    analysis: &crate::cmd::check::LspAnalysis,
    snapshot: LabelReuseSnapshot<'_>,
) -> bool {
    if analysis
        .sources
        .get(path)
        .is_some_and(|typed_source| typed_source == snapshot.source)
    {
        return true;
    }
    let Some(live_index) = snapshot.index else {
        return false;
    };
    live_index.label_at(byte_offset).is_none()
        && analysis
            .label_reuse_indexes
            .get(path)
            .is_none_or(|index| index.label_at(byte_offset).is_none())
}

impl LabelReuseIndex {
    pub fn from_module(module: &Module<Surface>, source: &str) -> Self {
        let tokens = crate::tokens::dump_module(source, module).unwrap_or_default();
        Self::from_module_with_tokens(module, &tokens)
    }

    pub(crate) fn from_module_with_tokens(
        module: &Module<Surface>,
        tokens: &[crate::tokens::ClassifiedToken],
    ) -> Self {
        let mut bindings: HashMap<String, BindingState> = HashMap::new();
        let mut occurrences = BTreeMap::new();

        for item in &module.items {
            match item {
                Item::Labels(labels, _) => {
                    collect_labels(labels, &mut bindings, &mut occurrences);
                }
                Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        if let TypeRecMember::Labels(labels, _) = member {
                            collect_labels(labels, &mut bindings, &mut occurrences);
                        }
                    }
                }
                _ => {}
            }
        }

        let mut label_imports = BTreeMap::new();
        for import_ in &module.imports {
            let crate::ast::ImportKind::Selective { items, .. } = &import_.kind else {
                continue;
            };
            for item in items {
                if let crate::ast::ImportItem::Label { name, span, .. } = item {
                    label_imports.insert(span.start, (name.clone(), *span));
                }
            }
        }

        let protected_labels = protected_label_spans(tokens);

        Self {
            bindings,
            occurrences,
            label_imports,
            protected_labels,
        }
    }

    pub fn binding_at(&self, byte_offset: u32) -> Option<LabelBinding> {
        let (_, occurrence) = self.occurrences.range(..=byte_offset).next_back()?;
        if byte_offset > occurrence.span.end {
            return None;
        }
        let binding = self.bindings.get(occurrence.name.as_str())?;
        Some(LabelBinding {
            name: occurrence.name.clone(),
            declaration_span: binding.declaration_span,
            declaration_entry_span: binding.declaration_entry_span,
            occurrences: binding.occurrences.clone(),
            cursor_span: occurrence.span,
            cursor_is_reuse: occurrence.is_reuse,
        })
    }

    /// True when the cursor is on a braced selective label item. Import-item
    /// rename is refused for the same reason as label declaration/reuse
    /// rename: changing one surface spelling without every label use would
    /// leave the program inconsistent.
    pub fn import_at(&self, byte_offset: u32) -> Option<(&str, Span)> {
        let (_, (name, span)) = self.label_imports.range(..=byte_offset).next_back()?;
        (byte_offset <= span.end).then_some((name.as_str(), *span))
    }

    /// True when the cursor is on any label-syntax spelling. Labels mint
    /// nominal declarations and may be imported independently of their
    /// generated type, so an ordinary identifier rename cannot preserve all
    /// of the coupled spellings safely.
    pub fn label_at(&self, byte_offset: u32) -> Option<Span> {
        let (_, span) = self.protected_labels.range(..=byte_offset).next_back()?;
        (byte_offset <= span.end).then_some(*span)
    }

    pub(crate) fn protect_label_span(&mut self, span: Span) {
        self.protected_labels.insert(span.start, span);
    }
}

fn collect_labels(
    labels: &Labels<Surface>,
    bindings: &mut HashMap<String, BindingState>,
    occurrences: &mut BTreeMap<u32, Occurrence>,
) {
    for entry in &labels.entries {
        if entry.is_reuse_marker() {
            let Some(binding) = bindings.get_mut(entry.name.as_str()) else {
                continue;
            };
            binding.occurrences.push(entry.name_span);
            occurrences.insert(
                entry.name_span.start,
                Occurrence {
                    name: entry.name.clone(),
                    span: entry.name_span,
                    is_reuse: true,
                },
            );
            continue;
        }
        if let Some(binding) = bindings.get_mut(entry.name.as_str()) {
            binding.occurrences.push(entry.name_span);
            continue;
        }
        bindings.insert(
            entry.name.clone(),
            BindingState {
                declaration_span: entry.name_span,
                declaration_entry_span: entry.meta.span,
                occurrences: vec![entry.name_span],
            },
        );
        occurrences.insert(
            entry.name_span.start,
            Occurrence {
                name: entry.name.clone(),
                span: entry.name_span,
                is_reuse: false,
            },
        );
    }
}

fn protected_label_spans<'a>(
    tokens: impl IntoIterator<Item = &'a crate::tokens::ClassifiedToken>,
) -> BTreeMap<u32, Span> {
    tokens
        .into_iter()
        .filter(|token| {
            matches!(
                token.kind,
                crate::tokens::TokenKind::EntityNameLabel
                    | crate::tokens::TokenKind::EntityNameLabelReference
                    | crate::tokens::TokenKind::EntityNameQualifiedLabelReference
            )
        })
        .map(|token| (token.span.start, token.span))
        .collect()
}

pub fn source_entry<'a>(source: &'a str, binding: &LabelBinding) -> Option<&'a str> {
    source.get(
        binding.declaration_entry_span.start as usize..binding.declaration_entry_span.end as usize,
    )
}

#[cfg(test)]
pub fn indexes_from_sources(
    sources: &HashMap<std::path::PathBuf, String>,
) -> HashMap<std::path::PathBuf, std::sync::Arc<LabelReuseIndex>> {
    sources
        .iter()
        .filter_map(|(path, source)| {
            crate::pass::parser::parse(source).ok().map(|module| {
                (
                    path.clone(),
                    std::sync::Arc::new(LabelReuseIndex::from_module(&module, source)),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_labels_are_protected_but_never_reuse_origins() {
        let source = concat!(
            "module pkg; labels { original: . };\n",
            "type {forward} = {original};\n",
            "labels Later = { forward: _ };\n",
        );
        let module = crate::pass::parser::parse(source).unwrap();
        let index = LabelReuseIndex::from_module(&module, source);
        let local = source.find("{forward}").unwrap() as u32 + 1;
        let target = source.find("{original}").unwrap() as u32 + 1;
        let reuse = source.find("forward: _").unwrap() as u32;
        for offset in [local, target, reuse] {
            assert!(index.label_at(offset).is_some());
            assert!(index.binding_at(offset).is_none());
        }
        let original = source.find("original:").unwrap() as u32;
        assert_eq!(index.binding_at(original).unwrap().name, "original");
    }

    #[test]
    fn typed_positions_are_stale_when_analyzed_label_becomes_ordinary() {
        let analyzed_source =
            "module pkg/main;\nlabels { field: . };\nfn f(field: Field) -> Field { {field=} }\n";
        let live_source =
            "module pkg/main;\nlabels { field: . };\nfn f(field: Field) -> Field {  field   }\n";
        let byte_offset = analyzed_source.rfind("field").expect("analyzed body label") as u32;
        assert_eq!(
            byte_offset,
            live_source.rfind("field").expect("live body reference") as u32,
            "fixture must keep the stale typed span at the live ordinary reference"
        );

        let path = std::path::PathBuf::from("/workspace/pkg/main.kio");
        let mut sources = HashMap::new();
        sources.insert(path.clone(), analyzed_source.to_owned());
        let analysis = crate::cmd::check::LspAnalysis {
            position_index: crate::pass::typecheck_full::PositionIndex::new(),
            file_to_module: BTreeMap::new(),
            label_reuse_indexes: indexes_from_sources(&sources),
            sources,
            generated_label_nominals: Default::default(),
            root_package_lowered: crate::pass::resolve::Package::from_parts(BTreeMap::new(), None),
            warnings: Vec::new(),
        };
        let live_module = crate::pass::parser::parse(live_source).expect("parse live source");
        let live_index = LabelReuseIndex::from_module(&live_module, live_source);

        assert!(
            analysis.label_reuse_indexes[&path]
                .label_at(byte_offset)
                .is_some(),
            "the analyzed cursor must be label syntax"
        );
        assert!(
            live_index.label_at(byte_offset).is_none(),
            "the live cursor must be an ordinary value reference"
        );
        assert!(
            !typed_label_positions_are_current(
                &path,
                byte_offset,
                &analysis,
                LabelReuseSnapshot {
                    source: live_source,
                    index: Some(&live_index),
                },
            ),
            "an ordinary live token cannot authenticate a typed binder from stale label syntax"
        );
    }

    #[test]
    fn marker_resolves_to_first_explicit_local_declaration() {
        let source = "module x; labels { field: . }; labels Row = { field: _, other: . };";
        let marker = source.rfind("field").expect("marker") as u32;
        let module = crate::pass::parser::parse(source).expect("parse");
        let binding = LabelReuseIndex::from_module(&module, source)
            .binding_at(marker)
            .expect("binding");
        assert!(binding.cursor_is_reuse);
        assert_eq!(
            binding.declaration_span.start,
            source.find("field").expect("declaration") as u32
        );
        assert_eq!(binding.occurrences.len(), 2);
    }

    #[test]
    fn marker_inside_recursive_group_resolves_to_earlier_group_declaration() {
        let source = "module x; rec { \
            labels A = { to_b: B, shared: . }; \
            labels B = { back: A, shared: _ }; \
        }";
        let marker = source.rfind("shared").expect("marker") as u32;
        let declaration = source.find("shared").expect("declaration") as u32;
        let module = crate::pass::parser::parse(source).expect("parse");
        let binding = LabelReuseIndex::from_module(&module, source)
            .binding_at(marker)
            .expect("group reuse binding");
        assert!(binding.cursor_is_reuse);
        assert_eq!(binding.declaration_span.start, declaration);
        assert_eq!(binding.occurrences.len(), 2);
    }

    #[test]
    fn selective_label_import_is_rename_protected() {
        let source = "module x; import origin({field}); fn f() -> . { {field=} }";
        let module = crate::pass::parser::parse(source).expect("parse");
        let index = LabelReuseIndex::from_module(&module, source);
        let offset = source.find("field").expect("import") as u32;
        let (name, span) = index.import_at(offset).expect("label import");
        assert_eq!(name, "field");
        assert_eq!(&source[span.start as usize..span.end as usize], "field");
        assert!(index.binding_at(offset).is_none());
        assert!(index.label_at(offset).is_some());
        let use_offset = source.rfind("field").expect("label use") as u32;
        assert!(index.label_at(use_offset).is_some());
    }

    #[test]
    fn imported_or_forward_marker_has_no_local_binding() {
        let source = "module x; labels Row = { field: _, other: . }; labels { field: . };";
        let marker = source.find("field").expect("marker") as u32;
        let module = crate::pass::parser::parse(source).expect("parse");
        assert!(
            LabelReuseIndex::from_module(&module, source)
                .binding_at(marker)
                .is_none()
        );
    }
}

use crate::ast::{BlockExposure, ImportItem, ImportKind, Item, Module, Surface, UserElaboratorDef};

pub(crate) struct BlockHeader {
    pub(crate) name: String,
    pub(crate) public_type: String,
    pub(crate) blocks: Vec<(BlockExposure, Option<String>)>,
}

impl BlockHeader {
    pub(crate) fn from(declaration: &UserElaboratorDef) -> Self {
        Self {
            name: declaration.name.clone(),
            public_type: crate::pretty::pretty_type(&declaration.call_ty),
            blocks: declaration
                .trailing_blocks
                .iter()
                .map(|block| {
                    (
                        block.exposure,
                        block.label.as_ref().map(|label| label.as_str().to_owned()),
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn matches_prefix(&self, labels: &[Option<String>]) -> bool {
        labels.len() <= self.blocks.len()
            && labels
                .iter()
                .zip(&self.blocks)
                .all(|(written, (_, expected))| written == expected)
    }

    pub(crate) fn valid_labels(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        self.blocks
            .iter()
            .enumerate()
            .all(|(index, (_, label))| match (index, label) {
                (0, None) => true,
                (0, Some(_)) | (_, None) => false,
                (_, Some(label)) => seen.insert(label),
            })
    }
}

/// The caller authenticates only the explicitly selected providers' header
/// snapshots. This presentation query neither parses bodies nor plans calls.
pub(crate) fn selected_block_header(
    module: &Module<Surface>,
    name: &str,
    offset: u32,
    mut provider: impl FnMut(&str) -> Option<Module<Surface>>,
) -> Option<BlockHeader> {
    let mut selected = None;
    for item in &module.items {
        if let Item::Elaborator(declaration, _) = item
            && declaration.name == name
            && declaration.name_span.start < offset
        {
            if selected.is_some() {
                return None;
            }
            selected = Some(BlockHeader::from(declaration));
        }
    }
    for import in &module.imports {
        if import.span.end > offset {
            continue;
        }
        let ImportKind::Selective { from, items } = &import.kind else {
            continue;
        };
        if !items
            .iter()
            .any(|item| matches!(item, ImportItem::Name { name: imported, .. } if imported == name))
        {
            continue;
        }
        let provider = provider(&from.segments.join("/"))?;
        for item in &provider.items {
            if let Item::Elaborator(declaration, _) = item
                && declaration.name == name
                && crate::pass::resolve::is_visible(&declaration.vis, &module.path)
            {
                if selected.is_some() {
                    return None;
                }
                selected = Some(BlockHeader::from(declaration));
            }
        }
    }
    selected.filter(|header| !header.blocks.is_empty() && header.valid_labels())
}

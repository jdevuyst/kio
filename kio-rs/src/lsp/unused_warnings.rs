use crate::ast::{DocComment, Import, ImportItem, ImportKind, Item, Module, TypeRecMember};
use crate::doc_markup::source::DocSource;
use crate::error::FixEdit;
use crate::pass::lexer::{Token, TokenKind, Trivia};
use crate::span::Span;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

pub(crate) fn source_warnings(
    modules: &[(PathBuf, Module)],
    sources: &HashMap<PathBuf, String>,
    file_to_module: &BTreeMap<PathBuf, String>,
    positions: &crate::pass::typecheck_full::PositionIndex,
    declarations: &crate::pass::full::LspSourceDeclarations,
    role_uses: &[(String, Span, String)],
    cancel: &super::cancel::CancellationToken,
) -> Option<Vec<crate::cmd::check::LspWarning>> {
    use crate::cmd::check::{LspWarning, LspWarningKind};
    use crate::error::Fix;
    use crate::pass::op_fold::ResolvedOperatorOrigin;
    use crate::pass::typecheck_full::ResolvedBinder;
    use crate::scope_walk::{CandidateOrigin, SourceBindingIndex};
    use crate::tokens::SourceNameKind;

    let mut operators_by_module: HashMap<&str, Vec<_>> = HashMap::new();
    for usage in &declarations.operator_uses {
        operators_by_module
            .entry(&usage.source_module)
            .or_default()
            .push(usage);
    }
    let mut roles_by_module: HashMap<&str, Vec<_>> = HashMap::new();
    for (module, span, binding) in role_uses {
        roles_by_module
            .entry(module)
            .or_default()
            .push((*span, binding.as_str()));
    }
    let mut warnings = Vec::new();
    for (file_path, module) in modules {
        if cancel.is_cancelled() {
            return None;
        }
        let source = sources
            .get(file_path)
            .expect("checked modules retain their source");
        let semantic_module = file_to_module
            .get(file_path)
            .expect("checked modules retain their identity");
        let (tokens, trailing) = crate::pass::lexer::lex_with_trailing(source)
            .expect("checked source lexes successfully");
        if !doc_dependency_uncertainty(source, &tokens, &trailing, module).is_empty() {
            continue;
        }
        let bindings = SourceBindingIndex::new(module);
        let mut graph = DeclarationGraph::for_module(module);
        let mut used_imports = HashSet::new();
        let mut mark_origin = |span, origin| {
            match origin {
                CandidateOrigin::Declaration(target) => {
                    if let (Some(owner), Some(target)) =
                        (item_at(module, span), item_at(module, target))
                    {
                        graph.add_dependency(owner, target);
                    }
                }
                CandidateOrigin::Import { span, .. } | CandidateOrigin::QualifiedImport(span) => {
                    used_imports.insert(span);
                }
                CandidateOrigin::Builtin(name) => {
                    // Repeated compiler-block imports form one idempotent set.
                    // Their resolver has no distinguished enabling occurrence.
                    for import in &module.imports {
                        if matches!(
                            (&import.kind, name),
                            (ImportKind::Intrinsics, "__intrinsics__")
                                | (ImportKind::Comptime, "__comptime__")
                        ) {
                            used_imports.insert(import.span);
                        }
                    }
                }
                CandidateOrigin::Local(_)
                | CandidateOrigin::Placeholder(_)
                | CandidateOrigin::NumberedBinding(_) => {}
            }
        };
        for mut reference in crate::tokens::source_dependencies(source, module)
            .expect("checked source retains its parsed dependency occurrences")
        {
            if cancel.is_cancelled() {
                return None;
            }
            if reference.declaration {
                continue;
            }
            if reference.kind == SourceNameKind::Value
                && matches!(
                    positions.binder_at(semantic_module, reference.span),
                    Some(
                        ResolvedBinder::TypeAlias { .. }
                            | ResolvedBinder::HostType { .. }
                            | ResolvedBinder::Newtype { .. }
                            | ResolvedBinder::TypeParam { .. }
                    )
                )
            {
                reference.kind = SourceNameKind::Type;
            }
            if let Some(origin) = bindings.origin(&reference) {
                mark_origin(reference.span, origin);
            }
        }
        if let Some(uses) = roles_by_module.get(semantic_module.as_str()) {
            for (span, binding) in uses {
                if let Some(origin) = bindings.selected_module_type_origin(binding, span.start) {
                    mark_origin(*span, origin);
                }
            }
        }
        let source_module = module
            .path
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join("/");
        if let Some(uses) = operators_by_module.get(source_module.as_str()) {
            for usage in uses {
                match usage.origin {
                    ResolvedOperatorOrigin::Local { declaration_span } => {
                        if let (Some(owner), Some(target)) = (
                            item_at(module, usage.use_span),
                            item_at(module, declaration_span),
                        ) {
                            graph.add_dependency(owner, target);
                        }
                    }
                    ResolvedOperatorOrigin::Import { import_span } => {
                        used_imports.insert(import_span);
                    }
                }
            }
        }
        for import in &module.imports {
            let items: Vec<_> = match &import.kind {
                ImportKind::Selective { items, .. } => items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            Some(index),
                            import_item_syntax(&tokens, item)
                                .expect("checked import owns its syntax"),
                            match item {
                                ImportItem::Name { span, .. }
                                | ImportItem::Label { span, .. }
                                | ImportItem::OperatorPattern { span, .. } => *span,
                            },
                        )
                    })
                    .collect(),
                _ => vec![(None, import.span, import.span)],
            };
            for (selection, span, identity) in items {
                if used_imports.contains(&identity) {
                    continue;
                }
                let fixes = import_removal(source, &tokens, import, selection)
                    .map(|edits| vec![Fix::machine_applicable("Remove unused import", edits)])
                    .unwrap_or_default();
                warnings.push(LspWarning {
                    kind: LspWarningKind::UnusedImport,
                    file_path: file_path.clone(),
                    span,
                    message: "unused import".to_owned(),
                    fixes,
                });
            }
        }
        let reachable = graph.reachable();
        for (index, item) in module.items.iter().enumerate() {
            if cancel.is_cancelled() {
                return None;
            }
            if reachable[index] {
                continue;
            }
            let Some(declaration) = private_declaration(source, &tokens, item) else {
                continue;
            };
            let fixes = if graph.has_surviving_reference(index) || declaration.removals.is_empty() {
                Vec::new()
            } else {
                vec![Fix::machine_applicable(
                    format!("Remove transitively unused private {}", declaration.kind),
                    declaration.removals,
                )]
            };
            warnings.push(LspWarning {
                kind: LspWarningKind::UnusedPrivateDeclaration,
                file_path: file_path.clone(),
                span: declaration.name_span,
                message: format!(
                    "private {} `{}` is transitively unused",
                    declaration.kind, declaration.name
                ),
                fixes,
            });
        }
    }
    Some(warnings)
}

fn item_at(module: &Module, span: Span) -> Option<usize> {
    let index = module
        .items
        .partition_point(|item| item.span().start <= span.start)
        .checked_sub(1)?;
    (span.end <= module.items[index].span().end).then_some(index)
}

/// Eligibility is separate from resolved dependency edges. Documentation's
/// checker validates snippets and references without retaining binding origins
/// in the ordinary typed snapshot; it therefore cannot prove an item unused.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DocDependencyUncertainty {
    pub(crate) executable_snippet: bool,
    pub(crate) structured_reference: bool,
}

impl DocDependencyUncertainty {
    pub(crate) fn is_empty(&self) -> bool {
        !self.executable_snippet && !self.structured_reference
    }
}

pub(crate) fn doc_dependency_uncertainty(
    source: &str,
    tokens: &[Token],
    trailing: &[Trivia],
    module: &Module,
) -> DocDependencyUncertainty {
    use crate::kiodoc::{attrs, directives, parse, refs};

    let mut attached = Vec::new();
    let mut attach = |doc: Option<&DocComment>| {
        if let Some(doc) = doc {
            attached.push(doc.span);
        }
    };
    attach(module.doc.as_ref());
    for item in &module.items {
        match item {
            Item::FnDef(def) => attach(def.doc.as_ref()),
            Item::TypeAlias(def) => attach(def.doc.as_ref()),
            Item::Newtype(def) => attach(def.doc.as_ref()),
            Item::LiteralAlias(def, _) => attach(def.doc.as_ref()),
            Item::Labels(def, _) => attach(def.doc.as_ref()),
            Item::LabelForward(def, _) => attach(def.doc.as_ref()),
            Item::Elaborator(def, _) => attach(def.doc.as_ref()),
            Item::Op(def, _) => attach(def.doc.as_ref()),
            Item::VariadicOperator(def, _) => attach(def.doc.as_ref()),
            Item::HostFn(def) => attach(def.doc.as_ref()),
            Item::HostType(def) => attach(def.doc.as_ref()),
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    attach(member.doc.as_ref());
                }
            }
            Item::TypeRecGroup(group) => {
                attach(group.doc.as_ref());
                for member in &group.members {
                    match member {
                        TypeRecMember::TypeAlias(def) => attach(def.doc.as_ref()),
                        TypeRecMember::Newtype(def) => attach(def.doc.as_ref()),
                        TypeRecMember::Labels(def, _) => attach(def.doc.as_ref()),
                    }
                }
            }
            Item::Equiv(_, _) => {}
        }
    }
    attached.sort_by_key(|span| span.start);
    let scope = refs::module_scope_from_surface(module);
    let mut uncertainty = DocDependencyUncertainty::default();
    for doc in DocSource::from_tokens(source, tokens, trailing) {
        let Some(first_payload) = doc.logical_to_source(0) else {
            continue;
        };
        let after = attached.partition_point(|span| span.start as usize <= first_payload);
        if after == 0 || first_payload > attached[after - 1].end as usize {
            continue;
        }
        for fence in parse::scan(&doc.text) {
            if fence.lang != "kio" {
                continue;
            }
            let Some(attrs) = fence.attrs_text.and_then(|text| attrs::parse(&text).ok()) else {
                continue;
            };
            // These are the active self-harness forms admitted by Kiodoc's
            // doc-comment extractor, including expected-rejection examples.
            if attrs.self_ref
                && attrs.harness_ref.is_none()
                && attrs.bare.is_empty()
                && attrs
                    .kvs
                    .iter()
                    .all(|kv| kv.key == "check_exit_code" && kv.value.parse::<i32>().is_ok())
            {
                uncertainty.executable_snippet = true;
            }
        }
        let overrides = refs::collect_ref_overrides(&doc.text);
        let prose = parse::blank_fences(&doc.text);
        let unresolved_origin = |name: &str| {
            !overrides.contains(name)
                && refs::resolve_in_module(name, &scope) == refs::RefOutcome::Resolved
        };
        uncertainty.structured_reference |= refs::scan_refs(&prose)
            .iter()
            .any(|reference| unresolved_origin(&reference.name))
            || directives::scan_directives(&prose)
                .0
                .iter()
                .any(|directive| unresolved_origin(&directive.term));
    }
    uncertainty
}

pub(crate) struct DeclarationGraph {
    dependencies: Vec<Vec<usize>>,
    roots: Vec<usize>,
    has_surviving_reference: Vec<bool>,
}

impl DeclarationGraph {
    pub(crate) fn for_module(module: &Module) -> Self {
        let mut graph = Self::new(module.items.len());
        for (index, item) in module.items.iter().enumerate() {
            let root = match item {
                Item::HostFn(_) | Item::HostType(_) | Item::Equiv(_, _) => true,
                Item::FnDef(def) => def.vis.is_pub(),
                Item::TypeAlias(def) => def.vis.is_pub(),
                Item::LiteralAlias(def, _) => def.vis.is_pub(),
                Item::Newtype(def) => {
                    def.vis.is_pub() || def.constructor.vis.is_pub() || def.projector.vis.is_pub()
                }
                Item::Labels(def, _) => def.vis.is_pub(),
                Item::LabelForward(def, _) => def.vis.is_pub(),
                Item::Elaborator(def, _) => def.vis.is_pub(),
                Item::Op(def, _) => def.vis.is_pub(),
                Item::VariadicOperator(def, _) => def.vis.is_pub(),
                Item::RecGroup(group, _) => group.members.iter().any(|member| member.vis.is_pub()),
                Item::TypeRecGroup(group) => group.members.iter().any(|member| match member {
                    TypeRecMember::TypeAlias(def) => def.vis.is_pub(),
                    TypeRecMember::Newtype(def) => {
                        def.vis.is_pub()
                            || def.constructor.vis.is_pub()
                            || def.projector.vis.is_pub()
                    }
                    TypeRecMember::Labels(def, _) => def.vis.is_pub(),
                }),
            };
            if root {
                graph.add_root(index);
            }
        }
        graph
    }

    pub(crate) fn new(declarations: usize) -> Self {
        Self {
            dependencies: vec![Vec::new(); declarations],
            roots: Vec::new(),
            has_surviving_reference: vec![false; declarations],
        }
    }

    pub(crate) fn add_root(&mut self, declaration: usize) {
        assert!(declaration < self.dependencies.len());
        self.roots.push(declaration);
    }

    pub(crate) fn add_dependency(&mut self, owner: usize, target: usize) {
        assert!(target < self.dependencies.len());
        self.dependencies[owner].push(target);
        if owner != target {
            self.has_surviving_reference[target] = true;
        }
    }

    pub(crate) fn reachable(&self) -> Vec<bool> {
        let mut live = vec![false; self.dependencies.len()];
        let mut pending = self.roots.clone();
        while let Some(declaration) = pending.pop() {
            if std::mem::replace(&mut live[declaration], true) {
                continue;
            }
            pending.extend(self.dependencies[declaration].iter().copied());
        }
        live
    }

    pub(crate) fn has_surviving_reference(&self, declaration: usize) -> bool {
        self.has_surviving_reference[declaration]
    }
}

pub(crate) struct PrivateDeclaration<'a> {
    pub(crate) name: &'a str,
    pub(crate) name_span: Span,
    pub(crate) kind: &'static str,
    pub(crate) removals: Vec<FixEdit>,
}

pub(crate) fn private_declaration<'a>(
    source: &str,
    tokens: &[Token],
    item: &'a Item,
) -> Option<PrivateDeclaration<'a>> {
    let (name, name_span, kind, mut owned, doc) = match item {
        Item::FnDef(def) if !def.vis.is_pub() && !def.name.starts_with('_') => {
            let head = tokens.partition_point(|token| token.span.start < def.meta.span.start);
            let keyword = tokens.get(head)?;
            if !matches!(&keyword.kind, TokenKind::Ident(word) if word == "fn") {
                return None;
            }
            let name = tokens.get(head + 1)?;
            if !matches!(&name.kind, TokenKind::Ident(word) if word == &def.name) {
                return None;
            }
            let start = if def.purity.is_pure() {
                let modifier = tokens.get(head.checked_sub(1)?)?;
                if !matches!(&modifier.kind, TokenKind::Ident(word) if word == "pure") {
                    return None;
                }
                modifier.span.start
            } else {
                def.meta.span.start
            };
            (
                def.name.as_str(),
                name.span,
                "function",
                Span::new(start, def.meta.span.end),
                def.doc.as_ref(),
            )
        }
        Item::TypeAlias(def) if !def.vis.is_pub() && !def.name.starts_with('_') => (
            def.name.as_str(),
            def.name_span,
            "type",
            def.meta.span,
            def.doc.as_ref(),
        ),
        _ => return None,
    };
    if let Some(doc) = doc {
        owned.start = doc.span.start.min(owned.start);
    }
    let mut separator = None;
    let mut changes_doc_attachment = false;
    if matches!(item, Item::FnDef(_)) {
        let next = tokens.partition_point(|token| token.span.start < owned.end);
        if let Some(token) = tokens
            .get(next)
            .filter(|token| token.kind == TokenKind::Semicolon)
        {
            changes_doc_attachment = token
                .leading_trivia
                .iter()
                .any(|trivia| matches!(trivia, Trivia::DocCommentLine { .. }));
            if source
                .get(owned.end as usize..token.span.start as usize)?
                .chars()
                .all(char::is_whitespace)
            {
                owned.end = token.span.end;
            } else {
                separator = Some(FixEdit::new(token.span, ""));
            }
        }
    }
    let mut removals = vec![whole_declaration_removal(source, owned)?];
    removals.extend(separator);
    if changes_doc_attachment {
        removals.clear();
    }
    Some(PrivateDeclaration {
        name,
        name_span,
        kind,
        removals,
    })
}

pub(crate) fn import_removal(
    source: &str,
    tokens: &[Token],
    import: &Import,
    selection: Option<usize>,
) -> Option<Vec<FixEdit>> {
    let ImportKind::Selective { items, .. } = &import.kind else {
        return selection
            .is_none()
            .then(|| whole_declaration_removal(source, import.span))
            .flatten()
            .map(|edit| vec![edit]);
    };
    let selected = selection?;
    let item = items.get(selected)?;
    if items.len() == 1 {
        return Some(vec![whole_declaration_removal(source, import.span)?]);
    }
    let syntax = import_item_syntax(tokens, item)?;
    let (first, after) = (
        tokens.partition_point(|token| token.span.start < syntax.start),
        tokens.partition_point(|token| token.span.start < syntax.end),
    );
    let comma = if selected + 1 < items.len() {
        tokens.get(after)?
    } else {
        tokens.get(first.checked_sub(1)?)?
    };
    if comma.kind != TokenKind::Comma {
        return None;
    }
    let (left, right) = if comma.span.start < syntax.start {
        (comma.span, syntax)
    } else {
        (syntax, comma.span)
    };
    // A comment between the item and its comma has its own source ownership.
    // Two token-range edits preserve it without moving a neighbouring comment.
    if source
        .get(left.end as usize..right.start as usize)?
        .chars()
        .all(char::is_whitespace)
    {
        Some(vec![FixEdit::new(Span::new(left.start, right.end), "")])
    } else {
        Some(vec![FixEdit::new(left, ""), FixEdit::new(right, "")])
    }
}

fn import_item_syntax(tokens: &[Token], item: &ImportItem) -> Option<Span> {
    match item {
        ImportItem::Name { span, .. } => Some(*span),
        ImportItem::Label { span, .. } => {
            let name = tokens.partition_point(|token| token.span.start < span.start);
            let open = tokens.get(name.checked_sub(1)?)?;
            let close = tokens.get(name + 1)?;
            if open.kind != TokenKind::LBrace || close.kind != TokenKind::RBrace {
                return None;
            }
            Some(Span::new(open.span.start, close.span.end))
        }
        ImportItem::OperatorPattern { span, .. } => {
            let after = tokens.partition_point(|token| token.span.start < span.end);
            let last = tokens.get(after.checked_sub(1)?)?;
            Some(Span::new(span.start, last.span.end))
        }
    }
}

fn whole_declaration_removal(source: &str, owned: Span) -> Option<FixEdit> {
    source.get(owned.start as usize..owned.end as usize)?;
    let line_start = source[..owned.start as usize]
        .rfind(['\r', '\n'])
        .map_or(0, |offset| offset + 1);
    let horizontal = |text: &str| text.bytes().all(|byte| matches!(byte, b' ' | b'\t'));
    if horizontal(&source[line_start..owned.start as usize]) {
        let tail = &source[owned.end as usize..];
        let tail_space = tail
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        let after_space = &tail[tail_space..];
        let newline = if after_space.starts_with("\r\n") {
            2
        } else if after_space.starts_with(['\r', '\n']) {
            1
        } else {
            0
        };
        if newline > 0 || after_space.is_empty() {
            return Some(FixEdit::new(
                Span::new(line_start as u32, owned.end + (tail_space + newline) as u32),
                "",
            ));
        }
    }
    Some(FixEdit::new(owned, ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::{lexer, parser};

    fn apply(source: &str, mut edits: Vec<FixEdit>) -> String {
        edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
        let mut result = source.to_owned();
        for edit in edits {
            result.replace_range(
                edit.span.start as usize..edit.span.end as usize,
                &edit.replacement,
            );
        }
        result
    }

    fn remove_import_item(source: &str, selected: usize) -> String {
        let module = parser::parse(source).expect("valid source");
        let tokens = lexer::lex(source).expect("valid tokens");
        let edits = import_removal(source, &tokens, &module.imports[0], Some(selected))
            .expect("owned import removal");
        let result = apply(source, edits);
        parser::parse(&result).expect("removal preserves source grammar");
        result
    }

    fn doc_uncertainty(doc: &str) -> DocDependencyUncertainty {
        let source = format!("module main;\n{doc}\nfn helper() {{ () }}\n");
        let module = parser::parse(&source).expect("valid source");
        let (tokens, trailing) = lexer::lex_with_trailing(&source).expect("valid tokens");
        doc_dependency_uncertainty(&source, &tokens, &trailing, &module)
    }

    #[test]
    fn executable_docs_withhold_candidates_even_for_trivial_or_rejected_bodies() {
        for doc in [
            "/// ```kio {@}\n/// helper(); ()\n/// ```",
            "/// ```kio {@}\n/// ()\n/// ```",
            "/// <!--kio {@ check_exit_code=14}\n/// let x = 1; ()\n/// -->",
        ] {
            assert_eq!(
                doc_uncertainty(doc),
                DocDependencyUncertainty {
                    executable_snippet: true,
                    structured_reference: false,
                }
            );
        }
    }

    #[test]
    fn ordinary_comments_prose_ignored_and_non_kio_fences_do_not_withhold() {
        for doc in [
            "// ```kio {@}\n// helper(); ()\n// ```",
            "/// helper appears in plain prose with a {@} marker",
            "/// ```kio {ignore}\n/// helper(); ()\n/// ```",
            "/// ```kio {@ ignore}\n/// helper(); ()\n/// ```",
            "/// ```text {@}\n/// helper(); ()\n/// ```",
            "/// ```kio {@ other=1}\n/// helper(); ()\n/// ```",
            "/// ```kio {@ check_exit_code=wrong}\n/// helper(); ()\n/// ```",
            "/// ```kio {}\n/// helper(); ()\n/// ```",
            "/// ```kio {@harness}\n/// helper(); ()\n/// ```",
        ] {
            assert!(doc_uncertainty(doc).is_empty(), "{doc}");
        }
    }

    #[test]
    fn structured_doc_dependencies_are_uncertainty_not_plain_prose_uses() {
        for doc in [
            "/// See [`helper`].",
            "/// [`@source helper`]",
            "/// [`@type helper`]",
        ] {
            assert_eq!(
                doc_uncertainty(doc),
                DocDependencyUncertainty {
                    executable_snippet: false,
                    structured_reference: true,
                }
            );
        }
        for doc in [
            "/// See helper.",
            "/// See [`helper`].\n/// [helper]: https://example.com/",
            "/// ```kio {ignore}\n/// [`helper`]\n/// ```",
            "/// See [`missing`].",
        ] {
            assert!(doc_uncertainty(doc).is_empty(), "{doc}");
        }
    }

    #[test]
    fn dead_cycles_do_not_become_live_from_their_own_references() {
        let mut graph = DeclarationGraph::new(6);
        graph.add_root(0);
        graph.add_dependency(0, 1);
        graph.add_dependency(1, 2);
        graph.add_dependency(2, 1);
        graph.add_dependency(3, 4);
        graph.add_dependency(4, 3);
        graph.add_dependency(4, 5);
        assert_eq!(graph.reachable(), [true, true, true, false, false, false]);
        assert!(graph.has_surviving_reference(5));
        assert!(graph.has_surviving_reference(3));
        assert!(!graph.has_surviving_reference(0));
    }

    #[test]
    fn reachability_is_iterative_across_a_deep_dependency_chain() {
        let mut graph = DeclarationGraph::new(50_000);
        graph.add_root(0);
        for owner in 0..49_999 {
            graph.add_dependency(owner, owner + 1);
        }
        assert!(graph.reachable().into_iter().all(|live| live));
    }

    #[test]
    fn public_scoped_host_and_equiv_items_are_roots_without_workspace_consumers() {
        let source = "module main; fn dead() { () } type Dead = .; pub fn exported() { () } pub(main) type Scoped = .; host type Host; host fn host_value() -> .; equiv witness { (); (); }";
        let module = parser::parse(source).expect("valid source");
        assert_eq!(
            DeclarationGraph::for_module(&module).reachable(),
            [false, false, true, true, true, true, true]
        );
    }

    #[test]
    fn label_removal_owns_both_braces_and_preserves_neighbour_comments() {
        let source = "module main;\nimport lib(kept, // neighbour\n {dead}, // next\n other);\n";
        let result = remove_import_item(source, 1);
        assert_eq!(
            result,
            "module main;\nimport lib(kept, // neighbour\n  // next\n other);\n"
        );
    }

    #[test]
    fn comments_between_selection_and_comma_are_preserved() {
        let source = "module main;\nimport lib(dead // keep comment\n , other);\n";
        assert_eq!(
            remove_import_item(source, 0),
            "module main;\nimport lib( // keep comment\n  other);\n"
        );
    }

    #[test]
    fn first_last_and_nested_operator_selections_remove_only_their_syntax() {
        for (source, selected, expected) in [
            (
                "module main; import lib(dead, kept);",
                0,
                "module main; import lib( kept);",
            ),
            (
                "module main; import lib(kept, dead);",
                1,
                "module main; import lib(kept);",
            ),
            (
                "module main; import lib(op (_) + _, kept);",
                0,
                "module main; import lib( kept);",
            ),
            (
                "module main; import lib(kept, op _ + (_));",
                1,
                "module main; import lib(kept);",
            ),
        ] {
            assert_eq!(remove_import_item(source, selected), expected);
        }
    }

    #[test]
    fn only_selection_removes_the_statement_without_its_leading_comment() {
        let source = "module main;\r\n// keep é\r\n\timport lib({dead});\r\nfn kept() { () }\r\n";
        assert_eq!(
            remove_import_item(source, 0),
            "module main;\r\n// keep é\r\nfn kept() { () }\r\n"
        );
    }

    #[test]
    fn declaration_removal_keeps_unattached_comments_and_removes_pure_and_docs() {
        let source =
            "module main;\n// keep\n\n/// owned\npure fn dead() { () }\nfn kept() { () }\n";
        let module = parser::parse(source).expect("valid source");
        let tokens = lexer::lex(source).expect("valid tokens");
        let declaration =
            private_declaration(source, &tokens, &module.items[0]).expect("private function");
        assert_eq!(declaration.name, "dead");
        assert_eq!(declaration.kind, "function");
        assert_eq!(
            &source[declaration.name_span.start as usize..declaration.name_span.end as usize],
            "dead"
        );
        let result = apply(source, declaration.removals);
        assert_eq!(result, "module main;\n// keep\n\nfn kept() { () }\n");
        parser::parse(&result).expect("complete function removed");
    }

    #[test]
    fn suppressed_and_public_items_are_not_removal_candidates() {
        let source = "module main; type _Ignored = .; fn _ignored() { () } pub type Visible = .; pub(main) fn scoped() { () } host type Host; host fn host_value() -> .;";
        let module = parser::parse(source).expect("valid source");
        let tokens = lexer::lex(source).expect("valid tokens");
        assert!(
            module
                .items
                .iter()
                .all(|item| private_declaration(source, &tokens, item).is_none())
        );
    }
    fn analyze_sources(sources: &[(&str, &str)]) -> crate::cmd::check::LspAnalysis {
        let dir = tempfile::tempdir().expect("temporary package");
        let mut bridge = String::from("package lint; bridge { ");
        for (name, source) in sources {
            std::fs::write(dir.path().join(format!("{name}.kio")), source).expect("module");
            bridge.push_str(name);
            bridge.push_str("; ");
        }
        bridge.push('}');
        std::fs::write(dir.path().join("lint.pkg.kio"), bridge).expect("package");
        crate::cmd::check::analyze_workspace_at_with_overlay_lsp(
            dir.path(),
            &crate::package_collection::SourceOverlay::empty(),
        )
        .expect("clean full analysis")
    }

    fn new_warnings(
        analysis: &crate::cmd::check::LspAnalysis,
    ) -> Vec<(&str, &crate::cmd::check::LspWarning)> {
        analysis
            .warnings
            .iter()
            .filter(|warning| warning.kind != crate::cmd::check::LspWarningKind::LocalBinding)
            .map(|warning| {
                let source = &analysis.sources[&warning.file_path];
                (
                    &source[warning.span.start as usize..warning.span.end as usize],
                    warning,
                )
            })
            .collect()
    }

    #[test]
    fn source_warning_graph_keeps_live_body_and_signature_dependencies() {
        let analysis = analyze_sources(&[
            (
                "provider",
                "module provider; pub fn used() -> . { () } pub fn unused() -> . { () }",
            ),
            (
                "main",
                "module main; import provider(used, unused); \
                type Dead = .; type Live = .; type _Ignored = .; \
                fn helper(_value: Live) -> . { used() } \
                fn dead() -> . { () } fn _ignored() -> . { () } \
                pub fn run() -> . { helper(()) }",
            ),
        ]);
        let mut names: Vec<_> = new_warnings(&analysis)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["Dead", "dead", "unused"]);
    }

    #[test]
    fn source_warning_graph_distinguishes_qualified_and_selective_type_bindings() {
        let analysis = analyze_sources(&[
            ("provider", "module provider; pub type Foo = .;"),
            (
                "main",
                "module main; import provider(Foo); import provider as q; \
                pub fn qualified(_value: q.Foo) -> . { () } \
                pub fn parameter[Foo](_value: Foo) -> . { () }",
            ),
        ]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 1, "{:?}", analysis.warnings);
        assert_eq!(warnings[0].0, "Foo");
        assert_eq!(
            warnings[0].1.kind,
            crate::cmd::check::LspWarningKind::UnusedImport
        );
    }

    #[test]
    fn source_warning_graph_withholds_removal_of_a_surviving_dead_dependency() {
        let source = "module main; fn leaf() -> . { () } fn dead() -> . { leaf() }";
        let analysis = analyze_sources(&[("main", source)]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 2);
        let leaf = warnings.iter().find(|(name, _)| *name == "leaf").unwrap().1;
        let dead = warnings.iter().find(|(name, _)| *name == "dead").unwrap().1;
        assert!(
            leaf.fixes.is_empty(),
            "a surviving declaration still references leaf"
        );
        let repaired = apply(source, dead.fixes[0].edits.clone());
        let rechecked = analyze_sources(&[("main", &repaired)]);
        let remaining = new_warnings(&rechecked);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "leaf");
        assert_eq!(remaining[0].1.fixes.len(), 1);
    }

    #[test]
    fn source_warning_graph_counts_actual_implicit_role_selection_only() {
        for (body, expected_dead) in [
            ("pub fn run() -> . { let _value = 1; () }", false),
            ("pub fn run() -> r.Number { 1 }", true),
        ] {
            let main = format!("module main; import roles as r; type Fallback = r.Number; {body}");
            let analysis = analyze_sources(&[
                ("roles", "module roles; pub host type Number role(i32);"),
                ("main", &main),
            ]);
            let warnings = new_warnings(&analysis);
            assert_eq!(
                warnings.len(),
                usize::from(expected_dead),
                "{:?}",
                analysis.warnings
            );
            if expected_dead {
                assert_eq!(warnings[0].0, "Fallback");
            }
        }
    }

    #[test]
    fn source_warning_graph_tracks_operator_and_fold_callable_edges() {
        let analysis = analyze_sources(&[
            (
                "syntax",
                "module syntax; pub fn combine(left: ., _right: .) -> . { left } \
                pub op _ + _ { impl combine; }; pub op _ - _ { impl combine; };",
            ),
            (
                "main",
                "module main; import syntax(op _ + _, op _ - _); \
                fn combine(left: ., _right: .) -> . { left } fn empty() -> . { () } \
                op _ * _ { impl combine; }; varop [% %] { foldl combine empty; }; \
                pub fn fixed() -> . { (() + ()) * () } \
                pub fn folded() -> . { [% (), () %] }",
            ),
        ]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 1, "{:?}", analysis.warnings);
        assert_eq!(warnings[0].0, "op _ - _");
    }

    #[test]
    fn source_warning_graph_tracks_labels_forwarding_and_generated_members() {
        let analysis = analyze_sources(&[
            ("labels", "module labels; pub labels { tag: ., unused: . };"),
            (
                "main",
                "module main; import labels({tag}, {unused}); \
                type Payload = .; labels { local: Payload }; type {forwarded} = {tag}; \
                pub fn run() -> . { let _ = {forwarded = ()}; let _ = Local.mk(()); () }",
            ),
        ]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 1, "{:?}", analysis.warnings);
        assert_eq!(warnings[0].0, "{unused}");
    }

    #[test]
    fn source_warning_graph_tracks_recursive_groups_and_loop_drivers() {
        let analysis = analyze_sources(&[(
            "main",
            "module main; \
            host fn loop[S][R](step: S -> (S | R), state: S) -> R; \
            fn driver[S][R](step: S -> (S | R), state: S) -> R { loop(step, state) } \
            type Payload = .; \
            rec { newtype A : Payload & B { constructor make_a; projector open_a; }; \
                newtype B : A { constructor make_b; projector open_b; }; } \
            rec(driver) fn repeat(value: A) -> A { recur repeat(value) } \
            pub fn run() -> . { let _ = repeat; () }",
        )]);
        assert!(
            new_warnings(&analysis).is_empty(),
            "{:?}",
            analysis.warnings
        );
    }

    #[test]
    fn source_warning_graph_tracks_elaborator_implementation_and_captures() {
        let analysis = analyze_sources(&[(
            "main",
            "module main; import __comptime__; \
            type Hidden = .; pure fn helper() -> . { () } \
            pure fn build(ct: __Comptime__, _helper: __Checked_term__, \
                _hidden: __Type__) -> __Checked_term__ { \
                __term_unit__(ct) \
            } \
            elab expand : . -> . { captures (helper, Hidden); impl build; }; \
            pub fn run() -> . { expand!(()) }",
        )]);
        assert!(
            new_warnings(&analysis).is_empty(),
            "{:?}",
            analysis.warnings
        );
    }

    #[test]
    fn source_warning_graph_tracks_erased_literals_without_shadowed_import_uses() {
        for (body, unused) in [
            ("pub fn run() -> r.Number { marker }", false),
            ("pub fn run(marker: r.Number) -> r.Number { marker }", true),
        ] {
            let source = format!("module main; import literals(marker); import roles as r; {body}");
            let analysis = analyze_sources(&[
                ("roles", "module roles; pub host type Number role(i32);"),
                ("literals", "module literals; pub literal marker = 1;"),
                ("main", &source),
            ]);
            let warnings = new_warnings(&analysis);
            assert_eq!(
                warnings.len(),
                usize::from(unused),
                "{:?}",
                analysis.warnings
            );
            if unused {
                assert_eq!(warnings[0].0, "marker");
            }
        }
    }

    #[test]
    fn source_warning_graph_treats_builtin_imports_as_exact_enablement_sets() {
        for (body, expected) in [
            (
                "pub fn run() -> . { () }",
                vec![
                    "import __intrinsics__;",
                    "import __intrinsics__;",
                    "import __comptime__;",
                ],
            ),
            (
                "pub fn run() -> . & . { __pair__(., ., (), ()) }",
                vec!["import __comptime__;"],
            ),
            (
                "pub pure fn reflect(ct: __Comptime__) -> __Checked_term__ { __term_unit__(ct) }",
                vec!["import __intrinsics__;", "import __intrinsics__;"],
            ),
        ] {
            let source = format!(
                "module main; import __intrinsics__; import __intrinsics__; import __comptime__; {body}"
            );
            let analysis = analyze_sources(&[("main", &source)]);
            let actual: Vec<_> = new_warnings(&analysis)
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            assert_eq!(actual, expected, "{:?}", analysis.warnings);
        }
    }

    #[test]
    fn source_warning_graph_keeps_equiv_host_and_scoped_public_dependencies_live() {
        let analysis = analyze_sources(&[(
            "main",
            "module main; \
            pub type Contract = .; host fn capability(value: Contract) -> Contract; \
            pure fn law_helper() -> . { () } equiv law { law_helper(); (); } \
            fn scoped_helper() -> . { () } pub(main) fn visible() -> . { scoped_helper() } \
            type Dead = .; type _Ignored = .;",
        )]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 1, "{:?}", analysis.warnings);
        assert_eq!(warnings[0].0, "Dead");
        assert_eq!(warnings[0].1.fixes.len(), 1);
    }

    #[test]
    fn private_removal_does_not_reattach_unowned_executable_documentation() {
        let source = "module main; fn dead() -> . { () }\n/// ```kio {@}\n/// dead(); ()\n/// ```\n; pub fn live() -> . { () }";
        let parsed = parser::parse(source).unwrap();
        let (tokens, trailing) = lexer::lex_with_trailing(source).unwrap();
        assert!(doc_dependency_uncertainty(source, &tokens, &trailing, &parsed).is_empty());
        let unsafe_edit = source
            .replace("fn dead() -> . { () }", "")
            .replace("; pub fn", " pub fn");
        let repaired = parser::parse(&unsafe_edit).unwrap();
        let (tokens, trailing) = lexer::lex_with_trailing(&unsafe_edit).unwrap();
        assert!(
            doc_dependency_uncertainty(&unsafe_edit, &tokens, &trailing, &repaired)
                .executable_snippet
        );
        let analysis = analyze_sources(&[("main", source)]);
        let warnings = new_warnings(&analysis);
        assert_eq!(warnings.len(), 1, "{:?}", analysis.warnings);
        assert_eq!(warnings[0].0, "dead");
        assert!(
            warnings[0].1.fixes.is_empty(),
            "removal must not activate an unowned snippet"
        );
    }

    #[test]
    fn import_documentation_cannot_supply_a_clean_unused_warning_snapshot() {
        let source = "module main;\n/// ```kio {@}\n/// unused(); ()\n/// ```\nimport provider(unused);\npub fn live() -> . { () }";
        let error = parser::parse(source).expect_err("documentation cannot attach to an import");
        assert!(
            error
                .diag()
                .1
                .contains("import` clauses are not documented")
        );
    }
}

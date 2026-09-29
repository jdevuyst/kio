//! Pre-lowering validation for consumer-visible surface registries.
//!
//! Literal aliases and operator bindings disappear before the ordinary
//! resolver builds its environment. This pass validates their written imports
//! and binding origins while the Surface tree still carries that information,
//! but schedules the resulting Name errors only after every import has succeeded.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ast::{
    ImportItem, ImportKind, Item, Lowered, Meta, Module, ModulePath, Surface, Type, TypeAlias,
    Visibility,
};
use crate::error::Error;
use crate::pass::resolve::{LocatedError, PRIME_INTRINSICS, Package, is_visible};
use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceOrigin {
    Declaration {
        module: String,
        name: String,
        ordinal: u32,
    },
    Module(String),
    Builtin {
        module: &'static str,
        name: &'static str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NamedKind {
    Ordinary,
    Literal,
    Elaborator,
    GeneratedLabelNominal,
}

impl NamedKind {
    fn requires_surface_validation(self) -> bool {
        !matches!(self, Self::Ordinary)
    }

    fn is_literal(self) -> bool {
        matches!(self, Self::Literal)
    }
}

#[derive(Clone, Debug)]
struct NamedDecl {
    name: String,
    span: Span,
    vis: Visibility,
    origin: SourceOrigin,
    kind: NamedKind,
    #[cfg(feature = "cli")]
    omitted_from_unsealed_contract: bool,
}

#[derive(Clone, Debug)]
struct LabelDecl {
    name: String,
    span: Span,
    vis: Visibility,
    origin: SourceOrigin,
}

#[derive(Clone, Debug)]
struct OperatorDecl {
    dispatch_key: String,
    grammar: crate::ast::OperatorGrammar,
    span: Span,
    vis: Visibility,
    origin: SourceOrigin,
}

#[derive(Clone, Debug)]
struct VisibilitySite {
    vis: Visibility,
    span: Span,
    description: String,
}

#[derive(Debug)]
struct RegistryModule {
    file_path: PathBuf,
    path: ModulePath,
    imports: Vec<crate::ast::Import>,
    local_named: Vec<NamedDecl>,
    named_exports: BTreeMap<String, Vec<NamedDecl>>,
    local_labels: Vec<LabelDecl>,
    label_exports: BTreeMap<String, LabelDecl>,
    local_operators: Vec<OperatorDecl>,
    operator_exports: BTreeMap<String, OperatorDecl>,
    visibility_sites: Vec<VisibilitySite>,
}

#[derive(Debug)]
struct RegistryIndex {
    modules: BTreeMap<String, RegistryModule>,
}

#[derive(Clone, Debug)]
struct VisibleOrigin {
    origin: SourceOrigin,
    span: Span,
    description: String,
    local: bool,
    kind: NamedKind,
}

fn module_path_string(path: &ModulePath) -> String {
    path.segments.join("/")
}

fn dummy_alias(name: String, vis: Visibility, span: Span) -> Item<Lowered> {
    Item::TypeAlias(TypeAlias {
        vis,
        name,
        name_span: span,
        type_params: Vec::new(),
        body: Type::Unit {
            meta: Meta::new(span),
        },
        meta: Meta::new(span),
        editable_span: None,
        doc: None,
    })
}

fn union_visibility(first: &Visibility, second: &Visibility) -> Visibility {
    match (first, second) {
        (Visibility::Public, _) | (_, Visibility::Public) => Visibility::Public,
        (Visibility::Private, other) | (other, Visibility::Private) => other.clone(),
        (Visibility::PublicIn(first), Visibility::PublicIn(second)) => {
            if first.segments.len() <= second.segments.len() {
                Visibility::PublicIn(first.clone())
            } else {
                Visibility::PublicIn(second.clone())
            }
        }
    }
}

struct ModuleProjection {
    module_path: String,
    registry: RegistryModule,
    skeleton: Module<Lowered>,
}

fn project_module(file_path: &Path, module: &Module<Surface>) -> ModuleProjection {
    let module_path = module_path_string(&module.path);
    let ordinal = Cell::new(0u32);
    let mut local_named = Vec::new();
    let mut named_exports: BTreeMap<String, Vec<NamedDecl>> = BTreeMap::new();
    let mut local_labels = Vec::new();
    let mut label_exports = BTreeMap::new();
    let mut local_operators = Vec::new();
    let mut operator_exports = BTreeMap::new();
    let mut visibility_sites = Vec::new();
    let mut seen_labels = BTreeSet::new();

    let mut add_named = |name: &str,
                         span: Span,
                         vis: Visibility,
                         kind: NamedKind,
                         _omitted_from_unsealed_contract: bool| {
        let origin = SourceOrigin::Declaration {
            module: module_path.clone(),
            name: name.to_owned(),
            ordinal: ordinal.get(),
        };
        ordinal.set(ordinal.get() + 1);
        let decl = NamedDecl {
            name: name.to_owned(),
            span,
            vis: vis.clone(),
            origin,
            kind,
            #[cfg(feature = "cli")]
            omitted_from_unsealed_contract: _omitted_from_unsealed_contract,
        };
        local_named.push(decl.clone());
        named_exports
            .entry(name.to_owned())
            .or_default()
            .push(decl.clone());
        decl
    };

    let mut add_visibility = |vis: &Visibility, span: Span, description: String| {
        visibility_sites.push(VisibilitySite {
            vis: vis.clone(),
            span,
            description,
        });
    };

    for item in &module.items {
        match item {
            Item::FnDef(def) => {
                add_named(
                    &def.name,
                    def.meta.span,
                    def.vis.clone(),
                    NamedKind::Ordinary,
                    false,
                );
            }
            Item::RecGroup(group, _) => {
                for member in &group.members {
                    add_named(
                        &member.name,
                        member.meta.span,
                        member.vis.clone(),
                        NamedKind::Ordinary,
                        false,
                    );
                }
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            add_named(
                                &alias.name,
                                alias.meta.span,
                                alias.vis.clone(),
                                NamedKind::Ordinary,
                                false,
                            );
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            add_named(
                                &newtype.name,
                                newtype.meta.span,
                                newtype.vis.clone(),
                                NamedKind::Ordinary,
                                false,
                            );
                            add_visibility(
                                &newtype.constructor.vis,
                                newtype.constructor.span,
                                format!("{}.{}", newtype.name, newtype.constructor.name),
                            );
                            add_visibility(
                                &newtype.projector.vis,
                                newtype.projector.span,
                                format!("{}.{}", newtype.name, newtype.projector.name),
                            );
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            if let Some(name) = &labels.type_alias_name {
                                add_named(
                                    name,
                                    labels.type_alias_span.unwrap_or(labels.meta.span),
                                    labels.vis.clone(),
                                    NamedKind::Ordinary,
                                    true,
                                );
                            }
                            for entry in &labels.entries {
                                if entry.is_reuse_marker()
                                    || !seen_labels.insert(entry.name.clone())
                                {
                                    continue;
                                }
                                let label = LabelDecl {
                                    name: entry.name.clone(),
                                    span: entry.name_span,
                                    vis: labels.vis.clone(),
                                    origin: SourceOrigin::Declaration {
                                        module: module_path.clone(),
                                        name: entry.name.clone(),
                                        ordinal: ordinal.get(),
                                    },
                                };
                                ordinal.set(ordinal.get() + 1);
                                local_labels.push(label.clone());
                                label_exports.entry(entry.name.clone()).or_insert(label);
                                let generated = crate::ast::mint_label_newtype_name(&entry.name);
                                add_named(
                                    &generated,
                                    entry.name_span,
                                    labels.vis.clone(),
                                    NamedKind::GeneratedLabelNominal,
                                    true,
                                );
                            }
                        }
                    }
                }
            }
            Item::TypeAlias(alias) => {
                add_named(
                    &alias.name,
                    alias.meta.span,
                    alias.vis.clone(),
                    NamedKind::Ordinary,
                    false,
                );
            }
            Item::LiteralAlias(alias, _) => {
                add_named(
                    &alias.name,
                    alias.meta.span,
                    alias.vis.clone(),
                    NamedKind::Literal,
                    true,
                );
            }
            Item::Newtype(newtype) => {
                add_named(
                    &newtype.name,
                    newtype.meta.span,
                    newtype.vis.clone(),
                    NamedKind::Ordinary,
                    false,
                );
                add_visibility(
                    &newtype.constructor.vis,
                    newtype.constructor.span,
                    format!("{}.{}", newtype.name, newtype.constructor.name),
                );
                add_visibility(
                    &newtype.projector.vis,
                    newtype.projector.span,
                    format!("{}.{}", newtype.name, newtype.projector.name),
                );
            }
            Item::Labels(labels, _) => {
                if let Some(name) = &labels.type_alias_name {
                    add_named(
                        name,
                        labels.type_alias_span.unwrap_or(labels.meta.span),
                        labels.vis.clone(),
                        NamedKind::Ordinary,
                        true,
                    );
                }
                for entry in &labels.entries {
                    if entry.is_reuse_marker() || !seen_labels.insert(entry.name.clone()) {
                        continue;
                    }
                    // The lowercase label occupies the consumed label
                    // registry; the uppercase spelling is its generated
                    // nominal. Both are selectable exports.
                    let label = LabelDecl {
                        name: entry.name.clone(),
                        span: entry.name_span,
                        vis: labels.vis.clone(),
                        origin: SourceOrigin::Declaration {
                            module: module_path.clone(),
                            name: entry.name.clone(),
                            ordinal: ordinal.get(),
                        },
                    };
                    ordinal.set(ordinal.get() + 1);
                    local_labels.push(label.clone());
                    label_exports.entry(entry.name.clone()).or_insert(label);
                    let generated = crate::ast::mint_label_newtype_name(&entry.name);
                    add_named(
                        &generated,
                        entry.name_span,
                        labels.vis.clone(),
                        NamedKind::GeneratedLabelNominal,
                        true,
                    );
                }
            }
            Item::LabelForward(forward, _) => {
                let label = LabelDecl {
                    name: forward.name.clone(),
                    span: forward.name_span,
                    vis: forward.vis.clone(),
                    origin: SourceOrigin::Declaration {
                        module: module_path.clone(),
                        name: forward.name.clone(),
                        ordinal: ordinal.get(),
                    },
                };
                ordinal.set(ordinal.get() + 1);
                local_labels.push(label.clone());
                label_exports.entry(forward.name.clone()).or_insert(label);
            }
            Item::Equiv(_, _) => {}
            Item::Elaborator(elaborator, _) => {
                add_named(
                    &elaborator.name,
                    elaborator.name_span,
                    elaborator.vis.clone(),
                    NamedKind::Elaborator,
                    true,
                );
            }
            Item::Op(op, _) => {
                let crate::ast::OpBody::Normal { pattern, .. } = &op.body;
                let grammar = crate::ast::OperatorGrammar::fixed(pattern);
                let dispatch_key = grammar.dispatch_key().render();
                let name = grammar.render();
                add_visibility(&op.vis, op.meta.span, format!("operator `{name}`"));
                let decl = OperatorDecl {
                    dispatch_key: dispatch_key.clone(),
                    grammar,
                    span: op.meta.span,
                    vis: op.vis.clone(),
                    origin: SourceOrigin::Declaration {
                        module: module_path.clone(),
                        name: name.clone(),
                        ordinal: ordinal.get(),
                    },
                };
                ordinal.set(ordinal.get() + 1);
                local_operators.push(decl.clone());
                operator_exports.entry(dispatch_key).or_insert(decl);
            }
            Item::VariadicOperator(fold, _) => {
                let grammar = crate::ast::OperatorGrammar::variadic(&fold.open, &fold.spec);
                let dispatch_key = grammar.dispatch_key().render();
                let name = grammar.render();
                add_visibility(&fold.vis, fold.meta.span, format!("operator `{name}`"));
                let decl = OperatorDecl {
                    dispatch_key: dispatch_key.clone(),
                    grammar,
                    span: fold.meta.span,
                    vis: fold.vis.clone(),
                    origin: SourceOrigin::Declaration {
                        module: module_path.clone(),
                        name: name.clone(),
                        ordinal: ordinal.get(),
                    },
                };
                ordinal.set(ordinal.get() + 1);
                local_operators.push(decl.clone());
                operator_exports.entry(dispatch_key).or_insert(decl);
            }
            Item::HostType(host) => {
                add_named(
                    &host.name,
                    host.meta.span,
                    Visibility::Public,
                    NamedKind::Ordinary,
                    false,
                );
            }
            Item::HostFn(host) => {
                add_named(
                    &host.name,
                    host.meta.span,
                    Visibility::Public,
                    NamedKind::Ordinary,
                    false,
                );
            }
        }
    }

    // The projected ordinary resolver sees only ordinary values/types and
    // generated nominal label types. Lowercase labels live in their own
    // registry and are selected only by a braced use item.
    let mut skeleton_bindings: BTreeMap<String, (Visibility, Span)> = BTreeMap::new();
    for decl in &local_named {
        visibility_sites.push(VisibilitySite {
            vis: decl.vis.clone(),
            span: decl.span,
            description: decl.name.clone(),
        });
        skeleton_bindings
            .entry(decl.name.clone())
            .and_modify(|(vis, _)| *vis = union_visibility(vis, &decl.vis))
            .or_insert_with(|| (decl.vis.clone(), decl.span));
    }
    for decl in &local_labels {
        visibility_sites.push(VisibilitySite {
            vis: decl.vis.clone(),
            span: decl.span,
            description: format!("label `{}`", decl.name),
        });
    }
    let skeleton_items = skeleton_bindings
        .into_iter()
        .map(|(name, (vis, span))| dummy_alias(name, vis, span))
        .collect();

    ModuleProjection {
        module_path,
        registry: RegistryModule {
            file_path: file_path.to_path_buf(),
            path: module.path.clone(),
            imports: module.imports.clone(),
            local_named,
            named_exports,
            local_labels,
            label_exports,
            local_operators,
            operator_exports,
            visibility_sites,
        },
        skeleton: Module {
            path: module.path.clone(),
            imports: module.imports.clone(),
            items: skeleton_items,
            meta: Meta::new(module.meta.span),
            doc: module.doc.clone(),
        },
    }
}

fn validate_visibility(index: &RegistryIndex) -> Result<(), LocatedError> {
    for module in index.modules.values() {
        for site in &module.visibility_sites {
            let Visibility::PublicIn(path) = &site.vis else {
                continue;
            };
            let prefix_ok = module.path.segments.len() >= path.segments.len()
                && module.path.segments[..path.segments.len()] == path.segments[..];
            if !prefix_ok {
                return Err(LocatedError {
                    file_path: module.file_path.clone(),
                    error: Error::import(
                        site.span,
                        format!(
                            "`pub({})` on `{}` must name the declaring module `{}` or one of its ancestors",
                            module_path_string(path),
                            site.description,
                            module_path_string(&module.path),
                        ),
                    ),
                });
            }
        }
    }
    Ok(())
}

fn validate_operator_imports_for_module(
    index: &RegistryIndex,
    module: &RegistryModule,
) -> Result<(), LocatedError> {
    for usage in &module.imports {
        let ImportKind::Selective { items, from } = &usage.kind else {
            continue;
        };
        let operator_items: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                ImportItem::OperatorPattern { grammar, span, .. } => Some((grammar, *span)),
                ImportItem::Name { .. } | ImportItem::Label { .. } => None,
            })
            .collect();
        if operator_items.is_empty() {
            continue;
        }
        let source = module_path_string(from);
        let Some(target) = index.modules.get(&source) else {
            return Err(LocatedError {
                file_path: module.file_path.clone(),
                error: Error::import(
                    from.span,
                    format!("module `{source}` is not in this package"),
                ),
            });
        };
        for (grammar, span) in operator_items {
            let rendered = grammar.render();
            let Some(decl) = target
                .operator_exports
                .get(&grammar.dispatch_key().render())
            else {
                return Err(LocatedError {
                    file_path: module.file_path.clone(),
                    error: Error::import(
                        span,
                        format!("module `{source}` exports no operator matching `{rendered}`"),
                    ),
                });
            };
            if grammar != &decl.grammar {
                return Err(LocatedError {
                    file_path: module.file_path.clone(),
                    error: Error::import(span, format!(
                        "imported operator grammar does not match module `{source}`: `{rendered}`"
                    )).with_help(format!("import {}({});", source, decl.grammar.render())),
                });
            }
            if !is_visible(&decl.vis, &module.path) {
                let error = match &decl.vis {
                    Visibility::PublicIn(path) => Error::import(
                        span,
                        format!(
                            "operator `{rendered}` from module `{source}` is restricted to `pub({})` and is not visible from `{}`",
                            module_path_string(path),
                            module_path_string(&module.path),
                        ),
                    ),
                    Visibility::Private => Error::import(
                        span,
                        format!("operator `{rendered}` in module `{source}` is private"),
                    )
                    .with_help(format!(
                        "add `pub` to `{rendered}` in module `{source}` to make it importable"
                    )),
                    Visibility::Public => unreachable!("public operator is visible"),
                };
                return Err(LocatedError {
                    file_path: module.file_path.clone(),
                    error,
                });
            }
        }
    }
    Ok(())
}

fn validate_operator_imports(index: &RegistryIndex) -> Result<(), LocatedError> {
    for module in index.modules.values() {
        validate_operator_imports_for_module(index, module)?;
    }
    Ok(())
}

fn validate_label_imports(index: &RegistryIndex) -> Result<(), LocatedError> {
    for module in index.modules.values() {
        for usage in &module.imports {
            let ImportKind::Selective { items, from } = &usage.kind else {
                continue;
            };
            let label_items: Vec<_> = items.iter().filter_map(ImportItem::as_label).collect();
            if label_items.is_empty() {
                continue;
            }
            let source = module_path_string(from);
            let Some(target) = index.modules.get(&source) else {
                return Err(LocatedError {
                    file_path: module.file_path.clone(),
                    error: Error::import(
                        from.span,
                        format!("module `{source}` is not in this package"),
                    ),
                });
            };
            for (name, span) in label_items {
                let Some(decl) = target.label_exports.get(name) else {
                    let mut error = Error::import(
                        span,
                        format!("module `{source}` does not export label `{name}`"),
                    );
                    if target.named_exports.contains_key(name) {
                        error = error.with_help(format!(
                            "`{name}` is an ordinary binding; import it without braces: `import {source}({name});`"
                        ));
                    } else {
                        let visible_labels = target
                            .label_exports
                            .values()
                            .filter(|candidate| is_visible(&candidate.vis, &module.path))
                            .map(|candidate| candidate.name.as_str());
                        if let Some(near) = crate::error::closest_name(name, visible_labels) {
                            error = error.with_help(format!("did you mean `{{{near}}}`?"));
                        }
                    }
                    return Err(LocatedError {
                        file_path: module.file_path.clone(),
                        error,
                    });
                };
                if !is_visible(&decl.vis, &module.path) {
                    let error = match &decl.vis {
                        Visibility::PublicIn(path) => Error::import(
                            span,
                            format!(
                                "label `{name}` from module `{source}` is restricted to `pub({})` and is not visible from `{}`",
                                module_path_string(path),
                                module_path_string(&module.path),
                            ),
                        ),
                        Visibility::Private => Error::import(
                            span,
                            format!("label `{name}` in module `{source}` is private"),
                        )
                        .with_help(format!(
                            "add `pub` to label `{name}` in module `{source}` to make it importable"
                        )),
                        Visibility::Public => unreachable!("public label is visible"),
                    };
                    return Err(LocatedError {
                        file_path: module.file_path.clone(),
                        error,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Give a directed namespace diagnostic when a bare selective item names only
/// a label export. The ordinary resolver still owns every other ordinary-use
/// error, including visibility and closest-name suggestions.
fn validate_ordinary_namespace_selection(index: &RegistryIndex) -> Result<(), LocatedError> {
    for module in index.modules.values() {
        for usage in &module.imports {
            let ImportKind::Selective { items, from } = &usage.kind else {
                continue;
            };
            let source = module_path_string(from);
            let Some(target) = index.modules.get(&source) else {
                continue;
            };
            for name in items.iter().filter_map(ImportItem::as_name) {
                if target.named_exports.contains_key(name) {
                    continue;
                }
                if target.label_exports.contains_key(name) {
                    return Err(LocatedError {
                        file_path: module.file_path.clone(),
                        error: Error::import(
                            usage.span,
                            format!(
                                "`{name}` in module `{source}` is a label, not an ordinary value or type"
                            ),
                        )
                        .with_help(format!(
                            "import label syntax with braces: `import {source}({{{name}}});`"
                        )),
                    });
                }
            }
        }
    }
    Ok(())
}

fn register_named_origin(
    visible: &mut BTreeMap<String, VisibleOrigin>,
    module_path: &str,
    name: &str,
    candidate: VisibleOrigin,
) -> Result<(), Error> {
    let Some(first) = visible.get(name) else {
        visible.insert(name.to_owned(), candidate);
        return Ok(());
    };
    if first.local
        && candidate.local
        && (first.kind.requires_surface_validation()
            || candidate.kind.requires_surface_validation())
    {
        return Err(Error::name_res(
            candidate.span,
            format!("duplicate top-level declaration `{name}`"),
        )
        .with_secondary(first.span, format!("`{name}` first declared here"))
        .with_help(format!(
            "each top-level name may be declared once — rename this `{name}` or remove the earlier declaration"
        )));
    }
    if first.origin == candidate.origin {
        if !first.local
            && !candidate.local
            && first.kind.is_literal()
            && candidate.kind.is_literal()
        {
            return Err(duplicate_consumed_import(
                first,
                &candidate,
                "literal alias",
                name,
            ));
        }
        if first.kind.requires_surface_validation() || candidate.kind.requires_surface_validation()
        {
            return Err(Error::name_res(
                candidate.span,
                format!("binding `{name}` is introduced more than once"),
            )
            .with_secondary(first.span, first.description.clone())
            .with_help(format!("remove the repeated import of `{name}`")));
        }
        return Ok(());
    }
    if !first.kind.requires_surface_validation() && !candidate.kind.requires_surface_validation() {
        return Ok(());
    }
    let noun = if first.kind.is_literal() || candidate.kind.is_literal() {
        "literal alias"
    } else {
        "binding"
    };
    Err(Error::name_res(
        candidate.span,
        format!("{noun} `{name}` has more than one source"),
    )
    .with_secondary(first.span, first.description.clone())
    .with_help(format!(
        "give `{name}` one origin in module `{module_path}`: remove one import or rename the local declaration"
    )))
}

fn duplicate_consumed_import(
    first: &VisibleOrigin,
    candidate: &VisibleOrigin,
    noun: &str,
    name: &str,
) -> Error {
    Error::name_res(candidate.span, format!("duplicate {noun} import `{name}`"))
        .with_secondary(first.span, format!("`{name}` first imported here"))
        .with_help(format!(
            "remove the repeated {noun} import; each explicit {noun} import may be written once"
        ))
}

fn named_origins(
    index: &RegistryIndex,
) -> Result<BTreeMap<String, BTreeMap<String, VisibleOrigin>>, LocatedError> {
    let mut scopes = BTreeMap::new();
    for (module_path, module) in &index.modules {
        let mut visible = BTreeMap::new();
        for usage in &module.imports {
            match &usage.kind {
                ImportKind::Selective { items, from } => {
                    let source = module_path_string(from);
                    let target = index
                        .modules
                        .get(&source)
                        .expect("surface imports were validated before origin checking");
                    for (name, span) in items.iter().filter_map(ImportItem::as_name_with_span) {
                        let Some(declarations) = target.named_exports.get(name) else {
                            continue;
                        };
                        for decl in declarations
                            .iter()
                            .filter(|decl| is_visible(&decl.vis, &module.path))
                        {
                            let description = match decl.kind {
                                NamedKind::Literal => format!(
                                    "`{name}` is imported as a literal alias from `{source}` here"
                                ),
                                NamedKind::Elaborator => format!(
                                    "`{name}` is imported as an elaborator from `{source}` here"
                                ),
                                NamedKind::GeneratedLabelNominal => format!(
                                    "`{name}` is imported as a generated label type from `{source}` here"
                                ),
                                NamedKind::Ordinary => {
                                    format!("`{name}` is imported from `{source}` here")
                                }
                            };
                            register_named_origin(
                                &mut visible,
                                module_path,
                                name,
                                VisibleOrigin {
                                    origin: decl.origin.clone(),
                                    span,
                                    description,
                                    local: false,
                                    kind: decl.kind,
                                },
                            )
                            .map_err(|error| LocatedError {
                                file_path: module.file_path.clone(),
                                error,
                            })?;
                        }
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    let source = module_path_string(path);
                    register_named_origin(
                        &mut visible,
                        module_path,
                        alias,
                        VisibleOrigin {
                            origin: SourceOrigin::Module(source.clone()),
                            span: usage.span,
                            description: format!("`{alias}` names module `{source}` here"),
                            local: false,
                            kind: NamedKind::Ordinary,
                        },
                    )
                    .map_err(|error| LocatedError {
                        file_path: module.file_path.clone(),
                        error,
                    })?;
                }
                ImportKind::Comptime => {
                    for name in crate::comptime::PUBLIC_COMPTIME_NAMES {
                        register_named_origin(
                            &mut visible,
                            module_path,
                            name,
                            VisibleOrigin {
                                origin: SourceOrigin::Builtin {
                                    module: "__comptime__",
                                    name,
                                },
                                span: usage.span,
                                description: format!(
                                    "`{name}` is imported from `__comptime__` here"
                                ),
                                local: false,
                                kind: NamedKind::Ordinary,
                            },
                        )
                        .map_err(|error| LocatedError {
                            file_path: module.file_path.clone(),
                            error,
                        })?;
                    }
                }
                ImportKind::Intrinsics => {
                    for name in PRIME_INTRINSICS {
                        register_named_origin(
                            &mut visible,
                            module_path,
                            name,
                            VisibleOrigin {
                                origin: SourceOrigin::Builtin {
                                    module: "__intrinsics__",
                                    name,
                                },
                                span: usage.span,
                                description: format!(
                                    "`{name}` is imported from `__intrinsics__` here"
                                ),
                                local: false,
                                kind: NamedKind::Ordinary,
                            },
                        )
                        .map_err(|error| LocatedError {
                            file_path: module.file_path.clone(),
                            error,
                        })?;
                    }
                }
            }
        }
        for decl in &module.local_named {
            let description = match decl.kind {
                NamedKind::Literal => {
                    format!("`{}` is declared as a literal alias here", decl.name)
                }
                NamedKind::Elaborator => {
                    format!("`{}` is declared as an elaborator here", decl.name)
                }
                NamedKind::GeneratedLabelNominal => format!(
                    "`{}` is generated by a same-module label declaration here",
                    decl.name
                ),
                NamedKind::Ordinary => format!("`{}` is declared locally here", decl.name),
            };
            register_named_origin(
                &mut visible,
                module_path,
                &decl.name,
                VisibleOrigin {
                    origin: decl.origin.clone(),
                    span: decl.span,
                    description,
                    local: true,
                    kind: decl.kind,
                },
            )
            .map_err(|error| LocatedError {
                file_path: module.file_path.clone(),
                error,
            })?;
        }
        scopes.insert(module_path.clone(), visible);
    }
    Ok(scopes)
}

fn register_label_origin(
    visible: &mut BTreeMap<String, VisibleOrigin>,
    module_path: &str,
    name: &str,
    candidate: VisibleOrigin,
) -> Result<(), Error> {
    let Some(first) = visible.get(name) else {
        visible.insert(name.to_owned(), candidate);
        return Ok(());
    };
    if first.origin == candidate.origin {
        if !first.local && !candidate.local {
            return Err(duplicate_consumed_import(first, &candidate, "label", name));
        }
        return Ok(());
    }
    Err(Error::name_res(
        candidate.span,
        format!("label `{name}` has more than one source"),
    )
    .with_secondary(first.span, first.description.clone())
    .with_help(format!(
        "give label `{name}` one origin in module `{module_path}`: remove one import or rename the local declaration"
    )))
}

fn validate_label_origins(index: &RegistryIndex) -> Result<(), LocatedError> {
    for (module_path, module) in &index.modules {
        let mut visible = BTreeMap::new();
        for decl in &module.local_labels {
            register_label_origin(
                &mut visible,
                module_path,
                &decl.name,
                VisibleOrigin {
                    origin: decl.origin.clone(),
                    span: decl.span,
                    description: format!("label `{}` is declared locally here", decl.name),
                    local: true,
                    kind: NamedKind::Ordinary,
                },
            )
            .map_err(|error| LocatedError {
                file_path: module.file_path.clone(),
                error,
            })?;
        }
        for usage in &module.imports {
            let ImportKind::Selective { items, from } = &usage.kind else {
                continue;
            };
            let source = module_path_string(from);
            let target = index
                .modules
                .get(&source)
                .expect("surface imports were validated before label-origin checking");
            for (name, span) in items.iter().filter_map(ImportItem::as_label) {
                let Some(decl) = target.label_exports.get(name) else {
                    continue;
                };
                if !is_visible(&decl.vis, &module.path) {
                    continue;
                }
                register_label_origin(
                    &mut visible,
                    module_path,
                    name,
                    VisibleOrigin {
                        origin: decl.origin.clone(),
                        span,
                        description: format!("label `{name}` is imported from `{source}` here"),
                        local: false,
                        kind: NamedKind::Ordinary,
                    },
                )
                .map_err(|error| LocatedError {
                    file_path: module.file_path.clone(),
                    error,
                })?;
            }
        }
    }
    Ok(())
}

fn register_operator_origin(
    visible: &mut BTreeMap<String, VisibleOrigin>,
    module_path: &str,
    dispatch_key: &str,
    grammar: &crate::ast::OperatorGrammar,
    candidate: VisibleOrigin,
) -> Result<(), Error> {
    let Some(first) = visible.get(dispatch_key) else {
        visible.insert(dispatch_key.to_owned(), candidate);
        return Ok(());
    };
    if first.origin == candidate.origin {
        if !first.local && !candidate.local {
            return Err(duplicate_consumed_import(
                first,
                &candidate,
                "operator",
                &grammar.render(),
            ));
        }
        return Ok(());
    }
    let name = grammar.render();
    Err(Error::name_res(
        candidate.span,
        format!("operator `{name}` has more than one source"),
    )
    .with_secondary(first.span, first.description.clone())
    .with_help(format!(
        "give operator `{name}` one origin in module `{module_path}`: remove one import or change one local pattern"
    )))
}

fn validate_operator_origins_for_module(
    index: &RegistryIndex,
    module_path: &str,
    module: &RegistryModule,
) -> Result<(), LocatedError> {
    let mut visible = BTreeMap::new();
    for decl in &module.local_operators {
        register_operator_origin(
            &mut visible,
            module_path,
            &decl.dispatch_key,
            &decl.grammar,
            VisibleOrigin {
                origin: decl.origin.clone(),
                span: decl.span,
                description: format!(
                    "operator `{}` is declared locally here",
                    decl.grammar.render()
                ),
                local: true,
                kind: NamedKind::Ordinary,
            },
        )
        .map_err(|error| LocatedError {
            file_path: module.file_path.clone(),
            error,
        })?;
    }
    for usage in &module.imports {
        let ImportKind::Selective { items, from } = &usage.kind else {
            continue;
        };
        let source = module_path_string(from);
        let Some(target) = index.modules.get(&source) else {
            continue;
        };
        for item in items {
            let ImportItem::OperatorPattern { grammar, span, .. } = item else {
                continue;
            };
            let dispatch_key = grammar.dispatch_key().render();
            let decl = target
                .operator_exports
                .get(&dispatch_key)
                .expect("operator imports were validated before origin checking");
            register_operator_origin(
                &mut visible,
                module_path,
                &dispatch_key,
                grammar,
                VisibleOrigin {
                    origin: decl.origin.clone(),
                    span: *span,
                    description: format!(
                        "operator `{}` is imported from `{source}` here",
                        grammar.render()
                    ),
                    local: false,
                    kind: NamedKind::Ordinary,
                },
            )
            .map_err(|error| LocatedError {
                file_path: module.file_path.clone(),
                error,
            })?;
        }
    }
    Ok(())
}

fn validate_operator_origins(index: &RegistryIndex) -> Result<(), LocatedError> {
    for (module_path, module) in &index.modules {
        validate_operator_origins_for_module(index, module_path, module)?;
    }
    Ok(())
}

#[cfg(test)]
fn operator_context_index(
    module: &Module<Surface>,
    providers: &[(PathBuf, Module<Surface>)],
) -> (String, RegistryIndex) {
    let module_path = module_path_string(&module.path);
    let mut modules = BTreeMap::new();
    for (file_path, provider) in providers {
        let projection = project_module(file_path, provider);
        modules.insert(projection.module_path, projection.registry);
    }
    let projection = project_module(Path::new(""), module);
    modules.insert(projection.module_path, projection.registry);
    (module_path, RegistryIndex { modules })
}

/// Check consumer projections against the selected provider declarations.
#[cfg(test)]
pub(crate) fn validate_operator_context(
    module: &Module<Surface>,
    providers: &[(PathBuf, Module<Surface>)],
) -> Result<(), Error> {
    let (module_path, index) = operator_context_index(module, providers);
    let module = index
        .modules
        .get(&module_path)
        .expect("current operator-context module was projected");
    validate_operator_imports_for_module(&index, module).map_err(|located| located.error)
}

/// Validate Surface-only registry names before any pass consumes them.
///
/// The projected `Package<Lowered>` carries only importable declaration
/// headers and written imports. Reusing the ordinary package builder and
/// resolver gives this pass the same module-coherence, visibility, import, and
/// cycle schedule as the full environment without allowing Surface forms to
/// cross the Kio' boundary.
pub fn validate_package(
    package_root: &Path,
    modules: &[(PathBuf, Module<Surface>)],
) -> Result<(), LocatedError> {
    let mut index_modules = BTreeMap::new();
    let mut skeletons = Vec::with_capacity(modules.len());
    for (file_path, module) in modules {
        let projection = project_module(file_path, module);
        index_modules.insert(projection.module_path, projection.registry);
        skeletons.push((file_path.clone(), projection.skeleton));
    }
    let index = RegistryIndex {
        modules: index_modules,
    };

    // Local Name errors are deliberately retained but not surfaced here. The
    // Import tier must finish first; registry-related local collisions are
    // reconstructed below, and ordinary collisions remain the normal
    // resolver's responsibility after lowering.
    let (package, _deferred_name_errors) =
        Package::build_deferring_contract_checks(package_root, skeletons, None)?;
    validate_visibility(&index)?;
    validate_operator_imports(&index)?;
    validate_label_imports(&index)?;
    validate_ordinary_namespace_selection(&index)?;
    package.resolve_imports()?;
    package.check_no_value_cycles()?;
    named_origins(&index)?;
    validate_label_origins(&index)?;
    validate_operator_origins(&index)
}

/// The exact source declaration selected by an ordinary elaborator import.
#[cfg(feature = "surface")]
#[derive(Clone, Debug)]
pub(crate) struct BlockDeclaration {
    pub blocks: Vec<crate::ast::TrailingBlockDecl<Surface>>,
    pub span: Span,
    pub file: PathBuf,
    pub module: String,
    #[cfg(feature = "lsp")]
    pub name: String,
}

#[cfg(feature = "lsp")]
#[derive(Clone)]
pub(crate) struct ResolvedBlockDeclaration {
    pub source_module: String,
    pub source_name: String,
    pub module: String,
    pub name: String,
    pub span: Span,
    pub file: PathBuf,
    pub blocks: Vec<(crate::ast::BlockExposure, Option<crate::ast::PathSegment>)>,
}

/// Read-only descriptor scopes built from the same written binding origins as
/// the surface registry. Body lowering never searches provider declarations.
#[cfg(feature = "surface")]
#[derive(Debug)]
pub(crate) struct PackageBlockScope {
    modules: BTreeMap<String, BTreeMap<String, BlockDeclaration>>,
}

#[cfg(feature = "surface")]
impl PackageBlockScope {
    pub(crate) fn from_modules(
        modules: &[(PathBuf, Module<Surface>)],
    ) -> Result<Self, LocatedError> {
        let index = RegistryIndex {
            modules: modules
                .iter()
                .map(|(file, module)| {
                    let projection = project_module(file, module);
                    (projection.module_path, projection.registry)
                })
                .collect(),
        };
        let mut declarations = BTreeMap::new();
        for (file_path, module) in modules {
            let path = module_path_string(&module.path);
            for item in &module.items {
                let Item::Elaborator(def, _) = item else {
                    continue;
                };
                let mut labels = BTreeSet::new();
                for (position, block) in def.trailing_blocks.iter().enumerate() {
                    let valid = match (&block.label, position) {
                        (None, 0) => true,
                        (Some(label), n) if n != 0 => labels.insert(label.as_str()),
                        _ => false,
                    };
                    if !valid {
                        return Err(LocatedError {
                            file_path: file_path.clone(),
                            error: Error::type_(block.meta.span,
                                "the first trailing block is unlabeled; later blocks have distinct labels")
                                .with_secondary(def.name_span, "elaborator declared here"),
                        });
                    }
                }
                declarations.insert(
                    (path.clone(), def.name.clone()),
                    BlockDeclaration {
                        blocks: def.trailing_blocks.clone(),
                        span: def.name_span,
                        file: file_path.clone(),
                        module: path.clone(),
                        #[cfg(feature = "lsp")]
                        name: def.name.clone(),
                    },
                );
            }
        }
        let modules = named_origins(&index)?
            .into_iter()
            .map(|(module, origins)| {
                let selected = origins
                    .into_iter()
                    .filter_map(|(name, selected)| {
                        if selected.kind != NamedKind::Elaborator {
                            return None;
                        }
                        let SourceOrigin::Declaration {
                            module,
                            name: declared,
                            ..
                        } = selected.origin
                        else {
                            unreachable!("an elaborator has a source declaration origin")
                        };
                        let declaration = declarations
                            .get(&(module, declared))
                            .expect("selected elaborator was projected from this package")
                            .clone();
                        Some((name, declaration))
                    })
                    .collect();
                (module, selected)
            })
            .collect();
        Ok(Self { modules })
    }

    pub(crate) fn declaration(&self, module: &str, name: &str) -> Option<&BlockDeclaration> {
        self.modules.get(module)?.get(name)
    }

    #[cfg(feature = "lsp")]
    pub(crate) fn try_resolved_declarations(
        &self,
        only_source_module: Option<&str>,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Option<Vec<ResolvedBlockDeclaration>> {
        let selected = match only_source_module {
            Some(module) => self
                .modules
                .get_key_value(module)
                .into_iter()
                .collect::<Vec<_>>(),
            None => self.modules.iter().collect(),
        };
        let mut facts = Vec::new();
        for (source_module, declarations) in selected {
            for (source_name, declaration) in declarations {
                if is_cancelled() {
                    return None;
                }
                if declaration.blocks.is_empty() {
                    continue;
                }
                facts.push(ResolvedBlockDeclaration {
                    source_module: source_module.clone(),
                    source_name: source_name.clone(),
                    module: declaration.module.clone(),
                    name: declaration.name.clone(),
                    span: declaration.span,
                    file: declaration.file.clone(),
                    blocks: declaration
                        .blocks
                        .iter()
                        .map(|block| (block.exposure, block.label.clone()))
                        .collect(),
                });
            }
        }
        Some(facts)
    }
}

/// Remove selective imports whose validated targets cannot contribute to an
/// unsealed dependency contract. This runs only after [`validate_package`], so
/// the written import graph still owns missing/private-item and cycle errors.
#[cfg(feature = "cli")]
pub(crate) fn filter_unsealed_contract_imports(modules: &mut [(PathBuf, Module<Surface>)]) {
    let index = RegistryIndex {
        modules: modules
            .iter()
            .map(|(file_path, module)| {
                let projection = project_module(file_path, module);
                (projection.module_path, projection.registry)
            })
            .collect(),
    };

    for (_, module) in modules {
        let importer = module.path.clone();
        module.imports.retain_mut(|usage| {
            let ImportKind::Selective { items, from } = &mut usage.kind else {
                return true;
            };
            let Some(target) = index.modules.get(&module_path_string(from)) else {
                return true;
            };
            items.retain(|item| {
                let omitted = match item {
                    ImportItem::Name { name, .. } => {
                        let Some(declarations) = target.named_exports.get(name) else {
                            return true;
                        };
                        let mut visible = declarations
                            .iter()
                            .filter(|declaration| is_visible(&declaration.vis, &importer));
                        visible.next().is_some_and(|first| {
                            first.omitted_from_unsealed_contract
                                && visible
                                    .all(|declaration| declaration.omitted_from_unsealed_contract)
                        })
                    }
                    ImportItem::Label { name, .. } => target
                        .label_exports
                        .get(name)
                        .is_some_and(|declaration| is_visible(&declaration.vis, &importer)),
                    ImportItem::OperatorPattern { grammar, .. } => target
                        .operator_exports
                        .get(&grammar.dispatch_key().render())
                        .is_some_and(|declaration| is_visible(&declaration.vis, &importer)),
                };
                !omitted
            });
            !items.is_empty()
        });
    }
}

#[cfg(feature = "surface")]
pub(crate) fn inferred_package_root(modules: &[(PathBuf, Module<Surface>)]) -> PathBuf {
    let Some((file_path, module)) = modules.first() else {
        return PathBuf::new();
    };
    let mut root = file_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    for _ in 1..module.path.segments.len() {
        if !root.pop() {
            return PathBuf::new();
        }
    }
    root
}

#[cfg(test)]
mod tests {

    #[test]
    fn import_grammar_exact_import_match_after_parse() {
        use crate::pass::parser::parse;
        let source = "module consumer; import provider(op _ ? _ : _); fn run(a: ., b: ., c: .) -> . { a ? b : c }";
        let consumer = parse(source).expect("provider-free syntax");
        let syntax_before = crate::pretty::pretty_module(&consumer);
        let correct = "module provider; pub fn choose(a: ., b: ., c: .) -> . { a } pub op _ ? _ : _ { impl choose; };";
        let mismatch =
            "module provider; pub fn choose(a: ., b: .) -> . { a } pub op _ ? _ { impl choose; };";
        let modules =
            |provider: &str| vec![(PathBuf::from("provider.kio"), parse(provider).unwrap())];
        validate_operator_context(&consumer, &modules(correct)).expect("exact provider");
        let error = validate_operator_context(&consumer, &modules(mismatch))
            .expect_err("same-key tail mismatch");
        assert!(
            error.diag().1.contains("grammar does not match"),
            "{error:?}"
        );
        let crate::ast::ImportKind::Selective { items, .. } = &consumer.imports[0].kind else {
            panic!("selective")
        };
        let crate::ast::ImportItem::OperatorPattern { span, .. } = items[0] else {
            panic!("operator")
        };
        assert_eq!(error.diag().0, span);
        assert_eq!(crate::pretty::pretty_module(&consumer), syntax_before);
        let unrelated = format!(
            "{correct} pub fn unrelated() -> . {{ () }} pub op !! _ {{ impl unrelated; }};"
        );
        validate_operator_context(&consumer, &modules(&unrelated))
            .expect("unrelated export preserves binding");
        let variadic = parse(
            "module consumer; import provider(varop [% %]); fn run(a: .) -> . { [% (a, a) %] }",
        )
        .unwrap();
        for (head, accepted) in [("[% %]", true), ("[* *]", false)] {
            let provider = format!("module provider; pub varop {head} {{ foldl step base; }};");
            let result = validate_operator_context(&variadic, &modules(&provider));
            assert_eq!(result.is_ok(), accepted, "{result:?}");
        }
    }

    #[test]
    fn import_grammar_provider_body_preserves_projection() {
        use crate::pass::parser::parse;
        let consumer = parse(
            "module consumer; import provider(varop [% %]); fn run(a: .) -> . { [% (a, a) %] }",
        )
        .unwrap();
        let mut prior = None;
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            for suffix in ["", " finalize finish;"] {
                let source = format!(
                    "module provider; pub varop [% %] {{ {mode} different_step different_base;{suffix} }};"
                );
                let provider = parse(&source).unwrap();
                let Item::VariadicOperator(declaration, _) = &provider.items[0] else {
                    panic!("variadic")
                };
                let grammar =
                    crate::ast::OperatorGrammar::variadic(&declaration.open, &declaration.spec);
                if let Some(prior) = &prior {
                    assert_eq!(prior, &grammar);
                }
                prior = Some(grammar);
                validate_operator_context(&consumer, &[(PathBuf::from("provider.kio"), provider)])
                    .unwrap();
            }
        }
        eprintln!("semantic grammar: modes=4 finalizer_states=2 projected_identity=1");
    }
    use super::*;
    use crate::error::Diagnostic;
    #[cfg(feature = "surface")]
    use crate::pass::parser::parse;
    use crate::pass::parser::parse_lazy;

    fn parsed_headers(sources: &[(&str, &str)]) -> Vec<(PathBuf, Module<Surface>)> {
        sources
            .iter()
            .map(|(file, source)| {
                let module = parse_lazy(source)
                    .unwrap_or_else(|error| panic!("parse header {file}: {error:?}"))
                    .module()
                    .clone();
                (PathBuf::from(file), module)
            })
            .collect()
    }

    #[cfg(feature = "surface")]
    fn parsed_modules(sources: &[(&str, &str)]) -> Vec<(PathBuf, Module<Surface>)> {
        sources
            .iter()
            .map(|(file, source)| {
                (
                    PathBuf::from(file),
                    parse(source).unwrap_or_else(|error| panic!("parse {file}: {error:?}")),
                )
            })
            .collect()
    }

    fn validate(sources: &[(&str, &str)]) -> Result<(), LocatedError> {
        validate_package(Path::new(""), &parsed_headers(sources))
    }

    fn expect_name_error(sources: &[(&str, &str)], needle: &str) {
        let error = validate(sources).expect_err("expected a surface-registry name error");
        match error.error {
            Error::NameRes(Diagnostic { message, .. }) => {
                assert!(message.contains(needle), "got: {message}");
            }
            other => panic!("expected NameRes, got {other:?}"),
        }
    }

    fn expect_duplicate_import_error(sources: &[(&str, &str)], needle: &str) {
        let error = validate(sources).expect_err("expected a duplicate-import name error");
        let Error::NameRes(diagnostic) = error.error else {
            panic!("expected NameRes, got {:?}", error.error);
        };
        assert!(
            diagnostic.message.contains(needle),
            "got: {}",
            diagnostic.message
        );
        let [first] = diagnostic.secondary() else {
            panic!(
                "expected one secondary label, got {:?}",
                diagnostic.secondary()
            );
        };
        assert!(
            first.span.start < diagnostic.span.start,
            "the later import must be primary: {diagnostic:?}"
        );
        assert!(first.text.contains("first imported here"));
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("remove")),
            "the diagnostic must explain how to resolve the duplicate: {diagnostic:?}"
        );
    }

    #[test]
    fn distinct_imported_literal_origins_conflict() {
        expect_name_error(
            &[
                ("first.kio", "module first; pub literal pick = 1;"),
                ("second.kio", "module second; pub literal pick = 2;"),
                (
                    "main.kio",
                    "module main; import first(pick); import second(pick);",
                ),
            ],
            "literal alias `pick` has more than one source",
        );
    }

    #[test]
    fn repeated_import_of_one_literal_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                ("origin.kio", "module origin; pub literal pick = 1;"),
                (
                    "main.kio",
                    "module main; import origin(pick); import origin(pick);",
                ),
            ],
            "duplicate literal alias import `pick`",
        );
    }

    #[test]
    fn imported_and_local_literal_origins_conflict() {
        expect_name_error(
            &[
                ("origin.kio", "module origin; pub literal pick = 1;"),
                (
                    "main.kio",
                    "module main; import origin(pick); literal pick = 2;",
                ),
            ],
            "literal alias `pick` has more than one source",
        );
    }

    #[test]
    fn newtype_members_do_not_occupy_free_top_level_names() {
        validate(&[(
            "main.kio",
            "module main;
             newtype Box : . { pub constructor pick; pub projector choose; };
             literal pick = 1;
             literal choose = 2;",
        )])
        .expect("newtype members are selected through their type, not as free bindings");
    }

    #[test]
    fn generated_label_type_is_the_selective_import_name() {
        validate(&[
            ("origin.kio", "module origin; pub labels { item: . };"),
            ("main.kio", "module main; import origin(Item, {item});"),
        ])
        .expect("labels export their lowercase syntax and uppercase nominal");
    }

    #[test]
    fn identity_alias_and_imported_generated_label_nominal_conflict() {
        expect_name_error(
            &[
                ("origin.kio", "module origin; pub labels { item: . };"),
                (
                    "main.kio",
                    "module main; import origin(Item); import origin as source; type Item = source.Item;",
                ),
            ],
            "binding `Item` has more than one source",
        );
    }

    #[test]
    fn generic_identity_alias_and_imported_generated_label_nominal_conflict() {
        expect_name_error(
            &[
                ("origin.kio", "module origin; pub labels { wrap[A]: A };"),
                (
                    "main.kio",
                    "module main; import origin(Wrap); import origin as source; type Wrap[A] = source.Wrap(A);",
                ),
            ],
            "binding `Wrap` has more than one source",
        );
    }

    #[test]
    fn named_label_aliases_keep_distinct_declaration_origins() {
        let modules = parsed_headers(&[(
            "origin.kio",
            "module origin; \
             labels Exact[A][B] = { item[A][B]: A & B }; \
             type Forward[A][B] = Item(A, B);",
        )]);
        let projection = project_module(&modules[0].0, &modules[0].1);
        let declarations = &projection.registry.local_named;
        for (left, right) in [("Exact", "Item"), ("Forward", "Item")] {
            let origin = |name: &str| {
                &declarations
                    .iter()
                    .find(|decl| decl.name == name)
                    .unwrap()
                    .origin
            };
            assert_ne!(origin(left), origin(right));
        }
    }

    #[test]
    fn distinct_imported_label_origins_conflict() {
        expect_name_error(
            &[
                ("first.kio", "module first; pub labels { item: . };"),
                ("second.kio", "module second; pub labels { item: . };"),
                (
                    "main.kio",
                    "module main; import first({item}); import second({item});",
                ),
            ],
            "label `item` has more than one source",
        );
    }

    #[test]
    fn repeated_import_of_one_label_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                ("origin.kio", "module origin; pub labels { item: . };"),
                (
                    "main.kio",
                    "module main; import origin({item}); import origin({item});",
                ),
            ],
            "duplicate label import `item`",
        );
    }

    #[test]
    fn imported_and_local_label_origins_conflict() {
        expect_name_error(
            &[
                ("origin.kio", "module origin; pub labels { item: . };"),
                (
                    "main.kio",
                    "module main; import origin({item}); labels { item: . };",
                ),
            ],
            "label `item` has more than one source",
        );
    }

    #[test]
    fn label_and_value_imports_select_independent_namespaces() {
        validate(&[
            (
                "first.kio",
                "module first; labels { item: . }; pub fn item() -> . { () }",
            ),
            (
                "second.kio",
                "module second; fn item() -> . { () } pub labels { item: . };",
            ),
            (
                "main.kio",
                "module main; import first(item); import second({item});",
            ),
        ])
        .expect("each spelling resolves only in its selected namespace");
    }

    #[test]
    fn local_label_and_literal_alias_namespaces_coexist() {
        validate(&[(
            "main.kio",
            "module main; literal item = 1; labels { item: . };",
        )])
        .expect("literal aliases and labels occupy distinct surface namespaces");
    }

    #[test]
    fn imported_label_and_literal_alias_namespaces_coexist() {
        validate(&[
            ("labels.kio", "module labels; pub labels { item: . };"),
            ("literals.kio", "module literals; pub literal item = 1;"),
            (
                "main.kio",
                "module main; import labels({item}); import literals(item);",
            ),
        ])
        .expect("distinct surface namespaces do not create one binding origin");
    }

    #[test]
    fn bare_label_import_has_directed_braces_help() {
        let error = validate(&[
            ("origin.kio", "module origin; pub labels { item: . };"),
            ("main.kio", "module main; import origin(item);"),
        ])
        .expect_err("a bare item must not select the label namespace");
        let Error::Import(diagnostic) = error.error else {
            panic!("expected Import diagnostic")
        };
        assert!(diagnostic.message.contains("is a label, not an ordinary"));
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("import origin({item})"))
        );
    }

    #[test]
    fn braced_ordinary_import_has_directed_bare_help() {
        let error = validate(&[
            ("origin.kio", "module origin; pub fn item() -> . { () }"),
            ("main.kio", "module main; import origin({item});"),
        ])
        .expect_err("braces must not select the ordinary namespace");
        let Error::Import(diagnostic) = error.error else {
            panic!("expected Import diagnostic")
        };
        assert!(diagnostic.message.contains("does not export label `item`"));
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("import origin(item)"))
        );
    }

    #[test]
    fn distinct_imported_elaborator_origins_conflict_in_either_import_order() {
        let first = "module first; pub elab same : [Source] Source -> [Target] Target { impl implementation; };";
        let second = "module second; pub elab same : [Source] Source -> [Target] Target { impl implementation; };";
        for main in [
            "module main; import first(same); import second(same);",
            "module main; import second(same); import first(same);",
        ] {
            expect_name_error(
                &[
                    ("first.kio", first),
                    ("second.kio", second),
                    ("main.kio", main),
                ],
                "binding `same` has more than one source",
            );
        }
    }

    #[test]
    fn elaborator_and_function_conflict_in_either_declaration_order() {
        for source in [
            "module main; fn same() -> . { () } elab same : [Source] Source -> [Target] Target { impl implementation; };",
            "module main; elab same : [Source] Source -> [Target] Target { impl implementation; }; fn same() -> . { () }",
        ] {
            expect_name_error(
                &[("main.kio", source)],
                "duplicate top-level declaration `same`",
            );
        }
    }

    #[test]
    fn generated_label_nominal_and_type_conflict_in_either_declaration_order() {
        for source in [
            "module main; type Item = .; labels { item: . };",
            "module main; labels { item: . }; type Item = .;",
        ] {
            expect_name_error(
                &[("main.kio", source)],
                "duplicate top-level declaration `Item`",
            );
        }
    }

    #[test]
    fn missing_import_precedes_local_literal_collision() {
        let error = validate(&[(
            "main.kio",
            "module main; import missing(absent); literal clash = 1; fn clash() -> . { () }",
        )])
        .expect_err("the missing module must be reported first");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn import_cycle_precedes_consumed_binding_collision() {
        let error = validate(&[
            (
                "first.kio",
                "module first; pub elab same : [Source] Source -> [Target] Target { impl implementation; };",
            ),
            (
                "second.kio",
                "module second; pub elab same : [Source] Source -> [Target] Target { impl implementation; };",
            ),
            (
                "main.kio",
                "module main; import cycle as cycle; import first(same); import second(same);",
            ),
            ("cycle.kio", "module cycle; import main as main;"),
        ])
        .expect_err("the value-import cycle must be reported first");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn distinct_imported_operator_origins_conflict() {
        expect_name_error(
            &[
                (
                    "first.kio",
                    "module first; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "second.kio",
                    "module second; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "module main; import first(op + _); import second(op + _);",
                ),
            ],
            "operator `op + _` has more than one source",
        );
    }

    #[test]
    fn incompatible_imported_operator_shapes_report_origins_before_bodies() {
        let providers = [
            (
                "binary.kio",
                "module binary;
                 pub fn choose(a: ., b: .) -> . { a }
                 pub op _ ? _ { impl choose; };",
            ),
            (
                "ternary.kio",
                "module ternary;
                 pub fn choose(a: ., b: ., c: .) -> . { a }
                 pub op _ ? _ : _ { impl choose; };",
            ),
        ];
        for (imports, body, incoming) in [
            (
                "import binary(op _ ? _); import ternary(op _ ? _ : _);",
                "a ? b : c",
                "op _ ? _ : _",
            ),
            (
                "import ternary(op _ ? _ : _); import binary(op _ ? _);",
                "a ? b",
                "op _ ? _",
            ),
        ] {
            let consumer =
                format!("module main; {imports} fn run(a: ., b: ., c: .) -> . {{ {body} }}");
            expect_name_error(
                &[providers[0], providers[1], ("main.kio", consumer.as_str())],
                &format!("operator `{incoming}` has more than one source"),
            );
        }
    }

    #[test]
    fn operator_origin_collision_waits_for_every_use() {
        let error = validate(&[
            (
                "first.kio",
                "module first; pub fn f() -> . { () } pub op + _ { impl f; };",
            ),
            (
                "second.kio",
                "module second; pub fn f() -> . { () } pub op + _ { impl f; };",
            ),
            (
                "main.kio",
                "module main;
                 import first(op + _);
                 import second(op + _);
                 import missing(absent);",
            ),
        ])
        .expect_err("the unrelated missing module must be reported first");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn scoped_operator_export_respects_the_importer() {
        let error = validate(&[
            (
                "scope/provider.kio",
                "module scope/provider;
                 pub(scope) fn f() -> . { () }
                 pub(scope) op + _ { impl f; };",
            ),
            (
                "outside.kio",
                "module outside; import scope/provider(op + _);",
            ),
        ])
        .expect_err("an outside module cannot import a scoped operator");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn newtype_member_visibility_descriptions_are_qualified() {
        for (members, expected) in [
            ("pub(other) constructor mk; pub projector un;", "Tag.mk"),
            ("pub constructor mk; pub(other) projector un;", "Tag.un"),
        ] {
            let source = format!("module deep/sub; pub newtype Tag : . {{ {members} }};");
            let error = validate(&[("deep/sub.kio", source.as_str())])
                .expect_err("a non-prefix member visibility must be rejected");
            assert!(matches!(error.error, Error::Import(_)));
            assert!(
                error.error.diagnostic().message.contains(expected),
                "got: {:?}",
                error.error
            );
        }
    }

    #[test]
    fn repeated_import_of_one_operator_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                (
                    "origin.kio",
                    "module origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "module main; import origin(op + _); import origin(op + _);",
                ),
            ],
            "duplicate operator import `op + _`",
        );
    }

    #[test]
    fn repeated_import_of_one_fold_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                (
                    "origin.kio",
                    "module origin; pub varop [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "module main; import origin(varop [* *]); import origin(varop [* *]);",
                ),
            ],
            "duplicate operator import `varop [* *]`",
        );
    }

    #[test]
    fn repeated_consumed_imports_in_one_import_are_duplicates() {
        for (provider, consumer, expected) in [
            (
                "module origin; pub literal pick = 1;",
                "module main; import origin(pick, pick);",
                "duplicate literal alias import `pick`",
            ),
            (
                "module origin; pub labels { item: . };",
                "module main; import origin({item}, {item});",
                "duplicate label import `item`",
            ),
            (
                "module origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                "module main; import origin(op + _, op + _);",
                "duplicate operator import `op + _`",
            ),
            (
                "module origin; pub varop [* *] { foldr cons nil; };",
                "module main; import origin(varop [* *], varop [* *]);",
                "duplicate operator import `varop [* *]`",
            ),
        ] {
            expect_duplicate_import_error(
                &[("origin.kio", provider), ("main.kio", consumer)],
                expected,
            );
        }
    }

    #[test]
    fn repeated_consumed_imports_wait_for_every_use() {
        let error = validate(&[
            (
                "origin.kio",
                "module origin;
                 pub literal pick = 1;
                 pub labels { item: . };
                 pub fn f() -> . { () }
                 pub op + _ { impl f; };
                 pub varop [* *] { foldr cons nil; };",
            ),
            (
                "main.kio",
                "module main;
                 import origin(pick, {item}, op + _, varop [* *]);
                 import origin(pick, {item}, op + _, varop [* *]);
                 import missing(absent);",
            ),
        ])
        .expect_err("the unrelated missing module must be reported first");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn imported_operator_and_local_operator_conflict_after_import_validation() {
        expect_name_error(
            &[
                (
                    "origin.kio",
                    "module origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "module main; import origin(op + _); fn g() -> . { () } op + _ { impl g; };",
                ),
            ],
            "operator `op + _` has more than one source",
        );
    }

    #[test]
    fn incompatible_imported_and_local_shapes_have_distinct_origins() {
        expect_name_error(
            &[
                (
                    "origin.kio",
                    "module origin;
                     pub fn choose(a: ., b: .) -> . { a }
                     pub op _ ? _ { impl choose; };",
                ),
                (
                    "main.kio",
                    "module main;
                     import origin(op _ ? _);
                     fn choose(a: ., b: ., c: .) -> . { a }
                     op _ ? _ : _ { impl choose; };
                     fn run(a: ., b: ., c: .) -> . { a ? b : c }",
                ),
            ],
            "operator `op _ ? _` has more than one source",
        );
    }

    #[test]
    fn distinct_imported_fold_origins_conflict() {
        expect_name_error(
            &[
                (
                    "first.kio",
                    "module first; pub varop [* *] { foldr cons nil; };",
                ),
                (
                    "second.kio",
                    "module second; pub varop [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "module main; import first(varop [* *]); import second(varop [* *]);",
                ),
            ],
            "operator `varop [* *]` has more than one source",
        );
    }

    #[test]
    fn imported_fold_and_local_fold_have_distinct_origins() {
        expect_name_error(
            &[
                (
                    "origin.kio",
                    "module origin; pub varop [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "module main;
                     import origin(varop [* *]);
                     varop [* *] { foldr local_cons local_nil; };",
                ),
            ],
            "operator `varop [* *]` has more than one source",
        );
    }

    #[test]
    #[cfg(feature = "surface")]
    fn registry_imports_survive_full_lowering() {
        let modules = parsed_modules(&[
            (
                "origin.kio",
                "module origin;
                 pub literal one = 1;
                 pub fn id(x: .) -> . { x }
                 pub op + _ { impl id; };
                 pub fn nil() -> . { () }
                 pub fn cons(x: ., xs: .) -> . { x }
                 pub varop [* *] { foldr cons nil; };
                 pub labels { item: . };",
            ),
            (
                "main.kio",
                "module main;
                 import origin(one, Item, {item}, op + _, varop [* *]);
                 fn unary() -> . { + () }
                 fn list() -> . { [* (), () *] }
                 fn labeled() -> Item { {item=} }",
            ),
        ]);
        validate_package(Path::new(""), &modules).expect("registry imports have one origin");
        <crate::pass::full::FullPipeline as crate::pipeline::Pipeline>::lower_package_named(
            modules, None, None,
        )
        .expect("registry imports lower through the parser bindings");
    }

    #[test]
    #[cfg(feature = "surface")]
    fn direct_full_pipeline_lowering_validates_registry_origins() {
        let modules = parsed_modules(&[
            ("first.kio", "module first; pub literal pick = 1;"),
            ("second.kio", "module second; pub literal pick = 2;"),
            (
                "main.kio",
                "module main; import first(pick); import second(pick);",
            ),
        ]);
        let error = <crate::pass::full::FullPipeline as crate::pipeline::Pipeline>::lower_package(
            modules, None,
        )
        .expect_err("direct lowering must not select one literal origin");
        assert!(matches!(error.error, Error::NameRes(_)), "got: {error:?}");
    }
}

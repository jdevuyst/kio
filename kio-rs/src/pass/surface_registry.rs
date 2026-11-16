//! Pre-lowering validation for consumer-visible surface registries.
//!
//! Literal aliases and operator bindings disappear before the ordinary
//! resolver builds its environment. This pass validates their written imports
//! and binding origins while the Surface tree still carries that information,
//! before lowering erases the declarations that define those bindings.

use std::cell::Cell;
use std::collections::BTreeMap;
#[cfg(feature = "surface")]
use std::collections::BTreeSet;
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
    Macro,
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
    let mut local_operators = Vec::new();
    let mut operator_exports = BTreeMap::new();
    let mut visibility_sites = Vec::new();

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
                                    false,
                                );
                            }
                            for entry in &labels.entries {
                                add_named(
                                    &entry.name,
                                    entry.name_span,
                                    labels.vis.clone(),
                                    NamedKind::Ordinary,
                                    false,
                                );
                                if let Some(members) = &entry.members {
                                    for member in [&members.constructor, &members.projector] {
                                        add_visibility(
                                            &member.vis,
                                            member.span,
                                            format!("{}.{}", entry.name, member.name),
                                        );
                                    }
                                }
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
                        false,
                    );
                }
                for entry in &labels.entries {
                    add_named(
                        &entry.name,
                        entry.name_span,
                        labels.vis.clone(),
                        NamedKind::Ordinary,
                        false,
                    );
                    if let Some(members) = &entry.members {
                        for member in [&members.constructor, &members.projector] {
                            add_visibility(
                                &member.vis,
                                member.span,
                                format!("{}.{}", entry.name, member.name),
                            );
                        }
                    }
                }
            }
            Item::Equiv(_, _) => {}
            Item::Macro(macro_def, _) => {
                add_named(
                    &macro_def.name,
                    macro_def.name_span,
                    macro_def.vis.clone(),
                    NamedKind::Macro,
                    true,
                );
            }
            Item::Op(op, _) => {
                let crate::ast::OpBody::Normal { kind, pattern, .. } = &op.body;
                let grammar = crate::ast::OperatorGrammar::fixed(*kind, pattern);
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

    // Compact and explicit nominal declarations share the ordinary namespace.
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
                ImportItem::Name { .. } => None,
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
                    )).with_help(format!("import {} {{ {} }}", source, decl.grammar.render())),
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

/// Check the explicit macro selector against the selected ordinary declaration.
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
            for item in items {
                let ImportItem::Name {
                    name,
                    span,
                    kind,
                    kind_span,
                    ..
                } = item
                else {
                    continue;
                };
                if let Some(declarations) = target.named_exports.get(name) {
                    let expected = match kind {
                        crate::ast::ImportNameKind::Function | crate::ast::ImportNameKind::Type => {
                            NamedKind::Ordinary
                        }
                        crate::ast::ImportNameKind::Literal => NamedKind::Literal,
                        crate::ast::ImportNameKind::Macro => NamedKind::Macro,
                    };
                    if !declarations.iter().any(|decl| decl.kind == expected) {
                        let mut error = Error::import(
                            *span,
                            format!("`{name}` in module `{source}` is not a {}", kind.spelling()),
                        );
                        let candidates = declarations
                            .iter()
                            .filter(|decl| is_visible(&decl.vis, &module.path))
                            .map(|decl| match decl.kind {
                                NamedKind::Macro => crate::ast::ImportNameKind::Macro,
                                NamedKind::Literal => crate::ast::ImportNameKind::Literal,
                                NamedKind::Ordinary if crate::naming::is_type_name(name) => {
                                    crate::ast::ImportNameKind::Type
                                }
                                NamedKind::Ordinary => crate::ast::ImportNameKind::Function,
                            })
                            .collect::<Vec<_>>();
                        if let Some(actual) = candidates.first().copied()
                            && candidates.iter().all(|candidate| *candidate == actual)
                            && let Some(tag) = kind_span
                        {
                            error = error
                                .with_help(format!(
                                    "select this declaration with `{}`",
                                    actual.spelling()
                                ))
                                .with_fix(crate::error::Fix::machine_applicable(
                                    format!("Select the {} declaration", actual.spelling()),
                                    vec![crate::error::FixEdit::new(*tag, actual.spelling())],
                                ));
                        }
                        return Err(LocatedError {
                            file_path: module.file_path.clone(),
                            error,
                        });
                    }
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
                    for item in items {
                        let Some(name) = item.as_name() else {
                            continue;
                        };
                        let (local, span) = item
                            .as_local_name_with_span()
                            .expect("named import has a local name");
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
                                NamedKind::Macro => {
                                    format!("`{name}` is imported as a macro from `{source}` here")
                                }
                                NamedKind::Ordinary => {
                                    format!("`{name}` is imported from `{source}` here")
                                }
                            };
                            register_named_origin(
                                &mut visible,
                                module_path,
                                local,
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
                NamedKind::Macro => {
                    format!("`{}` is declared as a macro here", decl.name)
                }
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
            let ImportItem::OperatorPattern {
                grammar,
                span,
                alias,
                ..
            } = item
            else {
                continue;
            };
            let decl = target
                .operator_exports
                .get(&grammar.dispatch_key().render())
                .expect("operator imports were validated before origin checking");
            let (local, local_span) = alias
                .as_ref()
                .map_or((grammar, *span), |alias| (&alias.local, alias.span));
            let dispatch_key = local.dispatch_key().render();
            register_operator_origin(
                &mut visible,
                module_path,
                &dispatch_key,
                local,
                VisibleOrigin {
                    origin: decl.origin.clone(),
                    span: local_span,
                    description: format!(
                        "operator `{}` is imported from `{source}` here",
                        local.render()
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

    let package = Package::build(package_root, skeletons, None)?;
    validate_visibility(&index)?;
    validate_operator_imports(&index)?;
    validate_ordinary_namespace_selection(&index)?;
    package.resolve_imports()?;
    package.check_no_value_cycles()?;
    named_origins(&index)?;
    validate_operator_origins(&index)
}

/// The exact source declaration selected by an ordinary macro import.
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
                let Item::Macro(def, _) = item else {
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
                                .with_secondary(def.name_span, "macro declared here"),
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
                        if selected.kind != NamedKind::Macro {
                            return None;
                        }
                        let SourceOrigin::Declaration {
                            module,
                            name: declared,
                            ..
                        } = selected.origin
                        else {
                            unreachable!("a macro has a source declaration origin")
                        };
                        let declaration = declarations
                            .get(&(module, declared))
                            .expect("selected macro was projected from this package")
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
    fn wrong_named_kind_fix_replaces_only_the_written_tag() {
        use crate::pass::parser::parse;
        for (provider, source, replacement) in [
            (
                "mod provider; pub macro identity : . -> . { impl implementation; }",
                "mod consumer; import provider { fn // keep\n identity as local }",
                "macro",
            ),
            (
                "mod provider; pub fn ordinary() -> . { () }",
                "mod consumer; import provider { macro ordinary as local }",
                "fn",
            ),
            (
                "mod provider; pub lit answer = 1;",
                "mod consumer; import provider { fn answer as local }",
                "lit",
            ),
        ] {
            let consumer = parse(source).expect("consumer");
            let provider = parse(provider).expect("provider");
            let (_, index) = super::operator_context_index(
                &consumer,
                &[(PathBuf::from("provider.kio"), provider)],
            );
            let error = super::validate_ordinary_namespace_selection(&index)
                .expect_err("kind mismatch")
                .error;
            let [fix] = error.diagnostic().fixes() else {
                panic!("one authenticated kind fix")
            };
            let [edit] = fix.edits.as_slice() else {
                panic!("one edit")
            };
            assert_eq!(edit.replacement, replacement);
            let mut corrected = source.to_owned();
            corrected.replace_range(
                edit.span.start as usize..edit.span.end as usize,
                &edit.replacement,
            );
            assert!(corrected.contains("as local"));
            if source.contains("// keep") {
                assert!(corrected.contains("// keep"));
            }
            let consumer = parse(&corrected).expect("corrected selection");
            let provider = parse(provider_source_for_test(replacement)).expect("provider");
            let (_, index) = super::operator_context_index(
                &consumer,
                &[(PathBuf::from("provider.kio"), provider)],
            );
            super::validate_ordinary_namespace_selection(&index).expect("correct kind");
        }
    }

    #[test]
    fn wrong_named_kind_fix_requires_a_visible_provider() {
        use crate::pass::parser::parse;
        let consumer = parse("mod consumer; import provider { fn identity }").unwrap();
        let provider =
            parse("mod provider; macro identity : . -> . { impl implementation; }").unwrap();
        let (_, index) =
            super::operator_context_index(&consumer, &[(PathBuf::from("provider.kio"), provider)]);
        let error = super::validate_ordinary_namespace_selection(&index)
            .unwrap_err()
            .error;
        assert!(error.diagnostic().fixes().is_empty());
    }

    fn provider_source_for_test(kind: &str) -> &'static str {
        match kind {
            "macro" => "mod provider; pub macro identity : . -> . { impl implementation; }",
            "fn" => "mod provider; pub fn ordinary() -> . { () }",
            "lit" => "mod provider; pub lit answer = 1;",
            _ => unreachable!(),
        }
    }

    #[test]
    fn import_grammar_exact_import_match_after_parse() {
        use crate::pass::parser::parse;
        let source = "mod consumer; import provider {op _ ? _ : _} fn run(a: ., b: ., c: .) -> . { a ? b : c }";
        let consumer = parse(source).expect("provider-free syntax");
        let syntax_before = crate::pretty::pretty_module(&consumer);
        let correct = "mod provider; pub fn choose(a: ., b: ., c: .) -> . { a } pub op _ ? _ : _ { impl choose; };";
        let mismatch =
            "mod provider; pub fn choose(a: ., b: .) -> . { a } pub op _ ? _ { impl choose; };";
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
            "mod consumer; import provider {op_var [% %]} fn run(a: .) -> . { [% (a, a) %] }",
        )
        .unwrap();
        for (head, accepted) in [("[% %]", true), ("[* *]", false)] {
            let provider = format!("mod provider; pub op_var {head} {{ foldl step base; }};");
            let result = validate_operator_context(&variadic, &modules(&provider));
            assert_eq!(result.is_ok(), accepted, "{result:?}");
        }
    }

    #[test]
    fn import_grammar_provider_body_preserves_projection() {
        use crate::pass::parser::parse;
        let consumer = parse(
            "mod consumer; import provider {op_var [% %]} fn run(a: .) -> . { [% (a, a) %] }",
        )
        .unwrap();
        let mut prior = None;
        for mode in ["foldl", "foldr", "foldl1", "foldr1"] {
            for suffix in ["", " finalize finish;"] {
                let source = format!(
                    "mod provider; pub op_var [% %] {{ {mode} different_step different_base;{suffix} }};"
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

    fn expect_ordinary_origin_error(sources: &[(&str, &str)], needle: &str) {
        let modules = parsed_headers(sources);
        validate_package(Path::new(""), &modules).expect("surface imports validate");
        let skeletons = modules
            .iter()
            .map(|(path, module)| (path.clone(), project_module(path, module).skeleton))
            .collect();
        let package = Package::build(Path::new(""), skeletons, None).expect("ordinary headers");
        package.resolve_imports().expect("ordinary imports resolve");
        let error = package
            .check_binding_origins()
            .expect_err("conflicting written origins");
        let Error::NameRes(diagnostic) = error.error else {
            panic!("expected NameRes, got {:?}", error.error);
        };
        assert!(
            diagnostic.message.contains(needle),
            "{}",
            diagnostic.message
        );
        let [first] = diagnostic.secondary() else {
            panic!("the earlier introduction remains identified");
        };
        assert!(first.span.start < diagnostic.span.start);
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("remove"))
        );
    }

    #[test]
    fn distinct_imported_literal_origins_conflict() {
        expect_name_error(
            &[
                ("first.kio", "mod first; pub lit pick = 1;"),
                ("second.kio", "mod second; pub lit pick = 2;"),
                (
                    "main.kio",
                    "mod main; import first {lit pick} import second {lit pick}",
                ),
            ],
            "literal alias `pick` has more than one source",
        );
    }

    #[test]
    fn repeated_import_of_one_literal_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                ("origin.kio", "mod origin; pub lit pick = 1;"),
                (
                    "main.kio",
                    "mod main; import origin {lit pick} import origin {lit pick}",
                ),
            ],
            "duplicate literal alias import `pick`",
        );
    }

    #[test]
    fn imported_and_local_literal_origins_conflict() {
        expect_name_error(
            &[
                ("origin.kio", "mod origin; pub lit pick = 1;"),
                (
                    "main.kio",
                    "mod main; import origin {lit pick} lit pick = 2;",
                ),
            ],
            "literal alias `pick` has more than one source",
        );
    }

    #[test]
    fn newtype_members_do_not_occupy_free_top_level_names() {
        validate(&[(
            "main.kio",
            "mod main;
             type { Box: . { pub mk; pub get; }; }
             lit mk = 1;
             lit get = 2;",
        )])
        .expect("newtype members are selected through their type, not as free bindings");
    }

    #[test]
    fn compact_nominal_type_uses_the_ordinary_selective_import_name() {
        validate(&[
            ("origin.kio", "mod origin; pub type { Item: .; }"),
            ("main.kio", "mod main; import origin {type Item}"),
        ])
        .expect("compact nominal entries export their exact ordinary type name");
    }

    #[test]
    fn nominal_registry_uses_one_ordinary_identity_per_written_declaration() {
        let modules = parsed_headers(&[(
            "api.kio",
            "mod api; pub type { Explicit: . { pub mk; get; }; } pub type Bundle = { Compact: . { pub(api) mk; get; }; Other: .; }; type Reused = Other;",
        )]);
        let projection = project_module(&modules[0].0, &modules[0].1);
        let declarations = &projection.registry.local_named;
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.name.as_str())
                .collect::<Vec<_>>(),
            ["Explicit", "Bundle", "Compact", "Other", "Reused"],
        );
        assert!(
            declarations
                .iter()
                .all(|declaration| declaration.kind == NamedKind::Ordinary)
        );
        let sites = &projection.registry.visibility_sites;
        assert!(sites.iter().any(|site| site.description == "Compact.mk"
            && matches!(&site.vis, Visibility::PublicIn(path) if path.segments.join("/") == "api")));
        assert!(
            sites
                .iter()
                .any(|site| site.description == "Compact.get" && site.vis == Visibility::Private)
        );
    }

    #[test]
    fn identity_alias_and_imported_compact_nominal_conflict() {
        expect_ordinary_origin_error(
            &[
                ("origin.kio", "mod origin; pub type { Item: .; };"),
                (
                    "main.kio",
                    "mod main; import origin {type Item} import origin as source; type Item = source.Item;",
                ),
            ],
            "binding `Item` has more than one source",
        );
    }

    #[test]
    fn generic_identity_alias_and_imported_compact_nominal_conflict() {
        expect_ordinary_origin_error(
            &[
                ("origin.kio", "mod origin; pub type { Wrap[A]: A; };"),
                (
                    "main.kio",
                    "mod main; import origin {type Wrap} import origin as source; type Wrap[A] = source.Wrap[A];",
                ),
            ],
            "binding `Wrap` has more than one source",
        );
    }

    #[test]
    fn named_compact_aliases_keep_distinct_declaration_origins() {
        let modules = parsed_headers(&[(
            "origin.kio",
            "mod origin; type Exact[A][B] = { Item[A][B]: A & B; }; type Forward[A][B] = Item[A][ B];",
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
    fn distinct_imported_nominal_origins_conflict() {
        expect_ordinary_origin_error(
            &[
                ("first.kio", "mod first; pub type { Item: .; };"),
                ("second.kio", "mod second; pub type { Item: .; };"),
                (
                    "main.kio",
                    "mod main; import first {type Item} import second {type Item}",
                ),
            ],
            "binding `Item` has more than one source",
        );
    }

    #[test]
    fn repeated_import_of_one_nominal_origin_is_a_duplicate() {
        expect_ordinary_origin_error(
            &[
                ("origin.kio", "mod origin; pub type { Item: .; };"),
                (
                    "main.kio",
                    "mod main; import origin {type Item} import origin {type Item}",
                ),
            ],
            "binding `Item` is introduced more than once",
        );
    }

    #[test]
    fn imported_and_local_nominal_origins_conflict() {
        expect_ordinary_origin_error(
            &[
                ("origin.kio", "mod origin; pub type { Item: .; };"),
                (
                    "main.kio",
                    "mod main; import origin {type Item} type { Item: .; };",
                ),
            ],
            "binding `Item` has more than one source",
        );
    }

    #[test]
    fn nominal_and_value_imports_select_exact_case_distinct_names() {
        validate(&[
            (
                "first.kio",
                "mod first; type { Item: .; }; pub fn item() -> . { () }",
            ),
            (
                "second.kio",
                "mod second; fn item() -> . { () } pub type { Item: .; };",
            ),
            (
                "main.kio",
                "mod main; import first {fn item} import second {type Item}",
            ),
        ])
        .expect("each exact type/value spelling selects its written provider");
    }

    #[test]
    fn local_nominal_and_literal_alias_names_are_case_distinct() {
        validate(&[("main.kio", "mod main; lit item = 1; type { Item: .; };")])
            .expect("ordinary type and value spellings are distinct");
    }

    #[test]
    fn imported_nominal_and_literal_alias_names_are_case_distinct() {
        validate(&[
            ("labels.kio", "mod labels; pub type { Item: .; };"),
            ("literals.kio", "mod literals; pub lit item = 1;"),
            (
                "main.kio",
                "mod main; import labels {type Item} import literals {lit item}",
            ),
        ])
        .expect("distinct ordinary spellings do not create one binding origin");
    }

    #[test]
    fn ordinary_import_does_not_infer_a_different_case_spelling() {
        let error = validate(&[
            ("origin.kio", "mod origin; pub type { Item: .; };"),
            ("main.kio", "mod main; import origin {fn item}"),
        ])
        .expect_err("a value-shaped import cannot infer a type-shaped declaration");
        let Error::Import(diagnostic) = error.error else {
            panic!("expected Import diagnostic")
        };
        assert!(diagnostic.message.contains("does not export `item`"));
    }

    #[test]
    fn distinct_imported_macro_origins_conflict_in_either_import_order() {
        let first = "mod first; pub macro same : [Source] Source -> [Target] Target { impl implementation; };";
        let second = "mod second; pub macro same : [Source] Source -> [Target] Target { impl implementation; };";
        for main in [
            "mod main; import first {macro same} import second {macro same}",
            "mod main; import second {macro same} import first {macro same}",
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
    fn macro_and_function_conflict_in_either_declaration_order() {
        for source in [
            "mod main; fn same() -> . { () } macro same : [Source] Source -> [Target] Target { impl implementation; };",
            "mod main; macro same : [Source] Source -> [Target] Target { impl implementation; }; fn same() -> . { () }",
        ] {
            expect_name_error(
                &[("main.kio", source)],
                "duplicate top-level declaration `same`",
            );
        }
    }

    #[test]
    #[cfg(feature = "surface")]
    fn compact_nominal_and_type_conflict_in_either_declaration_order() {
        for source in [
            "mod main; type Item = .; type { Item: .; };",
            "mod main; type { Item: .; }; type Item = .;",
        ] {
            let error = <crate::pass::full::FullPipeline as crate::pipeline::Pipeline>::lower_package_named(
                parsed_modules(&[("main.kio", source)]), None, None,
            ).expect_err("ordinary nominal lowering rejects the duplicate name");
            let Error::NameRes(diagnostic) = error.error else {
                panic!("expected a name-resolution error");
            };
            assert!(diagnostic.message.contains("nominal `Item` collides"));
            assert_eq!(
                &source[diagnostic.span.start as usize..diagnostic.span.end as usize],
                "Item"
            );
            assert!(
                diagnostic
                    .help()
                    .is_some_and(|help| help.contains("rename"))
            );
        }
    }

    #[test]
    fn missing_import_with_local_literal() {
        let error = validate(&[(
            "main.kio",
            "mod main; import missing {fn absent} lit value = 1; fn distinct() -> . { () }",
        )])
        .expect_err("the missing module must be reported");
        assert!(
            matches!(error.error, Error::Import(_)),
            "got: {:?}",
            error.error
        );
    }

    #[test]
    fn import_cycle_with_consumed_declarations() {
        let error = validate(&[
            (
                "first.kio",
                "mod first; pub macro same : [Source] Source -> [Target] Target { impl implementation; };",
            ),
            (
                "second.kio",
                "mod second; pub macro same : [Source] Source -> [Target] Target { impl implementation; };",
            ),
            (
                "main.kio",
                "mod main; import cycle as cycle; import first as first; import second as second;",
            ),
            ("cycle.kio", "mod cycle; import main as main;"),
        ])
        .expect_err("the value-import cycle must be reported");
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
                    "mod first; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "second.kio",
                    "mod second; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "mod main; import first {op + _} import second {op + _}",
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
                "mod binary;
                 pub fn choose(a: ., b: .) -> . { a }
                 pub op _ ? _ { impl choose; };",
            ),
            (
                "ternary.kio",
                "mod ternary;
                 pub fn choose(a: ., b: ., c: .) -> . { a }
                 pub op _ ? _ : _ { impl choose; };",
            ),
        ];
        for (imports, body, incoming) in [
            (
                "import binary {op _ ? _} import ternary {op _ ? _ : _}",
                "a ? b : c",
                "op _ ? _ : _",
            ),
            (
                "import ternary {op _ ? _ : _} import binary {op _ ? _}",
                "a ? b",
                "op _ ? _",
            ),
        ] {
            let consumer =
                format!("mod main; {imports} fn run(a: ., b: ., c: .) -> . {{ {body} }}");
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
                "mod first; pub fn f() -> . { () } pub op + _ { impl f; };",
            ),
            (
                "second.kio",
                "mod second; pub fn f() -> . { () } pub op + _ { impl f; };",
            ),
            (
                "main.kio",
                "mod main;
                 import first {op + _}
                 import second {op + _}
                 import missing {fn absent}",
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
                "mod scope/provider;
                 pub(scope) fn f() -> . { () }
                 pub(scope) op + _ { impl f; };",
            ),
            ("outside.kio", "mod outside; import scope/provider {op + _}"),
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
            ("pub(other) mk; pub get;", "Tag.mk"),
            ("pub mk; pub(other) get;", "Tag.get"),
        ] {
            for declaration in [
                format!("pub type {{ Tag: . {{ {members} }}; }}"),
                format!("pub type {{ Tag: . {{ {members} }}; Other: .; }}"),
                format!("pub type Alias = {{ Tag: . {{ {members} }}; }};"),
                format!(
                    "rec {{ pub type {{ Tag: Peer {{ {members} }}; }}; type {{ Peer: Tag; }}; }}"
                ),
            ] {
                let source = format!("mod deep/sub; {declaration}");
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
    }

    #[test]
    fn repeated_import_of_one_operator_origin_is_a_duplicate() {
        expect_duplicate_import_error(
            &[
                (
                    "origin.kio",
                    "mod origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "mod main; import origin {op + _} import origin {op + _}",
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
                    "mod origin; pub op_var [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "mod main; import origin {op_var [* *]} import origin {op_var [* *]}",
                ),
            ],
            "duplicate operator import `op_var [* *]`",
        );
    }

    #[test]
    fn repeated_nominal_items_in_one_import_are_duplicates() {
        expect_ordinary_origin_error(
            &[
                ("origin.kio", "mod origin; pub type { Item: .; };"),
                ("main.kio", "mod main; import origin {type Item; type Item}"),
            ],
            "binding `Item` is introduced more than once",
        );
    }

    #[test]
    fn repeated_consumed_imports_in_one_import_are_duplicates() {
        for (provider, consumer, expected) in [
            (
                "mod origin; pub lit pick = 1;",
                "mod main; import origin {lit pick; lit pick}",
                "duplicate literal alias import `pick`",
            ),
            (
                "mod origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                "mod main; import origin {op + _; op + _}",
                "duplicate operator import `op + _`",
            ),
            (
                "mod origin; pub op_var [* *] { foldr cons nil; };",
                "mod main; import origin {op_var [* *]; op_var [* *]}",
                "duplicate operator import `op_var [* *]`",
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
                "mod origin;
                 pub lit pick = 1;
                 pub type { Item: .; };
                 pub fn f() -> . { () }
                 pub op + _ { impl f; };
                 pub op_var [* *] { foldr cons nil; };",
            ),
            (
                "main.kio",
                "mod main;
                 import origin {lit pick; type Item; op + _; op_var [* *]}
                 import origin {lit pick; type Item; op + _; op_var [* *]}
                 import missing {fn absent}",
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
                    "mod origin; pub fn f() -> . { () } pub op + _ { impl f; };",
                ),
                (
                    "main.kio",
                    "mod main; import origin {op + _} fn g() -> . { () } op + _ { impl g; };",
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
                    "mod origin;
                     pub fn choose(a: ., b: .) -> . { a }
                     pub op _ ? _ { impl choose; };",
                ),
                (
                    "main.kio",
                    "mod main;
                     import origin {op _ ? _}
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
                    "mod first; pub op_var [* *] { foldr cons nil; };",
                ),
                (
                    "second.kio",
                    "mod second; pub op_var [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "mod main; import first {op_var [* *]} import second {op_var [* *]}",
                ),
            ],
            "operator `op_var [* *]` has more than one source",
        );
    }

    #[test]
    fn imported_fold_and_local_fold_have_distinct_origins() {
        expect_name_error(
            &[
                (
                    "origin.kio",
                    "mod origin; pub op_var [* *] { foldr cons nil; };",
                ),
                (
                    "main.kio",
                    "mod main;
                     import origin {op_var [* *]}
                     op_var [* *] { foldr local_cons local_nil; };",
                ),
            ],
            "operator `op_var [* *]` has more than one source",
        );
    }

    #[test]
    #[cfg(feature = "surface")]
    fn registry_imports_survive_full_lowering() {
        let modules = parsed_modules(&[
            (
                "origin.kio",
                "mod origin;
                 pub lit one = 1;
                 pub fn id(x: .) -> . { x }
                 pub op + _ { impl id; };
                 pub fn nil() -> . { () }
                 pub fn cons(x: ., xs: .) -> . { x }
                 pub op_var [* *] { foldr cons nil; };
                 pub type { Item: .; };",
            ),
            (
                "main.kio",
                "mod main;
                 import origin {lit one; type Item; op + _; op_var [* *]}
                 fn unary() -> . { + () }
                 fn list() -> . { [* (), () *] }
                 fn nominal() -> Item { Item.mk(()) }",
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
            ("first.kio", "mod first; pub lit pick = 1;"),
            ("second.kio", "mod second; pub lit pick = 2;"),
            (
                "main.kio",
                "mod main; import first {lit pick} import second {lit pick}",
            ),
        ]);
        let error = <crate::pass::full::FullPipeline as crate::pipeline::Pipeline>::lower_package(
            modules, None,
        )
        .expect_err("direct lowering must not select one literal origin");
        assert!(matches!(error.error, Error::NameRes(_)), "got: {error:?}");
    }
}

#[cfg(test)]
mod macro_import_tests {
    use super::*;

    fn validate(sources: &[(&str, &str)]) -> Result<(), LocatedError> {
        let modules = sources
            .iter()
            .map(|(path, source)| {
                (
                    PathBuf::from(path),
                    crate::pass::parser::parse(source).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        validate_package(Path::new(""), &modules)
    }

    #[test]
    fn qualified_alias_and_macro_binding_conflict_without_body_uses() {
        let provider = "mod provider; pub macro selected: . -> . { impl helper; }";
        for consumer in [
            "mod consumer; import provider as selected; import provider {macro selected}",
            "mod consumer; import provider {macro selected} import provider as selected;",
            "mod consumer; import provider as selected; macro selected: . -> . { impl helper; }",
        ] {
            let error =
                validate(&[("provider.kio", provider), ("consumer.kio", consumer)]).unwrap_err();
            assert!(matches!(error.error, Error::NameRes(_)), "{error:?}");
            assert!(
                error
                    .error
                    .diagnostic()
                    .message
                    .contains("more than one source"),
                "{error:?}"
            );
        }
    }

    #[test]
    fn macro_kind_is_validated_even_without_calls() {
        let provider =
            "mod provider; pub macro selected: . -> . { impl helper; } pub fn ordinary() { () }";
        for (consumer, actual_kind) in [
            ("mod consumer; import provider {fn selected}", "macro"),
            ("mod consumer; import provider {macro ordinary}", "fn"),
        ] {
            let error =
                validate(&[("provider.kio", provider), ("consumer.kio", consumer)]).unwrap_err();
            assert!(matches!(error.error, Error::Import(_)), "{error:?}");
            assert!(
                error.error.diagnostic().fixes()[0].edits[0].replacement == actual_kind,
                "{error:?}"
            );
        }
        validate(&[
            ("provider.kio", provider),
            (
                "consumer.kio",
                "mod consumer; import provider {macro selected as use_selected; fn ordinary}",
            ),
        ])
        .unwrap();
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use crate::ast::{
    Desugared, ImportItem, ImportKind, Item, LabelForward, Module, ModulePath, Visibility,
};
use crate::error::Error;
use crate::pass::resolve::{LocatedError, is_visible, visibility_covers};
use crate::span::Span;

use super::{LabelInfo, LabelOrigin, LabelTable, module_path_to_string};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LabelKey {
    module: String,
    name: String,
}

#[derive(Clone, Copy)]
enum Declaration<'a> {
    Minted(&'a LabelInfo),
    Forward(&'a LabelForward<Desugared>),
}

impl<'a> Declaration<'a> {
    fn visibility(self) -> &'a Visibility {
        match self {
            Self::Minted(info) => &info.vis,
            Self::Forward(forward) => &forward.vis,
        }
    }

    fn span(self) -> Span {
        match self {
            Self::Minted(info) => info.declaration_span,
            Self::Forward(forward) => forward.name_span,
        }
    }
}

struct Selection {
    key: LabelKey,
    imported: bool,
}

#[derive(Default)]
struct Namespace {
    labels: BTreeMap<String, Vec<Selection>>,
    qualified: BTreeMap<String, Vec<String>>,
}

impl Namespace {
    fn target(&self, forward: &LabelForward<Desugared>) -> Result<Selection, Error> {
        let missing = || {
            Error::name_res(
                forward.target_span,
                format!("label `{}` is not in scope", forward.target),
            )
        };
        let ambiguous = || {
            Error::name_res(
                forward.target_span,
                format!(
                    "label `{}` has more than one written binding",
                    forward.target
                ),
            )
            .with_help("remove the duplicate introduction before forwarding this label")
        };
        if let Some((alias, name)) = forward.target.split_once('.') {
            let providers = self.qualified.get(alias).ok_or_else(missing)?;
            let [provider] = providers.as_slice() else {
                return Err(ambiguous());
            };
            Ok(Selection {
                key: LabelKey {
                    module: provider.clone(),
                    name: name.to_owned(),
                },
                imported: true,
            })
        } else {
            let candidates = self.labels.get(&forward.target).ok_or_else(missing)?;
            let [candidate] = candidates.as_slice() else {
                return Err(ambiguous());
            };
            Ok(Selection {
                key: candidate.key.clone(),
                imported: candidate.imported,
            })
        }
    }
}

pub(super) fn complete_forwarded_labels(
    parsed: &[(PathBuf, Module<Desugared>)],
    local: &mut HashMap<String, LabelTable>,
) -> Result<(), LocatedError> {
    if !parsed.iter().any(|(_, module)| {
        module
            .items
            .iter()
            .any(|item| matches!(item, Item::LabelForward(_, _)))
    }) {
        return Ok(());
    }
    let modules: BTreeMap<_, _> = parsed
        .iter()
        .map(|(file, module)| (module_path_to_string(&module.path), (file, module)))
        .collect();
    let locate = |module: &str, error| LocatedError {
        file_path: modules[module].0.clone(),
        error,
    };
    let mut declarations = BTreeMap::new();
    let mut namespaces = BTreeMap::<String, Namespace>::new();
    for (module_name, (_, module)) in &modules {
        let namespace = namespaces.entry(module_name.clone()).or_default();
        for (name, info) in &local[module_name] {
            let key = LabelKey {
                module: module_name.clone(),
                name: name.clone(),
            };
            declarations.insert(key.clone(), Declaration::Minted(info));
            namespace
                .labels
                .entry(name.clone())
                .or_default()
                .push(Selection {
                    key,
                    imported: false,
                });
        }
        for item in &module.items {
            let Item::LabelForward(forward, _) = item else {
                continue;
            };
            let key = LabelKey {
                module: module_name.clone(),
                name: forward.name.clone(),
            };
            if let Some(first) = declarations.insert(key.clone(), Declaration::Forward(forward)) {
                return Err(locate(
                    module_name,
                    Error::name_res(
                        forward.name_span,
                        format!(
                            "label `{}` has more than one local declaration",
                            forward.name
                        ),
                    )
                    .with_secondary(first.span(), "the other declaration is here"),
                ));
            }
            namespace
                .labels
                .entry(forward.name.clone())
                .or_default()
                .push(Selection {
                    key,
                    imported: false,
                });
        }
        for use_ in &module.imports {
            match &use_.kind {
                ImportKind::Qualified { path, alias } => {
                    namespace
                        .qualified
                        .entry(alias.clone())
                        .or_default()
                        .push(module_path_to_string(path));
                }
                ImportKind::Selective { items, from } => {
                    for (name, _) in items.iter().filter_map(ImportItem::as_label) {
                        namespace
                            .labels
                            .entry(name.to_owned())
                            .or_default()
                            .push(Selection {
                                key: LabelKey {
                                    module: module_path_to_string(from),
                                    name: name.to_owned(),
                                },
                                imported: true,
                            });
                    }
                }
                ImportKind::Intrinsics | ImportKind::Comptime => {}
            }
        }
    }

    let mut edges = BTreeMap::new();
    let mut resolved = BTreeMap::new();
    for (key, declaration) in &declarations {
        let Declaration::Forward(forward) = declaration else {
            let Declaration::Minted(info) = declaration else {
                unreachable!("a non-forward label declaration has an explicit minting origin")
            };
            resolved.insert(key.clone(), (*info).clone());
            continue;
        };
        let selection = namespaces[&key.module]
            .target(forward)
            .map_err(|error| locate(&key.module, error))?;
        let target = declarations.get(&selection.key).ok_or_else(|| {
            locate(
                &key.module,
                Error::name_res(
                    forward.target_span,
                    format!(
                        "label `{}` is not declared in module `{}`",
                        selection.key.name, selection.key.module
                    ),
                ),
            )
        })?;
        let owner = &modules[&key.module].1.path;
        let target_owner = &modules[&selection.key.module].1.path;
        validate_forward_visibility(
            forward,
            owner,
            target.visibility(),
            target_owner,
            selection.imported,
        )
        .map_err(|error| locate(&key.module, error))?;
        edges.insert(key.clone(), selection.key);
    }

    for start in edges.keys() {
        let mut current = start.clone();
        let mut active = BTreeSet::new();
        let mut path = Vec::new();
        while !resolved.contains_key(&current) {
            if !active.insert(current.clone()) {
                let Declaration::Forward(forward) = declarations[&current] else {
                    unreachable!("only forwarding declarations enter the unresolved path")
                };
                return Err(locate(
                    &current.module,
                    Error::name_res(
                        forward.target_span,
                        format!(
                            "label forwarding cycle through `{}` has no explicit label declaration",
                            current.name
                        ),
                    )
                    .with_help("forward the family to an explicit `labels` declaration"),
                ));
            }
            path.push(current.clone());
            current = edges[&current].clone();
        }
        let terminal = &resolved[&current];
        let terminal_module = terminal.newtype_module.clone();
        let terminal_name = terminal.newtype_name.clone();
        for key in path.into_iter().rev() {
            let Declaration::Forward(forward) = declarations[&key] else {
                unreachable!("only forwarding declarations need terminal resolution")
            };
            resolved.insert(
                key,
                LabelInfo {
                    newtype_name: terminal_name.clone(),
                    newtype_module: terminal_module.clone(),
                    declaration_span: forward.name_span,
                    vis: forward.vis.clone(),
                    accessible: true,
                    origin: LabelOrigin::Local,
                },
            );
        }
    }

    let forwards = edges
        .keys()
        .map(|key| (key.clone(), resolved[key].clone()))
        .collect::<Vec<_>>();
    for (key, info) in forwards {
        local
            .get_mut(&key.module)
            .expect("forwarding owner has a module table")
            .insert(key.name, info);
    }
    Ok(())
}

fn validate_forward_visibility(
    forward: &LabelForward<Desugared>,
    owner: &ModulePath,
    target_visibility: &Visibility,
    target_owner: &ModulePath,
    imported: bool,
) -> Result<(), Error> {
    if imported && !is_visible(target_visibility, owner) {
        return Err(Error::import(
            forward.target_span,
            format!(
                "label `{}` is not visible from module `{}`",
                forward.target,
                module_path_to_string(owner),
            ),
        ));
    }
    if !visibility_covers(target_visibility, target_owner, &forward.vis, owner) {
        return Err(Error::import(forward.name_span, format!(
            "forwarded label `{}` is visible outside its target label's visibility", forward.name,
        )).with_help("reduce the forwarding declaration's visibility or expose its target at the required scope"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{PackageLabelTables, elaborate_package};
    use super::*;

    fn modules(sources: &[&str]) -> Vec<(PathBuf, Module<Desugared>)> {
        sources
            .iter()
            .map(|source| {
                let parsed = crate::pass::parser::parse(source).expect("parse label modules");
                let file = PathBuf::from(format!("{}.kio", module_path_to_string(&parsed.path)));
                let desugared =
                    crate::pass::desugar::desugar_module(parsed).expect("desugar label modules");
                (file, desugared)
            })
            .collect()
    }

    #[test]
    fn renamed_forwarding_chain_preserves_the_explicit_terminal() {
        let parsed = modules(&[
            "module origin; pub labels { foo: . };",
            "module middle; import origin as o; pub type {bar} = {o.foo};",
            "module outer; import middle({bar}); pub type {baz} = {bar};",
        ]);
        let tables = PackageLabelTables::build(&parsed).expect("resolve explicit forwarding chain");
        for (module, label) in [("middle", "bar"), ("outer", "baz")] {
            let info = &tables.local[module][label];
            assert_eq!(info.newtype_name, "Foo");
            assert_eq!(module_path_to_string(&info.newtype_module), "origin");
            assert_eq!(tables.local[module].len(), 1);
        }
        assert!(
            parsed[1]
                .1
                .items
                .iter()
                .all(|item| !matches!(item, Item::Newtype(_) | Item::TypeAlias(_)))
        );
    }

    #[test]
    fn qualified_forwarding_adds_terminal_import_only_at_a_label_use() {
        let provider = "module provider; import origin as o; pub type {bar} = {o.foo};";
        let origin = "module origin; pub labels { foo: . };";
        for (consumer, expected) in [
            (
                "module consumer; import provider as p; fn idle() -> . { () }",
                vec!["provider"],
            ),
            (
                "module consumer; import provider as p; fn value() { {p.bar = ()} }",
                vec!["origin", "provider"],
            ),
        ] {
            let (lowered, _) = elaborate_package(modules(&[origin, provider, consumer]), None)
                .expect("lower exact forwarding use");
            let consumer = &lowered
                .iter()
                .find(|(_, module)| module_path_to_string(&module.path) == "consumer")
                .expect("consumer module")
                .1;
            let mut providers = consumer
                .imports
                .iter()
                .filter_map(|use_| match &use_.kind {
                    ImportKind::Qualified { path, .. } => Some(module_path_to_string(path)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            providers.sort();
            assert_eq!(providers, expected);
        }
    }

    #[test]
    fn unused_upstream_forward_does_not_change_the_lowered_consumer() {
        let origin = "module origin; pub labels { foo: . };";
        let consumer = "module consumer; import provider as p; fn idle() -> . { () }";
        let lower_consumer = |provider| {
            let (lowered, _) = elaborate_package(modules(&[origin, provider, consumer]), None)
                .expect("lower provider extension");
            lowered
                .into_iter()
                .find(|(_, module)| module_path_to_string(&module.path) == "consumer")
                .expect("consumer module")
                .1
        };
        assert_eq!(
            lower_consumer("module provider; import origin as o;"),
            lower_consumer("module provider; import origin as o; pub type {bar} = {o.foo};"),
        );
    }

    #[test]
    fn terminal_equality_does_not_excuse_duplicate_forward_declarations() {
        let parsed = modules(&[
            "module origin; pub labels { foo: . };",
            "module provider; import origin as o; pub type {bar} = {o.foo}; pub type {bar} = {o.foo};",
        ]);
        let error = PackageLabelTables::build(&parsed)
            .err()
            .expect("reject duplicate introduction");
        assert!(
            error
                .error
                .diag()
                .1
                .contains("more than one local declaration")
        );
    }

    #[test]
    fn forwarding_requires_a_concrete_origin_and_does_not_supply_reuse_markers() {
        for (source, expected) in [
            (
                "module x; type {a} = {b}; type {b} = {a};",
                "forwarding cycle",
            ),
            (
                "module x; labels { foo: . }; type {bar} = {foo}; labels Later = { bar: _ };",
                "earlier",
            ),
        ] {
            let error = PackageLabelTables::build(&modules(&[source]))
                .err()
                .expect("reject invalid origin");
            assert!(error.error.diag().1.contains(expected), "{error:?}");
        }
    }

    #[test]
    fn forwarding_visibility_uses_scope_containment() {
        let origin = "module root/source; pub(root) labels { foo: . };";
        let valid =
            "module root/view; import root/source as o; pub(root/view) type {bar} = {o.foo};";
        assert!(PackageLabelTables::build(&modules(&[origin, valid])).is_ok());
        let too_wide = "module root/view; import root/source as o; pub type {bar} = {o.foo};";
        let error = PackageLabelTables::build(&modules(&[origin, too_wide]))
            .err()
            .expect("reject wider forward");
        assert!(
            error
                .error
                .diag()
                .1
                .contains("outside its target label's visibility")
        );
        let private = "module root/source; labels { foo: . };";
        let local = "module root/view; import root/source as o; type {bar} = {o.foo};";
        let error = PackageLabelTables::build(&modules(&[private, local]))
            .err()
            .expect("reject private import edge");
        assert!(error.error.diag().1.contains("not visible"));
    }
}

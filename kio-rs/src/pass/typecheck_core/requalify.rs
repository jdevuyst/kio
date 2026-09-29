use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::{
    Import, ImportItem, ImportKind, Module, ModulePath, PathSegment, Type, TypeParam, TypeRecMember,
};
use crate::pass::resolve::ResolvePhase;
use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PrimeLocalTypeAlias {
    pub(crate) alias: String,
    pub(crate) nominal: String,
    pub(crate) params: Vec<TypeParam>,
    pub(crate) span: Span,
}

/// Re-spells identity-canonical type paths into bindings that are valid in
/// one emitted Prime module. Source-spelled paths must bypass this helper so
/// a user alias keeps its written meaning.
#[derive(Debug)]
pub(crate) struct PrimeTypeRequalifier {
    current_module: String,
    selective_leaf_module: HashMap<String, String>,
    module_aliases: HashMap<String, BTreeSet<String>>,
    used_names: HashSet<String>,
    local_nominal_params: HashMap<String, Vec<TypeParam>>,
    reserved_local_aliases: HashMap<String, Vec<PrimeLocalTypeAlias>>,
    to_inject: Vec<Import>,
    local_aliases: Vec<PrimeLocalTypeAlias>,
    alias_counter: u32,
    namespace: String,
    span: Span,
}

impl PrimeTypeRequalifier {
    pub(crate) fn new<P>(module: &Module<P>) -> Self
    where
        P: ResolvePhase,
    {
        Self::new_with_namespace(module, "")
    }

    pub(crate) fn new_with_namespace<P>(module: &Module<P>, namespace: &str) -> Self
    where
        P: ResolvePhase,
    {
        let mut selective_leaf_module = HashMap::new();
        let mut module_aliases: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut used_names = HashSet::new();
        let mut local_nominal_params = HashMap::new();
        for import_ in &module.imports {
            match &import_.kind {
                ImportKind::Selective { items, from } => {
                    let module = module_path_key(from);
                    for name in items.iter().filter_map(ImportItem::as_name) {
                        selective_leaf_module
                            .entry(name.to_owned())
                            .or_insert_with(|| module.clone());
                        used_names.insert(name.to_owned());
                    }
                }
                ImportKind::Qualified { path, alias } => {
                    module_aliases
                        .entry(module_path_key(path))
                        .or_default()
                        .insert(alias.clone());
                    used_names.insert(alias.clone());
                }
                ImportKind::Intrinsics | ImportKind::Comptime => {}
            }
        }
        for item in &module.items {
            if let crate::ast::Item::TypeRecGroup(group) = item {
                for member in &group.members {
                    match member {
                        TypeRecMember::TypeAlias(alias) => {
                            used_names.insert(alias.name.clone());
                        }
                        TypeRecMember::Newtype(newtype) => {
                            used_names.insert(newtype.name.clone());
                            used_names.insert(newtype.constructor.name.clone());
                            used_names.insert(newtype.projector.name.clone());
                            local_nominal_params
                                .insert(newtype.name.clone(), newtype.type_params.clone());
                        }
                        TypeRecMember::Labels(_, ext) => match *ext {},
                    }
                }
                continue;
            }
            let name = crate::pass::resolve::item_name(item);
            if !name.is_empty() {
                used_names.insert(name.to_owned());
            }
            if let crate::ast::Item::Newtype(newtype) = item {
                used_names.insert(newtype.constructor.name.clone());
                used_names.insert(newtype.projector.name.clone());
                local_nominal_params.insert(newtype.name.clone(), newtype.type_params.clone());
            }
            if let crate::ast::Item::HostType(host) = item {
                local_nominal_params.insert(host.name.clone(), host.type_params.clone());
            }
        }
        Self {
            current_module: module_path_key(&module.path),
            selective_leaf_module,
            module_aliases,
            used_names,
            local_nominal_params,
            reserved_local_aliases: HashMap::new(),
            to_inject: Vec::new(),
            local_aliases: Vec::new(),
            alias_counter: 0,
            namespace: namespace.to_owned(),
            span: module.meta.span,
        }
    }

    pub(crate) fn rewrite<P>(&mut self, ty: &Type<P>, bound: &HashSet<String>) -> Type<P>
    where
        P: ResolvePhase + Clone,
    {
        match ty {
            Type::Path {
                segments,
                args,
                meta,
            } => {
                let args = args.iter().map(|arg| self.rewrite(arg, bound)).collect();
                let segments = if segments.len() >= 2 {
                    let leaf = segments
                        .last()
                        .expect("qualified path has a leaf")
                        .name
                        .clone();
                    let module = segments[..segments.len() - 1]
                        .iter()
                        .map(PathSegment::as_str)
                        .collect::<Vec<_>>()
                        .join("/");
                    self.bound_spelling(&module, &leaf, bound)
                        .into_iter()
                        .map(|name| PathSegment::new(name, meta.span))
                        .collect()
                } else {
                    segments.clone()
                };
                Type::Path {
                    segments,
                    args,
                    meta: meta.clone(),
                }
            }
            Type::Function {
                param,
                ret,
                meta,
                abi_arity,
                caps,
            } => Type::Function {
                param: Box::new(self.rewrite(param, bound)),
                ret: Box::new(self.rewrite(ret, bound)),
                meta: meta.clone(),
                abi_arity: *abi_arity,
                caps: caps.clone(),
            },
            Type::Product { left, right, meta } => Type::Product {
                left: Box::new(self.rewrite(left, bound)),
                right: Box::new(self.rewrite(right, bound)),
                meta: meta.clone(),
            },
            Type::Sum { left, right, meta } => Type::Sum {
                left: Box::new(self.rewrite(left, bound)),
                right: Box::new(self.rewrite(right, bound)),
                meta: meta.clone(),
            },
            Type::Forall { param, body, meta } => {
                let mut inner = bound.clone();
                inner.insert(param.name.clone());
                Type::Forall {
                    param: param.clone(),
                    body: Box::new(self.rewrite(body, &inner)),
                    meta: meta.clone(),
                }
            }
            Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => ty.clone(),
            Type::Goal {
                goal,
                args,
                meta,
                ext,
            } => Type::Goal {
                goal: *goal,
                args: args.iter().map(|arg| self.rewrite(arg, bound)).collect(),
                meta: meta.clone(),
                ext: ext.clone(),
            },
            Type::LabelSugar { ext, .. } => match *ext {},
        }
    }

    pub(crate) fn take_imports(&mut self) -> Vec<Import> {
        std::mem::take(&mut self.to_inject)
    }

    pub(crate) fn take_local_aliases(&mut self) -> Vec<PrimeLocalTypeAlias> {
        std::mem::take(&mut self.local_aliases)
    }

    #[cfg(feature = "surface")]
    pub(crate) fn ensure_module_import(&mut self, module: &str) -> String {
        self.ensure_module_import_avoiding(module, &BTreeSet::new())
    }

    #[cfg(feature = "surface")]
    pub(crate) fn ensure_module_import_avoiding(
        &mut self,
        module: &str,
        forbidden: &BTreeSet<String>,
    ) -> String {
        if let Some(alias) = self
            .module_aliases
            .get(module)
            .and_then(|aliases| aliases.iter().find(|alias| !forbidden.contains(*alias)))
        {
            return alias.clone();
        }
        let alias = loop {
            let candidate = self.fresh_alias(module);
            if !forbidden.contains(&candidate) {
                break candidate;
            }
        };
        self.to_inject.push(Import {
            trailing_trivia: Vec::new(),
            kind: ImportKind::Qualified {
                path: module_path_from_key(module, self.span),
                alias: alias.clone(),
            },
            span: self.span,
            leading_trivia: Vec::new(),
        });
        self.module_aliases
            .entry(module.to_owned())
            .or_default()
            .insert(alias.clone());
        alias
    }

    #[cfg(feature = "surface")]
    pub(crate) fn add_generated_import(&mut self, module: &str, alias: &str) {
        let aliases = self.module_aliases.entry(module.to_owned()).or_default();
        aliases.insert(alias.to_owned());
        self.used_names.insert(alias.to_owned());
    }

    #[cfg(feature = "surface")]
    pub(crate) fn reserve_local_alias(&mut self, alias: &PrimeLocalTypeAlias) {
        self.used_names.insert(alias.alias.clone());
        let aliases = self
            .reserved_local_aliases
            .entry(alias.nominal.clone())
            .or_default();
        if !aliases.iter().any(|existing| existing.alias == alias.alias) {
            aliases.push(alias.clone());
        }
    }

    fn bound_spelling(&mut self, module: &str, leaf: &str, bound: &HashSet<String>) -> Vec<String> {
        if module == self.current_module {
            if !bound.contains(leaf) {
                return vec![leaf.to_owned()];
            }
            if let Some(alias) = self
                .reserved_local_aliases
                .get(leaf)
                .and_then(|aliases| aliases.iter().find(|alias| !bound.contains(&alias.alias)))
            {
                return vec![alias.alias.clone()];
            }
            let alias = self.fresh_local_alias(leaf, bound);
            let params = self
                .local_nominal_params
                .get(leaf)
                .cloned()
                .unwrap_or_default();
            let params = params
                .into_iter()
                .enumerate()
                .map(|(index, mut param)| {
                    param.name = self.fresh_type_param(index, leaf, bound);
                    param
                })
                .collect();
            self.local_aliases.push(PrimeLocalTypeAlias {
                alias: alias.clone(),
                nominal: leaf.to_owned(),
                params,
                span: self.span,
            });
            return vec![alias];
        }
        if self
            .selective_leaf_module
            .get(leaf)
            .is_some_and(|selected| selected == module)
            && !bound.contains(leaf)
        {
            return vec![leaf.to_owned()];
        }
        if let Some(alias) = self
            .module_aliases
            .get(module)
            .and_then(|aliases| aliases.iter().find(|alias| !bound.contains(*alias)))
        {
            return vec![alias.clone(), leaf.to_owned()];
        }
        let alias = self.fresh_alias(module);
        self.to_inject.push(Import {
            trailing_trivia: Vec::new(),
            kind: ImportKind::Qualified {
                path: module_path_from_key(module, self.span),
                alias: alias.clone(),
            },
            span: self.span,
            leading_trivia: Vec::new(),
        });
        self.module_aliases
            .entry(module.to_owned())
            .or_default()
            .insert(alias.clone());
        vec![alias, leaf.to_owned()]
    }

    fn fresh_alias(&mut self, module: &str) -> String {
        if !self.namespace.is_empty() {
            let namespace = crate::naming::encode_name_component(&self.namespace);
            let module_word = crate::naming::encode_name_component(module);
            for salt in 0u32.. {
                let candidate = format!("_q{namespace}_m{module_word}_s{salt}");
                if self.used_names.insert(candidate.clone()) {
                    return candidate;
                }
            }
            unreachable!("u32 alias salt space is inexhaustible")
        }
        loop {
            let candidate = format!("_q{}", self.alias_counter);
            self.alias_counter += 1;
            if self.used_names.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    fn fresh_local_alias(&mut self, nominal: &str, bound: &HashSet<String>) -> String {
        if !self.namespace.is_empty() {
            let namespace = crate::naming::encode_name_component(&self.namespace);
            let nominal_word = crate::naming::encode_name_component(nominal);
            for salt in 0u32.. {
                let candidate = format!("Kio{namespace}_m{nominal_word}_s{salt}");
                if !bound.contains(&candidate) && self.used_names.insert(candidate.clone()) {
                    return candidate;
                }
            }
            unreachable!("u32 local-alias salt space is inexhaustible")
        }
        loop {
            let candidate = format!("Kioq{}", self.alias_counter);
            self.alias_counter += 1;
            if !bound.contains(&candidate) && self.used_names.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    fn fresh_type_param(&mut self, index: usize, nominal: &str, bound: &HashSet<String>) -> String {
        let mut salt = 0u32;
        loop {
            let candidate = format!("Qp{index}_s{salt}");
            salt += 1;
            if candidate != nominal
                && !bound.contains(&candidate)
                && self.used_names.insert(candidate.clone())
            {
                return candidate;
            }
        }
    }
}

fn module_path_key(path: &ModulePath) -> String {
    path.segments
        .iter()
        .map(PathSegment::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(feature = "surface")]
pub(crate) fn source_type_spelling<P>(
    ty: &Type<P>,
    module: &Module<P>,
    identity_is_canonical: bool,
) -> Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    source_type_spelling_with_bound(ty, module, identity_is_canonical, &HashSet::new())
}

pub(crate) fn source_type_spelling_with_bound<P>(
    ty: &Type<P>,
    module: &Module<P>,
    identity_is_canonical: bool,
    bound: &HashSet<String>,
) -> Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    source_type_spelling_with_lookup(ty, module, identity_is_canonical, bound)
}

#[cfg(feature = "surface")]
pub(crate) fn compact_source_type_spelling_with_bound<P>(
    ty: &Type<P>,
    module: &Module<P>,
    bound: &HashSet<String>,
) -> Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    source_spelling_from_canonical(
        ty,
        module,
        bound,
        &mut Vec::new(),
        QualifiedAliasPolicy::Shortest,
    )
}

pub(crate) fn source_type_spelling_with_lookup<
    P,
    L: crate::pass::resolve::ContractBinderLookup + ?Sized,
>(
    ty: &Type<P>,
    module: &Module<P>,
    identity_is_canonical: bool,
    bound: &L,
) -> Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    if !identity_is_canonical {
        return ty.clone();
    }
    source_spelling_from_canonical(
        ty,
        module,
        bound,
        &mut Vec::new(),
        QualifiedAliasPolicy::SourceOrder,
    )
}

#[derive(Clone, Copy)]
enum QualifiedAliasPolicy {
    SourceOrder,
    #[cfg(feature = "surface")]
    Shortest,
}

fn source_spelling_from_canonical<P, L: crate::pass::resolve::ContractBinderLookup + ?Sized>(
    ty: &Type<P>,
    module: &Module<P>,
    bound: &L,
    nested: &mut Vec<String>,
    qualified_alias_policy: QualifiedAliasPolicy,
) -> Type<P>
where
    P: crate::pass::resolve::ExportContractPhase + Clone,
{
    #[cfg(not(feature = "surface"))]
    let _ = qualified_alias_policy;
    match ty {
        Type::Path {
            segments,
            args,
            meta,
        } => {
            let args = args
                .iter()
                .map(|arg| {
                    source_spelling_from_canonical(
                        arg,
                        module,
                        bound,
                        nested,
                        qualified_alias_policy,
                    )
                })
                .collect();
            let segments = canonical_nominal_identity(segments)
                .and_then(|(owner, leaf)| {
                    let is_bound = nested.iter().rev().any(|name| name == &leaf)
                        || bound.contains_contract_binder(&leaf);
                    if owner == module_path_key(&module.path) && !is_bound {
                        return Some(vec![PathSegment::new(leaf, meta.span)]);
                    }
                    #[cfg(feature = "surface")]
                    if matches!(qualified_alias_policy, QualifiedAliasPolicy::Shortest) {
                        if !is_bound
                            && module.imports.iter().any(|import_| {
                                matches!(
                                    &import_.kind,
                                    ImportKind::Selective { items, from }
                                        if module_path_key(from) == owner
                                            && items
                                                .iter()
                                                .filter_map(ImportItem::as_name)
                                                .any(|name| name == leaf)
                                )
                            })
                        {
                            return Some(vec![PathSegment::new(leaf, meta.span)]);
                        }
                        let mut shortest: Option<&str> = None;
                        for import_ in &module.imports {
                            let ImportKind::Qualified { path, alias } = &import_.kind else {
                                continue;
                            };
                            if module_path_key(path) != owner
                                || nested.iter().rev().any(|name| name == alias)
                                || bound.contains_contract_binder(alias)
                            {
                                continue;
                            }
                            if shortest.is_none_or(|current| alias.len() < current.len()) {
                                shortest = Some(alias);
                            }
                        }
                        return shortest.map(|alias| {
                            vec![
                                PathSegment::new(alias.to_owned(), meta.span),
                                PathSegment::new(leaf, meta.span),
                            ]
                        });
                    }
                    for import_ in &module.imports {
                        match &import_.kind {
                            ImportKind::Selective { items, from }
                                if module_path_key(from) == owner
                                    && items
                                        .iter()
                                        .filter_map(ImportItem::as_name)
                                        .any(|name| name == leaf)
                                    && !is_bound =>
                            {
                                return Some(vec![PathSegment::new(leaf, meta.span)]);
                            }
                            ImportKind::Qualified { path, alias }
                                if module_path_key(path) == owner
                                    && !nested.iter().rev().any(|name| name == alias)
                                    && !bound.contains_contract_binder(alias) =>
                            {
                                return Some(vec![
                                    PathSegment::new(alias.clone(), meta.span),
                                    PathSegment::new(leaf, meta.span),
                                ]);
                            }
                            _ => {}
                        }
                    }
                    None
                })
                .unwrap_or_else(|| segments.clone());
            Type::synth_path_segments(segments, args, meta.span)
        }
        Type::Product { left, right, meta } => Type::Product {
            left: Box::new(source_spelling_from_canonical(
                left,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            right: Box::new(source_spelling_from_canonical(
                right,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            meta: meta.clone(),
        },
        Type::Sum { left, right, meta } => Type::Sum {
            left: Box::new(source_spelling_from_canonical(
                left,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            right: Box::new(source_spelling_from_canonical(
                right,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            meta: meta.clone(),
        },
        Type::Function {
            param,
            ret,
            meta,
            abi_arity,
            caps,
        } => Type::Function {
            param: Box::new(source_spelling_from_canonical(
                param,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            ret: Box::new(source_spelling_from_canonical(
                ret,
                module,
                bound,
                nested,
                qualified_alias_policy,
            )),
            meta: meta.clone(),
            abi_arity: *abi_arity,
            caps: caps.clone(),
        },
        Type::Forall { param, body, meta } => {
            nested.push(param.name.clone());
            let spelling = Type::Forall {
                param: param.clone(),
                body: Box::new(source_spelling_from_canonical(
                    body,
                    module,
                    bound,
                    nested,
                    qualified_alias_policy,
                )),
                meta: meta.clone(),
            };
            nested.pop();
            spelling
        }
        Type::Unit { .. } | Type::Bottom { .. } | Type::Infer { .. } => ty.clone(),
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
                    source_spelling_from_canonical(
                        arg,
                        module,
                        bound,
                        nested,
                        qualified_alias_policy,
                    )
                })
                .collect(),
            meta: meta.clone(),
            ext: ext.clone(),
        },
        Type::LabelSugar { ext, .. } => match *ext {},
    }
}

fn canonical_nominal_identity(segments: &[PathSegment]) -> Option<(String, String)> {
    let (leaf, owner) = segments.split_last()?;
    if owner.is_empty() {
        return None;
    }
    Some((
        owner
            .iter()
            .map(PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/"),
        leaf.name.clone(),
    ))
}

fn module_path_from_key(key: &str, span: Span) -> ModulePath {
    ModulePath {
        segments: key
            .split('/')
            .map(|segment| PathSegment::new(segment.to_owned(), span))
            .collect(),
        span,
    }
}

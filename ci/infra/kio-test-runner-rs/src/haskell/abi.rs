//! Independent implementation of the public Haskell name ABI.
//!
//! This deliberately does not share compiler source. Fixed vectors keep the
//! two implementations aligned while the runner remains an independent ABI
//! consumer.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ModuleId(Vec<String>);

impl ModuleId {
    pub(crate) fn from_path(path: &str) -> Self {
        if path.is_empty() {
            return Self(Vec::new());
        }
        let segments = path.split('/').collect::<Vec<_>>();
        assert!(
            segments.iter().all(|segment| !segment.is_empty()),
            "Haskell ABI module paths cannot contain empty segments: `{path}`"
        );
        Self(segments.into_iter().map(str::to_owned).collect())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ModuleItemId {
    module: ModuleId,
    item: String,
}

impl ModuleItemId {
    pub(crate) fn new(module: &str, item: &str) -> Self {
        Self {
            module: ModuleId::from_path(module),
            item: item.to_owned(),
        }
    }

    pub(crate) fn leaf(&self) -> &str {
        &self.item
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ItemId {
    Item(ModuleItemId),
    NewtypeMember {
        module: ModuleId,
        newtype: String,
        member: String,
    },
}

impl ItemId {
    pub(crate) fn item(module: &str, item: &str) -> Self {
        Self::Item(ModuleItemId::new(module, item))
    }

    pub(crate) fn newtype_member(module: &str, newtype: &str, member: &str) -> Self {
        Self::NewtypeMember {
            module: ModuleId::from_path(module),
            newtype: newtype.to_owned(),
            member: member.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BoundaryOwner {
    Env(ModuleItemId),
    Exp(ItemId),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum BoundaryRoot {
    Arg(u32),
    Ret,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum BoundaryStep {
    #[cfg(test)]
    App(u32),
    #[cfg(test)]
    Slot(u32),
    #[cfg(test)]
    CallbackArg(u32),
    CallbackRet,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct BoundaryId {
    owner: BoundaryOwner,
    root: BoundaryRoot,
    tail: Vec<BoundaryStep>,
}

impl BoundaryId {
    pub(crate) fn env(module: &str, item: &str, root: BoundaryRoot) -> Self {
        Self {
            owner: BoundaryOwner::Env(ModuleItemId::new(module, item)),
            root,
            tail: Vec::new(),
        }
    }

    pub(crate) fn exp(item: ItemId, root: BoundaryRoot) -> Self {
        Self {
            owner: BoundaryOwner::Exp(item),
            root,
            tail: Vec::new(),
        }
    }

    pub(crate) fn env_item(item: &ModuleItemId, root: BoundaryRoot) -> Self {
        Self {
            owner: BoundaryOwner::Env(item.clone()),
            root,
            tail: Vec::new(),
        }
    }

    pub(crate) fn nested(&self, step: BoundaryStep) -> Self {
        let mut nested = self.clone();
        nested.tail.push(step);
        nested
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum StructuralKey {
    BareNewtype(String),
    #[cfg(test)]
    QualifiedNewtype {
        module: ModuleId,
        newtype: String,
    },
    Positional(u32),
}

impl StructuralKey {
    pub(crate) fn bare(name: &str) -> Self {
        Self::BareNewtype(name.to_owned())
    }

    #[cfg(test)]
    pub(crate) fn qualified(module: &str, newtype: &str) -> Self {
        Self::QualifiedNewtype {
            module: ModuleId::from_path(module),
            newtype: newtype.to_owned(),
        }
    }

    pub(crate) fn positional(index: u32) -> Self {
        Self::Positional(index)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SumPatternId {
    boundary: BoundaryId,
    key: StructuralKey,
    index: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[cfg(test)]
enum PatternId {
    Product(BoundaryId),
    Sum(SumPatternId),
}

enum Name {
    #[cfg(test)]
    ModuleFn(ModuleItemId),
    HostField(ModuleItemId),
    ExportWrapper(ItemId),
    BoundaryAlias(BoundaryId),
    ProductPattern(BoundaryId),
    SumPattern(SumPatternId),
    #[cfg(test)]
    ProductSelector {
        boundary: BoundaryId,
        key: StructuralKey,
        index: u32,
    },
    #[cfg(test)]
    RankNViewType(PatternId),
    #[cfg(test)]
    RankNViewConstructor(PatternId),
    #[cfg(test)]
    RankNNoMatch(SumPatternId),
    #[cfg(test)]
    RankNViewFunction(PatternId),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Namespace {
    Type,
    Constructor,
    Value,
}

struct StructuralNames {
    target_namespace: String,
    type_claims: BTreeSet<String>,
    constructor_claims: BTreeSet<String>,
    value_claims: BTreeSet<String>,
    product_family: String,
    sum_family: String,
}

#[derive(Clone, Copy)]
enum StructuralIdentity {
    ProductFamily,
    SumFamily,
    Abi,
    HostType,
}

impl StructuralNames {
    fn new(target_namespace: &str) -> Self {
        let handle = target_namespace
            .rsplit('.')
            .next()
            .unwrap_or(target_namespace);
        let mut names = Self {
            target_namespace: target_namespace.to_owned(),
            type_claims: BTreeSet::from([
                handle.to_owned(),
                format!("{handle}Host"),
                format!("{handle}HostTypes"),
                "KioOpaque".to_owned(),
                "Type".to_owned(),
            ]),
            constructor_claims: BTreeSet::from([handle.to_owned(), format!("{handle}Host")]),
            value_claims: BTreeSet::from([format!("create{handle}"), "pkgHost".to_owned()]),
            product_family: String::new(),
            sum_family: String::new(),
        };

        let product_preferred = format!("{handle}Product");
        names.product_family = names.resolve(
            Namespace::Type,
            &product_preferred,
            StructuralIdentity::ProductFamily,
        );
        assert!(names.type_claims.insert(names.product_family.clone()));

        let sum_preferred = format!("{handle}Sum");
        names.sum_family = names.resolve(
            Namespace::Type,
            &sum_preferred,
            StructuralIdentity::SumFamily,
        );
        assert!(names.type_claims.insert(names.sum_family.clone()));
        names
    }

    #[cfg(test)]
    fn product_family(&self) -> &str {
        &self.product_family
    }

    #[cfg(test)]
    fn sum_family(&self) -> &str {
        &self.sum_family
    }

    fn host_assoc_name(&self, module: &str, leaf: &str) -> String {
        let item = ModuleItemId::new(module, leaf);
        let preferred = format!("HostType__{}", public_item(&item));
        if !self.type_claims.contains(&preferred) {
            return preferred;
        }
        let escaped = self.escape(Namespace::Type, &preferred, StructuralIdentity::HostType);
        assert!(
            !self.is_claimed_or_generated(Namespace::Type, &escaped),
            "escaped exact host type overlaps an existing name class: `{escaped}`"
        );
        escaped
    }

    fn render(&self, name: &Name) -> String {
        let preferred = name.render();
        self.resolve(name.namespace(), &preferred, StructuralIdentity::Abi)
    }

    fn resolve(
        &self,
        namespace: Namespace,
        preferred: &str,
        identity: StructuralIdentity,
    ) -> String {
        if !self.is_claimed_or_generated(namespace, preferred) {
            return preferred.to_owned();
        }
        let escaped = self.escape(namespace, preferred, identity);
        assert!(
            !self.is_claimed_or_generated(namespace, &escaped),
            "escaped structural Haskell name overlaps an existing name class: `{escaped}`"
        );
        escaped
    }

    fn is_claimed_or_generated(&self, namespace: Namespace, name: &str) -> bool {
        let claimed = match namespace {
            Namespace::Type => self.type_claims.contains(name),
            Namespace::Constructor => self.constructor_claims.contains(name),
            Namespace::Value => self.value_claims.contains(name),
        };
        claimed
            || match namespace {
                Namespace::Type => is_existing_generated_type_name(name),
                Namespace::Constructor => is_existing_generated_constructor_name(name),
                Namespace::Value => false,
            }
    }

    fn escape(
        &self,
        namespace: Namespace,
        preferred: &str,
        identity: StructuralIdentity,
    ) -> String {
        let role = match identity {
            StructuralIdentity::ProductFamily => "ProductFamily",
            StructuralIdentity::SumFamily => "SumFamily",
            StructuralIdentity::Abi => "View",
            StructuralIdentity::HostType => "HostType",
        };
        let prefix = match namespace {
            Namespace::Type => "KioStructT",
            Namespace::Constructor => "KioStructC",
            Namespace::Value => "kioStructV",
        };
        let owner = self.target_namespace.replace('_', "_u").replace('.', "_s");
        format!("{prefix}_N{}_{}_{}_{preferred}", owner.len(), owner, role)
    }
}

#[cfg(test)]
pub(crate) fn module_fn(item: &ModuleItemId) -> String {
    Name::ModuleFn(item.clone()).render()
}

pub(crate) fn host_field(item: &ModuleItemId) -> String {
    Name::HostField(item.clone()).render()
}

pub(crate) fn export_wrapper(item: &ItemId) -> String {
    Name::ExportWrapper(item.clone()).render()
}

pub(crate) fn host_assoc(target_namespace: &str, module: &str, leaf: &str) -> String {
    StructuralNames::new(target_namespace).host_assoc_name(module, leaf)
}

pub(crate) fn boundary_alias(target_namespace: &str, boundary: &BoundaryId) -> String {
    StructuralNames::new(target_namespace).render(&Name::BoundaryAlias(boundary.clone()))
}

pub(crate) fn product_pattern(target_namespace: &str, boundary: &BoundaryId) -> String {
    StructuralNames::new(target_namespace).render(&Name::ProductPattern(boundary.clone()))
}

pub(crate) fn sum_pattern(
    target_namespace: &str,
    boundary: &BoundaryId,
    key: StructuralKey,
    index: u32,
) -> String {
    StructuralNames::new(target_namespace).render(&Name::SumPattern(SumPatternId {
        boundary: boundary.clone(),
        key,
        index,
    }))
}

#[cfg(test)]
pub(crate) fn product_selector(
    target_namespace: &str,
    boundary: &BoundaryId,
    key: StructuralKey,
    index: u32,
) -> String {
    StructuralNames::new(target_namespace).render(&Name::ProductSelector {
        boundary: boundary.clone(),
        key,
        index,
    })
}

impl Name {
    fn namespace(&self) -> Namespace {
        match self {
            Self::BoundaryAlias(_) => Namespace::Type,
            #[cfg(test)]
            Self::RankNViewType(_) => Namespace::Type,
            Self::ProductPattern(_) | Self::SumPattern(_) => Namespace::Constructor,
            #[cfg(test)]
            Self::RankNViewConstructor(_) | Self::RankNNoMatch(_) => Namespace::Constructor,
            Self::HostField(_) | Self::ExportWrapper(_) => Namespace::Value,
            #[cfg(test)]
            Self::ModuleFn(_) | Self::ProductSelector { .. } | Self::RankNViewFunction(_) => {
                Namespace::Value
            }
        }
    }

    fn render(&self) -> String {
        match self {
            Self::BoundaryAlias(boundary) | Self::ProductPattern(boundary) => {
                return public_boundary(boundary);
            }
            Self::SumPattern(pattern) => {
                return format!(
                    "{}_Case{}_{}",
                    public_boundary(&pattern.boundary),
                    pattern.index,
                    public_key(&pattern.key)
                );
            }
            #[cfg(test)]
            Self::ProductSelector {
                boundary,
                key,
                index,
            } => {
                let owner = public_boundary(boundary);
                return format!(
                    "{}{}_Field{index}_{}",
                    owner[..1].to_ascii_lowercase(),
                    &owner[1..],
                    public_key(key)
                );
            }
            Self::HostField(item) => {
                if let Some(primary) = primary_module_item_name("host", item) {
                    return primary;
                }
            }
            Self::ExportWrapper(item) => {
                if let Some(primary) = primary_item_name("export", item) {
                    return primary;
                }
            }
            #[cfg(test)]
            Self::ModuleFn(_)
            | Self::RankNViewType(_)
            | Self::RankNViewConstructor(_)
            | Self::RankNNoMatch(_)
            | Self::RankNViewFunction(_) => {}
        }
        self.render_fallback()
    }

    fn render_fallback(&self) -> String {
        let mut bytes = vec![0x01];
        self.encode(&mut bytes);
        format!(
            "{}_H{}__{}",
            self.prefix(),
            lowercase_hex(&bytes),
            bounded_readable(&self.readable())
        )
    }

    fn prefix(&self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::ModuleFn(_) => "mod",
            Self::HostField(_) => "host",
            Self::ExportWrapper(_) => "exp",
            Self::BoundaryAlias(boundary) => boundary_prefix(boundary, "Env", "Exp"),
            Self::ProductPattern(boundary) => boundary_prefix(boundary, "EnvP", "ExpP"),
            Self::SumPattern(pattern) => boundary_prefix(&pattern.boundary, "EnvS", "ExpS"),
            #[cfg(test)]
            Self::ProductSelector { boundary, .. } => boundary_prefix(boundary, "envSel", "expSel"),
            #[cfg(test)]
            Self::RankNViewType(_) => "ViewT",
            #[cfg(test)]
            Self::RankNViewConstructor(_) => "ViewC",
            #[cfg(test)]
            Self::RankNNoMatch(_) => "NoMatch",
            #[cfg(test)]
            Self::RankNViewFunction(_) => "view",
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            #[cfg(test)]
            Self::ModuleFn(item) => {
                out.extend([0x01, 0x01]);
                encode_item(&ItemId::Item(item.clone()), out);
            }
            Self::HostField(item) => {
                out.extend([0x01, 0x02]);
                encode_item(&ItemId::Item(item.clone()), out);
            }
            Self::ExportWrapper(item) => {
                out.extend([0x01, 0x03]);
                encode_item(item, out);
            }
            Self::BoundaryAlias(boundary) => {
                out.push(0x10);
                encode_boundary(boundary, out);
            }
            Self::ProductPattern(boundary) => {
                out.push(0x20);
                encode_boundary(boundary, out);
            }
            Self::SumPattern(pattern) => {
                out.push(0x21);
                encode_sum_pattern(pattern, out);
            }
            #[cfg(test)]
            Self::ProductSelector {
                boundary,
                key,
                index,
            } => {
                out.push(0x22);
                encode_boundary(boundary, out);
                encode_key(key, out);
                push_u32(*index, out);
            }
            #[cfg(test)]
            Self::RankNViewType(pattern) => {
                out.push(0x30);
                encode_pattern(pattern, out);
            }
            #[cfg(test)]
            Self::RankNViewConstructor(pattern) => {
                out.push(0x31);
                encode_pattern(pattern, out);
            }
            #[cfg(test)]
            Self::RankNNoMatch(pattern) => {
                out.push(0x32);
                encode_sum_pattern(pattern, out);
            }
            #[cfg(test)]
            Self::RankNViewFunction(pattern) => {
                out.push(0x33);
                encode_pattern(pattern, out);
            }
        }
    }

    fn readable(&self) -> String {
        match self {
            #[cfg(test)]
            Self::ModuleFn(item) => module_item_readable(item),
            Self::HostField(item) => module_item_readable(item),
            Self::ExportWrapper(item) => item_readable(item),
            Self::BoundaryAlias(boundary) => boundary_readable(boundary),
            Self::ProductPattern(boundary) => format!("{}_P", boundary_readable(boundary)),
            Self::SumPattern(pattern) => sum_pattern_readable(pattern),
            #[cfg(test)]
            Self::ProductSelector {
                boundary,
                key,
                index,
            } => format!(
                "{}_Sel_{}_{}",
                boundary_readable(boundary),
                key_readable(key),
                index
            ),
            #[cfg(test)]
            Self::RankNViewType(pattern)
            | Self::RankNViewConstructor(pattern)
            | Self::RankNViewFunction(pattern) => pattern_readable(pattern),
            #[cfg(test)]
            Self::RankNNoMatch(pattern) => sum_pattern_readable(pattern),
        }
    }
}

fn boundary_prefix<'a>(boundary: &BoundaryId, env: &'a str, exp: &'a str) -> &'a str {
    match &boundary.owner {
        BoundaryOwner::Env(_) => env,
        BoundaryOwner::Exp(_) => exp,
    }
}

fn encode_module(module: &ModuleId, out: &mut Vec<u8>) {
    push_u32(
        module
            .0
            .len()
            .try_into()
            .expect("module segment count fits u32"),
        out,
    );
    for segment in &module.0 {
        encode_string(segment, out);
    }
}

fn encode_module_item(item: &ModuleItemId, out: &mut Vec<u8>) {
    encode_module(&item.module, out);
    encode_string(&item.item, out);
}

fn encode_item(item: &ItemId, out: &mut Vec<u8>) {
    match item {
        ItemId::Item(item) => {
            out.push(0x01);
            encode_module_item(item, out);
        }
        ItemId::NewtypeMember {
            module,
            newtype,
            member,
        } => {
            out.push(0x02);
            encode_module(module, out);
            encode_string(newtype, out);
            encode_string(member, out);
        }
    }
}

fn encode_boundary(boundary: &BoundaryId, out: &mut Vec<u8>) {
    match &boundary.owner {
        BoundaryOwner::Env(item) => {
            out.push(0x01);
            encode_module_item(item, out);
        }
        BoundaryOwner::Exp(item) => {
            out.push(0x02);
            encode_item(item, out);
        }
    }
    match boundary.root {
        BoundaryRoot::Arg(index) => {
            out.push(0x01);
            push_u32(index, out);
        }
        BoundaryRoot::Ret => out.push(0x02),
    }
    push_u32(
        boundary
            .tail
            .len()
            .try_into()
            .expect("boundary step count fits u32"),
        out,
    );
    for step in &boundary.tail {
        match step {
            #[cfg(test)]
            BoundaryStep::App(index) => {
                out.push(0x01);
                push_u32(*index, out);
            }
            #[cfg(test)]
            BoundaryStep::Slot(index) => {
                out.push(0x02);
                push_u32(*index, out);
            }
            #[cfg(test)]
            BoundaryStep::CallbackArg(index) => {
                out.push(0x03);
                push_u32(*index, out);
            }
            BoundaryStep::CallbackRet => out.push(0x04),
        }
    }
}

fn encode_key(key: &StructuralKey, out: &mut Vec<u8>) {
    match key {
        StructuralKey::BareNewtype(name) => {
            out.push(0x01);
            encode_string(name, out);
        }
        #[cfg(test)]
        StructuralKey::QualifiedNewtype { module, newtype } => {
            out.push(0x02);
            encode_module(module, out);
            encode_string(newtype, out);
        }
        StructuralKey::Positional(index) => {
            out.push(0x03);
            push_u32(*index, out);
        }
    }
}

fn encode_sum_pattern(pattern: &SumPatternId, out: &mut Vec<u8>) {
    encode_boundary(&pattern.boundary, out);
    encode_key(&pattern.key, out);
    push_u32(pattern.index, out);
}

#[cfg(test)]
fn encode_pattern(pattern: &PatternId, out: &mut Vec<u8>) {
    match pattern {
        PatternId::Product(boundary) => {
            out.push(0x01);
            encode_boundary(boundary, out);
        }
        PatternId::Sum(pattern) => {
            out.push(0x02);
            encode_sum_pattern(pattern, out);
        }
    }
}

fn encode_string(value: &str, out: &mut Vec<u8>) {
    push_u32(
        value
            .len()
            .try_into()
            .expect("ABI identity component fits u32"),
        out,
    );
    out.extend_from_slice(value.as_bytes());
}

fn push_u32(value: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push(char::from(HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    rendered
}

fn module_readable(module: &ModuleId) -> String {
    module
        .0
        .iter()
        .map(|part| crate::host_api::host_name_core(part))
        .collect::<Vec<_>>()
        .join("_")
}

fn module_item_readable(item: &ModuleItemId) -> String {
    let module = module_readable(&item.module);
    if module.is_empty() {
        crate::host_api::host_name_core(&item.item)
    } else {
        format!("{module}__{}", crate::host_api::host_name_core(&item.item))
    }
}

fn item_readable(item: &ItemId) -> String {
    match item {
        ItemId::Item(item) => module_item_readable(item),
        ItemId::NewtypeMember {
            module,
            newtype,
            member,
        } => {
            let module = module_readable(module);
            let newtype = crate::host_api::host_name_core(newtype);
            let member = crate::host_api::host_name_core(member);
            if module.is_empty() {
                format!("{newtype}__{member}")
            } else {
                format!("{module}__{newtype}__{member}")
            }
        }
    }
}

fn primary_module_item_name(prefix: &str, item: &ModuleItemId) -> Option<String> {
    let mut components = item.module.0.iter().map(String::as_str).collect::<Vec<_>>();
    components.push(&item.item);
    primary_component_name(prefix, &components)
}

fn primary_item_name(prefix: &str, item: &ItemId) -> Option<String> {
    let mut components = match item {
        ItemId::Item(item) => item.module.0.iter().map(String::as_str).collect::<Vec<_>>(),
        ItemId::NewtypeMember { module, .. } => {
            module.0.iter().map(String::as_str).collect::<Vec<_>>()
        }
    };
    match item {
        ItemId::Item(item) => components.push(&item.item),
        ItemId::NewtypeMember {
            newtype, member, ..
        } => {
            components.push(newtype);
            components.push(member);
        }
    }
    primary_component_name(prefix, &components)
}

fn primary_component_name(prefix: &str, components: &[&str]) -> Option<String> {
    if !primary_components_are_safe(components) {
        return None;
    }
    let mut rendered = prefix.to_owned();
    for component in components {
        rendered.push_str("__");
        rendered.push_str(&crate::host_api::host_name_core(component));
    }
    Some(rendered)
}

fn primary_components_are_safe(components: &[&str]) -> bool {
    !components.is_empty()
        && components.iter().all(|component| {
            !component.is_empty()
                && !component.contains("__")
                && !component.starts_with('_')
                && !component.ends_with('_')
        })
}

fn public_path<'a>(components: impl IntoIterator<Item = &'a str>) -> String {
    components
        .into_iter()
        .map(|part| crate::host_api::host_name_core(part).replace('_', "_u"))
        .collect::<Vec<_>>()
        .join("__")
}

fn public_item(item: &ModuleItemId) -> String {
    public_path(
        item.module
            .0
            .iter()
            .map(String::as_str)
            .chain([item.item.as_str()]),
    )
}

fn public_boundary(boundary: &BoundaryId) -> String {
    let mut name = match &boundary.owner {
        BoundaryOwner::Env(item) => format!("Host_{}", public_item(item)),
        BoundaryOwner::Exp(ItemId::Item(item)) => format!("Export_{}", public_item(item)),
        BoundaryOwner::Exp(ItemId::NewtypeMember {
            module,
            newtype,
            member,
        }) => format!(
            "Member_{}",
            public_path(
                module
                    .0
                    .iter()
                    .map(String::as_str)
                    .chain([newtype.as_str(), member.as_str()])
            )
        ),
    };
    name.push_str(&match boundary.root {
        BoundaryRoot::Arg(index) => format!("_Arg{index}"),
        BoundaryRoot::Ret => "_Result".to_owned(),
    });
    for step in &boundary.tail {
        name.push_str(&match step {
            #[cfg(test)]
            BoundaryStep::App(index) => format!("_App{index}"),
            #[cfg(test)]
            BoundaryStep::Slot(index) => format!("_Slot{index}"),
            #[cfg(test)]
            BoundaryStep::CallbackArg(index) => format!("_CallbackArg{index}"),
            BoundaryStep::CallbackRet => "_CallbackResult".to_owned(),
        });
    }
    name
}

fn public_key(key: &StructuralKey) -> String {
    match key {
        StructuralKey::BareNewtype(name) => format!("Type_{}", public_path([name.as_str()])),
        #[cfg(test)]
        StructuralKey::QualifiedNewtype { module, newtype } => format!(
            "TypeAt_{}",
            public_path(
                module
                    .0
                    .iter()
                    .map(String::as_str)
                    .chain([newtype.as_str()])
            )
        ),
        StructuralKey::Positional(index) => format!("Position{index}"),
    }
}

fn boundary_readable(boundary: &BoundaryId) -> String {
    let mut rendered = match &boundary.owner {
        BoundaryOwner::Env(item) => module_item_readable(item),
        BoundaryOwner::Exp(item) => item_readable(item),
    };
    match boundary.root {
        BoundaryRoot::Arg(index) => rendered.push_str(&format!("_arg{index}")),
        BoundaryRoot::Ret => rendered.push_str("_ret"),
    }
    for step in &boundary.tail {
        match step {
            #[cfg(test)]
            BoundaryStep::App(index) => rendered.push_str(&format!("_app{index}")),
            #[cfg(test)]
            BoundaryStep::Slot(index) => rendered.push_str(&format!("_slot{index}")),
            #[cfg(test)]
            BoundaryStep::CallbackArg(index) => rendered.push_str(&format!("_cbarg{index}")),
            BoundaryStep::CallbackRet => rendered.push_str("_cbret"),
        }
    }
    rendered
}

fn key_readable(key: &StructuralKey) -> String {
    match key {
        StructuralKey::BareNewtype(name) => crate::host_api::host_name_core(name),
        #[cfg(test)]
        StructuralKey::QualifiedNewtype { module, newtype } => {
            let module = module_readable(module);
            let newtype = crate::host_api::host_name_core(newtype);
            if module.is_empty() {
                newtype.clone()
            } else {
                format!("{module}_{newtype}")
            }
        }
        StructuralKey::Positional(index) => format!("pos{index}"),
    }
}

fn sum_pattern_readable(pattern: &SumPatternId) -> String {
    format!(
        "{}_S_{}_{}",
        boundary_readable(&pattern.boundary),
        key_readable(&pattern.key),
        pattern.index
    )
}

#[cfg(test)]
fn pattern_readable(pattern: &PatternId) -> String {
    match pattern {
        PatternId::Product(boundary) => format!("{}_P", boundary_readable(boundary)),
        PatternId::Sum(pattern) => sum_pattern_readable(pattern),
    }
}

fn bounded_readable(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len().min(64));
    for ch in value.chars() {
        let safe = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '\'' {
            ch
        } else {
            '_'
        };
        if rendered.len() == 64 {
            break;
        }
        rendered.push(safe);
    }
    if rendered.is_empty() {
        rendered.push_str("name");
    }
    rendered
}

fn is_existing_generated_type_name(name: &str) -> bool {
    is_existing_generated_constructor_name(name) || name.starts_with("HostType__")
}

fn is_existing_generated_constructor_name(name: &str) -> bool {
    name.starts_with("KioType_") || name.starts_with("KioExists_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_vectors_match_the_normative_name_abi() {
        assert_eq!(
            module_fn(&ModuleItemId::new("a/b", "c")),
            "mod_H0101010100000002000000016100000001620000000163__a_b__c"
        );
        assert_eq!(
            module_fn(&ModuleItemId::new("a", "b_c")),
            "mod_H0101010100000001000000016100000003625f63__a__bC"
        );
        assert_eq!(host_field(&ModuleItemId::new("a/b", "f")), "host__a__b__f");
        assert_eq!(host_field(&ModuleItemId::new("a_b", "f")), "host__aB__f");
        assert_eq!(host_field(&ModuleItemId::new("", "f")), "host__f");
        assert_eq!(
            host_field(&ModuleItemId::new("a_/b", "g")),
            "host_H010102010000000200000002615f00000001620000000167__a__b__g"
        );
        assert_eq!(
            host_field(&ModuleItemId::new("a/_b", "g")),
            "host_H01010201000000020000000161000000025f620000000167__a__b__g"
        );
        assert_eq!(
            export_wrapper(&ItemId::item("a", "foo__bar")),
            "exp_H0101030100000001000000016100000008666f6f5f5f626172__a__fooBar"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("a", "Foo", "bar")),
            "export__a__Foo__bar"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("api", "_Box", "mk")),
            "exp_H010103020000000100000003617069000000045f426f78000000026d6b__api___Box__mk"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("api", "A_b", "c")),
            "export__api__AB__c"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("api", "A", "b_c")),
            "export__api__A__bC"
        );
        assert_eq!(export_wrapper(&ItemId::item("a/b", "f")), "export__a__b__f");
        assert_eq!(
            export_wrapper(&ItemId::item("a_/b", "c")),
            "exp_H010103010000000200000002615f00000001620000000163__a__b__c"
        );
        assert_eq!(
            export_wrapper(&ItemId::item("a/_b", "c")),
            "exp_H01010301000000020000000161000000025f620000000163__a__b__c"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("api", "A_", "b")),
            "exp_H01010302000000010000000361706900000002415f0000000162__api__A___b"
        );
        assert_eq!(
            export_wrapper(&ItemId::newtype_member("api", "A", "_b")),
            "exp_H0101030200000001000000036170690000000141000000025f62__api__A___b"
        );
        assert_eq!(
            host_assoc("Greeter", "a/b", "Thing"),
            "HostType__a__b__Thing"
        );
        assert_eq!(
            host_assoc("Greeter", "a_/b", "Edge"),
            "HostType__a_u__b__Edge"
        );
        assert_eq!(
            host_assoc("Greeter", "a/_b", "Edge"),
            "HostType__a___ub__Edge"
        );
        assert_eq!(
            host_assoc("HostType__a__b__Thing", "a/b", "Thing"),
            "KioStructT_N27_HostType_u_ua_u_ub_u_uThing_HostType_HostType__a__b__Thing"
        );
        assert_eq!(
            host_assoc("HostType__aB__Thing", "a_b", "Thing"),
            "KioStructT_N23_HostType_u_uaB_u_uThing_HostType_HostType__aB__Thing"
        );

        let nested = BoundaryId::exp(ItemId::item("a", "f"), BoundaryRoot::Arg(0))
            .nested(BoundaryStep::Slot(0));
        assert_eq!(
            product_pattern("Greeter", &nested),
            "Export_a__f_Arg0_Slot0"
        );
        let outer = BoundaryId::exp(ItemId::item("a", "f"), BoundaryRoot::Arg(0));
        assert_eq!(
            sum_pattern("Greeter", &outer, StructuralKey::qualified("slot0", "P"), 0,),
            "Export_a__f_Arg0_Case0_TypeAt_slot0__P"
        );
        assert_eq!(
            product_selector("Greeter", &outer, StructuralKey::qualified("slot0", "Y"), 2,),
            "export_a__f_Arg0_Field2_TypeAt_slot0__Y"
        );

        let env = BoundaryId::env("m", "h", BoundaryRoot::Ret)
            .nested(BoundaryStep::App(2))
            .nested(BoundaryStep::CallbackArg(3))
            .nested(BoundaryStep::CallbackRet);
        assert_eq!(
            boundary_alias("Greeter", &env),
            "Host_m__h_Result_App2_CallbackArg3_CallbackResult"
        );
        assert_eq!(
            sum_pattern("Greeter", &env, StructuralKey::positional(7), 7),
            "Host_m__h_Result_App2_CallbackArg3_CallbackResult_Case7_Position7"
        );
        assert_eq!(
            Name::RankNViewType(PatternId::Product(env)).render(),
            "ViewT_H0130010100000001000000016d000000016802000000030100000002030000000304__m__h_ret_app2_cbarg3_cbret_P"
        );

        let sum = SumPatternId {
            boundary: outer,
            key: StructuralKey::bare("X"),
            index: 0,
        };
        let pattern = PatternId::Sum(sum.clone());
        assert_eq!(
            Name::RankNViewType(pattern.clone()).render(),
            "ViewT_H0130020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            Name::RankNViewConstructor(pattern.clone()).render(),
            "ViewC_H0131020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            Name::RankNNoMatch(sum).render(),
            "NoMatch_H01320201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            Name::RankNViewFunction(pattern).render(),
            "view_H0133020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
    }

    #[test]
    fn structural_family_fixed_vector_escapes_a_nominal_name_class() {
        let names = StructuralNames::new("KioType_api__");
        assert_eq!(
            names.product_family(),
            "KioStructT_N16_KioType_uapi_u_u_ProductFamily_KioType_api__Product"
        );
        assert_eq!(
            names.sum_family(),
            "KioStructT_N16_KioType_uapi_u_u_SumFamily_KioType_api__Sum"
        );
    }

    #[test]
    fn boundary_aliases_and_patterns_escape_same_namespace_facade_claims() {
        let boundary = BoundaryId::exp(ItemId::item("api", "pair"), BoundaryRoot::Ret);
        let alias = Name::BoundaryAlias(boundary.clone());
        let preferred_alias = alias.render();
        let escaped_alias = StructuralNames::new(&preferred_alias).render(&alias);
        assert_ne!(escaped_alias, preferred_alias);
        assert!(escaped_alias.starts_with("KioStructT_N"), "{escaped_alias}");

        let pattern = Name::ProductPattern(boundary);
        let preferred_pattern = pattern.render();
        let escaped_pattern = StructuralNames::new(&preferred_pattern).render(&pattern);
        assert_ne!(escaped_pattern, preferred_pattern);
        assert!(
            escaped_pattern.starts_with("KioStructC_N"),
            "{escaped_pattern}"
        );
    }
}

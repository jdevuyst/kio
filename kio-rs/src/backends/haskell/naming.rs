//! Injective public names for the Haskell package ABI.
//!
//! Host-authored names use readable `__`-separated source components when
//! every component is boundary-safe: it contains no `__` and neither starts
//! nor ends with `_`. Host fields and exports otherwise use the versioned
//! typed codec and its bounded cosmetic suffix. Exact host-type families keep
//! their separate raw `module\0leaf` hexadecimal fallback and unbounded
//! readable suffix.

use std::collections::{BTreeMap, BTreeSet};

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
    pub(crate) module: ModuleId,
    pub(crate) item: String,
}

impl ModuleItemId {
    pub(crate) fn new(module: &str, item: &str) -> Self {
        Self {
            module: ModuleId::from_path(module),
            item: item.to_owned(),
        }
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
pub(crate) enum BoundaryOwner {
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
    App(u32),
    Slot(u32),
    CallbackArg(u32),
    CallbackRet,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct BoundaryId {
    pub(crate) owner: BoundaryOwner,
    pub(crate) root: BoundaryRoot,
    pub(crate) tail: Vec<BoundaryStep>,
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

    pub(crate) fn nested(&self, step: BoundaryStep) -> Self {
        let mut nested = self.clone();
        nested.tail.push(step);
        nested
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum StructuralKey {
    BareNewtype(String),
    QualifiedNewtype { module: ModuleId, newtype: String },
    Positional(u32),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SumPatternId {
    pub(crate) boundary: BoundaryId,
    pub(crate) key: StructuralKey,
    pub(crate) index: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PatternId {
    Product(BoundaryId),
    Sum(SumPatternId),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum HaskellName {
    ModuleFn(ModuleItemId),
    HostField(ModuleItemId),
    ExportWrapper(ItemId),
    BoundaryAlias(BoundaryId),
    ProductPattern(BoundaryId),
    SumPattern(SumPatternId),
    ProductSelector {
        boundary: BoundaryId,
        key: StructuralKey,
        index: u32,
    },
    RankNViewType(PatternId),
    RankNViewConstructor(PatternId),
    RankNNoMatch(SumPatternId),
    RankNViewFunction(PatternId),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum HaskellNamespace {
    Type,
    Constructor,
    Value,
}

/// Import aliases for the standard modules referenced by generated Haskell.
///
/// Each alias is a strict descendant of the exact facade module. It therefore
/// cannot equal that module even when the configured namespace is itself a
/// standard module such as `Data.Text` or `Data.Int`. The distinct final
/// segments keep the compiler-owned import bindings disjoint from one another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StandardNames {
    pub(crate) data_kind: String,
    pub(crate) data_text: String,
    pub(crate) data_string: String,
    pub(crate) data_void: String,
    pub(crate) data_io_ref: String,
    pub(crate) ghc_type_lits: String,
}

impl StandardNames {
    pub(crate) fn new(target_namespace: &str) -> Self {
        let alias = |role: &str| format!("{target_namespace}.KioStandard{role}");
        Self {
            data_kind: alias("DataKind"),
            data_text: alias("DataText"),
            data_string: alias("DataString"),
            data_void: alias("DataVoid"),
            data_io_ref: alias("DataIORef"),
            ghc_type_lits: alias("GHCTypeLits"),
        }
    }

    pub(crate) fn imports(&self) -> [(&'static str, &'static str, &str); 6] {
        [
            ("base", "Data.Kind", self.data_kind.as_str()),
            ("text", "Data.Text", self.data_text.as_str()),
            ("base", "Data.String", self.data_string.as_str()),
            ("base", "Data.Void", self.data_void.as_str()),
            ("base", "Data.IORef", self.data_io_ref.as_str()),
            ("base", "GHC.TypeLits", self.ghc_type_lits.as_str()),
        ]
    }
}

/// Immutable naming context for the structural part of one emitted package.
///
/// Preferred public spellings remain unchanged unless they overlap a fixed
/// facade claim or an existing generated nominal type-name class. An escaped
/// spelling depends only on the full target namespace and the typed semantic
/// identity, never on which declarations happened to be visited first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StructuralNames {
    target_namespace: String,
    type_claims: BTreeSet<String>,
    constructor_claims: BTreeSet<String>,
    value_claims: BTreeSet<String>,
    product_family: String,
    sum_family: String,
}

/// Private runtime identities for one emitted package.
///
/// Each spelling derives from the complete target namespace plus one framed
/// implementation role. The `KioRuntimeT`, `KioRuntimeC`, and `kioRuntimeV`
/// classes are disjoint from every source-derived ABI, structural, and nominal
/// renderer. A fixed facade name is derived from at most the target namespace
/// plus a short suffix, while this framed hexadecimal rendering is strictly
/// longer than the namespace, so it cannot equal its own facade name either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeNames {
    target_namespace: String,
    pub(crate) opaque: String,
    pub(crate) unit: String,
    pub(crate) int: String,
    pub(crate) double: String,
    pub(crate) text: String,
    pub(crate) bool_: String,
    pub(crate) product: String,
    pub(crate) sum: String,
    pub(crate) function: String,
    pub(crate) foreign: String,
    pub(crate) force: String,
    pub(crate) call_function: String,
    pub(crate) project: String,
    pub(crate) project_head: String,
    pub(crate) project_tail: String,
    pub(crate) match_tag: String,
    pub(crate) as_bool: String,
    pub(crate) as_int: String,
    pub(crate) as_double: String,
    pub(crate) as_text: String,
    pub(crate) foreign_ref: String,
    pub(crate) require_host_type: String,
    pub(crate) required_host_types: String,
}

impl RuntimeNames {
    pub(crate) fn new(target_namespace: &str) -> Self {
        let private = |namespace, role, readable| {
            runtime_private_name(target_namespace, namespace, role, readable)
        };

        Self {
            target_namespace: target_namespace.to_owned(),
            opaque: private(HaskellNamespace::Type, "opaque", "Opaque"),
            unit: private(HaskellNamespace::Constructor, "unit", "Unit"),
            int: private(HaskellNamespace::Constructor, "int", "Int"),
            double: private(HaskellNamespace::Constructor, "double", "Double"),
            text: private(HaskellNamespace::Constructor, "text", "Text"),
            bool_: private(HaskellNamespace::Constructor, "bool", "Bool"),
            product: private(HaskellNamespace::Constructor, "product", "Product"),
            sum: private(HaskellNamespace::Constructor, "sum", "Sum"),
            function: private(HaskellNamespace::Constructor, "function", "Function"),
            foreign: private(HaskellNamespace::Constructor, "foreign", "Foreign"),
            force: private(HaskellNamespace::Value, "force", "force"),
            call_function: private(HaskellNamespace::Value, "call-function", "callFunction"),
            project: private(HaskellNamespace::Value, "project", "project"),
            project_head: private(HaskellNamespace::Value, "project-head", "projectHead"),
            project_tail: private(HaskellNamespace::Value, "project-tail", "projectTail"),
            match_tag: private(HaskellNamespace::Value, "match-tag", "matchTag"),
            as_bool: private(HaskellNamespace::Value, "as-bool", "asBool"),
            as_int: private(HaskellNamespace::Value, "as-int", "asInt"),
            as_double: private(HaskellNamespace::Value, "as-double", "asDouble"),
            as_text: private(HaskellNamespace::Value, "as-text", "asText"),
            foreign_ref: private(HaskellNamespace::Value, "foreign-ref", "foreignRef"),
            require_host_type: private(
                HaskellNamespace::Type,
                "require-host-type",
                "RequireHostType",
            ),
            required_host_types: private(
                HaskellNamespace::Type,
                "required-host-types",
                "RequiredHostTypes",
            ),
        }
    }

    pub(crate) fn missing_host_type(&self, assoc_name: &str) -> String {
        self.dynamic_type("missing-host-type", assoc_name, "MissingHostType")
    }

    pub(crate) fn deprecated_host_type(&self, assoc_name: &str) -> String {
        self.dynamic_type("deprecated-host-type", assoc_name, "DeprecatedHostType")
    }

    pub(crate) fn deprecated_host_constructor(&self, assoc_name: &str) -> String {
        runtime_private_name(
            &self.target_namespace,
            HaskellNamespace::Constructor,
            &format!("deprecated-host-constructor\0{assoc_name}"),
            "DeprecatedHostType",
        )
    }

    fn dynamic_type(&self, role: &str, assoc_name: &str, readable: &str) -> String {
        runtime_private_name(
            &self.target_namespace,
            HaskellNamespace::Type,
            &format!("{role}\0{assoc_name}"),
            readable,
        )
    }
}

fn runtime_private_name(
    target_namespace: &str,
    namespace: HaskellNamespace,
    role: &str,
    readable: &str,
) -> String {
    let mut identity = vec![0x01];
    encode_string(target_namespace, &mut identity);
    encode_string(role, &mut identity);
    let prefix = match namespace {
        HaskellNamespace::Type => "KioRuntimeT",
        HaskellNamespace::Constructor => "KioRuntimeC",
        HaskellNamespace::Value => "kioRuntimeV",
    };
    format!(
        "{prefix}_H{}__{}",
        lowercase_hex(&identity),
        bounded_readable(readable)
    )
}

#[derive(Clone, Copy)]
enum StructuralIdentity<'a> {
    ProductFamily,
    SumFamily,
    Abi(&'a HaskellName),
    HostType(&'a ModuleItemId),
}

impl StructuralNames {
    pub(crate) fn new(target_namespace: &str) -> Self {
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
                // Public ABI tombstone: preserve structural-name escaping
                // even though the private carrier no longer uses this spelling.
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
            HaskellNamespace::Type,
            &product_preferred,
            StructuralIdentity::ProductFamily,
        );
        assert!(names.type_claims.insert(names.product_family.clone()));

        let sum_preferred = format!("{handle}Sum");
        names.sum_family = names.resolve(
            HaskellNamespace::Type,
            &sum_preferred,
            StructuralIdentity::SumFamily,
        );
        assert!(names.type_claims.insert(names.sum_family.clone()));
        names
    }

    pub(crate) fn product_family(&self) -> &str {
        &self.product_family
    }

    pub(crate) fn sum_family(&self) -> &str {
        &self.sum_family
    }

    pub(crate) fn host_assoc_name(&self, module: &str, leaf: &str) -> String {
        let item = ModuleItemId::new(module, leaf);
        if let Some(preferred) = primary_module_item_name("HostType", &item)
            && !self.type_claims.contains(&preferred)
        {
            return preferred;
        }
        let fallback = fallback_host_assoc_name(module, leaf);
        if !self.type_claims.contains(&fallback) {
            return fallback;
        }
        let escaped = self.escape(
            HaskellNamespace::Type,
            &fallback,
            StructuralIdentity::HostType(&item),
        );
        assert!(
            !self.is_claimed_or_generated(HaskellNamespace::Type, &escaped),
            "escaped exact host type overlaps an existing name class: `{escaped}`"
        );
        escaped
    }

    pub(crate) fn render(&self, name: &HaskellName) -> String {
        assert!(
            name.is_structural(),
            "the structural naming context only renders structural ABI identities"
        );
        let preferred = name.render();
        self.resolve(name.namespace(), &preferred, StructuralIdentity::Abi(name))
    }

    fn resolve(
        &self,
        namespace: HaskellNamespace,
        preferred: &str,
        identity: StructuralIdentity<'_>,
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

    fn is_claimed_or_generated(&self, namespace: HaskellNamespace, name: &str) -> bool {
        let claimed = match namespace {
            HaskellNamespace::Type => self.type_claims.contains(name),
            HaskellNamespace::Constructor => self.constructor_claims.contains(name),
            HaskellNamespace::Value => self.value_claims.contains(name),
        };
        claimed
            || match namespace {
                HaskellNamespace::Type => is_existing_generated_type_name(name),
                HaskellNamespace::Constructor => is_existing_generated_constructor_name(name),
                HaskellNamespace::Value => false,
            }
    }

    fn escape(
        &self,
        namespace: HaskellNamespace,
        preferred: &str,
        identity: StructuralIdentity<'_>,
    ) -> String {
        let mut bytes = vec![0x01];
        encode_string(&self.target_namespace, &mut bytes);
        match identity {
            StructuralIdentity::ProductFamily => bytes.push(0x01),
            StructuralIdentity::SumFamily => bytes.push(0x02),
            StructuralIdentity::Abi(name) => {
                bytes.push(0x03);
                name.encode(&mut bytes);
            }
            StructuralIdentity::HostType(item) => {
                bytes.push(0x04);
                encode_module_item(item, &mut bytes);
            }
        }
        let prefix = match namespace {
            HaskellNamespace::Type => "KioStructT",
            HaskellNamespace::Constructor => "KioStructC",
            HaskellNamespace::Value => "kioStructV",
        };
        format!(
            "{prefix}_H{}__{}",
            lowercase_hex(&bytes),
            bounded_readable(preferred)
        )
    }
}

impl HaskellName {
    fn is_structural(&self) -> bool {
        !matches!(
            self,
            Self::ModuleFn(_) | Self::HostField(_) | Self::ExportWrapper(_)
        )
    }

    pub(crate) fn namespace(&self) -> HaskellNamespace {
        match self {
            Self::BoundaryAlias(_) | Self::RankNViewType(_) => HaskellNamespace::Type,
            Self::ProductPattern(_)
            | Self::SumPattern(_)
            | Self::RankNViewConstructor(_)
            | Self::RankNNoMatch(_) => HaskellNamespace::Constructor,
            Self::ModuleFn(_)
            | Self::HostField(_)
            | Self::ExportWrapper(_)
            | Self::ProductSelector { .. }
            | Self::RankNViewFunction(_) => HaskellNamespace::Value,
        }
    }

    pub(crate) fn render(&self) -> String {
        match self {
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
            _ => {}
        }
        self.render_fallback()
    }

    fn render_fallback(&self) -> String {
        let mut bytes = vec![0x01];
        self.encode(&mut bytes);
        let prefix = self.prefix();
        let exact = lowercase_hex(&bytes);
        let readable = bounded_readable(&self.readable());
        format!("{prefix}_H{exact}__{readable}")
    }

    fn prefix(&self) -> &'static str {
        match self {
            Self::ModuleFn(_) => "mod",
            Self::HostField(_) => "host",
            Self::ExportWrapper(_) => "exp",
            Self::BoundaryAlias(boundary) => boundary_owner_prefix(boundary, "Env", "Exp"),
            Self::ProductPattern(boundary) => boundary_owner_prefix(boundary, "EnvP", "ExpP"),
            Self::SumPattern(pattern) => boundary_owner_prefix(&pattern.boundary, "EnvS", "ExpS"),
            Self::ProductSelector { boundary, .. } => {
                boundary_owner_prefix(boundary, "envSel", "expSel")
            }
            Self::RankNViewType(_) => "ViewT",
            Self::RankNViewConstructor(_) => "ViewC",
            Self::RankNNoMatch(_) => "NoMatch",
            Self::RankNViewFunction(_) => "view",
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
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
            Self::RankNViewType(pattern) => {
                out.push(0x30);
                encode_pattern(pattern, out);
            }
            Self::RankNViewConstructor(pattern) => {
                out.push(0x31);
                encode_pattern(pattern, out);
            }
            Self::RankNNoMatch(pattern) => {
                out.push(0x32);
                encode_sum_pattern(pattern, out);
            }
            Self::RankNViewFunction(pattern) => {
                out.push(0x33);
                encode_pattern(pattern, out);
            }
        }
    }

    fn readable(&self) -> String {
        match self {
            Self::ModuleFn(item) | Self::HostField(item) => module_item_readable(item),
            Self::ExportWrapper(item) => item_readable(item),
            Self::BoundaryAlias(boundary) => boundary_readable(boundary),
            Self::ProductPattern(boundary) => format!("{}_P", boundary_readable(boundary)),
            Self::SumPattern(pattern) => sum_pattern_readable(pattern),
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
            Self::RankNViewType(pattern)
            | Self::RankNViewConstructor(pattern)
            | Self::RankNViewFunction(pattern) => pattern_readable(pattern),
            Self::RankNNoMatch(pattern) => sum_pattern_readable(pattern),
        }
    }
}

fn boundary_owner_prefix<'a>(boundary: &BoundaryId, env: &'a str, exp: &'a str) -> &'a str {
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
            BoundaryStep::App(index) => {
                out.push(0x01);
                push_u32(*index, out);
            }
            BoundaryStep::Slot(index) => {
                out.push(0x02);
                push_u32(*index, out);
            }
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
            .expect("Haskell ABI identity component fits u32"),
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
        .map(|part| crate::backends::public_names::host_name_core(part))
        .collect::<Vec<_>>()
        .join("_")
}

fn module_item_readable(item: &ModuleItemId) -> String {
    let module = module_readable(&item.module);
    if module.is_empty() {
        crate::backends::public_names::host_name_core(&item.item)
    } else {
        format!(
            "{module}__{}",
            crate::backends::public_names::host_name_core(&item.item)
        )
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
            let newtype = crate::backends::public_names::host_name_core(newtype);
            let member = crate::backends::public_names::host_name_core(member);
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
        rendered.push_str(&crate::backends::public_names::host_name_core(component));
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

fn fallback_host_assoc_name(module: &str, leaf: &str) -> String {
    let exact = format!("{module}\0{leaf}");
    let module = module
        .split('/')
        .map(crate::backends::public_names::host_name_core)
        .collect::<Vec<_>>()
        .join("/");
    let leaf = crate::backends::public_names::host_name_core(leaf);
    let readable_module = module
        .replace('/', "__")
        .chars()
        .map(sanitize_generated_name_char)
        .collect::<String>();
    let readable_leaf = leaf
        .chars()
        .map(sanitize_generated_name_char)
        .collect::<String>();
    format!(
        "HostType_H{}__{readable_module}__{readable_leaf}",
        lowercase_hex(exact.as_bytes())
    )
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
            BoundaryStep::App(index) => rendered.push_str(&format!("_app{index}")),
            BoundaryStep::Slot(index) => rendered.push_str(&format!("_slot{index}")),
            BoundaryStep::CallbackArg(index) => rendered.push_str(&format!("_cbarg{index}")),
            BoundaryStep::CallbackRet => rendered.push_str("_cbret"),
        }
    }
    rendered
}

fn key_readable(key: &StructuralKey) -> String {
    match key {
        StructuralKey::BareNewtype(name) => crate::backends::public_names::host_name_core(name),
        StructuralKey::QualifiedNewtype { module, newtype } => {
            let module = module_readable(module);
            let newtype = crate::backends::public_names::host_name_core(newtype);
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
    is_exact_nominal_name(name, "KioCarrier")
        || is_exact_nominal_name(name, "KioExistential")
        || is_primary_host_type_name(name)
        || is_exact_host_type_name(name)
}

fn is_primary_host_type_name(name: &str) -> bool {
    let Some(components) = name.strip_prefix("HostType__") else {
        return false;
    };
    let components = components.split("__").collect::<Vec<_>>();
    primary_components_are_safe(&components)
}

fn is_existing_generated_constructor_name(name: &str) -> bool {
    is_exact_nominal_name(name, "KioCarrier") || is_exact_nominal_name(name, "KioExistential")
}

fn is_exact_nominal_name(name: &str, prefix: &str) -> bool {
    let Some((module, leaf)) = decoded_generated_identity(name, &format!("{prefix}_H")) else {
        return false;
    };
    let readable_module = module
        .split('/')
        .map(crate::backends::public_names::host_name_core)
        .collect::<Vec<_>>()
        .join("/");
    let readable_leaf = crate::backends::public_names::host_name_core(&leaf);
    let readable = format!("{readable_module}_{readable_leaf}")
        .chars()
        .map(sanitize_generated_name_char)
        .collect::<String>();
    let exact = format!("{module}\0{leaf}");
    name == format!("{prefix}_H{}__{readable}", lowercase_hex(exact.as_bytes()))
}

fn is_exact_host_type_name(name: &str) -> bool {
    let Some((module, leaf)) = decoded_generated_identity(name, "HostType_H") else {
        return false;
    };
    let readable_module = module
        .split('/')
        .map(crate::backends::public_names::host_name_core)
        .collect::<Vec<_>>()
        .join("/")
        .replace('/', "__")
        .chars()
        .map(sanitize_generated_name_char)
        .collect::<String>();
    let readable_leaf = crate::backends::public_names::host_name_core(&leaf)
        .chars()
        .map(sanitize_generated_name_char)
        .collect::<String>();
    let exact = format!("{module}\0{leaf}");
    name == format!(
        "HostType_H{}__{readable_module}__{readable_leaf}",
        lowercase_hex(exact.as_bytes())
    )
}

fn decoded_generated_identity(name: &str, prefix: &str) -> Option<(String, String)> {
    let encoded_and_readable = name.strip_prefix(prefix)?;
    let (encoded, _) = encoded_and_readable.split_once("__")?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }
    let bytes = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(digits, 16).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let identity = String::from_utf8(bytes).ok()?;
    let (module, leaf) = identity.split_once('\0')?;
    if leaf.contains('\0') {
        return None;
    }
    Some((module.to_owned(), leaf.to_owned()))
}

fn sanitize_generated_name_char(ch: char) -> char {
    if ch.is_ascii_alphanumeric() || ch == '_' {
        ch
    } else {
        '_'
    }
}

#[derive(Default)]
pub(crate) struct NameClaims {
    by_semantic: BTreeMap<HaskellName, ClaimedName>,
    by_rendered: BTreeMap<(HaskellNamespace, String), HaskellName>,
}

struct ClaimedName {
    rendered: String,
    declaration: String,
}

impl NameClaims {
    /// Claim a declaration name. Returns `true` for a new declaration and
    /// `false` only for an identical semantic identity and payload.
    pub(crate) fn claim(&mut self, name: HaskellName, rendered: &str, declaration: &str) -> bool {
        if let Some(existing) = self.by_semantic.get(&name) {
            assert_eq!(
                existing.rendered, rendered,
                "one Haskell ABI identity rendered to two names: {name:?}"
            );
            assert_eq!(
                existing.declaration, declaration,
                "one Haskell ABI identity claimed incompatible declarations: {name:?}"
            );
            return false;
        }
        let key = (name.namespace(), rendered.to_owned());
        if let Some(existing) = self.by_rendered.get(&key) {
            panic!(
                "distinct Haskell ABI identities rendered to `{rendered}` in {:?}: {existing:?} and {name:?}",
                name.namespace()
            );
        }
        self.by_rendered.insert(key, name.clone());
        self.by_semantic.insert(
            name,
            ClaimedName {
                rendered: rendered.to_owned(),
                declaration: declaration.to_owned(),
            },
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_import_aliases_are_distinct_strict_namespace_descendants() {
        let names = StandardNames::new("Data.Text");
        let aliases = names
            .imports()
            .into_iter()
            .map(|(_, _, alias)| alias)
            .collect::<BTreeSet<_>>();

        assert_eq!(aliases.len(), 6);
        assert!(
            aliases
                .iter()
                .all(|alias| alias.starts_with("Data.Text.") && *alias != "Data.Text")
        );
        assert_eq!(names.data_text, "Data.Text.KioStandardDataText");
    }

    #[test]
    fn private_runtime_names_are_injective_and_class_disjoint() {
        let names = RuntimeNames::new("Pkg");
        let all = [
            &names.opaque,
            &names.unit,
            &names.int,
            &names.double,
            &names.text,
            &names.bool_,
            &names.product,
            &names.sum,
            &names.function,
            &names.foreign,
            &names.force,
            &names.call_function,
            &names.project,
            &names.project_head,
            &names.project_tail,
            &names.match_tag,
            &names.as_bool,
            &names.as_int,
            &names.as_double,
            &names.as_text,
            &names.foreign_ref,
            &names.require_host_type,
            &names.required_host_types,
        ];
        assert_eq!(all.into_iter().collect::<BTreeSet<_>>().len(), all.len());
        assert!(names.opaque.starts_with("KioRuntimeT_H"));
        assert!(names.unit.starts_with("KioRuntimeC_H"));
        assert!(names.force.starts_with("kioRuntimeV_H"));
    }

    #[test]
    fn private_runtime_names_are_namespace_and_role_derived() {
        let first = RuntimeNames::new("Foo.Runtime");
        let repeated = RuntimeNames::new("Foo.Runtime");
        let ancestor = RuntimeNames::new("Foo");

        assert_eq!(first, repeated);
        assert_ne!(first.opaque, ancestor.opaque);
        assert_ne!(first.unit, ancestor.unit);
        assert_ne!(first.force, ancestor.force);
        assert_ne!(
            first.missing_host_type("HostType__api__A"),
            first.missing_host_type("HostType__api__B")
        );
        assert_ne!(
            first.missing_host_type("HostType__api__A"),
            first.deprecated_host_type("HostType__api__A")
        );
    }

    #[test]
    fn public_primary_names_are_readable_and_unsafe_routes_fall_back() {
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a/b", "f")).render(),
            "host__a__b__f"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "Pair", "make_pair")).render(),
            "export__api__Pair__makePair"
        );
        let left = HaskellName::HostField(ModuleItemId::new("a_/b", "g")).render();
        let right = HaskellName::HostField(ModuleItemId::new("a/_b", "g")).render();
        assert!(left.starts_with("host_H"), "{left}");
        assert!(right.starts_with("host_H"), "{right}");
        assert_ne!(left, right);
    }

    #[test]
    fn public_item_names_are_readable_and_fallbacks_remain_framed() {
        assert_eq!(
            HaskellName::ModuleFn(ModuleItemId::new("a/b", "c")).render(),
            "mod_H0101010100000002000000016100000001620000000163__a_b__c"
        );
        assert_eq!(
            HaskellName::ModuleFn(ModuleItemId::new("a", "b_c")).render(),
            "mod_H0101010100000001000000016100000003625f63__a__bC"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a/b", "f")).render(),
            "host__a__b__f"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a_b", "f")).render(),
            "host__aB__f"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("", "f")).render(),
            "host__f"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a", "foo__bar")).render(),
            "host_H0101020100000001000000016100000008666f6f5f5f626172__a__fooBar"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a_/b", "g")).render(),
            "host_H010102010000000200000002615f00000001620000000167__a__b__g"
        );
        assert_eq!(
            HaskellName::HostField(ModuleItemId::new("a/_b", "g")).render(),
            "host_H01010201000000020000000161000000025f620000000167__a__b__g"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::item("a/b", "f")).render(),
            "export__a__b__f"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "A_b", "c")).render(),
            "export__api__AB__c"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "A", "b_c")).render(),
            "export__api__A__bC"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("a", "Foo", "bar")).render(),
            "export__a__Foo__bar"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "_Box", "mk")).render(),
            "exp_H010103020000000100000003617069000000045f426f78000000026d6b__api___Box__mk"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::item("a_/b", "c")).render(),
            "exp_H010103010000000200000002615f00000001620000000163__a__b__c"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::item("a/_b", "c")).render(),
            "exp_H01010301000000020000000161000000025f620000000163__a__b__c"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "A_", "b")).render(),
            "exp_H01010302000000010000000361706900000002415f0000000162__api__A___b"
        );
        assert_eq!(
            HaskellName::ExportWrapper(ItemId::newtype_member("api", "A", "_b")).render(),
            "exp_H0101030200000001000000036170690000000141000000025f62__api__A___b"
        );
        assert_ne!(
            HaskellName::ExportWrapper(ItemId::item("a", "foo__bar")).render(),
            HaskellName::ExportWrapper(ItemId::newtype_member("a", "Foo", "bar")).render()
        );
    }

    #[test]
    fn fixed_structural_vectors_keep_paths_and_roles_distinct() {
        let nested = BoundaryId::exp(ItemId::item("a", "f"), BoundaryRoot::Arg(0))
            .nested(BoundaryStep::Slot(0));
        assert_eq!(
            HaskellName::ProductPattern(nested.clone()).render(),
            "ExpP_H0120020100000001000000016100000001660100000000000000010200000000__a__f_arg0_slot0_P"
        );

        let outer = BoundaryId::exp(ItemId::item("a", "f"), BoundaryRoot::Arg(0));
        let arm = SumPatternId {
            boundary: outer.clone(),
            key: StructuralKey::QualifiedNewtype {
                module: ModuleId::from_path("slot0"),
                newtype: "P".to_owned(),
            },
            index: 0,
        };
        assert_eq!(
            HaskellName::SumPattern(arm).render(),
            "ExpS_H012102010000000100000001610000000166010000000000000000020000000100000005736c6f7430000000015000000000__a__f_arg0_S_slot0_P_0"
        );

        let outer_selector = HaskellName::ProductSelector {
            boundary: outer,
            key: StructuralKey::QualifiedNewtype {
                module: ModuleId::from_path("slot0"),
                newtype: "Y".to_owned(),
            },
            index: 2,
        };
        let nested_selector = HaskellName::ProductSelector {
            boundary: nested,
            key: StructuralKey::BareNewtype("Y".to_owned()),
            index: 2,
        };
        assert_eq!(
            outer_selector.render(),
            "expSel_H012202010000000100000001610000000166010000000000000000020000000100000005736c6f7430000000015900000002__a__f_arg0_Sel_slot0_Y_2"
        );
        assert_ne!(outer_selector.render(), nested_selector.render());
    }

    #[test]
    fn rank_n_helper_roles_are_distinct() {
        let sum = SumPatternId {
            boundary: BoundaryId::exp(ItemId::item("a", "f"), BoundaryRoot::Arg(0)),
            key: StructuralKey::BareNewtype("X".to_owned()),
            index: 0,
        };
        let pattern = PatternId::Sum(sum.clone());
        let names = [
            HaskellName::RankNViewType(pattern.clone()).render(),
            HaskellName::RankNViewConstructor(pattern.clone()).render(),
            HaskellName::RankNNoMatch(sum).render(),
            HaskellName::RankNViewFunction(pattern).render(),
        ];
        assert_eq!(
            names[0],
            "ViewT_H0130020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            names[1],
            "ViewC_H0131020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            names[2],
            "NoMatch_H01320201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            names[3],
            "view_H0133020201000000010000000161000000016601000000000000000001000000015800000000__a__f_arg0_S_X_0"
        );
        assert_eq!(
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );
    }

    #[test]
    fn remaining_codec_discriminants_have_fixed_vectors() {
        let env = BoundaryId::env("m", "h", BoundaryRoot::Ret)
            .nested(BoundaryStep::App(2))
            .nested(BoundaryStep::CallbackArg(3))
            .nested(BoundaryStep::CallbackRet);
        let positional = SumPatternId {
            boundary: env.clone(),
            key: StructuralKey::Positional(7),
            index: 7,
        };
        let vectors = [
            HaskellName::ExportWrapper(ItemId::item("a", "foo__bar")).render(),
            HaskellName::BoundaryAlias(env.clone()).render(),
            HaskellName::SumPattern(positional).render(),
            HaskellName::RankNViewType(PatternId::Product(env)).render(),
        ];
        assert_eq!(
            vectors,
            [
                "exp_H0101030100000001000000016100000008666f6f5f5f626172__a__fooBar",
                "Env_H01100100000001000000016d000000016802000000030100000002030000000304__m__h_ret_app2_cbarg3_cbret",
                "EnvS_H01210100000001000000016d000000016802000000030100000002030000000304030000000700000007__m__h_ret_app2_cbarg3_cbret_S_pos7_7",
                "ViewT_H0130010100000001000000016d000000016802000000030100000002030000000304__m__h_ret_app2_cbarg3_cbret_P",
            ]
        );
    }

    #[test]
    fn structural_family_fixed_vector_escapes_a_nominal_name_class() {
        let names = StructuralNames::new("KioCarrier_H6170690050726f64756374__api_");
        assert_eq!(
            names.product_family(),
            "KioStructT_H01000000284b696f436172726965725f48363137303639303035303732366636343735363337345f5f6170695f01__KioCarrier_H6170690050726f64756374__api_Product"
        );
        assert_eq!(
            names.sum_family(),
            "KioCarrier_H6170690050726f64756374__api_Sum"
        );
    }

    #[test]
    fn host_types_use_readable_names_with_identity_and_namespace_fallbacks() {
        let names = StructuralNames::new("Greeter");
        assert_eq!(
            names.host_assoc_name("a/b", "Thing"),
            "HostType__a__b__Thing"
        );
        assert_eq!(
            names.host_assoc_name("a__b", "Thing"),
            "HostType_H615f5f62005468696e67__aB__Thing"
        );
        assert_eq!(
            names.host_assoc_name("a_/b", "Edge"),
            "HostType_H615f2f620045646765__a___b__Edge"
        );
        assert_eq!(
            names.host_assoc_name("a/_b", "Edge"),
            "HostType_H612f5f620045646765__a___b__Edge"
        );

        let colliding = StructuralNames::new("HostType__a__b__Thing");
        assert_eq!(
            colliding.host_assoc_name("a/b", "Thing"),
            "HostType_H612f62005468696e67__a__b__Thing"
        );

        let fallback = "HostType_H615f5f62005468696e67__aB__Thing";
        let fallback_colliding = StructuralNames::new(fallback);
        assert_eq!(
            fallback_colliding.host_assoc_name("a__b", "Thing"),
            "KioStructT_H0100000029486f7374547970655f4836313566356636323030353436383639366536375f5f61425f5f5468696e67040000000100000004615f5f62000000055468696e67__HostType_H615f5f62005468696e67__aB__Thing"
        );
    }

    #[test]
    fn boundary_aliases_and_patterns_escape_same_namespace_facade_claims() {
        let boundary = BoundaryId::exp(ItemId::item("api", "pair"), BoundaryRoot::Ret);
        let alias = HaskellName::BoundaryAlias(boundary.clone());
        let preferred_alias = alias.render();
        let escaped_alias = StructuralNames::new(&preferred_alias).render(&alias);
        assert_ne!(escaped_alias, preferred_alias);
        assert!(escaped_alias.starts_with("KioStructT_H"), "{escaped_alias}");

        let pattern = HaskellName::ProductPattern(boundary);
        let preferred_pattern = pattern.render();
        let escaped_pattern = StructuralNames::new(&preferred_pattern).render(&pattern);
        assert_ne!(escaped_pattern, preferred_pattern);
        assert!(
            escaped_pattern.starts_with("KioStructC_H"),
            "{escaped_pattern}"
        );
    }

    #[test]
    fn safe_structural_names_keep_their_public_spellings() {
        let names = StructuralNames::new("Greeter");
        let alias =
            HaskellName::BoundaryAlias(BoundaryId::env("api", "pair", BoundaryRoot::Arg(0)));
        assert_eq!(names.product_family(), "GreeterProduct");
        assert_eq!(names.sum_family(), "GreeterSum");
        assert_eq!(names.render(&alias), alias.render());
    }

    #[test]
    fn claims_dedupe_only_identical_semantics_and_payloads() {
        let mut claims = NameClaims::default();
        let name = HaskellName::ModuleFn(ModuleItemId::new("a", "f"));
        let rendered = name.render();
        assert!(claims.claim(name.clone(), &rendered, "f :: ()"));
        assert!(!claims.claim(name, &rendered, "f :: ()"));
    }

    #[test]
    #[should_panic(expected = "incompatible declarations")]
    fn claims_reject_one_semantic_identity_with_two_payloads() {
        let mut claims = NameClaims::default();
        let name = HaskellName::ModuleFn(ModuleItemId::new("a", "f"));
        let rendered = name.render();
        claims.claim(name.clone(), &rendered, "f :: ()");
        claims.claim(name, &rendered, "f :: Bool");
    }
}
